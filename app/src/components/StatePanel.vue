<script setup lang="ts">
import { useGameStore } from "../stores/game";

const game = useGameStore();
</script>

<template>
  <aside class="w-64 shrink-0 border-l border-ash bg-ink/60 p-4 overflow-y-auto text-sm">
    <h2 class="text-ember font-bold tracking-wide mb-3">正本の状態</h2>

    <template v-if="game.state">
      <div class="mb-3">
        <span class="text-parchment/40">ターン</span>
        <span class="ml-2 text-parchment">{{ game.state.turn }}</span>
      </div>

      <div class="mb-3">
        <div class="text-parchment/40">現在地</div>
        <div class="text-parchment">{{ game.state.location }}</div>
      </div>

      <div class="mb-3">
        <div class="text-parchment/40">所持品</div>
        <div v-if="game.state.inventory.length" class="text-parchment">
          {{ game.state.inventory.join("、") }}
        </div>
        <div v-else class="text-parchment/30">なし</div>
      </div>

      <div class="mb-3">
        <div class="text-parchment/40">立っている状態</div>
        <div v-if="game.state.flags.length" class="text-parchment">
          {{ game.state.flags.join("、") }}
        </div>
        <div v-else class="text-parchment/30">なし</div>
      </div>

      <div v-if="game.state.entities.length" class="mb-3">
        <div class="text-parchment/40 mb-1">登場人物</div>
        <div v-for="e in game.state.entities" :key="e.id" class="mb-1">
          <div class="text-ember/70">{{ e.id }}</div>
          <div v-if="e.stats.length" class="text-parchment pl-2">
            <span v-for="s in e.stats" :key="s.key" class="mr-2">{{ s.key }}={{ s.value }}</span>
          </div>
          <div v-if="e.skills.length" class="text-glow/70 pl-2 text-xs">
            能力: {{ e.skills.join("、") }}
          </div>
        </div>
      </div>

      <div
        v-if="game.state.goal_reached"
        class="mt-4 rounded bg-ember/20 border border-ember/50 px-3 py-2 text-center text-glow"
      >
        goal 到達
      </div>
    </template>

    <p v-else class="text-parchment/30">ゲーム未開始</p>
  </aside>
</template>
