# failures.md — Kataribe 罠台帳

> 実装中に踏んだ/予見した罠を 1 件 1 entry で残す。未来の自分への接地。
> 教訓 → 契約 → 実装の順序は silent degradation の温床なので、実装観察を一次資料にする。

## crates/llm_client (2026-06-23 移植時)

### 1. tool-use の `arguments` は「JSON オブジェクト」ではなく「JSON 文字列」
OpenAI 互換の tool_call は `function.arguments` を **文字列**で返す (`"{\"narration\":...}"`)。
ネストした object ではない。`ResponseMessage` をそのまま StateDelta に deserialize できると
誤設計すると壊れる。`arguments: String` で受けて **二段階パース** (`serde_json::from_str`) する。
→ wire.rs `FunctionCallResponse.arguments: String` / parse.rs `extract`。

### 2. `tool_choice` 強制を尊重しないサーバ/モデルがある
互換サーバ・一部モデルは `tool_choice: {function}` を無視して content に直接 JSON を吐く。
tool_calls だけを前提にすると NoStructuredOutput で死ぬ。
→ フォールバック: tool_calls 不在なら content をフェンス除去して再パース (Python generate_json 同型)。
parse.rs `extract` の二経路。

### 3. provider ごとに JSON Schema のサブセットが違う (未検証・watch)
schemars が生成する schema は `$defs` / `$ref` / `title` / `$schema` を含む。
OpenAI は概ね受けるが、厳格な structured-output モード (`strict: true`) や一部 provider は
`$ref` や `additionalProperties` の扱いで弾く可能性がある。**実クラウド通しプレイで要検証**。
弾かれたら: (a) schema を inline 展開する、(b) strict を外す、(c) tool description で補強。
現状は素の schemars 出力をそのまま渡している (PoC)。
✅ **解決 (2026-06-23)**: claude-opus-4-8 @ Anthropic OpenAI 互換層は schemars 出力
($defs/$ref/title 含む) を**そのまま受理**。密室脱出 通しプレイ成功 (4/4 一発合格・goal 到達)。
他 provider (OpenAI strict モード等) は未検証だが、少なくとも Anthropic 互換では inline 展開不要。

### 4. パース失敗時に raw を捨てると再生成できない
`adjudicate` の Reject も JSON パース失敗も、LLM に**戻して直させる**のが self_repair の核。
raw を握りつぶすと「何を直せばいいか」を LLM に伝えられない。
→ `LlmError::Parse { source, raw }` で raw を保持。これが GM ターンループ(次フェーズ)の燃料。

### 5. ネットワーク経路はテストで固められない (PoC スコープの線引き)
実 API は鍵必須 + 非決定的。`chat_once`/`chat_with_retry` は単体テスト対象外にした。
代わりに **壊れる ser/de 境界** (wire 整形 / parse 二経路 / schema 生成 / config / 一過性判定) を
決定論テストで固めた (9 件)。実 API 通しは「実クラウド通しプレイ」フェーズに分離。
教訓: 非決定的な外部 I/O と決定論的な変換ロジックを**型で分離**しておくと、PoC で何を
証明できて何を後回しにするかの線が引ける。

### 6. reqwest は rustls-tls を明示する (決定論ツールチェーン)
既定 features の native-tls は系 OpenSSL に依存しうる。
`default-features = false, features = ["json", "rustls-tls"]` で系非依存に倒した。
compiler_version_policy (再現可能なツールチェーン) と同精神。

## crates/harness (2026-06-23 GM ターンループ)

### 7. 実 LLM 直結だと「却下→再生成」ロジックをテストできない
ループが `LlmClient::generate_delta` を直接呼ぶ設計にすると、self_repair の核心
(嘘を却下し理由を戻して直させる) が実 API + 非決定的応答なしには検証できなくなる。
→ `DeltaProposer` trait で依存性逆転。本番=LlmClient, テスト=ScriptedProposer(台本付き fake)。
ループは trait に対して書き、却下→再生成を**実 API なしで決定論 Green** にできた (6 件)。
llm_client #5 と同じ「非決定的 I/O と決定論ロジックを型で分離」の再適用。

### 8. messages はターンごとに state から再構築する (履歴に古い状態を溜めない)
会話履歴を延々と積み増すと、過去ターンの古い state 記述が文脈に残り「忘れない GM」の逆になる。
→ run_turn は毎ターン `scenario_brief + state_brief(現在の正本)` を新規に組む。
却下→再生成の within-turn だけ assistant/user を積む (その範囲は同一 state なので一貫)。
state が唯一の真実、文脈はそのスナップショット、という北極星の prompt 層での具体化。

### 9. `apply().expect()` は adjudicate との結線前提 — 乖離したら panic
run_turn は `adjudicate == Accept` を確認してから `apply(...).expect("adjudicate 済みなら成功")`。
これは「apply は adjudicate を内部で再実行し、同じ判定を返す」という gm_core の不変条件に依存する。
将来 adjudicate と apply の検証ロジックが乖離すると expect が panic する = 早期に気付ける設計上の
アラーム (silent な不整合より good)。ただし両者の検証を**二重管理しない**規律が前提 (gm_core 側の掟)。

### 10. async fn in trait の警告は in-crate 限定で allow
`DeltaProposer::propose` を native async fn in trait にすると `async_fn_in_trait` 警告
(auto-trait 漏れ / dyn 化困難の注意)。本 trait は harness 内でしか実装/消費せず generic で受ける
ので dyn 不要 → `#[allow(async_fn_in_trait)]` で抑制。外部公開 API になるなら要再検討。

### 11. `Box<dyn Error>` 返り値は最初の具体 Box 構築に推論固定される
bin `play` の `main() -> Result<(), Box<dyn Error>>` で、`return Err(Box::new(io_err))` と
書いたら戻り値エラー型が `Box<io::Error>` に**推論固定**され、他の `?` (String / serde_yaml::Error
→ Box<dyn Error>) の From 変換が全滅 (E0277 連鎖 5 件)。
→ 具体エラーは `Box::new(e)` でなく `e.into()` で返す。`From<E> for Box<dyn Error>` が効いて
dyn に widening される。戻り値が dyn なら**全ての error 構築を .into() に統一**するのが安全。
症状(5件のFrom未実装)は派手だが根は1行。mandate_logical_friction_processing の実例。

## crates/llm_client (2026-06-23 実 API 初投入で判明)

### 12. 新しめのモデルは `temperature` を非対応にしており送ると 400
claude-opus-4-8 @ Anthropic 互換層は `temperature` パラメータを deprecated 扱いで拒否
(`400 "temperature is deprecated for this model"`)。LocalAI は常に temperature を送っていたが、
クラウドの新モデルでは弾かれる。
→ `ChatRequest.temperature: Option<f32>` + `skip_serializing_if`。`LlmConfig.temperature` も
Option にし、**`LLM_TEMPERATURE` 明示時のみ送る** (既定は省略 = provider 既定に委ねる)。
.env.example も既定でコメントアウト。tool_choice 強制が構造を保証するので温度固定は不要だった。
教訓: 互換 API でも「全 provider 共通の必須パラメータ」は思ったより少ない。未指定で provider
既定に委ねるのが最も壊れにくい。送る前提でなく省略を既定にする。

### 13. `from_env()` が `dotenvy::dotenv()` を呼ぶとテスト不能になる
「api_key 欠落で Config エラー」を検証するテストが、実 .env 存在時に失敗。理由: from_env 内の
dotenv が .env のキーをプロセス env に再注入し、`remove_var` を打ち消す。
→ **.env 読み込みはアプリ入口 (bin main) の責務に移す**。from_env は env を読むだけ (副作用なし)。
慣習的にも正しい分離 (lib は環境を勝手に書き換えない)。dotenvy 依存も llm_client → harness へ移動。
教訓: テスト不能は設計の臭い。グローバル副作用 (env 書き換え) を純粋な読み取りから分離する。

## context7 接地で判明 (2026-06-23, 公式 platform.claude.com docs)

### 14. parallel tool use は既定 ON — first tool_call だけ読むと残りを黙殺
native Messages API の tool_choice は `disable_parallel_tool_use` を持ち、**既定では複数の
tool_use ブロックを返しうる**。OpenAI 互換層でも response.tool_calls が複数になる可能性がある。
parse::extract は `tool_calls.first()` だけ採用するので、モデルが emit_delta を複数返すと
残りを**黙って捨てる** = 北極星「矛盾しない」に反する潜在バグ。
現状: 単一ツールを tool_choice 強制しており通しプレイでは 1 件のみ返った (未発火)。
将来対策案: (a) 複数 tool_call 検出時は明示エラー or 先頭採用をログ化、(b) OpenAI 互換層で
parallel 抑制を渡せるか確認 (native は disable_parallel_tool_use、互換層は extra_body 経由か要調査)。

### 15. 再生成は tool_use→tool_result プロトコルを意図的に回避している
公式: forced tool の assistant 応答の後、native では tool_use ブロックに対応する tool_result を
返すのが正規 (tool_use_id で対応、tool_result の後にテキストを置くと invalid)。
我々の push_rejection は **応答の tool_calls を保持せず**、提案を**プレーン assistant テキストで
echo** + 却下理由を user テキストで積む → 履歴に dangling tool_call が無いので tool_result 要求を
回避できる。これは設計判断として正しい (我々は「ツールの出力に反応させる」のでなく「再提案させる」
ため)。ただし**却下→再生成の実 API 挙動は未検証** (happy path で発火せず)。敵対プレイで初検証する。
注意: 将来 maintainer が「正規の tool_use+tool_result に直す」と、forced tool 後に tool_result が
必須になり、かえって複雑化する。現設計 (プレーン echo) は意図的選択であることを明記。
✅ **検証 (2026-06-23, 敵対プレイ)**: 複数ステップを束ねた行動で LLM が原子性違反デルタを提案
→ エンジン却下 → プレーン echo + 却下理由を user テキストで還流 → LLM が合法な部分手に修正
(attempts=2, 2 ターンで再現)。**再生成のメッセージ形は実 Anthropic API で通る**ことを実証。
副産物: LLM は scenario_brief の gate を読み、不可能な単独行動 (解錠前 move・幻 master_key) は
そもそも提案せず narration で拒否した (prompt 層接地が有効)。却下が発火したのは「欲張って束ねた」
時のみ = 正本の原子性が「一手ずつの正しい前進」を強制する設計が実 LLM で機能。

## crates/harness (2026-06-23 数値ステータス 実 LLM 検証)

### 18. 数値を「LLM に見せる」「人間が観る」経路を忘れない
stat を GameState に足しただけでは不十分。(a) `prompt.rs::state_brief` に stats を含めないと
**LLM が現在値を読めず数値推論できない**（str=12 を知らずに「足りるか」を判断できない）。
(b) bin `play` の Accepted 出力に stats を含めないと**人間が変化を追えない**（最初の試走で
str 推移が見えず、goal 到達から逆算するしかなかった）。データを足したら「提示経路」と
「観測経路」を同時に通すのが鉄則。実 LLM 検証で両方を後追い修正した。
副次観察: LLM は鍛錬を request_roll(演出ダイス) + adjust_stat で表現し、stat 変化を伴わない
narration だけのターンもあった（narration は非検証なので許容、ただし「描写したら op を出せ」と
プロンプトで締めると一貫性が上がる。将来チューニング候補）。

## crates/gm_core (2026-06-23 Phase B 禁忌の二層防御)

### 19. 硬い禁忌も二層防御 — 強いモデルでは engine 強制が live で発火しない
邂逅で alice に豚肉を強要 (実 LLM, claude-opus-4-8) → LLM は profile (豚肉を断つ誓い) を読み、
違反 op (set_flag alice_ate_pork) を**提案すらせず** narration で拒否 + 好感度を自ら下げた
(attempts=1)。つまり Phase B の engine taboo 強制は **live では発火しなかった** (手前の prompt 層で
防がれた)。これは失敗でなく二層防御の確認: (1)profile=第一防衛線 (2)taboos=保証/backstop。
**含意**: 強い Opus では profile だけで足りるが、**同人配布で狙う弱いローカル LLM は profile を
無視して違反を提案しうる** → その時に engine 強制が一貫性を救う。Phase B は弱モデルほど効く。
engine 強制の正しさは決定論テスト (taboo_blocks_violating_delta) が証明済 (live 非発火≠未検証)。
教訓: 「正本>文章力」の各層 (世界状態/キャラ禁忌) は prompt+engine の二層。強モデルだと engine 層が
live で見えにくいが、それは prompt 層が効いている証拠であり、engine 層の価値 (弱モデル/保証) は別。

## crates/gm_core (2026-06-23 数値ステータス PoC)

### 17. Gate/StateOp に variant を足すと全 match 箇所がコンパイルエラー (= 機能、罠でない)
`Gate::StatAtLeast` 追加で `harness/prompt.rs::gate_brief` が non-exhaustive エラー (E0004)。
これは**バグでなく設計の利点** ── 網羅 match が「新条件を扱い忘れる」のを構造的に防ぐ
(北極星「矛盾しない」のコンパイラ強制版)。variant 追加時の更新箇所チェックリスト:
(a) gm_core engine.rs `adjudicate` (検証) + `apply` (適用), (b) spine.rs `Gate::eval`,
(c) harness prompt.rs `gate_brief` (LLM への日本語化), (d) llm_client schema テスト
(StateOp 追加時、schemars が自動で schema に載せるので**プロンプト変更は不要** = 機械生成の利点)。

### 16. (軽微) narration に二重エスケープ \n が混じることがある
敵対プレイ turn4 で narration に literal `\n\n` が出た。モデルが tool 引数 JSON に `\\n\\n`
(二重エスケープ) を書いたため、serde で 1 段戻しても `\n` が残った。我々のバグではなくモデル出力の癖。
UI 層を繋ぐ時は narration を表示前に正規化する (literal `\n` → 改行 or 除去) と良い。低優先。

## crates/harness (2026-06-24 memoria_bridge 実 LLM 実測)

### 20. エンジンの閾値は LLM の自然な増分に較正せよ — テスト値は実機と乖離する
trigger_recall (閾値 好感度>=30) を実 LLM で通しプレイ (claude-opus-4-8) → LLM は好感度を
**+1〜3/ターン**で realistic に刻み、4 ターンで 8 までしか届かず**トリガー未発火**。決定論テストは
`raise_affection(30)` を 1 デルタで直接入れていた (test-authored) ので、この乖離が見えなかった。
demo 盤面で閾値を 5 に下げたら 3 ターンで発火・cascade・goal 到達を観察。**教訓**: 数値 gate/
トリガー閾値は「テストで何を入れるか」でなく「実機 LLM が 1 ターンにどれだけ動かすか」で較正する。
シナリオ作者向けに「好感度は 1 イベント +1〜3 が相場」という increment 規約を残すと盤面設計が楽。

### 21. memoria_bridge 実機成功 + TF-IDF recall の十分性の境界
demo (閾値 5, cue=曖昧文字列「丘の樫の木で交わした幼い約束」) で端から端まで成功:
発火 → cascade (recall_promise→renew_vow) → goal、かつ cue が id/tag と **exact 不一致**なので
**TF-IDF cosine 経路**が発火し childhood_promise を正しく recall (┊ で伏線本文が surface)。
**ただし TF-IDF が効いたのは cue が伏線本文と語彙 (丘/樫の木/約束/幼) を共有していたから**。
含意: authored cue 設計では (a) exact id/tag = score 1.0 保証、(b) 語彙が重なる曖昧 cue = TF-IDF
で届く、の二段で**実用上十分**。神経 embedding が要るのは「語彙が完全に乖離した paraphrase を
跨ぐ」場合だけで、cue を作者が書く現設計では発生しない → **usage-over-extension 確定: 神経
embedding は不要**。`failures` でなく接地: 推測でなく実測で (a) の要否を判断できた。

### 22. 強モデルは profile だけで伏線を自発的に語る — bridge の固有価値の再定義
発火前の turn でも LLM は alice.profile (約束を交わした) を読み「昔ね、誰かと約束をした気がする」と
**トリガー無しで**伏線を先取り narration した。つまり「LLM に思い出させる」こと自体は profile で足りる
(強モデルでは)。memoria_bridge の固有価値は **(1) 精密な閾値での発火保証 (決定論)** と
**(2) 常時可視の profile に載せきれない大規模 lore の on-demand recall** に絞られる。#19 (禁忌の
二層防御) と同型: 強モデルでは prompt 層が効くので engine/bridge の価値は「保証」と「スケール」。
小さな profile で済む盤面では bridge を無理に使わない判断もありうる (expansion_contraction)。
備考: 注入された伏線を**次ターンで LLM が織り込む**経路 (commit 460652b) は、demo が発火ターンで
goal 到達・終了したため実機未観察 (単体テストでは検証済)。継続する盤面で別途観測したい。

## crates/harness (2026-06-24 実プレイで発見: narration には正本のバックストップが無い)

### 23. 「正本>文章力」は ops だけを守る — narration は engine が検証しない (行商ネックレス問題)
実プレイで、プレイヤーが「行商先で手に入れたこのネックレスをアリスにあげる」と入力したら、所持品が
空なのに GM がネックレス贈与シーンを**既成事実として narration**し、アリスが受け取って好感度が上がる
描写まで出た。**これは op 検証のバグではない**: ネックレスは誰の inventory にも入っておらず (AddItem は
現在地 items 限定 + NPC 譲渡 op が無いので、op で出せば却下される)、LLM は **op を一切出さず純粋に
narration だけ**で贈与を成立させた。エンジンは弾く対象 (op) が無いので素通りした。
【構造の核心 / #19 との非対称】「正本>文章力」の二層防御で、**ops には engine のハードな
バックストップが在る**(prompt が減らし engine が保証する) が、**narration には engine の
バックストップが原理的に無い** (narration は LLM の領分=非検証)。よって narration が正本に反する
事実を主張する経路は、**prompt 層だけが唯一の防衛線**。#19 (禁忌) では弱モデルの保証を engine が
担えたが、本件は engine が担えない種類の穴。
【対策 (prompt 層、実装済)】GM_SYSTEM に3点を刷り込んだ: (1)narration は非検証ゆえ「現在の状態に
反する出来事を起こすな、一貫性は GM 自身が守れ」、(2)**プレイヤーの行動文は『意図』であって事実
ではない** — 所持品に無い物を使う/渡すと述べても存在しない、(3)未所持物は narration で既成事実に
せず「手元に無い」と物語内で接地せよ。**実 LLM 検証 (2026-06-24, claude-opus-4-8)**: 同じネックレス
行動 → GM は「胸元や懐を探っても何もない…口にしただけで手元には無い」とアリスに「何もお持ちで
ないみたい」と言わせ、状態変化ゼロ (好感度=0 のまま) で接地。単体テスト gm_system_grounds_unowned_items
が刷り込み文言を固定。
【残存リスク】narration に engine backstop が無いのは設計の本質。強モデルは prompt 接地で防げるが、
弱いローカル LLM は依然 narration で捏造しうる。将来の強化案: (a) 譲渡/使用を検証可能な op に
モデル化し (NPC inventory + give op)、状態に関わる行為は narration でなく op 経由を強制する方向、
(b) narration 監査パス (op に無い状態主張を後段で検出) — ただし narration を解釈する以上 LLM 依存に
なり決定論を失う。usage-over-extension で当面は prompt 層硬化に留める。

## crates/gm_core (2026-06-24 閉世界 capability — メアリー・スー遮断)

### 24. 能力も閉世界で宣言する — 「思い出したように開花する力」を構造で断つ (#23 の一般化)
#23 (所持物) と同クラス: 未宣言の **能力** を narration で開花させると、その場の都合で催眠/予知を
発揮する万能キャラ (メアリー・スー) になる。一般化した原理 = **capability は正本に宣言された閉じた
集合。未宣言は存在しない**。Kataribe は既に「未宣言 stat は却下」を持っていた → これを能力に拡張。
【実装 (option: 宣言+gate+開花トリガー)】CharacterDef.skills / Scenario.initial_skills で閉じた能力集合を
宣言 (初期=GameState.skills{entity})。Gate::HasSkill で能力を op/移動/取得/trigger の前提条件にできる。
**開花は authored トリガーの grant_skill 効果のみ**: LLM が grant_skill op を提案すると adjudicate が
RejectReason::SkillGrantNotAllowed で却下、trigger effects は apply_ops 直行なので付与できる
(= 禁忌/トリガー双対の三例目。「開花は許される、ただし作者が書いた gated 発火としてのみ」)。
【二層】engine 層 = 未宣言/未開花スキルを参照する op gate が false で遮断 + LLM grant_skill 却下。
prompt 層 = state_brief が各 entity の「使える能力」を提示し、GM_SYSTEM が「列挙された能力しか使えない/
勝手に開花しない/未宣言の力で局面打開を書くな」を接地 (narration 側の唯一の防衛線, #23 同型)。
【実 LLM 検証 (2026-06-24, claude-opus-4-8)】「眠っていた予知能力を思い出して発揮する」行動 →
GM は「何も来ない。予知能力なんてものは最初から自分の中になかった」と接地、状態変化ゼロ。
PoC: skills_load_from_declaration / has_skill_gate_blocks_without_skill / llm_proposed_grant_skill_is_rejected
/ trigger_awakens_skill_then_gate_passes (儀式→開花→予知 gate 通過→goal の正面)。gm_core 33→37。
【副次】schemars が GrantSkill を tool schema に自動露出するので LLM は提案できてしまうが、adjudicate が
常に却下するので幻アイテム (master_key) と同じく無害。プロンプト変更ゼロで schema 追従 (#17 の利点)。

## crates/gm_core (2026-06-24 NPC inventory + 譲渡 — #23 の engine 側バックストップ)

### 25. 所持物も閉世界・キャラ別にし、「渡す」を検証可能な op にする (#23 の engine 化)
#23 (行商ネックレス) の prompt 層対策に加え、構造対策を実装。所持物を**キャラ別の閉世界**
(`GameState.inventory{entity:[ItemId]}`) にし、譲渡を検証可能な op にした: `StateOp::GiveItem
{from,to,item}` は `from` が item を所持していなければ却下 (ItemNotHeld)、`to` が未知 entity なら
却下 (UnknownEntity)。= **持っていない物は渡せない** を engine が保証 (narration でなく op 経由なら)。
【設計判断: 波及最小化】`AddItem`/`RemoveItem` は player 専用のまま (リテラル ~10 箇所の破壊回避、
「拾得は世界→player」という素直なモデル)。per-entity 化が要るのは `Gate::HasItem{entity,item}` と
GiveItem のみ。Gate::HasItem は entity 既定 player なので YAML/既存テストは serde default で無傷。
【二層の完成 (#23)】narration 経路は prompt 接地 (#23)、op 経路は GiveItem の engine 却下。両輪。
ただし依然 narration 捏造そのものは prompt しか catch できない (narration に backstop は無い、#23 の本質)。
【実 LLM 検証 (2026-06-24)】gift シナリオで「花を摘む」→AddItem flower (所持: player)、「アリスに渡す」
→GiveItem player→alice (所持: alice) → goal。LLM がプロンプト変更ゼロで AddItem→GiveItem を駆動
(schemars 露出 + state_brief のキャラ別所持表示)。PoC: give_transfers_held_item / cannot_give_unheld_item
(ネックレス遮断) / cannot_give_to_unknown_entity。gm_core 37→40。
【未対応 (将来)】NPC の所在 (location) を持たないので「同じ場所のキャラにしか渡せない」制約は未実装。
NPC が世界からアイテムを拾う経路も無い (AddItem は player 専用)。必要になってから。

## crates/gm_core + harness (2026-06-24 技能判定 — 出目を物語に還流する)

### 26. 技能判定の結果は同一ターンの narration に間に合わない → 次ターンに還流する
`StateOp::Check{entity,stat,sides,dc}`: エンジンが `1d{sides} + stat修正 vs dc` を振り成否を裁定
(CheckOutcome を返す)。LLM は出目も合計も詐称できない (op 構造上)。stat 未宣言→却下 (幻ステータスで
判定を盛れない)。**核心の罠**: LLM は同一デルタで narration + ops を書くので、判定の出目 (apply 後に
確定) は**その turn の narration に間に合わない**。解: 結果を次ターンの prompt に「直前の判定結果」と
して還流し、GM に結果へ沿って語らせる (memoria_bridge の「輪の閉じ」と同じパターン)。GM_SYSTEM に
「不確実な行動は check で判定させ、この turn は『試みる』までに留め、結果は次ターンに返る」と接地。
【実装】apply_ops を out-param (rolls/checks の &mut Vec) に refactor し fire_triggers/check_taboos と共有。
run_turn に recent_checks 引数追加 (recalled_lore と並ぶ carryover)。ApplyOutcome.checks / TurnOutcome.checks。
【実 LLM 検証 (2026-06-24, claude-opus-4-8)】力の試練で「力ずくで石扉をこじ開ける」→ LLM が
check str/1d20/DC15 を発行 (narration は『試みる』止まり) → 🎯 1d20(14)+12=26 → 成功。次ターンの
narration が「先ほどの一撃では動いた」と前回成功を踏まえた。出目をエンジンが確定し LLM は詐称不能。
PoC: check_resolves_with_stat_modifier / check_with_unknown_stat_is_rejected / check_is_deterministic /
check_result_is_fed_into_next_prompt。gm_core 40→43, harness 25→26。
【副次観察】成功しても LLM は「動いたがまだ開ききらない」と部分成功に脚色し再判定した。エンジンは
「成功=扉が全開」を強制しない (成否の二値のみ確定、程度は語りの領分)。盤面が「成功で flag/move」を
求めるなら、判定結果を受けてプレイヤー/LLM が次手 (set_flag 等) を出す二段が要る。これは設計どおり
(判定は出目の確定、状態遷移は別 op) だが、判定成功を自動で状態に反映させたい場合は将来 trigger 連携
(check 成功 → flag) を検討。

### 27. 情景がくどく二度出る → 「忘れない GM」が自分の語りは覚えていない (state 真実 ≠ narration 継続性)
【実プレイ発見 (2026-06-24, classroom 盤面)】2ターン目の narration が開幕の情景を丸ごと再描写し
(夕日・散らかった机)、しかも moka の「気づいて驚く」という**一度きりの登場ビートを再演**した
(再会なのに初対面の演技＝「矛盾しない GM」の破れ)。【原因 (現物で特定)】(a) `scenario_brief` が
場所の `description` を毎ターン system に入れる → 開幕の一度きりビート入り description が再レンダリング
される。(b) **`run_turn` は毎ターン messages を新規構築する** (state が唯一の真実という設計) ため、LLM は
自分が直前に何を語ったかの記憶を持たない → 静的情景をゼロから再 establish するしかない。【核心の罠】
正本エンジンは **state (数値/フラグ/位置) の真実**は握るが、**narration の継続性は別の文脈チャネル**で、
state-grounding ではカバーされない。「忘れない GM」は世界状態を忘れないが、ステートレス再構築ループでは
自分の散文を忘れる — これは state-truth とは独立した第二の失敗軸。【解】直前ターンの語りを
`recent_narration` として次ターンの prompt に還流し (check/lore carryover と同じ輪)、
`recent_narration_note` で「既出の静的情景・済んだ登場/挨拶/初対面の驚きを繰り返すな、続きから変化だけ
描け」と接地。**prompt 層のみ** (engine 不変)。run_turn に引数追加、CLI/app の両ループが last_narration を
持ち越す (campaign 遷移時はリセット=新しい情景)。【実 LLM 検証 (2026-06-24)】同じ冒頭行動で再プレイ →
2ターン目以降は会話の続きに直行、情景の再描写・驚きの再演が消え、継続フレーバー (「夕日が頬を染める」)
程度に留まった。PoC: recent_narration_is_woven_into_prompt_for_continuity /
no_recent_narration_means_no_continuity_block。harness 33→35。
【authoring 観測】location.description に一度きりビート (「モカが入ってきた」「気づくと驚いた表情」) を
書くと再演の温床。description は**持続する場所**を描き、登場/挨拶は GM の即興に委ねるのが筋
(継続性 note で大半は中和されるが、authoring 側でも分けると堅い)。

### 28. narration に tool-call マークアップが漏れる → 非検証ゆえソースで掃除する
【実プレイ発見 (2026-06-24, classroom)】表示された語りの末尾に `</narration>` と
`<parameter name="ops">[{...}]` が混入した。【原因 (現物で特定)】リクエストは tool_choice で
emit_delta を強制 → 応答は native tool_call の arguments(JSON) を `parse::extract` で StateDelta 化。
CLI/app は `delta.narration` だけ表示するので、見えた format token は **narration の文字列値の中**に
在る。モデルが narration 文字列へ `</narration>`/`<parameter name="ops">` 等の構造化出力 token を
漏らした (ops 配列自体は別フィールドで valid)。tool_call 全体は valid JSON なので extract はエラーに
せず素通り → 混入が残る。【核心】narration は**非検証** (LLM の領分、engine バックストップが原理的に
無い #23 と同型)。だから「弾く」のでなく**提示前に掃除**する。【解】`parse::sanitize_narration` を
`generate_delta` で適用 (extract 直後)。先頭の開きタグ (`<narration>`/`<parameter name="narration">`) を
剥がし、構造マークアップ (`</narration>` / `<parameter` / `<invoke` / `<function_calls>` 等) の最初の
出現以降を切り捨て、trim する。ops は valid な別フィールドなので無改変。提示層の `\n` 正規化(#16)と
同じ「正本を汚さない後処理」をソース(llm_client)に置くことで CLI/app/却下 echo すべてに効く。
【PoC】sanitizes_leaked_tool_markup_from_narration (実例 + 開きタグ + XML 関数タグ + 無改変ケース) /
extract_passes_leaked_tags_sanitize_cleans_them (extract 単体ではタグが残る=症状、sanitize で掃除、
ops は別フィールドで正常)。llm_client 9→11。
【棄却した代替】prompt で「XML タグを書くな」と刷り込む案は、format token 漏れを確実には防げず
(narration は非検証で唯一の防衛線が prompt なのは #23 と同じだが、ここは決定論的に掃除できる経路が
在る)、prompt を伸ばすだけなので不採用 (usage-over-extension)。決定論的な後処理が在る所はそちらを使う。

### 29. OpenAI 互換 ≠ tool_choice 対応 → no-tools / JSON モードが要る (さくら AI Engine)
【実機で分離・確定 (2026-06-24)】ユーザーが .env をさくらのクラウド AI Engine
(https://api.ai.sakura.ad.jp/v1, OpenAI 互換) に変更 → 「うまくいかない」。CLI 1ターンで再現すると
status=500 "Upstream server error"。**変数を分離**して原因を二つに切り分けた: (1) モデル
`preview/Qwen3-0.6B-cpu` は素の chat でも **504 Gateway Timeout** — CPU プレビューが応答しない(死因①、
モデル選択ミス)。マニュアルのサンプル `gpt-oss-120b` は 200。(2) `gpt-oss-120b` + `tool_choice` 強制は
**200 だが tool_calls:[] のまま** — さくらの serving (vLLM) は OpenAI の関数呼び出し強制を**実装して
いない**(死因②)。content に壊れた JSON もどきが出るだけ。語り部は tool-use 強制が前提なので構造化
出力が取れず Parse 失敗。【核心の罠】「OpenAI 互換」を名乗っても **tool_choice / function calling
対応は別物**。chat/completions は通っても tool 強制は通らないサーバが普通にある(特にローカル/廉価層)。
【解】`LlmConfig.use_tools`(`LLM_USE_TOOLS`、既定 true)で切替。off の時 tools/tool_choice を送らず、
`json_instruction`(schemars schema を載せた「JSON だけ出せ」system 指示)を積み、既存の content
フォールバックで拾う。prose 包みは first`{`..last`}` で救済。**実機実証**: さくら `gpt-oss-120b` +
`LLM_USE_TOOLS=false` で「モカに挨拶」が valid narration を生成・state 更新・日本語化けなし
(reqwest は正しい UTF-8。curl の化けはシェル由来でクライアント無実)。PoC:
parses_state_delta_from_plain_and_prose_content / json_instruction_carries_schema_and_directive /
config_defaults_to_tool_use。llm_client 11→14。
【設定の正解】さくらを使うには .env を: LLM_BASE_URL=https://api.ai.sakura.ad.jp/v1 /
LLM_MODEL=gpt-oss-120b (cpu プレビュー不可) / LLM_USE_TOOLS=false / LLM_API_KEY=<UUID>:<シークレット>
(ペア形式)。同人配布の北極星(受領者は tool 非対応の安い/ローカルモデルを使う)に直結する機能。

### 30. 推論モデルの `<thought>` CoT が no-tools の本体 JSON 抽出を壊す (#29 の続き)
【実機発見 (2026-06-25, Gemma4 @ no-tools)】「提案者エラー: 構造化出力のパース失敗: expected value at
line 1 column 1」。【原因 (生出力を現物トレースで特定)】Gemma は no-tools モードで `<thought>...
</thought>` に chain-of-thought を吐き、**その中に `` `[{"op":"adjust_stat",...}]` `` という JSON 断片を
書いてから** ```` ```json ```` フェンスで本体 StateDelta を出す。旧 content フォールバックは二重に破れる:
(1) `strip_code_fence` は content が**先頭からフェンスで始まる時だけ**剥がす → `<thought>` が前置きなので
フェンスが見つからず素通り → 直接 from_str が `<thought>` から始まって失敗 (= line 1 col 1)。
(2) フォールバック `extract_json_object` の first `{` が **thought 内の断片**に釣られ、first `{`..last `}` が
`{断片}]...</thought>...{本体}` という壊れた span になり parse 失敗 → 元の source エラーを返す。
【さらに罠】`StateDelta` は narration/ops が `#[serde(default)]` なので、無関係な断片 `{"op":...}` すら
**空デルタとして parse 成功**する。だから「最初に parse できた object」を採ると空の語りを拾う。
【解】(a) `parse::strip_reasoning_blocks` で `<think>`/`<thought>`/`<thinking>` (大小無視・終了タグ無しは
以降全切り) を抽出前に除去 → CoT のノイズと「フェンス前置き」を同時に消す。(b) フォールバックを first
`{`..last `}` から **string-aware な balanced 波括弧抽出 `json_objects` の最後の object** に置換 (「答えは
推論の後に来る」原則。空デルタ断片でなく本体を拾う)。どちらも提示層でなくソース(llm_client)に置き
extract 経路全体に効く。#28 (narration の format token 漏れ) と同じ「推論モデルが構造化出力の周りに
ノイズを足す」系だが、こちらは **no-tools で本体 JSON 自体が拾えなくなる**より重い破れ。
【PoC】reasoning_block_then_fenced_json_resolves (Gemma 実出力を忠実再現: `<thought>`+断片+フェンス) /
last_balanced_json_object_wins (タグ無しでも最後の object を採る=serde(default) の空拾い罠を固定)。
llm_client 14→16。
【棄却した代替】prompt で「思考を書くな」と刷り込む案は推論モデルの CoT を確実には抑えられず
(Gemma は構造的に thought を出す)、決定論的に掃除できる経路が在るのでソース後処理を採る (#28 同型・
usage-over-extension)。タグ依存を補うため (b) の convention-free な balanced 抽出を安全網として併設。
