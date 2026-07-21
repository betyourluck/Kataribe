# spec 21: 確定行動 — ダイスを振らない challenge (`resolution: none`)

**Status**: Done（2026-07-21。ユーザーが真因を特定して起票 → 同日実装。
gm_core 144 / workspace 301 green、clippy clean。実 LLM プレイでの採用率は実測残）

## 動機（機構の穴）

Gemini が生成するシナリオに、この形が繰り返し現れる:

```yaml
equip_hanakanmuri:
  description: 【装備する】地縛りの花冠
  requires: { ... }
  sides: 1
  dc: 1                 # 1d1 >= 1 = 必ず成功 = ダイスを装ったスイッチ
  on_success: { flag: eq_hanakanmuri, narration: コンの頭に花冠を乗せた。 }
```

当初は「作者向け仕様の書き方の問題」と読んだが、**真因は機構の語彙不足**だった
（2026-07-21 ユーザーが特定）。

**LLM が authored 効果を起動できる経路は `attempt_challenge` しかない。** トリガーは engine が
発火させるもので LLM からは触れず、`set_flag` 経由でトリガーを起こす定石は次の理由で破綻する:

```yaml
triggers:
  - id: wear
    when: { kind: flag_is, key: eq, value: true }
    effects:
      - { op: adjust_stat, ... }
      - { op: set_flag, key: eq, value: false }   # 「戻す」= 繰り返し可能にするための定石
```

`authored_only_flags()` は**トリガー効果の `set_flag` を書き先の意図に関係なく全部集める**ので、
このリセット 1 行で `eq` が専権フラグに落ちる → #50 のバックストップが**真偽どちらの向きも**
却下する → **LLM は二度と起動できない**。しかも語彙提示からも消えるので、作者からは
「書いたのに GM が使ってくれない」という静かな死に方をする。

「セットして戻す」は台帳が定石として書いてきた書き方だが、あれは**engine 駆動のループ**
（カウンタが閾値→効果→リセット）用だった。**起点が LLM の場合だけ、リセットが起点の権利を
食い潰す**。両立しない。

つまり `sides: 1 / dc: 1` は、無い primitive を偽のダイスで代用していた唯一の抜け道。しかも
percentile 盤面では `sides`/`dc` が validate で拒否されるため、**CoC 盤面ではその抜け道すら塞がっている**。

## 何を作るか

```yaml
challenges:
  equip_hanakanmuri:
    resolution: none              # 判定なし = 必ず成功する確定行動
    description: 【装備する】地縛りの花冠
    requires:
      kind: all
      of:
        - { kind: has_item, item: 地縛りの花冠 }
        - { kind: flag_is, key: eq_hanakanmuri, value: false }
    on_success:
      flag: eq_hanakanmuri        # 専権フラグを engine 側から立てられる
      narration: コンの頭に花冠を乗せた。
      effects: [ ... ]            # authored 効果 (trigger effects と同じ信頼モデル)
```

**位置づけ = 「LLM が起動できる authored 効果の束」**。閉世界は不変 — LLM は challenge を
**選ぶ**だけで、帰結 (flag/effects/narration) はすべて authored 側にある。

## 決定事項

| 項目 | 値 | 根拠 |
|---|---|---|
| enum 値 | `Resolution::None` (`resolution: none`) | `additive`/`percentile` と同じ列。「判定を行わない」を素直に表す |
| 判定 | 行わない。**RNG を消費しない** | 決定論。確定行動の有無でダイス列が変わらない (role_rng と同じ思想) |
| 帰結 | `on_success` のみ適用 (flag → effects → narration) | 失敗が存在しない |
| `CheckOutcome` | **出さない** | 🎯 行も伏せカード (spec 18) も出さない。判定していないものを判定に見せない |
| narration | `CheckOutcome` が無いので**発火ビートと同じ経路**で提示する (`FiredTrigger` 相当) | 結末文を捨てない。GM への還流も既存経路に乗る |
| 禁止フィールド | `sides`/`dc`/`count`/`times`/`stat`/`expr`/`tiers`/`modifiers`/degree スロット/`on_failure`/`pushable`/`spend_rules` | 判定が無い以上どれも無意味。load 時に `CertainActionShape` で弾く (壊れた宣言を実行経路に乗せない) |
| `requires` | **使える** (むしろ本命) | 「条件を満たすと選べるようになる行動」を宣言でき、`scenario_brief` に【前提】が出る |
| spec 09 射影 | **完全に射影する** | 決定論ゆえ「装備する + 移動する」を 1 ターンに束ねられる (ダイス challenge は帰結が apply 時確定ゆえ必ずターンを割る) |
| 提示 | `scenario_brief` に「(判定なし・確定)」 | GM が「振らずに選ぶ手」だと分かる |

## 実装

- `Resolution::None` 追加。`ChallengeDef` は既存フィールドのまま (禁止は validate で表現)。
- `adjudicate`: `None` は面数検査 (`DiceSidesInvalid`) を飛ばす。
- `apply_ops`: 判定・凍結 (spec 18) を通らず `on_success` を直適用。`CheckOutcome` を積まず、
  narration は `fired` に `FiredTrigger { id: challenge, narration, .. }` として載せる。
- `guaranteed_challenge_effects` (spec 09): `None` は `on_success` を丸ごと確定扱い。
- `validate`: `CertainActionShape { challenge, detail }` — 禁止フィールドを名指しで弾く。
- `scenario_brief`: 「(判定なし・確定)」表示。

## PoC

1. `certain_action_applies_outcome_without_rolling` — flag/effects/narration が適用され、
   `checks` は空、**RNG が消費されない** (同 seed で後続のダイスが変わらない)
2. `certain_action_can_set_authored_only_flags_from_llm` — 動機そのもの: トリガーが書き戻す
   専権フラグを、LLM が `attempt_challenge` 経由で立てられる (直接 `set_flag` は却下のまま)
3. `validate_rejects_dice_fields_on_certain_action` — sides/dc/stat/tiers/on_failure 等を弾く
4. `certain_action_effects_are_projected_in_adjudication` (spec 09) — 「確定行動 + 移動」の
   束ねが 1 ターンで受理される

## 同梱: lint「未宣言 location への `location_is`」

同じ Gemini 生成物で `{ kind: location_is, at: inventory }` (inventory は場所ではない) を踏んだ。
`Gate::LocationIs` の `at` は validate も lint も検査していないため、**requires が永久に false =
その challenge は一度も選べない**のに警告ゼロだった。`unknown_key_lints` と同じ非 fatal 警告
（開幕 ⚠）で名指しする。LLM 生成 content でこの類推ミスは繰り返し出るので費用対効果が高い。

## スコープ外

- `resolution: none` の contest 版 (対決は本質的にダイス)。
- 「失敗しうる確定行動」(= それは判定なので additive/percentile を使う)。
