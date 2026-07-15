/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{vue,ts}"],
  theme: {
    extend: {
      colors: {
        // 「語り部」: 暖炉の残り火を思わせる暖色テーマ。値は CSS 変数 (main.css) で定義し
        // data-theme (ライト/ダーク) で入れ替える。<alpha-value> で opacity 修飾子 (bg-ash/30 等) を保つ。
        ink: "rgb(var(--ink) / <alpha-value>)",             // 背景
        parchment: "rgb(var(--parchment) / <alpha-value>)", // 本文
        ember: "rgb(var(--ember) / <alpha-value>)",         // アクセント (熾火)
        ash: "rgb(var(--ash) / <alpha-value>)",             // 罫線・パネル
        glow: "rgb(var(--glow) / <alpha-value>)",           // 強調 (炎の明)
        warn: "rgb(var(--warn) / <alpha-value>)",           // 注意 (自己修復の ⚠ 等)
      },
      fontFamily: {
        serif: ['"Noto Serif JP"', "serif"],
      },
    },
  },
  plugins: [],
};
