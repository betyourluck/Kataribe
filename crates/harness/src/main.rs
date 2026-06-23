//! `play` — 実クラウド LLM での通しプレイ CLI。
//!
//! `LlmClient` を [`DeltaProposer`](harness::DeltaProposer) として配線し、密室脱出を回す。
//! **ネットワーク必須**ゆえ単体テスト対象外。ここが核心的未知の測定器:
//! 「LLM がエンジンの制約内で構造化出力を出し続けられるか」を実地で観る。
//!
//! 使い方:
//! ```text
//! # .env に LLM_API_KEY を設定してから
//! cargo run -p harness --bin play                 # 対話 (stdin から行動を入力)
//! cargo run -p harness --bin play < actions.txt   # 台本を流し込む
//! cargo run -p harness --bin play scenarios/foo.yaml   # シナリオ指定
//! ```

use std::error::Error;
use std::io::{self, BufRead, Write};

use std::path::Path;

use gm_core::{is_goal, GameState, Lang, Scenario};
use harness::{inject_cast, load_lore, resolve_recall, run_turn, TurnOutcome};
use llm_client::{LlmClient, LlmConfig};

/// 既定シナリオ (cwd 非依存: crate からの相対で解決)。
const DEFAULT_SCENARIO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../scenarios/locked_room.yaml");
/// 1 ターンあたりの再生成上限。
const MAX_ATTEMPTS: u32 = 4;

/// 却下理由の表示言語。`KATARIBE_LANG=en` で英語、既定は日本語。
fn lang_from_env() -> Lang {
    match std::env::var("KATARIBE_LANG").as_deref() {
        Ok("en") | Ok("En") | Ok("EN") => Lang::En,
        _ => Lang::Ja,
    }
}
/// 初期 RNG seed (決定論再現用。将来は引数化)。
const SEED: u64 = 42;

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
    let client = LlmClient::new(config)?;

    // --- シナリオ ---
    let scenario_path = std::env::args().nth(1).unwrap_or_else(|| DEFAULT_SCENARIO.to_string());
    let yaml = std::fs::read_to_string(&scenario_path)
        .map_err(|e| format!("シナリオを読めません ({scenario_path}): {e}"))?;
    let mut scenario = Scenario::from_yaml(&yaml)?;

    // シナリオが cast 宣言した外部キャラ (scenarios/ の隣の characters/) だけを注入する。
    let chars_dir = Path::new(&scenario_path)
        .parent()
        .and_then(|p| p.parent())
        .map(|root| root.join("characters"))
        .unwrap_or_else(|| Path::new("characters").to_path_buf());
    inject_cast(&mut scenario, &chars_dir)?;

    // 伏線 lore (scenarios/ の隣の memoria/) をロード。トリガー発火点で recall する (memoria_bridge)。
    let lore_dir = Path::new(&scenario_path)
        .parent()
        .and_then(|p| p.parent())
        .map(|root| root.join("memoria"))
        .unwrap_or_else(|| Path::new("memoria").to_path_buf());
    let lore = load_lore(&lore_dir)?;
    eprintln!("[伏線] {} 件ロード", lore.len());

    // 初期 stat (HP/STR 等) をシナリオから読んで状態を作る。
    let mut state = scenario.initial_state(SEED);

    // --- 開幕描写 ---
    println!("=== {} ===", scenario.title);
    if let Some(loc) = scenario.location(&state.location) {
        println!("{}\n", loc.description);
    }
    println!("(行動を入力。Ctrl-D / Ctrl-Z で終了)\n");

    // --- ターンループ ---
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    // 直前ターンの発火で recall された伏線。次ターンの語りに織り込ませる (memoria_bridge, 輪の閉じ)。
    let mut pending_lore: Vec<harness::MemoryFragment> = Vec::new();
    // 直前ターンの技能判定の結果。次ターンの語りに還流する (出目は apply 後確定)。
    let mut pending_checks: Vec<gm_core::CheckOutcome> = Vec::new();
    loop {
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
            &pending_lore,   // 前ターンの伏線を注入
            &pending_checks, // 前ターンの判定結果を注入
        )
        .await;
        pending_lore = Vec::new(); // 注入済み。今ターンの発火で詰め直す。
        pending_checks = Vec::new();
        match outcome {
            Ok(TurnOutcome::Accepted { narration, rolls, checks, fired, attempts }) => {
                println!("\n{narration}");
                for r in &rolls {
                    let mark = if r.success { "成功" } else { "失敗" };
                    println!("  🎲 1d{} = {} (DC {}) → {mark}", r.sides, r.result, r.dc);
                }
                // 技能判定の結果 (出目+修正 vs DC)。次ターンの語りに還流する。
                for c in &checks {
                    let mark = if c.success { "成功" } else { "失敗" };
                    println!(
                        "  🎯 {} {} 判定: 1d{}({}){:+} = {} (DC {}) → {mark}",
                        c.entity, c.stat, c.sides, c.roll, c.modifier, c.total, c.dc
                    );
                }
                pending_checks = checks; // 次ターンへ持ち越し
                // 反応ビート (Phase C) + memoria_bridge: 発火点で伏線を recall して語りに注入。
                for beat in resolve_recall(&lore, &fired) {
                    println!("  ✦ {}", beat.narration);
                    for frag in &beat.recalled {
                        // 伏線 (不変 lore) を「思い出した記憶」として差し込む。
                        println!("    ┊ {}", frag.text.trim().replace('\n', "\n    ┊ "));
                        // 次ターンの語りに織り込ませるため持ち越す。
                        pending_lore.push(frag.clone());
                    }
                }
                // 核心的未知の計測: 何回の再生成で合法な ops に収束したか。
                if attempts > 1 {
                    println!("  [GM は {attempts} 回目の提案で筋を通した]");
                }
                println!(
                    "  [所在: {} / 所持: {} / 能力値: {}]",
                    state.location,
                    inventory(&state),
                    stats_line(&state),
                );

                if is_goal(&state, &scenario) {
                    println!("\n🎉 クリア。goal 到達 (turn {}).", state.turn);
                    break;
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

fn stats_line(state: &GameState) -> String {
    if state.entities.is_empty() {
        "なし".to_string()
    } else {
        state
            .entities
            .iter()
            .map(|(eid, stats)| {
                let kv = stats
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{eid}({kv})")
            })
            .collect::<Vec<_>>()
            .join(" / ")
    }
}
