# 08. chronicle の retrieval 化 — 長編でも「忘れない GM」

Status: **Phase A+B Done（2026-07-08 同日実装・PoC green。実 LLM プレイでの想起の効き・ノイズ率の計測が残）**
Scope: chronicle（GM の中期記憶）の注入を「新しい方優先の打ち切り」から
「直近 K 無条件 + 古い経緯の関連想起」の二層へ。gm_core は**無改修**（harness + 呼び出し側のみ）。

## 問題（長編でだけ破れる「忘れない GM」）

現行 chronicle は GM の summary 1 行 × 全ターンを蓄積し、`history_note` が**文字予算 2400・
新しい方優先**で注入する。1 エントリ 60〜100 字 → 収まるのは直近 25〜30 ターン。
40〜60 ターンの 1 時間セッションでは**序盤の経緯が「(それ以前の経緯は省略)」に潰れる** —
売り（忘れない・矛盾しない）が長編でだけ静かに破れる。

## 決定（2026-07-08 ユーザー査読済み）

**A+B を Phase 1 で実装、C（章要約圧縮）は Phase 2 へ延期。**

### A. Retrieval 型二層注入（本命）

- 予算を二分: **直近 K ターンは無条件で全文**（連続性の保証、現行と同じ）+
  **それより古い経緯から関連 M 件だけ想起**。
- 関連度: `LoreStore` の**文字 bigram TF-IDF cosine を流用**（依存ゼロ・決定論・テスト済み）。
- クエリ: `プレイヤーの行動文 + 現在地 LocationId + present EntityId 列`。
- 重み: **location 一致 ×2.0、present entity 重なり ×1.5**、テキスト類似はそのまま ×1.0
  （エントリ文書にタグ ID も含めるので、タグだけの一致でも bigram 重なりで底が付く）。
- 予算配分: 総 2400 字に対し **直近 60%（約 1440 字、目安 18 ターン）/ 関連 40%
  （約 960 字、上限 10 件）**。**関連 0 件時は直近が全予算へ自動拡張**（= 現行挙動）。
  履歴全体が総予算に収まるなら全文（retrieval 不要、現行と同一出力）。
- 追加 API コストゼロ・決定論・弱モデルでも動く。

### B. エンジン機械タグ付け（A の精度の土台）

`TurnLog` に harness が**受理時点で機械的に確定記録**する（LLM の summary に依存しない
engine 事実 = 検索の接地）。全フィールド serde default = **旧セーブ（spec 07）互換**:

- `location: LocationId`（適用後の現在地）
- `present: [EntityId]`（適用後にその場に居た NPC）
- `flags_set: [FlagKey]`（このターンに真化したフラグ = `flag_turns` の差分、
  op / トリガー効果 / challenge 帰結の全経路捕捉）
- `checks: [String]`（技能判定の要約「STR 1d20+3=17 vs DC15 成功」）
- `items: [String]`（所持品の増減 = apply 前後の inventory 差分、「+祠の鍵」「-回復薬」）

計上は `run_turn` 内（apply の前後を見られる唯一の場所）→ `TurnOutcome::Accepted.tags`
で運び、`chronicle_entry` が `TurnLog` に焼く。呼び出し側 (CLI/app) の変更は引数の糸通しのみ。

### C. 章要約への階層圧縮（Phase 2・見送り）

溢れた古い経緯を LLM に「3 行の章要約」へ圧縮させる案。**見送り理由**: 追加 API コスト・
非決定論・弱モデルの圧縮品質。**再検討条件**: A+B 実装後の実プレイで「関連想起で拾えない
重要伏線」が定量的に確認できたら Phase 2 として起票。

## 北極星との整合

- **可変状態の正本はあくまで `GameState`**。chronicle（TurnLog）は「確定した語り素材
  （不変の記録）」であり、可変世界状態は持たない（Memoria と同じ境界）。
- A が選ぶのは「**確定済みの記録のどれを prompt に見せるか**」だけ。直近 K の無条件
  ブロックが連続性を保証し、relevance 層は**現状なら完全に捨てられていた古い経緯を
  追加で救う方向にしか働かない** — 「曖昧な recall が忘れる GM を再現する」掟には
  抵触しない（厳密に現行の上位互換）。
- B のタグは engine の事実（location / presence / flag_turns / inventory / checks）の
  写しであり、LLM は関与できない。

## 実装（Phase A+B、PoC = Red→Green）

1. `harness::memoria::LoreStore` に pub(crate) スコア API（recall と共用、DRY）。
2. `TurnLog` タグ 5 種（serde default）+ `ChronicleTags` + `TurnOutcome::Accepted.tags`
   + `run_turn` の計上（inventory snapshot / flag_turns 差分 / 適用後 location・present）。
3. `history_note(history, query)` 二層化（`run_turn` がクエリを組む — 呼び出し側無改修）。
4. CLI / app の `chronicle_entry` 呼び出しへ糸通し。
5. PoC:
   - 60 ターン合成セッションで**序盤 (T3) のアイテム入手が終盤 (T60) の関連行動で想起される**
     （ユーザー指定の測定ケース。決定論なので成功率 = assert）
   - タグの機械計上（flag/item/location/present が Accepted に載る）
   - 関連 0 件時は現行挙動（直近が全予算）
   - 旧セーブ（タグ無し TurnLog yaml）の deserialize 互換
- 実プレイ計測（実 LLM での想起の効き・ノイズ率）は実装後の通しプレイで。

## 決定パラメータ（rev1）

| 項目 | 値 |
|---|---|
| 総予算 | 2400 字（現行維持） |
| 直近層 | 60%（約 1440 字） |
| 関連層 | 40%（約 960 字）・上限 10 件・関連 0 件時は直近へ譲渡 |
| location 一致 | スコア ×2.0 |
| present 重なり | スコア ×1.5 |
| 足切り | cosine 由来スコア 0.05 未満は出さない（ノイズ遮断、チューニング可） |
