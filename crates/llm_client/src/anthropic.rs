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

use crate::wire::{ChatMessage, FunctionCallResponse, ResponseMessage, Role, ToolCallResponse};

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

/// [`ChatMessage`] 列からネイティブリクエストを組み立てる。
///
/// - 先頭の連続 system → `system` ブロック配列 (末尾に cache_control)。
/// - 先頭以外の system (万一混じった場合) → user に降格 (ネイティブは先頭 system のみ)。
/// - `tool` = `Some((name, description, schema))` なら tools + tool_choice 強制。
pub(crate) fn build_request(
    model: &str,
    max_tokens: u32,
    temperature: Option<f32>,
    messages: Vec<ChatMessage>,
    tool: Option<(String, String, Value)>,
) -> MessagesRequest {
    let mut system: Vec<SystemBlock> = Vec::new();
    let mut turns: Vec<TurnMessage> = Vec::new();
    for m in messages {
        match m.role {
            Role::System if turns.is_empty() => system.push(SystemBlock {
                kind: "text",
                text: m.content,
                cache_control: None,
            }),
            // 先頭以外の system は user へ降格 (壊さない)。Role::Tool は現状未使用だが同様に降格。
            Role::System | Role::Tool | Role::User => turns.push(TurnMessage {
                role: "user",
                content: m.content,
            }),
            Role::Assistant => turns.push(TurnMessage {
                role: "assistant",
                content: m.content,
            }),
        }
    }
    // 安定プレフィックスの末尾に breakpoint。可変な turns 側には置かない
    // (毎ターン別内容 → 読まれないキャッシュ書込 1.25× の無駄になるだけ)。
    if let Some(last) = system.last_mut() {
        last.cache_control = Some(CacheControl::ephemeral());
    }

    let (tools, tool_choice) = match tool {
        Some((name, description, schema)) => (
            vec![ToolDef {
                name: name.clone(),
                description,
                input_schema: schema,
            }],
            Some(ToolChoice { kind: "tool", name }),
        ),
        None => (Vec::new(), None),
    };

    MessagesRequest {
        model: model.to_string(),
        max_tokens,
        temperature,
        system,
        messages: turns,
        tools,
        tool_choice,
    }
}

// --- レスポンス ---------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MessagesResponse {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub usage: Option<Usage>,
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

impl MessagesResponse {
    /// OpenAI 形の [`ResponseMessage`] へ写す — 既存 [`crate::parse::extract`] に合流させ、
    /// tool_use 主経路 + content フォールバック (救済含む) の単一抽出経路を保つ。
    pub(crate) fn into_response_message(self) -> ResponseMessage {
        let mut texts: Vec<String> = Vec::new();
        let mut tool_calls: Vec<ToolCallResponse> = Vec::new();
        for block in self.content {
            match block {
                ContentBlock::Text { text } => texts.push(text),
                ContentBlock::ToolUse { input } => tool_calls.push(ToolCallResponse {
                    function: FunctionCallResponse {
                        arguments: input.to_string(),
                    },
                }),
                ContentBlock::Other => {}
            }
        }
        ResponseMessage {
            content: if texts.is_empty() {
                None
            } else {
                Some(texts.join("\n"))
            },
            tool_calls,
        }
    }
}
