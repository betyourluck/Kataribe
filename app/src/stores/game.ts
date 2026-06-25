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
// 同梱パッケージ (初回起動時の既定一覧。repo root 相対)。
const BUILTIN_PACKAGES = ["packages/houkago", "packages/promise_demo", "packages/escape"];

// backend `list_packages` が返す1項目 (フォルダ一覧表示用)。
export interface PackageEntry {
  path: string;
  title: string;
  description: string;
  playable: boolean; // 単一シナリオ entry のみ今は playable (campaign-entry は後続)
  error: string | null;
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
  // 現在地に居る NPC (顔アイコン行)。icon は asset:// URL 化済み。
  presentCharacters: CharacterView[];
  // 背景の明るさ 0..100 (大きいほど画像が明るく見える=暗幕が薄い)。グラフィック設定。
  bgBrightness: number;
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
      presentCharacters: [],
      bgBrightness: loadBgBrightness(),
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
  },

  actions: {
    // 背景の明るさを設定 (即時反映 + localStorage 永続化)。グラフィック設定タブから呼ぶ。
    setBgBrightness(v: number) {
      this.bgBrightness = Math.max(0, Math.min(100, Math.round(v)));
      localStorage.setItem(BG_BRIGHTNESS_KEY, String(this.bgBrightness));
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

    async newGame(packagePath?: string) {
      const path = packagePath ?? this.packagePath;
      if (!path) return;
      this.loading = true;
      this.error = null;
      try {
        // 言語設定タブの選択 (localStorage) を backend へ。却下理由の localize に効く。
        const lang = localStorage.getItem("kataribe.lang") || null;
        const view = await invoke<GameView>("new_game", { packagePath: path, lang });
        this.started = true;
        this.packagePath = path;
        this.title = view.title;
        this.state = view.state;
        this.background = assetUrl(view.background);
        this.presentCharacters = view.present_characters.map((c) => ({ ...c, icon: assetUrl(c.icon) }));
        this.log = [{ kind: "opening", text: view.description }];
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
          }
          if (turn.attempts > 1) {
            this.log.push({ kind: "system", text: `GM は ${turn.attempts} 回目の提案で筋を通した` });
          }
          if (turn.goal_reached) {
            // 結末ナレーション (authored) があれば語りとして出す。
            if (turn.goal_narration) {
              this.log.push({ kind: "narration", text: turn.goal_narration });
            }
            // どの goal に達したか (複数 goal の識別)。
            const label = turn.goal_id ? `🎉 結末「${turn.goal_id}」に到達した。` : "🎉 クリア。goal に到達した。";
            this.log.push({ kind: "system", text: label });
          }
        } else {
          this.log.push({ kind: "reject", reasons: turn.reasons, attempts: turn.attempts });
        }
        this.state = turn.state;
        // 背景は location 変化で差し替え (Phase 2 でイベント CG も同経路)。
        this.background = assetUrl(turn.background);
        this.presentCharacters = turn.present_characters.map((c) => ({ ...c, icon: assetUrl(c.icon) }));
      } catch (e) {
        this.error = String(e);
      } finally {
        this.loading = false;
      }
    },
  },
});
