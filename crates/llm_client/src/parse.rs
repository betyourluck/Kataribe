//! 構造化出力の抽出。tool-use が主経路、フェンス JSON がフォールバック。
//!
//! LocalAI `llm_client.py::generate_json` (```json フェンス除去) と
//! `orchestrator.py::_strip_code_fence` の堅牢性を継承する。

use serde::de::DeserializeOwned;

use crate::error::LlmError;
use crate::wire::ResponseMessage;

/// ```/```json フェンスを剥がす。フェンスが無ければそのまま返す。
pub fn strip_code_fence(raw: &str) -> String {
    let trimmed = raw.trim();
    if !trimmed.starts_with("```") {
        return trimmed.to_string();
    }
    // 先頭・末尾の ``` 行を落とす (言語指定 ```json も含む)。
    let lines: Vec<&str> = trimmed
        .lines()
        .filter(|l| !l.trim_start().starts_with("```"))
        .collect();
    lines.join("\n").trim().to_string()
}

/// 応答 message から `T` を取り出す。
///
/// 1. `tool_calls` があれば最初の関数の `arguments` (JSON 文字列) を `T` にパース。
/// 2. 無ければ `content` をフェンス除去して `T` にパース (フォールバック)。
/// 3. どちらも無ければ [`LlmError::NoStructuredOutput`]。
///
/// パース失敗時は **raw を保持した** [`LlmError::Parse`] を返す ── 再生成プロンプトに添えるため。
pub fn extract<T: DeserializeOwned>(message: &ResponseMessage) -> Result<T, LlmError> {
    if let Some(call) = message.tool_calls.first() {
        let raw = &call.function.arguments;
        return serde_json::from_str::<T>(raw).map_err(|source| LlmError::Parse {
            source,
            raw: raw.clone(),
        });
    }

    if let Some(content) = message.content.as_deref() {
        if !content.trim().is_empty() {
            let cleaned = strip_code_fence(content);
            return serde_json::from_str::<T>(&cleaned).map_err(|source| LlmError::Parse {
                source,
                raw: cleaned,
            });
        }
    }

    Err(LlmError::NoStructuredOutput)
}
