//! ターンループの失敗型。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum HarnessError {
    /// 提案者 (LLM) 側の失敗 (接続不能・構造化出力不能・パース失敗など)。
    #[error("提案者エラー: {0}")]
    Proposer(#[from] llm_client::LlmError),

    /// テスト用 scripted 提案者の台本が尽きた等。
    #[error("提案が得られない: {0}")]
    NoProposal(String),

    /// 外部キャラ定義ファイルの読み込み/パース失敗。
    #[error("キャラ定義の読み込み失敗 ({path}): {detail}")]
    CharacterLoad { path: String, detail: String },
}
