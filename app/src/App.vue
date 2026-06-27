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
import { ref, watch, onMounted } from "vue";
import { useGameStore } from "./stores/game";
import TitleBar from "./components/TitleBar.vue";
import PackageDialog from "./components/PackageDialog.vue";
import SettingsDialog from "./components/SettingsDialog.vue";
import ConversationLog from "./components/ConversationLog.vue";
import StatePanel from "./components/StatePanel.vue";
import ActionInput from "./components/ActionInput.vue";

const game = useGameStore();
const showSettings = ref(false);
const showPackages = ref(false);

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
  const px = Number(localStorage.getItem("kataribe.fontScale")) || 16;
  document.documentElement.style.fontSize = `${px}px`;
  window.addEventListener("pointerdown", ensureBgmPlaying);
  window.addEventListener("keydown", ensureBgmPlaying);
});
</script>

<template>
  <div class="flex flex-col h-screen w-screen overflow-hidden">
    <!-- カスタムタイトルバー (ネイティブ装飾の代替) -->
    <TitleBar
      :title="game.title"
      @open-settings="showSettings = true"
      @open-packages="showPackages = true"
    />

    <!-- ヘッダー: 再生パッケージの選択 + 開始 -->
    <header class="flex items-center gap-3 px-6 py-2.5 border-b border-ash bg-ink">
      <span class="text-parchment/45 text-sm truncate">
        {{ game.title || "パッケージを選んで開始" }}
      </span>
      <div class="ml-auto flex items-center gap-2">
        <select
          v-model="game.packagePath"
          :disabled="game.loading"
          class="rounded bg-ash/40 px-2 py-1 text-sm text-parchment focus:outline-none"
        >
          <option
            v-for="p in game.packages"
            :key="p.path"
            :value="p.path"
            :disabled="!p.playable || !!p.error"
          >
            {{ p.error ? `⚠ ${p.path}` : p.title }}
          </option>
        </select>
        <button
          :disabled="game.loading"
          class="rounded bg-ember/80 hover:bg-ember px-3 py-1 text-sm text-ink font-bold disabled:opacity-40"
          @click="game.newGame()"
        >
          新しいゲーム
        </button>
      </div>
    </header>

    <!-- 本体 -->
    <div class="flex flex-1 overflow-hidden">
      <main class="flex-1 flex flex-col min-w-0 bg-cover bg-center transition-[background-image] duration-700" :style="game.backgroundStyle">
        <div
          v-if="!game.started"
          class="flex-1 flex items-center justify-center text-parchment/40 px-6 text-center"
        >
          パッケージを選んで「新しいゲーム」を押すと、忘れない・矛盾しない GM が物語を始めます。
        </div>
        <ConversationLog v-else />
        <ActionInput />
      </main>

      <StatePanel />
    </div>

    <!-- ループ BGM (不可視の再生デバイス。src は store.bgm に追従)。 -->
    <audio ref="bgmEl" loop class="hidden" />

    <!-- ダイアログ (TitleBar のボタンから開く) -->
    <PackageDialog v-if="showPackages" @close="showPackages = false" />
    <SettingsDialog v-if="showSettings" @close="showSettings = false" />
  </div>
</template>
