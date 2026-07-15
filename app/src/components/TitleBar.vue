<script setup lang="ts">
/**
 * カスタムタイトルバー (SomniumTextor の TitleBar.vue 同型)。
 * Tauri の OS ネイティブ装飾 (decorations:false) を無効化し Vue 側で描画する。
 *
 * - ドラッグ移動は data-tauri-drag-region 属性で Tauri に委任 (ボタンには付けない)。
 * - 設定(Cog) / パッケージ一覧(List) ボタンは親 (App.vue) にダイアログ表示を emit する。
 * - 最小化/最大化トグル/閉じるは @tauri-apps/api/window を動的 import で叩く
 *   (ブラウザ環境=Tauri 外でも crash しない)。
 */
import { theme, toggleTheme } from "../theme";
import { t } from "../i18n";

// git の最新タグ (ビルド時に vite.config が注入)。例 "v0.3.2"。タグ無しは空 = 非表示。
const version = __APP_VERSION__;

defineProps<{
  title?: string;
  /** 使用中の AI モデル名 (パッケージ一覧アイコンの右にバッジ表示。空なら出さない)。 */
  model?: string;
  /** 配布サイトに現在版より新しいアプリがある時 true (「最新版があります」を出す)。 */
  updateAvailable?: boolean;
  /** 配布サイトの最新版タグ (hover 表示用。例 "v0.3.3")。 */
  latestVersion?: string;
}>();

/**
 * モデル名バッジのベンダー別配色 (一目でどの系統のモデルかを判別する)。
 * gemini=ブルーに白 / gpt=エメラルドグリーン / claude=くすみ系オレンジベージュ /
 * qwen=パープル / grok=濃い灰色。該当なしは既定 (ash) のまま = 空 style を返す。
 */
function badgeStyle(model: string): Record<string, string> {
  const m = model.toLowerCase();
  const paint = (bg: string, fg = "#ffffff") => ({
    backgroundColor: bg,
    color: fg,
    borderColor: "transparent",
  });
  if (m.startsWith("gemini")) return paint("#2563eb"); // ブルーに白
  if (m.includes("gpt")) return paint("#059669"); // エメラルドグリーン
  if (m.startsWith("claude")) return paint("#c1795a"); // くすみ系オレンジベージュ
  if (m.startsWith("qwen")) return paint("#7c3aed"); // パープル
  if (m.startsWith("grok")) return paint("#3f3f46", "#e4e4e7"); // 濃い灰色
  return {};
}

const emit = defineEmits<{
  (e: "open-settings"): void;
  (e: "open-packages"): void;
  (e: "open-update"): void;
  (e: "save-log"): void;
}>();

async function win(method: "minimize" | "toggleMaximize" | "close") {
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow()[method]();
  } catch (e) {
    console.warn(`[TitleBar] window.${method} unavailable:`, e);
  }
}
</script>

<template>
  <div
    data-tauri-drag-region
    class="flex items-center h-8 shrink-0 bg-ink border-b border-ash select-none"
  >
    <div data-tauri-drag-region class="px-3 text-xs font-bold tracking-widest text-glow pointer-events-none">
      {{ t("titlebar.brand") }}<span v-if="version" class="ml-1.5 text-[10px] font-normal tracking-normal text-parchment/40">{{ version }}</span><span v-if="title" class="text-parchment/40 font-normal"> — {{ title }}</span>
    </div>

    <div data-tauri-drag-region class="flex-1 h-full"></div>

    <!-- テーマ切替 (ダーク時=太陽でライトへ / ライト時=月でダークへ)。既定ダーク・localStorage 永続。 -->
    <button
      class="tb-btn"
      :title="theme === 'dark' ? t('titlebar.toLight') : t('titlebar.toDark')"
      :aria-label="t('titlebar.themeToggle')"
      @click="toggleTheme"
    >
      <!-- 太陽 (ダーク時) -->
      <svg v-if="theme === 'dark'" width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">
        <circle cx="12" cy="12" r="4" />
        <path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" />
      </svg>
      <!-- 月 (ライト時) -->
      <svg v-else width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">
        <path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z" />
      </svg>
    </button>

    <!-- アプリ操作: ログ保存 / 設定 / パッケージ一覧 -->
    <button class="tb-btn" :title="t('titlebar.saveLog')" :aria-label="t('titlebar.saveLogAria')" @click="emit('save-log')">
      <!-- Document with down-arrow (ログをファイルへ書き出す) -->
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">
        <path d="M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z" />
        <path d="M14 3v6h6" />
        <path d="M12 12v5" />
        <path d="M9.5 14.5 12 17l2.5-2.5" />
      </svg>
    </button>
    <button class="tb-btn" :title="t('titlebar.settings')" :aria-label="t('titlebar.settings')" @click="emit('open-settings')">
      <!-- Cog -->
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6">
        <circle cx="12" cy="12" r="3" />
        <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
      </svg>
    </button>
    <!-- 更新通知: 現在版より新しい配布版がある時だけ (設定とパッケージ一覧の間)。
         クリックで配布サイトを既定ブラウザで開く (自動更新はしない)。 -->
    <button
      v-if="updateAvailable"
      class="tb-update"
      :title="latestVersion ? t('titlebar.updateOpen', { version: latestVersion }) : t('titlebar.updateOpenGeneric')"
      @click="emit('open-update')"
    >
      {{ t('titlebar.updateAvailable') }}
    </button>

    <button class="tb-btn" :title="t('titlebar.packages')" :aria-label="t('titlebar.packages')" @click="emit('open-packages')">
      <!-- List -->
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round">
        <line x1="8" y1="6" x2="20" y2="6" />
        <line x1="8" y1="12" x2="20" y2="12" />
        <line x1="8" y1="18" x2="20" y2="18" />
        <circle cx="4" cy="6" r="0.9" fill="currentColor" stroke="none" />
        <circle cx="4" cy="12" r="0.9" fill="currentColor" stroke="none" />
        <circle cx="4" cy="18" r="0.9" fill="currentColor" stroke="none" />
      </svg>
    </button>

    <!-- 使用中の AI モデル名バッジ (どのモデルで遊んでいるかの常時表示。設定 → AIモデル で変更) -->
    <span
      v-if="model"
      data-tauri-drag-region
      class="mx-1 max-w-[12rem] truncate rounded-full border border-ash bg-ash/40 px-2 text-[10px] font-bold leading-4 text-parchment/70"
      :style="badgeStyle(model)"
      :title="t('titlebar.modelBadge', { model })"
    >
      {{ model }}
    </span>

    <div class="w-px h-4 mx-1 bg-ash"></div>

    <!-- ウィンドウ操作 -->
    <button class="tb-btn" :title="t('titlebar.minimize')" :aria-label="t('titlebar.minimize')" @click="win('minimize')">
      <svg width="11" height="11" viewBox="0 0 10 10"><line x1="0" y1="5" x2="10" y2="5" stroke="currentColor" stroke-width="1.2" /></svg>
    </button>
    <button class="tb-btn" :title="t('titlebar.maximize')" :aria-label="t('titlebar.maximize')" @click="win('toggleMaximize')">
      <svg width="11" height="11" viewBox="0 0 10 10"><rect x="0.6" y="0.6" width="8.8" height="8.8" fill="none" stroke="currentColor" stroke-width="1.2" /></svg>
    </button>
    <button class="tb-btn tb-close" :title="t('titlebar.close')" :aria-label="t('titlebar.close')" @click="win('close')">
      <svg width="11" height="11" viewBox="0 0 10 10">
        <line x1="0" y1="0" x2="10" y2="10" stroke="currentColor" stroke-width="1.2" />
        <line x1="10" y1="0" x2="0" y2="10" stroke="currentColor" stroke-width="1.2" />
      </svg>
    </button>
  </div>
</template>

<style scoped>
.tb-btn {
  width: 44px;
  height: 32px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: transparent;
  border: none;
  /* テーマ対応 (本文色の減光)。ライト背景でもアイコンが見える。 */
  color: rgb(var(--parchment) / 0.5);
  cursor: pointer;
  transition: background 0.15s, color 0.15s;
}
.tb-btn:hover {
  background: rgb(var(--parchment) / 0.1);
  color: rgb(var(--parchment) / 0.95);
}
.tb-close:hover {
  background: #e53935;
  color: #fff;
}
/* 「最新版があります」= ember アクセントの控えめなリンク (設定とパッケージ一覧の間)。 */
.tb-update {
  height: 20px;
  display: flex;
  align-items: center;
  margin: 0 6px;
  padding: 0 8px;
  border: 1px solid rgb(var(--ember) / 0.5);
  border-radius: 9999px;
  background: rgb(var(--ember) / 0.14);
  color: rgb(var(--ember));
  font-size: 10px;
  font-weight: 700;
  white-space: nowrap;
  cursor: pointer;
  transition: background 0.15s, color 0.15s;
}
.tb-update:hover {
  background: rgb(var(--ember) / 0.28);
}
</style>
