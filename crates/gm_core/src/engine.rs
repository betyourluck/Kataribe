//! 正本の裁定者。LLM の提案を裁き、受理時のみ原子的に state を更新する。

use serde::{Deserialize, Serialize};

use crate::spine::Scenario;
use crate::state::{GameState, StateDelta, StateOp};

/// 裁定結果。`Reject` は人間/LLM 双方に読める理由を含む。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    Accept,
    Reject { reasons: Vec<String> },
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
                reasons: vec![format!("現在地 '{}' がシナリオに存在しない", state.location)],
            };
        }
    };

    let mut reasons = Vec::new();

    for op in &delta.ops {
        match op {
            StateOp::AddItem { item } => {
                if state.has_item(item) {
                    reasons.push(format!("'{item}' は既に所持している"));
                    continue;
                }
                match loc.items.get(item) {
                    None => reasons.push(format!("'{item}' はこの場所には存在しない")),
                    Some(gate) => {
                        if !gate.eval(state) {
                            reasons.push(format!("'{item}' はまだ取得できない (前提条件が未達)"));
                        }
                    }
                }
            }
            StateOp::RemoveItem { item } => {
                if !state.has_item(item) {
                    reasons.push(format!("'{item}' を所持していないので手放せない"));
                }
            }
            StateOp::SetFlag { key, value } => {
                if !scenario.allowed_flags.contains(key) {
                    reasons.push(format!("フラグ '{key}' は許可されていない"));
                    continue;
                }
                if *value && !scenario.flag_gate(key).eval(state) {
                    reasons.push(format!("フラグ '{key}' を立てる前提条件が未達"));
                }
            }
            StateOp::Move { to } => match loc.exits.iter().find(|e| &e.to == to) {
                None => reasons.push(format!("'{to}' への出口は存在しない")),
                Some(exit) => {
                    if !exit.gate.eval(state) {
                        reasons.push(format!("'{to}' への移動条件が未達"));
                    }
                }
            },
            StateOp::RequestRoll { sides, dc: _ } => {
                if *sides < 1 {
                    reasons.push("ダイスの面数は1以上でなければならない".to_string());
                }
                // 出目はエンジンが振る。LLM は結果を主張できない (op 構造上不可能)。
            }
        }
    }

    if reasons.is_empty() {
        Verdict::Accept
    } else {
        Verdict::Reject { reasons }
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
        }
    }
    state.turn += 1;
    Ok(rolls)
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
    use crate::state::{RngState, StateOp};

    // 密室脱出シナリオをコンパイル時に埋め込む (cwd 非依存)。
    const LOCKED_ROOM: &str = include_str!("../../../scenarios/locked_room.yaml");

    fn scenario() -> Scenario {
        Scenario::from_yaml(LOCKED_ROOM).expect("locked_room.yaml がパースできること")
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
                assert!(reasons.iter().any(|r| r.contains("存在しない")));
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
}
