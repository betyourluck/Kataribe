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
  /** d100 ロールアンダー判定の成功度 (spec 16)。critical/extreme/hard/regular/failure/fumble。加算式は null。 */
  degree: string | null;
}

/** 可変量ダイス (roll_stat) の監査記録 (spec 16)。「SAN -4 (1d6=4)」の素材。 */
export interface StatRollView {
  entity: string;
  key: string;
  count: number;
  sides: number;
  bonus: number;
  rolls: number[];
  amount: number;
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
  /** scenario の lint (非 fatal な作者向け警告。死んだ flag_hint 等)。開幕ログに ⚠ で出す。 */
  warnings: string[];
  /** あらすじ全量 (spec 10)。新規開始は空、再開はセーブから復元。以後 TurnView の差分で伸びる。 */
  synopsis: SynopsisView[];
  /** 「最近の出来事」= 未圧縮 chronicle の 1 行要約列 (再開時の初期表示)。 */
  recent_log: LogLineView[];
  /** マップ (spec 15) — 訪問済み+1歩先の有向グラフ。 */
  map: MapView;
}

/** あらすじ 1 章 (spec 10)。一度確定したら不変 (append-only)。リスト key は upto_turn。 */
export interface SynopsisView {
  /** この章が覆う最終ターン (識別子・リスト key)。 */
  upto_turn: number;
  /** 章題 (表示専用 — モジュール title or「ターン m〜n」)。 */
  title: string;
  /** 圧縮された物語 (400 字以内)。 */
  text: string;
}

/** 「最近の出来事」の 1 行 (未圧縮 chronicle の要約)。 */
export interface LogLineView {
  turn: number;
  summary: string;
}

/** マップの 1 ノード (spec 15)。visited=false は frontier (未踏)。
 *  frontier は title/description/image を伏せる (「？」表示・ネタバレ回避)。 */
export interface MapNode {
  id: string;
  /** 表示名 (Location.title、空なら id へフォールバック)。frontier は空。 */
  title: string;
  /** 場所の説明 (クリックで詳細に出す)。frontier は空。 */
  description: string;
  /** 場所の画像の絶対パス (frontend が convertFileSrc で URL 化)。無ければ null。 */
  image: string | null;
  current: boolean;
  visited: boolean;
}

/** マップの 1 辺 (有向の出口)。locked=true は gate 未達 (🔒・今は通れない)。 */
export interface MapEdge {
  from: string;
  to: string;
  locked: boolean;
}

/** 右ペインのマップ view (spec 15) — 訪問済み+1歩先の有向グラフ。 */
export interface MapView {
  nodes: MapNode[];
  edges: MapEdge[];
}

/** 手動セーブスロット一覧の 1 項目 (spec 07 Phase D)。exists=false は空きスロット。 */
export interface SlotView {
  /** スロット番号 (1..=5)。 */
  slot: number;
  exists: boolean;
  /** セーブ時点のターン数 (空きなら 0)。 */
  turn: number;
  /** 保存日時 (epoch ms)。locale 表示は frontend の責務。無ければ null。 */
  saved_at_ms: number | null;
  /** 直前の語りの冒頭 (シーン識別の手がかり)。 */
  snippet: string;
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
  /** 累積キャッシュ読取トークン (spec 14)。セッション累積 hit rate = hit_tokens / total_tokens。 */
  hit_tokens: number;
  /** 累積入力トークン (spec 14)。 */
  total_tokens: number;
  /** 直近 N 件の per-request 計測点 (spec 14、有界リングバッファ。曲線メーターは Phase E)。 */
  recent: { cached: number; prompt: number }[];
}

export interface TurnView {
  accepted: boolean;
  narration: string;
  rolls: RollView[];
  checks: CheckView[];
  /** 可変量ダイス (roll_stat) の監査記録 (spec 16)。 */
  stat_rolls: StatRollView[];
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
  /** このターンで確定したあらすじ章の追記差分 (append-only ゆえ push するだけ)。 */
  new_synopsis: SynopsisView[];
  /** このターンで chronicle に積まれた行の差分 (「最近の出来事」用。却下ターンは空)。 */
  new_log: LogLineView[];
  /** エピローグ本文 (spec 11)。到達 goal に epilogue_prompt があり終端のときだけ非 null。
   *  生成失敗時は null (結末文 + バナーの従来表示へフォールバック)。 */
  epilogue: string | null;
  /** マップ (spec 15) — 訪問済み+1歩先の有向グラフ (移動/遷移で変わるので毎ターン)。 */
  map: MapView;
}

// 会話ログの 1 エントリ (frontend ローカルの描画モデル)。
export type LogEntry =
  | { kind: "opening"; text: string }
  | { kind: "player"; text: string }
  | { kind: "narration"; text: string }
  // narration は authored な物語ビート (常時表示)。recalled (memoria/伏線) は次ターンに GM が
  // 語りへ織り込むので既定で畳む (expanded で展開)。
  | { kind: "beat"; narration: string; recalled: string[]; expanded?: boolean }
  | { kind: "rolls"; rolls: RollView[] }
  | { kind: "checks"; checks: CheckView[] }
  | { kind: "statrolls"; stat_rolls: StatRollView[] }
  | { kind: "reject"; reasons: string[]; attempts: number }
  // 自己修復 (GM が筋を通すまでの試行) — 既定で畳み、⚠ アイコンのみ表示。expanded で展開。
  | { kind: "selfrepair"; attempts: number; reasons: string[][]; expanded?: boolean }
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
  /** 作者が納本時に自己申告する対応 Kataribe バージョン (例 "v0.2.0")。未申告なら null。 */
  kataribe_version: string | null;
  /** 配布物 (正規化済み zip) の sha256 (spec 17)。未対応の書庫は null/欠落。 */
  sha256?: string | null;
  /** 配布物の差し替え日時 (ISO8601)。更新バッジの表示に使う人間値。 */
  file_updated_at?: string | null;
}

/** 更新あり 1 件 (spec 17 機構③。judgement は hash の相違のみ)。 */
export interface PackageUpdate {
  /** packagePaths のパス (ローカル一覧の行と突き合わせるキー)。 */
  path: string;
  id: string;
  /** サイト側の差し替え日時 (ISO8601)。 */
  file_updated_at: string | null;
  /** 手元の版 (取得時の写し)。欠落は null → 表示は「(不明)」。 */
  local_version: string | null;
  /** 手元の取得時刻 (unix 秒)。 */
  installed_at_unix: number;
}

/** 更新完了の報告 (トースト素材)。版は表示のみで判定には使わない。 */
export interface UpdateResult {
  title: string;
  from_version: string | null;
  to_version: string | null;
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
