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
    /// xAI Grok の推論制御 (spec 12 Phase D)。**対象モデル (grok-4.3/4.5) には既定で送る**
    /// (opt-out) — 未送出だと xAI 側の既定 (4.3=low 常時思考 / 4.5=high) が適用され、
    /// 思考が max_tokens を食い潰して空デルタ/タイムアウトになる (grok-4.3 実測の真因仮説)。
    /// 他モデル/他サーバには送らない (None = キーごと省略)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<&'static str>,
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
    /// 使用量。キャッシュ計測 (`prompt_tokens_details.cached_tokens`) の一次ソース (#45)。
    /// OpenAI / xAI / Gemini 互換が返す。無い・形が違うサーバでも壊れない (default/Option)。
    #[serde(default)]
    pub usage: Option<ChatUsage>,
}

/// OpenAI 互換の usage。`prompt_tokens_details.cached_tokens` > 0 = プレフィックスが
/// キャッシュから読まれた (xAI 84% 引き / OpenAI 50% 引きの対象)。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChatUsage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PromptTokensDetails {
    #[serde(default)]
    pub cached_tokens: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Choice {
    pub message: ResponseMessage,
    /// 終了理由 (`stop`/`tool_calls`/`length`/...)。canonical `Finish` の材料
    /// (empty-response 防御 spec 12 Phase D)。返さないサーバでも壊れない。
    #[serde(default)]
    pub finish_reason: Option<String>,
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
    /// 呼び出し ID。canonical `ToolCall.id` に運ぶ (返さないサーバは空扱い)。
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FunctionCallResponse {
    /// **JSON 文字列** (オブジェクトではない)。canonical への decode 境界で 1 回だけ parse する
    /// (写経元 D2)。
    #[serde(default)]
    pub arguments: String,
    /// 関数名。単一ツール強制 (emit_delta) では分岐に使わないが canonical へ運ぶ。
    #[serde(default)]
    pub name: Option<String>,
}

