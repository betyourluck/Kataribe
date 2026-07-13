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

    /// 伏線 lore ファイル (`memoria/*.yaml`) の読み込み/パース失敗。
    #[error("伏線の読み込み失敗 ({path}): {detail}")]
    LoreLoad { path: String, detail: String },

    /// Campaign file / モジュール scenario の読み込み・パース・整合性エラー (orchestration 層)。
    #[error("campaign の読み込み失敗 ({path}): {detail}")]
    CampaignLoad { path: String, detail: String },

    /// パッケージ (配布フォルダ) の読み込み・パース・整合性エラー (package.yaml / entry / 自己完結検査)。
    #[error("package の読み込み失敗 ({path}): {detail}")]
    PackageLoad { path: String, detail: String },

    /// セーブの書き込み失敗 (spec 07)。
    #[error("セーブの書き込み失敗 ({path}): {detail}")]
    SessionSave { path: String, detail: String },

    /// セーブの読み込み・パース・版不一致 (spec 07)。
    #[error("セーブの読み込み失敗 ({path}): {detail}")]
    SessionLoad { path: String, detail: String },

    /// あらすじ要約の失敗 (spec 10 — タイムアウト・空応答・API エラー)。
    /// 非致命: 呼び出し側は abandon してプレイを続ける (あふれ=次ターン再試行 / 遷移=範囲凍結)。
    #[error("あらすじ要約の失敗: {0}")]
    Summarize(String),
}
