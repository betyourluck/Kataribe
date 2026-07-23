# spec 23: 複数同時プレイ — ホスト権威 + WebRTC (P2P)

**Status**: Draft rev2 + **Phase 0 凍結済み + Phase A Done + Phase B Done**
（2026-07-23 ユーザー発案・同日設計対話で骨格確定 → 同日査読 12 件（矛盾 1〜12）+
接地情報 4 件を反映 → 同日 data_contract `Multiplayer` 節を凍結 → Phase A 実装 +
目視回帰 Green → Phase B 実装 = 多人数ターンループをネット無しで固定。C〜E 残）

> 番号の注: spec 22 は「あらすじ圧縮時の意味記憶抽出」に予約済み（ユーザー判断で保留中・
> ファイル未作成）。本 spec は 23 を取る。outcast リポジトリの Spec 23（書庫サーバ）とは
> 別リポジトリの別採番であり無関係。

## 動機

2〜3 人の友人同士で、同じ盤面を同時にプレイしたい。ターンごとに各自が制限時間内に
行動を入力し、締切で全員分がまとめて GM に送られる。プレイ中は WebRTC の音声で
わいわい会話できる（TRPG の卓の空気をオンラインに持ち込む）。

シグナリング用の小さなサーバ（ノックサーバー）はさくら VPS に置くが、
**ゲーム通信も音声も P2P** — サーバはピアを引き合わせるだけで、プレイの中身を見ない。
（rev2: ただし到達性のために同 VPS に TURN リレーを併設する。下記「TURN は v1 必須」）

## 外部接地（rev2 査読で確定した事実）

- **Tauri の WebView2 で WebRTC は動く**。Windows の WebView は Chromium (WebView2) で、
  `RTCPeerConnection`/DataChannel/audio が使える。実例: SecureBit.chat が Tauri v2 +
  WebRTC DTLS 1.2 で運用。`getUserMedia` は macOS で `Info.plist` に
  `NSMicrophoneUsageDescription` 必須・Windows は OS のマイク権限ダイアログ・
  Tauri v2 の capability 設定が要る（Phase D の作業項目）。
- **Rust 側 `webrtc-rs` は現役だが重い**。0.20 系 (Sans-I/O コア `rtc`) が最新線。ただし
  音声のキャプチャ/エコーキャンセル/リサンプルまで自前になるので v1 では採らない
  （旧・未決 5 は **frontend (RTCPeerConnection) で確定**。提示層の仕掛けは提示層に）。
- **TURN 無しの P2P は 20〜25% 失敗する**。大規模計測で direct 成功は 75〜80%、
  Carrier Grade NAT は対称 NAT として振る舞い**常に TURN を要する** — 日本の 4G/5G は
  CGNAT が標準なので、3 人卓で誰か 1 人でもモバイル回線なら卓ごと繋がらない。
- **DataChannel はゲーム向き**。既定は信頼・順序保証 (SCTP が断片化・フロー制御を担う)。
  ただし **16KB 超のメッセージは Head-of-Line blocking を起こす**ので、Phase 0 で
  メッセージ最大サイズと分割規則を凍結する。

## 決定（2026-07-23 設計対話 + rev2 査読）

### 決定 1: ホスト権威。レプリケーションは捨てる

**ホストだけが完全な正本（`GameSession`）を持ち、他の参加者はフィルタ済みの
view DTO を受け取る。** state の複製・同期・合意プロトコルは作らない。

根拠:
- 正本が 2 つあると split-brain = 「矛盾する GM」が構造的に起きうる（campaign 設計で
  二エンジン同期を棄却したのと同じ判断。正本は常に 1 つ）。
- 対象は友人同士 (2〜3 人)。ホストを信頼できる関係が前提なので、
  ビザンチン耐性に払うコストが正当化されない。
- **既存アーキテクチャがそのままスケールする**: app は既に「backend が正本を握り、
  frontend は view DTO を描くだけ」に分かれている。リモート参加者とは
  **Tauri IPC の代わりにネットワーク越しに同じ DTO を受け取る frontend** であり、
  プロトコルの大半は既に設計済み（`GameView`/`TurnView`/`StateView`）。

受け入れる脆さ: ホストが落ちるとセッションが止まる。ただし `SessionSave`（spec 07）は
rng cursor まで完全なので、**セーブファイルを渡せば別の人がホストを引き継いで
同じ運命の続きから再開できる**。これは実装コストでなく運用の割り切り。

**rev2 追記（矛盾 2）**: DataChannel を frontend に置く帰結として、**卓の生死はホストの
WebView プロセスに依存する** — ホストがウィンドウを閉じれば backend（正本・セーブ）は
無傷でも卓は全断する。v1 はこれを仕様として明記（「ホストはウィンドウを閉じない」）し、
UI 側でホストの終了時に「卓が閉じます」確認を出す。将来ホスト常駐性を上げたくなったら
DataChannel を Rust 側 (webrtc-rs / rtc) へ移す移行パスがある — `GameTransport` seam は
どちら側に置いても frontend からは同じに見えるよう、**wire 形式（JSON メッセージ）を
Phase 0 で backend 非依存に凍結しておく**。

### 決定 2: AI 提供者はホスト

LLM を叩くのはホストだけ。ホストの鍵・ホストのモデル設定を使う。

検討して棄却した案 — **ターンごと/章ごとの AI 持ち回り**（各自のクライアントが自分の
鍵で LLM を叩く）:
- prompt の `state_brief` には〔秘匿〕付きで全員分の隠し属性が載る（GM がゲームを
  回すのに必要な設計）。つまり **AI を回す人は構造的に GM の秘密を全部見る**。
  持ち回りにすると秘密を知る人間がターンごとに増える。
- prompt を他人のマシンへ送る配送路・失敗時のフォールバック・鍵設定の個人差
  （モデル/プロバイダ/キャッシュ挙動がターンごとに揺れる）という複雑さを背負う。
- ホスト固定なら**秘密を知る人間がちょうど 1 人**に収まり、それは元々ホストの役割。

**コスト公平性はセッション単位のホスト持ち回りで社会的に解決する**（「今日はお前が
ホスト」）。短編パッケージには章の切れ目が無いので、章単位の移譲機構を作っても
実効粒度はどうせセッションになる。v1 は持ち回りを機構化しない —
セーブファイルの受け渡し（手動）で成立する。

### 決定 3: 多 PC は作らない — 「主人公 + 仲間」で回避

エンジンの正本は主人公 1 + entities（NPC/仲間）の形を既に持つ。プレイヤー 2/3 は
**仲間 entity（`CharacterDef`）を操作する**。エンジン改修はほぼゼロ:
- 数値/属性/スキルは per-entity で既にある（好感度・NPC stat と同じ機構）。
- `location` は単一 = **パーティは一緒に移動する**。これは制約ではなく
  2〜3 人協力プレイの自然な形（分断が要る盤面は v2 の問い）。

**rev2 確定（矛盾 6）— 仲間のアイテムは既存 2 op で回る。per-entity 化は v1 不要**:
`AddItem`/`RemoveItem` は player 専用のままでよい。仲間が床のアイテムを持つ経路は
**「主人公が拾い（`add_item`）、仲間へ渡す（`give_item` = 既に per-entity）」の 2 op** で
表現でき、しかも **spec 09 の逐次射影のおかげで `[add_item, give_item]` を同一ターンに
束ねられる**（拾った直後の譲渡は射影クローンが所持を見るので一発受理）。
決定 3 の「location 単一 = パーティ同行」と組むと fiction 上も「主人公が拾って手渡す」は
自然（エンジン視点の二択 [per-entity 化 or 禁止] でなく、盤面視点の既存機構の再利用）。

**rev2 追補（同日・査読側の補強）— 消費も逆向きの 2 手で同じ射影に載る**:
仲間の持ち物の消費（薬を飲む等）は `give_item(仲間→主人公)` → `remove_item` の 2 手。
拾得と対称で、spec 09 の射影が同一ターンの束ねを受理する。GM_SYSTEM の定石は
拾得・譲渡・消費をまとめて一行にする:
「**アイテムの拾得/譲渡/消費は、必ず主人公を経由する 2 手で表現せよ。
仲間単独での add/remove は出してはならない**」。真の per-entity
拾得（主人公が居ない場所での拾得）は多 PC 化＝location 分裂とセットの問題なので v2。

**rev2 追記（矛盾 12）— 帰属マッピングは契約に載せる**: 「誰がどの entity を操作するか」を
`participants: [{peer_id, entity_id, display_name}]` として Phase 0 の contract に凍結する。
これが無いと合成 prompt の発話者名も「ops の entity を正しく振れ」の接地も宙に浮く。
主人公スロット（= ホストとは限らない。ホスト以外が主人公を執る卓も許す）も
participants の 1 行として表現し、entity_id = `player` で区別する。

### 決定 4: ターンの形 — 入力窓 + 締切 + 合成 prompt

1 ターン = 全員の入力を 1 つの束にして 1 回の LLM 呼び出し。

- ホストが入力窓を開く。**締切は三系統（rev2・矛盾 7）**:
  ①全員提出で即締め ②タイマー満了 ③ホストの強制締め（AFK で卓が止まるのを
  ホスト判断で打ち切る脱出口）。
- **未提出者は「（黙って様子を見ている）」として prompt に載せる**（rev2 で確定 =
  旧・未決 1 を解消）。省くより優れる理由が二つ: GM が不在と誤認して語りから
  消すのを防ぐ（#37 系 = presence は明示接地が要る）／他プレイヤーに AFK が
  透明になる（画面上も「入力待ち: ○○」を出す）。
- ホストが `「アキラ: 扉を調べる」「ユイ: 廊下を見張る」` のように**発話者名つきで
  束ねて** user メッセージを組む → 既存 `run_turn` に流す。発話者名は
  `participants.display_name`、帰属先は `participants.entity_id`（矛盾 12 の解）。
- GM_SYSTEM に多人数の接地を足す:「行動は複数人の合作。各人の行動をそれぞれ
  解決し、書いた本人の entity に帰属させよ（ops の entity を正しく振れ）」。
- **タイマーはホスト時刻基準**（rev2）: ホストが「残り N 秒」を周期ブロードキャストし、
  ゲストは表示するだけ。ゲスト側時計と同期しない（締切の正はホストの受信締切のみ =
  権威の一貫）。
- **トークン経済が良い**: 3 人が個別にターンを回すと 3 倍のフルプロンプト再課金だが、
  束ねれば 1 ターン 1 往復のまま（spec 09 の束ね経済と同じ向き）。

## アーキテクチャ

```
[ホスト app]                        [ゲスト app] ×1〜2
 Tauri backend = 正本                frontend のみが実働
 GameSession / LLM / saves            (backend は遊休)
      ↑ invoke (従来どおり)               ↑
 frontend ── GameTransport ──┐      frontend ── GameTransport
   (卓の生死はこのプロセス)   │                     │
                        WebRTC DataChannel (DTLS, P2P or TURN)
                             │                     │
                             └──── 音声 (WebRTC audio, mesh) ────┘

[ノックサーバー + coturn @ さくら VPS]
 WebSocket シグナリング (SDP/ICE 仲介 + 部屋コード + 再 join 待受)
 coturn = TURN リレー (turns:443?transport=tcp、一時クレデンシャル)。
 リレー経由でも中身は DTLS で見えない。
```

### transport seam（要の抽象）— rev2 で双方向に改訂（矛盾 1）

frontend の `invoke` 呼び出しは `stores/game.ts` に 29 箇所 + SettingsDialog に集中
している。**セッション系 command だけを `GameTransport` interface の裏に置く**。
ただし通信は Request/Response だけではない — 既存の `synopsis-compacting` /
`synopsis-failed` / `epilogue-writing` 等は **Tauri `emit` によるサーバプッシュ**なので、
interface は最初から双方向に切る:

```ts
interface GameTransport {
  request(cmd: string, args: unknown): Promise<unknown>;  // invoke 相当
  onEvent(handler: (name: string, payload: unknown) => void): void;  // listen 相当
}
```

- `LocalTransport` = `invoke` + `listen` の薄い包み（挙動不変。ホスト自身もこれを使う）。
- `RemoteTransport` = DataChannel 越しに同じ要求を送り、同じ DTO を受け取る。
  ホスト frontend が backend の `listen` を購読し、宛先別に DataChannel へ転送する
  （push の中継はホスト frontend の責務 = 決定 1 の rev2 追記と同じ依存）。
- 設定・パッケージ管理・ログ保存などホストローカルな command は seam に**入れない**
  （ゲストは自分のローカル設定を従来どおり使う）。

### 宛先別 view（唯一の本質的改修）

現状の `state_view` は「プレイヤー = 1 人」前提で、`secret_attributes` は
`id == PLAYER` の分だけ通す。これを **`state_view(..., viewer: &EntityId)`** に
一般化する — spec 06 が既に「宛先別秘匿（GM=全員 / player UI=本人のみ /
NPC 間=不可）」の概念を持つので、その「本人」を viewer 引数にするだけ。
各ゲストには自分の entity の秘密だけが載った view を配る。
**フィルタはホスト側 DTO 段階で行う**（ネットに乗る前に落とす。frontend で隠すのは
秘匿ではない）。viewer の解決は `participants` の peer_id→entity_id（矛盾 12）。

### アセット（背景/顔アイコン/BGM/SE）— rev2 で DTO 一本化を明記（矛盾 3）

DTO は現状ホストローカルの**絶対パス**を運ぶ（`convertFileSrc` で asset URL 化）。
これはゲストでは解決できない。**ゲストも同じパッケージを持つ前提**にする:
- ワイヤには**アセット ID とパッケージ識別**を載せ、各クライアントが自分のローカル
  コピーで解決する（配布は書庫 = spec 05 が既にある）。
- **DTO は一種類にする**: Remote だけ ID 化すると DTO が二形態に分裂するので、
  **Phase A で Local 含め全 DTO をアセット ID に置き換える**（背景/アイコン/BGM/SE の
  絶対パス欄 → ID 欄。解決は frontend が `resolve_asset` 系 command で行う）。これは
  単騎プレイにも波及する破壊的変更なので、Phase A のスコープに明示的に含め、
  既存の全アセット表示（背景・顔アイコン・CG・BGM・SE・結末 SE）の回帰を目視確認する。
- 版ズレは spec 17 の **`SourceMeta.content_hash`（パッケージ zip 全体の sha256。
  アセット単位ではない — Phase 0 で固定）** で照合し、不一致は警告
  （プレイは止めない — 絵が違うだけで正本はホスト）。手動配置（メタ無し）の
  パッケージは照合不能なので「照合できません」を出すだけ。
- アセットのストリーミング配信はしない（v1 スコープ外）。

### ダイス開帳（spec 18）の共有 — rev2 で導入時期を前倒し（矛盾 8）

開帳の `revealed` カウンタは現状 frontend ローカル。多人数では**セッション状態
`RevealState` に昇格**し、ホストが順序づける: 誰かの開帳クリック → ホストへ reveal
要求 → ホストがカウンタを進めて全員に配信（先着勝ち・競合はホストの受信順で決定 =
権威の自然な延長）。全員が同じ瞬間に出目を見る = 卓の「せーの」を保つ。

**導入は Phase B**（Phase D ではない）: `state_view(viewer)` と同時に `RevealState` を
ホスト session に持たせ、単騎でも fake transport でも同じ経路を通す（B の時点では
配信先が 1 人なだけ）。Phase D はこれを DataChannel に流すだけになる。
決断（プッシュ/差分買い）は v1 ではその判定の主体 entity を操作するプレイヤーに出す
のが理想だが、複雑化するので **v1 はホスト操作**（宛先制は多 PC 化とセットで v2）。

### ノックサーバー — rev2 で再接続を明記（矛盾 4）

さくら VPS 上の最小 WebSocket サービス。責務:
- 部屋コードの発行と、同じ部屋のピアへの SDP・ICE 中継。
- **部屋は全員接続後も TTL（既定 10 分・接続イベントで延長）で待受を残す** —
  WebRTC は切断が日常なので、同一 room_code での**再 knock → 再シグナリング**を
  受け付ける（これは「中途参加」ではない: participants に既に居る peer の張り直し）。
- ゲームの語彙を一切知らない（Kataribe の型に依存しない独立小物）。

### TURN は v1 必須（rev2・矛盾 5 = 旧「スコープ外」から昇格。2026-07-23 ポート改訂）

**同 VPS に coturn を同居させ、ICE 設定に `turns:5349?transport=tcp` を含める。**
根拠は外部接地のとおり — 日本のモバイル回線 (CGNAT) は対称 NAT として振る舞い、
TURN 無しでは 3 人卓の 1 人がモバイルなだけで卓ごと成立しない。音声の
「わいわい」が本 spec の動機なので、到達性の保証は動機の一部である。
- **旧 `turns:443` は Phase C 前に改訂（ユーザー決定）**: 既存 VPS の 443 は Caddy
  (HTTP 層) が握っており、TURN は HTTP でないので Host 名では同居できない。
  さくら VPS は 1 契約 1 IPv4 (追加 IP オプション無し) ゆえ 443 維持 = VPS 借り増し。
  CGNAT が塞ぐのは着信側で、発信 3478/5349 はモバイル網でまず通る — 443 が効く敵
  （443 しか通さない企業 FW）は友人卓の脅威モデルでは稀。**Phase E の実測
  （direct/relay 比率・接続失敗）で 5349 が塞がれる卓が現実に出たら追加 VPS で拡張**。
- **v1 実配備の ICE は `turn:3478` (udp/tcp) — Phase C 実装時の追補**: turns (TLS) は
  coturn が turn.{DOMAIN} の証明書を要るが、Caddy は proxy しないドメインの証明書を
  取れない（ダミーサイト + 共有 volume でパスを掘る手はあるが、更新時の reload 結合が
  脆く友人卓 v1 の運用に見合わない）。**平文 TURN でも秘匿は不変** — TURN 認証は
  HMAC (パスワード非送信)・リレーされる中身は元々 DTLS-SRTP。TLS の利得は
  ファイアウォール偽装のみで、それは 5349 でも大きくない（443 の議論と同じ向き）。
  turns:5349 は証明書配管が入ってからの拡張として knock の `TURN_URLS` env で
  差し替えられる形にしておく。
- クレデンシャルは**一時方式**（TURN REST API 形式: ノックサーバーが期限付き
  username + HMAC-SHA1 を発行 = coturn `use-auth-secret` 方式）— 静的パスワードを
  配ると VPS が野良リレー化する。**運用防御 3 点を凍結**: ①TTL は分単位
  （再接続で再発行）②部屋作成の IP 単位 rate limit ③coturn の帯域クォータ
  （total-quota / bps-capacity）。この 3 点がソース公開でも破れない守りの実体。
- リレー経由でも中身は DTLS-SRTP で暗号化されたまま（VPS は復号できない）。
- 帯域コストはリレーに落ちた接続分のみ（Opus 32kbps × 高々数本 = 微小）。

### セキュリティの線 — rev2 で具体化（矛盾 9）

- DataChannel/音声は WebRTC 標準の DTLS-SRTP で暗号化。
- **部屋コードは 128bit 以上のエントロピー**（base62 22 桁を Phase 0 で凍結）。
  推測参加を計算量で遮断する。
- **ノックサーバーは信頼点である**ことを自覚的に明記: SDP には DTLS フィンガー
  プリントが含まれ、サーバが改竄すれば MITM できる。v1 は「自分たちで立てた VPS を
  信頼する」で割り切る（友人卓の脅威モデルに整合）。将来公開運用するなら SDP への
  署名（部屋コードから導出した鍵で HMAC）を足す余地を wire 形式に残す
  （メッセージに予約フィールド `sig?` を置くだけ）。
- **LLM の鍵はホストのマシンから出ない**（決定 2 の帰結）。
- ゲストに渡るのはフィルタ済み view のみ。ただし**ホストは全知**（セーブも平文）。
  人狼型の秘匿盤面はこの構図と根本的に相性が悪い — 技術で塞がず、
  「秘匿盤面ではホスト = 非プレイヤーの GM 役を置く」遊び方の約束とする（v1 非対応と明記）。

### 言語（rev2・矛盾 10 = 旧・未決 3 を解消）

**卓の言語 = ホストの言語**。GM の語り・却下理由 localize・システム文はホスト設定
（`KATARIBE_LANG`）で生成される。ゲストの UI クローム（ボタン・設定画面）は
ゲスト自身の `t()` のまま — 混在は起きるが、「卓の言語: 日本語」を参加時に表示して
仕様であることを明示する。v1 はこれで割り切る。

### セーブ引き継ぎ（rev2・矛盾 11 = 旧・未決 4 を解消）

`SessionSave.content.path` はホストのローカル絶対パスなので、別 PC ではそのままでは
解決できない。**セーブ形式は変えない**（旧セーブ互換を壊さない）— 代わりに
**resume 系 command に `path_override` 引数（任意）** を足し、引き継ぎ側は自分の
パッケージ置き場を指して開く。パッケージ同一性は spec 17 content hash で照合し、
不一致は警告（authored content が違うと再開後の整合は保証できない旨）。

## Phase 分割（rev2 改訂）

- **Phase 0 — 契約凍結 (✅2026-07-23)**: data_contract に `Multiplayer` 節。凍結対象:
  - ワイヤの名詞: 部屋 / `participants: [{peer_id, entity_id, display_name}]` /
    メッセージ種別（join・state-sync・turn-input・reveal・event-push・timer-tick…）
  - **`protocol_version` の交換**（join 時。不一致は接続拒否 = 静かな解釈違いを作らない）
  - **メッセージ最大サイズ**（16KB の HoL 境界を踏まえ上限と分割規則。`GameView` 全量
    sync だけが大きくなりうるので、それのみ分割対象にする想定）
  - タイマー則（ホスト時刻基準・残り秒ブロードキャスト）/ 未提出者の扱い（「黙っている」）
  - 部屋コード生成則（base62 22 桁）/ coturn 一時クレデンシャル形式 / `sig?` 予約
  - content hash 照合の粒度（パッケージ zip 全体）/ ノックサーバーの部屋 TTL と再 join
- **Phase A — transport seam + DTO のアセット ID 化 (✅2026-07-23 実装 → 同日
  ユーザー目視回帰 Green = Done)**: `GameTransport`（request + onEvent の双方向、
  `app/src/transport.ts`。onEvent は購読解除関数を返す = HMR 多重購読の防止）を入れ
  `LocalTransport`（invoke + listen の薄い包み）で従来挙動を維持。seam を通るのは
  play_turn / resolve_dice_decision / play_contest_round / facts_add・edit・delete +
  push 3 イベント（App.vue の listen 直呼びを onEvent へ移設）。
  **同時に全 DTO のアセット欄を絶対パス→ID へ置換**（背景/BGM/顔アイコン/ビート CG・SE/
  結末 SE/マップ CG。Local にも波及する破壊的変更をここで畳んだ — 後の Phase でやると
  Remote/Local の DTO が分裂する）。解決は新 command `resolve_asset_path`（kind+id →
  絶対パス、session の package_root 起点、**seam の外** = 各クライアントが自分のコピーで
  解決する asset_wire の実装）→ frontend の `prefetchAssets`（view/turn 到着時に ID 群を
  一括解決してキャッシュ、以後は同期 `assetUrl(kind, id)` — revealNext 等の同期経路を
  async 化しないための設計。キャッシュはパッケージ替わり = applyGameView でクリア）。
  旧セーブは無傷（アセットはセーブに入らず毎回 scenario から導出）。
  検証: app backend 22 green + clippy clean + vue-tsc/vite build green +
  単騎プレイの全アセット目視回帰 Green (2026-07-23 ユーザー確認)。
- **Phase B — 多人数ターンループ（ネット無しで検証）(✅2026-07-23 実装 = Done)**:
  - **`state_view(..., viewer)` の宛先別化**: spec 06 の「本人」を引数化。secret 属性は
    viewer 本人の分だけ DTO に通す（フィルタはホスト側 DTO 段階 = ネットに乗る前）。
    hidden（本人未知）は viewer が誰でも全員分落ちたまま。ホスト画面の viewer は
    `GameSession::viewer_entity()`（host_peer→entity。**ホストが仲間を操作する卓では
    ホスト画面も本人の秘密だけ** — 正本とセーブでは全知だが画面はプレイヤー視界に揃える）。
  - **`participants` 導入**: `GameSession.participants` + `set_participants(participants,
    host_peer_id)`（門番 `validate_participants` = 空/peer 重複/entity 二重操作/幻 entity/
    主人公スロット≠1/ホスト不在を拒否）。**セーブ非対象**（卓は揮発 — 再開時は join フローで
    張り直す）。ゲスト向け fan-out の原料は `state_view_for(peer_id)`（TurnView の他欄は共有、
    差があるのは state だけ）。
  - **入力窓**: `submit_turn_input(peer_id, action)`（再提出は上書き・空は拒否）+
    `turn_input_status`（submitted/waiting を宣言順で返す = all_submitted 締切と
    「入力待ち: ○○」の材料）。締切三系統の**判断はホスト frontend の責務** — backend は
    `play_party_turn` で「いま締める」と言われた時点の提出物を合成する。
    **受理で窓クリア・却下は提出物を残す**（書き直すか締め直すかをホストが選ぶ）。
    全員未提出のままの締切は拒否。
  - **合成**: `harness::compose_party_action`（`PartyInput` 列 → 「名前 (entity): 行動」行、
    未提出者は `SILENT_ACTION`「（黙って様子を見ている）」で必ず載せる）。play_turn は
    `do_play_turn` に切り出し、単騎 (party 空) と多人数が同じ実体を通る。
  - **GM_SYSTEM の多人数接地**: `prompt::party_note(party)`（party ≥2 のみ）を run_turn が
    最初の system ブロック**末尾**に足す — 単騎の prompt は **1 バイトも変わらない**
    （安定プレフィックス不変 = キャッシュ無風。PoC で byte 一致を固定）。中身 = 参加者列挙
    （名前 (entity) — 主人公/仲間）+ 帰属規律（ops は書いた本人の操作 entity へ・省略は
    主人公扱いで却下）+ 「黙っている ≠ 不在」（語りから消すな・行動を捏造するな）+
    item idiom（拾得・譲渡・消費は主人公経由 2 手、spec 09 射影で同一ターン束ね可）。
    `run_turn` に `party: &[PartyMember]` 引数追加（CLI は常に `&[]` = 単騎）。
  - **`RevealState` のセッション状態昇格**: `GameSession.reveal: RevealView{revealed,total}`。
    伏せ直しは 3 点 — 受理ターン（rolls+checks+stat_rolls 数）/ プッシュ振り直し（1）/
    対決ラウンド（player の 1 枚）。`reveal_next`（飽和インクリメント = 二重クリック・競合
    無害、先着勝ち = session lock の獲得順）/ `reveal_all`（演出オフ・脱出口）。frontend は
    演出をローカル即時のまま、カウンタ進行を transport 越しに通知（単騎でも同じ経路。
    演出オフ経路は 3 箇所とも reveal_all で追認）。Phase D はこの通知を reveal_order 配信に
    するだけ。
  - **検証 (ネット無しで全ロジック固定)**: harness PoC 3 本 Red→Green
    （compose の黙っている合成 / party_note の接地 3 点 + 単騎 byte 不変 /
    **2 クライアント統合** = 2 人の入力 (1 人未提出) → 合成 → run_turn 1 回 → 仲間 entity へ
    帰属した op が一発受理・正本は仲間側だけ動く・prompt に両者の行）+ app PoC 4 本
    Red→Green（宛先別 secret フィルタ / participants 門番 / 開帳カウンタの単調・飽和 /
    入力窓の宣言順区分け）。workspace 312 green（+3）・app backend 26 green（+4）・
    clippy clean・vue-tsc/vite build green。
  - **Phase C への注記**: RemoteTransport はこの同じ command 群（submit_turn_input /
    turn_input_status / play_party_turn / state_view_for / reveal_next / reveal_all）を
    DataChannel 越しに叩く — B の時点では配送先が 1 人 (ホスト自身) なだけ。
    `set_participants` はホスト専用 (join 完了時に一度) なので seam の外。
    多人数の卓 UI（参加者設定・入力待ち表示・タイマー）は C の join フローと同時に作る
    （B は logic-only、GUI からはまだ届かない）。
- **Phase C — WebRTC 結線**: ノックサーバー（VPS・再 join 待受つき）+ **coturn**（一時
  クレデンシャル）/ DataChannel で RemoteTransport / 部屋コード join フロー /
  パッケージ hash 照合 / `path_override` 付き resume。
- **Phase D — 音声 + 開帳配信**: audio mesh（ミュート UI・macOS `Info.plist` と
  Tauri capability のマイク権限）/ `RevealState` の DataChannel 配信 / ホスト終了時の
  「卓が閉じます」確認。
- **Phase E — 実測**: 3 人 live プレイ（モバイル回線 1 人を意図的に混ぜる）。
  direct/relay 比率 / 入力窓の体感時間 / **多人数 prompt で GM が行動を正しく各人に
  帰属させるか（核心的未知）** / 再接続の実効性。

## スコープ外（v1）

持ち回りの機構化（セーブ受け渡しで代替）/ 観戦者・**中途参加**（再接続は別 — 既存
participant の張り直しは v1 に含む）/ 真の多 PC（per-PC location 分裂・per-entity 拾得・
決断の宛先制）/ 人狼型秘匿盤面の多人数対応 / テキストチャット（音声がある）/
アセットのストリーミング配信 / ホスト移譲の自動化 / SDP 署名（`sig?` 予約のみ）。

## 未決（実装前に決める）

1. 入力窓の既定秒数（三系統の締切は確定済み。数字だけ playtest で較正）。
2. `GameView` 全量 sync の実測サイズ（分割規則が実際に要るか。Phase 0 で上限だけ
   凍結し、超えたら分割を実装する条件付き項目にできるか）。
3. ~~ノックサーバーの実装置き場~~ **✅決着 (2026-07-23 ユーザー決定)**: コードは
   **Kataribe リポ `knock/`**（gm_core 非依存の独立クレート・workspace 外 =
   app/src-tauri と同じ隔離）、イメージは Kataribe CI が
   `ghcr.io/betyourluck/kataribe-knock` へ push、**デプロイは outcast の
   docker-compose.prod.yml + Caddy サブドメイン**（既存 kataribe_app と同型の
   GHCR pull 方式。VPS 借り増し不要）。public リポで問題ない — プロトコルは
   data_contract とクライアントコードで既に公開・エンドポイントは CT ログで公開・
   守りの実体はソース非依存の運用防御（TURN 節の 3 点）。むしろ「信頼点を
   自前ホストできる」ことがユーザーの安全装置（書庫 siteUrl と同じ思想）。
   契約は data_contract `knock_hosting`。

## 台帳

- 契約: data_contract `Multiplayer` 節（✅2026-07-23 凍結。protocol_version /
  participants / room_code / messages / package_match / input_window / transport
  (64KB + coturn 一時クレデンシャル) / asset_wire / item_idiom / language /
  resume_handoff の 11 項）
- 罠: failures.md（実装開始後）
- 配布側: ゲストが同じパッケージを要する旨は実装後に outcast package_spec へ
  （作者向けというよりプレイヤー向け注記なので、書き場所は Phase C で判断）

## rev2 査読の反映記録（2026-07-23）

受諾 11 / 修正採用 1（すべて同日反映）:
- 矛盾 1 → `GameTransport` を request + onEvent の双方向に。
- 矛盾 2 → ホスト frontend が卓の生死を握る旨を仕様化 + Rust 側移行パスを wire 凍結で担保。
- 矛盾 3 → アセット ID 化を Phase A で Local 含め一本化。hash 粒度はパッケージ全体で固定。
- 矛盾 4 → 部屋 TTL + 再 knock（再接続は v1 に含む。中途参加とは区別）。
- 矛盾 5 → TURN (coturn + turns:443?transport=tcp + 一時クレデンシャル) をスコープ外から v1 必須へ昇格。
- 矛盾 6 → **反論を採用せず既存機構で解決**: per-entity 化でも禁止でもなく、
  「主人公が拾い仲間へ渡す」2 op 束ね（spec 09 逐次射影が同一ターン受理を保証）+
  GM_SYSTEM 定石接地。エンジン改修ゼロを維持。
- 矛盾 7 → 締切三系統 + 未提出は「黙っている」で prompt に残す（AFK の透明性と #37 系接地）。
- 矛盾 8 → `RevealState` 昇格を Phase B へ前倒し（D は配信のみ）。
- 矛盾 9 → 部屋コード base62 22 桁凍結 + ノックサーバーが信頼点である旨の明記 + `sig?` 予約。
- 矛盾 10 → 卓言語 = ホスト言語で割り切り、参加時に明示。
- 矛盾 11 → セーブ形式不変 + resume `path_override` 引数。
- 矛盾 12 → `participants: [{peer_id, entity_id, display_name}]` を Phase 0 契約に追加。
