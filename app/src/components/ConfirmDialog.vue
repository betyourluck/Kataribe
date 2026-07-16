<script setup lang="ts">
/**
 * 自前の確認ダイアログ (window.confirm の置き換え)。
 * WebView2 の window.confirm/alert は本文に tauri://localhost のオリジンを混ぜて表示して
 * しまうため、羊皮紙テーマに合わせた自作モーダルで置き換える。状態は store.confirmDialog が
 * 握り、OK/キャンセルで store.resolveConfirm(bool) が askConfirm() の Promise を解決する。
 *
 * - 幕クリック / Esc / キャンセル = false、OK = true。
 * - 開いたら OK ボタンへフォーカス (Enter で即決定できる)。
 */
import { ref, watch, nextTick } from "vue";
import { useGameStore } from "../stores/game";
import { t } from "../i18n";

const game = useGameStore();
const okBtn = ref<HTMLButtonElement | null>(null);

// 開いたら OK ボタンへフォーカス (キーボードだけで確定/取消できる)。
watch(
  () => game.confirmDialog,
  async (d) => {
    if (d) {
      await nextTick();
      okBtn.value?.focus();
    }
  },
);

function onKey(e: KeyboardEvent) {
  if (e.key === "Escape") {
    e.preventDefault();
    game.resolveConfirm(false);
  } else if (e.key === "Enter") {
    e.preventDefault();
    game.resolveConfirm(true);
  }
}
</script>

<template>
  <div
    v-if="game.confirmDialog"
    class="fixed inset-0 z-[70] flex items-center justify-center bg-black/60"
    @click.self="game.resolveConfirm(false)"
    @keydown="onKey"
  >
    <div class="w-[26rem] max-w-[92vw] rounded-lg border border-ash bg-ink shadow-2xl" role="alertdialog" aria-modal="true">
      <p class="px-5 pt-5 pb-4 text-sm text-parchment whitespace-pre-wrap leading-relaxed">
        {{ game.confirmDialog.message }}
      </p>
      <div class="flex justify-end gap-2 px-5 pb-4">
        <button
          class="rounded px-3 py-1.5 text-sm text-parchment/70 hover:bg-ash/60 hover:text-parchment"
          @click="game.resolveConfirm(false)"
        >
          {{ t("confirm.cancel") }}
        </button>
        <button
          ref="okBtn"
          class="rounded bg-ember/80 hover:bg-ember px-4 py-1.5 text-sm text-ink font-bold"
          @click="game.resolveConfirm(true)"
        >
          {{ game.confirmDialog.confirmLabel }}
        </button>
      </div>
    </div>
  </div>
</template>
