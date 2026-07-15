/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{vue,ts}"],
  theme: {
    extend: {
      colors: {
        // 「語り部」: 暖炉の残り火を思わせる暗い暖色テーマ。
        ink: "#15110e",        // 背景 (焦げ茶の黒)
        parchment: "#e8ddc8",  // 本文 (羊皮紙)
        ember: "#d98a4a",      // アクセント (熾火)
        ash: "#3a322b",        // 罫線・パネル
        glow: "#f0c27b",       // 強調 (炎の明)
        warn: "#d9645a",       // 注意 (薄めの赤・ember の赤寄り兄弟。自己修復の ⚠ 等)
      },
      fontFamily: {
        serif: ['"Noto Serif JP"', "serif"],
      },
    },
  },
  plugins: [],
};
