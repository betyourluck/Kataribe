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
import { ref, onMounted } from "vue";
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

// 起動時: パッケージ一覧の取得 + 保存済みフォントサイズ (表示設定) の適用。
onMounted(() => {
  game.refreshPackages();
  const px = Number(localStorage.getItem("kataribe.fontScale")) || 16;
  document.documentElement.style.fontSize = `${px}px`;
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

    <!-- ダイアログ (TitleBar のボタンから開く) -->
    <PackageDialog v-if="showPackages" @close="showPackages = false" />
    <SettingsDialog v-if="showSettings" @close="showSettings = false" />
  </div>
</template>
