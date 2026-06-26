<script setup lang="ts">
import { ref, computed } from "vue";
import { useGameStore } from "../stores/game";
import Icon from "./Icon.vue";

const game = useGameStore();

// 顔アイコンをクリックして詳細を見るキャラ (presence → クリックでステータス)。
const selectedId = ref<string | null>(null);
const selectedEntity = computed(
  () => game.state?.entities.find((e) => e.id === selectedId.value) ?? null,
);
const selectedName = computed(
  () => game.presentCharacters.find((c) => c.id === selectedId.value)?.name ?? selectedId.value ?? "",
);
function initials(name: string): string {
  return name.trim().slice(0, 2);
}
</script>

<template>
  <aside class="w-64 shrink-0 border-l border-ash bg-ink/60 p-4 overflow-y-auto text-sm flex flex-col">
    <h2 class="text-ember font-bold tracking-wide mb-3">正本の状態</h2>

    <template v-if="game.state">
      <div class="mb-3 flex items-center">
        <span class="text-parchment/40 flex items-center gap-1.5"><Icon name="turn" />ターン</span>
        <span class="ml-2 text-parchment">{{ game.state.turn }}</span>
      </div>

      <div class="mb-3">
        <div class="text-parchment/40 flex items-center gap-1.5"><Icon name="location" />現在地</div>
        <div class="text-parchment">{{ game.state.location }}</div>
      </div>

      <div class="mb-3">
        <div class="text-parchment/40 flex items-center gap-1.5"><Icon name="bag" />所持品</div>
        <div v-if="game.state.inventory.length" class="text-parchment">
          {{ game.state.inventory.join("、") }}
        </div>
        <div v-else class="text-parchment/30">なし</div>
      </div>

      <div class="mb-3">
        <div class="text-parchment/40 flex items-center gap-1.5"><Icon name="flag" />フラグ</div>
        <div v-if="game.state.flags.length" class="text-parchment">
          {{ game.state.flags.join("、") }}
        </div>
        <div v-else class="text-parchment/30">なし</div>
      </div>

      <div
        v-if="game.state.goal_reached"
        class="mt-2 rounded bg-ember/20 border border-ember/50 px-3 py-2 text-center text-glow"
      >
        goal 到達
      </div>

      <!-- この場にいる人物 (主人公 + NPC) の顔アイコン行。クリックでステータス。 -->
      <!-- 居ない人物のパラメータは出さない (presence のみ可視)。 -->
      <div v-if="game.presentCharacters.length" class="mt-auto pt-4 border-t border-ash/60">
        <div class="text-parchment/40 mb-2">この場にいる</div>
        <div class="flex flex-wrap gap-3">
          <button
            v-for="c in game.presentCharacters"
            :key="c.id"
            class="flex flex-col items-center gap-1 group focus:outline-none"
            :title="c.name"
            @click="selectedId = c.id"
          >
            <!-- アイコンは CSS background で描画 (asset protocol の MIME に寛容)。無ければ initials。 -->
            <span
              class="w-12 h-12 rounded-full overflow-hidden border border-ash bg-ash/40 bg-cover bg-center flex items-center justify-center text-parchment/70 group-hover:border-ember transition-colors"
              :style="c.icon ? { backgroundImage: `url(${c.icon})` } : {}"
            >
              <span v-if="!c.icon" class="text-xs">{{ initials(c.name) }}</span>
            </span>
            <span class="text-[10px] text-parchment/60 max-w-[3.5rem] truncate">{{ c.name }}</span>
          </button>
        </div>
      </div>
    </template>

    <p v-else class="text-parchment/30">ゲーム未開始</p>

    <!-- 顔アイコンクリックで開くキャラステータス -->
    <div
      v-if="selectedEntity"
      class="fixed inset-0 z-40 flex items-center justify-center bg-black/50"
      @click.self="selectedId = null"
    >
      <div class="w-72 max-w-[90vw] rounded-lg border border-ash bg-ink p-4 shadow-2xl">
        <header class="flex items-center mb-3">
          <h3 class="text-glow font-bold">{{ selectedName }}</h3>
          <button class="ml-auto text-parchment/50 hover:text-parchment" aria-label="閉じる" @click="selectedId = null">✕</button>
        </header>
        <div v-if="selectedEntity.attributes.length" class="mb-2 text-sm">
          <span v-for="a in selectedEntity.attributes" :key="a.key" class="mr-3 text-parchment/80">
            {{ a.key }}: <span class="text-parchment">{{ a.value }}</span>
          </span>
        </div>
        <div v-if="selectedEntity.stats.length" class="mb-2 text-sm text-parchment">
          <span v-for="s in selectedEntity.stats" :key="s.key" class="mr-3">{{ s.key }}={{ s.value }}</span>
        </div>
        <div v-if="selectedEntity.skills.length" class="mb-2 text-sm text-glow/80">
          能力: {{ selectedEntity.skills.join("、") }}
        </div>
        <div v-if="selectedEntity.items.length" class="text-sm text-parchment/70">
          所持: {{ selectedEntity.items.join("、") }}
        </div>
        <p
          v-if="!selectedEntity.stats.length && !selectedEntity.attributes.length && !selectedEntity.skills.length && !selectedEntity.items.length"
          class="text-parchment/40 text-sm"
        >
          （まだ判明している情報はない）
        </p>
      </div>
    </div>
  </aside>
</template>
