/**
 * ライト/ダークのテーマ切替。
 *
 * 配色は CSS 変数 (main.css の `:root` / `:root[data-theme="light"]`) で定義し、`<html>` の
 * `data-theme` 属性だけを切り替える。tailwind の色は `rgb(var(--x) / <alpha-value>)` を指すので、
 * セマンティック名 (bg-ink=背景 / text-parchment=本文 / …) の**値だけ**が入れ替わり、既存の全
 * ユーティリティ (opacity 付き含む) が自動で反転する。
 *
 * 既定はダーク。状態は localStorage `kataribe.theme` に置く。
 */
import { ref } from "vue";

export type Theme = "dark" | "light";
const KEY = "kataribe.theme";

function stored(): Theme {
  return localStorage.getItem(KEY) === "light" ? "light" : "dark";
}

/** 現在のテーマ (リアクティブ)。トグルボタンのアイコン切替に使う。 */
export const theme = ref<Theme>(stored());

/** `data-theme` を `<html>` に反映する (CSS 変数がこれで切り替わる)。起動時に main.ts が呼ぶ (フラッシュ防止)。 */
export function applyTheme(): void {
  document.documentElement.dataset.theme = theme.value;
}

/** ダーク⇄ライトを切り替え、localStorage に保存して即反映する。 */
export function toggleTheme(): void {
  theme.value = theme.value === "dark" ? "light" : "dark";
  localStorage.setItem(KEY, theme.value);
  applyTheme();
}
