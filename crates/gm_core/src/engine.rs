//! 正本の裁定者。LLM の提案を裁き、受理時のみ原子的に state を更新する。

use serde::{Deserialize, Serialize};

use crate::reason::RejectReason;
use crate::spine::Scenario;
use crate::state::{GameState, StateDelta, StateOp, TriggerId, PLAYER};

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

/// 技能判定の結果。`1d{sides} + modifier` を振り `total >= dc` で成否。LLM は出目も合計も持てない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckOutcome {
    pub entity: String,
    pub stat: String,
    pub sides: u32,
    pub roll: u32,
    pub modifier: i64,
    pub total: i64,
    pub dc: u32,
    pub success: bool,
    /// 該当した極 (tier) 名 (authored challenge の大失敗/大成功)。素の判定や非クリティカルでは `None`。
    #[serde(default)]
    pub tier: Option<String>,
}

/// 発火したトリガー (Phase C)。`narration` は語りへ注入する指示。
///
/// `recall` は Memoria 橋渡しの cue を**そのまま passthrough** したもの (engine は解釈しない)。
/// 上位 (harness) が `recall` を Memoria で解決して伏線を語りに注入する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FiredTrigger {
    pub id: TriggerId,
    pub narration: String,
    pub recall: Option<String>,
}

/// デルタ受理時の適用結果。ダイスの出目と、その適用が連鎖発火させたトリガー群。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyOutcome {
    /// `request_roll` とトリガー効果が振ったダイスの出目 (適用順)。
    pub rolls: Vec<RollOutcome>,
    /// この適用で行われた技能判定の結果。次ターンの語りに還流する。
    pub checks: Vec<CheckOutcome>,
    /// この適用で発火した反応ビート (authored 順・連鎖含む)。語りに注入する。
    pub fired: Vec<FiredTrigger>,
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
                if state.has_item(PLAYER, item) {
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
                if !state.has_item(PLAYER, item) {
                    reasons.push(RejectReason::ItemNotHeld { item: item.clone() });
                }
            }
            StateOp::GiveItem { from, to, item } => {
                // 持っていない物は渡せない (#23 の engine 側バックストップ)。
                if !state.has_item(from, item) {
                    reasons.push(RejectReason::ItemNotHeld { item: item.clone() });
                }
                // 幻のキャラには渡せない (閉世界)。
                if !scenario.knows_entity(to) {
                    reasons.push(RejectReason::UnknownEntity { entity: to.clone() });
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
            StateOp::Check { entity, stat, sides, dc: _ } => {
                if *sides < 1 {
                    reasons.push(RejectReason::DiceSidesInvalid);
                }
                // 修正に使う stat は宣言済みでなければならない (幻ステータスで判定を盛れない)。
                if !scenario.knows_stat(entity, stat) {
                    reasons.push(RejectReason::UnknownStat { key: stat.clone() });
                }
            }
            StateOp::AttemptChallenge { entity, challenge } => {
                // 閉世界: 宣言された challenge にしか挑めない (幻チャレンジ遮断)。
                match scenario.challenge(challenge) {
                    None => reasons.push(RejectReason::UnknownChallenge {
                        challenge: challenge.clone(),
                    }),
                    Some(def) => {
                        // 判定の素性は authored だが、挑戦する entity がその stat を宣言してなければ判定できない。
                        if !scenario.knows_stat(entity, &def.stat) {
                            reasons.push(RejectReason::UnknownStat { key: def.stat.clone() });
                        }
                        if def.sides < 1 {
                            reasons.push(RejectReason::DiceSidesInvalid);
                        }
                    }
                }
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
            StateOp::GrantSkill { entity, skill } => {
                // 能力の開花は authored トリガーの専権。LLM 提案は常に却下 (メアリー・スー遮断)。
                // trigger effects は apply_ops 直行なのでこの検証を通らず付与できる。
                reasons.push(RejectReason::SkillGrantNotAllowed {
                    entity: entity.clone(),
                    skill: skill.clone(),
                });
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
    // clone への射影 (dice/jud定 は捨て、taboo 評価のためだけに state を進める)。
    apply_ops(&mut projected, scenario, delta, &mut Vec::new(), &mut Vec::new());
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
/// 含まれる [`StateOp::RequestRoll`] はここで決定論的に振られる。適用後、発火条件が
/// 真化したトリガー (Phase C) を連鎖発火させ、その出目と発火ビートも [`ApplyOutcome`] に含める。
pub fn apply(
    state: &mut GameState,
    scenario: &Scenario,
    delta: &StateDelta,
) -> Result<ApplyOutcome, Verdict> {
    // まず純粋関数で全検証 — ここを通ってから初めて state に触れる (原子性の担保)。
    match adjudicate(state, scenario, delta) {
        rejected @ Verdict::Reject { .. } => return Err(rejected),
        Verdict::Accept => {}
    }

    let mut rolls = Vec::new();
    let mut checks = Vec::new();
    apply_ops(state, scenario, delta, &mut rolls, &mut checks);
    state.turn += 1;
    // 反応ビート (禁忌の双対)。受理・適用済みの実 state に対して発火判定する。
    let fired = fire_triggers(state, scenario, &mut rolls, &mut checks);
    Ok(ApplyOutcome { rolls, checks, fired })
}

/// 受理・適用後の `state` に対し、発火条件 `when` が真でまだ発火していないトリガーを発火させる。
///
/// 禁忌 (`check_taboos`) の双対: 禁忌が「真化を却下」するのに対し、トリガーは「真化で発火」する。
/// 発火は authored な `effects` を **検証せず** 原子適用し (シナリオ作者の信頼済データ、LLM 提案でない)、
/// [`GameState::fired`] に latch して二度目の発火を抑止する (edge-triggered once)。
/// 効果が別トリガーの `when` を真化させる連鎖は、新たな発火が無くなるまで settle する
/// (各トリガーは高々 1 回発火するので必ず停止)。authored 順に評価して決定論を保つ。
fn fire_triggers(
    state: &mut GameState,
    scenario: &Scenario,
    rolls: &mut Vec<RollOutcome>,
    checks: &mut Vec<CheckOutcome>,
) -> Vec<FiredTrigger> {
    let mut fired = Vec::new();
    loop {
        // 未発火かつ発火条件成立の最初のトリガー (authored 順)。
        let next = scenario
            .triggers
            .iter()
            .find(|t| !state.fired.contains(&t.id) && t.when.eval(state));
        let Some(t) = next else { break };

        // 効果は authored・信頼済なので validate せず原子適用する。
        let effect_delta = StateDelta::new(String::new(), t.effects.clone());
        apply_ops(state, scenario, &effect_delta, rolls, checks);

        state.fired.insert(t.id.clone());
        fired.push(FiredTrigger {
            id: t.id.clone(),
            narration: t.narration.clone(),
            recall: t.recall.clone(), // cue を passthrough。解釈は harness。
        });
    }
    fired
}

/// delta の各 op を state に適用する (検証なし)。`apply` と taboo 射影が共有する。
/// [`StateOp::RequestRoll`]/[`StateOp::Check`] はここで決定論的に振られ、`rolls`/`checks` に積まれる。
fn apply_ops(
    state: &mut GameState,
    scenario: &Scenario,
    delta: &StateDelta,
    rolls: &mut Vec<RollOutcome>,
    checks: &mut Vec<CheckOutcome>,
) {
    for op in &delta.ops {
        match op {
            StateOp::AddItem { item } => {
                state.add_to_inventory(PLAYER, item);
            }
            StateOp::RemoveItem { item } => {
                state.remove_from_inventory(PLAYER, item);
            }
            StateOp::GiveItem { from, to, item } => {
                // adjudicate が from 所持・to 既知を保証済。原子的に移す。
                state.remove_from_inventory(from, item);
                state.add_to_inventory(to, item);
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
            StateOp::Check { entity, stat, sides, dc } => {
                // 技能判定: 1d{sides} + stat修正 vs dc。出目も合計もエンジンが決める。
                let roll = state.rng.roll(*sides);
                let modifier = state.stat_of(entity, stat);
                let total = roll as i64 + modifier;
                checks.push(CheckOutcome {
                    entity: entity.clone(),
                    stat: stat.clone(),
                    sides: *sides,
                    roll,
                    modifier,
                    total,
                    dc: *dc,
                    success: total >= *dc as i64,
                    tier: None, // 素の判定は極を持たない (tier は authored challenge の専権)。
                });
            }
            StateOp::AttemptChallenge { entity, challenge } => {
                // adjudicate が challenge 既知・stat 宣言済を保証済。authored 定義から判定を組む。
                // ここに到達する challenge は必ず存在する (adjudicate 通過後)。
                if let Some(def) = scenario.challenge(challenge) {
                    let roll = state.rng.roll(def.sides);
                    let modifier = state.stat_of(entity, &def.stat);
                    let total = roll as i64 + modifier;
                    // 極 (tier): 自然出目が min(=1)/max(=sides) に該当する authored tier を引く。
                    // 該当 tier に flag があれば engine が直書きする (allowed_flags 宣言済を validate が保証)。
                    let hit = def.tiers.iter().find(|(_, t)| match t.natural {
                        crate::spine::Natural::Min => roll == 1,
                        crate::spine::Natural::Max => roll == def.sides,
                    });
                    let tier = hit.map(|(name, _)| name.clone());
                    if let Some((_, t)) = hit {
                        if let Some(flag) = &t.flag {
                            state.flags.insert(flag.clone(), true);
                        }
                    }
                    checks.push(CheckOutcome {
                        entity: entity.clone(),
                        stat: def.stat.clone(),
                        sides: def.sides,
                        roll,
                        modifier,
                        total,
                        dc: def.dc,
                        success: total >= def.dc as i64,
                        tier,
                    });
                }
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
            StateOp::GrantSkill { entity, skill } => {
                // ここに到達するのは authored トリガーの effect のみ (LLM 提案は adjudicate で却下済)。
                state.grant_skill(entity, skill);
            }
        }
    }
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
    // 反応ビート (Phase C) の最小盤面。
    const TRIGGER_RECALL: &str = include_str!("../../../scenarios/trigger_recall.yaml");
    // 閉世界 capability (スキル覚醒) の最小盤面。
    const SKILL_AWAKENING: &str = include_str!("../../../scenarios/skill_awakening.yaml");
    // NPC inventory + 譲渡 (give_item) の最小盤面。
    const GIFT: &str = include_str!("../../../scenarios/gift.yaml");
    // 技能判定の大失敗が世界を変える (fumble-as-trigger, PoC-1) の最小盤面。
    const FUMBLE_CHECK: &str = include_str!("../../../scenarios/fumble_check.yaml");

    fn scenario() -> Scenario {
        Scenario::from_yaml(LOCKED_ROOM).expect("locked_room.yaml がパースできること")
    }

    fn trial() -> Scenario {
        Scenario::from_yaml(STRENGTH_TRIAL).expect("strength_trial.yaml がパースできること")
    }

    fn route() -> Scenario {
        Scenario::from_yaml(HEROINE_ROUTE).expect("heroine_route.yaml がパースできること")
    }

    fn recall() -> Scenario {
        Scenario::from_yaml(TRIGGER_RECALL).expect("trigger_recall.yaml がパースできること")
    }

    fn awakening() -> Scenario {
        Scenario::from_yaml(SKILL_AWAKENING).expect("skill_awakening.yaml がパースできること")
    }

    fn gift() -> Scenario {
        Scenario::from_yaml(GIFT).expect("gift.yaml がパースできること")
    }

    fn fumble() -> Scenario {
        Scenario::from_yaml(FUMBLE_CHECK).expect("fumble_check.yaml がパースできること")
    }

    /// アリスの好感度を増やす delta (発火条件を跨ぐための糖衣)。
    fn raise_affection(amount: i64) -> StateDelta {
        d(vec![StateOp::AdjustStat {
            entity: "alice".into(),
            key: "好感度".into(),
            delta: amount,
        }])
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
        let out = apply(&mut s, &sc, &d(vec![StateOp::RequestRoll { sides: 20, dc: 10 }]))
            .expect("ダイス要求自体は合法");
        assert_eq!(out.rolls.len(), 1);
        let outcome = &out.rolls[0];
        assert!((1..=20).contains(&outcome.result));
        assert_eq!(outcome.success, outcome.result >= 10);
        assert_eq!(s.rng.cursor, 1, "1回振ったので cursor が進む");
    }

    // -------------------------------------------------------------------------
    // 技能判定 PoC: 1d{sides} + stat修正 vs dc。出目も合計もエンジンが裁く (LLM は持てない)。
    // -------------------------------------------------------------------------

    /// 【技能判定】判定は 1d{sides} に宣言済み stat を修正として足し、dc と比べる。
    #[test]
    fn check_resolves_with_stat_modifier() {
        let sc = trial(); // str=12
        let mut s = sc.initial_state(42);
        let out = apply(&mut s, &sc, &d(vec![StateOp::Check {
            entity: PLAYER.into(),
            stat: "str".into(),
            sides: 20,
            dc: 15,
        }]))
        .expect("宣言済み stat の判定は合法");
        assert_eq!(out.checks.len(), 1);
        let c = &out.checks[0];
        assert_eq!(c.modifier, 12, "str=12 が修正に乗る");
        assert!((1..=20).contains(&c.roll), "1d20 の出目");
        assert_eq!(c.total, c.roll as i64 + 12, "合計 = 出目 + 修正");
        assert_eq!(c.success, c.total >= 15, "total>=dc で成功");
        assert_eq!(s.rng.cursor, 1, "1回振ったので cursor が進む");
    }

    /// 【幻ステータス遮断】未宣言の stat を修正に使う判定は却下 (判定を盛れない)。
    #[test]
    fn check_with_unknown_stat_is_rejected() {
        let sc = trial();
        let s = sc.initial_state(42);
        let v = adjudicate(&s, &sc, &d(vec![StateOp::Check {
            entity: PLAYER.into(),
            stat: "mana".into(), // 未宣言
            sides: 20,
            dc: 10,
        }]));
        match v {
            Verdict::Reject { reasons } => assert!(reasons
                .iter()
                .any(|r| matches!(r, RejectReason::UnknownStat { key } if key == "mana"))),
            Verdict::Accept => panic!("未宣言 stat の判定を受理してはならない"),
        }
    }

    /// 【決定論】同じ seed なら同じ判定結果 (監査可能)。
    #[test]
    fn check_is_deterministic() {
        let sc = trial();
        let mut a = sc.initial_state(7);
        let mut b = sc.initial_state(7);
        let chk = |st: &mut GameState| {
            apply(st, &sc, &d(vec![StateOp::Check {
                entity: PLAYER.into(),
                stat: "str".into(),
                sides: 20,
                dc: 10,
            }]))
            .unwrap()
            .checks
        };
        assert_eq!(chk(&mut a), chk(&mut b), "同じ seed なら同じ判定結果");
    }

    // -------------------------------------------------------------------------
    // fumble-as-trigger PoC-1: authored challenge の大失敗(natural 1)が宣言済フラグを
    // 直書きし、それを gate にした既存トリガーが同じ適用内で発火する。
    // tier/flag は authored、LLM は challenge を「選ぶ」だけ (帰結を持てない=閉世界)。
    // -------------------------------------------------------------------------

    /// 【fumble-as-trigger】大失敗(natural 1) → engine が authored flag 直書き → trigger 発火 → goal。
    #[test]
    fn attempt_challenge_crit_fail_sets_flag_and_fires_trigger() {
        let sc = fumble();
        assert!(sc.validate().is_empty(), "正しいシナリオは validate を通る");
        let mut s = sc.initial_state(19); // seed 19 → 1d6 初回が natural 1
        assert!(!is_goal(&s, &sc));

        let out = apply(
            &mut s,
            &sc,
            &d(vec![StateOp::AttemptChallenge {
                entity: PLAYER.into(),
                challenge: "drawer_pick".into(),
            }]),
        )
        .expect("authored challenge への挑戦は合法");

        // 出目と tier (engine が裁く)。
        assert_eq!(out.checks.len(), 1);
        let c = &out.checks[0];
        assert_eq!(c.roll, 1, "seed 19 で 1d6 は natural 1");
        assert_eq!(c.tier.as_deref(), Some("crit_fail"), "natural min → crit_fail tier");
        assert!(!c.success, "1+2=3 < dc5 なので判定自体は失敗");

        // 帰結: authored flag が engine 直書きで立ち、それを gate にした trigger が同一適用で発火。
        assert_eq!(
            s.flags.get("fumble_drawer"),
            Some(&true),
            "engine が authored 定義から fumble_drawer を直書き (LLM 経路でない)"
        );
        assert!(
            out.fired.iter().any(|f| f.id == "drawer_jam"),
            "fumble_drawer を gate にした既存トリガーが発火する"
        );
        assert!(is_goal(&s, &sc), "trigger が drawer_jammed を立て goal 到達 (失敗が分岐になった)");
    }

    /// 【閉世界】宣言されていない challenge には挑めない (幻チャレンジ遮断)。
    #[test]
    fn attempt_unknown_challenge_is_rejected() {
        let sc = fumble();
        let s = sc.initial_state(19);
        let v = adjudicate(
            &s,
            &sc,
            &d(vec![StateOp::AttemptChallenge {
                entity: PLAYER.into(),
                challenge: "teleport".into(), // 未宣言
            }]),
        );
        match v {
            Verdict::Reject { reasons } => assert!(reasons.iter().any(
                |r| matches!(r, RejectReason::UnknownChallenge { challenge } if challenge == "teleport")
            )),
            Verdict::Accept => panic!("未宣言 challenge への挑戦を受理してはならない"),
        }
    }

    /// 【非クリティカル】natural min/max でなければ tier は付かず、帰結フラグも立たない。
    #[test]
    fn attempt_challenge_non_crit_sets_no_flag() {
        let sc = fumble();
        let mut s = sc.initial_state(42); // seed 42 → 1d6 は 1 でも 6 でもない
        let out = apply(
            &mut s,
            &sc,
            &d(vec![StateOp::AttemptChallenge {
                entity: PLAYER.into(),
                challenge: "drawer_pick".into(),
            }]),
        )
        .unwrap();
        let c = &out.checks[0];
        assert!(c.roll != 1 && c.roll != 6, "natural でない出目 (seed 42)");
        assert_eq!(c.tier, None, "natural でなければ tier 無し");
        assert_eq!(s.flags.get("fumble_drawer"), None, "帰結フラグは立たない");
        assert!(out.fired.is_empty(), "トリガー発火なし");
    }

    /// 【load 時参照整合】challenge の tier flag が allowed_flags に無ければ validate が弾く
    /// (engine が幻参照のフラグを立てる経路を作らせない)。
    #[test]
    fn validate_rejects_undeclared_tier_flag() {
        let yaml = r#"
title: bad
start: room
allowed_flags: []
challenges:
  bad_check:
    stat: str
    sides: 6
    dc: 5
    tiers:
      crit_fail: { natural: min, flag: ghost_flag }
locations:
  room:
    description: x
    items: {}
    exits: []
goal: { kind: always }
"#;
        let sc = Scenario::from_yaml(yaml).expect("パースは通る (整合性検査は別工程)");
        let errs = sc.validate();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                crate::spine::ScenarioError::ChallengeFlagUndeclared { flag, .. } if flag == "ghost_flag"
            )),
            "未宣言の tier flag は validate で検出されるべき"
        );
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

    // -------------------------------------------------------------------------
    // 反応ビート PoC (Phase C): 禁忌の双対。真化を却下する代わりに真化で発火する。
    // 「伏線が必ず回収される」をエンジンが保証する (LLM の忘却に依存しない)。
    // -------------------------------------------------------------------------

    /// 【発火】好感度が閾値 (30) を越えると trigger が発火し、効果と語りが返る。
    #[test]
    fn trigger_fires_on_threshold_and_applies_effect() {
        let sc = recall();
        let mut s = sc.initial_state(7);
        assert!(!s.flag("promise_remembered"));

        let out = apply(&mut s, &sc, &raise_affection(30)).expect("好感度上昇は合法");

        assert!(s.flag("promise_remembered"), "発火効果でフラグが立つ");
        assert!(
            out.fired.iter().any(|f| f.id == "recall_promise"),
            "recall_promise が発火したと返る"
        );
        assert!(
            out.fired.iter().any(|f| f.id == "recall_promise" && !f.narration.is_empty()),
            "語りの指示が載っている"
        );
        assert!(s.fired.contains("recall_promise"), "発火済みが latch される");
    }

    /// 【連鎖】効果が次の trigger の when を真化させ、同じ適用内で settle する。
    /// 好感度 30 → recall_promise → (promise_remembered) → renew_vow → goal 到達。
    #[test]
    fn trigger_cascade_settles_in_one_apply() {
        let sc = recall();
        let mut s = sc.initial_state(7);
        assert!(!is_goal(&s, &sc));

        let out = apply(&mut s, &sc, &raise_affection(30)).expect("好感度上昇は合法");

        // 一度の適用で 2 つの反応ビートが連鎖発火する。
        let ids: Vec<&str> = out.fired.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, vec!["recall_promise", "renew_vow"], "authored 順に連鎖発火");
        assert!(s.flag("vow_renewed"));
        assert!(is_goal(&s, &sc), "連鎖の果てに goal (vow_renewed) 到達");
    }

    /// 【閾値未満】条件が成立しなければ発火しない。
    #[test]
    fn trigger_does_not_fire_below_threshold() {
        let sc = recall();
        let mut s = sc.initial_state(7);
        let out = apply(&mut s, &sc, &raise_affection(20)).expect("好感度上昇は合法");
        assert!(out.fired.is_empty(), "好感度 20 では発火しない");
        assert!(!s.flag("promise_remembered"));
        assert!(s.fired.is_empty());
    }

    /// 【once / latch】一度発火した trigger は、when が真のままでも二度と発火しない。
    #[test]
    fn trigger_fires_at_most_once() {
        let sc = recall();
        let mut s = sc.initial_state(7);
        apply(&mut s, &sc, &raise_affection(30)).unwrap(); // 1 回目: 連鎖発火
        assert!(s.fired.contains("recall_promise") && s.fired.contains("renew_vow"));

        // さらに好感度を上げても (when は依然真) 再発火しない。
        let out = apply(&mut s, &sc, &raise_affection(5)).expect("好感度上昇は合法");
        assert!(out.fired.is_empty(), "latch 済みなので再発火しない");
    }

    /// 【純粋性】adjudicate は trigger を発火させない (state を一切変えない)。
    /// 発火は受理・適用後の apply の責務であり、裁定は純粋なまま。
    #[test]
    fn adjudicate_does_not_fire_triggers() {
        let sc = recall();
        let s = sc.initial_state(7);
        let v = adjudicate(&s, &sc, &raise_affection(30));
        assert!(v.is_accept(), "好感度上昇自体は受理される");
        assert!(!s.flag("promise_remembered"), "adjudicate は発火させない (純粋)");
        assert!(s.fired.is_empty(), "adjudicate は fired を変えない");
    }

    // -------------------------------------------------------------------------
    // NPC inventory + 譲渡 PoC: 持っていない物は渡せない (#23 の engine 側バックストップ)。
    // 所持物は閉世界・キャラ別。player は拾い、NPC は譲渡でのみ受け取る。
    // -------------------------------------------------------------------------

    /// 【正規の譲渡】花を摘んでアリスに渡すと、アリスの所持物に移り goal 到達。
    #[test]
    fn give_transfers_held_item() {
        let sc = gift();
        let mut s = sc.initial_state(7);
        apply(&mut s, &sc, &d(vec![StateOp::AddItem { item: "flower".into() }]))
            .expect("花は摘める");
        assert!(s.has_item(PLAYER, "flower"));
        apply(&mut s, &sc, &d(vec![StateOp::GiveItem {
            from: PLAYER.into(),
            to: "alice".into(),
            item: "flower".into(),
        }]))
        .expect("所持している花は渡せる");
        assert!(s.has_item("alice", "flower"), "アリスの所持物に移る");
        assert!(!s.has_item(PLAYER, "flower"), "player の手からは離れる");
        assert!(is_goal(&s, &sc), "goal (alice が flower を所持) 到達");
    }

    /// 【行商ネックレス遮断】所持していない物は渡せない (engine バックストップ)。
    #[test]
    fn cannot_give_unheld_item() {
        let sc = gift();
        let mut s = sc.initial_state(7);
        // 摘む前に渡そうとする。
        let delta = d(vec![StateOp::GiveItem {
            from: PLAYER.into(),
            to: "alice".into(),
            item: "flower".into(),
        }]);
        match adjudicate(&s, &sc, &delta) {
            Verdict::Reject { reasons } => assert!(reasons
                .iter()
                .any(|r| matches!(r, RejectReason::ItemNotHeld { item } if item == "flower"))),
            Verdict::Accept => panic!("持っていない物の譲渡を受理してはならない"),
        }
        assert!(apply(&mut s, &sc, &delta).is_err());
        assert!(!s.has_item("alice", "flower"), "却下なら誰の手にも渡らない");
    }

    /// 【幻のキャラ遮断】存在しない entity には渡せない (閉世界)。
    #[test]
    fn cannot_give_to_unknown_entity() {
        let sc = gift();
        let mut s = sc.initial_state(7);
        apply(&mut s, &sc, &d(vec![StateOp::AddItem { item: "flower".into() }])).unwrap();
        let v = adjudicate(&s, &sc, &d(vec![StateOp::GiveItem {
            from: PLAYER.into(),
            to: "ghost".into(),
            item: "flower".into(),
        }]));
        match v {
            Verdict::Reject { reasons } => assert!(reasons
                .iter()
                .any(|r| matches!(r, RejectReason::UnknownEntity { entity } if entity == "ghost"))),
            Verdict::Accept => panic!("幻のキャラへの譲渡を受理してはならない"),
        }
    }

    // -------------------------------------------------------------------------
    // 閉世界 capability PoC: 能力は宣言された閉じた集合。開花は authored トリガーのみ。
    // = メアリー・スー (その場で能力開花) の構造遮断。未宣言の力は存在しない。
    // -------------------------------------------------------------------------

    /// 【宣言】スキルはシナリオ宣言から読まれる (player=initial_skills, NPC=CharacterDef.skills)。
    #[test]
    fn skills_load_from_declaration() {
        let sc = awakening();
        let s = sc.initial_state(7);
        assert!(s.has_skill(PLAYER, "剣術"), "player の宣言済みスキル");
        assert!(s.has_skill("alice", "癒し"), "NPC の宣言済みスキル");
        assert!(!s.has_skill(PLAYER, "予知"), "未宣言/未開花の能力は存在しない");
    }

    /// 【能力 gate】予知を持たないうちは、予知 gate の扉を越えられない。
    #[test]
    fn has_skill_gate_blocks_without_skill() {
        let sc = awakening();
        let s = sc.initial_state(7);
        let v = adjudicate(&s, &sc, &d(vec![StateOp::Move { to: "beyond".into() }]));
        assert!(!v.is_accept(), "予知が無ければ beyond へ出られない");
    }

    /// 【メアリー・スー遮断】LLM が grant_skill で能力をその場で生やそうとしても却下される。
    #[test]
    fn llm_proposed_grant_skill_is_rejected() {
        let sc = awakening();
        let mut s = sc.initial_state(7);
        let delta = d(vec![StateOp::GrantSkill { entity: PLAYER.into(), skill: "予知".into() }]);
        match adjudicate(&s, &sc, &delta) {
            Verdict::Reject { reasons } => assert!(reasons.iter().any(|r| matches!(
                r,
                RejectReason::SkillGrantNotAllowed { skill, .. } if skill == "予知"
            ))),
            Verdict::Accept => panic!("LLM の能力開花を受理してはならない (メアリー・スー)"),
        }
        // apply も却下し、state は無傷 (予知は生えない)。
        assert!(apply(&mut s, &sc, &delta).is_err());
        assert!(!s.has_skill(PLAYER, "予知"));
        assert_eq!(s.turn, 0);
    }

    /// 【正規の開花】儀式 (フラグ) → トリガー grant_skill が予知を開花 → 予知 gate を越えて goal。
    /// 開花は authored トリガーの専権であり、その後の能力 gate が正しく通る (双対の正面)。
    #[test]
    fn trigger_awakens_skill_then_gate_passes() {
        let sc = awakening();
        let mut s = sc.initial_state(7);
        assert!(!is_goal(&s, &sc));

        // 儀式を行う → トリガー awaken_foresight が発火し予知を開花。
        let out = apply(&mut s, &sc, &d(vec![StateOp::SetFlag { key: "awakening_rite".into(), value: true }]))
            .expect("儀式は行える");
        assert!(out.fired.iter().any(|f| f.id == "awaken_foresight"), "トリガーが開花を起こす");
        assert!(s.has_skill(PLAYER, "予知"), "authored トリガーは能力を付与できる");

        // 今度は予知 gate の扉を越えられる。
        apply(&mut s, &sc, &d(vec![StateOp::Move { to: "beyond".into() }]))
            .expect("予知を得たので beyond へ出られる");
        assert!(is_goal(&s, &sc), "goal (beyond) 到達");
    }

    /// 【却下時は不発】不正 op を含むデルタは却下され、trigger も発火しない (原子性)。
    #[test]
    fn rejected_delta_fires_no_trigger() {
        let sc = recall();
        let mut s = sc.initial_state(7);
        // 好感度 +30 (単体なら閾値を跨ぐ) と未宣言 stat の不正 op を束ねる。
        let delta = d(vec![
            StateOp::AdjustStat { entity: "alice".into(), key: "好感度".into(), delta: 30 },
            StateOp::AdjustStat { entity: "alice".into(), key: "mana".into(), delta: 1 }, // 未宣言で不正
        ]);
        assert!(apply(&mut s, &sc, &delta).is_err());
        assert_eq!(s.stat_of("alice", "好感度"), 0, "却下なら好感度も動かない");
        assert!(!s.flag("promise_remembered"), "却下されたデルタは trigger を発火させない");
        assert!(s.fired.is_empty());
        assert_eq!(s.turn, 0);
    }
}
