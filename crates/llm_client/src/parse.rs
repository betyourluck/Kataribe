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

/// LLM が narration の本文に紛れ込ませた tool-call マークアップを除去する。
///
/// 強モデルでも稀に narration の**文字列値**へ `</narration>` や `<parameter name="ops">` 等の
/// 構造化出力 format token を漏らす (実プレイで観測。ops 配列自体は別フィールドで正常)。
/// tool_call は valid JSON なので [`extract`] はエラーにせず素通り → narration に混入が残る。
/// narration は非検証 (LLM の領分) ゆえ、**提示前にここで掃除**する (提示層の `\n` 正規化と
/// 同じ「正本を汚さない後処理」)。先頭の開きタグは剥がし、構造マークアップ以降は切り捨てる。
pub fn sanitize_narration(s: &str) -> String {
    let mut t = s.trim();
    // 先頭が開きタグで始まる稀ケースを剥がす。
    for open in ["<narration>", "<parameter name=\"narration\">", "<parameter name='narration'>"] {
        if let Some(rest) = t.strip_prefix(open) {
            t = rest.trim_start();
        }
    }
    // 構造マークアップが現れたら、そこ以降は漏れた構造とみなして切る (最初の出現で切断)。
    const CUT: &[&str] = &[
        "</narration>",
        "<narration>",
        "</parameter>",
        "<parameter name=",
        "<parameter",
        "<function_calls>",
        "</function_calls>",
        "<invoke",
        "</invoke>",
    ];
    let cut = CUT.iter().filter_map(|m| t.find(m)).min().unwrap_or(t.len());
    t[..cut].trim().to_string()
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
            // まず素直にパース。失敗したら prose に包まれた JSON を first '{'..last '}' で救済する
            // (no-tools モードでモデルが前置きを付けても拾える堅牢性)。
            return match serde_json::from_str::<T>(&cleaned) {
                Ok(v) => Ok(v),
                Err(source) => extract_json_object(&cleaned)
                    .and_then(|obj| serde_json::from_str::<T>(obj).ok())
                    .ok_or(LlmError::Parse { source, raw: cleaned }),
            };
        }
    }

    Err(LlmError::NoStructuredOutput)
}

/// prose に包まれた JSON を救済する: 最初の `{` から最後の `}` までを返す (no-tools モード堅牢化)。
fn extract_json_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let end = s.rfind('}')?;
    (end > start).then(|| &s[start..=end])
}
