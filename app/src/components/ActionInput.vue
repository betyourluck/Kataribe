<script setup lang="ts">
import { ref } from "vue";
import { useGameStore } from "../stores/game";

const game = useGameStore();
const text = ref("");

async function send() {
  const action = text.value;
  if (!action.trim() || game.loading) return;
  text.value = "";
  await game.playTurn(action);
}
</script>

<template>
  <div class="border-t border-ash bg-ink px-6 py-3">
    <div class="flex gap-2 items-end">
      <textarea
        v-model="text"
        rows="2"
        :disabled="!game.started || game.loading || game.cleared"
        placeholder="行動を入力（Enter で送信 / Shift+Enter で改行）"
        class="flex-1 resize-none rounded-lg bg-ash/40 px-3 py-2 text-parchment placeholder-parchment/30 focus:outline-none focus:ring-1 focus:ring-ember/50 disabled:opacity-40"
        @keydown.enter.exact.prevent="send"
      />
      <button
        :disabled="!game.started || game.loading || game.cleared || !text.trim()"
        class="rounded-lg bg-ember/80 hover:bg-ember px-4 py-2 text-ink font-bold disabled:opacity-30 disabled:cursor-not-allowed"
        @click="send"
      >
        語る
      </button>
    </div>
    <p v-if="game.error" class="text-ember/80 text-sm mt-2">エラー: {{ game.error }}</p>
  </div>
</template>
