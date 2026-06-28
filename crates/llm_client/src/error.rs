//! クライアントが返す失敗の型。`Parse` は **raw を保持する** ──
//! GM ターンループが却下理由と一緒に LLM へ戻して再生成させるため (self_repair 同型)。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum LlmError {
    /// 設定不備 (環境変数欠落など)。ネットワーク前に弾く。
    #[error("設定エラー: {0}")]
    Config(String),

    /// HTTP 層の失敗 (接続不能・タイムアウト・TLS など)。リトライ対象。
    /// `detail` は **source 連鎖を平坦化**した文面 (reqwest の "error sending request for url" は
    /// 真因をラップして隠すので、"…: operation timed out" 等の根本原因まで surface する)。
    #[error("HTTP エラー: {detail}")]
    Http {
        #[source]
        source: reqwest::Error,
        detail: String,
    },

    /// API がエラーステータスを返した。`status` でリトライ可否を判断する。
    #[error("API エラー (status={status}): {body}")]
    Api { status: u16, body: String },

    /// 応答に choices が無い / message が空。
    #[error("LLM が空の応答を返した")]
    EmptyResponse,

    /// tool-use を強制したのに tool_calls もフェンス JSON も得られなかった。
    #[error("構造化出力が得られなかった (tool_call 不在かつ content も JSON でない)")]
    NoStructuredOutput,

    /// JSON のパースに失敗。**`raw` を保持** して再生成プロンプトに添えられるようにする。
    #[error("構造化出力のパース失敗: {source}\n--- raw ---\n{raw}")]
    Parse {
        source: serde_json::Error,
        raw: String,
    },
}

/// reqwest エラーから [`LlmError::Http`] を作る。source 連鎖を平坦化して `detail` に詰める
/// ("error sending request for url (…)" の下に隠れた timeout/connect/TLS の真因まで見せる)。
impl From<reqwest::Error> for LlmError {
    fn from(source: reqwest::Error) -> Self {
        let mut detail = source.to_string();
        let mut cause: Option<&dyn std::error::Error> = std::error::Error::source(&source);
        while let Some(inner) = cause {
            detail.push_str(": ");
            detail.push_str(&inner.to_string());
            cause = inner.source();
        }
        LlmError::Http { source, detail }
    }
}

impl LlmError {
    /// 一過性 (リトライで回復しうる) か。HTTP 障害と 5xx / 429 を対象とする。
    pub fn is_transient(&self) -> bool {
        match self {
            LlmError::Http { source, .. } => {
                source.is_timeout() || source.is_connect() || source.is_request()
            }
            LlmError::Api { status, .. } => *status == 429 || (500..600).contains(status),
            _ => false,
        }
    }
}
