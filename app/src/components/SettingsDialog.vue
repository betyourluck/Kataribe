<script setup lang="ts">
/**
 * 設定ダイアログ (TitleBar の Cog ボタンから開く)。左ペインに4タブ:
 * - 表示: UI フォントサイズ (localStorage、即時適用)
 * - 言語設定: 却下理由などの表示言語 ja/en (localStorage、次の新しいゲームから)
 * - AIモデル: .env の LLM 設定 (base_url/model/api_key) を編集 → backend が env 即時反映 + .env 永続化
 * - ヘルプ: 操作の手引き
 */
import { ref, onMounted } from "vue";
import { invoke } from "@tauri-apps/api/core";

const emit = defineEmits<{ (e: "close"): void }>();

type Tab = "display" | "language" | "model" | "help";
const tab = ref<Tab>("display");
const tabs: { id: Tab; label: string }[] = [
  { id: "display", label: "表示" },
  { id: "language", label: "言語設定" },
  { id: "model", label: "AIモデル" },
  { id: "help", label: "ヘルプ" },
];

// --- 表示 (フォント) ---
const FONT_KEY = "kataribe.fontScale";
const fontScale = ref<number>(Number(localStorage.getItem(FONT_KEY)) || 16);
function applyFont() {
  document.documentElement.style.fontSize = `${fontScale.value}px`;
  localStorage.setItem(FONT_KEY, String(fontScale.value));
}

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
  } catch (e) {
    llmStatus.value = `保存失敗: ${e}`;
  }
}

onMounted(loadLlm);
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
                <option :value="14">小 (14px)</option>
                <option :value="16">標準 (16px)</option>
                <option :value="18">大 (18px)</option>
                <option :value="20">特大 (20px)</option>
              </select>
            </label>
            <p class="text-parchment/40 text-xs">UI 全体の基準フォントサイズを変えます（即時適用・localStorage に保存）。</p>
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
            <p class="text-parchment/40">語り部 — クラウド LLM をナレーター、決定論エンジンを正本とした、忘れない・矛盾しない GM。</p>
          </section>
        </div>
      </div>
    </div>
  </div>
</template>
