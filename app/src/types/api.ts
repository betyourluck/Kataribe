// backend (app/src-tauri/src/lib.rs) の view DTO のミラー。
// 状態の真実は backend が握る。これは描画用スナップショットの型。

export interface StatView {
  key: string;
  value: number;
}

export interface EntityView {
  id: string;
  stats: StatView[];
}

export interface StateView {
  turn: number;
  location: string;
  inventory: string[];
  flags: string[];
  entities: EntityView[];
  goal_reached: boolean;
}

export interface RollView {
  sides: number;
  dc: number;
  result: number;
  success: boolean;
}

export interface BeatView {
  narration: string;
  recalled: string[];
}

export interface GameView {
  title: string;
  location: string;
  description: string;
  state: StateView;
}

export interface TurnView {
  accepted: boolean;
  narration: string;
  rolls: RollView[];
  beats: BeatView[];
  attempts: number;
  reasons: string[];
  state: StateView;
  goal_reached: boolean;
}

// 会話ログの 1 エントリ (frontend ローカルの描画モデル)。
export type LogEntry =
  | { kind: "opening"; text: string }
  | { kind: "player"; text: string }
  | { kind: "narration"; text: string }
  | { kind: "beat"; narration: string; recalled: string[] }
  | { kind: "rolls"; rolls: RollView[] }
  | { kind: "reject"; reasons: string[]; attempts: number }
  | { kind: "system"; text: string };
