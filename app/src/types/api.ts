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
  /** 設定・背景・性向 (authored の語り素材)。プロフィールダイアログの本文。無ければ空。 */
  profile: string;
}

/** フラグ一覧の 1 エントリ。title は表示名 (空なら key へフォールバック)、
 *  turn/cause は「いつ・何をして立ったか」(flag_turns × chronicle の join。無ければ null)。 */
export interface FlagView {
  key: string;
  title: string;
  turn: number | null;
  cause: string | null;
}

export interface StateView {
  turn: number;
  /** 現在地の LocationId (機械用セレクタ)。 */
  location: string;
  /** 現在地の表示名 (Location.title)。空なら id へフォールバックして表示する。 */
  location_title: string;
  inventory: string[];
  flags: FlagView[];
  entities: EntityView[];
  goal_reached: boolean;
  /** 名前付き goal (目標) の一覧 (authored 順)。単一 goal のシナリオでは空。 */
  goals: GoalView[];
  /** 到達した goal の id (一覧のハイライト用)。未到達なら null。 */
  reached_goal: string | null;
}

/** 目標一覧の 1 エントリ。title は人間向け表示名 (空なら id へフォールバック)、
 *  hint は「何をすればだいたい行けるか」の authored 道しるべ (空なら無し)。 */
export interface GoalView {
  id: string;
  title: string;
  hint: string;
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
  /** authored challenge の結末効果音の絶対パス (convertFileSrc で URL 化 → one-shot 再生)。無ければ null。 */
  sound: string | null;
}

export interface BeatView {
  narration: string;
  recalled: string[];
  /** 発火時のイベント CG の絶対パス (convertFileSrc で URL 化する)。無ければ null。 */
  image: string | null;
  /** イベント CG の表示モード ("background" | "overlay")。未指定なら null (=background 扱い)。 */
  image_mode: string | null;
  /** 発火時の SE の絶対パス (convertFileSrc で URL 化 → one-shot 再生)。無ければ null。 */
  sound: string | null;
}

/** 顔アイコン行の 1 キャラ。icon は backend 解決済みの絶対パス (store で asset URL 化)。 */
export interface CharacterView {
  id: string;
  name: string;
  icon: string | null;
}

export interface GameView {
  title: string;
  location: string;
  description: string;
  state: StateView;
  /** 現在地の背景画像の絶対パス (convertFileSrc で URL 化する)。無ければ null。 */
  background: string | null;
  /** 現在地のループ BGM の絶対パス (convertFileSrc で URL 化 → <audio loop>)。無ければ null。 */
  bgm: string | null;
  /** 現在地に居る NPC (顔アイコン行)。 */
  present_characters: CharacterView[];
  /** オートセーブから再開したときの再開情報 (spec 07 Phase C)。新規開始なら null。 */
  resumed: ResumeView | null;
}

/** セーブから再開したとき開幕ログへ出す情報。 */
export interface ResumeView {
  /** 再開時点のターン数。 */
  turn: number;
  /** 前回までの語り (「前回のあらすじ」としてログに出す)。 */
  last_narration: string;
  /** 版不一致などの警告 (拒否はしない)。 */
  warnings: string[];
}

/** campaign のモジュール遷移 (前モジュールの結末 → 次モジュールへ state を糸通しして差し替え)。 */
export interface TransitionView {
  module_title: string;
  location: string;
  description: string;
}

/** プロンプトキャッシュの健全性 (セッション累計)。連続 miss の検知で「キャッシュ経路が
 *  壊れているかも」を警告する材料 (#44/#45 — 漏出は usage が一次ソース)。 */
export interface CacheStatView {
  /** 直近リクエストの cache read トークン (0 = miss)。 */
  last_cache_read: number;
  /** 連続で cache read が 0 だった回数 (1 回でもヒットで 0 にリセット)。 */
  consecutive_misses: number;
  /** 累計リクエスト数。初回は書き込みゆえ miss が正常なので判定は 2 回目以降を見る。 */
  total_requests: number;
}

export interface TurnView {
  accepted: boolean;
  narration: string;
  rolls: RollView[];
  checks: CheckView[];
  beats: BeatView[];
  attempts: number;
  reasons: string[];
  /** 受理までに却下された各試行の理由 (試行順・localize 済み)。空なら一発合格。 */
  retries: string[][];
  state: StateView;
  goal_reached: boolean;
  /** 到達した名前付き goal の id (複数 goal のどれに達したか)。単一 goal/未到達なら null。 */
  goal_id: string | null;
  /** 到達 goal の表示名 (authored title)。空/未到達なら null (表示は id へフォールバック)。 */
  goal_title: string | null;
  /** 到達 goal の結末ナレーション (authored)。空/未到達なら null。 */
  goal_narration: string | null;
  /** 現在地の背景画像の絶対パス (convertFileSrc で URL 化する)。無ければ null。 */
  background: string | null;
  /** 現在地のループ BGM の絶対パス (convertFileSrc で URL 化 → <audio loop>)。無ければ null。 */
  bgm: string | null;
  /** 現在地に居る NPC (顔アイコン行)。 */
  present_characters: CharacterView[];
  /** campaign で次モジュールへ遷移したときの遷移先開幕情報。単発/未遷移なら null。
   *  このとき state/background/present_characters は**遷移先**を指す (goal_* は遷移元の結末)。 */
  transition: TransitionView | null;
  /** プロンプトキャッシュの健全性 (このセッションの累計)。 */
  cache: CacheStatView;
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
  | { kind: "retries"; reasons: string[][] }
  | { kind: "system"; text: string };

// ============================================================================
// 配布サイト「Kataribe 書庫」統合 (spec 05 Phase C)
// ============================================================================

/** 書庫 API `/api/packages` の一覧 1 項目 (backend RemotePackage のミラー)。 */
export interface RemotePackage {
  id: string;
  title: string;
  description: string;
  category: string;
  /** 性・流血描写の自己申告。倫理制約の強い LLM ではプレイできない可能性の目印。 */
  is_mature: boolean;
  file_size: number;
  uploader_display_name: string;
  download_count: number;
  avg_rating: number | null;
  review_count: number;
}

/** 書庫の一覧応答 (items + ページネーション)。 */
export interface RemoteList {
  items: RemotePackage[];
  total: number;
  page: number;
  page_size: number;
}

/** 取得結果 (packagePaths へ登録する絶対パス + 表示用 title)。 */
export interface InstalledPackage {
  path: string;
  title: string;
}
