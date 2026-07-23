<script setup lang="ts">
/**
 * マイクの ON/OFF (spec 23 Phase D) — 会話ペインに**常設**する FAB。
 *
 * ダイアログの中に置くと、閉じた瞬間に「マイクが入っているか」が見えなくなる。
 * 音声機能でそれが一番まずい状態なので、卓に居る間はここに出しっぱなしにする
 * (ユーザーFB 2026-07-23)。読み上げ操作 (TtsControls) はホバーで浮き出る一方、
 * こちらは**常に見える** — 隠していい情報ではないため。
 *
 * ボタン自身が自分の声で脈動する。キャラの席色リングを見に行かなくても、マイクが
 * 拾えているかがその場で分かる (ひとりで検証するときの一次確認になる)。
 */
import { computed } from "vue";
import { t } from "../i18n";
import { useGameStore } from "../stores/game";
import { toggleMic } from "../table";

const game = useGameStore();
const multi = computed(() => game.multi);

/** 自分が操作している entity (卓開始前は未確定 = 脈動なし)。 */
const myEntity = computed(
  () => multi.value.assignments.find((a) => a.peerId === multi.value.myPeerId)?.entityId ?? "",
);
const level = computed(() =>
  multi.value.micOn ? (game.multi.voiceLevels[myEntity.value] ?? 0) : 0,
);
/** 自分の声で外周が太る。OFF のときは必ず 0 (OS の表示と一致させる)。 */
const pulse = computed(() => ({
  boxShadow: level.value > 0.02 ? `0 0 0 ${Math.round(level.value * 6)}px rgb(var(--ember) / 0.35)` : "none",
  transition: "box-shadow 80ms linear",
}));

async function toggle() {
  await toggleMic(!multi.value.micOn);
}
</script>

<template>
  <button
    v-if="multi.role !== 'solo'"
    type="button"
    class="absolute bottom-16 right-4 grid place-items-center h-11 w-11 rounded-full border transition-colors"
    :class="
      multi.micOn
        ? 'bg-ember/85 border-ember text-ink hover:bg-ember'
        : 'bg-ink/70 border-ash text-parchment/60 hover:text-parchment hover:border-ember'
    "
    :style="pulse"
    :title="multi.micOn ? t('table.micOn') : t('table.micOff')"
    :aria-label="multi.micOn ? t('table.micOn') : t('table.micOff')"
    :aria-pressed="multi.micOn"
    @click="toggle"
  >
    <!-- マイク (ON) / 斜線つきマイク (OFF)。線画は UI 装飾なので currentColor で描く。 -->
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" class="w-5 h-5">
      <rect x="9" y="3" width="6" height="11" rx="3" />
      <path d="M5 11a7 7 0 0 0 14 0M12 18v3" stroke-linecap="round" />
      <path v-if="!multi.micOn" d="M4 4l16 16" stroke-linecap="round" />
    </svg>
  </button>
</template>
