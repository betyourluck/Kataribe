// 卓のオーケストレーション (spec 23 Phase C) — store と rtc の糊。
//
// store は transport しか知らない (tableHooks 経由の逆呼び出しのみ)。ここが
// HostTable / GuestLink のライフサイクルと、卓メッセージ ↔ store 反映を仲介する。
// - ホスト: 卓を開く → 席に entity を割り当て → set_participants → table_start 配布 →
//   入力窓の運用 (自動締切 all_submitted / タイマー / 手動 = 三系統、契約 input_window)。
// - ゲスト: join → hello (package_match) → table_start で盤面を受信 → 提出/開帳は
//   RemoteTransport (GuestLink) 越し。

import { invoke } from "@tauri-apps/api/core";
import { GuestLink, HostTable, judgePackageMatch } from "./rtc";
import type { TableHello } from "./rtc";
import { LocalTransport, tableHooks, transport } from "./transport";
import { useGameStore, freshMultiState } from "./stores/game";
import type { GameView, TurnView } from "./types/api";
import { t } from "./i18n";

/** ノックサーバー URL (設定・localStorage 永続)。既定は公式 (契約 knock_hosting)。 */
const KNOCK_URL_KEY = "kataribe.knockUrl";
export function knockUrl(): string {
  return localStorage.getItem(KNOCK_URL_KEY)?.trim() || "wss://knock.outcasts.jp/ws";
}
export function setKnockUrl(url: string) {
  localStorage.setItem(KNOCK_URL_KEY, url.trim());
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
  return code;
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

/** ゲスト: 部屋へ入る。パッケージはローカルの手持ちを指定 (契約 asset_wire / package_match)。 */
export async function guestJoin(roomCode: string, packagePath: string): Promise<void> {
  const store = useGameStore();
  // アセット解決 root の登録 (ゲストの backend は session を持たない — これだけを持つ)。
  await invoke("begin_guest_session", { packagePath });
  const hash = await invoke<string | null>("package_content_hash", { packagePath });
  const link = new GuestLink({
    displayName: tableName() || "guest",
    packageId: null,
    contentHash: hash,
  });
  link.onTable = (m) => void onGuestTable(m, packagePath, hash);
  link.onDisconnected = () => {
    store.multi.connected = false;
    store.logToast = t("table.disconnected");
  };
  await link.join(knockUrl(), roomCode);
  guestLink = link;
  transport.swap(link);
  store.multi = {
    ...freshMultiState(),
    role: "guest",
    roomCode,
    myPeerId: link.peerId,
    connected: false, // hello (host) を受けて true
  };
}

/** ゲスト: 再接続 (再 knock = identity 維持の張り直し)。 */
export async function guestReconnect(): Promise<void> {
  const store = useGameStore();
  if (!guestLink) return;
  await guestLink.reconnect(store.multi.roomCode);
}

/** ゲスト: 卓メッセージの反映。 */
async function onGuestTable(
  m: Record<string, unknown>,
  packagePath: string,
  myHash: string | null,
) {
  const store = useGameStore();
  switch (m.type) {
    case "hello": {
      const h = m as unknown as TableHello;
      store.multi.connected = true;
      store.multi.hostName = h.display_name;
      const match = judgePackageMatch(
        { package_id: null, content_hash: myHash },
        { package_id: h.package_id, content_hash: h.content_hash },
      );
      store.multi.packageWarning =
        match === "ok" ? null : match === "mismatch" ? t("table.pkgMismatch") : t("table.pkgUnknown");
      break;
    }
    case "table_start": {
      const view = m.view as GameView;
      await store.applyGameView(view, packagePath);
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
      store.multi.timerRemaining = Number(m.remaining_secs);
      break;
  }
}

/** ゲストの自分の peer_id (提出に使う)。GuestLink 確立後に確定する。 */
export function guestPeerId(): string {
  return guestLink?.peerId ?? "";
}

/** 卓を畳む (両ロール)。ゲストは単騎の配送路へ戻る。 */
export function leaveTable() {
  const store = useGameStore();
  hostStopTimer();
  hostTable?.close();
  hostTable = null;
  guestLink?.close();
  guestLink = null;
  tableHooks.onLocalReveal = undefined;
  tableHooks.onLocalInputStatus = undefined;
  transport.reset();
  store.multi = freshMultiState();
}
