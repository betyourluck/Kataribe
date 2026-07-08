# 05. シナリオ配布サイト — packages の zip マーケット（第一歩）

Status: **サーバ側着地済（outcast Spec 23、デプロイ残）／ Kataribe 側 Phase C（GUI 統合）✅実装済（2026-07-08、GUI 実機の目視確認のみ残）** / 2026-07-02 起草 → 2026-07-08 改訂
Scope: `packages/<name>/`（自己完結フォルダ）を zip で配布するサイト。

実装の正本は **outcast リポジトリの `specs/23_kataribe_package_uploader.md`**（rev5）。
本 spec は **Kataribe 側から見た契約**（zip 形式・検証責務の分担・GUI 統合）を凍結する。
サーバ実装の詳細（DB スキーマ・認証委譲・インフラ）はあちらの台帳を参照。

## 北極星整合

- **Package B-core の帰結**: 配布単位は既に「zip→解凍→そのまま動く」自己完結フォルダとして
  凍結済み（`package.yaml` + `scenarios/` (+`characters/` `memoria/` `images/` `audios/` `campaign.yaml`)）。
  本 spec はその**流通経路**を作るだけで、Kataribe engine（gm_core/harness）は無改修。
- **受領側の正本検証は既存のまま**: DL したパッケージも結局 `load_package` /
  `load_campaign_package` が読み、閉世界検査（validate）を通る。サイト側の検証は
  **前置きフィルタ**であって正本ではない（正本 > 配布メタ）。
- **同人配布の北極星**: 受領者は安い/ローカルモデルで遊ぶ想定（no-tools #29 と同層）。
  配布物に API キーや engine バイナリは含めない。パッケージ＝データのみ。

## 着地したもの（outcast Spec 23、2026-07-07 実装・デプロイのみ残）

サイト「**Kataribe 書庫**」= `kataribe.outcasts.jp`。outcast リポジトリの
`kataribe/backend`（Axum + Postgres 別 DB）+ `kataribe/frontend`（Vue3 + Tailwind、
古書店・羊皮紙テーマ）の**独立 Docker コンテナ**。起草時の「outcast backend に同居」から
**スタンドアロンサービスへ変更**（Kataribe 側の障害・脆弱性が Outcasts 本体を汚染しない疎結合）。

- **認証 = 検証委譲（C-017）**: 独自認証を持たない。セッションクッキーの domain を
  `.outcasts.jp` に拡張し、書込系 API のみ Cookie を Outcasts `/api/me` へ内部転送して
  本人確認。停止・失効の判定は Outcasts に一元化。閲覧/一覧/DL は無認証・委譲呼び出しゼロ。
  起草時の「Master session 同居」案より疎で良い形に着地。
- **アップロード**: 登録ユーザーのみ。検疫 dir → zip 検証 → 正規化 → 公開の順。
  1 ユーザー 1 日 10 件・1 ファイル 100MB・ディスク残量ガード。
- **spec 05 起草時に無かった追加機能**: レビュー（★1〜5 + 感想 500 字、1 人 1 件 UPSERT・
  自己採点禁止）／DL 数ランキング（週間/月間/累計）／固定 6 カテゴリ／**Mature フラグ**
  （性・流血の自己申告バッジ。倫理制約の強い LLM では**プレイできない可能性**の目印＝
  嗜好情報であると同時にプレイアビリティ情報）／詳細ページ（軽量、短編 2〜3 時間推奨の文化文言）。
- **制作ガイド `/guide` + `package_spec.md`**: あらすじ + 仕様 md を LLM に渡せばパッケージ
  yaml がほぼ自動で作れる**制作パイプラインの入口**。仕様 md は固定 URL
  `/package_spec.md` で生 markdown 配信（llms.txt の流儀）。実効性検証済（2026-07-07、
  生成パッケージ「記憶の檻」が GUI でそのままロード・起動）。
  **⚠運用義務**: エンジンの data_contract / 作者向け仕様を変えたら
  `outcast/kataribe/frontend/public/package_spec.md` の追従修正が要る（CLAUDE.md 末尾に同旨）。

## zip 契約（Kataribe 側が依存する事実）

受領側（GUI Phase C・手動 DL の双方）はこの契約を前提にできる:

1. **配布物は常にフォルダ包み形（Wrapped）**: `<フォルダ>/package.yaml` の 1 段構造。
   直下形（Flat）のアップロードも受理されるが、サーバが**再圧縮なしのエントリ名リネーム**
   （raw copy）で正規形に統一してから保存する。**展開ロジックは Wrapped 前提で書いてよい**。
2. **サーバの受入検証（前置きフィルタ）**: zip として可読／実効ルートに `package.yaml` と
   `scenarios/` が実在／zip slip なし（`..`・絶対パスは `enclosed_name` で一括拒否）／
   暗号化 zip 拒否／zip bomb 上限（展開後合計 500MB・エントリ数 10,000）／
   **拒否拡張子 denylist**（exe dll so dylib bat cmd com scr msi ps1 vbs sh jar app **js wasm**、
   大文字小文字無視。allowlist 案から反転＝ユーザー決定 2026-07-07。パッケージはデータのみで
   コードを持たない前提。Kataribe 側仕様がスクリプト同梱に進化したら Spec 23 の改訂として扱う）。
3. **`package.yaml` の中身は parse しない**: 存在確認のみ。title/description はフォーム自己申告が正
   （起草時の「manifest から抽出＝単一真実源」案は棄却 — メタ自動抽出は将来スコープ外）。
   entry の指すファイルの実在も見ない。**壊れたパッケージは DL 後の `load_package` で初めて
   発覚しうる**（二層原則どおり正本検証は受領側。サイトは構造 + 安全性のみ）。
4. **sha256/file_size は正規化後の配布物基準**（Flat アップロード原本とは一致しない）。
5. DL は `GET /api/packages/:id/download`（無認証、`application/zip`、
   `Content-Disposition: attachment; filename="kataribe-<id>.zip"`）。
   id は ad-safe UUID（広告ブロッカー対策で "ad" を含まない）。

## Phase C — Kataribe GUI 統合（✅実装済 2026-07-08、実機目視のみ残）

- **PackageDialog を 2 タブ化**（ローカル / 配布サイト）: 書庫の一覧 fetch
  （`GET /api/packages?category=&q=&sort=&page=`、検索・6 カテゴリ・3 種ソート・
  ページネーション・★/DL数/サイズ/Mature バッジ表示）→「取得」で zip DL →
  **`app_data_dir/packages/` に展開**（spec 07 saves と同じ流儀＝repo を汚さない）→
  `packagePaths` へ自動登録（以降は既存経路）。
- **HTTP は backend（Rust）が担う**: `fetch_site_packages` / `install_site_package` の
  Tauri command 2 つ。CORS 回避 + DL 上限 110MB のストリーム受信 + 一時ファイル検疫。
  一覧 DTO は必須フィールドで受ける（寛容な deserialize は失敗を隠す — Grok 空デルタの教訓）。
- **クライアント側 zip 検証 `app/src-tauri/src/site.rs`**（サーバを信用しない二層）:
  zip slip（enclosed_name 一括拒否）/ Wrapped 以外拒否（契約 1。Flat・複数トップ・
  package.yaml 欠落は改竄シグナル）/ 拒否拡張子 denylist（サーバ F8 の鏡）/
  zip bomb 上限（同 F7 の鏡）/ 暗号化拒否。展開先の同名衝突は `名前_2` で回避
  （再取得で旧フォルダを上書きしない — 進行中セーブの参照先を壊さない）。
  展開後に `read_manifest` が読めなければフォルダごと撤去（恒久エラー行を作らない）。
- サイト URL は設定項目（`localStorage["kataribe.siteUrl"]`、既定 =
  `https://kataribe.outcasts.jp`、自前サーバも指せる＝Outcasts 固有ロックインを避ける）。
- engine semver: manifest の `engine` はサーバが読まないため、互換警告は
  **展開後の受領側**（既存の `load_package` 警告と同じ層）のまま。
- **PoC**: unit 7 本（zip slip 遮断・Flat/複数トップ/manifest 欠落の拒否・拒否拡張子・
  正常展開・衝突回避 `_2`）+ opt-in 統合 2 本（`--ignored`: 実 dev 書庫の一覧 DTO
  deserialize / モック書庫相手の DL→検証→展開→manifest 読み end-to-end、
  sealed_shrine の実 zip で green）。frontend vue-tsc + vite build green。
- **実機残**: GUI 起動しての目視（タブ表示・取得ボタン→ローカル一覧に現れる→new_game）。
  dev 書庫の DL は統合テスト残骸行（実ファイル無し＝500）でなく実パッケージを
  アップロードしてから。

## 査読事項の決着（起草時の未決 6 件）

1. **検証コードの共有** → 案 A よりさらに疎に決着: サーバは manifest を**そもそも読まない**
   （構造 + 安全性のみ）。リポジトリ間依存ゼロ、共有 crate 不要。
2. **DL の公開範囲** → 完全公開・無認証。DL カウントは**素朴カウント（dedup なし）+
   JST 日次集計**。操作対策は運用データが取れてから（ユーザー決定 2026-07-07）。
   ⚠既知の含意: ランキングは DL 数由来なので水増し可能な指標（日次テーブルが残るので
   後から dedup / 異常検知を足す余地あり）。
3. **名前空間** → slug 自体を廃止。id = ad-safe UUID、タイトル重複は許容
   （表示名 + uploader_display_name スナップショットで識別）。
4. **engine semver** → サーバ側では扱わない（上記 Phase C 参照、受領側の責務）。
5. **課金有無** → スコープ外のまま（導入するなら別 spec）。
6. **セッション揮発性** → MemoryStore のままで問題なし（Kataribe はストアを読まない委譲方式。
   クッキー domain 拡張の移行の綾＝既存ログインは再ログインまで飛ばない、は Spec 23 Notes）。

## 運用ノート

- **公開ゲート**: エンジン本体リポジトリは未公開（2026-07-08 時点）。公開して配布物ビルドが
  立ったら書庫の `/guide` からエンジン入手先リンクを張る。それまで本番デプロイは保留
  （Spec 23 Notes「Kataribe エンジンへのリンク（保留中）」）。
- 軽微な既知事項: `download_package` は DL カウント加算後にファイルを open するため、
  ファイル欠損時もカウントが増える（報告のみ、実害小）。
