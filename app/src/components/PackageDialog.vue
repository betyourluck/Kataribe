<script setup lang="ts">
/**
 * パッケージ一覧ダイアログ (TitleBar の List ボタンから開く)。
 * - 「ローカル」タブ: localStorage が保持するパッケージフォルダのパスを追加/削除する。
 * - 「配布サイト」タブ (spec 05 Phase C): 書庫サイトの一覧を fetch し、選んだ zip を
 *   DL→検証→展開→パス登録までアプリ内で完結させる。サイト URL は設定項目 (既定 = 公式)。
 */
import { ref } from "vue";
import { useGameStore, SITE_CATEGORIES } from "../stores/game";
import type { RemotePackage } from "../types/api";
import { t } from "../i18n";
import Icon from "./Icon.vue";

const game = useGameStore();
const emit = defineEmits<{ (e: "close"): void }>();

const tab = ref<"local" | "site">("local");

// --- ローカルタブ ---
const newPath = ref("");
function add() {
  game.addPackage(newPath.value);
  newPath.value = "";
}

// --- 配布サイトタブ ---
const siteUrlInput = ref(game.siteUrl);
const q = ref("");
const category = ref("");
const sort = ref("new");
const lastInstalled = ref<string | null>(null);

function openSiteTab() {
  tab.value = "site";
  // 初回だけ自動 fetch (URL 変更や検索は明示操作で)。
  if (!game.remote && !game.remoteLoading) search(1);
}

function applySiteUrl() {
  game.setSiteUrl(siteUrlInput.value);
  siteUrlInput.value = game.siteUrl; // 正規化 (空→既定) を反映
  search(1);
}

function search(page: number) {
  game.fetchSitePackages({ page, q: q.value, category: category.value, sort: sort.value });
}

async function install(p: RemotePackage) {
  lastInstalled.value = null;
  const installed = await game.installSitePackage(p.id);
  if (installed) lastInstalled.value = t("packages.installed", { title: installed.title });
}

// カテゴリ表示名は i18n（`packages.categories.<id>`、空 id は all）。未知カテゴリは id を出す。
function categoryLabel(id: string): string {
  const key = `packages.categories.${id || "all"}`;
  const label = t(key);
  return label === key ? id : label;
}

function fmtSize(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)}MB`;
  return `${Math.max(1, Math.round(bytes / 1024))}KB`;
}

function totalPages(): number {
  const r = game.remote;
  return r ? Math.max(1, Math.ceil(r.total / r.page_size)) : 1;
}
</script>

<template>
  <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/60" @click.self="emit('close')">
    <div class="w-[44rem] max-w-[92vw] h-[80vh] flex flex-col rounded-lg border border-ash bg-ink shadow-2xl">
      <header class="flex items-center gap-4 px-4 py-3 border-b border-ash">
        <h2 class="text-glow font-bold tracking-wide">{{ t("packages.title") }}</h2>
        <nav class="flex gap-1 text-sm">
          <button
            class="rounded px-3 py-1"
            :class="tab === 'local' ? 'bg-ember/80 text-ink font-bold' : 'text-parchment/60 hover:text-parchment'"
            @click="tab = 'local'"
          >
            {{ t("packages.tabLocal") }}
          </button>
          <button
            class="rounded px-3 py-1"
            :class="tab === 'site' ? 'bg-ember/80 text-ink font-bold' : 'text-parchment/60 hover:text-parchment'"
            @click="openSiteTab"
          >
            {{ t("packages.tabSite") }}
          </button>
        </nav>
        <button class="ml-auto text-parchment/50 hover:text-parchment" :aria-label="t('packages.close')" @click="emit('close')">✕</button>
      </header>

      <!-- ============ ローカルタブ ============ -->
      <template v-if="tab === 'local'">
        <div class="flex-1 overflow-y-auto px-4 py-3 space-y-2">
          <p v-if="!game.packages.length" class="text-parchment/40 text-sm py-6 text-center">
            {{ t("packages.localEmpty") }}
          </p>
          <div
            v-for="p in game.packages"
            :key="p.path"
            class="flex items-start gap-3 rounded border border-ash/60 bg-ash/20 px-3 py-2"
          >
            <div class="min-w-0 flex-1">
              <div class="flex items-center gap-2">
                <span class="font-bold text-parchment truncate">{{ p.error ? p.path : p.title }}</span>
                <span v-if="p.error" class="shrink-0 rounded bg-red-900/60 px-1.5 text-xs text-red-200">{{ t("packages.loadFailed") }}</span>
              </div>
              <div class="text-xs text-parchment/45 truncate">{{ p.path }}</div>
              <div v-if="p.description && !p.error" class="text-xs text-parchment/60 mt-0.5">{{ p.description }}</div>
              <div v-if="p.error" class="text-xs text-red-300/80 mt-0.5">{{ p.error }}</div>
            </div>
            <button
              class="shrink-0 text-parchment/40 hover:text-red-400 text-sm"
              :title="t('packages.removeTitle')"
              :aria-label="t('packages.remove')"
              @click="game.removePackage(p.path)"
            >
              {{ t("packages.remove") }}
            </button>
          </div>
        </div>

        <footer class="flex items-center gap-2 px-4 py-3 border-t border-ash">
          <input
            v-model="newPath"
            :placeholder="t('packages.pathPlaceholder')"
            class="flex-1 rounded bg-ash/40 px-2 py-1 text-sm text-parchment focus:outline-none"
            @keyup.enter="add"
          />
          <button
            class="shrink-0 text-parchment/60 hover:text-ember px-1.5 py-1"
            :title="t('packages.browseTitle')"
            :aria-label="t('packages.browse')"
            @click="game.browseAndAddPackage()"
          >
            <Icon name="folder" :size="18" />
          </button>
          <button
            :disabled="!newPath.trim()"
            class="rounded bg-ember/80 hover:bg-ember px-3 py-1 text-sm text-ink font-bold disabled:opacity-40"
            @click="add"
          >
            {{ t("packages.add") }}
          </button>
        </footer>
      </template>

      <!-- ============ 配布サイトタブ ============ -->
      <template v-else>
        <!-- サイト URL + 検索コントロール -->
        <div class="px-4 py-2 border-b border-ash space-y-2">
          <div class="flex items-center gap-2">
            <span class="text-xs text-parchment/50 shrink-0">{{ t("packages.site") }}</span>
            <input
              v-model="siteUrlInput"
              placeholder="https://kataribe.outcasts.jp"
              class="flex-1 rounded bg-ash/40 px-2 py-1 text-xs text-parchment focus:outline-none"
              @keyup.enter="applySiteUrl"
            />
            <button
              class="rounded bg-ash/60 hover:bg-ash px-2 py-1 text-xs text-parchment"
              :title="t('packages.connectTitle')"
              @click="applySiteUrl"
            >
              {{ t("packages.connect") }}
            </button>
          </div>
          <div class="flex items-center gap-2">
            <input
              v-model="q"
              :placeholder="t('packages.searchPlaceholder')"
              class="flex-1 rounded bg-ash/40 px-2 py-1 text-sm text-parchment focus:outline-none"
              @keyup.enter="search(1)"
            />
            <select v-model="category" class="rounded bg-ash/40 px-1 py-1 text-sm text-parchment" @change="search(1)">
              <option v-for="c in SITE_CATEGORIES" :key="c.id" :value="c.id">{{ categoryLabel(c.id) }}</option>
            </select>
            <select v-model="sort" class="rounded bg-ash/40 px-1 py-1 text-sm text-parchment" @change="search(1)">
              <option value="new">{{ t("packages.sortNew") }}</option>
              <option value="popular">{{ t("packages.sortPopular") }}</option>
              <option value="rating">{{ t("packages.sortRating") }}</option>
            </select>
            <button
              class="rounded bg-ember/80 hover:bg-ember px-3 py-1 text-sm text-ink font-bold"
              @click="search(1)"
            >
              {{ t("packages.search") }}
            </button>
          </div>
        </div>

        <!-- 一覧 -->
        <div class="flex-1 overflow-y-auto px-4 py-3 space-y-2">
          <p v-if="game.remoteLoading" class="text-parchment/40 text-sm py-6 text-center">{{ t("packages.loading") }}</p>
          <p v-else-if="game.remoteError" class="text-red-300/90 text-sm py-6 text-center whitespace-pre-wrap">
            {{ game.remoteError }}
          </p>
          <p v-else-if="!game.remote || !game.remote.items.length" class="text-parchment/40 text-sm py-6 text-center">
            {{ t("packages.notFound") }}
          </p>
          <template v-else>
            <div
              v-for="p in game.remote.items"
              :key="p.id"
              class="flex items-start gap-3 rounded border border-ash/60 bg-ash/20 px-3 py-2"
            >
              <div class="min-w-0 flex-1">
                <div class="flex items-center gap-2">
                  <span class="font-bold text-parchment truncate">{{ p.title }}</span>
                  <span class="shrink-0 rounded bg-ash/70 px-1.5 text-xs text-parchment/70">{{ categoryLabel(p.category) }}</span>
                  <span
                    v-if="p.kataribe_version"
                    class="shrink-0 rounded bg-ash/50 px-1.5 text-xs text-parchment/60"
                    :title="t('packages.kataribeVersionTitle')"
                  >
                    Kataribe {{ p.kataribe_version }}
                  </span>
                  <span
                    v-if="p.is_mature"
                    class="shrink-0 rounded bg-red-900/70 px-1.5 text-xs text-red-200"
                    :title="t('packages.matureTitle')"
                  >
                    Mature
                  </span>
                </div>
                <div class="text-xs text-parchment/45 mt-0.5">
                  <span v-if="p.review_count > 0">{{ t("packages.rating", { rating: (p.avg_rating ?? 0).toFixed(1), count: p.review_count }) }}</span>
                  <span v-else>{{ t("packages.unrated") }}</span>
                  <span class="mx-1.5">·</span>DL {{ p.download_count }}
                  <span class="mx-1.5">·</span>{{ fmtSize(p.file_size) }}
                  <span class="mx-1.5">·</span>{{ p.uploader_display_name }}
                </div>
                <div v-if="p.description" class="text-xs text-parchment/60 mt-0.5">{{ p.description }}</div>
              </div>
              <button
                class="shrink-0 rounded bg-ember/80 hover:bg-ember px-3 py-1 text-sm text-ink font-bold disabled:opacity-40"
                :disabled="game.installingId !== null"
                :title="t('packages.installTitle')"
                @click="install(p)"
              >
                {{ game.installingId === p.id ? t("packages.installing") : t("packages.install") }}
              </button>
            </div>
          </template>
        </div>

        <!-- フッター: 取得結果 + ページネーション -->
        <footer class="flex items-center gap-2 px-4 py-2 border-t border-ash text-sm">
          <span v-if="lastInstalled" class="text-emerald-300/90 text-xs truncate">✓ {{ lastInstalled }}</span>
          <div class="ml-auto flex items-center gap-2" v-if="game.remote && totalPages() > 1">
            <button
              class="rounded bg-ash/50 hover:bg-ash px-2 py-0.5 text-xs text-parchment disabled:opacity-40"
              :disabled="game.remote.page <= 1 || game.remoteLoading"
              @click="search(game.remote.page - 1)"
            >
              {{ t("packages.prev") }}
            </button>
            <span class="text-xs text-parchment/50">{{ game.remote.page }} / {{ totalPages() }}</span>
            <button
              class="rounded bg-ash/50 hover:bg-ash px-2 py-0.5 text-xs text-parchment disabled:opacity-40"
              :disabled="game.remote.page >= totalPages() || game.remoteLoading"
              @click="search(game.remote.page + 1)"
            >
              {{ t("packages.next") }}
            </button>
          </div>
        </footer>
      </template>
    </div>
  </div>
</template>
