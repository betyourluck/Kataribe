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
- 盤面に『世界観』『主人公（プレイヤー）の設定』が与えられたら、それに沿って語ること。\
**NPC は主人公の設定（職業・年齢・立場など）を認識し、それに沿って接する**（例: 主人公が教師なら、\
生徒の NPC は教師として接する）。設定を無視してプレイヤーを別の立場として扱ってはならない。\n\
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
- **数値 (好感度・HP・能力値など) の変化は narration に書くだけでは正本に反映されない。\
必ず adjust_stat op で起こすこと。** 親しくなった・傷ついた等を語ったら、対応する数値変化を \
adjust_stat で出す (例: 好感度が上がる → adjust_stat で +1〜+3)。\n\
- **NPC の数値 (好感度など) を変える adjust_stat / scale_stat / check では、entity にその NPC の \
id を必ず指定すること。** entity を省略すると主人公に適用され、主人公がその数値を持たなければ \
却下されて変化が起きない (好感度は主人公でなく NPC が持つ)。『能力値』の表示で誰がどの数値を \
持つか確認してから entity を指定せよ。\n\
- 成否が不確実な行動 (力ずくの突破・隠れる・見破る・説得など) は結果を決めつけず、check op \
(entity / 修正に使う stat / sides / dc) でエンジンに判定させること。判定の出目と成否はエンジンが \
確定し**次のターンに返る**ので、それを見てから帰結を語る (この turn の narration では「試みる」までに留める)。\n\
- 判定結果が返ったら、**なぜ成功（または失敗）したのかを物語内の原因として後付けで語ること**。\
出目という確定事実に「理由」を与えて物語の因果に翻訳する (例: 成功=蝶番の緩みに気づいた／失敗=手が \
汗で滑った)。DC との差が大きいほど決定的に、僅差なら紙一重に、大失敗/大成功なら劇的に描く。\n\
- 存在しないアイテムの取得や、条件を満たさない移動を ops に書いても**エンジンに却下される**。\n\
  嘘の状態変更で物語を進めることはできない。今ある盤面の事実に忠実であること。\n\
- 何も状態が変わらない描写だけのターンなら ops は空でよい。\n\
narration と ops を必ず構造化出力として提出すること (ツール emit_delta、またはサーバ指示の JSON 形式)。";

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
        Gate::StatAtMost { entity, key, value } => {
            format!("{entity} の能力「{key}」が {value} 以下であること")
        }
        Gate::HasSkill { entity, skill } => format!("{entity} が能力「{skill}」を持っていること"),
        Gate::AttributeIs { entity, key, value } => {
            format!("{entity} の「{key}」が「{value}」であること")
        }
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

    // 世界観 (語りの素材)。情景・時代・舞台設定を語りに一貫させる。
    if !scenario.world.trim().is_empty() {
        s.push_str(&format!("\n## 世界観\n{}\n", scenario.world.trim()));
    }

    // 主人公(プレイヤー)の設定。**NPC はこの設定に沿ってプレイヤーを認識・反応する**
    // (例: 主人公が教師なら、生徒の NPC は教師として接する)。
    let p = &scenario.protagonist;
    if !p.name.trim().is_empty() || !p.profile.trim().is_empty() {
        s.push_str("\n## 主人公（プレイヤー）\n");
        if !p.name.trim().is_empty() {
            s.push_str(&format!("- 呼称: {}\n", p.name.trim()));
        }
        if !p.profile.trim().is_empty() {
            s.push_str(&format!("- 設定: {}\n", p.profile.trim()));
        }
    }

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
    // 各キャラの文字列属性 (クラス/職業/種族 等。可変。トリガーで書き換わる)。
    let attributes = if state.attributes.values().all(|a| a.is_empty()) {
        "なし".to_string()
    } else {
        state
            .attributes
            .iter()
            .filter(|(_, a)| !a.is_empty())
            .map(|(eid, a)| {
                let kv = a
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{eid}: {kv}")
            })
            .collect::<Vec<_>>()
            .join(" / ")
    };
    format!(
        "# 現在の状態 (turn {})\n- 現在地: {}\n- 所持品: {}\n- 立っている状態: {}\n- 能力値: {}\n- 使える能力: {}\n- 属性: {}",
        state.turn, state.location, inv, flags, entities, skills, attributes,
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

/// 直前ターンの語りを「いま続く情景」として渡し、**既出の描写の繰り返しを禁じる** (空なら空文字)。
///
/// ターンループは毎回 messages を新規構築する (state が唯一の真実) ため、LLM は自分が直前に
/// 何を語ったかの記憶を持たない → 静的情景 (時刻・天候・部屋の様子) や一度きりのビート (登場・挨拶)
/// をゼロから再establish して「情景がくどく二度出る」。直前の語りを継続文脈として渡し、
/// 「繰り返さず続きから変化だけ描け」と接地して継続性 (矛盾しない GM) を保つ。
pub fn recent_narration_note(prev: &str) -> String {
    if prev.trim().is_empty() {
        return String::new();
    }
    format!(
        "\n\n# 直前までの語り（情景はここから継続する。繰り返さないこと）\n\
        以下は直前のターンであなたが語った内容です。**既に確立した静的な情景（時刻・天候・\
        部屋の様子・既に済んだ登場・挨拶・相手の初対面の驚きなど）を再び描写しないこと**。\
        同じ説明を二度せず、この続きとして「変化・反応・新しい展開」だけを描いてください。\n---\n{}\n---\n",
        prev.trim()
    )
}

/// 直前ターンの技能判定の結果を、今回の語りに反映させる指示にする (空なら空文字)。
///
/// 出目・修正・合計・成否はエンジンが確定した**動かせない事実**。GM はこの結果に沿って
/// narration し、**なぜその結果になったかを物語内の原因として後付けで語る** (数字を因果へ翻訳)。
/// DC との差 (margin) と極 (tier) を渡すのは、後付けの強さを接地させるため — 大差なら決定的に、
/// 僅差なら紙一重に、大失敗/大成功なら劇的に。出目・成否は覆してはならない。
pub fn check_outcome_note(checks: &[CheckOutcome]) -> String {
    if checks.is_empty() {
        return String::new();
    }
    let mut s = String::from(
        "\n\n# 直前の判定結果（出目・成否はエンジンが確定済で覆せない）\n\
        この結果に沿って語り、**なぜ成功（または失敗）したのかを物語内の原因として後付けで説明すること**。\n\
        単に「成功した／失敗した」と述べるのでなく、DC との差をその場面の因果に翻訳せよ\
        （差が大きいほど決定的・鮮やかに、僅差なら辛うじて・紙一重に、大失敗/大成功なら劇的に）。\n",
    );
    for c in checks {
        let mark = if c.success { "成功" } else { "失敗" };
        let margin = c.total - c.dc as i64;
        let gap = if margin >= 0 {
            format!("DC を {margin} 上回った")
        } else {
            format!("DC に {} 届かなかった", -margin)
        };
        let tier = match &c.tier {
            Some(t) => format!("／極={t}（大失敗または大成功＝劇的に）"),
            None => String::new(),
        };
        s.push_str(&format!(
            "- {} の「{}」判定: 1d{}({}) + 修正{:+} = {} vs DC {} → {mark}（{gap}{tier}）\n",
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
