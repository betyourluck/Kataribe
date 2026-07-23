// WebRTC 配送層 (spec 23 Phase C)。
//
// 役割は 3 つ:
// - Signaling: ノックサーバー (knock/) との WebSocket。create/join と SDP/ICE 中継だけ。
// - HostTable: ホスト側ハブ。ゲストの DataChannel を受け、game_request を **whitelist +
//   peer 束縛**で自分の backend (LocalTransport) へ流し、push を全員へ配る。
//   卓の生死はこの WebView が握る (spec 23 決定 1 rev2 — ホストはウィンドウを閉じない)。
// - GuestLink: ゲスト側。ホストへの DataChannel を GameTransport として実装 —
//   ゲストの frontend は「ネット越しに同じ DTO を受け取る frontend」になる。
//
// DataChannel は reliable + ordered (既定)。メッセージは JSON 1 行 (契約 messages)。
// 上限 64KB — 超過は送るが警告する (未決2 の計測点。恒常的に超えるなら分割を実装)。

import type { GameEventHandler, GameEventName, GameTransport } from "./transport";
import { GAME_EVENTS } from "./transport";
import { voice } from "./voice";

/** join/create で交換するプロトコル版 (knock 側 msg.rs と一致。不一致は接続拒否)。 */
export const KNOCK_PROTOCOL_VERSION = 1;

/** TURN 一時クレデンシャル (knock の TurnCred と 1:1)。 */
export interface TurnCredView {
  urls: string[];
  username: string;
  credential: string;
  ttl_secs: number;
}

type SignalKind = "offer" | "answer" | "candidate";

/** 卓メンバーの自己紹介 (DataChannel 開通直後に双方が送る)。package_match の照合材料。 */
export interface TableHello {
  type: "hello";
  role: "host" | "guest";
  protocol_version: number;
  display_name: string;
  /** spec 17 出所メタ由来。手動配置は null = 「照合できません」(プレイは止めない)。 */
  package_id: string | null;
  content_hash: string | null;
  /**
   * ホストが中継へ預けた zip の sha256 (契約 `package_relay`)。ゲストはこれを鍵に
   * キャッシュを引き、無ければ部屋コードで落とす。**null = 中継を使わない卓**
   * (アップロード失敗・自前 knock の LAN 卓) で、その時だけ手動選択 + hash 照合の
   * 旧経路が生きる。古い版のピアはこの欄を持たない → undefined も null 扱い。
   */
  relay_sha256?: string | null;
}

/** hello に載せる自己紹介 (両ロール共通の材料)。 */
export interface HelloIdentity {
  displayName: string;
  packageId: string | null;
  contentHash: string | null;
  /** ホストのみ: 中継に預けた zip の sha256 (預けていなければ null)。 */
  relaySha256: string | null;
}

/** hash 照合の結果 (契約 package_match)。 */
export type PackageMatch = "ok" | "mismatch" | "unknown";

export function judgePackageMatch(
  mine: { package_id: string | null; content_hash: string | null },
  theirs: { package_id: string | null; content_hash: string | null },
): PackageMatch {
  if (!mine.content_hash || !theirs.content_hash) return "unknown";
  return mine.content_hash === theirs.content_hash ? "ok" : "mismatch";
}

// ---------------------------------------------------------------------------
// Signaling — ノックサーバーとの会話 (ゲームの語彙はここに無い)
// ---------------------------------------------------------------------------

interface SignalingEvents {
  onPeerJoined?: (peerId: string) => void;
  onPeerRejoined?: (peerId: string) => void;
  onPeerLeft?: (peerId: string) => void;
  onSignal?: (from: string, kind: SignalKind, payload: unknown) => void;
  /** WS が切れた (再 knock の契機。部屋は TTL まで待受けている)。 */
  onClosed?: () => void;
}

export class Signaling {
  private ws: WebSocket | null = null;
  private pendingReply: { resolve: (m: Record<string, unknown>) => void; reject: (e: Error) => void } | null = null;
  events: SignalingEvents = {};
  peerId = "";
  roomCode = "";
  turn: TurnCredView | null = null;

  /** 接続して create/join の準備をする。 */
  private connect(url: string): Promise<WebSocket> {
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(url);
      ws.onopen = () => resolve(ws);
      ws.onerror = () => reject(new Error(`ノックサーバーに接続できません: ${url}`));
      ws.onclose = () => {
        this.pendingReply?.reject(new Error("ノックサーバーとの接続が切れました"));
        this.pendingReply = null;
        this.events.onClosed?.();
      };
      ws.onmessage = (ev) => this.dispatch(String(ev.data));
      this.ws = ws;
    });
  }

  private dispatch(text: string) {
    let m: Record<string, unknown>;
    try {
      m = JSON.parse(text) as Record<string, unknown>;
    } catch {
      return;
    }
    switch (m.type) {
      case "created":
      case "joined": {
        const p = this.pendingReply;
        this.pendingReply = null;
        p?.resolve(m);
        break;
      }
      case "error": {
        // create/join 待ちならその失敗として返す。それ以外 (中継先不明等) は握らず console へ。
        const p = this.pendingReply;
        this.pendingReply = null;
        if (p) p.reject(new Error(String(m.message ?? m.code)));
        else console.warn("[knock]", m.code, m.message);
        break;
      }
      case "peer_joined":
        this.events.onPeerJoined?.(String(m.peer_id));
        break;
      case "peer_rejoined":
        this.events.onPeerRejoined?.(String(m.peer_id));
        break;
      case "peer_left":
        this.events.onPeerLeft?.(String(m.peer_id));
        break;
      case "signal":
        this.events.onSignal?.(String(m.from), m.kind as SignalKind, m.payload);
        break;
    }
  }

  private send(obj: Record<string, unknown>) {
    this.ws?.send(JSON.stringify(obj));
  }

  private awaitReply(): Promise<Record<string, unknown>> {
    return new Promise((resolve, reject) => {
      this.pendingReply = { resolve, reject };
    });
  }

  /** 部屋を開く (ホスト)。戻り = 部屋コード。 */
  async create(knockUrl: string): Promise<string> {
    await this.connect(knockUrl);
    const reply = this.awaitReply();
    this.send({ type: "create", protocol_version: KNOCK_PROTOCOL_VERSION });
    const m = await reply;
    this.roomCode = String(m.room_code);
    this.peerId = String(m.peer_id);
    this.turn = (m.turn as TurnCredView | undefined) ?? null;
    return this.roomCode;
  }

  /** 部屋へ入る (ゲスト)。`reuse` = 再 knock (前回の peer_id で張り直し)。戻り = 先住ピア。 */
  async join(knockUrl: string, roomCode: string, reuse?: string): Promise<string[]> {
    await this.connect(knockUrl);
    const reply = this.awaitReply();
    this.send({
      type: "join",
      room_code: roomCode.trim(),
      protocol_version: KNOCK_PROTOCOL_VERSION,
      ...(reuse ? { peer_id: reuse } : {}),
    });
    const m = await reply;
    this.roomCode = roomCode.trim();
    this.peerId = String(m.peer_id);
    this.turn = (m.turn as TurnCredView | undefined) ?? null;
    return (m.peers as string[] | undefined) ?? [];
  }

  signal(to: string, kind: SignalKind, payload: unknown) {
    this.send({ type: "signal", kind, to, payload });
  }

  close() {
    const ws = this.ws;
    this.ws = null;
    if (ws) {
      ws.onclose = null; // 意図した close は onClosed (再接続契機) を発火させない
      ws.close();
    }
  }
}

// ---------------------------------------------------------------------------
// RTCPeerConnection の共通配線
// ---------------------------------------------------------------------------

function iceServersFrom(turn: TurnCredView | null): RTCIceServer[] {
  if (!turn || turn.urls.length === 0) return [];
  return [{ urls: turn.urls, username: turn.username, credential: turn.credential }];
}

function newPeer(sig: Signaling, remote: string): RTCPeerConnection {
  const pc = new RTCPeerConnection({ iceServers: iceServersFrom(sig.turn) });
  pc.onicecandidate = (ev) => {
    if (ev.candidate) sig.signal(remote, "candidate", ev.candidate.toJSON());
  };
  // 音声 (spec 23 Phase D): **offer/answer を作る前**に sendrecv トランシーバを張る。
  // これでマイクの ON/OFF が replaceTrack だけで済み、再ネゴシエーションが要らない。
  voice.attach(remote, pc);
  return pc;
}

/** 契約 transport のメッセージ上限。超過は送るが警告 (未決2 の計測点)。 */
const MESSAGE_LIMIT = 64 * 1024;

function sendJson(ch: RTCDataChannel, obj: unknown) {
  const text = JSON.stringify(obj);
  if (text.length > MESSAGE_LIMIT) {
    console.warn(`[rtc] メッセージが契約上限 64KB を超過 (${text.length}B) — 分割実装の判断材料 (spec 23 未決2)`);
  }
  if (ch.readyState === "open") ch.send(text);
}

// ---------------------------------------------------------------------------
// HostTable — ホスト側ハブ (正本の門番)
// ---------------------------------------------------------------------------

/** ゲストから受ける command の whitelist。これ以外は拒否 —
 * play_turn / play_party_turn / set_participants / facts_* はホスト専権
 * (締切の判断・卓の編成は決定 4 のとおりホストの責務)。 */
const GUEST_COMMANDS = new Set([
  "submit_turn_input",
  "turn_input_status",
  "reveal_next",
  "reveal_all",
  "state_view_for",
]);

export interface GuestSeat {
  peerId: string;
  displayName: string;
  packageMatch: PackageMatch;
  connected: boolean;
}

export class HostTable {
  private sig = new Signaling();
  private pcs = new Map<string, RTCPeerConnection>();
  private channels = new Map<string, RTCDataChannel>();
  private unlisten: (() => void) | null = null;
  /** hello 済みのゲスト席 (UI の「入室: ○○」表示素材)。 */
  seats = new Map<string, GuestSeat>();
  onSeatsChanged?: () => void;
  /** 同じ表示名の席を置き換えた (入り直し or 同名の別人)。ホストへ知らせる。 */
  onSeatReplaced?: (displayName: string) => void;
  /** ゲスト発の開帳が確定した (ホスト UI も追従して開く)。 */
  onRevealOrder?: (rv: { revealed: number; total: number }) => void;
  /** ゲストの提出で入力窓が動いた (ホスト UI の「入力待ち」更新)。 */
  onInputStatus?: (st: unknown) => void;

  constructor(
    private local: GameTransport,
    private hello: HelloIdentity,
  ) {}

  /**
   * 中継へ預けた zip の sha256 を確定する (契約 `package_relay`)。
   * 部屋コードが要るので**アップロードは open の後**になる — 以後の hello に載る。
   */
  setRelaySha256(sha256: string | null) {
    this.hello.relaySha256 = sha256;
  }

  /** 部屋を開く。以後、ゲストの offer を待ち受ける (offer は入る側が出す規約)。 */
  async open(knockUrl: string): Promise<string> {
    const code = await this.sig.create(knockUrl);
    this.sig.events.onSignal = (from, kind, payload) => void this.onSignal(from, kind, payload);
    this.sig.events.onPeerLeft = (peer) => this.dropSeat(peer, /*keep*/ true);
    // backend の push (synopsis-compacting 等) を全ゲストへ中継 (transport seam の onEvent 面)。
    this.unlisten = this.local.onEvent((name, payload) => {
      this.broadcast({ type: "game_event", name, payload });
    });
    return code;
  }

  private async onSignal(from: string, kind: SignalKind, payload: unknown) {
    if (kind === "offer") {
      // 再 knock の再 offer も同じ経路 — 古い接続は作り直す (張り直しの実体)。
      this.pcs.get(from)?.close();
      voice.detach(from);
      const pc = newPeer(this.sig, from);
      this.pcs.set(from, pc);
      pc.ondatachannel = (ev) => this.wireChannel(from, ev.channel);
      await pc.setRemoteDescription(payload as RTCSessionDescriptionInit);
      const answer = await pc.createAnswer();
      await pc.setLocalDescription(answer);
      this.sig.signal(from, "answer", pc.localDescription?.toJSON());
    } else if (kind === "candidate") {
      await this.pcs.get(from)?.addIceCandidate(payload as RTCIceCandidateInit).catch(() => {});
    }
  }

  private wireChannel(peerId: string, ch: RTCDataChannel) {
    this.channels.set(peerId, ch);
    ch.onmessage = (ev) => void this.onGuestMessage(peerId, String(ev.data));
    ch.onclose = () => this.dropSeat(peerId, /*keep*/ true);
  }

  private async onGuestMessage(peerId: string, text: string) {
    let m: Record<string, unknown>;
    try {
      m = JSON.parse(text) as Record<string, unknown>;
    } catch {
      return;
    }
    if (m.type === "hello") {
      const h = m as unknown as TableHello;
      // **同じ表示名の古い席は畳む**。再入場は新しい peer_id で来る (再 knock と違い
      // identity が変わる) ので、放っておくと同じ人が二席を占め、参加者が水増しされる。
      // 席を消すだけでなく接続も切る — 残しておくと本人は入れているつもりのまま、
      // ホストからは見えない席で待ち続けることになる。
      for (const [old, seat] of [...this.seats]) {
        if (old !== peerId && seat.displayName === h.display_name) {
          this.channels.get(old)?.close();
          this.channels.delete(old);
          this.pcs.get(old)?.close();
          this.pcs.delete(old);
          this.seats.delete(old);
          // **黙って追い出さない**。同一人物の入り直しなら情報だが、たまたま同名の
          // 別人なら先客が理由も分からず消えることになる (卓は名前で人を識別する)。
          this.onSeatReplaced?.(h.display_name);
        }
      }
      this.seats.set(peerId, {
        peerId,
        displayName: h.display_name,
        packageMatch: judgePackageMatch(
          { package_id: this.hello.packageId, content_hash: this.hello.contentHash },
          { package_id: h.package_id, content_hash: h.content_hash },
        ),
        connected: true,
      });
      // ホストの自己紹介を返す (ゲスト側は role=host の相手を配送先として確定する)。
      const reply: TableHello = {
        type: "hello",
        role: "host",
        protocol_version: KNOCK_PROTOCOL_VERSION,
        display_name: this.hello.displayName,
        package_id: this.hello.packageId,
        content_hash: this.hello.contentHash,
        relay_sha256: this.hello.relaySha256,
      };
      this.sendTo(peerId, reply);
      this.onSeatsChanged?.();
      return;
    }
    if (m.type === "game_request") {
      const id = m.id as number;
      const cmd = String(m.cmd);
      if (!GUEST_COMMANDS.has(cmd)) {
        this.sendTo(peerId, { type: "game_response", id, ok: false, error: `この操作はホスト専権です: ${cmd}` });
        return;
      }
      // peer 束縛: 自分の peer_id でしか提出・閲覧できない (他人の秘密 view を引けない)。
      const args = { ...((m.args as Record<string, unknown>) ?? {}) };
      if (cmd === "submit_turn_input" || cmd === "state_view_for") {
        args.peerId = peerId;
      }
      try {
        const result = await this.local.request<unknown>(cmd, args);
        this.sendTo(peerId, { type: "game_response", id, ok: true, result });
        // 開帳・提出はセッション状態が動く → 全員へ配って画面を揃える (卓の「せーの」)。
        if (cmd === "reveal_next" || cmd === "reveal_all") {
          const rv = result as { revealed: number; total: number };
          this.broadcast({ type: "reveal_order", revealed: rv.revealed, total: rv.total });
          this.onRevealOrder?.(rv);
        } else if (cmd === "submit_turn_input") {
          this.broadcast({ type: "input_status", status: result });
          this.onInputStatus?.(result);
        }
      } catch (e) {
        this.sendTo(peerId, { type: "game_response", id, ok: false, error: String(e) });
      }
    }
  }

  private dropSeat(peerId: string, keep: boolean) {
    const seat = this.seats.get(peerId);
    if (seat) {
      // 再 knock で戻れるよう席は残し、接続状態だけ落とす (AFK/断の透明性)。
      if (keep) seat.connected = false;
      else this.seats.delete(peerId);
      this.onSeatsChanged?.();
    }
  }

  sendTo(peerId: string, obj: unknown) {
    const ch = this.channels.get(peerId);
    if (ch) sendJson(ch, obj);
  }

  broadcast(obj: unknown) {
    for (const ch of this.channels.values()) sendJson(ch, obj);
  }

  /** ホスト UI 発の開帳/入力状態/ターン結果/タイマーをゲストへ配る (store から呼ぶ)。 */
  broadcastRevealOrder(rv: { revealed: number; total: number }) {
    this.broadcast({ type: "reveal_order", revealed: rv.revealed, total: rv.total });
  }
  broadcastInputStatus(status: unknown) {
    this.broadcast({ type: "input_status", status });
  }
  /** 残り秒の配布。**null = タイマー停止**（止めたことを伝えないと数字が残り続ける）。 */
  broadcastTimerSync(remainingSecs: number | null) {
    this.broadcast({ type: "timer_sync", remaining_secs: remainingSecs });
  }

  /**
   * **解散を伝えてから**畳む。ゲスト側で「ホストが卓を閉じた」と「回線が切れた」を
   * 区別できないと、意図的な解散のあとも再接続を回し続けることになる。
   *
   * DataChannel は `close()` で未送信分を捨てうるので、送出後にわずかに待ってから畳む。
   */
  closeAnnounced() {
    this.broadcast({ type: "table_closed" });
    setTimeout(() => this.close(), 200);
  }
  /** 締切→ターン確定の配布。turn は共有部、state は宛先別 (state_view_for で引いて渡す)。 */
  async distributeTurn(turn: unknown) {
    for (const [peerId, ch] of this.channels) {
      try {
        const state = await this.local.request<unknown>("state_view_for", { peerId });
        sendJson(ch, { type: "party_turn", turn, state });
      } catch (e) {
        console.warn("[rtc] 宛先別 view の配布に失敗:", peerId, e);
      }
    }
  }

  close() {
    this.unlisten?.();
    this.unlisten = null;
    for (const pc of this.pcs.values()) pc.close();
    this.pcs.clear();
    this.channels.clear();
    voice.close(); // マイクは必ず手放す (卓を出たのにインジケータが残るのは最悪の裏切り)
    this.sig.close();
  }
}

// ---------------------------------------------------------------------------
// GuestLink — ゲスト側 (RemoteTransport = GameTransport 実装)
// ---------------------------------------------------------------------------

export class GuestLink implements GameTransport {
  private sig = new Signaling();
  private pcs = new Map<string, RTCPeerConnection>();
  private hostChannel: RTCDataChannel | null = null;
  private pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();
  private handlers = new Set<GameEventHandler>();
  private nextId = 1;
  private knockUrl = "";
  private helloSent = new Set<string>();
  hostHello: TableHello | null = null;
  /** ホストのシグナリング peer_id。participants 側のホストは "host" 固定なので、
   *  音声レベルを席へ写すにはこの対応が要る (voice は peer_id しか知らない)。 */
  hostSignalId = "";
  /** 卓イベント (party_turn / input_status / reveal_order / timer_sync / hello)。store が購読。 */
  onTable?: (msg: Record<string, unknown>) => void;
  /** 全断した (再 knock は reconnect() で。部屋は TTL まで待受)。 */
  onDisconnected?: () => void;

  constructor(private hello: HelloIdentity) {}

  /** 自分の peer_id (join 後に確定。submit_turn_input / state_view_for の鍵)。 */
  get peerId(): string {
    return this.sig.peerId;
  }

  /** 部屋へ入り、先住ピア全員へ offer を出す (入る側が offer の規約)。 */
  async join(knockUrl: string, roomCode: string): Promise<void> {
    this.knockUrl = knockUrl;
    const peers = await this.sig.join(knockUrl, roomCode);
    this.sig.events.onSignal = (from, kind, payload) => void this.onSignal(from, kind, payload);
    this.sig.events.onPeerRejoined = () => {};
    for (const p of peers) await this.offerTo(p);
  }

  /** 再 knock (契約 room_code)。identity (peer_id) を保って張り直す。 */
  async reconnect(roomCode: string): Promise<void> {
    const reuse = this.sig.peerId || undefined;
    // 古い WS を先に畳む。DataChannel だけが死んで WS が生きている場合、閉じずに
    // 再 join すると同じ peer_id の接続がノックサーバー側で二重になる
    // (close() は peerId を保持するので reuse は生き残る)。
    this.sig.close();
    const peers = await this.sig.join(this.knockUrl, roomCode, reuse);
    this.sig.events.onSignal = (from, kind, payload) => void this.onSignal(from, kind, payload);
    this.helloSent.clear();
    this.hostChannel = null;
    for (const p of peers) await this.offerTo(p);
  }

  private async offerTo(remote: string) {
    this.pcs.get(remote)?.close();
    const pc = newPeer(this.sig, remote);
    this.pcs.set(remote, pc);
    const ch = pc.createDataChannel("kataribe");
    ch.onopen = () => {
      if (!this.helloSent.has(remote)) {
        this.helloSent.add(remote);
        const hello: TableHello = {
          type: "hello",
          role: "guest",
          protocol_version: KNOCK_PROTOCOL_VERSION,
          display_name: this.hello.displayName,
          package_id: this.hello.packageId,
          content_hash: this.hello.contentHash,
          relay_sha256: null, // 中継へ預けるのはホストだけ
        };
        sendJson(ch, hello);
      }
    };
    ch.onmessage = (ev) => this.onChannelMessage(remote, ch, String(ev.data));
    ch.onclose = () => {
      if (this.hostChannel === ch) {
        this.hostChannel = null;
        for (const p of this.pending.values()) p.reject(new Error("ホストとの接続が切れました"));
        this.pending.clear();
        this.onDisconnected?.();
      }
    };
    const offer = await pc.createOffer();
    await pc.setLocalDescription(offer);
    this.sig.signal(remote, "offer", pc.localDescription?.toJSON());
  }

  private async onSignal(from: string, kind: SignalKind, payload: unknown) {
    if (kind === "answer") {
      await this.pcs.get(from)?.setRemoteDescription(payload as RTCSessionDescriptionInit).catch(() => {});
    } else if (kind === "candidate") {
      await this.pcs.get(from)?.addIceCandidate(payload as RTCIceCandidateInit).catch(() => {});
    } else if (kind === "offer") {
      // **音声 mesh (Phase D)**: 他ゲストからの offer に応える。ゲーム配送は依然
      // guest→host だけ (この PC に DataChannel は張らない) で、ここで繋ぐのは音だけ。
      // 後から入ってきた人が offer を出す側 = 「入る側が offer」の規約は不変。
      this.pcs.get(from)?.close();
      voice.detach(from);
      const pc = newPeer(this.sig, from);
      this.pcs.set(from, pc);
      await pc.setRemoteDescription(payload as RTCSessionDescriptionInit);
      const answer = await pc.createAnswer();
      await pc.setLocalDescription(answer);
      this.sig.signal(from, "answer", pc.localDescription?.toJSON());
    }
  }

  private onChannelMessage(remote: string, ch: RTCDataChannel, text: string) {
    let m: Record<string, unknown>;
    try {
      m = JSON.parse(text) as Record<string, unknown>;
    } catch {
      return;
    }
    switch (m.type) {
      case "hello": {
        const h = m as unknown as TableHello;
        if (h.role === "host") {
          // role=host の相手が配送先 (部屋の作成者)。他ゲストとの接続は音声 (Phase D) 用に残す。
          this.hostChannel = ch;
          this.hostHello = h;
          this.hostSignalId = remote;
          this.onTable?.(m);
        }
        break;
      }
      case "game_response": {
        const p = this.pending.get(m.id as number);
        this.pending.delete(m.id as number);
        if (!p) break;
        if (m.ok) p.resolve(m.result);
        else p.reject(new Error(String(m.error)));
        break;
      }
      case "game_event": {
        const name = String(m.name) as GameEventName;
        if ((GAME_EVENTS as readonly string[]).includes(name)) {
          for (const h of this.handlers) h(name, m.payload);
        }
        break;
      }
      default:
        // **卓メッセージは既定で転送する** (table_start / party_turn / input_status /
        // reveal_order / timer_sync …)。ここを allowlist で列挙していたせいで
        // `table_start` が黙って捨てられ、ゲストの盤面が一度も始まらなかった
        // (failures #75)。転送側 (HostTable) に新しい種別が増えたとき、受け側の
        // 列挙を足し忘れても**落ちない**ようにする — 上で明示的に処理しているのは
        // 配送層のもの (hello / game_response / game_event) だけで、それ以外は
        // すべて卓の語彙として store が解釈する (知らない型は store 側で無視される)。
        this.onTable?.(m);
        break;
    }
  }

  // --- GameTransport ---

  request<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    const ch = this.hostChannel;
    if (!ch || ch.readyState !== "open") {
      return Promise.reject(new Error("ホストに接続していません"));
    }
    const id = this.nextId++;
    const p = new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
    });
    sendJson(ch, { type: "game_request", id, cmd, args: args ?? {} });
    return p;
  }

  onEvent(handler: GameEventHandler): () => void {
    this.handlers.add(handler);
    return () => this.handlers.delete(handler);
  }

  close() {
    for (const pc of this.pcs.values()) pc.close();
    this.pcs.clear();
    this.hostChannel = null;
    voice.close();
    this.sig.close();
  }
}
