// 読み上げ (TTS)。**提示層だけの機能** — 正本 (gm_core) も prompt も語りも一切変えない。
// 読むのは受理ターンの narration だけで、TTS の ON/OFF で物語の書かれ方は変わらない
// (変わると chronicle/synopsis に残る記録まで再生設定で食い違う)。
//
// エンジンは `@aituber-onair/voice` に委ねる (MIT・ランタイム依存ゼロ)。エンジン差は
// ライブラリが吸収するので、Kataribe 側は**共通の 3 パラメータ (速度・高さ・音量) を
// エンジン別のキーへ写す**だけを持つ。
//
// **値の import は動的だけ** (`import type` は型のみで実体を持たない) — 静的に一つでも
// 値を取ると、TTS を使わないセッションでもライブラリが起動時に読み込まれる。
import type { VoiceEngineAdapter } from "@aituber-onair/voice";

/** 対応エンジン。ブラウザ内蔵は導入ゼロ、ローカル 2 種は別アプリの常駐が要る。 */
export type TtsEngine = "webSpeech" | "voicevox" | "aivisSpeech" | "openaiCompatible";

export interface TtsSettings {
  engine: TtsEngine;
  /** ローカルエンジンのサーバー URL。空なら既定値。 */
  serverUrl: string;
  /** 話者 ID (webSpeech は音声名)。空なら自動選択。 */
  speaker: string;
  /** 速度。1.0 = 標準 (どのエンジンも倍率なので共通)。 */
  rate: number;
  /** 高さ。0 = 標準 (エンジン別の実レンジへ写す)。 */
  pitch: number;
  /** 音量。1.0 = 標準・最大。 */
  volume: number;
  /** OpenAI 互換エンドポイントのモデル名 (**必須** — 空だとライブラリが例外を投げる)。 */
  model: string;
}

/** ローカルエンジンの既定エンドポイント (ライブラリの定数と同じ値)。 */
export const DEFAULT_SERVER_URL: Record<TtsEngine, string> = {
  webSpeech: "",
  voicevox: "http://localhost:50021",
  aivisSpeech: "http://localhost:10101",
  // OpenAI 互換は**エンドポイント全体**を渡す (base URL ではない)。既定は
  // Irodori-TTS-Server の既定ポート。他の OpenAI 互換 TTS サーバーにも同じ口で刺さる。
  openaiCompatible: "http://localhost:8088/v1/audio/speech",
};

/**
 * 話者一覧をサーバーから引けるか。**OpenAI 互換は非対応** (ライブラリが例外を投げる) —
 * voice の語彙はサーバー実装ごとに違い、共通の列挙 API が無いため。UI は自由入力にする。
 */
export function supportsVoiceList(engine: TtsEngine): boolean {
  return engine !== "openaiCompatible";
}

/**
 * 高さ・音量を持つか。**OpenAI 互換は速度 (`speed`) しか持たない** — API の仕様であって
 * 実装の手抜きではないので、UI 側でスライダを無効化して誤解を作らない。
 */
export function supportsPitchAndVolume(engine: TtsEngine): boolean {
  return engine !== "openaiCompatible";
}

/** ブラウザ内蔵以外は外部サーバーが要る (UI が導入の注意を出す判断に使う)。 */
export function needsServer(engine: TtsEngine): boolean {
  return engine !== "webSpeech";
}

export const DEFAULT_SETTINGS: TtsSettings = {
  engine: "webSpeech",
  serverUrl: "",
  speaker: "",
  rate: 1.0,
  pitch: 0,
  volume: 1.0,
  model: "",
};

const LS_ENABLED = "kataribe.tts.enabled";
const LS_SETTINGS = "kataribe.tts.settings";
const LS_VOICES = "kataribe.tts.voices";

let adapter: VoiceEngineAdapter | null = null;
let booting: Promise<VoiceEngineAdapter | null> | null = null;
/** アダプタを組んだ時の設定。変わったら組み直す。 */
let builtWith = "";
/** 文分割 (長文の打ち切り対策)。アダプタと同じ動的 import から受け取る。 */
let splitSentence: ((text: string) => string[]) | null = null;
/** 選んだ日本語音声名 (webSpeech)。アダプタを組み直すたびに探し直さないため。 */
let pickedJaVoice = "";
/** 今読み上げている世代。stop() / 次のターンで進め、古い世代の残りを捨てる。 */
let generation = 0;
/**
 * 読み上げの直列鎖。**1 文ずつ順に合成・再生する**ための連結 Promise。
 *
 * まとめてアダプタへ投入すると、ネットワーク型エンジン (VOICEVOX/AivisSpeech) では
 * 全文の合成リクエストが**一斉に並列発射**される。ローカル GPU に十数本を同時に投げると
 * 後続がライブラリ既定の 30 秒タイムアウトに掛かり、こちらは失敗を握り潰すので
 * **その文が黙って飛ぶ** (読み上げが途中で切れたように聞こえる)。
 *
 * かといって 1 文ずつ await して積むと、その隙間に後発の `queue: true` が割り込んで
 * 順序が崩れる。**自前の鎖に繋ぐ**ことで、直列と順序保存を同時に満たす。
 */
let chain: Promise<void> = Promise.resolve();

/** この環境で読み上げできるか (WebView2/Chromium は speechSynthesis を持つ)。 */
export function isSupported(): boolean {
  return typeof globalThis.speechSynthesis !== "undefined";
}

/**
 * ユーザー設定の ON/OFF (localStorage 永続)。
 *
 * **未設定は ON** — 読み上げ操作が出るのは作者が `use_tts: true` と宣言した盤面だけなので、
 * そこで既定 OFF にすると宣言が何も起こさない (ホバーで隠れた操作を自力で見つけて押すまで
 * 無音のまま = 二重に隠れる)。宣言のない盤面では speak 自体が呼ばれないので、
 * 既定 ON にしても勝手に喋り出す事故は起きない。
 */
export function loadEnabled(): boolean {
  return localStorage.getItem(LS_ENABLED) !== "0";
}

export function saveEnabled(on: boolean): void {
  localStorage.setItem(LS_ENABLED, on ? "1" : "0");
}

export function loadSettings(): TtsSettings {
  try {
    const raw = localStorage.getItem(LS_SETTINGS);
    if (!raw) return { ...DEFAULT_SETTINGS };
    // 欠けたキーは既定で埋める (設定項目が増えても古い保存を壊さない)。
    return { ...DEFAULT_SETTINGS, ...(JSON.parse(raw) as Partial<TtsSettings>) };
  } catch {
    return { ...DEFAULT_SETTINGS };
  }
}

export function saveSettings(s: TtsSettings): void {
  localStorage.setItem(LS_SETTINGS, JSON.stringify(s));
  // 次の読み上げで新しい設定のアダプタが組まれる。
  invalidate();
}

/** 設定が変わったのでアダプタを捨てる (次回 speak で組み直す)。 */
export function invalidate(): void {
  stop();
  adapter = null;
  booting = null;
  builtWith = "";
}

/** 実効サーバー URL (未入力なら既定)。 */
export function effectiveUrl(s: TtsSettings): string {
  return s.serverUrl.trim() || DEFAULT_SERVER_URL[s.engine];
}

/**
 * 共通 3 パラメータをエンジン別のキーへ写す。**レンジの意味が違う所だけ変換する**:
 * 速度はどれも倍率なのでそのまま、高さは webSpeech が 0〜2 (1=標準)・ローカル 2 種が
 * ±0.15 (0=標準) なので、Kataribe の −1〜+1 (0=標準) から写す。
 */
function engineOptions(s: TtsSettings): Record<string, unknown> {
  switch (s.engine) {
    case "voicevox":
      return {
        engineType: "voicevox",
        voicevoxApiUrl: effectiveUrl(s),
        voicevoxSpeedScale: s.rate,
        voicevoxPitchScale: s.pitch * 0.15,
        voicevoxVolumeScale: s.volume,
      };
    case "aivisSpeech":
      return {
        engineType: "aivisSpeech",
        aivisSpeechApiUrl: effectiveUrl(s),
        aivisSpeechSpeedScale: s.rate,
        aivisSpeechPitchScale: s.pitch * 0.15,
        aivisSpeechVolumeScale: s.volume,
      };
    case "openaiCompatible":
      // **未検証** (2026-07-22): サーバーを立てずに口だけ作った。CORS を返すか、
      // 実際に鳴るかは実サーバーで要確認。速度以外のパラメータは API に無い。
      return {
        engineType: "openaiCompatible",
        openAiCompatibleApiUrl: effectiveUrl(s),
        openAiCompatibleModel: s.model.trim(),
        openAiCompatibleSpeed: s.rate,
      };
    default:
      return {
        engineType: "webSpeech",
        webSpeechRate: s.rate,
        webSpeechPitch: 1 + s.pitch,
        webSpeechVolume: s.volume,
        webSpeechLanguage: "ja-JP",
      };
  }
}

/**
 * 日本語の音声を一つ選ぶ (webSpeech で話者未指定のとき)。speechSynthesis の既定は
 * OS ロケール次第で英語音声になりうるので、ja を明示的に拾う。
 */
async function pickJapaneseVoice(): Promise<string> {
  // 一度見つけたら覚える。中断のたびにアダプタを組み直す (stop() の注記) ので、
  // ここで毎回 voiceschanged を最大 1 秒待つと読み上げの出だしが遅れる。
  if (pickedJaVoice) return pickedJaVoice;
  const synth = globalThis.speechSynthesis;
  if (!synth) return "";
  let voices = synth.getVoices();
  if (voices.length === 0) {
    voices = await new Promise<SpeechSynthesisVoice[]>((resolve) => {
      const done = () => resolve(synth.getVoices());
      const timer = setTimeout(done, 1000);
      synth.addEventListener("voiceschanged", () => {
        clearTimeout(timer);
        done();
      }, { once: true });
    });
  }
  // 見つからなかった時は覚えない (後から voices が揃う環境で拾い直せるように)。
  const found = voices.find((v) => /^ja/i.test(v.lang))?.name ?? "";
  if (found) pickedJaVoice = found;
  return found;
}

/** アダプタを遅延生成する (TTS を使うまでエンジン群のチャンクを読み込まない)。 */
async function ensureAdapter(): Promise<VoiceEngineAdapter | null> {
  const s = loadSettings();
  const key = JSON.stringify(s);
  if (adapter && builtWith === key) return adapter;
  if (booting && builtWith === key) return booting;
  builtWith = key;
  booting = (async () => {
    if (s.engine === "webSpeech" && !isSupported()) return null;
    const mod = await import("@aituber-onair/voice");
    splitSentence = mod.splitSentence;
    const speaker =
      s.speaker.trim() || (s.engine === "webSpeech" ? await pickJapaneseVoice() : "");
    adapter = new mod.VoiceEngineAdapter({
      ...engineOptions(s),
      speaker,
    } as never);
    return adapter;
  })();
  return booting;
}

export interface VoiceOption {
  id: string;
  label: string;
}

/**
 * 取得済みの話者一覧のキャッシュ。**話者 ID だけ保存しても選択肢が無ければ select は
 * 空白に見える**ので、一覧ごと持ち越す (設定を開き直すたびにサーバーへ問い合わせない
 * 利点もある)。エンジンとサーバー URL を鍵にし、どちらか変わったら捨てる
 * (別エンジンの ID は通用しない)。
 */
export function loadVoiceList(s: TtsSettings): VoiceOption[] {
  try {
    const raw = localStorage.getItem(LS_VOICES);
    if (!raw) return [];
    const c = JSON.parse(raw) as { engine?: string; url?: string; voices?: VoiceOption[] };
    if (c.engine !== s.engine || c.url !== effectiveUrl(s)) return [];
    return c.voices ?? [];
  } catch {
    return [];
  }
}

export function saveVoiceList(s: TtsSettings, voices: VoiceOption[]): void {
  localStorage.setItem(
    LS_VOICES,
    JSON.stringify({ engine: s.engine, url: effectiveUrl(s), voices }),
  );
}

export function clearVoiceList(): void {
  localStorage.removeItem(LS_VOICES);
}

/** 設定画面の話者一覧。ローカルエンジンはサーバーへ問い合わせる (失敗は例外)。 */
export async function listVoices(
  s: TtsSettings,
): Promise<VoiceOption[]> {
  const mod = await import("@aituber-onair/voice");
  const voices = await mod.getVoiceEngineVoiceList(s.engine, {
    apiUrl: effectiveUrl(s) || undefined,
    language: "ja-JP",
  });
  return voices.map((v) => ({ id: v.id, label: v.label }));
}

/**
 * 読み上げる。既定は**前の読み上げを打ち切って置き換える** — ターンが進んだのに前の語りが
 * 喋り続けるのは「矛盾しない GM」の音声版の破れになる。
 *
 * `queue: true` は**前に続けて足す**。同じターンの中で後から出る文 (エピローグ) 用で、
 * 語りを途中で切らずに繋ぐ。
 *
 * 長文は文単位に割る (Chrome 系は 1 発話が長いと途中で切れる既知の癖があり、ライブラリの
 * `splitSentence` がそのまま使える)。**自前の直列鎖 (`chain`) に繋ぐ**のが要点 —
 * まとめて投入すると合成リクエストが並列発射されてタイムアウトで文が飛び、1 文ずつ
 * await して積むと隙間に後発が割り込む。鎖なら直列と順序保存を同時に満たす。
 */
export async function speak(text: string, opts?: { queue?: boolean }): Promise<void> {
  const body = text.trim();
  if (!body) return;
  if (!opts?.queue) {
    // 割り込み = 前の読み上げを捨てる。世代を進めるのもアダプタの始末も stop() が担う
    // ので、**アダプタを取る前に**呼ぶ (後だと今から使うアダプタを捨ててしまう)。
    stop();
    chain = Promise.resolve(); // 前の鎖は世代チェックで空回りするので繋ぎ直す
  }
  // 続けて足す時は現在の世代に相乗りする (互いを無効化しない)。
  const mine = generation;
  const a = await ensureAdapter();
  if (!a) return;
  // アダプタ生成を待つ間に stop() / 次のターンが来ていたら、もう投入しない。
  if (mine !== generation) return;
  const sentences = splitSentence ? splitSentence(body) : [body];
  for (const sentence of sentences) {
    chain = chain.then(async () => {
      // 鎖に積んだ後で打ち切られたら (stop / 次のターン) 残りは読まない。
      if (mine !== generation) return;
      // stop() は待機中の Promise を reject する = 中断の正常系。読み上げ失敗もプレイを
      // 止める理由にならないので握り潰す (TTS は装飾であって正本ではない)。
      await a.speakText(sentence).catch(() => {});
    });
  }
  await chain;
}

/**
 * 設定画面の試聴。**失敗を投げる** — speak() は握り潰すが、こちらは
 * 「サーバーが起動していない」等をユーザーに見せるのが目的 (設定の接地)。
 */
export async function test(sample: string): Promise<void> {
  invalidate();
  const a = await ensureAdapter();
  if (!a) throw new Error("speechSynthesis unavailable");
  await a.speakText(sample);
}

/**
 * 読み上げを即座に止める (スキップ / OFF / 新しいターン / ゲーム切り替え)。
 *
 * **再生中に止めたアダプタは二度と喋らない** (ライブラリの罠・実測で確認):
 * `BrowserAudioPlayer.stop()` は `audioElement.pause()` と `currentTime = 0` をするだけで
 * `play()` の Promise を settle しない (pause は `ended` も `error` も発火しない)。すると
 * `VoiceEngineAdapter.processQueue` が `await playPreparedSpeech` で永久に止まり
 * **`isProcessingQueue` が true のまま残る** → 以後の `speakText` は
 * `if (this.isProcessingQueue) return;` で即座に捨てられる (キューに積まれたまま鳴らない)。
 * こちらは失敗を握り潰す設計なので、症状は例外でなく**無音**として現れる。
 *
 * ゆえに**再生中に止めたときだけ**アダプタを捨てて次の speak で組み直す。合成待ちや
 * 停止済みならライブラリのキューは正常に巻き戻るので、そのまま使い回す。
 * ブラウザ内蔵 (webSpeech) は self-playing で `speechSynthesis.cancel()` が発話を
 * settle させるため本来この罠を踏まないが、判定を分けても得がないので一律で扱う。
 */
export function stop(): void {
  generation++;
  const dying = adapter;
  const wasPlaying = dying?.isPlaying() ?? false; // stop() が畳む前に見る
  dying?.stop();
  // アダプタ生成前に停止が来た場合の保険。
  globalThis.speechSynthesis?.cancel();
  if (wasPlaying) {
    adapter = null;
    booting = null;
    builtWith = "";
  }
}
