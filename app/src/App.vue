<script setup lang="ts">
/**
 * App.vue — Kataribe GM プレイ画面のルート。
 *
 * レイアウト:
 * - ヘッダー: シナリオ選択 + 「新しいゲーム」
 * - 本体: 左=会話ログ / 右=正本の状態パネル
 * - フッター: 行動入力
 *
 * 状態の真実は backend (GameState) が握る。ここは command が返す view を描画するだけ。
 */
import { useGameStore, SCENARIOS } from "./stores/game";
import ConversationLog from "./components/ConversationLog.vue";
import StatePanel from "./components/StatePanel.vue";
import ActionInput from "./components/ActionInput.vue";

const game = useGameStore();
</script>

<template>
  <div class="flex flex-col h-screen w-screen overflow-hidden">
    <!-- ヘッダー -->
    <header class="flex items-center gap-3 px-6 py-3 border-b border-ash bg-ink">
      <h1 class="text-glow font-bold tracking-widest">語り部</h1>
      <span v-if="game.title" class="text-parchment/50 text-sm">— {{ game.title }}</span>
      <div class="ml-auto flex items-center gap-2">
        <select
          v-model="game.scenarioPath"
          :disabled="game.loading"
          class="rounded bg-ash/40 px-2 py-1 text-sm text-parchment focus:outline-none"
        >
          <option v-for="s in SCENARIOS" :key="s.path" :value="s.path">{{ s.label }}</option>
        </select>
        <button
          :disabled="game.loading"
          class="rounded bg-ember/80 hover:bg-ember px-3 py-1 text-sm text-ink font-bold disabled:opacity-40"
          @click="game.newGame()"
        >
          新しいゲーム
        </button>
      </div>
    </header>

    <!-- 本体 -->
    <div class="flex flex-1 overflow-hidden">
      <main class="flex-1 flex flex-col min-w-0">
        <div
          v-if="!game.started"
          class="flex-1 flex items-center justify-center text-parchment/40 px-6 text-center"
        >
          シナリオを選んで「新しいゲーム」を押すと、忘れない・矛盾しない GM が物語を始めます。
        </div>
        <ConversationLog v-else />
        <ActionInput />
      </main>

      <StatePanel />
    </div>
  </div>
</template>
