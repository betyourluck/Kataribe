//! 構造化出力の抽出。tool-use が主経路、フェンス JSON がフォールバック。
//!
//! LocalAI `llm_client.py::generate_json` (```json フェンス除去) と
//! `orchestrator.py::_strip_code_fence` の堅牢性を継承する。

use serde::de::DeserializeOwned;

use crate::error::LlmError;
use crate::wire::ResponseMessage;

/// ASCII の needle を大小無視で探し、haystack 内の **バイト位置**を返す。
/// needle が ASCII なので一致位置は必ず char 境界 (UTF-8 継続バイトは >= 0x80)。
fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    let (h, n) = (haystack.as_bytes(), needle.as_bytes());
    if n.is_empty() || h.len() < n.len() {
        return None;
    }
    (0..=h.len() - n.len()).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

/// 推論モデルが構造化出力の前に吐く chain-of-thought ブロックを中身ごと除去する。
///
/// Gemma の `<thought>` / DeepSeek・Qwen の `<think>` 等 (大小無視)。no-tools モードで
/// CoT に JSON 断片 (`{"op":...}`) が混じると本体抽出を妨げる (`<thought>` がフェンスの前に
/// 来るので [`strip_code_fence`] が効かず、first `{` が断片に釣られる) ため、抽出前に掃除する。
/// 終了タグが無ければ開始タグ以降を全て CoT とみなして切る。
pub fn strip_reasoning_blocks(raw: &str) -> String {
    let mut out = raw.to_string();
    for tag in ["think", "thought", "thinking"] {
        let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
        while let Some(s) = find_ci(&out, &open) {
            let end = match find_ci(&out[s..], &close) {
                Some(rel) => s + rel + close.len(),
                None => out.len(),
            };
            out.replace_range(s..end, "");
        }
    }
    out
}

/// 文字列中の top-level な `{...}` (バランスした波括弧) を出現順に返す。
/// JSON 文字列値の中の波括弧・エスケープは数えない (string-aware)。波括弧は ASCII なので
/// スライス境界は常に char 境界。
fn json_objects(s: &str) -> Vec<&str> {
    let (mut objs, mut depth, mut start) = (Vec::new(), 0usize, 0usize);
    let (mut in_str, mut escaped) = (false, false);
    for (i, &b) in s.as_bytes().iter().enumerate() {
        if in_str {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => {
                if depth == 0 {
                    start = i;
                }
                depth += 1;
            }
            b'}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    objs.push(&s[start..=i]);
                }
            }
            _ => {}
        }
    }
    objs
}

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
        return from_str_lenient::<T>(raw).map_err(|source| LlmError::Parse {
            source,
            raw: raw.clone(),
        });
    }

    if let Some(content) = message.content.as_deref() {
        // 推論モデルの CoT ブロックを先に落とす (no-tools モードの堅牢化)。
        let content = strip_reasoning_blocks(content);
        if !content.trim().is_empty() {
            let cleaned = strip_code_fence(&content);
            // まず素直にパース。失敗したら prose に包まれた JSON を balanced な `{...}` から救済する。
            // StateDelta は serde(default) で空 object すら通るので、**最後の** object を採る
            // (答えは推論の後に来る = 前置きの断片でなく本体を拾う)。
            return match from_str_lenient::<T>(&cleaned) {
                Ok(v) => Ok(v),
                Err(source) => json_objects(&content)
                    .into_iter()
                    .rev()
                    .find_map(|obj| from_str_lenient::<T>(obj).ok())
                    .ok_or(LlmError::Parse { source, raw: cleaned }),
            };
        }
    }

    Err(LlmError::NoStructuredOutput)
}

/// JSON テキストを `T` へ。素直な from_str が失敗したら、実 LLM で観測した崩れ形を
/// **決定論的に**直してから再試行する (#28/#30 と同族のソース後処理、#40):
/// - `"ops"` が**文字列** (Gemini が `"ops": "\n"` や JSON 配列を二重エンコードした文字列を
///   出す) → 空白のみなら `[]`、JSON 配列としてパースできればその配列に差し替える。
///
/// 失敗時は**最初の** serde エラーを返す (崩れの一次症状を診断に残す)。
fn from_str_lenient<T: DeserializeOwned>(raw: &str) -> Result<T, serde_json::Error> {
    let first = match serde_json::from_str::<T>(raw) {
        Ok(v) => return Ok(v),
        Err(e) => e,
    };
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Err(first);
    };
    let Some(obj) = value.as_object_mut() else {
        return Err(first);
    };
    let Some(ops_str) = obj.get("ops").and_then(|v| v.as_str()) else {
        return Err(first);
    };
    let fixed = if ops_str.trim().is_empty() {
        serde_json::Value::Array(Vec::new())
    } else {
        match serde_json::from_str::<serde_json::Value>(ops_str.trim()) {
            Ok(arr @ serde_json::Value::Array(_)) => arr,
            _ => return Err(first),
        }
    };
    obj.insert("ops".to_string(), fixed);
    serde_json::from_value::<T>(value).map_err(|_| first)
}
