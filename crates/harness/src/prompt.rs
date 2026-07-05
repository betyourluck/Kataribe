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
- **『この場にいる』の一覧が、いまその場に居る人物の唯一の真実である。** 一覧に無いキャラを \
その場に居るように語ったり、台詞・行動をさせたりしてはならない — 場所の説明文にキャラが \
書かれていても、一覧に居なければその人物は**もうそこに居ない** (説明文は静的、一覧が現在)。\
逆に一覧に居るキャラを不在として扱うな。キャラの登場・退場をあなたが起こすことはできない \
(それは筋書きの出来事が起こす)。その場に居ない人物との会話・接触を narration に書くな。\n\
- **登場人物は『使える能力』に列挙された能力しか使えない。** そこに無い能力 (催眠・予知・隠された力 \
など) を、その場で思い出したように発揮させてはならない。能力は物語の都合で勝手に開花しない \
(開花するのは筋書きが定めた出来事のときだけで、それはエンジンが起こす)。未宣言の力で局面を \
打開する展開を narration に書くな = キャラを万能のメアリー・スーにしない。\n\
- 世界状態の変更 (アイテム取得・フラグ・移動・ダイス) は必ず ops に構造化して書くこと。\n\
- **盤面に『状態フラグ』が列挙されていたら、その条件 (例: 賢者から鍵の在処を聞く) が会話・出来事で \
満たされた瞬間に set_flag でそのフラグを立てること。** 知識や状態の獲得は narration に書くだけでは \
正本に残らない (次のターンには忘れる) ので、必ず ops で記録せよ。ただし条件をまだ満たしていないのに \
先回りで立ててはならない (満たされていなければエンジンが却下する)。\n\
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
- **盤面に『挑戦』が列挙され、プレイヤーの行動がそれに該当するなら、自分で check を組むのでなく \
attempt_challenge でその id を選ぶこと。** 難易度 (sides/dc) も成否で立つフラグも作者が定めており、\
エンジンが振って帰結を確定する (出目も結果も詐称できない)。能力に依らない運試しの挑戦もある。\n\
- 判定結果が返ったら、**なぜ成功（または失敗）したのかを物語内の原因として後付けで語ること**。\
出目という確定事実に「理由」を与えて物語の因果に翻訳する (例: 成功=蝶番の緩みに気づいた／失敗=手が \
汗で滑った)。DC との差が大きいほど決定的に、僅差なら紙一重に、大失敗/大成功なら劇的に描く。\n\
- **いつ振らないか / 1 回の判定の射程**: 武器を構える・身構える・対峙する・睨み合うは『態勢』であって \
決着ではない。ここでダイスを振るな (緊迫を描いて場を進め、決着は後のビートに委ねる)。**1 回の判定は \
その狭い行動だけを解決し、物語の山場の決着へ飛躍させてはならない** (例: 一突きの成功を『魔王を倒した』に \
拡大しない)。重要な相手 (名前付きの敵・魔王級) の撃破や物語の決定的な勝敗は、作者が定めた条件 \
(挑戦・ゴール) を複数のビートを経て満たしたときにのみ起きる。**エンジンが state に記録していない決着 \
(ゴール未達) は、まだ起きていない** — 大敵をあっけなく倒す・場を終わらせる帰結を narration に書くな。\n\
- 存在しないアイテムの取得や、条件を満たさない移動を ops に書いても**エンジンに却下される**。\n\
  嘘の状態変更で物語を進めることはできない。今ある盤面の事実に忠実であること。\n\
- 何も状態が変わらない描写だけのターンなら ops は空でよい。\n\
- **盤面に『投票』が宣言されていたら、投票できる局面ではその場の生存 NPC 全員分の票を \
cast_vote op で並べること** (voter にその NPC の id、target に投票先。票は narration に書く \
だけでは開票されない)。誰に入れるかは各キャラの性格・積み重なった疑念・秘匿役職に沿って \
あなたが決めてよい — それが推理劇の演出である。ただし **NPC の票をプレイヤーの票に \
引きずられて揃えるな** — 各 NPC は自分の視点だけから独立に決めよ。票が割れるのは自然で \
あり、全員一致は稀である (プレイヤーの指名が毎回そのまま処刑になるのは推理劇として破綻)。\
プレイヤーの票はプレイヤーの行動文の意図から汲んで cast_vote にせよ。投票権が一部の者に絞られた局面 (夜の狩り等) でも同じ — **その局面で \
投票できる者が生きているなら、その者の票を必ず cast_vote で出せ** (プレイヤーの行動が別のこと \
でも忘れるな)。票を出さなければその局面では何も起きない (狩りの不発)。現在の状態に \
**「いま投票が開いている」の行が出ていたら、それが合図である — そこに列挙された者の票を \
このターンの ops に必ず並べよ**。逆に、盤面の資料に \
**『## 投票』の節が無ければ、このゲームに投票の機構は存在しない — cast_vote を一切提案するな** \
(多数決らしい場面でも、意図は語りと他の op で表す)。**開票・処刑・襲撃の \
帰結はあなたが起こせない** (筋書きの出来事が確定する) — 誰が死ぬかを先取りして語るな。\n\
- **〔秘匿〕と注記された属性 (役職など) はゲーム的秘匿情報である。登場人物どうしは互いに \
知らない** — 各キャラは自分の分だけを知っている前提で演じよ (他人の秘匿属性を知っているかの \
ような行動・台詞をさせるな)。narration の地の文でこれを明かすな・匂わせの断定 (『人狼らしく』\
等) をするな — 疑いや推理は登場人物の台詞・行動として描け。役職能力の結果 (占いの判定等) は \
**当人だけの知識**であり、当人の口から語られるまで他キャラは知らない。秘匿属性に基づく人物の \
**隠密行動 (夜の襲撃等) を、実行者がわかる形で地の文に描くな** — 実行の瞬間は語らず、\
**結果だけ** (翌朝の発見・残された痕跡) を描け。誰の仕業かは盤面が明かすまで伏せたままにする。\n\
- **summary には、このターンの経緯 1 行 (誰が何をして何が起きたか、確定した事実だけ) を毎ターン \
書くこと。** これは以後のターンの『これまでの経緯』としてあなた自身に引き継がれる記憶になる — \
書かなければ経緯は忘れられる。物語的な修辞は不要、事実の記録に徹する \
(例: 「アリスに花を渡し、幼い頃の約束を打ち明けた」)。\n\
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
        Gate::TurnsSince { entity, key, turns } => {
            format!("{entity} の「{key}」に刻まれた時から {turns} ターン以上経つこと")
        }
        Gate::HasVoted { entity } => format!("{entity} が投票 (cast_vote) を済ませていること"),
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
            s.push_str("- アイテム:\n");
            for (item, li) in &loc.items {
                match li.take() {
                    // 備え付けは取れない旨と「その場で使える」を先回りで接地 (却下前に防ぐ)。
                    gm_core::TakeMode::Fixed => s.push_str(&format!(
                        "  - {item} (備え付け・取得不可。取らずにその場で使える)\n"
                    )),
                    gm_core::TakeMode::Infinite => s.push_str(&format!(
                        "  - {item} (取得条件: {}。何度でも取れる)\n",
                        gate_brief(li.when())
                    )),
                    gm_core::TakeMode::Once => s.push_str(&format!(
                        "  - {item} (取得条件: {})\n",
                        gate_brief(li.when())
                    )),
                }
            }
        }
        if !loc.exits.is_empty() {
            s.push_str("- 出口:\n");
            for exit in &loc.exits {
                s.push_str(&format!("  - {} (移動条件: {})\n", exit.to, gate_brief(&exit.gate)));
            }
        }
    }
    // authored challenge (技能判定)。LLM は attempt_challenge で id を選んで挑む。
    if !scenario.challenges.is_empty() {
        s.push_str("\n## 挑戦 (不確実な行動の判定。attempt_challenge で id を選んで挑む)\n");
        for (id, c) in &scenario.challenges {
            let label = if c.description.trim().is_empty() { id.as_str() } else { c.description.trim() };
            let basis = match &c.stat {
                Some(stat) => format!("{stat} 判定"),
                None => "運 (能力に依らない)".to_string(),
            };
            // 前提条件 (requires) があれば明示 — 満たすまでこの挑戦は選べない。
            let req = match &c.requires {
                Some(g) => format!("【前提: {}】", gate_brief(g)),
                None => String::new(),
            };
            s.push_str(&format!("- {label} (id: {id}): {basis}{req}\n"));
        }
    }
    // 使えるフラグの語彙 (spec 03 の拡張)。LLM が set_flag してよいフラグ = allowed − authored 専権
    // (トリガー効果/challenge 帰結が立てるフラグは見せない = 先取り set_flag の誘惑を作らない)。
    // 閉集合を見せることで幻フラグの発明 (却下ループの素) を断つ。表示名 (flag_titles)・
    // ヒント (flag_hints) があれば添える (flag_rules が早まりを守るのは従来どおり)。
    // 帳簿フラグ (hidden_flags) は語彙にも出さない (LLM に触らせない内部変数)。
    let usable: std::collections::BTreeSet<_> = scenario
        .usable_flags()
        .into_iter()
        .filter(|f| !scenario.hidden_flags.contains(f))
        .collect();
    if !usable.is_empty() {
        s.push_str(
            "\n## 状態フラグ (set_flag で立てられるのはこれだけ。条件が満たされた瞬間に立てる)\n",
        );
        for flag in &usable {
            let mut line = format!("- {flag}");
            if let Some(title) = scenario.flag_titles.get(flag).filter(|t| !t.trim().is_empty()) {
                line.push_str(&format!("（{}）", title.trim()));
            }
            if let Some(hint) = scenario.flag_hints.get(flag).filter(|h| !h.trim().is_empty()) {
                line.push_str(&format!(": {}", hint.trim()));
            }
            line.push('\n');
            s.push_str(&line);
        }
    }
    // 投票権の宣言 (spec 06 Phase D の surfacing)。GM が「いま誰が投票できるか」を知り、
    // 投票の局面で cast_vote を並べられるようにする (challenge の surfacing と同じ役割)。
    if !scenario.vote_rules.is_empty() {
        s.push_str("\n## 投票 (票は cast_vote op で入れる。開票は筋書きの出来事が行う)\n");
        for rule in &scenario.vote_rules {
            let who = match &rule.voter_attribute {
                None => "生存者なら誰でも投票できる".to_string(),
                Some(va) => format!("{}={} の者だけが投票できる", va.key, va.value),
            };
            s.push_str(&format!("- {} のとき: {who}\n", gate_brief(&rule.when)));
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
pub fn state_brief(state: &GameState, scenario: &Scenario) -> String {
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
    // 立っているフラグ。帳簿フラグ (hidden_flags) は出さず (hidden_stats と同じ扱い)、
    // 表示名 (flag_titles) があれば添える (id は ops 用にそのまま残す)。
    let flags: Vec<String> = state
        .flags
        .iter()
        .filter(|(k, v)| **v && !scenario.hidden_flags.contains(*k))
        .map(|(k, _)| match scenario.flag_titles.get(k).filter(|t| !t.trim().is_empty()) {
            Some(title) => format!("{k}（{}）", title.trim()),
            None => k.clone(),
        })
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
                // 内部用の帳簿 stat (hidden_stats) は提示しない (タイマー/カウンタの露出防止)。
                let kv = stats
                    .iter()
                    .filter(|(k, _)| !scenario.hidden_stats.contains(*k))
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{eid}: {kv}")
            })
            .filter(|line| !line.ends_with(": "))
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
    // secret 属性 (spec 06) は GM には全員分見せる (ゲームを回すのに必要) が、
    // 秘匿情報である注記〔秘匿〕を添える (演じ分け規律は GM_SYSTEM が刷り込む)。
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
                    .map(|(k, v)| {
                        if scenario.secret_attributes.contains(k) {
                            format!("{k}={v}〔秘匿〕")
                        } else {
                            format!("{k}={v}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{eid}: {kv}")
            })
            .collect::<Vec<_>>()
            .join(" / ")
    };
    // いまこの場に居る NPC (実効 presence = 場所ベース ± override)。GM が「誰が居るか」を
    // 知る唯一の経路 — 場所の説明文は静的 (退場後もキャラが書かれたまま) なのでこちらが真実。
    let present = scenario.present_at(state);
    let present = if present.is_empty() {
        "誰もいない (主人公のみ)".to_string()
    } else {
        present
            .iter()
            .map(|id| match scenario.characters.get(id).map(|c| c.name.trim()) {
                Some(name) if !name.is_empty() && name != id => format!("{name} ({id})"),
                _ => id.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    // いま条件が真になっている投票規則 (#37)。静的な規則 (scenario_brief) + 一般義務 (GM_SYSTEM)
    // だけでは絞られた局面 (夜の狩り) で票が出ないことが実測で再発したため、第三層として
    // 「いま誰が票を出せるか」を生存・属性で絞った名前列挙にし、現在形の義務として突きつける。
    let alive = |e: &str| {
        state.entities.get(e).and_then(|s| s.get("生存")).is_none_or(|v| *v == 1)
    };
    // NPC 分は「必ず並べよ」の義務、player 分は**代行禁止** (#39: 票はプレイヤーの選択であり、
    // 未指名なら narration で促す = 夜の襲撃先/占い先を「聞くターン」が成立する)。
    let mut open_votes: Vec<String> = Vec::new();
    for rule in &scenario.vote_rules {
        if !rule.when.eval(state) {
            continue;
        }
        let matches_rule = |id: &str| match &rule.voter_attribute {
            None => true,
            Some(va) => state.attribute_of(id, &va.key) == va.value,
        };
        let npcs: Vec<String> = scenario
            .characters
            .keys()
            .filter(|id| alive(id) && matches_rule(id))
            .map(|id| match scenario.characters.get(id).map(|c| c.name.trim()) {
                Some(name) if !name.is_empty() && name != id => format!("{name} ({id})"),
                _ => id.clone(),
            })
            .collect();
        let player_ok = alive(gm_core::PLAYER) && matches_rule(gm_core::PLAYER);
        if npcs.is_empty() && !player_ok {
            continue;
        }
        let who = match &rule.voter_attribute {
            Some(va) => format!("{}={} の生存者", va.key, va.value),
            None => "生存者なら誰でも".to_string(),
        };
        let mut note = format!("({who}) ");
        if !npcs.is_empty() {
            note.push_str(&format!(
                "{} — **この者たちの票をこのターンの ops に cast_vote で必ず並べよ** \
                 (各自の視点から独立に決める。票を出さなければこの局面では何も起きない)。",
                npcs.join(", ")
            ));
        }
        if player_ok {
            if state.votes.contains_key(gm_core::PLAYER) {
                note.push_str("プレイヤー (player) の票は受領済み。");
            } else {
                note.push_str(
                    "プレイヤー (player) にも投票権がある — **票を代行するな**。\
                     行動文で対象を指名していれば (襲う/占う/投票する等の言い回しを問わず) \
                     **その票を必ず voter=player の cast_vote として ops に含めよ — \
                     narration で襲撃や占いを描写するだけでは正本には何も起きていない**。\
                     まだ指名していなければ narration の結びでプレイヤーに対象の指名を促せ。",
                );
            }
        }
        open_votes.push(note);
    }
    let open_votes = if open_votes.is_empty() {
        String::new()
    } else {
        format!("\n- ⚠ **いま投票が開いている**。{}", open_votes.join(" ／ "))
    };
    format!(
        "# 現在の状態 (turn {})\n- 現在地: {}\n- この場にいる: {}\n- 所持品: {}\n- 立っている状態: {}\n- 能力値: {}\n- 使える能力: {}\n- 属性: {}{}",
        state.turn, state.location, present, inv, flags, entities, skills, attributes, open_votes,
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

/// 経緯ログ (chronicle) を「これまでの経緯」として注入する (空なら空文字)。
///
/// 過去ターンの 1 行要約列 = GM の**中期記憶**。state (正本) は事実の現在値しか持たず、
/// recent_narration は直前 1 ターンしか運ばないので、「数ターン前に何があったか」はこの経路で
/// しか GM に届かない (経過を忘れる GM の対策)。文字予算内で**新しい方を優先**し、溢れた
/// 古い方は省略した旨を明示する (無限に伸びて prompt を食い潰さない)。
pub fn history_note(history: &[crate::TurnLog]) -> String {
    if history.is_empty() {
        return String::new();
    }
    // 予算 (文字数)。新しい方から詰め、溢れた時点で打ち切る。
    const BUDGET: usize = 2400;
    let mut lines: Vec<String> = Vec::new();
    let mut used = 0usize;
    let mut dropped = false;
    for log in history.iter().rev() {
        let line = format!("- T{} プレイヤー「{}」→ {}\n", log.turn, log.player, log.summary);
        let cost = line.chars().count();
        if used + cost > BUDGET {
            dropped = true;
            break;
        }
        used += cost;
        lines.push(line);
    }
    lines.reverse();
    let mut s = String::from(
        "\n\n# これまでの経緯 (古い順・確定した記録)\n\
         過去のターンに実際に起きたことの要約です。これに矛盾する語りをせず、済んだ出来事を\
         初めてのように繰り返さないこと。\n",
    );
    if dropped {
        s.push_str("- (それ以前の経緯は省略)\n");
    }
    for line in &lines {
        s.push_str(line);
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
    // authored 結末ナレーション付きの判定は同ターンに語られ済み → LLM に再描写させない (二重語り回避)。
    // narration の無い判定 (素の Check / 結末文なし challenge) だけを LLM に還流して語らせる。
    let checks: Vec<&CheckOutcome> = checks.iter().filter(|c| c.narration.is_empty()).collect();
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
