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
export type TtsEngine = "webSpeech" | "voicevox" | "aivisSpeech";

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
}

/** ローカルエンジンの既定エンドポイント (ライブラリの定数と同じ値)。 */
export const DEFAULT_SERVER_URL: Record<TtsEngine, string> = {
  webSpeech: "",
  voicevox: "http://localhost:50021",
  aivisSpeech: "http://localhost:10101",
};

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
};

const LS_ENABLED = "kataribe.tts.enabled";
const LS_SETTINGS = "kataribe.tts.settings";

let adapter: VoiceEngineAdapter | null = null;
let booting: Promise<VoiceEngineAdapter | null> | null = null;
/** アダプタを組んだ時の設定。変わったら組み直す。 */
let builtWith = "";
/** 文分割 (長文の打ち切り対策)。アダプタと同じ動的 import から受け取る。 */
let splitSentence: ((text: string) => string[]) | null = null;
/** 今読み上げている世代。stop() / 次のターンで進め、古い世代の残りを捨てる。 */
let generation = 0;

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
  return voices.find((v) => /^ja/i.test(v.lang))?.name ?? "";
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

/** 設定画面の話者一覧。ローカルエンジンはサーバーへ問い合わせる (失敗は例外)。 */
export async function listVoices(
  s: TtsSettings,
): Promise<{ id: string; label: string }[]> {
  const mod = await import("@aituber-onair/voice");
  const voices = await mod.getVoiceEngineVoiceList(s.engine, {
    apiUrl: effectiveUrl(s) || undefined,
    language: "ja-JP",
  });
  return voices.map((v) => ({ id: v.id, label: v.label }));
}

/**
 * narration を読み上げる。**前の読み上げは必ず打ち切る** — ターンが進んだのに前の語りが
 * 喋り続けるのは「矛盾しない GM」の音声版の破れになる。
 *
 * 長文は文単位に割って順に流す (Chrome 系は 1 発話が長いと途中で切れる既知の癖があり、
 * ライブラリの `splitSentence` がそのまま使える)。
 */
export async function speak(text: string): Promise<void> {
  const body = text.trim();
  if (!body) return;
  const a = await ensureAdapter();
  if (!a) return;
  stop();
  const mine = ++generation;
  for (const sentence of splitSentence ? splitSentence(body) : [body]) {
    // 世代が進んでいたら (= スキップされた/次のターンが来た) 残りは捨てる。
    if (mine !== generation) return;
    try {
      await a.speakText(sentence);
    } catch {
      // stop() は待機中の Promise を reject する = 中断の正常系。読み上げ失敗も
      // プレイを止める理由にならないので握り潰す (TTS は装飾であって正本ではない)。
      return;
    }
  }
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

/** 読み上げを即座に止める (スキップ / OFF / 新しいターン / ゲーム切り替え)。 */
export function stop(): void {
  generation++;
  adapter?.stop();
  // アダプタ生成前に停止が来た場合の保険。
  globalThis.speechSynthesis?.cancel();
}
