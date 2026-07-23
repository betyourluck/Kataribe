<script setup lang="ts">
/**
 * 卓バー (spec 23) — 卓の運用を**行動入力欄のすぐ上に常設**する。
 *
 * 卓ダイアログはモーダルなので、開いている間は行動入力に触れない。運用の操作
 * (締切・退出・マイク) がダイアログの中だけにあると、「提出する」と「締める」が
 * 同時に見えず、ホストは自分の行動を出せないまま締切を押すことになる
 * (2026-07-23 ユーザー実測 — 提出 0/2 のまま「誰も行動を提出していません」で弾かれる)。
 *
 * **卓に居る間ずっと出る** (卓開始前も)。提出まわりの表示だけが開始後に現れる —
 * ゲストが「ホストが席を整えています」の間もマイクと退出には手が届く必要がある。
 * 単騎では何も描かない。
 */
import { computed } from "vue";
import { t } from "../i18n";
import { useGameStore } from "../stores/game";
import { confirmAndLeave, hostCloseWindow, toggleMic } from "../table";

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

// --- マイク (spec 23 Phase D) ---
// 自分が操作している entity のレベルでボタンが脈動する。キャラの席色リングを見に
// 行かなくても、マイクが拾えているかがその場で分かる。OFF では必ず静止させて
// OS のマイク使用インジケータと見た目を一致させる。
const myEntity = computed(
  () => multi.value.assignments.find((a) => a.peerId === multi.value.myPeerId)?.entityId ?? "",
);
const micLevel = computed(() =>
  multi.value.micOn ? (multi.value.voiceLevels[myEntity.value] ?? 0) : 0,
);
const micPulse = computed(() => ({
  boxShadow:
    micLevel.value > 0.02
      ? `0 0 0 ${Math.round(micLevel.value * 5)}px rgb(var(--ember) / 0.35)`
      : "none",
  transition: "box-shadow 80ms linear",
}));

async function toggle() {
  await toggleMic(!multi.value.micOn);
}

/** 退出は両ロールとも確認を挟む (押し間違いの取り返しがつかない — table.ts が文言を持つ)。 */
const leave = confirmAndLeave;
</script>

<template>
  <div
    v-if="multi.role !== 'solo'"
    class="flex flex-wrap items-center gap-x-3 gap-y-1 border-t border-ash/60 bg-ash/20 px-4 py-1.5 text-xs"
  >
    <template v-if="multi.started">
      <span class="text-parchment/80">
        {{ t("table.submitted") }}: {{ submittedCount }} / {{ totalCount }}
      </span>
      <span v-if="iSubmitted" class="text-glow">{{ t("table.barSubmitted") }}</span>
      <span v-if="waitingNames" class="text-parchment/50 truncate">
        {{ t("table.barWaiting", { names: waitingNames }) }}
      </span>
      <span v-if="multi.timerRemaining !== null" class="text-ember">⏱ {{ multi.timerRemaining }}s</span>
    </template>
    <span v-else class="text-parchment/50">{{ t("table.waitingStart") }}</span>
    <!-- 切断は WebRTC の日常。黙って止まらず、取りに行っていることを見せる。 -->
    <span v-if="multi.reconnecting !== null" class="text-ember">
      {{ t("table.reconnecting", { n: multi.reconnecting }) }}
    </span>
    <span v-else-if="multi.role === 'guest' && !multi.connected" class="text-ember">
      {{ t("table.barDisconnected") }}
    </span>

    <!-- 操作は右寄せでひと塊に (卓開始前は PASS と締切が消えるので、囲って位置を保つ)。 -->
    <span class="ml-auto flex items-center gap-2">
    <!-- PASS = 「意図して何もしない」。未提出 (離席・考え中) と区別して提出済みに数えるので、
         全員が決めた時点で番が進む。待つことも手であり、遅延イベントはこれでしか進まない。 -->
    <button
      v-if="multi.started"
      class="rounded border border-ash px-2 py-0.5 text-parchment/70 hover:border-ember hover:text-parchment disabled:opacity-40"
      :disabled="game.loading || iSubmitted"
      :title="t('table.passHint')"
      @click="game.submitPartyInput('', true)"
    >
      {{ t("table.pass") }}
    </button>
    <!-- 退出 → 締切 → マイク の順で右寄せ。退出は破壊的なので締切から一番遠い側に置き、
         ホストには確認を挟む (卓を閉じると全員のセッションが終わる)。 -->
    <button
      class="rounded border border-ember/60 px-2 py-0.5 text-ember hover:bg-ember/10"
      @click="leave"
    >
      {{ multi.role === "host" ? t("table.closeTable") : t("table.leaveTable") }}
    </button>
    <button
      v-if="multi.role === 'host' && multi.started"
      class="rounded bg-ember/80 px-2.5 py-0.5 font-bold text-ink hover:bg-ember disabled:opacity-40"
      :disabled="game.loading || submittedCount === 0"
      :title="submittedCount === 0 ? t('table.barCloseDisabled') : ''"
      @click="hostCloseWindow()"
    >
      {{ t("table.closeNow") }}
    </button>
    <button
      type="button"
      class="grid place-items-center h-7 w-7 rounded-full border transition-colors"
      :class="
        multi.micOn
          ? 'bg-ember/85 border-ember text-ink hover:bg-ember'
          : 'bg-ink/60 border-ash text-parchment/60 hover:text-parchment hover:border-ember'
      "
      :style="micPulse"
      :title="multi.micOn ? t('table.micOn') : t('table.micOff')"
      :aria-label="multi.micOn ? t('table.micOn') : t('table.micOff')"
      :aria-pressed="multi.micOn"
      @click="toggle"
    >
      <!-- マイク (ON) / 斜線つきマイク (OFF)。線画は UI 装飾なので currentColor で描く。 -->
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" class="w-4 h-4">
        <rect x="9" y="3" width="6" height="11" rx="3" />
        <path d="M5 11a7 7 0 0 0 14 0M12 18v3" stroke-linecap="round" />
        <path v-if="!multi.micOn" d="M4 4l16 16" stroke-linecap="round" />
      </svg>
    </button>
    </span>
  </div>
</template>
