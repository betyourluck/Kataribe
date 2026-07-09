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
defineProps<{ title?: string }>();

const emit = defineEmits<{
  (e: "open-settings"): void;
  (e: "open-packages"): void;
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
      語り部<span v-if="title" class="text-parchment/40 font-normal"> — {{ title }}</span>
    </div>

    <div data-tauri-drag-region class="flex-1 h-full"></div>

    <!-- アプリ操作: ログ保存 / 設定 / パッケージ一覧 -->
    <button class="tb-btn" title="会話ログをテキスト保存" aria-label="ログ保存" @click="emit('save-log')">
      <!-- Document with down-arrow (ログをファイルへ書き出す) -->
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">
        <path d="M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z" />
        <path d="M14 3v6h6" />
        <path d="M12 12v5" />
        <path d="M9.5 14.5 12 17l2.5-2.5" />
      </svg>
    </button>
    <button class="tb-btn" title="設定" aria-label="設定" @click="emit('open-settings')">
      <!-- Cog -->
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6">
        <circle cx="12" cy="12" r="3" />
        <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
      </svg>
    </button>
    <button class="tb-btn" title="パッケージ一覧" aria-label="パッケージ一覧" @click="emit('open-packages')">
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

    <div class="w-px h-4 mx-1 bg-ash"></div>

    <!-- ウィンドウ操作 -->
    <button class="tb-btn" title="最小化" aria-label="最小化" @click="win('minimize')">
      <svg width="11" height="11" viewBox="0 0 10 10"><line x1="0" y1="5" x2="10" y2="5" stroke="currentColor" stroke-width="1.2" /></svg>
    </button>
    <button class="tb-btn" title="最大化/復帰" aria-label="最大化" @click="win('toggleMaximize')">
      <svg width="11" height="11" viewBox="0 0 10 10"><rect x="0.6" y="0.6" width="8.8" height="8.8" fill="none" stroke="currentColor" stroke-width="1.2" /></svg>
    </button>
    <button class="tb-btn tb-close" title="閉じる" aria-label="閉じる" @click="win('close')">
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
  color: #9ca3af;
  cursor: pointer;
  transition: background 0.15s, color 0.15s;
}
.tb-btn:hover {
  background: rgba(255, 255, 255, 0.07);
  color: #f3f4f6;
}
.tb-close:hover {
  background: #e53935;
  color: #fff;
}
</style>
