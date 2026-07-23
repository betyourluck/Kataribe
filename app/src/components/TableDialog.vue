<script setup lang="ts">
/**
 * 卓ダイアログ (spec 23 Phase C) — 多人数プレイの開設と参加。
 *
 * - ホスト: ゲーム開始済みの状態で「卓を開く」→ パッケージが中継へ預けられる →
 *   部屋コードを友人に伝える → 入室した席に entity を割り当てて「卓を開始」。
 *   以後の入力窓の運用 (自動締切 / タイマー / 手動締切) もここから。
 * - ゲスト: 表示名と部屋コードだけで「参加」— パッケージはホストから自動で届く
 *   (契約 package_relay)。中継を使わない卓のために手動選択も残してある。以後の画面は
 *   ホストから届く view で描かれ、入力は「提出」になる。
 */
import { computed, ref } from "vue";
import { t } from "../i18n";
import { useGameStore } from "../stores/game";
import {
  guestJoin,
  guestReconnect,
  hostCloseWindow,
  hostOpenTable,
  hostStartTable,
  hostStartTimer,
  hostStopTimer,
  knockUrl,
  leaveTable,
  setAutoClose,
  setKnockUrl,
  setTableName,
  tableName,
} from "../table";

const emit = defineEmits<{ (e: "close"): void }>();
const game = useGameStore();

const name = ref(tableName());
const knock = ref(knockUrl());
const roomInput = ref("");
// 既定は中継 (ホストの実ファイルが届く)。手動選択は中継を使わない卓のための逃げ道。
const manualPkg = ref(false);
const joinPackage = ref(game.packagePath || game.packagePaths[0] || "");
const busy = ref(false);
const error = ref<string | null>(null);
const autoCloseOn = ref(true);
const timerSecs = ref(90);
// 部屋コードは既定で伏せる — 配信・画面共有に映ると誰でも入れる (22 桁の強度は
// 「推測」への守りであって「映り込み」には無力)。コピーは伏せたままワンクリックで可能。
const showCode = ref(false);

const multi = computed(() => game.multi);
/** 割り当て候補 entity (主人公 + 現在の盤面に居る entity 群)。 */
const entityOptions = computed(() => {
  const ids = new Set<string>(["player"]);
  for (const e of game.state?.entities ?? []) ids.add(e.id);
  return [...ids];
});

function persistInputs() {
  setTableName(name.value);
  setKnockUrl(knock.value);
}

async function openTable() {
  busy.value = true;
  error.value = null;
  try {
    persistInputs();
    await hostOpenTable();
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = false;
  }
}

async function startTable() {
  busy.value = true;
  error.value = null;
  try {
    await hostStartTable();
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = false;
  }
}

async function join() {
  busy.value = true;
  error.value = null;
  try {
    persistInputs();
    await guestJoin(roomInput.value, manualPkg.value ? joinPackage.value : undefined);
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = false;
  }
}

async function reconnect() {
  busy.value = true;
  try {
    await guestReconnect();
  } catch (e) {
    error.value = String(e);
  } finally {
    busy.value = false;
  }
}

function toggleAutoClose() {
  autoCloseOn.value = !autoCloseOn.value;
  setAutoClose(autoCloseOn.value);
}

function copyCode() {
  void navigator.clipboard?.writeText(multi.value.roomCode);
}

/** 締切 → ダイアログを閉じる。締めた直後に見たいのは語りであってこの画面ではない
 *  (以後の運用は入力欄の上の卓バーで足りる)。 */
function closeWindowAndDismiss() {
  void hostCloseWindow();
  emit("close");
}

/** 卓を畳む。ホストが抜けると全員のセッションが終わるので、ホストにだけ確認を挟む。 */
const confirmLeave = ref(false);
function leave() {
  if (multi.value.role === "host" && !confirmLeave.value) {
    confirmLeave.value = true;
    return;
  }
  confirmLeave.value = false;
  leaveTable();
}
</script>

<template>
  <div class="fixed inset-0 z-50 grid place-items-center bg-ink/70" @click.self="$emit('close')">
    <div class="w-[34rem] max-w-[92vw] max-h-[86vh] overflow-y-auto rounded-lg border border-ash bg-ink p-5 text-parchment shadow-xl">
      <div class="mb-3 flex items-center justify-between">
        <h2 class="text-lg font-bold">{{ t("table.title") }}</h2>
        <button class="text-parchment/60 hover:text-parchment" @click="$emit('close')">✕</button>
      </div>

      <p v-if="error" class="mb-3 rounded border border-ember/60 bg-ember/10 p-2 text-sm">{{ error }}</p>

      <!-- 音声 (spec 23 Phase D) の ON/OFF は会話ペインの FAB (MicFab) が持つ。
           ダイアログに置くと閉じた瞬間に「マイクが入っているか」が見えなくなるため
           (ユーザーFB 2026-07-23)。ここでは状態を二重に持たない。 -->
      <p v-if="multi.role !== 'solo'" class="mb-3 text-xs text-parchment/60">
        {{ multi.micOn ? t("table.micOnHint") : t("table.micOffHint") }}
      </p>

      <!-- 未参加: ホスト開設 / ゲスト参加の二択 -->
      <template v-if="multi.role === 'solo'">
        <label class="mb-1 block text-sm text-parchment/70">{{ t("table.yourName") }}</label>
        <input v-model="name" class="mb-3 w-full rounded border border-ash bg-ash/30 px-2 py-1" :placeholder="t('table.namePlaceholder')" />
        <label class="mb-1 block text-sm text-parchment/70">{{ t("table.knockUrl") }}</label>
        <input v-model="knock" class="mb-4 w-full rounded border border-ash bg-ash/30 px-2 py-1 font-mono text-xs" />

        <div class="mb-4 rounded border border-ash p-3">
          <h3 class="mb-1 font-bold">{{ t("table.hostSection") }}</h3>
          <p class="mb-2 text-xs text-parchment/60">{{ t("table.hostHint") }}</p>
          <button
            class="rounded bg-ember/80 px-3 py-1 font-bold text-ink hover:bg-ember disabled:opacity-40"
            :disabled="busy || !game.started"
            @click="openTable"
          >
            {{ t("table.openTable") }}
          </button>
          <span v-if="!game.started" class="ml-2 text-xs text-parchment/50">{{ t("table.needGame") }}</span>
        </div>

        <div class="rounded border border-ash p-3">
          <h3 class="mb-1 font-bold">{{ t("table.guestSection") }}</h3>
          <label class="mb-1 block text-sm text-parchment/70">{{ t("table.roomCode") }}</label>
          <input v-model="roomInput" class="mb-2 w-full rounded border border-ash bg-ash/30 px-2 py-1 font-mono" />
          <p class="mb-2 text-xs text-parchment/60">{{ t("table.pkgAutoHint") }}</p>
          <label class="mb-2 flex items-center gap-1 text-xs text-parchment/70">
            <input v-model="manualPkg" type="checkbox" />
            {{ t("table.manualPkg") }}
          </label>
          <template v-if="manualPkg">
            <label class="mb-1 block text-sm text-parchment/70">{{ t("table.pkgForJoin") }}</label>
            <select v-model="joinPackage" class="mb-3 w-full rounded border border-ash bg-ash/30 px-2 py-1">
              <option v-for="p in game.packagePaths" :key="p" :value="p">{{ p }}</option>
            </select>
          </template>
          <button
            class="rounded bg-ember/80 px-3 py-1 font-bold text-ink hover:bg-ember disabled:opacity-40"
            :disabled="busy || !roomInput.trim() || (manualPkg && !joinPackage)"
            @click="join"
          >
            {{ t("table.join") }}
          </button>
        </div>
      </template>

      <!-- ホスト: 席と運用 -->
      <template v-else-if="multi.role === 'host'">
        <div class="mb-3 flex items-center gap-2">
          <span class="text-sm text-parchment/70">{{ t("table.roomCode") }}:</span>
          <!-- 既定は伏せ字 (配信の映り込み対策)。クリックで表示切替、コピーは伏せたまま可能。 -->
          <code
            class="cursor-pointer select-none rounded bg-ash/40 px-2 py-0.5 font-mono text-sm"
            :title="t('table.codeToggle')"
            @click="showCode = !showCode"
          >
            {{ showCode ? multi.roomCode : "••••••••••••" }}
          </code>
          <button class="rounded border border-ash px-2 py-0.5 text-xs hover:bg-ash/40" @click="copyCode">
            {{ t("table.copy") }}
          </button>
        </div>

        <!-- パッケージ中継の現況 (契約 package_relay)。失敗しても卓は成立する。 -->
        <p
          v-if="multi.relay !== 'off'"
          class="mb-3 text-xs"
          :class="multi.relay === 'failed' ? 'text-ember' : 'text-parchment/60'"
        >
          {{
            multi.relay === "uploading"
              ? t("table.relayUploading")
              : multi.relay === "ready"
                ? t("table.relayReady")
                : t("table.relayFailed")
          }}
        </p>

        <h3 class="mb-1 font-bold">{{ t("table.seats") }}</h3>
        <div v-for="s in multi.seats" :key="s.peerId" class="mb-1 flex items-center gap-2 text-sm">
          <span class="w-2 shrink-0" :class="s.connected ? 'text-green-400' : 'text-parchment/40'">●</span>
          <span class="min-w-[7rem]">{{ s.displayName }}</span>
          <select v-model="s.entityId" class="rounded border border-ash bg-ash/30 px-1 py-0.5 text-xs" :disabled="multi.started">
            <option value="">{{ t("table.unassigned") }}</option>
            <option v-for="id in entityOptions" :key="id" :value="id">{{ id }}</option>
          </select>
          <span v-if="s.packageMatch === 'mismatch'" class="text-xs text-ember">{{ t("table.pkgMismatchShort") }}</span>
          <span v-else-if="s.packageMatch === 'unknown'" class="text-xs text-parchment/50">{{ t("table.pkgUnknownShort") }}</span>
        </div>

        <button
          v-if="!multi.started"
          class="mt-3 rounded bg-ember/80 px-3 py-1 font-bold text-ink hover:bg-ember disabled:opacity-40"
          :disabled="busy || multi.seats.filter((s) => s.entityId).length < 2"
          @click="startTable"
        >
          {{ t("table.startTable") }}
        </button>

        <template v-else>
          <h3 class="mb-1 mt-3 font-bold">{{ t("table.inputWindow") }}</h3>
          <p class="text-sm text-parchment/70">
            {{ t("table.submitted") }}: {{ multi.inputStatus?.submitted.length ?? 0 }} /
            {{ (multi.inputStatus?.submitted.length ?? 0) + (multi.inputStatus?.waiting.length ?? 0) }}
            <span v-if="multi.timerRemaining !== null" class="ml-2 text-ember">⏱ {{ multi.timerRemaining }}s</span>
          </p>
          <div class="mt-2 flex flex-wrap items-center gap-2">
            <button class="rounded bg-ember/80 px-3 py-1 text-sm font-bold text-ink hover:bg-ember disabled:opacity-40" :disabled="game.loading" @click="closeWindowAndDismiss">
              {{ t("table.closeNow") }}
            </button>
            <label class="flex items-center gap-1 text-xs text-parchment/70">
              <input type="checkbox" :checked="autoCloseOn" @change="toggleAutoClose" />
              {{ t("table.autoClose") }}
            </label>
            <span class="flex items-center gap-1 text-xs">
              <input v-model.number="timerSecs" type="number" min="10" max="600" class="w-16 rounded border border-ash bg-ash/30 px-1 py-0.5" />
              <button class="rounded border border-ash px-2 py-0.5 hover:bg-ash/40" @click="hostStartTimer(timerSecs)">{{ t("table.startTimer") }}</button>
              <button v-if="multi.timerRemaining !== null" class="rounded border border-ash px-2 py-0.5 hover:bg-ash/40" @click="hostStopTimer()">{{ t("table.stopTimer") }}</button>
            </span>
          </div>
        </template>

        <!-- ホストが抜けると全員のセッションが終わる (正本はホストにしか無い) ので確認を挟む。 -->
        <p v-if="confirmLeave" class="mt-4 rounded border border-ember/60 bg-ember/10 p-2 text-xs">
          {{ t("table.closeTableConfirm") }}
        </p>
        <button
          class="mt-2 rounded border border-ember/60 px-3 py-1 text-sm text-ember hover:bg-ember/10"
          @click="leave"
        >
          {{ confirmLeave ? t("table.closeTableReally") : t("table.closeTable") }}
        </button>
      </template>

      <!-- ゲスト: 状態 -->
      <template v-else>
        <p class="mb-2 text-sm">
          {{ t("table.guestStatus", { host: multi.hostName || "?" }) }}
          <span :class="multi.connected ? 'text-green-400' : 'text-ember'">●</span>
        </p>
        <p v-if="multi.relay === 'downloading'" class="mb-2 text-sm text-parchment/60">
          {{ t("table.relayDownloading") }}
        </p>
        <p v-if="multi.packageWarning" class="mb-2 rounded border border-ember/60 bg-ember/10 p-2 text-xs">⚠ {{ multi.packageWarning }}</p>
        <p v-if="!multi.started" class="mb-2 text-sm text-parchment/60">{{ t("table.waitingStart") }}</p>
        <p v-if="multi.timerRemaining !== null" class="mb-2 text-sm text-ember">⏱ {{ multi.timerRemaining }}s</p>
        <div class="flex gap-2">
          <button v-if="!multi.connected" class="rounded bg-ember/80 px-3 py-1 text-sm font-bold text-ink hover:bg-ember" :disabled="busy" @click="reconnect">
            {{ t("table.reconnect") }}
          </button>
          <button class="rounded border border-ember/60 px-3 py-1 text-sm text-ember hover:bg-ember/10" @click="leave">
            {{ t("table.leaveTable") }}
          </button>
        </div>
      </template>
    </div>
  </div>
</template>
