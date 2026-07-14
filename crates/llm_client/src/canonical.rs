//! プロバイダ中立の canonical モデル (spec 12 Phase A)。
//!
//! Driver 相当 ([`crate::LlmClient::generate`] / [`crate::LlmClient::generate_structured`]) は
//! この型だけを組み、各 adapter (openai_compat / anthropic) が encode/decode 純関数で
//! wire と相互変換する。**Driver は wire 形を一切見ない** (写経元 §4 の adapter 契約)。
//! 翻訳マトリクスの正本は specs/12_unified_tool_layer.md §6 / data_contract `UnifiedToolLayer` 節。

use serde_json::Value;

use crate::config::Effort;
use crate::wire::ChatMessage;

/// プロバイダ中立のリクエスト。
#[derive(Debug, Clone)]
pub(crate) struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
    pub tool_choice: ToolChoice,
    /// 明示設定時のみ送る (None なら provider 既定。新しめモデルは送ると 400)。
    pub temperature: Option<f32>,
    pub max_tokens: u32,
    /// 推論の深さ (spec 12 Phase B)。**None なら送らない** (opt-in)。方言への写像は adapter:
    /// Claude = `thinking: adaptive` + `output_config.effort` / Grok (Phase D) = `reasoning_effort`。
    pub effort: Option<Effort>,
}

/// ツール定義。`parameters` は JSON Schema (schemars 機械生成の単一真実源)。
#[derive(Debug, Clone)]
pub(crate) struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// ツール選択。Kataribe v1 は `None` (generate) と `Specific` (emit_delta 強制) のみ使う。
/// Auto/Required は canonical の語彙として保持 (将来の Driver/Registry 拡張で型を変えないため)。
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum ToolChoice {
    None,
    Auto,
    Required,
    Specific(String),
}

/// 応答終了理由。`Length` は empty-response 防御 (spec 12 Phase D) の判定材料 —
/// 推論モデルが budget を思考に使い切った空応答の一次シグナル。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Finish {
    Stop,
    ToolUse,
    Length,
    Other,
}

/// プロバイダ中立の usage。`cache_read` は CacheStat (GUI キャッシュ健全性警告 #44/#45) の
/// 一次ソースで、adapter が各 wire の該当フィールドから正規化する。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Usage {
    #[allow(dead_code)]
    pub prompt: u64,
    #[allow(dead_code)]
    pub completion: u64,
    pub cache_read: u64,
}

/// ツール呼び出し。`args` は **必ず JSON オブジェクト** (写経元 D2) —
/// OpenAI 系の「arguments は JSON 文字列」は adapter の decode 境界で 1 回だけ parse する。
#[derive(Debug, Clone)]
pub(crate) struct ToolCall {
    /// プロバイダが返さなければ空文字。Gemini adapter (Phase C) は client 単位の
    /// 単調カウンタから `call_{seq}_{index}` を合成して埋める (rev4・Must 4)。
    #[allow(dead_code)]
    pub id: String,
    /// 単一ツール強制 (emit_delta) では分岐に使わないが、canonical としては運ぶ。
    #[allow(dead_code)]
    pub name: String,
    pub args: Value,
}

/// プロバイダ中立の応答。
#[derive(Debug, Clone)]
pub(crate) struct ChatResponse {
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    #[allow(dead_code)]
    pub finish: Finish,
    pub usage: Usage,
}
