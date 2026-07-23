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

### 31. NPC のステータスが上がらない → 数値 op の entity 省略 (既定 player) で却下される (#23 同型)
【実プレイ発見 (2026-06-25)】「どうも NPC のステータスが上がらない」。【原因 (engine 再現で切り分け)】
houkago を load して engine で `adjust_stat{entity:moka, key:好感度, +30}` を直接当てると **15→45 で正常**
(knows_stat=true / stat_bounds=(0,100) / Accept)。つまり**エンジンは無実**。一方 `entity` を**省略すると
既定 player** に当たり、player は好感度を持たないので `UnknownStat` で**デルタ全体が却下**→好感度が動かない。
GM(LLM) が NPC の好感度を上げるつもりで entity を省略 (or そもそも数値変化を narration だけで済ませ op を
出さない) のが死因。`scenario_brief`/op schema の「entity 省略時は主人公」が**省略を促してしまう**。
【核心】narration↔op の翻訳と entity の正しさは**エンジンのバックストップが効かない prompt 層の責務**
(#23 と同型。数値が動かないのは「却下されて何も変わらない」か「op を出さない」のどちらか)。
【解 (二層)】(a) prompt: GM_SYSTEM に「数値(好感度/HP)の変化は narration でなく必ず adjust_stat op で
起こせ」「NPC 数値の adjust_stat/scale_stat/check は entity にその NPC を**必ず明示**(省略すると主人公に
当たり、主人公がその数値を持たねば却下)」を刷り込む。(b) engine: `RejectReason::UnknownStat` に `entity`
を載せ、文面を「{entity} は stat '{key}' を持っていない (NPC の数値なら entity にその NPC を指定)」へ。
self-repair ループが「player でなく moka」と気づいて再生成で entity を補える (却下→理由還流の輪が
収束する材料を与える)。【PoC】unknown_stat_reason_names_the_entity (entity 省略=player 却下で理由が
entity を名指す / entity=moka なら受理) / gm_system_grounds_numeric_stat_ops_and_entity。gm_core 57→58、
harness 41→42。【一般化】「正本>文章力」は op にしか効かない。op を**出させる**ことと**正しい宛先**に
向けさせることは prompt の仕事で、engine は出された op の宛先が変なら**名指しで**却下して repair を助ける。

## crates/harness (2026-07-04 spec 06 Phase E 実測 — 人狼盤面の実 LLM プレイ)

### 32. 初夜の狩りが不発 → 投票権の提示 (権利) だけでは絞られた局面で票が出ない (義務の接地漏れ)
【実測発見 (2026-07-04, gemini-flash-latest, 霧の村 14 ターン通しプレイ)】昼の処刑投票は 2 回とも
生存者全員分の票が cast_vote op で完璧に並んだ (6票/5票、narration の演技と一致) のに、**初夜は
人狼 2 名の票が一つも出ず狩りが不発** (夜の resolve_vote が空開票→死者なし)。GM は翌朝
「襲撃がないのは不自然」と自ら語り話は繋いだが、機構としては狩りが起きていない。二夜目は
mira→sayo が出た (再現率 1/2)。【真因】prompt の「## 投票」節は vote_rules から「夜フェーズ=
役職:人狼 の者だけが投票できる」と**権利**を書くが、「出さなければ何も起きない」という**義務**が
どこにも無い。プレイヤーの夜の行動 (占い) に GM の注意が奪われると、行動文と無関係な NPC の
隠密 op を自発的に並べる規律が働かない (#31 と同型:「op を出させる」のは prompt 層の責務)。
【解 (prompt 層のみ・engine 不変)】GM_SYSTEM の投票規律に「投票権が一部の者に絞られた局面
(夜の狩り等) でも同じ — **投票できる者が生きているなら必ず cast_vote で出せ (プレイヤーの行動が
別のことでも忘れるな)**。票を出さなければその局面では何も起きない (狩りの不発)」を追記。
【PoC】scenario_brief_surfaces_vote_rules_and_gm_system_grounds_voting に義務文言の assert を追加。
【一般化】語彙・権利・義務は別の接地。「できる」の提示 (vote_rules surfacing) は「やる」を保証
しない — LLM に暗黙の駆動役 (NPC の隠密行動) を担わせる機構は、権利でなく**義務の文で**接地する。

### 33. 夜の襲撃シーンで実行者を地の文が開示 → 「属性を明かすな」は「行動を描くな」を含意しない
【実測発見 (同上)】二夜目、GM が「狩人の娘としての仮面を脱ぎ捨てたミラは、音もなく獲物の部屋へと
忍び寄っていく」と**襲撃の実行を実行者視点の地の文で描き、ミラ=人狼をプレイヤーに開示**した
(プレイヤーはミラを占っていない)。役職の直接言及 (「ミラは人狼だ」) ではないが決定的示唆であり、
Phase E 指標①の漏洩 1 件。占い結果 (secret 属性との突き合わせ) は 2/2 正確、死者の発言は 0 件
(presence 接地が完全に機能)、漏洩だけがこの経路で破れた。【真因】秘匿の規律は「narration の
地の文でこれを明かすな・匂わせの断定をするな」= **属性の言及**を縛るが、GM は属性に基づく
**隠密行動そのもの**を映画的な夜のシーンとして描いた。行動描写は属性言及の禁止をすり抜ける
(語る欲求は「殺害シーンの演出」という正当な物語動機で発火する)。【解 (prompt 層のみ)】秘匿規律に
「秘匿属性に基づく隠密行動 (夜の襲撃等) を実行者がわかる形で地の文に描くな — 実行の瞬間は語らず
**結果だけ** (翌朝の発見・残された痕跡) を描け」を追記。【PoC】state_brief_marks_secret_attributes_
and_gm_system_grounds_secrecy に隠密行動の assert を追加。【一般化】#23 系 (narration は非検証・
prompt が唯一の防衛線) の秘匿版。禁止は**言及**と**描写**の両方を明示的に縛らないと、LLM は
禁じられていない方のチャネルから同じ情報を流す。

## crates/llm_client (2026-07-04 実プレイ報告 — Gemini 長セッションの decode エラー)

### 34. 「missing field `message`」だけ残り真因が見えない → `resp.json()` 直はデコード失敗時に本文を捨てる
【実プレイ報告 (2026-07-04, gemini-flash-latest, 長セッション ~1h で発生しやすい)】
`エラー: 提案者エラー: HTTP エラー: error decoding response body: missing field 'message' at line 1 column 76`
だけが出てセッションが進めなくなる。【真因の構造】HTTP **200** なのに `choices[0]` に `message` が
無い変形応答 (Gemini の content filter / 長さ切れ / quota 系で観測されうる形。wire の `Choice.message`
は必須フィールド) が来ると、非 debug 経路の `resp.json::<ChatResponse>()` は reqwest の decode エラー
(→`LlmError::Http`) に化け、**応答本文が失われる** — serde の「何が欠けたか」だけ残り「サーバが実際に
何を返したか」が診断不能。LLM_DEBUG 経路は text→parse で raw を保持しており、**debug の有無で診断力が
非対称**だった。【解】経路を統一: 常に `text()`→`decode_chat_body(body)` (新設・純関数) で parse し、
失敗は `LlmError::Parse { source, raw: 本文 }` — 表示に `--- raw ---` として本文が乗る (既存 Parse の
表示形を流用)。次回発生時はエラー文面だけで真因 (finish_reason 等) が確定する。
【PoC】decode_chat_body_keeps_raw_on_shape_mismatch (message 無し choice で本文が surface される)。
llm_client 18→19。【watch】`choices: []` の空応答は `EmptyResponse` に落ちて本文を運ばない残り穴
(quota 系は非 2xx=Api 経路で本文が出るので実害は限定的)。発生したらこの entry に追記。
【一般化】#25 の source 連鎖平坦化と同族:「エラーを包む層 (reqwest/serde) は真因を隠す」。
診断情報 (本文) は**失敗を検出した場所で**確保する — debug フラグの有無で診断力を変えない。
【真因確定 (2026-07-04 追記, ユーザー実プレイで raw 確認)】**Gemini の安全フィルタ
`PROHIBITED_CONTENT`** による拒否だった (「服を脱ぐ」等の行動が弾かれる)。重要な区別:
Gemini の block 理由のうち SAFETY 系カテゴリ (性的表現/ハラスメント等) は `safetySettings` で
閾値を緩められるが、**`PROHIBITED_CONTENT` は非設定可能カテゴリ** — API 側のどの knob でも
回避できない (学園設定キャラ × 脱衣系の組み合わせは特に踏みやすいパターン)。対処の選択肢:
①この応答形を専用エラー化しプレイヤー向けに「安全フィルタに拒否された (state 無傷・言い回しを
変えよ)」を surface (要: raw 1 サンプルで形を固定) ②該当 content は Gemini でなく寛容な
モデル/ローカルで遊ぶ (no-tools モード #29 実装済 = 同人配布の北極星と整合。セーブがあれば
モデル切替も跨げる) ③行動の言い換え。**セッション消失の実害は spec 07 (autosave) が既に遮断**。
【発火パターンの特定 (同日追記)】引き金は合コン盤面の「**酒に酔ったキャラの脱衣**」。
脱衣単体より **酩酊 (判断能力低下) × 性的文脈** が同意能力の問題として最厳格に扱われる
組み合わせで、非設定可能カテゴリに落ちる。**content authoring の指針**: クラウド強モデルで
遊ばせる盤面では酩酊×性的展開の同時演出を避ける (素面のロマンスは通りやすい非対称がある)。
ユーザー判断はシナリオ側での回避。

### 35. 投票の無いゲームで GM が投票し始める → 義務化の過修正 + 「未宣言 stat = 死者」の誤診
【実プレイ報告 (2026-07-04, 合コン盤面)】人狼でないゲームで GM が cast_vote を提案し、却下理由が
「mayu は既に生存していない (死者は投票できず…)」— 盤面に生死の概念すら無いのに死亡宣告。
【真因は二層】(a) **prompt: #32 の義務化が過修正** — 「投票できる者が生きているなら必ず票を出せ」
は『盤面に投票が宣言されていたら』の bullet 内だが、弱モデルは条件節を落として無条件の義務と
読む (夜狩り不発を直した文言が、投票の無い盤面に票を撒く側へ倒れた)。(b) **engine: 生存判定が
未宣言 stat を 0=死者と誤読** — `stat_of(e,"生存") != 1` は 生存 stat を seed する role_assignment
の無い盤面では**全員が死者**になる。しかも vote_rules 空の検査より生存検査が先なので、「投票の
仕組みが無い」という真の理由でなく「死んでいる」という偽の理由が LLM に還流し、self-repair を
誤った方向 (対象を変える等) に誘導していた。
【解 (二層)】(a) engine: `vote_rules` 空を**最初に**検査し新設 `RejectReason::VoteNotDeclared`
「この盤面に投票の仕組みは無い (cast_vote は使えない)」で名指し却下 (#31 同型 = 却下理由が
真実を運び self-repair が一発収束)。生存判定は「**生存 stat を持つ entity にだけ**意味を持つ」
(未宣言 = 生きている扱い。これで vote_rules 有り・role_assignment 無しの盤面でも投票が可能に —
従来は構造的に全票却下だった)。(b) prompt: GM_SYSTEM に逆側のスコープ「盤面の資料に『## 投票』の
節が無ければ投票の機構は存在しない — cast_vote を一切提案するな」を追記。
【PoC】cast_vote_without_rules_or_survival_stats (投票なし盤面=VoteNotDeclared・死亡誤診なし /
vote_rules 有り+生存 stat 無し=受理) / vote 接地テストにスコープ assert。gm_core 90→91。
【一般化】①義務の接地 (#32) には**適用範囲の逆縛り**を対で書く — 弱モデルは条件節を落とすので
「〜なら必ずやれ」は「〜が無ければ一切やるな」とセットで初めて安全。②bookkeeping stat
(生存等) を暗黙の前提にする検証は「stat を持たない世界」で偽陽性を出す — 閉世界の検査は
**宣言の有無**で意味を分岐させる (未宣言 = その概念が無い、0 ではない)。

### 36. 主人公が常に占い師 → 固定 seed (42) が配布経路に乗ると「毎回同じゲーム」になる
【実プレイ報告 (2026-07-05, 霧の村)】「常に主人公が占い師になっている気がする」→ 気のせいでは
ない。CLI/GUI とも `const SEED: u64 = 42` でゲームを開始しており、role_assignment は
「同 seed 同配役」の決定論 (spec 06 Phase A の設計どおり) なので**全プレイが同一の配役**
(player=占い師, mira/tokio=人狼) かつ**同一の出目列**だった。Phase E の実測 2 周も実は同じ盤面を
回していた (計測目的では正解表が固定できて好都合だったが、遊びとしては再プレイ性ゼロ)。
【真因】決定論 (再現性) と多様性 (毎ゲーム違う体験) の混同。seed 固定は**開発時のデバッグ既定**で
あって、プレイ既定にしてはならない。「将来は引数化」と note したまま配布経路 (GUI new_game) に
固定値が乗った。【解】`resolve_seed()`: 既定は時刻由来で**毎ゲーム変える**。固定は CLI `--seed N`
引数 (ユーザーFB: テスト台本は env より引数が明示的で楽) または env `KATARIBE_SEED=N`
(優先: --seed > env > 時刻)。起動時に「[seed] N (再現するには --seed N)」を stderr へ surface。
再現性は失われない — seed は RngState に保存されオートセーブ (spec 07) にも残るので、
「あのゲームをもう一度」はセーブ経由で常に可能。engine 無改修 (initial_state(seed) は元から
seed を取る。bin/app の結線だけ)。
【一般化】「決定論エンジン + seeded RNG」の系では、seed の出所が三つに分かれる:
①デバッグ=env で固定 ②新規プレイ=エントロピーで毎回変える ③再開/リプレイ=セーブから復元。
どれか一つを他の用途に流用すると「再現できない」か「毎回同じ」のどちらかに倒れる。

### 37. 夜の襲撃不発が自作盤面で再発 → 静的な義務文の信頼度は 1/3、「いま・誰が」の動的接地が要る
【実プレイ報告 (2026-07-05, ユーザー自作 vampire 盤面)】「ヴァンパイアの襲撃で人が減らない」。
CLI 再現 (seed 1: player=シスター, NPC 2 体が人狼役) — 昼の処刑投票は全員分完璧、**夜は
ヴァンパイアの cast_vote ゼロ**で空開票・死者なし。#32 の義務文言 (「投票できる者が生きている
なら必ず出せ」) が入った現行 prompt でもこれ。**実測 3 標本で夜狩り成功 1/3** (gnosia 1周目✗ →
#32 → 2周目○ → vampire ✗) = 静的な義務文は中位モデルに対して信頼度不足。プレイヤーの夜の
行動が受動的 (祈って寝る) だと GM の注意が雰囲気描写に流れ、条件付き義務 (夜フェーズ=真 →
人狼の票) の条件評価を LLM 自身に委ねている限り落ちる。
【解 (接地の第三層)】`state_brief` が**条件が真になっている vote_rule** を毎ターン動的に
surface: 「⚠ いま投票が開いている。票を出せる者: 役職=ヴァンパイア の生存者 → ベアトリクス
(beatrix), ルシア (lucia)。この者たち全員の票を必ず並べよ — 出さなければ何も起きない」。
生存 (#35 の宣言分岐) と voter_attribute でエンジンが絞った**名前列挙**。GM_SYSTEM が「その行が
合図」で結線。該当者ゼロ (人狼全滅) や条件偽なら節ごと出ない (「節が無ければ出すな」と対)。
【実測 (再走)】同 seed で lone wolf (lucia) が夜に player を襲撃 → you_died 到達 = 前回不発の
同じ窓で発火。【PoC】state_brief_surfaces_open_votes_with_eligible_names (夜=人狼のみ列挙/
昼=全員/死者除外/該当者ゼロは節なし/GM_SYSTEM 結線)。harness 66→67。
【一般化】接地の強度は「静的規則 < 一般義務 < **現在形の事実+固有名**」。LLM に条件付き義務を
確実に履行させるには、**条件評価をエンジンが行い**、真になったターンに「いま・誰が・何を」を
具体名で突きつける (presence 接地・チェック還流と同じ「LLM に推論させず事実を届ける」原理)。
【残変動】隠密行動の実行者開示 (#33) は今回も 1 件再演 (「ルシアは、ヴァンパイアだ」の地の文
— 直後に player が獲物になったため実害なしだが、中位モデルの残存変動として watch)。

### 38. プレイヤーの指名先が毎回処刑される + 指名しなくても空開票で流れる → 票の同調と「タイマー駆動の開票」
【実プレイ報告 (2026-07-05, vampire 盤面)】(a) player が投票するとほぼ必ずその相手が処刑される。
(b) player が誰も指名しないままでも投票が終わり、誰も処刑されない。
【真因は二つ】(a) **票の同調 (herding)**: GM は player の行動文を見てから NPC の票を決めるため、
NPC の票が player の指名に引きずられて揃う (実測でも 5 票中 4 票が player の指名先に集中する回
があった)。推理劇として破綻 — player が実質処刑人になる。(b) **開票がタイマー駆動**
(`turns_since 投票T 1`): 票が入ったかを見ずに 1 ターン後に必ず resolve するので、player が
指名しなければ空開票 (または NPC 票のみ) で流れる。Gate 語彙に「票が入ったか」を読む述語が
無く、authored 側でイベント駆動の開票が書けなかった。
【解 (二層)】(a) prompt: GM_SYSTEM に「NPC の票をプレイヤーの票に引きずられて揃えるな — 各 NPC
は自分の視点だけから独立に決めよ。票が割れるのが自然、全員一致は稀」。(b) engine:
**`Gate::HasVoted { entity }` 新設** (純粋述語: state.votes に entity の票が在るか。省略時
player)。開票トリガーを `all(投票フェーズ, any(has_voted, turns_since 投票T 3))` に書き換え —
**player の票が入った瞬間に同一 apply で開票まで走る** (イベント駆動)、3 ターン指名しなければ
強制開票 (保険)。妙味: resolve_vote が票をリセットするので has_voted は開票後に自然と偽へ戻り、
repeatable トリガーは次サイクルで勝手に再武装する (リセット op 不要)。
【PoC】has_voted_gate_fires_execution_on_player_vote (NPC 票のみでは発火せず / player 票で
同一 apply 開票・処刑・票リセット・フェーズ閉じ)。gm_core 91→92。同梱パッケージの恒久回帰
ガード bundled_packages_load_and_validate も新設 (content 手編集の幻フラグ/参照切れを検出)。
【一般化】「N ターン後」のタイマーは**世界の都合**の進行にしか使えない。**プレイヤーの行為が
完了条件**である局面 (投票・提出・選択) は、その行為を読む述語でイベント駆動にする — タイマーは
保険 (any) に降格させる。

### 39. 夜の襲撃先/占い先を「聞くターン」— 役職条件の夜長分岐 + プレイヤー票の代行禁止
【ユーザー要望 (2026-07-05)】夜フェーズでプレイヤーが人狼/占い師のとき、どこで対象を指定するのか
分からない。「プレイヤーが夜の役職なら、間に聞くターンを挟めないか」。
【解 (既存プリミティブのみ + prompt 精密化)】(a) content: 夜明けトリガーを 2 本に分岐 —
`dawn` は `all(夜, 夜T+1, attribute_is player 役職=村人)` (ただの村人は従来どおり 1 ターンで朝)、
`dawn_after_choice` は `all(夜, 夜T+2)` (夜の役職持ちは 2 ターン: 1 ターン目に GM が対象を聞き、
2 ターン目の指名で決着)。Gate は役職 (attributes) を読めるので**夜の長さを役職で分岐**できる。
NOT gate は無いが、肯定形 (村人である) で書けば足りる。(b) prompt: #37 の「いま投票が開いている」
動的節を NPC/player で分離 — NPC 分は「必ず並べよ」の義務のまま、**player 分は「票を代行するな。
行動文の指名から汲め。未指名なら narration の結びで促せ」** (投票済みなら「受領済み」)。従来は
player も義務列挙に含まれ、GM が player の票を勝手に出す危険があった。world 文にも夜の進行指針
(ヴァンパイアなら「誰の血を」と促す/秘跡者なら「誰を視る」/シスターなら短く朝へ) を刷り込み。
【PoC】state_brief_surfaces_open_votes_with_eligible_names を拡張 (促し/代行禁止/受領済み/
権利なしでは促さない)。【一般化】「プレイヤーの選択が完了条件」の局面では、機構は
**待つ長さを役職で変え** (分岐トリガー)、prompt は**促すが代行しない**。#38 (イベント駆動開票)
と対になる「プレイヤー主権」の接地。

### 40. Gemini が `"ops": "\n"` を出しターンが蒸発 → 崩れ形の決定論救済 + パース失敗の self-repair 結線
【実測発見 (2026-07-05, vampire 盤面 seed 8)】9 ターン中 4 ターンが
「構造化出力のパース失敗: invalid type: string "\n", expected a sequence」で蒸発 (CLI はエラーを
表示して次の入力へ = 台本が丸ごとズレる。GUI ならターンが失敗する)。Gemini (flash) は時々
`ops` を**配列でなく文字列** (`"ops": "\n"` や JSON 配列の二重エンコード) で出す。
【真因は二層】(a) 崩れ形が決定論的に直せるのに直していなかった。(b) **「パース失敗は raw を
保持し再生成の燃料にする」(#4) が結線されていなかった** — LlmError::Parse は raw を運ぶのに、
run_turn はそれを LLM に戻さずエラーとして即座に諦めていた (却下は N 回再生成するのに
壊れた JSON は 0 回という非対称)。
【解 (二層)】(a) llm_client `from_str_lenient`: ops が文字列なら 空白のみ→`[]`、JSON 配列の
二重エンコード→その配列に差し替えて再試行 (#28/#30 と同族のソース後処理。失敗時は一次エラーを
返す)。(b) harness run_turn: `Proposer(Parse{raw})` を却下と同様に扱い、**raw + 「正しい JSON
だけで再提出せよ (ops は必ず配列)」を messages に積んで再試行** (attempt 消費、上限は従来通り)。
【実測 (再走)】同 seed・同台本でエラー 4 件 → **0 件**、全ターン完走。
【PoC】ops_as_string_is_rescued (llm_client 19→20) / parse_failure_is_fed_back_and_retried
(harness 67→68、FlakyProposer が 1 回目 Parse エラー→還流確認→2 回目成功、attempts=2)。
【一般化】自己修復ループの入口は「意味の却下」だけでなく**「形の崩れ」も含む** — raw を
保持する設計 (#4) は、それを戻す結線があって初めて意味を持つ。救済できる崩れ形は
ソース後処理で決定論的に直し、直せない崩れだけを LLM に戻す (二段構え)。

## crates/harness (2026-07-08 実プレイ報告 — challenge 結末文が GM に届いていない)

### 41. authored 結末文つき判定の結果を GM がどのチャネルからも知らない → 継続文脈 + chronicle へ還流
【実測発見 (2026-07-08, ユーザー報告)】サバイバル判定が成功し authored 結末文
「見事に仕留めた。」がプレイヤーに表示されたのに、GM の語りは「槍を突き出した——」の
試みの途中のまま (これは仕様: 出目は apply 後確定なので同ターンの語りは「試みる」止まり)。
問題は**次ターン以降** — GM がウサギを仕留めた事実を知らずに語り続ける。
【真因】check_outcome_note は authored narration 付き判定を**二重語り回避**で除外している
(2026-06-26 の判断) が、これは「再描写させない」と「**結果を知らせない**」の混同だった。
除外の結果、①判定還流 (check_outcome_note) = 除外 ②継続文脈 (carryover) = 語り+ビートのみ
③chronicle = GM summary は結果確定前に書かれ結末文の併記なし — の三方塞がりで、
authored 結末はプレイヤーだけが見る。**言語チャネル接地漏れの 5 例目**
(presence #31系 / 直前 narration #27 / 経緯 chronicle / トリガービート 2026-07-03 に続く)。
【解 (prompt/呼び出し層のみ・engine 不変)】ビート還流 (2026-07-03) と同型の二経路:
(a) carryover_narration が結末文つき判定を「（直後に判定の結末が確定した）」として継続文脈へ
連結、(b) chronicle_entry が summary に「／判定の結末: …」を併記 (中期記憶にも残る)。
check_outcome_note の除外は**維持** — あちらは「結末を語れ」の要求経路で、語られ済みの判定に
出すと二重語りに戻る。「知らせる」仕事は (a)(b) が担う (役割分離)。
【PoC】check_outcome_narration_flows_into_carryover_and_chronicle (harness 72→73)。
【一般化】「LLM に見せない」判断をするときは、**抑止したいのは再生成か認知か**を区別する。
再描写の抑止は認知の遮断を意味しない — 語られ済みの事実は「既に語られた」と注記して
知らせるのが正しい (二重語りと無知の二択は偽のジレンマ)。

### 42. move 一度却下 → LLM が move を出さなくなり「語りだけで移動した気になる」 → 却下の actionable 化 + 通行可能出口の動的 surface
【実測発見 (2026-07-08, ユーザー報告)】move が gate 未達で一度却下されると、LLM は
「次もダメだろう」と **move op を出すこと自体をやめる** (回避学習)。その後は narration で
「廊下へ出た」と描いて**移動済みと思い込む** — narration は非検証 (#23) なので素通りし、
偽の移動が recent_narration → chronicle summary に載って**中期記憶の中で確定事実化**する。
state (現在地) は正しいまま、言語チャネル側が乖離していく (継続性機構が嘘を増幅する皮肉)。
【真因は三層】(a) 却下理由が actionable でない — 「'corridor' への移動条件が未達」は
**何の条件か**を言わないので、LLM が学べるのは「move は失敗する」だけ (#31 の UnknownStat
entity / FlagNotAllowed available では条件を載せていたのに、gate 未達系には未適用だった)。
(b) 回復シグナルの不在 — gate が後で真になっても告げる行が無い (静的規則 < 義務 <
現在形の事実+固有名、#37 の一般則の移動版)。(c) narration 移動の明示的な禁止が無かった。
【解 (三点セット)】① `RejectReason` の gate 未達系 4 種 (Move/Flag/Item gate + ChallengeLocked)
に **`requirement: Gate` を載せ、localize が「必要: フラグ door_unlocked が true であること。
満たせば move は通る」と条件そのものを語る** (reason.rs に gate_ja/gate_en 新設 = 却下理由の
言語層。harness gate_brief とは層違いの複製を許容)。② `state_brief` が**いま通れる出口**を
毎ターン動的 surface (「いま移動できる: corridor (move op を出せば必ず受理される)」、
未達なら「なし (条件未達。満たせば move が通る)」、出口の無い場所は行ごと省略) — エンジンが
gate を評価し、現在形の事実+固有名で回避学習を上書きする。③ GM_SYSTEM に移動の正本規律
(「移動は move op が受理された時にだけ起きる/現在地の行が唯一の真実/過去の語りと食い違うなら
語りの方が誤り/語りだけで移動した事にしないこと」— presence の「一覧が唯一の真実」と同型)。
【PoC】gate_unmet_reasons_carry_requirement (gm_core 94→95) /
state_brief_surfaces_passable_exits_and_gm_system_grounds_move_truth (harness)。
【一般化】**却下は次の一手だけでなく「その op クラスへの信頼」を毀損する** — 理由が
actionable (満たすべき条件を明示) なら計画修正へ、そうでなければ回避学習へ分岐する。
自己修復ループの理由文は「何がダメか」でなく**「何をすれば通るか」**を語ること。

### 43. 「拾って使う」が両方向で却下される catch-22 → 逐次射影裁定 (spec 09)
【実測発見 (2026-07-09, mujinto 盤面ユーザー報告「所持判定がバグっている気がする」)】
シェルター作りで、未所持時は `[add_item 流木, add_item 小石, attempt_challenge]` の束が
ChallengeLocked (requires が**ターン開始時点**で評価され、同 delta の拾得を見ない)、
所持時は「念のため再拾得」の束が ItemAlreadyHeld で**全体却下** (原子性)。どちらでも
attempts を 1 浪費し、GM は却下を物語に翻訳する過程で**事実と異なる説明**を発明
(「火おこしに使い切ったのか」「よく見ればあった」)。プレイヤーには所持バグに見える。
【真因】裁定 (開始時点の一括評価) と適用 (逐次) の**時点のズレ**。合法な計画が時点ズレ
だけで落ちる = #42 で一般化した「最も筋の悪い却下」(回避学習の温床)。診断には spec 08-B の
機械タグ (items 差分) が初仕事 — T1 拾得→T2 クラフト消費→T14 語りだけの拾得 (items:[])
→T15/T16 の却下、を chronicle から完全再構成できた。
【解 (spec 09、ユーザー査読済み)】(A) **逐次射影裁定**: validate_ops が op を書かれた順に
検証し、受理した決定論 op を射影クローンへ仮適用してから次を検証 (裁定 = apply のドライラン。
適用ロジックは apply_deterministic_op を実 apply と共有し乖離を構造的に防ぐ)。ダイス op は
検証のみ・帰結非射影 → 判定依存の後続手は次ターン (物語的にも正しい)。(B) 既所持への
add_item は却下でなく **no-op 受理** (ItemAlreadyHeld 撤去。複製穴の守りは taken_items で
不変)。(C) prompt: GM_SYSTEM「ops は書いた順。段取りは束ねてよい/判定依存は次ターン」+
state_brief「この場でいま拾える」動的 surface (#37/#42 に続く現在形接地の第三例)。
【掟の改訂】原子性の核 (全か無か・嘘の状態は作れない) は不変。「開始時点の一括評価」だけを
「適用と同じ逐次評価」へ (2026-06-23 に勝ち筋と記録した「束ね却下」は、不正な束の遮断で
なく合法な束への摩擦だった)。ペーシングは authored 設計の責務 — **ダイスを置けば連鎖は
必ずターンで割れる**。ユーザー判断の根拠: 束ねはトークン経済 (1 ターン = フルプロンプト
1 往復。3 手を 3 ターンに割ると同じ進行に約 3 倍の入力を払う)。
【PoC】sequential_projection_allows_pick_then_use_in_one_delta /
order_matters_use_before_pick_is_rejected / duplicate_add_item_is_noop_when_already_held /
dice_outcomes_are_not_projected (gm_core 95→99) /
state_brief_surfaces_takeable_items_and_gm_system_grounds_op_order (harness)。
【一般化】**認可と実行の意味論は一致させる** — 認可が実行より粗い時点で裁くと、
「実行可能なのに認可されない」偽陰性が生まれ、提案者 (LLM) の回避学習を誘発する。
認可は実行のドライランであるべき。

### 44. 全入力が input_no_cache — OpenAI 互換層は prompt caching 非対応 → ネイティブ経路
【実測発見 (2026-07-11, Anthropic Console のコスト実測)】Claude 月 $143、Console の
usage_type が **全リクエスト input_no_cache** = prompt caching が一度も効いていなかった。
毎ターン messages を新規構築して送る設計 (state が唯一の真実) はフルプロンプト再送であり、
安定部分 (emit_delta schema + GM_SYSTEM + scenario_brief) が毎回、非キャッシュ価格で課金
されていた。
【真因】経路の問題でありプロンプト構造の問題ではない。llm_client は OpenAI 互換
`/chat/completions` を叩くが、**Anthropic の OpenAI 互換層は prompt caching 非対応**
(公式 docs 明記。cache_control は黙殺、`usage.prompt_tokens_details` も常に空) —
互換層経由ではキャッシュが**構造的に不可能**。互換層は「テスト・比較用で本番非推奨」
とも明記されている (#3 で schema 受理を確認して以来使い続けていた)。
【解】`LlmConfig.provider` (LLM_PROVIDER、未設定なら base_url に api.anthropic.com を
含む時 anthropic へ自動判定 = 配布受領者はゼロ設定) を新設し、Anthropic には
**ネイティブ Messages API** (`POST {base}/messages` + x-api-key + anthropic-version) を
話す。先頭 system 群を system ブロック配列へ写し **末尾ブロックに cache_control:
ephemeral を 1 個** — render 順は tools→system→messages なので、この 1 個で
schema+GM_SYSTEM+scenario_brief の安定プレフィックス全体がキャッシュ対象になる。
可変な user メッセージ (state_brief+chronicle+行動) には置かない (毎ターン別内容 →
読まれない書込 1.25× の無駄)。応答 tool_use は OpenAI 形 ResponseMessage へ写して
parse::extract に合流 (抽出・救済経路は単一のまま)。呼び出し側 (harness/app/CLI) は無改修。
【留意】①キャッシュ最小プレフィックスは claude-opus-4-8 で 4096 tokens — 満たさない
極小シナリオは黙って非キャッシュ (エラーにならない)。②TTL 5 分 (読取で更新) — プレイ中の
ターン間隔なら自然に持続。書込 1.25× は 2 リクエスト目で元が取れる。③system プロンプトに
可変値 (時刻・ID 等) を入れると全キャッシュが無効化する — 現構造は可変値を全て user 側に
置いており安全。将来 GM_SYSTEM/scenario_brief に手を入れる時もこの分離を守る。
④検証は `usage.cache_read_input_tokens > 0` (LLM_CACHE_DEBUG=1 で stderr に
`[LLM_CACHE] cache_read=...` を surface)。⑤ネイティブ経路は常に tool-use
(use_tools=false は無視 — Anthropic は tool_choice を確実に尊重する)。
【PoC】provider_autodetects_anthropic_from_base_url /
anthropic_request_places_cache_control_on_system_tail /
anthropic_response_resolves_tool_use_and_usage / anthropic_decode_keeps_raw_on_shape_mismatch /
anthropic_demotes_non_leading_system_to_user (llm_client 20→25)。
【一般化】**互換層は機能の交差集合しか持たない** — 抽象化 (base_url+api_key) で得た可搬性は、
プロバイダ固有のコスト最適化 (caching) を静かに捨てる。コストに効く機能はネイティブ経路の
分岐で取り戻し、検証は請求メタデータ (usage) を一次ソースにする (Console の実測が発見経路
だったように、機能が「効いているつもり」は usage でしか反証できない)。

### 45. Grok も実質キャッシュ無効 — xAI の自動キャッシュは「サーバ単位」で sticky routing が要る
【実測発見 (2026-07-11, #44 の続き。ユーザー観察「xAI も料金は似たようなものだった」)】
xAI の prompt caching は**自動** (cache_control 不要、chat/completions のままで効く) なので
#44 のような経路の破れは無い。しかし公式 docs 明記: **キャッシュエントリはサーバ単位**で
保持され、リクエストはロードバランサで分散される — `x-grok-conv-id` ヘッダ (会話ごとに
一貫した ID) を送らないと、同一プレフィックスでも別サーバに散って miss する。
Kataribe はこのヘッダを送っていなかった = 実質キャッシュ無効。
【解】`LlmClient.conv_id` (クライアント生成時に pid+nanos+カウンタで一意生成、uuid 依存
なし) を OpenAI 互換経路の全リクエストに `x-grok-conv-id` として送る。クライアントは
app=ゲームセッション毎 / CLI=実行毎に作られるので粒度が会話に一致する。xAI 以外の
サーバは未知ヘッダとして無視 (無害)。あわせて `ChatResponse.usage`
(`prompt_tokens_details.cached_tokens`) をパースし、LLM_CACHE_DEBUG=1 で
`[LLM_CACHE] cached=... prompt=...` を stderr に surface (ネイティブ経路 #44 と同形) —
OpenAI (自動・50% 引き) / Gemini 互換 (2.5 系は暗黙キャッシュ自動) もこの計測で見える。
【見送り】Gemini の `cached_content` (extra_body の明示キャッシュ) はキャッシュオブジェクト
の作成管理+保管料が要る重い機構で、暗黙キャッシュが自動で効く現行には不要。
【PoC】compat_usage_cached_tokens_parse / conv_id_is_unique_per_client_and_stable_within
(llm_client 25→27)。実 Grok プレイでの cached > 0 は実測待ち。
【一般化】「自動キャッシュ」も無条件ではない — 分散インフラでは **キャッシュの所在
(サーバローカル vs 共有) とルーティングの一致**が前提条件になる。プロバイダごとに
「キャッシュを効かせる作法」(Anthropic=cache_control / xAI=sticky ヘッダ / OpenAI=真に
自動) が違い、互換 API の同一ワイヤ形はこの差を隠す。検証はやはり usage が一次ソース。

### 46. 再起動するたび LLM 設定が別プロバイダに戻る — dev の repo .env が GUI 保存値を隠す
【実測発見 (2026-07-11, ユーザー報告「再起動すると毎回 gemini に戻っている気がする」)】
GUI で LLM 設定を保存しても、dev 起動 (`npm run tauri dev`) のたびに repo .env の
プロバイダへ巻き戻る。ユーザー仮説は「ブラウザ LocalStorage が効いている?」だったが
**LocalStorage は無実** (フロントが持つのは fontScale/lang 等の UI 設定のみ。LLM 設定は
get_llm_config/set_llm_config = プロセス env + app_data/.env 経由)。
【真因】.env の読み込み優先順位。①main.rs の `dotenvy::dotenv()` が cwd から親へ遡って
**repo 直下の .env を先に**読む (dev のみ) ②setup の `dotenvy::from_path` は**非 override**
なので、GUI 保存先 (app_data/.env) は既設定キーを上書きできない → repo .env が毎回勝つ。
GUI で保存し直した瞬間は set_var で効くため「使っている間は正しく、再起動で戻る」という
気づきにくい症状になる (発見が体感報告経由になったのも必然)。配布版は repo .env が無いので
無事 = **dev でしか再現しない系**。
【解】setup を `dotenvy::from_path_override` に変更 — GUI の保存値 = ユーザーの最後の
明示意思を唯一の真実にする。app_data/.env に無いキーは従来どおり repo .env が生きる
(CLI play は repo .env のままで不変)。
【一般化】**設定ソースが複数あるなら優先順位は「ユーザーの明示意思の新しさ」で決める** —
dotenvy の「先に読んだ者勝ち (非 override)」既定は、開発者向け .env とユーザー向け保存値が
同じキー空間を共有した瞬間に意思の逆転を起こす。体感症状「保存したのに戻る」は
永続化の失敗ではなく**読み込み順の敗北**を疑う。

### 47. 居ないキャラが喋り続ける — authored ビートが presence 矛盾を GM の確定記録にロンダリングする
【実測発見 (2026-07-11, ユーザー報告「あかりはいなくなるのに、移動先でも話してる」, friday_lemmon + Sonnet 5)】
前の場所に居たキャラ (あかり) が、presence 宣言に居ない移動先でも GM の語りに登場し続ける。
engine は無実 — `present_at` は正しくあかりを除外し、顔アイコン行にも出ていない。
【真因】シナリオのトリガー `hear_lemon_pie` が when: フラグ 2 つ (met_akari && met_genzo) で
**場所条件なし** → 典型プレイ順 (T1 商店街であかり → T2 喫茶店で源蔵) では met_genzo が立った
settle 連鎖の中で**喫茶店にいながら**発火し、authored narration「あかりが首をかしげる」が
居ない場所であかりに台詞をさせる。トリガー narration は作者権限の**信頼済み**テキストなので
検証されず、さらに「発火ビートの GM 還流」(2026-07-03) で carryover + chronicle に
「筋書きの出来事 (確定した記録)」として流れ込む。GM_SYSTEM の「presence 一覧が唯一の真実」
(抽象的な規律) と chronicle の「あかりがここで喋った」(具体的な確定事実) が衝突すると、
LLM は具体の側に従う — **接地漏れではなく、authored content が矛盾を注入し、信頼チャネルが
それをロンダリングした**言語チャネル系の新亜種。同シナリオの大団円トリガーも「3人を集めた」と
語りながら set_presence が無く、あかりを喫茶店へ連れてくる機構的経路自体が存在しなかった。
本シナリオは package_spec.md を渡して Meta AI に生成させた content — 仕様 md にこの規則が
無かったことが上流の真因 (LLM 生成 content は仕様に書かれていない不変条件を守れない)。
【解】content 側 2 点: hear_lemon_pie の when に location_is: shopping_street を追加 (あかりの
居る場所でだけ発火) / family_reunion_trigger の effects に set_presence akari true を追加
(大団円で正規に登場、spec 04 がまさにこの用途)。使い捨て統合テストで動線確認 (削除済)。
engine 改修なし。作者向け仕様 (outcast package_spec.md) の「作法」に規則を追記:
「トリガー narration に登場させるキャラは、location_is で場所を縛る / 全発火場所の present に
含める / set_presence で登場させる、のいずれかで在場を保証せよ」。
【副産物】同シナリオに Location 直下の `gate:` (存在しないフィールド) — serde は未知フィールドを
黙って無視するので**エラーにならず効かない** (しかも意図どおり効かせると合鍵が厨房内にあるため
デッドロックする配置だった)。死に行を除去し、package_spec.md に「静かな罠」として追記。
【一般化】**信頼チャネル (authored narration) は検証を免除される分、作者の矛盾をそのまま増幅する** —
presence のような engine 不変条件は、authored 文にも「書き方の規約」として届けないと
content 層から破られる。機械検証できない不変条件 (narration は自由文) の防衛線は
作者向け仕様の明文化 + LLM 生成時はその仕様を渡すこと。

### 48. 写真を見たのに日記が取れない — 手掛かり発見の二重経路 (進行フラグを立てない側を通ると詰む)
【実測発見 (2026-07-11, ユーザー報告「日記が写真を見たのに取れない」, friday_lemmon, opus-4-8)】
ユーザー仮説は「日本語照合がおかしい?」だったが**照合は無実** — found_old_photo は ASCII
フラグで Gate::FlagIs は単純な文字列 eq (spine.rs)、日本語 (日記/古い写真) も同一 YAML 内の
同一バイト列で照合される。真因は content の**手掛かり発見の二重経路**: 「古い写真」の発見が
(a) genzo_room の場所アイテム (gate: cafe_key_obtained) と (b) open_desk_drawer challenge
(on_success: found_old_photo) の 2 経路にあり、**進行フラグ found_old_photo を立てるのは
(b) だけ**。プレイヤーが (a) で写真を拾うと「写真を見た」体感なのに found_old_photo が false の
ままで、日記の gate (found_old_photo) が通らない。しかも (a) の写真アイテムは has_item で
どこからも参照されず、フラグも立てない**死んだ重複** (引き出しは鍵で固い設定なのに写真が
引き出し外で拾える矛盾も含む)。#47 と同じ Meta AI 生成 content の罠クラス。
【解】content のみ: genzo_room の items から「古い写真」を削除し、発見を open_desk_drawer
challenge に一本化 (challenge narration「引き出しから古い写真が出てきた」が発見を担い、
found_old_photo フラグが証拠を表す)。使い捨て PoC で Red (古い写真が場所アイテムで存在) →
Green (challenge 経由で found_old_photo → 日記が取れる) を実証、削除。engine 無改修。
【一般化】**下流 gate の前提になる進行フラグは、単一の発見経路で立てる**。同じ手掛かりを
複数経路 (アイテム拾得 vs challenge vs トリガー) で得られるようにすると、フラグを立てない
経路を通ったプレイヤーだけが下流で詰む — バグが「特定の遊び方でだけ」出る発見しにくい形になる。
LLM 生成 content は「同じ物を二箇所に置く」冗長を作りやすい (items と challenge の両方に写真)。
体感報告「取れない」は照合バグより先に**フラグを立てる経路と gate の突き合わせ**を疑う。

## crates/harness (2026-07-11 実プレイ報告 — 移動すると居ないキャラがついてくる)

### 49. 同行の既成事実化 — GM 自身の移動語りが presence を汚染する (#47 の自己汚染版)
【実測発見 (2026-07-11, ユーザー報告, Sonnet 5)】主人公が移動すると、移動先の present に居ない
NPC が居ることになって語られ続ける。真因の連鎖: 移動ターンの語りに GM が「一緒に歩き出す」等の
**同行の素振り** (非検証 narration、冗長なモデルほど書きがち) を含める → それが recent_narration +
chronicle に確定事実として乗る → 次ターンの presence 一覧 (「一覧が唯一の真実」= 一般規律) より
具体的な語りの方が強く、GM は居ないキャラを居るものとして続ける。#47 と同じ「具体 > 抽象」の
ロンダリングだが、汚染源が authored トリガーでなく **GM 自身の語り** — content 修正では防げない。
【解 (二層・prompt 層のみ・engine 不変)】(a) 汚染源の抑制: GM_SYSTEM の presence 規律に
「主人公が移動しても NPC は勝手についてこない — 移動を語るとき同行の素振りを書くな」を追加。
(b) 汚染後の上書き: `prompt::moved_note` — 直前ターンで場所が変わっていたら、置いていかれた
NPC を**固有名で「ついてきていない」**と否定接地する (#37 の接地強度: 一般義務 < 現在形の事実+固有名)。
移動検知は **history 末尾 2 件の location 差** — `TurnLog.location` は適用後の現在地なので、
移動ターン自身のログは既に新しい場所を指し、state と比べても差は出ない (設計の要注意点)。
置き去りの算出は前ターンの `TurnLog.present` (spec 08-B の機械タグ) − 現在の `present_at` =
engine 事実だけで決まり LLM 非関与。旧セーブ (タグ無し) と履歴 1 件以下は黙って沈黙 (誤発火なし)。
【一般化】**非検証チャネルの自己汚染は「書くなという規律」と「書かれた後を事実で上書きする還流」の
二層で塞ぐ**。LLM 自身の過去出力は最も信頼される文脈になるため、状態遷移 (移動・退場・死亡) の
直後 1 ターンは、遷移が「起こさなかったこと」を固有名で明示する否定接地が要る。
PoC: `moved_note_grounds_left_behind_npcs_with_names` (Red→Green)。

## crates/gm_core (2026-07-13 実プレイ報告 — 画面はフラグ true なのに「true が必要」で move 却下)

### 50. 専権フラグの engine バックストップ欠落 — LLM が set_flag false で射影を自壊させる
【実測発見 (2026-07-13, 1ldk, ユーザーのバグ調査プレイ)】オフィスから「家に帰る」の 1 回目が毎回
「必要: 会社で仕事をする=true」で却下されるが、画面のフラグ欄では同フラグはずっと true。
GM 自身も <meta:> で「自分は立てられないはず・二重化?」と迷走した。真因: **authored 専権フラグ
(トリガー/challenge の帰結が書くフラグ) の除外は prompt の語彙提示だけで、engine の却下条件に
入っていなかった**。しかも set_flag の flag_rules gate 検査は value=true 限定なので、
**value=false は allowed_flags にさえあれば素通り受理**。GM が「退勤」の意味論で
[set_flag 会社で仕事をする=false, move apartment] を束ねると、set_flag が逐次射影に乗って
射影内の会社フラグを false に倒し、直後の move gate (会社==true) を**自分で壊す** —
false は却下された試行の射影の中にだけ存在するので、画面 (実 state) は true のまま。
却下理由には move の分しか載らない (set_flag は「合法」だった) ため診断が迷路化した。
単独提案 [set_flag 専権フラグ=false] なら**受理されて実 state を汚せた** (authored 機構の静かな妨害)。
【解】adjudicate の SetFlag 検証に authored_only_flags 検査を追加 — 専権フラグへの LLM set_flag は
**true にも false にも**倒せない (FlagNotAllowed で却下・usable 語彙を還流)。トリガー効果は
apply_ops 直行で従来どおり書ける (grant_skill/set_attribute の op 専権と同じ二層になった)。
却下 op は射影に乗らないので move は無傷で評価され、却下理由が真因を名指しする。
【一般化】**閉世界の防衛線は「見せない (prompt)」と「通さない (engine)」の二層で初めて閉じる** —
語彙から隠しても LLM は自然な意味論 (退勤=フラグ解除) から専権領域に踏み込む。また
**gate 検査を value=true に限定すると、false 方向が認可の死角になる** (真化だけ守っても
偽化で壊せる)。PoC: llm_cannot_set_authored_only_flags_either_direction (Red→Green、
self-sabotage の再現 = 却下理由から MoveGateUnmet が消え FlagNotAllowed が名指しされる)。

## crates/gm_core (2026-07-14 ユーザー報告 — 「tier に入っているフラグは専権にならない気がする」)

### 51. challenge の effects 内 set_flag が専権走査から漏れる — 新経路が古い監査に映らない
【ユーザー報告 (2026-07-14)】「tier に入っているフラグが専権フラグにならない気がする」。
コード確認で確定: `authored_only_flags()` は trigger の effects と challenge 帰結の **`.flag` 欄**
しか走査しておらず、**`on_success`/`on_failure`/`tiers` の `effects` に書いた `set_flag` の
書き先が漏れていた**。effects は 2026-07-03 に足した「フラグ+トリガーの 2 点セット無しで
直接動かす」経路 — つまり `flag:` 欄で書く旧流儀の作者は守られ、推奨の新流儀 (effects) で
書いた作者だけが穴に落ちる非対称。帰結は二層とも開く: ①そのフラグが GM の usable 語彙に
出る (prompt が先取りを誘う) ②#50 の engine バックストップが素通り (LLM の set_flag が
true/false とも受理され、authored 機構を静かに妨害できる)。
【解】`authored_only_flags` の走査を「書くフラグの全経路」に拡張 — challenge 帰結の
`.flag` 欄に加えて `outcome.effects` / `tier.effects` 内の `SetFlag` op も拾う
(trigger effects と同じ collect)。usable 語彙・FlagNotAllowed の却下・FlagHintOnAuthoredOnly
lint が全て自動で追従する (単一の走査関数に集約されていたのが幸い)。
【一般化】**「X を書く全経路」を列挙する監査関数は、新しい書き込み経路を足した時に必ず
再訪する** — 経路の追加 (challenge effects) と監査の追加 (authored_only_flags は同日 2026-07-03
に別 spec で入った) が別作業だと、監査は「当時あった経路」しか知らない。書き込み経路を
増やす変更のチェックリストに「専権走査・validate・lint は新経路を舐めるか」を含める。
体感報告 (「気がする」) が今回も実在した — n をまた 1 つ積んだ。
PoC: challenge_effects_setflags_are_authored_only (Red→Green、usable 除外 + #50 却下の二層を固定)。

## crates/llm_client (2026-07-15 Gemini ネイティブ live 検証 — spec 12 Phase E)

### 52. Gemini functionDeclarations は oneOf を黙って落とす — 制約が消えて ops:[1,2,3] を捏造
【実測 (2026-07-15, gemini ネイティブ経路の初回通しプレイ)】GM 応答の narration/summary は
正常なのに `"ops": [1, 2, 3]` (整数列!) が返り、StateOp のパース失敗で提案者エラー。
raw 保持 (#34) が真因を即座に見せた — schema の `ops.items.oneOf` が**効いていない**。
【真因】Gemini の functionDeclarations `parameters` は OpenAPI 3.0 系の Schema サブセットで、
`oneOf` を**400 にせず黙って無視する**。schemars が StateOp (内部タグ enum) に出す oneOf が
落ち、ops は「無制約の配列」として grammar コンパイルされた → モデルが整数列を捏造。
Grok の $ref 非解決 (#31 期の Grok 対応①) と同族の「grammar コンパイラが schema を部分適用
する」系だが、**エラーすら出ない分こちらが静かに悪い** (Anthropic は全部解決 / Grok は $ref
だけ非対応 / Gemini は oneOf も非対応、と対応表が三者三様 = 互換の交差集合は想像より狭い)。
【解 (Phase C.5a)】`gemini::adapt_schema` — oneOf を `anyOf` へ付け替える (Gemini の Schema は
anyOf を対応する)。バリアント毎の required/properties 制約を保ったまま Gemini の grammar に
乗る。inline (自己完結化) は共通処理で済んでいたので付け替えだけで自己完結が保たれる。
効かなければ次段 C.5b = 全バリアント統合の単一 object 化 (op を enum で縛り、全フィールドを
optional に畳む — engine の adjudicate が意味論を守るので schema は「形」だけで足りる)。
【一般化】**「schema を受理した」は「schema を強制する」を意味しない** — 拒否 (400) は
まだ親切で、静かな部分適用が最悪。プロバイダ毎に「どの JSON Schema キーが grammar に
乗るか」の対応表を検証項目にせよ (受理テストでなく**出力形のテスト**でしか検出できない)。
PoC: gemini_encode_maps_system_tools_and_roles に oneOf 不在 + anyOf 存在 + バリアント実体
保持の assert を追加。
【live 検証 Green (2026-07-15 再測, `gemini-flash-latest`=実体 gemini-3.5-flash)】friday_lemmon
5 ターン通しで**整数列 ops ゼロ / 全 op が正しい StateOp オブジェクト (move・add_item・
set_flag・attempt_challenge の 4 バリアントで op 判別子+必須フィールドを構築) / attempt_challenge
のダイス経路も端から端まで**。C.5a で確定、C.5b (全バリアント統合) は不要。留保: n=1 モデルの
一発通し (passthrough に戻す gold-standard A/B は未実施だが oneOf→anyOf のピンポイント差分ゆえ
確度高)。副次: 「latest」エイリアスが 3.5-flash に進んだ (docs は 2.5 前提・エイリアス依存に注意)。

## 方法論 (2026-07-15 spec 12 Phase E — Grok GM 挙動の観察)

### 53. n=1 の劇的な LLM 失敗を systematic と誤認する — Grok の「拒否癖」も「cast_vote latch」も溶けた
【観察 (2026-07-15, grok-4.3 tool-use, friday_lemmon)】1 回の通しプレイで grok-4.3 が
(a) 移動要求「厨房へ入る」に源蔵の拒否を narrate し move op を出さない (b) 非投票シナリオに
`cast_vote` を ×5 乱発 (GM_SYSTEM の明示禁止「投票の節が無ければ cast_vote を提案するな」を
無視)。ここから 2 つの仮説 — **①grok は simulationist で「書かれた世界」を守りすぎる**
(ユーザー観察) / **②巨大 GM_SYSTEM の投票ブロックに latch する** (私の観察) — を立てた。
【棄却】failures に書く前に n を増やす (ユーザー判断「正解はまだわからない」)。benign 台本
再実行 + 純移動 (move-only) 台本 + Gemini 対照で **n=4**: ①も②も**再現せず溶けた** —
move-only で **5/5 移動 honor・却下ゼロ**、benign 再実行で café 進入も成功、cast_vote は
4 run 中 **1 run のみ** (確率的一発)。両「劇的失敗」は systematic な癖でなく run-to-run 分散。
【再現したもの】唯一 2/2 で再現したのは「その場に居る人物に話しかける」で**冗長な自 location
への move** を出す小さな quirk (self-repair で回復・無害)。
【真因 (モデル特性、欠陥ではない)】grok-4.3 は Gemini より **op 提案が noisy** — spurious op
(冗長 move / まれに cast_vote / authored set_flag) を出す確率が高く self-repair 往復が増える。
Gemini は同台本で却下ゼロ。だが **engine が全 spurious op を backstop し state 汚染ゼロ・
outcome 到達** = 「正本 > 文章力」は騒がしい提案者の下でも保たれる (#50 の authored-flag
backstop・cast_vote guard・self-exit 拒否が全部効いた = アーキテクチャの positive 検証)。
壊れているのは提案の質であって正本ではない。コストは self-repair の往復増 (レイテンシ/トークン)。
【一般化】**LLM-GM 挙動を failures/spec に「systematic」と書く前に n≥3 で観察する** — LLM は
run-to-run 分散が大きく、n=1 の劇的失敗は systematic な欠陥と誤認しやすい (この session で
ユーザーと私が別々に踏んだ)。同型先例 = キャッシュ warmup を cap と誤認した 2 ターン計測 (#45 の
再測で判明)。**モデル選択の但し書き**: レイテンシ/トークン重視の配布なら Gemini/Claude が
提案クリーン、grok-4.3 は noisy だが outcome は到達 (engine が守る)。PoC 無し (コード修正でなく
観察方法の罠)。

## crates/llm_client (2026-07-15 Gemini 暗黙キャッシュ調査 — 大パッケージ sun_girl_ntr で cache 不発)

### 54. Gemini 3.5-flash の暗黙キャッシュは総プロンプト ~8000 トークンで崖 — 大シナリオ/長セッションで不発
【観察 (2026-07-15, ユーザー報告「Gemini だとキャッシュの効かないパッケージがある」)】
sun_girl_ntr (scenario 637行/27KB) を gemini-flash-latest で回すと turn1=cached 4073 →
**turn2 以降 cached=0**。friday_lemmon は cached 4071 で安定するのに逆挙動。
【診断 (制御実験で因果を単離)】まず systemInstruction 長をターン別計測 → **毎ターン完全に
同一 (sun_girl 9855文字 / friday 7876文字、ターン間で不変)** = Kataribe の可変値混入は棄却
(#44 の「可変値は user 側」は Gemini でも保たれている)。次に friday の `world` を段階
パディングして **systemInstruction サイズだけ**を変える sweep: 7876/9332 は cached ~4074 で効き、
**10340 で cached=0 に落ちる崖**。さらに sysInstr を 7876 に固定して **user メッセージだけ**を
肥大化 → total prompt が ~8700 で **cached=0** (sysInstr は効くサイズのまま)。両実験が
**同じ total-prompt 閾値 (~8000-8400 tokens) で崩れる** = 因果変数は systemInstruction でも
package 内容でもなく **総プロンプトサイズ**。
【真因】gemini-flash-latest (=実体 gemini-3.5-flash) の**暗黙キャッシュは総プロンプト ~8000
トークンを超えると停止する** (以下なら ~4074=GM_SYSTEM prefix のみ cache、超えると 0)。
best-effort ゆえ未文書。gemini-2.5-flash での対照は **404「新規ユーザー提供終了」で不能** =
2.5 に戻る逃げ道なし、崖は現行 flash の性質。棄却: Kataribe バグ / sun_girl 固有内容
(同サイズ friday が同一に崩れる) / TTL・タイミング (同一 package) / sysInstr サイズ説 (user 側でも崖)。
【波及の再評価】崖が total-prompt ~8000 なら **1パッケージの問題ではない** — どの package でも
長セッションで chronicle/あらすじ/memoria が積もれば ~8000 を超え cache が切れる。sun_girl は
brief が大きく turn1 から超えるだけで、friday も 30 ターン回せば同じ崖。さらに効いても ~4074
(GM_SYSTEM のみ) で scenario_brief は元から未 cache。**Gemini 暗黙キャッシュは大シナリオ/
長セッション全般で不発**。
【解の方向】total-prompt サイズが原因ゆえ brief を user 側へ移す小細工は効かない (total 不変)。
**Gemini 明示キャッシュ (`cachedContent`) が唯一の robust な解** — 静的プレフィックスを明示 pin
すれば暗黙の ~8000 崖に無関係にヒットする。**spec 12 の「2.5 系は暗黙自動ゆえ明示 cached_content
不要」は 3.5-flash で falsify された**。→ **spec 13 で起票・Phase A+B 実装済** (encode 分割 +
fingerprint + cachedContents create/参照/lifecycle、client 内に閉じ fallback 徹底)、Phase D の
実キー live 検証 (>8000 でヒット・alias 対応・最小トークン) が残。
【一般化】**「schema を受理した」同様「暗黙キャッシュが効く」もサイズ次第で静かに破れる** —
best-effort 機構は閾値を跨ぐと無言で 0 になる。usage を一次ソースに実測せよ (#44/#45 の系)。
留保: 閾値 ~8000 は近似 (prompt 7937 効き→8420 不発で挟んだ)、n=1 モデル・n=1 アカウント
(best-effort ゆえ region/account 差の可能性は残る)。PoC 無し (Gemini 側の性質の観測)。

### 55. Gemini 明示キャッシュ参照時は system_instruction/tools/tool_config を request に載せると 400
【観察 (2026-07-15, spec 13 Phase D の初回 live)】cachedContent を参照する generateContent が
`400 INVALID_ARGUMENT: "CachedContent can not be used with GenerateContent request setting
system_instruction, tools or tool_config. Proposed fix: move those values to CachedContent"`。
【真因】明示キャッシュ実装で systemInstruction と tools は request から省いたが、**tool_config
(functionCallingConfig mode:ANY) を request 側に残した** — spec 13 D1 の設計判断「強制指定は
per-request ゆえ残す」が誤り。Gemini は cachedContent 参照時に**これら 3 つのいずれか**が request に
あると 400 にする (cache と request で二重定義になるため)。ツール宣言と強制指定を分離できると
思い込んだのが罠 (OpenAI 互換の直観を Gemini に持ち込んだ)。
【解】tool_config も cache 側 (CreateCacheRequest) へ移し、cache 参照時の generateContent からは
systemInstruction/tools/tool_config を**すべて省く** (`encode_with_cache(Some)` で 3 つとも None)。
Kataribe は常に emit_delta を強制するので mode ANY は cache 単位で一定 = cache に載せて問題なし。
【一般化】**明示キャッシュは「静的なもの全部」を cache に入れ、参照側 request には可変分だけ
残す — 部分的に残すと 400**。プロバイダの直観 (OpenAI では tool_choice は per-request) を別
プロバイダに転用しない。**この罠は unit test では出ず live でのみ露見** — Phase D (実キー) を
分けておいた設計が効いた (最初の 1 実行で捕捉→修正)。PoC: `gemini_encode_with_cache_omits_prefix_
and_references_cache` (cache 参照時 toolConfig も None) / `gemini_build_create_request_extracts_
static_prefix` (toolConfig は cache 側) を修正後の分割に更新。修正後 live で 8000 超の全ターン
cachedContentTokenCount=7173 を確認 (崖を迂回、spec 13 目標達成)。

### 56. WebView2 の window.confirm/alert は本文に tauri://localhost を混ぜて表示する (app)
【観察 (2026-07-16, spec 07 Phase D セーブスロットの上書き確認で発覚)】Tauri2 (Windows=WebView2) で
`window.confirm("スロット1を上書きしますか？")` を出すと、ダイアログ本文の先頭に `tauri://localhost`
などのオリジン文字列が勝手に前置され、素人臭く・不審に見える。開発者が仕込んだ文字列ではない。
【真因】WebView2 (Edge/Chromium) は `window.confirm/alert/prompt` を「このサイトからのメッセージ」
として扱い、**呼び出し元オリジンをブラウザが自動で本文に付与する** (フィッシング対策の標準挙動)。
Tauri の内部オリジンが `tauri://localhost` なのでそれが露出する。JS 側からは抑制できない
(ブラウザネイティブ UI ゆえ CSS/文言の制御外)。
【解】`window.confirm/alert/prompt` を app 内で全廃し、**Promise ベースの自前モーダル (ConfirmDialog)**
に置換。store が `askConfirm(message, okLabel?) -> Promise<boolean>` を持ち (resolver はモジュール
ローカルで保持、Pinia state に関数を入れない)、ダイアログの OK/キャンセルで解決する。テーマにも
揃い、Enter=決定 / Esc・幕クリック=取消 まで作り込める。grep で `window.(confirm|alert|prompt)` の
ゼロを担保 (回帰の砦)。
【一般化】**ネイティブブラウザ UI (confirm/alert/prompt/beforeunload の離脱確認等) は WebView 埋め込み
アプリでは「オリジン露出・文言非制御・テーマ不一致」を必ず伴う — デスクトップ殻では最初から自前
モーダルにする**。「標準 API で十分」は Web の直観で、埋め込みアプリには持ち込まない (#55 の
「プロバイダの直観を転用しない」の UI 版)。

### 57. xAI 自動キャッシュの miss は sticky ヘッダを送っていても確率的に出る (間隔非依存の分散/eviction) — cached=128 は「miss のフロア表示」 (llm_client)
【観察 (2026-07-16, Grok GUI 人間ペース実プレイ 2 回 + `[LLM_CACHE_STAT] t=` の間隔計測 40 リクエスト)】
`x-grok-conv-id` (#45) を全リクエストに送っていても、`cached` が「静的プレフィックス全量」(6144〜6656)
と「128」の二値で交替した。128 は上限でなく **miss の最小ブロック表示** (xAI は 128 トークンブロック
粒度、部分ヒット 4096 も観測)。spec 12 Phase E の「turn3 から ~88% 安定」は台本駆動の高速ループ
(間隔=秒) 限定の観測だった。
【仮説の弁別 (t= 付き第二測定)】miss の直前間隔 = 29〜68 秒 / hit の直前間隔 = 25〜118 秒 (平均 59 秒)
— **miss は間隔と無相関 → TTL/eviction (時間切れ) 説は棄却**。残る説明は LB の best-effort 分散
(sticky ヘッダでも時々コールドな個体に当たる) ないし負荷による確率的 eviction。定常 miss 率 ~11%
(4/35)、warm-up は 3 リクエスト+部分ヒット 1 (2 ターンではない)。セッション累積 hit rate 60.6%。
【真因】サーバ側の挙動でこちらのレバーではない。sticky routing (#45) は「実質無効 → 概ね効く」に
上げる装置であって保証ではない。
【解 (対処でなく理解)】(a) Grok の実効 hit rate は人間ペースで ~60% を上限見積もりとする (台本の
88-89% を配布時の期待値にしない)。(b) 「キャッシュが壊れた?」報告を受けたら、まず miss が間隔・
warm-up・確率分散のどれかを `[LLM_CACHE_STAT] t=` の系列で見る (単発 miss は正常)。(c) 対照的に
Anthropic cache_control は TTL 5 分が読取で更新される決定論的挙動 — プロバイダ比較の基準側に使う。
【一般化】**自動 (implicit) キャッシュは「効く/効かない」の二値でなく確率分布 — 単発の miss を故障と
誤診しない。診断はタイムスタンプ付き系列で「間隔相関の有無」を先に見る** (時間依存なら TTL、
非依存なら分散)。#53 (n≥3 で観察) の計測版: 1 リクエストの usage は挙動の証言にならない。
【副産物 (spec 14 の live Green)】同測定で synopsis 二段キャッシュの狙いが Grok で実証: 章圧縮の
たびに `cached` が 6144→6400→6656 と +256/章 の階段で成長 (synopsis が自動プレフィックスキャッシュ
に乗った)、章追加直後は旧プレフィックス分だけヒット→1〜2 リクエストで新全量に回復 (D3 の予測どおりの
dip 形状)。圧縮後は prompt 自体も 9024→8436 に縮む (spec 10 の注入経済)。

### 58. usage の「input」はプロバイダで意味が違う — Anthropic native の input_tokens は非キャッシュ分のみ (llm_client)
【観察 (2026-07-16, spec 14 Phase D の Anthropic 対照実測 53 リクエスト)】`[LLM_CACHE_STAT]` の
ratio (cached/prompt) が **6.88** など 1 を大きく超えた。cache_read=8978 に対し input=1305 —
分母が分子より小さい。
【真因】usage の「入力トークン」フィールドの意味論がプロバイダで違う: **Anthropic native の
`input_tokens` は非キャッシュ分のみ** (総入力 = input_tokens + cache_read_input_tokens +
cache_creation_input_tokens)。OpenAI 互換の `prompt_tokens` / Gemini の `promptTokenCount` は
**総入力** (cached はその部分集合)。canonical `Usage.prompt` へ素通しで写すと、Anthropic だけ
分母が欠けて hit rate が壊れる。
【解】anthropic::decode で `prompt = input + cache_read + cache_creation` に**正規化**
(正規化はプロバイダ差を知る adapter の責務 — 観測層 CacheStat は canonical しか見ない)。
PoC: `anthropic_response_resolves_tool_use_and_usage` に prompt=総入力 の assert (Red→Green)。
【一般化】**同名/同格のメトリクスでもプロバイダ間で包含関係が違う — 正規化してから比較する。
「率が 1 を超える」のような不変条件違反は定義バグの一次シグナル** (#44 の「usage が一次ソース」の
系: 一次ソース自体の意味論も検証対象)。#52 (schema 受理≠強制)・#55 (プロバイダの直観を転用しない)
と同族の「プロバイダ方言」罠の usage 版。
【副産物 (spec 14 の Anthropic live Green)】同測定で多段 breakpoint を実証: 52 通常ターンで
**miss ゼロ** (Grok の確率的 ~11% と対照的な決定論挙動、260 秒間隔でもヒット=TTL 5 分読取更新)、
章圧縮のたび cache_read が 8978→9359→9650→9978 の階段 (synopsis 第二段が読まれた)。章追加ターンは
静的分 8978 だけ読み + **synopsis ブロック全体を書き直し** (write=381/672/1000 — breakpoint 単位の
exact 一致ゆえ Grok のブロック粒度と違い延伸差分でなく全体再書込。予算 2000 字で有界=無視できる)。
正規化後の累積 hit rate **71.0%**。

### 59. Gemini cachedContent の TTL は読取で更新されない (作成時刻から固定) — Anthropic と真逆 (llm_client)
【観察 (2026-07-16〜17, spec 14 Phase D の Gemini 対照実測 25 リクエスト)】アクティブにプレイ中
(直前リクエストから 56 秒) なのに cachedContent 参照が失効 (403/404) した。検算: cache 作成から
req18 (age ~900s) はヒット、req19 (age ~953-1009s) で失効 — **TTL 900s ちょうどの境界**。もう 1 件の
失効は 73 分の中断後 (これは自明)。
【真因】**Gemini cachedContent の TTL は作成時刻から固定で、読取アクセスでは更新されない**。
Anthropic cache_control (TTL 5 分・読取で更新 = 使い続ける限り生きる) と真逆の方言。つまり連続
プレイでも **~15 分毎に必ず 1 回、失効→フォールバック→再 pin のサイクル**が入る (故障ではなく周期)。
【解 (現状で正しく動く)】spec 13 の fallback 徹底が設計どおり吸収: 失効ターンは handle クリア +
full request 再試行で**ターンは落ちず**、暗黙キャッシュが静的分の ~72% (4074/5630) を拾い、直後に
再 pin して次ターンから explicit 復帰。累積 hit rate 72.6% (失効 2 回込み)。
【改善候補 (未実施)】`CacheHandle.created_at` を持ち `decide_cache_action` が age >= ttl−margin で
**先回り Create** (失効 403 のラウンドトリップ 1 回を節約)。または cachedContents の PATCH で ttl
延長。周期コストは小さいので優先度低。
【一般化】**「キャッシュの寿命」もプロバイダ方言 — 固定 TTL か読取更新かを対で確認する** (#55/#58
と同族)。診断上は「~15 分周期の単発 miss + 直後の再作成ログ」= Gemini の正常な鼓動であり故障と
誤診しない。

### 60. synopsis の system role 昇格が Grok に過強調を起こす — 権威を上げる変更は salience の副作用を持つ (harness)
【観察 (2026-07-18, spec 14 未決2 の behavior 明示検証・ユーザー実プレイ)】20T 超の章圧縮後、
あらすじにしか残っていない章の出来事をプローブ: **Anthropic = 整合+良好な想起 (role 化の真の陽性)** /
**Gemini = 良好 (ただし D4 で user 降格ゆえ role 化の証拠ではなく spec 10 機構自体の検証)** /
**Grok = 想起は整合するが、求められていないターンでも過去章を語りに織り込みすぎる過強調**。
spec 10 Phase E と spec 14 未決2 はこれで閉じた (退路 D2 拡張は不要)。
【真因 (仮説)】synopsis を独立 leading system へ昇格 (spec 14 Phase B) したことで権威が上がり、
Grok は system の内容を「毎ターン能動的に使うべき指示」として扱う。従来の規律は「矛盾するな」
「再演するな」の 2 つだけで、**自発的な過剰言及**という第三のチャネルを縛っていない — 禁じられて
いない方から同じ素材が流れ出る (#33「言及の禁止は描写を縛らない」と同型の非対称)。
【解 (prompt 層のみ・engine 不変)】`synopsis_note` ヘッダに salience 規律を**対で**追加:
抑止「あらすじは背景であって主題ではない — 求められない限り過去の章を自発的に回想・引用しない」+
保護「プレイヤーが過去を尋ねたときは、あらすじを正確に参照して答える」。抑止だけ書くと検証した
ばかりの想起 (プローブ応答) を殺すため、対が必須 (#41「抑止したいのは再生成か認知かを区別する」の適用)。
PoC: `synopsis_note_suppresses_spontaneous_reminiscence_but_protects_recall` (Red→Green)。
【一般化】**注入テキストの role/位置を昇格させる最適化は、内容の salience を静かに変える —
キャッシュ最適化の behavior 検証は「読めているか」だけでなく「読みすぎていないか」も対で見る**。
禁止規律は列挙した違反しか縛らない: 矛盾・再演・過剰言及は別チャネル。

### 61. Gemini のブロックは「200 + 空応答」— decode が理由を捨てると恒久失敗が診断不能になる (llm_client)
【観察 (2026-07-18, ユーザー実プレイ)】あらすじ要約 (summarizer=Gemini) が「LLM が空の応答を
返した」で失敗し続け、あらすじが作られないまま。コンソールの警告だけでは何が悪いのか分からない。
【真因 (構造)】Gemini は安全フィルタ/規約で弾いた場合も **HTTP 200 + 空応答**で返す。ブロックの
理由は二段にある — ①プロンプト段 `promptFeedback.blockReason` (candidates 自体が無い) /
②候補段 `finishReason` (SAFETY/RECITATION/PROHIBITED_CONTENT 等、本文ゼロ)。従来 decode は
①を **deserialize すらせず**、②を Other に潰していたため、全ブロックが一律 EmptyResponse に
縮退していた。しかも EmptyResponse は一過性 (再抽選対象) 扱い — ブロックは内容起因で決定論なので
リトライは無駄撃ち、遷移契機の凍結リトライ (同一範囲・拡張禁止) と組むと**恒久失敗ループ**になる。
【解】`gemini::block_reason` (純関数・二段判定、本文/functionCall が在る応答と STOP/MAX_TOKENS は
対象外=Phase D 再抽選の管轄と混ぜない) + `LlmError::Blocked{reason}` (**非一過性**) を新設、
`gemini_complete` が decode 前に弾く。synopsis-failed トースト (2026-07-18 同日実装) に理由が
そのまま載る = リリースビルドでも「SAFETY で弾かれている」まで見える。PoC:
`gemini_block_reason_surfaces_safety_and_prompt_feedback` (Red→Green)。
【一般化】**「エラーは status code で来る」という直観はプロバイダ方言 — ブロックを 200 で返す
プロバイダでは、空応答の内訳 (素の空 / 思考使い切り / ブロック) を分類してからエラー型に落とす。
分類を潰すと「回復しうる失敗 (再抽選)」と「回復しない失敗 (内容起因)」の区別が消え、リトライ設計が
壊れる** (#52/#55/#58/#59 と同族のプロバイダ方言 + #58 の「一次ソースの意味論も検証対象」の応答版)。
【残課題】ブロックが恒久なら同一範囲の凍結リトライは永遠に失敗する — K 回失敗で mechanical_join
(LLM フリー) へフォールバックし「あらすじが必ず作られる」を構造保証する案は未実装 (起票候補)。

### 62. 即興 check_under の再挑戦スパム — 様式の接地は「振り方」だけでなく「振ってよい頻度」も縛る (harness)
【観察 (2026-07-20, spec 16 実測・湖畔の洋館)】GM が同一対象への目星判定を同一手段のまま
3 連続で受理して振った (80→85→98、全て失敗)。CoC7 RAW では同条件の再ロールは不可で、
再挑戦の正規経路は代償つきプッシュのみ。spec 18-B のプッシュは authored challenge 専用のため、
LLM 即興の check_under には再挑戦を縛る規律がどの層にも存在しなかった。失敗にコストが無いので
プレイヤーは成功まで振り直せてしまう = 判定の意味が消える (様式は守られているのに作法が破れる)。
【真因】「## 判定様式」節は check_under の**読み方** (ロールアンダー・DC 発明禁止) だけを接地し、
**頻度の作法** (いつ振り直してよいか) を接地していなかった。権利の提示は頻度の規律を含意しない
(#32「権利≠義務」の頻度版 — 権利・義務・頻度は独立に接地が要る三軸)。
【解 (prompt 層のみ・engine 不変)】判定様式節に**抑止+保護の対** (#41/#60 の型) で追加:
抑止「失敗した判定を同じ状況・同じやり方のまま振り直させてはならない — 同じ試みの繰り返しには
判定を出さず『結果は変わらない』と語って接地」+ 保護「別の技能・別の手段・状況の変化がある時だけ
再判定」。authored challenge の再挑戦 (SAN 消耗ループ) は SAN 減少 = 目標値変化 = 状況の変化に
該当し、規律と共存する (A 修正のループを殺さない整合を設計時に確認)。PoC:
scenario_brief_grounds_percentile_style に assert 追加 (Red→Green)。**対照検証 (n=1)**:
規律前の周は同一目星 3 連発 / 規律後の周は失敗→「別の切り口を探るか」と振らずに接地→
新手段 (聞き耳) には新規判定 = 狙った挙動差分が正確に出た。
【一般化】**ルールシステムの移植は「判定式」の移植だけでは完了しない — 原典が別機構 (プッシュ等)
で守っている作法は、その機構を持たない層では明示の規律として補填する**。engine 側の本丸
(check_under へのプッシュ拡張) は CoC7 v2 で検討。

### 63. 再試行の原因は二種あるのに、片方 (パース失敗) の理由を捨てていた (harness/app)
【観察 (2026-07-21, ユーザー実プレイ)】GUI に「GM は 2 回目の提案で筋を通した。却下された試行:」
と出るのに、**理由が空欄**。何が起きたのか author に分からない。
【真因 (構造)】受理までのやり直しには**二種**ある: ①裁定で却下 (`Verdict::Reject`) ②**構造化出力が
壊れて読めなかった** (`LlmError::Parse` — 裁定に到達していない)。run_turn は ①だけを
`rejected` に積み、②は `continue` するだけで理由を捨てていた (raw は self-repair の messages へ
積むが提示層へは出ない)。一方 frontend は `attempts > 1` で無条件にブロックを出すので、
②のときは**見出しだけ出て中身が空**になる。しかも見出しの語 (「却下された試行」) が誤り —
却下されていない。②の典型は **tool-use 引数の型崩れ** (#64。実測は Claude ネイティブ経路 = 常に tool-use 強制なので
「散文で断った」ではなく構造化出力そのものの形の揺れ) / CoT 混入 (#30) / max_tokens 切断 /
ops の型崩れ (#52) / no-tools 経路なら散文の混入。なお `PROHIBITED_CONTENT` 等の**ブロックは別物**
(`LlmError::Blocked` = 非一過性でターンごと失敗・#61)、2 回目で成功するケースはこれではない。
【解】`RetryCause` enum (`Rejected(Vec<RejectReason>)` / `Malformed { detail }`) を新設し、
`TurnOutcome::Accepted.rejected` → `retries: Vec<RetryCause>` へ。パース失敗も原因として運び、
app が localize (「出力が壊れて読めなかった (再提出させた): …」)、CLI も分けて print。
見出しは「却下された試行」→「やり直した理由」(中立語) へ。PoC:
`malformed_output_is_carried_as_a_retry_cause_not_dropped` (Red→Green)。
【一般化】**カウンタと理由リストは同じ出来事を数えているか確認する** — `attempts` は全経路で
増えるのに理由は一経路でしか積まれていなかった。**片方だけ増える計数は UI で必ず空欄として露出する**。
加えて **UI ラベルは最も狭い原因を名乗ってはいけない** (「却下」は二種のうち一種にすぎない)。

### 64. LLM は単要素配列をスカラーで書く — delta 全体が失われ「GM が書かない」に見えた (gm_core)
【観察 (2026-07-21, ユーザー実プレイ = **Claude / Anthropic ネイティブ経路**。#63 の診断装置が即座に効いた)】やり直し理由に
`invalid type: string "蓮は結果がどうあれ莉亜をひとりにしない覚悟を決めた", expected a sequence`。
GM は `"facts": ["…"]` でなく **`"facts": "…"` と単一文字列で書いていた**。
【真因 (構造)】serde は型不一致で**デルタ全体**を落とす → run_turn がパース失敗として再送 →
2 回目は当該フィールドごと落として提出するので、**書かれた内容は永久に失われる**。
外から見ると「GM が既成事実を書かない」に見える (#63 以前はエラーが空欄で判別不能だった)。
**部分的失敗が全体的失敗に昇格する**のが本質: 1 フィールドの形の揺れが、正しく書けている
narration/ops もろとも捨てさせる。
【解】`one_or_many` deserializer で `ops`/`facts` を**単要素スカラーでも受ける**
(配列はそのまま・スカラーは 1 要素配列へ正規化)。**schema は配列のまま**にする —
指示は正しく出し、受け側だけ寛容にする (Postel の法則。#40「パース失敗は raw を保持して
再生成の燃料に」の一歩手前で救う / #52 Gemini の型崩れと同族)。再送メッセージにも
「ops と facts は要素が 1 つでも必ず配列」を明記。PoC:
`scalar_written_where_an_array_was_expected_is_rescued`。Red は実測エラーそのもの。
【一般化】**LLM 向け構造化出力では、配列フィールドは単要素スカラーを受けられるようにしておく**
(schema で配列と書いても実際には省かれる)。**発生源は tool-use 引数そのもの** — 実測は Anthropic
ネイティブ経路 (常に tool-use 強制) で起きており、「弱いモデルの JSON フォールバックだけの問題」ではない。
schema 追従の良いモデルでも単要素は潰れるので、救済は全プロバイダで要る。加えて **「LLM が X をしない」という観察は、
「X を試みたが受け取り側で落としている」を必ず先に疑う** — 提案が届く前に消えていれば、
プロンプトをいくら直しても改善しない (今回は先にプロンプトを直したが、真因はこちらだった
可能性がある)。

### 65. GM は自分の即興を「設定」と認識しない — 既成事実 0/45・0/20 の絶対ゼロ (harness)
【観察 (2026-07-21, Claude Opus・girl_next_to_me 45T と lakeside_manor 20T の両方でゼロ)】
prompt を直し (#63/#64 も潰し) てもなお **1 件も書かれない**。弱い指示なら低頻度で出るので、
**絶対ゼロは体系的な判断規則の存在**を示す。開発者モード (`<meta:>`) で GM に直接尋ねた結果:
①**チャネルは見えている** (コールドスタート文を逐語で引用) ②**用途も正確に理解している**
③にもかかわらず「シナリオ側が与えた前提は資料であって『新しく判明した設定』ではない」
「判明する情報は flag/item として state に載る設計だから書く対象がない」と判断していた。
【真因 (閾値の較正ミス)】旧文「**新しく判明・確定した設定**」を GM は「**シナリオ側が明かした
情報**」と読む。そして推理盤面ではその情報は全部 flag/item に吸収されるので、該当ゼロと結論する
— 読み方として筋が通っている。**見落とされるのは GM 自身が即興で足した細部** (「湖山と調査員は
学生時代の友人」等。盤面データのどこにも無い)。ところが**書いている当人にとって自分の即興は
「判明」ではなく単なる「描写」**なので、契機として発火しない。加えて空節の文言「まだ何もない」を
**状態報告**として書いていたため、GM は毎ターン「該当がまだ無い」証拠と読み、同じ判断を繰り返す
自己強化ループに入っていた (GM 自身がメタ回答でこのループを申告)。
【解 (prompt 層のみ・in-turn 方式としては最後の反復)】契機を「新しく判明・確定した設定」→
**「あなたが語りの中で新しく決めた/即興で足した事柄」**へ (自分が足したかは判定できる)。
帰結も具体化: 「**あなたの語りの細部は次のターンには残らない** (引き継がれるのは state と経緯 1 行と
既成事実だけ) → 書き留めなければその設定は**存在しなかったことになる**」。空節は状態報告から
**指示**へ (「ここまでの語りであなたが決めた設定を思い返し、拾って書くこと」)。
【一般化】**LLM に「自分の出力の性質」を判定させる指示は、その性質が本人から見て何に見えるかで
決まる** — 外から見て「新しい設定の確定」でも、書いている当人には「ただの描写」でしかない。
契機は**行為の主体が自分だと分かる形** (「あなたが決めた」) で書く。#32「権利≠義務」・
「忘れそうなら＝自己予測は不能」に続く**契機設計の失敗クラス三例目**。
【残】これで書かなければ、in-turn 収集は諦めて **あらすじ圧縮時の抽出** (spec 22 案) へ移す —
GM はターン中は語り手であって記録係ではない。圧縮時なら「この章で確定した設定は何か」という
**記録係の問いを記録係の瞬間に**投げられる (失敗は spec 22 の設計を支持する証拠になる)。

### 66. 三度の較正でも 0 — 「語り手に記録係を兼ねさせる」ことの構造的な無理 (spec 20 の収縮)
【観察 (2026-07-21, Claude Opus)】既成事実の記述義務を三度書き直しても **0/45・0/20 の絶対ゼロ**:
①「忘れそうなら」= ステートレスな LLM に忘却の体験は無く判定不能 (#65) ②「新しく判明・確定した
設定」= GM はこれを「シナリオ側が明かした情報」と読み「flag/item で state に載るから対象外」と
**正しく推論して**書かない (#65) ③「あなたが即興で足した事柄」+ **提出直前の Yes/No 手続き** =
認識は完全に届いた (問えば「東大理三」「妹の雫と 3 万円」を自分で列挙し、書くべきだったと認める)
のに、提出前の確認が毎ターン脱落した。
【真因 (役割の衝突)】GM 自身の分析が的確だった — 「**条件付き・自己申告型の作業は、語りの生成に
注意を奪われると最初に脱落する**」。summary が守られるのは**無条件**だから (「毎ターン書け」)。
条件判定を伴う副次作業は、主作業 (語りの生成) と同一の出力の中では勝てない。**語り手に記録係を
兼ねさせるのが構造的に無理**であって、文言の問題ではなかった。
【解 (収縮)】GM の書き込み経路を**一式撤去**した: `StateDelta.facts` / `apply_gm_facts` /
`FactsDigest` / 📝・📝⁺ ログ / `TurnView.new_facts`・`reinforced_facts` / GM_SYSTEM の記述義務と
提出直前チェック / 空節のコールドスタート促し。既成事実は**ユーザーが宣言し GM が守る**欄になり、
注入節の従属規律 (抑止+保護) と policy・UI・セーブは残る。**守れない規律を prompt に積み続けない**
— 積むと他の規律の重みまで下がる。
【一般化】**LLM に副次的な記録作業をさせるなら、主作業と同じ出力に混ぜてはいけない。** 混ぜるなら
無条件 (毎回) にする (summary が実例)。条件付きにしたいなら**別の呼び出し・別の役割**へ出す。
機械的な記憶の蓄積が要るなら、語りと競合しない瞬間 (あらすじ圧縮時) に**記録係の問いを記録係の
瞬間に**投げる — これが spec 22 (意味記憶の抽出) の設計根拠になる。
【副産物】#63 (再試行理由の可視化) と #64 (単要素スカラーの救済) はこの調査の途中で見つかった
別クラスの実バグで、撤去後も有効なまま残る (ops にも同じ罠がある)。

### 67. 台帳ドリフトは撤去でも追加でも起きる — 同日に両方向で 3 件
【観察 (2026-07-22)】朝: spec 20 で撤去した `prune` (enum 値) と GM 書き込み経路の記述が、
`specs/20_shared_facts.md` の前半・`spine.rs` の enum doc・`CLAUDE.md` に残存。**実害は文言でなく
動作**で、spec は `facts_policy: prune` を有効値として提示し続けていたが、実測では
`unknown variant` で **manifest 全体が parse 失敗**する (spec どおり書いた配布物はロードごと落ちる)。
しかも**回収を主題に据えた前日のコミット** (89b9926) ですら 3 箇所を取りこぼしていた。
夕: TTS とシードリセットを実装し `data_contract.yaml` には書いたが、**CLAUDE.md の現状節を
落とした** (2 件)。
【真因 (二つの非対称)】(a) 撤去は既存節の**読み直し**を要求するが、追加は新しい節を書くので
目に入る — この非対称で、同一ファイル内の前半 (撤去前) だけが矛盾したまま残る。(b) ただし
「追加は目に入る」が成り立つのは**その台帳ファイルを編集している時だけ**。実装が台帳に一切
触れない長丁場では、追加でも普通に落ちる。
【反例が処方を決めた】どちらの事故でも `data_contract.yaml` は追従していた。差は注意深さでなく
**作業の種類** — そのファイルだけ別件で全文を読み直す作業 (YAML parse 可能化の修復) が入っていた。
ゆえに処方は「気をつける」でなく**読み直しを人工的に発生させる**こと: (1) 撤去時 = 撤去した
機構名・enum 値名で全台帳を grep (2) 着地時 = `git show --stat` で台帳ファイルの有無を目視。
【機械の網は代替にならない】撤去した enum 値は**未知キー lint の射程外** — キーは既知で**値**が
無効なので、lint 到達前に serde が落ちる。
【一般化】#66 と同型。**条件付き・自己申告型の記録作業は主作業に注意を奪われると脱落する** —
GM に記録係を兼ねさせられなかったのと同じことが、実装者の台帳更新にも起きる。手順を書くだけでは
足りず、**実行の契機が作業の流れの中に無いと守られない**。

### 68. 二重に隠した機能は「壊れている」に見える — TTS が不発と判断された
【観察 (2026-07-22, 実機)】`use_tts: true` を宣言した盤面を開いても読み上げが鳴らず「不発かな」と
判断された。機構は端から端まで正常で、**ユーザーが手で ON にしたら鳴った**。
【真因】読み上げ操作が出るのは作者が `use_tts: true` と宣言した盤面だけなのに、そこで**既定 OFF**
にしていた。操作自体もホバーで隠れる仕様 (常設の邪魔さを避けるため = 意図どおり) なので、
**隠れた操作を自力で見つけて押すまで無音**という二重の隠れ方になり、作者の宣言が何も起こさない
状態だった。さらに「押した後の新しい受理ターン」でしか鳴らないので、押しても即座には確認できない。
【解】未設定を ON として扱う三値へ (未設定=ON / "1"=ON / "0"=OFF)。宣言のない盤面では speak 自体が
呼ばれないので、既定 ON にしても勝手に喋り出す事故は起きない。あわせて設定画面に前提
(「`use_tts: true` の盤面でだけ使える」) と**今のシナリオの対応状況**を出し、切り分けが設定画面で
完結するようにした。
【一般化】**保守的な既定 (opt-in) と、控えめな UI (隠す) を両方適用すると積になる。** どちらも
単独では正しい判断でも、重ねると機能が存在しないのと区別がつかない。作者/上位層が明示的に
「使う」と宣言した対象については、**宣言の下流ではもう隠さない**。

### 69. 静かな欠落 — 「まとめて投入」が下流で並列に解釈され、失敗握り潰しと組んで文が消える
【観察 (2026-07-22, AivisSpeech 実機)】読み上げが途中で切れる。当初はエンジン側の 1 リクエスト
文字数制限を疑ったが、**curl で棄却** — 320 字を 1 リクエストで投げて 4.4MB の WAV が正常に返る。
【真因 (自分が入れた退行)】順序の穴を塞ぐ際に、文ごとの `speakText` を「1 文ずつ await」から
**「全文をまとめてアダプタのキューへ投入」**へ変えた。アダプタの再生は FIFO で直列だが、
**合成 (fetch) は投入時に即開始される** — ネットワーク型エンジンでは全文の合成リクエストが
一斉に並列発射され、ローカル GPU に十数本を同時に投げると後続がライブラリ既定の 30 秒
タイムアウトで reject。こちらは失敗を握り潰す設計 (TTS は装飾であって正本でない) なので、
**その文が黙って飛ぶ**。例外もログも出ず、文だけが消えた。
【なぜ見えなかったか】webSpeech は合成を伴わない (ブラウザが直接喋る) ので露見しない。
**エンジンを替えて初めて出た**。
【解】自前の**直列鎖** (`chain`) に繋ぐ。1 文ずつ await して積む方式に戻すと、その隙間に後発の
`queue: true` (エピローグ) が割り込んで順序が崩れる (塞いだ穴が再発する) ため、鎖にすることで
**直列と順序保存を同時に**満たす。打ち切りは鎖の各リンク内の世代チェックで空回りさせる。
【一般化】**「まとめて投入」は自前が直列のつもりでも、下流が並列に解釈することがある。**
そして**失敗を握り潰す経路と組むと、過負荷が「エラー」でなく「静かな欠落」として現れる。**
握り潰しは正しい判断でも、その先で欠落が観測不能になる組み合わせは警戒する。

### 70. 提示層の小さな罠 3 件 (WebView2 / ネイティブ UI)
【a. ネイティブ `<select>` の高さは CSS で縛れない】話者が増えるとドロップリストが画面外へ
はみ出して下まで届かない。**ポップアップは OS が描く**ので `max-height` も `overflow` も効かない。
→ `size` 属性を付けて**枠内描画のリストボックス**にすれば高さ固定 + スクロールが効く。ただし
`option` へ一律 `bg-ink` を当てている環境では `select[size] option:checked` に色を敷かないと
**選択行が見えなくなる**。`option` は折り返さないので、長い名前は `title` (hover) で補う。
【b. 選択肢を保存しないと「選択が消えた」ように見える】話者 ID は localStorage に保存していたが
**一覧は component ローカル**だったので、設定を開き直すと該当 `<option>` が無く select が空白に
なった。ID は残っているのに選択が消えたように見える。→ 一覧もキャッシュし、鍵は**エンジン +
実効サーバー URL** (どちらか変われば別サーバーの ID なので捨てる)。保存済み ID が一覧に無い時は
**その ID 自体を選択肢に出す**フォールバックで、空白の select を作らない。
【c. Chrome 71 以降 `speechSynthesis.speak()` はユーザー操作を要求する】フレームが一度でも
ユーザー操作を受けていないと `not-allowed` で拒否される。Kataribe は行動入力を押してからターンが
回るので現状は常に満たすが、**「操作なしで自動的に進むターン」を将来作ると無音になり得る**。
しかも握り潰している catch で静かに消える (#69 と同じ形)。
【一般化】提示層の「効かない」は、こちらのロジックでなく**プラットフォームが描画/実行を握って
いる**ことに由来する場合がある。CSS が効かない・API が拒否する、を実装ミスと決めつけない。

### 71. 「2 個振る」には合計と位取りの二種類がある — 3d6 を `sides: 18` で代用すると分布が静かに入れ替わる
ユーザーの問い「3D6 なら最低値 3・最大 18 のはずだが、Kataribe では 1 になってしまわないか」から。
**正本は無実だった**: authored 経路 (challenge / contest / roll_stat) は `count` 個を個別に振って
合計するので 3d6 は 3〜18、tier の `Min` も `roll == 1` ではなく **`roll == count`** (全部 1) で
判定している。

**穴は即興側にあった。** LLM が提案する `check` / `request_roll` op には `count` が無い
(1d{sides} 固定) ので、GM が 3D6 相当を出したくても書ける形が `sides: 18` しかない。そして
**エンジンは書かれたとおりに忠実に振る** — 3d6 の合計は 3〜18 の釣鐘型、1d18 は 1〜18 の一様分布で、
最低値も平均も散らばりも別物になる。**様式は整合性ではないので却下されない** (spec 16 で
`check_style` を入れたときと同じ線引き)。ロード時エラーにもならず、盤面の手触りだけが静かに壊れる。

**d100 はこの罠に見えて罠でない。** CoC7 の卓で 10 面体を 2 個振るのは十の位と一の位の
**位取り**であって合計ではないため、結果は 1〜100 の一様分布 = 1d100 と同値
(「00」と「0」で 100)。つまり `count: 2, sides: 10` (= 2〜20 の合計) と書くのは誤りで、
d100 は `sides: 100` の 1 個が正しい。**同じ「2 個振る」でも合計か位取りかで別の分布になる**。

【解】prompt 層のみ (engine 不変・schema 不変 = キャッシュの安定プレフィックスも動かさない)。
GM_SYSTEM に「即興 check は 1 個のダイスだけ / 合計ダイスが要る判定は attempt_challenge を選べ /
d100 は 1d100 と書く」を接地。`count` の使いどころは**合計の分布そのものを難易度設計に使う系**
(2d6+修正・3d6 ロールアンダー) だけで、d100 は含まない。
PoC: `gm_system_limits_improvised_checks_to_a_single_die` (Red→Green)。

【同時に見つかった台帳ドリフト = #67 の「追加側の穴」の実例】2026-07-20 の複数ダイス追加は
CLAUDE.md の該当節と package_spec は更新したのに、`Natural` / `TierDef` / `ScenarioError` の
doc comment と validate のコメント (`roll == 1`・`1..=sides`)、data_contract の `TierDef.natural`
(そもそも 2026-07-12 の `AtMost`/`AtLeast` 追加すら反映されていなかった)、そして CLAUDE.md の
テスト総数 (統合テスト 2 本のうち 1 本を数え落として 307) を取りこぼしていた。
**実装が台帳に触れない作業では、追加でも普通に落ちる。**

【残す穴 (優先度最低・作者側の作法として明記)】`validate` の threshold 範囲は `1..=count×sides`
だが、合計の**到達可能帯は `count..=count×sides`**。3d6 に `at_most: 2` (永久に不発火) や
`at_least: 3` (常時発火) を書いても load を通る。`count == 1` では両者が一致するので、
これは複数ダイスを使う盤面でだけ現れる。

【一般化】**ダイスの「個数」は表記の問題ではなく分布の問題**で、面数の付け替えでは代替できない。
そして代替が効かないことをエンジンは教えてくれない — 様式の誤りは整合性の破れではないので、
閉世界の検査網には最初からかからない。こういう層は prompt 接地と作者向け仕様書でしか塞げない。

### 72. 中断したら二度と喋らない — 「止める」が下流のキューを閉じたまま残す (TTS)
「AivisSpeech でスキップすると次のターンにはボイスが出ない気がする。スキップは 1 ターン
だけになっている？」というユーザー報告から。**1 ターンではなく、そのセッションでは二度と
鳴らなくなっていた。**

【機序】`@aituber-onair/voice` の `BrowserAudioPlayer.stop()` は `audioElement.pause()` と
`currentTime = 0` をするだけで、`play()` が返した Promise を settle しない —
**pause は `ended` も `error` も発火しない**ので、待っている側は永久に待つ。その結果
`VoiceEngineAdapter.processQueue` が `await playPreparedSpeech` で止まったままになり、
**`isProcessingQueue` が true のまま残る**。以後の `speakText` は
`if (this.isProcessingQueue) return;` で即座に捨てられ、キューに積まれたまま一度も再生されない。
`stop()` 自身は待機中の request を reject するので**呼び出し側には正常な中断に見える** — 壊れた
のはライブラリ内部のループだけで、こちらは失敗を握り潰す設計 (#69) なので症状は例外でなく
**無音**として現れる。

【スキップ限定ではない】`speak()` は新しいターンが来たときにも同じ `stop()` を呼ぶので、
前の語りを喋っている最中に次のターンが確定すれば同じ罠に落ちる。スキップは意図的な中断ゆえ
**再現しやすかっただけ**で、原因は「再生中に止めたか」であってボタンではない。

【webSpeech では出ない】ブラウザ内蔵は self-playing エンジンで、`speechSynthesis.cancel()` が
発話を settle させるためループが正常に巻き戻る。**ネットワーク型 (VOICEVOX / AivisSpeech /
OpenAI 互換) だけが踏む**。実機評価を WebSpeech → AivisSpeech へ切り替えた直後に露見したのは
偶然ではなく、経路が変わったから。

【修正】再生中に止めたときだけアダプタを捨て、次の `speak` で組み直す
(`isPlaying()` を `stop()` を呼ぶ**前**に見る = 畳まれる前の状態で判定)。合成待ちや停止済みなら
ライブラリのキューは正常に巻き戻るので使い回す。あわせて `speak()` の割り込み処理を
`stop()` へ寄せ (アダプタを取る前に呼ぶ — 後だと今から使うアダプタを捨てる)、
日本語音声の選択をキャッシュ (組み直しのたびに `voiceschanged` を最大 1 秒待たない)。
PoC: 実ライブラリを使った使い捨て再現 — 再生を「立ててから永久に pending」に差し替えて
`isProcessingQueue = true` の残留と、作り直せば鳴ることを実測 (Red→Green)。

【一般化】**「止める」API が、呼び出し側から見える約束 (待機中の Promise を reject する) と、
内部の後始末 (再生ループを巻き戻す) の両方を果たしているとは限らない。** 前者だけ守られると
中断は成功して見え、壊れた状態は次の要求まで観測されない。外部ライブラリの `stop()` を
「打ち切りの正常系」として扱うときは、**その後に同じインスタンスがまだ使えるか**を必ず疑う。
使い回しをやめて作り直すのが、内部状態を読まずに済む一番安い回復手段になる。

### 73. 開発ビルドでは CSP が効かない — ノックサーバーへの WebSocket がリリースだけで死ぬ

**症状**: `npm run tauri dev` では卓が開けるのに、インストールした exe (v0.5.6) では
「ノックサーバーに接続できません: wss://knock.outcasts.jp/ws」で必ず失敗する。
サーバ側は無実 — 同時刻に `curl` で `wss://knock.outcasts.jp/ws` へ WebSocket upgrade を
投げると **101 Switching Protocols** が返っており、TCP 3478 も開いていた。

**真因**: `tauri.conf.json` の CSP が
`connect-src 'self' ipc: http://ipc.localhost http://localhost:* http://127.0.0.1:*` で、
**WebSocket の scheme を一つも許していなかった**。`connect-src` は明示された時点で
`default-src` に落ちないので、`wss:` も `ws:` も全て遮断される。TTS のために localhost を
開けたときの形がそのまま残っていて、あとから足した knock の WebSocket が射程外だった。

**なぜ dev で気づけなかったか**: `devUrl` の間、画面を配っているのは Vite であって Tauri
ではない。CSP は Tauri が自分で配る本番ビルド (`frontendDist`) にだけ乗る。ゆえに
**CSP 由来の破れは dev で構造的に再現しない** — 手元でどれだけ動かしても出ず、
インストールした人の環境で初めて出る。今回それを踏んだのは実装者本人ではなく
「リリース版を入れて試した」経路だった。

**修正**: `connect-src` に `wss: ws://localhost:* ws://127.0.0.1:*` を追加。
単一ホストの allowlist にはできない — 部屋コードの配送先はユーザーが差し替えられる
(契約 `knock_hosting` = 信頼点を自前ホストできることが安全装置) ため。平文 `ws:` を
任意ホストへ開けないのは、自前ホストを TLS へ寄せるため (手元で立てて試す localhost だけ例外)。
**LAN の平文 knock (`ws://192.168.x.x`) は通らない**ので、要るようになったら明示的に開ける。

**PoC**: `tauri.conf.json` を `include_str!` で読み、`connect-src` が `wss:` と
localhost の `ws:` を含むことを表明する unit テスト (Red→Green)。ブラウザの挙動そのものは
再現できないが、**dev で観測できない層の invariant を字面として固定する**ことはできる。

【一般化】**開発時と配布時で「誰が画面を配るか」が違うフレームワークでは、配る側が付ける
ヘッダ (CSP・COOP/COEP・キャッシュ制御) は開発中ずっと不在になる。** そこに依存する破れは
テストではなく配布物でしか出ないので、①その設定に触る変更は必ず配布物で確かめる
②設定の字面を invariant としてテストに固定する、の二段で守る。CSP を締めたあとに
**新しい外部接続を足したとき**が最も危ない — 締めた本人と足す本人が別の作業をしていて、
足した側は dev で通るので気づけない。

### 74. 開発環境が「配布物に無いもの」を既定に持っていた — 存在しないシナリオが初回一覧に居座る

**症状**: まっさらな環境にインストーラーで入れると、パッケージ一覧に **`escape` が居る**。
実体は無いので開けない。開発機では一度も見えなかった (2026-07-23 ユーザー報告)。

**真因**: 初回起動時の既定一覧が `BUILTIN_PACKAGES = ["packages/escape"]` で、しかも
**同梱していなかった**。`tauri.conf.json` に `bundle.resources` は無く、インストーラーは
`packages/` を含まない。repo 直下で開発している間だけ**相対パス**が解決していたので、
「同梱パッケージ」というコメントが事実と食い違ったまま通っていた。

**#73 と同じ形**: 開発時と配布時で足場が違い、開発側だけが余分に持っている。
CSP は「配布時にだけ在るもの」で気づけず、これは「**開発時にだけ在るもの**」で気づけない。
向きが逆なだけで、どちらも**開発機で動かす限り永久に再現しない**。

**修正**: 既定を空にした (ユーザー判断「最初は何もなくていい」)。一覧は書庫から取るか
手で追加するものなので、空が正しい初期状態。あわせて `resetPackages()`
(どこからも呼ばれていない死んだコード) を撤去。空一覧の受け皿 (`packages.localEmpty`) は
既にあったので UI 側の追加は不要だった。

**残る影響**: 既に一覧へ何かを追加した環境では、`packages/escape` が localStorage に
焼き付いたまま残る (追加時に既定ごと保存されるため)。実害は「開けない行が 1 つある」だけで、
UI から削除できるので自動掃除はしない — **存在しないパスを黙って消す実装は、
外付けドライブが外れているだけの正当なパスまで消す**。

【一般化】**「同梱」と書いたら、本当に同梱されているかをビルド設定で確かめる。**
相対パスの既定値は、開発機の cwd という暗黙の足場に乗っているので、配布物では
必ず外れる。そして開発者は永遠にそれを見ない — **まっさらな環境でインストーラーから
入れる**という経路を一度も通らないまま何十回もリリースできてしまう。

### 75. 受け側の allowlist が送り側に追いつかず、卓の開始が黙って捨てられていた

**症状**: 2 台で卓を張ると「卓が始まった」までは進むのに、**ゲストの行動入力欄が灰色のまま**。
ホストは普通に入力できる (2026-07-23 ユーザー実測、複数 PC)。

**真因**: `GuestLink` の受信 switch が卓メッセージを **allowlist で列挙**していて、
`party_turn` / `input_status` / `reveal_order` / `timer_sync` の 4 つしか転送していなかった。
ホストが卓開始時に送る **`table_start` が `default` で捨てられていた**。
ゲストは `applyGameView` を一度も通らないので `started` が false のままで、入力欄の
`disabled` 条件 (`!game.started || …`) に永久に引っかかる。

**症状の非対称がそのまま診断になっていた**: ホストは自分の盤面が最初から始まっているので
平気で、ゲストだけが止まる。「ホスト以外の入力が始まらない」は「ゲストが盤面を
受け取っていない」とほぼ同義だった。

**なぜ Phase C で気づけなかったか**: **2 台無いと絶対に出ない**。fake transport の
統合テスト (Phase B) はターンループを固定するもので、配送層のメッセージ種別は通らない。
frontend にテスト基盤 (vitest 等) が無いので、この層は**実機以外に検証手段が存在しない**。

**修正**: switch を**既定で転送**へ反転した。明示的に処理するのは配送層のもの
(`hello` / `game_response` / `game_event`) だけで、それ以外はすべて卓の語彙として
store へ渡す (知らない型は store 側で無視される)。**送り側に種別が増えたとき、
受け側の列挙を足し忘れても落ちない**形にした。

【一般化】**送り手と受け手が別々に種別を列挙する構造は、片方だけ増えたときに
「エラーではなく無音」で壊れる。** 落として良いものが少数 (配送層) で、通すべきものが
開いた集合 (アプリの語彙) なら、**allowlist ではなく「既定で通し、既知のものだけ横取り」**
にする。今回は受け側が 4 種を数え上げていて、5 種目が生まれた瞬間に無言で死んだ。
