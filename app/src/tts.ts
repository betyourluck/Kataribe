// 読み上げ (TTS)。**提示層だけの機能** — 正本 (gm_core) も prompt も語りも一切変えない。
// 読むのは受理ターンの narration だけで、TTS の ON/OFF で物語の書かれ方は変わらない
// (変わると chronicle/synopsis に残る記録まで再生設定で食い違う)。
//
// エンジンは `@aituber-onair/voice` に委ねる (MIT・ランタイム依存ゼロ)。v1 はブラウザ内蔵の
// Web Speech (`webSpeech`) だけを使う = API キーもサーバも要らず、CSP の変更も要らない
// (fetch も blob も通らない)。VOICEVOX 等へ広げる時はこのファイルの engineType を増やすだけで、
// 呼び出し側 (store) の契約は変えない。
// **値の import は動的だけ** (`import type` は型のみで実体を持たない) — 静的に一つでも
// 値を取ると、TTS を使わないセッションでもライブラリが起動時に読み込まれる。
import type { VoiceEngineAdapter } from "@aituber-onair/voice";

/** 音量は 0.0〜1.0。localStorage キー。 */
const LS_ENABLED = "kataribe.tts.enabled";

let adapter: VoiceEngineAdapter | null = null;
let booting: Promise<VoiceEngineAdapter | null> | null = null;
/** 文分割 (長文の打ち切り対策)。アダプタと同じ動的 import から受け取る。 */
let splitSentence: ((text: string) => string[]) | null = null;
/** 今読み上げている世代。stop() / 次のターンで進め、古い世代の残りを捨てる。 */
let generation = 0;

/** この環境で読み上げできるか (WebView2/Chromium は speechSynthesis を持つ)。 */
export function isSupported(): boolean {
  return typeof globalThis.speechSynthesis !== "undefined";
}

/** ユーザー設定の ON/OFF (localStorage 永続)。既定 OFF = 黙って鳴り出さない。 */
export function loadEnabled(): boolean {
  return localStorage.getItem(LS_ENABLED) === "1";
}

export function saveEnabled(on: boolean): void {
  localStorage.setItem(LS_ENABLED, on ? "1" : "0");
}

/**
 * 日本語の音声を一つ選ぶ。speechSynthesis の既定は OS ロケール次第で英語音声になりうるので、
 * ja を明示的に拾う (見つからなければ undefined = エンジン既定に委ねる)。
 */
async function pickJapaneseVoice(): Promise<string | undefined> {
  const synth = globalThis.speechSynthesis;
  if (!synth) return undefined;
  let voices = synth.getVoices();
  if (voices.length === 0) {
    // 初回は非同期で埋まる。onvoiceschanged を短く待つ (来なくても既定で進む)。
    voices = await new Promise<SpeechSynthesisVoice[]>((resolve) => {
      const done = () => resolve(synth.getVoices());
      const timer = setTimeout(done, 1000);
      synth.addEventListener("voiceschanged", () => {
        clearTimeout(timer);
        done();
      }, { once: true });
    });
  }
  return voices.find((v) => /^ja/i.test(v.lang))?.name;
}

/** アダプタを遅延生成する (TTS を使うまでエンジン群のチャンクを読み込まない)。 */
async function ensureAdapter(): Promise<VoiceEngineAdapter | null> {
  if (adapter) return adapter;
  if (booting) return booting;
  booting = (async () => {
    if (!isSupported()) return null;
    const mod = await import("@aituber-onair/voice");
    const { VoiceEngineAdapter: Adapter } = mod;
    splitSentence = mod.splitSentence;
    const speaker = await pickJapaneseVoice();
    adapter = new Adapter({
      engineType: "webSpeech",
      speaker: speaker ?? "",
      webSpeechLanguage: "ja-JP",
    });
    return adapter;
  })();
  return booting;
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

/** 読み上げを即座に止める (スキップ / OFF / 新しいターン / ゲーム切り替え)。 */
export function stop(): void {
  generation++;
  adapter?.stop();
  // アダプタ生成前に停止が来た場合の保険 (音声が既に鳴っていることはないが、
  // 起動中の world で voiceschanged 待ちだった場合に残骸を残さない)。
  globalThis.speechSynthesis?.cancel();
}
