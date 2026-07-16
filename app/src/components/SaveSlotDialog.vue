<script setup lang="ts">
/**
 * 手動セーブスロットのダイアログ (spec 07 Phase D)。
 * - mode="save": プレイ中の状態を 5 スロットのどれかへ保存 (上書きは confirm)。
 *   一覧はプレイ中 session のパッケージ (保存先の真実は backend session が握る)。
 * - mode="load": 選択中パッケージのスロットから再開 (プレイ中なら confirm —
 *   backend が GameSession を丸ごと差し替え、GM はロードされた記憶だけを読み直す)。
 * メタ表示 = turn + 保存日時 (locale) + 直前の語りの冒頭 (シーン識別の手がかり)。
 */
import { ref, onMounted } from "vue";
import { useGameStore } from "../stores/game";
import type { SlotView } from "../types/api";
import { t } from "../i18n";
import Icon from "./Icon.vue";

const props = defineProps<{ mode: "save" | "load" }>();
const emit = defineEmits<{ (e: "close"): void }>();
const game = useGameStore();

const slots = ref<SlotView[]>([]);
const loading = ref(true);
const busy = ref(false);

onMounted(async () => {
  try {
    slots.value = await game.listSlots(props.mode === "save");
  } catch (e) {
    game.logToast = String(e);
    emit("close");
    return;
  } finally {
    loading.value = false;
  }
});

function fmtDate(ms: number | null): string {
  return ms ? new Date(ms).toLocaleString() : "";
}

async function pick(s: SlotView) {
  if (busy.value) return;
  if (props.mode === "save") {
    if (
      s.exists &&
      !(await game.askConfirm(t("slots.overwriteConfirm", { slot: s.slot, turn: s.turn }), t("slots.overwriteOk")))
    ) {
      return;
    }
    busy.value = true;
    const saved = await game.saveToSlot(s.slot);
    busy.value = false;
    if (saved) emit("close");
  } else {
    if (!s.exists) return;
    // プレイ中のロードは進行を置き換える (前のプレイの記憶は GM から消える) ので確認する。
    if (game.started && !(await game.askConfirm(t("slots.loadConfirm"), t("slots.loadOk")))) return;
    busy.value = true;
    const ok = await game.loadSlot(s.slot);
    busy.value = false;
    if (ok) emit("close");
  }
}
</script>

<template>
  <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/60" @click.self="emit('close')">
    <div class="w-[36rem] max-w-[92vw] flex flex-col rounded-lg border border-ash bg-ink shadow-2xl">
      <header class="flex items-center gap-2 px-4 py-3 border-b border-ash">
        <Icon :name="props.mode === 'save' ? 'save' : 'load'" :size="16" class="text-ember" />
        <h2 class="text-glow font-bold tracking-wide">
          {{ props.mode === "save" ? t("slots.saveTitle") : t("slots.loadTitle") }}
        </h2>
        <button class="ml-auto text-parchment/50 hover:text-parchment" :aria-label="t('slots.close')" @click="emit('close')">✕</button>
      </header>

      <div class="px-4 py-3 space-y-2">
        <p v-if="loading" class="text-parchment/40 text-sm py-6 text-center">{{ t("slots.loading") }}</p>
        <template v-else>
          <button
            v-for="s in slots"
            :key="s.slot"
            class="w-full text-left rounded border px-3 py-2 transition-colors"
            :class="[
              s.exists
                ? 'border-ash/60 bg-ash/20 hover:border-ember/60 hover:bg-ash/40'
                : props.mode === 'save'
                  ? 'border-dashed border-ash/40 hover:border-ember/60 hover:bg-ash/20 text-parchment/50'
                  : 'border-dashed border-ash/30 text-parchment/30 cursor-default',
              busy ? 'opacity-50 pointer-events-none' : '',
            ]"
            @click="pick(s)"
          >
            <div class="flex items-center gap-2">
              <span class="font-bold" :class="s.exists ? 'text-ember' : ''">{{ t("slots.slot", { n: s.slot }) }}</span>
              <template v-if="s.exists">
                <span class="text-xs rounded bg-ash/70 px-1.5 text-parchment/80">turn {{ s.turn }}</span>
                <span class="ml-auto text-xs text-parchment/45">{{ fmtDate(s.saved_at_ms) }}</span>
              </template>
              <span v-else class="text-sm">{{ t("slots.empty") }}</span>
            </div>
            <div v-if="s.snippet" class="mt-1 text-xs text-parchment/60 truncate">{{ s.snippet }}</div>
          </button>
        </template>
      </div>

      <footer class="px-4 pb-3 text-xs text-parchment/40">
        {{ props.mode === "save" ? t("slots.saveNote") : t("slots.loadNote") }}
      </footer>
    </div>
  </div>
</template>
