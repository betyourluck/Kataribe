//! 提案者の抽象。ターンループを実 LLM から切り離す境界 (依存性逆転)。
//!
//! 本番は [`LlmClient`] が実装し、テストは scripted な fake が StateDelta 列を返す。
//! これで「却下→再生成」ロジックを実 API 無しで決定論的に検証できる。

use gm_core::StateDelta;
use llm_client::{ChatMessage, LlmClient};

use crate::error::HarnessError;

/// messages から 1 ターン分の [`StateDelta`] を提案する。
///
/// ループ側が messages (GM persona / 盤面 / 却下理由) を組み立てて渡す。
/// 提案者は「構造化出力を返す」責務だけを負い、裁定はしない。
#[allow(async_fn_in_trait)] // 本 crate 内でしか実装/消費しないため dyn 化の懸念なし
pub trait DeltaProposer {
    async fn propose(&self, messages: &[ChatMessage]) -> Result<StateDelta, HarnessError>;
}

/// 本番実装: クラウド LLM に tool-use 強制で StateDelta を出させる。
///
/// ネットワーク経路のため単体テスト対象外 (llm_client と同じ線引き)。
/// 実 API 通しは「実クラウド通しプレイ」フェーズで検証する。
impl DeltaProposer for LlmClient {
    async fn propose(&self, messages: &[ChatMessage]) -> Result<StateDelta, HarnessError> {
        let delta = self.generate_delta(messages.to_vec()).await?;
        Ok(delta)
    }
}
