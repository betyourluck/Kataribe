<script setup lang="ts">
/**
 * マップパネル (spec 15) — 訪問済み+1歩先の有向グラフ。右ペインの第3タブ。
 *
 * 可視範囲=霧: 訪問済みノードと、その1歩先 (frontier) だけを描く。奥は霧 (backend が落とす)。
 * ノードは**丸**で名前を出さず、クリックで下の詳細パネルに名前・説明・画像を出す。frontier は
 * ネタバレ回避で「？」+「まだ到達していない」だけ (backend が title/description/image を伏せる)。
 * レイアウトは現在地起点の BFS 距離でランク (縦)、同ランクを横並びの簡易階層。掴んでドラッグでパン。
 *
 * engine 無改修の派生表示 — 状態の真実は backend。ここは game.map を描くだけ。
 */
import { computed, ref, onBeforeUnmount } from "vue";
// spec 23 Phase A: MapNode.image はアセット ID — store の prefetch 済みキャッシュから引く。
import { assetUrl } from "../stores/game";
import { useGameStore } from "../stores/game";
import { t } from "../i18n";

const game = useGameStore();

// 丸ノードの直径とランク間隔 (px)。
const NODE_D = 26;
const GAP_X = 30;
const GAP_Y = 46;
const PAD = 22;
const R = NODE_D / 2;

interface Placed {
  id: string;
  x: number;
  y: number;
  current: boolean;
  visited: boolean;
}
interface Link {
  x1: number;
  y1: number;
  x2: number;
  y2: number;
  mx: number;
  my: number;
  locked: boolean;
  live: boolean;
}

const layout = computed(() => {
  const nodes = game.map.nodes;
  const edges = game.map.edges;
  if (!nodes.length) return null;

  // 無向隣接で BFS 距離 (ランク) を出す。
  const adj = new Map<string, string[]>();
  nodes.forEach((n) => adj.set(n.id, []));
  edges.forEach((e) => {
    adj.get(e.from)?.push(e.to);
    adj.get(e.to)?.push(e.from);
  });
  const startId = nodes.find((n) => n.current)?.id ?? nodes[0].id;
  const dist = new Map<string, number>([[startId, 0]]);
  const queue: string[] = [startId];
  while (queue.length) {
    const cur = queue.shift() as string;
    for (const nb of adj.get(cur) ?? []) {
      if (!dist.has(nb)) {
        dist.set(nb, (dist.get(cur) as number) + 1);
        queue.push(nb);
      }
    }
  }
  let maxD = 0;
  dist.forEach((d) => (maxD = Math.max(maxD, d)));
  nodes.forEach((n) => {
    if (!dist.has(n.id)) dist.set(n.id, ++maxD);
  });

  const byRank = new Map<number, string[]>();
  nodes.forEach((n) => {
    const r = dist.get(n.id) as number;
    if (!byRank.has(r)) byRank.set(r, []);
    (byRank.get(r) as string[]).push(n.id);
  });

  const pos = new Map<string, { x: number; y: number }>();
  let maxRowW = 0;
  const ranks = [...byRank.keys()].sort((a, b) => a - b);
  ranks.forEach((r) => {
    const ids = byRank.get(r) as string[];
    const rowW = ids.length * NODE_D + (ids.length - 1) * GAP_X;
    maxRowW = Math.max(maxRowW, rowW);
    ids.forEach((id, i) => {
      pos.set(id, { x: PAD + i * (NODE_D + GAP_X), y: PAD + r * (NODE_D + GAP_Y) });
    });
  });
  const width = PAD * 2 + Math.max(NODE_D, maxRowW);
  const height = PAD * 2 + (Math.max(...ranks) + 1) * NODE_D + Math.max(...ranks) * GAP_Y;

  const placed: Placed[] = nodes.map((n) => {
    const p = pos.get(n.id) as { x: number; y: number };
    return { id: n.id, x: p.x, y: p.y, current: n.current, visited: n.visited };
  });

  // 辺は丸の中心を結び、両端を丸の縁 (半径 R) で止める (矢印分を to 側で少し余分に)。
  const links: Link[] = edges
    .filter((e) => pos.has(e.from) && pos.has(e.to))
    .map((e) => {
      const a = pos.get(e.from) as { x: number; y: number };
      const b = pos.get(e.to) as { x: number; y: number };
      const ax = a.x + R;
      const ay = a.y + R;
      const bx = b.x + R;
      const by = b.y + R;
      const len = Math.hypot(bx - ax, by - ay) || 1;
      const ux = (bx - ax) / len;
      const uy = (by - ay) / len;
      return {
        x1: ax + ux * R,
        y1: ay + uy * R,
        x2: bx - ux * (R + 4),
        y2: by - uy * (R + 4),
        mx: (ax + bx) / 2,
        my: (ay + by) / 2,
        locked: e.locked,
        live: e.from === startId && !e.locked,
      };
    });

  return { placed, links, width, height };
});

// --- 選択 (クリックで下の詳細パネルに出す) ---
const selectedId = ref<string | null>(null);
const selected = computed(() => game.map.nodes.find((n) => n.id === selectedId.value) ?? null);
const selectedImg = computed(() =>
  selected.value?.image ? assetUrl("images", selected.value.image) : null,
);

// --- 掴んでドラッグでパン (スクロールバーより直感的、ユーザーFB) ---
const scroller = ref<HTMLElement | null>(null);
const dragging = ref(false);
let drag: { x: number; y: number; left: number; top: number } | null = null;
let moved = false; // ドラッグ後の click を選択と誤認しないための閾値フラグ

function onDown(e: PointerEvent) {
  const el = scroller.value;
  if (!el) return;
  drag = { x: e.clientX, y: e.clientY, left: el.scrollLeft, top: el.scrollTop };
  moved = false;
  dragging.value = true;
  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", onUp);
}
function onMove(e: PointerEvent) {
  const el = scroller.value;
  if (!drag || !el) return;
  const dx = e.clientX - drag.x;
  const dy = e.clientY - drag.y;
  if (Math.abs(dx) > 4 || Math.abs(dy) > 4) moved = true;
  el.scrollLeft = drag.left - dx;
  el.scrollTop = drag.top - dy;
}
function onUp() {
  drag = null;
  dragging.value = false;
  window.removeEventListener("pointermove", onMove);
  window.removeEventListener("pointerup", onUp);
}
onBeforeUnmount(() => {
  window.removeEventListener("pointermove", onMove);
  window.removeEventListener("pointerup", onUp);
});

// ノードのクリック (ドラッグでなければ選択トグル)。
function onNodeClick(id: string) {
  if (moved) return;
  selectedId.value = selectedId.value === id ? null : id;
}
</script>

<template>
  <div class="h-full flex flex-col">
    <p v-if="!layout" class="text-parchment/40 text-xs py-4">{{ t("map.empty") }}</p>

    <template v-else>
      <!-- グラフ (ドラッグでパン) -->
      <div
        ref="scroller"
        class="flex-1 overflow-hidden select-none touch-none"
        :class="dragging ? 'cursor-grabbing' : 'cursor-grab'"
        @pointerdown="onDown"
      >
        <div class="relative" :style="{ width: layout.width + 'px', height: layout.height + 'px' }">
          <svg class="absolute inset-0 pointer-events-none" :width="layout.width" :height="layout.height">
            <defs>
              <marker id="mp-arrow" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse">
                <path d="M0 0 L10 5 L0 10 z" class="mp-arrow" />
              </marker>
              <marker id="mp-arrow-live" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="6.5" markerHeight="6.5" orient="auto-start-reverse">
                <path d="M0 0 L10 5 L0 10 z" class="mp-arrow-live" />
              </marker>
            </defs>
            <g v-for="(lk, i) in layout.links" :key="i">
              <line
                :x1="lk.x1" :y1="lk.y1" :x2="lk.x2" :y2="lk.y2"
                :class="lk.locked ? 'mp-edge-locked' : lk.live ? 'mp-edge-live' : 'mp-edge'"
                :marker-end="lk.locked ? undefined : lk.live ? 'url(#mp-arrow-live)' : 'url(#mp-arrow)'"
              />
              <text v-if="lk.locked" :x="lk.mx" :y="lk.my + 4" text-anchor="middle" class="mp-lock">🔒</text>
            </g>
          </svg>
          <button
            v-for="p in layout.placed"
            :key="p.id"
            class="mp-node"
            :class="{ current: p.current, frontier: !p.visited, selected: p.id === selectedId }"
            :style="{ left: p.x + 'px', top: p.y + 'px', width: NODE_D + 'px', height: NODE_D + 'px' }"
            :title="p.visited ? p.id : t('map.legendFrontier')"
            @click="onNodeClick(p.id)"
          >
            <span v-if="!p.visited">？</span>
          </button>
        </div>
      </div>

      <!-- 凡例 -->
      <div class="mt-2 pt-2 border-t border-ash/50 flex flex-wrap gap-x-3 gap-y-1 text-[10px] text-parchment/45">
        <span class="flex items-center gap-1"><span class="mp-key mp-key-current"></span>{{ t("map.legendCurrent") }}</span>
        <span class="flex items-center gap-1"><span class="mp-key mp-key-visited"></span>{{ t("map.legendVisited") }}</span>
        <span class="flex items-center gap-1"><span class="mp-key mp-key-frontier"></span>{{ t("map.legendFrontier") }}</span>
        <span class="flex items-center gap-1">🔒 {{ t("map.legendLocked") }}</span>
      </div>

      <!-- 詳細 (クリックした場所の名前・説明・画像。frontier は「？」) -->
      <div class="mt-2 pt-2 border-t border-ash/50 min-h-[2rem]">
        <template v-if="selected">
          <template v-if="selected.visited">
            <div class="text-xs font-bold text-parchment mb-1">{{ selected.title || selected.id }}</div>
            <img v-if="selectedImg" :src="selectedImg" class="w-full rounded mb-1.5 border border-ash/50" />
            <p v-if="selected.description" class="text-[11px] leading-relaxed text-parchment/70 whitespace-pre-line">
              {{ selected.description }}
            </p>
          </template>
          <template v-else>
            <div class="text-xs font-bold text-parchment/50 mb-1">？</div>
            <p class="text-[11px] text-parchment/50">{{ t("map.undiscovered") }}</p>
          </template>
        </template>
        <p v-else class="text-[10px] text-parchment/30">{{ t("map.selectHint") }}</p>
      </div>
    </template>
  </div>
</template>

<style scoped>
.mp-node {
  position: absolute;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 9999px;
  border: 1.5px solid rgb(var(--parchment) / 0.45);
  background: rgb(var(--ash) / 0.5);
  color: rgb(var(--parchment) / 0.6);
  font-size: 12px;
  line-height: 1;
  cursor: pointer;
  box-sizing: border-box;
  transition: box-shadow 0.15s, transform 0.1s;
}
.mp-node:hover {
  transform: scale(1.12);
}
.mp-node.current {
  background: rgb(var(--ember) / 0.9);
  border-color: rgb(var(--ember));
  box-shadow: 0 0 0 3px rgb(var(--ember) / 0.22);
}
.mp-node.frontier {
  border-style: dashed;
  border-color: rgb(var(--parchment) / 0.28);
  background: transparent;
  color: rgb(var(--parchment) / 0.4);
}
.mp-node.selected {
  box-shadow: 0 0 0 2px rgb(var(--glow) / 0.9);
}
.mp-edge {
  stroke: rgb(var(--parchment) / 0.3);
  stroke-width: 1.5;
}
.mp-edge-live {
  stroke: rgb(var(--ember) / 0.85);
  stroke-width: 2;
}
.mp-edge-locked {
  stroke: rgb(var(--parchment) / 0.25);
  stroke-width: 1.5;
  stroke-dasharray: 4 3;
}
.mp-arrow {
  fill: rgb(var(--parchment) / 0.3);
}
.mp-arrow-live {
  fill: rgb(var(--ember) / 0.85);
}
.mp-lock {
  font-size: 11px;
}
.mp-key {
  width: 12px;
  height: 12px;
  border-radius: 9999px;
  border: 1.5px solid rgb(var(--parchment) / 0.45);
  background: rgb(var(--ash) / 0.5);
}
.mp-key-current {
  background: rgb(var(--ember) / 0.9);
  border-color: rgb(var(--ember));
}
.mp-key-frontier {
  border-style: dashed;
  border-color: rgb(var(--parchment) / 0.28);
  background: transparent;
}
</style>
