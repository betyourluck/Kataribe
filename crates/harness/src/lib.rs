//! # harness — GM ターンループ
//!
//! 三権分立を 1 ターンに結線する脚: **LLM が提案し (`llm_client`)、エンジンが裁き (`gm_core`)**。
//! 提案 → 裁定 → 却下なら理由を戻して再生成 → 受理なら原子適用。
//! LocalAI `orchestrator.py::_self_repair_loop` と同型。
//!
//! ループは [`DeltaProposer`] trait に対して書かれており、実 LLM ([`llm_client::LlmClient`]) と
//! テスト用 scripted fake を差し替えられる。これで「却下→再生成」の正しさを実 API 無しで実証する。

mod error;
pub mod prompt;
mod proposer;
mod turn;

pub use error::HarnessError;
pub use proposer::DeltaProposer;
pub use turn::{run_turn, TurnOutcome};

// =============================================================================
// PoC: GM ターンループの実証 (Red→Green)
// 実 LLM の代わりに台本付き提案者を差し込み、「正本 > 文章力」のループ版を固める。
// 嘘の op は却下され、理由を戻すと正しく直る ── これが勝ち筋の最小証明。
// =============================================================================
#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use gm_core::{GameState, Lang, Scenario, StateDelta, StateOp};
    use llm_client::ChatMessage;

    use super::*;

    const LOCKED_ROOM: &str = include_str!("../../../scenarios/locked_room.yaml");

    fn scenario() -> Scenario {
        Scenario::from_yaml(LOCKED_ROOM).expect("locked_room.yaml がパースできること")
    }
    fn fresh(sc: &Scenario) -> GameState {
        GameState::new(sc.start.clone(), 42)
    }
    fn delta(ops: Vec<StateOp>) -> StateDelta {
        StateDelta::new("（語り）", ops)
    }

    /// 台本どおりに StateDelta を返す fake 提案者。渡された messages を記録する
    /// (却下理由が再生成プロンプトに積まれることを検証するため)。
    struct ScriptedProposer {
        scripted: Mutex<VecDeque<StateDelta>>,
        seen: Mutex<Vec<Vec<ChatMessage>>>,
    }
    impl ScriptedProposer {
        fn new(deltas: Vec<StateDelta>) -> Self {
            Self {
                scripted: Mutex::new(deltas.into()),
                seen: Mutex::new(Vec::new()),
            }
        }
        fn call_count(&self) -> usize {
            self.seen.lock().unwrap().len()
        }
        /// n 回目 (1-origin) の propose に渡された messages を結合した文字列。
        fn seen_text(&self, n: usize) -> String {
            let seen = self.seen.lock().unwrap();
            seen[n - 1].iter().map(|m| m.content.clone()).collect::<Vec<_>>().join("\n")
        }
    }
    impl DeltaProposer for ScriptedProposer {
        async fn propose(&self, messages: &[ChatMessage]) -> Result<StateDelta, HarnessError> {
            self.seen.lock().unwrap().push(messages.to_vec());
            self.scripted
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| HarnessError::NoProposal("台本が尽きた".into()))
        }
    }

    /// 【一発合格】合法な提案はそのまま受理され、state に適用される。
    #[tokio::test]
    async fn accepts_legal_delta_in_one_shot() {
        let sc = scenario();
        let mut s = fresh(&sc);
        let p = ScriptedProposer::new(vec![delta(vec![StateOp::SetFlag {
            key: "drawer_opened".into(),
            value: true,
        }])]);

        let outcome = run_turn(&p, &mut s, &sc, "引き出しを調べる", 3, Lang::Ja).await.unwrap();
        match outcome {
            TurnOutcome::Accepted { attempts, .. } => assert_eq!(attempts, 1),
            other => panic!("受理されるべき: {other:?}"),
        }
        assert!(s.flag("drawer_opened"), "受理された op は適用される");
        assert_eq!(s.turn, 1);
    }

    /// 【却下→再生成】幻のアイテムは却下され、続く合法提案で受理される (self_repair の核)。
    #[tokio::test]
    async fn regenerates_after_rejection() {
        let sc = scenario();
        let mut s = fresh(&sc);
        let p = ScriptedProposer::new(vec![
            // 1回目: 存在しない master_key を掴もうとする嘘 → 却下
            delta(vec![StateOp::AddItem { item: "master_key".into() }]),
            // 2回目: 正しい一手 → 受理
            delta(vec![StateOp::SetFlag { key: "drawer_opened".into(), value: true }]),
        ]);

        let outcome = run_turn(&p, &mut s, &sc, "鍵を探す", 3, Lang::Ja).await.unwrap();
        match outcome {
            TurnOutcome::Accepted { attempts, .. } => assert_eq!(attempts, 2, "2回目で受理"),
            other => panic!("最終的に受理されるべき: {other:?}"),
        }
        assert!(s.flag("drawer_opened"));
        assert_eq!(s.turn, 1, "受理は 1 回だけ");
        assert_eq!(p.call_count(), 2, "提案は 2 回呼ばれた");
    }

    /// 【却下理由の還流】2回目の propose に、1回目の却下理由が積まれている。
    #[tokio::test]
    async fn rejection_reasons_are_fed_back() {
        let sc = scenario();
        let mut s = fresh(&sc);
        let p = ScriptedProposer::new(vec![
            delta(vec![StateOp::AddItem { item: "master_key".into() }]),
            delta(vec![StateOp::SetFlag { key: "drawer_opened".into(), value: true }]),
        ]);

        run_turn(&p, &mut s, &sc, "鍵を探す", 3, Lang::Ja).await.unwrap();

        let second = p.seen_text(2);
        assert!(second.contains("却下"), "再生成プロンプトに却下の文脈があるはず");
        assert!(
            second.contains("存在しない"),
            "master_key が存在しない旨の却下理由が還流しているはず"
        );
    }

    /// 【原子性 × ループ】最大試行まで嘘を続けると Rejected で終わり、state は無傷。
    #[tokio::test]
    async fn exhausts_retries_and_leaves_state_intact() {
        let sc = scenario();
        let mut s = fresh(&sc);
        let p = ScriptedProposer::new(vec![
            delta(vec![StateOp::AddItem { item: "master_key".into() }]),
            delta(vec![StateOp::Move { to: "corridor".into() }]), // 解錠前で却下
            delta(vec![StateOp::AddItem { item: "rusty_key".into() }]), // 引き出し前で却下
        ]);

        let outcome = run_turn(&p, &mut s, &sc, "力ずくで脱出する", 3, Lang::Ja).await.unwrap();
        match outcome {
            TurnOutcome::Rejected { attempts, last_reasons } => {
                assert_eq!(attempts, 3);
                assert!(!last_reasons.is_empty());
            }
            other => panic!("却下され続けるべき: {other:?}"),
        }
        assert_eq!(s.turn, 0, "却下のみなら turn は進まない");
        assert_eq!(s.location, "cell", "却下のみなら移動しない");
        assert!(s.inventory.is_empty(), "却下のみなら所持品は増えない");
    }

    /// 【ダイス経路】request_roll はループを通ってエンジンが振り、結果が返る (決定論)。
    #[tokio::test]
    async fn dice_roll_flows_through_turn() {
        let sc = scenario();
        let mut s = fresh(&sc); // seed=42, cursor=0
        let p = ScriptedProposer::new(vec![delta(vec![StateOp::RequestRoll { sides: 20, dc: 10 }])]);

        let outcome = run_turn(&p, &mut s, &sc, "聞き耳を立てる", 3, Lang::Ja).await.unwrap();
        match outcome {
            TurnOutcome::Accepted { rolls, .. } => {
                assert_eq!(rolls.len(), 1);
                assert!((1..=20).contains(&rolls[0].result));
                assert_eq!(rolls[0].success, rolls[0].result >= 10);
            }
            other => panic!("ダイス要求自体は合法: {other:?}"),
        }
        assert_eq!(s.rng.cursor, 1, "エンジンが 1 回振ったので cursor が進む");
    }

    /// 【プロンプト健全性】盤面要約にシナリオ要素が、状態要約に現在地が含まれる。
    #[test]
    fn prompt_reflects_scenario_and_state() {
        let sc = scenario();
        let s = fresh(&sc);
        let brief = prompt::scenario_brief(&sc);
        assert!(brief.contains("密室脱出"), "タイトルが含まれる");
        assert!(brief.contains("corridor"), "出口先の場所が含まれる");
        assert!(brief.contains("rusty_key"), "取得可能アイテムが含まれる");

        let sb = prompt::state_brief(&s);
        assert!(sb.contains("cell"), "現在地が含まれる");
    }
}
