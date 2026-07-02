# 05. シナリオ配布サイト — packages の zip マーケット（第一歩）

Status: **Draft（査読待ち・実装未着手）** / 2026-07-02
Scope: `packages/<name>/`（自己完結フォルダ）を zip で配布するサイト。
アップローダは **outcast サーバ**（`D:/Github/outcast`、Axum + Vue の掲示板）に同居させ、
**アカウントは Outcasts の Master を流用**する（認証を新規実装しない）。
課金は本 spec のスコープ外（将来 Phase、下記「未決」参照）。

## 北極星整合

- **Package B-core の帰結**: 配布単位は既に「zip→解凍→そのまま動く」自己完結フォルダとして
  凍結済み（`package.yaml` + `characters/` + `scenarios/` + `images/` + `audios/` (+`campaign.yaml`)）。
  本 spec はその**流通経路**を作るだけで、Kataribe engine（gm_core/harness）は無改修。
- **受領側の正本検証は既存のまま**: DL したパッケージも結局 `load_package` /
  `load_campaign_package` が読み、閉世界検査（validate）を通る。サイト側の検証は
  **前置きフィルタ**であって正本ではない（正本 > 配布メタ）。
- **同人配布の北極星**: 受領者は安い/ローカルモデルで遊ぶ想定（no-tools #29 と同層）。
  配布物に API キーや engine バイナリは含めない。パッケージ＝データのみ。

## 問題（現状は手渡ししかない）

パッケージは GUI の `localStorage["kataribe.packagePaths"]` にローカルフォルダパスを
手入力して読む。作者が作品を届ける経路（アップロード→一覧→DL）が存在しない。

## 設計

### サーバ側（outcast に新モジュール）

outcast backend に `handlers/packages.rs` を新設。**既存パターンの流用**で新規機構を作らない:

- **認証**: アップロードは Master の session 認証（`upload_servant_avatar` と同型の
  `Multipart` ハンドラ、tower_sessions cookie）。閲覧/一覧/DL は公開（掲示板の閲覧と同じ）＋
  既存 rate limit 機構を DL にも適用。
- **保存**: zip は `uploads/packages/<id>/<version>.zip`（既存 `ServeDir::new("uploads")` が
  そのまま配信）。メタは DB テーブル `packages`
  （id / slug / title / description / author_master_id / version / engine / size / dl_count / created_at）。
  title/description/engine/version は **zip 内 `package.yaml` から抽出**（自己申告フォームと
  manifest の二重管理をしない＝単一真実源）。
- **受入検証（前置きフィルタ、セキュリティ必須）**:
  1. **zip slip 遮断**: エントリ名に `..`/絶対パス/ドライブレターを含む zip は拒否
     （harness `resolve_asset` の ID 検証と同じ思想をアーカイブ層で）。
  2. **zip bomb 対策**: zip サイズ上限・展開後合計サイズ上限・エントリ数上限。
  3. **package.yaml 実在 + parse**: ルート直下に `package.yaml` があり、必須フィールド
     （title/entry/engine）が読めること。entry が指すファイルの zip 内実在。
  4. **拡張子 allowlist**: `.yaml` / 画像（svg/png/jpg/webp）/ 音声（wav/ogg/mp3）のみ。
     実行可能物・アーカイブ入れ子は拒否。
- **API**（Servant API と同じ `/api/` 階層、ただし人間向けなので agent 名前空間外）:
  - `GET /api/packages?q=&page=` — 一覧・検索（title/description 全文）
  - `GET /api/packages/{id}` — 詳細（メタ + DL URL）
  - `GET /api/packages/{id}/download` — zip（dl_count++）
  - `POST /api/packages`（Master session + multipart zip）— 新規/新バージョン
  - `DELETE /api/packages/{id}`（作者 or admin）

### 検証コードの共有方法（査読事項①）

サーバ側で `package.yaml` を parse する時、Kataribe の型をどう使うか:

- **案A（推奨・疎結合）**: outcast は Kataribe に**依存しない**。manifest の必須フィールドだけを
  読む最小 struct を outcast 側に持つ（serde の数十行）。深い検証（閉世界・cast 整合）は
  受領側 `load_package` の責務のまま（二層の役割分担が明確、リポジトリ間依存ゼロ）。
- **案B**: `kataribe-manifest` を共有 crate に切り出し両方から使う（DRY だが、outcast の
  ビルドが Kataribe のリリースサイクルに縛られる。配布 > DRY の Package B-core 判断と逆行）。

### outcast frontend

- パッケージ一覧ページ（title/author/DL数/engine バージョン）+ 詳細 + アップロードフォーム。
  既存のダークゴシックテーマに乗せる。掲示板と独立したナビ項目「シナリオ配布」。

### Kataribe GUI 統合（受領側）

- 設定に「配布サイトから取得」: 一覧 fetch（`GET /api/packages`）→ 選択 → zip DL →
  **ローカル packages ディレクトリに展開** → `packagePaths` に追加（以降は既存経路）。
- 展開時に**クライアント側でも zip slip 検証**（サーバを信用しない二層）。
- サイト URL は設定項目（既定 = 公式、自前サーバも指せる＝ Outcasts 固有ロックインを避ける）。

## Phase 分割

- **Phase A（outcast backend）**: DB テーブル + 受入検証 + upload/list/download API。
  PoC: zip slip / zip bomb / manifest 不正の Red→Green + 正常 upload→DL 往復。
- **Phase B（outcast frontend）**: 一覧/詳細/アップロードページ。
- **Phase C（Kataribe GUI）**: リモート一覧 → DL → 展開 → 登録。PoC: 展開時 zip slip 遮断。
- **Phase D（運用）**: バージョン更新フロー・通報/削除（admin）・DL rate limit 調整。

実装 spec は **Phase A/B は outcast リポジトリの台帳**（あちらの spec 番号）で起票し、
本 spec は Kataribe 側から見た契約（zip 形式・検証責務の分担・GUI 統合）の正本とする。

## 未決（査読事項）

1. **検証コードの共有**: 案A（最小 struct 複製）で良いか。
2. **DL の公開範囲**: 完全公開 + rate limit で良いか（要ログインにすると受領障壁）。
3. **名前空間**: slug はグローバル一意か、`author/name` スコープか。
4. **engine semver**: 現状 manifest の `engine` は表示のみ。DL 時に GUI 側で互換警告を出すか。
5. **課金有無**: マネタイズ（有料パッケージ・決済）は本 spec 外。導入するなら別 spec。
6. **outcast セッションの揮発性**: tower_sessions が MemoryStore（再起動で消える）のは
   既知の増幅器。アップロード UX に影響するなら outcast 側で永続 store 化を先行するか。
