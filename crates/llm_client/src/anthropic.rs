//! Anthropic ネイティブ Messages API (`POST {base_url}/messages`) のワイヤ型と変換。
//!
//! **なぜ存在するか (#44)**: OpenAI 互換層 (`/chat/completions`) は **prompt caching 非対応**
//! (公式 docs に明記。cache_control を送っても黙殺され `usage.prompt_tokens_details` も常に空)。
//! Kataribe は毎ターン messages を新規構築して送るため、互換層経由では全入力が
//! 非キャッシュ価格 (input_no_cache) になっていた。ネイティブ API なら安定プレフィックス
//! (render 順 tools→system = emit_delta schema + GM_SYSTEM + scenario_brief) の末尾に
//! `cache_control: ephemeral` を置け、2 ターン目以降その部分が 0.1× のキャッシュ読取になる。
//!
//! 変換の境界: 呼び出し側 (harness) は従来どおり OpenAI 形の [`ChatMessage`] を組む。
//! この module がネイティブ形へ写し、応答も OpenAI 形の [`ResponseMessage`] へ写し戻して
//! 既存 [`crate::parse::extract`] に合流させる — 抽出・救済ロジックの経路は単一のまま。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::canonical;
use crate::wire::Role;

/// 必須ヘッダ `anthropic-version` の値。
pub(crate) const ANTHROPIC_VERSION: &str = "2023-06-01";

// --- リクエスト ---------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MessagesRequest {
    pub model: String,
    pub max_tokens: u32,
    /// 明示設定時のみ送る (claude-opus-4-8 等は temperature 非対応で 400)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// 先頭 system 群。**末尾ブロックに cache_control** = tools→system の安定プレフィックス
    /// 全体をキャッシュする breakpoint (最大 4 個中 1 個だけ使う)。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub system: Vec<SystemBlock>,
    pub messages: Vec<TurnMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    /// adaptive thinking (spec 12 Phase B)。effort 設定時のみ `{"type":"adaptive"}` を送る
    /// (未設定なら**キーごと送らない** = 現行動作。budget_tokens は送らない — Opus 4.8 で 400)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<Thinking>,
    /// `output_config.effort` (effort は output_config の**中** — トップレベルでない)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<OutputConfig>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Thinking {
    #[serde(rename = "type")]
    pub kind: &'static str, // 常に "adaptive"
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct OutputConfig {
    pub effort: &'static str, // low | medium | high | xhigh | max
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SystemBlock {
    #[serde(rename = "type")]
    pub kind: &'static str, // 常に "text"
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CacheControl {
    #[serde(rename = "type")]
    pub kind: &'static str, // 常に "ephemeral" (TTL 5 分、読取で更新。書込 1.25× / 読取 0.1×)
}

impl CacheControl {
    fn ephemeral() -> Self {
        Self { kind: "ephemeral" }
    }
}

/// 会話ターン。content は素の文字列 (ネイティブ API は string content を受理する)。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct TurnMessage {
    pub role: &'static str, // "user" | "assistant"
    pub content: String,
}

/// ネイティブ形のツール定義 (OpenAI 形と違い function 包みが無く、schema キーは `input_schema`)。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// 特定ツールの呼び出し強制 (`{"type":"tool","name":...}`)。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ToolChoice {
    #[serde(rename = "type")]
    pub kind: &'static str, // 常に "tool"
    pub name: String,
}

/// canonical → ネイティブ Messages リクエスト (spec 12 Phase A/B の encode 純関数)。
///
/// - 先頭の連続 system → `system` ブロック配列 (末尾に cache_control)。
/// - 先頭以外の system (万一混じった場合) → user に降格 (ネイティブは先頭 system のみ)。
/// - tools があれば ToolDef + `{type: tool, name}` で強制 (ネイティブは tool_choice を確実に
///   尊重するので use_tools は関係ない = 常に tool-use)。
/// - effort 設定時のみ `thinking: adaptive` + `output_config.effort` (Phase B、opt-in)。
pub(crate) fn encode(req: &canonical::ChatRequest) -> MessagesRequest {
    let mut system: Vec<SystemBlock> = Vec::new();
    let mut turns: Vec<TurnMessage> = Vec::new();
    for m in &req.messages {
        match m.role {
            Role::System if turns.is_empty() => system.push(SystemBlock {
                kind: "text",
                text: m.content.clone(),
                cache_control: None,
            }),
            // 先頭以外の system は user へ降格 (壊さない)。Role::Tool は現状未使用だが同様に降格。
            Role::System | Role::Tool | Role::User => turns.push(TurnMessage {
                role: "user",
                content: m.content.clone(),
            }),
            Role::Assistant => turns.push(TurnMessage {
                role: "assistant",
                content: m.content.clone(),
            }),
        }
    }
    // 安定プレフィックスの末尾に breakpoint。可変な turns 側には置かない
    // (毎ターン別内容 → 読まれないキャッシュ書込 1.25× の無駄になるだけ)。
    if let Some(last) = system.last_mut() {
        last.cache_control = Some(CacheControl::ephemeral());
    }

    let (tools, tool_choice) = match req.tools.first() {
        Some(t) => (
            vec![ToolDef {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: t.parameters.clone(),
            }],
            Some(ToolChoice { kind: "tool", name: t.name.clone() }),
        ),
        None => (Vec::new(), None),
    };

    // effort は opt-in (None なら thinking/output_config ともキーごと送らない = 現行動作)。
    // 形は公式例に固定: thinking {type: adaptive} + output_config {effort} (spec 12 rev4)。
    let (thinking, output_config) = match req.effort {
        Some(e) => (
            Some(Thinking { kind: "adaptive" }),
            Some(OutputConfig { effort: e.as_str() }),
        ),
        None => (None, None),
    };

    MessagesRequest {
        model: req.model.clone(),
        max_tokens: req.max_tokens,
        temperature: req.temperature,
        system,
        messages: turns,
        tools,
        tool_choice,
        thinking,
        output_config,
    }
}

// --- レスポンス ---------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MessagesResponse {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub usage: Option<Usage>,
    /// 終了理由 (`end_turn`/`tool_use`/`max_tokens`/...)。canonical `Finish` の材料。
    #[serde(default)]
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        /// tool の引数 (OpenAI 形と違い **JSON オブジェクト**。文字列ではない)。
        #[serde(default)]
        input: Value,
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        name: Option<String>,
    },
    /// 未知ブロック (thinking 等) は無視する (前方互換)。
    #[serde(other)]
    Other,
}

/// トークン使用量。キャッシュ計数が **本修正の Green 判定**
/// (`cache_read_input_tokens > 0` = プレフィックスがキャッシュから読まれた)。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

/// ネイティブ応答 → canonical (spec 12 Phase A)。
///
/// tool_use の `input` は最初から JSON オブジェクトなので写像は恒等 (写経元 D2 —
/// 従来の「文字列化 → OpenAI 形 → 再パース」の往復を廃止)。抽出 (`parse::extract`) は
/// canonical に対する単一経路に合流する。
pub(crate) fn decode(resp: MessagesResponse) -> canonical::ChatResponse {
    let usage = resp
        .usage
        .as_ref()
        .map(|u| canonical::Usage {
            prompt: u.input_tokens,
            completion: u.output_tokens,
            cache_read: u.cache_read_input_tokens,
        })
        .unwrap_or_default();
    let finish = match resp.stop_reason.as_deref() {
        Some("end_turn") => canonical::Finish::Stop,
        Some("tool_use") => canonical::Finish::ToolUse,
        Some("max_tokens") => canonical::Finish::Length,
        _ => canonical::Finish::Other,
    };
    let mut texts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<canonical::ToolCall> = Vec::new();
    for block in resp.content {
        match block {
            ContentBlock::Text { text } => texts.push(text),
            ContentBlock::ToolUse { input, id, name } => tool_calls.push(canonical::ToolCall {
                id: id.unwrap_or_default(),
                name: name.unwrap_or_default(),
                args: input,
            }),
            ContentBlock::Other => {}
        }
    }
    canonical::ChatResponse {
        text: if texts.is_empty() { None } else { Some(texts.join("\n")) },
        tool_calls,
        finish,
        usage,
    }
}
