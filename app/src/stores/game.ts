import { defineStore } from "pinia";
import { invoke } from "@tauri-apps/api/core";
import type { GameView, TurnView, StateView, LogEntry } from "../types/api";

// localStorage キー: ユーザーが選べるパッケージフォルダのパス一覧 (配布物の置き場)。
const PACKAGES_KEY = "kataribe.packagePaths";
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
      packagePath: paths[0] ?? BUILTIN_PACKAGES[0],
      packagePaths: paths,
      packages: [],
    };
  },

  getters: {
    // ゴール到達済みか (入力を締める判断に使う)。
    cleared: (s): boolean => s.state?.goal_reached ?? false,
  },

  actions: {
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

    async newGame(packagePath?: string) {
      const path = packagePath ?? this.packagePath;
      if (!path) return;
      this.loading = true;
      this.error = null;
      try {
        const view = await invoke<GameView>("new_game", { packagePath: path });
        this.started = true;
        this.packagePath = path;
        this.title = view.title;
        this.state = view.state;
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
            this.log.push({ kind: "system", text: "🎉 クリア。goal に到達した。" });
          }
        } else {
          this.log.push({ kind: "reject", reasons: turn.reasons, attempts: turn.attempts });
        }
        this.state = turn.state;
      } catch (e) {
        this.error = String(e);
      } finally {
        this.loading = false;
      }
    },
  },
});
