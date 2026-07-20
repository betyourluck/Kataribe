<script setup lang="ts">
/**
 * 対決パネル (spec 18 Phase C)。
 *
 * attempt_contest で開いた対決を、決着まで LLM を介さずラウンド制で回す —
 * ⚔ をクリックするたび双方が振り (player の出目は伏せカードで開く)、帰結が原子適用される。
 * 何交換あっても消費トークンはゼロ。決着の digest だけが次の GM ターンに渡る。
 */
import { useGameStore } from "../stores/game";
import { t } from "../i18n";

const game = useGameStore();
</script>

<template>
  <div
    v-if="game.showContest && game.contest"
    class="rounded-lg border border-ember/70 bg-ash/30 px-4 py-3 space-y-2 contest-glow"
  >
    <p class="text-sm text-ember font-bold">
      ⚔
      {{
        t("contest.title", {
          desc: game.contest.description || game.contest.contest,
          name: game.contest.opponent_name,
        })
      }}
    </p>
    <p v-if="game.contest.rounds > 0" class="text-xs text-parchment/60">
      {{
        t("contest.tally", {
          n: game.contest.rounds,
          w: game.contest.wins,
          l: game.contest.losses,
          d: game.contest.ties,
        })
      }}
    </p>
    <div class="flex items-center gap-3">
      <button
        class="rounded bg-ember/80 hover:bg-ember px-4 py-1.5 text-sm text-ink font-bold disabled:opacity-40"
        :disabled="game.fighting || game.hasUnrevealedDice"
        :title="t('contest.roundTitle')"
        @click="game.playContestRound()"
      >
        {{ t("contest.round", { n: game.contest.rounds + 1 }) }}
      </button>
      <span class="text-xs text-parchment/40">{{ t("contest.note") }}</span>
    </div>
  </div>
</template>

<style scoped>
.contest-glow {
  animation: contest-breathe 2.2s ease-in-out infinite;
}
@keyframes contest-breathe {
  0%,
  100% {
    box-shadow: 0 0 4px rgb(var(--ember) / 0.2);
  }
  50% {
    box-shadow: 0 0 12px rgb(var(--ember) / 0.45);
  }
}
</style>
