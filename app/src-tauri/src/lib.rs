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
    load_lore, load_package, read_manifest, resolve_recall, run_turn, LoreStore, MemoryFragment,
    TurnOutcome,
};
use llm_client::{LlmClient, LlmConfig};
use serde::Serialize;
use tokio::sync::Mutex;

/// 1 ターンあたりの再生成上限 (CLI `play` と同値)。
const MAX_ATTEMPTS: u32 = 4;
/// 初期 RNG seed (決定論再現。将来引数化)。
const SEED: u64 = 42;
/// 既定パッケージ (リポジトリ root からの相対フォルダ)。
const DEFAULT_PACKAGE: &str = "packages/houkago";

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
    /// 直前ターンの語り。次ターンに「続く情景」として渡し、既出描写の繰り返しを防ぐ (継続性)。
    last_narration: String,
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

/// パッケージ一覧の1項目 (GUI のフォルダ一覧表示用)。frontend が localStorage に持つパスごとに作る。
#[derive(Serialize)]
struct PackageEntry {
    /// localStorage が保持するパス (repo root 相対 or 絶対)。
    path: String,
    title: String,
    description: String,
    /// 単一シナリオ entry のみ今は playable (campaign-entry の load_package 対応は後続)。
    playable: bool,
    /// package.yaml が読めない等のエラー (一覧から外さず理由を表示する)。
    error: Option<String>,
}

/// localStorage 由来のパス列について、各 `package.yaml` の manifest を読み一覧 view を返す。
/// entry は解決しない (一覧は title/description だけ要る、campaign パッケージも一覧には出す)。
#[tauri::command]
async fn list_packages(paths: Vec<String>) -> Vec<PackageEntry> {
    let root = repo_root();
    paths
        .into_iter()
        .map(|p| {
            let dir = if Path::new(&p).is_absolute() {
                PathBuf::from(&p)
            } else {
                root.join(&p)
            };
            match read_manifest(&dir) {
                Ok(m) => {
                    let playable = !m.entry.contains("campaign");
                    PackageEntry {
                        path: p,
                        title: m.title,
                        description: m.description,
                        playable,
                        error: None,
                    }
                }
                Err(e) => PackageEntry {
                    path: p.clone(),
                    title: p,
                    description: String::new(),
                    playable: false,
                    error: Some(e.to_string()),
                },
            }
        })
        .collect()
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
/// .env file へ永続化 (repo root、dev ツール前提)。AIモデルタブの保存。
#[tauri::command]
fn set_llm_config(
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
    // 2) .env file に永続化 (再起動後も効く)。
    let updates = [
        ("LLM_BASE_URL".to_string(), base_url),
        ("LLM_MODEL".to_string(), model),
        ("LLM_API_KEY".to_string(), api_key),
        ("LLM_USE_TOOLS".to_string(), use_tools_s.to_string()),
    ];
    upsert_env(&repo_root().join(".env"), &updates).map_err(|e| format!(".env の保存に失敗: {e}"))
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

#[tauri::command]
async fn new_game(
    package_path: Option<String>,
    lang: Option<String>,
    session: tauri::State<'_, SharedSession>,
) -> Result<GameView, String> {
    let root = repo_root();
    let rel = package_path.unwrap_or_else(|| DEFAULT_PACKAGE.to_string());
    let pkg_dir = if Path::new(&rel).is_absolute() {
        PathBuf::from(&rel)
    } else {
        root.join(&rel)
    };

    // パッケージを読む (entry シナリオ + player/globals 注入 + 自己完結検査)。
    let loaded = load_package(&pkg_dir).map_err(|e| e.to_string())?;
    // 伏線 (パッケージ内 memoria/) をロード。無ければ空。
    let lore = load_lore(&pkg_dir.join("memoria")).map_err(|e| e.to_string())?;

    // LLM クライアント (.env は main で読み込み済)。
    let config = LlmConfig::from_env().map_err(|e| e.to_string())?;
    let client = LlmClient::new(config).map_err(|e| e.to_string())?;

    let scenario = loaded.scenario;
    let state = scenario.initial_state(SEED);
    let title = if loaded.manifest.title.is_empty() {
        scenario.title.clone()
    } else {
        loaded.manifest.title.clone()
    };
    let view = GameView {
        title,
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
        last_narration: String::new(),
        // 言語設定タブ由来の lang を優先、無ければ env 既定。
        lang: match lang.as_deref() {
            Some("en") | Some("En") | Some("EN") => Lang::En,
            Some("ja") | Some("Ja") | Some("JA") => Lang::Ja,
            _ => lang_from_env(),
        },
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
    )
    .await
    .map_err(|e| e.to_string())?;

    let view = match outcome {
        TurnOutcome::Accepted { narration, rolls, checks, fired, attempts } => {
            // 次ターンの継続文脈に持ち越す (既出情景の繰り返し防止)。
            sess.last_narration = narration.clone();
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
        .invoke_handler(tauri::generate_handler![
            new_game,
            play_turn,
            list_packages,
            get_llm_config,
            set_llm_config
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
