import { defineStore } from "pinia";
import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import { t } from "../i18n";
import type {
  GameView,
  TurnView,
  StateView,
  LogEntry,
  CharacterView,
  RemoteList,
  InstalledPackage,
  StatRollView,
  SynopsisView,
  LogLineView,
  SlotView,
  MapView,
} from "../types/api";

// d100 ロールアンダーの成功度 (spec 16) の表示ラベル。内部 id は英語 (ログ検索・セーブ安定)、
// 表示はこの言語表で差し替え可能。未知 id は素通し (前方互換)。
export function degreeLabel(degree: string): string {
  const key = `log.degree${degree.charAt(0).toUpperCase()}${degree.slice(1)}`;
  const label = t(key);
  return label === key ? degree : label;
}

// 可変量ダイス (roll_stat) の監査行 (spec 16): 「player SAN -4 (1d6=4)」。
export function statRollLine(sr: StatRollView): string {
  const bonus = sr.bonus !== 0 ? (sr.bonus > 0 ? `+${sr.bonus}` : `${sr.bonus}`) : "";
  const amount = sr.amount >= 0 ? `+${sr.amount}` : `${sr.amount}`;
  return `${sr.entity} ${sr.key} ${amount} (${sr.count}d${sr.sides}${bonus}=${sr.rolls.join("+")})`;
}

// アセット絶対パス → asset:// URL のメモ化 (convertFileSrc を毎回呼ばない。spec 01 小論点2)。
const assetUrlCache = new Map<string, string>();
function assetUrl(path: string | null): string | null {
  if (!path) return null;
  let url = assetUrlCache.get(path);
  if (!url) {
    url = convertFileSrc(path);
    assetUrlCache.set(path, url);
  }
  return url;
}

// localStorage キー: ユーザーが選べるパッケージフォルダのパス一覧 (配布物の置き場)。
const PACKAGES_KEY = "kataribe.packagePaths";
// 前回追加したパッケージの「親フォルダ」。参照ダイアログの初期ディレクトリに使う
// (多くの人は同じ親フォルダの下に複数パッケージを置くので、次回そこから選べる)。
const LAST_PKG_PARENT_KEY = "kataribe.lastPackageParent";
function loadLastPackageParent(): string {
  return localStorage.getItem(LAST_PKG_PARENT_KEY) || "";
}
/** パスの親フォルダを返す (Windows `\` と Unix `/` の両区切りに対応、末尾区切りは無視)。 */
function parentDir(path: string): string {
  const p = path.trim().replace(/[/\\]+$/, "");
  const i = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
  return i > 0 ? p.slice(0, i) : "";
}
// 背景の明るさ (0=暗幕最大で真っ暗 〜 100=暗幕なしで画像そのまま)。既定は中間の 50。
const BG_BRIGHTNESS_KEY = "kataribe.bgBrightness";
function loadBgBrightness(): number {
  const v = Number(localStorage.getItem(BG_BRIGHTNESS_KEY));
  return Number.isFinite(v) && v >= 0 && v <= 100 ? v : 50;
}
// 音量 0..100 (BGM ループと SE one-shot に共通でかかる)。既定は中間の 50。
const AUDIO_VOLUME_KEY = "kataribe.audioVolume";
const AUDIO_MUTED_KEY = "kataribe.audioMuted";
function loadAudioVolume(): number {
  const v = Number(localStorage.getItem(AUDIO_VOLUME_KEY));
  return Number.isFinite(v) && v >= 0 && v <= 100 ? v : 50;
}
function loadAudioMuted(): boolean {
  return localStorage.getItem(AUDIO_MUTED_KEY) === "true";
}
// --- 本文テキスト設定 (GM の語りの見た目。提示層のみ・localStorage 永続) ---
const MSG_FONT_KEY = "kataribe.msgFont";
const MSG_COLOR_KEY = "kataribe.msgColor";
const MSG_SHADOW_KEY = "kataribe.msgShadow";
/** 既定の本文色 (tailwind の parchment)。カラーピッカーの初期値と「既定に戻す」に使う。 */
export const DEFAULT_MSG_COLOR = "#e8ddc8";
/** 本文フォントの選択肢 (id → CSS font-family)。OS 同梱フォントへのフォールバック連鎖で環境差を吸収。 */
export const MESSAGE_FONTS: { id: string; label: string; family: string }[] = [
  { id: "default", label: "標準 (UI と同じ)", family: "" },
  {
    id: "mincho",
    label: "明朝",
    family: '"Yu Mincho", "游明朝", "Hiragino Mincho ProN", "MS PMincho", serif',
  },
  {
    id: "gothic",
    label: "ゴシック",
    family: '"Yu Gothic", "游ゴシック", "Hiragino Kaku Gothic ProN", "Meiryo", sans-serif',
  },
  {
    id: "maru",
    label: "丸ゴシック",
    family: '"HG丸ｺﾞｼｯｸM-PRO", "Hiragino Maru Gothic ProN", "Yu Gothic", sans-serif',
  },
];
function loadMsgFont(): string {
  const v = localStorage.getItem(MSG_FONT_KEY) || "default";
  return MESSAGE_FONTS.some((f) => f.id === v) ? v : "default";
}
function loadMsgColor(): string {
  return localStorage.getItem(MSG_COLOR_KEY) || "";
}
function loadMsgShadow(): number {
  const v = Number(localStorage.getItem(MSG_SHADOW_KEY));
  return Number.isFinite(v) && v >= 0 && v <= 100 ? v : 0;
}
// ビート (✦) / 想起 (┊) ブロックに敷く黒背景の濃さ 0..100 (0=なし)。色付き文字が
// 背景画像に溶けて読みにくい問題への手当て。本文 (語り) には敷かない。
const BEAT_BG_KEY = "kataribe.beatBgOpacity";
function loadBeatBgOpacity(): number {
  const v = Number(localStorage.getItem(BEAT_BG_KEY));
  return Number.isFinite(v) && v >= 0 && v <= 100 ? v : 40;
}

// 右ペイン (状態パネル) の幅 px。ドラッグハンドルで可変・localStorage 永続。
const PANEL_WIDTH_KEY = "kataribe.panelWidth";
export const PANEL_WIDTH_MIN = 200;
export const PANEL_WIDTH_MAX = 640;
function loadPanelWidth(): number {
  const v = Number(localStorage.getItem(PANEL_WIDTH_KEY));
  return Number.isFinite(v) && v >= PANEL_WIDTH_MIN && v <= PANEL_WIDTH_MAX ? v : 256; // 既定 w-64
}

// 会話ログのテキスト保存先フォルダ (空 = backend の既定 app_data_dir/logs)。
const LOG_DIR_KEY = "kataribe.logDir";
function loadLogDir(): string {
  return localStorage.getItem(LOG_DIR_KEY) || "";
}

// 同梱パッケージ (初回起動時の既定一覧。repo root 相対)。escape のみ
// (houkago は harness fixture へ移設、他サンプルは 2026-07-10 に配布から削除)。
const BUILTIN_PACKAGES = ["packages/escape"];

// --- AI モデルプロファイル (複数の LLM 設定を登録・切替。localStorage 永続) ---
// 動機: ヘビーユーザーは複数モデルを試す。従来は .env を手で書き換えていたのを、登録済み
// プロファイルから選んで「決定」で .env へ反映する形にする。**.env の書き込みは決定時のみ**
// (選択変更だけでは書かない)。API キーは平文で localStorage に入る (BYO-key・ローカル app)。
const AI_PROFILES_KEY = "kataribe.aiModelProfiles";
export interface AiModelProfile {
  id: string; // アプリ生成の主キー (name 重複を許すため)
  name: string; // 表示名 (重複可)
  model: string; // LLM_MODEL
  baseUrl: string; // LLM_BASE_URL
  apiKey: string; // LLM_API_KEY (平文・表示時マスク)
  useTools: boolean; // LLM_USE_TOOLS (ツール呼び出し)
}
// localStorage から読む (壊れていれば空)。全項目を型で検査し、欠けは既定で補う (前方互換)。
export function loadAiProfiles(): AiModelProfile[] {
  try {
    const raw = localStorage.getItem(AI_PROFILES_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter((p) => p && typeof p.id === "string" && typeof p.name === "string")
      .map((p) => ({
        id: p.id,
        name: p.name,
        model: typeof p.model === "string" ? p.model : "",
        baseUrl: typeof p.baseUrl === "string" ? p.baseUrl : "",
        apiKey: typeof p.apiKey === "string" ? p.apiKey : "",
        useTools: p.useTools !== false, // 既定 true
      }));
  } catch {
    return [];
  }
}
export function saveAiProfiles(list: AiModelProfile[]) {
  localStorage.setItem(AI_PROFILES_KEY, JSON.stringify(list));
}
// アプリ側の主キー生成 (name 重複を許すため)。WebView2 は crypto.randomUUID 対応。
export function newProfileId(): string {
  try {
    return crypto.randomUUID();
  } catch {
    return `p_${Date.now()}_${Math.floor(Math.random() * 1e9)}`;
  }
}
// プロファイルが現在の .env 設定と一致するか (初期表示で選択状態を復元する判定)。
// name/id は .env に無いので接続を決める 4 項目 (trim 済) で突き合わせる。
export function profileMatchesConfig(
  p: AiModelProfile,
  cfg: { base_url: string; model: string; api_key: string; use_tools: boolean },
): boolean {
  return (
    p.baseUrl.trim() === cfg.base_url.trim() &&
    p.model.trim() === cfg.model.trim() &&
    p.apiKey.trim() === cfg.api_key.trim() &&
    p.useTools === cfg.use_tools
  );
}

// --- 配布サイト「Kataribe 書庫」(spec 05 Phase C) ---
// サイト URL は設定項目 (既定 = 公式)。自前サーバも指せる = Outcasts 固有ロックインを避ける。
const SITE_URL_KEY = "kataribe.siteUrl";
export const DEFAULT_SITE_URL = "https://kataribe.outcasts.jp";
function loadSiteUrl(): string {
  return localStorage.getItem(SITE_URL_KEY) || DEFAULT_SITE_URL;
}
/** 書庫の固定 6 カテゴリ (outcast Spec 23。id はサーバのキー、label は表示名)。 */
export const SITE_CATEGORIES: { id: string; label: string }[] = [
  { id: "", label: "すべて" },
  { id: "mystery", label: "推理ゲーム" },
  { id: "escape", label: "脱出ゲーム" },
  { id: "daily", label: "現代日常" },
  { id: "horror", label: "ホラー" },
  { id: "fantasy", label: "ファンタジー" },
  { id: "sf_cyber", label: "SF・サイバー" },
];

// backend `list_packages` が返す1項目 (フォルダ一覧表示用)。
export interface PackageEntry {
  path: string;
  title: string;
  description: string;
  playable: boolean; // manifest が読めれば true (単発・campaign-entry 双方)。読込エラー時のみ false
  error: string | null;
  // オートセーブが在ればその時点のターン数 (「続きから (turn N)」ボタンの提示素)。無ければ null。
  autosave_turn: number | null;
  // 手動セーブスロット (spec 07 Phase D) が 1 つでも在るか (削除確認に使う)。
  has_slots: boolean;
}

// localStorage からパス一覧を読む (壊れていれば同梱既定にフォールバック)。
function loadPaths(): string[] {
  try {
    const raw = localStorage.getItem(PACKAGES_KEY);
    if (raw) {
      const parsed = JSON.parse(raw);
      if (Array.isArray(parsed) && parsed.every((p) => typeof p === "string")) return parsed;
    }
  } catch {
    /* 壊れた localStorage は無視して既定へ */
  }
  return [...BUILTIN_PACKAGES];
}
function savePaths(paths: string[]) {
  localStorage.setItem(PACKAGES_KEY, JSON.stringify(paths));
}

interface GameState {
  started: boolean;
  title: string;
  log: LogEntry[];
  state: StateView | null;
  loading: boolean;
  error: string | null;
  // 現在の背景画像 (asset:// URL)。場所/イベントで差し替え。無ければ null。
  background: string | null;
  // 現在地のループ BGM (asset:// URL)。場所変化で差し替え。無ければ null。
  bgm: string | null;
  // 現在地に居る NPC (顔アイコン行)。icon は asset:// URL 化済み。
  presentCharacters: CharacterView[];
  // 背景の明るさ 0..100 (大きいほど画像が明るく見える=暗幕が薄い)。グラフィック設定。
  bgBrightness: number;
  // 本文フォント (MESSAGE_FONTS の id)。表示設定。
  msgFont: string;
  // 本文の文字色 (hex)。空 = テーマ既定 (parchment)。表示設定。
  msgColor: string;
  // 本文の影の濃さ 0..100 (0=なし)。背景画像の上の可読性向上。表示設定。
  msgShadow: number;
  // ビート (✦) / 想起 (┊) ブロックの黒背景の濃さ 0..100 (0=なし)。表示設定。
  beatBgOpacity: number;
  // 右ペイン (状態パネル) の幅 px。ドラッグハンドルで可変。
  panelWidth: number;
  // 音量 0..100 (BGM/SE 共通)。サウンド設定。
  audioVolume: number;
  // ミュート (true なら音を出さない)。サウンド設定。
  audioMuted: boolean;
  // コンボリストで選択中のパッケージのパス (次に開始/ロードする対象)。
  packagePath: string;
  // いま実際にプレイ中のゲームのパッケージパス (session の真実)。コンボリストの選択
  // (packagePath) とは独立 — 選択だけ切り替えても動かない。セーブはこのゲームに対して行う
  // ので、packagePath がこれと食い違う間はセーブを無効化する (保存先の取り違え防止)。
  // プレイ前は空。new_game/resume/load_slot が applyGameView で確定させる。
  activePackagePath: string;
  // localStorage が保持するパッケージフォルダのパス一覧。
  packagePaths: string[];
  // 前回追加したパッケージの親フォルダ (参照ダイアログの初期ディレクトリ)。無ければ空。
  lastPackageParent: string;
  // 各パスの manifest を読んだ一覧 view (backend list_packages の結果)。
  packages: PackageEntry[];
  // --- 配布サイト (spec 05 Phase C) ---
  // 書庫サイトの URL (設定項目、localStorage 永続)。
  siteUrl: string;
  // 書庫の一覧応答 (fetch 済みのページ)。未取得なら null。
  remote: RemoteList | null;
  // 一覧 fetch / 取得中フラグとエラー (ダイアログの表示分岐)。
  remoteLoading: boolean;
  remoteError: string | null;
  // 取得 (DL→展開) 中のパッケージ id。null なら待機。
  installingId: string | null;
  // 会話ログのテキスト保存先フォルダ (空 = 既定)。設定「ログ」タブで指定。
  logDir: string;
  // ログ保存/フォルダ操作の一時トースト (App.vue が数秒表示して消す)。
  logToast: string;
  // 使用中の AI モデル名 (TitleBar バッジ + OS ウィンドウタイトル)。get_llm_config から取得。
  llmModel: string;
  // 配布サイトに現在版より新しいアプリがあるか (TitleBar の「最新版があります」表示)。
  updateAvailable: boolean;
  // 配布サイトの最新版タグ (表示用。例 "v0.3.3")。
  latestVersion: string;
  // 開発者モード (KATARIBE_DEV_MODE)。ON で GM に「テストプレイ・<meta:> 質問可」を刷り込む。
  devMode: boolean;
  // キャッシュ連続 miss の警告を出したか (エッジトリガー latch。ヒット復帰で再武装)。
  cacheWarned: boolean;
  // あらすじ (spec 10)。圧縮済み章の全量 (append-only — TurnView の差分を push して伸ばす)。
  synopsis: SynopsisView[];
  // 「最近の出来事」= 未圧縮 chronicle の 1 行要約列 (あらすじタブの下段)。
  recentLog: LogLineView[];
  // backend があらすじ圧縮中 (synopsis-compacting イベント)。ローディング文言を切り替える。
  compacting: boolean;
  // backend がエピローグ生成中 (epilogue-writing イベント、spec 11)。同じくローディング文言用。
  writingEpilogue: boolean;
  // マップ (spec 15) — 訪問済み+1歩先の有向グラフ。移動/遷移で backend が差し替える。
  map: MapView;
  // 自前の確認ダイアログ (WebView2 の window.confirm は tauri://localhost の URL を出すため自作)。
  // null なら非表示。askConfirm() がこれをセットし、ConfirmDialog が OK/キャンセルで解決する。
  confirmDialog: { message: string; confirmLabel: string } | null;
}

// 確認ダイアログの解決子 (Pinia state に関数を持たせず、モジュールローカルで保持)。
let confirmResolver: ((ok: boolean) => void) | null = null;

export const useGameStore = defineStore("game", {
  state: (): GameState => {
    const paths = loadPaths();
    return {
      started: false,
      title: "",
      log: [],
      state: null,
      loading: false,
      error: null,
      background: null,
      bgm: null,
      presentCharacters: [],
      bgBrightness: loadBgBrightness(),
      msgFont: loadMsgFont(),
      msgColor: loadMsgColor(),
      msgShadow: loadMsgShadow(),
      beatBgOpacity: loadBeatBgOpacity(),
      panelWidth: loadPanelWidth(),
      audioVolume: loadAudioVolume(),
      audioMuted: loadAudioMuted(),
      packagePath: paths[0] ?? BUILTIN_PACKAGES[0],
      activePackagePath: "",
      packagePaths: paths,
      lastPackageParent: loadLastPackageParent(),
      packages: [],
      siteUrl: loadSiteUrl(),
      remote: null,
      remoteLoading: false,
      remoteError: null,
      installingId: null,
      logDir: loadLogDir(),
      logToast: "",
      llmModel: "",
      updateAvailable: false,
      latestVersion: "",
      devMode: false,
      cacheWarned: false,
      synopsis: [],
      recentLog: [],
      compacting: false,
      writingEpilogue: false,
      map: { nodes: [], edges: [] },
      confirmDialog: null,
    };
  },

  getters: {
    // ゴール到達済みか (入力を締める判断に使う)。
    cleared: (s): boolean => s.state?.goal_reached ?? false,
    // 会話ペインに敷く背景スタイル (画像の上に暗幕を重ねて文字可読性を確保)。
    // 暗幕の濃さは bgBrightness で可変 (明るいほど薄い暗幕)。
    backgroundStyle: (s): Record<string, string> => {
      if (!s.background) return {};
      const base = Math.max(0, Math.min(1, (100 - s.bgBrightness) / 100));
      const top = (base * 0.9).toFixed(3);
      const bot = base.toFixed(3);
      return {
        backgroundImage: `linear-gradient(rgba(20,16,12,${top}), rgba(20,16,12,${bot})), url("${s.background}")`,
        backgroundSize: "cover",
        backgroundPosition: "center",
      };
    },
    // 実効音量 0..1 (BGM/SE 共通)。ミュート時は 0。<audio>.volume と new Audio に渡す。
    audioGain: (s): number => (s.audioMuted ? 0 : Math.max(0, Math.min(1, s.audioVolume / 100))),
    // 本文フォント (会話ログの container に inherit させる。空 = UI 既定のまま)。
    messageFontFamily: (s): string =>
      MESSAGE_FONTS.find((f) => f.id === s.msgFont)?.family ?? "",
    // 本文 (語り系要素) の色 + 影。inline style なので class (text-parchment 等) より優先される。
    narrationStyle: (s): Record<string, string> => {
      const style: Record<string, string> = {};
      if (s.msgColor) style.color = s.msgColor;
      if (s.msgShadow > 0) {
        const a = s.msgShadow / 100;
        // 二層の影: 輪郭 (下 1px) + にじみ (広め)。濃さはスライダーに比例。
        style.textShadow =
          `0 1px ${(1 + a * 5).toFixed(1)}px rgba(0,0,0,${(a * 0.95).toFixed(2)}), ` +
          `0 0 ${Math.round(a * 14)}px rgba(0,0,0,${(a * 0.6).toFixed(2)})`;
      }
      return style;
    },
    // ビート/想起ブロックに敷く黒の透過背景 (0 なら敷かない)。ember/glow の色付き文字が
    // 背景画像に溶ける読みにくさへの手当て。本文 (語り) はそのまま (narrationStyle の影が担当)。
    beatBgStyle: (s): Record<string, string> =>
      s.beatBgOpacity > 0
        ? { backgroundColor: `rgba(0,0,0,${(s.beatBgOpacity / 100).toFixed(2)})` }
        : {},
  },

  actions: {
    // 自前の確認ダイアログを開き、ユーザーの選択 (OK=true / キャンセル=false) を Promise で返す。
    // WebView2 の window.confirm は本文に tauri://localhost を混ぜてしまうので、これで置き換える。
    // 二重呼び出し (前の確認が未解決) は前をキャンセル扱いで畳んでから開く。
    askConfirm(message: string, confirmLabel?: string): Promise<boolean> {
      if (confirmResolver) {
        confirmResolver(false);
        confirmResolver = null;
      }
      this.confirmDialog = { message, confirmLabel: confirmLabel ?? t("confirm.ok") };
      return new Promise<boolean>((resolve) => {
        confirmResolver = resolve;
      });
    },
    // ConfirmDialog のボタンから呼ぶ。ダイアログを閉じて Promise を解決する。
    resolveConfirm(ok: boolean) {
      this.confirmDialog = null;
      confirmResolver?.(ok);
      confirmResolver = null;
    },

    // 開発者モードの現在値を backend (プロセス env) から取り直す (起動時)。
    async refreshDevMode() {
      try {
        this.devMode = await invoke<boolean>("get_dev_mode");
      } catch {
        /* Tauri 外では既定 false のまま */
      }
    },
    // 開発者モードを切り替える (env 即時反映 + app_data/.env 永続化)。次の play_turn から効く。
    async setDevMode(enabled: boolean) {
      await invoke("set_dev_mode", { enabled });
      this.devMode = enabled;
    },
    // 使用中の AI モデル名を backend から取り直す (起動時 + AIモデル設定の保存後)。
    // TitleBar のバッジと OS ウィンドウタイトル (タスクバー/Alt+Tab) の両方に反映する。
    async refreshLlmModel() {
      try {
        const cfg = await invoke<{ model: string }>("get_llm_config");
        this.llmModel = cfg.model ?? "";
      } catch {
        return; // Tauri 外 (ブラウザ) や backend 未接続では静かに諦める
      }
      try {
        const { getCurrentWindow } = await import("@tauri-apps/api/window");
        await getCurrentWindow().setTitle(
          this.llmModel
            ? t("store.windowTitleModel", { model: this.llmModel })
            : t("store.windowTitle"),
        );
      } catch {
        /* ウィンドウ API が無い環境ではバッジ表示のみ */
      }
    },

    // 配布サイトに新しいアプリがあるか確認する (起動時)。現在版 = ビルド時に埋めた git タグ
    // (__APP_VERSION__)。判定は backend (fetch_app_update) の純関数に委ね、結果だけ受け取る。
    // 自動更新はしない — 通知だけ (クリックでサイトを既定ブラウザで開く)。
    async checkAppUpdate() {
      try {
        const status = await invoke<{ update_available: boolean; latest_version: string }>(
          "fetch_app_update",
          { siteUrl: this.siteUrl, currentVersion: __APP_VERSION__ || "" },
        );
        this.updateAvailable = status.update_available;
        this.latestVersion = status.latest_version;
      } catch {
        // オフライン / 配布サイト未設定 / Tauri 外は静かに諦める (更新通知は非必須)。
        this.updateAvailable = false;
      }
    },

    // 「最新版があります」クリック: 配布サイトを既定ブラウザで開く (アプリ更新は手動)。
    async openUpdateSite() {
      try {
        await invoke("open_external_url", { url: this.siteUrl });
      } catch (e) {
        this.logToast = t("store.openSiteFailed", { error: String(e) });
      }
    },

    // 書庫のパッケージ詳細ページを既定ブラウザで開く (説明の全文・レビューはサイト側で読む)。
    // 開くのは常にユーザー登録の siteUrl 起点 — id はパス成分として encode し origin を変えられない。
    async openSitePackagePage(id: string) {
      try {
        await invoke("open_external_url", {
          url: `${this.siteUrl}/packages/${encodeURIComponent(id)}`,
        });
      } catch (e) {
        this.logToast = t("store.openSiteFailed", { error: String(e) });
      }
    },

    // 背景の明るさを設定 (即時反映 + localStorage 永続化)。グラフィック設定タブから呼ぶ。
    setBgBrightness(v: number) {
      this.bgBrightness = Math.max(0, Math.min(100, Math.round(v)));
      localStorage.setItem(BG_BRIGHTNESS_KEY, String(this.bgBrightness));
    },

    // 右ペインの幅を設定 (ドラッグ中に即時反映 + localStorage 永続化)。範囲でクランプ。
    setPanelWidth(px: number) {
      this.panelWidth = Math.max(PANEL_WIDTH_MIN, Math.min(PANEL_WIDTH_MAX, Math.round(px)));
      localStorage.setItem(PANEL_WIDTH_KEY, String(this.panelWidth));
    },

    // 本文フォントを設定 (即時反映 + localStorage 永続化)。表示設定タブから呼ぶ。
    setMsgFont(id: string) {
      this.msgFont = MESSAGE_FONTS.some((f) => f.id === id) ? id : "default";
      localStorage.setItem(MSG_FONT_KEY, this.msgFont);
    },
    // 本文の文字色を設定 (空 = テーマ既定へ戻す)。
    setMsgColor(hex: string) {
      this.msgColor = hex;
      if (hex) localStorage.setItem(MSG_COLOR_KEY, hex);
      else localStorage.removeItem(MSG_COLOR_KEY);
    },
    // 本文の影の濃さを設定 (0 = なし)。
    setMsgShadow(v: number) {
      this.msgShadow = Math.max(0, Math.min(100, Math.round(v)));
      localStorage.setItem(MSG_SHADOW_KEY, String(this.msgShadow));
    },
    // ビート/想起の黒背景の濃さを設定 (0 = なし)。表示設定タブから呼ぶ。
    setBeatBgOpacity(v: number) {
      this.beatBgOpacity = Math.max(0, Math.min(100, Math.round(v)));
      localStorage.setItem(BEAT_BG_KEY, String(this.beatBgOpacity));
    },

    // 音量を設定 (即時反映 + localStorage 永続化)。サウンド設定タブから呼ぶ。
    setAudioVolume(v: number) {
      this.audioVolume = Math.max(0, Math.min(100, Math.round(v)));
      localStorage.setItem(AUDIO_VOLUME_KEY, String(this.audioVolume));
    },
    // ミュート切替 (即時反映 + localStorage 永続化)。
    setAudioMuted(b: boolean) {
      this.audioMuted = b;
      localStorage.setItem(AUDIO_MUTED_KEY, String(b));
    },
    // SE を one-shot 再生する (発火ビート由来)。ミュート/音量 0 なら鳴らさない。
    // BGM はループ要素 (App.vue の <audio>) が担うので、ここは効果音だけ。
    playSe(url: string | null) {
      const gain = this.audioGain;
      if (!url || gain <= 0) return;
      try {
        const a = new Audio(url);
        a.volume = gain;
        void a.play().catch(() => {
          /* 自動再生制約・デコード失敗は握りつぶす (没入の付帯機能ゆえ致命でない) */
        });
      } catch {
        /* Audio 生成失敗も無視 */
      }
    },

    // localStorage のパス一覧から各 package.yaml の manifest を読み、一覧 view を更新する。
    async refreshPackages() {
      try {
        this.packages = await invoke<PackageEntry[]>("list_packages", {
          paths: this.packagePaths,
        });
        // 選択中パスが一覧から消えていたら先頭へ寄せる。
        if (!this.packagePaths.includes(this.packagePath) && this.packagePaths.length) {
          this.packagePath = this.packagePaths[0];
        }
      } catch (e) {
        this.error = String(e);
      }
    },

    // パッケージフォルダのパスを一覧に追加する (localStorage に永続化)。
    // 追加できたら「親フォルダ」を覚え、次回の参照ダイアログの初期位置にする。
    addPackage(path: string) {
      const p = path.trim();
      if (!p || this.packagePaths.includes(p)) return;
      this.packagePaths.push(p);
      savePaths(this.packagePaths);
      const parent = parentDir(p);
      if (parent) {
        this.lastPackageParent = parent;
        localStorage.setItem(LAST_PKG_PARENT_KEY, parent);
      }
      this.refreshPackages();
    },

    // OS ネイティブのフォルダ選択ダイアログでパッケージフォルダを選び、そのまま一覧へ追加する
    // (パッケージ一覧の「参照」ボタン)。初期ディレクトリは前回追加の親フォルダ。
    // 選択がキャンセルされたら何もしない。無効な (package.yaml の無い) フォルダを選んでも
    // 追加はされ、一覧に「読込失敗」で並ぶ (手入力パスと同じ扱い)。
    async browseAndAddPackage() {
      try {
        const picked = await invoke<string | null>("pick_package_folder", {
          start: this.lastPackageParent,
        });
        if (picked) this.addPackage(picked);
      } catch (e) {
        this.error = t("store.folderPickFailed", { error: String(e) });
      }
    },

    // パスを一覧から外す。
    async removePackage(path: string) {
      // セーブ (autosave + 手動スロット) は app_data/saves のファイルなので、一覧からパスを
      // 消すだけでは孤児として残り続ける。セーブがあるパッケージなら削除するか確認する
      // (キャンセル = セーブは残す = パスを再追加すれば「続きから」もスロットも復活する)。
      const entry = this.packages.find((p) => p.path === path);
      if (entry?.autosave_turn != null || entry?.has_slots) {
        const title = entry.title || path;
        const msg =
          entry.autosave_turn != null
            ? t("store.deleteSaveConfirm", { title, turn: entry.autosave_turn })
            : t("store.deleteSlotsConfirm", { title });
        if (await this.askConfirm(msg, t("store.deleteConfirmOk"))) {
          try {
            await invoke("delete_autosave", { packagePath: path });
          } catch (e) {
            this.logToast = t("store.deleteSaveFailed", { error: String(e) });
          }
        }
      }
      this.packagePaths = this.packagePaths.filter((p) => p !== path);
      savePaths(this.packagePaths);
      if (this.packagePath === path) this.packagePath = this.packagePaths[0] ?? "";
      this.refreshPackages();
    },

    // 書庫サイトの URL を設定する (localStorage 永続。空なら既定 = 公式へ戻す)。
    setSiteUrl(url: string) {
      const u = url.trim();
      this.siteUrl = u || DEFAULT_SITE_URL;
      localStorage.setItem(SITE_URL_KEY, this.siteUrl);
      // URL が変わったら前のサイトの一覧は無効。
      this.remote = null;
      this.remoteError = null;
    },

    // 書庫の一覧を取得する (無認証の公開 API。backend が HTTP を担い CORS を回避)。
    async fetchSitePackages(opts?: { page?: number; q?: string; category?: string; sort?: string }) {
      this.remoteLoading = true;
      this.remoteError = null;
      try {
        this.remote = await invoke<RemoteList>("fetch_site_packages", {
          siteUrl: this.siteUrl,
          page: opts?.page ?? 1,
          q: opts?.q ?? null,
          category: opts?.category ?? null,
          sort: opts?.sort ?? null,
        });
      } catch (e) {
        this.remote = null;
        this.remoteError = String(e);
      } finally {
        this.remoteLoading = false;
      }
    },

    // 書庫からパッケージを取得する: zip DL → クライアント側検証 (zip slip 遮断) → 展開 →
    // packagePaths へ登録。展開先は backend が app data dir に据える。成功なら登録先パスを返す。
    async installSitePackage(id: string): Promise<InstalledPackage | null> {
      if (this.installingId) return null; // 直列化 (多重 DL しない)
      this.installingId = id;
      this.remoteError = null;
      try {
        // spec 17 A-1: サーバ申告の sha256 を expected として渡す (DL 破損の一致検証 +
        // 出所メタの基準)。一覧に無ければ null (古い書庫 = 検証なしで従来どおり)。
        const expected = this.remote?.items.find((p) => p.id === id)?.sha256 ?? null;
        const installed = await invoke<InstalledPackage>("install_site_package", {
          siteUrl: this.siteUrl,
          id,
          sha256: expected,
        });
        this.addPackage(installed.path);
        return installed;
      } catch (e) {
        this.remoteError = String(e);
        return null;
      } finally {
        this.installingId = null;
      }
    },

    // --- 会話ログのテキスト保存 (ユーザーFB 2026-07-09) ---

    // ログ保存先フォルダを設定する (空 = 既定 app_data_dir/logs へ戻す)。
    setLogDir(path: string) {
      this.logDir = path.trim();
      if (this.logDir) localStorage.setItem(LOG_DIR_KEY, this.logDir);
      else localStorage.removeItem(LOG_DIR_KEY);
    },

    // 会話ログをプレーンテキストへ整形する (ConversationLog の見た目に沿う)。
    formatLog(): string {
      const lines: string[] = [];
      for (const e of this.log) {
        switch (e.kind) {
          case "opening":
            lines.push(e.text);
            break;
          case "player":
            lines.push(`> ${t("log.you")}: ${e.text}`);
            break;
          case "narration":
            lines.push(e.text);
            break;
          case "beat":
            if (e.narration.trim()) lines.push(`✦ ${e.narration}`);
            for (const r of e.recalled) lines.push(`  ┊ ${r}`);
            break;
          case "rolls":
            for (const r of e.rolls)
              lines.push(
                `🎲 1d${r.sides} = ${r.result} (DC ${r.dc}) → ${r.success ? t("log.success") : t("log.fail")}`,
              );
            break;
          case "checks":
            for (const c of e.checks) {
              // percentile (degree あり) はロールアンダー書式 (spec 16)。
              if (c.degree) {
                lines.push(
                  `🎯 ${t("log.checkLabel", { entity: c.entity, stat: c.stat })}: d100=${c.roll} ${c.success ? "≤" : ">"} ${c.dc} → ${degreeLabel(c.degree)}`,
                );
                if (c.narration) lines.push(c.narration);
                continue;
              }
              const mod = c.modifier >= 0 ? `+${c.modifier}` : `${c.modifier}`;
              lines.push(
                `🎯 ${t("log.checkLabel", { entity: c.entity, stat: c.stat })}: 1d${c.sides}(${c.roll})${mod} = ${c.total} (DC ${c.dc}) → ${c.success ? t("log.success") : t("log.fail")}`,
              );
              if (c.narration) lines.push(c.narration);
            }
            break;
          case "statrolls":
            for (const sr of e.stat_rolls) lines.push(`🎲 ${statRollLine(sr)}`);
            break;
          case "reject":
            lines.push(t("log.rejectHeader", { attempts: e.attempts }));
            for (const r of e.reasons) lines.push(`  - ${r}`);
            break;
          case "selfrepair":
            // ログ保存は畳まず全文 (診断情報を残す)。
            lines.push(t("log.selfrepairDone", { attempts: e.attempts }));
            if (e.reasons.length) {
              lines.push(t("log.rejectedAttempts"));
              e.reasons.forEach((rs, i) =>
                lines.push(`  ${t("log.selfrepairAttempt", { n: i + 1, reasons: rs.join(" / ") })}`),
              );
            }
            break;
          case "system":
            lines.push(`── ${e.text} ──`);
            break;
        }
        lines.push(""); // エントリ間に空行
      }
      return lines.join("\n");
    },

    // 現在のログを「日時_パッケージ名.txt」で保存する。backend がフォルダを解決・書き込む。
    async saveLog(): Promise<void> {
      if (!this.started || !this.log.length) {
        this.logToast = t("store.noLogToSave");
        return;
      }
      const now = new Date();
      const p = (n: number) => String(n).padStart(2, "0");
      const stamp =
        `${now.getFullYear()}${p(now.getMonth() + 1)}${p(now.getDate())}` +
        `_${p(now.getHours())}${p(now.getMinutes())}${p(now.getSeconds())}`;
      // パッケージ名をファイル名に使える形へ (パス特殊文字・空白を除去、長すぎ切り詰め)。
      const safeTitle =
        (this.title || "kataribe")
          .replace(/[\\/:*?"<>|]/g, "")
          .replace(/\s+/g, "_")
          .slice(0, 40) || "kataribe";
      const fileName = `${stamp}_${safeTitle}.txt`;
      const header = `# ${this.title || t("store.brandFallback")}\n# ${t("store.logHeaderDate")}: ${now.toLocaleString()}\n\n`;
      try {
        const path = await invoke<string>("save_log_file", {
          folder: this.logDir,
          fileName,
          content: header + this.formatLog(),
        });
        this.logToast = t("store.logSaved", { path });
      } catch (e) {
        this.logToast = t("store.saveFailed", { error: String(e) });
      }
    },

    // ログフォルダを OS のファイルマネージャで開く (設定ダイアログのボタン)。
    async openLogFolder() {
      try {
        await invoke("open_log_folder", { folder: this.logDir });
      } catch (e) {
        this.logToast = t("store.openFolderFailed", { error: String(e) });
      }
    },

    // パッケージ一覧を同梱既定に戻す (設定ダイアログから)。
    resetPackages() {
      this.packagePaths = [...BUILTIN_PACKAGES];
      savePaths(this.packagePaths);
      this.packagePath = this.packagePaths[0];
      this.refreshPackages();
    },

    // new_game / resume_game 共通の view 反映。resume なら再開マーカーと前回までの語りをログに出す。
    applyGameView(view: GameView, path: string) {
      this.started = true;
      this.packagePath = path;
      // このゲームが「プレイ中の真実」。以後コンボリストを別へ切り替えても動かない
      // (セーブはこのパスに対して有効。packagePath がこれと食い違えばセーブは無効化)。
      this.activePackagePath = path;
      this.title = view.title;
      this.state = view.state;
      this.background = assetUrl(view.background);
      this.bgm = assetUrl(view.bgm);
      this.presentCharacters = view.present_characters.map((c) => ({ ...c, icon: assetUrl(c.icon) }));
      this.map = view.map ?? { nodes: [], edges: [] };
      this.log = [{ kind: "opening", text: view.description }];
      this.cacheWarned = false; // 新しいセッション = 新しいクライアント (計測もゼロから)
      // あらすじ (spec 10): 新規開始は空、再開はセーブから全量復元。
      this.synopsis = view.synopsis ?? [];
      this.recentLog = view.recent_log ?? [];
      this.compacting = false;
      // scenario の lint (作者向け・非 fatal)。死んだ flag_hint 等を開幕で報せる。
      for (const w of view.warnings ?? []) {
        this.log.push({ kind: "system", text: `⚠ ${w}` });
      }
      if (view.resumed) {
        this.log.push({ kind: "system", text: t("store.resumeMarker", { turn: view.resumed.turn }) });
        if (view.resumed.last_narration) {
          this.log.push({ kind: "narration", text: view.resumed.last_narration });
        }
        for (const w of view.resumed.warnings) {
          this.log.push({ kind: "system", text: `⚠ ${w}` });
        }
      }
    },

    async newGame(packagePath?: string) {
      const path = packagePath ?? this.packagePath;
      if (!path) return;
      this.loading = true;
      this.error = null;
      try {
        // 言語設定タブの選択 (localStorage) を backend へ。却下理由の localize に効く。
        const lang = localStorage.getItem("kataribe.lang") || null;
        const view = await invoke<GameView>("new_game", { packagePath: path, lang });
        this.applyGameView(view, path);
      } catch (e) {
        this.error = String(e);
      } finally {
        this.loading = false;
      }
    },

    // オートセーブから再開 (spec 07 Phase C)。正本と語りの継続性は backend が復元する。
    async resumeGame(packagePath?: string) {
      const path = packagePath ?? this.packagePath;
      if (!path) return;
      this.loading = true;
      this.error = null;
      try {
        const lang = localStorage.getItem("kataribe.lang") || null;
        const view = await invoke<GameView>("resume_game", { packagePath: path, lang });
        this.applyGameView(view, path);
      } catch (e) {
        this.error = String(e);
      } finally {
        this.loading = false;
      }
    },

    // --- 手動セーブスロット (spec 07 Phase D) ---

    // スロット一覧を取得する。forSave=true はプレイ中 session のパッケージ (保存先の真実は
    // backend session が握る)、false はヘッダーで選択中のパッケージ (「続きから」と同じ意味論)。
    async listSlots(forSave: boolean): Promise<SlotView[]> {
      return await invoke<SlotView[]>("list_save_slots", {
        packagePath: forSave ? null : this.packagePath,
      });
    },

    // 現在のプレイ状態をスロットへ保存する (上書き確認はダイアログ側)。成功なら更新後の SlotView。
    async saveToSlot(slot: number): Promise<SlotView | null> {
      try {
        const v = await invoke<SlotView>("save_slot", { slot });
        this.logToast = t("store.slotSaved", { slot });
        // スロットが立った可能性があるので一覧の has_slots を取り直す (削除確認の材料)。
        this.refreshPackages();
        return v;
      } catch (e) {
        this.logToast = t("store.slotSaveFailed", { error: String(e) });
        return null;
      }
    },

    // スロットからロードして再開する。backend が GameSession を丸ごと差し替える =
    // プレイ中でも前のプレイは忘れられ、GM は次ターンからロードされた記憶だけを読み直す。
    async loadSlot(slot: number): Promise<boolean> {
      if (!this.packagePath) return false;
      this.loading = true;
      this.error = null;
      try {
        const lang = localStorage.getItem("kataribe.lang") || null;
        const view = await invoke<GameView>("load_slot", {
          packagePath: this.packagePath,
          slot,
          lang,
        });
        this.applyGameView(view, this.packagePath);
        return true;
      } catch (e) {
        this.error = String(e);
        return false;
      } finally {
        this.loading = false;
      }
    },

    async playTurn(action: string) {
      const trimmed = action.trim();
      if (!trimmed || this.loading || !this.started) return;
      this.log.push({ kind: "player", text: trimmed });
      this.loading = true;
      this.error = null;
      try {
        const turn = await invoke<TurnView>("play_turn", { action: trimmed });
        if (turn.accepted) {
          if (turn.narration) this.log.push({ kind: "narration", text: turn.narration });
          if (turn.rolls.length) this.log.push({ kind: "rolls", rolls: turn.rolls });
          if (turn.checks.length) {
            this.log.push({ kind: "checks", checks: turn.checks });
            // challenge の結末効果音を one-shot 再生 (受理ターンのみ。ビート SE と同経路)。
            for (const c of turn.checks) this.playSe(assetUrl(c.sound));
          }
          // 可変量ダイス (spec 16): 「SAN -4 (1d6=4)」の監査行。
          if (turn.stat_rolls.length) {
            this.log.push({ kind: "statrolls", stat_rolls: turn.stat_rolls });
          }
          for (const b of turn.beats) {
            // narration も recalled も無い「効果のみ」の発火はログに出さない (裸の ✦ を防ぐ)。
            // CG は turn.beats から、SE は下で別途処理するのでログに積まなくても失われない。
            if (b.narration.trim() || b.recalled.length) {
              this.log.push({ kind: "beat", narration: b.narration, recalled: b.recalled, expanded: false });
            }
            // 発火 SE を one-shot 再生 (受理ターンのみ。CG と同様、語りの瞬間に鳴らす)。
            this.playSe(assetUrl(b.sound));
          }
          if (turn.attempts > 1) {
            // 自己修復は既定で畳む (⚠ アイコンのみ) — メタ情報の没入低下を避ける。
            // クリックで「N 回目で筋を通した」+ 却下理由を展開 (author 診断)。
            this.log.push({ kind: "selfrepair", attempts: turn.attempts, reasons: turn.retries, expanded: false });
          }
          // goal 到達: 単発/終端なら goal_reached、campaign 継続なら transition で signal。
          if (turn.goal_reached || turn.transition) {
            // 結末ナレーション (authored) があれば語りとして出す (遷移元モジュールの結末)。
            if (turn.goal_narration) {
              this.log.push({ kind: "narration", text: turn.goal_narration });
            }
            // 表示は authored title を優先し、無ければ id (機械用セレクタ) へフォールバック。
            const goalLabel = turn.goal_title ?? turn.goal_id;
            if (turn.transition) {
              // campaign: この章の結末 → 次モジュールへ。入力は締めず続行。
              const end = goalLabel
                ? t("store.chapterEndNamed", { goal: goalLabel })
                : t("store.chapterEndGeneric");
              this.log.push({
                kind: "system",
                text: t("store.transitionTo", { end, module: turn.transition.module_title }),
              });
              // 遷移先モジュールの開幕描写。
              this.log.push({ kind: "opening", text: turn.transition.description });
            } else {
              // 単発シナリオ/キャンペーン終端 = クリア。
              const label = goalLabel
                ? t("store.clearedNamed", { goal: goalLabel })
                : t("store.clearedGeneric");
              this.log.push({ kind: "system", text: label });
            }
          }
          // エピローグ (spec 11)。表示順 = 結末文 → バナー → エピローグで幕
          // (バナーが余韻をぶった切らない)。narration と同じ本文スタイルで積む
          // = 会話ログのテキスト保存にも自然に含まれる。
          if (turn.epilogue) {
            this.log.push({ kind: "system", text: t("store.epilogueMarker") });
            this.log.push({ kind: "narration", text: turn.epilogue });
          }
        } else {
          this.log.push({ kind: "reject", reasons: turn.reasons, attempts: turn.attempts });
        }
        // キャッシュ健全性の警告 (#44/#45 — キャッシュの静かな漏出は usage が一次ソース)。
        // 連続 miss が閾値を越えた瞬間に 1 回だけ出す。ヒット復帰で再武装するエッジトリガー。
        // 初回リクエストは書き込みゆえ miss が正常 → total_requests>=2 で除外。
        const cs = turn.cache;
        if (cs.last_cache_read > 0) {
          this.cacheWarned = false;
        } else if (!this.cacheWarned && cs.total_requests >= 2 && cs.consecutive_misses >= 3) {
          this.cacheWarned = true;
          this.log.push({
            kind: "system",
            text: t("store.cacheWarning", { misses: cs.consecutive_misses }),
          });
        }
        // あらすじ (spec 10): 追記差分を push (append-only)。章が確定したら「最近の出来事」から
        // その章に呑まれた行 (turn <= upto_turn) を取り除く。会話ログには出さない
        // (物語の外の帳簿イベント — 更新はタブを見れば分かる、ユーザーFB 2026-07-14)。
        for (const line of turn.new_log ?? []) this.recentLog.push(line);
        for (const s of turn.new_synopsis ?? []) {
          this.synopsis.push(s);
          this.recentLog = this.recentLog.filter((l) => l.turn > s.upto_turn);
        }
        this.state = turn.state;
        this.presentCharacters = turn.present_characters.map((c) => ({ ...c, icon: assetUrl(c.icon) }));
        // マップ (spec 15) — 移動/遷移で backend が差し替える (却下でも現状スナップショット)。
        if (turn.map) this.map = turn.map;
        // 背景は受理ターンのみ更新する。却下 = 物語が進んでいないので現在の背景 (=直前の CG) を保つ。
        // イベント CG は瞬間 (spec 01 #3): 発火ターンに出て、次の受理ターンで場所背景へ復帰する。
        // campaign 遷移は前章の CG を持ち越さず遷移先の場所背景にする。
        if (turn.accepted) {
          const cgBeat = turn.transition
            ? undefined
            : [...turn.beats]
                .reverse()
                .find((b) => b.image && (b.image_mode ?? "background") === "background");
          this.background = cgBeat?.image ? assetUrl(cgBeat.image) : assetUrl(turn.background);
          // BGM は場所変化で差し替え。同一 URL なら再代入せずループを切らさない (CG と違い持続)。
          const nextBgm = assetUrl(turn.bgm);
          if (nextBgm !== this.bgm) this.bgm = nextBgm;
        }
      } catch (e) {
        this.error = String(e);
      } finally {
        this.loading = false;
        this.compacting = false; // 圧縮インジケータはターン完了で必ず解除
        this.writingEpilogue = false; // エピローグも同様
      }
    },
  },
});
