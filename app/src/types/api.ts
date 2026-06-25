// backend (app/src-tauri/src/lib.rs) の view DTO のミラー。
// 状態の真実は backend が握る。これは描画用スナップショットの型。

export interface StatView {
  key: string;
  value: number;
}

/** 文字列属性 (クラス/職業/種族 等)。可変・トリガーで書き換わる。 */
export interface StatStrView {
  key: string;
  value: string;
}

export interface EntityView {
  id: string;
  stats: StatView[];
  skills: string[];
  items: string[];
  attributes: StatStrView[];
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

export interface CheckView {
  entity: string;
  stat: string;
  sides: number;
  roll: number;
  modifier: number;
  total: number;
  dc: number;
  success: boolean;
  /** authored challenge の結末ナレーション (毎回・同ターン)。無ければ空。 */
  narration: string;
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
  /** 現在地の背景画像の絶対パス (convertFileSrc で URL 化する)。無ければ null。 */
  background: string | null;
}

export interface TurnView {
  accepted: boolean;
  narration: string;
  rolls: RollView[];
  checks: CheckView[];
  beats: BeatView[];
  attempts: number;
  reasons: string[];
  state: StateView;
  goal_reached: boolean;
  /** 到達した名前付き goal の id (複数 goal のどれに達したか)。単一 goal/未到達なら null。 */
  goal_id: string | null;
  /** 到達 goal の結末ナレーション (authored)。空/未到達なら null。 */
  goal_narration: string | null;
  /** 現在地の背景画像の絶対パス (convertFileSrc で URL 化する)。無ければ null。 */
  background: string | null;
}

// 会話ログの 1 エントリ (frontend ローカルの描画モデル)。
export type LogEntry =
  | { kind: "opening"; text: string }
  | { kind: "player"; text: string }
  | { kind: "narration"; text: string }
  | { kind: "beat"; narration: string; recalled: string[] }
  | { kind: "rolls"; rolls: RollView[] }
  | { kind: "checks"; checks: CheckView[] }
  | { kind: "reject"; reasons: string[]; attempts: number }
  | { kind: "system"; text: string };
