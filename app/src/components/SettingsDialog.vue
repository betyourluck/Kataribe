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
import Icon from "./Icon.vue";
import { t, setLocale, locale, type Locale } from "../i18n";
import {
  DEFAULT_MSG_COLOR,
  MESSAGE_FONTS,
  useGameStore,
  loadAiProfiles,
  saveAiProfiles,
  newProfileId,
  profileMatchesConfig,
  type AiModelProfile,
} from "../stores/game";

const emit = defineEmits<{ (e: "close"): void }>();
const game = useGameStore();

type Tab = "display" | "graphics" | "sound" | "log" | "language" | "model" | "dev" | "help";
const tab = ref<Tab>("display");
// ラベルは i18n（`settings.tabs.<id>`）。id は機械用のまま。
const tabs: Tab[] = ["display", "graphics", "sound", "log", "language", "model", "dev", "help"];

// --- 開発者モード (KATARIBE_DEV_MODE) ---
const devStatus = ref("");
async function toggleDevMode(enabled: boolean) {
  devStatus.value = t("settings.status.saving");
  try {
    await game.setDevMode(enabled);
    devStatus.value = enabled ? t("settings.status.devOn") : t("settings.status.devOff");
  } catch (e) {
    devStatus.value = t("settings.status.saveFailed", { error: String(e) });
  }
}

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
// UI ロケールは i18n の共有 ref に一元化 (localStorage kataribe.lang と同期)。select は
// locale を直接 v-model し、変更で setLocale → UI が即時に切り替わる。engine 由来メッセージ
// (却下理由) は従来どおり次の new_game で反映される (lang を new_game 時に backend へ渡す経路)。
const lang = locale;
function applyLang() {
  setLocale(lang.value as Locale);
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
    llmStatus.value = t("settings.status.loadFailed", { error: String(e) });
  }
}

// --- AI モデルプロファイル (複数登録・切替。localStorage) ---
// 流れ: コンボで選ぶ → 下のフォームに即反映 (表示のみ) → 既存の「保存」で .env へ書込。
// 決定ボタンは廃止 (選択→保存の二度手間を無くし、選択=表示・保存=.env 反映に分離)。
const profiles = ref<AiModelProfile[]>([]);
const selectedProfileId = ref("");
// 新規追加フォーム (➕ で開く)。設定は下のフォーム値を使うので、ここでは表示名だけ入力する。
const showAddForm = ref(false);
const draftName = ref("");

// 現在の .env と一致するプロファイルを選択状態にする (初期表示・保存後の同期)。
function syncSelectionToConfig() {
  const hit = profiles.value.find((p) => profileMatchesConfig(p, llm.value));
  selectedProfileId.value = hit ? hit.id : "";
}

// コンボで選んだら、下のフォームへ即反映する (表示のみ・.env には書かない)。
function onSelectProfile() {
  const p = profiles.value.find((x) => x.id === selectedProfileId.value);
  if (!p) return;
  llm.value = {
    base_url: p.baseUrl,
    model: p.model,
    api_key: p.apiKey,
    use_tools: p.useTools,
  };
  llmStatus.value = t("settings.status.profileShowing", { name: p.name });
}

// [➕] 表示名の入力欄を開く。設定は下のフォームの現在値を登録する。
function openAddForm() {
  draftName.value = "";
  showAddForm.value = true;
}
function cancelAddForm() {
  showAddForm.value = false;
}
// 下のフォームの現在値 + 入力した表示名で新規プロファイルを登録し、選択状態にする。
function saveDraft() {
  const name = draftName.value.trim();
  if (!name) {
    llmStatus.value = t("settings.status.nameRequired");
    return;
  }
  const profile: AiModelProfile = {
    id: newProfileId(),
    name,
    model: llm.value.model.trim(),
    baseUrl: llm.value.base_url.trim(),
    apiKey: llm.value.api_key.trim(),
    useTools: llm.value.use_tools,
  };
  profiles.value = [...profiles.value, profile];
  saveAiProfiles(profiles.value);
  selectedProfileId.value = profile.id;
  showAddForm.value = false;
  llmStatus.value = t("settings.status.profileAdded", { name });
}

// [🗑] 選択中プロファイルを削除する (確認あり)。.env には触れない。
async function deleteProfile() {
  const p = profiles.value.find((x) => x.id === selectedProfileId.value);
  if (!p) {
    llmStatus.value = t("settings.status.selectToDelete");
    return;
  }
  if (!(await game.askConfirm(t("settings.status.confirmDelete", { name: p.name }), t("store.deleteConfirmOk")))) return;
  profiles.value = profiles.value.filter((x) => x.id !== p.id);
  saveAiProfiles(profiles.value);
  selectedProfileId.value = "";
  llmStatus.value = t("settings.status.profileDeleted", { name: p.name });
}
// --- あらすじ要約用モデル (spec 10) ---
// 実体は env (SUMMARY_LLM_*、app_data/.env)。localStorage の選択 id は UI 表示用。
// 空 = GM と同じ client を共用 (既定)。選択 = 即保存 (フォーム編集が無いので選択が決定)。
const SUMMARY_PROFILE_KEY = "kataribe.summaryProfileId";
const summaryProfileId = ref(localStorage.getItem(SUMMARY_PROFILE_KEY) || "");
const summaryStatus = ref("");
async function applySummaryProfile() {
  try {
    if (!summaryProfileId.value) {
      await invoke("set_summary_llm_config", { baseUrl: "", model: "", apiKey: "" });
      localStorage.removeItem(SUMMARY_PROFILE_KEY);
      summaryStatus.value = t("settings.status.summarySameAsGm");
      return;
    }
    const p = profiles.value.find((x) => x.id === summaryProfileId.value);
    if (!p) return;
    await invoke("set_summary_llm_config", {
      baseUrl: p.baseUrl.trim(),
      model: p.model.trim(),
      apiKey: p.apiKey.trim(),
    });
    localStorage.setItem(SUMMARY_PROFILE_KEY, summaryProfileId.value);
    summaryStatus.value = t("settings.status.summaryUsing", { name: p.name });
  } catch (e) {
    summaryStatus.value = t("settings.status.saveFailed", { error: String(e) });
  }
}

async function saveLlm() {
  llmStatus.value = t("settings.status.saving");
  try {
    await invoke("set_llm_config", {
      baseUrl: llm.value.base_url.trim(),
      model: llm.value.model.trim(),
      apiKey: llm.value.api_key.trim(),
      useTools: llm.value.use_tools,
    });
    llmStatus.value = t("settings.status.llmSaved");
    syncSelectionToConfig(); // 直接編集が登録済みと一致すればコンボの選択に反映
    game.refreshLlmModel(); // TitleBar のバッジ + ウィンドウタイトルへ即時反映
  } catch (e) {
    llmStatus.value = t("settings.status.saveFailed", { error: String(e) });
  }
}

onMounted(async () => {
  profiles.value = loadAiProfiles();
  await loadLlm(); // .env を読んでから一致プロファイルを選択状態にする
  syncSelectionToConfig();
  loadDefaultLogDir();
  game.refreshDevMode();
});
</script>

<template>
  <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/60" @click.self="emit('close')">
    <div class="w-[46rem] max-w-[94vw] h-[32rem] max-h-[88vh] flex flex-col rounded-lg border border-ash bg-ink shadow-2xl">
      <header class="flex items-center px-4 py-3 border-b border-ash">
        <h2 class="text-glow font-bold tracking-wide">{{ t("settings.title") }}</h2>
        <button class="ml-auto text-parchment/50 hover:text-parchment" :aria-label="t('settings.close')" @click="emit('close')">✕</button>
      </header>

      <div class="flex flex-1 min-h-0">
        <!-- 左ペイン: タブ (loop 変数は i18n の t() と衝突しないよう tb) -->
        <nav class="w-40 shrink-0 border-r border-ash py-2">
          <button
            v-for="tb in tabs"
            :key="tb"
            class="block w-full text-left px-4 py-2 text-sm"
            :class="tab === tb ? 'bg-ash/40 text-glow font-bold' : 'text-parchment/60 hover:text-parchment hover:bg-ash/20'"
            @click="tab = tb"
          >
            {{ t(`settings.tabs.${tb}`) }}
          </button>
        </nav>

        <!-- 右ペイン: ページ -->
        <div class="flex-1 overflow-y-auto p-5 min-w-0">
          <!-- 表示 -->
          <section v-if="tab === 'display'" class="space-y-3">
            <h3 class="text-parchment font-bold">{{ t("settings.display.heading") }}</h3>
            <label class="block text-sm text-parchment/70">
              {{ t("settings.display.fontSize") }}
              <select
                v-model.number="fontScale"
                class="mt-1 block w-40 rounded bg-ash/40 px-2 py-1 text-parchment focus:outline-none"
                @change="applyFont"
              >
                <option :value="16">{{ t("settings.display.fontSmall") }}</option>
                <option :value="18">{{ t("settings.display.fontNormal") }}</option>
                <option :value="20">{{ t("settings.display.fontLarge") }}</option>
                <option :value="24">{{ t("settings.display.fontXlarge") }}</option>
              </select>
            </label>
            <p class="text-parchment/40 text-xs">{{ t("settings.display.fontNote") }}</p>

            <hr class="border-ash/60" />
            <h3 class="text-parchment font-bold">{{ t("settings.display.bodyHeading") }}</h3>
            <label class="block text-sm text-parchment/70">
              {{ t("settings.display.font") }}
              <select
                :value="game.msgFont"
                class="mt-1 block w-56 rounded bg-ash/40 px-2 py-1 text-parchment focus:outline-none"
                @change="game.setMsgFont(($event.target as HTMLSelectElement).value)"
              >
                <option v-for="f in messageFonts" :key="f.id" :value="f.id">{{ t(`settings.display.fonts.${f.id}`) }}</option>
              </select>
            </label>
            <div class="flex items-end gap-3">
              <label class="block text-sm text-parchment/70">
                {{ t("settings.display.color") }}
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
                {{ t("settings.display.resetDefault") }}
              </button>
            </div>
            <label class="block text-sm text-parchment/70">
              {{ t("settings.display.shadow", { value: game.msgShadow }) }}
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
                {{ t("settings.display.preview") }}
              </p>
            </div>
            <p class="text-parchment/40 text-xs">
              {{ t("settings.display.bodyNote") }}
            </p>

            <hr class="border-ash/60" />
            <h3 class="text-parchment font-bold">{{ t("settings.display.beatHeading") }}</h3>
            <label class="block text-sm text-parchment/70">
              {{ t("settings.display.beatOpacity", { value: game.beatBgOpacity }) }}
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
                <p class="text-ember">{{ t("settings.display.previewBeat") }}</p>
                <p class="text-glow/70 text-sm pl-3 border-l border-ash">{{ t("settings.display.previewRecall") }}</p>
              </div>
            </div>
            <p class="text-parchment/40 text-xs">
              {{ t("settings.display.beatNote") }}
            </p>
          </section>

          <!-- グラフィック -->
          <section v-else-if="tab === 'graphics'" class="space-y-3">
            <h3 class="text-parchment font-bold">{{ t("settings.graphics.heading") }}</h3>
            <label class="block text-sm text-parchment/70">
              {{ t("settings.graphics.brightness", { value: game.bgBrightness }) }}
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
              {{ t("settings.graphics.note") }}
            </p>
            <!-- プレビュー: 現在の背景に暗幕を重ねたサンプル -->
            <div
              v-if="game.background"
              class="mt-2 h-24 w-64 rounded border border-ash"
              :style="game.backgroundStyle"
            />
            <p v-else class="text-parchment/40 text-xs">{{ t("settings.graphics.noPreview") }}</p>

            <!-- ダイスの開帳演出 (spec 18 Phase A) -->
            <label class="flex items-center gap-2 text-sm text-parchment/70 pt-2">
              <input
                type="checkbox"
                :checked="game.diceReveal"
                class="accent-ember"
                @change="game.setDiceReveal(($event.target as HTMLInputElement).checked)"
              />
              {{ t("settings.graphics.diceReveal") }}
            </label>
            <p class="text-parchment/40 text-xs">{{ t("settings.graphics.diceRevealNote") }}</p>
          </section>

          <!-- サウンド -->
          <section v-else-if="tab === 'sound'" class="space-y-3">
            <h3 class="text-parchment font-bold">{{ t("settings.sound.heading") }}</h3>
            <label class="flex items-center gap-2 text-sm text-parchment/70">
              <input
                type="checkbox"
                class="accent-ember"
                :checked="game.audioMuted"
                @change="game.setAudioMuted(($event.target as HTMLInputElement).checked)"
              />
              {{ t("settings.sound.mute") }}
            </label>
            <label class="block text-sm text-parchment/70">
              {{ t("settings.sound.volume", { value: game.audioVolume }) }}
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
              {{ t("settings.sound.note") }}
            </p>
          </section>

          <!-- ログ (会話ログのテキスト保存) -->
          <section v-else-if="tab === 'log'" class="space-y-3">
            <h3 class="text-parchment font-bold">{{ t("settings.log.heading") }}</h3>
            <p class="text-parchment/60 text-sm">
              {{ t("settings.log.introPre") }}
              <span class="text-glow">{{ t("settings.log.recordIcon") }}</span>
              {{ t("settings.log.introPost") }}
            </p>
            <label class="block text-sm text-parchment/70">
              {{ t("settings.log.folder") }}
              <input
                v-model="logDirInput"
                :placeholder="defaultLogDir || t('settings.log.folderPlaceholder')"
                class="mt-1 block w-full rounded bg-ash/40 px-2 py-1 text-parchment focus:outline-none"
                @keyup.enter="applyLogDir"
              />
            </label>
            <div class="flex items-center gap-2">
              <button
                class="rounded bg-ember/80 hover:bg-ember px-3 py-1 text-sm text-ink font-bold"
                @click="applyLogDir"
              >
                {{ t("settings.log.apply") }}
              </button>
              <button
                class="rounded bg-ash/40 hover:bg-ash/70 px-3 py-1 text-sm text-parchment/80"
                :disabled="!game.logDir"
                :class="{ 'opacity-40': !game.logDir }"
                @click="((logDirInput = ''), applyLogDir())"
              >
                {{ t("settings.log.resetDefault") }}
              </button>
              <button
                class="ml-auto rounded bg-ash/40 hover:bg-ash/70 px-3 py-1 text-sm text-parchment/80"
                @click="game.openLogFolder()"
              >
                {{ t("settings.log.openFolder") }}
              </button>
            </div>
            <p class="text-parchment/40 text-xs">
              {{ t("settings.log.note", { dir: defaultLogDir || t("settings.log.defaultDir") }) }}
            </p>
          </section>

          <!-- 言語設定 -->
          <section v-else-if="tab === 'language'" class="space-y-3">
            <h3 class="text-parchment font-bold">{{ t("settings.language") }}</h3>
            <label class="block text-sm text-parchment/70">
              {{ t("settings.displayLanguage") }}
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
              {{ t("settings.languageNote") }}
            </p>
          </section>

          <!-- AIモデル (.env 連動) -->
          <section v-else-if="tab === 'model'" class="space-y-3">
            <h3 class="text-parchment font-bold">{{ t("settings.model.heading") }}</h3>

            <!-- 登録モデル (localStorage)。選ぶと下のフォームに即反映 → 「保存」で .env へ書込。 -->
            <div class="space-y-2">
              <div class="flex items-center gap-1">
                <select v-model="selectedProfileId" @change="onSelectProfile"
                  class="flex-1 rounded bg-ash/40 px-2 py-1 text-sm text-parchment focus:outline-none">
                  <option value="" disabled>{{ t("settings.model.selectPlaceholder") }}</option>
                  <option v-for="p in profiles" :key="p.id" :value="p.id">
                    {{ p.name }}（{{ p.model || t("settings.model.modelUnset") }}）
                  </option>
                </select>
                <button
                  class="grid h-8 w-8 place-items-center rounded text-parchment/60 hover:bg-ash/60 hover:text-parchment"
                  :title="t('settings.model.addTitle')" :aria-label="t('settings.model.addAria')" @click="openAddForm">
                  <Icon name="plus" :size="16" />
                </button>
                <button
                  class="grid h-8 w-8 place-items-center rounded text-parchment/60 hover:bg-ash/60 hover:text-parchment disabled:opacity-40"
                  :disabled="!selectedProfileId" :title="t('settings.model.deleteTitle')" :aria-label="t('settings.model.deleteAria')"
                  @click="deleteProfile">
                  <Icon name="trash" :size="16" />
                </button>
              </div>

              <!-- 追加: 表示名だけ入力 (設定は下のフォームの現在値を使う)。 -->
              <div v-if="showAddForm" class="flex items-center gap-1">
                <input v-model="draftName" :placeholder="t('settings.model.draftPlaceholder')"
                  class="flex-1 rounded bg-ash/40 px-2 py-1 text-sm text-parchment focus:outline-none"
                  @keydown.enter="saveDraft" />
                <button class="rounded bg-ember/80 hover:bg-ember px-3 py-1 text-sm text-ink font-bold"
                  @click="saveDraft">{{ t("settings.model.register") }}</button>
                <button class="rounded px-2 py-1 text-sm text-parchment/60 hover:text-parchment"
                  @click="cancelAddForm">{{ t("settings.model.cancel") }}</button>
              </div>
            </div>

            <p class="text-parchment/50 text-xs">
              {{ t("settings.model.intro") }}
            </p>
            <label class="block text-sm text-parchment/70">
              {{ t("settings.model.modelName") }}
              <input v-model="llm.model" :placeholder="t('settings.model.modelPlaceholder')"
                class="mt-1 block w-full rounded bg-ash/40 px-2 py-1 text-parchment focus:outline-none" />
            </label>
            <label class="block text-sm text-parchment/70">
              {{ t("settings.model.endpoint") }}
              <input v-model="llm.base_url" :placeholder="t('settings.model.endpointPlaceholder')"
                class="mt-1 block w-full rounded bg-ash/40 px-2 py-1 text-parchment focus:outline-none" />
            </label>
            <label class="block text-sm text-parchment/70">
              {{ t("settings.model.apiKey") }}
              <input v-model="llm.api_key" type="password" :placeholder="t('settings.model.apiKeyPlaceholder')"
                class="mt-1 block w-full rounded bg-ash/40 px-2 py-1 text-parchment focus:outline-none" />
            </label>
            <label class="flex items-center gap-2 text-sm text-parchment/70">
              <input v-model="llm.use_tools" type="checkbox" class="accent-ember" />
              {{ t("settings.model.useTools") }}
            </label>
            <p class="text-parchment/40 text-xs -mt-1">
              {{ t("settings.model.useToolsNote") }}
            </p>
            <div class="flex items-center gap-3 pt-1">
              <button class="rounded bg-ember/80 hover:bg-ember px-3 py-1 text-sm text-ink font-bold" @click="saveLlm">
                {{ t("settings.model.save") }}
              </button>
              <span class="text-xs text-parchment/60">{{ llmStatus }}</span>
            </div>
            <p class="text-parchment/40 text-xs">
              {{ t("settings.model.saveNote") }}
            </p>

            <!-- あらすじ要約用モデル (spec 10)。長編の章あらすじ生成に使う。安いモデルで十分。 -->
            <div class="pt-3 border-t border-ash/60 space-y-2">
              <h4 class="text-parchment font-bold text-sm">{{ t("settings.model.summaryHeading") }}</h4>
              <select
                v-model="summaryProfileId"
                @change="applySummaryProfile"
                class="block w-full rounded bg-ash/40 px-2 py-1 text-sm text-parchment focus:outline-none"
              >
                <option value="">{{ t("settings.model.summarySameAsGm") }}</option>
                <option v-for="p in profiles" :key="p.id" :value="p.id">
                  {{ p.name }}（{{ p.model || t("settings.model.modelUnset") }}）
                </option>
              </select>
              <p class="text-parchment/40 text-xs">
                {{ t("settings.model.summaryNote") }}
              </p>
              <span v-if="summaryStatus" class="text-xs text-parchment/60">{{ summaryStatus }}</span>
            </div>
          </section>

          <!-- 開発者 -->
          <section v-else-if="tab === 'dev'" class="space-y-3">
            <h3 class="text-parchment font-bold">{{ t("settings.dev.heading") }}</h3>
            <label class="flex items-center gap-2 text-sm text-parchment/70">
              <input
                type="checkbox"
                class="accent-ember"
                :checked="game.devMode"
                @change="toggleDevMode(($event.target as HTMLInputElement).checked)"
              />
              {{ t("settings.dev.enable") }}
            </label>
            <span class="block text-xs text-ember/80 h-4">{{ devStatus }}</span>
            <p class="text-parchment/50 text-xs leading-relaxed">
              {{ t("settings.dev.descPre") }}
              <code class="text-glow">{{ t("settings.dev.descMeta") }}</code>
              {{ t("settings.dev.descPost") }}
            </p>
            <div class="rounded border border-ash/60 bg-ash/20 p-3 text-xs text-parchment/60 leading-relaxed">
              <p class="text-parchment/80 font-bold mb-1">{{ t("settings.dev.examplesTitle") }}</p>
              <p><code class="text-glow">{{ t("settings.dev.example1") }}</code></p>
              <p><code class="text-glow">{{ t("settings.dev.example2") }}</code></p>
              <p><code class="text-glow">{{ t("settings.dev.example3") }}</code></p>
              <p class="mt-1 text-parchment/40">
                {{ t("settings.dev.examplesNote") }}
              </p>
            </div>
          </section>

          <!-- ヘルプ -->
          <section v-else class="space-y-2 text-sm text-parchment/70 leading-relaxed">
            <h3 class="text-parchment font-bold">{{ t("settings.help.heading") }}</h3>
            <p>{{ t("settings.help.line1") }}</p>
            <p>{{ t("settings.help.line2Pre") }} <span class="text-glow">⚙</span> {{ t("settings.help.line2Mid") }} <span class="text-glow">☰</span> {{ t("settings.help.line2Post") }}</p>
            <p>{{ t("settings.help.line3Pre") }} <code>packages/houkago</code>{{ t("settings.help.line3Post") }}</p>
            <p>{{ t("settings.help.line4") }}</p>
            <p>{{ t("settings.help.line5") }}</p>
            <p class="text-parchment/40">{{ t("settings.help.tagline") }}</p>
          </section>
        </div>
      </div>
    </div>
  </div>
</template>
