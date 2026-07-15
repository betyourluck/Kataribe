<script setup lang="ts">
import { ref, nextTick, computed } from "vue";
import { useGameStore } from "../stores/game";
import { t } from "../i18n";

const game = useGameStore();
const text = ref("");
const ta = ref<HTMLTextAreaElement | null>(null);

// 入力できる状態か (未開始/思案中/クリア後は不可)。
const disabled = computed(() => !game.started || game.loading || game.cleared);
// 送信できるか (中身がある & 入力可)。
const canSend = computed(() => !!text.value.trim() && !disabled.value);

// 入力に応じて高さを内容ぴったりへ (上に伸びる)。下端固定レイアウトなので増えた分は上方向へ伸びる。
const MAX_PX = 200; // これを超えたら内部スクロール。
function autoGrow() {
  const el = ta.value;
  if (!el) return;
  el.style.height = "auto";
  el.style.height = `${Math.min(el.scrollHeight, MAX_PX)}px`;
}

async function send() {
  const action = text.value;
  if (!action.trim() || disabled.value) return;
  text.value = "";
  // クリア後に高さを最小へ戻す。
  await nextTick();
  autoGrow();
  await game.playTurn(action);
}
</script>

<template>
  <div class="border-t border-ash bg-ink px-6 py-3">
    <!-- 入力欄: textarea の中に送信マーク (↵) を浮かせる (Claude Code 風) -->
    <div
      class="relative rounded-xl bg-ash/40 ring-1 ring-transparent focus-within:ring-ember/50 transition"
      :class="{ 'opacity-40': disabled }"
    >
      <textarea
        ref="ta"
        v-model="text"
        rows="1"
        :disabled="disabled"
        :placeholder="t('action.placeholder')"
        class="block w-full resize-none bg-transparent px-3.5 py-2.5 pr-12 text-parchment placeholder-parchment/30 focus:outline-none disabled:cursor-not-allowed leading-relaxed"
        style="max-height: 200px; overflow-y: auto"
        @input="autoGrow"
        @keydown.enter.exact.prevent="send"
      />
      <!-- 送信マーク: 中身があると浮き上がる。クリックかショートカット (Enter) で送信。 -->
      <button
        v-show="text.trim()"
        :disabled="!canSend"
        :aria-label="t('action.send')"
        :title="t('action.sendTitle')"
        class="absolute right-2 bottom-2 grid place-items-center h-8 w-8 rounded-lg bg-ember/85 hover:bg-ember text-ink disabled:opacity-40 disabled:cursor-not-allowed transition"
        @click="send"
      >
        <!-- ↵ リターンマーク (corner-down-left) -->
        <svg viewBox="0 0 24 24" class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="2.2"
             stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M9 10 4 15l5 5" />
          <path d="M20 4v7a4 4 0 0 1-4 4H4" />
        </svg>
      </button>
    </div>
    <p v-if="game.error" class="text-ember/80 text-sm mt-2">{{ t("action.error", { error: game.error }) }}</p>
  </div>
</template>
