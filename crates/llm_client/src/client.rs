//! HTTP クライアント本体。OpenAI 互換 chat/completions を叩き、指数 backoff でリトライする。
//!
//! `AsyncOpenAI` (Python) の Rust 版。ネットワーク経路は実キーが要るため単体テスト対象外
//! (壊れる ser/de は wire.rs / parse.rs 側で固める)。実 API 通しは「実クラウド通しプレイ」フェーズ。

use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::sync::Mutex;

use crate::anthropic;
use crate::canonical;
use crate::config::{LlmConfig, Provider};
use crate::error::LlmError;
use crate::gemini;
use crate::openai_compat;
use crate::parse;
use crate::wire::{ChatMessage, ChatRequest, ChatResponse};

/// プロンプトキャッシュの健全性の計測値。`cache_read`>0 = 安定プレフィックスがキャッシュから
/// 読まれた (入力コスト減)。GUI が**連続 miss** を検知して「キャッシュ経路が壊れているかも」を
/// 警告する材料になる — #44 (Anthropic 互換層は caching 非対応) / #45 (xAI は sticky ヘッダ必須)
/// の「キャッシュの静かな漏出は usage が一次ソース」を GUI へ引き上げる。
#[derive(Debug, Clone, Default, Serialize)]
pub struct CacheStat {
    /// 直近リクエストの cache read トークン (0 = miss)。
    pub last_cache_read: u64,
    /// 連続で cache read が 0 だった回数 (1 回でもヒットで 0 にリセット)。
    pub consecutive_misses: u32,
    /// 累計リクエスト数。初回は書き込みゆえ miss が正常なので、判定は 2 回目以降を見る。
    pub total_requests: u32,
}

impl CacheStat {
    /// 1 リクエスト分の cache read を記録する (純粋・テスト可)。
    pub(crate) fn record(&mut self, cache_read: u64) {
        self.total_requests = self.total_requests.saturating_add(1);
        self.last_cache_read = cache_read;
        if cache_read > 0 {
            self.consecutive_misses = 0;
        } else {
            self.consecutive_misses = self.consecutive_misses.saturating_add(1);
        }
    }
}

/// クラウド LLM ナレーター脚。
pub struct LlmClient {
    http: reqwest::Client,
    config: LlmConfig,
    /// セッション識別子。OpenAI 互換経路で `x-grok-conv-id` として送る (#45) —
    /// xAI のキャッシュは**サーバ単位**で、このヘッダが無いとロードバランサで散って
    /// 同一プレフィックスでも miss する (sticky routing)。xAI 以外は未知ヘッダとして無視。
    /// クライアントは app=ゲームセッション毎 / CLI=実行毎に作られるので粒度が会話に一致する。
    conv_id: String,
    /// キャッシュ健全性の計測 (interior mutability — propose は `&self`)。両経路のリクエストで
    /// cache read を記録し、GUI が連続 miss を警告に出す。
    cache_stat: Mutex<CacheStat>,
    /// Gemini の呼び出し id 合成用の単調カウンタ (spec 12 rev4 Must 4)。リクエスト毎に
    /// リセットしない — 却下→再生成の同一ターン内で `call_0` が重複しないため。
    call_seq: std::sync::atomic::AtomicU64,
    /// spec 13: Gemini 明示キャッシュのセッションハンドル。fingerprint が現在の静的プレフィックスと
    /// 一致すれば reuse、違えば作り直す (campaign 遷移等)。失効時はクリアして full request へ透過。
    gemini_cache: Mutex<Option<gemini::CacheHandle>>,
}

impl LlmClient {
    pub fn new(config: LlmConfig) -> Result<Self, LlmError> {
        let http = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .build()?;
        Ok(Self {
            http,
            config,
            conv_id: new_conv_id(),
            cache_stat: Mutex::new(CacheStat::default()),
            call_seq: std::sync::atomic::AtomicU64::new(0),
            gemini_cache: Mutex::new(None),
        })
    }

    pub fn config(&self) -> &LlmConfig {
        &self.config
    }

    /// セッション識別子 (x-grok-conv-id に載せる値)。
    pub fn conv_id(&self) -> &str {
        &self.conv_id
    }

    /// キャッシュ健全性のスナップショット (GUI の警告判定用)。lock 毒化時は既定値。
    pub fn cache_stat(&self) -> CacheStat {
        self.cache_stat.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// 1 リクエスト分の cache read を計測に記録する (両経路の chat_once / messages_once から呼ぶ)。
    fn record_cache(&self, cache_read: u64) {
        if let Ok(mut g) = self.cache_stat.lock() {
            g.record(cache_read);
        }
    }

    /// プレーンなテキスト生成 (`generate`)。ツール無し。
    pub async fn generate(&self, messages: Vec<ChatMessage>) -> Result<String, LlmError> {
        let req = canonical::ChatRequest {
            model: self.config.model.clone(),
            messages,
            tools: Vec::new(),
            tool_choice: canonical::ToolChoice::None,
            temperature: self.config.temperature,
            max_tokens: self.config.max_tokens,
            effort: self.config.effort,
        };
        let resp = self.complete(req).await?;
        resp.text
            .filter(|c| !c.trim().is_empty())
            .ok_or(LlmError::EmptyResponse)
    }

    /// 構造化出力 (`generate_json` の Rust 版)。
    ///
    /// 単一ツール `tool_name` を `tool_choice` で強制し、`parameters` schema に沿わせる。
    /// 応答は tool_calls もしくはフェンス JSON から `T` に解決する (抽出は canonical に
    /// 対する単一経路 [`parse::extract`])。
    pub async fn generate_structured<T: DeserializeOwned>(
        &self,
        messages: Vec<ChatMessage>,
        tool_name: &str,
        tool_description: &str,
        parameters: serde_json::Value,
    ) -> Result<T, LlmError> {
        let req = canonical::ChatRequest {
            model: self.config.model.clone(),
            messages,
            tools: vec![canonical::ToolSpec {
                name: tool_name.to_string(),
                description: tool_description.to_string(),
                parameters,
            }],
            tool_choice: canonical::ToolChoice::Specific(tool_name.to_string()),
            // temperature は config 任せ (未設定なら送らない)。
            temperature: self.config.temperature,
            max_tokens: self.config.max_tokens,
            effort: self.config.effort,
        };
        let resp = self.complete(req).await?;
        parse::extract::<T>(&resp)
    }

    /// canonical リクエストを 1 回完了させる — **adapter seam の単一入口** (spec 12 Phase A)。
    ///
    /// 経路選択 (provider match) はここだけ。各経路は encode 純関数 → HTTP (リトライ込み) →
    /// decode 純関数で canonical に戻る。キャッシュ計測 ([`CacheStat`]) も canonical usage から
    /// **一元記録**する (成功 1 回 = 記録 1 回。リトライの失敗試行は usage を持たないので
    /// 従来の per-成功記録と同値)。
    async fn complete(
        &self,
        req: canonical::ChatRequest,
    ) -> Result<canonical::ChatResponse, LlmError> {
        let resp = match self.config.provider {
            // Anthropic ネイティブ経路 (#44): 安定プレフィックス末尾の cache_control で
            // schema+system がキャッシュされる。tool_choice を確実に尊重するので常に tool-use
            // (use_tools は無視 = 従来動作)。effort 方言 (Phase B) も encode が持つ。
            Provider::Anthropic => {
                let native = anthropic::encode(&req);
                let raw = self.messages_with_retry(&native).await?;
                anthropic::decode(raw)
            }
            // OpenAI 互換経路: tool-use / no-tools (#29) の分岐は encode が担う。
            // decode + empty-response 防御 (Phase D) は試行毎に掛かる = 再抽選に乗る。
            Provider::OpenAiCompat => {
                let wire_req = openai_compat::encode(&req, self.config.use_tools);
                self.compat_with_retry(&wire_req).await?
            }
            // Gemini ネイティブ経路 (Phase C) + 明示キャッシュ (spec 13): 静的プレフィックスを
            // cachedContent に pin し、暗黙キャッシュの ~8000 崖 (failures #54) を迂回する。
            Provider::Gemini => self.gemini_complete(req).await?,
        };
        self.record_cache(resp.usage.cache_read);
        Ok(resp)
    }

    /// chat/completions を 1 回叩く (リトライ無し)。
    async fn chat_once(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        // 診断: LLM_DEBUG が設定されていれば送信ボディと生応答を stderr に出す。
        // tool_choice/schema を受理しつつ応答形が噛み合わないサーバ (Grok 等) の切り分け用。
        let debug = std::env::var("LLM_DEBUG").is_ok();
        if debug {
            eprintln!(
                "[LLM_DEBUG] request -> {}",
                serde_json::to_string(req).unwrap_or_default()
            );
        }
        let resp = self
            .http
            .post(self.config.chat_endpoint())
            .bearer_auth(&self.config.api_key)
            // xAI の sticky routing (#45)。他サーバは未知ヘッダとして無視する。
            .header("x-grok-conv-id", &self.conv_id)
            .json(req)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api {
                status: status.as_u16(),
                body,
            });
        }
        // 常に text→parse (json() 直は使わない)。2xx なのに形が合わない応答 (Gemini の
        // content filter / 長さ切れ / quota 系の変形) でデコードに失敗した時、json() は本文を
        // 捨て「missing field `message`」だけが残る — raw を保持して真因を診断可能にする (#34)。
        let body = resp.text().await?;
        if debug {
            eprintln!("[LLM_DEBUG] response <- {body}");
        }
        let decoded = decode_chat_body(body)?;
        // surface (ネイティブ経路の [LLM_CACHE] と同形)。CacheStat への記録は canonical usage
        // から complete() が一元で行う (成功 1 回 = 記録 1 回で従来と同値)。
        if debug || std::env::var("LLM_CACHE_DEBUG").is_ok() {
            if let Some(u) = &decoded.usage {
                let cached = u
                    .prompt_tokens_details
                    .as_ref()
                    .map(|d| d.cached_tokens)
                    .unwrap_or(0);
                eprintln!(
                    "[LLM_CACHE] cached={} prompt={} completion={}",
                    cached, u.prompt_tokens, u.completion_tokens
                );
            }
        }
        Ok(decoded)
    }

    /// 指数 backoff 付きで chat を叩き canonical まで解決する。一過性エラーのみリトライ
    /// (tenacity 同型)。decode と empty-response 防御 (spec 12 Phase D — 推論モデルが
    /// budget を思考に使い切った finish=length の空応答) を**試行の中**に含めることで、
    /// 空応答が思考の再抽選に乗る (Parse エラーは非一過性のまま = 従来どおり即失敗)。
    async fn compat_with_retry(
        &self,
        req: &ChatRequest,
    ) -> Result<canonical::ChatResponse, LlmError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let result = match self.chat_once(req).await {
                Ok(raw) => {
                    openai_compat::decode(raw).and_then(openai_compat::reject_empty_reasoning)
                }
                Err(e) => Err(e),
            };
            match result {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    if attempt >= self.config.max_retries || !e.is_transient() {
                        return Err(e);
                    }
                    // 1s, 2s, 4s ... 上限 10s (wait_exponential(min=1, max=10) 同型)。
                    let secs = (1u64 << (attempt - 1)).min(10);
                    tokio::time::sleep(Duration::from_secs(secs)).await;
                }
            }
        }
    }

    // --- Anthropic ネイティブ Messages API (#44) --------------------------------

    /// `POST {base_url}/messages` を 1 回叩く (リトライ無し)。
    /// 認証は Bearer でなく `x-api-key` + `anthropic-version` (ネイティブ API の作法)。
    async fn messages_once(
        &self,
        req: &anthropic::MessagesRequest,
    ) -> Result<anthropic::MessagesResponse, LlmError> {
        let debug = std::env::var("LLM_DEBUG").is_ok();
        if debug {
            eprintln!(
                "[LLM_DEBUG] request -> {}",
                serde_json::to_string(req).unwrap_or_default()
            );
        }
        let resp = self
            .http
            .post(self.config.messages_endpoint())
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", anthropic::ANTHROPIC_VERSION)
            .json(req)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api { status: status.as_u16(), body });
        }
        let body = resp.text().await?;
        if debug {
            eprintln!("[LLM_DEBUG] response <- {body}");
        }
        let decoded = decode_messages_body(body)?;
        // surface: LLM_CACHE_DEBUG=1 (または LLM_DEBUG) で stderr に 1 行。CacheStat への
        // 記録は canonical usage から complete() が一元で行う。
        if debug || std::env::var("LLM_CACHE_DEBUG").is_ok() {
            if let Some(u) = &decoded.usage {
                eprintln!(
                    "[LLM_CACHE] cache_read={} cache_write={} input={} output={}",
                    u.cache_read_input_tokens,
                    u.cache_creation_input_tokens,
                    u.input_tokens,
                    u.output_tokens
                );
            }
        }
        Ok(decoded)
    }

    /// 指数 backoff 付きで Messages API を叩く ([`Self::chat_with_retry`] のネイティブ版)。
    async fn messages_with_retry(
        &self,
        req: &anthropic::MessagesRequest,
    ) -> Result<anthropic::MessagesResponse, LlmError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.messages_once(req).await {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    if attempt >= self.config.max_retries || !e.is_transient() {
                        return Err(e);
                    }
                    let secs = (1u64 << (attempt - 1)).min(10);
                    tokio::time::sleep(Duration::from_secs(secs)).await;
                }
            }
        }
    }

    // --- Gemini ネイティブ generateContent (spec 12 Phase C) ------------------------

    /// `POST {base}/v1beta/models/{model}:generateContent` を 1 回叩く (リトライ無し)。
    /// 認証は **`x-goog-api-key` ヘッダ** — キーを URL クエリに載せない (K5。ログ/プロキシ
    /// へのキー露出を避ける。live 確証は Phase E、通らなければ query key へ改訂)。
    async fn gemini_once(
        &self,
        req: &gemini::GenerateContentRequest,
    ) -> Result<gemini::GenerateContentResponse, LlmError> {
        let debug = std::env::var("LLM_DEBUG").is_ok();
        if debug {
            eprintln!(
                "[LLM_DEBUG] request -> {}",
                serde_json::to_string(req).unwrap_or_default()
            );
        }
        let resp = self
            .http
            .post(self.config.gemini_endpoint())
            .header("x-goog-api-key", &self.config.api_key)
            .json(req)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api { status: status.as_u16(), body });
        }
        let body = resp.text().await?;
        if debug {
            eprintln!("[LLM_DEBUG] response <- {body}");
        }
        let decoded = decode_gemini_body(body)?;
        // surface (#44/#45 と同形)。Gemini 2.5 系は暗黙キャッシュが自動 —
        // cachedContentTokenCount > 0 = プレフィックスがキャッシュから読まれた。
        if debug || std::env::var("LLM_CACHE_DEBUG").is_ok() {
            if let Some(u) = &decoded.usage_metadata {
                eprintln!(
                    "[LLM_CACHE] cached={} prompt={} completion={}",
                    u.cached_content_token_count, u.prompt_token_count, u.candidates_token_count
                );
            }
        }
        Ok(decoded)
    }

    /// 指数 backoff 付きで generateContent を叩く ([`Self::chat_with_retry`] の Gemini 版)。
    async fn gemini_with_retry(
        &self,
        req: &gemini::GenerateContentRequest,
    ) -> Result<gemini::GenerateContentResponse, LlmError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.gemini_once(req).await {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    if attempt >= self.config.max_retries || !e.is_transient() {
                        return Err(e);
                    }
                    let secs = (1u64 << (attempt - 1)).min(10);
                    tokio::time::sleep(Duration::from_secs(secs)).await;
                }
            }
        }
    }

    /// Gemini リクエストを明示キャッシュ (spec 13) 込みで完了させる。fingerprint で cache を
    /// reuse/再作成し、systemInstruction+tools を cachedContent に載せて generateContent は
    /// 可変 contents だけ送る。作成失敗・サイズゲート未満・無効化は full request に fallback
    /// (キャッシュは最適化であって正しさの前提ではない — turn は絶対に落とさない)。
    async fn gemini_complete(
        &self,
        req: canonical::ChatRequest,
    ) -> Result<canonical::ChatResponse, LlmError> {
        let fp = gemini::fingerprint(&req);
        let static_chars = gemini::static_prefix_chars(&req);
        // std Mutex guard は await を跨げない — 判定だけ lock 内で済ませて即 drop。
        let action = {
            let guard = self.gemini_cache.lock();
            let handle = guard.as_ref().ok().and_then(|g| g.as_ref());
            gemini::decide_cache_action(
                self.config.gemini_cache_enabled,
                self.config.gemini_cache_min_chars,
                static_chars,
                fp,
                handle,
            )
        };
        let cache_name = match action {
            gemini::CacheAction::Reuse(name) => Some(name),
            gemini::CacheAction::Bypass => None,
            gemini::CacheAction::Create => match self.gemini_create_cache(&req, fp).await {
                Ok(name) => Some(name),
                Err(e) => {
                    if std::env::var("LLM_CACHE_DEBUG").is_ok() {
                        eprintln!("[LLM_CACHE] cachedContent 作成失敗 → full request にフォールバック: {e}");
                    }
                    None
                }
            },
        };

        let native = gemini::encode_with_cache(&req, cache_name.clone());
        let raw = match self.gemini_with_retry(&native).await {
            Ok(r) => r,
            // cache 参照の失効兆候 → handle をクリアして full request で 1 回だけ再試行 (透過)。
            Err(e) if cache_name.is_some() && gemini::is_cache_miss_error(&e) => {
                if let Ok(mut g) = self.gemini_cache.lock() {
                    *g = None;
                }
                if std::env::var("LLM_CACHE_DEBUG").is_ok() {
                    eprintln!("[LLM_CACHE] cachedContent 失効 → full request で再試行");
                }
                self.gemini_with_retry(&gemini::encode(&req)).await?
            }
            Err(e) => return Err(e),
        };
        let seq = self.call_seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(gemini::decode(raw, seq))
    }

    /// `POST {base}/v1beta/cachedContents` で静的プレフィックスを pin し、handle を保存して
    /// resource name を返す (spec 13 Phase B)。認証は generateContent と同じ x-goog-api-key。
    async fn gemini_create_cache(
        &self,
        req: &canonical::ChatRequest,
        fp: u64,
    ) -> Result<String, LlmError> {
        let create = gemini::build_create_request(req, self.config.gemini_cache_ttl_secs);
        let resp = self
            .http
            .post(self.config.cachedcontents_endpoint())
            .header("x-goog-api-key", &self.config.api_key)
            .json(&create)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api { status: status.as_u16(), body });
        }
        let body = resp.text().await?;
        let parsed: gemini::CreateCacheResponse =
            serde_json::from_str(&body).map_err(|source| LlmError::Parse { source, raw: body })?;
        if parsed.name.is_empty() {
            return Err(LlmError::Api { status: 200, body: "cachedContents create: name が空".into() });
        }
        if let Ok(mut g) = self.gemini_cache.lock() {
            *g = Some(gemini::CacheHandle { name: parsed.name.clone(), fingerprint: fp });
        }
        if std::env::var("LLM_CACHE_DEBUG").is_ok() {
            eprintln!("[LLM_CACHE] cachedContent 作成: {}", parsed.name);
        }
        Ok(parsed.name)
    }
}

/// 2xx 応答の本文を [`ChatResponse`] へ。形が合わなければ [`LlmError::Parse`] で
/// **本文 (raw) を保持**する — serde の「missing field」だけでは真因 (content filter /
/// 長さ切れ等のサーバ都合の変形応答) が見えないため (#34)。
pub(crate) fn decode_chat_body(body: String) -> Result<ChatResponse, LlmError> {
    serde_json::from_str::<ChatResponse>(&body)
        .map_err(|source| LlmError::Parse { source, raw: body })
}

/// Messages API 版の [`decode_chat_body`]。同じく **raw を保持** (#34 同型)。
pub(crate) fn decode_messages_body(body: String) -> Result<anthropic::MessagesResponse, LlmError> {
    serde_json::from_str::<anthropic::MessagesResponse>(&body)
        .map_err(|source| LlmError::Parse { source, raw: body })
}

/// generateContent 版の [`decode_chat_body`]。同じく **raw を保持** (#34 同型)。
pub(crate) fn decode_gemini_body(body: String) -> Result<gemini::GenerateContentResponse, LlmError> {
    serde_json::from_str::<gemini::GenerateContentResponse>(&body)
        .map_err(|source| LlmError::Parse { source, raw: body })
}

/// セッション識別子を作る。プロセス ID + 単調カウンタ + 起動時刻ナノ秒 —
/// 会話を跨いで衝突しなければよい (暗号強度は不要)。uuid 依存を増やさない。
fn new_conv_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("kataribe-{}-{}-{}", std::process::id(), nanos, n)
}

