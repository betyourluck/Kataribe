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

use gm_core::{is_goal, GameState, Scenario};
use harness::{run_turn, TurnOutcome};
use llm_client::{LlmClient, LlmConfig};

/// 既定シナリオ (cwd 非依存: crate からの相対で解決)。
const DEFAULT_SCENARIO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../scenarios/locked_room.yaml");
/// 1 ターンあたりの再生成上限。
const MAX_ATTEMPTS: u32 = 4;
/// 初期 RNG seed (決定論再現用。将来は引数化)。
const SEED: u64 = 42;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // .env を読み込む (アプリ入口の責務。LlmConfig::from_env は env を読むだけ)。
    dotenvy::dotenv().ok();

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
    let scenario = Scenario::from_yaml(&yaml)?;

    let mut state = GameState::new(scenario.start.clone(), SEED);

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

        match run_turn(&client, &mut state, &scenario, action.trim(), MAX_ATTEMPTS).await {
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
                println!("  [所在: {} / 所持: {}]", state.location, inventory(&state));

                if is_goal(&state, &scenario) {
                    println!("\n🎉 脱出成功。goal 到達 (turn {}).", state.turn);
                    break;
                }
            }
            Ok(TurnOutcome::Rejected { last_reasons, attempts }) => {
                println!("\n（GM は {attempts} 回試みたが、筋の通る一手を出せなかった）");
                for r in &last_reasons {
                    println!("  - {r}");
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
