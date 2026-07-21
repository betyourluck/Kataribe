<script setup lang="ts">
// 約束事 (spec 20): GM が書き溜め、ユーザーも追加・編集・削除できる覚え書きリスト。
// 並びは backend がスコア降順で返す (LLM 注入と同じ見え方 = 消えかけの約束事が下に集まる
// 退場予告)。編集/追加はユーザー専権の操作で、スコアが上がり押し出されにくくなる。
import { ref, computed } from "vue";
import { useGameStore } from "../stores/game";
import { t } from "../i18n";
import Icon from "./Icon.vue";

const game = useGameStore();
const newText = ref("");
const editingId = ref<number | null>(null);
const editText = ref("");

// 約束事権限 (spec 20 Phase E)。open=追加・編集・削除 / prune=削除のみ (既定) /
// locked=タブごと非表示 (ここには来ない)。UI で隠し、backend でも拒否する二層。
const canWrite = computed(() => game.factsPolicy === "open");
const canDelete = computed(() => game.factsPolicy !== "locked");

async function add() {
  const text = newText.value.trim();
  if (!text) return;
  await game.factsAdd(text);
  newText.value = "";
}

function startEdit(id: number, text: string) {
  editingId.value = id;
  editText.value = text;
}

async function saveEdit() {
  if (editingId.value != null && editText.value.trim()) {
    await game.factsEdit(editingId.value, editText.value);
  }
  editingId.value = null;
}
</script>

<template>
  <div class="flex flex-col h-full min-h-0">
    <div class="text-parchment/40 mb-2 flex items-center gap-1.5">
      <Icon name="pencil" />{{ t("state.tabFacts") }}
      <span class="ml-auto text-[10px] text-parchment/35">{{ game.facts.length }}/20</span>
    </div>

    <p v-if="!game.facts.length" class="text-xs text-parchment/40 leading-relaxed">
      {{ t("state.factsEmpty") }}
    </p>

    <ul class="space-y-1.5 flex-1 min-h-0 overflow-y-auto pr-1">
      <li
        v-for="m in game.facts"
        :key="m.id"
        class="group rounded border border-ash/60 bg-ash/20 px-2 py-1 text-xs"
      >
        <template v-if="editingId === m.id">
          <input
            v-model="editText"
            maxlength="60"
            class="w-full rounded bg-ink/60 px-1.5 py-0.5 text-parchment focus:outline-none"
            @keyup.enter="saveEdit"
            @keyup.esc="editingId = null"
          />
          <div class="mt-1 flex gap-2 justify-end">
            <button class="text-parchment/50 hover:text-parchment" @click="editingId = null">
              {{ t("state.factsCancel") }}
            </button>
            <button class="text-ember hover:text-glow" @click="saveEdit">
              {{ t("state.factsSave") }}
            </button>
          </div>
        </template>
        <template v-else>
          <div class="flex items-start gap-2">
            <!-- 出所バッジ: user の手が触れた行は ember (押し出され保護の可視化)。 -->
            <span
              class="shrink-0 rounded px-1 text-[10px] leading-4"
              :class="m.origin === 'user' ? 'bg-ember/30 text-glow' : 'bg-ash/60 text-parchment/60'"
            >
              {{ m.origin === "user" ? t("state.factsUser") : t("state.factsGm") }}
            </span>
            <span class="flex-1 text-parchment/85 leading-snug break-words min-w-0">{{ m.text }}</span>
            <!-- 参照スコア (小さく)。低い行から退場していく。 -->
            <span class="shrink-0 text-[10px] text-parchment/35" :title="`score ${m.score} / T${m.turn}`">
              {{ m.score }}
            </span>
          </div>
          <div
            v-if="canWrite || canDelete"
            class="mt-0.5 flex gap-2 justify-end opacity-0 group-hover:opacity-70 transition-opacity"
          >
            <button
              v-if="canWrite"
              class="hover:text-glow"
              :title="t('state.factsEditTitle')"
              @click="startEdit(m.id, m.text)"
            >
              <Icon name="pencil" :size="11" />
            </button>
            <button
              v-if="canDelete"
              class="hover:text-glow"
              :title="t('state.factsDeleteTitle')"
              @click="game.factsDelete(m.id)"
            >
              <Icon name="trash" :size="11" />
            </button>
          </div>
        </template>
      </li>
    </ul>

    <!-- 追記は open 盤面だけ。制限中は「壊れている」に見えないよう理由を一行出す。 -->
    <div v-if="canWrite" class="mt-2 pt-2 border-t border-ash/60 flex gap-1.5">
      <input
        v-model="newText"
        maxlength="60"
        :placeholder="t('state.factsPlaceholder')"
        class="flex-1 min-w-0 rounded bg-ash/40 px-2 py-1 text-xs text-parchment focus:outline-none"
        @keyup.enter="add"
      />
      <button
        class="rounded bg-ember/80 hover:bg-ember px-2 py-1 text-xs text-ink font-bold disabled:opacity-40"
        :disabled="!newText.trim() || !game.started"
        @click="add"
      >
        {{ t("state.factsAdd") }}
      </button>
    </div>
    <p v-else class="mt-2 pt-2 border-t border-ash/60 text-[11px] leading-snug text-parchment/40">
      {{ t("state.factsRestricted") }}
    </p>
  </div>
</template>
