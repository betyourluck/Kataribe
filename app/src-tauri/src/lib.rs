//! Kataribe デスクトップ殻のバックエンド (Tauri2)。
//!
//! 役割は **harness のターンループを Tauri command で叩くだけ**。正本 (gm_core) も
//! ナレーター脚 (llm_client) も裁定結線 (harness) も一切変えない。CLI `play` を GUI に
//! 置き換えた「提示経路」であり、状態の真実は終始 backend (GameState) が握る。frontend は
//! 状態を持たず、command が返す view DTO を描画するだけ。
//!
//! command:
//! - `new_game(scenario_path?)`: シナリオ + characters + 伏線をロードし初期 state を作って session に格納
//! - `play_turn(action)`: session を lock し run_turn → 発火 recall を pending_lore に持ち越し → view を返す

mod site;
mod update;

use std::path::{Component, Path, PathBuf};

use gm_core::{is_goal, CheckOutcome, GameState, ImageMode, Lang, Scenario, ScenarioError, PLAYER};
use harness::{
    advance_campaign_injected, carryover_narration, chronicle_entry, is_campaign_entry,
    load_campaign_package, load_lore, load_module_injected, load_package, load_session,
    read_manifest, resolve_asset, resolve_recall, run_turn, save_session, AssetKind, Campaign,
    CampaignMemory, LoreStore, MemoryFragment, ModuleId, PackageManifest, SavedContent,
    SessionSave, Summarizer, Synopsis, SynopsisJob, TurnLog, TurnOutcome, SAVE_VERSION,
};
use llm_client::{CacheStat, LlmClient, LlmConfig};
use serde::{Deserialize, Serialize};
use tauri::Manager;
use tokio::sync::Mutex;

/// 1 ターンあたりの再生成上限 (CLI `play` と同値)。
const MAX_ATTEMPTS: u32 = 4;
/// 初期 RNG seed を決める。既定は**新しいゲームごとに変える** (時刻由来) — 固定 seed だと
/// 配役 (role_assignment) も出目列も毎回同一になる (実プレイ発見: 主人公が常に占い師)。
/// 再現したい時は `KATARIBE_SEED=42`。seed は RngState に保存されオートセーブにも残る。
fn resolve_seed() -> u64 {
    if let Ok(v) = std::env::var("KATARIBE_SEED") {
        if let Ok(n) = v.trim().parse::<u64>() {
            return n;
        }
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(42)
}
/// 既定パッケージ (リポジトリ root からの相対フォルダ)。
const DEFAULT_PACKAGE: &str = "packages/escape";

// =============================================================================
// frontend 向け view DTO (状態の真実ではなく、描画用スナップショット)
// =============================================================================

#[derive(Serialize)]
struct StatView {
    key: String,
    value: i64,
}

#[derive(Serialize)]
struct EntityView {
    id: String,
    stats: Vec<StatView>,
    /// 獲得済みの能力 (閉世界 capability)。ここに無い能力は存在しない。
    skills: Vec<String>,
    /// 所持物 (閉世界)。NPC は譲渡 (GiveItem) でのみ受け取る。
    items: Vec<String>,
    /// 文字列属性 (クラス/職業/種族 等。可変。トリガーで書き換わる)。
    attributes: Vec<StatStrView>,
    /// 設定・背景・性向 (authored の語り素材、CharacterDef.profile / Protagonist.profile)。
    /// プロフィールダイアログの本文。無ければ空。
    profile: String,
}

/// 文字列属性の 1 エントリ (key=value)。
#[derive(Serialize)]
struct StatStrView {
    key: String,
    value: String,
}

#[derive(Serialize)]
struct StateView {
    turn: u32,
    /// 現在地の LocationId (機械用セレクタ)。
    location: String,
    /// 現在地の表示名 (`Location.title`)。空なら frontend が id へフォールバック。
    location_title: String,
    inventory: Vec<String>,
    flags: Vec<FlagView>,
    entities: Vec<EntityView>,
    goal_reached: bool,
    /// このモジュールの名前付き goal (目標) の一覧 (authored 順)。
    /// プレイヤーに「何を目指せる盤面か」を示す (when/narration はネタバレゆえ出さない。
    /// hint は作者が意図的に開示する道しるべ)。単一 goal (無名・後方互換) のシナリオでは空。
    goals: Vec<GoalView>,
    /// 到達した goal の id (一覧のハイライト用)。未到達なら None。
    reached_goal: Option<String>,
}

/// フラグ一覧の 1 エントリ。title は表示名 (flag_titles、空なら frontend が key へフォールバック)、
/// cause は「何をして立ったか」= flag_turns (真化ターン) を chronicle の該当ターン要約と join した文。
#[derive(Serialize)]
struct FlagView {
    key: String,
    title: String,
    /// 立ったターン (flag_turns)。無ければ None (旧セーブ等)。
    turn: Option<u32>,
    /// 立った経緯 (chronicle の該当ターン要約)。無ければ None。
    cause: Option<String>,
}

/// 目標一覧の 1 エントリ (id + 表示名 + プレイヤー向けヒント)。
#[derive(Serialize)]
struct GoalView {
    id: String,
    /// 人間向けの表示名 (id は機械用セレクタ)。空なら frontend が id へフォールバック。
    title: String,
    /// 「何をすればだいたい行けるか」の authored ヒント。空なら表示なし。
    hint: String,
}

#[derive(Serialize)]
struct RollView {
    sides: u32,
    dc: u32,
    result: u32,
    success: bool,
}

/// 技能判定の結果 view。
#[derive(Serialize)]
struct CheckView {
    entity: String,
    stat: String,
    sides: u32,
    /// ダイス個数 (既定 1)。roll は素の合計 (3D6×5 系、2026-07-20)。
    count: u32,
    /// 出目の乗数 (既定 1)。total = 合計×times+修正。
    times: i64,
    roll: u32,
    modifier: i64,
    total: i64,
    dc: u32,
    success: bool,
    /// authored challenge の結末ナレーション (毎回・同ターン)。無ければ空。
    narration: String,
    /// authored challenge の結末効果音の絶対パス (frontend が convertFileSrc で URL 化 → one-shot 再生)。
    /// 無ければ None (未指定 or 解決失敗)。
    sound: Option<String>,
    /// d100 ロールアンダー判定の成功度 (spec 16)。critical/extreme/hard/regular/failure/fumble
    /// の機械 id (表示は frontend の言語表)。加算式判定は None。
    degree: Option<String>,
    /// spec 18 Phase B: プッシュ (振り直し) を経て確定した判定か。
    pushed: bool,
    /// spec 18 Phase B: 差分買いで支払った量 (0 = 買っていない)。
    spent: i64,
    /// spec 18 Phase B: 決断待ちで凍結中か (帰結未適用・結末文なし。決断 UI が続く)。
    pending: bool,
}

/// 差分買いの 1 段 (決断 UI のボタン素材。spec 18 Phase B)。
#[derive(Serialize)]
struct BuyOptionView {
    /// 買い上げ先: percentile = regular/hard/extreme、additive = success。
    degree: String,
    cost: i64,
    /// 支払い元 stat (表示用)。
    from: String,
    /// 支払い後の残量 (表示用 —「残 41」)。
    remaining: i64,
}

/// 決断待ちの判定と選択肢 (spec 18 Phase B)。TurnView/GameView に載り、frontend が
/// 開帳後に決断パネルを出す。決断が確定するまで次のターンは回せない。
#[derive(Serialize)]
struct DecisionView {
    challenge: String,
    entity: String,
    stat: String,
    can_push: bool,
    /// プッシュの代償 (stat, 量)。無償なら None。
    push_cost_from: Option<String>,
    push_cost_amount: Option<i64>,
    buys: Vec<BuyOptionView>,
}

/// 進行中の対決の view (spec 18 Phase C)。frontend の ContestPanel の素。
#[derive(Serialize)]
struct ContestView {
    contest: String,
    description: String,
    opponent: String,
    /// 相手の表示名 (CharacterDef.name。無ければ id)。
    opponent_name: String,
    rounds: u32,
    wins: u32,
    losses: u32,
    ties: u32,
}

fn contest_view(state: &GameState, scenario: &Scenario) -> Option<ContestView> {
    let p = state.pending_contest.as_ref()?;
    let def = scenario.contest(&p.contest)?;
    Some(ContestView {
        contest: p.contest.clone(),
        description: def.description.clone(),
        opponent: def.opponent.clone(),
        opponent_name: scenario
            .characters
            .get(&def.opponent)
            .map(|c| c.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| def.opponent.clone()),
        rounds: p.rounds,
        wins: p.wins,
        losses: p.losses,
        ties: p.ties,
    })
}

/// 先頭の決断待ちを view にする (無ければ None)。
fn decision_view(state: &GameState, scenario: &Scenario) -> Option<DecisionView> {
    let opts = gm_core::decision_options(state, scenario)?;
    Some(DecisionView {
        challenge: opts.pending.challenge.clone(),
        entity: opts.pending.entity.clone(),
        stat: opts.pending.stat.clone(),
        can_push: opts.can_push,
        push_cost_from: opts.push_cost.as_ref().map(|(f, _)| f.clone()),
        push_cost_amount: opts.push_cost.as_ref().map(|(_, a)| *a),
        buys: opts
            .buys
            .into_iter()
            .map(|b| BuyOptionView {
                remaining: state.stat_of(PLAYER, &b.from) - b.cost,
                degree: b.degree,
                cost: b.cost,
                from: b.from,
            })
            .collect(),
    })
}

/// 可変量ダイス (`roll_stat`) の監査 view (spec 16)。「SAN -4 (1d6=4)」の素材。
#[derive(Serialize)]
struct StatRollView {
    entity: String,
    key: String,
    count: u32,
    sides: u32,
    bonus: i64,
    rolls: Vec<u32>,
    amount: i64,
}

/// 発火した反応ビート + recall された伏線 (語りに織り込む素材)。
#[derive(Serialize)]
struct BeatView {
    narration: String,
    recalled: Vec<String>,
    /// 発火時のイベント CG の絶対パス (frontend が convertFileSrc で URL 化)。無ければ None。
    image: Option<String>,
    /// イベント CG の表示モード ("background" | "overlay")。未指定なら None (=background 扱い)。
    image_mode: Option<String>,
    /// 発火時の SE の絶対パス (frontend が convertFileSrc → one-shot 再生)。無ければ None。
    sound: Option<String>,
}

/// あらすじ 1 章の view (spec 10)。リスト key は upto_turn (title は表示専用)。
#[derive(Serialize, Clone)]
struct SynopsisView {
    upto_turn: u32,
    title: String,
    text: String,
}

/// 「最近の出来事」の 1 行 (未圧縮 chronicle の要約。あらすじタブの下段)。
#[derive(Serialize, Clone)]
struct LogLineView {
    turn: u32,
    summary: String,
}

fn synopsis_view(e: &harness::SynopsisEntry) -> SynopsisView {
    SynopsisView { upto_turn: e.upto_turn, title: e.title.clone(), text: normalize(&e.text) }
}
fn log_line_view(l: &TurnLog) -> LogLineView {
    LogLineView { turn: l.turn, summary: normalize(&l.summary) }
}

/// 開幕 view (new_game の戻り)。
#[derive(Serialize)]
struct GameView {
    title: String,
    location: String,
    description: String,
    state: StateView,
    /// 現在地の背景画像の絶対パス (frontend が convertFileSrc で URL 化)。無ければ None。
    background: Option<String>,
    /// 現在地のループ BGM の絶対パス (frontend が convertFileSrc → <audio loop>)。無ければ None。
    bgm: Option<String>,
    /// 現在地に居る NPC (顔アイコン行)。
    present_characters: Vec<CharacterView>,
    /// オートセーブから再開したときの再開情報 (spec 07 Phase C)。新規開始なら None。
    resumed: Option<ResumeView>,
    /// scenario の lint (非 fatal な作者向け警告。死んだ flag_hint 等)。開幕ログに ⚠ で出す。
    warnings: Vec<String>,
    /// あらすじ全量 (spec 10)。新規開始は空、再開はセーブから復元。以後は TurnView の差分で伸びる。
    synopsis: Vec<SynopsisView>,
    /// 「最近の出来事」= 未圧縮 chronicle の 1 行要約列 (再開時の初期表示。以後は差分で伸びる)。
    recent_log: Vec<LogLineView>,
    /// マップ (spec 15) — 訪問済み+1歩先の有向グラフ。
    map: MapView,
    /// 決断待ちの判定 (spec 18 Phase B)。再開時にセーブから復元される (決断はセーブを跨いで生きる)。
    decision: Option<DecisionView>,
    /// 進行中の対決 (spec 18 Phase C)。再開時にセーブから復元される。
    contest: Option<ContestView>,
    /// 約束事全量 (spec 20)。新規開始は空、再開はセーブから復元。
    facts: Vec<FactView>,
    /// 約束事のユーザー権限 (spec 20 Phase E): "open" | "prune" | "locked"。
    /// frontend は locked でタブごと隠し、prune で追加/編集 UI を出さない。
    facts_policy: String,
}

/// [`gm_core::FactsPolicy`] を frontend 向けの文字列へ。
fn facts_policy_str(p: gm_core::FactsPolicy) -> String {
    match p {
        gm_core::FactsPolicy::Open => "open",
        gm_core::FactsPolicy::Prune => "prune",
        gm_core::FactsPolicy::Locked => "locked",
    }
    .to_string()
}

/// 約束事の 1 行 (spec 20)。frontend の約束事タブと 📝 ログ行の素材。
#[derive(Serialize, Clone)]
struct FactView {
    id: u64,
    /// "gm" | "user" (UI バッジ)。
    origin: String,
    text: String,
    turn: u32,
    score: u32,
}

fn fact_views(list: &[harness::FactEntry]) -> Vec<FactView> {
    // 並びはスコア降順 (同点 id 昇順) = LLM 注入と同じ (見え方を一致させる)。
    harness::sorted_for_display(list)
        .into_iter()
        .map(|m| FactView {
            id: m.id,
            origin: match m.origin {
                harness::FactOrigin::Gm => "gm".into(),
                harness::FactOrigin::User => "user".into(),
            },
            text: m.text.clone(),
            turn: m.turn,
            score: m.score,
        })
        .collect()
}

/// scenario の lint を作者向けの表示文にする (非 fatal — load は拒否せず開幕に ⚠ で報せる。
/// fatal にすると配布済み content が受領側で死ぬので警告に留める)。
fn scenario_warnings(scenario: &Scenario) -> Vec<String> {
    scenario
        .lints()
        .into_iter()
        .map(|l| match l {
            ScenarioError::FlagHintOnAuthoredOnly { flag } => format!(
                "フラグ「{flag}」の flag_hint は GM に届きません（トリガー/challenge が立てる専権フラグのため）。\
                 GM に立てさせるならトリガー/challenge 側の set_flag を外し、筋書きで立てるならヒントを外してください"
            ),
            // spec 21 同梱: 幻の場所を指す location_is は永久に false = その Gate は死んでいる。
            ScenarioError::UnknownLocationInGate { origin, at } => format!(
                "{origin} の location_is が、宣言されていない場所「{at}」を指しています。\
                 この条件は永久に成立しません（挑戦なら一度も選べず、出口なら通れません）。\
                 locations に無い名前です — 所持品を対象にするなら has_item を使ってください"
            ),
            other => format!("{other:?}"),
        })
        .collect()
}

/// セーブから再開したときに frontend が開幕ログへ出す情報。
#[derive(Serialize)]
struct ResumeView {
    /// 再開時点のターン数。
    turn: u32,
    /// 前回までの語り (継続文脈のスナップショット。ログに「前回のあらすじ」として出す)。
    last_narration: String,
    /// 版不一致などの警告 (拒否はしない — content の軽微な修正でセーブを殺さない)。
    warnings: Vec<String>,
}

/// 1 ターンの結果 view (play_turn の戻り)。
#[derive(Serialize)]
struct TurnView {
    /// 受理されたか (false なら却下されつづけた)。
    accepted: bool,
    narration: String,
    rolls: Vec<RollView>,
    checks: Vec<CheckView>,
    /// 可変量ダイス (roll_stat) の監査記録 (spec 16)。
    stat_rolls: Vec<StatRollView>,
    beats: Vec<BeatView>,
    attempts: u32,
    /// 却下時の理由 (session.lang で localize 済み)。
    reasons: Vec<String>,
    /// 受理されたが、それまでに却下された各試行の理由 (試行順・localize 済み)。空なら一発合格。
    /// 「なぜ筋を通すのに N 回かかったか」を author に見せる (Grok 等で却下が多い時の診断)。
    retries: Vec<Vec<String>>,
    state: StateView,
    goal_reached: bool,
    /// 到達した名前付き goal の id (複数 goal のどれに達したか)。単一 goal/未到達なら None。
    goal_id: Option<String>,
    /// 到達 goal の表示名 (authored title)。空/未到達なら None (frontend は id へフォールバック)。
    goal_title: Option<String>,
    /// 到達 goal の結末ナレーション (authored)。空/未到達なら None。
    goal_narration: Option<String>,
    /// 現在地の背景画像の絶対パス (frontend が convertFileSrc で URL 化)。無ければ None。
    background: Option<String>,
    /// 現在地のループ BGM の絶対パス (frontend が convertFileSrc → <audio loop>)。無ければ None。
    bgm: Option<String>,
    /// 現在地に居る NPC (顔アイコン行)。
    present_characters: Vec<CharacterView>,
    /// campaign で次モジュールへ遷移したとき、遷移先モジュールの開幕情報。単発/未遷移なら None。
    /// このとき state/background/present_characters は**遷移先**を指す (goal_* は遷移元の結末)。
    transition: Option<TransitionView>,
    /// プロンプトキャッシュの健全性 (このセッションの累計)。frontend が連続 miss を検知して
    /// 「キャッシュ経路が壊れているかも」を警告する (#44/#45 — 漏出は usage が一次ソース)。
    cache: CacheStat,
    /// このターンで確定したあらすじ章の**追記差分** (spec 10)。append-only ゆえ frontend は
    /// push するだけ。通常 0〜1 件 (凍結リトライの強制消化と遷移圧縮が重なると 2 件)。
    new_synopsis: Vec<SynopsisView>,
    /// このターンで chronicle に積まれた行の差分 (「最近の出来事」用。通常 1 件、遷移ターンは
    /// 章替わりマーカー込みで 2 件、却下ターンは 0 件)。
    new_log: Vec<LogLineView>,
    /// エピローグ本文 (spec 11)。到達 goal に epilogue_prompt があり終端 (遷移しない) の
    /// 受理ターンだけ Some。生成失敗時は None (結末文 + バナーの従来表示 = フォールバック)。
    epilogue: Option<String>,
    /// 約束事 (spec 20): 変化があったターンだけ全量スナップショット (≤20 件で軽量。
    /// 強化のスコア変動も並び替えごと届く = frontend は差し替えるだけ)。無変化なら None。
    facts: Option<Vec<FactView>>,
    /// このターンで採用された GM 約束事行 (📝 表示。捨てられた行は載らない)。
    new_facts: Vec<String>,
    /// dedup 強化された既存行のテキスト (📝⁺ 表示 — silent なスコア変化を作らない)。
    reinforced_facts: Vec<String>,
    /// 約束事のユーザー権限 (spec 20 Phase E)。campaign 遷移で盤面が変われば追従する。
    facts_policy: String,
    /// マップ (spec 15) — 訪問済み+1歩先の有向グラフ。移動/遷移で変わるので毎ターン返す
    /// (却下ターンは state 不変ゆえ現状スナップショット)。
    map: MapView,
    /// 決断待ちの判定 (spec 18 Phase B)。Some の間、frontend は開帳後に決断パネルを出し、
    /// resolve_dice_decision が確定するまで入力を締める。
    decision: Option<DecisionView>,
    /// 進行中の対決 (spec 18 Phase C)。Some の間、frontend は ⚔ パネルを出し入力を締める。
    contest: Option<ContestView>,
}

/// campaign のモジュール遷移 (前モジュールの goal 到達 → 次モジュールへ state を糸通しして差し替え)。
#[derive(Serialize)]
struct TransitionView {
    /// 遷移先モジュールのタイトル。
    module_title: String,
    /// 遷移先の開始ロケーション id。
    location: String,
    /// 遷移先の開幕描写。
    description: String,
}

/// 現在地の背景画像を解決して絶対パス文字列にする (frontend が convertFileSrc で URL 化)。
/// gm_core は不透明 ID を持つだけ。ここ (提示層) が package_root を起点に解決する。
fn background_for(scenario: &Scenario, state: &GameState, root: &Path) -> Option<String> {
    let id = scenario.location(&state.location)?.image.as_ref()?;
    resolve_asset(root, AssetKind::Images, id).map(|p| p.to_string_lossy().into_owned())
}

/// 現在地のループ BGM を解決して絶対パス文字列にする (frontend が convertFileSrc で URL 化)。
/// `images` でなく `audios` フォルダから引く以外は `background_for` と同経路。無ければ None。
fn bgm_for(scenario: &Scenario, state: &GameState, root: &Path) -> Option<String> {
    let id = scenario.location(&state.location)?.bgm.as_ref()?;
    resolve_asset(root, AssetKind::Audios, id).map(|p| p.to_string_lossy().into_owned())
}

/// 顔アイコン行の 1 キャラ。`icon` は解決済み絶対パス (無ければ None → frontend が initials)。
#[derive(Serialize)]
struct CharacterView {
    id: String,
    name: String,
    icon: Option<String>,
}

/// 現在地に「いる」キャラを顔アイコン行用に列挙する (presence)。
/// 先頭に主人公 (player) を常に置き、続けて現在地の NPC (`present` が非空ならそれ、空なら全 characters)。
fn present_characters(scenario: &Scenario, state: &GameState, root: &Path) -> Vec<CharacterView> {
    let resolve_icon = |icon: &Option<String>| -> Option<String> {
        icon.as_ref()
            .and_then(|i| resolve_asset(root, AssetKind::Images, i))
            .map(|p| p.to_string_lossy().into_owned())
    };
    // 主人公は常にこの場に居る。名前は protagonist.name (無ければ "あなた")。
    let player_name = if scenario.protagonist.name.trim().is_empty() {
        "あなた".to_string()
    } else {
        scenario.protagonist.name.clone()
    };
    let mut out = vec![CharacterView {
        id: PLAYER.to_string(),
        name: player_name,
        icon: resolve_icon(&scenario.protagonist.icon),
    }];
    // 現在地の実効 NPC presence (場所ベース ± present_overrides、spec 04)。トリガーで登場/退場した結果を反映。
    for id in scenario.present_at(state) {
        if let Some(def) = scenario.characters.get(&id) {
            out.push(CharacterView {
                id: id.clone(),
                name: if def.name.is_empty() { id.clone() } else { def.name.clone() },
                icon: resolve_icon(&def.icon),
            });
        }
    }
    out
}

/// 到達した名前付き goal の (id, 表示名, 結末ナレーション) を view 用に取り出す。
fn goal_view(
    state: &GameState,
    scenario: &Scenario,
) -> (Option<String>, Option<String>, Option<String>) {
    match scenario.reached_goal(state) {
        Some(g) => (
            Some(g.id.clone()),
            (!g.title.trim().is_empty()).then(|| g.title.clone()),
            (!g.narration.trim().is_empty()).then(|| normalize(&g.narration)),
        ),
        None => (None, None, None),
    }
}

// =============================================================================
// マップ (spec 15) — 訪問済み+1歩先の有向グラフ (engine 無改修の派生表示)
// =============================================================================

/// マップの 1 ノード (ロケーション)。`visited=false` は frontier (未踏の1歩先)。
/// **frontier はネタバレ回避で title/description/image を伏せる** (frontend が「？」表示)。
#[derive(Serialize)]
struct MapNode {
    id: String,
    /// 表示名 (Location.title、空なら frontend が id へフォールバック)。frontier は空。
    title: String,
    /// 場所の説明 (クリックで詳細パネルに出す)。frontier は空 (未踏)。
    description: String,
    /// 場所のイベント CG/背景画像の絶対パス (frontend が convertFileSrc で URL 化)。
    /// visited かつ Location.image があるときのみ。frontier は None。
    image: Option<String>,
    /// 現在地か。
    current: bool,
    /// 訪問済みか (false = frontier = 未踏の1歩先。丸だけ描き「？」で示す)。
    visited: bool,
}

/// マップの 1 辺 (有向の出口)。`locked=true` は gate 未達 (🔒・今は通れない)。
#[derive(Serialize)]
struct MapEdge {
    from: String,
    to: String,
    locked: bool,
}

/// 右ペインのマップ view (spec 15)。
#[derive(Serialize)]
struct MapView {
    nodes: Vec<MapNode>,
    edges: Vec<MapEdge>,
}

/// 訪問済み + その1歩先だけを描く有向グラフを組む (可視範囲=霧、ユーザー確定)。
/// **engine 無改修の派生**: `Scenario.exits` (グラフ) + `GameState` (現在地・Gate::eval) +
/// chronicle (`TurnLog.location`=訪問済み) から導く。訪問済みは history 由来ゆえ正本に
/// `visited` を足さない。gate 評価は既存 `Gate::eval`。
///
/// - visited = {start, 現在地} ∪ {history の location}、現 scenario の location に限定
///   (campaign 遷移で前モジュールの location が history に残るのを除外)。
/// - frontier = visited の各出口先 (1歩先・未訪問でも名前を出す)。奥は霧 = 出さない。
/// - edges = visited ノードの exits のみ (frontier からの辺は描かない = その先は霧)。
fn map_view(
    scenario: &Scenario,
    state: &GameState,
    history: &[TurnLog],
    root: &Path,
) -> MapView {
    use std::collections::BTreeSet;
    // 訪問済み: 現 scenario に実在する location のみ採る (幻ノード・他モジュールを除外)。
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mark = |id: &str, set: &mut BTreeSet<String>| {
        if scenario.location(id).is_some() {
            set.insert(id.to_string());
        }
    };
    mark(&scenario.start, &mut visited);
    mark(&state.location, &mut visited);
    for log in history {
        if !log.location.is_empty() {
            mark(&log.location, &mut visited);
        }
    }
    // visited から出る辺を集め、行き先の未訪問を frontier (1歩先) に積む。
    let mut frontier: BTreeSet<String> = BTreeSet::new();
    let mut edges: Vec<MapEdge> = Vec::new();
    for from in &visited {
        let Some(loc) = scenario.location(from) else { continue };
        for exit in &loc.exits {
            // 行き先が現 scenario に無いなら描かない (幻ノードを作らない)。
            if scenario.location(&exit.to).is_none() {
                continue;
            }
            if !visited.contains(&exit.to) {
                frontier.insert(exit.to.clone());
            }
            edges.push(MapEdge {
                from: from.clone(),
                to: exit.to.clone(),
                locked: !exit.gate.eval(state),
            });
        }
    }
    // ノード = visited ∪ frontier (両者は互いに素)。決定論順 (visited→frontier、各キー昇順)。
    // visited は名前/説明/画像を載せる (クリックで詳細パネルへ)。frontier は伏せる
    // (「？」+「まだ到達していない」= ネタバレ回避、可視範囲=霧の一貫)。
    let node = |id: &String, is_visited: bool| {
        let loc = scenario.location(id);
        let (title, description, image) = if is_visited {
            (
                loc.map(|l| if l.title.is_empty() { id.clone() } else { l.title.clone() })
                    .unwrap_or_else(|| id.clone()),
                loc.map(|l| normalize(&l.description)).unwrap_or_default(),
                loc.and_then(|l| l.image.as_ref())
                    .and_then(|im| resolve_asset(root, AssetKind::Images, im))
                    .map(|p| p.to_string_lossy().into_owned()),
            )
        } else {
            (String::new(), String::new(), None)
        };
        MapNode {
            id: id.clone(),
            title,
            description,
            image,
            current: *id == state.location,
            visited: is_visited,
        }
    };
    let nodes = visited
        .iter()
        .map(|id| node(id, true))
        .chain(frontier.iter().map(|id| node(id, false)))
        .collect();
    MapView { nodes, edges }
}

// =============================================================================
// session (backend が握る可変の真実。sled のような排他資源ではないので manage 可)
// =============================================================================

struct GameSession {
    state: GameState,
    scenario: Scenario,
    lore: LoreStore,
    client: LlmClient,
    /// 直前ターンの発火で recall された伏線。次ターンの語りに注入する (memoria_bridge)。
    pending_lore: Vec<MemoryFragment>,
    /// 直前ターンの技能判定の結果。次ターンの語りに還流する。
    pending_checks: Vec<CheckOutcome>,
    /// 直前ターンの語り。次ターンに「続く情景」として渡し、既出描写の繰り返しを防ぐ (継続性)。
    last_narration: String,
    /// 経緯ログ (chronicle)。GM の summary を蓄積し「これまでの経緯」として還流する (中期記憶)。
    history: Vec<TurnLog>,
    lang: Lang,
    /// パッケージのフォルダ (アセット解決の起点)。`images/{id}` 等をここから解決する。
    package_root: PathBuf,
    /// campaign-entry パッケージなら地図 (モジュール接続トポロジ)。単発シナリオなら None。
    campaign: Option<Campaign>,
    /// 現在のモジュール id (campaign 時のみ意味を持つ)。advance の `from`。
    current_module: ModuleId,
    /// campaign の場所フラグ記憶 (spec 02)。再訪したモジュールで persistent フラグを復元する。
    campaign_memory: CampaignMemory,
    /// package manifest (campaign 前進で遷移先モジュールへ player/globals/world を継承させるのに要る)。
    manifest: PackageManifest,
    /// オートセーブの書き先 (app data dir。解決不能なら None = セーブ無効で続行)。spec 07 Phase C。
    save_path: Option<PathBuf>,
    /// 起動時に指定されたパッケージパス (SessionSave.content に刻む再ロード参照)。
    package_path: String,
    /// あらすじ (spec 10)。圧縮済み章 + 遷移契機の凍結リトライ範囲。セーブ対象。
    synopsis: Synopsis,
    /// あらすじ要約用の専用 client (SUMMARY_LLM_*)。None なら GM の client を共用。
    summarizer: Option<LlmClient>,
    /// 約束事 (spec 20)。正本の外の覚え書き。セーブ対象、campaign 遷移でも持ち越す。
    facts: Vec<harness::FactEntry>,
}

/// new_game 前は None。
type SharedSession = Mutex<Option<GameSession>>;

// =============================================================================
// helpers
// =============================================================================

/// リポジトリ root (scenarios/ characters/ memoria/ が在る所)。
/// `app/src-tauri` からコンパイル時に解決 (cwd 非依存)。
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// LLM 設定 (.env) の永続化先: `app_data_dir/.env`。saves/logs/packages と同じ per-user
/// 書込可能な場所で、**配布 exe でも書ける**。旧 `repo_root()/.env` はビルド時に焼いた開発パス
/// (CARGO_MANIFEST_DIR/../..) で、インストール版では存在せず保存に失敗していた (spec 07 の
/// app_data_dir 移行漏れ)。`set_llm_config` が書き、起動時 setup がここから読む。
fn config_env_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join(".env"))
}

/// パスを字句的に正規化する (`.`/`..` を畳み、native 区切りに統一)。
/// **Tauri asset protocol は `..` を含むパスを 403 で拒否する** (トラバーサル防止) ため、
/// scope 許可・アセット解決の前に絶対パスを綺麗にしておく (repo_root が `../..` を残すのが原因)。
fn normalize_path(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// =============================================================================
// 会話ログのテキスト保存 (ユーザーFB 2026-07-09)
// =============================================================================

/// ログの既定置き場: `app_data_dir/logs`。設定でフォルダを指定しなければここへ書く。
fn default_log_dir(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join("logs"))
}

/// 有効なログフォルダを解決する (指定が空なら既定へ)。
fn resolve_log_dir(app: &tauri::AppHandle, folder: &str) -> Result<PathBuf, String> {
    if folder.trim().is_empty() {
        default_log_dir(app).ok_or_else(|| "アプリデータ置き場を解決できません".to_string())
    } else {
        Ok(PathBuf::from(folder.trim()))
    }
}

/// 既定ログフォルダのパス (設定ダイアログの placeholder 表示用)。
#[tauri::command]
fn get_default_log_dir(app: tauri::AppHandle) -> String {
    default_log_dir(&app).map(|d| d.to_string_lossy().into_owned()).unwrap_or_default()
}

/// 会話ログを 1 テキストファイルへ保存する。ファイル名は frontend が組む
/// (日時 + パッケージ名。locale-aware な日時整形は JS 側が得意)。返り値は書いた絶対パス。
/// **ファイル名はパス要素を含まない単一名に限る** (トラバーサル遮断 = zip 展開と同じ思想)。
#[tauri::command]
fn save_log_file(
    app: tauri::AppHandle,
    folder: String,
    file_name: String,
    content: String,
) -> Result<String, String> {
    if file_name.is_empty()
        || file_name.contains('/')
        || file_name.contains('\\')
        || file_name.contains("..")
    {
        return Err("不正なファイル名です".to_string());
    }
    let dir = resolve_log_dir(&app, &folder)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("フォルダを作成できません: {e}"))?;
    let path = dir.join(&file_name);
    std::fs::write(&path, content).map_err(|e| format!("書き込みに失敗しました: {e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

/// パッケージフォルダを OS のフォルダ選択ダイアログで選ばせる (パッケージ一覧の「参照」)。
/// tauri-plugin-dialog を足さず rfd (ネイティブダイアログ) を custom command で完結させる
/// (open_log_folder が tauri-plugin-shell を避けたのと同じ「追加権限ゼロ」方針)。**同期コマンド
/// ゆえメインスレッドで走る** (Tauri v2) — モーダルダイアログのメッセージポンプが正しく回る。
/// `start` が実在フォルダなら初期ディレクトリに使う (前回追加したパッケージの親フォルダ)。
/// 返り値は選ばれた絶対パス、キャンセルなら None。
#[tauri::command]
fn pick_package_folder(start: Option<String>) -> Option<String> {
    let mut dialog = rfd::FileDialog::new().set_title("パッケージフォルダを選択");
    if let Some(dir) = start
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(Path::new)
        .filter(|p| p.is_dir())
    {
        dialog = dialog.set_directory(dir);
    }
    dialog.pick_folder().map(|p| p.to_string_lossy().into_owned())
}

/// ログフォルダを OS のファイルマネージャで開く (設定ダイアログのボタン)。
#[tauri::command]
fn open_log_folder(app: tauri::AppHandle, folder: String) -> Result<(), String> {
    let dir = resolve_log_dir(&app, &folder)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("フォルダを作成できません: {e}"))?;
    open_in_file_manager(&dir)
}

/// フォルダを OS 標準のファイルマネージャで開く (プラットフォーム別)。
fn open_in_file_manager(dir: &Path) -> Result<(), String> {
    os_open(&dir.to_string_lossy()).map_err(|e| format!("フォルダを開けません: {e}"))
}

/// フォルダ or URL を OS 標準ハンドラ (ファイルマネージャ / 既定ブラウザ) で開く。
/// tauri-plugin-shell を足さず std::process で完結させる (custom command ゆえ追加権限不要)。
/// Windows の `explorer <url>` / macOS の `open <url>` / Linux の `xdg-open <url>` は
/// いずれも URL を既定ブラウザへ委譲する。explorer は成功時も非0を返すので spawn の成否だけ見る。
fn os_open(target: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let program = "xdg-open";
    std::process::Command::new(program)
        .arg(target)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("開けません: {e}"))
}

/// URL を既定ブラウザで開く (更新通知のクリック等)。http/https のみ受理。
/// URL は呼び出し側 (フロント) が持つ設定値 = 配布サイト。API 応答由来の URL は開かない
/// (攻撃者が誘導する外部 URL を踏まない — 開くのは常にユーザーが登録した siteUrl)。
#[tauri::command]
fn open_external_url(url: String) -> Result<(), String> {
    let u = url.trim();
    if !(u.starts_with("http://") || u.starts_with("https://")) {
        return Err("http:// または https:// の URL のみ開けます".to_string());
    }
    os_open(u)
}

/// 手動セーブスロットの本数 (spec 07 Phase D)。スロット番号は 1..=SAVE_SLOTS。
const SAVE_SLOTS: u8 = 5;

/// パッケージ dir の安定キー (FNV-1a 64bit hex)。オートセーブ/スロットのファイル名 stem。
/// 依存ゼロ・プロセス/バージョン非依存の安定ハッシュ。
fn package_save_stem(pkg_dir: &Path) -> String {
    let key = pkg_dir.to_string_lossy();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// セーブのファイル名 (拡張子込み)。`slot=None` はオートセーブ、`Some(n)` は手動スロット n。
/// 純関数 = PoC でスロット同士・オートセーブとの非衝突を固定する。
fn save_file_name(stem: &str, slot: Option<u8>) -> String {
    match slot {
        None => format!("{stem}.yaml"),
        Some(n) => format!("{stem}_slot{n}.yaml"),
    }
}

/// セーブの置き場 (`app_data_dir/saves`)。app data dir = OS 標準のアプリデータ置き場 —
/// 配布 zip を差し替えてもセーブが消えない (パッケージフォルダを汚さない)。
fn saves_dir(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join("saves"))
}

/// パッケージ別オートセーブのパス (spec 07 Phase C): `saves/<パスのFNVハッシュ>.yaml`。
/// パッケージ別 1 autosave (最新進捗)。
fn autosave_path(app: &tauri::AppHandle, pkg_dir: &Path) -> Option<PathBuf> {
    Some(saves_dir(app)?.join(save_file_name(&package_save_stem(pkg_dir), None)))
}

/// 手動セーブスロットのパス (spec 07 Phase D): `saves/<ハッシュ>_slot{n}.yaml`。
/// autosave と同じ器 (SessionSave) を別名で書くだけ — 「気に入ったシーン」の凍結点。
fn slot_save_path(app: &tauri::AppHandle, pkg_dir: &Path, slot: u8) -> Option<PathBuf> {
    Some(saves_dir(app)?.join(save_file_name(&package_save_stem(pkg_dir), Some(slot))))
}

/// package_path (repo 相対 or 絶対) を正規化済み絶対パスへ解決する
/// (セーブのハッシュ・パッケージロードの起点。`..` は asset protocol が拒否するので畳む)。
fn resolve_pkg_dir(rel: &str) -> PathBuf {
    let p = Path::new(rel);
    let dir = if p.is_absolute() { PathBuf::from(rel) } else { repo_root().join(rel) };
    normalize_path(&dir)
}

/// 手動セーブスロット一覧の 1 項目 (スロットダイアログ表示用、spec 07 Phase D)。
#[derive(Serialize)]
struct SlotView {
    /// スロット番号 (1..=SAVE_SLOTS)。
    slot: u8,
    /// セーブが存在するか (false なら空きスロット、以下のメタは零値)。
    exists: bool,
    /// セーブ時点のターン数。
    turn: u32,
    /// 保存日時 (file mtime, epoch ms)。frontend が locale 表示する。取れなければ None。
    saved_at_ms: Option<u64>,
    /// 直前の語りの冒頭 (シーン識別の手がかり。「気に入ったシーン」を探す目印)。
    snippet: String,
}

/// 語りの冒頭をスロット一覧用に整形する (60 字 + …。改行は空白へ、char 境界安全)。
fn narration_snippet(s: &str) -> String {
    let t = normalize(s).replace('\n', " ");
    let t = t.trim();
    let mut out: String = t.chars().take(60).collect();
    if t.chars().count() > 60 {
        out.push('…');
    }
    out
}

/// スロット 1 本の view を作る。読めない/版不一致は「空き」扱い (寛容 — autosave_turn と同基準)。
fn slot_view(app: &tauri::AppHandle, pkg_dir: &Path, slot: u8) -> SlotView {
    let path = slot_save_path(app, pkg_dir, slot);
    let loaded = path.as_deref().and_then(|p| load_session(p).ok());
    match loaded {
        Some(save) => SlotView {
            slot,
            exists: true,
            turn: save.state.turn,
            saved_at_ms: path
                .as_deref()
                .and_then(|p| std::fs::metadata(p).ok())
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64),
            snippet: narration_snippet(&save.last_narration),
        },
        None => SlotView { slot, exists: false, turn: 0, saved_at_ms: None, snippet: String::new() },
    }
}

/// 却下理由の表示言語。`KATARIBE_LANG=en` で英語、既定は日本語 (CLI と同値)。
fn lang_from_env() -> Lang {
    match std::env::var("KATARIBE_LANG").as_deref() {
        Ok("en") | Ok("En") | Ok("EN") => Lang::En,
        _ => Lang::Ja,
    }
}

/// narration の literal `\n` を実改行へ正規化する (failures #16。提示層の責務、正本は触らない)。
fn normalize(s: &str) -> String {
    s.replace("\\n", "\n")
}

fn state_view(state: &GameState, scenario: &Scenario, history: &[TurnLog]) -> StateView {
    // stat / skill / 所持物 のいずれかを持つ entity の和集合。
    let ids: std::collections::BTreeSet<&String> = state
        .entities
        .keys()
        .chain(state.skills.keys())
        .chain(state.inventory.keys())
        .chain(state.attributes.keys())
        .collect();
    let entities = ids
        .into_iter()
        .map(|id| EntityView {
            id: id.clone(),
            stats: {
                // authored 宣言順 (YAML の記述順) で並べる。実行時 GameState は BTreeMap で
                // 順序を持たないので、Scenario::stat_order (initial_stats/CharacterDef.stats の
                // 記述順) を参照する。プレイヤー UI からは hidden_stats (GM は見る秘密) も
                // internal_stats (engine 帳簿) も両方隠す (プレイヤーから見れば同一 = 非表示)。
                let m = state.entities.get(id);
                let order = scenario.stat_order(id);
                let ui_hidden =
                    |k: &String| scenario.hidden_stats.contains(k) || scenario.internal_stats.contains(k);
                let mut out: Vec<StatView> = Vec::new();
                for k in &order {
                    if ui_hidden(k) {
                        continue;
                    }
                    if let Some(v) = m.and_then(|m| m.get(k)) {
                        out.push(StatView { key: k.clone(), value: *v });
                    }
                }
                // 宣言に無い runtime stat (role_assignment の帳簿等) は末尾に (BTreeMap 順で安定)。
                if let Some(m) = m {
                    for (k, v) in m {
                        if !order.contains(k) && !ui_hidden(k) {
                            out.push(StatView { key: k.clone(), value: *v });
                        }
                    }
                }
                out
            },
            skills: state
                .skills
                .get(id)
                .map(|s| s.iter().cloned().collect())
                .unwrap_or_default(),
            items: state
                .inventory
                .get(id)
                .map(|s| s.iter().cloned().collect())
                .unwrap_or_default(),
            attributes: {
                // stats と同じく authored 宣言順 (Scenario::attribute_order)。
                // secret 属性 (役職等, spec 06) はプレイヤー UI では本人分のみ (NPC 分は DTO 段階で
                // 落とす)。hidden 属性 (本人未知) は本人分も含め全員分落とす (当人すら知らない正体・
                // 呪いを UI が漏らさない — GM prompt だけが見る)。
                let m = state.attributes.get(id);
                let order = scenario.attribute_order(id);
                let visible = |k: &String| {
                    !scenario.hidden_attributes.contains(k)
                        && (id == PLAYER || !scenario.secret_attributes.contains(k))
                };
                let mut out: Vec<StatStrView> = Vec::new();
                for k in &order {
                    if !visible(k) {
                        continue;
                    }
                    if let Some(v) = m.and_then(|m| m.get(k)) {
                        out.push(StatStrView { key: k.clone(), value: v.clone() });
                    }
                }
                if let Some(m) = m {
                    for (k, v) in m {
                        if !order.contains(k) && visible(k) {
                            out.push(StatStrView { key: k.clone(), value: v.clone() });
                        }
                    }
                }
                out
            },
            profile: if id == PLAYER {
                scenario.protagonist.profile.clone()
            } else {
                scenario
                    .characters
                    .get(id)
                    .map(|c| c.profile.clone())
                    .unwrap_or_default()
            },
        })
        .collect();
    // 隠しゴール (visible: false) は到達するまで一覧に出さない (到達で開示され ✓ 付きで現れる)。
    // DTO 自体から落とす = 提示層より手前でネタバレを断つ (when/narration を出さないのと同じ衛生)。
    let reached = scenario.reached(state);
    StateView {
        turn: state.turn,
        location: state.location.clone(),
        // 表示名 (authored title)。空なら frontend が id へフォールバック (FlagView と同じ流儀)。
        location_title: scenario
            .location(&state.location)
            .map(|l| l.title.clone())
            .unwrap_or_default(),
        // 上段の「所持品」は主人公の物 (NPC の所持物は登場人物ブロックに出る)。
        inventory: state
            .inventory
            .get(PLAYER)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default(),
        flags: state
            .flags
            .iter()
            // プレイヤー UI からは hidden_flags (GM は見る秘密) も internal_flags (engine 帳簿) も
            // 両方隠す (プレイヤーから見れば同一 = 非表示)。
            .filter(|(k, v)| {
                **v && !scenario.hidden_flags.contains(*k) && !scenario.internal_flags.contains(*k)
            })
            .map(|(k, _)| {
                // 真化ターン (正本) と chronicle (経緯ログ) を join して「何をして立ったか」を出す。
                let turn = state.flag_turns.get(k).copied();
                let cause = turn.and_then(|n| {
                    history.iter().find(|log| log.turn == n).map(|log| log.summary.clone())
                });
                FlagView {
                    key: k.clone(),
                    title: scenario.flag_titles.get(k).cloned().unwrap_or_default(),
                    turn,
                    cause,
                }
            })
            .collect(),
        entities,
        goal_reached: is_goal(state, scenario),
        goals: scenario
            .goals
            .iter()
            .filter(|g| g.visible || reached.as_deref() == Some(g.id.as_str()))
            .map(|g| GoalView { id: g.id.clone(), title: g.title.clone(), hint: g.hint.clone() })
            .collect(),
        reached_goal: reached,
    }
}

// =============================================================================
// Tauri commands
// =============================================================================

/// パッケージ一覧の1項目 (GUI のフォルダ一覧表示用)。frontend が localStorage に持つパスごとに作る。
#[derive(Serialize)]
struct PackageEntry {
    /// localStorage が保持するパス (repo root 相対 or 絶対)。
    path: String,
    title: String,
    description: String,
    /// プレイ可能か (manifest が読めれば true。単発・campaign-entry 双方対応)。読込エラー時のみ false。
    playable: bool,
    /// package.yaml が読めない等のエラー (一覧から外さず理由を表示する)。
    error: Option<String>,
    /// オートセーブが在ればその時点のターン数 (「続きから (turn N)」の提示素。無ければ None)。
    autosave_turn: Option<u32>,
    /// 手動セーブスロット (spec 07 Phase D) が 1 つでも在るか (パッケージ削除時の確認に使う)。
    has_slots: bool,
    /// 出所メタ (spec 17) が在れば取得元サイトと書庫 id。サイトタブの「取得済み」判定に使う
    /// (同じ配布物を `_2` で二重取得させない)。手動配置・自作は None = 判定の対象外。
    source_site: Option<String>,
    source_id: Option<String>,
}

/// localStorage 由来のパス列について、各 `package.yaml` の manifest を読み一覧 view を返す。
/// entry は解決しない (一覧は title/description だけ要る、campaign パッケージも一覧には出す)。
#[tauri::command]
async fn list_packages(app: tauri::AppHandle, paths: Vec<String>) -> Vec<PackageEntry> {
    // spec 17 rev2 A-3: 更新スワップのクラッシュ残骸を掃除 (tmp 削除 / .bak 自動復旧)。
    // 書庫取得物の置き場 (app_data/packages) だけを走査する (repo 同梱・手動配置は触らない)。
    // **更新中はしない**: `.update_tmp_*` は残骸と展開中の staging を名前で区別できないので、
    // 更新の最中に一覧を開くと自分の staging を消してしまう (旧は復旧されるが更新が落ちる)。
    if !UPDATING.load(std::sync::atomic::Ordering::Acquire) {
        if let Ok(data_dir) = app.path().app_data_dir() {
            update::cleanup_leftovers(&data_dir.join("packages"));
        }
    }
    paths
        .into_iter()
        .map(|p| {
            let dir = resolve_pkg_dir(&p);
            // オートセーブの有無 (spec 07 Phase C)。読めない/版不一致は「続き無し」扱い (寛容)。
            let autosave_turn = autosave_path(&app, &dir)
                .and_then(|sp| load_session(&sp).ok())
                .map(|s| s.state.turn);
            // 手動スロット (Phase D) の有無 (存在チェックのみ = 5 本 parse しない)。
            let has_slots = (1..=SAVE_SLOTS)
                .any(|n| slot_save_path(&app, &dir, n).is_some_and(|sp| sp.exists()));
            // 出所メタ (spec 17)。無ければ手動配置 = 更新機構も取得済み判定も触らない。
            let meta = update::read_source_meta(&dir);
            let (source_site, source_id) = meta
                .map(|m| (Some(m.site_url), Some(m.id)))
                .unwrap_or((None, None));
            match read_manifest(&dir) {
                Ok(m) => {
                    // 単発シナリオも campaign-entry も playable (new_game が entry を分岐)。
                    let playable = true;
                    PackageEntry {
                        path: p,
                        title: m.title,
                        description: m.description,
                        playable,
                        error: None,
                        autosave_turn,
                        has_slots,
                        source_site,
                        source_id,
                    }
                }
                Err(e) => PackageEntry {
                    path: p.clone(),
                    title: p,
                    description: String::new(),
                    playable: false,
                    error: Some(e.to_string()),
                    autosave_turn: None,
                    has_slots,
                    source_site,
                    source_id,
                },
            }
        })
        .collect()
}

/// パッケージのセーブ (autosave + 手動スロット全部) を削除する (一覧から外す時の孤児掃除)。
/// セーブは `app_data/saves/` のファイルなので、localStorage の一覧からパスを消すだけでは
/// 残り続けて溜まる。frontend が削除確認の上で呼ぶ。
/// 1 つでも削除したら true、元々無ければ false (どちらも成功)。
#[tauri::command]
fn delete_autosave(app: tauri::AppHandle, package_path: String) -> Result<bool, String> {
    let dir = resolve_pkg_dir(&package_path);
    let mut targets: Vec<PathBuf> = Vec::new();
    targets.push(autosave_path(&app, &dir).ok_or("アプリデータフォルダを解決できない")?);
    for n in 1..=SAVE_SLOTS {
        if let Some(p) = slot_save_path(&app, &dir, n) {
            targets.push(p);
        }
    }
    let mut removed = false;
    for p in targets {
        if p.exists() {
            std::fs::remove_file(&p).map_err(|e| format!("セーブの削除に失敗: {e}"))?;
            removed = true;
        }
    }
    Ok(removed)
}

/// LLM 接続設定の view (設定ダイアログの AIモデルタブ用)。ローカル app ゆえ api_key も返す
/// (ユーザー自身の鍵を編集できるようにする)。
#[derive(Serialize)]
struct LlmConfigView {
    base_url: String,
    model: String,
    api_key: String,
    /// tool-use (function calling) を使うか。さくら等 tool_choice 非対応サーバはオフにする。
    use_tools: bool,
}

/// `LLM_USE_TOOLS` を解釈する (既定 true。"false"/"0"/"no"/"off" のみ false)。config.rs と同基準。
fn parse_use_tools() -> bool {
    std::env::var("LLM_USE_TOOLS")
        .ok()
        .map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "false" | "0" | "no" | "off"))
        .unwrap_or(true)
}

/// 現在の LLM 設定 (プロセス env = 起動時 .env 由来) を返す。AIモデルタブの初期値。
#[tauri::command]
fn get_llm_config() -> LlmConfigView {
    let opt = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    LlmConfigView {
        base_url: opt("LLM_BASE_URL").unwrap_or_else(|| "https://api.openai.com/v1".into()),
        model: opt("LLM_MODEL").unwrap_or_else(|| "gpt-4o-mini".into()),
        api_key: std::env::var("LLM_API_KEY").unwrap_or_default(),
        use_tools: parse_use_tools(),
    }
}

/// LLM 設定を更新する: プロセス env を即時差し替え (次の new_game の from_env が反映) +
/// `app_data_dir/.env` へ永続化 (再起動後も効く。配布 exe でも書ける — 旧 repo_root/.env は
/// インストール版で存在せず保存に失敗していた)。AIモデルタブの保存。
#[tauri::command]
fn set_llm_config(
    app: tauri::AppHandle,
    base_url: String,
    model: String,
    api_key: String,
    use_tools: bool,
) -> Result<(), String> {
    // 1) プロセス env を更新 (この後の new_game が拾う)。edition 2021 ゆえ set_var は safe。
    let use_tools_s = if use_tools { "true" } else { "false" };
    std::env::set_var("LLM_BASE_URL", &base_url);
    std::env::set_var("LLM_MODEL", &model);
    std::env::set_var("LLM_API_KEY", &api_key);
    std::env::set_var("LLM_USE_TOOLS", use_tools_s);
    // 2) app_data_dir/.env に永続化 (再起動後も効く)。親フォルダは初回に作る。
    let updates = [
        ("LLM_BASE_URL".to_string(), base_url),
        ("LLM_MODEL".to_string(), model),
        ("LLM_API_KEY".to_string(), api_key),
        ("LLM_USE_TOOLS".to_string(), use_tools_s.to_string()),
    ];
    let path = config_env_path(&app).ok_or_else(|| "app_data_dir を解決できない".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("設定フォルダの作成に失敗: {e}"))?;
    }
    upsert_env(&path, &updates).map_err(|e| format!(".env の保存に失敗: {e}"))
}

/// あらすじ要約用 LLM 設定の view (spec 10)。enabled=false なら GM と同じ client を共用する。
#[derive(Serialize)]
struct SummaryLlmConfigView {
    base_url: String,
    model: String,
    api_key: String,
    /// base_url か model のどちらかが設定されていれば true (summary_from_env の有効判定と同基準)。
    enabled: bool,
}

/// 現在のあらすじ要約用設定を返す。設定「AIモデル」タブの初期値。
#[tauri::command]
fn get_summary_llm_config() -> SummaryLlmConfigView {
    let opt = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    let base_url = opt("SUMMARY_LLM_BASE_URL");
    let model = opt("SUMMARY_LLM_MODEL");
    SummaryLlmConfigView {
        enabled: base_url.is_some() || model.is_some(),
        base_url: base_url.unwrap_or_default(),
        model: model.unwrap_or_default(),
        api_key: std::env::var("SUMMARY_LLM_API_KEY").unwrap_or_default(),
    }
}

/// あらすじ要約用 LLM 設定を更新する (`set_llm_config` と同経路 = プロセス env 即時 +
/// app_data/.env 永続)。**全て空 = 無効化** (GM と同じ client 共用へ戻す —
/// 空値は summary_from_env が未設定として filter する)。次の new_game/resume から効く。
#[tauri::command]
fn set_summary_llm_config(
    app: tauri::AppHandle,
    base_url: String,
    model: String,
    api_key: String,
) -> Result<(), String> {
    std::env::set_var("SUMMARY_LLM_BASE_URL", &base_url);
    std::env::set_var("SUMMARY_LLM_MODEL", &model);
    std::env::set_var("SUMMARY_LLM_API_KEY", &api_key);
    let updates = [
        ("SUMMARY_LLM_BASE_URL".to_string(), base_url),
        ("SUMMARY_LLM_MODEL".to_string(), model),
        ("SUMMARY_LLM_API_KEY".to_string(), api_key),
    ];
    let path = config_env_path(&app).ok_or_else(|| "app_data_dir を解決できない".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("設定フォルダの作成に失敗: {e}"))?;
    }
    upsert_env(&path, &updates).map_err(|e| format!(".env の保存に失敗: {e}"))
}

/// env フラグの truthy 判定 (harness::prompt::is_truthy と同基準)。`1`/`true`/`yes`/`on`。
fn env_is_truthy(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

/// 開発者モード (KATARIBE_DEV_MODE) が有効か。設定「開発者」タブの初期値。
/// 有効時は run_turn が GM に「テストプレイ中・`<meta: ...>` でメタ質問を受ける」旨を刷り込む。
#[tauri::command]
fn get_dev_mode() -> bool {
    std::env::var("KATARIBE_DEV_MODE").ok().map(|v| env_is_truthy(&v)).unwrap_or(false)
}

/// 開発者モードを切り替える: プロセス env を即時差し替え (次の play_turn の run_turn が拾う) +
/// `app_data_dir/.env` へ永続化 (set_llm_config と同経路・#46 と同流儀=GUI 保存値が唯一の真実)。
#[tauri::command]
fn set_dev_mode(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    let v = if enabled { "true" } else { "false" };
    std::env::set_var("KATARIBE_DEV_MODE", v);
    let path = config_env_path(&app).ok_or_else(|| "app_data_dir を解決できない".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("設定フォルダの作成に失敗: {e}"))?;
    }
    upsert_env(&path, &[("KATARIBE_DEV_MODE".to_string(), v.to_string())])
        .map_err(|e| format!(".env の保存に失敗: {e}"))
}

/// `.env` の指定キーを upsert する。既存行は値だけ差し替え、無ければ末尾に追記。
/// コメント行・他キー・順序は保つ (鍵以外の設定を壊さない)。
fn upsert_env(path: &Path, updates: &[(String, String)]) -> std::io::Result<()> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for line in existing.lines() {
        let t = line.trim_start();
        let mut replaced = false;
        if !t.starts_with('#') {
            if let Some(eq) = t.find('=') {
                let key = t[..eq].trim_end();
                if let Some((k, v)) = updates.iter().find(|(k, _)| k.as_str() == key) {
                    out.push(format!("{k}={v}"));
                    seen.push(k.clone());
                    replaced = true;
                }
            }
        }
        if !replaced {
            out.push(line.to_string());
        }
    }
    for (k, v) in updates {
        if !seen.contains(k) {
            out.push(format!("{k}={v}"));
        }
    }
    std::fs::write(path, out.join("\n") + "\n")
}

// =============================================================================
// 配布サイト「Kataribe 書庫」統合 (spec 05 Phase C)
// =============================================================================

/// 書庫 API `/api/packages` の一覧 1 項目 (サーバ応答の写し。outcast Spec 23)。
/// 必須フィールドで受ける — 形が合わなければ沈黙の空欄でなくエラーにする (寛容な
/// deserialize は失敗を隠す)。サーバ側の追加フィールドは serde が黙って無視する。
#[derive(Serialize, Deserialize)]
struct RemotePackage {
    id: String,
    title: String,
    description: String,
    category: String,
    /// 性・流血描写の自己申告。倫理制約の強い LLM ではプレイできない可能性の目印。
    is_mature: bool,
    file_size: i64,
    uploader_display_name: String,
    download_count: i64,
    avg_rating: Option<f64>,
    review_count: i64,
    /// 作者が納本時に自己申告する対応 Kataribe バージョン (例 "v0.2.0")。未申告なら None
    /// (Option ゆえ古い版の書庫や未申告パッケージでも deserialize は通る)。
    kataribe_version: Option<String>,
    /// 配布物 (正規化済み zip) の sha256 (spec 17 機構②)。install 時の一致検証と
    /// 更新検知の一次ソース。未対応の古い書庫/自前サーバは None (更新検知は静かに無効)。
    #[serde(default)]
    sha256: Option<String>,
    /// 配布物の差し替え日時 (ISO8601)。更新バッジの hover 表示に使う人間値
    /// (版番号はサーバに無い = manifest 非 parse 原則の維持。spec 17 表示設計)。
    #[serde(default)]
    file_updated_at: Option<String>,
}

/// 書庫の一覧応答 (items + ページネーション)。
#[derive(Serialize, Deserialize)]
struct RemoteList {
    items: Vec<RemotePackage>,
    total: i64,
    page: i64,
    page_size: i64,
}

/// サイト URL を検証して正規形 (末尾スラッシュなし) に整える。
fn normalize_site_url(site_url: &str) -> Result<String, String> {
    let base = site_url.trim().trim_end_matches('/').to_string();
    if base.starts_with("http://") || base.starts_with("https://") {
        Ok(base)
    } else {
        Err("サイト URL は http:// または https:// で始めてください".to_string())
    }
}

/// 書庫クライアント (一覧 fetch / zip DL 共用)。呼び出しはユーザー操作の頻度なので都度生成。
fn site_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP クライアントの初期化に失敗: {e}"))
}

/// 配布サイトのパッケージ一覧を取得する (無認証の公開 API)。
/// q/category/sort は空文字なら送らない (サーバ既定 = 全件・新着順)。
#[tauri::command]
async fn fetch_site_packages(
    site_url: String,
    page: Option<i64>,
    q: Option<String>,
    category: Option<String>,
    sort: Option<String>,
) -> Result<RemoteList, String> {
    let base = normalize_site_url(&site_url)?;
    let mut query: Vec<(&str, String)> = vec![("page", page.unwrap_or(1).max(1).to_string())];
    if let Some(v) = q.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        query.push(("q", v));
    }
    if let Some(v) = category.filter(|s| !s.is_empty()) {
        query.push(("category", v));
    }
    if let Some(v) = sort.filter(|s| !s.is_empty()) {
        query.push(("sort", v));
    }
    let res = site_client()?
        .get(format!("{base}/api/packages"))
        .query(&query)
        .send()
        .await
        .map_err(|e| format!("配布サイトに接続できません: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("配布サイトがエラーを返しました: {}", res.status()));
    }
    res.json::<RemoteList>()
        .await
        .map_err(|e| format!("一覧の形式が読めません (配布サイトではない URL の可能性): {e}"))
}

/// アプリ更新情報 API (`GET /api/app`) の応答。必要なフィールドだけ拾い残りは無視。
#[derive(Deserialize)]
struct AppInfo {
    /// 配布が有効か (サーバがリリースを取り下げていれば false → 通知しない)。
    #[serde(default)]
    available: bool,
    /// 配布サイトの最新版タグ。例 "v0.3.3"。
    #[serde(default)]
    version: String,
}

/// フロントへ返す更新判定 (表示は提示層が決める)。
#[derive(Serialize)]
struct AppUpdateStatus {
    /// 現在版が配布版より古く、かつ配布が有効なら true (= 「最新版があります」を出す)。
    update_available: bool,
    /// 配布サイトの最新版 (表示用)。例 "v0.3.3"。
    latest_version: String,
}

/// "v0.3.3" / "0.3.3" 等を数値成分列 `[0, 3, 3]` へ。`v`/`V` 前置は剥がし、各成分は
/// 先頭の連続する数字だけを採る (`3-rc1` → 3)。非数値・欠損は 0 扱いで比較を壊さない。
fn parse_version(v: &str) -> Vec<u64> {
    v.trim()
        .trim_start_matches(['v', 'V'])
        .split('.')
        .map(|p| {
            let digits: String = p.chars().take_while(char::is_ascii_digit).collect();
            digits.parse::<u64>().unwrap_or(0)
        })
        .collect()
}

/// `latest` が `current` より新しいか。成分ごとに数値比較し、長さ違いは 0 埋め
/// (`0.3` と `0.3.0` は同値)。純関数 = PoC でテストする更新判定の核。
fn is_newer(latest: &str, current: &str) -> bool {
    let (a, b) = (parse_version(latest), parse_version(current));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

/// 配布サイトの最新版を問い合わせ、現在版 (git タグ = フロントの `__APP_VERSION__`) と比較する。
/// `current_version` が空 (タグ無しの開発ビルド) の時は判定不能 = 通知しない。
/// オフライン/未設定サイトはエラーを返すのでフロントが静かに握り潰す (更新通知は非必須)。
#[tauri::command]
async fn fetch_app_update(
    site_url: String,
    current_version: String,
) -> Result<AppUpdateStatus, String> {
    let base = normalize_site_url(&site_url)?;
    let res = site_client()?
        .get(format!("{base}/api/app"))
        .send()
        .await
        .map_err(|e| format!("配布サイトに接続できません: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("配布サイトがエラーを返しました: {}", res.status()));
    }
    let info = res
        .json::<AppInfo>()
        .await
        .map_err(|e| format!("更新情報の形式が読めません: {e}"))?;
    let update_available = info.available
        && !current_version.trim().is_empty()
        && is_newer(&info.version, &current_version);
    Ok(AppUpdateStatus {
        update_available,
        latest_version: info.version,
    })
}

/// 取得結果 (packagePaths へ登録する絶対パス + 表示用 title)。
#[derive(Serialize)]
struct InstalledPackage {
    path: String,
    title: String,
}

/// DL 受入上限 — サーバのファイル上限 100MB + 余裕 (無限ストリームへの蓋)。
const MAX_DOWNLOAD_BYTES: u64 = 110 * 1024 * 1024;

/// パッケージ zip を `tmp` へストリーム DL する (新規取得・更新の共用)。上限超過で即中断。
async fn download_package_zip(base: &str, id: &str, tmp: &Path) -> Result<(), String> {
    let mut res = site_client()?
        .get(format!("{base}/api/packages/{id}/download"))
        .send()
        .await
        .map_err(|e| format!("配布サイトに接続できません: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("ダウンロードに失敗しました: {}", res.status()));
    }
    let mut out =
        std::fs::File::create(tmp).map_err(|e| format!("一時ファイルを作成できません: {e}"))?;
    let mut written: u64 = 0;
    while let Some(chunk) = res
        .chunk()
        .await
        .map_err(|e| format!("ダウンロード中に切断されました: {e}"))?
    {
        written += chunk.len() as u64;
        if written > MAX_DOWNLOAD_BYTES {
            return Err("ファイルが大きすぎます (110MB 超)".to_string());
        }
        std::io::Write::write_all(&mut out, &chunk)
            .map_err(|e| format!("一時ファイルへの書き込みに失敗: {e}"))?;
    }
    Ok(())
}

/// 配布サイトからパッケージ zip を DL し、検証・展開して packages 置き場に据える。
/// 展開先は `app_data_dir/packages/<フォルダ名>` (spec 07 saves と同じ流儀 — repo を汚さず、
/// 配布 zip の差し替えでも消えない)。zip 検証は `site::extract_package_zip`
/// (クライアント側でも zip slip 遮断 = サーバを信用しない二層)。
#[tauri::command]
async fn install_site_package(
    app: tauri::AppHandle,
    site_url: String,
    id: String,
    sha256: Option<String>,
) -> Result<InstalledPackage, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("アプリデータ置き場を解決できません: {e}"))?;
    install_from_site(&site_url, &id, sha256.as_deref(), &data_dir).await
}

/// install_site_package の本体 (Tauri 非依存 = 実サーバ相手の統合テストが書ける)。
/// `expected_sha256` はサーバ申告 (一覧の `RemotePackage.sha256`、spec 17 rev2 A-1) —
/// Some なら受信バイト列の自前計算と一致検証し、不一致は DL 破損として中止する
/// (壊れた基準を SourceMeta に記録すると更新検知が恒常的に狂うため)。None (古い書庫) は
/// 検証なしで自前計算値を記録する。
async fn install_from_site(
    site_url: &str,
    id: &str,
    expected_sha256: Option<&str>,
    data_dir: &Path,
) -> Result<InstalledPackage, String> {
    let base = normalize_site_url(site_url)?;
    // id は URL 片に乗る。UUID の字種 (hex + ハイフン) のみ許す。
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return Err("不正なパッケージ id です".to_string());
    }

    let packages_dir = data_dir.join("packages");
    let dl_dir = data_dir.join("downloads");
    std::fs::create_dir_all(&packages_dir).map_err(|e| format!("展開先を作成できません: {e}"))?;
    std::fs::create_dir_all(&dl_dir).map_err(|e| format!("一時置き場を作成できません: {e}"))?;
    let tmp = dl_dir.join(format!("{id}.zip.part"));

    // --- DL (ストリームで一時ファイルへ。上限超過で即中断) ---
    if let Err(e) = download_package_zip(&base, id, &tmp).await {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    // --- 受信バイト列の指紋 (spec 17 rev2 A-1) ---
    // サーバ申告 (一覧の sha256) と一致検証してから基準として採用する。不一致 = DL 破損。
    let content_hash = match update::sha256_file(&tmp) {
        Ok(h) => h,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("ダウンロードの検証に失敗: {e}"));
        }
    };
    if let Some(expected) = expected_sha256 {
        if !expected.eq_ignore_ascii_case(&content_hash) {
            let _ = std::fs::remove_file(&tmp);
            return Err("ダウンロードが破損しています (ハッシュ不一致)。もう一度お試しください".to_string());
        }
    }

    // --- 検証 + 展開 (同期 IO/CPU なので blocking プールへ) ---
    let tmp2 = tmp.clone();
    let pk2 = packages_dir.clone();
    let extracted = tokio::task::spawn_blocking(move || site::extract_package_zip(&tmp2, &pk2))
        .await
        .map_err(|e| format!("展開タスクの実行に失敗: {e}"));
    let _ = std::fs::remove_file(&tmp); // 一時 zip は常に破棄
    let installed = extracted??;

    // --- 受領側検証の入口: manifest が読めるか (深い検証は new_game 時の validate) ---
    match read_manifest(&installed) {
        Ok(m) => {
            // --- 出所メタ (spec 17 機構①): 更新検知・編集検知の基準をフォルダ自身に記録。
            // 書けなくてもパッケージは使える (更新だけ効かない) = 非致命の警告どまり。
            match update::tree_hash(&installed) {
                Ok(tree) => {
                    let meta = update::SourceMeta {
                        site_url: base.clone(),
                        id: id.to_string(),
                        version: (!m.version.trim().is_empty()).then(|| m.version.clone()),
                        content_hash,
                        tree_hash: tree,
                        installed_at_unix: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0),
                    };
                    if let Err(e) = update::write_source_meta(&installed, &meta) {
                        eprintln!("[警告] 出所メタを書けませんでした (更新検知は無効): {e}");
                    }
                }
                Err(e) => eprintln!("[警告] tree_hash を計算できませんでした (更新検知は無効): {e}"),
            }
            Ok(InstalledPackage {
                path: installed.to_string_lossy().into_owned(),
                title: m.title,
            })
        }
        Err(e) => {
            // パッケージとして読めない配布物は据え置かない (一覧の恒久エラー行を作らない)。
            let _ = std::fs::remove_dir_all(&installed);
            Err(format!("パッケージとして読めません: {e}"))
        }
    }
}

// =============================================================================
// パッケージ更新 (spec 17 Phase C) — 更新検知と上書き取得
// =============================================================================

/// 「更新あり」1 件 (frontend のバッジ素材)。判定は hash の**相違**のみ (新旧の順序は無い)。
#[derive(Serialize)]
struct PackageUpdate {
    /// `packagePaths` が持つパス (frontend のキー = 表示行との突き合わせに使う)。
    path: String,
    /// 書庫のパッケージ id。
    id: String,
    /// サイト側の差し替え日時 (ISO8601)。版番号はサーバに無いので日時で示す。
    file_updated_at: Option<String>,
    /// 手元の版 (取得時 package.yaml の写し)。欠落は None → 表示は「(不明)」。
    local_version: Option<String>,
    /// 手元の取得時刻 (unix 秒)。表示の locale 変換は提示層。
    installed_at_unix: u64,
}

/// 更新完了の報告 (トースト素材)。版は人間向け表示のみで、判定には一切使わない。
#[derive(Serialize)]
struct UpdateResult {
    title: String,
    from_version: Option<String>,
    to_version: Option<String>,
}

/// 更新の排他 (rev2 B-10): 同時に 1 件だけ。実行中は検知もスキップする
/// (スワップ中のフォルダを走査して誤判定しない)。
static UPDATING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 更新中フラグの RAII ガード (どの return でも必ず降りる)。
struct UpdateGuard;
impl UpdateGuard {
    /// 取れなければ None (既に別の更新が進行中)。
    fn acquire() -> Option<Self> {
        UPDATING
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .ok()
            .map(|_| UpdateGuard)
    }
}
impl Drop for UpdateGuard {
    fn drop(&mut self) {
        UPDATING.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// 書庫の詳細 (`GET /api/packages/{id}`) を引く。更新検知・更新取得の共通経路。
async fn fetch_package_detail(base: &str, id: &str) -> Result<RemotePackage, String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return Err("不正なパッケージ id です".to_string());
    }
    let res = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .connect_timeout(std::time::Duration::from_secs(8))
        .build()
        .map_err(|e| format!("HTTP クライアントの初期化に失敗: {e}"))?
        .get(format!("{base}/api/packages/{id}"))
        .send()
        .await
        .map_err(|e| format!("配布サイトに接続できません: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("配布サイトがエラーを返しました: {}", res.status()));
    }
    res.json::<RemotePackage>()
        .await
        .map_err(|e| format!("詳細の形式が読めません: {e}"))
}

/// 出所メタが「いま設定中のサイト由来」かを判定する (rev2 A-4 SSRF 遮断)。
/// 一致する時だけネットワークに出る — 細工メタを手動配置されても照会先はユーザー自身が
/// 登録したサイトだけ (`open_external_url` の原則と同じ)。
fn meta_matches_site(meta: &update::SourceMeta, normalized_site: &str) -> bool {
    meta.site_url.trim_end_matches('/') == normalized_site
}

/// `packagePaths` の各フォルダについて更新の有無を照会する (spec 17 機構③)。
///
/// **失敗はすべて沈黙** (rev2 B-8): オフライン・404・5xx・パース失敗はその項目について
/// 何も主張せず、単に結果に載せない。検知は best-effort であり、一覧を壊さない。
#[tauri::command]
async fn check_package_updates(site_url: String, paths: Vec<String>) -> Vec<PackageUpdate> {
    // 更新の実行中は走査しない (スワップ中のフォルダを見て誤判定しない)。
    if UPDATING.load(std::sync::atomic::Ordering::Acquire) {
        return Vec::new();
    }
    let Ok(base) = normalize_site_url(&site_url) else {
        return Vec::new();
    };
    // メタが在り、かつ現在のサイト由来のものだけを照会対象にする。
    let targets: Vec<(String, update::SourceMeta)> = paths
        .into_iter()
        .filter_map(|p| {
            let meta = update::read_source_meta(&resolve_pkg_dir(&p))?;
            meta_matches_site(&meta, &base).then_some((p, meta))
        })
        .collect();

    // 並列照会 (件数はユーザーの登録数 = 高々数十)。
    let mut tasks = Vec::with_capacity(targets.len());
    for (path, meta) in targets {
        let base = base.clone();
        tasks.push(tokio::spawn(async move {
            let remote = fetch_package_detail(&base, &meta.id).await.ok()?;
            let server_hash = remote.sha256?; // 未対応の書庫 → 検知は静かに無効
            (!server_hash.eq_ignore_ascii_case(&meta.content_hash)).then_some(PackageUpdate {
                path,
                id: meta.id,
                file_updated_at: remote.file_updated_at,
                local_version: meta.version,
                installed_at_unix: meta.installed_at_unix,
            })
        }));
    }
    let mut out = Vec::new();
    for t in tasks {
        if let Ok(Some(u)) = t.await {
            out.push(u);
        }
    }
    out
}

/// 手元のフォルダが取得後に編集されているか (機構④ 2.)。更新前の確認ダイアログの判定材料。
/// メタが無い/計算不能は false (触らない側に倒す — 聖域は聖域のまま)。
#[tauri::command]
fn package_is_locally_edited(path: String) -> bool {
    let dir = resolve_pkg_dir(&path);
    let Some(meta) = update::read_source_meta(&dir) else {
        return false;
    };
    update::tree_hash(&dir).map(|h| h != meta.tree_hash).unwrap_or(false)
}

/// 書庫の最新版でパッケージを**同じ場所へ**上書き更新する (spec 17 機構④)。
///
/// 守りは三重: (a) プレイ中は拒否、(b) ローカル編集は frontend が確認済み (`force`)、
/// (c) スワップは失敗時に旧フォルダへ復旧。**メタの更新はスワップ成功後のみ** (rev2 B-7) —
/// 失敗した hash を書くと「更新あり」が二度と点かなくなる。
#[tauri::command]
async fn update_site_package(
    session: tauri::State<'_, SharedSession>,
    site_url: String,
    path: String,
    force: bool,
) -> Result<UpdateResult, String> {
    let Some(_guard) = UpdateGuard::acquire() else {
        return Err("別のパッケージを更新中です。完了までお待ちください".to_string());
    };
    let dir = resolve_pkg_dir(&path);
    let meta = update::read_source_meta(&dir)
        .ok_or_else(|| "このパッケージは配布サイトから取得したものではありません".to_string())?;
    let base = normalize_site_url(&site_url)?;
    if !meta_matches_site(&meta, &base) {
        return Err("取得元サイトが現在の設定と異なります".to_string());
    }

    // (a) プレイ中ガード: Windows は再生中の BGM 等がフォルダの rename を失敗させる。
    // frontend もボタンを disable するが、正本の在り処を握る backend が最終判断する。
    {
        let guard = session.lock().await;
        if let Some(s) = guard.as_ref() {
            if s.package_root == dir {
                return Err("プレイ中のパッケージは更新できません。プレイを終了してからお試しください".to_string());
            }
        }
    }

    // (b) ローカル編集: force が無ければここで止める (frontend が確認ダイアログを出す)。
    if !force {
        let current = update::tree_hash(&dir)?;
        if current != meta.tree_hash {
            return Err("このパッケージはローカルで編集されています".to_string());
        }
    }

    // --- サーバの現在値を引く (sha256 = DL 検証の expected、version は表示に使わない) ---
    let remote = fetch_package_detail(&base, &meta.id).await?;

    let parent = dir
        .parent()
        .ok_or_else(|| "パッケージの親フォルダを解決できません".to_string())?
        .to_path_buf();
    let tmp_zip = parent.join(format!(".{}.zip.part", meta.id));
    let staging = parent.join(format!(".update_tmp_{}", meta.id));

    // 一時物は成功・失敗を問わず必ず片付ける (rev2 B-7)。
    let cleanup = |zip: &Path, stage: &Path| {
        let _ = std::fs::remove_file(zip);
        let _ = std::fs::remove_dir_all(stage);
    };

    if let Err(e) = download_package_zip(&base, &meta.id, &tmp_zip).await {
        cleanup(&tmp_zip, &staging);
        return Err(e);
    }
    let content_hash = match update::sha256_file(&tmp_zip) {
        Ok(h) => h,
        Err(e) => {
            cleanup(&tmp_zip, &staging);
            return Err(format!("ダウンロードの検証に失敗: {e}"));
        }
    };
    if let Some(expected) = remote.sha256.as_deref() {
        if !expected.eq_ignore_ascii_case(&content_hash) {
            cleanup(&tmp_zip, &staging);
            return Err("ダウンロードが破損しています (ハッシュ不一致)。もう一度お試しください".to_string());
        }
    }

    // --- 展開は staging へ (rev2 A-2: tmp 自体が package root = 二重構造を作らない) ---
    let (zip2, stage2) = (tmp_zip.clone(), staging.clone());
    let extracted = tokio::task::spawn_blocking(move || {
        site::extract_package_zip_to(&zip2, &stage2)?;
        // 据える前に「パッケージとして読めるか」を確認する (壊れた配布物で旧を潰さない)。
        read_manifest(&stage2).map_err(|e| format!("パッケージとして読めません: {e}"))
    })
    .await
    .map_err(|e| format!("展開タスクの実行に失敗: {e}"));
    let _ = std::fs::remove_file(&tmp_zip); // zip はここで用済み
    let new_manifest = match extracted.and_then(|r| r) {
        Ok(m) => m,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    };

    // --- スワップ (失敗時は旧フォルダへ復旧。パス・フォルダ名は不変) ---
    if let Err(e) = update::swap_in_place(&staging, &dir) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }

    // --- メタ更新はここまで来た時だけ (rev2 B-7) ---
    let to_version = (!new_manifest.version.trim().is_empty()).then(|| new_manifest.version.clone());
    let new_meta = update::SourceMeta {
        site_url: base,
        id: meta.id,
        version: to_version.clone(),
        content_hash,
        tree_hash: update::tree_hash(&dir)?,
        installed_at_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    };
    if let Err(e) = update::write_source_meta(&dir, &new_meta) {
        eprintln!("[警告] 出所メタを更新できませんでした (次回の更新検知は無効): {e}");
    }
    Ok(UpdateResult {
        title: new_manifest.title,
        from_version: meta.version,
        to_version,
    })
}

/// [`open_package`] の戻り値: (package root, 開始 scenario, campaign(単発は None), 現在 module,
/// manifest, 警告)。clippy type_complexity 回避の別名 (呼び出し側のタプル分解は透過で無改修)。
type OpenedPackage = (PathBuf, Scenario, Option<Campaign>, ModuleId, PackageManifest, Vec<String>);

/// パッケージを開く共通部 (new_game / resume_game): scope 許可 + entry 分岐ロード + 注入。
/// `module` 指定で campaign のそのモジュールを開く (再開用。単発パッケージでは無視)。
fn open_package(
    app: &tauri::AppHandle,
    rel: &str,
    module: Option<&ModuleId>,
) -> Result<OpenedPackage, String> {
    let root = repo_root();
    let pkg_dir = if Path::new(rel).is_absolute() {
        PathBuf::from(rel)
    } else {
        root.join(rel)
    };
    // `..` を畳む (asset protocol は `..` 入りパスを 403 拒否。scope 許可・解決の前に必須)。
    let pkg_dir = normalize_path(&pkg_dir);

    // アセット配信: このパッケージのフォルダだけを asset protocol scope に許可する
    // (静的 allowlist でなくロード時に動的追加 = 任意パス対応かつ安全。spec 01 #2)。
    app.asset_protocol_scope()
        .allow_directory(&pkg_dir, true)
        .map_err(|e| format!("アセット scope の許可に失敗: {e}"))?;

    // entry が campaign.yaml なら開始 (または指定) モジュールを、単発なら entry シナリオを読む。
    // どちらも package の player/globals/world を注入する (campaign は各モジュールへ継承)。
    let manifest = read_manifest(&pkg_dir).map_err(|e| e.to_string())?;
    if is_campaign_entry(&manifest.entry) {
        let loaded = load_campaign_package(&pkg_dir).map_err(|e| e.to_string())?;
        let target = module.cloned().unwrap_or_else(|| loaded.start_module.clone());
        let scenario = if target == loaded.start_module {
            loaded.scenario
        } else {
            // 再開: セーブに刻まれた途中モジュールを注入込みで開く。
            load_module_injected(&loaded.campaign, &pkg_dir, &loaded.manifest, &target)
                .map_err(|e| e.to_string())?
        };
        // campaign モジュールの未知フィールド lint は後続 (loader が生テキストを返さない)。
        Ok((pkg_dir, scenario, Some(loaded.campaign), target, loaded.manifest, Vec::new()))
    } else {
        let loaded = load_package(&pkg_dir).map_err(|e| e.to_string())?;
        Ok((pkg_dir, loaded.scenario, None, ModuleId::new(), loaded.manifest, loaded.warnings))
    }
}

#[tauri::command]
async fn new_game(
    app: tauri::AppHandle,
    package_path: Option<String>,
    lang: Option<String>,
    session: tauri::State<'_, SharedSession>,
) -> Result<GameView, String> {
    let rel = package_path.unwrap_or_else(|| DEFAULT_PACKAGE.to_string());
    let (pkg_dir, scenario, campaign, current_module, manifest, lint_warnings) =
        open_package(&app, &rel, None)?;
    // 伏線 (パッケージ内 memoria/) をロード。無ければ空。
    let lore = load_lore(&pkg_dir.join("memoria")).map_err(|e| e.to_string())?;

    // LLM クライアント (.env は main で読み込み済)。
    let config = LlmConfig::from_env().map_err(|e| e.to_string())?;
    // あらすじ要約用の専用 client (SUMMARY_LLM_*、spec 10)。未設定なら GM の client 共用。
    let summarizer = LlmConfig::summary_from_env(&config)
        .map_err(|e| e.to_string())?
        .map(LlmClient::new)
        .transpose()
        .map_err(|e| e.to_string())?;
    let mut client = LlmClient::new(config).map_err(|e| e.to_string())?;
    // 判定様式 (spec 16): 盤面が使わない判定 op を schema から落とす (percentile → check を
    // 隠し check_under を出す / additive (既定) → 逆)。セッション開始時に一度だけ確定。
    client.set_excluded_ops(harness::excluded_check_ops(&scenario));

    let seed = resolve_seed();
    eprintln!("[seed] {seed} (再現するには KATARIBE_SEED={seed})");
    let state = scenario.initial_state(seed);
    let title = if manifest.title.is_empty() {
        scenario.title.clone()
    } else {
        manifest.title.clone()
    };
    let view = GameView {
        title,
        location: state.location.clone(),
        description: scenario
            .location(&state.location)
            .map(|l| l.description.clone())
            .unwrap_or_default(),
        state: state_view(&state, &scenario, &[]),
        background: background_for(&scenario, &state, &pkg_dir),
        bgm: bgm_for(&scenario, &state, &pkg_dir),
        present_characters: present_characters(&scenario, &state, &pkg_dir),
        resumed: None,
        warnings: {
            let mut w = lint_warnings;
            w.extend(scenario_warnings(&scenario));
            w
        },
        synopsis: Vec::new(),
        recent_log: Vec::new(),
        map: map_view(&scenario, &state, &[], &pkg_dir),
        decision: None, // 新規開始に決断の持ち越しは無い
        contest: None,
        facts: Vec::new(), // 新規開始に約束事は無い (spec 20)
        facts_policy: facts_policy_str(scenario.facts_policy),
    };

    let save_path = autosave_path(&app, &pkg_dir);
    *session.lock().await = Some(GameSession {
        state,
        scenario,
        lore,
        client,
        pending_lore: Vec::new(),
        pending_checks: Vec::new(),
        package_root: pkg_dir,
        last_narration: String::new(),
        history: Vec::new(),
        campaign,
        current_module,
        campaign_memory: CampaignMemory::new(),
        manifest,
        save_path,
        package_path: rel,
        synopsis: Synopsis::default(),
        summarizer,
        facts: Vec::new(),
        // 言語設定タブ由来の lang を優先、無ければ env 既定。
        lang: match lang.as_deref() {
            Some("en") | Some("En") | Some("EN") => Lang::En,
            Some("ja") | Some("Ja") | Some("JA") => Lang::Ja,
            _ => lang_from_env(),
        },
    });
    Ok(view)
}

/// オートセーブから再開する (spec 07 Phase C)。実体は [`restore_session`]。
#[tauri::command]
async fn resume_game(
    app: tauri::AppHandle,
    package_path: String,
    lang: Option<String>,
    session: tauri::State<'_, SharedSession>,
) -> Result<GameView, String> {
    let save_path = autosave_path(&app, &resolve_pkg_dir(&package_path))
        .ok_or("アプリデータフォルダを解決できない")?;
    restore_session(&app, package_path, &save_path, lang, session.inner()).await
}

/// セーブファイルから session を復元する共通部 (オートセーブ resume / 手動スロット load)。
/// パッケージは content 参照から再ロード (骨格の単一真実源)、正本 state と語りの継続性
/// (chronicle/last_narration/pending_*/synopsis) はセーブから復元する。campaign は途中
/// モジュール + campaign_memory も復元。
///
/// **`GameSession` を丸ごと差し替える** — LLM は毎ターン messages を state/chronicle/synopsis
/// から新規構築する (持続会話は無い) ので、プレイ中にロードしても前のプレイの記憶は構造的に
/// 残らない (次ターンから GM はロードされた記憶だけを読み直す)。
/// 以後のオートセーブ書き先は常に autosave パス — ロード元スロットは上書きされない (凍結点)。
async fn restore_session(
    app: &tauri::AppHandle,
    package_path: String,
    load_from: &Path,
    lang: Option<String>,
    session: &SharedSession,
) -> Result<GameView, String> {
    // セーブを先に読む (campaign の途中モジュール指定が open_package に要る)。
    let save = load_session(load_from).map_err(|e| e.to_string())?;

    let (pkg_dir, scenario, campaign, current_module, manifest, lint_warnings) =
        open_package(app, &package_path, save.module.as_ref())?;
    let lore = load_lore(&pkg_dir.join("memoria")).map_err(|e| e.to_string())?;
    let config = LlmConfig::from_env().map_err(|e| e.to_string())?;
    let summarizer = LlmConfig::summary_from_env(&config)
        .map_err(|e| e.to_string())?
        .map(LlmClient::new)
        .transpose()
        .map_err(|e| e.to_string())?;
    let mut client = LlmClient::new(config).map_err(|e| e.to_string())?;
    // 判定様式 (spec 16): new_game と同じくセッション開始時に確定。
    client.set_excluded_ops(harness::excluded_check_ops(&scenario));

    // 版不一致は警告のみ (typo 修正でセーブを全滅させない。壊れは engine の閉世界却下が守る)。
    let mut warnings = Vec::new();
    if save.package_version != manifest.version {
        warnings.push(format!(
            "パッケージの版がセーブ時 ({}) と異なります ({})。内容変更により整合しない可能性があります",
            save.package_version, manifest.version
        ));
    }

    let state = save.state;
    let title = if manifest.title.is_empty() {
        scenario.title.clone()
    } else {
        manifest.title.clone()
    };
    let view = GameView {
        title,
        location: state.location.clone(),
        description: scenario
            .location(&state.location)
            .map(|l| l.description.clone())
            .unwrap_or_default(),
        state: state_view(&state, &scenario, &save.history),
        background: background_for(&scenario, &state, &pkg_dir),
        bgm: bgm_for(&scenario, &state, &pkg_dir),
        present_characters: present_characters(&scenario, &state, &pkg_dir),
        resumed: Some(ResumeView {
            turn: state.turn,
            last_narration: normalize(&save.last_narration),
            warnings,
        }),
        warnings: {
            let mut w = lint_warnings;
            w.extend(scenario_warnings(&scenario));
            w
        },
        // あらすじ全量 + 未圧縮 tail (spec 10)。再開直後からタブが埋まる。
        synopsis: save.synopsis.entries.iter().map(synopsis_view).collect(),
        recent_log: {
            let upto = save.synopsis.compressed_upto();
            save.history.iter().filter(|l| l.turn > upto).map(log_line_view).collect()
        },
        map: map_view(&scenario, &state, &save.history, &pkg_dir),
        // 決断待ち・対決はセーブを跨いで生きる (spec 18) — 再開直後にパネルを復元する。
        decision: decision_view(&state, &scenario),
        contest: contest_view(&state, &scenario),
        // 約束事 (spec 20) — セーブから復元してタブを埋める。
        facts: fact_views(&save.facts),
        facts_policy: facts_policy_str(scenario.facts_policy),
    };

    // オートセーブの書き先 (ロード元がスロットでも常に autosave パス = スロットは凍結点のまま)。
    let autosave = autosave_path(app, &pkg_dir);
    *session.lock().await = Some(GameSession {
        state,
        scenario,
        lore,
        client,
        pending_lore: save.pending_lore,
        pending_checks: save.pending_checks,
        package_root: pkg_dir,
        last_narration: save.last_narration,
        history: save.history,
        campaign,
        current_module,
        campaign_memory: save.campaign_memory,
        manifest,
        save_path: autosave,
        package_path,
        synopsis: save.synopsis,
        summarizer,
        facts: save.facts,
        lang: match lang.as_deref() {
            Some("en") | Some("En") | Some("EN") => Lang::En,
            Some("ja") | Some("Ja") | Some("JA") => Lang::Ja,
            _ => lang_from_env(),
        },
    });
    Ok(view)
}

/// 現在の session を SessionSave (正本 + 語りの継続性) へスナップショットする。
/// オートセーブ (play_turn) と手動スロット (save_slot) の共通部 — 同じ器を書く。
fn session_save_of(sess: &GameSession) -> SessionSave {
    SessionSave {
        version: SAVE_VERSION,
        content: SavedContent::Package { path: sess.package_path.clone() },
        package_version: sess.manifest.version.clone(),
        module: sess.campaign.is_some().then(|| sess.current_module.clone()),
        state: sess.state.clone(),
        campaign_memory: sess.campaign_memory.clone(),
        history: sess.history.clone(),
        last_narration: sess.last_narration.clone(),
        pending_checks: sess.pending_checks.clone(),
        pending_lore: sess.pending_lore.clone(),
        facts: sess.facts.clone(),
        synopsis: sess.synopsis.clone(),
    }
}

// =============================================================================
// 約束事 (spec 20) — ユーザー専権の編集 (追加・編集・削除)。成功後は即時 autosave
// (眺めるだけで消える事故を防ぐ)。GM 提案の採否は play_turn 側 (apply_gm_facts)。
// =============================================================================

/// 約束事編集コマンドの戻り: 更新後の全量 (スコア降順) + 満杯 add で押し出された行 (トースト用)。
#[derive(Serialize)]
struct FactsOpView {
    facts: Vec<FactView>,
    evicted: Option<String>,
}

/// 約束事編集後の即時 autosave。失敗は警告のみ (編集自体は成立させる — play_turn の流儀)。
fn facts_autosave(sess: &GameSession) {
    if let Some(path) = &sess.save_path {
        if let Err(e) = save_session(path, &session_save_of(sess)) {
            eprintln!("[警告] 約束事編集のオートセーブ失敗: {e}");
        }
    }
}

#[tauri::command]
async fn facts_add(
    text: String,
    session: tauri::State<'_, SharedSession>,
) -> Result<FactsOpView, String> {
    let mut guard = session.lock().await;
    let sess = guard.as_mut().ok_or("ゲームが開始されていません")?;
    // 権限は UI で隠すだけでなくここでも通さない (二層防衛 — #50 の教訓)。
    if !sess.scenario.facts_policy.allows_write() {
        return Err("このシナリオでは約束事の追記は作者が制限しています".into());
    }
    let (added, evicted) = harness::apply_user_add(&mut sess.facts, &text, sess.state.turn);
    if added.is_none() {
        return Err("約束事が空です".into());
    }
    facts_autosave(sess);
    Ok(FactsOpView { facts: fact_views(&sess.facts), evicted: evicted.map(|m| m.text) })
}

#[tauri::command]
async fn facts_edit(
    id: u64,
    text: String,
    session: tauri::State<'_, SharedSession>,
) -> Result<FactsOpView, String> {
    let mut guard = session.lock().await;
    let sess = guard.as_mut().ok_or("ゲームが開始されていません")?;
    if !sess.scenario.facts_policy.allows_write() {
        return Err("このシナリオでは約束事の編集は作者が制限しています".into());
    }
    if harness::apply_user_edit(&mut sess.facts, id, &text).is_none() {
        return Err("約束事を編集できません (空または対象が見つからない)".into());
    }
    facts_autosave(sess);
    Ok(FactsOpView { facts: fact_views(&sess.facts), evicted: None })
}

#[tauri::command]
async fn facts_delete(
    id: u64,
    session: tauri::State<'_, SharedSession>,
) -> Result<FactsOpView, String> {
    let mut guard = session.lock().await;
    let sess = guard.as_mut().ok_or("ゲームが開始されていません")?;
    if !sess.scenario.facts_policy.allows_delete() {
        return Err("このシナリオでは約束事の削除は作者が制限しています".into());
    }
    if !harness::apply_user_delete(&mut sess.facts, id) {
        return Err("対象の約束事が見つかりません".into());
    }
    facts_autosave(sess);
    Ok(FactsOpView { facts: fact_views(&sess.facts), evicted: None })
}

// =============================================================================
// 手動セーブスロット (spec 07 Phase D) — 「気に入ったシーンから何度でもやり直す」
// =============================================================================

/// 手動セーブスロットの一覧 (5 本、空きも含む)。
/// `package_path` 省略時は**プレイ中 session のパッケージ** (セーブモード = 保存先の真実は
/// session が握る — ヘッダーの選択を後から変えても保存先はプレイ中のゲーム)。
/// 指定時はそのパッケージ (ロードモード = ヘッダーで選択中のパッケージ、「続きから」と同じ意味論)。
#[tauri::command]
async fn list_save_slots(
    app: tauri::AppHandle,
    package_path: Option<String>,
    session: tauri::State<'_, SharedSession>,
) -> Result<Vec<SlotView>, String> {
    let pkg_dir = match package_path {
        Some(p) => resolve_pkg_dir(&p),
        None => session
            .lock()
            .await
            .as_ref()
            .map(|s| s.package_root.clone())
            .ok_or("ゲームが開始されていません")?,
    };
    Ok((1..=SAVE_SLOTS).map(|n| slot_view(&app, &pkg_dir, n)).collect())
}

/// 現在のプレイ状態を手動スロットへ保存する。autosave と同じ器 (SessionSave) をスロット名で
/// 書くだけ — autosave が「最新進捗」、スロットは「気に入ったシーンの凍結点」(ロードしても
/// 上書きされないので、同じ場面を何度でもロールプレイし直せる)。
#[tauri::command]
async fn save_slot(
    app: tauri::AppHandle,
    slot: u8,
    session: tauri::State<'_, SharedSession>,
) -> Result<SlotView, String> {
    if !(1..=SAVE_SLOTS).contains(&slot) {
        return Err(format!("スロットは 1〜{SAVE_SLOTS} です"));
    }
    let guard = session.lock().await;
    let sess = guard.as_ref().ok_or("ゲームが開始されていません")?;
    let path = slot_save_path(&app, &sess.package_root, slot)
        .ok_or("アプリデータフォルダを解決できない")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("セーブフォルダの作成に失敗: {e}"))?;
    }
    let save = session_save_of(sess);
    let pkg_root = sess.package_root.clone();
    drop(guard);
    save_session(&path, &save).map_err(|e| e.to_string())?;
    Ok(slot_view(&app, &pkg_root, slot))
}

/// 手動スロットから再開する。実体は [`restore_session`] (resume_game と同経路) —
/// `GameSession` を丸ごと差し替えるので、プレイ中でも前のプレイは忘れられ、GM は次ターンから
/// ロードされた state/chronicle/あらすじだけを読み直す (LLM に持続会話は無い)。
/// ロード時に autosave は書かない (「眺めるだけロード」が最新進捗を壊さない —
/// autosave が動くのはロード後の最初の受理ターンから)。
#[tauri::command]
async fn load_slot(
    app: tauri::AppHandle,
    package_path: String,
    slot: u8,
    lang: Option<String>,
    session: tauri::State<'_, SharedSession>,
) -> Result<GameView, String> {
    if !(1..=SAVE_SLOTS).contains(&slot) {
        return Err(format!("スロットは 1〜{SAVE_SLOTS} です"));
    }
    let path = slot_save_path(&app, &resolve_pkg_dir(&package_path), slot)
        .ok_or("アプリデータフォルダを解決できない")?;
    restore_session(&app, package_path, &path, lang, session.inner()).await
}

/// あらすじ圧縮ジョブを実行する (spec 10)。成功 = complete / 失敗 = abandon (非致命 —
/// あふれ契機は次ターン再計算、遷移契機は範囲凍結で同一リトライ)。
/// 要約は SUMMARY_LLM_* の専用 client、無ければ GM の client を共用する。
/// 失敗は `synopsis-failed` イベントで frontend にも通知する (リリースビルドは
/// コンソールが無く eprintln だけでは一般ユーザーが気づけない — 恒久失敗 =
/// 規約違反等で永遠にあらすじが作られない事態を可視化する)。
async fn run_synopsis_job(app: &tauri::AppHandle, sess: &mut GameSession, job: &SynopsisJob) {
    use tauri::Emitter;
    let req = sess.synopsis.build_request(&sess.history, job);
    let result = match &sess.summarizer {
        Some(s) => s.summarize(&req).await,
        None => sess.client.summarize(&req).await,
    };
    match result {
        Ok(text) => sess.synopsis.complete(job, &text),
        Err(e) => {
            eprintln!("[警告] あらすじ要約に失敗 (プレイは続行し後で再試行): {e}");
            let _ = app.emit("synopsis-failed", e.to_string());
            sess.synopsis.abandon(job);
        }
    }
}

#[tauri::command]
async fn play_turn(
    app: tauri::AppHandle,
    action: String,
    session: tauri::State<'_, SharedSession>,
) -> Result<TurnView, String> {
    use tauri::Emitter;
    let mut guard = session.lock().await;
    let sess = guard
        .as_mut()
        .ok_or("ゲームが開始されていません (先に new_game を呼んでください)")?;

    // spec 18 Phase B: 決断待ちの間はターンを回さない (frontend の入力ロックと二層)。
    // 凍結された帰結が未確定のまま GM に語らせると、確定前の世界を既成事実化してしまう。
    if !sess.state.pending_decisions.is_empty() {
        return Err("ダイスの決断が残っています。先に受け入れる/押す/払うを選んでください".into());
    }
    // spec 18 Phase C: 対決の進行中もターンを回さない (決着が先。engine 側の
    // ContestInProgress 却下と二層)。
    if sess.state.pending_contest.is_some() {
        return Err("対決が進行中です。決着がつくまで次の行動はできません".into());
    }

    // spec 10: このターンで増えた分の差分計上用スナップショット (あらすじ / chronicle)。
    let syn_before = sess.synopsis.entries.len();
    let hist_before = sess.history.len();

    // 前ターンの伏線・判定結果・語りを取り出して注入し、pending を空にする。
    let pending = std::mem::take(&mut sess.pending_lore);
    let pending_checks = std::mem::take(&mut sess.pending_checks);
    let prev_narration = std::mem::take(&mut sess.last_narration);
    let outcome = run_turn(
        &sess.client,
        &mut sess.state,
        &sess.scenario,
        action.trim(),
        MAX_ATTEMPTS,
        sess.lang,
        &pending,
        &pending_checks,
        &prev_narration,
        &sess.history,
        &sess.synopsis.entries,
        &sess.facts,
    )
    .await
    .map_err(|e| e.to_string())?;

    let mut view = match outcome {
        TurnOutcome::Accepted {
            narration,
            summary,
            facts,
            rolls,
            checks,
            stat_rolls,
            fired,
            attempts,
            rejected,
            tags,
        } => {
            // 約束事 (spec 20): GM 提案の採否を決める。採用 📝 / 強化 📝⁺ だけを表示し
            // (捨てられた行は見せない)、変化があればスナップショット全量を届ける。
            let facts_digest = harness::apply_gm_facts(&mut sess.facts, &facts, sess.state.turn);
            // locked 盤面では約束事は GM 専用の内部記憶 — 全量も 📝/📝⁺ 行もプレイヤーへ出さない
            // (タブごと隠すのと同じ思想。engine 側の記録は続く)。
            let facts_visible = sess.scenario.facts_policy.is_visible();
            let reinforced_texts: Vec<String> = if facts_visible {
                facts_digest
                    .reinforced
                    .iter()
                    .filter_map(|id| sess.facts.iter().find(|m| m.id == *id).map(|m| m.text.clone()))
                    .collect()
            } else {
                Vec::new()
            };
            let facts_changed = facts_visible
                && (!facts_digest.accepted.is_empty() || !facts_digest.reinforced.is_empty());
            let accepted_texts =
                if facts_visible { facts_digest.accepted.clone() } else { Vec::new() };
            // 発火ビートの cue を Memoria で解決 (memoria_bridge)。
            let resolved = resolve_recall(&sess.lore, &fired);
            // ビートは GM が見ていない筋書きの出来事 — 継続文脈と経緯ログの両方へ併記する。
            let beat_texts: Vec<String> = resolved.iter().map(|b| b.narration.clone()).collect();
            // 次ターンの継続文脈に持ち越す (既出情景の繰り返し防止。ビート込み)。
            sess.last_narration = carryover_narration(&narration, &beat_texts, &checks);
            // 経緯ログに積む (GM の summary、無ければ narration 冒頭へ fallback。
            // tags/checks は engine 事実の機械タグ = retrieval の接地、spec 08-B)。
            sess.history.push(chronicle_entry(
                sess.state.turn,
                action.trim(),
                &summary,
                &narration,
                &beat_texts,
                &tags,
                &checks,
            ));
            let beats = resolved
                .iter()
                .map(|b| BeatView {
                    narration: normalize(&b.narration),
                    recalled: b.recalled.iter().map(|f| normalize(&f.text)).collect(),
                    // イベント CG の ID を package_root 起点に解決 (背景と同経路)。
                    image: b.image.as_ref().and_then(|id| {
                        resolve_asset(&sess.package_root, AssetKind::Images, id)
                            .map(|p| p.to_string_lossy().into_owned())
                    }),
                    image_mode: b.image_mode.map(|m| match m {
                        ImageMode::Background => "background".to_string(),
                        ImageMode::Overlay => "overlay".to_string(),
                    }),
                    // 発火 SE の ID を package_root 起点に解決 (背景と同経路、audios フォルダ)。
                    sound: b.sound.as_ref().and_then(|id| {
                        resolve_asset(&sess.package_root, AssetKind::Audios, id)
                            .map(|p| p.to_string_lossy().into_owned())
                    }),
                })
                .collect();
            // 次ターンの語りに織り込ませる伏線を持ち越す。
            sess.pending_lore = resolved.into_iter().flat_map(|b| b.recalled).collect();

            let check_views: Vec<CheckView> = checks
                .iter()
                .map(|c| CheckView {
                    entity: c.entity.clone(),
                    stat: c.stat.clone(),
                    sides: c.sides,
                    count: c.count,
                    times: c.times,
                    roll: c.roll,
                    modifier: c.modifier,
                    total: c.total,
                    dc: c.dc,
                    success: c.success,
                    narration: normalize(&c.narration),
                    // 結末効果音 ID を audios/ から解決 (発火ビートの SE と同経路)。
                    sound: (!c.sound.is_empty())
                        .then(|| resolve_asset(&sess.package_root, AssetKind::Audios, &c.sound))
                        .flatten()
                        .map(|p| p.to_string_lossy().into_owned()),
                    degree: c.degree.clone(),
                    pushed: c.pushed,
                    spent: c.spent,
                    pending: c.pending,
                })
                .collect();
            let stat_roll_views: Vec<StatRollView> = stat_rolls
                .iter()
                .map(|sr| StatRollView {
                    entity: sr.entity.clone(),
                    key: sr.key.clone(),
                    count: sr.count,
                    sides: sr.sides,
                    bonus: sr.bonus,
                    rolls: sr.rolls.clone(),
                    amount: sr.amount,
                })
                .collect();
            // 次ターンの語りに還流する判定結果を持ち越す。
            sess.pending_checks = checks;

            let (goal_id, goal_title, goal_narration) = goal_view(&sess.state, &sess.scenario);
            TurnView {
                accepted: true,
                narration: normalize(&narration),
                rolls: rolls
                    .iter()
                    .map(|r| RollView {
                        sides: r.sides,
                        dc: r.dc,
                        result: r.result,
                        success: r.success,
                    })
                    .collect(),
                checks: check_views,
                stat_rolls: stat_roll_views,
                beats,
                attempts,
                reasons: Vec::new(),
                // 受理前に却下された各試行の理由を localize して author に見せる。
                retries: rejected
                    .iter()
                    .map(|rs| rs.iter().map(|r| r.localize(sess.lang)).collect())
                    .collect(),
                state: state_view(&sess.state, &sess.scenario, &sess.history),
                goal_reached: is_goal(&sess.state, &sess.scenario),
                goal_id,
                goal_title,
                goal_narration,
                background: background_for(&sess.scenario, &sess.state, &sess.package_root),
                bgm: bgm_for(&sess.scenario, &sess.state, &sess.package_root),
                present_characters: present_characters(&sess.scenario, &sess.state, &sess.package_root),
                transition: None,
                cache: sess.client.cache_stat(),
                new_synopsis: Vec::new(), // 圧縮ジョブの後に差分で埋める
                new_log: Vec::new(),
                epilogue: None, // 終端判定の後に埋める (spec 11)
                facts: facts_changed.then(|| fact_views(&sess.facts)),
                new_facts: accepted_texts,
                reinforced_facts: reinforced_texts,
                facts_policy: facts_policy_str(sess.scenario.facts_policy),
                map: map_view(&sess.scenario, &sess.state, &sess.history, &sess.package_root),
                // spec 18 Phase B: 決断つき判定が凍結されたらパネル素材を載せる。
                decision: decision_view(&sess.state, &sess.scenario),
                // spec 18 Phase C: attempt_contest で対決が開いたらパネル素材を載せる。
                contest: contest_view(&sess.state, &sess.scenario),
            }
        }
        TurnOutcome::Rejected { last_reasons, attempts } => TurnView {
            accepted: false,
            narration: String::new(),
            rolls: Vec::new(),
            checks: Vec::new(),
            stat_rolls: Vec::new(),
            beats: Vec::new(),
            attempts,
            reasons: last_reasons.iter().map(|r| r.localize(sess.lang)).collect(),
            retries: Vec::new(),
            // 却下では state 無傷。現状スナップショットを返す。
            state: state_view(&sess.state, &sess.scenario, &sess.history),
            goal_reached: is_goal(&sess.state, &sess.scenario),
            // 却下では state 不変ゆえ goal も変わらない。スナップショットとして同様に返す。
            goal_id: goal_view(&sess.state, &sess.scenario).0,
            goal_title: goal_view(&sess.state, &sess.scenario).1,
            goal_narration: goal_view(&sess.state, &sess.scenario).2,
            background: background_for(&sess.scenario, &sess.state, &sess.package_root),
            bgm: bgm_for(&sess.scenario, &sess.state, &sess.package_root),
            present_characters: present_characters(&sess.scenario, &sess.state, &sess.package_root),
            transition: None,
            cache: sess.client.cache_stat(),
            new_synopsis: Vec::new(),
            new_log: Vec::new(),
            epilogue: None,
            // 却下では GM 提案は捨てられる (state 無傷と同じ扱い = 約束事も無変化)。
            facts: None,
            new_facts: Vec::new(),
            reinforced_facts: Vec::new(),
            facts_policy: facts_policy_str(sess.scenario.facts_policy),
            map: map_view(&sess.scenario, &sess.state, &sess.history, &sess.package_root),
            // 却下 = state 無傷 (決断があったならそのまま残っているはずだが、そもそも決断中は
            // play_turn 自体をガードで弾く)。
            decision: decision_view(&sess.state, &sess.scenario),
            contest: contest_view(&sess.state, &sess.scenario),
        },
    };

    // --- campaign 前進 (reached → transition の結線、CLI play と同型) ---
    // goal 到達 + campaign パッケージなら、発火 GoalId で次モジュールへ state を糸通しして遷移する。
    // 駆動は LLM 非依存 (engine が決める GoalId と作者の地図だけ)。
    // spec 10: このターンで遷移契機の圧縮を回したか (回したならあふれ契機は次ターンへ譲る =
    // 1 ターン高々 1 ジョブ、失敗直後の即再試行で応答を二重に止めない)。
    let mut ran_transition_job = false;
    if view.accepted && view.goal_reached {
        if let Some(campaign) = sess.campaign.clone() {
            let from = sess.current_module.clone();
            let advance = advance_campaign_injected(
                &campaign,
                &sess.package_root,
                &sess.manifest,
                &mut sess.campaign_memory,
                &from,
                &sess.scenario,
                &sess.state,
            )
            .map_err(|e| e.to_string())?;
            // 辺が在る = 次モジュールへ (骨格だけ差し替え、状態は transition で持ち越し済)。
            // 辺が無い (advance=None) = 終端エンディング → goal_reached=true のまま = キャンペーン完了。
            if let Some(adv) = advance {
                // spec 10: 章の締めであらすじ圧縮 (章替わりマーカーを刻む前 = 新章の行を
                // 範囲に混ぜない。章題は遷移元モジュールの title)。
                let from_title = sess.scenario.title.clone();
                if let Some(job) = sess.synopsis.on_transition(&sess.history, &from_title) {
                    let _ = app.emit("synopsis-compacting", ());
                    run_synopsis_job(&app, sess, &job).await;
                    ran_transition_job = true;
                }
                sess.current_module = adv.module_id;
                sess.scenario = adv.scenario;
                sess.state = adv.state;
                // 判定様式は遷移先モジュールに従う (spec 16。schema はどのみち scenario_brief と
                // 一緒に変わるのでキャッシュ影響は遷移時のみ)。
                sess.client.set_excluded_ops(harness::excluded_check_ops(&sess.scenario));
                // 新モジュール = 新しい情景。継続文脈・伏線・判定の持ち越しをリセット。
                sess.last_narration = String::new();
                sess.pending_lore.clear();
                sess.pending_checks.clear();
                // 経緯 (chronicle) は捨てない — 章を跨いで覚えるのが眼目。章替わりを刻む。
                sess.history.push(TurnLog {
                    turn: sess.state.turn,
                    player: "（章の移り変わり）".into(),
                    summary: format!("『{}』へ移った", sess.scenario.title),
                    ..Default::default()
                });

                let description = sess
                    .scenario
                    .location(&sess.state.location)
                    .map(|l| normalize(&l.description))
                    .unwrap_or_default();
                view.transition = Some(TransitionView {
                    module_title: sess.scenario.title.clone(),
                    location: sess.state.location.clone(),
                    description,
                });
                // パネル類は遷移先を指す (goal_* は遷移元の結末のまま残す)。
                view.state = state_view(&sess.state, &sess.scenario, &sess.history);
                view.background = background_for(&sess.scenario, &sess.state, &sess.package_root);
                view.bgm = bgm_for(&sess.scenario, &sess.state, &sess.package_root);
                view.present_characters =
                    present_characters(&sess.scenario, &sess.state, &sess.package_root);
                // マップは遷移先モジュールのグラフへ差し替え (history は章跨ぎで残るが
                // map_view が現 scenario の location だけに絞るので前章のノードは出ない)。
                view.map = map_view(&sess.scenario, &sess.state, &sess.history, &sess.package_root);
                // キャンペーンは続くので入力を締めない (終端は advance=None で締まる)。
                view.goal_reached = false;
            }
        }
    }

    // spec 10: あふれ契機 (+ 遷移凍結のリトライ) の圧縮。受理ターンのみ・1 ターン高々 1 ジョブ。
    if view.accepted && !ran_transition_job {
        if let Some(job) = sess.synopsis.next_job(&sess.history) {
            let _ = app.emit("synopsis-compacting", ());
            run_synopsis_job(&app, sess, &job).await;
        }
    }
    // spec 10: このターンの差分を view に載せる (あらすじは append-only ゆえ frontend は push のみ)。
    view.new_synopsis = sess.synopsis.entries[syn_before..].iter().map(synopsis_view).collect();
    view.new_log = sess.history[hist_before..].iter().map(log_line_view).collect();

    // オートセーブ (spec 07 Phase C): 受理ターン + campaign 遷移が全て確定したこの地点で書く。
    // 却下では書かない (state 無傷 = セーブも不変)。失敗は警告のみ (救済機構が本体を殺さない)。
    if view.accepted {
        if let Some(path) = sess.save_path.clone() {
            let save = session_save_of(sess);
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = save_session(&path, &save) {
                eprintln!("[警告] オートセーブ失敗: {e}");
            }
        }
    }

    // --- エピローグ (spec 11): 到達 + 終端 + 指示あり ---
    // 「いつ」は engine (reached_goal)、「何を」は LLM。終端 = campaign なら advance 辺なし
    // (遷移していれば上の campaign 前進が goal_reached=false にしている)。**autosave の後**に
    // 生成する = 生成の失敗・クラッシュがセーブを巻き込まない (SessionSave に epilogue は無い)。
    // 失敗は skip — 結末文 + バナーの従来表示へフォールバック (narration が土台、非致命)。
    if view.accepted && view.goal_reached {
        if let Some(goal) = sess.scenario.reached_goal(&sess.state) {
            if goal.epilogue_prompt.as_deref().is_some_and(|p| !p.trim().is_empty()) {
                let req = harness::build_epilogue_request(
                    &sess.scenario,
                    goal,
                    &sess.synopsis.entries,
                    &sess.history,
                    &sess.last_narration,
                );
                let _ = app.emit("epilogue-writing", ());
                match harness::generate_epilogue(&sess.client, &req).await {
                    Ok(text) => view.epilogue = Some(normalize(&text)),
                    Err(e) => eprintln!("[警告] エピローグ生成に失敗 (結末文で幕): {e}"),
                }
            }
        }
    }

    Ok(view)
}

/// 決断の確定結果 view (spec 18 Phase B)。frontend が最終の判定行・語り・発火ビートを
/// 会話ログへ差し込み、state パネルを更新する。
#[derive(Serialize)]
struct DecisionResultView {
    /// 最終の判定 (narration/sound/pushed/spent 込み)。プッシュは新しい出目 = 開帳カードで出す。
    check: CheckView,
    stat_rolls: Vec<StatRollView>,
    beats: Vec<BeatView>,
    /// 支払い (差分買い / プッシュ代償)。表示用。
    spent_from: Option<String>,
    spent_amount: Option<i64>,
    push_paid_from: Option<String>,
    push_paid_amount: Option<i64>,
    state: StateView,
    goal_reached: bool,
    goal_id: Option<String>,
    goal_title: Option<String>,
    goal_narration: Option<String>,
    /// 続けて次の決断が待っているか (1 ターン複数凍結時)。
    decision: Option<DecisionView>,
    map: MapView,
}

/// 決断 (受け入れ / プッシュ / 差分買い) を確定する (spec 18 Phase B)。
///
/// LLM を呼ばない「プレイヤー op」— engine が凍結していた帰結をここで原子適用する。
/// choice: "accept" | "push" | "buy" (+ degree)。確定後は autosave (正本が動いたので)。
/// 制限 (v1): 決断の帰結で campaign 遷移 goal に達しても自動遷移しない (次の通常ターンの
/// play_turn が advance する)。エピローグ生成もここでは行わない (goal バナーと結末文のみ)。
#[tauri::command]
async fn resolve_dice_decision(
    session: tauri::State<'_, SharedSession>,
    choice: String,
    degree: Option<String>,
) -> Result<DecisionResultView, String> {
    let mut guard = session.lock().await;
    let sess = guard.as_mut().ok_or("ゲームが開始されていません")?;

    let choice = match choice.as_str() {
        "accept" => gm_core::DecisionChoice::Accept,
        "push" => gm_core::DecisionChoice::Push,
        "buy" => gm_core::DecisionChoice::Buy {
            degree: degree.ok_or("buy には degree が必要です")?,
        },
        other => return Err(format!("不明な決断です: {other}")),
    };
    let r = gm_core::resolve_decision(&mut sess.state, &sess.scenario, choice).map_err(|e| {
        match e {
            gm_core::DecisionError::NoPending => "決断待ちの判定がありません".to_string(),
            gm_core::DecisionError::UnknownChallenge => {
                "この判定の定義が見つかりません (パッケージが更新された可能性)".to_string()
            }
            gm_core::DecisionError::NotPushable => "この判定は押せません".to_string(),
            gm_core::DecisionError::NotBuyable => "買い取れません (支払いが足りません)".to_string(),
        }
    })?;

    // 発火ビートの解決 (play_turn と同経路) と、次ターンへの語り素材の持ち越し。
    let resolved = resolve_recall(&sess.lore, &r.fired);
    let beat_texts: Vec<String> = resolved.iter().map(|b| b.narration.clone()).collect();
    // 継続文脈: 直前の語りに決断の結末を継ぎ足す (判定結末文・ビートを含む)。
    let base = std::mem::take(&mut sess.last_narration);
    sess.last_narration = carryover_narration(&base, &beat_texts, std::slice::from_ref(&r.check));
    // 経緯ログ: このターンの行に決断を併記する (中期記憶にも決断が残る)。
    if let Some(last) = sess.history.last_mut() {
        let what = if r.check.pushed {
            format!("／決断: 押して振り直し→{}", if r.check.success { "成功" } else { "失敗" })
        } else if r.check.spent > 0 {
            format!("／決断: {} を {} 払って成功に変えた",
                r.spent.as_ref().map(|(f, _)| f.as_str()).unwrap_or("代償"), r.check.spent)
        } else {
            "／決断: 失敗を受け入れた".to_string()
        };
        last.summary.push_str(&what);
    }
    // 還流: 凍結行を最終結果で差し替え (pending のままだと note から除外され GM が知らない)。
    if let Some(slot) = sess.pending_checks.iter_mut().find(|c| c.pending) {
        *slot = r.check.clone();
    }
    sess.pending_lore.extend(resolved.iter().flat_map(|b| b.recalled.clone()));

    let beats: Vec<BeatView> = resolved
        .iter()
        .map(|b| BeatView {
            narration: normalize(&b.narration),
            recalled: b.recalled.iter().map(|f| normalize(&f.text)).collect(),
            image: b.image.as_ref().and_then(|id| {
                resolve_asset(&sess.package_root, AssetKind::Images, id)
                    .map(|p| p.to_string_lossy().into_owned())
            }),
            image_mode: b.image_mode.map(|m| match m {
                ImageMode::Background => "background".to_string(),
                ImageMode::Overlay => "overlay".to_string(),
            }),
            sound: b.sound.as_ref().and_then(|id| {
                resolve_asset(&sess.package_root, AssetKind::Audios, id)
                    .map(|p| p.to_string_lossy().into_owned())
            }),
        })
        .collect();
    let check = CheckView {
        entity: r.check.entity.clone(),
        stat: r.check.stat.clone(),
        sides: r.check.sides,
        count: r.check.count,
        times: r.check.times,
        roll: r.check.roll,
        modifier: r.check.modifier,
        total: r.check.total,
        dc: r.check.dc,
        success: r.check.success,
        narration: normalize(&r.check.narration),
        sound: (!r.check.sound.is_empty())
            .then(|| resolve_asset(&sess.package_root, AssetKind::Audios, &r.check.sound))
            .flatten()
            .map(|p| p.to_string_lossy().into_owned()),
        degree: r.check.degree.clone(),
        pushed: r.check.pushed,
        spent: r.check.spent,
        pending: false,
    };
    let stat_roll_views: Vec<StatRollView> = r
        .stat_rolls
        .iter()
        .map(|sr| StatRollView {
            entity: sr.entity.clone(),
            key: sr.key.clone(),
            count: sr.count,
            sides: sr.sides,
            bonus: sr.bonus,
            rolls: sr.rolls.clone(),
            amount: sr.amount,
        })
        .collect();

    // 決断で正本が動いた → autosave (受理ターンと同じ流儀。失敗は警告のみ)。
    if let Some(path) = sess.save_path.clone() {
        let save = session_save_of(sess);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = save_session(&path, &save) {
            eprintln!("[警告] オートセーブ失敗: {e}");
        }
    }

    let (goal_id, goal_title, goal_narration) = goal_view(&sess.state, &sess.scenario);
    Ok(DecisionResultView {
        check,
        stat_rolls: stat_roll_views,
        beats,
        spent_from: r.spent.as_ref().map(|(f, _)| f.clone()),
        spent_amount: r.spent.as_ref().map(|(_, a)| *a),
        push_paid_from: r.push_paid.as_ref().map(|(f, _)| f.clone()),
        push_paid_amount: r.push_paid.as_ref().map(|(_, a)| *a),
        state: state_view(&sess.state, &sess.scenario, &sess.history),
        goal_reached: is_goal(&sess.state, &sess.scenario),
        goal_id,
        goal_title,
        goal_narration,
        decision: decision_view(&sess.state, &sess.scenario),
        map: map_view(&sess.scenario, &sess.state, &sess.history, &sess.package_root),
    })
}

/// 対決 1 ラウンドの結果 view (spec 18 Phase C)。
#[derive(Serialize)]
struct ContestRoundView {
    /// player の振り (success = 勝ち)。伏せカードで開く。
    player: CheckView,
    /// 相手の振り (即開示)。narration にはラウンド帰結文を載せる (表示の合流点)。
    opponent: CheckView,
    /// player 視点: win / lose / tie。
    outcome: String,
    stat_rolls: Vec<StatRollView>,
    beats: Vec<BeatView>,
    /// 決着したら Some。digest は GM へ還流した 1 行と同じ文。
    ended: Option<ContestEndView>,
    state: StateView,
    goal_reached: bool,
    goal_id: Option<String>,
    goal_title: Option<String>,
    goal_narration: Option<String>,
    /// 続いていれば Some (次ラウンドの帳簿)。決着後は None。
    contest: Option<ContestView>,
    map: MapView,
}

#[derive(Serialize)]
struct ContestEndView {
    rounds: u32,
    wins: u32,
    losses: u32,
    ties: u32,
    reason: String,
    digest: String,
}

/// 対決を 1 ラウンド進める (spec 18 Phase C)。LLM は呼ばれない (トークンゼロ)。
/// 決着時は digest を継続文脈 + 経緯ログへ併記し (次の GM ターンが読む)、autosave する。
#[tauri::command]
async fn play_contest_round(
    session: tauri::State<'_, SharedSession>,
) -> Result<ContestRoundView, String> {
    let mut guard = session.lock().await;
    let sess = guard.as_mut().ok_or("ゲームが開始されていません")?;

    let r = gm_core::contest_round(&mut sess.state, &sess.scenario).map_err(|e| match e {
        gm_core::ContestError::NoContest => "進行中の対決がありません".to_string(),
        gm_core::ContestError::UnknownContest => {
            "この対決の定義が見つかりません (パッケージが更新された可能性)".to_string()
        }
    })?;

    // 発火ビート (帰結からの連鎖)。lore 解決は play_turn と同経路。
    let resolved = resolve_recall(&sess.lore, &r.fired);
    sess.pending_lore.extend(resolved.iter().flat_map(|b| b.recalled.clone()));
    let beats: Vec<BeatView> = resolved
        .iter()
        .map(|b| BeatView {
            narration: normalize(&b.narration),
            recalled: b.recalled.iter().map(|f| normalize(&f.text)).collect(),
            image: b.image.as_ref().and_then(|id| {
                resolve_asset(&sess.package_root, AssetKind::Images, id)
                    .map(|p| p.to_string_lossy().into_owned())
            }),
            image_mode: b.image_mode.map(|m| match m {
                ImageMode::Background => "background".to_string(),
                ImageMode::Overlay => "overlay".to_string(),
            }),
            sound: b.sound.as_ref().and_then(|id| {
                resolve_asset(&sess.package_root, AssetKind::Audios, id)
                    .map(|p| p.to_string_lossy().into_owned())
            }),
        })
        .collect();

    let to_view = |c: &CheckOutcome, narration: &str, sound: &str| CheckView {
        entity: c.entity.clone(),
        stat: c.stat.clone(),
        sides: c.sides,
        count: c.count,
        times: c.times,
        roll: c.roll,
        modifier: c.modifier,
        total: c.total,
        dc: c.dc,
        success: c.success,
        narration: normalize(narration),
        sound: (!sound.is_empty())
            .then(|| resolve_asset(&sess.package_root, AssetKind::Audios, sound))
            .flatten()
            .map(|p| p.to_string_lossy().into_owned()),
        degree: c.degree.clone(),
        pushed: false,
        spent: 0,
        pending: false,
    };
    // ラウンド帰結文/SE は相手側の行に載せる (player カード開帳 → 相手の行と同時に結末が出る)。
    let player = to_view(&r.player, "", "");
    let opponent = to_view(&r.opponent, &r.narration, &r.sound);

    // 決着: digest を GM の継続文脈 + 経緯ログへ (次ターンの語りの素)。
    let ended = r.ended.as_ref().map(|end| {
        let digest = harness::contest_digest(end);
        let base = std::mem::take(&mut sess.last_narration);
        sess.last_narration = carryover_narration(&base, std::slice::from_ref(&digest), &[]);
        if let Some(h) = sess.history.last_mut() {
            h.summary.push_str(&format!("／{digest}"));
        }
        ContestEndView {
            rounds: end.rounds,
            wins: end.wins,
            losses: end.losses,
            ties: end.ties,
            reason: end.reason.clone(),
            digest,
        }
    });

    // ラウンドごとに正本が動く → autosave (クラッシュしても交換の途中から再開できる)。
    if let Some(path) = sess.save_path.clone() {
        let save = session_save_of(sess);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = save_session(&path, &save) {
            eprintln!("[警告] オートセーブ失敗: {e}");
        }
    }

    let (goal_id, goal_title, goal_narration) = goal_view(&sess.state, &sess.scenario);
    let stat_roll_views = r
        .stat_rolls
        .iter()
        .map(|sr| StatRollView {
            entity: sr.entity.clone(),
            key: sr.key.clone(),
            count: sr.count,
            sides: sr.sides,
            bonus: sr.bonus,
            rolls: sr.rolls.clone(),
            amount: sr.amount,
        })
        .collect();
    Ok(ContestRoundView {
        player,
        opponent,
        outcome: r.outcome.clone(),
        stat_rolls: stat_roll_views,
        beats,
        ended,
        state: state_view(&sess.state, &sess.scenario, &sess.history),
        goal_reached: is_goal(&sess.state, &sess.scenario),
        goal_id,
        goal_title,
        goal_narration,
        contest: contest_view(&sess.state, &sess.scenario),
        map: map_view(&sess.scenario, &sess.state, &sess.history, &sess.package_root),
    })
}

pub fn run() {
    tauri::Builder::default()
        .manage(SharedSession::new(None))
        .setup(|app| {
            // 前回 set_llm_config が保存した app_data_dir/.env を読み込む (無ければ何もしない)。
            // **override で読む**: dev では main.rs の dotenvy が repo .env を先に読んでおり、
            // 非 override だと GUI で保存した設定が repo .env に毎回隠れる —「再起動すると
            // 別プロバイダに戻る」の真因。GUI の保存値 = ユーザーの最後の明示意思が唯一の真実。
            // (repo .env は CLI play 用。app_data/.env が未保存のキーは従来どおり repo 値が生きる。)
            if let Some(p) = config_env_path(app.handle()) {
                dotenvy::from_path_override(&p).ok();
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            new_game,
            resume_game,
            play_turn,
            resolve_dice_decision,
            play_contest_round,
            list_packages,
            list_save_slots,
            save_slot,
            load_slot,
            get_llm_config,
            set_llm_config,
            get_summary_llm_config,
            set_summary_llm_config,
            get_dev_mode,
            set_dev_mode,
            fetch_site_packages,
            install_site_package,
            check_package_updates,
            package_is_locally_edited,
            update_site_package,
            fetch_app_update,
            open_external_url,
            get_default_log_dir,
            save_log_file,
            open_log_folder,
            pick_package_folder,
            delete_autosave,
            facts_add,
            facts_edit,
            facts_delete
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::{meta_matches_site, normalize_path, normalize_site_url};

    /// 【SSRF 遮断 (spec 17 rev2 A-4)】照会に出るのは**現在設定の siteUrl と一致する
    /// 出所メタだけ**。細工メタ (別サイト) を手動配置されても、Kataribe が触りに行く先は
    /// 常にユーザー自身が登録したサイトに限られる。末尾スラッシュの揺れは吸収する。
    #[test]
    fn only_meta_from_the_configured_site_is_queried() {
        let site = normalize_site_url("https://kataribe.outcasts.jp/").unwrap();
        let meta = |url: &str| super::update::SourceMeta {
            site_url: url.to_string(),
            id: "id".into(),
            version: None,
            content_hash: "h".into(),
            tree_hash: "t".into(),
            installed_at_unix: 0,
        };
        assert!(meta_matches_site(&meta("https://kataribe.outcasts.jp"), &site));
        assert!(
            meta_matches_site(&meta("https://kataribe.outcasts.jp/"), &site),
            "末尾スラッシュの揺れは吸収する"
        );
        assert!(
            !meta_matches_site(&meta("https://evil.example"), &site),
            "別サイト由来のメタは照会に出ない"
        );
        assert!(
            !meta_matches_site(&meta("https://kataribe.outcasts.jp.evil.example"), &site),
            "前方一致では通さない (部分文字列の罠)"
        );
    }
    use std::path::{Path, PathBuf};

    /// 【本人未知属性の UI 秘匿 + 場所表示名 (2026-07-08)】`hidden_attributes` はプレイヤー UI
    /// から**本人分も含め**落ちる (secret は本人分は見える、の一段上)。`Location.title` は
    /// `StateView.location_title` として出る (空なら frontend が id へフォールバック)。
    #[test]
    fn state_view_drops_hidden_attributes_even_for_player_and_surfaces_location_title() {
        let sc = gm_core::Scenario::from_yaml(concat!(
            "title: t\nstart: v\n",
            "initial_attributes: { クラス: 剣士, 真の正体: 吸血鬼 }\n",
            "secret_attributes: [クラス]\n",
            "hidden_attributes: [真の正体]\n",
            "locations: { v: { title: 宿屋の広間, description: d, items: {}, exits: [] } }\n",
            "goal: { kind: always }\n"
        ))
        .unwrap();
        assert!(sc.validate().is_empty(), "{:?}", sc.validate());
        let s = sc.initial_state(1);
        let view = super::state_view(&s, &sc, &[]);

        let player = view.entities.iter().find(|e| e.id == "player").expect("player が居る");
        assert!(
            player.attributes.iter().any(|a| a.key == "クラス"),
            "secret 属性は本人分は見える (人狼の自役職と同じ)"
        );
        assert!(
            !player.attributes.iter().any(|a| a.key == "真の正体"),
            "hidden 属性は本人分も UI から落ちる (当人すら知らない)"
        );
        assert_eq!(view.location, "v", "id は機械用のまま");
        assert_eq!(view.location_title, "宿屋の広間", "表示名が DTO に出る");
    }

    /// 【更新判定 PoC (2026-07-15)】`/api/app` の version と現在版 (git タグ) の比較。
    /// `v` 前置・成分長の違い・非数値サフィックスに寛容で、真に新しい時だけ true。
    #[test]
    fn is_newer_detects_available_update_across_formats() {
        use super::is_newer;
        // 配布版が新しい → 更新あり (v 前置の有無混在も比較できる)。
        assert!(is_newer("v0.3.3", "v0.3.2"), "patch 上がり");
        assert!(is_newer("0.4.0", "v0.3.9"), "minor 上がり (v 前置混在)");
        assert!(is_newer("v1.0.0", "v0.9.9"), "major 上がり");
        // 同値・古い → 更新なし。
        assert!(!is_newer("v0.3.3", "v0.3.3"), "同一版は通知しない");
        assert!(!is_newer("v0.3", "v0.3.0"), "0.3 と 0.3.0 は同値 (0 埋め)");
        assert!(!is_newer("v0.3.2", "v0.3.3"), "現在版の方が新しければ通知しない");
        // 非数値サフィックスは数字先頭だけ採る (0-rc1 → 0)。プレリリース序列は扱わない割り切り。
        assert!(is_newer("v0.4.1", "v0.4.0-rc1"), "0-rc1 は patch 0 扱い → 0.4.1 が新しい");
        assert!(!is_newer("v0.4.0-rc2", "v0.4.0"), "0-rc2 も patch 0 扱い → 同値で通知しない");
    }

    /// 【統合・opt-in】実書庫の一覧 API が RemoteList へ deserialize できる (DTO の契約確認)。
    /// 実行: cargo test --lib -- --ignored fetch_site_packages_deserializes
    #[tokio::test]
    #[ignore = "稼働中の書庫サーバ (localhost:4000) が要る"]
    async fn fetch_site_packages_deserializes_live_list() {
        let base = std::env::var("KATARIBE_SITE_TEST_URL")
            .unwrap_or_else(|_| "http://localhost:4000".to_string());
        let list = super::fetch_site_packages(base, Some(1), None, None, Some("popular".into()))
            .await
            .expect("一覧が RemoteList へ deserialize できる");
        assert!(list.page >= 1 && list.page_size > 0, "ページネーション情報がある");
        // items が空でも契約違反ではない (dev の中身次第)。在れば必須フィールドが埋まっている。
        if let Some(p) = list.items.first() {
            assert!(!p.id.is_empty() && !p.title.is_empty());
        }
    }

    /// 【統合・opt-in】書庫 (実 dev サーバ or モック) から DL→検証→展開→manifest 読みの
    /// 全経路が通る。実行:
    ///   KATARIBE_SITE_TEST_URL=<base> KATARIBE_SITE_TEST_ID=<uuid> \
    ///     cargo test --lib -- --ignored install_from_site
    #[tokio::test]
    #[ignore = "稼働中の書庫サーバ (KATARIBE_SITE_TEST_URL) が要る"]
    async fn install_from_site_end_to_end() {
        let base = std::env::var("KATARIBE_SITE_TEST_URL")
            .unwrap_or_else(|_| "http://localhost:4000".to_string());
        let id = std::env::var("KATARIBE_SITE_TEST_ID").expect("KATARIBE_SITE_TEST_ID を設定");
        let data_dir = std::env::temp_dir().join(format!(
            "kataribe_site_e2e_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let installed = super::install_from_site(&base, &id, None, &data_dir)
            .await
            .expect("DL→検証→展開→manifest 読みが通る");
        assert!(
            Path::new(&installed.path).join("package.yaml").is_file(),
            "展開先に package.yaml がある"
        );
        assert!(!installed.title.is_empty(), "manifest の title が読めている");
        // spec 17 Phase A: 書庫取得物には出所メタが書かれ、tree_hash が現状と一致する
        // (install 直後 = 編集なし)。
        let meta = crate::update::read_source_meta(Path::new(&installed.path))
            .expect("出所メタが書かれている");
        assert!(!meta.content_hash.is_empty());
        assert_eq!(
            meta.tree_hash,
            crate::update::tree_hash(Path::new(&installed.path)).unwrap(),
            "install 直後の tree_hash は再計算と一致 (編集なし)"
        );
    }

    /// 【セーブスロット PoC (2026-07-16, spec 07 Phase D)】ファイル名は安定で、
    /// 5 スロットは互いに・オートセーブとも衝突しない (同フォルダ同居の前提)。
    #[test]
    fn save_file_names_are_stable_and_slots_distinct_from_autosave() {
        use super::{package_save_stem, save_file_name, SAVE_SLOTS};
        let stem = package_save_stem(Path::new(r"D:\pkgs\houkago"));
        assert_eq!(
            stem,
            package_save_stem(Path::new(r"D:\pkgs\houkago")),
            "同一パスは常に同じ stem (プロセス/バージョン非依存)"
        );
        assert_ne!(
            stem,
            package_save_stem(Path::new(r"D:\pkgs\escape")),
            "別パッケージは別 stem"
        );
        let auto = save_file_name(&stem, None);
        let slots: Vec<String> =
            (1..=SAVE_SLOTS).map(|n| save_file_name(&stem, Some(n))).collect();
        assert!(!slots.contains(&auto), "スロットはオートセーブと衝突しない");
        let uniq: std::collections::BTreeSet<&String> = slots.iter().collect();
        assert_eq!(uniq.len(), slots.len(), "{SAVE_SLOTS} スロットは互いに別ファイル");
        assert!(auto.ends_with(".yaml") && slots.iter().all(|s| s.ends_with(".yaml")));
    }

    /// 【セーブスロット PoC】一覧の語り冒頭 (snippet) は 60 字で切り (char 境界安全 =
    /// 日本語で panic しない)、literal `\n` も実改行も空白へ畳む (1 行表示)。
    #[test]
    fn narration_snippet_truncates_at_char_boundary_and_flattens_newlines() {
        use super::narration_snippet;
        let long = "あ".repeat(100);
        let s = narration_snippet(&long);
        assert_eq!(s.chars().count(), 61, "60 字 + …");
        assert!(s.ends_with('…'));
        assert_eq!(narration_snippet("一行目\\n二行目"), "一行目 二行目", "literal \\n は空白へ");
        assert_eq!(narration_snippet("一行目\n二行目"), "一行目 二行目", "実改行も空白へ");
        assert_eq!(narration_snippet("短い"), "短い", "短文はそのまま (… なし)");
    }

    /// 【マップ PoC (2026-07-16, spec 15)】可視範囲=霧: 訪問済み (a,b) とその1歩先 (c) だけ
    /// 出し、その先 (d) は霧で出さない。辺は visited ノードから出るものだけ (c→d は描かない)。
    #[test]
    fn map_view_shows_visited_and_one_hop_frontier_hiding_the_rest() {
        let sc = gm_core::Scenario::from_yaml(concat!(
            "title: t\nstart: a\n",
            "locations:\n",
            "  a: { description: d, items: {}, exits: [ { to: b } ] }\n",
            "  b: { description: d, items: {}, exits: [ { to: c } ] }\n",
            "  c: { description: d, items: {}, exits: [ { to: d } ] }\n",
            "  d: { description: d, items: {}, exits: [] }\n",
            "goal: { kind: always }\n"
        ))
        .unwrap();
        let mut s = sc.initial_state(1);
        s.location = "a".into();
        // 過去に b を通った記録 (現在地は a に戻っている)。
        let history =
            vec![harness::TurnLog { turn: 1, location: "b".into(), ..Default::default() }];
        let m = super::map_view(&sc, &s, &history, Path::new("."));

        let ids: std::collections::BTreeSet<&str> =
            m.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains("a") && ids.contains("b"), "訪問済み a,b は出る");
        assert!(ids.contains("c"), "1歩先 c は名前だけ出る (frontier)");
        assert!(!ids.contains("d"), "その先 d は霧 = 出さない");

        let a = m.nodes.iter().find(|n| n.id == "a").unwrap();
        assert!(a.current && a.visited, "a は現在地かつ訪問済み");
        let c = m.nodes.iter().find(|n| n.id == "c").unwrap();
        assert!(!c.visited && !c.current, "c は未踏 (frontier)");
        assert!(
            c.title.is_empty() && c.description.is_empty(),
            "frontier は名前・説明を伏せる (「？」表示 = ネタバレ回避)"
        );
        assert!(!a.title.is_empty() && a.description == "d", "訪問済みは名前と説明を持つ");

        // 辺は visited (a,b) から出るものだけ。c→d は描かない (c は frontier)。
        assert!(m.edges.iter().any(|e| e.from == "a" && e.to == "b"));
        assert!(m.edges.iter().any(|e| e.from == "b" && e.to == "c"));
        assert!(!m.edges.iter().any(|e| e.from == "c"), "frontier からの辺は描かない");
    }

    /// 【マップ PoC】gate 未達の出口は locked (🔒)、現在地は current。
    #[test]
    fn map_view_marks_locked_exits_and_current() {
        let sc = gm_core::Scenario::from_yaml(concat!(
            "title: t\nstart: a\n",
            "locations:\n",
            "  a: { description: d, items: {}, exits: [ { to: b, gate: { kind: flag_is, key: open, value: true } } ] }\n",
            "  b: { description: d, items: {}, exits: [] }\n",
            "goal: { kind: always }\n"
        ))
        .unwrap();
        let s = sc.initial_state(1); // 現在地 a、flag open は未設定 (=false)。
        let m = super::map_view(&sc, &s, &[], Path::new("."));

        let e = m.edges.iter().find(|e| e.from == "a" && e.to == "b").expect("a→b がある");
        assert!(e.locked, "gate (flag open=true) 未達なので locked (🔒)");
        assert!(m.nodes.iter().find(|n| n.id == "a").unwrap().current, "a が現在地");
    }

    /// 【`..` 畳み】asset protocol が拒否する `..` をパスから除去する (403 の原因対策)。
    #[test]
    fn normalize_path_collapses_parent_dirs() {
        #[cfg(windows)]
        let p = Path::new(r"D:\Github\Kataribe\app\src-tauri\..\..\packages\houkago");
        #[cfg(windows)]
        let want = PathBuf::from(r"D:\Github\Kataribe\packages\houkago");
        #[cfg(not(windows))]
        let p = Path::new("/home/u/proj/app/src-tauri/../../packages/houkago");
        #[cfg(not(windows))]
        let want = PathBuf::from("/home/u/proj/packages/houkago");
        let got = normalize_path(p);
        assert_eq!(got, want, ".. が畳まれ '..' を含まない");
        assert!(!got.to_string_lossy().contains(".."), "結果に '..' が残らない");
    }
}
