// 卓の音声 (spec 23 Phase D) — mesh の「音」だけを持つ層。
//
// 中枢は **トランシーバを先に置く** こと: PC を作った時点で audio の `sendrecv`
// トランシーバを 1 本張っておくと、マイクの ON/OFF は `replaceTrack` だけで済み
// **再ネゴシエーションが一度も要らない**。OFF を `track.stop()` の完全解放
// (= OS のマイク使用インジケータが消える) にできるのはこの構造のおかげで、
// 後から `addTrack` する素朴な作りだと OFF のたびに SDP をやり直すことになる。
//
// **既定は OFF**。音声なし参加 = 一度も `getUserMedia` を呼ばない = 権限ダイアログも
// 出ない (契約どおりの挙動であると同時に、WebView の権限まわりが荒れても卓が
// 壊れないという実利がある)。
//
// レベル (発話インジケータ) はここで RMS まで出し、entity への写像は table.ts が持つ
// (この層は peer_id しか知らない = ゲームの語彙を持ち込まない)。

/** レベル更新の頻度。60fps では回さない (契約 Phase D: throttle 10〜15Hz)。 */
const LEVEL_HZ = 12;

/** 自分のマイクを表す擬似 peer_id (レベル配布のキー)。 */
export const LOCAL_PEER = "__local__";

interface PeerAudio {
  /** 自分の音を送る口。`replaceTrack(null)` で黙る。 */
  sender: RTCRtpSender | null;
  /** 相手の音を鳴らす要素 (Web Audio だけでは再生されないので要素が要る)。 */
  el: HTMLAudioElement | null;
  analyser: AnalyserNode | null;
  source: MediaStreamAudioSourceNode | null;
}

class VoiceMesh {
  private peers = new Map<string, PeerAudio>();
  private ctx: AudioContext | null = null;
  private localStream: MediaStream | null = null;
  private localAnalyser: AnalyserNode | null = null;
  private localSource: MediaStreamAudioSourceNode | null = null;
  private timer: number | undefined;
  private buf = new Uint8Array(0);

  /** マイクが入っているか (UI の真実。OFF は完全解放であって「掴んだままミュート」ではない)。 */
  micOn = false;
  /** レベル配布 (peer_id → 0..1)。table.ts が entity へ写して store に載せる。 */
  onLevels?: (levels: Record<string, number>) => void;
  /** マイクの取得に失敗した (権限拒否・デバイス無し)。UI が理由を出すため。 */
  onMicError?: (message: string) => void;

  /**
   * PC ができたら呼ぶ。audio の sendrecv トランシーバを張り、相手の音を受ける口を配線する。
   * **offer/answer を作る前に呼ぶこと** — 後から足すと再ネゴシエーションが要る。
   */
  attach(peerId: string, pc: RTCPeerConnection) {
    this.detach(peerId);
    const entry: PeerAudio = { sender: null, el: null, analyser: null, source: null };
    const tx = pc.addTransceiver("audio", { direction: "sendrecv" });
    entry.sender = tx.sender;
    // マイクが既に入っているなら今すぐ載せる (後から入った相手にも自分の声が届く)。
    const track = this.localStream?.getAudioTracks()[0] ?? null;
    if (track) void tx.sender.replaceTrack(track).catch(() => {});
    pc.ontrack = (ev) => this.playRemote(peerId, ev.streams[0] ?? new MediaStream([ev.track]));
    this.peers.set(peerId, entry);
  }

  /** 相手の音を鳴らし、レベル計測につなぐ。 */
  private playRemote(peerId: string, stream: MediaStream) {
    const entry = this.peers.get(peerId);
    if (!entry) return;
    if (!entry.el) {
      const el = new Audio();
      el.autoplay = true;
      // 自動再生の制約に当たることがあるが、卓では必ずユーザー操作 (参加/マイク) の
      // 後に音が来るので実害は薄い。失敗は握り潰す (音が出ないだけで卓は続く)。
      el.srcObject = stream;
      void el.play().catch(() => {});
      entry.el = el;
    } else {
      entry.el.srcObject = stream;
    }
    const ctx = this.audioContext();
    if (!ctx) return;
    entry.source?.disconnect();
    entry.analyser?.disconnect();
    const source = ctx.createMediaStreamSource(stream);
    const analyser = ctx.createAnalyser();
    analyser.fftSize = 512;
    source.connect(analyser); // destination へは繋がない (再生は <audio> の担当・二重に鳴らさない)
    entry.source = source;
    entry.analyser = analyser;
    this.startLevelLoop();
  }

  /** PC を畳んだら呼ぶ。 */
  detach(peerId: string) {
    const entry = this.peers.get(peerId);
    if (!entry) return;
    entry.source?.disconnect();
    entry.analyser?.disconnect();
    if (entry.el) {
      entry.el.srcObject = null;
      entry.el.pause();
    }
    this.peers.delete(peerId);
  }

  /**
   * マイクの ON/OFF。**OFF は完全解放** — `track.stop()` してデバイスを手放すので
   * OS のマイク使用インジケータが消える。「掴んだままミュート」の中間状態は作らない
   * (状態が二段あると UI と OS 表示の対応が曖昧になる)。
   */
  async setMic(on: boolean): Promise<void> {
    if (on === this.micOn) return;
    if (on) {
      let stream: MediaStream;
      try {
        stream = await navigator.mediaDevices.getUserMedia({
          audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
        });
      } catch (e) {
        this.onMicError?.(String(e));
        return;
      }
      this.localStream = stream;
      this.micOn = true;
      const track = stream.getAudioTracks()[0] ?? null;
      for (const entry of this.peers.values()) {
        if (entry.sender) void entry.sender.replaceTrack(track).catch(() => {});
      }
      const ctx = this.audioContext();
      if (ctx) {
        void ctx.resume().catch(() => {});
        this.localSource = ctx.createMediaStreamSource(stream);
        this.localAnalyser = ctx.createAnalyser();
        this.localAnalyser.fftSize = 512;
        this.localSource.connect(this.localAnalyser); // 自分の声は鳴らさない (ハウリング防止)
      }
      this.startLevelLoop();
    } else {
      this.micOn = false;
      for (const entry of this.peers.values()) {
        if (entry.sender) void entry.sender.replaceTrack(null).catch(() => {});
      }
      this.localSource?.disconnect();
      this.localAnalyser?.disconnect();
      this.localSource = null;
      this.localAnalyser = null;
      // **ここが完全解放**。stop() までやらないと OS のインジケータは消えない。
      this.localStream?.getTracks().forEach((t) => t.stop());
      this.localStream = null;
      this.emitLevels();
    }
  }

  /** 卓を畳む。マイクは必ず手放す (卓を出たのにインジケータが残るのは最悪の裏切り)。 */
  close() {
    void this.setMic(false);
    for (const id of [...this.peers.keys()]) this.detach(id);
    this.stopLevelLoop();
    void this.ctx?.close().catch(() => {});
    this.ctx = null;
  }

  private audioContext(): AudioContext | null {
    if (!this.ctx) {
      try {
        this.ctx = new AudioContext();
      } catch {
        return null; // 解析ができないだけ = 音は鳴る。インジケータが静止する。
      }
    }
    return this.ctx;
  }

  private startLevelLoop() {
    if (this.timer !== undefined) return;
    this.timer = window.setInterval(() => this.emitLevels(), Math.round(1000 / LEVEL_HZ));
  }

  private stopLevelLoop() {
    if (this.timer !== undefined) window.clearInterval(this.timer);
    this.timer = undefined;
  }

  private emitLevels() {
    const levels: Record<string, number> = {};
    if (this.localAnalyser) levels[LOCAL_PEER] = this.rms(this.localAnalyser);
    for (const [peerId, entry] of this.peers) {
      if (entry.analyser) levels[peerId] = this.rms(entry.analyser);
    }
    this.onLevels?.(levels);
  }

  /** 時間領域の RMS を 0..1 へ。小さい声でもリングが動くよう軽く持ち上げる。 */
  private rms(analyser: AnalyserNode): number {
    const n = analyser.fftSize;
    if (this.buf.length !== n) this.buf = new Uint8Array(n);
    analyser.getByteTimeDomainData(this.buf);
    let sum = 0;
    for (let i = 0; i < n; i++) {
      const v = (this.buf[i] - 128) / 128;
      sum += v * v;
    }
    const raw = Math.sqrt(sum / n);
    return Math.min(1, raw * 4);
  }
}

export const voice = new VoiceMesh();
