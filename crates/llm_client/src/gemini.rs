//! Gemini ネイティブ adapter (`generateContent`) — spec 12 Phase C。
//!
//! canonical ⇄ Gemini wire の **encode/decode 純関数**。HTTP・リトライ・認証
//! (`x-goog-api-key` ヘッダ — キーを URL クエリに載せない、spec 12 K5) は client 核が担う。
//!
//! 翻訳の要点 (写経元 §6/§8a、rev4 で精密化):
//! - system は `systemInstruction` (model ターンに畳まない — 写経元 D3)
//! - tools は `tools[].functionDeclarations`、単一ツール強制は
//!   `toolConfig.functionCallingConfig {mode: ANY, allowedFunctionNames: [name]}` (K2)
//! - 応答 `functionCall.args` は最初からオブジェクト (D2 の写像は恒等)
//! - Gemini は呼び出し id を持たない → **client 単位の単調カウンタ**から `call_{seq}_{index}`
//!   を合成 (rev4 Must 4 — リクエスト毎リセットの `call_0` は却下→再生成で衝突する)
//! - v1beta は camelCase を受理する (キー名は serde rename_all で固定)

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::canonical;
use crate::error::LlmError;
use crate::wire::Role;

// --- リクエスト ---------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GenerateContentRequest {
    pub contents: Vec<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<SystemInstruction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDecl>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_config: Option<ToolConfig>,
    pub generation_config: GenerationConfig,
    /// spec 13: 明示キャッシュ (cachedContent) 参照。`Some(name)` のとき systemInstruction/tools は
    /// **送らず** cache が前置する (二重送信を避ける)。`None` は従来どおり inline (skip される = 回帰ゼロ)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_content: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Content {
    pub role: &'static str, // "user" | "model"
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Part {
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SystemInstruction {
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolDecl {
    pub function_declarations: Vec<FunctionDecl>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct FunctionDecl {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolConfig {
    pub function_calling_config: FunctionCallingConfig,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FunctionCallingConfig {
    pub mode: &'static str, // "ANY" (強制) | "AUTO"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_function_names: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GenerationConfig {
    pub max_output_tokens: u32,
    /// 明示設定時のみ送る (Gemini は temperature 対応 — 他 adapter と同じ None 既定)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

/// canonical → Gemini ネイティブリクエスト (encode 純関数)。
///
/// - 先頭の連続 system → `systemInstruction` (パート毎に保持)。
/// - 先頭以外の system (万一混じった場合) → user に降格 (anthropic encode と同じ流儀)。
/// - assistant → role "model"。
/// - `effort` は送らない (Gemini の thinkingConfig 方言は未実装 — 対象は Claude/Grok のみ)。
/// - no-tools モード (`use_tools=false`) は Gemini では無視 (functionCallingConfig を
///   確実に尊重するため不要 — Anthropic と同じ扱い、spec 12 K4)。
pub(crate) fn encode(req: &canonical::ChatRequest) -> GenerateContentRequest {
    encode_with_cache(req, None)
}

/// [`encode`] の明示キャッシュ版 (spec 13 Phase A)。`cached` が `Some(name)` のとき
/// **systemInstruction と tools を送らず** `cachedContent: name` を参照する (静的プレフィックスは
/// cache が前置 = 暗黙キャッシュの ~8000 崖 (failures #54) を迂回)。`None` なら従来 body と完全一致
/// (`cached_content` は skip される = 回帰ゼロ)。tool_config (mode ANY の**強制指定**) は request 側に
/// 残す — cache が持つのはツール宣言、「どれを強制するか」は per-request の指定 (D1)。
pub(crate) fn encode_with_cache(
    req: &canonical::ChatRequest,
    cached: Option<String>,
) -> GenerateContentRequest {
    let mut system_parts: Vec<Part> = Vec::new();
    let mut contents: Vec<Content> = Vec::new();
    for m in &req.messages {
        match m.role {
            Role::System if contents.is_empty() => {
                system_parts.push(Part { text: m.content.clone() })
            }
            Role::System | Role::Tool | Role::User => contents.push(Content {
                role: "user",
                parts: vec![Part { text: m.content.clone() }],
            }),
            Role::Assistant => contents.push(Content {
                role: "model",
                parts: vec![Part { text: m.content.clone() }],
            }),
        }
    }

    let (tool_decls, tool_config) = if req.tools.is_empty() {
        (None, None)
    } else {
        let decls = req
            .tools
            .iter()
            .map(|t| FunctionDecl {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: adapt_schema(&t.parameters),
            })
            .collect();
        // 単一ツール強制 (Specific) は mode ANY + allowedFunctionNames (K2 の写像)。
        // それ以外は AUTO (モデル任せ)。
        let config = match &req.tool_choice {
            canonical::ToolChoice::Specific(name) => FunctionCallingConfig {
                mode: "ANY",
                allowed_function_names: Some(vec![name.clone()]),
            },
            _ => FunctionCallingConfig { mode: "AUTO", allowed_function_names: None },
        };
        (
            Some(vec![ToolDecl { function_declarations: decls }]),
            Some(ToolConfig { function_calling_config: config }),
        )
    };

    // cache が静的プレフィックス (systemInstruction + tools + tool_config) を持つ時は二重送信
    // しない。**Gemini は cachedContent 参照時にこれらのいずれかが request にあると 400**
    // ("CachedContent can not be used with GenerateContent request setting system_instruction,
    // tools or tool_config" — Phase D live で確認)。強制指定 (mode ANY) も cache 側に載せる。
    let use_cache = cached.is_some();
    GenerateContentRequest {
        contents,
        system_instruction: if use_cache || system_parts.is_empty() {
            None
        } else {
            Some(SystemInstruction { parts: system_parts })
        },
        tools: if use_cache { None } else { tool_decls },
        tool_config: if use_cache { None } else { tool_config },
        generation_config: GenerationConfig {
            max_output_tokens: req.max_tokens,
            temperature: req.temperature,
        },
        cached_content: cached,
    }
}

/// FNV-1a 64bit の 1 ステップ (決定論・stable、CLAUDE.md の save パス命名と同族)。
/// `#[allow(dead_code)]`: Phase A では [`fingerprint`] (テスト検証済) の部品どまり — Phase B の
/// cache lifecycle (client の reuse/再作成判定) で wire する。
#[allow(dead_code)]
fn fnv1a(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 静的プレフィックス (model + 先頭 system + tools) の安定 fingerprint (spec 13 Phase A)。
/// 明示キャッシュの reuse/再作成判定に使う: 同じプレフィックス→同 key、scenario/model/tools 変化→別 key。
/// **可変な user/assistant メッセージは含めない** (cache の対象外)。session-local で使い (永続しない)、
/// プロセス内の決定論で足りる。
/// `#[allow(dead_code)]`: Phase A では PoC で決定論を固定するのみ — Phase B で client の
/// `gemini_cache` 照合に wire する (同 key → reuse / 別 key → 再作成)。
#[allow(dead_code)]
pub(crate) fn fingerprint(req: &canonical::ChatRequest) -> u64 {
    let mut h = fnv1a(0xcbf2_9ce4_8422_2325, req.model.as_bytes());
    // 先頭の連続 system 群 (= systemInstruction になる部分) だけが静的プレフィックス。
    for m in &req.messages {
        match m.role {
            Role::System => {
                h = fnv1a(h, b"\x00sys\x00");
                h = fnv1a(h, m.content.as_bytes());
            }
            _ => break,
        }
    }
    for t in &req.tools {
        h = fnv1a(h, b"\x00tool\x00");
        h = fnv1a(h, t.name.as_bytes());
        h = fnv1a(h, t.description.as_bytes());
        h = fnv1a(h, t.parameters.to_string().as_bytes());
    }
    h
}

// --- 明示キャッシュ / cachedContent (spec 13 Phase B) --------------------------

/// 明示キャッシュのセッションハンドル。client が `Mutex<Option<CacheHandle>>` で保持する。
/// `fingerprint` が現リクエストの静的プレフィックスと一致すれば reuse、違えば作り直す。
#[derive(Debug, Clone)]
pub(crate) struct CacheHandle {
    /// `cachedContents/<id>`。
    pub name: String,
    /// 作成時の静的プレフィックス fingerprint (再作成判定)。
    pub fingerprint: u64,
}

/// cache をどう扱うか ([`decide_cache_action`] の結果)。
#[derive(Debug)]
pub(crate) enum CacheAction {
    /// 既存 cache を参照する。
    Reuse(String),
    /// cache を (再) 作成する (handle 無し or fingerprint 不一致 = scenario 変化)。
    Create,
    /// cache を使わない (無効 or サイズゲート未満) — 従来の full request。
    Bypass,
}

/// cache 判定 (純粋・spec 13 D2/D3)。`enabled` off / `static_chars < min_chars` は Bypass。
/// handle の fingerprint が現在と一致すれば Reuse、それ以外 (None/不一致) は Create。
pub(crate) fn decide_cache_action(
    enabled: bool,
    min_chars: usize,
    static_chars: usize,
    current_fp: u64,
    handle: Option<&CacheHandle>,
) -> CacheAction {
    if !enabled || static_chars < min_chars {
        return CacheAction::Bypass;
    }
    match handle {
        Some(h) if h.fingerprint == current_fp => CacheAction::Reuse(h.name.clone()),
        _ => CacheAction::Create,
    }
}

/// 静的プレフィックス (先頭 system + tools) の文字数 = サイズゲートの近似トークン量。
/// 可変 user は含めない。明示キャッシュの最小トークン閾値に対する**安いゲート** — 正確さは
/// create 失敗の fallback が守るので char 近似で足りる。
pub(crate) fn static_prefix_chars(req: &canonical::ChatRequest) -> usize {
    let mut n = 0;
    for m in &req.messages {
        match m.role {
            Role::System => n += m.content.chars().count(),
            _ => break,
        }
    }
    for t in &req.tools {
        n += t.name.chars().count()
            + t.description.chars().count()
            + t.parameters.to_string().chars().count();
    }
    n
}

/// `POST /v1beta/cachedContents` の作成リクエスト。静的プレフィックス + TTL を pin する。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateCacheRequest {
    /// **`models/<name>` 形式必須** (create API の要求)。
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<SystemInstruction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDecl>>,
    /// 強制指定 (mode ANY) も cache に載せる — Gemini は cachedContent 参照時に request 側の
    /// tool_config を 400 で拒否する (Phase D live)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_config: Option<ToolConfig>,
    /// `"900s"` 形式。
    pub ttl: String,
}

/// create 応答 (必要なのは resource name だけ)。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct CreateCacheResponse {
    /// `cachedContents/<id>`。
    #[serde(default)]
    pub name: String,
}

/// 静的プレフィックス (systemInstruction + tools) から create body を組む (spec 13 Phase B)。
/// `encode_with_cache(req, None)` の抽出を**再利用** — cache する中身が、cache 不使用時に
/// generateContent へ inline されるものと必ず一致する (乖離が起きない)。可変 contents は含めない。
pub(crate) fn build_create_request(
    req: &canonical::ChatRequest,
    ttl_secs: u64,
) -> CreateCacheRequest {
    let full = encode_with_cache(req, None);
    let model = if req.model.starts_with("models/") {
        req.model.clone()
    } else {
        format!("models/{}", req.model)
    };
    CreateCacheRequest {
        model,
        system_instruction: full.system_instruction,
        tools: full.tools,
        tool_config: full.tool_config,
        ttl: format!("{ttl_secs}s"),
    }
}

/// cache 参照が失効した兆候か (TTL 切れ等で cachedContent 不在)。保守的に Api 403/404 のみ —
/// transient (429/5xx) は gemini_with_retry が既に捌く。正確なトリガーは live で確定 (Phase D)。
pub(crate) fn is_cache_miss_error(err: &LlmError) -> bool {
    matches!(err, LlmError::Api { status, .. } if *status == 403 || *status == 404)
}

/// JSON Schema を Gemini の Schema サブセット (OpenAPI 3.0 系) へ適応させる (Phase C.5a)。
///
/// **実測の罠 (2026-07-15, failures.md #52)**: Gemini の functionDeclarations は `oneOf` を
/// **400 にせず黙って落とす** — schemars が StateOp に出す `ops.items.oneOf` の制約が消え、
/// モデルが `"ops": [1, 2, 3]` (整数列!) を捏造した。Grok の $ref 非解決 (#①) と同族の
/// 「grammar コンパイラが schema を部分適用する」系だが、こちらは**エラーすら出ない**分
/// 静かに悪い。Gemini の Schema は `anyOf` を対応する (2.0 系〜) ので付け替える —
/// バリアント毎の required/properties 制約を保ったまま Gemini の grammar に乗る。
/// (それでも制約が効かない場合の次段 = 全バリアント統合の単一 object 化 C.5b、live で判断。)
pub(crate) fn adapt_schema(schema: &Value) -> Value {
    match schema {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                let key = if k == "oneOf" { "anyOf" } else { k.as_str() };
                out.insert(key.to_string(), adapt_schema(v));
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(adapt_schema).collect()),
        other => other.clone(),
    }
}

// --- レスポンス ---------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GenerateContentResponse {
    #[serde(default)]
    pub candidates: Vec<Candidate>,
    #[serde(default)]
    pub usage_metadata: Option<UsageMetadata>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Candidate {
    #[serde(default)]
    pub content: Option<CandidateContent>,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CandidateContent {
    #[serde(default)]
    pub parts: Vec<RespPart>,
}

/// 応答パート。text と functionCall が別パートで混在しうる (どちらも Option で受ける)。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RespPart {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub function_call: Option<FunctionCall>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct FunctionCall {
    #[serde(default)]
    pub name: String,
    /// 最初から JSON オブジェクト (D2 の写像は恒等)。
    #[serde(default)]
    pub args: Value,
}

/// Gemini の暗黙キャッシュ計数 (`cachedContentTokenCount`) を含む usage。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsageMetadata {
    #[serde(default)]
    pub prompt_token_count: u64,
    #[serde(default)]
    pub candidates_token_count: u64,
    #[serde(default)]
    pub cached_content_token_count: u64,
}

/// Gemini ネイティブ応答 → canonical (decode 純関数)。
///
/// `seq` は client 単位の単調カウンタ (rev4 Must 4) — Gemini は呼び出し id を返さないので
/// `call_{seq}_{index}` を合成する (同一ターン内の却下→再生成でも衝突しない)。
pub(crate) fn decode(resp: GenerateContentResponse, seq: u64) -> canonical::ChatResponse {
    let usage = resp
        .usage_metadata
        .as_ref()
        .map(|u| canonical::Usage {
            prompt: u.prompt_token_count,
            completion: u.candidates_token_count,
            cache_read: u.cached_content_token_count,
        })
        .unwrap_or_default();

    let Some(candidate) = resp.candidates.into_iter().next() else {
        return canonical::ChatResponse {
            text: None,
            tool_calls: Vec::new(),
            finish: canonical::Finish::Other,
            usage,
        };
    };

    let mut texts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<canonical::ToolCall> = Vec::new();
    for part in candidate.content.map(|c| c.parts).unwrap_or_default() {
        if let Some(text) = part.text {
            texts.push(text);
        }
        if let Some(fc) = part.function_call {
            tool_calls.push(canonical::ToolCall {
                id: format!("call_{seq}_{}", tool_calls.len()),
                name: fc.name,
                args: fc.args,
            });
        }
    }

    // Gemini に tool_use 相当の finishReason は無い — functionCall の有無から導出 (写経元 §6)。
    let finish = if !tool_calls.is_empty() {
        canonical::Finish::ToolUse
    } else {
        match candidate.finish_reason.as_deref() {
            Some("STOP") => canonical::Finish::Stop,
            Some("MAX_TOKENS") => canonical::Finish::Length,
            _ => canonical::Finish::Other,
        }
    };

    canonical::ChatResponse {
        text: if texts.is_empty() { None } else { Some(texts.join("\n")) },
        tool_calls,
        finish,
        usage,
    }
}
