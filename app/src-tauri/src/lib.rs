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
/// tauri-plugin-shell を足さず std::process で完結させる (custom command ゆえ追加権限不要)。
fn open_in_file_manager(dir: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let program = "xdg-open";
    // explorer は成功時も非0を返すことがあるので、起動 (spawn) の成否だけを見る。
    std::process::Command::new(program)
        .arg(dir)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("フォルダを開けません: {e}"))
}

/// パッケージ別オートセーブの置き場 (spec 07 Phase C): `app_data_dir/saves/<パスのFNVハッシュ>.yaml`。
/// app data dir = OS 標準のアプリデータ置き場 — 配布 zip を差し替えてもセーブが消えない
/// (パッケージフォルダを汚さない)。パスの安定ハッシュでパッケージ別 1 autosave。
fn autosave_path(app: &tauri::AppHandle, pkg_dir: &Path) -> Option<PathBuf> {
    let dir = app.path().app_data_dir().ok()?.join("saves");
    let key = pkg_dir.to_string_lossy();
    // FNV-1a 64bit (依存ゼロ・プロセス/バージョン非依存の安定ハッシュ)。
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    Some(dir.join(format!("{h:016x}.yaml")))
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
            stats: state
                .entities
                .get(id)
                .map(|stats| {
                    stats
                        .iter()
                        // 内部用の帳簿 stat (hidden_stats) は状態パネルに出さない。
                        .filter(|(k, _)| !scenario.hidden_stats.contains(*k))
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
            attributes: state
                .attributes
                .get(id)
                .map(|a| {
                    a.iter()
                        // secret 属性 (役職等, spec 06) はプレイヤー UI では本人分のみ。
                        // NPC 分は DTO 段階で落とす (隠しゴールと同じネタバレ衛生)。
                        // hidden 属性 (本人未知) は**本人分も含め全員分**落とす — 当人すら
                        // 知らない正体・呪いを UI が漏らさない (GM prompt だけが見る)。
                        .filter(|(k, _)| {
                            !scenario.hidden_attributes.contains(*k)
                                && (id == PLAYER || !scenario.secret_attributes.contains(*k))
                        })
                        .map(|(k, v)| StatStrView { key: k.clone(), value: v.clone() })
                        .collect()
                })
                .unwrap_or_default(),
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
            // 帳簿フラグ (hidden_flags) は UI にも出さない (hidden_stats と同じ扱い)。
            .filter(|(k, v)| **v && !scenario.hidden_flags.contains(*k))
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
}

/// localStorage 由来のパス列について、各 `package.yaml` の manifest を読み一覧 view を返す。
/// entry は解決しない (一覧は title/description だけ要る、campaign パッケージも一覧には出す)。
#[tauri::command]
async fn list_packages(app: tauri::AppHandle, paths: Vec<String>) -> Vec<PackageEntry> {
    let root = repo_root();
    paths
        .into_iter()
        .map(|p| {
            let dir = if Path::new(&p).is_absolute() {
                PathBuf::from(&p)
            } else {
                root.join(&p)
            };
            // オートセーブの有無 (spec 07 Phase C)。読めない/版不一致は「続き無し」扱い (寛容)。
            let autosave_turn = autosave_path(&app, &normalize_path(&dir))
                .and_then(|sp| load_session(&sp).ok())
                .map(|s| s.state.turn);
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
                    }
                }
                Err(e) => PackageEntry {
                    path: p.clone(),
                    title: p,
                    description: String::new(),
                    playable: false,
                    error: Some(e.to_string()),
                    autosave_turn: None,
                },
            }
        })
        .collect()
}

/// パッケージのオートセーブを削除する (一覧から外す時の孤児セーブ掃除)。
/// セーブは `app_data/saves/<パスのハッシュ>.yaml` のファイルなので、localStorage の一覧から
/// パスを消すだけでは残り続けて溜まる。frontend が削除確認の上で呼ぶ。
/// 削除したら true、元々無ければ false (どちらも成功)。
#[tauri::command]
fn delete_autosave(app: tauri::AppHandle, package_path: String) -> Result<bool, String> {
    let root = repo_root();
    let dir = if Path::new(&package_path).is_absolute() {
        PathBuf::from(&package_path)
    } else {
        root.join(&package_path)
    };
    let save_path = autosave_path(&app, &normalize_path(&dir))
        .ok_or("アプリデータフォルダを解決できない")?;
    if save_path.exists() {
        std::fs::remove_file(&save_path).map_err(|e| format!("セーブの削除に失敗: {e}"))?;
        return Ok(true);
    }
    Ok(false)
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

/// 取得結果 (packagePaths へ登録する絶対パス + 表示用 title)。
#[derive(Serialize)]
struct InstalledPackage {
    path: String,
    title: String,
}

/// DL 受入上限 — サーバのファイル上限 100MB + 余裕 (無限ストリームへの蓋)。
const MAX_DOWNLOAD_BYTES: u64 = 110 * 1024 * 1024;

/// 配布サイトからパッケージ zip を DL し、検証・展開して packages 置き場に据える。
/// 展開先は `app_data_dir/packages/<フォルダ名>` (spec 07 saves と同じ流儀 — repo を汚さず、
/// 配布 zip の差し替えでも消えない)。zip 検証は `site::extract_package_zip`
/// (クライアント側でも zip slip 遮断 = サーバを信用しない二層)。
#[tauri::command]
async fn install_site_package(
    app: tauri::AppHandle,
    site_url: String,
    id: String,
) -> Result<InstalledPackage, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("アプリデータ置き場を解決できません: {e}"))?;
    install_from_site(&site_url, &id, &data_dir).await
}

/// install_site_package の本体 (Tauri 非依存 = 実サーバ相手の統合テストが書ける)。
async fn install_from_site(
    site_url: &str,
    id: &str,
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
    let download = async {
        let mut res = site_client()?
            .get(format!("{base}/api/packages/{id}/download"))
            .send()
            .await
            .map_err(|e| format!("配布サイトに接続できません: {e}"))?;
        if !res.status().is_success() {
            return Err(format!("ダウンロードに失敗しました: {}", res.status()));
        }
        let mut out = std::fs::File::create(&tmp)
            .map_err(|e| format!("一時ファイルを作成できません: {e}"))?;
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
    };
    if let Err(e) = download.await {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
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
        Ok(m) => Ok(InstalledPackage {
            path: installed.to_string_lossy().into_owned(),
            title: m.title,
        }),
        Err(e) => {
            // パッケージとして読めない配布物は据え置かない (一覧の恒久エラー行を作らない)。
            let _ = std::fs::remove_dir_all(&installed);
            Err(format!("パッケージとして読めません: {e}"))
        }
    }
}

/// パッケージを開く共通部 (new_game / resume_game): scope 許可 + entry 分岐ロード + 注入。
/// `module` 指定で campaign のそのモジュールを開く (再開用。単発パッケージでは無視)。
fn open_package(
    app: &tauri::AppHandle,
    rel: &str,
    module: Option<&ModuleId>,
) -> Result<(PathBuf, Scenario, Option<Campaign>, ModuleId, PackageManifest, Vec<String>), String> {
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
    let client = LlmClient::new(config).map_err(|e| e.to_string())?;

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
        // 言語設定タブ由来の lang を優先、無ければ env 既定。
        lang: match lang.as_deref() {
            Some("en") | Some("En") | Some("EN") => Lang::En,
            Some("ja") | Some("Ja") | Some("JA") => Lang::Ja,
            _ => lang_from_env(),
        },
    });
    Ok(view)
}

/// オートセーブから再開する (spec 07 Phase C)。パッケージは content 参照から再ロード
/// (骨格の単一真実源)、正本 state と語りの継続性 (chronicle/last_narration/pending_*) は
/// セーブから復元する。campaign は途中モジュール + campaign_memory も復元。
#[tauri::command]
async fn resume_game(
    app: tauri::AppHandle,
    package_path: String,
    lang: Option<String>,
    session: tauri::State<'_, SharedSession>,
) -> Result<GameView, String> {
    // セーブを先に読む (campaign の途中モジュール指定が open_package に要る)。
    let root = repo_root();
    let dir0 = if Path::new(&package_path).is_absolute() {
        PathBuf::from(&package_path)
    } else {
        root.join(&package_path)
    };
    let save_path =
        autosave_path(&app, &normalize_path(&dir0)).ok_or("アプリデータフォルダを解決できない")?;
    let save = load_session(&save_path).map_err(|e| e.to_string())?;

    let (pkg_dir, scenario, campaign, current_module, manifest, lint_warnings) =
        open_package(&app, &package_path, save.module.as_ref())?;
    let lore = load_lore(&pkg_dir.join("memoria")).map_err(|e| e.to_string())?;
    let config = LlmConfig::from_env().map_err(|e| e.to_string())?;
    let summarizer = LlmConfig::summary_from_env(&config)
        .map_err(|e| e.to_string())?
        .map(LlmClient::new)
        .transpose()
        .map_err(|e| e.to_string())?;
    let client = LlmClient::new(config).map_err(|e| e.to_string())?;

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
    };

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
        save_path: Some(save_path),
        package_path,
        synopsis: save.synopsis,
        summarizer,
        lang: match lang.as_deref() {
            Some("en") | Some("En") | Some("EN") => Lang::En,
            Some("ja") | Some("Ja") | Some("JA") => Lang::Ja,
            _ => lang_from_env(),
        },
    });
    Ok(view)
}

/// あらすじ圧縮ジョブを実行する (spec 10)。成功 = complete / 失敗 = abandon (非致命 —
/// あふれ契機は次ターン再計算、遷移契機は範囲凍結で同一リトライ)。
/// 要約は SUMMARY_LLM_* の専用 client、無ければ GM の client を共用する。
async fn run_synopsis_job(sess: &mut GameSession, job: &SynopsisJob) {
    let req = sess.synopsis.build_request(&sess.history, job);
    let result = match &sess.summarizer {
        Some(s) => s.summarize(&req).await,
        None => sess.client.summarize(&req).await,
    };
    match result {
        Ok(text) => sess.synopsis.complete(job, &text),
        Err(e) => {
            eprintln!("[警告] あらすじ要約に失敗 (プレイは続行し後で再試行): {e}");
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
    )
    .await
    .map_err(|e| e.to_string())?;

    let mut view = match outcome {
        TurnOutcome::Accepted {
            narration,
            summary,
            rolls,
            checks,
            fired,
            attempts,
            rejected,
            tags,
        } => {
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
                    run_synopsis_job(sess, &job).await;
                    ran_transition_job = true;
                }
                sess.current_module = adv.module_id;
                sess.scenario = adv.scenario;
                sess.state = adv.state;
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
                // キャンペーンは続くので入力を締めない (終端は advance=None で締まる)。
                view.goal_reached = false;
            }
        }
    }

    // spec 10: あふれ契機 (+ 遷移凍結のリトライ) の圧縮。受理ターンのみ・1 ターン高々 1 ジョブ。
    if view.accepted && !ran_transition_job {
        if let Some(job) = sess.synopsis.next_job(&sess.history) {
            let _ = app.emit("synopsis-compacting", ());
            run_synopsis_job(sess, &job).await;
        }
    }
    // spec 10: このターンの差分を view に載せる (あらすじは append-only ゆえ frontend は push のみ)。
    view.new_synopsis = sess.synopsis.entries[syn_before..].iter().map(synopsis_view).collect();
    view.new_log = sess.history[hist_before..].iter().map(log_line_view).collect();

    // オートセーブ (spec 07 Phase C): 受理ターン + campaign 遷移が全て確定したこの地点で書く。
    // 却下では書かない (state 無傷 = セーブも不変)。失敗は警告のみ (救済機構が本体を殺さない)。
    if view.accepted {
        if let Some(path) = sess.save_path.clone() {
            let save = SessionSave {
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
                synopsis: sess.synopsis.clone(),
            };
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = save_session(&path, &save) {
                eprintln!("[警告] オートセーブ失敗: {e}");
            }
        }
    }

    Ok(view)
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
            list_packages,
            get_llm_config,
            set_llm_config,
            get_summary_llm_config,
            set_summary_llm_config,
            get_dev_mode,
            set_dev_mode,
            fetch_site_packages,
            install_site_package,
            get_default_log_dir,
            save_log_file,
            open_log_folder,
            pick_package_folder,
            delete_autosave
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::normalize_path;
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
        let installed = super::install_from_site(&base, &id, &data_dir)
            .await
            .expect("DL→検証→展開→manifest 読みが通る");
        assert!(
            Path::new(&installed.path).join("package.yaml").is_file(),
            "展開先に package.yaml がある"
        );
        assert!(!installed.title.is_empty(), "manifest の title が読めている");
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
