//! campaign.rs — モジュールグラフの結線 (orchestration 層)。
//!
//! 三権分立に新しい権を足さない。gm_core が純粋関数で持つ二つの端
//! ([`Scenario::reached`] = 分岐セレクタ、[`Scenario::transition`] = 状態の糸通し) を、
//! **authored な地図 (campaigns/\*.yaml)** に従って繋ぐだけの脚。file I/O と地図参照ゆえ
//! engine ではなく harness の責務 (gm_core は file path を知らないまま不変)。
//!
//! 駆動は **LLM 非依存**: 「どのエンディングに着いたか (engine が決める GoalId)」と
//! 「その GoalId がどの辺を引くか (作者が書いた edges)」だけで次モジュールが決まる。
//! 帰結 (HP/所持品/能力/世界フラグ) は transition が運ぶ — 生成器は値を持てない。

use std::collections::BTreeMap;
use std::path::Path;

use gm_core::{GameState, GoalId, Scenario};
use serde::Deserialize;

use crate::error::HarnessError;
use crate::loader::inject_cast;

/// モジュール (= 自己完結 scenario) の識別子。campaign 内で一意。
pub type ModuleId = String;

/// campaigns/\*.yaml の凍結スキーマ: authored なモジュール接続トポロジ。
#[derive(Debug, Clone, Deserialize)]
pub struct Campaign {
    /// 表示用 (任意)。
    #[serde(default)]
    pub title: String,
    /// 開始モジュール id。
    pub start: ModuleId,
    /// モジュール id → scenario file path (repo root 相対)。
    pub modules: BTreeMap<ModuleId, String>,
    /// `(from, on_goal) → to` の分岐表。authored 順で最初に一致した辺を引く。
    #[serde(default)]
    pub edges: Vec<CampaignEdge>,
}

/// 「現モジュールであるエンディングに着いたら次モジュールへ」の一本の辺。
#[derive(Debug, Clone, Deserialize)]
pub struct CampaignEdge {
    /// 現モジュール。
    pub from: ModuleId,
    /// [`Scenario::reached`] が返したエンディング id。
    pub on_goal: GoalId,
    /// 遷移先モジュール。
    pub to: ModuleId,
}

/// [`advance_campaign`] が遷移を起こした時の結果 (次モジュールの骨格 + 糸通しした状態)。
#[derive(Debug, Clone)]
pub struct Advance {
    /// 遷移先モジュールの id。
    pub module_id: ModuleId,
    /// 次モジュールの骨格 (load + inject_cast + validate 済)。
    pub scenario: Scenario,
    /// `transition(prev_state, prev_scenario)` で状態を糸通しした次状態。
    pub state: GameState,
}

impl Campaign {
    pub fn from_yaml(s: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(s)
    }

    /// `(from, goal)` に対応する次モジュール id (authored 順で最初の一致)。
    /// 辺が無ければ `None` = 終端エンディング (この枝でキャンペーン完了)。純粋参照。
    pub fn next(&self, from: &str, goal: &str) -> Option<&ModuleId> {
        self.edges
            .iter()
            .find(|e| e.from == from && e.on_goal == goal)
            .map(|e| &e.to)
    }

    /// モジュール id に対応する scenario file path (repo root 相対)。
    pub fn module_path(&self, id: &str) -> Option<&str> {
        self.modules.get(id).map(|s| s.as_str())
    }
}

/// campaigns/\*.yaml を読む。
pub fn load_campaign(path: &Path) -> Result<Campaign, HarnessError> {
    let text = std::fs::read_to_string(path).map_err(|e| HarnessError::CampaignLoad {
        path: path.display().to_string(),
        detail: e.to_string(),
    })?;
    Campaign::from_yaml(&text).map_err(|e| HarnessError::CampaignLoad {
        path: path.display().to_string(),
        detail: e.to_string(),
    })
}

/// campaign のモジュール id を [`Scenario`] として load する。
///
/// read + `inject_cast` (cast 宣言した外部キャラを注入) + `validate` (authored 整合性を
/// load 時に弾く = 幻フラグ/幻 goal を実行経路に乗せない)。`root` は repo root
/// (scenarios/ characters/ memoria/ が在る所)。
pub fn load_module(campaign: &Campaign, root: &Path, id: &str) -> Result<Scenario, HarnessError> {
    let rel = campaign
        .module_path(id)
        .ok_or_else(|| HarnessError::CampaignLoad {
            path: id.to_string(),
            detail: format!("モジュール '{id}' が campaign の modules に無い"),
        })?;
    let path = root.join(rel);
    let text = std::fs::read_to_string(&path).map_err(|e| HarnessError::CampaignLoad {
        path: path.display().to_string(),
        detail: e.to_string(),
    })?;
    let mut scenario = Scenario::from_yaml(&text).map_err(|e| HarnessError::CampaignLoad {
        path: path.display().to_string(),
        detail: e.to_string(),
    })?;
    inject_cast(&mut scenario, &root.join("characters"))?;
    let errs = scenario.validate();
    if !errs.is_empty() {
        return Err(HarnessError::CampaignLoad {
            path: path.display().to_string(),
            detail: format!("scenario 整合性エラー: {errs:?}"),
        });
    }
    Ok(scenario)
}

/// **goal 到達後の campaign 前進** (reached → transition の結線本体)。
///
/// 現在の `(current_module, scenario, state)` を見て、[`Scenario::reached`] が返した
/// GoalId が campaign の辺に対応するなら、次モジュールを load して [`Scenario::transition`]
/// した [`Advance`] を返す。
///
/// - `reached()` が `None` (まだどのエンディングにも未到達) → `Ok(None)`
/// - 到達したが辺が無い (終端エンディング) → `Ok(None)` (呼び側が「キャンペーン完了」と判断)
/// - 辺が在る → 次モジュールを load + transition して `Ok(Some(Advance))`
///
/// state は読むだけ (遷移先 state は新規に作って返す)。LLM 非依存。
pub fn advance_campaign(
    campaign: &Campaign,
    root: &Path,
    current_module: &str,
    scenario: &Scenario,
    state: &GameState,
) -> Result<Option<Advance>, HarnessError> {
    // どのエンディングに着いたか (engine が決定論的に決める)。未到達なら前進しない。
    let Some(goal) = scenario.reached(state) else {
        return Ok(None);
    };
    // その GoalId がどの辺を引くか (作者の地図)。辺が無ければ終端エンディング。
    let Some(next_id) = campaign.next(current_module, &goal) else {
        return Ok(None);
    };
    let next_id = next_id.clone();
    // 次モジュールの骨格を load し、状態を持ち越して糸通しする (骨格だけ差し替え)。
    let next_scenario = load_module(campaign, root, &next_id)?;
    let next_state = next_scenario.transition(state, scenario);
    Ok(Some(Advance {
        module_id: next_id,
        scenario: next_scenario,
        state: next_state,
    }))
}

// =============================================================================
// PoC-2c: reached()→transition の結線を Red→Green で固める。
// 「大失敗 → どのエンディング(GoalId) → それが引く辺 → 次モジュールへ state を持ち越し遷移」。
// 駆動は state と地図だけ = LLM 非依存 (実 API 無しで orchestration の正しさを実証)。
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use gm_core::{apply, GameState, StateDelta, StateOp, PLAYER};
    use std::path::PathBuf;

    const ESCAPE: &str = include_str!("../../../packages/escape/campaign.yaml");

    /// escape パッケージの root (campaign.yaml / scenarios/ が在る所)。module path はここからの相対。
    fn repo_root() -> PathBuf {
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages/escape")).to_path_buf()
    }
    fn campaign() -> Campaign {
        Campaign::from_yaml(ESCAPE).expect("escape.yaml がパースできること")
    }
    fn d(ops: Vec<StateOp>) -> StateDelta {
        StateDelta::new("", ops)
    }

    /// 【地図の参照】edges が (from, on_goal) → to を authored 順で引ける。辺が無ければ None。
    #[test]
    fn campaign_parses_and_looks_up_edges() {
        let c = campaign();
        assert_eq!(c.start, "study");
        assert_eq!(c.next("study", "jammed_ending").map(String::as_str), Some("cellar"));
        assert_eq!(c.next("study", "opened_ending").map(String::as_str), Some("forest"));
        assert_eq!(c.next("study", "no_such_goal"), None, "未定義の goal は辺なし");
        assert_eq!(c.next("cellar", "jammed_ending"), None, "別モジュールの辺は引かない");
        assert_eq!(c.module_path("cellar"), Some("scenarios/cellar.yaml"));
    }

    /// 【結線の核心】大失敗で着いたエンディング(GoalId)が次モジュールを選び、
    /// 状態(hp/世界フラグ)を持ち越して遷移する。局所フラグと fired は捨てられる。
    #[test]
    fn advance_carries_state_across_fired_goal_branch() {
        let c = campaign();
        let root = repo_root();
        let study = load_module(&c, &root, "study").expect("study を load できる");

        // seed 19 で 1d6 → natural 1 (大失敗)。判定前に世界フラグと局所フラグを立てておく。
        let mut s = study.initial_state(19);
        apply(&mut s, &study, &d(vec![StateOp::SetFlag { key: "searched_study".into(), value: true }])).unwrap();
        apply(&mut s, &study, &d(vec![StateOp::SetFlag { key: "lamp_lit".into(), value: true }])).unwrap();
        apply(
            &mut s,
            &study,
            &d(vec![StateOp::AttemptChallenge {
                entity: PLAYER.into(),
                challenge: "drawer_pick".into(),
            }]),
        )
        .unwrap();

        // 大失敗 → fumble_drawer → trigger → drawer_jammed → reached() == jammed_ending。
        assert_eq!(
            study.reached(&s).as_deref(),
            Some("jammed_ending"),
            "大失敗が jammed_ending を選ぶ (これが次モジュールの分岐セレクタ)"
        );

        // orchestration: 発火 GoalId で辺を引き、次モジュールへ state を糸通しして遷移。
        let adv = advance_campaign(&c, &root, "study", &study, &s)
            .expect("advance は成功する")
            .expect("辺が在るので遷移が起きる");

        assert_eq!(adv.module_id, "cellar", "jammed_ending の辺で cellar へ");
        assert_eq!(adv.scenario.title, "地下蔵", "次モジュールの骨格に差し替わった");
        assert_eq!(adv.state.location, "cellar", "場所は次モジュールの start にリセット");
        assert_eq!(adv.state.stat("hp"), 8, "hp は持ち越し (cellar の宣言 12 を上書き=忘れない GM)");
        assert_eq!(
            adv.state.flags.get("searched_study"),
            Some(&true),
            "世界フラグ (study.global_flags) は持ち越す"
        );
        assert_eq!(adv.state.flags.get("lamp_lit"), None, "局所フラグは捨てる (その場限り)");
        assert_eq!(adv.state.flags.get("drawer_jammed"), None, "局所の帰結フラグも次モジュールには来ない");
        assert!(adv.state.fired.is_empty(), "発火済みトリガーはリセット (次モジュールの反応は新規)");
    }

    /// 【終端エンディング】到達したが辺が無いモジュールでは前進しない (キャンペーン完了)。
    #[test]
    fn advance_stops_at_terminal_ending() {
        let c = campaign();
        let root = repo_root();
        // cellar は goal: always (即到達) だが、cellar 発の辺は地図に無い = 終端。
        let cellar = load_module(&c, &root, "cellar").expect("cellar を load できる");
        let s = cellar.initial_state(1);
        assert!(cellar.reached(&s).is_some(), "cellar は goal=always で即到達");
        let adv = advance_campaign(&c, &root, "cellar", &cellar, &s).expect("成功する");
        assert!(adv.is_none(), "辺の無いエンディングでは遷移しない (終端=キャンペーン完了)");
    }

    /// 【未到達】どのエンディングにも未達なら前進しない。
    #[test]
    fn advance_does_nothing_before_goal() {
        let c = campaign();
        let root = repo_root();
        let study = load_module(&c, &root, "study").expect("study を load できる");
        let s = study.initial_state(19); // 何もしていない = 未到達
        assert_eq!(study.reached(&s), None, "開始時は未到達");
        let adv = advance_campaign(&c, &root, "study", &study, &s).expect("成功する");
        assert!(adv.is_none(), "未到達では遷移しない");
    }

    // 念のため: GameState の clone を経ない参照渡しで state を読むだけであることを型で固定。
    #[allow(dead_code)]
    fn _state_is_read_only(s: &GameState) -> &GameState {
        s
    }
}
