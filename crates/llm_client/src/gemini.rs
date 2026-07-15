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

    // cache が静的プレフィックス (systemInstruction + tools) を持つ時は二重送信しない。
    let use_cache = cached.is_some();
    GenerateContentRequest {
        contents,
        system_instruction: if use_cache || system_parts.is_empty() {
            None
        } else {
            Some(SystemInstruction { parts: system_parts })
        },
        tools: if use_cache { None } else { tool_decls },
        tool_config,
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
