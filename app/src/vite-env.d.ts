/// <reference types="vite/client" />

/** ビルド時に vite.config が git の最新タグを注入する (例 "v0.3.2"、タグ無しは "")。 */
declare const __APP_VERSION__: string;

declare module "*.vue" {
  import type { DefineComponent } from "vue";
  const component: DefineComponent<{}, {}, any>;
  export default component;
}
