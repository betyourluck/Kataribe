<script setup lang="ts">
import { useGameStore } from "../stores/game";

const game = useGameStore();
</script>

<template>
  <!-- 本文フォントは container で inherit (空なら UI 既定のまま)。色+影は語り系要素にだけ当てる。 -->
  <div class="flex-1 overflow-y-auto px-6 py-5 space-y-4" :style="{ fontFamily: game.messageFontFamily }">
    <template v-for="(entry, i) in game.log" :key="i">
      <!-- 開幕描写 -->
      <p
        v-if="entry.kind === 'opening'"
        class="text-glow/90 italic whitespace-pre-wrap leading-relaxed"
        :style="game.narrationStyle"
      >
        {{ entry.text }}
      </p>

      <!-- プレイヤーの行動 -->
      <div v-else-if="entry.kind === 'player'" class="flex justify-end">
        <div class="max-w-[80%] rounded-lg bg-ash/60 px-4 py-2 text-parchment/90">
          <span class="text-ember/70 text-xs mr-2">あなた</span>{{ entry.text }}
        </div>
      </div>

      <!-- GM の語り -->
      <p
        v-else-if="entry.kind === 'narration'"
        class="whitespace-pre-wrap leading-relaxed text-parchment"
        :style="game.narrationStyle"
      >
        {{ entry.text }}
      </p>

      <!-- 反応ビート + 想起された伏線 (黒の透過背景 = 表示設定で濃さ調整、可読性の手当て) -->
      <div
        v-else-if="entry.kind === 'beat'"
        class="border-l-2 border-ember/60 pl-3 space-y-1 rounded-r py-1.5 pr-3"
        :style="game.beatBgStyle"
      >
        <p v-if="entry.narration.trim()" class="text-ember" :style="{ textShadow: game.narrationStyle.textShadow ?? '' }">✦ {{ entry.narration }}</p>
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
      <div v-else-if="entry.kind === 'checks'" class="space-y-1">
        <template v-for="(c, j) in entry.checks" :key="j">
          <p class="text-sm text-parchment/70">
            🎯 {{ c.entity }} の{{ c.stat }}判定: 1d{{ c.sides }}({{ c.roll }}){{ c.modifier >= 0 ? "+" + c.modifier : c.modifier }} = {{ c.total }} (DC {{ c.dc }}) →
            <span :class="c.success ? 'text-glow' : 'text-ember/60'">{{ c.success ? "成功" : "失敗" }}</span>
          </p>
          <!-- authored 結末ナレーション (毎回・同ターン)。失敗を必ず描く。 -->
          <p v-if="c.narration" class="text-parchment/90 whitespace-pre-wrap" :style="game.narrationStyle">{{ c.narration }}</p>
        </template>
      </div>

      <!-- 却下 (正本が嘘を弾いた) -->
      <div v-else-if="entry.kind === 'reject'" class="rounded-lg bg-ash/30 px-4 py-2 text-sm">
        <p class="text-ember/80">（GM は {{ entry.attempts }} 回試みたが、筋の通る一手を出せなかった）</p>
        <ul class="list-disc list-inside text-parchment/60 mt-1">
          <li v-for="(reason, j) in entry.reasons" :key="j">{{ reason }}</li>
        </ul>
        <p class="text-parchment/40 mt-1">※ 状態は変化していません。別の行動を試してください。</p>
      </div>

      <!-- 受理までに却下された試行の理由 (なぜ筋を通すのに N 回かかったか) -->
      <div v-else-if="entry.kind === 'retries'" class="rounded-lg bg-ash/20 px-4 py-2 text-xs text-parchment/55">
        <p class="text-parchment/45">却下された試行:</p>
        <ul class="list-disc list-inside mt-0.5">
          <li v-for="(reasons, j) in entry.reasons" :key="j">
            {{ j + 1 }} 回目: {{ reasons.join(" / ") }}
          </li>
        </ul>
      </div>

      <!-- システム告知 -->
      <p v-else-if="entry.kind === 'system'" class="text-center text-glow/80 text-sm">
        {{ entry.text }}
      </p>
    </template>

    <p v-if="game.loading" class="text-parchment/40 text-sm animate-pulse">GM が思案している……</p>
  </div>
</template>
