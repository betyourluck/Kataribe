<script setup lang="ts">
// 読み上げの操作 (ON/OFF・スキップ)。会話ペインの右下に**ホバーで浮き出る** —
// 常設すると物語の邪魔になるが、喋っている最中に止めたくなるのは必ず起きるので、
// マウスを寄せれば即座に届く位置に置く。
//
// **作者が `use_tts: true` を宣言した盤面にだけ出る** (宣言のない配布物は無音のまま)。
// 表示条件は親 (App.vue) が持つ。
import { computed } from "vue";
import { useGameStore } from "../stores/game";
import { t } from "../i18n";
import Icon from "./Icon.vue";

const game = useGameStore();

const label = computed(() =>
  game.ttsEnabled ? t("tts.disable") : t("tts.enable"),
);
</script>

<template>
  <div
    class="absolute bottom-3 right-4 flex items-center gap-1 rounded-full bg-ink/70 backdrop-blur-sm
           px-1.5 py-1 shadow-lg ring-1 ring-glow/10
           opacity-0 group-hover:opacity-100 focus-within:opacity-100 transition-opacity duration-200"
  >
    <!-- ON/OFF。OFF にした瞬間に喋っているものも止める (store 側で stop)。 -->
    <button
      type="button"
      class="p-1.5 rounded-full text-glow/60 hover:text-ember hover:bg-glow/10 transition-colors"
      :class="{ 'text-ember': game.ttsEnabled }"
      :title="label"
      :aria-label="label"
      @click="game.toggleTts()"
    >
      <Icon :name="game.ttsEnabled ? 'speaker' : 'speaker-off'" :size="15" />
    </button>
    <!-- スキップ。読み上げ中だけ押せる (押しても物語は進まない = 音を切るだけ)。 -->
    <button
      type="button"
      class="p-1.5 rounded-full text-glow/60 hover:text-ember hover:bg-glow/10 transition-colors
             disabled:opacity-30 disabled:hover:text-glow/60 disabled:hover:bg-transparent"
      :disabled="!game.ttsEnabled"
      :title="t('tts.skip')"
      :aria-label="t('tts.skip')"
      @click="game.skipTts()"
    >
      <Icon name="skip" :size="15" />
    </button>
  </div>
</template>
