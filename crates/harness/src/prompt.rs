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
(それは筋書きの出来事が起こす)。その場に居ない人物との会話・接触を narration に書くな。\
**主人公が移動しても、NPC は勝手についてこない** — 移動を語るとき、NPC が同行する素振り \
(一緒に歩き出す・後を追う・連れ立って向かう等) を書くな。同行は筋書きの出来事だけが起こし、\
移動先に誰が居るかは移動後の『この場にいる』一覧だけが決める。\n\
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
- **場所の移動は move op が受理された時にだけ起きる。** narration で移動を描いても現在地は \
1 ミリも変わらない — 現在の状態の「現在地」の行が**唯一の真実**であり、あなたの過去の語りと \
食い違うなら誤っているのは語りの方である (実際にはまだ移動していない)。過去に move が却下されて \
いても諦めるな — 却下理由に書かれた条件を満たせば move は通る。「いま移動できる」の行に \
挙がった行き先へは move が必ず受理される。移動できないなら、できない理由を物語として描け — \
**語りだけで移動した事にしないこと** (現在地と語りの乖離は最悪の矛盾である)。\n\
- 何も状態が変わらない描写だけのターンなら ops は空でよい。\n\
- **ops は書いた順に適用される。** 「拾ってから使う」「汲んでから飲む」のような段取りは、\
正しい順に並べれば 1 ターンに束ねてよい (先の op の結果を後の op が前提にできる。\
例: add_item 流木 → attempt_challenge シェルター作り)。既に持っている物への add_item は\
無害 (何も起きない) なので、所持が不確かでも拾ってから使ってよい。ただし\
**判定 (check / attempt_challenge) の結果に依存する手は束ねられない** — 出目は提出後に\
確定するので、結果を見てから次のターンで動け。\n\
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
**結果だけ** (翌朝の発見・残された痕跡) を描け。誰の仕業かは盤面が明かすまで伏せたままにする。\
さらに**〔秘匿:本人未知〕と注記された属性は当人すら知らない** — 当人にも明かすな (本人視点の \
自覚・独白・気配としても描くな)。その属性が引き起こす効果は、**原因を伏せて現象だけ**を描け \
(盤面のトリガーが明かすまで、当人は自分に何が起きているか知らないままである)。\n\
- **〔秘匿〕と注記されたフラグ・数値は、プレイヤーの画面に出ていない裏の状態である** \
(隠し進行・裏の好感度など)。あなたは追うために知っているが、その値・真偽を地の文で明示するな — \
**その状態が引き起こす現象だけ**を描き、原因 (フラグ/数値) は伏せよ。\n\
- **summary には、このターンの経緯 1 行 (誰が何をして何が起きたか、確定した事実だけ) を毎ターン \
書くこと。** これは以後のターンの『これまでの経緯』としてあなた自身に引き継がれる記憶になる — \
書かなければ経緯は忘れられる。物語的な修辞は不要、事実の記録に徹する \
(例: 「アリスに花を渡し、幼い頃の約束を打ち明けた」)。\n\
- **約束事 (facts)**: state に載らない小さな事実 (固有名・約束・プレイヤーの意図) を忘れそうなら \
facts に **60 字以内の 1 行**で追記せよ。毎ターン書くものではない。既にある約束事を繰り返すな \
(繰り返しは強化として扱われる)。**約束事に書いてあっても state に無い要素 (アイテム・能力・出来事) を \
付与・確定してはならない** — 約束事が効いてよいのは語りの一貫性 (呼称・約束・意図) だけ。\n\
narration と ops を必ず構造化出力として提出すること (ツール emit_delta、またはサーバ指示の JSON 形式)。";

/// 【開発者モード】シナリオ作者のテストプレイであることを LLM に伝え、`<meta: ...>` 形式の
/// メタ質問への応答規律を刷り込む先頭ブロック。`KATARIBE_DEV_MODE` が truthy な時だけ system
/// プロンプトの**先頭**に注入される (通常プレイには一切出ない)。
///
/// 狙い: プレイ中に「なぜ GM がそう振る舞ったか」を作者が直接問い、接地の破れ (居ないキャラの
/// 台詞・誤った却下解釈・幻フラグ等) をその場で診断する。正本 (engine) は不変 — これは prompt 層の
/// 可視化装置であり、メタ質問のターンは状態を変えない (ops 空) ことを LLM に要求する。
pub const DEV_META: &str = "\
【開発者モード / テストプレイ中】\n\
現在のプレイは、このシナリオを作っている開発者によるテストプレイです。\
プレイヤー（開発者）は通常の行動文に加えて、`<meta: ...>` という形式で\
**メタ質問**（物語の外側からの問い）を挟むことがあります。\
例: `<meta: なぜ今あかりが登場した？>` `<meta: この却下の理由は？>` `<meta: いま盤面で何が見えている？>`。\n\
- **`<meta: ...>` を含む入力を受けたら**、物語を進めず、GM としての判断根拠を開発者に率直に説明すること。\
なぜそう語ったか / 盤面（現在の状態・この場にいる一覧・使えるフラグ等）のどの情報に基づいたか / \
なぜ ops をそう組んだか / 直前の却下理由をどう解釈したか を、取り繕わずに答えよ。\
確信が持てない点は「確信が持てない」と正直に添えてよい。**間違えたと気づいたなら、なぜ間違えたかを説明せよ**。\n\
- **メタ質問のターンは状態を変えない。** 説明は narration に書き、**ops は必ず空**にすること \
(メタ質問はゲームを進行させない)。summary も空でよい。\n\
- `<meta: ...>` を含まない通常の行動文には、これまで通り GM として物語で応答すること \
(開発者モードでも物語の一貫性の規律は変わらない)。";

/// GM の system プロンプトを組む (GM_SYSTEM + scenario_brief、dev モードなら先頭に [`DEV_META`])。
///
/// `dev` は [`dev_mode_enabled`] が env から決める。純粋関数なので env に触れずテストできる。
pub fn gm_system_prompt(scenario: &Scenario, dev: bool) -> String {
    let base = format!("{}\n\n{}", GM_SYSTEM, scenario_brief(scenario));
    if dev {
        // 「あらかじめ最初に描いておく」— DEV_META を先頭に置く。dev/非 dev で安定プレフィックスが
        // 分かれるだけなので prompt caching は保たれる (セッション内で dev フラグは不変)。
        format!("{DEV_META}\n\n{base}")
    } else {
        base
    }
}

/// `KATARIBE_DEV_MODE` が truthy なら開発者モード。env 直読み (`LLM_DEBUG` と同流儀 —
/// app/CLI/run_turn の signature を変えずに効かせる)。未設定・空・偽値は false。
pub fn dev_mode_enabled() -> bool {
    std::env::var("KATARIBE_DEV_MODE").as_deref().map(is_truthy).unwrap_or(false)
}

/// env フラグの truthy 判定 (純粋)。`1` / `true` / `yes` / `on` (大小無視・前後空白無視) を真とする。
pub(crate) fn is_truthy(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

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
            // 判定主体が authored 固定なら誰の判定かを明示 (entity は engine が上書きするので
            // GM は指定不要 — 誤って player を指定しても正しい主体で振られる)。
            let basis = match (&c.entity, &c.stat) {
                (Some(e), Some(stat)) => format!("{e} の {stat} 判定 (主体は固定済み・entity 指定不要)"),
                (Some(e), None) => format!("{e} の運試し (能力に依らない)"),
                (None, Some(stat)) => format!("{stat} 判定"),
                (None, None) => "運 (能力に依らない)".to_string(),
            };
            // percentile challenge はロールアンダーである旨を明示 (spec 16 — 「低いほど良い」を
            // 加算式の癖で読み違えさせない)。
            let basis = match c.resolution {
                // percentile challenge はロールアンダーである旨を明示 (spec 16)。
                gm_core::Resolution::Percentile => {
                    format!("{basis} — d100 ロールアンダー (技能値以下で成功)")
                }
                // 確定行動 (spec 21): 振らずに必ず起きる。前提さえ満たせば選べる手だと伝える。
                gm_core::Resolution::None => "判定なし・確定 (前提を満たせば必ず成功する)".to_string(),
                gm_core::Resolution::Additive => basis,
            };
            // 前提条件 (requires) があれば明示 — 満たすまでこの挑戦は選べない。
            let req = match &c.requires {
                Some(g) => format!("【前提: {}】", gate_brief(g)),
                None => String::new(),
            };
            s.push_str(&format!("- {label} (id: {id}): {basis}{req}\n"));
        }
    }
    // 対決 (spec 18 Phase C)。attempt_contest で「開く」と、決着まで LLM を介さず
    // engine とプレイヤーが交互に振る (一括型 cadence)。GM の責務は開始の描写と、
    // 決着後に知らされる digest を踏まえた語りだけ。
    if !scenario.contests.is_empty() {
        s.push_str(
            "\n## 対決 (attempt_contest で id を選んで開く。開いた後の決着はエンジンとプレイヤーが直接つける)\n",
        );
        for (id, c) in &scenario.contests {
            let label = if c.description.trim().is_empty() { id.as_str() } else { c.description.trim() };
            let req = match &c.requires {
                Some(g) => format!("【前提: {}】", gate_brief(g)),
                None => String::new(),
            };
            s.push_str(&format!("- {label} (id: {id}): 相手 = {}{req}\n", c.opponent));
        }
        s.push_str(
            "対決を開いたターンは**始まりの描写まで** — 交換の経過や決着を先取りして語るな \
             (出目はまだ存在しない)。決着は次のターンに「対決の結果」として知らされる。\n",
        );
    }
    // 判定様式 (spec 16)。percentile 盤面では check_under の意味論を接地する — schema 入替
    // (見せない) と対の「読み方」の接地。GM_SYSTEM は盤面非依存の const を保つ (全盤面に
    // percentile 文言を撒かない)。scenario_brief はセッション内安定 = prompt caching も不変。
    if scenario.check_style == gm_core::CheckStyle::Percentile {
        s.push_str(
            "\n## 判定様式 (この盤面は d100 ロールアンダー)\n\
             技能判定は check_under op で行う (1d100 を振り、その技能の現在値**以下**なら成功 — \
             **出目は低いほど良い**)。成功度 (クリティカル/イクストリーム/ハード/成功/失敗/\
             ファンブル) はエンジンが決める。加算式の check は**この盤面では使うな**。\
             DC を自分で決めてはならない — 目標値は技能の現在値そのものである。\
             失敗した判定を、同じ状況・同じやり方のまま**振り直させてはならない** — \
             失敗は確定した事実であり、同じ試みの繰り返しには判定を出さず\
             「結果は変わらない」と語って接地せよ。再判定を振ってよいのは、\
             別の技能・別の手段・状況の変化 (新しい情報・道具・場所) がある時だけ。\n",
        );
    }
    // 使えるフラグの語彙 (spec 03 の拡張)。LLM が set_flag してよいフラグ = allowed − authored 専権
    // (トリガー効果/challenge 帰結が立てるフラグは見せない = 先取り set_flag の誘惑を作らない)。
    // 閉集合を見せることで幻フラグの発明 (却下ループの素) を断つ。表示名 (flag_titles)・
    // ヒント (flag_hints) があれば添える (flag_rules が早まりを守るのは従来どおり)。
    // 帳簿フラグ (hidden_flags) は語彙にも出さない (LLM に触らせない内部変数)。
    let usable: std::collections::BTreeSet<_> = scenario
        .usable_flags()
        .into_iter()
        // 帳簿 (internal_flags) も秘匿 (hidden_flags) も set_flag 語彙には出さない
        // (前者は触らせない内部変数、後者は GM に casually 立てさせない秘密)。
        .filter(|f| !scenario.hidden_flags.contains(f) && !scenario.internal_flags.contains(f))
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
    // 立っているフラグ。internal_flags (engine 帳簿) は GM からも隠す。hidden_flags
    // (プレイヤー非表示の秘密) は 〔秘匿〕注記付きで GM に見せる (明かすな規律は GM_SYSTEM)。
    // 表示名 (flag_titles) があれば添える (id は ops 用にそのまま残す)。
    let flags: Vec<String> = state
        .flags
        .iter()
        .filter(|(k, v)| **v && !scenario.internal_flags.contains(*k))
        .map(|(k, _)| {
            let base = match scenario.flag_titles.get(k).filter(|t| !t.trim().is_empty()) {
                Some(title) => format!("{k}（{}）", title.trim()),
                None => k.clone(),
            };
            if scenario.hidden_flags.contains(k) {
                format!("{base}〔秘匿〕")
            } else {
                base
            }
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
                // internal_stats (engine 帳簿) は GM からも隠す。hidden_stats (プレイヤー非表示の
                // 秘密) は 〔秘匿〕注記付きで GM に見せる (明かすな規律は GM_SYSTEM)。
                let kv = stats
                    .iter()
                    .filter(|(k, _)| !scenario.internal_stats.contains(*k))
                    .map(|(k, v)| {
                        if scenario.hidden_stats.contains(k) {
                            format!("{k}={v}〔秘匿〕")
                        } else {
                            format!("{k}={v}")
                        }
                    })
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
                        if scenario.hidden_attributes.contains(k) {
                            // 本人未知 (当人にも見えない・当人にすら明かさない) — secret より強い。
                            format!("{k}={v}〔秘匿:本人未知〕")
                        } else if scenario.secret_attributes.contains(k) {
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
        // MSRV 1.80 のため is_none_or (1.82〜) は使わない。
        state.entities.get(e).and_then(|s| s.get("生存")).map_or(true, |v| *v == 1)
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
    // いま通れる出口 (#42、#37 の移動版)。gate はエンジンが毎ターン評価し、通れる先だけを
    // 固有名で突きつける — 一度却下された LLM の「move は失敗する」という回避学習を
    // 現在形の事実で上書きする。出口の無い場所では行ごと出さない。
    let exits_now = match scenario.location(&state.location) {
        Some(loc) if !loc.exits.is_empty() => {
            let open: Vec<&str> = loc
                .exits
                .iter()
                .filter(|e| e.gate.eval(state))
                .map(|e| e.to.as_str())
                .collect();
            if open.is_empty() {
                "\n- いま移動できる出口: なし (条件未達。満たせば move が通る)".to_string()
            } else {
                format!(
                    "\n- いま移動できる: {} (move op を出せば必ず受理される)",
                    open.join(", ")
                )
            }
        }
        _ => String::new(),
    };
    // この場でいま拾えるアイテム (spec 09-C、#37 投票/#42 出口に続く現在形接地の第三例)。
    // 取得不能 (備え付け/持ち去り済み/gate 未達/既所持) は列挙しない — 「拾う」が narration
    // だけで済まされる穴 (#23 型) を、拾える物の固有名列挙で塞ぐ。
    let takeable_now = match scenario.location(&state.location) {
        Some(loc) if !loc.items.is_empty() => {
            let now: Vec<&str> = loc
                .items
                .iter()
                .filter(|(id, li)| {
                    li.take() != gm_core::TakeMode::Fixed
                        && !(li.take() == gm_core::TakeMode::Once
                            && state.already_taken(&state.location, id))
                        && !state.has_item(gm_core::PLAYER, id)
                        && li.when().eval(state)
                })
                .map(|(id, _)| id.as_str())
                .collect();
            if now.is_empty() {
                String::new()
            } else {
                format!(
                    "\n- この場でいま拾える: {} (add_item op で入手できる。語りで拾っても手には入らない)",
                    now.join(", ")
                )
            }
        }
        _ => String::new(),
    };
    format!(
        "# 現在の状態 (turn {})\n- 現在地: {}{}{}\n- この場にいる: {}\n- 所持品: {}\n- 立っている状態: {}\n- 能力値: {}\n- 使える能力: {}\n- 属性: {}{}",
        state.turn, state.location, exits_now, takeable_now, present, inv, flags, entities, skills, attributes, open_votes,
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

/// [`history_note`] の retrieval クエリ (spec 08-A) — 「いまのターンの文脈」。
/// `run_turn` がプレイヤーの行動文・現在地・実効 presence から組む。
pub struct HistoryQuery<'a> {
    /// プレイヤーの行動文 (このターンの入力)。
    pub action: &'a str,
    /// 現在地 LocationId。
    pub location: &'a str,
    /// いまこの場に居る NPC (実効 presence)。
    pub present: Vec<String>,
}

/// あらすじ (spec 10) を「これまでのあらすじ」として注入する (段落 0 件なら空文字)。
///
/// 圧縮済みの章 segment 列 = GM の**長期の物語記憶**。chronicle の注入予算からあふれた
/// 古い経緯の「物語の連続した流れ」はこの経路でしか GM に届かない (retrieval は個別事実の
/// ピンポイント想起 — 役割分担であって置き換えではない)。[`history_note`] の**前**に置く
/// (古い記憶 → 新しい記憶の時系列順)。
///
/// 予算 2000 字・新しい章優先。あふれたら最古の章から「(それ以前の章は省略)」に潰す —
/// 省略章の個別事実は retrieval が全量 chronicle から拾えるため「忘れない」は破れない。
pub fn synopsis_note(synopsis: &[crate::SynopsisEntry]) -> String {
    if synopsis.is_empty() {
        return String::new();
    }
    const BUDGET: usize = 2000;
    let block_of = |e: &crate::SynopsisEntry| format!("## {} (〜T{})\n{}\n", e.title, e.upto_turn, e.text);

    // 新しい章から予算まで拾い、提示は古い順に戻す。
    let mut kept: Vec<String> = Vec::new();
    let mut used = 0usize;
    for e in synopsis.iter().rev() {
        let block = block_of(e);
        let cost = block.chars().count();
        if used + cost > BUDGET {
            break;
        }
        used += cost;
        kept.push(block);
    }
    // 予算が 1 章も入らない縮退でも、最新章だけは必ず出す (注入ゼロは本末転倒)。
    if kept.is_empty() {
        kept.push(block_of(synopsis.last().expect("non-empty")));
    }
    kept.reverse();

    let mut s = String::from(
        "\n\n# これまでのあらすじ (確定した過去の物語)\n\
         圧縮済みの章の要約です。これに矛盾する語りをせず、済んだ出来事を初めてのように\
         繰り返さないこと。あらすじは背景であって主題ではない — 語りの主は現在のターンの\
         行動への応答であり、求められない限り過去の章を自発的に回想・引用しないこと。\
         プレイヤーが過去を尋ねたときは、あらすじを正確に参照して答えること。\n",
    );
    if kept.len() < synopsis.len() {
        s.push_str("(それ以前の章は省略)\n");
    }
    for block in kept {
        s.push_str(&block);
    }
    s
}

/// 経緯ログ (chronicle) を「これまでの経緯」として注入する (空なら空文字)。
///
/// 過去ターンの 1 行要約列 = GM の**中期記憶**。state (正本) は事実の現在値しか持たず、
/// recent_narration は直前 1 ターンしか運ばないので、「数ターン前に何があったか」はこの経路で
/// しか GM に届かない (経過を忘れる GM の対策)。
///
/// **二層注入 (spec 08-A)**: 全量が総予算に収まるなら全文 (retrieval 不要)。溢れる長編では
/// **直近は無条件** (連続性の保証、予算 60%) + **それより古い経緯は今の文脈に関連するものだけ
/// 想起** (予算 40%・上限 10 件)。関連度は memoria と同じ文字 bigram TF-IDF cosine
/// (決定論・追加 API 無し) に、engine 機械タグの一致 (location ×2.0 / present ×1.5) を掛ける。
/// 関連 0 件なら直近が全予算へ拡張 (= 旧挙動)。従来「(それ以前の経緯は省略)」で完全に捨てて
/// いた序盤の経緯が、関連する時だけ蘇る — 長編でも「忘れない GM」を保つ。
pub fn history_note(history: &[crate::TurnLog], query: &HistoryQuery) -> String {
    if history.is_empty() {
        return String::new();
    }
    // 予算 (文字数)。直近層 60% / 関連層 40% (spec 08 rev1)。
    const BUDGET: usize = 2400;
    const RECENT_BUDGET: usize = BUDGET * 60 / 100;
    const RELEVANT_BUDGET: usize = BUDGET - RECENT_BUDGET;
    const RELEVANT_MAX: usize = 10;
    // タグ増幅後スコアの足切り (ノイズ遮断)。
    const MIN_SCORE: f64 = 0.05;

    let line_of = |log: &crate::TurnLog| {
        format!("- T{} プレイヤー「{}」→ {}\n", log.turn, log.player, log.summary)
    };

    let header = "\n\n# これまでの経緯 (古い順・確定した記録)\n\
         過去のターンに実際に起きたことの要約です。これに矛盾する語りをせず、済んだ出来事を\
         初めてのように繰り返さないこと。\n";

    // 全量が総予算に収まるなら全文 (retrieval 不要 = 従来と同じ出力)。
    let total: usize = history.iter().map(|l| line_of(l).chars().count()).sum();
    if total <= BUDGET {
        let mut s = String::from(header);
        for log in history {
            s.push_str(&line_of(log));
        }
        return s;
    }

    // --- 直近層: 新しい方から予算まで無条件 (連続性の保証) ---
    let mut recent_count = 0usize;
    let mut used = 0usize;
    for log in history.iter().rev() {
        let cost = line_of(log).chars().count();
        if used + cost > RECENT_BUDGET {
            break;
        }
        used += cost;
        recent_count += 1;
    }
    let cut = history.len() - recent_count;
    let older = &history[..cut];

    // --- 関連層: 古い経緯を「いまの文脈」でスコアし、関連するものだけ想起 ---
    // 文書にはタグ (location/present/flags/items = engine 事実) も含める — summary の語彙が
    // 貧弱 (弱モデル fallback) でも id の bigram 重なりで検索が接地する (spec 08-B の狙い)。
    let fragments: Vec<crate::memoria::MemoryFragment> = older
        .iter()
        .map(|log| crate::memoria::MemoryFragment {
            id: format!("T{}", log.turn),
            tags: Vec::new(),
            text: format!(
                "{} {} {} {} {} {}",
                log.player,
                log.summary,
                log.location,
                log.present.join(" "),
                log.flags_set.join(" "),
                log.items.join(" ")
            ),
        })
        .collect();
    let store = crate::memoria::LoreStore::new(fragments);
    let qtext = format!("{} {} {}", query.action, query.location, query.present.join(" "));
    let base = store.scores(&qtext);
    let mut scored: Vec<(usize, f64)> = older
        .iter()
        .enumerate()
        .map(|(i, log)| {
            let mut s = base[i];
            // engine 機械タグの一致で増幅 (spec 08 rev1: location ×2.0 / present ×1.5)。
            if !query.location.is_empty() && log.location == query.location {
                s *= 2.0;
            }
            if log.present.iter().any(|p| query.present.contains(p)) {
                s *= 1.5;
            }
            (i, s)
        })
        .filter(|(_, s)| *s >= MIN_SCORE)
        .collect();
    // スコア降順、同点は新しい方を優先 (決定論)。
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then_with(|| b.0.cmp(&a.0))
    });
    let mut selected: Vec<usize> = Vec::new();
    let mut rel_used = 0usize;
    for (i, _) in scored {
        if selected.len() >= RELEVANT_MAX {
            break;
        }
        let cost = line_of(&older[i]).chars().count();
        if rel_used + cost > RELEVANT_BUDGET {
            continue; // 長すぎる 1 件は飛ばし、残予算に収まる次点を拾う
        }
        rel_used += cost;
        selected.push(i);
    }
    selected.sort_unstable(); // 提示は時系列 (古い順)

    // 関連 0 件なら直近層を全予算へ拡張 (= 旧挙動と同じ「新しい方優先」)。
    if selected.is_empty() {
        let mut lines: Vec<String> = Vec::new();
        let mut used = 0usize;
        for log in history.iter().rev() {
            let line = line_of(log);
            let cost = line.chars().count();
            if used + cost > BUDGET {
                break;
            }
            used += cost;
            lines.push(line);
        }
        lines.reverse();
        let mut s = String::from(header);
        s.push_str("- (それ以前の経緯は省略)\n");
        for line in &lines {
            s.push_str(line);
        }
        return s;
    }

    let mut s = String::from(header);
    s.push_str(
        "「(関連)」の行は、古い経緯のうち今の場面に関わるものだけを再掲した確定記録である。\n",
    );
    for &i in &selected {
        s.push_str(&format!(
            "- (関連) T{} プレイヤー「{}」→ {}\n",
            older[i].turn, older[i].player, older[i].summary
        ));
    }
    s.push_str("- (それ以前の経緯は省略)\n");
    for log in &history[cut..] {
        s.push_str(&line_of(log));
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

/// 移動直後の否定接地: 直前ターンで場所が変わったとき、前の場所に居て今ここに居ない NPC を
/// **固有名で「ついてきていない」**と告げる (該当なしなら空文字)。
///
/// GM 自身の移動ターンの語り (「一緒に歩き出す」等の同行の素振り = 非検証 narration) が
/// recent_narration/chronicle 経由で presence を汚染し、次の場所で居ないキャラが居ることに
/// なる (failures #49、#47 の自己汚染版)。一覧 (一般規律) は具体的な語りに負けるので、
/// **否定の事実 + 固有名** (#37 の接地強度) で移動直後の 1 ターンだけ上書きする。
///
/// 移動の検知は history 末尾 2 件の location 差 — `TurnLog.location` は**適用後**の現在地
/// なので、移動ターン自身のログは既に新しい場所を指す (state と比べても差は出ない)。
/// 旧セーブ (location タグ無し="") や履歴 1 件以下では黙って空 (誤発火しない)。
pub fn moved_note(scenario: &Scenario, state: &GameState, history: &[crate::TurnLog]) -> String {
    let n = history.len();
    if n < 2 {
        return String::new();
    }
    let (before, after) = (&history[n - 2], &history[n - 1]);
    if before.location.is_empty() || after.location.is_empty() || before.location == after.location
    {
        return String::new();
    }
    // 実効 presence (現在の真実) に居ない、移動前の場所の NPC = 置いていかれた側。
    let now = scenario.present_at(state);
    let left: Vec<String> = before
        .present
        .iter()
        .filter(|id| !now.contains(*id))
        .map(|id| match scenario.characters.get(id).map(|c| c.name.trim()) {
            Some(name) if !name.is_empty() => format!("{name} ({id})"),
            _ => id.clone(),
        })
        .collect();
    if left.is_empty() {
        return String::new();
    }
    format!(
        "\n\n# 直前の移動\n直前のターンで {} から {} へ移動した。**{} はついてきていない** — \
        いまこの場に居るのは『この場にいる』一覧の通りで、それだけが真実。直前の語りや経緯に\
        同行・見送りの素振りが書かれていても、その人物はこの場に居ない。\
        居ない人物の台詞・行動・気配を書くな。",
        before.location,
        after.location,
        left.join("、")
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
    // 凍結中 (pending=決断待ち) は最終結果ではないので還流しない — 決断確定後の final check を
    // 呼び出し側 (app) が差し替えて還流する (spec 18 Phase B)。
    let checks: Vec<&CheckOutcome> =
        checks.iter().filter(|c| c.narration.is_empty() && !c.pending).collect();
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
        // spec 18 Phase B: プッシュ/差分買いを経た判定は、その決断ごと語りに織り込ませる
        // (押した無茶・支払った代償は物語の素材 — 黙って結果だけ語ると決断が消える)。
        let decision = if c.pushed {
            "／この判定は**押して振り直された** — 一度失敗し、代償を覚悟で無理を通した経緯を語りに含めよ"
        } else if c.spent > 0 {
            "／この成功は**代償を支払って買い取られた** — 何かをすり減らして成功に変えた手応えを語りに含めよ"
        } else {
            ""
        };
        // percentile (spec 16): degree が「どのくらい良かったか」を担う (margin の代替)。
        // 表示は d100=出目 ≤/> 目標値 → 成功度 (ロールアンダー = 低いほど良い)。
        if let Some(degree) = &c.degree {
            let label = degree_label_ja(degree);
            let rel = if c.success { "≤" } else { ">" };
            s.push_str(&format!(
                "- {} の「{}」判定: d100={} {rel} 目標値{} → {label}（成功度に応じた強さで因果を語れ。\
                 クリティカル/ファンブルは劇的に、イクストリーム/ハードは鮮やかに、僅差は紙一重に）{decision}\n",
                c.entity, c.stat, c.roll, c.dc
            ));
            continue;
        }
        let mark = if c.success { "成功" } else { "失敗" };
        // 複数ダイス/乗数 (3D6×5 系) は素の合計と乗数を明示する (既定 1d/×1 は従来書式)。
        let dice = if c.count > 1 || c.times > 1 {
            let mult = if c.times > 1 { format!("×{}", c.times) } else { String::new() };
            format!("{}d{}(合計{}){}", c.count, c.sides, c.roll, mult)
        } else {
            format!("1d{}({})", c.sides, c.roll)
        };
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
            "- {} の「{}」判定: {dice} + 修正{:+} = {} vs DC {} → {mark}（{gap}{tier}）{decision}\n",
            c.entity, c.stat, c.modifier, c.total, c.dc
        ));
    }
    s
}

/// 成功度 (degree) の日本語表示 (spec 16 決定 1: 内部 id は英語・表示は差し替え可能な
/// 言語表。初期値は公式日本語版に馴染むカタカナ)。prompt/CLI が共用する。
pub fn degree_label_ja(degree: &str) -> &'static str {
    match degree {
        "critical" => "クリティカル",
        "extreme" => "イクストリーム成功",
        "hard" => "ハード成功",
        "regular" => "成功",
        "fumble" => "ファンブル",
        _ => "失敗",
    }
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
