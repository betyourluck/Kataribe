<script setup lang="ts">
import { ref, computed, watch, onMounted, onBeforeUnmount } from "vue";
import { useGameStore } from "../stores/game";
import Icon from "./Icon.vue";

const game = useGameStore();

// 右ペインは縦タブ 2 枚 (progress=進行: ターン/目標/この場 ・ world=状態: 現在地/所持品/フラグ)。
// 1 枚に全部積むと全体スクロールになるのでタブで切り替える。
const activeTab = ref<"progress" | "world">("progress");

// 顔アイコンをクリックして詳細を見るキャラ (presence → クリックでプロフィール)。
const selectedId = ref<string | null>(null);
const selectedEntity = computed(
  () => game.state?.entities.find((e) => e.id === selectedId.value) ?? null,
);
const selectedName = computed(
  () => game.presentCharacters.find((c) => c.id === selectedId.value)?.name ?? selectedId.value ?? "",
);
// ダイアログヘッダの顔アイコン (presence 行と同じ解決済み URL を使い回す)。
const selectedIcon = computed(
  () => game.presentCharacters.find((c) => c.id === selectedId.value)?.icon ?? null,
);
const selectedIsEmpty = computed(() => {
  const e = selectedEntity.value;
  return (
    !!e &&
    !e.stats.length &&
    !e.attributes.length &&
    !e.skills.length &&
    !e.items.length &&
    !e.profile
  );
});
// profile 本文はもう 1 ステップ奥 (ダイアログ内のアイコンクリックで開く)。キャラ切替でリセット。
const showProfile = ref(false);
watch(selectedId, () => {
  showProfile.value = false;
});
function initials(name: string): string {
  return name.trim().slice(0, 2);
}
function onKeydown(e: KeyboardEvent) {
  if (e.key === "Escape") selectedId.value = null;
  // IME 変換中はショートカットを発火させない (変換候補操作のキーを奪わない)。
  if (e.isComposing) return;
  if (!e.ctrlKey || e.altKey || e.metaKey) return;
  if (e.key === "Tab") {
    // Ctrl+Tab: 進行⇄状態のトグル (2 枚なので往復が最速。Shift 併用も同じトグル)。
    e.preventDefault();
    activeTab.value = activeTab.value === "progress" ? "world" : "progress";
  } else if (e.key === "1" || e.key === "2") {
    // Ctrl+1/2: 直接選択 (タブが増えた時の拡張枠と同じ慣習)。
    e.preventDefault();
    activeTab.value = e.key === "1" ? "progress" : "world";
  }
}
onMounted(() => window.addEventListener("keydown", onKeydown));
onBeforeUnmount(() => window.removeEventListener("keydown", onKeydown));
</script>

<template>
  <aside class="w-64 shrink-0 border-l border-ash bg-ink/60 text-sm flex">
    <!-- 縦タブ rail: 全体スクロールを避けるため 2 枚に分ける (進行 / 状態)。 -->
    <!-- rail は背景・罫線なし (透明)、タブは普段半透明で控えめに。 -->
    <nav class="w-7 shrink-0 flex flex-col items-stretch pt-2 gap-0.5">
      <button
        class="flex flex-col items-center gap-1 py-2 border-l-2 transition-opacity focus:outline-none"
        :class="
          activeTab === 'progress'
            ? 'border-ember text-glow'
            : 'border-transparent text-parchment opacity-40 hover:opacity-90'
        "
        title="進行 (ターン・目標・この場にいる) — Ctrl+1 / Ctrl+Tab で切替"
        @click="activeTab = 'progress'"
      >
        <Icon name="target" :size="12" />
        <span class="text-[9px] tracking-widest" style="writing-mode: vertical-rl">進行</span>
      </button>
      <button
        class="flex flex-col items-center gap-1 py-2 border-l-2 transition-opacity focus:outline-none"
        :class="
          activeTab === 'world'
            ? 'border-ember text-glow'
            : 'border-transparent text-parchment opacity-40 hover:opacity-90'
        "
        title="状態 (現在地・所持品・フラグ) — Ctrl+2 / Ctrl+Tab で切替"
        @click="activeTab = 'world'"
      >
        <Icon name="location" :size="12" />
        <span class="text-[9px] tracking-widest" style="writing-mode: vertical-rl">状態</span>
      </button>
    </nav>

    <div class="flex-1 min-w-0 p-4 overflow-y-auto flex flex-col">
      <template v-if="game.state">
        <!-- 1枚め「進行」: ターン / 目標 / この場にいる -->
        <template v-if="activeTab === 'progress'">
          <div class="mb-3 flex items-center">
            <span class="text-parchment/40 flex items-center gap-1.5"><Icon name="turn" />ターン</span>
            <span class="ml-2 text-parchment">{{ game.state.turn }}</span>
          </div>

          <!-- 目標 (named goal) の一覧: 「何を目指せる盤面か」をプレイヤーに示す。 -->
          <!-- when/narration はネタバレゆえ出さず、hint (作者が意図的に開示する道しるべ) を添える。 -->
          <!-- 増えたら領域内で独立スクロール。バーは常時表示 (overflow-y-scroll) で
               ガター幅を確保し、出現/消滅による横のカクつきを防ぐ。 -->
          <div v-if="game.state.goals.length" class="mb-3 flex-1 min-h-0 flex flex-col">
            <div class="text-parchment/40 mb-2 flex items-center gap-1.5"><Icon name="target" />目標</div>
            <ul class="goal-list space-y-1.5 flex-1 min-h-0 overflow-y-scroll pr-1">
              <li
                v-for="g in game.state.goals"
                :key="g.id"
                class="rounded border px-2 py-1 text-xs"
                :class="
                  g.id === game.state.reached_goal
                    ? 'border-ember/60 bg-ember/15 text-glow'
                    : 'border-ash/60 bg-ash/20 text-parchment/70'
                "
              >
                <div class="flex items-center gap-2">
                  <span
                    class="w-1.5 h-1.5 rounded-full shrink-0"
                    :class="g.id === game.state.reached_goal ? 'bg-glow' : 'bg-parchment/30'"
                  ></span>
                  <span class="truncate">{{ g.title || g.id }}</span>
                  <span v-if="g.id === game.state.reached_goal" class="ml-auto shrink-0">✓ 到達</span>
                </div>
                <p v-if="g.hint" class="mt-0.5 pl-3.5 text-[11px] leading-snug text-parchment/50">
                  {{ g.hint }}
                </p>
              </li>
            </ul>
          </div>

          <div
            v-if="game.state.goal_reached"
            class="rounded bg-ember/20 border border-ember/50 px-3 py-2 text-center text-glow"
          >
            goal 到達
          </div>

          <!-- この場にいる人物 (主人公 + NPC) の顔アイコン行。クリックでプロフィール。 -->
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

        <!-- 2枚め「状態」: 現在地 / 所持品 / フラグ -->
        <template v-else>
          <div class="mb-3">
            <div class="text-parchment/40 flex items-center gap-1.5"><Icon name="location" />現在地</div>
            <!-- 表示は authored title を優先、無ければ id (機械用セレクタ) へフォールバック。hover で id。 -->
            <div class="text-parchment" :title="game.state.location">
              {{ game.state.location_title || game.state.location }}
            </div>
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
            <!-- 表示名 (title || key) のチップ。hover で「いつ・何をして立ったか」(chronicle join) を出す。 -->
            <div v-if="game.state.flags.length" class="flex flex-wrap gap-1.5 mt-1">
              <span
                v-for="f in game.state.flags"
                :key="f.key"
                class="px-2 py-0.5 rounded bg-ash/40 border border-ash text-xs text-parchment/80"
                :title="f.cause ? `T${f.turn}: ${f.cause}` : f.turn ? `T${f.turn} に成立` : ''"
              >
                {{ f.title || f.key }}
              </span>
            </div>
            <div v-else class="text-parchment/30">なし</div>
          </div>
        </template>
      </template>

      <p v-else class="text-parchment/30">ゲーム未開始</p>
    </div>

    <!-- 顔アイコンクリックで開くプロフィールカード -->
    <Transition name="profile">
      <div
        v-if="selectedEntity"
        class="fixed inset-0 z-40 flex items-center justify-center bg-black/60 backdrop-blur-[2px]"
        @click.self="selectedId = null"
      >
        <div
          class="profile-card w-[30rem] max-w-[92vw] max-h-[80vh] overflow-y-auto rounded-xl border border-ash bg-gradient-to-b from-ash/50 via-ink to-ink shadow-2xl"
        >
          <!-- ヘッダ: 顔アイコン (クリックで profile 本文を開閉) + 名前 + 属性チップ -->
          <header class="relative flex items-center gap-3 p-4 pb-3 border-b border-ash/60">
            <button
              class="relative w-16 h-16 rounded-full shrink-0 focus:outline-none transition-shadow"
              :class="
                selectedEntity.profile
                  ? 'cursor-pointer ring-2 ring-ember/50 ring-offset-2 ring-offset-ink hover:ring-glow'
                  : 'cursor-default ring-2 ring-ash ring-offset-2 ring-offset-ink'
              "
              :title="selectedEntity.profile ? 'プロフィールを見る' : ''"
              :aria-expanded="showProfile"
              @click="selectedEntity.profile && (showProfile = !showProfile)"
            >
              <span
                class="w-full h-full rounded-full bg-ash/40 bg-cover bg-center flex items-center justify-center text-parchment/70"
                :style="selectedIcon ? { backgroundImage: `url(${selectedIcon})` } : {}"
              >
                <span v-if="!selectedIcon" class="text-lg">{{ initials(selectedName) }}</span>
              </span>
              <!-- profile がある印: 右下の小さなバッジ -->
              <span
                v-if="selectedEntity.profile"
                class="absolute -bottom-1 -right-1 w-5 h-5 rounded-full bg-ink border border-ember/60 flex items-center justify-center text-ember text-[10px] leading-none"
                aria-hidden="true"
              >
                {{ showProfile ? "−" : "…" }}
              </span>
            </button>
            <div class="min-w-0">
              <h3 class="text-glow font-bold text-lg leading-tight truncate">{{ selectedName }}</h3>
              <div v-if="selectedEntity.attributes.length" class="mt-1.5 flex flex-wrap gap-1">
                <span
                  v-for="a in selectedEntity.attributes"
                  :key="a.key"
                  class="px-2 py-0.5 rounded-full bg-ember/15 border border-ember/40 text-[11px] leading-4"
                  :title="a.key"
                >
                  <span class="text-parchment/50">{{ a.key }}</span>
                  <span class="text-glow ml-1">{{ a.value }}</span>
                </span>
              </div>
            </div>
            <button
              class="absolute top-2 right-2 w-7 h-7 rounded-full flex items-center justify-center text-parchment/50 hover:text-parchment hover:bg-ash/60 transition-colors"
              aria-label="閉じる"
              @click="selectedId = null"
            >
              ✕
            </button>
          </header>

          <!-- プロフィール本文 (authored の語り素材)。初期は畳み、顔アイコンクリックで開く。 -->
          <Transition name="reveal">
            <p
              v-if="showProfile && selectedEntity.profile"
              class="mx-4 mt-3 pl-3 border-l-2 border-ember/40 text-[13px] leading-relaxed text-parchment/75 whitespace-pre-line"
            >
              {{ selectedEntity.profile }}
            </p>
          </Transition>

          <div class="p-4 pt-3 space-y-4">
            <!-- ステータス: 3列グリッド -->
            <section v-if="selectedEntity.stats.length">
              <h4 class="flex items-center gap-1.5 text-parchment/40 text-xs tracking-wider mb-2">
                <Icon name="gauge" />ステータス
              </h4>
              <div class="grid grid-cols-3 gap-x-5 gap-y-1.5">
                <div
                  v-for="s in selectedEntity.stats"
                  :key="s.key"
                  class="flex items-baseline justify-between border-b border-ash/40 pb-0.5"
                >
                  <span class="text-parchment/60 text-xs truncate mr-2">{{ s.key }}</span>
                  <span class="text-glow font-semibold tabular-nums">{{ s.value }}</span>
                </div>
              </div>
            </section>

            <!-- 能力: チップ -->
            <section v-if="selectedEntity.skills.length">
              <h4 class="flex items-center gap-1.5 text-parchment/40 text-xs tracking-wider mb-2">
                <Icon name="sparkle" />能力
              </h4>
              <div class="flex flex-wrap gap-1.5">
                <span
                  v-for="sk in selectedEntity.skills"
                  :key="sk"
                  class="px-2 py-0.5 rounded bg-glow/10 border border-glow/30 text-xs text-glow"
                >
                  {{ sk }}
                </span>
              </div>
            </section>

            <!-- 所持: チップ -->
            <section v-if="selectedEntity.items.length">
              <h4 class="flex items-center gap-1.5 text-parchment/40 text-xs tracking-wider mb-2">
                <Icon name="bag" />所持
              </h4>
              <div class="flex flex-wrap gap-1.5">
                <span
                  v-for="it in selectedEntity.items"
                  :key="it"
                  class="px-2 py-0.5 rounded bg-ash/40 border border-ash text-xs text-parchment/80"
                >
                  {{ it }}
                </span>
              </div>
            </section>

            <p v-if="selectedIsEmpty" class="text-parchment/40 text-sm">
              （まだ判明している情報はない）
            </p>
          </div>
        </div>
      </div>
    </Transition>
  </aside>
</template>

<style scoped>
/* プロフィールカードの入退場: 幕はフェード、カードは軽く浮き上がる */
.profile-enter-active,
.profile-leave-active {
  transition: opacity 0.18s ease;
}
.profile-enter-from,
.profile-leave-to {
  opacity: 0;
}
.profile-enter-active .profile-card,
.profile-leave-active .profile-card {
  transition: transform 0.18s ease;
}
.profile-enter-from .profile-card,
.profile-leave-to .profile-card {
  transform: scale(0.96) translateY(8px);
}

/* 目標一覧の常時表示スクロールバー: 細身・ash でテーマに馴染ませる */
.goal-list::-webkit-scrollbar {
  width: 6px;
}
.goal-list::-webkit-scrollbar-track {
  background: transparent;
}
.goal-list::-webkit-scrollbar-thumb {
  background: rgba(58, 50, 43, 0.9); /* ash */
  border-radius: 3px;
}
.goal-list::-webkit-scrollbar-thumb:hover {
  background: rgba(217, 138, 74, 0.5); /* ember */
}

/* profile 本文の開閉: ふわっと開く */
.reveal-enter-active,
.reveal-leave-active {
  transition:
    opacity 0.16s ease,
    transform 0.16s ease;
}
.reveal-enter-from,
.reveal-leave-to {
  opacity: 0;
  transform: translateY(-4px);
}
</style>
