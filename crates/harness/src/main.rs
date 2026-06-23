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
use harness::{load_characters, run_turn, TurnOutcome};
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

    // 外部キャラ定義 (scenarios/ の隣の characters/) を読み、inline に無い entity を補う。
    let chars_dir = Path::new(&scenario_path)
        .parent()
        .and_then(|p| p.parent())
        .map(|root| root.join("characters"))
        .unwrap_or_else(|| Path::new("characters").to_path_buf());
    for (id, def) in load_characters(&chars_dir)? {
        scenario.characters.entry(id).or_insert(def); // inline 優先、無ければ外部ファイル
    }

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
    loop {
        print!("> ");
        io::stdout().flush().ok();

        let action = match lines.next() {
            Some(Ok(l)) if !l.trim().is_empty() => l,
            Some(Ok(_)) => continue, // 空行はスキップ
            Some(Err(e)) => return Err(e.into()),
            None => break, // EOF
        };

        match run_turn(&client, &mut state, &scenario, action.trim(), MAX_ATTEMPTS, lang).await {
            Ok(TurnOutcome::Accepted { narration, rolls, attempts }) => {
                println!("\n{narration}");
                for r in &rolls {
                    let mark = if r.success { "成功" } else { "失敗" };
                    println!("  🎲 1d{} = {} (DC {}) → {mark}", r.sides, r.result, r.dc);
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
    if state.inventory.is_empty() {
        "なし".to_string()
    } else {
        state.inventory.iter().cloned().collect::<Vec<_>>().join(", ")
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
