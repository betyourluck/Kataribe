//! # harness — GM ターンループ
//!
//! 三権分立を 1 ターンに結線する脚: **LLM が提案し (`llm_client`)、エンジンが裁き (`gm_core`)**。
//! 提案 → 裁定 → 却下なら理由を戻して再生成 → 受理なら原子適用。
//! LocalAI `orchestrator.py::_self_repair_loop` と同型。
//!
//! ループは [`DeltaProposer`] trait に対して書かれており、実 LLM ([`llm_client::LlmClient`]) と
//! テスト用 scripted fake を差し替えられる。これで「却下→再生成」の正しさを実 API 無しで実証する。

mod asset;
mod campaign;
mod error;
mod loader;
mod memoria;
mod package;
pub mod prompt;
mod proposer;
mod turn;

pub use asset::{resolve_asset, AssetKind};
pub use campaign::{
    advance_campaign, load_campaign, load_module, Advance, Campaign, CampaignEdge, ModuleId,
};
pub use package::{
    inject_package, load_package, read_manifest, Globals, LoadedPackage, PackageManifest, PlayerDef,
};
pub use error::HarnessError;
pub use loader::{inject_cast, load_characters};
pub use memoria::{load_lore, resolve_recall, FiredBeat, LoreStore, Memoria, MemoryFragment};
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

    const LOCKED_ROOM: &str = include_str!("../fixtures/locked_room.yaml");

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

        let outcome = run_turn(&p, &mut s, &sc, "引き出しを調べる", 3, Lang::Ja, &[], &[], "").await.unwrap();
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

        let outcome = run_turn(&p, &mut s, &sc, "鍵を探す", 3, Lang::Ja, &[], &[], "").await.unwrap();
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

        run_turn(&p, &mut s, &sc, "鍵を探す", 3, Lang::Ja, &[], &[], "").await.unwrap();

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

        let outcome = run_turn(&p, &mut s, &sc, "力ずくで脱出する", 3, Lang::Ja, &[], &[], "").await.unwrap();
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

        let outcome = run_turn(&p, &mut s, &sc, "聞き耳を立てる", 3, Lang::Ja, &[], &[], "").await.unwrap();
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

    /// 【memoria_bridge 結線】トリガー発火 (engine) → recall cue → Memoria が伏線を返す、の
    /// 端から端まで。可変状態は正本 (GameState) に在り、Memoria は伏線のみ持つ (境界の実証)。
    #[test]
    fn trigger_fire_bridges_to_recalled_lore() {
        use gm_core::apply;

        const TRIGGER_RECALL: &str = include_str!("../fixtures/trigger_recall.yaml");
        let sc = Scenario::from_yaml(TRIGGER_RECALL).unwrap();
        let mut s = sc.initial_state(7);

        // 好感度を閾値 30 まで上げる → recall_promise が発火 (cue=childhood_promise を運ぶ)。
        let out = apply(
            &mut s,
            &sc,
            &StateDelta::new(
                "",
                vec![StateOp::AdjustStat {
                    entity: "alice".into(),
                    key: "好感度".into(),
                    delta: 30,
                }],
            ),
        )
        .expect("好感度上昇は合法");
        assert!(out.fired.iter().any(|f| f.id == "recall_promise"));

        // 発火列の cue を Memoria で解決 → 伏線が語りに載る。
        let store = load_lore(std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/memoria"
        )))
        .unwrap();
        let beats = resolve_recall(&store, &out.fired);
        let promise = beats.iter().find(|b| b.id == "recall_promise").unwrap();
        assert!(!promise.recalled.is_empty(), "発火点で伏線が recall される");

        // 境界: 可変の真実 (好感度) は正本にあり、Memoria の伏線は不変 lore のみ。
        assert_eq!(s.stat_of("alice", "好感度"), 30, "数値の真実は engine が握る");
        assert!(
            promise.recalled[0].text.contains("樫の木") || promise.recalled[0].text.contains("小指"),
            "Memoria が返すのは伏線 (可変状態ではない)"
        );
    }

    /// 【memoria_bridge 注入】recall された伏線が、次ターンの提案プロンプトに「思い出された記憶」
    /// として載る (ナレーターが語りに織り込めるようになる、輪の閉じ)。
    #[tokio::test]
    async fn recalled_lore_is_woven_into_prompt() {
        let sc = scenario();
        let mut s = fresh(&sc);
        let p = ScriptedProposer::new(vec![delta(vec![StateOp::SetFlag {
            key: "drawer_opened".into(),
            value: true,
        }])]);
        let lore = vec![MemoryFragment {
            id: "childhood_promise".into(),
            tags: vec![],
            text: "丘の上の古い樫の木の下で、二人は小指を絡めて誓った。".into(),
        }];

        run_turn(&p, &mut s, &sc, "暖炉を見つめる", 3, Lang::Ja, &lore, &[], "").await.unwrap();

        let prompt_text = p.seen_text(1);
        assert!(prompt_text.contains("思い出された記憶"), "想起の見出しが prompt に載る");
        assert!(prompt_text.contains("樫の木"), "伏線の本文が prompt に注入される");
        assert!(prompt_text.contains("ops には書かない"), "状態変更でない旨の境界指示が載る");
    }

    /// 【継続性の注入】直前の語りが次ターンの prompt に「続く情景」として載り、既出描写の
    /// 繰り返し禁止が指示される (情景がくどく二度出る問題の対策)。空なら注入しない。
    #[tokio::test]
    async fn recent_narration_is_woven_into_prompt_for_continuity() {
        let sc = scenario();
        let mut s = fresh(&sc);
        let p = ScriptedProposer::new(vec![delta(vec![StateOp::SetFlag {
            key: "drawer_opened".into(),
            value: true,
        }])]);
        let prev = "夕日が差し込む教室。モカが振り向いて微笑んだ。";

        run_turn(&p, &mut s, &sc, "話しかける", 3, Lang::Ja, &[], &[], prev).await.unwrap();

        let prompt_text = p.seen_text(1);
        assert!(prompt_text.contains("直前までの語り"), "継続の見出しが prompt に載る");
        assert!(prompt_text.contains("モカが振り向いて微笑んだ"), "直前の語り本文が注入される");
        assert!(prompt_text.contains("繰り返さない") || prompt_text.contains("再び描写しない"), "繰り返し禁止を指示する");
    }

    /// 直前の語りが無い (初回ターン等) なら継続ブロックを注入しない。
    #[tokio::test]
    async fn no_recent_narration_means_no_continuity_block() {
        let sc = scenario();
        let mut s = fresh(&sc);
        let p = ScriptedProposer::new(vec![delta(vec![StateOp::SetFlag {
            key: "drawer_opened".into(),
            value: true,
        }])]);
        run_turn(&p, &mut s, &sc, "見回す", 3, Lang::Ja, &[], &[], "").await.unwrap();
        assert!(!p.seen_text(1).contains("直前までの語り"), "直前の語り無しなら注入しない");
    }

    /// 伏線が無いターンでは想起ブロックを注入しない (ノイズを足さない)。
    #[tokio::test]
    async fn no_lore_means_no_injection() {
        let sc = scenario();
        let mut s = fresh(&sc);
        let p = ScriptedProposer::new(vec![delta(vec![StateOp::SetFlag {
            key: "drawer_opened".into(),
            value: true,
        }])]);

        run_turn(&p, &mut s, &sc, "周囲を見回す", 3, Lang::Ja, &[], &[], "").await.unwrap();
        assert!(!p.seen_text(1).contains("思い出された記憶"), "伏線無しなら注入しない");
    }

    /// 【技能判定の還流】直前ターンの判定結果が次ターンの提案プロンプトに「直前の判定結果」
    /// として載る (出目は apply 後確定なので同一ターンに間に合わず、次ターンの語りに反映)。
    #[tokio::test]
    async fn check_result_is_fed_into_next_prompt() {
        use gm_core::CheckOutcome;
        let sc = scenario();
        let mut s = fresh(&sc);
        let p = ScriptedProposer::new(vec![delta(vec![StateOp::SetFlag {
            key: "drawer_opened".into(),
            value: true,
        }])]);
        let checks = vec![CheckOutcome {
            entity: "player".into(),
            stat: "str".into(),
            sides: 20,
            roll: 14,
            modifier: 3,
            total: 17,
            dc: 15,
            success: true,
            tier: None,
            narration: String::new(),
        }];

        run_turn(&p, &mut s, &sc, "扉をこじ開ける", 3, Lang::Ja, &[], &checks, "").await.unwrap();
        let prompt_text = p.seen_text(1);
        assert!(prompt_text.contains("直前の判定結果"), "判定結果の見出しが載る");
        assert!(prompt_text.contains("成功"), "成否が載る");
        assert!(prompt_text.contains("DC 15"), "DC が prompt に載る");
    }

    /// 【判定の後付け接地】判定結果の note は「なぜ成功/失敗したか」の物語内原因の後付けを要求し、
    /// 後付けの強さを接地する原料 (DC との差=margin、極=tier) を surface する (failures #26 の精密化)。
    #[test]
    fn check_note_demands_causal_reason_and_surfaces_margin_and_tier() {
        use gm_core::CheckOutcome;
        // 成功 (margin +6)。
        let win = vec![CheckOutcome {
            entity: "player".into(), stat: "話術".into(), sides: 20,
            roll: 18, modifier: 3, total: 21, dc: 15, success: true, tier: None,
            narration: String::new(),
        }];
        let note = prompt::check_outcome_note(&win);
        assert!(note.contains("なぜ"), "なぜその結果になったかの後付けを要求する");
        assert!(note.contains("原因"), "物語内の『原因』として語らせる");
        assert!(note.contains("DC を 6 上回った"), "成功 margin (+6) を surface する");

        // 失敗 (margin -3) + 極 (大失敗)。
        let fumble = vec![CheckOutcome {
            entity: "player".into(), stat: "str".into(), sides: 20,
            roll: 1, modifier: 2, total: 3, dc: 6, success: false, tier: Some("crit_fail".into()),
            narration: String::new(),
        }];
        let note2 = prompt::check_outcome_note(&fumble);
        assert!(note2.contains("DC に 3 届かなかった"), "失敗 margin (-3) を surface する");
        assert!(note2.contains("crit_fail"), "極 (tier) を surface して劇的な後付けを促す");
    }

    /// 【二重語り回避】authored 結末ナレーション付きの判定は同ターンに語られ済みなので、
    /// 次ターンの check_outcome_note から除外される (LLM に再描写させない)。narration 無しは還流する。
    #[test]
    fn check_note_skips_checks_with_authored_narration() {
        use gm_core::CheckOutcome;
        let mk = |narration: &str| CheckOutcome {
            entity: "player".into(), stat: "STR".into(), sides: 20,
            roll: 5, modifier: 0, total: 5, dc: 15, success: false,
            tier: None, narration: narration.into(),
        };
        // authored 文ありの判定だけ → note は空 (再描写不要)。
        assert!(prompt::check_outcome_note(&[mk("扉はびくともしない。")]).is_empty(),
            "authored narration 付きは LLM 還流から除外");
        // narration 無しの判定 → 従来どおり還流する。
        assert!(!prompt::check_outcome_note(&[mk("")]).is_empty(),
            "narration 無しは LLM に語らせるため還流する");
    }

    /// GM_SYSTEM が「判定結果の後付け（なぜ成功/失敗したか）」を刷り込む。
    #[test]
    fn gm_system_demands_post_hoc_reason_for_checks() {
        let s = prompt::GM_SYSTEM;
        assert!(s.contains("後付け"), "判定結果に理由を後付けする旨を刷り込む");
        assert!(s.contains("なぜ") && s.contains("原因"), "なぜ成功/失敗したかを物語内の原因として語らせる");
    }

    /// 【galge spine の機構】好感度の閾値トリガーが関係の「段」を刻み、名前付き goal に至る:
    /// 20→素を見せる、40→打ち明ける、50(+打ち明け)→告白。**インライン安定シナリオ**で機構を固定する
    /// (houkago の authored 内容は作者が随時いじるので、テストは配布コンテンツに依存させない)。
    #[test]
    fn galge_spine_fires_threshold_beats_and_reaches_named_goal() {
        use gm_core::apply;
        let sc = Scenario::from_yaml(concat!(
            "title: spine\nstart: room\nallowed_flags: [opened, confided]\n",
            "characters:\n  moka:\n    name: モカ\n    stats: { 好感度: { initial: 0, min: 0, max: 100 } }\n",
            "triggers:\n",
            "  - { id: opens_up, when: { kind: stat_at_least, entity: moka, key: 好感度, value: 20 },",
            "      effects: [ { op: set_flag, key: opened, value: true } ], narration: 素 }\n",
            "  - { id: confide, when: { kind: stat_at_least, entity: moka, key: 好感度, value: 40 },",
            "      effects: [ { op: set_flag, key: confided, value: true } ], narration: 打ち明け }\n",
            "goals:\n",
            "  - { id: confession, when: { kind: all, of: [ {kind: flag_is, key: confided, value: true},",
            "      {kind: stat_at_least, entity: moka, key: 好感度, value: 50} ] } }\n",
            "locations:\n  room: { description: d, items: {}, exits: [] }\n"
        ))
        .unwrap();
        assert!(sc.validate().is_empty());
        let mut s = sc.initial_state(1);
        let bump = |n: i64| {
            StateDelta::new(
                "",
                vec![StateOp::AdjustStat { entity: "moka".into(), key: "好感度".into(), delta: n }],
            )
        };

        let o1 = apply(&mut s, &sc, &bump(20)).unwrap();
        assert!(o1.fired.iter().any(|f| f.id == "opens_up"), "20 で素を見せるビート");
        assert_eq!(sc.reached(&s), None, "20 では未到達");

        let o2 = apply(&mut s, &sc, &bump(20)).unwrap();
        assert!(o2.fired.iter().any(|f| f.id == "confide"), "40 で打ち明けビート");
        assert_eq!(sc.reached(&s), None, "打ち明けただけでは未到達");

        apply(&mut s, &sc, &bump(10)).unwrap();
        assert_eq!(sc.reached(&s).as_deref(), Some("confession"), "打ち明けを経て50で告白");
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

    /// 【主人公の認識】world / protagonist が scenario_brief に surface され、GM_SYSTEM が
    /// 「NPC は主人公の設定に沿って接する」を刷り込む (教師なのにモカが認識しない問題の対策)。
    #[test]
    fn scenario_brief_surfaces_world_and_protagonist() {
        let sc = Scenario::from_yaml(concat!(
            "title: t\n",
            "world: 現代日本の高校。\n",
            "protagonist: { name: 先生, profile: 25才の高校教師。 }\n",
            "start: room\nallowed_flags: []\n",
            "locations:\n  room: { description: d, items: {}, exits: [] }\n",
            "goal: { kind: always }\n"
        ))
        .unwrap();
        let brief = prompt::scenario_brief(&sc);
        assert!(brief.contains("世界観") && brief.contains("現代日本の高校"), "world が surface される");
        assert!(
            brief.contains("主人公") && brief.contains("先生") && brief.contains("高校教師"),
            "主人公(プレイヤー)の設定が surface される"
        );
        assert!(
            prompt::GM_SYSTEM.contains("主人公の設定") && prompt::GM_SYSTEM.contains("教師"),
            "GM_SYSTEM が NPC の主人公認識を刷り込む"
        );
    }

    /// 【正本の接地 / 行商ネックレス対策】GM プロンプトは「行動文は意図」「所持品に無い物は
    /// 存在しない」「narration は非検証ゆえ GM 自身が矛盾を防ぐ」を刷り込む (failures #23)。
    /// narration には engine バックストップが無いので、この刷り込みが唯一の防衛線。
    #[test]
    fn gm_system_grounds_unowned_items() {
        let s = prompt::GM_SYSTEM;
        assert!(s.contains("意図"), "プレイヤー行動文が『意図』であることを明示する");
        assert!(
            s.contains("所持品リストに無い") || s.contains("手元に無い"),
            "未所持の物は存在しない旨を刷り込む"
        );
        assert!(
            s.contains("検証されない"),
            "narration は非検証=GM 自身が一貫性を守る旨を明示する"
        );
    }

    /// 【NPC 数値の接地】GM_SYSTEM が「数値変化は adjust_stat で起こす」「NPC 数値は entity 明示」を
    /// 刷り込む (好感度が上がらない = 数値を語りだけで済ます/entity 省略で player に当たり却下、の対策)。
    #[test]
    fn gm_system_grounds_numeric_stat_ops_and_entity() {
        let s = prompt::GM_SYSTEM;
        assert!(s.contains("adjust_stat"), "数値変化は adjust_stat op で起こす旨を刷り込む");
        assert!(s.contains("好感度"), "好感度を例に接地する");
        assert!(
            s.contains("entity を省略") || s.contains("entity にその NPC"),
            "NPC 数値は entity 明示 (省略すると主人公に当たる) 旨を刷り込む"
        );
    }
}
