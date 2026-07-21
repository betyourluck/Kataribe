//! `play` — 実クラウド LLM での通しプレイ CLI。
//!
//! `LlmClient` を [`DeltaProposer`](harness::DeltaProposer) として配線し、密室脱出を回す。
//! **ネットワーク必須**ゆえ単体テスト対象外。ここが核心的未知の測定器:
//! 「LLM がエンジンの制約内で構造化出力を出し続けられるか」を実地で観る。
//!
//! 使い方:
//! ```text
//! # .env に LLM_API_KEY を設定してから
//! cargo run -p harness --bin play                          # 既定シナリオ (対話)
//! cargo run -p harness --bin play scenarios/foo.yaml       # 単一シナリオ指定
//! cargo run -p harness --bin play --campaign campaigns/escape.yaml  # キャンペーン (複数モジュール通し)
//! cargo run -p harness --bin play --package packages/escape        # パッケージ (player/globals/world 注入込み)
//! cargo run -p harness --bin play --resume kataribe_autosave.yaml   # セーブから再開 (spec 07)
//! cargo run -p harness --bin play --save my_save.yaml               # オートセーブ先の指定 (既定 kataribe_autosave.yaml)
//! cargo run -p harness --bin play --seed 42                         # seed 固定 (テスト/再現用。省略時は毎回変わる)
//! cargo run -p harness --bin play < actions.txt            # 台本を流し込む
//! ```
//!
//! キャンペーンモードでは goal 到達ごとに [`advance_campaign`] が発火 GoalId で次モジュールを
//! 選び、状態を持ち越して骨格を差し替える (reached→transition の結線、PoC-2c)。

use std::error::Error;
use std::io::{self, BufRead, Write};

use std::path::{Path, PathBuf};

use gm_core::{GameState, Lang, Scenario};
use harness::{
    advance_campaign, inject_cast, load_campaign, load_lore, load_module, resolve_recall, run_turn,
    Campaign, Summarizer, TurnOutcome,
};
use llm_client::{LlmClient, LlmConfig};

/// 既定シナリオ (cwd 非依存: crate からの相対で解決)。houkago は fixtures 移設済。
const DEFAULT_SCENARIO: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/houkago/scenarios/classroom.yaml");
/// 1 ターンあたりの再生成上限。
const MAX_ATTEMPTS: u32 = 4;

/// 却下理由の表示言語。`KATARIBE_LANG=en` で英語、既定は日本語。
fn lang_from_env() -> Lang {
    match std::env::var("KATARIBE_LANG").as_deref() {
        Ok("en") | Ok("En") | Ok("EN") => Lang::En,
        _ => Lang::Ja,
    }
}
/// 初期 RNG seed を決める。既定は**毎ゲーム変える** (時刻由来) — 固定 seed だと配役
/// (role_assignment) も出目列も毎回同一になる (実プレイ発見: 主人公が常に占い師)。
/// 再現したい時は `KATARIBE_SEED=42` で固定 (seed は RngState に保存されセーブにも残る)。
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
/// オートセーブの既定パス (spec 07)。`--save <path>` で変更可。
const DEFAULT_SAVE: &str = "kataribe_autosave.yaml";

/// `--key <value>` 形式の引数を取り出す (無ければ None、値欠落は Err)。
fn take_flag_value(args: &mut Vec<String>, key: &str) -> Result<Option<String>, String> {
    match args.iter().position(|a| a == key) {
        Some(i) if i + 1 < args.len() => {
            let v = args.remove(i + 1);
            args.remove(i);
            Ok(Some(v))
        }
        Some(_) => Err(format!("{key} の後に値を指定してください")),
        None => Ok(None),
    }
}

/// あらすじ圧縮ジョブを実行する (spec 10)。成功 = complete / 失敗 = abandon (非致命 —
/// あふれ契機は次ターン再計算、遷移契機は範囲凍結で同一リトライ)。
/// 要約は SUMMARY_LLM_* の専用 client、無ければ GM の client を共用する。
async fn run_synopsis_job(
    summary_client: Option<&LlmClient>,
    gm_client: &LlmClient,
    synopsis: &mut harness::Synopsis,
    history: &[harness::TurnLog],
    job: &harness::SynopsisJob,
) {
    let req = synopsis.build_request(history, job);
    let result = match summary_client {
        Some(s) => s.summarize(&req).await,
        None => gm_client.summarize(&req).await,
    };
    match result {
        Ok(text) => synopsis.complete(job, &text),
        Err(e) => {
            eprintln!("[警告] あらすじ要約に失敗 (プレイは続行し後で再試行): {e}");
            synopsis.abandon(job);
        }
    }
}

/// エピローグ (spec 11)。到達 goal に epilogue_prompt があれば GM の client で 1 回生成して
/// 表示する (終端の呼び出し側 = ここが発火可否の最終判定)。失敗は skip — 結末文 + バナーの
/// 従来表示へフォールバック (非致命)。
async fn print_epilogue(
    client: &LlmClient,
    scenario: &Scenario,
    state: &gm_core::GameState,
    synopsis: &harness::Synopsis,
    history: &[harness::TurnLog],
    last_narration: &str,
) {
    let Some(goal) = scenario.reached_goal(state) else { return };
    if !goal.epilogue_prompt.as_deref().is_some_and(|p| !p.trim().is_empty()) {
        return;
    }
    println!("  （エピローグを紡いでいます…）");
    let req =
        harness::build_epilogue_request(scenario, goal, &synopsis.entries, history, last_narration);
    match harness::generate_epilogue(client, &req).await {
        Ok(text) => println!("\n―― エピローグ ――\n{}", text.replace("\\n", "\n")),
        Err(e) => eprintln!("[警告] エピローグ生成に失敗 (結末文で幕): {e}"),
    }
}

/// `parent/parent` を repo root とみなす (scenarios/ campaigns/ characters/ memoria/ の親)。
fn root_of(path: &str) -> PathBuf {
    Path::new(path)
        .parent()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // .env を読み込む (アプリ入口の責務。LlmConfig::from_env は env を読むだけ)。
    dotenvy::dotenv().ok();
    let lang = lang_from_env();

    // --- 設定 ---
    let config = match LlmConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            eprintln!("→ .env.example を .env にコピーし、LLM_API_KEY を設定してください。");
            std::process::exit(1);
        }
    };
    eprintln!("[接続] {} / model={}", config.base_url, config.model);
    // mut: シナリオ確定後に判定様式 (spec 16) の除外 op を設定する。
    let mut client = LlmClient::new(config)?;

    // --- シナリオ / キャンペーン / パッケージ / 再開 ---
    // `--campaign <path>` でキャンペーンモード、`--package <dir>` でパッケージモード
    // (player/globals/world を entry シナリオへ注入 = GUI と同じ経路)、
    // `--resume <file>` でセーブから再開 (content 参照から起動形を再構成)、
    // それ以外は単一シナリオ (第1引数 or 既定)。
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    // `--save <path>` はどのモードにも重ねられる (オートセーブ先。既定 DEFAULT_SAVE)。
    let save_path = match args.iter().position(|a| a == "--save") {
        Some(i) if i + 1 < args.len() => {
            let p = args.remove(i + 1);
            args.remove(i);
            PathBuf::from(p)
        }
        Some(i) => {
            args.remove(i);
            PathBuf::from(DEFAULT_SAVE)
        }
        None => PathBuf::from(DEFAULT_SAVE),
    };

    // spec 10: 要約モデルの別指定。優先順位 = 引数 > .env の SUMMARY_LLM_* > GM 設定共用。
    // 引数は env へ写すだけ (summary_from_env が一元解決)。app_data は探さない (app 専用)。
    if let Some(v) = take_flag_value(&mut args, "--summary-model")? {
        std::env::set_var("SUMMARY_LLM_MODEL", v);
    }
    if let Some(v) = take_flag_value(&mut args, "--summary-base-url")? {
        std::env::set_var("SUMMARY_LLM_BASE_URL", v);
    }
    let summary_client: Option<LlmClient> = match LlmConfig::summary_from_env(client.config())? {
        Some(c) => {
            eprintln!("[あらすじ要約] {} / model={}", c.base_url, c.model);
            Some(LlmClient::new(c)?)
        }
        None => None,
    };

    // `--seed <N>` で新規開始の seed を固定 (テスト/再現用)。台本テストが env より明示的に書ける。
    // 優先順位: --seed 引数 > KATARIBE_SEED env > 時刻エントロピー (resolve_seed)。
    let seed_arg: Option<u64> = match args.iter().position(|a| a == "--seed") {
        Some(i) if i + 1 < args.len() => {
            let v = args.remove(i + 1);
            args.remove(i);
            match v.trim().parse::<u64>() {
                Ok(n) => Some(n),
                Err(_) => return Err(format!("--seed の値が数値でない: {v}").into()),
            }
        }
        Some(_) => return Err("--seed の後に数値を指定してください".into()),
        None => None,
    };

    // セーブから再開するなら、content 参照で起動形 (campaign/package/scenario) を選び直す。
    let resume_save: Option<harness::SessionSave> =
        if args.first().map(String::as_str) == Some("--resume") {
            let p = args
                .get(1)
                .ok_or("--resume の後にセーブファイルのパスを指定してください")?;
            let save = harness::load_session(Path::new(p))?;
            eprintln!("[再開] {} (turn {})", p, save.state.turn);
            Some(save)
        } else {
            None
        };

    // 起動形の解決: resume 時はセーブの content、通常時は引数から。
    let (campaign_arg, package_arg, scenario_arg): (Option<String>, Option<String>, Option<String>) =
        match &resume_save {
            Some(save) => match &save.content {
                harness::SavedContent::Campaign { path } => (Some(path.clone()), None, None),
                harness::SavedContent::Package { path } => (None, Some(path.clone()), None),
                harness::SavedContent::Scenario { path } => (None, None, Some(path.clone())),
            },
            None => match args.first().map(String::as_str) {
                Some("--campaign") => (
                    Some(
                        args.get(1)
                            .ok_or("--campaign の後に campaign file のパスを指定してください")?
                            .clone(),
                    ),
                    None,
                    None,
                ),
                Some("--package") => (
                    None,
                    Some(
                        args.get(1)
                            .ok_or("--package の後に package フォルダのパスを指定してください")?
                            .clone(),
                    ),
                    None,
                ),
                Some(p) => (None, None, Some(p.to_string())),
                None => (None, None, Some(DEFAULT_SCENARIO.to_string())),
            },
        };

    // オートセーブに刻む content 参照 (再開時の再ロード元)。
    let content_ref: harness::SavedContent = if let Some(p) = &campaign_arg {
        harness::SavedContent::Campaign { path: p.clone() }
    } else if let Some(p) = &package_arg {
        harness::SavedContent::Package { path: p.clone() }
    } else {
        harness::SavedContent::Scenario {
            path: scenario_arg.clone().unwrap_or_default(),
        }
    };
    let mut package_version = String::new();

    let (campaign, mut current_module, mut scenario, root): (
        Option<Campaign>,
        Option<String>,
        Scenario,
        PathBuf,
    ) = if let Some(camp_path) = &campaign_arg {
        let camp = load_campaign(Path::new(camp_path))?;
        let root = root_of(camp_path);
        // 再開時はセーブに刻まれたモジュールから (無ければ開始モジュール)。
        let start = resume_save
            .as_ref()
            .and_then(|s| s.module.clone())
            .unwrap_or_else(|| camp.start.clone());
        let scen = load_module(&camp, &root, &start)?;
        eprintln!("[キャンペーン] {} / モジュール={start}", camp.title);
        (Some(camp), Some(start), scen, root)
    } else if let Some(dir) = &package_arg {
        let root = PathBuf::from(dir);
        let loaded = harness::load_package(&root)?;
        eprintln!("[パッケージ] {}", loaded.manifest.title);
        // 版不一致は警告のみ (typo 修正でセーブが全滅しないよう拒否しない)。
        if let Some(save) = &resume_save {
            if save.package_version != loaded.manifest.version {
                eprintln!(
                    "[警告] package の版がセーブ時 ({}) と異なる ({})。content 変更により整合しない可能性",
                    save.package_version, loaded.manifest.version
                );
            }
        }
        package_version = loaded.manifest.version.clone();
        (None, None, loaded.scenario, root)
    } else {
        let scenario_path = scenario_arg.unwrap_or_else(|| DEFAULT_SCENARIO.to_string());
        let yaml = std::fs::read_to_string(&scenario_path)
            .map_err(|e| format!("シナリオを読めません ({scenario_path}): {e}"))?;
        let mut scen = Scenario::from_yaml(&yaml)?;
        let root = root_of(&scenario_path);
        // シナリオが cast 宣言した外部キャラだけを注入する。
        inject_cast(&mut scen, &root.join("characters"))?;
        (None, None, scen, root)
    };

    // 伏線 lore (root の隣の memoria/) をロード。トリガー発火点で recall する (memoria_bridge)。
    let lore = load_lore(&root.join("memoria"))?;
    eprintln!("[伏線] {} 件ロード", lore.len());

    // 判定様式 (spec 16): 盤面が使わない判定 op を schema から落とす (GUI と同経路)。
    // campaign 遷移時は遷移先モジュールで再設定する (mut のまま保持)。
    client.set_excluded_ops(harness::excluded_check_ops(&scenario));

    // 初期 stat (HP/STR 等) をシナリオから読んで状態を作る。再開時はセーブの正本をそのまま使う。
    let mut state = match &resume_save {
        Some(save) => save.state.clone(),
        None => {
            let seed = seed_arg.unwrap_or_else(resolve_seed);
            eprintln!("[seed] {seed} (再現するには --seed {seed})");
            scenario.initial_state(seed)
        }
    };

    // あらすじ (spec 10)。再開時はセーブから復元 (凍結リトライ範囲込み)。
    let mut synopsis: harness::Synopsis =
        resume_save.as_ref().map(|s| s.synopsis.clone()).unwrap_or_default();

    // 約束事 (spec 20)。**編集は GUI 専用** (CLI からは読むだけ) — セーブから復元して
    // prompt へ注入し、autosave にそのまま書き戻す。
    let facts_list: Vec<harness::FactEntry> =
        resume_save.as_ref().map(|s| s.facts.clone()).unwrap_or_default();

    // --- 開幕描写 ---
    println!("=== {} ===", scenario.title);
    if let Some(loc) = scenario.location(&state.location) {
        println!("{}\n", loc.description);
    }
    if let Some(save) = &resume_save {
        println!("── セーブから再開 (turn {}) ──", save.state.turn);
        if !synopsis.entries.is_empty() {
            println!("（これまでのあらすじ）");
            for e in &synopsis.entries {
                println!("◆ {} — {}", e.title, e.text);
            }
            println!();
        }
        if !save.last_narration.is_empty() {
            println!("（前回までの語り）\n{}\n", save.last_narration);
        }
    }
    println!("(行動を入力。Ctrl-D / Ctrl-Z で終了)\n");
    eprintln!("[オートセーブ] {}", save_path.display());

    // --- ターンループ ---
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    // 直前ターンの発火で recall された伏線。次ターンの語りに織り込ませる (memoria_bridge, 輪の閉じ)。
    // 語りの継続性 (pending_*/last_narration/history) も再開時はセーブから戻す —
    // state だけでは「経緯を忘れる GM」に戻る (chronicle は state と独立の第二チャネル)。
    let mut pending_lore: Vec<harness::MemoryFragment> =
        resume_save.as_ref().map(|s| s.pending_lore.clone()).unwrap_or_default();
    // 直前ターンの技能判定の結果。次ターンの語りに還流する (出目は apply 後確定)。
    let mut pending_checks: Vec<gm_core::CheckOutcome> =
        resume_save.as_ref().map(|s| s.pending_checks.clone()).unwrap_or_default();
    // 直前ターンの語り。次ターンに「続く情景」として渡し、既出描写の繰り返しを防ぐ (継続性)。
    let mut last_narration =
        resume_save.as_ref().map(|s| s.last_narration.clone()).unwrap_or_default();
    // 経緯ログ (chronicle)。GM の書く summary を蓄積し「これまでの経緯」として還流する (中期記憶)。
    let mut history: Vec<harness::TurnLog> =
        resume_save.as_ref().map(|s| s.history.clone()).unwrap_or_default();
    // campaign の場所フラグ記憶 (spec 02)。再訪したモジュールで persistent フラグを復元する。
    let mut campaign_memory = resume_save
        .as_ref()
        .map(|s| s.campaign_memory.clone())
        .unwrap_or_default();
    loop {
        // オートセーブ (spec 07): 受理ターンと campaign 遷移が全て確定したこの地点で書く。
        // turn 0 (未プレイ) では書かない — 既存セーブを起動しただけで潰さないため。
        if state.turn > 0 {
            let save = harness::SessionSave {
                version: harness::SAVE_VERSION,
                content: content_ref.clone(),
                package_version: package_version.clone(),
                module: current_module.clone(),
                state: state.clone(),
                campaign_memory: campaign_memory.clone(),
                history: history.clone(),
                last_narration: last_narration.clone(),
                pending_checks: pending_checks.clone(),
                pending_lore: pending_lore.clone(),
                facts: facts_list.clone(),
                synopsis: synopsis.clone(),
            };
            if let Err(e) = harness::save_session(&save_path, &save) {
                // 救済機構がセッション本体を殺さない: 警告して続行。
                eprintln!("[警告] オートセーブ失敗: {e}");
            }
        }

        print!("> ");
        io::stdout().flush().ok();

        let action = match lines.next() {
            Some(Ok(l)) if !l.trim().is_empty() => l,
            Some(Ok(_)) => continue, // 空行はスキップ
            Some(Err(e)) => return Err(e.into()),
            None => break, // EOF
        };

        let outcome = run_turn(
            &client,
            &mut state,
            &scenario,
            action.trim(),
            MAX_ATTEMPTS,
            lang,
            &pending_lore,    // 前ターンの伏線を注入
            &pending_checks,  // 前ターンの判定結果を注入
            &last_narration,  // 前ターンの語りを継続文脈として注入 (繰り返し防止)
            &history,         // 経緯ログ (中期記憶)。過去ターンの要約を還流
            &synopsis.entries, // あらすじ (長期の物語記憶、spec 10)
            &facts_list,           // 約束事 (ピン留めの覚え書き、spec 20)
        )
        .await;
        pending_lore = Vec::new(); // 注入済み。今ターンの発火で詰め直す。
        pending_checks = Vec::new();
        match outcome {
            Ok(TurnOutcome::Accepted {
                narration,
                summary,
                rolls,
                checks,
                stat_rolls,
                fired,
                attempts,
                retries,
                tags,
            }) => {
                // literal `\n` を実改行へ (#16 の CLI 版。正本は触らない提示層の掃除)。
                println!("\n{}", narration.replace("\\n", "\n"));
                // 発火ビートを先に解決 (表示と GM への還流の素)。ビートは GM が見ていない
                // 筋書きの出来事なので、経緯ログと継続文脈の両方へ併記する。
                let beats = resolve_recall(&lore, &fired);
                let beat_texts: Vec<String> = beats.iter().map(|b| b.narration.clone()).collect();
                // 経緯ログに積む (GM の summary、無ければ narration 冒頭へ fallback。
                // tags/checks は engine 事実の機械タグ = retrieval の接地、spec 08-B)。
                history.push(harness::chronicle_entry(
                    state.turn,
                    action.trim(),
                    &summary,
                    &narration,
                    &beat_texts,
                    &tags,
                    &checks,
                ));
                // 次ターンの継続文脈に持ち越す (ビート込み)。
                last_narration = harness::carryover_narration(&narration, &beat_texts, &checks);
                for r in &rolls {
                    let mark = if r.success { "成功" } else { "失敗" };
                    println!("  🎲 1d{} = {} (DC {}) → {mark}", r.sides, r.result, r.dc);
                }
                // 技能判定の結果。percentile (degree あり) はロールアンダー書式 (spec 16)。
                for c in &checks {
                    if let Some(degree) = &c.degree {
                        let rel = if c.success { "≤" } else { ">" };
                        println!(
                            "  🎯 {} {} 判定: d100={} {rel} 目標値{} → {}",
                            c.entity,
                            c.stat,
                            c.roll,
                            c.dc,
                            harness::prompt::degree_label_ja(degree)
                        );
                        continue;
                    }
                    let mark = if c.success { "成功" } else { "失敗" };
                    let dice = if c.count > 1 || c.times > 1 {
                        let mult = if c.times > 1 { format!("×{}", c.times) } else { String::new() };
                        format!("{}d{}(合計{}){}", c.count, c.sides, c.roll, mult)
                    } else {
                        format!("1d{}({})", c.sides, c.roll)
                    };
                    println!(
                        "  🎯 {} {} 判定: {dice}{:+} = {} (DC {}) → {mark}",
                        c.entity, c.stat, c.modifier, c.total, c.dc
                    );
                }
                // 可変量ダイス (spec 16): 「SAN -4 (1d6=4)」— 出目まで監査可能に表示。
                for sr in &stat_rolls {
                    let dice: Vec<String> = sr.rolls.iter().map(|r| r.to_string()).collect();
                    let bonus = if sr.bonus != 0 { format!("{:+}", sr.bonus) } else { String::new() };
                    println!(
                        "  🎲 {} {} {:+} ({}d{}{}={})",
                        sr.entity,
                        sr.key,
                        sr.amount,
                        sr.count,
                        sr.sides,
                        bonus,
                        dice.join("+")
                    );
                }
                pending_checks = checks; // 次ターンへ持ち越し
                // spec 18 Phase C: 対決 (contest) は CLI では自動でラウンドを回し切る
                // (⚔ クリックは GUI の演出。台本流し込みと衝突させない)。決着 digest を
                // 継続文脈と経緯ログへ併記して GM に還流する。
                while state.pending_contest.is_some() {
                    match gm_core::contest_round(&mut state, &scenario) {
                        Ok(r) => {
                            let line = |c: &gm_core::CheckOutcome| -> String {
                                if let Some(deg) = &c.degree {
                                    format!(
                                        "{} d100={} ≤{} [{}]",
                                        c.entity,
                                        c.roll,
                                        c.dc,
                                        harness::prompt::degree_label_ja(deg)
                                    )
                                } else {
                                    format!("{} {}d{}({}){:+}={}", c.entity, c.count, c.sides, c.roll, c.modifier, c.total)
                                }
                            };
                            let mark = match r.outcome.as_str() {
                                "win" => "勝ち",
                                "lose" => "負け",
                                _ => "引き分け",
                            };
                            println!("  ⚔ {} vs {} → {mark}", line(&r.player), line(&r.opponent));
                            if !r.narration.is_empty() {
                                println!("    {}", r.narration);
                            }
                            for beat in resolve_recall(&lore, &r.fired) {
                                println!("  ✦ {}", beat.narration);
                            }
                            if let Some(end) = r.ended {
                                let digest = harness::contest_digest(&end);
                                println!("  [{digest}]");
                                last_narration =
                                    harness::carryover_narration(&last_narration, std::slice::from_ref(&digest), &[]);
                                if let Some(h) = history.last_mut() {
                                    h.summary.push_str(&format!("／{digest}"));
                                }
                            }
                        }
                        Err(_) => break, // 防御 (定義消失等) — 帳簿は engine 側で破棄済み
                    }
                }
                // spec 18 Phase B: 決断つき判定 (プッシュ/差分買い) の決断 UI は GUI の責務 —
                // CLI (作者テスト用) は**自動 Accept** で失敗帰結を確定して進む (対話プロンプトは
                // 台本流し込みと衝突するため足さない)。最終 check を還流用に差し替える。
                while !state.pending_decisions.is_empty() {
                    match gm_core::resolve_decision(&mut state, &scenario, gm_core::DecisionChoice::Accept) {
                        Ok(r) => {
                            println!(
                                "  [決断つき判定 → CLI は自動で失敗を受け入れた (プッシュ/買いは GUI で)]"
                            );
                            if !r.check.narration.is_empty() {
                                println!("  {}", r.check.narration);
                            }
                            for b in resolve_recall(&lore, &r.fired) {
                                println!("  ✦ {}", b.narration);
                            }
                            // 凍結行を最終結果で差し替え (pending は還流されないため)。
                            if let Some(slot) = pending_checks.iter_mut().find(|c| c.pending) {
                                *slot = r.check;
                            }
                        }
                        Err(_) => break, // 防御 (UnknownChallenge 等) — 凍結は engine 側で破棄済み
                    }
                }
                // 反応ビート (Phase C) + memoria_bridge: 発火点で伏線を recall して語りに注入。
                for beat in beats {
                    println!("  ✦ {}", beat.narration);
                    for frag in &beat.recalled {
                        // 伏線 (不変 lore) を「思い出した記憶」として差し込む。
                        println!("    ┊ {}", frag.text.trim().replace('\n', "\n    ┊ "));
                        // 次ターンの語りに織り込ませるため持ち越す。
                        pending_lore.push(frag.clone());
                    }
                }
                // 核心的未知の計測: 何回の再生成で合法な ops に収束したか + なぜ却下されたか。
                if attempts > 1 {
                    println!("  [GM は {attempts} 回目の提案で筋を通した]");
                    // やり直しの原因は二種 (却下 / 出力が壊れて読めなかった)。区別して出す。
                    for (i, cause) in retries.iter().enumerate() {
                        match cause {
                            harness::RetryCause::Rejected(reasons) => {
                                let why: Vec<String> =
                                    reasons.iter().map(|r| r.localize(lang)).collect();
                                println!("    ✗ {} 回目却下: {}", i + 1, why.join(" / "));
                            }
                            harness::RetryCause::Malformed { detail } => {
                                println!("    ✗ {} 回目は出力が壊れて読めなかった: {detail}", i + 1);
                            }
                        }
                    }
                }
                println!(
                    "  [所在: {} / 所持: {} / 能力値: {}]",
                    state.location,
                    inventory(&state),
                    stats_line(&state, &scenario),
                );

                // goal 到達処理: キャンペーンなら発火 GoalId で次モジュールへ遷移、単発なら終了。
                if let Some(reached) = scenario.reached(&state) {
                    match &campaign {
                        Some(camp) => {
                            let from = current_module.as_deref().unwrap_or("");
                            match advance_campaign(camp, &root, &mut campaign_memory, from, &scenario, &state)? {
                                // 辺が在る = 次モジュールへ。状態を持ち越し骨格だけ差し替える。
                                Some(adv) => {
                                    // spec 10: 章の締めであらすじ圧縮 (章替わりマーカーを刻む前 =
                                    // 新章の行を範囲に混ぜない。章題は遷移元モジュールの title)。
                                    if let Some(job) = synopsis.on_transition(&history, &scenario.title)
                                    {
                                        println!("  （あらすじをまとめています…）");
                                        run_synopsis_job(
                                            summary_client.as_ref(),
                                            &client,
                                            &mut synopsis,
                                            &history,
                                            &job,
                                        )
                                        .await;
                                    }
                                    println!(
                                        "\n━━ エンディング『{reached}』→ 次モジュール『{}』へ ━━",
                                        adv.scenario.title
                                    );
                                    current_module = Some(adv.module_id);
                                    scenario = adv.scenario;
                                    state = adv.state;
                                    // 判定様式は遷移先モジュールに従う (spec 16)。
                                    client.set_excluded_ops(harness::excluded_check_ops(&scenario));
                                    pending_lore = Vec::new();
                                    pending_checks = Vec::new();
                                    last_narration = String::new(); // 新モジュール=新しい情景
                                    // 経緯は捨てない (跨いで覚えるのが chronicle の眼目)。章替わりを刻む。
                                    history.push(harness::TurnLog {
                                        turn: state.turn,
                                        player: "（章の移り変わり）".into(),
                                        summary: format!("『{}』へ移った", scenario.title),
                                        ..Default::default()
                                    });
                                    println!("=== {} ===", scenario.title);
                                    if let Some(loc) = scenario.location(&state.location) {
                                        println!("{}\n", loc.description);
                                    }
                                    continue;
                                }
                                // 辺が無い = 終端エンディング。バナー → エピローグで幕 (spec 11)。
                                None => {
                                    println!(
                                        "\n🎉 キャンペーン完了。エンディング『{reached}』(turn {}).",
                                        state.turn
                                    );
                                    print_epilogue(
                                        &client, &scenario, &state, &synopsis, &history,
                                        &last_narration,
                                    )
                                    .await;
                                    break;
                                }
                            }
                        }
                        None => {
                            // 単発シナリオのクリア。バナー → エピローグで幕 (spec 11)。
                            println!("\n🎉 クリア。goal 到達 (turn {}).", state.turn);
                            print_epilogue(
                                &client, &scenario, &state, &synopsis, &history, &last_narration,
                            )
                            .await;
                            break;
                        }
                    }
                }

                // spec 10: あふれ契機 (+ 遷移凍結のリトライ) の圧縮。1 ターン高々 1 ジョブ。
                // 遷移ターンは上の分岐で continue/break 済みなのでここには来ない。
                if let Some(job) = synopsis.next_job(&history) {
                    println!("  （あらすじをまとめています…）");
                    run_synopsis_job(summary_client.as_ref(), &client, &mut synopsis, &history, &job)
                        .await;
                }
            }
            Ok(TurnOutcome::Rejected { last_reasons, attempts }) => {
                println!("\n（GM は {attempts} 回試みたが、筋の通る一手を出せなかった）");
                for r in &last_reasons {
                    println!("  - {}", r.localize(lang));
                }
                println!("  ※ 状態は変化していません。別の行動を試してください。");
            }
            Err(e) => {
                eprintln!("\n[エラー] {e}");
                eprintln!("→ ネットワーク / API キー / モデルの tool-use 対応を確認してください。");
            }
        }
    }

    println!("\nセッション終了 (turn {}).", state.turn);
    Ok(())
}

fn inventory(state: &GameState) -> String {
    if state.inventory.values().all(|s| s.is_empty()) {
        "なし".to_string()
    } else {
        state
            .inventory
            .iter()
            .filter(|(_, s)| !s.is_empty())
            .map(|(eid, s)| format!("{eid}: {}", s.iter().cloned().collect::<Vec<_>>().join(", ")))
            .collect::<Vec<_>>()
            .join(" / ")
    }
}

fn stats_line(state: &GameState, scenario: &Scenario) -> String {
    if state.entities.is_empty() {
        "なし".to_string()
    } else {
        state
            .entities
            .iter()
            .map(|(eid, stats)| {
                // プレイヤー向け表示なので hidden_stats (GM は見る秘密) も internal_stats
                // (engine 帳簿) も両方隠す。
                let kv = stats
                    .iter()
                    .filter(|(k, _)| {
                        !scenario.hidden_stats.contains(*k) && !scenario.internal_stats.contains(*k)
                    })
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{eid}({kv})")
            })
            .filter(|line| !line.ends_with("()"))
            .collect::<Vec<_>>()
            .join(" / ")
    }
}
