import { defineStore } from "pinia";
import { invoke } from "@tauri-apps/api/core";
import type { GameView, TurnView, StateView, LogEntry } from "../types/api";

// 選択できるシナリオ (scenarios/ の相対パス)。
export const SCENARIOS = [
  { path: "scenarios/locked_room.yaml", label: "密室脱出" },
  { path: "scenarios/strength_trial.yaml", label: "力の試練" },
  { path: "scenarios/heroine_route.yaml", label: "邂逅 (好感度)" },
  { path: "scenarios/trigger_recall_demo.yaml", label: "約束の想起 (反応ビート)" },
];

interface GameState {
  started: boolean;
  title: string;
  log: LogEntry[];
  state: StateView | null;
  loading: boolean;
  error: string | null;
  scenarioPath: string;
}

export const useGameStore = defineStore("game", {
  state: (): GameState => ({
    started: false,
    title: "",
    log: [],
    state: null,
    loading: false,
    error: null,
    scenarioPath: SCENARIOS[0].path,
  }),

  getters: {
    // ゴール到達済みか (入力を締める判断に使う)。
    cleared: (s): boolean => s.state?.goal_reached ?? false,
  },

  actions: {
    async newGame(scenarioPath?: string) {
      const path = scenarioPath ?? this.scenarioPath;
      this.loading = true;
      this.error = null;
      try {
        const view = await invoke<GameView>("new_game", { scenarioPath: path });
        this.started = true;
        this.scenarioPath = path;
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
