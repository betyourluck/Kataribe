<script setup lang="ts">
import { useGameStore } from "../stores/game";

const game = useGameStore();
</script>

<template>
  <div class="flex-1 overflow-y-auto px-6 py-5 space-y-4">
    <template v-for="(entry, i) in game.log" :key="i">
      <!-- 開幕描写 -->
      <p v-if="entry.kind === 'opening'" class="text-glow/90 italic whitespace-pre-wrap leading-relaxed">
        {{ entry.text }}
      </p>

      <!-- プレイヤーの行動 -->
      <div v-else-if="entry.kind === 'player'" class="flex justify-end">
        <div class="max-w-[80%] rounded-lg bg-ash/60 px-4 py-2 text-parchment/90">
          <span class="text-ember/70 text-xs mr-2">あなた</span>{{ entry.text }}
        </div>
      </div>

      <!-- GM の語り -->
      <p v-else-if="entry.kind === 'narration'" class="whitespace-pre-wrap leading-relaxed text-parchment">
        {{ entry.text }}
      </p>

      <!-- 反応ビート + 想起された伏線 -->
      <div v-else-if="entry.kind === 'beat'" class="border-l-2 border-ember/60 pl-3 space-y-1">
        <p class="text-ember">✦ {{ entry.narration }}</p>
        <p
          v-for="(line, j) in entry.recalled"
          :key="j"
          class="text-glow/70 text-sm whitespace-pre-wrap pl-3 border-l border-ash"
        >
          {{ line }}
        </p>
      </div>

      <!-- ダイス -->
      <div v-else-if="entry.kind === 'rolls'" class="space-y-0.5">
        <p v-for="(r, j) in entry.rolls" :key="j" class="text-sm text-parchment/70">
          🎲 1d{{ r.sides }} = {{ r.result }} (DC {{ r.dc }}) →
          <span :class="r.success ? 'text-glow' : 'text-ember/60'">{{ r.success ? "成功" : "失敗" }}</span>
        </p>
      </div>

      <!-- 技能判定 (出目 + 能力修正 vs DC) -->
      <div v-else-if="entry.kind === 'checks'" class="space-y-0.5">
        <p v-for="(c, j) in entry.checks" :key="j" class="text-sm text-parchment/70">
          🎯 {{ c.entity }} の{{ c.stat }}判定: 1d{{ c.sides }}({{ c.roll }}){{ c.modifier >= 0 ? "+" + c.modifier : c.modifier }} = {{ c.total }} (DC {{ c.dc }}) →
          <span :class="c.success ? 'text-glow' : 'text-ember/60'">{{ c.success ? "成功" : "失敗" }}</span>
        </p>
      </div>

      <!-- 却下 (正本が嘘を弾いた) -->
      <div v-else-if="entry.kind === 'reject'" class="rounded-lg bg-ash/30 px-4 py-2 text-sm">
        <p class="text-ember/80">（GM は {{ entry.attempts }} 回試みたが、筋の通る一手を出せなかった）</p>
        <ul class="list-disc list-inside text-parchment/60 mt-1">
          <li v-for="(reason, j) in entry.reasons" :key="j">{{ reason }}</li>
        </ul>
        <p class="text-parchment/40 mt-1">※ 状態は変化していません。別の行動を試してください。</p>
      </div>

      <!-- システム告知 -->
      <p v-else-if="entry.kind === 'system'" class="text-center text-glow/80 text-sm">
        {{ entry.text }}
      </p>
    </template>

    <p v-if="game.loading" class="text-parchment/40 text-sm animate-pulse">GM が思案している……</p>
  </div>
</template>
