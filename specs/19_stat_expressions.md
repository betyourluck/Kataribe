# spec 19: 式修正 — 判定の修正値/目標値を stat の式で書く

**Status**: Done（2026-07-20 起草 → 同日実装。実 LLM 影響なし = prompt/schema 不変）
**動機**: CoC7 のキャラ作成ガイド（note 記事）を受けたユーザー要望 —
「割り振りは YAML 手書きでいい。**ダイスのときの演算**が欲しい（CON+SIZ/2 で補正とか）」。

## 何を作ったか

`(CON + SIZ) / 2` のような**整数式**を判定の修正値（additive）/ 目標値（percentile）に
書ける。評価は **engine が判定のたびに現在値で行う** — 手書きの派生値と違い、
CON が削られれば補正も落ちる（生きたシート）。

```yaml
challenges:
  club:
    description: 棍棒で殴る
    expr: "(CON + SIZ) / 2"     # additive: 修正値になる
    sides: 20
    dc: 15
  dodge:
    resolution: percentile
    expr: "DEX * 2"             # percentile: 目標値になる
contests:
  grapple:
    opponent: ghoul
    player_roll: { expr: "(STR + SIZ) / 4", sides: 20 }   # RollSpec にも書ける
    opponent_roll: "組みつき"
```

## 設計（北極星整合）

- **authored 専権**: 式を書けるのは YAML（`ChallengeDef.expr` / `RollSpec.expr`）だけ。
  LLM は challenge/contest を「選ぶ」だけで式を持てない — 既存の閉世界と同じ線。
- **閉世界**: 式が参照する stat は判定主体の宣言済みキー必須。load 時 validate
  （`ChallengeExprInvalid` / `ContestRollInvalid`、既定主体 = `entity` or player）+
  裁定時の二層目（op の entity 上書きで主体が変わるケースは `UnknownStat` で還流）。
- **決定論**: 整数演算のみ（`+ - * / ( )` と単項マイナス、全角演算子 `× ÷ （）` も受理）。
  除算は 0 方向切り捨て（CoC の端数切り捨て準拠）。リテラル `/0` は load 時に弾き、
  実行時の stat 由来ゼロ除算は 0 に倒す（判定を落とさない安全側）。
- **`stat` と排他**: 併記は validate が弾く（どちらが効くかの曖昧さを作らない）。
- **提示層**: 式 challenge の `CheckOutcome.stat` は空のまま（式は素性であって表示名でない。
  提示は description が担う）。prompt/schema は不変 = キャッシュ影響ゼロ。

## 実装

- `gm_core::expr`（新モジュール）: トークナイザ + 再帰下降パーサ + `Expr::eval`/`stats()`。
  依存ゼロ・純関数。日本語 stat 名可（演算子・括弧・空白以外の連続文字）。
- 適用 3 箇所を `stat_or_expr()` に集約: challenge additive 修正 / percentile 目標値 /
  contest の `roll_side`。
- PoC: expr 単体 2 本（文法・評価・切り捨て・全角 / stats 列挙・静的エラー）+
  engine 2 本（**生きた派生値** = CON を削ると次の判定から補正が落ちる /
  validate 3 種 + contest 側の健全性）。

## スコープ外（据え置き）

- DB（ダメージ・ボーナス）の**帯テーブル**（STR+SIZ 65–84 → −1 等）: 線形式では書けない。
  既存の `modifiers`（Gate 条件つき bonus）で帯を書けるので機構追加は需要が出てから。
- 派生 stat の**初期化**（HP=(CON+SIZ)/10 を自動計算して seed）: ユーザー決定で作者の
  手書きに委ねる（キャラメイクは Kataribe の外）。
- `flag_rules`/Gate への式（`stat_at_least` の value を式に等）: 使い道が出たら。
