//! 正本の裁定者。LLM の提案を裁き、受理時のみ原子的に state を更新する。

use serde::{Deserialize, Serialize};

use crate::reason::RejectReason;
use crate::spine::Scenario;
use crate::state::{GameState, StateDelta, StateOp};

/// 裁定結果。`Reject` は**構造化された**理由を含む (文面は提示層が言語ごとに生成)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    Accept,
    Reject { reasons: Vec<RejectReason> },
}

impl Verdict {
    pub fn is_accept(&self) -> bool {
        matches!(self, Verdict::Accept)
    }
}

/// ダイスの出目。エンジンが振った結果であり、LLM は関与しない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollOutcome {
    pub sides: u32,
    pub dc: u32,
    pub result: u32,
    pub success: bool,
}

/// 唯一の裁定者。**`state` を一切変更しない純粋関数**。
///
/// 1つでも不正な op があれば `Reject` を返す (理由は全件収集)。
pub fn adjudicate(state: &GameState, scenario: &Scenario, delta: &StateDelta) -> Verdict {
    let loc = match scenario.location(&state.location) {
        Some(l) => l,
        None => {
            return Verdict::Reject {
                reasons: vec![RejectReason::CurrentLocationMissing {
                    location: state.location.clone(),
                }],
            };
        }
    };

    let mut reasons = Vec::new();

    // op 単体の力学 (所持/移動/gate/stat 宣言) を検証。
    validate_ops(&mut reasons, state, scenario, loc, delta);

    // 硬い禁忌 (Phase B): op 単体が合法なら、delta 適用後に taboo(Gate) が真化しないか検査。
    // adjudicate は純粋なので state の clone へ射影 (project) して評価する。
    if reasons.is_empty() {
        check_taboos(&mut reasons, state, scenario, delta);
    }

    if reasons.is_empty() {
        Verdict::Accept
    } else {
        Verdict::Reject { reasons }
    }
}

/// op 単体の力学を検証して reasons に積む (taboo は別。state を変えない)。
fn validate_ops(
    reasons: &mut Vec<RejectReason>,
    state: &GameState,
    scenario: &Scenario,
    loc: &crate::spine::Location,
    delta: &StateDelta,
) {
    for op in &delta.ops {
        match op {
            StateOp::AddItem { item } => {
                if state.has_item(item) {
                    reasons.push(RejectReason::ItemAlreadyHeld { item: item.clone() });
                    continue;
                }
                match loc.items.get(item) {
                    None => reasons.push(RejectReason::ItemNotHere { item: item.clone() }),
                    Some(gate) => {
                        if !gate.eval(state) {
                            reasons.push(RejectReason::ItemGateUnmet { item: item.clone() });
                        }
                    }
                }
            }
            StateOp::RemoveItem { item } => {
                if !state.has_item(item) {
                    reasons.push(RejectReason::ItemNotHeld { item: item.clone() });
                }
            }
            StateOp::SetFlag { key, value } => {
                if !scenario.allowed_flags.contains(key) {
                    reasons.push(RejectReason::FlagNotAllowed { key: key.clone() });
                    continue;
                }
                if *value && !scenario.flag_gate(key).eval(state) {
                    reasons.push(RejectReason::FlagGateUnmet { key: key.clone() });
                }
            }
            StateOp::Move { to } => match loc.exits.iter().find(|e| &e.to == to) {
                None => reasons.push(RejectReason::NoExit { to: to.clone() }),
                Some(exit) => {
                    if !exit.gate.eval(state) {
                        reasons.push(RejectReason::MoveGateUnmet { to: to.clone() });
                    }
                }
            },
            StateOp::RequestRoll { sides, dc: _ } => {
                if *sides < 1 {
                    reasons.push(RejectReason::DiceSidesInvalid);
                }
                // 出目はエンジンが振る。LLM は結果を主張できない (op 構造上不可能)。
            }
            StateOp::AdjustStat { entity, key, delta: _ } => {
                if !scenario.knows_stat(entity, key) {
                    reasons.push(RejectReason::UnknownStat { key: key.clone() });
                }
                // 算術 (current + delta) と境界クランプは apply がエンジンとして行う。
            }
            StateOp::ScaleStat { entity, key, num: _, den } => {
                if !scenario.knows_stat(entity, key) {
                    reasons.push(RejectReason::UnknownStat { key: key.clone() });
                }
                if *den == 0 {
                    reasons.push(RejectReason::DivideByZero { key: key.clone() });
                }
            }
        }
    }
}

/// delta を `state` の clone に射影し、各キャラの taboo(Gate) が **false→true** に
/// 真化するなら却下理由を積む (硬い禁忌の強制)。射影は純粋 (元 state は不変)。
fn check_taboos(
    reasons: &mut Vec<RejectReason>,
    state: &GameState,
    scenario: &Scenario,
    delta: &StateDelta,
) {
    // taboo を持つキャラが居なければ射影コストを払わない。
    if scenario.characters.values().all(|c| c.taboos.is_empty()) {
        return;
    }
    let mut projected = state.clone();
    let _ = apply_ops(&mut projected, scenario, delta); // clone への射影 (dice は捨て)
    for (eid, def) in &scenario.characters {
        for taboo in &def.taboos {
            if !taboo.eval(state) && taboo.eval(&projected) {
                reasons.push(RejectReason::TabooViolated { entity: eid.clone() });
            }
        }
    }
}

/// `adjudicate` が `Accept` の時のみデルタを**原子的に**適用する。
///
/// `Reject` の場合 `state` は一切変更されず、`Err(Verdict::Reject)` を返す。
/// 含まれる [`StateOp::RequestRoll`] はここで決定論的に振られ、結果を返す。
pub fn apply(
    state: &mut GameState,
    scenario: &Scenario,
    delta: &StateDelta,
) -> Result<Vec<RollOutcome>, Verdict> {
    // まず純粋関数で全検証 — ここを通ってから初めて state に触れる (原子性の担保)。
    match adjudicate(state, scenario, delta) {
        rejected @ Verdict::Reject { .. } => return Err(rejected),
        Verdict::Accept => {}
    }

    let rolls = apply_ops(state, scenario, delta);
    state.turn += 1;
    Ok(rolls)
}

/// delta の各 op を state に適用する (検証なし)。`apply` と taboo 射影が共有する。
/// [`StateOp::RequestRoll`] はここで決定論的に振られ、結果を返す。
fn apply_ops(state: &mut GameState, scenario: &Scenario, delta: &StateDelta) -> Vec<RollOutcome> {
    let mut rolls = Vec::new();
    for op in &delta.ops {
        match op {
            StateOp::AddItem { item } => {
                state.inventory.insert(item.clone());
            }
            StateOp::RemoveItem { item } => {
                state.inventory.remove(item);
            }
            StateOp::SetFlag { key, value } => {
                state.flags.insert(key.clone(), *value);
            }
            StateOp::Move { to } => {
                state.location = to.clone();
            }
            StateOp::RequestRoll { sides, dc } => {
                let result = state.rng.roll(*sides);
                rolls.push(RollOutcome {
                    sides: *sides,
                    dc: *dc,
                    result,
                    success: result >= *dc,
                });
            }
            // --- 算術はエンジンが行う。LLM は意図だけ提案、値は持てない ---
            StateOp::AdjustStat { entity, key, delta } => {
                let next = state.stat_of(entity, key) + delta; // 加減
                let clamped = clamp_stat(scenario, entity, key, next);
                state.set_stat(entity, key, clamped);
            }
            StateOp::ScaleStat { entity, key, num, den } => {
                // den != 0 は adjudicate が保証済。乗算先行で精度を確保。
                let next = state.stat_of(entity, key).saturating_mul(*num) / den;
                let clamped = clamp_stat(scenario, entity, key, next);
                state.set_stat(entity, key, clamped);
            }
        }
    }
    rolls
}

/// stat を宣言された境界 `[min, max]` に収める。max 未宣言なら上限なし。
fn clamp_stat(scenario: &Scenario, entity: &str, key: &str, value: i64) -> i64 {
    let (min, max) = scenario.stat_bounds(entity, key);
    let v = value.max(min);
    max.map_or(v, |m| v.min(m))
}

/// クリア条件を満たしているか。
pub fn is_goal(state: &GameState, scenario: &Scenario) -> bool {
    scenario.goal.eval(state)
}

// =============================================================================
// PoC: 正本エンジンの実証 (Red→Green)
// クラウドLLM を繋ぐ前に、最重要の「裁定」脚をテストで固める。
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::reason::RejectReason;
    use crate::state::{RngState, StateOp, PLAYER};

    // 密室脱出シナリオをコンパイル時に埋め込む (cwd 非依存)。
    const LOCKED_ROOM: &str = include_str!("../../../scenarios/locked_room.yaml");
    // 数値の最小盤面。
    const STRENGTH_TRIAL: &str = include_str!("../../../scenarios/strength_trial.yaml");
    // キャラ別ステータスの最小盤面。
    const HEROINE_ROUTE: &str = include_str!("../../../scenarios/heroine_route.yaml");

    fn scenario() -> Scenario {
        Scenario::from_yaml(LOCKED_ROOM).expect("locked_room.yaml がパースできること")
    }

    fn trial() -> Scenario {
        Scenario::from_yaml(STRENGTH_TRIAL).expect("strength_trial.yaml がパースできること")
    }

    fn route() -> Scenario {
        Scenario::from_yaml(HEROINE_ROUTE).expect("heroine_route.yaml がパースできること")
    }

    fn fresh(sc: &Scenario) -> GameState {
        GameState::new(sc.start.clone(), 42)
    }

    fn d(ops: Vec<StateOp>) -> StateDelta {
        StateDelta::new("", ops)
    }

    #[test]
    fn yaml_contract_loads() {
        let sc = scenario();
        assert_eq!(sc.start, "cell");
        assert!(sc.locations.contains_key("cell"));
        assert!(sc.locations.contains_key("corridor"));
    }

    /// 正規の筋を通すと goal に到達する。
    #[test]
    fn legal_playthrough_reaches_goal() {
        let sc = scenario();
        let mut s = fresh(&sc);
        assert!(!is_goal(&s, &sc));

        apply(&mut s, &sc, &d(vec![StateOp::SetFlag { key: "drawer_opened".into(), value: true }]))
            .expect("引き出しはいつでも開けられる");
        apply(&mut s, &sc, &d(vec![StateOp::AddItem { item: "rusty_key".into() }]))
            .expect("引き出しを開けたので鍵が取れる");
        apply(&mut s, &sc, &d(vec![StateOp::SetFlag { key: "door_unlocked".into(), value: true }]))
            .expect("鍵を持っているので解錠できる");
        apply(&mut s, &sc, &d(vec![StateOp::Move { to: "corridor".into() }]))
            .expect("解錠したので廊下へ出られる");

        assert!(is_goal(&s, &sc), "goal (location_is corridor) に到達しているはず");
        assert_eq!(s.turn, 4);
    }

    /// 引き出しを開ける前に鍵は取れない。
    #[test]
    fn take_key_before_opening_drawer_is_rejected() {
        let sc = scenario();
        let s = fresh(&sc);
        let v = adjudicate(&s, &sc, &d(vec![StateOp::AddItem { item: "rusty_key".into() }]));
        assert!(!v.is_accept(), "drawer_opened 未達なので鍵取得は却下されるべき");
    }

    /// 鍵なしで扉は解錠できない。
    #[test]
    fn open_door_without_key_is_rejected() {
        let sc = scenario();
        let s = fresh(&sc);
        let v = adjudicate(&s, &sc, &d(vec![StateOp::SetFlag { key: "door_unlocked".into(), value: true }]));
        assert!(!v.is_accept());
    }

    /// 解錠前に廊下へは出られない。
    #[test]
    fn move_without_unlock_is_rejected() {
        let sc = scenario();
        let s = fresh(&sc);
        let v = adjudicate(&s, &sc, &d(vec![StateOp::Move { to: "corridor".into() }]));
        assert!(!v.is_accept());
    }

    /// 【敵対ターン】存在しない「マスターキー」を持っていると嘘をついても、
    /// エンジンが LLM の流暢さに勝つ。これが「正本 > 文章力」の最小証明。
    #[test]
    fn phantom_master_key_is_rejected() {
        let sc = scenario();
        let s = fresh(&sc);
        let v = adjudicate(&s, &sc, &d(vec![StateOp::AddItem { item: "master_key".into() }]));
        match v {
            Verdict::Reject { reasons } => {
                assert!(reasons.iter().any(|r| matches!(
                    r,
                    RejectReason::ItemNotHere { item } if item == "master_key"
                )));
            }
            Verdict::Accept => panic!("幻のアイテムを受理してはならない"),
        }
    }

    /// 【原子性】一部が不正なデルタは全体却下、state は無傷。
    #[test]
    fn mixed_delta_is_atomic() {
        let sc = scenario();
        let mut s = fresh(&sc);
        let delta = d(vec![
            StateOp::SetFlag { key: "drawer_opened".into(), value: true }, // 単体なら合法
            StateOp::AddItem { item: "master_key".into() },                // 不正
        ]);
        let result = apply(&mut s, &sc, &delta);
        assert!(result.is_err(), "不正な op を含むデルタは却下されるべき");
        assert!(!s.flag("drawer_opened"), "却下されたデルタは state を変えてはならない");
        assert_eq!(s.turn, 0, "却下では turn が進まない");
    }

    /// ダイスは決定論的・監査可能。同じ seed/cursor は同じ目を返す。
    #[test]
    fn dice_are_deterministic_and_in_range() {
        let mut a = RngState { seed: 7, cursor: 0 };
        let mut b = RngState { seed: 7, cursor: 0 };
        let seq_a: Vec<u32> = (0..8).map(|_| a.roll(6)).collect();
        let seq_b: Vec<u32> = (0..8).map(|_| b.roll(6)).collect();
        assert_eq!(seq_a, seq_b, "同じ seed なら同じ出目列");
        assert!(seq_a.iter().all(|&r| (1..=6).contains(&r)), "1d6 は 1..=6");
    }

    /// request_roll は op 構造上 LLM が結果を持てない。エンジンが振り、DC で成否判定。
    #[test]
    fn request_roll_is_adjudicated_by_engine() {
        let sc = scenario();
        let mut s = fresh(&sc);
        let rolls = apply(&mut s, &sc, &d(vec![StateOp::RequestRoll { sides: 20, dc: 10 }]))
            .expect("ダイス要求自体は合法");
        assert_eq!(rolls.len(), 1);
        let outcome = &rolls[0];
        assert!((1..=20).contains(&outcome.result));
        assert_eq!(outcome.success, outcome.result >= 10);
        assert_eq!(s.rng.cursor, 1, "1回振ったので cursor が進む");
    }

    // =========================================================================
    // 数値ステータス PoC: 四則演算をエンジンが代行する (LLM は値を持てない)
    // =========================================================================

    /// 初期 stat はシナリオから読まれる。
    #[test]
    fn stats_load_from_scenario() {
        let sc = trial();
        let s = sc.initial_state(42);
        assert_eq!(s.stat("hp"), 10);
        assert_eq!(s.stat("str"), 12);
        assert_eq!(s.stat("gold"), 0);
        assert_eq!(s.stat("mana"), 0, "未宣言 stat は 0 扱い");
    }

    /// 【加減】AdjustStat はエンジンが current + delta を計算する。LLM は値を書かない。
    #[test]
    fn adjust_stat_is_computed_by_engine() {
        let sc = trial();
        let mut s = sc.initial_state(42);
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat { entity: PLAYER.into(), key: "str".into(), delta: 3 }]))
            .expect("宣言済 stat の加算は合法");
        assert_eq!(s.stat("str"), 15, "12 + 3 をエンジンが計算");
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat { entity: PLAYER.into(), key: "gold".into(), delta: 25 }]))
            .expect("加算");
        assert_eq!(s.stat("gold"), 25);
    }

    /// 【0クランプ】HP は 0 未満にならない。死亡判定 (hp>=1 gate) の土台。
    #[test]
    fn hp_clamps_at_zero_and_blocks_exit() {
        let sc = trial();
        let mut s = sc.initial_state(42);
        // まず脱出に必要な力をつける。
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat { entity: PLAYER.into(), key: "str".into(), delta: 3 }])).unwrap();
        // 致命の一撃。-100 でも 0 でクランプ (負の HP にならない)。
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat { entity: PLAYER.into(), key: "hp".into(), delta: -100 }])).unwrap();
        assert_eq!(s.stat("hp"), 0, "HP は 0 でクランプ");
        // str は足りるが hp=0 なので脱出 gate (hp>=1) を満たせない = 死んでいては出られない。
        let v = adjudicate(&s, &sc, &d(vec![StateOp::Move { to: "hall".into() }]));
        assert!(!v.is_accept(), "hp=0 では hall へ出られない");
    }

    /// 【乗除】ScaleStat はエンジンが current * num / den を計算する。
    #[test]
    fn scale_stat_multiplies_and_divides() {
        let sc = trial();
        let mut s = sc.initial_state(42);
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat { entity: PLAYER.into(), key: "gold".into(), delta: 10 }])).unwrap();
        // ×2: 報酬を倍に。
        apply(&mut s, &sc, &d(vec![StateOp::ScaleStat { entity: PLAYER.into(), key: "gold".into(), num: 2, den: 1 }])).unwrap();
        assert_eq!(s.stat("gold"), 20, "10 × 2 をエンジンが計算");
        // ÷2: 半減。
        apply(&mut s, &sc, &d(vec![StateOp::ScaleStat { entity: PLAYER.into(), key: "gold".into(), num: 1, den: 2 }])).unwrap();
        assert_eq!(s.stat("gold"), 10, "20 / 2 をエンジンが計算");
    }

    /// 【ゼロ除算ガード】den=0 はエンジンが却下する。LLM は /0 で壊せない。state 無傷。
    #[test]
    fn divide_by_zero_is_rejected() {
        let sc = trial();
        let mut s = sc.initial_state(42);
        let before = s.stat("gold");
        let v = adjudicate(&s, &sc, &d(vec![StateOp::ScaleStat { entity: PLAYER.into(), key: "gold".into(), num: 1, den: 0 }]));
        match v {
            Verdict::Reject { reasons } => assert!(reasons
                .iter()
                .any(|r| matches!(r, RejectReason::DivideByZero { key } if key == "gold"))),
            Verdict::Accept => panic!("ゼロ除算を受理してはならない"),
        }
        let r = apply(&mut s, &sc, &d(vec![StateOp::ScaleStat { entity: PLAYER.into(), key: "gold".into(), num: 1, den: 0 }]));
        assert!(r.is_err(), "apply も却下する");
        assert_eq!(s.stat("gold"), before, "却下では state 無傷");
    }

    /// 【未宣言 stat の遮断】シナリオに無い stat は作れない (幻ステータス却下)。
    #[test]
    fn unknown_stat_is_rejected() {
        let sc = trial();
        let s = sc.initial_state(42);
        let v = adjudicate(&s, &sc, &d(vec![StateOp::AdjustStat { entity: PLAYER.into(), key: "mana".into(), delta: 9000 }]));
        match v {
            Verdict::Reject { reasons } => assert!(reasons
                .iter()
                .any(|r| matches!(r, RejectReason::UnknownStat { key } if key == "mana"))),
            Verdict::Accept => panic!("未宣言 stat の操作を受理してはならない"),
        }
    }

    /// 【数値 gate × 正規プレイ】鍛えて力 15 にしてから扉を押すと脱出できる。
    #[test]
    fn train_then_exit_reaches_goal() {
        let sc = trial();
        let mut s = sc.initial_state(42);
        assert!(!is_goal(&s, &sc));
        // 力 12 のままでは押せない。
        assert!(!adjudicate(&s, &sc, &d(vec![StateOp::Move { to: "hall".into() }])).is_accept());
        // 鍛錬して 12 → 15。
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat { entity: PLAYER.into(), key: "str".into(), delta: 3 }])).unwrap();
        // 今度は押せる。
        apply(&mut s, &sc, &d(vec![StateOp::Move { to: "hall".into() }]))
            .expect("str>=15 かつ hp>=1 なら脱出できる");
        assert!(is_goal(&s, &sc), "goal (hall) 到達");
    }

    // -------------------------------------------------------------------------
    // キャラ別ステータス PoC: 数値が entity ごとに紐づく (外部キャラ定義から)
    // -------------------------------------------------------------------------

    /// キャラ定義ファイルから各 entity の初期 stat が読まれる。
    #[test]
    fn character_stats_load_from_scenario() {
        let sc = route();
        let s = sc.initial_state(7);
        assert_eq!(s.stat_of("alice", "好感度"), 0);
        assert_eq!(s.stat_of("player", "好感度"), 0, "player は alice と別の数値空間");
    }

    /// 【entity 指定】好感度はアリスに紐づく。player の同名 stat とは別物。
    #[test]
    fn adjust_targets_named_entity() {
        let sc = route();
        let mut s = sc.initial_state(7);
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat {
            entity: "alice".into(),
            key: "好感度".into(),
            delta: 30,
        }]))
        .expect("アリスの好感度は宣言済");
        assert_eq!(s.stat_of("alice", "好感度"), 30);
        assert_eq!(s.stat_of("player", "好感度"), 0, "player には影響しない");
    }

    /// 【境界】好感度は宣言された上限 100 でクランプされる。
    #[test]
    fn affection_clamps_at_declared_max() {
        let sc = route();
        let mut s = sc.initial_state(7);
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat {
            entity: "alice".into(),
            key: "好感度".into(),
            delta: 200,
        }]))
        .unwrap();
        assert_eq!(s.stat_of("alice", "好感度"), 100, "max=100 でクランプ");
    }

    /// 【未宣言の遮断】alice が持たない stat / 未知の entity は却下。
    #[test]
    fn unknown_stat_or_entity_is_rejected() {
        let sc = route();
        let s = sc.initial_state(7);
        // alice は mana を宣言していない。
        assert!(!adjudicate(&s, &sc, &d(vec![StateOp::AdjustStat {
            entity: "alice".into(),
            key: "mana".into(),
            delta: 1,
        }]))
        .is_accept());
        // ghost という entity は存在しない (何も宣言していない)。
        assert!(!adjudicate(&s, &sc, &d(vec![StateOp::AdjustStat {
            entity: "ghost".into(),
            key: "好感度".into(),
            delta: 1,
        }]))
        .is_accept());
    }

    /// 【キャラ別数値 gate】アリスの好感度 50 で goal 到達。
    #[test]
    fn affection_gate_reaches_goal() {
        let sc = route();
        let mut s = sc.initial_state(7);
        assert!(!is_goal(&s, &sc));
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat {
            entity: "alice".into(),
            key: "好感度".into(),
            delta: 50,
        }]))
        .unwrap();
        assert!(is_goal(&s, &sc), "alice の好感度 >= 50 で goal");
    }

    // -------------------------------------------------------------------------
    // 硬い禁忌 PoC (Phase B): キャラは自分の禁忌を破れない (正本 > 文章力 のキャラ版)
    // -------------------------------------------------------------------------

    /// 【禁忌の強制】アリスの禁忌 (豚肉を断つ=flag alice_ate_pork) を立てる delta は却下。
    #[test]
    fn taboo_blocks_violating_delta() {
        let sc = route();
        let s = sc.initial_state(7);
        // op 単体は合法 (allowed_flags に在り gate も Always) だが、taboo が真化するので却下。
        let v = adjudicate(
            &s,
            &sc,
            &d(vec![StateOp::SetFlag { key: "alice_ate_pork".into(), value: true }]),
        );
        match v {
            Verdict::Reject { reasons } => assert!(reasons
                .iter()
                .any(|r| matches!(r, RejectReason::TabooViolated { entity } if entity == "alice"))),
            Verdict::Accept => panic!("禁忌を破る delta を受理してはならない"),
        }
    }

    /// 【禁忌の原子性】禁忌を破る op を含むデルタは全体却下、合法 op の効果も適用されない。
    #[test]
    fn taboo_violation_is_atomic() {
        let sc = route();
        let mut s = sc.initial_state(7);
        let delta = d(vec![
            StateOp::AdjustStat { entity: "alice".into(), key: "好感度".into(), delta: 10 }, // 合法
            StateOp::SetFlag { key: "alice_ate_pork".into(), value: true },                  // 禁忌
        ]);
        assert!(apply(&mut s, &sc, &delta).is_err());
        assert_eq!(s.stat_of("alice", "好感度"), 0, "却下なら好感度も動かない");
        assert!(!s.flag("alice_ate_pork"));
        assert_eq!(s.turn, 0);
    }

    /// 禁忌に無関係な合法 delta は通る (禁忌は無関係な行動を妨げない)。
    #[test]
    fn taboo_does_not_block_unrelated() {
        let sc = route();
        let mut s = sc.initial_state(7);
        apply(&mut s, &sc, &d(vec![StateOp::AdjustStat {
            entity: "alice".into(),
            key: "好感度".into(),
            delta: 10,
        }]))
        .expect("禁忌と無関係な好感度上昇は通る");
        assert_eq!(s.stat_of("alice", "好感度"), 10);
    }

    /// 【既定 entity】entity 省略のデルタ (LLM/YAML) は "player" に解決される。
    #[test]
    fn omitted_entity_defaults_to_player() {
        // entity を書かない (LLM/YAML が省略した) op は "player" に解決される。
        let op: StateOp = serde_yaml::from_str("op: adjust_stat\nkey: hp\ndelta: -1").unwrap();
        match op {
            StateOp::AdjustStat { entity, .. } => assert_eq!(entity, PLAYER),
            other => panic!("adjust_stat であるべき: {other:?}"),
        }
    }

    /// 【原子性 × stat】不正 op を含むデルタは全体却下、stat も無傷。
    #[test]
    fn mixed_stat_delta_is_atomic() {
        let sc = trial();
        let mut s = sc.initial_state(42);
        let delta = d(vec![
            StateOp::AdjustStat { entity: PLAYER.into(), key: "str".into(), delta: 3 },   // 単体なら合法
            StateOp::ScaleStat { entity: PLAYER.into(), key: "gold".into(), num: 1, den: 0 }, // ゼロ除算で不正
        ]);
        assert!(apply(&mut s, &sc, &delta).is_err());
        assert_eq!(s.stat("str"), 12, "却下されたデルタは stat を変えない");
        assert_eq!(s.turn, 0);
    }
}
