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
mod save;
mod turn;

pub use asset::{resolve_asset, AssetKind};
pub use campaign::{
    advance_campaign, advance_campaign_injected, load_campaign, load_module, load_module_injected,
    Advance, Campaign, CampaignEdge, CampaignMemory, ModuleId,
};
pub use package::{
    inject_package, is_campaign_entry, load_campaign_package, load_package, read_manifest, Globals,
    LoadedCampaignPackage, LoadedPackage, PackageManifest, PlayerDef,
};
pub use error::HarnessError;
pub use loader::{inject_cast, load_characters};
pub use memoria::{load_lore, resolve_recall, FiredBeat, LoreStore, Memoria, MemoryFragment};
pub use proposer::DeltaProposer;
pub use save::{load_session, save_session, SavedContent, SessionSave, SAVE_VERSION};
pub use turn::{carryover_narration, chronicle_entry, run_turn, TurnLog, TurnOutcome};

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

        let outcome = run_turn(&p, &mut s, &sc, "引き出しを調べる", 3, Lang::Ja, &[], &[], "", &[]).await.unwrap();
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

        let outcome = run_turn(&p, &mut s, &sc, "鍵を探す", 3, Lang::Ja, &[], &[], "", &[]).await.unwrap();
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

        run_turn(&p, &mut s, &sc, "鍵を探す", 3, Lang::Ja, &[], &[], "", &[]).await.unwrap();

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

        let outcome = run_turn(&p, &mut s, &sc, "力ずくで脱出する", 3, Lang::Ja, &[], &[], "", &[]).await.unwrap();
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

        let outcome = run_turn(&p, &mut s, &sc, "聞き耳を立てる", 3, Lang::Ja, &[], &[], "", &[]).await.unwrap();
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

        run_turn(&p, &mut s, &sc, "暖炉を見つめる", 3, Lang::Ja, &lore, &[], "", &[]).await.unwrap();

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

        run_turn(&p, &mut s, &sc, "話しかける", 3, Lang::Ja, &[], &[], prev, &[]).await.unwrap();

        let prompt_text = p.seen_text(1);
        assert!(prompt_text.contains("直前までの語り"), "継続の見出しが prompt に載る");
        assert!(prompt_text.contains("モカが振り向いて微笑んだ"), "直前の語り本文が注入される");
        assert!(prompt_text.contains("繰り返さない") || prompt_text.contains("再び描写しない"), "繰り返し禁止を指示する");
    }

    /// 【経緯ログ / chronicle】過去ターンの要約列が「これまでの経緯」として prompt に載り、
    /// GM が数ターン前の経過を保持する (recent_narration=直前 1 ターンの中期記憶版。
    /// 「経過を忘れる GM」の対策)。TurnOutcome は GM 自身が書いた summary を運び、
    /// 蓄積は呼び出し側 (CLI/app) が chronicle_entry で行う。
    #[tokio::test]
    async fn chronicle_history_is_woven_into_prompt_and_summary_carried() {
        let sc = scenario();
        let mut s = fresh(&sc);
        let mut d0 = delta(vec![StateOp::SetFlag { key: "drawer_opened".into(), value: true }]);
        d0.summary = "机の引き出しをこじ開けた".into();
        let p = ScriptedProposer::new(vec![d0]);
        let history = vec![
            TurnLog { turn: 1, player: "部屋を見回す".into(), summary: "古びた書斎を調べ始めた".into() },
            TurnLog { turn: 2, player: "机に近づく".into(), summary: "机上の蝋燭に火を灯した".into() },
        ];

        let outcome = run_turn(&p, &mut s, &sc, "引き出しを調べる", 3, Lang::Ja, &[], &[], "", &history)
            .await
            .unwrap();

        let text = p.seen_text(1);
        assert!(text.contains("# これまでの経緯"), "経緯の見出しが prompt に載る: {text}");
        assert!(
            text.contains("古びた書斎を調べ始めた") && text.contains("蝋燭に火を灯した"),
            "過去ターンの要約が古い順に注入される: {text}"
        );
        match outcome {
            TurnOutcome::Accepted { summary, .. } => {
                assert_eq!(summary, "机の引き出しをこじ開けた", "GM の書いた summary を運ぶ")
            }
            _ => panic!("受理されるはず"),
        }
    }

    /// 経緯が無い初回ターンは経緯ブロックを注入しない (ノイズを足さない)。
    #[tokio::test]
    async fn empty_history_means_no_chronicle_block() {
        let sc = scenario();
        let mut s = fresh(&sc);
        let p = ScriptedProposer::new(vec![delta(vec![StateOp::SetFlag {
            key: "drawer_opened".into(),
            value: true,
        }])]);
        run_turn(&p, &mut s, &sc, "見回す", 3, Lang::Ja, &[], &[], "", &[]).await.unwrap();
        // GM_SYSTEM は summary の説明で『これまでの経緯』に言及するので、注入見出し (#) で判定する。
        assert!(!p.seen_text(1).contains("# これまでの経緯"), "経緯なしなら注入しない");
    }

    /// 【弱モデル fallback】summary を書かないモデルでも経緯が残るよう、chronicle_entry は
    /// narration 冒頭へフォールバックする (文字境界安全な切り詰め)。summary があればそのまま。
    #[test]
    fn chronicle_entry_falls_back_to_truncated_narration() {
        let long = "夕暮れの書斎。埃をかぶった机の引き出しに手をかけると、軋みながら開いた。".repeat(5);
        let e = chronicle_entry(3, "引き出しを開ける", "", &long, &[]);
        assert!(e.summary.chars().count() <= 81, "narration 冒頭へ切り詰める (…込み)");
        assert!(e.summary.starts_with("夕暮れの書斎"), "冒頭から取る");
        assert_eq!(e.turn, 3);
        assert_eq!(e.player, "引き出しを開ける");

        let e2 = chronicle_entry(4, "話す", "アリスに約束を打ち明けた", &long, &[]);
        assert_eq!(e2.summary, "アリスに約束を打ち明けた", "summary があればそのまま");
    }

    /// 【発火ビートの GM 還流】トリガーの authored narration はプレイヤーには表示されるが
    /// **GM は見ていない** (発火は GM の提案後に engine 側で起きる) — 次ターンの継続文脈
    /// (carryover_narration) と経緯ログ (chronicle_entry の beats) の両方に併記して、
    /// GM が筋書きの出来事と噛み合った語りを続けられるようにする (#27 のトリガー版)。
    #[test]
    fn fired_beats_flow_into_carryover_and_chronicle() {
        let beats = vec!["石壁が轟音とともに崩れ、隠し通路が現れた。".to_string()];

        // 継続文脈: GM の語り + 筋書きの出来事の連結。
        let carry = carryover_narration("祭壇に手を触れると、微かに紋様が光った。", &beats);
        assert!(carry.contains("紋様が光った"), "GM の語りが残る");
        assert!(carry.contains("筋書きの出来事"), "ビートが筋書きの出来事として連結される");
        assert!(carry.contains("隠し通路が現れた"), "ビート本文が入る");
        // ビートが無ければそのまま (余計なマーカーを足さない)。
        assert_eq!(carryover_narration("素の語り", &[]), "素の語り");

        // 経緯ログ: summary にビートを併記 (GM の summary はビートを知らないため)。
        let e = chronicle_entry(5, "祭壇に触れる", "祭壇に触れて紋様が光った", "（語り）", &beats);
        assert!(e.summary.contains("祭壇に触れて紋様が光った"), "GM の summary が残る");
        assert!(
            e.summary.contains("出来事") && e.summary.contains("隠し通路"),
            "ビートが出来事として併記される: {}",
            e.summary
        );
    }

    /// 【パース失敗の self-repair 結線 (#40)】壊れた構造化出力 (LlmError::Parse) は却下と
    /// 同じく「raw + 修正指示を戻して再提出」させる — 従来はターンが丸ごとエラーで蒸発した
    /// (Gemini 実測: `"ops": "\n"` で 9 ターン中 4 ターン消失)。raw 保持 (llm_client #4) の
    /// 「再生成の燃料」がここで初めて燃える。
    #[tokio::test]
    async fn parse_failure_is_fed_back_and_retried() {
        struct FlakyProposer {
            calls: Mutex<u32>,
        }
        impl DeltaProposer for FlakyProposer {
            async fn propose(
                &self,
                messages: &[ChatMessage],
            ) -> Result<StateDelta, HarnessError> {
                let mut n = self.calls.lock().unwrap();
                *n += 1;
                if *n == 1 {
                    // 1 回目: 壊れた JSON (実測の ops 文字列崩れを模す)。
                    let bad = r#"{"narration":"x","ops":"@@"}"#;
                    let source = serde_json::from_str::<StateDelta>(bad).unwrap_err();
                    return Err(HarnessError::Proposer(llm_client::LlmError::Parse {
                        source,
                        raw: bad.to_string(),
                    }));
                }
                // 2 回目: 修正指示が messages に積まれていることを確認してから正しい提案。
                let fed_back = messages
                    .iter()
                    .any(|m| m.content.contains("JSON として壊れていて読めなかった"));
                assert!(fed_back, "raw+修正指示が還流されている");
                Ok(StateDelta::new("直した", vec![]))
            }
        }

        let sc = scenario();
        let mut state = fresh(&sc);
        let p = FlakyProposer { calls: Mutex::new(0) };
        let out = run_turn(&p, &mut state, &sc, "調べる", 3, Lang::Ja, &[], &[], "", &[])
            .await
            .expect("パース失敗はエラーでなく再生成で回復する");
        match out {
            TurnOutcome::Accepted { narration, attempts, .. } => {
                assert_eq!(narration, "直した");
                assert_eq!(attempts, 2, "1 回目=壊れ、2 回目=修復");
            }
            other => panic!("Accepted であるべき: {other:?}"),
        }
    }

    /// 【いま開いている投票の動的 surfacing (#37)】静的な規則 (scenario_brief) + 一般義務
    /// (GM_SYSTEM #32) だけでは、絞られた局面 (夜の狩り) で票が出ない事象が実測で再発した
    /// (信頼度 ~1/3: gnosia 1周目✗ → 修正 → 2周目○ → vampire ✗)。第三層として state_brief が
    /// **条件が真になっている vote_rule** を現在形で surface — 「いま票を出せる者」を生存・
    /// 属性で絞った**名前列挙**にし、このターンの義務として接地する。
    #[test]
    fn state_brief_surfaces_open_votes_with_eligible_names() {
        let sc = Scenario::from_yaml(concat!(
            "title: t\nstart: v\n",
            "allowed_flags: [投票フェーズ, 夜フェーズ]\n",
            "vote_rules:\n",
            "  - when: { kind: flag_is, key: 投票フェーズ, value: true }\n",
            "  - when: { kind: flag_is, key: 夜フェーズ, value: true }\n",
            "    voter_attribute: { key: 役職, value: 人狼 }\n",
            "initial_attributes: { 役職: 村人 }\n",
            "characters:\n",
            "  alice: { name: アリス, attributes: { 役職: 人狼 } }\n",
            "  bob: { name: ボブ, attributes: { 役職: 村人 } }\n",
            "locations: { v: { description: d, present: [alice, bob], items: {}, exits: [] } }\n",
            "goal: { kind: always }\n"
        ))
        .unwrap();
        let mut s = sc.initial_state(1);

        // どの規則も条件が偽 → 動的節は出ない (GM_SYSTEM の「節が無ければ出すな」と対)。
        let brief = prompt::state_brief(&s, &sc);
        assert!(!brief.contains("いま投票が開いている"), "フェーズ外では出ない: {brief}");

        // 夜: 人狼 (アリス) だけが列挙される。村人 (ボブ) は出ず、player (村人) の促し節も出ない。
        s.flags.insert("夜フェーズ".into(), true);
        let brief = prompt::state_brief(&s, &sc);
        let line = brief
            .lines()
            .find(|l| l.contains("いま投票が開いている"))
            .expect("夜は動的節が出る");
        assert!(line.contains("アリス (alice)"), "投票できる者を名前 (id) で列挙: {line}");
        assert!(!line.contains("ボブ"), "投票できない者は列挙しない: {line}");
        assert!(
            line.contains("cast_vote") && line.contains("必ず"),
            "NPC 分はこのターンの義務として接地: {line}"
        );
        assert!(!line.contains("促せ"), "player に投票権が無ければ促し節は出ない: {line}");

        // 昼: 生存者なら誰でも → NPC は義務列挙、player は**代行禁止 + 未指名なら促し** (#39)。
        s.flags.insert("夜フェーズ".into(), false);
        s.flags.insert("投票フェーズ".into(), true);
        let brief = prompt::state_brief(&s, &sc);
        let line = brief.lines().find(|l| l.contains("いま投票が開いている")).unwrap();
        assert!(
            line.contains("player") && line.contains("アリス") && line.contains("ボブ"),
            "昼は生存者全員が現れる: {line}"
        );
        assert!(
            line.contains("代行") && line.contains("促せ"),
            "player の票は代行禁止・未指名なら narration で促す: {line}"
        );
        // 実測 (vampire seed 8 run3): GM が吸血シーンを丸ごと語ったのに cast_vote を出さず、
        // 物語と正本が乖離した。「描写だけでは起きていない」を player 節にも明記する。
        assert!(
            line.contains("描写するだけでは"),
            "narration 描写だけでは票にならないことを player 節でも接地: {line}"
        );

        // player が投票済みなら促さない (受領済みの明示)。
        s.votes.insert("player".into(), "alice".into());
        let brief = prompt::state_brief(&s, &sc);
        let line = brief.lines().find(|l| l.contains("いま投票が開いている")).unwrap();
        assert!(line.contains("受領済み"), "投票済みなら促しでなく受領を示す: {line}");
        assert!(!line.contains("促せ"), "投票済みで促さない: {line}");
        s.votes.clear();

        // 死者は列挙しない: 人狼が全滅した夜は該当者ゼロ = 節ごと出ない。
        s.flags.insert("投票フェーズ".into(), false);
        s.flags.insert("夜フェーズ".into(), true);
        s.entities.entry("alice".into()).or_default().insert("生存".into(), 0);
        let brief = prompt::state_brief(&s, &sc);
        assert!(
            !brief.contains("いま投票が開いている"),
            "票を出せる生存者がいなければ節は出ない: {brief}"
        );

        // GM_SYSTEM が動的節を合図として結びつける。
        assert!(
            prompt::GM_SYSTEM.contains("いま投票が開いている"),
            "GM_SYSTEM が動的節を義務の合図として言及する"
        );
    }

    /// 【セーブ / ロード (spec 07 Phase A)】進行中セッションの正本 (state: rng カーソル・
    /// votes・present_overrides・flags 込み) と語りの継続性 (chronicle/last_narration/
    /// pending_*) が YAML 1 file を roundtrip して同値に戻る。骨格は保存しない (content 参照
    /// のみ)。版不一致のセーブは**黙って壊れた再開をせず**拒否する。
    #[test]
    fn session_save_roundtrips_state_and_carryovers() {
        let sc = scenario();
        let mut state = fresh(&sc);
        // 進行中らしい状態を作る (どの可変状態も丸ごと運ばれることの見本)。
        state.turn = 7;
        state.flags.insert("door_open".into(), true);
        state.votes.insert("mira".into(), "yuren".into());
        state.present_overrides.insert("alice".into(), false);
        let _ = state.rng.roll(20); // rng カーソルを進める (出目まで再現の証拠)

        let save = SessionSave {
            version: SAVE_VERSION,
            content: SavedContent::Package { path: "packages/gnosia_village".into() },
            package_version: "0.1.0".into(),
            module: None,
            state: state.clone(),
            campaign_memory: CampaignMemory::new(),
            history: vec![TurnLog { turn: 1, player: "見回す".into(), summary: "六人が集った".into() }],
            last_narration: "霧が窓を這う。".into(),
            pending_checks: vec![],
            pending_lore: vec![],
        };
        let path = std::env::temp_dir().join("kataribe_poc_session_save.yaml");
        save_session(&path, &save).expect("保存できる");
        let loaded = load_session(&path).expect("読める");
        assert_eq!(loaded.state, state, "正本が丸ごと同値で戻る (rng カーソル込み)");
        assert_eq!(loaded.history.len(), 1, "chronicle が戻る");
        assert_eq!(loaded.last_narration, "霧が窓を這う。", "継続性が戻る");
        assert!(matches!(loaded.content, SavedContent::Package { ref path } if path.contains("gnosia")));

        // 版不一致は拒否 (v1 は実験的 — 黙って壊れた再開をしない)。
        let mut old = save.clone();
        old.version = 999;
        save_session(&path, &old).expect("保存はできる");
        assert!(load_session(&path).is_err(), "版不一致はロード拒否");
        std::fs::remove_file(&path).ok();
    }

    /// 【経緯の予算】history_note は文字予算内で新しい方を残し、古い方から省略する
    /// (省略した旨も明示)。無限に伸びて prompt を食い潰さない。
    #[test]
    fn history_note_respects_budget_drops_oldest() {
        let history: Vec<TurnLog> = (1..=200)
            .map(|i| TurnLog {
                turn: i,
                player: format!("行動{i}"),
                summary: format!("ターン{i}の出来事があった。廊下を歩き、扉を確かめ、灯りを整えた。"),
            })
            .collect();
        let note = prompt::history_note(&history);
        assert!(note.contains("ターン200の出来事"), "最新は必ず残る");
        assert!(!note.contains("ターン1の出来事"), "最古は予算で落ちる");
        assert!(note.contains("省略"), "省略した旨を明示する");
    }

    /// GM_SYSTEM が「summary に経緯 1 行を書け」を刷り込む (書かれなければ経緯が残らない)。
    #[test]
    fn gm_system_demands_turn_summary() {
        let g = prompt::GM_SYSTEM;
        assert!(g.contains("summary"), "summary の記述義務が刷り込まれる");
        assert!(g.contains("経緯") || g.contains("要約"), "経緯の 1 行要約であることを説明する");
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
        run_turn(&p, &mut s, &sc, "見回す", 3, Lang::Ja, &[], &[], "", &[]).await.unwrap();
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

        run_turn(&p, &mut s, &sc, "周囲を見回す", 3, Lang::Ja, &[], &[], "", &[]).await.unwrap();
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

        run_turn(&p, &mut s, &sc, "扉をこじ開ける", 3, Lang::Ja, &[], &checks, "", &[]).await.unwrap();
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

    /// 【判定の射程 / 山場の保護】GM_SYSTEM が「態勢では振らない」「1 回の判定を決着へ飛躍させない」
    /// 「大敵の撃破は authored 条件でのみ・ゴール未達は未発生」を刷り込む (魔王あっけなく撃破の対策)。
    #[test]
    fn gm_system_grounds_dice_timing_and_no_one_shot_climax() {
        let s = prompt::GM_SYSTEM;
        assert!(s.contains("態勢"), "構える/身構えるは態勢であって決着でない旨を刷り込む");
        assert!(s.contains("決着へ飛躍") || s.contains("決着へ飛躍させてはならない"), "1 回の判定を山場の決着へ拡大させない");
        assert!(
            s.contains("ゴール未達") && s.contains("まだ起きていない"),
            "engine 未記録の決着 (ゴール未達) は未発生 = ungrounded な大敵撃破を narration に書かせない"
        );
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

        let sb = prompt::state_brief(&s, &sc);
        assert!(sb.contains("cell"), "現在地が含まれる");
    }

    /// 【内部 stat の秘匿 (spec 04 追補)】hidden_stats に挙げた帳簿 stat (タイマー等) は
    /// state_brief の能力値行に出ない (主人公の可視ステータスを汚さない)。
    #[test]
    fn state_brief_hides_hidden_stats() {
        let sc = Scenario::from_yaml(concat!(
            "title: t\nstart: room\n",
            "initial_stats: { hp: 10, x_turn: 0 }\n",
            "hidden_stats: [x_turn]\n",
            "goal: { kind: always }\n",
            "locations:\n  room: { description: d, items: {}, exits: [] }\n"
        ))
        .unwrap();
        let s = sc.initial_state(1);
        let sb = prompt::state_brief(&s, &sc);
        assert!(sb.contains("hp=10"), "可視 stat は出る");
        assert!(!sb.contains("x_turn"), "内部タイマー stat は隠れる: {sb}");
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

    /// 【知識フラグの surfacing / spec 03】flag_hints が scenario_brief に出て、GM_SYSTEM が
    /// 「条件が満たされた瞬間に set_flag」を刷り込む (下流 gate に出ないフラグも LLM に可視化)。
    #[test]
    fn scenario_brief_surfaces_flag_hints_and_gm_system_demands_setting() {
        let sc = Scenario::from_yaml(concat!(
            "title: t\nstart: room\n",
            "allowed_flags: [知った_鍵の在処]\n",
            "flag_hints: { 知った_鍵の在処: プレイヤーが賢者から鍵の在処を聞いたら立てる }\n",
            "locations:\n  room: { description: d, items: {}, exits: [] }\n",
            "goal: { kind: always }\n"
        ))
        .unwrap();
        let brief = prompt::scenario_brief(&sc);
        assert!(brief.contains("状態フラグ"), "状態フラグ節が出る");
        assert!(
            brief.contains("知った_鍵の在処") && brief.contains("賢者から鍵の在処を聞いたら"),
            "フラグ名とヒントが surface される: {brief}"
        );
        assert!(
            prompt::GM_SYSTEM.contains("状態フラグ") && prompt::GM_SYSTEM.contains("set_flag"),
            "GM_SYSTEM が条件成立時の set_flag を刷り込む"
        );
        assert!(
            prompt::GM_SYSTEM.contains("先回り") || prompt::GM_SYSTEM.contains("満たしていない"),
            "早まった set_flag を戒める (flag_rules バックストップと対)"
        );
    }

    /// 【使えるフラグの語彙提示】scenario_brief が「LLM が set_flag してよいフラグ」
    /// (= allowed − authored 専権) を表示名・ヒント付きで列挙する。語彙の閉集合を見せる
    /// ことで幻フラグの発明 (却下ループの素) を断ち、authored 専権 (トリガー/challenge が
    /// 立てるフラグ) は見せない (先取り set_flag の誘惑を作らない)。state_brief の
    /// 「立っている状態」にも表示名を添える。
    #[tokio::test]
    async fn scenario_brief_lists_usable_flags_with_titles_excluding_authored() {
        let sc = Scenario::from_yaml(concat!(
            "title: t\nstart: room\n",
            "allowed_flags: [聞いた_在処, 罠が作動]\n",
            "flag_titles: { 聞いた_在処: 鍵の在処の知識 }\n",
            "flag_hints: { 聞いた_在処: 賢者から在処を聞いたら立てる }\n",
            "triggers:\n",
            "  - id: trap\n",
            "    when: { kind: flag_is, key: 聞いた_在処, value: true }\n",
            "    effects: [{ op: set_flag, key: 罠が作動, value: true }]\n",
            "locations:\n  room: { description: d, items: {}, exits: [] }\n",
            "goal: { kind: always }\n"
        ))
        .unwrap();
        let brief = prompt::scenario_brief(&sc);
        assert!(brief.contains("状態フラグ"), "フラグ語彙の節が出る: {brief}");
        assert!(
            brief.contains("聞いた_在処") && brief.contains("鍵の在処の知識") && brief.contains("賢者から在処を聞いたら"),
            "使えるフラグが id+表示名+ヒント付きで列挙される: {brief}"
        );
        // authored 専権フラグは節に出ない (先取り set_flag の誘惑を作らない)。
        // ※ trigger の定義自体は brief に出ないので、含まれていなければ節にも無い。
        assert!(!brief.contains("罠が作動"), "authored 専権フラグは語彙に出さない: {brief}");

        // state_brief の「立っている状態」にも表示名が添えられる (id は ops 用に残す)。
        let mut s = sc.initial_state(1);
        gm_core::apply(
            &mut s,
            &sc,
            &gm_core::StateDelta::new("", vec![StateOp::SetFlag { key: "聞いた_在処".into(), value: true }]),
        )
        .unwrap();
        let sb = prompt::state_brief(&s, &sc);
        assert!(
            sb.contains("聞いた_在処（鍵の在処の知識）"),
            "立っている状態に表示名が添えられる: {sb}"
        );
    }

    /// 【内部フラグの秘匿 (hidden_flags)】帳簿フラグ (`x_done` 等) は state_brief の
    /// 「立っている状態」にも scenario_brief の語彙節にも出ない (提示層が一切出さない =
    /// `hidden_stats` と同じ扱い)。gate/トリガーの評価は不変で効く。
    #[test]
    fn hidden_flags_are_skipped_by_prompt_layers() {
        let sc = Scenario::from_yaml(concat!(
            "title: t\nstart: room\n",
            "allowed_flags: [x_done, 知った_合図]\n",
            "hidden_flags: [x_done]\n",
            "flag_hints: { 知った_合図: 合図を教わったら立てる }\n",
            "locations:\n  room: { description: d, items: {}, exits: [] }\n",
            "goal: { kind: always }\n"
        ))
        .unwrap();
        // 語彙節: 見えるフラグは列挙され、隠しフラグは出ない。
        let brief = prompt::scenario_brief(&sc);
        assert!(brief.contains("知った_合図"), "見えるフラグは語彙に出る: {brief}");
        assert!(!brief.contains("x_done"), "隠しフラグは語彙に出ない: {brief}");

        // state_brief: 両方 true でも隠しフラグは「立っている状態」に出ない。
        let mut s = sc.initial_state(1);
        gm_core::apply(
            &mut s,
            &sc,
            &gm_core::StateDelta::new(
                "",
                vec![
                    StateOp::SetFlag { key: "x_done".into(), value: true },
                    StateOp::SetFlag { key: "知った_合図".into(), value: true },
                ],
            ),
        )
        .unwrap();
        let sb = prompt::state_brief(&s, &sc);
        assert!(sb.contains("知った_合図"), "見えるフラグは載る: {sb}");
        assert!(!sb.contains("x_done"), "隠しフラグは載らない: {sb}");
    }

    /// 【投票の prompt 接地 (spec 06 Phase D)】engine が CastVote を受理できても、GM が
    /// 「いま誰が投票できるか・票は op で出す」を知らなければ実プレイで使われない
    /// (challenge の実プレイ surfacing と同じギャップ)。scenario_brief が vote_rules を
    /// 平易な日本語で列挙し、GM_SYSTEM が「投票の局面では生存 NPC 全員分の票を
    /// cast_vote で並べよ / 開票はあなたが起こせない」を刷り込む。
    #[test]
    fn scenario_brief_surfaces_vote_rules_and_gm_system_grounds_voting() {
        let sc = Scenario::from_yaml(concat!(
            "title: t\nstart: v\n",
            "allowed_flags: [投票フェーズ, 夜フェーズ]\n",
            "role_assignment: { key: 役職, pool: { 人狼: 1, 村人: 1 }, among: [player, alice] }\n",
            "vote_rules:\n",
            "  - when: { kind: flag_is, key: 投票フェーズ, value: true }\n",
            "  - when: { kind: flag_is, key: 夜フェーズ, value: true }\n",
            "    voter_attribute: { key: 役職, value: 人狼 }\n",
            "characters: { alice: { name: A } }\n",
            "locations: { v: { description: d, items: {}, exits: [] } }\n",
            "goal: { kind: always }\n"
        ))
        .unwrap();
        let brief = prompt::scenario_brief(&sc);
        assert!(brief.contains("## 投票"), "投票の節が出る: {brief}");
        assert!(
            brief.contains("投票フェーズ") && brief.contains("誰でも"),
            "voter 条件なしの rule は「誰でも」: {brief}"
        );
        assert!(
            brief.contains("役職=人狼"),
            "voter_attribute 付きの rule は条件を surface: {brief}"
        );

        let g = prompt::GM_SYSTEM;
        assert!(g.contains("cast_vote"), "GM_SYSTEM が cast_vote の使用を義務化する");
        assert!(
            g.contains("全員分") || g.contains("並べ"),
            "投票の局面で NPC 全員分の票を並べる規律"
        );
        assert!(
            g.contains("開票") && (g.contains("起こせない") || g.contains("筋書き")),
            "開票は GM が起こせないことを刷り込む"
        );
        // Phase E 実測 (2026-07-04, gemini-flash): 初夜に人狼の票が一つも出ず狩りが不発した。
        // 「誰が投票できるか」(権利) だけでは足りず「出さなければ何も起きない」(義務) の接地が要る。
        assert!(
            g.contains("投票できる者が生きているなら") && g.contains("出さなければ"),
            "投票権が絞られた局面 (夜の狩り等) でも該当者の票を必ず出す規律"
        );
        // 実プレイ #35: 上の義務化が過修正になり、投票機構の無い盤面 (合コン) でも弱モデルが
        // cast_vote を出した。義務は「## 投票 の節がある盤面」に明示スコープする (無ければ禁止)。
        assert!(
            g.contains("『## 投票』の節が無ければ") && g.contains("一切"),
            "投票の無い盤面では cast_vote を一切出さないスコープ規律 (#35)"
        );
        // 実プレイ #38: NPC の票がプレイヤーの票に同調し、player の指名先がほぼ毎回処刑される
        // (GM はプレイヤーの行動文を見てから NPC の票を決めるため引きずられやすい)。
        assert!(
            g.contains("引きずられ") && g.contains("割れる"),
            "NPC の票はプレイヤーの票から独立に決める規律 (#38)"
        );
    }

    /// 【秘匿属性の prompt 接地 (spec 06 Phase B)】GM は secret 属性を全員分見る (ゲームを
    /// 回すのに必要) が、**秘匿情報である注記**が添えられ、GM_SYSTEM が演じ分け規律
    /// (互いに知らない/地の文で明かさない/役職能力の結果は当人だけの知識) を刷り込む。
    #[tokio::test]
    async fn state_brief_marks_secret_attributes_and_gm_system_grounds_secrecy() {
        let sc = Scenario::from_yaml(concat!(
            "title: t\nstart: v\n",
            "role_assignment: { key: 役職, pool: { 人狼: 1, 村人: 1 }, among: [player, alice] }\n",
            "secret_attributes: [役職]\n",
            "characters: { alice: { name: A } }\n",
            "locations: { v: { description: d, items: {}, exits: [] } }\n",
            "goal: { kind: always }\n"
        ))
        .unwrap();
        let s = sc.initial_state(1);
        let sb = prompt::state_brief(&s, &sc);
        assert!(sb.contains("役職="), "GM には secret 属性が全員分見える: {sb}");
        assert!(sb.contains("〔秘匿〕"), "secret 属性に秘匿注記が付く: {sb}");

        let g = prompt::GM_SYSTEM;
        assert!(g.contains("秘匿"), "GM_SYSTEM が秘匿情報の扱いを刷り込む");
        assert!(
            g.contains("互いに知らない") || g.contains("自分の分だけ"),
            "登場人物どうしは互いに知らない前提の演じ分けを刷り込む"
        );
        assert!(g.contains("地の文"), "地の文で明かさない規律を刷り込む");
        // Phase E 実測 (2026-07-04, gemini-flash): 夜の襲撃シーンを実行者視点の地の文で描き、
        // 「仮面を脱ぎ捨てたミラは獲物の部屋へ」と人狼の正体を開示した。隠密行動は結果だけを語る。
        assert!(
            g.contains("隠密") && g.contains("結果だけ"),
            "秘匿属性に基づく隠密行動 (夜の襲撃等) は実行者を伏せ、結果だけを描く規律"
        );
    }

    /// 【presence の prompt 接地】GM は「いま誰がこの場に居るか」を presence でしか知れない —
    /// 場所説明文は静的 (退場後もキャラが書かれたまま) なので、`state_brief` が**実効 presence**
    /// (base ± override) を毎ターン surface し、GM_SYSTEM が「一覧が唯一の真実 (説明文より優先)・
    /// 一覧に無いキャラを出すな・居るキャラを無視するな」を刷り込む。UI (顔アイコン行) にしか
    /// 出ていなかった穴 (人を減らしても語りから消えない/増やしても居ないと扱われる) を塞ぐ。
    #[test]
    fn state_brief_surfaces_effective_presence_and_gm_system_grounds_it() {
        let sc = Scenario::from_yaml(concat!(
            "title: t\nstart: room\n",
            "characters:\n  alice: { name: アリス }\n  bob: { name: ボブ }\n",
            "locations:\n",
            "  room: { description: カウンターの奥にアリスが立っている, present: [alice], exits: [] }\n",
            "goal: { kind: always }\n"
        ))
        .unwrap();
        let mut s = sc.initial_state(1);
        let brief = prompt::state_brief(&s, &sc);
        assert!(brief.contains("この場にいる"), "presence 節が毎ターン出る: {brief}");
        assert!(brief.contains("アリス"), "base presence の alice が出る: {brief}");
        assert!(!brief.contains("ボブ"), "居ない bob は出ない: {brief}");

        // 退場/登場の override が反映される (静的な説明文と食い違っても一覧が真実)。
        s.present_overrides.insert("alice".into(), false);
        s.present_overrides.insert("bob".into(), true);
        let brief = prompt::state_brief(&s, &sc);
        assert!(brief.contains("ボブ"), "登場した bob が出る: {brief}");
        assert!(!brief.contains("アリス"), "退場した alice は出ない: {brief}");

        // 誰もいない場合はその旨を明示 (空欄でなく)。
        s.present_overrides.insert("bob".into(), false);
        let brief = prompt::state_brief(&s, &sc);
        assert!(brief.contains("誰もいない"), "無人はその旨を明示: {brief}");

        // GM_SYSTEM の刷り込み: 一覧が唯一の真実・説明文より優先・勝手に出し入れしない。
        let g = prompt::GM_SYSTEM;
        assert!(g.contains("この場にいる"), "GM_SYSTEM が presence 節を参照する");
        assert!(
            g.contains("説明文") && (g.contains("登場させ") || g.contains("居ない")),
            "説明文より一覧が真実・一覧外のキャラを出すなを刷り込む"
        );
    }

    /// 【アイテム取得様式の prompt 接地】fixed (備え付け) は「取得不可・その場で使える」を、
    /// infinite (自販機等) は「何度でも取れる」を scenario_brief が先回りで GM に教える
    /// (却下される前に防ぐ prompt 層。engine 側の却下は gm_core の PoC が固定)。
    #[test]
    fn scenario_brief_surfaces_item_take_modes() {
        let sc = Scenario::from_yaml(concat!(
            "title: t\nstart: room\n",
            "locations:\n",
            "  room:\n",
            "    description: d\n",
            "    items:\n",
            "      ジュース: { when: { kind: always }, take: infinite }\n",
            "      シャワー: { take: fixed }\n",
            "      鍵: { kind: always }\n",
            "    exits: []\n",
            "goal: { kind: always }\n"
        ))
        .unwrap();
        let brief = prompt::scenario_brief(&sc);
        assert!(
            brief.contains("シャワー") && brief.contains("備え付け") && brief.contains("その場で使える"),
            "fixed は取得不可とその場で使えることを surface: {brief}"
        );
        assert!(
            brief.contains("ジュース") && brief.contains("何度でも取れる"),
            "infinite は何度でも取れることを surface: {brief}"
        );
        assert!(brief.contains("鍵"), "旧形式 (once) も従来どおり列挙される: {brief}");
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
