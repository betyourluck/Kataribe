<script setup lang="ts">
// UI chrome のモノカラー線アイコン (Feather/Lucide 風)。stroke=currentColor ゆえ
// 親のテキスト色に追従する = テーマ一貫のモノクロ。パッケージ同梱画像とは別物 (UI 装飾)。
defineProps<{
  name: "location" | "bag" | "flag" | "turn" | "gauge" | "sparkle" | "target" | "load" | "new" | "plus" | "trash" | "folder" | "book" | "save" | "play";
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
    <!-- ロード (手動スロット): トレイから↑ (データを取り出す) -->
    <template v-else-if="name === 'load'">
      <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
      <path d="M7 8l5-5 5 5" />
      <path d="M12 3v12" />
    </template>
    <!-- 新規作成 (新しいゲーム): 書類 + プラス -->
    <template v-else-if="name === 'new'">
      <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8Z" />
      <path d="M14 3v5h5" />
      <path d="M12 12v6" />
      <path d="M9 15h6" />
    </template>
    <!-- 追加: プラス -->
    <template v-else-if="name === 'plus'">
      <path d="M12 5v14" />
      <path d="M5 12h14" />
    </template>
    <!-- あらすじ: 開いた本 -->
    <template v-else-if="name === 'book'">
      <path d="M2 4h7a3 3 0 0 1 3 3v13a2 2 0 0 0-2-2H2Z" />
      <path d="M22 4h-7a3 3 0 0 0-3 3v13a2 2 0 0 1 2-2h8Z" />
    </template>
    <!-- 参照 (フォルダを選ぶ): 閉じたフォルダ -->
    <template v-else-if="name === 'folder'">
      <path d="M4 20h16a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-8l-2-3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" />
    </template>
    <!-- セーブ (手動スロット): トレイに↓ (データをしまう) -->
    <template v-else-if="name === 'save'">
      <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
      <path d="M7 10l5 5 5-5" />
      <path d="M12 15V3" />
    </template>
    <!-- 続きから: 再生 (▶) -->
    <template v-else-if="name === 'play'">
      <path d="M7 4l13 8-13 8Z" />
    </template>
    <!-- 削除: ゴミ箱 -->
    <template v-else-if="name === 'trash'">
      <path d="M3 6h18" />
      <path d="M8 6V4a1 1 0 0 1 1-1h6a1 1 0 0 1 1 1v2" />
      <path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6" />
      <path d="M10 11v6" />
      <path d="M14 11v6" />
    </template>
  </svg>
</template>
