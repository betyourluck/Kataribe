/**
 * 軽量 i18n（依存ゼロ）。UI 文言のロケール切替を Pinia/vue-i18n なしで担う。
 *
 * 設計:
 * - `locale` は単一の reactive ref。localStorage `kataribe.lang`（engine 言語と同じキー）と同期し、
 *   engine 由来メッセージ（却下理由の localize）と UI 文言のロケールを一元化する。
 * - `t(key, params?)` は messages のドットパスを引く。現ロケールに無ければ ja へ、
 *   それも無ければ key 自身を返す（未翻訳が「消える」より key が見える方がデバッグしやすい）。
 * - `t` は render 中に `locale.value` を読むので、テンプレートで `t(...)` を呼べば
 *   ロケール変更で自動再描画される（リアクティブ即時切替）。ページ再読込は不要。
 * - `<script setup>` は import をテンプレートへ公開するので `import { t } from "../i18n"` だけで
 *   テンプレートから `{{ t('...') }}` が使える。
 *
 * 将来 vue-i18n へ移すときも、呼び出し規約（`t('area.key', {param})`）を今から揃えておけば
 * 置換は import 差し替えで済む。ja/en の 2 言語・中規模文字列には本実装で十分。
 */
import { ref } from "vue";
import { messages } from "./messages";

export type Locale = "ja" | "en";

const LANG_KEY = "kataribe.lang";

function initialLocale(): Locale {
  return localStorage.getItem(LANG_KEY) === "en" ? "en" : "ja";
}

/** 現在の UI ロケール（reactive）。テンプレート/算出プロパティから読むと切替に追従する。 */
export const locale = ref<Locale>(initialLocale());

/** ロケールを切り替えて localStorage に永続化する（言語設定タブから呼ぶ）。 */
export function setLocale(l: Locale) {
  locale.value = l;
  localStorage.setItem(LANG_KEY, l);
}

/** ドットパスでネスト辞書を引く（`"titlebar.settings"` → messages[loc].titlebar.settings）。 */
function lookup(dict: Record<string, unknown>, key: string): string | undefined {
  let cur: unknown = dict;
  for (const part of key.split(".")) {
    if (cur && typeof cur === "object" && part in (cur as Record<string, unknown>)) {
      cur = (cur as Record<string, unknown>)[part];
    } else {
      return undefined;
    }
  }
  return typeof cur === "string" ? cur : undefined;
}

/** `{name}` 形式のプレースホルダを params で置換する（複数形/格変化は扱わない割り切り）。 */
function interpolate(raw: string, params: Record<string, string | number>): string {
  return raw.replace(/\{(\w+)\}/g, (_, k) => (k in params ? String(params[k]) : `{${k}}`));
}

/**
 * UI 文言を引く。`locale.value` を読むのでテンプレートで呼べば切替に追従する。
 * 未翻訳は ja へフォールバックし、それも無ければ key をそのまま返す。
 */
export function t(key: string, params?: Record<string, string | number>): string {
  const raw = lookup(messages[locale.value], key) ?? lookup(messages.ja, key) ?? key;
  return params ? interpolate(raw, params) : raw;
}
