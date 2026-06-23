//! Scenario / GameState を LLM 可読な文字列に落とす。
//!
//! 正本 (gm_core) が最終裁定するので、prompt は「嘘をつきにくくする」補助でしかない。
//! それでも gate/出口/アイテムを明示して見せることで、却下→再生成の回数を減らす。

use gm_core::{GameState, Scenario};
use gm_core::spine::Gate;

/// GM の役割定義。世界状態の変更は ops 経由のみ、という拘束を毎ターン刷り込む。
pub const GM_SYSTEM: &str = "\
あなたは TRPG のゲームマスター (GM) です。プレイヤーの行動に応じて物語を進めます。\n\
- narration には情景・NPC の台詞・行動の結果を自由に書いてよい。\n\
- 世界状態の変更 (アイテム取得・フラグ・移動・ダイス) は必ず ops に構造化して書くこと。\n\
- 存在しないアイテムの取得や、条件を満たさない移動を ops に書いても**エンジンに却下される**。\n\
  嘘の状態変更で物語を進めることはできない。今ある盤面の事実に忠実であること。\n\
- 何も状態が変わらない描写だけのターンなら ops は空でよい。\n\
必ず emit_delta ツールで {narration, ops} を提出すること。";

/// 条件 (Gate) を平易な日本語にする。LLM に前提条件を理解させるため。
fn gate_brief(gate: &Gate) -> String {
    match gate {
        Gate::Always => "条件なし".to_string(),
        Gate::HasItem { item } => format!("「{item}」を所持していること"),
        Gate::FlagIs { key, value } => format!("状態「{key}」が {value} であること"),
        Gate::LocationIs { at } => format!("「{at}」にいること"),
        Gate::StatAtLeast { key, value } => format!("能力「{key}」が {value} 以上であること"),
        Gate::All { of } => {
            let parts: Vec<String> = of.iter().map(gate_brief).collect();
            format!("すべて満たす({})", parts.join(" / "))
        }
        Gate::Any { of } => {
            let parts: Vec<String> = of.iter().map(gate_brief).collect();
            format!("いずれか満たす({})", parts.join(" / "))
        }
    }
}

/// シナリオの盤面を要約する (場所・アイテム・出口・ゴール)。
pub fn scenario_brief(scenario: &Scenario) -> String {
    let mut s = format!("# シナリオ: {}\n", scenario.title);
    s.push_str("\n## 場所\n");
    for (id, loc) in &scenario.locations {
        s.push_str(&format!("### {id}\n{}\n", loc.description));
        if !loc.items.is_empty() {
            s.push_str("- 取得可能アイテム:\n");
            for (item, gate) in &loc.items {
                s.push_str(&format!("  - {item} (取得条件: {})\n", gate_brief(gate)));
            }
        }
        if !loc.exits.is_empty() {
            s.push_str("- 出口:\n");
            for exit in &loc.exits {
                s.push_str(&format!("  - {} (移動条件: {})\n", exit.to, gate_brief(&exit.gate)));
            }
        }
    }
    s.push_str(&format!("\n## クリア条件\n{}\n", gate_brief(&scenario.goal)));
    s
}

/// 現在の正本状態を要約する。LLM が見てよい唯一の真実のスナップショット。
pub fn state_brief(state: &GameState) -> String {
    let inv = if state.inventory.is_empty() {
        "なし".to_string()
    } else {
        state.inventory.iter().cloned().collect::<Vec<_>>().join(", ")
    };
    let flags: Vec<String> = state
        .flags
        .iter()
        .filter(|(_, v)| **v)
        .map(|(k, _)| k.clone())
        .collect();
    let flags = if flags.is_empty() { "なし".to_string() } else { flags.join(", ") };
    format!(
        "# 現在の状態 (turn {})\n- 現在地: {}\n- 所持品: {}\n- 立っている状態: {}",
        state.turn, state.location, inv, flags,
    )
}

/// 却下された時に LLM へ戻す修正指示。理由をそのまま見せて ops を直させる (self_repair の核)。
pub fn rejection_feedback(reasons: &[String]) -> String {
    let mut s = String::from(
        "提出された ops はエンジンに却下されました。以下の理由をすべて解消し、\
         今ある盤面の事実だけを使って ops を修正し、emit_delta を再提出してください。\n",
    );
    for r in reasons {
        s.push_str(&format!("- {r}\n"));
    }
    s
}
