<script setup lang="ts">
/**
 * 設定ダイアログ (TitleBar の Cog ボタンから開く)。左ペインにタブ:
 * - 表示: UI フォントサイズ (localStorage、即時適用)
 * - グラフィック: 背景画像の明るさ (暗幕の濃さ、localStorage、即時適用)
 * - 言語設定: 却下理由などの表示言語 ja/en (localStorage、次の新しいゲームから)
 * - AIモデル: .env の LLM 設定 (base_url/model/api_key) を編集 → backend が env 即時反映 + .env 永続化
 * - ヘルプ: 操作の手引き
 */
import { computed, ref, onMounted } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { DEFAULT_MSG_COLOR, MESSAGE_FONTS, useGameStore } from "../stores/game";

const emit = defineEmits<{ (e: "close"): void }>();
const game = useGameStore();

type Tab = "display" | "graphics" | "sound" | "log" | "language" | "model" | "help";
const tab = ref<Tab>("display");
const tabs: { id: Tab; label: string }[] = [
  { id: "display", label: "表示" },
  { id: "graphics", label: "グラフィック" },
  { id: "sound", label: "サウンド" },
  { id: "log", label: "ログ" },
  { id: "language", label: "言語設定" },
  { id: "model", label: "AIモデル" },
  { id: "help", label: "ヘルプ" },
];

// --- ログ (保存先フォルダ) ---
const logDirInput = ref(game.logDir);
const defaultLogDir = ref("");
async function loadDefaultLogDir() {
  try {
    defaultLogDir.value = await invoke<string>("get_default_log_dir");
  } catch {
    /* 取得できなくても placeholder が空になるだけ */
  }
}
function applyLogDir() {
  game.setLogDir(logDirInput.value);
  logDirInput.value = game.logDir; // 正規化 (trim) を反映
}

// --- 表示 (フォント) ---
const FONT_KEY = "kataribe.fontScale";
const fontScale = ref<number>(Number(localStorage.getItem(FONT_KEY)) || 18);
function applyFont() {
  document.documentElement.style.fontSize = `${fontScale.value}px`;
  localStorage.setItem(FONT_KEY, String(fontScale.value));
}

// --- 本文テキスト (フォント/色/影 — store が localStorage 永続を担う) ---
const messageFonts = MESSAGE_FONTS;
// カラーピッカーは常に具体値が要る (空 = テーマ既定 parchment を表示)。
const msgColorValue = computed(() => game.msgColor || DEFAULT_MSG_COLOR);
// プレビュー: 本文フォント + 色/影を実際の見た目で確認する。
const previewStyle = computed(() => ({
  fontFamily: game.messageFontFamily,
  ...game.narrationStyle,
}));

// --- 言語設定 ---
const LANG_KEY = "kataribe.lang";
const lang = ref<string>(localStorage.getItem(LANG_KEY) || "ja");
function applyLang() {
  localStorage.setItem(LANG_KEY, lang.value);
}

// --- AIモデル (.env 連動) ---
interface LlmConfigView {
  base_url: string;
  model: string;
  api_key: string;
  use_tools: boolean;
}
const llm = ref<LlmConfigView>({ base_url: "", model: "", api_key: "", use_tools: true });
const llmStatus = ref("");
async function loadLlm() {
  try {
    llm.value = await invoke<LlmConfigView>("get_llm_config");
  } catch (e) {
    llmStatus.value = `読込失敗: ${e}`;
  }
}
async function saveLlm() {
  llmStatus.value = "保存中…";
  try {
    await invoke("set_llm_config", {
      baseUrl: llm.value.base_url.trim(),
      model: llm.value.model.trim(),
      apiKey: llm.value.api_key.trim(),
      useTools: llm.value.use_tools,
    });
    llmStatus.value = "保存しました（.env に永続化／次の『新しいゲーム』から有効）";
    game.refreshLlmModel(); // TitleBar のバッジ + ウィンドウタイトルへ即時反映
  } catch (e) {
    llmStatus.value = `保存失敗: ${e}`;
  }
}

onMounted(() => {
  loadLlm();
  loadDefaultLogDir();
});
</script>

<template>
  <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/60" @click.self="emit('close')">
    <div class="w-[46rem] max-w-[94vw] h-[32rem] max-h-[88vh] flex flex-col rounded-lg border border-ash bg-ink shadow-2xl">
      <header class="flex items-center px-4 py-3 border-b border-ash">
        <h2 class="text-glow font-bold tracking-wide">設定</h2>
        <button class="ml-auto text-parchment/50 hover:text-parchment" aria-label="閉じる" @click="emit('close')">✕</button>
      </header>

      <div class="flex flex-1 min-h-0">
        <!-- 左ペイン: タブ -->
        <nav class="w-40 shrink-0 border-r border-ash py-2">
          <button
            v-for="t in tabs"
            :key="t.id"
            class="block w-full text-left px-4 py-2 text-sm"
            :class="tab === t.id ? 'bg-ash/40 text-glow font-bold' : 'text-parchment/60 hover:text-parchment hover:bg-ash/20'"
            @click="tab = t.id"
          >
            {{ t.label }}
          </button>
        </nav>

        <!-- 右ペイン: ページ -->
        <div class="flex-1 overflow-y-auto p-5 min-w-0">
          <!-- 表示 -->
          <section v-if="tab === 'display'" class="space-y-3">
            <h3 class="text-parchment font-bold">表示</h3>
            <label class="block text-sm text-parchment/70">
              フォントサイズ
              <select
                v-model.number="fontScale"
                class="mt-1 block w-40 rounded bg-ash/40 px-2 py-1 text-parchment focus:outline-none"
                @change="applyFont"
              >
                <option :value="16">小 (16px)</option>
                <option :value="18">標準 (18px)</option>
                <option :value="20">大 (20px)</option>
                <option :value="24">特大 (24px)</option>
              </select>
            </label>
            <p class="text-parchment/40 text-xs">UI 全体の基準フォントサイズを変えます（即時適用・localStorage に保存）。</p>

            <hr class="border-ash/60" />
            <h3 class="text-parchment font-bold">本文テキスト（GM の語り）</h3>
            <label class="block text-sm text-parchment/70">
              フォント
              <select
                :value="game.msgFont"
                class="mt-1 block w-56 rounded bg-ash/40 px-2 py-1 text-parchment focus:outline-none"
                @change="game.setMsgFont(($event.target as HTMLSelectElement).value)"
              >
                <option v-for="f in messageFonts" :key="f.id" :value="f.id">{{ f.label }}</option>
              </select>
            </label>
            <div class="flex items-end gap-3">
              <label class="block text-sm text-parchment/70">
                文字色
                <input
                  type="color"
                  :value="msgColorValue"
                  class="mt-1 block h-8 w-16 cursor-pointer rounded bg-ash/40 p-0.5"
                  @input="game.setMsgColor(($event.target as HTMLInputElement).value)"
                />
              </label>
              <button
                class="rounded bg-ash/40 hover:bg-ash/70 px-2 py-1 text-xs text-parchment/70"
                :disabled="!game.msgColor"
                :class="{ 'opacity-40': !game.msgColor }"
                @click="game.setMsgColor('')"
              >
                既定に戻す
              </button>
            </div>
            <label class="block text-sm text-parchment/70">
              文字の影（{{ game.msgShadow }}）
              <input
                type="range"
                min="0"
                max="100"
                step="5"
                :value="game.msgShadow"
                class="mt-2 block w-64 accent-ember"
                @input="game.setMsgShadow(+($event.target as HTMLInputElement).value)"
              />
            </label>
            <!-- プレビュー: 現在の背景 (あれば) の上に本文サンプルを敷いて実際の見え方を確認 -->
            <div class="mt-1 w-full max-w-md rounded border border-ash px-4 py-3" :style="game.backgroundStyle">
              <p class="whitespace-pre-wrap leading-relaxed text-parchment" :style="previewStyle">
                霧が窓の外を這う。囲炉裏の火が爆ぜて、誰かが息を呑んだ。—— 本文はこの見た目で表示されます。
              </p>
            </div>
            <p class="text-parchment/40 text-xs">
              会話ログの語りの文章に適用されます（即時適用・localStorage に保存）。影は背景画像の上での読みやすさに効きます。
            </p>

            <hr class="border-ash/60" />
            <h3 class="text-parchment font-bold">筋書き・伏線（✦ / ┊）</h3>
            <label class="block text-sm text-parchment/70">
              背景の濃さ（{{ game.beatBgOpacity }}）
              <input
                type="range"
                min="0"
                max="100"
                step="5"
                :value="game.beatBgOpacity"
                class="mt-2 block w-64 accent-ember"
                @input="game.setBeatBgOpacity(+($event.target as HTMLInputElement).value)"
              />
            </label>
            <!-- プレビュー: 現在の背景の上にビート/想起ブロックを敷いて実際の見え方を確認 -->
            <div class="mt-1 w-full max-w-md rounded border border-ash px-4 py-3" :style="game.backgroundStyle">
              <div class="border-l-2 border-ember/60 pl-3 space-y-1 rounded-r py-1.5 pr-3" :style="game.beatBgStyle">
                <p class="text-ember">✦ 祭壇の奥で、何かが目を覚ました。</p>
                <p class="text-glow/70 text-sm pl-3 border-l border-ash">丘の樫の木の下で、二人は約束を交わした。</p>
              </div>
            </div>
            <p class="text-parchment/40 text-xs">
              発火イベント（✦）と想起された記憶（┊）の下に敷く黒の透過背景です。0 でなし、右に動かすほど濃く＝読みやすくなります。本文の語りには敷きません。
            </p>
          </section>

          <!-- グラフィック -->
          <section v-else-if="tab === 'graphics'" class="space-y-3">
            <h3 class="text-parchment font-bold">グラフィック</h3>
            <label class="block text-sm text-parchment/70">
              背景の明るさ（{{ game.bgBrightness }}）
              <input
                type="range"
                min="0"
                max="100"
                step="5"
                :value="game.bgBrightness"
                class="mt-2 block w-64 accent-ember"
                @input="game.setBgBrightness(+($event.target as HTMLInputElement).value)"
              />
            </label>
            <p class="text-parchment/40 text-xs">
              背景画像にかける暗幕の濃さを調整します（右に動かすほど画像が明るく、左ほど暗く＝文字が読みやすく）。即時適用・localStorage に保存。
            </p>
            <!-- プレビュー: 現在の背景に暗幕を重ねたサンプル -->
            <div
              v-if="game.background"
              class="mt-2 h-24 w-64 rounded border border-ash"
              :style="game.backgroundStyle"
            />
            <p v-else class="text-parchment/40 text-xs">（ゲーム開始後、背景のあるパッケージでプレビューが出ます）</p>
          </section>

          <!-- サウンド -->
          <section v-else-if="tab === 'sound'" class="space-y-3">
            <h3 class="text-parchment font-bold">サウンド</h3>
            <label class="flex items-center gap-2 text-sm text-parchment/70">
              <input
                type="checkbox"
                class="accent-ember"
                :checked="game.audioMuted"
                @change="game.setAudioMuted(($event.target as HTMLInputElement).checked)"
              />
              ミュート（BGM・効果音を鳴らさない）
            </label>
            <label class="block text-sm text-parchment/70">
              音量（{{ game.audioVolume }}）
              <input
                type="range"
                min="0"
                max="100"
                step="5"
                :value="game.audioVolume"
                :disabled="game.audioMuted"
                class="mt-2 block w-64 accent-ember disabled:opacity-40"
                @input="game.setAudioVolume(+($event.target as HTMLInputElement).value)"
              />
            </label>
            <p class="text-parchment/40 text-xs">
              場所のループ BGM と発火時の効果音に共通でかかります（即時適用・localStorage に保存）。音の出るアセットを同梱したパッケージで有効です。
            </p>
          </section>

          <!-- ログ (会話ログのテキスト保存) -->
          <section v-else-if="tab === 'log'" class="space-y-3">
            <h3 class="text-parchment font-bold">ログ</h3>
            <p class="text-parchment/60 text-sm">
              タイトルバーの
              <span class="text-glow">記録アイコン</span>
              を押すと、現在の会話ログを「日時_パッケージ名.txt」で保存します。
            </p>
            <label class="block text-sm text-parchment/70">
              保存先フォルダ
              <input
                v-model="logDirInput"
                :placeholder="defaultLogDir || '(既定のアプリデータ内 logs フォルダ)'"
                class="mt-1 block w-full rounded bg-ash/40 px-2 py-1 text-parchment focus:outline-none"
                @keyup.enter="applyLogDir"
              />
            </label>
            <div class="flex items-center gap-2">
              <button
                class="rounded bg-ember/80 hover:bg-ember px-3 py-1 text-sm text-ink font-bold"
                @click="applyLogDir"
              >
                適用
              </button>
              <button
                class="rounded bg-ash/40 hover:bg-ash/70 px-3 py-1 text-sm text-parchment/80"
                :disabled="!game.logDir"
                :class="{ 'opacity-40': !game.logDir }"
                @click="((logDirInput = ''), applyLogDir())"
              >
                既定に戻す
              </button>
              <button
                class="ml-auto rounded bg-ash/40 hover:bg-ash/70 px-3 py-1 text-sm text-parchment/80"
                @click="game.openLogFolder()"
              >
                フォルダを開く
              </button>
            </div>
            <p class="text-parchment/40 text-xs">
              空欄なら既定の場所（{{ defaultLogDir || "アプリデータ内の logs" }}）へ保存します。「フォルダを開く」でエクスプローラー等が開きます（フォルダが無ければ作成）。
            </p>
          </section>

          <!-- 言語設定 -->
          <section v-else-if="tab === 'language'" class="space-y-3">
            <h3 class="text-parchment font-bold">言語設定</h3>
            <label class="block text-sm text-parchment/70">
              表示言語
              <select
                v-model="lang"
                class="mt-1 block w-40 rounded bg-ash/40 px-2 py-1 text-parchment focus:outline-none"
                @change="applyLang"
              >
                <option value="ja">日本語</option>
                <option value="en">English</option>
              </select>
            </label>
            <p class="text-parchment/40 text-xs">
              却下理由などエンジン由来メッセージの言語。次の「新しいゲーム」から有効です（UI 文言の i18n は今後）。
            </p>
          </section>

          <!-- AIモデル (.env 連動) -->
          <section v-else-if="tab === 'model'" class="space-y-3">
            <h3 class="text-parchment font-bold">AIモデル</h3>
            <label class="block text-sm text-parchment/70">
              モデル名 (LLM_MODEL)
              <input v-model="llm.model" placeholder="claude-opus-4-8 / gpt-4o-mini 等"
                class="mt-1 block w-full rounded bg-ash/40 px-2 py-1 text-parchment focus:outline-none" />
            </label>
            <label class="block text-sm text-parchment/70">
              エンドポイント (LLM_BASE_URL)
              <input v-model="llm.base_url" placeholder="https://api.anthropic.com/v1"
                class="mt-1 block w-full rounded bg-ash/40 px-2 py-1 text-parchment focus:outline-none" />
            </label>
            <label class="block text-sm text-parchment/70">
              API キー (LLM_API_KEY)
              <input v-model="llm.api_key" type="password" placeholder="sk-... / さくらは UUID:シークレット"
                class="mt-1 block w-full rounded bg-ash/40 px-2 py-1 text-parchment focus:outline-none" />
            </label>
            <label class="flex items-center gap-2 text-sm text-parchment/70">
              <input v-model="llm.use_tools" type="checkbox" class="accent-ember" />
              ツール呼び出し (function calling) を使う
            </label>
            <p class="text-parchment/40 text-xs -mt-1">
              OpenAI / Anthropic は ON。さくら AI Engine やローカル OpenAI 互換サーバなど tool_choice 非対応は OFF
              （プロンプトで JSON 出力を指示する経路に切替）。
            </p>
            <div class="flex items-center gap-3 pt-1">
              <button class="rounded bg-ember/80 hover:bg-ember px-3 py-1 text-sm text-ink font-bold" @click="saveLlm">
                保存
              </button>
              <span class="text-xs text-parchment/60">{{ llmStatus }}</span>
            </div>
            <p class="text-parchment/40 text-xs">
              .env を書き換えます（プロセスへ即時反映＋ファイル永続化）。次の「新しいゲーム」から新モデルで接続します。
            </p>
          </section>

          <!-- ヘルプ -->
          <section v-else class="space-y-2 text-sm text-parchment/70 leading-relaxed">
            <h3 class="text-parchment font-bold">ヘルプ</h3>
            <p>・上部のパッケージを選び「新しいゲーム」で開始。下の入力欄に行動を打ち、Enter で送信します。</p>
            <p>・タイトルバーの <span class="text-glow">⚙</span> が設定、<span class="text-glow">☰</span> がパッケージ一覧です。</p>
            <p>・パッケージ一覧では、配布フォルダのパスを追加/削除できます（例: <code>packages/houkago</code>）。</p>
            <p>・AIモデルタブで接続先・モデル・API キーを切り替えられます（.env を書き換え）。</p>
            <p>・タイトルバーの記録アイコンで会話ログをテキスト保存できます（保存先は「ログ」タブで指定）。</p>
            <p class="text-parchment/40">語り部 — クラウド LLM をナレーター、決定論エンジンを正本とした、忘れない・矛盾しない GM。</p>
          </section>
        </div>
      </div>
    </div>
  </div>
</template>
