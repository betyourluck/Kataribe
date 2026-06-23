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

use std::path::{Path, PathBuf};

use gm_core::{is_goal, CheckOutcome, GameState, Lang, Scenario, PLAYER};
use harness::{
    inject_cast, load_lore, resolve_recall, run_turn, LoreStore, MemoryFragment, TurnOutcome,
};
use llm_client::{LlmClient, LlmConfig};
use serde::Serialize;
use tokio::sync::Mutex;

/// 1 ターンあたりの再生成上限 (CLI `play` と同値)。
const MAX_ATTEMPTS: u32 = 4;
/// 初期 RNG seed (決定論再現。将来引数化)。
const SEED: u64 = 42;
/// 既定シナリオ (リポジトリ root からの相対)。
const DEFAULT_SCENARIO: &str = "scenarios/locked_room.yaml";

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
}

#[derive(Serialize)]
struct StateView {
    turn: u32,
    location: String,
    inventory: Vec<String>,
    flags: Vec<String>,
    entities: Vec<EntityView>,
    goal_reached: bool,
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
    roll: u32,
    modifier: i64,
    total: i64,
    dc: u32,
    success: bool,
}

/// 発火した反応ビート + recall された伏線 (語りに織り込む素材)。
#[derive(Serialize)]
struct BeatView {
    narration: String,
    recalled: Vec<String>,
}

/// 開幕 view (new_game の戻り)。
#[derive(Serialize)]
struct GameView {
    title: String,
    location: String,
    description: String,
    state: StateView,
}

/// 1 ターンの結果 view (play_turn の戻り)。
#[derive(Serialize)]
struct TurnView {
    /// 受理されたか (false なら却下されつづけた)。
    accepted: bool,
    narration: String,
    rolls: Vec<RollView>,
    checks: Vec<CheckView>,
    beats: Vec<BeatView>,
    attempts: u32,
    /// 却下時の理由 (session.lang で localize 済み)。
    reasons: Vec<String>,
    state: StateView,
    goal_reached: bool,
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
    lang: Lang,
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

fn state_view(state: &GameState, scenario: &Scenario) -> StateView {
    // stat / skill / 所持物 のいずれかを持つ entity の和集合。
    let ids: std::collections::BTreeSet<&String> = state
        .entities
        .keys()
        .chain(state.skills.keys())
        .chain(state.inventory.keys())
        .collect();
    let entities = ids
        .into_iter()
        .map(|id| EntityView {
            id: id.clone(),
            stats: state
                .entities
                .get(id)
                .map(|stats| {
                    stats
                        .iter()
                        .map(|(k, v)| StatView { key: k.clone(), value: *v })
                        .collect()
                })
                .unwrap_or_default(),
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
        })
        .collect();
    StateView {
        turn: state.turn,
        location: state.location.clone(),
        // 上段の「所持品」は主人公の物 (NPC の所持物は登場人物ブロックに出る)。
        inventory: state
            .inventory
            .get(PLAYER)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default(),
        flags: state
            .flags
            .iter()
            .filter(|(_, v)| **v)
            .map(|(k, _)| k.clone())
            .collect(),
        entities,
        goal_reached: is_goal(state, scenario),
    }
}

// =============================================================================
// Tauri commands
// =============================================================================

#[tauri::command]
async fn new_game(
    scenario_path: Option<String>,
    session: tauri::State<'_, SharedSession>,
) -> Result<GameView, String> {
    let root = repo_root();
    let rel = scenario_path.unwrap_or_else(|| DEFAULT_SCENARIO.to_string());
    let scen_path = if Path::new(&rel).is_absolute() {
        PathBuf::from(&rel)
    } else {
        root.join(&rel)
    };

    let yaml = std::fs::read_to_string(&scen_path)
        .map_err(|e| format!("シナリオを読めません ({}): {e}", scen_path.display()))?;
    let mut scenario = Scenario::from_yaml(&yaml).map_err(|e| format!("シナリオの解析失敗: {e}"))?;

    // シナリオが cast 宣言した外部キャラだけを注入 (CLI と同経路、無差別注入しない)。
    inject_cast(&mut scenario, &root.join("characters")).map_err(|e| e.to_string())?;
    // 伏線 (memoria/) をロード。
    let lore = load_lore(&root.join("memoria")).map_err(|e| e.to_string())?;

    // LLM クライアント (.env は main で読み込み済)。
    let config = LlmConfig::from_env().map_err(|e| e.to_string())?;
    let client = LlmClient::new(config).map_err(|e| e.to_string())?;

    let state = scenario.initial_state(SEED);
    let view = GameView {
        title: scenario.title.clone(),
        location: state.location.clone(),
        description: scenario
            .location(&state.location)
            .map(|l| l.description.clone())
            .unwrap_or_default(),
        state: state_view(&state, &scenario),
    };

    *session.lock().await = Some(GameSession {
        state,
        scenario,
        lore,
        client,
        pending_lore: Vec::new(),
        pending_checks: Vec::new(),
        lang: lang_from_env(),
    });
    Ok(view)
}

#[tauri::command]
async fn play_turn(
    action: String,
    session: tauri::State<'_, SharedSession>,
) -> Result<TurnView, String> {
    let mut guard = session.lock().await;
    let sess = guard
        .as_mut()
        .ok_or("ゲームが開始されていません (先に new_game を呼んでください)")?;

    // 前ターンの伏線・判定結果を取り出して注入し、pending を空にする。
    let pending = std::mem::take(&mut sess.pending_lore);
    let pending_checks = std::mem::take(&mut sess.pending_checks);
    let outcome = run_turn(
        &sess.client,
        &mut sess.state,
        &sess.scenario,
        action.trim(),
        MAX_ATTEMPTS,
        sess.lang,
        &pending,
        &pending_checks,
    )
    .await
    .map_err(|e| e.to_string())?;

    let view = match outcome {
        TurnOutcome::Accepted { narration, rolls, checks, fired, attempts } => {
            // 発火ビートの cue を Memoria で解決 (memoria_bridge)。
            let resolved = resolve_recall(&sess.lore, &fired);
            let beats = resolved
                .iter()
                .map(|b| BeatView {
                    narration: normalize(&b.narration),
                    recalled: b.recalled.iter().map(|f| normalize(&f.text)).collect(),
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
                    roll: c.roll,
                    modifier: c.modifier,
                    total: c.total,
                    dc: c.dc,
                    success: c.success,
                })
                .collect();
            // 次ターンの語りに還流する判定結果を持ち越す。
            sess.pending_checks = checks;

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
                beats,
                attempts,
                reasons: Vec::new(),
                state: state_view(&sess.state, &sess.scenario),
                goal_reached: is_goal(&sess.state, &sess.scenario),
            }
        }
        TurnOutcome::Rejected { last_reasons, attempts } => TurnView {
            accepted: false,
            narration: String::new(),
            rolls: Vec::new(),
            checks: Vec::new(),
            beats: Vec::new(),
            attempts,
            reasons: last_reasons.iter().map(|r| r.localize(sess.lang)).collect(),
            // 却下では state 無傷。現状スナップショットを返す。
            state: state_view(&sess.state, &sess.scenario),
            goal_reached: is_goal(&sess.state, &sess.scenario),
        },
    };
    Ok(view)
}

pub fn run() {
    tauri::Builder::default()
        .manage(SharedSession::new(None))
        .invoke_handler(tauri::generate_handler![new_game, play_turn])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
