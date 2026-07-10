//! HTTP クライアント本体。OpenAI 互換 chat/completions を叩き、指数 backoff でリトライする。
//!
//! `AsyncOpenAI` (Python) の Rust 版。ネットワーク経路は実キーが要るため単体テスト対象外
//! (壊れる ser/de は wire.rs / parse.rs 側で固める)。実 API 通しは「実クラウド通しプレイ」フェーズ。

use std::time::Duration;

use serde::de::DeserializeOwned;

use crate::anthropic;
use crate::config::{LlmConfig, Provider};
use crate::error::LlmError;
use crate::parse;
use crate::wire::{
    ChatMessage, ChatRequest, ChatResponse, FunctionDef, Tool, ToolChoice, ToolKind,
};

/// クラウド LLM ナレーター脚。
pub struct LlmClient {
    http: reqwest::Client,
    config: LlmConfig,
}

impl LlmClient {
    pub fn new(config: LlmConfig) -> Result<Self, LlmError> {
        let http = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .build()?;
        Ok(Self { http, config })
    }

    pub fn config(&self) -> &LlmConfig {
        &self.config
    }

    /// プレーンなテキスト生成 (`generate`)。ツール無し。
    pub async fn generate(&self, messages: Vec<ChatMessage>) -> Result<String, LlmError> {
        if self.config.provider == Provider::Anthropic {
            let req = anthropic::build_request(
                &self.config.model,
                self.config.max_tokens,
                self.config.temperature,
                messages,
                None,
            );
            let resp = self.messages_with_retry(&req).await?;
            return resp
                .into_response_message()
                .content
                .filter(|c| !c.trim().is_empty())
                .ok_or(LlmError::EmptyResponse);
        }
        let req = ChatRequest {
            model: self.config.model.clone(),
            messages,
            temperature: self.config.temperature,
            max_tokens: self.config.max_tokens,
            tools: Vec::new(),
            tool_choice: None,
        };
        let resp = self.chat_with_retry(&req).await?;
        resp.first_message()
            .and_then(|m| m.content.clone())
            .filter(|c| !c.trim().is_empty())
            .ok_or(LlmError::EmptyResponse)
    }

    /// 構造化出力 (`generate_json` の Rust 版)。
    ///
    /// 単一ツール `tool_name` を `tool_choice` で強制し、`parameters` schema に沿わせる。
    /// 応答は tool_calls もしくはフェンス JSON から `T` に解決する。
    pub async fn generate_structured<T: DeserializeOwned>(
        &self,
        messages: Vec<ChatMessage>,
        tool_name: &str,
        tool_description: &str,
        parameters: serde_json::Value,
    ) -> Result<T, LlmError> {
        // Anthropic ネイティブ経路 (#44): 安定プレフィックス末尾の cache_control で
        // schema+system がキャッシュされる。tool_choice を確実に尊重するので常に tool-use。
        if self.config.provider == Provider::Anthropic {
            let req = anthropic::build_request(
                &self.config.model,
                self.config.max_tokens,
                self.config.temperature,
                messages,
                Some((tool_name.to_string(), tool_description.to_string(), parameters)),
            );
            let resp = self.messages_with_retry(&req).await?;
            let message = resp.into_response_message();
            return parse::extract::<T>(&message);
        }
        // tool-use 対応サーバ (OpenAI 互換) は tool_choice 強制で構造を保証。
        // 非対応サーバ (さくら AI Engine / ローカル互換) は tools を送らず、prompt で JSON 出力を
        // 指示して content から拾う (extract のフォールバック)。
        let mut messages = messages;
        let (tools, tool_choice) = if self.config.use_tools {
            let tool = Tool {
                kind: ToolKind::Function,
                function: FunctionDef {
                    name: tool_name.to_string(),
                    description: tool_description.to_string(),
                    parameters,
                },
            };
            (vec![tool], Some(ToolChoice::force(tool_name)))
        } else {
            messages.push(ChatMessage::system(json_instruction(&parameters)));
            (Vec::new(), None)
        };
        let req = ChatRequest {
            model: self.config.model.clone(),
            // temperature は config 任せ (未設定なら送らない)。
            temperature: self.config.temperature,
            max_tokens: self.config.max_tokens,
            messages,
            tools,
            tool_choice,
        };
        let resp = self.chat_with_retry(&req).await?;
        let message = resp.first_message().ok_or(LlmError::EmptyResponse)?;
        parse::extract::<T>(message)
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
        decode_chat_body(body)
    }

    /// 指数 backoff 付きで chat を叩く。一過性エラーのみリトライ (tenacity 同型)。
    async fn chat_with_retry(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.chat_once(req).await {
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
        // キャッシュ計測の surface: LLM_CACHE_DEBUG=1 (または LLM_DEBUG) で stderr に 1 行。
        // 修正の Green 判定 = 2 リクエスト目以降 cache_read > 0 (0 のままなら無効化要因がある)。
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

/// no-tools モードで「schema に従う JSON だけを出力せよ」と指示する system メッセージ本文。
/// tool_choice 非対応サーバ (さくら AI Engine / ローカル OpenAI 互換) 向け。schema は単一真実源。
pub(crate) fn json_instruction(schema: &serde_json::Value) -> String {
    format!(
        "重要: このサーバはツール呼び出し (function calling) に対応していません。\
        応答は次の JSON Schema に厳密に従う JSON オブジェクトを **1つだけ** 出力し、\
        前置き・説明・コードフェンスのラベル等、余計なテキストを一切含めないでください。\n\
        JSON Schema:\n{}",
        serde_json::to_string(schema).unwrap_or_default()
    )
}
