# 06. 秘匿役職とランダム割り当て — 人狼盤面（グノーシア型）

Status: **In Progress（rev2 査読反映済・Phase A 実装済）** / 2026-07-03
Scope: 社会的推理ゲーム（人狼/グノーシア型）を Kataribe のシナリオとして書けるようにする。
役職は**ゲーム開始時にランダム割り当て**（player 含む・グノーシア式で確定）、**各キャラは
自分以外の役職を知らず、プレイヤーも自分以外は知らない**。新機構は3つ
（ランダム割り当て / 宛先別秘匿 / 投票・死亡の正本経路）。

> rev2 (2026-07-03): 査読で「決定済みと未決の衝突 7 件・設計緊張 3 件」を指摘され解消。
> RNG=専用ストリーム / player 配布=確定 / 死亡=ResolveVote のエンジン効果に一本化 /
> カウンタ=ResolveVote が原子更新 / 同数=seeded 抽選 / secret の宛先定義を精密化 /
> 勝敗 Gate の式密輸を bookkeeping stat で解消。

## 北極星整合

- **割り当てはエンジンの専権**: 役職の shuffle は `initial_state(seed)` から**派生した専用
  ストリーム `role_rng`** で行う（本流 `state.rng` は消費しない＝配役の有無でプレイ中の
  ダイス列が変わらない）。LLM は割り当てに関与できない（「出目は正本」原則）。seed が
  同じなら同じ配役＝再現・監査可能。グノーシアのループ構造とも噛み合う。
- **票の集計・死亡の確定はエンジン**: LLM は「誰が誰に投票するか」の意図だけ提案し、
  開票・同数解決・死亡の帰結はエンジンが確定する（LLM は開票結果を捏造できない）。
- **chronicle が議論を覚える**: 「T3 でアリスがボブを疑った」「T5 でボブが占い師を騙った」
  を GM が保持することがこのジャンルの成立条件。既存の経緯ログがそのまま心臓部になる。
- **正直な限界**: narration 経由の役職漏洩は構造保証できない（narration は非検証、#23 と
  同じ非対称）。砦は prompt 接地のみで、実 LLM プレイでの漏洩計測が本 spec の核心的未知。

## 問題（現状で書けないもの）

1. **ランダム初期化が無い**: `initial_state` は authored 固定値のみ。「6 人に 人狼2・占い師1・
   村人3 を配る」を書けない。
2. **宛先別の秘匿が無い**: `hidden_stats`/`hidden_flags` は「全提示層から隠す」。役職は
   宛先別の可視性（下記 機構②）が要る。
3. **動的な死亡の正本経路が無い**: `set_presence` は現在 authored トリガー専権。本 spec で
   その専権を「**authored トリガー、または `ResolveVote` のエンジン効果**」に拡張する
   （LLM が直接提案できないことは不変＝遮断の本質は保つ。動的な退場は必ずエンジンの
   開票を経由する）。

## 設計

### 機構① ランダム役職割り当て（`role_assignment`）

```yaml
role_assignment:
  key: 役職                                  # 書き込む属性キー
  pool: { 人狼: 2, 占い師: 1, 村人: 3 }        # 役職 → 人数
  among: [player, alice, bob, chris, diana, eri]  # 配布先 (player を含む・グノーシア式)
```

- `initial_state(seed)` が seed から**派生した専用ストリーム `role_rng`**（例: seed に固定
  ラベルを混ぜた splitmix 系）で決定論 shuffle し、各 entity の `attributes[key]` に書く。
  本流 `state.rng` の cursor は動かさない。
- `role_assignment` 自体が属性キーの**宣言**を兼ねる（`initial_attributes`/`CharacterDef.attributes`
  との二重宣言は不要）。値の閉集合 = pool のキー。
- **bookkeeping stat の自動生成**: 割り当てと同時に、役職ごとの生存カウンタ
  （`生存人狼数`・`生存村人数` 等 = pool のキーから機械生成）と各 entity の `生存`(=1) を
  初期化する。以後の更新は `ResolveVote` の専権（機構③）。
- validate: `pool の人数合計 == among の人数` / `among ⊆ {player} ∪ characters`（幻キャラ遮断）。
- 割り当て後は既存機構がそのまま効く: `Gate::AttributeIs {entity, key: 役職, value: 人狼}` で
  トリガー・goal の条件に使える。`set_attribute` は authored 専権のままなので LLM は役職を
  書き換えられない。

### 機構② 宛先別秘匿（`secret_attributes`）

```yaml
secret_attributes: [役職]   # ゲーム的秘匿情報の属性キー
```

**可視性の正確な定義**（実装のフィルタ基準）:

| 宛先 | 見えるもの |
|---|---|
| GM（prompt / `state_brief`） | **全員分**（秘匿注記付き。ゲームを回すのに必要） |
| プレイヤー UI（`state_view` / プロフィールカード） | **player 本人の分のみ**。NPC 分は DTO 段階で落とす（隠しゴールと同じネタバレ衛生） |
| 登場人物どうし（物語内） | **互いに不可**。GM が演じ分けで維持する（構造でなく prompt 規律） |

- 既存 `hidden_stats`/`hidden_flags`（全部から隠す＝帳簿の秘匿）とは別語彙・別軸。
- **GM_SYSTEM 接地（漏洩対策の砦）**:
  - 「secret な属性は登場人物どうし**互いに知らない**。各キャラは自分の役職だけを知っている
    前提で演じよ（他人の役職を知っているかのような行動・台詞をさせるな）」
  - 「narration の地の文で役職を明かすな・匂わせの断定をするな（『人狼らしく』等）。
    疑いは登場人物の台詞・行動として描け」
  - 「役職能力の結果（占いの判定等）は**当人だけの知識**。当人の口から語られるまで
    他キャラは知らない」

### 機構③ 投票と死亡（`CastVote` + `ResolveVote`）

- **`StateOp::CastVote { voter, target }`**（LLM 提案可）: 「voter が target に票を入れる意図」。
  検証: voter/target の `生存`=1・投票フェーズ中（flag gate）・voter に投票権（フェーズにより
  gate 可: 夜の襲撃は `AttributeIs 役職=人狼` のみ等）。GM は 1 ターンに NPC 全員分の票を
  ops として並べる（**票の意図は LLM、開票はエンジン**）。
- **`ResolveVote`（authored トリガー専権の効果 op・第5例）**: 投票締切のトリガー（フェーズ
  進行）の effects に書く。LLM が提案すると `adjudicate` が却下。発火時にエンジンが
  **一箇所で原子適用**する:
  1. 開票（最多得票者を確定。**同数は `role_rng` と同系の seeded 抽選**＝決定論）
  2. 対象の `stats[生存] = 0`（Gate/集計用の正本）
  3. 対象の `present_overrides = false`（表示制御への投影＝画面から退場、発言も止まる —
     presence の prompt 接地が「死者の発言」を防ぐ既存の砦になる）
  4. **bookkeeping stat の再計算**（`生存人狼数`・`生存村人数`、および差分 stat `村人優位` =
     生存村人数 − 生存人狼数）
  5. 票のリセット
- **生存表現の役割分担（明文化）**: `stats[生存]` が Gate・集計の**正本**、`present_overrides`
  はその**表示への投影**。どちらも ResolveVote だけが書く（プレイ中の動的死亡について）。
  「死んだのに発言する」は presence 接地（state_brief の「この場にいる」）が検出基準。
- **勝敗 Gate は式を評価しない**（既存 Gate は stat 単体比較のみ）。比較が要る条件は
  ResolveVote が原子更新する**差分 stat** に畳む:
  ```yaml
  goals:
    - id: village_win
      title: 村の勝利
      when: { kind: stat_at_most, entity: player, key: 生存人狼数, value: 0 }
    - id: wolves_win
      title: 人狼の勝利
      when: { kind: stat_at_most, entity: player, key: 村人優位, value: 0 }  # ResolveVote が計算する差分 stat
  ```
  （`stat_compare` 系の新 Gate は導入しない — 式の評価を Gate に持ち込まず、計算は
  エンジンの適用時に閉じる、という既存の境界を保つ。）

### フェーズ進行（新機構不要 — 既存プリミティブで組む）

昼議論 → 投票 → 夜（襲撃）→ 朝（結果）のループは既存機構で書ける:

- フェーズは `hidden_flags` の帳簿フラグ（`投票フェーズ` 等）+ `record_turn`/`turns_since`
  （「議論 N ターンで投票へ」）+ repeatable トリガーで回す。
- 朝の結果発表はトリガー `narration`（発火ビート）で authored に語る — ビートの GM 還流に
  より、GM は「誰が吊られたか」を知った上で次の議論を語れる。
- 占い等の**情報系役職アクションは op 化しない**（Phase D まで維持）: 占い結果は「世界状態」
  でなく「当人の知識」であり、GM は secret 属性の真値を知っているので語りで正しく処理
  できる。正本に置くべきは死亡・票のような**世界の事実**だけ（境界の維持）。
  **既知の緊張点**: これは #23 と同じ「narration は非検証」の非対称を自ら抱える選択である。
  GM が誤った占い結果を語る・過去の占い結果を忘れる可能性は構造保証できないため、
  **Phase E の測定項目に「誤占い率」を含める**（破綻が大きければ役職アクションの op 化を
  再検討する）。

## Phase 分割

- **Phase A（engine）✅2026-07-03 実装済**: `role_assignment` — `role_rng`（seed ^ "ROLE_RNG" の
  専用ストリーム）による Fisher–Yates 決定論 shuffle + bookkeeping stat 自動生成（生存=1 /
  `生存{役職}数` を player に）+ validate（人数整合/幻キャラ/重複配布）。
  PoC green: `role_assignment_deals_roles_deterministically_without_touching_main_rng`（同 seed
  同配役・pool どおりの人数・**本流 cursor 0**・20 seed で複数配役）/
  `validate_rejects_role_assignment_mismatches`。
- **Phase B（提示層）**: `secret_attributes` — 宛先別フィルタ（GM=全員・秘匿注記 /
  player UI=本人のみ）+ プロフィールカード + GM_SYSTEM 演じ分け規律。
  PoC: DTO に NPC 役職が出ない / player 自身は出る / state_brief には注記付きで全員分出る。
- **Phase C（engine）**: `CastVote` + `ResolveVote` — 開票の決定論 / 同数の seeded 抽選 /
  LLM の resolve_vote 却下 / 死亡の原子適用（生存 stat + presence 投影 + カウンタ・差分 stat
  再計算 + 票リセット）。
  PoC: 全項目 Red→Green（判定基準は上記「生存表現の役割分担」）。
- **Phase D（content）**: ドッグフード盤面 `packages/gnosia_village/`（5〜6 キャラ、
  フェーズ進行、役職 hint、勝敗 goal）。
- **Phase E（実測・核心的未知）**: 実 LLM で議論 5〜10 ターンを通しプレイし計測:
  ①**役職漏洩率**（地の文での直接言及/決定的示唆）②**誤占い率**（GM が secret 属性と
  矛盾する占い結果を語る）③**演じ分けの破綻**（死者の発言等 — presence 接地が検出基準）。
  強モデル前提コンテンツと割り切るか、弱モデル向け緩和（役職公開バリアント）を用意するかを
  ここで判断。

## スコープ外・将来拡張

- **NPC ごとの知識分離の構造保証**（per-NPC の LLM コンテキスト分離）: 単一 GM の演じ分け
  （prompt 規律）で始める。Phase E で破綻が大きければ将来 spec で多コンテキスト化を検討。
- **entity 別の秘匿例外**: 初版の `secret_attributes` はキー単位一律。「霊媒師は死者の役職を
  知る」のような役職別の開示は GM の語り処理に委ねる。**将来この例外を構造化する場合、
  secret_attributes を entity 別の可視性宣言に拡張する必要がある**（ここに明記して先送り）。
- グノーシアのループ（周回で情報を持ち越す構造）: campaign/transition と相性は良いが本 spec 外。
- 投票 UI（GUI ボタン等）: まず自然言語行動（「アリスに投票する」）で。

## 未決（査読事項）

1. **弱モデル向けバリアント**: Phase E の漏洩率次第で「役職公開モード」（secret を外して
   同じ盤面を回す）を用意するか。判断は Phase E 後。
2. **`role_rng` の導出方式**: seed + 固定ラベルの splitmix 等、具体アルゴリズムは Phase A の
   実装時に確定（決定論・本流非干渉という要件のみ凍結）。
