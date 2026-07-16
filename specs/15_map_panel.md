# 15. マップパネル — 訪問済み＋1歩先の有向グラフ（右ペイン第4タブ）

Status: **Phase A+B 実装済（2026-07-16。可視範囲=霧をユーザー確定）。Phase C=GUI 目視が残。**
Scope: **app 提示層のみ** — 右ペインに「マップ」縦タブを追加し、ロケーションの有向グラフを
SVG で描く。**gm_core / harness / 正本 / prompt は無改修**（マップは既存データからの派生表示）。

## 動機

プレイヤーが「どこへ行ける?」を GM に尋ねる往復（＝1ターン = フルプロンプト1往復）を減らす。
GM の prompt は **既に**出口を知っている（#42 で `state_brief` に「いま通れる出口」を動的 surface
済み）ので **prompt は不変** — 効くのは**プレイヤー側の探り行動の削減**という間接的トークン節約。
マップは「今どこにいて、どこへ行けて、その先どう繋がるか」を一目にする可視化。

## 北極星との整合

- **engine 無改修**: マップは `Scenario.exits`（有向グラフそのもの）+ `GameState`（現在地・gate
  評価）+ chronicle（`TurnLog.location` = 訪問済み）から導く**派生表示**。可変状態を増やさない
  （訪問済みは history 由来 = `GameState` に `visited` を足さない）。gate 評価は既存 `Gate::eval`。
- **ネタバレ制御 = 霧（fog of war）**: 探索の発見を殺さない（北極星「矛盾しない GM」の体験版）。
  全図/作者制御はスコープ外（将来、必要になれば `Location` に秘匿フラグを足す）。

## 可視範囲（霧）— ユーザー確定

- `visited` = `{start, 現在地}` ∪ `{history の各 TurnLog.location}`、**現 scenario に存在する
  ロケーションに限定**（campaign 遷移で前モジュールの location が history に残るのを除外）。
- `frontier` = `visited` の各ノードの `exits.to`（**1歩先。未訪問でも名前を出す**）。
- `nodes` = `visited` ∪ `frontier`。**奥（frontier からさらに先）は霧 = 出さない**。
- `edges` = `visited` ノードから出る `exits` のみ（frontier からの辺は描かない = その先は霧）。

## gate 表示

各辺の gate を backend が評価（`exit.gate.eval(&state)`）:
- `from == 現在地` かつ通れる → **「いま行ける」**（強調・実線矢印）。
- gate 未達 → **🔒（破線・今は不可）**。行き先名は出す（1歩先の存在）が **gate 条件文は出さない**
  （「master_key が必要」等はネタバレ）。
- その他（通れるが現在地からでない構造的な繋がり）→ 淡い実線。

## データ（data_contract 追記）

```
MapNode  { id, title, current: bool, visited: bool }   # visited=false は frontier（未踏の1歩先）
MapEdge  { from, to, locked: bool }                    # locked = gate 未達（🔒）
MapView  { nodes: [MapNode], edges: [MapEdge] }
```
`GameView.map` / `TurnView.map` に載せる（開幕 + 毎ターン。移動で現在地・可視範囲が変わる）。
却下ターンは state 不変ゆえマップも不変（現状スナップショットを返す）。

## レイアウト（frontend）

現在地起点の **BFS 距離でランク列**（x = rank × 間隔）、同ランクを縦に等分配（y）。SVG で
有向辺を矢印描画。ノード数は数〜十数なので簡易レイアウトで足りる（force-directed は過剰）。
現在地ノードを ember で強調、frontier（未踏）は淡く破線枠、🔒 辺は破線。

## 実装（Phase）

- **Phase A — backend `map_view(scenario, state, history) -> MapView` + DTO + PoC**:
  霧の範囲（visited ∪ frontier）・gate ロック・campaign で他モジュール location を除外。
  PoC: `map_view_shows_visited_and_one_hop_frontier_hiding_the_rest` /
  `map_view_marks_locked_exits_and_current`。
- **Phase B — frontend `MapPanel.vue`（SVG）**: 第4タブ「マップ」（Ctrl+4）+ `Icon(map)` +
  i18n（ja/en）+ types。`activeTab` 拡張、Ctrl+Tab 巡回に追加。
- **Phase C — GUI 目視**: レイアウト・現在地強調・🔒・移動でのマップ更新・campaign 遷移で
  マップが遷移先グラフに差し替わる。

## スコープ外

- モジュール跨ぎの全体マップ（章をまたぐ地図）。
- 手動レイアウト（作者が座標指定）・立ち絵的な意匠。
- 作者の per-location 秘匿（可視範囲は霧に固定。全図/作者制御は将来 spec）。
