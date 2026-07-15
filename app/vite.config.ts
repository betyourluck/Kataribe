import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";
import { execSync } from "node:child_process";

// @ts-expect-error process は nodejs グローバル
const host = process.env.TAURI_DEV_HOST;

// ビルド時に git の最新タグ (例 "v0.3.2") を取り込み、タイトルバーの版表示に使う
// (__APP_VERSION__ で参照)。タグを手動 bump しなくても自動追従する
// (tauri.conf.json の version フィールドとは独立)。git 不在/タグ無しは空文字 = 非表示。
function gitVersion(): string {
  try {
    return execSync("git describe --tags --abbrev=0", { encoding: "utf8" }).trim();
  } catch {
    return "";
  }
}

// https://vite.dev/config/ — Tauri 開発向け設定。
export default defineConfig(async () => ({
  plugins: [vue()],
  define: {
    __APP_VERSION__: JSON.stringify(gitVersion()),
  },
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
