# 01. 没入のための画像・音声アセット

Status: **Locked（#1〜#6 合意済・実装待ち）** / 2026-06-26
Scope: packages に `images/` `audios/` を同梱し、場所背景・イベント CG・キャラ顔アイコン・BGM/SE を出す。

## 北極星整合

- **gm_core は純粋のまま**: アセット ID を不透明 `Option<String>` として運ぶだけ。パスを join せず・ファイルも読まない。`Location.description`/`Trigger.narration` と同じ「engine が解釈しない語り素材」カテゴリ。
- **解決は harness/app**: `load_package(dir)` で既知の package root を `GameSession` に保持し `root/{kind}/{id}` へ解決。
- **配布は zip→解凍で動く**: アセットはパッケージフォルダ同梱（`packages/<name>/images/`・`audios/`）。
- **可変状態にしない**: 「現在の背景」は state でなく提示層が turn 情報から導出する派生値。

---

## 決定事項（#1〜#6・合意済）

### #1 アセット参照の置き場 → gm_core に inline
`Option<String>` を各型に足すだけ。`serde` passthrough。別 `assets.yaml` は不採用（分散は事故る）。

### #2 Tauri 配信 → asset protocol 一択
- base64 IPC は画像/音声で毎回コピー＝肥大ゆえ不採用。
- `convertFileSrc(absPath)` → `asset://` URL を `<img>`/`<audio>`/CSS background が読む。
- **scope の綻びと対処**: scope は静的 allowlist だが、パッケージは localStorage の**任意パス**（`$APPDATA/.../packages/*` 固定にできない）。→ **ランタイムで動的追加**: `list_packages`/`new_game` でロードしたパッケージの dir だけを asset scope に許可（`app.asset_protocol_scope().allow_directory(root, true)` 相当）。ロード済みパッケージのみ許可＝安全かつ任意パス対応。persisted-scope は後で。
- CSP（現状 null）を**この機に締める**: `default-src 'self' ipc: http://ipc.localhost; img-src 'self' asset: http://asset.localhost; media-src 'self' asset: http://asset.localhost`。

### #3 イベント CG → **瞬間（momentary）** + overlay 予約
- イベント CG は**瞬間の絵（beat）**であって設定（setting）ではない。設定は location（持続・engine 接地）が担う。
- 発火ターンに背景を上書きして表示し、プレイヤーが読む間は残す。**次の受理ターンで場所背景へ復帰**する
  （却下ターンは物語が進んでいないので CG を保持）。CG は提示層が turn から導く派生値で、可変状態を持たない。
- `Trigger.image_mode: "background" | "overlay"`（任意・未指定=background）を足し、将来 overlay を実装コストゼロで選べるよう予約。
- **改訂経緯（2026-06-26 実プレイ発見）**: 当初は「背景上書き・**持続**（場所が変わるまで）」だったが、
  実プレイで「落石はもう起きたのに CG が祠を出るまで居座る」＝ beat を setting に昇格させた壁紙化が露呈
  （情景の二度出し #27 の視覚版）。実プレイ発見が pre-play の前提（案A 持続）を上書きした。
  恒久的な見た目の変化が要るなら CG でなく location（別場所/トランジション＝engine 接地）で表す。

### #4 キャラ presence → `Location.cast`
- `Location.cast: [EntityId]`、**空＝全 cast 表示**（後方互換）。「ここに居ないキャラはグレーアウト」が自然にできる。
- `CharacterDef.icon`/`Protagonist.icon` は**必須にせず fallback**（無ければ initials 表示）で配布ハードルを下げる。

### #5 アセット欠落 → 寛容 + warn
- load 時に `{kind}/{id}` が無ければ `warn!` ログ + UI スキップ（描画しない）。
- dev ビルドのみ `--strict-assets` で fail。
- **ID バリデーション**: `^[A-Za-z0-9._-]{1,64}$`（`/` と `..` を完全遮断＝traversal 対策）。

### #6 フェーズ順（体感インパクト順に微調整）
基盤 → **場所背景 → 顔アイコン → イベント CG → 音声**（音声は容量/著作権/フェード実装が重いので最後）。

---

## データモデル（gm_core 追加・全て任意 passthrough）

```
Location.image:      Option<String>   # 背景画像 ID（images/ 配下のファイル名）
Location.bgm:        Option<String>   # ループ BGM ID（audios/ 配下）
Location.cast:       [EntityId]       # この場所に居るキャラ。空=全 cast
Trigger.image:       Option<String>   # 発火時のイベント CG（FiredTrigger に載る）
Trigger.image_mode:  Option<"background"|"overlay">  # 既定 background
Trigger.sound:       Option<String>   # 発火時の SE
CharacterDef.icon:   Option<String>   # 顔アイコン ID
Protagonist.icon:    Option<String>   # 主人公の顔アイコン ID
```

ID は `^[A-Za-z0-9._-]{1,64}$` のファイル名のみ。gm_core は ID を解釈しない（不透明）。

---

## 解決の責務分界（harness）

- **単一関数に集約**: `resolve_asset(root: &Path, kind: AssetKind, id: &str) -> Option<PathBuf>`。
  - `kind = Images | Audios`（将来 voice 等へ拡張）。
  - ID を正規表現で検証（不正は `None`）→ `root/{kind}/{id}` を組み、存在すれば `Some`（欠落は `None`＋warn）。
- 提示層（app command）が `resolve_asset` → 絶対パス → `convertFileSrc` で URL 化して view DTO に載せる。gm_core はこの経路に関与しない。

---

## ビュー DTO（app）

- `GameView`/`TurnView`:
  - `background: Option<String>`（解決済み asset URL）。
  - `bgm: Option<String>`。
  - `BeatView` 拡張: `image: Option<String>` / `image_mode` / `sound`。
  - `present_characters: [{ id, name, icon_url: Option<String> }]`。
- `GameSession` に `package_root: PathBuf` を追加（解決の起点）。

## frontend 描画

- **背景**: 会話ペイン背景に `background`（CSS）。発火 CG はその受理ターンだけ場所背景を上書きし、
  次の受理ターンで場所背景へ戻る（瞬間。#3）。`background` は受理ターンのみ更新（却下は保持）。
- **顔アイコン行**: 右ペイン下部に `present_characters`。icon 無しは initials。**クリックでそのキャラの `StateView.entities`（stats/skills/attributes）をポップオーバー**（engine 改修ゼロ）。
- **音声**: `<audio loop>` で BGM（location 変化でクロスフェード）、SE は発火時 one-shot。音量/ミュート UI。
- **キャッシュ**: `convertFileSrc` は `Map<string,url>` でメモ化（毎回呼ばない）。

---

## フェーズ計画（各 Phase Red→Green、gm_core は純粋を維持）

- **Phase 0 — 基盤**: `GameSession.package_root` 追加 / `resolve_asset`（ID サニタイズ込み） / `tauri.conf` の assetProtocol enable + CSP 締め / ランタイム scope 動的追加。
- **Phase 1 — 場所背景**: `Location.image` → 背景。最小で没入が一気に上がる。
- **Phase 1.5 — 顔アイコン**: `CharacterDef.icon`/`Protagonist.icon` + `Location.cast` + 顔アイコン行 + クリック→ステータス（engine 改修ゼロの見せ場）。
- **Phase 2 — イベント CG ✅**: `Trigger.image` + `ImageMode`（background／overlay 予約）→ `FiredTrigger`/`FiredBeat`/`BeatView` を passthrough。frontend は**瞬間**（発火ターンに場所背景を上書き → 次の受理ターンで場所背景へ復帰、却下は保持。#3 改訂）。gm_core 純粋維持・PoC `trigger_image_passthrough_to_fired`/`trigger_image_mode_defaults_to_none`。ドッグフード= `sealed_shrine` の `awakening`/`rockfall` に CG。
- **Phase 3 — 音声**: `Location.bgm`（ループ・フェード）/ `Trigger.sound`（SE）。

---

## 検証物（ドッグフード）

`sealed_shrine` に画像を同梱して各 Phase を実体で確認:
- Phase 1: `gate`/`altar` に背景。
- Phase 1.5: `見習い冒険者` の顔アイコン（聖剣で魔法剣士に転職したら差し替えも検討）。
- Phase 2: `awakening`(転職) / `rockfall`(落石) のイベント CG。
- Phase 3: 祠の環境音 BGM。

（配布物への恒久テスト結合はしない方針＝使い捨てテストで動線確認、Phase ごとの PoC は gm_core/harness の合成シナリオで固定する。）
