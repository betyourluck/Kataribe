# 11. エピローグ — goal 到達時の engine 主導の締めくくり語り

Status: **Phase A〜C Done（2026-07-14 rev3 査読 → 同日実装・PoC green。Phase D = 実 LLM での
実測 (記録との矛盾ゼロか・後日談の質・生成時間) + GUI/CLI 目視が残）**
Scope: goal 到達で即ブツ切りになる終幕に、**このプレイスルー固有の回想と余韻**を足す。
`reached()` 発火時に GM へエピローグを 1 回だけ書かせ、結末ナレーションと共に提示する。

## 用語（rev2: 保存の定義を分離 — 本 spec 内で一貫して使う）

- **chronicle** = `Vec<TurnLog>`。engine/prompt が次ターン以降に読む**正史**（`history_note` 還流・
  retrieval 対象・`SessionSave.history` としてセーブされる）。
- **会話ログ** = frontend の `LogEntry` 列。UI が表示する**読了ログ**（テキスト保存機能
  `formatLog` の対象。セーブ対象ではない）。
- エピローグは**会話ログにのみ**積む。chronicle には積まない（終幕なので次ターンは無いが、
  定義として明確に）。**`SessionSave` には含めない**。

## 問題（幕がストンと落ちる）

goal 到達の現在の演出は「authored 結末文 (`GoalDef.narration`) + 到達バナー」で終わり。
数十ターン積み上げた旅路（拾った物・出会い・判定の綾）は一切回収されず、**不完全燃焼**。
authored 結末文は分岐の定型文であって、「今回の」冒険の締めくくりではない。

## 決定（設計の核）

**「いつ」は engine、「何を」は LLM。** エピローグの契機を LLM の判断（「終了条件に達したと
思ったら」）にしない — 到達判定は `reached()` の専権であり、判断を LLM に渡すと
早すぎる幕引き・引かない幕・弱モデルの不発（#32/#37 の「権利では動かない」）を全部抱え込む。
条件が揃った瞬間は engine が完璧に知っているので、**GM に気づかせる必要そのものを消す**。

**責務の書き分け（rev2）**: gm_core は「goal に到達したか」(`reached_goal()`) までを裁く
（改修は語り素材 1 フィールドの追加のみ = 判定・状態は無改修）。**「セッションが実際に
終わるか」（= campaign の advance 辺が無いか）の最終判定は呼び出し側 (app/CLI) の責務** —
辺の有無は campaign 地図を読まないと分からず、それは元々 harness/app 層の持ち物
（`advance_campaign` の戻りで判る）。発火可否 = app/CLI が「到達 + 終端 + epilogue_prompt
あり」を突き合わせて決める。

却下した代替案（ユーザー原案 = authoring 定石）: `when: all(本来の条件, flag エピローグ)` +
flag_rules の守り + flag_hints の促し。engine 無改修で今すぐ書ける利点はあるが、
(a) 弱モデルでの set_flag 不発 = 実質ソフトロック（促しは静的ヒント止まり）、
(b) 全シナリオ作者に配線負担（書庫配布では作者×受領者モデルの積で脆さが増える）、
(c) 対話的な余韻は将来「エピローグモード」(v2) で機構側にも足せる、で機構化を採る
（2026-07-14 ユーザー確定）。

## データ（gm_core: 1 フィールド）

```yaml
goals:
  - id: village_win
    title: 村の勝利
    when: { ... }
    narration: 朝日が昇り、村に平和が戻った。   # 従来どおり (分岐の定型文・必須運用)
    epilogue_prompt: 生存者たちのその後を、一人ずつ短く。犠牲者への追想を忘れずに。  # 演出指示
```

- **`GoalDef.epilogue_prompt: Option<String>`**（serde default = 既存 YAML 無改修）。
  rev2 で `epilogue` から改名 — 中身は**生成指示**であって本文ではない（`TurnView.epilogue`
  = 生成された本文、と同名衝突すると型が同じ `Option<String>` なだけに混乱する）。
  `title`/`hint` と同類の非検証・engine 非解釈の語り素材。**None = 従来どおり即結末**（opt-in）。
- goal ごとに書ける = 分岐エンディングごとに違う余韻を演出できる。
- **`narration` は廃止しない — エピローグの土台**: ①決定論の錨 = 必ず出る authored 正典
  であり生成失敗時のフォールバック。②生成の接地素材 = 「封印か討伐か死か」という結末の
  **意味**を LLM に伝える（無いと結末自体を語り間違えるリスク — 非検証チャネルなので
  誰も直せない）。③役割分担 = narration「何が起きたか」の宣言 / epilogue「それがこの旅路に
  とって何だったか」の回想・後日談。
- **フォールバック保証（rev2）**: `epilogue_prompt` を書いた goal に `narration` が空なら
  **lint 警告**（`ScenarioError::EpilogueWithoutNarration`、`Scenario::lints()` = 非 fatal —
  「生成失敗時にバナーだけになる」を作者に名指しで報せる。fatal にしない線引きは
  FlagHintOnAuthoredOnly と同じ「意図どおり動かない書き方 → 警告」）。
  **空の定義 = `trim().is_empty()`**（rev3: `None` だけでなく `Some("")`・空白のみも空。
  epilogue_prompt 側の有無判定も同基準 — 空白だけの指示は「書いていない」扱い）。
  rev1 の「narration 空 + epilogue のみも選べる」は**撤回** — フォールバックを謳う設計と矛盾。
- 旧形式の単一 `goal: Gate` に `epilogue_prompt` を書いた場合: serde は無視するが
  **`unknown_key_lints` が未知キーとして警告する**（黙殺ではない）。エピローグが欲しい
  scenario は named `goals` へ移行する（後方互換は不変）。

## 発火と生成（harness + 呼び出し側）

- **契機**: 受理ターンで `reached_goal()` が epilogue_prompt 付き goal を返し、かつ**終端**
  （単発 goal / campaign の advance 辺なし。判定は app/CLI — 上記責務分担）。
  **campaign の遷移 goal では発火しない = 遷移時は追加演出なし**（rev2 明言: 次章の開幕描写が
  テンポを担う、が v1 の設計判断。spec 10 の synopsis は内部要約でありプレイヤー向け演出では
  ないので根拠にしない。章ごとの余韻が欲しくなったら v2 で `epilogue_on_transition` を検討）。
- **生成**: `run_turn` ではなくプレーン生成 1 回（ops 不要・ダイス不要 = Summarizer と同じ
  線引き）。**GM の client を使う**（`SUMMARY_LLM_*` は使わない — エピローグは見せ場であり
  ナレーターの声で語られるべき）。
- **素材（接地）— rev2 で予算を明記**（「終端 = コンテキスト最大」でタイムアウト率が最悪化
  するのを防ぐ。通常ターンの注入量と同等に抑える = GM が毎ターン読んでいる量を超えない）:
  - world/protagonist + 到達 goal（id/title/**結末文**）+ authored `epilogue_prompt` 指示
  - synopsis 全章（`synopsis_note` と同じ**予算 2000 字**・新しい章優先）
  - 未圧縮 chronicle の tail（`history_note` と同じ**予算 2400 字**・新しい方優先）
  - last_narration（最終場面との接続）
- **タイムアウト（rev2）**: 専用 30 秒（`EPILOGUE_TIMEOUT_SECS`。Summarizer 15 秒より
  長め = 見せ場、request_timeout 120 秒より短い = 終幕を人質にしない）。
- **規律（決定 — rev2 で未決から昇格)**: 「**起きたことは記録のとおりに・これから起きること
  （後日談）は自由に**」を既定の system 規律に入れる。エピローグは終幕なので以後のターンを
  汚染しない（#47 の经路なし）が、プレイヤーが読む確定文なので記録との矛盾は禁止。後日談の
  想像の**方向づけ**は authored `epilogue_prompt` が担う。長さは指示で目安 600 字
  （機械カットはしない — narration と同じ非検証チャネルの扱い）。
- **失敗時**: skip して従来表示（結末文 + バナー）へフォールバック。非致命・リトライなし。
- **セーブとの順序（rev3 実装メモ）**: 終端ターンでも autosave は従来どおり書かれる
  （既存挙動不変）。**エピローグ生成は autosave の後**に行う — 生成の失敗・クラッシュが
  セーブを巻き込まない。`SessionSave` に epilogue フィールドは追加しないので、
  保存経路に分岐は生まれない（Phase C で迷わないための一言）。

```rust
// harness (pure・テスト可)
pub struct EpilogueRequest { /* goal 情報 + synopsis (予算) + chronicle tail (予算) + last_narration + 指示 */ }
pub fn epilogue_messages(req: &EpilogueRequest) -> Vec<ChatMessage>;  // system=規律 / user=素材
// 生成 helper (ネットワーク経路・テスト対象外。CoT 除去 #30 + 空応答 Err + 30 秒は Summarizer 同型)
pub async fn generate_epilogue(client: &LlmClient, req: &EpilogueRequest) -> Result<String, HarnessError>;
```

## 提示（決定 — rev2 で表示順を変更）

- **表示順: 結末ナレーション → 到達バナー（🎉）→「―― エピローグ ――」→ エピローグ本文。**
  rev1 の「バナーが最後」は 600 字の余韻を 🎉 がぶった切る（不完全燃焼の解消と逆行）ため撤回。
  **エピローグで幕が下りる**。
- app: `TurnView.epilogue: Option<String>`（生成本文）。frontend は区切り + narration と同じ
  本文スタイルで**会話ログに**積む（→ テキスト保存 `formatLog` に自然に含まれる。chronicle・
  セーブには含めない — 用語節の定義どおり）。生成中は `epilogue-writing` イベント →
  「エピローグを紡いでいます……」（spec 10 の compacting と同型）。
- CLI: 同順で println。

## 北極星との整合

- 到達判定・状態は不変（エピローグは提示層の出来事。ops を持たない = 状態を動かせない）。
- 「エンジンが裁き、LLM が語る」の分業そのまま — 幕を引く権限は一切 LLM に渡していない。
- spec 10（あらすじ）が素材供給源になる続編 — 長編ほどエピローグが豊かになる。

## 実装（Phase 分割、各 Phase Red→Green）

- **Phase A（gm_core）**: `GoalDef.epilogue_prompt`（serde default）+ lint
  `EpilogueWithoutNarration`。PoC: parse + 省略時 None + lint（narration が None/空文字/空白のみ
  で警告・実文で沈黙 = `trim().is_empty()` 基準）+ **旧形式 `goal: Gate` に `epilogue_prompt`
  を書いた YAML が `unknown_key_lints` で警告になる回帰テスト**（rev3: Gate は
  deny_unknown_fields ではなく serde 黙殺 → 生 YAML 走査の lint が防衛線、という前提を固定。
  GoalDef 側は既知キーが型から導出されるので追加に自動追従することも同テストで担保）。
  data_contract の Goal 節へ同時に追記（optional 追加なので前倒し不要 — 凍結は同 commit で足りる）。
- **Phase B（harness）**: `EpilogueRequest` / `epilogue_messages`（規律文言 = 記録矛盾禁止・
  後日談許可・目安 600 字・本文のみ）/ 素材の予算カット / `generate_epilogue`（30 秒）。
  PoC: メッセージ形状 + 規律文言 + 予算。
- **Phase C（app/CLI）**: 終端判定（advance 辺なし）に epilogue 分岐 / `TurnView.epilogue` /
  `epilogue-writing` イベント / frontend 表示（会話ログ）+ CLI println。build green、
  実挙動は GUI/CLI 目視。
- **Phase D（実測）**: epilogue_prompt 付き goal を 1 盤面に書き（ドッグフード）、実 LLM で
  ①記録との矛盾ゼロか ②後日談の質 ③生成時間、を確認。

## 未決

なし（rev1 の未決 4 点は査読で全て決定に昇格: ①終端のみ=決定・遷移は演出なしと明言
②後日談許可=既定規律に決定 ③表示順=バナー→エピローグに変更して決定 ④エピローグモード=v2 送り）。
