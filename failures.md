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
