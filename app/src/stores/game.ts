import { defineStore } from "pinia";
import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import type { GameView, TurnView, StateView, LogEntry, CharacterView } from "../types/api";

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
// 背景の明るさ (0=暗幕最大で真っ暗 〜 100=暗幕なしで画像そのまま)。既定はやや明るめ。
const BG_BRIGHTNESS_KEY = "kataribe.bgBrightness";
function loadBgBrightness(): number {
  const v = Number(localStorage.getItem(BG_BRIGHTNESS_KEY));
  return Number.isFinite(v) && v >= 0 && v <= 100 ? v : 35;
}
// 音量 0..100 (BGM ループと SE one-shot に共通でかかる)。既定は控えめ。
const AUDIO_VOLUME_KEY = "kataribe.audioVolume";
const AUDIO_MUTED_KEY = "kataribe.audioMuted";
function loadAudioVolume(): number {
  const v = Number(localStorage.getItem(AUDIO_VOLUME_KEY));
  return Number.isFinite(v) && v >= 0 && v <= 100 ? v : 60;
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

// 同梱パッケージ (初回起動時の既定一覧。repo root 相対)。
const BUILTIN_PACKAGES = ["packages/houkago", "packages/promise_demo", "packages/escape"];

// backend `list_packages` が返す1項目 (フォルダ一覧表示用)。
export interface PackageEntry {
  path: string;
  title: string;
  description: string;
  playable: boolean; // manifest が読めれば true (単発・campaign-entry 双方)。読込エラー時のみ false
  error: string | null;
  // オートセーブが在ればその時点のターン数 (「続きから (turn N)」ボタンの提示素)。無ければ null。
  autosave_turn: number | null;
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
  // 音量 0..100 (BGM/SE 共通)。サウンド設定。
  audioVolume: number;
  // ミュート (true なら音を出さない)。サウンド設定。
  audioMuted: boolean;
  // 選択中パッケージのパス。
  packagePath: string;
  // localStorage が保持するパッケージフォルダのパス一覧。
  packagePaths: string[];
  // 各パスの manifest を読んだ一覧 view (backend list_packages の結果)。
  packages: PackageEntry[];
}

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
      audioVolume: loadAudioVolume(),
      audioMuted: loadAudioMuted(),
      packagePath: paths[0] ?? BUILTIN_PACKAGES[0],
      packagePaths: paths,
      packages: [],
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
  },

  actions: {
    // 背景の明るさを設定 (即時反映 + localStorage 永続化)。グラフィック設定タブから呼ぶ。
    setBgBrightness(v: number) {
      this.bgBrightness = Math.max(0, Math.min(100, Math.round(v)));
      localStorage.setItem(BG_BRIGHTNESS_KEY, String(this.bgBrightness));
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
    addPackage(path: string) {
      const p = path.trim();
      if (!p || this.packagePaths.includes(p)) return;
      this.packagePaths.push(p);
      savePaths(this.packagePaths);
      this.refreshPackages();
    },

    // パスを一覧から外す。
    removePackage(path: string) {
      this.packagePaths = this.packagePaths.filter((p) => p !== path);
      savePaths(this.packagePaths);
      if (this.packagePath === path) this.packagePath = this.packagePaths[0] ?? "";
      this.refreshPackages();
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
      this.title = view.title;
      this.state = view.state;
      this.background = assetUrl(view.background);
      this.bgm = assetUrl(view.bgm);
      this.presentCharacters = view.present_characters.map((c) => ({ ...c, icon: assetUrl(c.icon) }));
      this.log = [{ kind: "opening", text: view.description }];
      if (view.resumed) {
        this.log.push({ kind: "system", text: `── 続きから (turn ${view.resumed.turn}) ──` });
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
          if (turn.checks.length) this.log.push({ kind: "checks", checks: turn.checks });
          for (const b of turn.beats) {
            this.log.push({ kind: "beat", narration: b.narration, recalled: b.recalled });
            // 発火 SE を one-shot 再生 (受理ターンのみ。CG と同様、語りの瞬間に鳴らす)。
            this.playSe(assetUrl(b.sound));
          }
          if (turn.attempts > 1) {
            this.log.push({ kind: "system", text: `GM は ${turn.attempts} 回目の提案で筋を通した` });
            // なぜ却下されたか (試行ごとの理由) を author に見せる。
            if (turn.retries.length) this.log.push({ kind: "retries", reasons: turn.retries });
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
              const end = goalLabel ? `結末「${goalLabel}」` : "この章の結末";
              this.log.push({
                kind: "system",
                text: `${end}に到達。次の章『${turn.transition.module_title}』へ。`,
              });
              // 遷移先モジュールの開幕描写。
              this.log.push({ kind: "opening", text: turn.transition.description });
            } else {
              // 単発シナリオ/キャンペーン終端 = クリア。
              const label = goalLabel ? `🎉 結末「${goalLabel}」に到達した。` : "🎉 クリア。goal に到達した。";
              this.log.push({ kind: "system", text: label });
            }
          }
        } else {
          this.log.push({ kind: "reject", reasons: turn.reasons, attempts: turn.attempts });
        }
        this.state = turn.state;
        this.presentCharacters = turn.present_characters.map((c) => ({ ...c, icon: assetUrl(c.icon) }));
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
      }
    },
  },
});
