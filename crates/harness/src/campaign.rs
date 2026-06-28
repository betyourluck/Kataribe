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
use crate::package::{inject_package, PackageManifest};

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

/// **campaign のフラグ記憶** (spec 02 — 「その場所に持つ」の蓄積層)。
///
/// `ModuleId → (FlagKey → bool)`。各モジュールの `persistent_flags` の最新値を**モジュール別**に覚える。
/// gm_core の `transition` は二値 (global/局所) しか知らない — この層が独立に糸通しすることで
/// 「再訪したモジュールでだけフラグが蘇る」第三の値を実現する。namespace は `ModuleId` で分離
/// (A の `chest_opened` は B に漏れず、B の同名フラグとも衝突しない)。セッション保持 (save/load は後段)。
pub type CampaignMemory = BTreeMap<ModuleId, BTreeMap<String, bool>>;

/// 遷移**元**モジュールの `persistent_flags` を `state.flags` から読み、記憶に上書き保存する。
/// 設定済み (state.flags に在る) フラグだけ覚える (未設定は既定 false ＝覚える必要なし)。
fn harvest_persistent(
    memory: &mut CampaignMemory,
    module: &str,
    scenario: &Scenario,
    state: &GameState,
) {
    for flag in &scenario.persistent_flags {
        if let Some(&value) = state.flags.get(flag) {
            memory
                .entry(module.to_string())
                .or_default()
                .insert(flag.clone(), value);
        }
    }
}

/// 遷移**先**モジュールの記憶を `state.flags` に重ねる (再訪なら過去の場所フラグが蘇る)。
/// 初訪なら記憶が無く何も起きない。`transition` が局所として捨てた persistent フラグの復元点。
fn overlay_persistent(memory: &CampaignMemory, module: &str, state: &mut GameState) {
    if let Some(flags) = memory.get(module) {
        for (flag, &value) in flags {
            state.flags.insert(flag.clone(), value);
        }
    }
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
    build_module(campaign, root, id, None)
}

/// [`load_module`] の **package 注入版**。campaign-entry パッケージで使う。
///
/// `load_module` との差は `inject_package` を `inject_cast` の後・`validate` の前に挟むこと
/// (package の player/globals/world が各モジュールへ継承され、`global_flags ⊆ allowed_flags`
/// 閉世界検査を inject 後に走らせるので幻フラグ扱いされない順序)。
pub fn load_module_injected(
    campaign: &Campaign,
    root: &Path,
    manifest: &PackageManifest,
    id: &str,
) -> Result<Scenario, HarnessError> {
    build_module(campaign, root, id, Some(manifest))
}

/// モジュール scenario の組み立て本体。`manifest` を渡すと package を注入する
/// (`inject_cast` → (任意)`inject_package` → `validate` の順 = 注入後に閉世界検査)。
fn build_module(
    campaign: &Campaign,
    root: &Path,
    id: &str,
    manifest: Option<&PackageManifest>,
) -> Result<Scenario, HarnessError> {
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
    if let Some(m) = manifest {
        inject_package(&mut scenario, m); // player/globals/world を各モジュールへ継承
    }
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
/// `memory` = campaign のフラグ記憶 (spec 02)。遷移で更新される (遷移元を harvest・遷移先を overlay)。
pub fn advance_campaign(
    campaign: &Campaign,
    root: &Path,
    memory: &mut CampaignMemory,
    current_module: &str,
    scenario: &Scenario,
    state: &GameState,
) -> Result<Option<Advance>, HarnessError> {
    advance_with(campaign, root, None, memory, current_module, scenario, state)
}

/// [`advance_campaign`] の **package 注入版**。campaign-entry パッケージで使う
/// (遷移先モジュールにも package の player/globals/world を継承させる)。
pub fn advance_campaign_injected(
    campaign: &Campaign,
    root: &Path,
    manifest: &PackageManifest,
    memory: &mut CampaignMemory,
    current_module: &str,
    scenario: &Scenario,
    state: &GameState,
) -> Result<Option<Advance>, HarnessError> {
    advance_with(campaign, root, Some(manifest), memory, current_module, scenario, state)
}

/// goal 到達後の前進本体。`manifest` を渡すと遷移先モジュールへ package を注入する。
/// `memory` は spec 02 の場所フラグ蓄積: 遷移元を harvest し、遷移先を overlay する。
fn advance_with(
    campaign: &Campaign,
    root: &Path,
    manifest: Option<&PackageManifest>,
    memory: &mut CampaignMemory,
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
    // spec 02: 遷移元モジュールの場所フラグを記憶に刻む (その場所を出る瞬間の最新値)。
    harvest_persistent(memory, current_module, scenario, state);
    // 次モジュールの骨格を load し、状態を持ち越して糸通しする (骨格だけ差し替え)。
    let next_scenario = build_module(campaign, root, &next_id, manifest)?;
    let mut next_state = next_scenario.transition(state, scenario);
    // spec 02: 遷移先モジュールの場所フラグ記憶を復元 (再訪なら過去の状態が蘇る)。
    overlay_persistent(memory, &next_id, &mut next_state);
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
    const REVISIT: &str = include_str!("../fixtures/campaign_revisit.yaml");

    /// escape パッケージの root (campaign.yaml / scenarios/ が在る所)。module path はここからの相対。
    fn repo_root() -> PathBuf {
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages/escape")).to_path_buf()
    }
    fn campaign() -> Campaign {
        Campaign::from_yaml(ESCAPE).expect("escape.yaml がパースできること")
    }
    /// spec 02 の再訪サイクル fixture の root (village.yaml / forest.yaml が在る所)。
    fn revisit_root() -> PathBuf {
        Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures")).to_path_buf()
    }
    fn revisit_campaign() -> Campaign {
        Campaign::from_yaml(REVISIT).expect("campaign_revisit.yaml がパースできること")
    }
    const GLOBAL_CAMPAIGN: &str = include_str!("../fixtures/campaign_global.yaml");

    /// 【再現】ゲームプレイで set した global フラグが goal 遷移で次ステージへ持ち越されるか。
    /// 「goals で global_flags の値を true にして次ステージで反映されない」の切り分け。
    #[test]
    fn global_flag_set_in_play_carries_to_next_stage() {
        let c = Campaign::from_yaml(GLOBAL_CAMPAIGN).expect("campaign_global.yaml パース");
        let root = revisit_root();
        let a = load_module(&c, &root, "a").expect("stage_a を load");

        // ゲームプレイで cleared を立てる (set_flag op)。cleared は a の allowed_flags + global_flags。
        let mut s = a.initial_state(1);
        apply(&mut s, &a, &d(vec![StateOp::SetFlag { key: "cleared".into(), value: true }]))
            .expect("cleared は allowed なので受理");
        assert_eq!(s.flags.get("cleared"), Some(&true), "a で cleared=true");
        assert_eq!(a.reached(&s).as_deref(), Some("done"), "cleared で goal done に到達");

        // goal 到達 → 次ステージ b へ advance。global フラグが持ち越されるはず。
        let mut mem = CampaignMemory::new();
        let adv = advance_campaign(&c, &root, &mut mem, "a", &a, &s)
            .expect("advance 成功")
            .expect("辺があるので遷移");
        assert_eq!(adv.module_id, "b");
        assert_eq!(
            adv.state.flags.get("cleared"),
            Some(&true),
            "set した global フラグは次ステージへ持ち越される (反映されないならここで失敗)"
        );
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
        let mut mem = CampaignMemory::new();
        let adv = advance_campaign(&c, &root, &mut mem, "study", &study, &s)
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
        let mut mem = CampaignMemory::new();
        let adv = advance_campaign(&c, &root, &mut mem, "cellar", &cellar, &s).expect("成功する");
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
        let mut mem = CampaignMemory::new();
        let adv = advance_campaign(&c, &root, &mut mem, "study", &study, &s).expect("成功する");
        assert!(adv.is_none(), "未到達では遷移しない");
    }

    /// 【spec 02 核心】「その場所に持つ」フラグは再訪で蘇り、局所フラグは蘇らない。
    /// village で宝箱を開け松明を灯す → 森へ → 村へ**再訪**。宝箱(persistent)は覚えているが
    /// 松明(局所)は消え、森の局所フラグも漏れない。campaign 記憶 (CampaignMemory) が糸通しする。
    #[test]
    fn persistent_flag_survives_revisit_local_flag_does_not() {
        let c = revisit_campaign();
        let root = revisit_root();
        let mut mem = CampaignMemory::new();

        // --- 1) village: 宝箱を開け(persistent)・松明を灯し(局所)・村を出る合図を立てる ---
        let village = load_module(&c, &root, "village").expect("village を load できる");
        let mut s = village.initial_state(1);
        apply(&mut s, &village, &d(vec![StateOp::SetFlag { key: "chest_opened".into(), value: true }])).unwrap();
        apply(&mut s, &village, &d(vec![StateOp::SetFlag { key: "torch_lit".into(), value: true }])).unwrap();
        apply(&mut s, &village, &d(vec![StateOp::SetFlag { key: "leave_village".into(), value: true }])).unwrap();
        assert_eq!(village.reached(&s).as_deref(), Some("to_forest"));

        // village → forest。遷移元 village の persistent (chest_opened) を記憶へ harvest。
        let to_forest = advance_campaign(&c, &root, &mut mem, "village", &village, &s)
            .unwrap()
            .expect("辺が在る");
        assert_eq!(to_forest.module_id, "forest");
        assert_eq!(to_forest.state.flags.get("chest_opened"), None, "森には村の場所フラグは漏れない");
        assert_eq!(to_forest.state.flags.get("torch_lit"), None, "局所フラグは捨てられる");
        assert_eq!(mem["village"].get("chest_opened"), Some(&true), "village の場所フラグが記憶された");

        // --- 2) forest: 村へ戻る合図を立てる ---
        let forest = to_forest.scenario;
        let mut fs = to_forest.state;
        apply(&mut fs, &forest, &d(vec![StateOp::SetFlag { key: "return_village".into(), value: true }])).unwrap();
        assert_eq!(forest.reached(&fs).as_deref(), Some("to_village"));

        // forest → village (再訪)。遷移先 village の記憶を overlay → 宝箱が蘇る。
        let back = advance_campaign(&c, &root, &mut mem, "forest", &forest, &fs)
            .unwrap()
            .expect("戻り辺が在る");
        assert_eq!(back.module_id, "village", "戻り辺で village へ再訪");
        assert_eq!(
            back.state.flags.get("chest_opened"),
            Some(&true),
            "その場所に持つフラグは再訪で蘇る (宝箱はもう開いている)"
        );
        assert_eq!(back.state.flags.get("torch_lit"), None, "局所フラグは再訪でも消えたまま");
        assert_eq!(back.state.flags.get("return_village"), None, "森の局所フラグは村へ漏れない");
        assert_eq!(back.state.flags.get("leave_village"), None, "村の局所合図も復元されない (persistent でない)");
    }

    /// 【閉世界】`persistent_flags` が `allowed_flags` 未宣言なら validate が弾く (幻の場所フラグ)。
    #[test]
    fn validate_rejects_undeclared_persistent_flag() {
        let yaml = r#"
title: t
start: room
allowed_flags: [a]
persistent_flags: [ghost]
goal: { kind: always }
locations:
  room: { description: d, exits: [] }
"#;
        let sc = Scenario::from_yaml(yaml).unwrap();
        assert!(
            sc.validate().iter().any(|e| matches!(e,
                gm_core::ScenarioError::PersistentFlagUndeclared { flag } if flag == "ghost")),
            "未宣言の場所フラグを validate が弾く: {:?}",
            sc.validate()
        );
    }

    // 念のため: GameState の clone を経ない参照渡しで state を読むだけであることを型で固定。
    #[allow(dead_code)]
    fn _state_is_read_only(s: &GameState) -> &GameState {
        s
    }
}
