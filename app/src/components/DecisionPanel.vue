<script setup lang="ts">
/**
 * 決断パネル (spec 18 Phase B)。
 *
 * 開帳された失敗に対し「受け入れる / 押して振り直す / 差分を買い取る」をプレイヤーが選ぶ。
 * ここが本物の決断 — 出目は engine が握るが、**押すか退くか・いくら払うかはプレイヤーの選択**。
 * LLM は関与しない (トークン消費ゼロのプレイヤー op)。
 */
import { useGameStore, degreeLabel } from "../stores/game";
import { t } from "../i18n";

const game = useGameStore();

function buyLabel(degree: string): string {
  // additive の "success" は degree 表ではなく通常成功のラベル。
  return degree === "success" ? t("log.success") : degreeLabel(degree);
}
</script>

<template>
  <div
    v-if="game.showDecision && game.decision"
    class="rounded-lg border border-ember/70 bg-ash/30 px-4 py-3 space-y-2 decision-glow"
  >
    <p class="text-sm text-ember font-bold">
      {{ t("decision.title", { entity: game.decision.entity, stat: game.decision.stat }) }}
    </p>
    <div class="flex flex-wrap gap-2">
      <!-- 受け入れる -->
      <button
        class="rounded bg-ash/60 hover:bg-ash px-3 py-1.5 text-sm text-parchment disabled:opacity-40"
        :disabled="game.deciding"
        :title="t('decision.acceptTitle')"
        @click="game.resolveDecision('accept')"
      >
        {{ t("decision.accept") }}
      </button>
      <!-- 押して振り直す -->
      <button
        v-if="game.decision.can_push"
        class="rounded bg-ember/80 hover:bg-ember px-3 py-1.5 text-sm text-ink font-bold disabled:opacity-40"
        :disabled="game.deciding"
        :title="t('decision.pushTitle')"
        @click="game.resolveDecision('push')"
      >
        {{
          game.decision.push_cost_from
            ? t("decision.pushWithCost", {
                from: game.decision.push_cost_from,
                amount: game.decision.push_cost_amount ?? 0,
              })
            : t("decision.push")
        }}
      </button>
      <!-- 差分買い (段階ごと) -->
      <button
        v-for="b in game.decision.buys"
        :key="b.degree"
        class="rounded bg-glow/20 hover:bg-glow/30 border border-glow/50 px-3 py-1.5 text-sm text-glow disabled:opacity-40"
        :disabled="game.deciding"
        :title="t('decision.buyTitle')"
        @click="game.resolveDecision('buy', b.degree)"
      >
        {{
          t("decision.buy", {
            from: b.from,
            cost: b.cost,
            degree: buyLabel(b.degree),
            remaining: b.remaining,
          })
        }}
      </button>
    </div>
    <p class="text-xs text-parchment/40">{{ t("decision.note") }}</p>
  </div>
</template>

<style scoped>
/* 決断待ちの明滅 — 開帳カード (DiceReveal) と同系の ember の呼吸。 */
.decision-glow {
  animation: decision-breathe 2.2s ease-in-out infinite;
}
@keyframes decision-breathe {
  0%,
  100% {
    box-shadow: 0 0 4px rgb(var(--ember) / 0.2);
  }
  50% {
    box-shadow: 0 0 12px rgb(var(--ember) / 0.45);
  }
}
</style>
