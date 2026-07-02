<script setup lang="ts">
// UI chrome のモノカラー線アイコン (Feather/Lucide 風)。stroke=currentColor ゆえ
// 親のテキスト色に追従する = テーマ一貫のモノクロ。パッケージ同梱画像とは別物 (UI 装飾)。
defineProps<{
  name: "location" | "bag" | "flag" | "turn" | "gauge" | "sparkle" | "target";
  size?: number;
}>();
</script>

<template>
  <svg
    :width="size ?? 14"
    :height="size ?? 14"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="2"
    stroke-linecap="round"
    stroke-linejoin="round"
    class="inline-block shrink-0"
    aria-hidden="true"
  >
    <!-- 現在地: マップのピン -->
    <template v-if="name === 'location'">
      <path d="M20 10c0 6-8 12-8 12s-8-6-8-12a8 8 0 0 1 16 0Z" />
      <circle cx="12" cy="10" r="3" />
    </template>
    <!-- 所持品: カバン (ブリーフケース) -->
    <template v-else-if="name === 'bag'">
      <rect x="3" y="7" width="18" height="13" rx="2" />
      <path d="M8 7V5a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" />
      <path d="M3 12h18" />
    </template>
    <!-- フラグ: 旗 -->
    <template v-else-if="name === 'flag'">
      <path d="M5 21V4" />
      <path d="M5 4h12l-2.5 4 2.5 4H5" />
    </template>
    <!-- ターン: 時計 -->
    <template v-else-if="name === 'turn'">
      <circle cx="12" cy="12" r="9" />
      <path d="M12 7v5l3 2" />
    </template>
    <!-- ステータス: 波形 (activity) -->
    <template v-else-if="name === 'gauge'">
      <path d="M22 12h-4l-3 8L9 4l-3 8H2" />
    </template>
    <!-- 能力: 四芒星のきらめき -->
    <template v-else-if="name === 'sparkle'">
      <path d="M12 3l2 6 6 2-6 2-2 6-2-6-6-2 6-2 2-6Z" />
    </template>
    <!-- 結末 (ゴール): 的 -->
    <template v-else-if="name === 'target'">
      <circle cx="12" cy="12" r="9" />
      <circle cx="12" cy="12" r="5" />
      <circle cx="12" cy="12" r="1" />
    </template>
  </svg>
</template>
