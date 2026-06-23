//! OpenAI 互換 chat/completions のワイヤ型 (request / response)。
//!
//! ここは **LLM との境界の唯一の真実**。壊れるのはこの ser/de なので、PoC テストで固める。
//! tool-use 強制で構造化出力 (`emit_delta` 関数の arguments) を取り出すのが主経路。

use serde::{Deserialize, Serialize};

/// メッセージ役割。`tool` ロールは将来のツール結果返却用 (現状未使用)。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// 送信メッセージ。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: Role::System, content: content.into() }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: Role::User, content: content.into() }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: Role::Assistant, content: content.into() }
    }
}

// --- リクエスト ---------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    /// 明示設定時のみ送る。新しめのモデル (例: claude-opus-4-8) は temperature を
    /// 非対応にしており、送ると 400 を返す。未設定 (None) なら provider 既定に委ねる。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
}

/// 関数ツール定義。`parameters` は schemars 生成の JSON Schema (gm_core が単一真実源)。
#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    #[serde(rename = "type")]
    pub kind: ToolKind,
    pub function: FunctionDef,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Function,
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// 特定関数の呼び出しを強制する (`{"type":"function","function":{"name":...}}`)。
#[derive(Debug, Clone, Serialize)]
pub struct ToolChoice {
    #[serde(rename = "type")]
    pub kind: ToolKind,
    pub function: ToolChoiceFunction,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolChoiceFunction {
    pub name: String,
}

impl ToolChoice {
    pub fn force(name: impl Into<String>) -> Self {
        Self {
            kind: ToolKind::Function,
            function: ToolChoiceFunction { name: name.into() },
        }
    }
}

// --- レスポンス ---------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct ChatResponse {
    #[serde(default)]
    pub choices: Vec<Choice>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Choice {
    pub message: ResponseMessage,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResponseMessage {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCallResponse>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolCallResponse {
    pub function: FunctionCallResponse,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FunctionCallResponse {
    /// **JSON 文字列** (オブジェクトではない)。これを再パースして StateDelta にする。
    /// (`name` は `tool_choice` で単一ツールを強制しているため分岐に使わず、受信しても無視する。)
    #[serde(default)]
    pub arguments: String,
}

impl ChatResponse {
    /// 最初の choice の message を取り出す。
    pub fn first_message(&self) -> Option<&ResponseMessage> {
        self.choices.first().map(|c| &c.message)
    }
}
