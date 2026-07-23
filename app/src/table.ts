// 卓のオーケストレーション (spec 23 Phase C) — store と rtc の糊。
//
// store は transport しか知らない (tableHooks 経由の逆呼び出しのみ)。ここが
// HostTable / GuestLink のライフサイクルと、卓メッセージ ↔ store 反映を仲介する。
// - ホスト: 卓を開く → パッケージを中継へ預ける → 席に entity を割り当て →
//   set_participants → table_start 配布 → 卓開始と同時に中継を捨てる →
//   入力窓の運用 (自動締切 all_submitted / タイマー / 手動 = 三系統、契約 input_window)。
// - ゲスト: join → hello (中継の sha256 を受け取る) → パッケージを自動取得 →
//   table_start で盤面を受信 → 提出/開帳は RemoteTransport (GuestLink) 越し。

import { invoke } from "@tauri-apps/api/core";
import { GuestLink, HostTable, judgePackageMatch } from "./rtc";
import type { TableHello } from "./rtc";
import { LocalTransport, tableHooks, transport } from "./transport";
import { useGameStore, freshMultiState, SEAT_COLORS } from "./stores/game";
import type { GameView, TurnView } from "./types/api";
import { t } from "./i18n";
import { voice, LOCAL_PEER } from "./voice";

/** ノックサーバー URL (設定・localStorage 永続)。既定は公式 (契約 knock_hosting)。 */
const KNOCK_URL_KEY = "kataribe.knockUrl";
export function knockUrl(): string {
  return localStorage.getItem(KNOCK_URL_KEY)?.trim() || "wss://knock.outcasts.jp/ws";
}
export function setKnockUrl(url: string) {
  localStorage.setItem(KNOCK_URL_KEY, url.trim());
}

/**
 * 締切タイマーの秒数 (localStorage 永続)。**設定はダイアログ・起動は卓バー**なので、
 * 値を ref で持たず永続キーに置いて両方から読む (卓バーの開始ボタンが
 * ダイアログを開かなくても同じ秒数を使う)。
 */
const TIMER_SECS_KEY = "kataribe.tableTimerSecs";
export function timerSeconds(): number {
  const n = Number(localStorage.getItem(TIMER_SECS_KEY));
  return Number.isFinite(n) && n >= 10 ? Math.min(n, 600) : 90;
}
export function setTimerSeconds(secs: number) {
  localStorage.setItem(TIMER_SECS_KEY, String(secs));
}

/** 卓での表示名 (localStorage 永続)。 */
const TABLE_NAME_KEY = "kataribe.tableName";
export function tableName(): string {
  return localStorage.getItem(TABLE_NAME_KEY)?.trim() || "";
}
export function setTableName(name: string) {
  localStorage.setItem(TABLE_NAME_KEY, name.trim());
}

let hostTable: HostTable | null = null;
let guestLink: GuestLink | null = null;
let autoClose = true;
let timerHandle: number | undefined;
/** ゲストが実際に使うパッケージのパス (中継で受け取ったもの、または手動選択)。 */
let guestPackagePath = "";
/** ホストが意図して卓を閉じた (= 回線不調ではない)。再接続を回さないための門。 */
let hostClosedTable = false;

/** ホスト: 卓を開く (ゲーム開始済みが前提 — 正本はもう在る)。戻り = 部屋コード。 */
export async function hostOpenTable(): Promise<string> {
  const store = useGameStore();
  if (!store.started) throw new Error(t("table.needGame"));
  const hash = await invoke<string | null>("package_content_hash", {
    packagePath: store.activePackagePath,
  });
  const table = new HostTable(new LocalTransport(), {
    displayName: tableName() || "GM",
    packageId: null,
    contentHash: hash,
    relaySha256: null, // 部屋コードが要るのでアップロードは open の後
  });
  table.onSeatsChanged = () => syncSeats();
  table.onRevealOrder = (rv) => store.applyRevealOrder(rv);
  table.onInputStatus = (st) => {
    store.multi.inputStatus = st as { submitted: string[]; waiting: string[] };
    maybeAutoClose();
  };
  const code = await table.open(knockUrl());
  hostTable = table;
  store.multi = {
    ...freshMultiState(),
    role: "host",
    roomCode: code,
    myPeerId: "host", // set_participants 時に自分の席として使う論理 id (シグナリングの peer とは別)
    connected: true,
  };
  syncSeats();
  // store → table の逆呼び出し (自分の開帳・提出をゲストへ配る)。
  tableHooks.onLocalReveal = (rv) => {
    hostTable?.broadcastRevealOrder(rv);
    store.applyRevealOrder(rv); // 自分の適用位置も追従 (エコー整合)
  };
  tableHooks.onLocalInputStatus = (st) => {
    hostTable?.broadcastInputStatus(st);
    maybeAutoClose();
  };
  // --- パッケージを中継へ預ける (契約 package_relay) ---
  // 正はホストの実ファイル (改変版で遊ぶ卓が成立する)。失敗しても卓は開いたまま —
  // ゲストは手動選択の旧経路へ落ちる (LAN 卓・サーバ不達の fallback)。
  store.multi.relay = "uploading";
  try {
    const up = await invoke<{ sha256: string }>("relay_upload_package", {
      siteUrl: store.siteUrl,
      roomCode: code,
      packagePath: store.activePackagePath,
    });
    table.setRelaySha256(up.sha256);
    store.multi.relay = "ready";
  } catch (e) {
    console.warn("[relay] パッケージの配布に失敗:", e);
    store.multi.relay = "failed";
    store.logToast = t("table.relayFailed");
  }
  return code;
}

/** ホスト: 中継の一時ファイルを捨てる (卓開始・卓を閉じたとき)。取りこぼしは TTL が回収。 */
async function relayDelete() {
  const store = useGameStore();
  if (store.multi.role !== "host" || !store.multi.roomCode) return;
  if (store.multi.relay !== "ready") return;
  try {
    await invoke("relay_delete_package", {
      siteUrl: store.siteUrl,
      roomCode: store.multi.roomCode,
    });
  } catch (e) {
    // 失敗は握り潰してよい — サーバ側 TTL sweep が残骸を回収する (削除の二層)。
    console.warn("[relay] 取り下げに失敗 (TTL で回収される):", e);
  }
}

/** ホスト: 席一覧を store へ写す (自分 + hello 済みゲスト)。 */
function syncSeats() {
  const store = useGameStore();
  if (!hostTable) return;
  const seats = [
    {
      peerId: "host",
      displayName: tableName() || "GM",
      packageMatch: "ok",
      connected: true,
      entityId: store.multi.seats.find((s) => s.peerId === "host")?.entityId ?? "player",
    },
  ];
  for (const seat of hostTable.seats.values()) {
    seats.push({
      peerId: seat.peerId,
      displayName: seat.displayName,
      packageMatch: seat.packageMatch,
      connected: seat.connected,
      entityId: store.multi.seats.find((s) => s.peerId === seat.peerId)?.entityId ?? "",
    });
  }
  store.multi.seats = seats;
}

/** ホスト: 席の entity 割り当てを確定して卓を開始する (set_participants → table_start 配布)。 */
export async function hostStartTable(): Promise<void> {
  const store = useGameStore();
  if (!hostTable) throw new Error(t("table.notOpen"));
  const seats = store.multi.seats.filter((s) => s.entityId);
  const participants = seats.map((s) => ({
    peer_id: s.peerId,
    entity_id: s.entityId,
    display_name: s.displayName,
  }));
  await invoke("set_participants", { participants, hostPeerId: "host" });
  // 割り当ての可視化素材 (顔アイコンの席色リング + プロフィールの「プレイヤー: ○○」)。
  store.multi.assignments = participants.map((p, i) => ({
    peerId: p.peer_id,
    entityId: p.entity_id,
    displayName: p.display_name,
    color: SEAT_COLORS[i % SEAT_COLORS.length],
  }));
  // 各ゲストへ「いまの盤面」を宛先別 view で配る (途中の卓開きでも情景が繋がる)。
  for (const s of seats) {
    if (s.peerId === "host") continue;
    const view = await invoke<GameView>("current_game_view", { peerId: s.peerId });
    hostTable.sendTo(s.peerId, { type: "table_start", view, participants });
  }
  store.multi.started = true;
  store.multi.inputStatus = { submitted: [], waiting: seats.map((s) => s.peerId) };
  hostTable.broadcastInputStatus(store.multi.inputStatus);
  store.log.push({ kind: "system", text: t("table.started") });
  // 全員そろった = 中継の役目は終わり (契約 package_relay の削除の二層・一層目)。
  // hello の sha256 は announce し続ける — 既に持っている人はキャッシュで繋がる。
  void relayDelete();
}

/** ホスト: 自動締切 (all_submitted) — 全員提出で即締め (契約 input_window ①)。 */
function maybeAutoClose() {
  const store = useGameStore();
  if (!autoClose || store.multi.role !== "host" || !store.multi.started) return;
  const st = store.multi.inputStatus;
  if (st && st.waiting.length === 0 && st.submitted.length > 0 && !store.loading) {
    void hostCloseWindow();
  }
}
export function setAutoClose(on: boolean) {
  autoClose = on;
}

/** ホスト: タイマー締切 (契約 input_window ②)。残り秒はホスト時刻基準で全員へ配る。 */
export function hostStartTimer(seconds: number) {
  const store = useGameStore();
  hostStopTimer();
  store.multi.timerRemaining = seconds;
  hostTable?.broadcastTimerSync(seconds); // 開始を即座に配る (1 秒待たせない)
  timerHandle = window.setInterval(() => {
    const remaining = (store.multi.timerRemaining ?? 0) - 1;
    store.multi.timerRemaining = remaining;
    hostTable?.broadcastTimerSync(remaining);
    if (remaining <= 0) {
      hostStopTimer();
      // 誰も提出していなければ締められない (backend が拒否する) — タイマーだけ畳む。
      if (store.multi.inputStatus?.submitted.length) void hostCloseWindow();
    }
  }, 1000);
}
export function hostStopTimer() {
  if (timerHandle !== undefined) window.clearInterval(timerHandle);
  timerHandle = undefined;
  const store = useGameStore();
  store.multi.timerRemaining = null;
  // 止めたことを配らないと、ゲストの画面に最後の数字が残り続ける。
  hostTable?.broadcastTimerSync(null);
}

/** ホスト: 入力窓を締めて 1 ターン回す (契約 input_window ③ = host_forced も同じ経路)。 */
export async function hostCloseWindow(): Promise<void> {
  const store = useGameStore();
  if (store.loading) return;
  hostStopTimer();
  store.loading = true;
  store.error = null;
  try {
    const turn = await invoke<TurnView>("play_party_turn");
    store.log.push({ kind: "system", text: t("table.turnClosed") });
    await store.ingestTurn(turn);
    if (turn.accepted) {
      store.multi.inputStatus = {
        submitted: [],
        waiting: store.multi.seats.filter((s) => s.entityId).map((s) => s.peerId),
      };
    }
    hostTable?.broadcastInputStatus(store.multi.inputStatus);
    // ゲストへターンを配布 (state は宛先別 = state_view_for を添える)。
    await hostTable?.distributeTurn(turn);
  } catch (e) {
    store.error = String(e);
  } finally {
    store.loading = false;
    store.compacting = false;
    store.writingEpilogue = false;
  }
}

/**
 * ゲスト: 部屋へ入る。
 *
 * 既定ではパッケージを選ばない — ホストが中継へ預けた実ファイルを hello の sha256 を
 * 鍵に自動取得する (契約 `package_relay`)。`manualPackagePath` は fallback
 * (中継を使わない卓 = 自前 knock の LAN 卓・サーバ不達) で、その時だけ手持ちとの
 * hash 照合 (`package_match`) が生きる。
 */
export async function guestJoin(roomCode: string, manualPackagePath?: string): Promise<void> {
  const store = useGameStore();
  let hash: string | null = null;
  guestPackagePath = "";
  hostClosedTable = false;
  if (manualPackagePath) {
    // アセット解決 root の登録 (ゲストの backend は session を持たない — これだけを持つ)。
    await invoke("begin_guest_session", { packagePath: manualPackagePath });
    guestPackagePath = manualPackagePath;
    hash = await invoke<string | null>("package_content_hash", {
      packagePath: manualPackagePath,
    });
  }
  const link = new GuestLink({
    displayName: tableName() || "guest",
    packageId: null,
    contentHash: hash,
    relaySha256: null,
  });
  link.onTable = (m) => enqueueGuestTable(m, hash);
  link.onDisconnected = () => {
    // ホストが閉じた場合は table_closed で既に畳んである — 回線不調と混同しない。
    if (hostClosedTable) return;
    store.multi.connected = false;
    store.logToast = t("table.disconnected");
    scheduleReconnect(); // 手動ボタンを待たずに取りに行く
  };
  // **join の前に**卓の状態を据える。hello は接続直後に飛んでくるので、後から
  // freshMultiState で上書きすると hello が書いた値 (connected / 中継の現況) が消え、
  // 中継の取得も roomCode 空で走ることになる。
  guestLink = link;
  transport.swap(link);
  store.multi = {
    ...freshMultiState(),
    role: "guest",
    roomCode,
    myPeerId: "",
    connected: false, // hello (host) を受けて true
  };
  await link.join(knockUrl(), roomCode);
  store.multi.myPeerId = link.peerId;
}

// ---------------------------------------------------------------------------
// 自動再接続 — 切れたら黙って諦めない
// ---------------------------------------------------------------------------
//
// WebRTC は切断が日常で、部屋は TTL (既定 10 分・接続イベントで延長) まで待ち受けている。
// その間なら identity (peer_id) を保ったまま張り直せるので、手動ボタンを待たずに取りに行く。
// 間隔は指数的に伸ばす — ノックサーバーは IP 単位のレート制限を持つので、詰めて叩くと
// 自分で自分を締め出す。TTL を越えたら諦めて「入り直してほしい」と言う (黙って止まらない)。

const RECONNECT_DELAYS = [1_000, 2_000, 4_000, 8_000, 15_000, 15_000, 30_000];
/** 諦めるまでの試行回数 (末尾 30s × 残り ≒ 部屋の TTL 10 分をわずかに越える)。 */
const RECONNECT_MAX = 20;

let reconnectTimer: number | undefined;

function cancelReconnect() {
  if (reconnectTimer !== undefined) window.clearTimeout(reconnectTimer);
  reconnectTimer = undefined;
  useGameStore().multi.reconnecting = null;
}

function scheduleReconnect() {
  const store = useGameStore();
  const attempt = (store.multi.reconnecting ?? 0) + 1;
  if (attempt > RECONNECT_MAX) {
    store.multi.reconnecting = null;
    store.logToast = t("table.reconnectGaveUp");
    return;
  }
  store.multi.reconnecting = attempt;
  const delay = RECONNECT_DELAYS[Math.min(attempt - 1, RECONNECT_DELAYS.length - 1)];
  reconnectTimer = window.setTimeout(() => {
    void attemptReconnect();
  }, delay);
}

async function attemptReconnect() {
  const store = useGameStore();
  // 卓を出た後に発火した遅延は捨てる (leaveTable は guestLink を null にする)。
  if (!guestLink || store.multi.role !== "guest") return cancelReconnect();
  if (store.multi.connected) return cancelReconnect();
  try {
    await guestLink.reconnect(store.multi.roomCode);
    // ここで成功なのは**シグナリングまで**。ホストとの DataChannel が開いて hello が
    // 返るまで connected は立たないので、次の試行を予約したまま待つ (hello 受信で畳む)。
  } catch (e) {
    console.warn("[table] 再接続に失敗:", e);
  }
  scheduleReconnect();
}

/** ゲスト: 再接続 (再 knock = identity 維持の張り直し)。手動ボタンからも呼ぶ。 */
export async function guestReconnect(): Promise<void> {
  const store = useGameStore();
  if (!guestLink) return;
  cancelReconnect(); // 手動で押されたら自動の待ち時間は捨てて今すぐ行く
  store.multi.reconnecting = 1;
  await guestLink.reconnect(store.multi.roomCode);
  scheduleReconnect();
}

/**
 * ゲスト: 卓メッセージを**到着順に直列で**処理する。
 *
 * hello の処理はパッケージ取得 (DL・展開) を待つので、素朴に `void` で投げると
 * `table_start` が追い越して「まだパッケージが無いのに盤面を描く」ことになる。
 * 鎖にすれば直列と順序保存を同時に満たす (tts.ts の投入鎖と同型)。
 */
let guestChain: Promise<void> = Promise.resolve();
function enqueueGuestTable(m: Record<string, unknown>, myHash: string | null) {
  guestChain = guestChain
    .then(() => onGuestTable(m, myHash))
    .catch((e) => console.warn("[table] 卓メッセージの処理に失敗:", e));
}

/** ゲスト: 卓メッセージの反映。 */
async function onGuestTable(m: Record<string, unknown>, myHash: string | null) {
  const store = useGameStore();
  switch (m.type) {
    case "hello": {
      const h = m as unknown as TableHello;
      store.multi.connected = true;
      cancelReconnect(); // ホストと繋がった = 再接続の輪はここで畳む
      store.multi.hostName = h.display_name;
      const relaySha = h.relay_sha256 ?? null;
      if (relaySha) {
        // --- 中継経路: 正はホストの実ファイル (改変版でもそのまま遊べる) ---
        // 同一版を持っていれば再 DL しない (キャッシュ鍵 = zip の sha256)。
        store.multi.relay = "downloading";
        try {
          const pkg = await invoke<{ path: string; title: string }>("relay_fetch_package", {
            siteUrl: store.siteUrl,
            roomCode: store.multi.roomCode,
            expectedSha256: relaySha,
          });
          guestPackagePath = pkg.path;
          await invoke("begin_guest_session", { packagePath: pkg.path });
          store.multi.relay = "ready";
          store.multi.packageWarning = null;
        } catch (e) {
          store.multi.relay = "failed";
          store.multi.packageWarning = t("table.relayDownloadFailed", { error: String(e) });
        }
        break;
      }
      // --- fallback: 中継を使わない卓 — 手持ちとの hash 照合 (契約 package_match) ---
      const match = judgePackageMatch(
        { package_id: null, content_hash: myHash },
        { package_id: h.package_id, content_hash: h.content_hash },
      );
      store.multi.packageWarning =
        match === "ok" ? null : match === "mismatch" ? t("table.pkgMismatch") : t("table.pkgUnknown");
      if (!guestPackagePath) store.multi.packageWarning = t("table.noPackage");
      break;
    }
    case "table_start": {
      const view = m.view as GameView;
      const parts = (m.participants as { peer_id: string; entity_id: string; display_name: string }[]) ?? [];
      store.multi.assignments = parts.map((p, i) => ({
        peerId: p.peer_id,
        entityId: p.entity_id,
        displayName: p.display_name,
        color: SEAT_COLORS[i % SEAT_COLORS.length],
      }));
      if (!guestPackagePath) {
        // 中継が使えず手動選択もしていない = アセットを解決する足場が無い。
        store.multi.packageWarning = t("table.noPackage");
      }
      await store.applyGameView(view, guestPackagePath);
      store.multi.started = true;
      store.log.push({ kind: "system", text: t("table.joinedStarted", { host: store.multi.hostName }) });
      if (store.multi.packageWarning) {
        store.log.push({ kind: "system", text: `⚠ ${store.multi.packageWarning}` });
      }
      break;
    }
    case "party_turn": {
      const turn = m.turn as TurnView;
      // state は宛先別 (自分の秘密だけが載った view) に差し替えられて届く。
      turn.state = m.state as TurnView["state"];
      store.log.push({ kind: "system", text: t("table.turnClosed") });
      await store.ingestTurn(turn);
      break;
    }
    case "input_status":
      store.multi.inputStatus = m.status as { submitted: string[]; waiting: string[] };
      break;
    case "reveal_order":
      store.applyRevealOrder(m as unknown as { revealed: number; total: number });
      break;
    case "timer_sync":
      // null = 停止。Number(null) は 0 なので、素直に変換すると「0s」が残り続ける。
      store.multi.timerRemaining =
        m.remaining_secs === null || m.remaining_secs === undefined
          ? null
          : Number(m.remaining_secs);
      break;
    case "table_closed":
      // ホストが意図して閉じた = 回線の不調ではない。再接続を回さず、そう伝える。
      hostClosedTable = true;
      store.log.push({ kind: "system", text: t("table.hostClosed") });
      store.logToast = t("table.hostClosed");
      leaveTable();
      break;
  }
}

/** ゲストの自分の peer_id (提出に使う)。GuestLink 確立後に確定する。 */
export function guestPeerId(): string {
  return guestLink?.peerId ?? "";
}

// ---------------------------------------------------------------------------
// 音声 (spec 23 Phase D) — voice は peer_id しか知らないので、席への写像はここが持つ
// ---------------------------------------------------------------------------

/** シグナリングの peer_id を participants の peer_id へ寄せる。 */
function participantPeer(signalPeer: string): string {
  const store = useGameStore();
  if (signalPeer === LOCAL_PEER) {
    // 自分。ホストの participants 上の id は "host" 固定 (シグナリング id とは別)。
    return store.multi.role === "host" ? "host" : guestPeerId();
  }
  // ゲストから見たホストは、シグナリング id で届くので "host" へ読み替える。
  if (guestLink && signalPeer === guestLink.hostSignalId) return "host";
  return signalPeer;
}

/** 発話レベルを entity へ写して store に載せる (席色リングの脈動の素材)。 */
voice.onLevels = (levels) => {
  const store = useGameStore();
  if (store.multi.role === "solo") return;
  const out: Record<string, number> = {};
  for (const [peer, level] of Object.entries(levels)) {
    const p = participantPeer(peer);
    const seat = store.multi.assignments.find((a) => a.peerId === p);
    if (seat) out[seat.entityId] = level;
  }
  store.multi.voiceLevels = out;
};

voice.onMicError = (message) => {
  const store = useGameStore();
  store.multi.micOn = false;
  store.logToast = t("table.micDenied", { error: message });
};

/** マイクの ON/OFF。**OFF は完全解放** (OS のマイク使用インジケータが消える)。 */
export async function toggleMic(on: boolean): Promise<void> {
  const store = useGameStore();
  await voice.setMic(on);
  store.multi.micOn = voice.micOn;
  if (!voice.micOn) store.multi.voiceLevels = {};
}

/**
 * 確認してから卓を畳む (両ロール)。**押し間違いの取り返しがつかない**ので必ず訊く —
 * ホストは全員のセッションが終わり、ゲストは部屋コードの入れ直しになる。
 * 卓バーとダイアログの両方がここを呼ぶ (確認の実装を二箇所に持たない)。
 */
export async function confirmAndLeave(): Promise<void> {
  const store = useGameStore();
  const host = store.multi.role === "host";
  const ok = await store.askConfirm(
    host ? t("table.closeTableConfirm") : t("table.leaveTableConfirm"),
    host ? t("table.closeTableReally") : t("table.leaveTable"),
  );
  if (!ok) return;
  leaveTable();
}

/** 卓を畳む (両ロール)。ゲストは単騎の配送路へ戻る。 */
export function leaveTable() {
  const store = useGameStore();
  hostStopTimer();
  cancelReconnect();
  // 卓を開いたまま閉じた場合の中継の後始末 (開始時に消していれば冪等な二度目)。
  void relayDelete();
  // 解散を伝えてから畳む — ゲスト側で「閉じられた」と「切れた」を区別させる。
  hostTable?.closeAnnounced();
  hostTable = null;
  guestLink?.close();
  guestLink = null;
  tableHooks.onLocalReveal = undefined;
  tableHooks.onLocalInputStatus = undefined;
  transport.reset();
  guestPackagePath = "";
  voice.close(); // hostTable/guestLink が無い経路でもマイクは必ず手放す
  store.multi = freshMultiState();
}
