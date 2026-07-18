<script setup lang="ts">
/**
 * App.vue — Kataribe GM プレイ画面のルート。
 *
 * レイアウト:
 * - カスタム TitleBar (decorations:false。Cog=設定 / List=パッケージ一覧 / ウィンドウ操作)
 * - ヘッダー: 再生するパッケージ選択 + 「新しいゲーム」
 * - 本体: 左=会話ログ / 右=正本の状態パネル / フッター=行動入力
 *
 * 状態の真実は backend (GameState) が握る。ここは command が返す view を描画するだけ。
 * パッケージパスの追加/削除は PackageDialog、その他設定は SettingsDialog に分離。
 */
import { computed, ref, watch, onMounted } from "vue";
import { listen } from "@tauri-apps/api/event";
import { useGameStore } from "./stores/game";
import { t } from "./i18n";
import TitleBar from "./components/TitleBar.vue";
import PackageDialog from "./components/PackageDialog.vue";
import SettingsDialog from "./components/SettingsDialog.vue";
import SaveSlotDialog from "./components/SaveSlotDialog.vue";
import ConfirmDialog from "./components/ConfirmDialog.vue";
import ConversationLog from "./components/ConversationLog.vue";
import StatePanel from "./components/StatePanel.vue";
import ActionInput from "./components/ActionInput.vue";
import Icon from "./components/Icon.vue";

const game = useGameStore();
const showSettings = ref(false);
const showPackages = ref(false);
// 手動セーブスロットのダイアログ (spec 07 Phase D)。null = 非表示。
const slotDialog = ref<"save" | "load" | null>(null);

// ログ保存/フォルダ操作の一時トースト。store.logToast をこの ref が数秒だけ映して消す。
const toast = ref("");
let toastTimer: ReturnType<typeof setTimeout> | undefined;
watch(
  () => game.logToast,
  (msg) => {
    if (!msg) return;
    toast.value = msg;
    game.logToast = ""; // 消費 (同じメッセージの再表示も拾えるように即クリア)
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (toast.value = ""), 4000);
  },
);

// 右ペインの幅ドラッグ。パネルは画面右端に接するので 幅 = 画面幅 − ポインタ X。
// pointermove/up を window に張り、capture で外れても追従させる (ドラッグ中の即時反映)。
function startPanelResize(e: PointerEvent) {
  e.preventDefault();
  const onMove = (ev: PointerEvent) => game.setPanelWidth(window.innerWidth - ev.clientX);
  const onUp = () => {
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("pointerup", onUp);
    document.body.style.userSelect = "";
    document.body.style.cursor = "";
  };
  document.body.style.userSelect = "none"; // ドラッグ中のテキスト選択を抑止
  document.body.style.cursor = "col-resize";
  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", onUp);
}

// 選択中パッケージのオートセーブ (spec 07 Phase C)。在れば「続きから (turn N)」を提示する。
const selectedAutosaveTurn = computed(
  () => game.packages.find((p) => p.path === game.packagePath)?.autosave_turn ?? null,
);

// コンボリストの選択がプレイ中のゲームと食い違うとき、セーブは無効化する。
// (セーブは「今プレイ中のゲーム」に対する操作。選択だけ別へ変えた状態で押すと、
//  保存先=プレイ中のゲーム / ロード一覧=選択中のゲーム で食い違い混乱する。)
const saveMismatch = computed(
  () => game.started && game.packagePath !== game.activePackagePath,
);

// コンボリストは「最後に追加(ダウンロード)したものを上」に並べる (ダウンロードしたばかりを
// すぐ選べる方が使いやすい、ユーザーFB)。packagePaths は追加順ゆえ逆順が新しい順。
// reverse は元配列を壊すのでコピーに対して行う。
const packagesNewestFirst = computed(() => [...game.packages].reverse());

// ループ BGM の <audio> 要素。store.bgm (場所の BGM) を src に流し、音量はミュート/音量設定に追従。
// 状態の真実は store が握り、ここは再生デバイスとして従う (CG/SE と分業: SE は one-shot で store が鳴らす)。
const bgmEl = ref<HTMLAudioElement | null>(null);

// BGM を (再)生成する。停止中で src が設定済みなら play() を試みる。
// **autoplay 制約対策**: 起動直後 (new_game の await 跨ぎ) の初回 play は弾かれることがあるので、
// この関数をユーザー操作 (pointerdown/keydown) でも呼び、実 gesture context で確実に解錠する。
function ensureBgmPlaying() {
  const el = bgmEl.value;
  if (!el || !game.bgm || !el.paused) return;
  el.volume = game.audioGain;
  el.play().catch(() => {
    /* まだ gesture が足りない等は次の操作でリトライ */
  });
}

// store.bgm が変わったら src を差し替える (場所変化)。null なら停止。
watch(
  () => game.bgm,
  (url) => {
    const el = bgmEl.value;
    if (!el) return;
    if (url) {
      if (el.src !== url) el.src = url;
      ensureBgmPlaying();
    } else {
      el.pause();
      el.removeAttribute("src");
    }
  },
);
// 音量/ミュート変更を再生中の BGM へ即時反映 (src は触らずループを切らさない)。
watch(
  () => game.audioGain,
  (g) => {
    if (bgmEl.value) bgmEl.value.volume = g;
  },
);

// 起動時: パッケージ一覧の取得 + 保存済みフォントサイズ (表示設定) の適用。
// + ユーザー操作のたびに BGM 再生を試みる (初回 play が autoplay で弾かれても次の操作で復帰する)。
onMounted(() => {
  game.refreshPackages();
  game.refreshLlmModel(); // TitleBar のモデル名バッジ + OS ウィンドウタイトル
  game.checkAppUpdate(); // 配布サイトに新しいアプリがあれば TitleBar に「最新版があります」
  const px = Number(localStorage.getItem("kataribe.fontScale")) || 18; // 既定 = 標準 18px
  document.documentElement.style.fontSize = `${px}px`;
  window.addEventListener("pointerdown", ensureBgmPlaying);
  window.addEventListener("keydown", ensureBgmPlaying);
  // あらすじ圧縮の開始合図 (spec 10)。play_turn は同期で回すので、この間ローディング文言を
  // 「あらすじをまとめています……」へ切り替える (解除は playTurn の finally)。
  listen("synopsis-compacting", () => {
    game.compacting = true;
  });
  // エピローグ生成の開始合図 (spec 11)。同じ仕組みで文言を切り替える。
  listen("epilogue-writing", () => {
    game.writingEpilogue = true;
  });
  // あらすじ生成の失敗通知 (spec 10)。リリースビルドはコンソールが無いので、
  // トーストで可視化する (恒久失敗 = 規約違反等で永遠にあらすじが無いまま、を防ぐ)。
  // プレイは続行され次の受理ターンで自動再試行される。
  listen<string>("synopsis-failed", (ev) => {
    game.logToast = t("store.synopsisFailed", { error: ev.payload });
  });
});
</script>

<template>
  <div class="flex flex-col h-screen w-screen overflow-hidden">
    <!-- カスタムタイトルバー (ネイティブ装飾の代替) -->
    <TitleBar
      :title="game.title"
      :model="game.llmModel"
      :update-available="game.updateAvailable"
      :latest-version="game.latestVersion"
      @open-settings="showSettings = true"
      @open-packages="showPackages = true"
      @open-update="game.openUpdateSite()"
      @save-log="game.saveLog()"
    />

    <!-- ヘッダー: 再生パッケージの選択 + 開始 -->
    <header class="flex items-center gap-3 px-6 py-2.5 border-b border-ash bg-ink">
      <span class="text-parchment/45 text-sm truncate">
        {{ game.title || t("app.selectToStart") }}
      </span>
      <!-- 右端は定位置レイアウト: select は固定幅、ボタンはアイコンの固定スロット
           (続きからが無い時は invisible = 場所を保ったまま消す。隣がズレない)。 -->
      <div class="ml-auto flex items-center gap-2">
        <select
          v-model="game.packagePath"
          :disabled="game.loading"
          class="w-[28rem] max-w-[50vw] truncate rounded bg-ash/40 px-2 py-1 text-sm text-parchment focus:outline-none"
        >
          <option
            v-for="p in packagesNewestFirst"
            :key="p.path"
            :value="p.path"
            :disabled="!p.playable || !!p.error"
          >
            {{ p.error ? `⚠ ${p.path}` : p.title }}
          </option>
        </select>
        <button
          :disabled="game.loading || selectedAutosaveTurn == null"
          :class="[
            'grid h-8 w-8 place-items-center rounded bg-ember/80 hover:bg-ember text-ink disabled:opacity-40',
            selectedAutosaveTurn == null ? 'invisible' : '',
          ]"
          :title="t('app.resumeTitle', { turn: selectedAutosaveTurn ?? 0 })"
          :aria-label="t('app.resumeAria')"
          @click="game.resumeGame()"
        >
          <Icon name="play" :size="18" />
        </button>
        <!-- セーブ: プレイ中の状態を手動スロットへ (spec 07 Phase D)。プレイ前 or 一覧の選択が
             プレイ中のゲームと食い違うときは無効 (保存先の取り違え防止)。 -->
        <button
          :disabled="game.loading || !game.started || saveMismatch"
          class="grid h-8 w-8 place-items-center rounded text-parchment/60 hover:bg-ash/60 hover:text-parchment disabled:opacity-40"
          :title="saveMismatch ? t('app.saveSlotsMismatch') : t('app.saveSlots')"
          :aria-label="t('app.saveSlots')"
          @click="slotDialog = 'save'"
        >
          <Icon name="save" :size="18" />
        </button>
        <!-- ロード: 選択中パッケージのスロットから再開 (プレイ中でも前のプレイを置き換える)。 -->
        <button
          :disabled="game.loading"
          class="grid h-8 w-8 place-items-center rounded text-parchment/60 hover:bg-ash/60 hover:text-parchment disabled:opacity-40"
          :title="t('app.loadSlots')"
          :aria-label="t('app.loadSlots')"
          @click="slotDialog = 'load'"
        >
          <Icon name="load" :size="18" />
        </button>
        <!-- 新しいゲーム: 通常は枠なし (アイコンのみ)、hover で従来の箱が浮かぶ。 -->
        <button
          :disabled="game.loading"
          class="grid h-8 w-8 place-items-center rounded text-parchment/60 hover:bg-ash/60 hover:text-parchment disabled:opacity-40"
          :title="t('app.newGame')"
          :aria-label="t('app.newGame')"
          @click="game.newGame()"
        >
          <Icon name="new" :size="18" />
        </button>
      </div>
    </header>

    <!-- 本体 -->
    <div class="flex flex-1 overflow-hidden">
      <main
        class="flex-1 flex flex-col min-w-0 bg-cover bg-center transition-[background-image] duration-700"
        :style="game.backgroundStyle"
      >
        <div
          v-if="!game.started"
          class="flex-1 flex items-center justify-center text-parchment/40 px-6 text-center"
        >
          {{ t("app.emptyHint") }}
        </div>
        <!-- メッセージは背景画像 (暗幕) の上の物語コンテンツ = 背景がある時はテーマに関わらず
             dark 配色で描く (濃色文字が暗幕に埋もれない)。入力欄 (ActionInput) は UI クロームゆえ
             グローバルテーマに従う (ライトでは明るい入力欄)。 -->
        <ConversationLog v-else :data-theme="game.background ? 'dark' : null" />
        <ActionInput />
      </main>

      <!-- 右ペインの幅可変ツマミ。ドラッグで StatePanel の幅を変える (localStorage 永続)。
           パネルは常に画面右端に接するので 幅 = 画面幅 − ポインタ X。 -->
      <div
        class="panel-resizer shrink-0"
        role="separator"
        aria-orientation="vertical"
        :aria-label="t('app.resizePanel')"
        :title="t('app.resizePanel')"
        @pointerdown="startPanelResize"
        @dblclick="game.setPanelWidth(256)"
      ></div>

      <StatePanel />
    </div>

    <!-- ループ BGM (不可視の再生デバイス。src は store.bgm に追従)。 -->
    <audio ref="bgmEl" loop class="hidden" />

    <!-- ダイアログ (TitleBar のボタンから開く) -->
    <PackageDialog v-if="showPackages" @close="showPackages = false" />
    <SettingsDialog v-if="showSettings" @close="showSettings = false" />
    <!-- 手動セーブスロット (ヘッダーのセーブ/ロードボタンから開く。spec 07 Phase D) -->
    <SaveSlotDialog v-if="slotDialog" :mode="slotDialog" @close="slotDialog = null" />
    <!-- 自前の確認ダイアログ (window.confirm 置き換え。store.askConfirm が開く) -->
    <ConfirmDialog />

    <!-- ログ保存などの一時トースト (右下に数秒) -->
    <transition name="toast">
      <div
        v-if="toast"
        class="fixed bottom-4 right-4 z-[60] max-w-[32rem] rounded-lg border border-ash bg-ink/95 px-4 py-2 text-sm text-parchment shadow-2xl"
      >
        {{ toast }}
      </div>
    </transition>
  </div>
</template>

<style scoped>
.toast-enter-active,
.toast-leave-active {
  transition: opacity 0.25s ease, transform 0.25s ease;
}
.toast-enter-from,
.toast-leave-to {
  opacity: 0;
  transform: translateY(8px);
}

/* 右ペインの幅可変ツマミ。細い当たり判定 + hover/ドラッグでハイライト。ダブルクリックで既定幅に戻る。 */
.panel-resizer {
  width: 6px;
  cursor: col-resize;
  background: transparent;
  transition: background-color 0.15s ease;
  touch-action: none; /* pointer ドラッグを妨げない (スクロールに奪われない) */
}
.panel-resizer:hover,
.panel-resizer:active {
  background: rgb(var(--ember) / 0.7);
}
</style>
