# 17. パッケージ更新 — 配布サイト取得物の更新検知と上書き取得

Status: **Done（2026-07-20）** — Phase 0+A+C (2026-07-19) / B=outcast デプロイ・C 追補=二重取得の封鎖 (2026-07-19) / D=実書庫 e2e ユーザー確認 (2026-07-20)。rename リトライは失敗未観測につき見送り

> Phase 0+A 実装メモ (2026-07-19): data_contract `package_updates` 節を凍結。app に
> `update.rs`（SourceMeta / sha256_file / tree_hash / cleanup_leftovers）、install 経路に
> expected sha256 一致検証（不一致で中止）+ 出所メタ書き込み（tree_hash 込み・書けなくても
> 非致命）、extract に混入メタ skip、`list_packages` 冒頭に残骸掃除（app_data/packages のみ
> 走査）。`RemotePackage.sha256: Option` + frontend が install に expected を受け渡し。
> `installed_at` は unix 秒に変更（chrono 依存を足さない・表示は提示層が locale 変換 —
> rev2 からの唯一の意図的乖離）。PoC 5 本 Red 相当→Green（sha256 固定ベクトル / tree_hash
> 決定論+除外 / メタ roundtrip+破損 None / 掃除 3 分岐 / 混入 skip）。app backend 19 green。

> rev2 (2026-07-19): 査読 A×5 (実装を止める矛盾)・B×5 (決定論性の欠落)・C (未決への推奨) を
> 全反映。A-1 配信バイト列と DB sha256 の対応を契約に凍結 + install 時のサーバ申告検証 /
> A-2 スワップの二重構造を extract 先固定で解消 / A-3 クラッシュ残骸の掃除と自動復旧 /
> A-4 site_url の SSRF を「現在設定の siteUrl と一致時のみ照会」で遮断 / A-5+未決3 =
> 一覧+詳細の両露出で確定 / B-6 tree_hash の厳密定義 (hex・\n 固定・無視リスト) /
> B-7 メタ更新はスワップ成功後のみ / B-8 check 失敗はバッジ非破壊・無トースト /
> B-9 version フォールバック / B-10 check↔update の排他。未決 3 点は全て決定済みへ。
Scope: 書庫（配布サイト）から取得したパッケージについて、**サイト側の差し替えを検知して
「更新あり」を表示し、ワンクリックで同じフォルダへ上書き更新**できるようにする
（VSCode 拡張の更新モデル）。検知の一次ソースは **content hash**（ユーザー決定 2026-07-19、
案 c）。Kataribe / outcast（書庫サーバ）の両側に跨る spec — サーバ側は要件のみ凍結し、
実装詳細は outcast Spec 23 の追補として管理する。

> 動機（2026-07-19 ユーザー要望）: 作者は書庫の差し替え API でシナリオをけっこう更新するが、
> 受領側の再取得は `名前_2` の別フォルダになり（衝突回避）、更新の存在も分からない。
> 「プラグインのように更新が分かり、上書きで取得したい。バージョンの確認もしたい」。

## 前提の実測（2026-07-19 接地済み）

- **ローカル ↔ サイトの紐付けが無い**: 展開フォルダは zip のトップフォルダ名で、由来
  （サイト URL / パッケージ id / 取得時の内容）を記録していない。再取得は
  `unique_dir` が `_2` に逃がす（site.rs）。
- **サーバは sha256 を既に持っている**: 納本・差し替え（`PUT /packages/:id/file`）の
  パイプライン「検疫 → 検証 → 正規化 → sha256」が DB に保存済み。**ただし API 応答
  (`PackageSummary`) に露出していない** — outcast 側の残作業は SELECT + 応答 1 列の追加のみ。
  `file_updated_at`（差し替え日時）は応答に露出済み。
- **サーバは manifest を parse しない**（spec 23 の前置きフィルタ原則）: 作者の版番号
  (package.yaml `version`) はサーバに存在しない。**版番号の事前表示（v0.2→v0.3）は原則を
  破らない限り不可能** — 本 spec はこの原則を維持する（下記 表示設計）。
- **セーブはフォルダパスに紐づく**: `saves/<パスの FNV-1a>.yaml`。**同じフォルダへ上書き
  すればセーブ・スロットは無傷**で、版不一致は spec 07 の警告（非 fatal）が既に守る。
  `packagePaths`（localStorage）もパス不変なら無改修。

## 北極星整合

- **検知は機械値（hash）、表示は人間値**: 更新の有無は作者の申告（semver）でなく
  **サーバが計算した配布物の sha256** で判定する（申告の上げ忘れという穴を作らない —
  #47/#50 の「自己申告を唯一の根拠にしない」の適用）。版番号・日時は人間向け表示に回す。
- **受領側検証は不変**: 更新の取得は新規取得と同一の二層検証（サーバ前置きフィルタ +
  クライアント `extract_package_zip` の鏡）を通る。更新だからと検証を緩めない。
- **ローカルの正本を黙って壊さない**: 上書きは (a) プレイ中は不可、(b) ローカル編集を
  検知したら警告、(c) スワップは失敗時に旧フォルダへ復旧、の三重で守る。
- **手動配置は聖域**: 出所メタの無いパッケージ（repo 同梱・自作・手動コピー）は更新対象外。
  機構が誤って触る経路が構造的に無い。

## 設計

### 機構① 出所メタ `.kataribe_source.json`（Kataribe 単独・install 時に記録）

書庫からの取得 (`install_from_site`) 成功時、展開フォルダ直下に書く:

```json
{
  "site_url": "https://kataribe.outcasts.jp",
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "version": "0.2",
  "content_hash": "sha256hex...",
  "tree_hash": "sha256hex...",
  "installed_at": "2026-07-19T12:34:56Z"
}
```

- `content_hash` = **ダウンロードした zip の sha256 をクライアントが自前計算**し、
  **サーバ申告 (`RemotePackage.sha256`) と一致検証してから保存する**（rev2 A-1）:
  - 一致 → SourceMeta に保存（実際に受け取ったバイト列の指紋 = 以後の更新判定の基準）。
  - 不一致 → **DL 破損としてインストール中止**（壊れた基準を記録すると恒常的な
    偽陽性/偽陰性になる）。
  - サーバ申告が無い（`sha256: None` = 古い書庫/未対応の自前サーバ）→ 検証なしで
    自前計算値を保存（更新検知は機構③でどのみち無効だが、将来のサーバ対応時に備える）。
  - `install_site_package` は frontend が一覧で受けた `sha256` を **expected として引数で
    受け取る**（Option）。
- `version` = 展開後 package.yaml の `version` の写し（人間向け表示用スナップショット）。
  **空/欠落なら null**（rev2 B-9）— 表示は常に「v{version} / (不明)」のフォールバックを通す。
- `tree_hash` = **展開直後のフォルダ内容の正規化ハッシュ**（ローカル編集検知用・機構④)。
  **厳密定義（rev2 B-6 — 実装がブレない一意の式）**:
  ```
  files = walk(package_root) から通常ファイルのみ
          （除外: .kataribe_source.json / .DS_Store / Thumbs.db / desktop.ini。
            空ディレクトリ・symlink は無視 — extract が symlink を作らないことと整合）
  各 entry = 相対パス(UTF-8・'/' 区切り) + '\0' + hex(sha256(file_bytes)) + '\n'   // \n 固定
  tree_hash = hex(sha256(相対パスの辞書順 (バイト列比較) で entry を連結))
  ```
  OS が落とす付随ファイル（.DS_Store 等）は除外リストで吸収し「編集あり」の偽陽性を防ぐ。
  それ以外の隠しファイルは**編集あり判定に含める**（作者の意図的な追加ファイルを守る側に倒す）。
  パスの Unicode 正規化はしない（比較は同一マシンの install 直後 ↔ 更新直前で閉じる）。
- `installed_at` = 取得時刻（表示用）。
- **zip 内に `.kataribe_source.json` が混入していたら展開時に skip**（作者が更新済み
  フォルダをそのまま再 zip して納本した場合の混入対策。tree_hash の除外と一貫し、
  メタは常にクライアントが書いた値だけが存在する）。

### 機構② サーバの hash 露出（outcast Spec 23 追補・要件のみ）

- `PackageSummary`（一覧 `GET /api/packages` と詳細 `GET /api/packages/:id` の共通形）に
  **`sha256: String` を追加**（DB 保存済みの値を SELECT に足すだけ）。
- **一覧・詳細の両方に載せる（rev2 決定 3）** — 詳細のみだと将来「サイトタブで
  取得済み/更新ありバッジ」を出す拡張で N 回の detail fetch が要る。一覧に在れば無料。
- **配信の契約（rev2 A-1・Phase 0 で凍結する要）**:
  `GET /api/packages/:id/download` は **DB の sha256 に対応する正規化済み zip バイナリを
  そのまま返す**（再圧縮・変換をしない）。この対応が破れるとクライアントの一致検証が
  インストールを恒常的に中止する — サーバ側の不変条件として outcast 追補に明記する。
- Kataribe 側 `RemotePackage` に `sha256: Option<String>` を追加（Option = 未対応の古い
  書庫/自前サーバでも deserialize が通る前方互換。None なら更新検知は静かに無効）。

### 機構③ 更新検知とバッジ（Kataribe）

- 新 command `check_package_updates(paths)` — `packagePaths` の各フォルダについて:
  1. `.kataribe_source.json` が無い → スキップ（手動配置 = 更新対象外）。
  2. **SSRF 遮断（rev2 A-4）**: メタの `site_url` が**現在ユーザーが設定している
     `kataribe.siteUrl`（正規化後）と一致する場合のみ**ネットワークに出る。不一致は
     手動配置扱いでスキップ — 細工されたメタを手動配置されても、照会先は常に
     ユーザー自身が登録したサイトだけ（`open_external_url` の「開くのは登録 siteUrl 起点
     のみ」と同じ原則。自前サーバ利用者は自分の設定と一致するので普通に動く）。
  3. 設定 siteUrl へ `GET /api/packages/{id}`（並列・タイムアウト短め）。
  4. `meta.content_hash != server.sha256` → **更新あり** `{ path, file_updated_at,
     local_version, installed_at }` を返す。
- **失敗の意味論（rev2 B-8）**: 取得失敗（オフライン・404・5xx・パース失敗）は
  **その項目について何も主張しない** — 前回のバッジ状態を消さず、キャッシュを汚さず、
  エラートーストも出さない（404 = サーバ側削除の可能性もあるが、削除は「更新」ではない
  ので沈黙が正しい）。検知は best-effort であり、失敗が一覧を壊すことは無い。
- **排他（rev2 B-10）**: `update_site_package` 実行中は check をスキップし、update 自体も
  排他（同時に 1 件。frontend は `updatingPath` で他の更新ボタンを disable —
  `installingId` と同じ流儀）。
- **判定は「違うか」だけ**（新旧比較ではない）: hash に順序は無いので、作者が版を戻した
  場合も「内容が違う」として更新可能になる（ユーザー決定: ダウングレード論点は hash 採用で
  消滅）。
- UI（PackageDialog ローカルタブ）: 該当行に **「更新あり」チップ**（ember 系）+
  hover で「サイト側 {file_updated_at} 差し替え / 手元 {installed_at} 取得 (v{version})」。
  チェックの実行タイミングは未決 1。
- **版番号の事前表示はしない**（サーバが manifest を parse しない原則の維持）。
  「バージョンの確認」は二段で満たす: 更新前 = 手元の版 + サイトの差し替え日時 /
  更新後 = トーストで「『{title}』を v{旧} → v{新} に更新しました」（新版はローカルで
  読めるようになった package.yaml から）。

### 機構④ 上書き更新 `update_site_package(path)`（Kataribe）

1. **プレイ中ガード**: 対象がプレイ中のパッケージ（`activePackagePath`）なら拒否
   （「プレイを終了してから更新してください」— Windows はフォルダ内ファイルの
   ハンドル（BGM 再生等）が rename を失敗させるため、構造的に避ける）。frontend は
   ボタン自体を disable。
2. **ローカル編集検知**: 現在のフォルダの `tree_hash` を再計算し、メタの値と不一致なら
   確認ダイアログ「このパッケージはローカルで編集されています（{N} ファイルが取得時と
   異なります）。更新すると変更は失われます」→ 上書き / キャンセル（決定 2 —
   不一致ファイル数の表示は査読 C の提案を採用）。一致なら即進行。
3. **事前掃除（rev2 A-3）**: 対象の `旧.bak` が既に在れば削除してから開始
   （前回の残骸で rename が失敗しない）。
4. DL + 検証は新規取得と同一経路（サーバ申告 sha256 との一致検証込み）。**展開は
   `extract_package_zip_to(zip, exact_dest)`（展開先を固定する変種）で
   `packages/.update_tmp_{id}` の直下へトップフォルダを剥がして展開する**（rev2 A-2 —
   tmp 自体が package root になり、二重構造 `旧パス/<新トップ名>/…` を構造的に作らない）。
5. **スワップ（失敗時復旧つき）**: `旧 → 旧.bak` に rename → `tmp → 旧パス` に rename →
   `.bak` 削除。途中失敗は `.bak → 旧` を戻す（旧フォルダが必ず生き残る = 更新は
   全か無か）。**フォルダ名・パスは既存を維持**（zip のトップフォルダ名が変わっていても
   同じ場所へ = セーブの FNV キーと packagePaths の安定が眼目）。
6. **メタ更新はスワップ成功後のみ（rev2 B-7）**: DL/検証/展開のどの失敗でも SourceMeta は
   旧値のまま（失敗した新 hash を書き込むと「更新あり」が二度と点かなくなる）。
   一時 zip (`.part`) と `.update_tmp_*` は成功・失敗を問わず必ず削除する。
   成功後に content_hash / tree_hash / version / installed_at を書き直し、一覧を再読込して
   トースト「『{title}』を v{旧} → v{新} に更新しました」（version 欠落は「(不明)」）。

**クラッシュ復旧（rev2 A-3・起動時/一覧読込時の掃除）**: `packages/` 直下を走査し、
- `.update_tmp_*` → 無条件削除（書きかけ展開の残骸）。
- `X.bak` が在り `X` が無い → **`X.bak → X` に自動復旧**（スワップ中間でのクラッシュ）。
- `X.bak` と `X` が両方在る → `.bak` を削除（スワップ完了後の削除だけ失敗した残骸）。

## 実装 Phase

- **Phase 0**: data_contract 凍結。rev2 で凍結必須になった 4 点を含む: A-1 の配信契約
  （download は DB sha256 対応バイナリをそのまま返す + クライアント一致検証）/
  A-2 の `extract_package_zip_to`（展開先固定）/ A-4 の siteUrl 一致制約 /
  B-6 の tree_hash 厳密式（無視リスト込み）。
- **Phase A（Kataribe 単独・先行可）**: install 時の `SourceMeta` 書き込み
  （expected sha256 の一致検証・不一致で中止）+ `tree_hash` 計算 + zip 内混入 skip +
  **起動時/一覧読込時の残骸掃除と `.bak` 自動復旧（A-3 — 掃除は update より先に入れて
  おくと Phase C の失敗にも耐える）**。PoC: メタの roundtrip / tree_hash の決定論
  （ファイル順に依らない・無視リスト・`.kataribe_source.json` 除外）/ 混入 zip の skip /
  expected 不一致でメタ未作成 + インストール中止 / 掃除の 3 分岐（tmp 削除・bak 復旧・
  bak 破棄）。
- **✅Phase B（outcast・別リポ、2026-07-19 デプロイ済み）**: `PackageSummary.sha256` 露出
  （一覧+詳細）+ 配信契約の明文化（Spec 23 追補）。実サーバ
  `GET /api/packages` / `GET /api/packages/:id` の両方で `sha256` と `file_updated_at` を
  確認済み。Kataribe 側は `RemotePackage.sha256: Option`（None 耐性 = 未対応書庫でも壊れない）。
- **✅Phase C（Kataribe・結線、2026-07-19）**: `check_package_updates`（siteUrl 一致制約・
  並列照会・失敗の沈黙）+ `package_is_locally_edited` + `update_site_package`
  （プレイ中ガード（session の `package_root` 照合）/ 編集検知は `force` で二層 /
  事前掃除 / staging 展開 / スワップ復旧 / メタ更新は成功後のみ / `UpdateGuard` の排他 /
  版遷移トースト）+ `site::extract_package_zip_to` + ローカルタブのバッジと更新ボタン。
  PoC 4 本: スワップ原子性（staged 不在の失敗注入で旧フォルダ生存・残骸なし）/
  展開先固定（zip トップ名が変わっても同じパス・二重構造にならない・書きかけ残骸を捨てる）/
  siteUrl 不一致メタは照会に出ない（部分文字列の罠も）/ 掃除に `.zip.part` を追加。
  app backend 22 green・clippy clean・frontend build green。
- **✅Phase D（e2e 実測、2026-07-20 ユーザー確認）**: 実書庫で 納本 → 取得 → 差し替え →
  バッジ点灯 → 上書き更新 → セーブ生存、の通しを実機確認。Windows スワップの rename 失敗は
  観測されず — **リトライ 1 回の追加は見送り**（`.bak` 復旧 + 起動時掃除の守りで十分。
  以後の実運用で失敗が報告されたら再検討）。**spec 17 はこれで Done**。

## 核心的未知（Phase D で測る）

1. **Windows スワップの実挙動**: ウイルススキャナ・インデクサが直後の rename を失敗させる
   頻度（`.bak` 復旧が守るが、リトライ 1 回を足すべきかは実測で）。
2. **偽陽性の頻度**: 同一内容の再アップは正規化 zip のメタデータ差で hash が変わり
   「更新あり」になり得る（既知の限界として受容 — 再アップ自体を更新イベントと見なす。
   気になれば将来サーバの正規化を決定的にして消せる）。

## 決定事項（rev2 査読で確定）

1. **チェックのタイミング = ローカルタブを開くたび自動**（VSCode モデル）。
   オフライン/失敗は B-8 の沈黙 semantics で安全。N は十数個規模・並列・短タイムアウトで
   体感影響小。手動ボタンは足さない（タブを開き直せば再チェックされる。需要が出たら）。
2. **ローカル編集検知時 = 「上書き / キャンセル」の 2 択**。「編集版を `_2` に並置」の
   第 3 択はセーブ分岐を生むので将来需要が出てから。
   **【2026-07-19 改訂・Phase C 実装時のユーザー決定】不一致ファイル数は表示しない** —
   件数を出すには `SourceMeta` に per-file hash を持たせる必要があり、Phase 0 で凍結した
   「フォルダ全体で `tree_hash` 一つ」の形を崩す。凍結を優先し、警告は
   「ローカルで編集されています / 更新すると変更は失われます」に留める（査読 C の提案は撤回）。
3. **`sha256` は一覧+詳細の両方に露出**（コストは 1 列・将来のサイトタブ
   「取得済み/更新あり」バッジの布石）。

## スコープ外（据え置き）

- 自動更新（無確認での差し替え）— 確認したい、がユーザー要望なので常に明示操作。
- 複数版の並存・ロールバック UI（`.bak` は復旧専用で即削除）。
- ~~サイトタブ側の「取得済み」バッジ~~ → **✅2026-07-19 実装（Phase C 追補・実プレイで発覚）**:
  「二回押すと二重にできる」＝ `unique_dir` の `_2` 並置が spec 05 の名残として残っていた。
  Phase A で出所メタが入り判定材料が揃ったので、`list_packages` が `source_site`/`source_id`
  を返し、**現在の siteUrl かつ同じ書庫 id が手元に在ればサイトタブのボタンを「取得済み」で
  disable** する（更新はローカルタブの役割 = 役割分担の確定）。手動配置・別サイト由来は対象外。
- 書庫以外のソース（自前 URL 直指定等）の更新。

## 参照

- 取得経路: app `install_from_site` / `site::extract_package_zip`・`unique_dir`（`_2` 回避）。
- サーバ: outcast Spec 23（`zip_check::sha256_and_size` / `PUT /packages/:id/file` /
  `PackageSummary`）。契約の正本は本 spec、実装詳細は outcast 側追補。
- セーブ安定性: spec 07（`saves/<パス FNV-1a>.yaml`・版不一致は警告のみ）。
