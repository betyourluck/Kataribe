import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

// @ts-expect-error process は nodejs グローバル
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/ — Tauri 開発向け設定。
export default defineConfig(async () => ({
  plugins: [vue()],
  // rust のエラーを Vite が隠さないように。
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    watch: {
      // src-tauri は Vite で監視しない。
      ignored: ["**/src-tauri/**"],
    },
  },
}));
