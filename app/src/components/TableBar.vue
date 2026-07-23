<script setup lang="ts">
/**
 * 卓バー (spec 23) — 入力窓の現況を**行動入力欄のすぐ上に常設**する。
 *
 * 卓ダイアログはモーダルなので、開いている間は行動入力に触れない。締切ボタンだけが
 * ダイアログの中にあると「提出する」と「締める」が同時に見えず、ホストは自分の行動を
 * 出せないまま締切を押すことになる (2026-07-23 ユーザー実測 — 提出 0/2 のまま
 * 「誰も行動を提出していません」で弾かれる)。運用の状態は常に見えている必要がある。
 *
 * 卓が始まっている間だけ出る。単騎では何も描かない。
 */
import { computed } from "vue";
import { t } from "../i18n";
import { useGameStore } from "../stores/game";
import { hostCloseWindow } from "../table";

const game = useGameStore();
const multi = computed(() => game.multi);

/** peer_id → 表示名 (卓開始時に確定した割り当てから引く)。 */
const nameOf = (peerId: string) =>
  multi.value.assignments.find((a) => a.peerId === peerId)?.displayName ?? peerId;

const submittedCount = computed(() => multi.value.inputStatus?.submitted.length ?? 0);
const totalCount = computed(
  () =>
    (multi.value.inputStatus?.submitted.length ?? 0) +
    (multi.value.inputStatus?.waiting.length ?? 0),
);
/** 自分は提出済みか (再提出は上書きなので「出し直せる」ことも示す)。 */
const iSubmitted = computed(
  () => multi.value.inputStatus?.submitted.includes(multi.value.myPeerId) ?? false,
);
/** まだ出していない人の名前 (AFK の透明性 — 誰を待っているかを隠さない)。 */
const waitingNames = computed(() =>
  (multi.value.inputStatus?.waiting ?? []).map(nameOf).join("、"),
);
</script>

<template>
  <div
    v-if="multi.started"
    class="flex flex-wrap items-center gap-x-3 gap-y-1 border-t border-ash/60 bg-ash/20 px-4 py-1.5 text-xs"
  >
    <span class="text-parchment/80">
      {{ t("table.submitted") }}: {{ submittedCount }} / {{ totalCount }}
    </span>
    <span v-if="iSubmitted" class="text-glow">{{ t("table.barSubmitted") }}</span>
    <span v-if="waitingNames" class="text-parchment/50 truncate">
      {{ t("table.barWaiting", { names: waitingNames }) }}
    </span>
    <span v-if="multi.timerRemaining !== null" class="text-ember">⏱ {{ multi.timerRemaining }}s</span>
    <!-- 締切はホストの専権 (決定 4)。ゲストには出さない。 -->
    <button
      v-if="multi.role === 'host'"
      class="ml-auto rounded bg-ember/80 px-2.5 py-0.5 font-bold text-ink hover:bg-ember disabled:opacity-40"
      :disabled="game.loading || submittedCount === 0"
      :title="submittedCount === 0 ? t('table.barCloseDisabled') : ''"
      @click="hostCloseWindow()"
    >
      {{ t("table.closeNow") }}
    </button>
  </div>
</template>
