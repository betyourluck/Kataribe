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
import { ref, onMounted } from "vue";
import { useGameStore } from "./stores/game";
import ConversationLog from "./components/ConversationLog.vue";
import StatePanel from "./components/StatePanel.vue";
import ActionInput from "./components/ActionInput.vue";

const game = useGameStore();

// パッケージフォルダ追加用の入力 (localStorage の一覧に積む)。
const newPath = ref("");
function addPackage() {
  game.addPackage(newPath.value);
  newPath.value = "";
}

// 起動時に localStorage のパス一覧から manifest を読み、一覧を描く。
onMounted(() => game.refreshPackages());
</script>

<template>
  <div class="flex flex-col h-screen w-screen overflow-hidden">
    <!-- ヘッダー -->
    <header class="flex items-center gap-3 px-6 py-3 border-b border-ash bg-ink">
      <h1 class="text-glow font-bold tracking-widest">語り部</h1>
      <span v-if="game.title" class="text-parchment/50 text-sm">— {{ game.title }}</span>
      <div class="ml-auto flex items-center gap-2">
        <select
          v-model="game.packagePath"
          :disabled="game.loading"
          class="rounded bg-ash/40 px-2 py-1 text-sm text-parchment focus:outline-none"
        >
          <option
            v-for="p in game.packages"
            :key="p.path"
            :value="p.path"
            :disabled="!p.playable || !!p.error"
          >
            {{ p.error ? `⚠ ${p.path}` : p.title }}{{ !p.playable && !p.error ? "（campaign 後続）" : "" }}
          </option>
        </select>
        <input
          v-model="newPath"
          placeholder="パッケージフォルダのパスを追加"
          :disabled="game.loading"
          class="w-56 rounded bg-ash/40 px-2 py-1 text-sm text-parchment focus:outline-none"
          @keyup.enter="addPackage"
        />
        <button
          :disabled="game.loading || !newPath.trim()"
          class="rounded bg-ash/60 hover:bg-ash px-2 py-1 text-sm text-parchment disabled:opacity-40"
          @click="addPackage"
        >
          ＋追加
        </button>
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
          パッケージを選んで「新しいゲーム」を押すと、忘れない・矛盾しない GM が物語を始めます。
        </div>
        <ConversationLog v-else />
        <ActionInput />
      </main>

      <StatePanel />
    </div>
  </div>
</template>
