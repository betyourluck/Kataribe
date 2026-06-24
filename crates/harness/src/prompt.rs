//! Scenario / GameState を LLM 可読な文字列に落とす。
//!
//! 正本 (gm_core) が最終裁定するので、prompt は「嘘をつきにくくする」補助でしかない。
//! それでも gate/出口/アイテムを明示して見せることで、却下→再生成の回数を減らす。

use gm_core::spine::Gate;
use gm_core::{CheckOutcome, GameState, Lang, RejectReason, Scenario};

use crate::memoria::MemoryFragment;

/// GM の役割定義。世界状態の変更は ops 経由のみ、という拘束を毎ターン刷り込む。
///
/// 重要: narration はエンジンに**検証されない** (op だけが裁定される)。よって「正本に反する
/// 出来事を narration で起こさない」一貫性は、エンジンのバックストップが効かず GM 自身が守るしかない。
/// プレイヤーの行動文も『意図』であって事実でないことを明示し、所持していない物の使用/譲渡を
/// 既成事実にさせない (= 行商ネックレス問題の prompt 層対策、failures #23)。
pub const GM_SYSTEM: &str = "\
あなたは TRPG のゲームマスター (GM) です。プレイヤーの行動に応じて物語を進めます。\n\
- narration には情景・NPC の台詞・行動の結果を自由に書いてよい。ただし**現在の状態 \
(所持品・立っている状態・所在・能力値) に反する出来事を起こしてはならない**。narration は \
エンジンに検証されないので、矛盾しない一貫性はあなた自身が守ること (それが「矛盾しない GM」の責務)。\n\
- **プレイヤーの行動文は『意図』であって事実ではない。** プレイヤーが所持品リストに無いアイテムを \
持っている・使う・渡す・見せると述べても、それは存在しない。盤面や所持品に無い事物を前提にした \
行動は、その前提を成り立たせてはならない。代わりに narration で「それは手元に無い」と物語の中で \
接地せよ (例: 鞄を探っても、そんな品は入っていない)。既成事実として書いてはならない。\n\
- **登場人物は『使える能力』に列挙された能力しか使えない。** そこに無い能力 (催眠・予知・隠された力 \
など) を、その場で思い出したように発揮させてはならない。能力は物語の都合で勝手に開花しない \
(開花するのは筋書きが定めた出来事のときだけで、それはエンジンが起こす)。未宣言の力で局面を \
打開する展開を narration に書くな = キャラを万能のメアリー・スーにしない。\n\
- 世界状態の変更 (アイテム取得・フラグ・移動・ダイス) は必ず ops に構造化して書くこと。\n\
- 成否が不確実な行動 (力ずくの突破・隠れる・見破る・説得など) は結果を決めつけず、check op \
(entity / 修正に使う stat / sides / dc) でエンジンに判定させること。判定の出目と成否はエンジンが \
確定し**次のターンに返る**ので、それを見てから帰結を語る (この turn の narration では「試みる」までに留める)。\n\
- 存在しないアイテムの取得や、条件を満たさない移動を ops に書いても**エンジンに却下される**。\n\
  嘘の状態変更で物語を進めることはできない。今ある盤面の事実に忠実であること。\n\
- 何も状態が変わらない描写だけのターンなら ops は空でよい。\n\
必ず emit_delta ツールで {narration, ops} を提出すること。";

/// 条件 (Gate) を平易な日本語にする。LLM に前提条件を理解させるため。
fn gate_brief(gate: &Gate) -> String {
    match gate {
        Gate::Always => "条件なし".to_string(),
        Gate::HasItem { entity, item } => format!("{entity} が「{item}」を所持していること"),
        Gate::FlagIs { key, value } => format!("状態「{key}」が {value} であること"),
        Gate::LocationIs { at } => format!("「{at}」にいること"),
        Gate::StatAtLeast { entity, key, value } => {
            format!("{entity} の能力「{key}」が {value} 以上であること")
        }
        Gate::HasSkill { entity, skill } => format!("{entity} が能力「{skill}」を持っていること"),
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

/// シナリオの盤面を要約する (登場人物・場所・アイテム・出口・ゴール)。
pub fn scenario_brief(scenario: &Scenario) -> String {
    let mut s = format!("# シナリオ: {}\n", scenario.title);

    // 登場人物の profile (設定・背景・性格・性向)。語りで一貫させるための素材。
    if !scenario.characters.is_empty() {
        s.push_str("\n## 登場人物\n");
        for (id, c) in &scenario.characters {
            let name = if c.name.is_empty() { id.as_str() } else { c.name.as_str() };
            s.push_str(&format!("### {name} ({id})\n"));
            if !c.profile.trim().is_empty() {
                s.push_str(&format!("{}\n", c.profile.trim()));
            }
        }
    }

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
    if !scenario.goals.is_empty() {
        s.push_str("\n## クリア条件 (いずれかの結末へ)\n");
        for g in &scenario.goals {
            s.push_str(&format!("- {}: {}\n", g.id, gate_brief(&g.when)));
        }
    } else if let Some(goal) = &scenario.goal {
        s.push_str(&format!("\n## クリア条件\n{}\n", gate_brief(goal)));
    }
    s
}

/// 現在の正本状態を要約する。LLM が見てよい唯一の真実のスナップショット。
pub fn state_brief(state: &GameState) -> String {
    // 所持物はキャラ別 (誰が何を持つかを LLM に見せる = 譲渡の前提)。
    let inv = if state.inventory.values().all(|s| s.is_empty()) {
        "なし".to_string()
    } else {
        state
            .inventory
            .iter()
            .filter(|(_, s)| !s.is_empty())
            .map(|(eid, s)| format!("{eid}: {}", s.iter().cloned().collect::<Vec<_>>().join(", ")))
            .collect::<Vec<_>>()
            .join(" / ")
    };
    let flags: Vec<String> = state
        .flags
        .iter()
        .filter(|(_, v)| **v)
        .map(|(k, _)| k.clone())
        .collect();
    let flags = if flags.is_empty() { "なし".to_string() } else { flags.join(", ") };
    // キャラ別の能力値 (entity ごとに 1 行)。
    let entities = if state.entities.is_empty() {
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
                format!("{eid}: {kv}")
            })
            .collect::<Vec<_>>()
            .join(" / ")
    };
    // 各キャラが使える能力 (閉世界。ここに無い能力は存在しない)。
    let skills = if state.skills.values().all(|s| s.is_empty()) {
        "なし".to_string()
    } else {
        state
            .skills
            .iter()
            .filter(|(_, s)| !s.is_empty())
            .map(|(eid, s)| format!("{eid}: {}", s.iter().cloned().collect::<Vec<_>>().join(", ")))
            .collect::<Vec<_>>()
            .join(" / ")
    };
    format!(
        "# 現在の状態 (turn {})\n- 現在地: {}\n- 所持品: {}\n- 立っている状態: {}\n- 能力値: {}\n- 使える能力: {}",
        state.turn, state.location, inv, flags, entities, skills,
    )
}

/// memoria_bridge: 直前ターンの発火で recall された伏線を、語りに織り込ませる指示にする。
///
/// 伏線は**不変 lore** であり状態変更ではない — だから「思い出す様子を narration に織り込め、
/// ops には書くな」と明示する (正本の境界を prompt 層でも守る)。空なら空文字 (注入しない)。
pub fn recalled_lore_note(fragments: &[MemoryFragment]) -> String {
    if fragments.is_empty() {
        return String::new();
    }
    let mut s = String::from(
        "\n\n# いま思い出された記憶（語りに織り込むこと）\n\
         直前の出来事をきっかけに、登場人物が次の記憶を想起しています。今回の narration に、\
         自然に思い出す様子として織り込んでください。これは状態変更ではないので ops には書かないこと。\n",
    );
    for f in fragments {
        // 改行は潰して 1 行の箇条書きにする (split_whitespace が前後空白も処理)。
        let text = f.text.split_whitespace().collect::<Vec<_>>().join(" ");
        s.push_str(&format!("- {text}\n"));
    }
    s
}

/// 直前ターンの技能判定の結果を、今回の語りに反映させる指示にする (空なら空文字)。
///
/// 出目・修正・合計・成否はエンジンが確定した**動かせない事実**。GM はこの結果に沿って
/// narration せよ (成功なら成功の、失敗なら失敗の帰結を描く)。出目を覆してはならない。
pub fn check_outcome_note(checks: &[CheckOutcome]) -> String {
    if checks.is_empty() {
        return String::new();
    }
    let mut s = String::from(
        "\n\n# 直前の判定結果（この結果に沿って語ること。出目はエンジンが確定済で覆せない）\n",
    );
    for c in checks {
        let mark = if c.success { "成功" } else { "失敗" };
        s.push_str(&format!(
            "- {} の「{}」判定: 1d{}({}) + 修正{:+} = {} (DC {}) → {mark}\n",
            c.entity, c.stat, c.sides, c.roll, c.modifier, c.total, c.dc
        ));
    }
    s
}

/// 却下された時に LLM へ戻す修正指示。構造化理由を `lang` でレンダリングして見せ、
/// ops を直させる (self_repair の核)。
pub fn rejection_feedback(reasons: &[RejectReason], lang: Lang) -> String {
    let mut s = String::from(
        "提出された ops はエンジンに却下されました。以下の理由をすべて解消し、\
         今ある盤面の事実だけを使って ops を修正し、emit_delta を再提出してください。\n",
    );
    for r in reasons {
        s.push_str(&format!("- {}\n", r.localize(lang)));
    }
    s
}
