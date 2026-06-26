<script setup lang="ts">
/**
 * パッケージ一覧ダイアログ (TitleBar の List ボタンから開く)。
 * localStorage が保持するパッケージフォルダのパスを追加/削除する (配布物の置き場管理)。
 * 一覧の各行は backend list_packages が返した manifest view (title/description/playable/error)。
 */
import { ref } from "vue";
import { useGameStore } from "../stores/game";

const game = useGameStore();
const emit = defineEmits<{ (e: "close"): void }>();

const newPath = ref("");
function add() {
  game.addPackage(newPath.value);
  newPath.value = "";
}
</script>

<template>
  <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/60" @click.self="emit('close')">
    <div class="w-[40rem] max-w-[92vw] max-h-[80vh] flex flex-col rounded-lg border border-ash bg-ink shadow-2xl">
      <header class="flex items-center px-4 py-3 border-b border-ash">
        <h2 class="text-glow font-bold tracking-wide">パッケージ一覧</h2>
        <button class="ml-auto text-parchment/50 hover:text-parchment" aria-label="閉じる" @click="emit('close')">✕</button>
      </header>

      <!-- 一覧 -->
      <div class="flex-1 overflow-y-auto px-4 py-3 space-y-2">
        <p v-if="!game.packages.length" class="text-parchment/40 text-sm py-6 text-center">
          パッケージがありません。下のフォームでフォルダパスを追加してください。
        </p>
        <div
          v-for="p in game.packages"
          :key="p.path"
          class="flex items-start gap-3 rounded border border-ash/60 bg-ash/20 px-3 py-2"
        >
          <div class="min-w-0 flex-1">
            <div class="flex items-center gap-2">
              <span class="font-bold text-parchment truncate">{{ p.error ? p.path : p.title }}</span>
              <span v-if="p.error" class="shrink-0 rounded bg-red-900/60 px-1.5 text-xs text-red-200">読込失敗</span>
            </div>
            <div class="text-xs text-parchment/45 truncate">{{ p.path }}</div>
            <div v-if="p.description && !p.error" class="text-xs text-parchment/60 mt-0.5">{{ p.description }}</div>
            <div v-if="p.error" class="text-xs text-red-300/80 mt-0.5">{{ p.error }}</div>
          </div>
          <button
            class="shrink-0 text-parchment/40 hover:text-red-400 text-sm"
            title="一覧から削除"
            aria-label="削除"
            @click="game.removePackage(p.path)"
          >
            削除
          </button>
        </div>
      </div>

      <!-- 追加フォーム -->
      <footer class="flex items-center gap-2 px-4 py-3 border-t border-ash">
        <input
          v-model="newPath"
          placeholder="パッケージフォルダのパス (例: packages/houkago)"
          class="flex-1 rounded bg-ash/40 px-2 py-1 text-sm text-parchment focus:outline-none"
          @keyup.enter="add"
        />
        <button
          :disabled="!newPath.trim()"
          class="rounded bg-ember/80 hover:bg-ember px-3 py-1 text-sm text-ink font-bold disabled:opacity-40"
          @click="add"
        >
          追加
        </button>
      </footer>
    </div>
  </div>
</template>
