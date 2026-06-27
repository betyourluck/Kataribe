# 03. 知識フラグのヒント — 会話で立つフラグを LLM に見せる

Status: **Done（prompt-layer 実装済・PoC green）** / 2026-06-27
Scope: 「〇〇を知る」のような**会話で立つ知識フラグ**を、LLM が確実に `set_flag` できるよう
prompt に surface する。弱モデル向けロバスト化（同人配布の北極星）。

## 北極星整合

- **gm_core は純粋のまま**: `flag_hints` は**非検証の語り素材**（`world`/`protagonist.profile` と同類）。
  engine は値を解釈しない。ただしキーは `allowed_flags` 宣言必須（幻フラグへのヒントを load 時に弾く）。
- **prompt 層だけ**: surfacing と GM_SYSTEM 指示のみ。adjudicate/apply は無改修。

## 問題（prompt 層の穴）

`scenario_brief` は `allowed_flags` を一覧にしない。フラグが LLM の目に入るのは:
1. 下流の gate（アイテム取得条件・出口・挑戦 requires・クリア条件）に出るとき（`gate_brief` 経由）。
2. `state_brief` で**既に true** のフラグのみ。

→ **下流参照のない純粋な知識フラグは、立つ前は不可視**。「賢者から鍵の在処を聞いた」を後で
何の前提にもしていなければ、LLM はそのフラグの存在を知らず立てない。命名が自己説明的なら強モデルは
推測で立てるが、弱モデルは不安定。

## 設計（既存機構との分業 — 拡張は最小）

「LLM に確実に立てさせる」は既存機構では本質的に届かない:
- `flag_rules` gate = **守る**バックストップ（前提未達の `set_flag` を `FlagGateUnmet` で却下）。
  だが LLM を**促す**ことはしない。
- authored トリガー = 行動・場所に紐づく学びには最適（決定論発火）。だが**純粋な会話イベント**
  （賢者が口で教える）は状態条件に乗らないので使えない。

残る狭い穴 = **会話で立つ知識フラグ × 弱モデル**。ここに `flag_hints` を足す。

```
Scenario.flag_hints: BTreeMap<FlagKey, String>   # flag → 「立てる条件」の説明文
```

- **opt-in**: 作者が選んだフラグだけ surface する（全 `allowed_flags` を撒くと、authored トリガー/
  challenge 専権フラグまで LLM に直接立てさせたくなる誘惑＝ノイズ。opt-in で回避）。
- **`flag_rules` と対で使うのが定石**: ヒントが**促し**、gate が**守る**。
  ヒントを見て LLM が早まって立てても、gate が未達なら却下される（早まり対策）。

### prompt surfacing

`scenario_brief` に「## 状態フラグ (条件が満たされた瞬間に set_flag で立てる)」節。
GM_SYSTEM に「盤面に状態フラグが列挙されていたら条件成立の瞬間 set_flag、ただし先回りで立てるな」。

### 定石（authoring）

```yaml
allowed_flags: [met_sage, 知った_鍵の在処]
flag_hints:
  知った_鍵の在処: 賢者から鍵の在処を聞いたら立てる   # 促し
flag_rules:
  知った_鍵の在処: { kind: flag_is, key: met_sage, value: true }  # 守り（賢者に会う前は却下）
```

- **会話で立つ知識** → `flag_hints`（promote）+ `flag_rules`（guard）
- **行動・場所に紐づく学び** → authored トリガー（`when: location_is library` → `set_flag`）
- どちらも `allowed_flags` で閉世界。立った後はエンジンが gate を強制（知ったフリで越えられない）。

## PoC（Red→Green）

- gm_core `validate_rejects_undeclared_flag_hint`: `flag_hints` のキーが `allowed_flags` 未宣言なら
  `ScenarioError::FlagHintUndeclared` で弾く。
- harness `scenario_brief_surfaces_flag_hints_and_gm_system_demands_setting`: flag_hints が brief に出て、
  GM_SYSTEM が条件成立時の set_flag と先回り戒めを刷り込む。

## スコープ外

- 弱モデルでの実効性は実 LLM プレイで計測（強モデルは命名で既に立つので差が出にくい）。
- ヒントの多言語化（現状は authored 文字列そのまま）。
