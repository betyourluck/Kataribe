# 16. d100 ロールアンダー判定 — パーセンタイル技能盤面（CoC 互換の書き味）

Status: **Phase 0〜E 実装済（2026-07-19 同日）— 実 LLM 実測（核心的未知 3 点）と GUI 目視が残** / 2026-07-19

> 実装メモ: PoC 9 本 Red 相当→Green（gm_core 6 + harness 2 + llm_client 1）。ドッグフード盤面
> `packages/lakeside_manor/`「湖畔の洋館」（percentile 様式・SAN 1/1d6・目星/図書館・真相/発狂
> goal + epilogue_prompt）を同梱、engine 動線（日記→地下室→SAN チェック→goal）は使い捨て
> テストで一発 Green。schema 入替は `LlmClient::set_excluded_ops`（app/CLI が new_game と
> campaign 遷移で設定）。可変量ダイスの監査は `StatRollOutcome` → `TurnView.stat_rolls` →
> 「🎲 player SAN -4 (1d6=4)」。outcast package_spec.md への percentile 節追加は
> **アプリのリリースと同時**（旧エンジンは percentile challenge を load できない =
> sides 必須の旧 serde 形式のため、先に spec だけ配ると壊れたパッケージを誘発する）。

> rev2 (2026-07-19): 査読で CoC7 原典との一致を検証（degree 定義・fumble 帯・整数除算・
> d100=1..100 生成すべて RAW 一致の確認）。Must 3 点（critical=01 表記と内部表現の注記 /
> fumble は将来の有効目標値基準 / タイポ）+ Should 4 点（表示置換の明記・判定順は critical
> 先勝ち・modifiers 近似の明文化と拡張置き場・roll_stat と goal の順序）+ Nit（ja 表記は
> 公式準拠カタカナ・エッジケーステスト）を反映。未決 4 点は全て決定済みへ。
Scope: クトゥルフ神話 TRPG（CoC 7 版）系の「1d100 を技能値**以下**で成功」（ロールアンダー％）
の判定様式を Kataribe で書けるようにする。既存のロールハイ加算式（`1d{sides}+stat ≥ dc`）は
**一切変えず**、別モードとして併存させる。新機構は 3 つ
（①ロールアンダー判定と成功度 / ②パーセンタイル challenge / ③可変量ダイス op）＋様式スイッチ。

> 動機（2026-07-19 ユーザー要望）: 受領者から「クトゥルフ神話TRPG系はカバーできるか」。
> 現状の判定は roll-high 加算式のみで、CoC の芯（ロールアンダー％・成功度・1d6 の SAN 減少）が
> 書けない。世界観・探索・秘匿（`hidden_*`）・破滅エンド（`stat_at_most`）は既存機構で高カバー。
> 欠けている判定レイヤーだけを足す。
>
> IP 境界: 機構は「パーセンタイル・ロールアンダー」という**汎用ルール**として実装する。
> CoC のルールブック表記・固有名は engine/spec に内蔵しない（配布 content 側の責任範囲）。
> ラヴクラフトのマイソス自体はほぼ PD であり世界観パッケージは作れる。

## 北極星整合

- **出目も技能値も正本**: 技能（目星 60 等）は**数値 stat** で持つ（新しい「数値技能」概念は
  導入しない — stat+ロールアンダーの合成で CoC の技能ロールになる）。エンジンが d100 を振り、
  現在の stat 値と比べ、**成功度（degree）まで決定論で計算**する。LLM は「目星で調べる」と
  意図を言うだけで、出目・成否・成功度を一切持てない（op 構造上不可能 = 既存 Check と同型）。
- **帰結は authored**: SAN 減少・ダメージのような帰結量のダイス（機構③）は **authored 専権**
  （trigger/challenge の effects のみ）。LLM が「1d6 ダメージを受けた」を op で提案する経路は
  作らない（#23/#24 の穴を開けない）。
- **既存盤面は無風**: 全フィールド serde default・様式スイッチは opt-in。既存 YAML・既存
  セーブ・既存テストは無改修で通る（`sides`/`dc` の default 化だけ validate で従来の必須性を
  保証する — 下記 機構②）。
- **正直な限界**: CoC の手触りの一部（ボーナス/ペナルティダイス・プッシュロール・対抗ロール・
  幸運消費）は v1 に含めない（下記スコープ外）。「CoC 完全互換」ではなく「CoC の書き味で
  探索ものが書ける」が本 spec のゴール。

## 問題（現状で書けないもの）

1. **ロールアンダー判定が無い**: 成否は `total >= dc`（engine.rs の一箇所）のロールハイのみ。
   「1d100 ≤ 技能値」の方向が書けない。
2. **成功度が無い**: 既存 tier は**自然出目の固定帯**（min/max/at_most+threshold）。CoC の
   イグストリーム（≤ 値/5）/ ハード（≤ 値/2）は**技能値に依存する動的な帯**で、静的
   threshold では表現できない（エンジンが判定時に計算するしかない）。
3. **可変量ダイスが無い**: `adjust_stat` は固定 delta のみ。「SAN チェック失敗で 1d6 減少」
   「1d8 ダメージ」の**振った量を stat に反映する**プリミティブが無い。

## 設計

### 様式スイッチ `Scenario.check_style`（opt-in）

```yaml
check_style: percentile      # 省略時 additive (従来どおり・既定)
```

- **engine の意味論には触れない提示/語彙スイッチ**: percentile の盤面では
  (a) LLM の op 語彙（schemars schema）から加算式 `check` を除外し `check_under` を露出する
  （additive の盤面では逆 — `filter_authored_only_ops` と同じ機構で oneOf から落とす。
  Grok の grammar にも出ない=構造的に混ぜさせない）、
  (b) **`scenario_brief` に「## 判定様式」節**として「技能判定は d100 ロールアンダー
  （check_under op、技能値以下で成功・低いほど良い）。加算式 check は使うな」を接地
  （GM_SYSTEM は盤面非依存の const を保つ — percentile 文言を全盤面に撒かない。
  scenario_brief はセッション内で安定なので prompt caching の静的プレフィックス性も保たれる）。
- **engine は却下しない**: 様式違いの op も裁定上は健全（state を汚す経路が無い）ので、
  二層防衛（見せない+通さない）のうち「見せない」だけを適用する。#50 の二層が必要なのは
  **整合性が壊れる**専権侵犯であり、様式は規約であって整合性ではない（設計判断として凍結）。
- schema が scenario 依存になるのは初だが、tools ブロックはセッション内で安定なので
  prompt caching への影響は無い（spec 14 の静的プレフィックス性は保たれる）。

### 機構① ロールアンダー判定と成功度（`StateOp::CheckUnder` + degree）

```yaml
# LLM が提案する即興技能ロール (既存 Check の percentile 版)
- { op: check_under, entity: player, key: 目星 }
```

- エンジンが `1d100` を振り、**目標値 = その entity の stat 現在値**、`roll <= 目標値` で成功。
  `stat` 未宣言は `UnknownStat` で却下（閉世界・既存 Check と同一）。`entity` 省略時 player。
- **成功度（degree）を決定論で計算**（7 版準拠の固定ルール、v1 は authored 上書き無し）:

  | degree (機械 id) | 条件 | 備考 |
  |---|---|---|
  | `critical` | 出目 = **01** | 常に成功（目標値 0 でも成功を保証） |
  | `extreme` | 出目 ≤ 目標値/5 | 整数除算（端数切り捨て = 原典どおり） |
  | `hard` | 出目 ≤ 目標値/2 | 整数除算 |
  | `regular` | 出目 ≤ 目標値 | 通常成功 |
  | `failure` | 出目 > 目標値 | 通常失敗 |
  | `fumble` | 目標値 < 50 なら出目 96–100 / 目標値 ≥ 50 なら出目 100 | 常に失敗 |

  - 出目は **1..=100 で生成**する（原典の「00+0 = 100」相当。内部表現を 0–99 で持つ実装に
    写す場合は 0 がクリティカルに対応する — 本実装は既存 `roll(sides)`=1..=sides を使うので
    01=1）。
  - **判定順は critical 先勝ち**: `roll == 1 → critical` → fumble → extreme → hard →
    regular → failure。現行定義では critical 帯と fumble 帯は交差しないが、将来拡張でも
    「01 は常に成功」が壊れない順序を実装に固定する（査読 Should）。
  - **fumble の閾値基準は「有効目標値」**: 原典の "the number required to pass a roll" は
    難易度込みの実効値。v1 は有効目標値 = stat 値（+modifiers）なのでそのまま比較するが、
    将来「ハード成功を要求する challenge」等を導入した場合は**その実効目標値**で
    `< 50` を判定すること（査読 Must の注記）。
- `CheckOutcome` に `degree: Option<String>`（serde default = 旧セーブ/加算式は None）。
  既存フィールドへの写像: `sides=100` / `roll=出目` / `dc=実効目標値` / `total=出目` /
  `modifier=目標値修正の合算`（機構②の modifiers）/ `success=degree が成功側か`。
  **表示上の注意（査読 Should）**: 既存の `total >= dc` 前提の表示（`1d20(14)+12=26 ≥ 15`）は
  percentile では **`d100=42 ≤ 60 → ハード成功`** の形に置換される（提示層が degree の有無で
  書式を分岐）。また `modifier` は**出目でなく目標値への加算**である旨をログ/監査で
  読み違えないこと（dc に合算済みの値が入る）。
- 即興 `check_under` は**帰結を持たない**（成否と degree の surface のみ。語りは GM、
  機械的帰結が要る判定は機構②の authored challenge で書く）— 既存 Check と同じ役割分担。
- 還流: 既存 `check_outcome_note` の margin/tier surfacing に degree を追加
  （「d100=42 ≤ 60 → ハード成功」。margin の代わりに degree が「どのくらい良かったか」を
  担う — 後付け接地の精密化 2026-06-24 のパーセンタイル版）。

### 機構② パーセンタイル challenge（`ChallengeDef.resolution` + degree 別帰結）

```yaml
challenges:
  san_check_corpse:
    resolution: percentile     # 省略時 additive (従来どおり)
    description: 変わり果てた遺体を直視する正気度ロール
    stat: SAN                  # percentile では必須 = 目標値に使う stat
    entity: player             # 既存の判定主体固定もそのまま効く
    on_success:                # regular 以上の成功 (degree 別が無い時の受け皿)
      effects:
        - { op: adjust_stat, entity: player, key: SAN, delta: -1 }
      narration: 目を逸らさずに済んだ。だが忘れられはしない。
    on_failure:
      effects:
        - { op: roll_stat, entity: player, key: SAN, count: 1, sides: 6, negate: true }
      narration: 視界が歪み、喉の奥から声にならない悲鳴が漏れた。
    on_fumble:                 # 任意。無ければ on_failure に落ちる
      effects:
        - { op: roll_stat, entity: player, key: SAN, count: 1, sides: 10, negate: true }
      narration: 心の底が抜けた。
```

- `resolution: percentile`（フラットフィールド・serde default `additive` — data 付き enum を
  避ける tier `natural` と同じ流儀）。
- percentile では: `stat` **必須**（目標値）。`sides`/`dc` は**使わない**。
- **`sides`/`dc` を serde default 化**し、必須性は validate へ移す:
  - additive: `sides == 0` → `ChallengeShapeInvalid`（従来の必須性を load 時に保証 —
    既定値 0 の黙殺で壊れた挑戦を実行経路に乗せない）。
  - percentile: `stat` 欠落 / `sides != 0` / `dc != 0` → `PercentileChallengeShape`
    （書き間違い = 加算式との混同を load 時に名指し）。
  - percentile + `tiers` → `TierWithPercentile`（自然出目帯と degree の二重クリティカルは
    authored 意図が曖昧になるため禁止。新機能ゆえ配布済み content の互換問題は無い =
    lint でなく validate でよい）。
- **degree 別の帰結スロット**（すべて任意・`ChallengeOutcome` 型を再利用）:
  `on_critical` / `on_extreme` / `on_hard` / `on_fumble`。フォールバック連鎖は
  `critical → extreme → hard → on_success` / `fumble → on_failure`
  （例: extreme だけ書けば critical も extreme の帰結を使う…ではなく**上位から自分の
  スロット→無ければ次段**: critical は on_critical → on_extreme → on_hard → on_success の順で
  最初に在るものを使う。fumble は on_fumble → on_failure）。narration/sound/flag/effects の
  解決は既存の tier 優先ロジックと同型（degree スロットが通常成否より優先）。
- `modifiers`（既存の条件付き有利/不利）: percentile では **bonus を目標値に加算**する
  （「補助があれば技能値 +10 相当」）。出目に足すロールハイと逆方向になるが、
  「bonus 正 = 有利」の作者向け意味論を両様式で一致させるための凍結。
  **原典との差異（決定 2 として明文化）**: CoC 原典のボーナス/ペナルティダイスは
  「十の位のダイスをもう 1 個振って有利/不利な方を採る」方式であり、目標値 ± とは
  **確率分布が異なる近似**である。将来対応する場合は `Resolution` でなく
  **`ChallengeMod`（modifiers）側**に `dice: bonus|penalty` を足す（判定様式でなく
  修正の表現形の問題として拡張する — 査読 Should の設計メモ）。
- `requires` / `entity` 固定 / `guaranteed_challenge_effects`（spec 09 の全帰結共通射影）は
  そのまま効く（共通効果の抽出対象に degree スロットも含める — 全スロットの厳密交差）。

### 機構③ 可変量ダイス op（`StateOp::RollStat`・authored 専権）

```yaml
- { op: roll_stat, entity: player, key: SAN, count: 1, sides: 6, bonus: 0, negate: true }
```

- エンジンが `count × d(sides) + bonus` を振り、`negate` に応じて ± を stat へ
  clamp 適用（`adjust_stat` の delta を振ってから当てるのと等価。bonus/negate は
  serde default 0/false）。出目は `ApplyOutcome.rolls` に載せ提示層が
  「SAN -4 (1d6=4)」を表示（出目まで監査可能）。RNG は本流ストリーム
  （apply 時確定 — 裁定は非射影。spec 09 の「ダイス帰結は非射影」に自動で従う）。
- **AUTHORED_ONLY_OPS の第 6 例**: LLM 提案は `adjudicate` が `StatRollNotAllowed` で却下
  （ダメージ量の捏造遮断 — grant_skill/set_attribute/record_turn/set_presence/resolve_vote と
  同型の二層: schema 除外 + engine 却下）。trigger/challenge の effects からは `apply_ops`
  直行で使える。
- **goal 判定との順序（査読 Should・spec 09 準拠）**: `roll_stat` は apply 内で確定し
  （effects 適用 → トリガー settle）、goal 到達 (`reached`) は **apply 完了後**に呼び出し側が
  評価する — 「SAN 1d6 減少 → SAN≤0 → 発狂 goal」は**同一ターンに** damage → goal の順で
  成立する（減少と判定の間に隙は無い）。
- validate: `sides == 0` / `count == 0` は `RollStatShapeInvalid`（ゼロ面ダイスを load 時に弾く。
  effects 内の走査は set_attribute の幻キー検査と同じ経路）。
- CoC 以外にも効く汎用機構（可変ダメージ・ランダム報酬）。SAN の「1/1d6」は
  on_success に `adjust_stat -1`・on_failure に `roll_stat 1d6 negate` で書ける（上例）。

### 提示層（app/CLI/prompt）

- `CheckView` に `degree`（文字列）。**内部 id は英語のまま**（ログ検索性・セーブ安定）、
  表示は**提示層の言語表**で変換（後から差し替え可能な構造 — 決定 1）。初期値は
  KADOKAWA 公式日本語版に馴染むカタカナ:
  `critical=クリティカル` / `extreme=イクストリーム成功` / `hard=ハード成功` /
  `regular=成功` / `failure=失敗` / `fumble=ファンブル`。
  🎯 行の書式: `🎯 目星 d100=42 ≤ 60 → ハード成功`。
- `scenario_brief` の「## 挑戦」: percentile challenge は
  「{stat} の d100 ロールアンダー判定（技能値以下で成功）」と surface。
- `roll_stat` の出目は既存 rolls 表示に「SAN -4 (1d6=4)」形式で載る（app/CLI 共通）。

## 実装 Phase

- **Phase 0**: data_contract に `check_style` / `CheckUnder` / `resolution`+degree スロット /
  `RollStat` / degree 表を凍結（本 spec の写し）。
- **Phase A（engine: degree + CheckUnder）**: degree 計算の純関数（`fn degree(roll, target)`）+
  `StateOp::CheckUnder` の adjudicate/apply。PoC: 決定論 seed で 6 degree 全帯の Red→Green。
  **エッジケース（査読 Nit）**: `target ∈ {0, 1, 49, 50, 100}` × `roll ∈ {1, 2, 96, 99, 100}` の
  マトリクスを純関数テストで固定 — 特に **target=0 でも roll=01 は critical（成功）**を保証 /
  target=49 と 50 の fumble 帯切替（96 が fumble ↔ failure）/ target=100 で roll=100 のみ
  fumble / 整数除算の端（target=1: extreme 帯 = 0 で 01 のみ critical 成功）。
- **Phase B（engine: percentile challenge）**: `resolution` + degree スロット + フォールバック
  連鎖 + modifiers の目標値加算 + validate 3 種（shape/percentile shape/tier 衝突）。
  PoC: SAN チェック「1/1d6」の成功/失敗/fumble 3 経路 + validate Red→Green。
- **Phase C（engine: RollStat）**: AUTHORED_ONLY_OPS 追加 + `StatRollNotAllowed` +
  effects 経由の apply + rolls surface + validate。PoC: LLM 提案却下 / trigger 経由の
  可変減少がクランプされる / 同 seed 同減少量。
- **Phase D（prompt/schema）**: `check_style` による schema の check/check_under 入替
  （`filter_authored_only_ops` 拡張）+ GM_SYSTEM/state_brief 接地 + `check_outcome_note` の
  degree 対応。PoC: percentile 盤面の schema に check が無く check_under が在る / 接地文言。
- **Phase E（提示層 + ドッグフード + 実測）**: app `CheckView.degree` / CLI 表示 +
  ドッグフード盤面（洋館探索の最小 CoC 風 fixture: SAN・目星・図書館・SAN≤0 発狂 goal +
  `hidden_*` の真相）+ **実 LLM 実測（核心的未知）**。outcast package_spec.md 追従
  （percentile 節の追加）もここ。

## 核心的未知（Phase E で測るもの）

1. **様式の混同率**: percentile 盤面で LLM が加算式の癖（DC を言い出す・出目が大きい方が
   良いと語る）を出さないか。schema 入替（構造遮断）+ 接地でどこまで消えるか。
2. **SAN ループの手触り**: challenge 選択 → roll_stat 減少 → `stat_at_most` 発狂 goal の
   一連が「CoC らしい」テンポで回るか（判定の帰結が次ターンに割れるペーシングとの相性）。
3. **degree の語りの質**: 「ハード成功」を GM が物語の因果に翻訳できるか
   （margin 後付け接地の degree 版が効くか）。

## 決定事項（rev2 査読で確定）

1. **degree の ja 表記**: 内部 id は英語（ログ検索性・セーブ安定）、表示は提示層の
   差し替え可能なテーブル。初期値は公式日本語版に寄せたカタカナ
   （クリティカル / イクストリーム成功 / ハード成功 / 成功 / 失敗 / ファンブル）。
2. **modifiers の目標値加算**: v1 はこれで凍結。原典のボーナス/ペナルティダイスは
   「extra tens die」方式で**分布が異なる近似**であることを機構②に明文化済み。
   将来対応は `ChallengeMod` 側に `dice: bonus|penalty` を足す（Resolution は触らない）。
3. **fumble/クリティカル帯**: v1 は 7 版固定（authored 上書きなし）。ハウスルール需要は低い。
4. **既存 `Check`（加算式）の percentile 盤面での扱い**: schema から隠すのみ・engine は
   受理で確定（様式は規約であって整合性ではない — 二層目は整合性の破れにだけ使う）。

## スコープ外（v2 以降・据え置き）

- **対抗ロール**（degree 比較）/ **プッシュロール**（失敗後の悪化つき再挑戦）/
  **ボーナス・ペナルティダイス** / **幸運消費**（ポイントで出目を買う）/
  **技能成長判定**（セッション末の経験チェック — authored トリガー + roll_stat で
  近似は今でも書ける）。
- 戦闘ラウンド制・DEX 順イニシアチブ（ターン概念の再設計が要る — 別 spec 級）。
- ダメージボーナス（STR+SIZ 由来の db）: `roll_stat` を 2 発並べれば近似可能なので機構不要。

## 参照

- 既存判定: `crates/gm_core/src/engine.rs` の `total >= def.dc`（一箇所）/ `CheckOutcome` /
  tier 解決。challenge 定義: `crates/gm_core/src/spine.rs` `ChallengeDef`。
- 専権 op の型: `AUTHORED_ONLY_OPS`（state.rs）+ schema 除外（llm_client
  `filter_authored_only_ops`）+ adjudicate 却下の二層。
- 可視性 3 軸（2026-07-19）: マイソスの真相は `hidden_*`（GM は見る秘密）が既に担う。
