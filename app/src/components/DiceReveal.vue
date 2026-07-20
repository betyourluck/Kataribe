<script setup lang="ts">
/**
 * ダイスの開帳カード (spec 18 Phase A)。
 *
 * 出目は engine が確定済み (seeded RNG) — このカードは**開帳**の演出であって決定ではない。
 * クリック → 数字スクランブル (減速しながらパラパラ変わる) → 確定出目に着地 → `revealed` を
 * emit し、親 (ConversationLog) が store.revealNext で本物の行に差し替える。
 * 着地の数字と本物の行の数字は同じなので、開帳から結果表示への連続性が保たれる。
 */
import { onBeforeUnmount, ref } from "vue";
import { t } from "../i18n";

const props = defineProps<{
  /** 何の判定か (結果を含めない)。例「探索者 の目星判定」「1d20 (DC 12)」 */
  label: string;
  /** 確定した出目 (スクランブルの着地点)。 */
  final: number;
  /** スクランブル中の乱数の上限 (d20 なら 20)。 */
  max: number;
}>();
const emit = defineEmits<{ (e: "revealed"): void }>();

const state = ref<"idle" | "rolling">("idle");
const display = ref("?");
let timer: number | undefined;

function start() {
  if (state.value !== "idle") return;
  state.value = "rolling";
  const t0 = performance.now();
  const DURATION = 1100; // スクランブル時間 (減速込み)
  const tick = () => {
    const p = (performance.now() - t0) / DURATION;
    if (p >= 1) {
      // 着地: 確定出目を一拍見せてから本物の行に差し替える。
      display.value = String(props.final);
      timer = window.setTimeout(() => emit("revealed"), 400);
      return;
    }
    display.value = String(1 + Math.floor(Math.random() * Math.max(1, props.max)));
    // power2.out 風の減速: 進行に応じて間隔を 40ms → 200ms へ伸ばす。
    timer = window.setTimeout(tick, 40 + 160 * p * p);
  };
  tick();
}

onBeforeUnmount(() => window.clearTimeout(timer));
</script>

<template>
  <button
    type="button"
    class="dice-card flex items-center gap-3 rounded-lg border px-4 py-2 text-left w-full transition"
    :class="
      state === 'idle'
        ? 'border-ember/60 bg-ash/30 hover:bg-ash/50 cursor-pointer idle-glow'
        : 'border-ember bg-ash/40 cursor-default'
    "
    :title="state === 'idle' ? t('log.diceOpenTitle') : ''"
    @click="start"
  >
    <span class="text-sm text-parchment/70">🎲 {{ label }}</span>
    <span
      class="ml-auto grid place-items-center min-w-[3.5rem] h-9 rounded bg-ink/60 border border-ember/50 text-xl font-bold tabular-nums"
      :class="state === 'rolling' ? 'text-ember' : 'text-parchment/50'"
    >
      {{ display }}
    </span>
    <span v-if="state === 'idle'" class="text-xs text-ember/80 shrink-0">{{ t("log.diceOpen") }}</span>
  </button>
</template>

<style scoped>
/* 未開帳の誘い: ember の呼吸 (結果は既に確定している — 光っているのは器だけ)。 */
.idle-glow {
  animation: dice-breathe 1.8s ease-in-out infinite;
}
@keyframes dice-breathe {
  0%,
  100% {
    box-shadow: 0 0 4px rgb(var(--ember) / 0.25);
  }
  50% {
    box-shadow: 0 0 14px rgb(var(--ember) / 0.55);
  }
}
</style>
