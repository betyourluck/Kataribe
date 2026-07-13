//! シナリオ脊椎 (拘束)。beat/場所のグラフと gate 条件で、即興が筋から外れすぎないよう縛る。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::state::{
    default_entity, AttrKey, ChallengeId, EntityId, FlagKey, GameState, GoalId, ItemId, LocationId,
    RngState, SkillId, StateOp, StatKey, TriggerId, DEFAULT_GOAL, PLAYER,
};

/// state に対して評価される条件。
///
/// 内部タグ方式 (`kind` フィールド)。serde_yaml 0.9 で素直なマップとして書ける:
/// `{ kind: has_item, item: rusty_key }` / `{ kind: flag_is, key: ..., value: true }`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Gate {
    /// 常に通る。
    Always,
    /// 指定キャラが指定アイテムを所持している。`entity` 省略時は主人公。
    HasItem {
        #[serde(default = "default_entity")]
        entity: EntityId,
        item: ItemId,
    },
    /// 指定フラグが指定値である。
    FlagIs { key: FlagKey, value: bool },
    /// 指定の場所にいる。
    LocationIs { at: LocationId },
    /// 指定キャラの stat が value 以上である (数値条件)。未設定は 0 扱い。`entity` 省略時は主人公。
    StatAtLeast {
        #[serde(default = "default_entity")]
        entity: EntityId,
        key: StatKey,
        value: i64,
    },
    /// 指定キャラの stat が value 以下である ([`Gate::StatAtLeast`] の双対)。未設定は 0 扱い。
    /// hp は 0 クランプなので `stat_at_most hp 0` が「気絶/死」を表せる (HP0 End を goal に書く経路)。
    /// `entity` 省略時は主人公。
    StatAtMost {
        #[serde(default = "default_entity")]
        entity: EntityId,
        key: StatKey,
        value: i64,
    },
    /// 指定キャラが能力を獲得済みである (能力条件)。閉世界: 宣言/開花した能力のみ true。`entity` 省略時は主人公。
    HasSkill {
        #[serde(default = "default_entity")]
        entity: EntityId,
        skill: SkillId,
    },
    /// 指定キャラの文字列属性が value と一致する (クラス/職業条件)。未設定は空文字扱い。
    /// 「魔法剣士なら〜」のように転職後の状態を縛れる。`entity` 省略時は主人公。
    AttributeIs {
        #[serde(default = "default_entity")]
        entity: EntityId,
        key: AttrKey,
        value: String,
    },
    /// 指定キャラの stat に刻まれたターンから **`turns` ターン以上経過**している
    /// (`現在turn - stat >= turns`)。[`StateOp::RecordTurn`](crate::StateOp::RecordTurn) と対で
    /// 「〇〇から N ターン後に発火」を組む。stat 未設定 (=0) だと turn>=turns で誤発火しうるので、
    /// 「〇〇が起きた」フラグと `all` で束ねるのが定石。`entity` 省略時は主人公。
    TurnsSince {
        #[serde(default = "default_entity")]
        entity: EntityId,
        key: StatKey,
        turns: u32,
    },
    /// 指定キャラの票が**現在の票箱に入っている** (spec 06 / #38)。`entity` 省略時は主人公。
    /// 「プレイヤーが投票したら開票」をイベント駆動で書く述語 — `resolve_vote` が票を
    /// リセットするので開票後は自然に偽へ戻り、repeatable トリガーは次サイクルで再武装する。
    /// タイマー (`turns_since`) と `any` で束ねれば「票が入るか N ターンで強制開票」も書ける。
    HasVoted {
        #[serde(default = "default_entity")]
        entity: EntityId,
    },
    /// すべての子条件が通る (AND)。
    All { of: Vec<Gate> },
    /// いずれかの子条件が通る (OR)。
    Any { of: Vec<Gate> },
}

impl Gate {
    pub fn eval(&self, s: &GameState) -> bool {
        match self {
            Gate::Always => true,
            Gate::HasItem { entity, item } => s.has_item(entity, item),
            Gate::FlagIs { key, value } => s.flag(key) == *value,
            Gate::LocationIs { at } => &s.location == at,
            Gate::StatAtLeast { entity, key, value } => s.stat_of(entity, key) >= *value,
            Gate::StatAtMost { entity, key, value } => s.stat_of(entity, key) <= *value,
            Gate::HasSkill { entity, skill } => s.has_skill(entity, skill),
            Gate::AttributeIs { entity, key, value } => s.attribute_of(entity, key) == value,
            Gate::TurnsSince { entity, key, turns } => {
                // 現在ターン - 刻まれたターン >= turns。turn は u32 だが減算は i64 で行う
                // (stat は i64・未設定 0)。記録前は turn - 0 = turn なので flag と束ねて守る。
                i64::from(s.turn) - s.stat_of(entity, key) >= i64::from(*turns)
            }
            Gate::HasVoted { entity } => s.votes.contains_key(entity),
            Gate::All { of } => of.iter().all(|g| g.eval(s)),
            Gate::Any { of } => of.iter().any(|g| g.eval(s)),
        }
    }

    /// この gate が `s` で **false のとき、満たされていない葉条件**を列挙する
    /// (却下理由の診断用: `All` の中でどの条件が false かを名指しする — 「バグか本当に
    /// 未達か」を作者/LLM が切り分けられる)。真なら空。
    ///
    /// - `All` は false の子だけを再帰収集 (満たしている条件はノイズなので出さない)。
    /// - `Any` は「どれか一つ満たせばよい」まとまりなので、全滅時は **Any ノード自体**を
    ///   1 件返す (子を individually 並べると『全部未達』に読めて誤解を生む)。
    /// - 葉が false ならその葉自身。
    pub fn unmet(&self, s: &GameState) -> Vec<Gate> {
        if self.eval(s) {
            return Vec::new();
        }
        match self {
            Gate::All { of } => of.iter().flat_map(|g| g.unmet(s)).collect(),
            other => vec![other.clone()],
        }
    }
}

fn default_gate() -> Gate {
    Gate::Always
}

fn default_true() -> bool {
    true
}

/// 場所からの出口。`gate` 未達なら [`Gate::Always`]。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exit {
    pub to: LocationId,
    #[serde(default = "default_gate")]
    pub gate: Gate,
}

/// 場所アイテムの取得様式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TakeMode {
    /// 一度だけ (既定)。取得すると場所から無くなる ([`GameState::taken_items`] に記録され、
    /// 手放して戻っても再取得=複製は却下)。
    #[default]
    Once,
    /// 何度でも取れる (自販機のジュース等)。取得しても場所に残る。
    Infinite,
    /// 備え付け (シャワー/テレビのリモコン等)。取得不可 — 却下理由が「取らずにその場で
    /// 使える」を LLM に説明し、self-repair で語り直しへ誘導する。
    Fixed,
}

/// 場所アイテムの宣言。旧形式 (Gate 直書き = `take: once`) と新形式 (`{when, take}`) の
/// 両方を受ける (untagged。既存 YAML は無改修)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LocationItem {
    /// 旧形式: 取得 gate をそのまま書く (= `take: once`)。`kind` タグで判別されるので先に試す。
    Legacy(Gate),
    /// 新形式: 取得条件 (`when`、省略時 always) + 取得様式 (`take`、省略時 once)。
    Def {
        #[serde(default = "default_gate")]
        when: Gate,
        #[serde(default)]
        take: TakeMode,
    },
}

impl LocationItem {
    /// 取得条件の gate。
    pub fn when(&self) -> &Gate {
        match self {
            LocationItem::Legacy(g) => g,
            LocationItem::Def { when, .. } => when,
        }
    }

    /// 取得様式 (旧形式は once)。
    pub fn take(&self) -> TakeMode {
        match self {
            LocationItem::Legacy(_) => TakeMode::Once,
            LocationItem::Def { take, .. } => *take,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    /// 人間向け**表示名** (id=機械用セレクタ / title=表示名 の三層思想、[`GoalDef`] の title と
    /// 同類)。提示層 (GUI の現在地) が使う非検証の提示素材。空なら提示層が id へフォールバック。
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// 背景画像のアセット ID (`images/` 配下のファイル名)。提示層が解決して背景にする。
    /// **engine は解釈しない不透明 string** (description/narration と同じ語り素材カテゴリ、北極星)。
    #[serde(default)]
    pub image: Option<String>,
    /// ループ BGM のアセット ID (`audios/` 配下のファイル名)。提示層がこの場所に居る間ループ再生する。
    /// **engine は解釈しない不透明 string** (image と同じ語り素材カテゴリ、北極星)。
    #[serde(default)]
    pub bgm: Option<String>,
    /// この場所に「いる」NPC (presence)。提示層が顔アイコン行に出す。
    /// **空なら scenario.characters 全員**にフォールバック (後方互換)。engine は使わない不透明データ。
    /// この場に居る NPC (**明示宣言**)。空 (未宣言含む) なら誰もいない — NPC を出す場所には
    /// 必ず書く (旧「空なら全 characters」は廃止、2026-07-02)。実効 presence はこれ ±
    /// `GameState.present_overrides` ([`Scenario::present_at`])。
    #[serde(default)]
    pub present: BTreeSet<EntityId>,
    /// 場所にあるアイテム → 取得条件 + 取得様式 (旧形式 Gate 直書き / 新形式 `{when, take}`)。
    #[serde(default)]
    pub items: BTreeMap<ItemId, LocationItem>,
    #[serde(default)]
    pub exits: Vec<Exit>,
}

/// stat の宣言: 初期値と境界 (clamp の上下限)。`min` 省略時 0、`max` 省略時なし。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatDecl {
    pub initial: i64,
    #[serde(default)]
    pub min: i64,
    #[serde(default)]
    pub max: Option<i64>,
}

/// キャラクター定義 (`characters/*.yaml`)。不変の authored 内容 (GameDefinitions 相当)。
///
/// 数値 (`stats`) は正本がキャラ別に握る可変状態の宣言。`profile` は語りの素材 (柔らかい性向含む、
/// いずれ Memoria が想起)。`taboos` は硬い禁忌 (Phase B でエンジンが強制)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterDef {
    #[serde(default)]
    pub name: String,
    /// 設定・背景・性格・性向 (検証しない語りの素材。可変世界状態は持たない)。
    #[serde(default)]
    pub profile: String,
    /// このキャラが持つ stat の宣言。`initial` が [`Scenario::initial_state`] の初期値。
    #[serde(default)]
    pub stats: BTreeMap<StatKey, StatDecl>,
    /// 宣言された閉じた能力集合 (催眠/予知 等)。初期スキル。未宣言の能力は存在しない
    /// (メアリー・スー遮断)。開花は authored トリガーの grant_skill 効果のみ。
    #[serde(default)]
    pub skills: BTreeSet<SkillId>,
    /// 初期所持品 (閉世界)。[`Scenario::initial_state`] でこの entity に seed される。
    #[serde(default)]
    pub inventory: BTreeSet<ItemId>,
    /// 顔アイコンのアセット ID (`images/` 配下)。提示層が presence 表示に使う不透明 string。
    #[serde(default)]
    pub icon: Option<String>,
    /// 初期の文字列属性 (クラス/種族 等)。宣言したキーが閉世界の許可集合になり、トリガーの
    /// set_attribute はこのキーにしか書けない (未宣言キーは load 時 validate で弾く)。
    #[serde(default)]
    pub attributes: BTreeMap<AttrKey, String>,
    /// 硬い禁忌: これが true になる delta を却下する (Phase B でエンジン強制)。
    #[serde(default)]
    pub taboos: Vec<Gate>,
}

/// 反応ビート (Phase C)。禁忌 ([`CharacterDef::taboos`]) の双対 — 真化を**却下**する代わりに、
/// 真化したら**発火**する。発火条件 `when` が成立した瞬間 (edge)、authored な `effects` を
/// エンジンが原子適用し、`narration` を語りに注入する。同じ「delta 適用後の Gate 評価」機構を
/// 禁忌と共有する (禁忌は射影 clone で却下判定、トリガーは実 state で発火)。
///
/// `effects` は authored (シナリオ作者が書く信頼済の op) なので検証しない — LLM 提案ではない。
/// 一度発火すると [`GameState::fired`] に latch され、`when` が真のままでも二度と発火しない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trigger {
    /// 発火済み追跡のための識別子 (シナリオ内で一意)。
    pub id: TriggerId,
    /// 発火条件。delta 適用後の state で真化した瞬間に発火する。
    pub when: Gate,
    /// 発火時にエンジンが原子適用する機械的効果 (フラグ・stat・解放)。authored・信頼済。
    #[serde(default)]
    pub effects: Vec<StateOp>,
    /// 発火時に語りへ注入する指示 (例「アリスは子供時代の約束を思い出す」)。検証しない。
    #[serde(default)]
    pub narration: String,
    /// Memoria 橋渡し (memoria_bridge): 発火時に伏線/性格を recall するための cue (tag/id)。
    /// gm_core はこれを**解釈せず運ぶだけ** (engine は Memoria 非依存・決定論のまま)。
    /// 解決は上位 (harness) の責務。`None` なら recall しない静的な反応ビート。
    #[serde(default)]
    pub recall: Option<String>,
    /// 発火時のイベント CG (画像 ID)。**engine は解釈しない不透明 string** (背景/narration と同類)。
    /// 提示層が `images/{id}` を解決して描画する。`None` なら CG 差し替え無し。
    #[serde(default)]
    pub image: Option<String>,
    /// イベント CG の出し方 (既定 [`ImageMode::Background`])。engine は使わない不透明データ。
    #[serde(default)]
    pub image_mode: Option<ImageMode>,
    /// 発火時の SE (効果音) のアセット ID (`audios/` 配下)。**engine は解釈しない不透明 string**
    /// (image と同類)。提示層が発火ターンに one-shot 再生する。`None` なら SE 無し。
    #[serde(default)]
    pub sound: Option<String>,
    /// **繰り返し発火**するか (既定 false = edge-triggered once)。
    ///
    /// `false`: 一度発火すると [`GameState::fired`] に latch され二度と発火しない (伏線・覚醒など一回性のビート)。
    /// `true`: 永続 latch しない。`when` が再び真化すれば将来のターンで再発火する
    /// (カウンタ閾値→効果→リセットのループ等)。リセットは authored 効果で書く
    /// (`scale_stat` num:0 でハードリセット / `adjust_stat` 負で繰り越し)。
    /// **停止性**: repeatable でも 1 回の `apply` (settle) 内では高々 1 回しか発火しない
    /// (効果が `when` を真のままにしても無限ループしない)。複数閾値を一度に跨いでも発火は次ターンへ繰り越す。
    #[serde(default)]
    pub repeatable: bool,
}

/// イベント CG の表示モード。`Trigger.image` をどう出すか (提示層が解釈する)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageMode {
    /// 背景を上書き・持続 (シーン切替)。既定。
    Background,
    /// 既存背景の上に重ねる (予約・提示層の実装は将来)。
    Overlay,
}

/// tier がどの**自然出目** (修正前の素の `1d{sides}`) で発火するか。
/// `min`/`max` は die サイズに依存しない極点、`at_most`/`at_least` は [`TierDef::threshold`] と
/// 組で**幅**を持たせる閾値。
///
/// **判定するのは自然出目そのもの** (stat 修正も modifiers の bonus も乗る前) なので、
/// 「素の下振れ = 大失敗」という tier の設計思想は幅を持たせても保たれる。d100 のように
/// `sides` が大きく `min` (=1 のみ, 1%) では滅多に発火しない盤面で、下位/上位帯を極にできる。
///
/// 全て単項 (data を持たない) ゆえ YAML では文字列 (`natural: at_most`)。閾値は兄弟フィールド
/// `threshold` で持つ (`{ natural: at_most, threshold: 20 }`)。既存の `natural: min` は不変。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Natural {
    /// 最小目 (`roll == 1`)。大失敗 (fumble) の定番条件。`threshold` 不要。
    Min,
    /// 最大目 (`roll == sides`)。大成功 (crit) の定番条件。`threshold` 不要。
    Max,
    /// `roll <= threshold`。下位帯を極にする (d100 で「20 以下は大失敗」等)。`threshold` 必須 (`1..=sides`)。
    AtMost,
    /// `roll >= threshold`。上位帯を極にする (d100 で「96 以上は大成功」等)。`threshold` 必須 (`1..=sides`)。
    AtLeast,
}

/// 判定結果の極 (tier)。**作者が定義する** — どの自然出目で発火し、何を帰結フラグに立てるか。
/// `flag` は任意: tier を認識だけして帰結フラグを持たない極も書ける (例: 大成功で語りだけ変える)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierDef {
    /// この tier が発火する自然出目の極。
    pub natural: Natural,
    /// `at_most`/`at_least` の閾値 (`1..=sides`、[`Scenario::validate`] が検査)。
    /// `min`/`max` では不要 (無視される)。省略時 `None`。
    #[serde(default)]
    pub threshold: Option<u32>,
    /// 該当時に**同一 apply 内で原子適用**する機械効果 (authored 専権 — trigger effects と
    /// 同じ信頼モデル。LLM は challenge を「選ぶ」だけで帰結を持てない)。stat/attribute/
    /// スキル等をフラグ+トリガーの2点セット無しで直接動かせる。`attempt_challenge` は
    /// 無限再帰の芽なので validate が弾く (連鎖は flag→トリガー経由で書く)。
    #[serde(default)]
    pub effects: Vec<StateOp>,
    /// 該当時に engine が直書きする帰結フラグ (任意)。`allowed_flags` 宣言必須 ([`Scenario::validate`])。
    #[serde(default)]
    pub flag: Option<FlagKey>,
    /// 該当時の結末ナレーション (authored・任意)。`CheckOutcome.narration` に載り**毎回・同ターン**に出る
    /// (トリガーと違い latch されないので、繰り返す判定でも毎回語れる)。フラグ無しの極でも語れる。
    #[serde(default)]
    pub narration: String,
    /// 該当時の結末効果音のアセット ID (`audios/` 配下・任意)。`CheckOutcome.sound` に載り
    /// **毎回・同ターン**に one-shot 再生される。engine 非解釈の不透明 string (narration と同列)。
    #[serde(default)]
    pub sound: String,
}

/// challenge の通常成否 (total>=dc / 未満) の帰結。フラグと結末ナレーションを任意で持つ。
/// narration は `CheckOutcome.narration` に載り**毎回・同ターン**に出る (非 latch=繰り返す失敗も毎回語れる)。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChallengeOutcome {
    /// engine が直書きする帰結フラグ (任意)。`allowed_flags` 宣言必須。
    #[serde(default)]
    pub flag: Option<FlagKey>,
    /// 該当時に**同一 apply 内で原子適用**する機械効果 (authored 専権 — trigger effects と
    /// 同じ信頼モデル)。通常成否と極 (tier) の effects は併存する (フラグと同じ)。
    /// `attempt_challenge` は無限再帰の芽なので validate が弾く。
    #[serde(default)]
    pub effects: Vec<StateOp>,
    /// 結末ナレーション (authored・任意)。失敗を必ず描きたい時に使う (LLM 任せにしない)。
    #[serde(default)]
    pub narration: String,
    /// 結末効果音のアセット ID (`audios/` 配下・任意)。`CheckOutcome.sound` に載り
    /// **毎回・同ターン**に one-shot 再生される。engine 非解釈の不透明 string (narration と同列)。
    #[serde(default)]
    pub sound: String,
}

/// authored challenge。**判定の素性と帰結を作者が握る閉じた定義**。
/// LLM は [`StateOp::AttemptChallenge`] で challenge を**選ぶ**だけで、ここを author できない。
/// challenge の条件付き修正: `when` (Gate) が真なら `bonus` を出目合計に加える。
/// 「導師の教えが立っていれば +5」「傷を負っていれば −3」等。`bonus` は負も可 (ペナルティ)。
/// `when` は純粋 Gate なので flag/stat/attribute/skill/all/any どれでも条件にできる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChallengeMod {
    pub when: Gate,
    pub bonus: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChallengeDef {
    /// LLM への提示文 (どんな行動の判定か。`scenario_brief` に出して GM に選ばせる)。
    #[serde(default)]
    pub description: String,
    /// **判定主体の authored 固定** (任意)。`Some` なら op の `entity` を**上書き**して、
    /// この entity の stat で振る (LLM が entity を省略/誤指定しても正しい主体で判定される —
    /// 実測: LLM は既定で player を主体にするため、NPC の stat を使う challenge が
    /// UnknownStat で毎回却下されていた)。「判定の素性 (stat/sides/dc/帰結) は authored」の
    /// 主体版 = 閉世界の一貫。`None` なら従来どおり op の entity (省略時 player)。
    #[serde(default)]
    pub entity: Option<EntityId>,
    /// この挑戦に挑める前提条件 (Gate)。`Some` で偽なら `attempt_challenge` を却下 (挑戦の解禁/封鎖)。
    /// 「導師に会うまでは秘奥義に挑めない」等。`None` なら常に挑める。
    #[serde(default)]
    pub requires: Option<Gate>,
    /// 条件付き修正 (有利/不利)。`when` が真の分だけ `bonus` を出目合計に加える (順不同・合算)。
    #[serde(default)]
    pub modifiers: Vec<ChallengeMod>,
    /// 修正に使う stat (挑戦する entity が宣言済みであること)。**省略可** —
    /// `None` なら能力に依らない純粋ダイス (修正値 0、運試し)。`Some` なら 1d{sides}+stat修正。
    #[serde(default)]
    pub stat: Option<StatKey>,
    pub sides: u32,
    pub dc: u32,
    /// 通常成功 (`total >= dc`) の帰結 (フラグ + 結末ナレーション、いずれも任意)。`flag` は `allowed_flags` 宣言必須。
    #[serde(default)]
    pub on_success: Option<ChallengeOutcome>,
    /// 通常失敗 (`total < dc`) の帰結 (フラグ + 結末ナレーション、いずれも任意)。`flag` は `allowed_flags` 宣言必須。
    #[serde(default)]
    pub on_failure: Option<ChallengeOutcome>,
    /// 極 (tier) の定義。キー = tier 名 (`crit_fail` 等)。自然出目の min/max で発火。
    /// 通常成否 (on_success/on_failure) と**併存**する。`CheckOutcome.tier` に surface する。
    #[serde(default)]
    pub tiers: BTreeMap<String, TierDef>,
}

/// シナリオの**静的整合性**の破れ。`scenarios/*.yaml` を load した直後に検査する
/// (apply 中の panic でなく load 時の構造化エラーで弾く=幻参照を実行経路に乗せない)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScenarioError {
    /// challenge の tier が立てる帰結フラグが `allowed_flags` に宣言されていない (幻フラグ)。
    ChallengeFlagUndeclared {
        challenge: ChallengeId,
        tier: String,
        flag: FlagKey,
    },
    /// `global_flags` に挙げたフラグが `allowed_flags` に宣言されていない (幻の世界フラグ)。
    GlobalFlagUndeclared { flag: FlagKey },
    /// `persistent_flags` に挙げたフラグが `allowed_flags` に宣言されていない (幻の場所フラグ)。
    PersistentFlagUndeclared { flag: FlagKey },
    /// `flag_hints` のキーが `allowed_flags` に宣言されていない (幻フラグへのヒント)。
    FlagHintUndeclared { flag: FlagKey },
    /// `flag_hints` のキーが**専権フラグ** (トリガー/challenge の effects が書く) に付いている
    /// (二重所有の罠)。ヒントは「GM に set_flag で立てさせたい」意図表明だが、専権フラグは GM の
    /// usable 一覧に一切出ないので**ヒントが死ぬ**。フラグの書き手を GM か engine のどちらか一方に
    /// 決める (トリガー/challenge から外して純粋 set_flag にするか、ヒントを外す)。
    /// **lint** ([`Scenario::lints`]) — プレイは壊れないので load は拒否しない (警告表示のみ。
    /// fatal にすると配布済み content が受領側で死ぬ)。
    FlagHintOnAuthoredOnly { flag: FlagKey },
    /// `flag_titles` のキーが `allowed_flags` に宣言されていない (幻フラグへの表示名)。
    FlagTitleUndeclared { flag: FlagKey },
    /// `hidden_flags` のキーが `allowed_flags` に宣言されていない (幻フラグの秘匿)。
    HiddenFlagUndeclared { flag: FlagKey },
    /// トリガーの `set_attribute` が宣言されていない属性キーに書こうとしている (幻属性遮断)。
    /// player は `initial_attributes`、NPC は `CharacterDef::attributes` でキーを宣言する。
    AttributeKeyUndeclared {
        trigger: TriggerId,
        entity: EntityId,
        key: AttrKey,
    },
    /// `secret_attributes` のキーがどこにも宣言されていない (幻属性の秘匿)。
    /// initial_attributes / CharacterDef.attributes / role_assignment.key のいずれかで宣言する。
    SecretAttributeUndeclared { key: AttrKey },
    /// `hidden_attributes` のキーがどこにも宣言されていない (幻属性の本人未知秘匿)。
    /// 宣言先は `SecretAttributeUndeclared` と同じ。
    HiddenAttributeUndeclared { key: AttrKey },
    /// `vote_rules` の voter_attribute キーがどこにも宣言されていない (幻属性の投票権)。
    VoteRuleAttributeUndeclared { key: AttrKey },
    /// challenge の effects に `attempt_challenge` が入っている (A→A の無限再帰の芽)。
    /// 判定の連鎖は flag→トリガー経由で書く。
    ChallengeEffectRecursive { challenge: ChallengeId },
    /// challenge の authored 判定主体 (`ChallengeDef::entity`) が、判定に使う stat を宣言して
    /// いない (幻主体/幻ステータス)。player は `initial_stats`、NPC は `CharacterDef::stats` で宣言。
    ChallengeStatUndeclared {
        challenge: ChallengeId,
        entity: EntityId,
        stat: StatKey,
    },
    /// tier の `at_most`/`at_least` 閾値が `1..=sides` の範囲外 (常時発火/絶対不発火の幻値)。
    TierThresholdOutOfRange {
        challenge: ChallengeId,
        tier: String,
        threshold: u32,
        sides: u32,
    },
    /// `role_assignment` の pool 人数合計と among の人数が一致しない (配りきれない/余る)。
    RoleAssignmentCountMismatch { pool_total: u32, among: usize },
    /// `role_assignment.among` にこのシナリオが知らない entity が居る (幻キャラへの配布)。
    RoleAssignmentUnknownEntity { entity: EntityId },
    /// `role_assignment.among` に同じ entity が二度居る (重複配布)。
    RoleAssignmentDuplicateEntity { entity: EntityId },
    /// 勝利条件が無い (`goal` も `goals` も未指定)。到達不能なシナリオ。
    NoGoal,
}

/// 役職のランダム割り当て (spec 06 Phase A)。人狼/グノーシア型の秘匿役職盤面の初期化。
///
/// **割り当てはエンジンの専権** — [`Scenario::initial_state`] が seed から**派生した専用
/// ストリーム** (role_rng) で決定論 shuffle し、各 entity の `attributes[key]` に書く。
/// LLM は関与できない (「出目は正本」の配役版)。同 seed 同配役 = 再現・監査可能。
/// 本流 `state.rng` は消費しない (配役の有無でプレイ中のダイス列が変わらない)。
/// この宣言自体が属性キーの宣言を兼ねる (値の閉集合 = pool のキー)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleAssignment {
    /// 書き込む属性キー (例: 役職)。
    pub key: AttrKey,
    /// 役職 → 人数。キー順 (BTreeMap) に展開してから shuffle するので決定論。
    pub pool: BTreeMap<String, u32>,
    /// 配布先 (player を含められる = グノーシア式)。人数は pool の合計と一致必須。
    pub among: Vec<EntityId>,
}

/// 投票権の宣言 (spec 06 Phase C)。[`crate::StateOp::CastVote`] は vote_rules の
/// **いずれかに合致**したときだけ受理される — rule が一つも合致しなければ却下
/// (**デフォルト拒否**。将来「観戦者」等を足しても安全)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoteRule {
    /// この rule が有効な状況 (フェーズフラグ等)。省略時 Always。
    #[serde(default = "default_gate")]
    pub when: Gate,
    /// 投票権を持つ voter の条件 (省略時 = 生存者なら誰でも)。複数条件が要るまで単数
    /// (必要になったら `voter_attributes: [..]` に拡張する余地を残す、査読で合意)。
    #[serde(default)]
    pub voter_attribute: Option<AttrRequirement>,
}

/// 属性の一致条件 (`attributes[key] == value`)。voter のようなパラメトリックな主語に
/// 使うため Gate 本体には入れない (Gate は entity 固定書きの純粋述語のまま)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttrRequirement {
    pub key: AttrKey,
    pub value: String,
}

/// 名前付き goal (エンディング)。複数を authored 順に持ち、最初に成立したものが
/// [`Scenario::reached`] の戻り値 = 次モジュールの**分岐セレクタ**になる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalDef {
    pub id: GoalId,
    pub when: Gate,
    /// 目標一覧の表示名 (authored、非検証の提示素材)。id はスペース等を避ける機械用の
    /// 分岐セレクタゆえ、人間向けの文はこちらに書く。空なら提示層が id へフォールバック。
    #[serde(default)]
    pub title: String,
    /// false なら**隠しゴール** — 到達するまで提示層が目標一覧に出さない (到達で開示)。
    /// 到達判定 `reached` は不変で効く = engine 非解釈の提示層宣言 (`hidden_stats` と同類)。
    /// 既定 true (既存 YAML は無改修で全 goal 表示)。
    #[serde(default = "default_true")]
    pub visible: bool,
    /// プレイヤー向けの道しるべ (authored、非検証の提示素材)。「この goal は何をすれば
    /// だいたい行けるか」を提示層 (目標一覧) が表示する。`when` の条件そのものは
    /// ネタバレゆえ出さない設計の、作者が意図的に開示するヒント。空なら表示なし。
    #[serde(default)]
    pub hint: String,
    /// 到達時に語りへ注入する結末ナレーション (authored、非検証の語り素材)。
    /// 複数 goal のどれに達したかを提示層が出すための文面。空なら結末の語りなし。
    #[serde(default)]
    pub narration: String,
}

/// 主人公(プレイヤー)の設定。**語りの素材** (非検証) — NPC がプレイヤーを認識・反応する材料。
/// package.player から注入される (gm_core は値を解釈せず prompt へ供給するだけ。CharacterDef.profile と同類)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Protagonist {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub profile: String,
    /// 主人公の顔アイコンのアセット ID (`images/` 配下)。提示層が presence 表示に使う不透明 string。
    #[serde(default)]
    pub icon: Option<String>,
}

/// シナリオ全体。`scenarios/*.yaml` から読み込まれる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scenario {
    #[serde(default)]
    pub title: String,
    /// 世界観 lore (語りの素材、非検証)。package.world から注入。可変状態は持たない (北極星)。
    #[serde(default)]
    pub world: String,
    /// 主人公(プレイヤー)の設定 (語りの素材)。package.player から注入。NPC が認識する。
    #[serde(default)]
    pub protagonist: Protagonist,
    pub start: LocationId,
    #[serde(default)]
    pub allowed_flags: BTreeSet<FlagKey>,
    /// **世界フラグ** (campaign 横断で持ち越す)。`allowed_flags` の部分集合。
    /// [`Scenario::transition`] はこれに挙げたフラグだけ次モジュールへ運び、残り (局所) は捨てる。
    #[serde(default)]
    pub global_flags: BTreeSet<FlagKey>,
    /// **場所フラグ** (その場所＝このモジュールに持つ)。`allowed_flags` の部分集合 (spec 02)。
    /// global と違い ambient に全モジュールへ漏らさず、**このモジュールを再訪したときだけ**復元される
    /// (例: `chest_opened`)。蓄積は engine でなく harness の campaign 層が担う
    /// (gm_core はこの宣言を持つだけ・`transition` は局所と同じく捨てる)。
    #[serde(default)]
    pub persistent_flags: BTreeSet<FlagKey>,
    /// フラグを true にするための gate。記載なければ [`Gate::Always`]。
    #[serde(default)]
    pub flag_rules: BTreeMap<FlagKey, Gate>,
    /// **知識フラグのヒント** (`flag → 立てる条件の説明文`、spec 03)。提示層が prompt に surface し、
    /// LLM が「会話で情報が伝わった瞬間」に `set_flag` を出せるようにする (弱モデル向けロバスト化)。
    /// **非検証の語り素材** (engine は値を解釈しない、`world`/`profile` と同類) だが、キーは
    /// `allowed_flags` 宣言必須 (幻フラグへのヒントを load 時に弾く)。`flag_rules` の gate と対で使う
    /// — ヒントが**促し**、gate が**守る** (早まった set_flag を却下するバックストップ)。
    #[serde(default)]
    pub flag_hints: BTreeMap<FlagKey, String>,
    /// フラグの**表示名** (エイリアス。authored・非検証の提示素材、goal の `title` と同じ三層思想:
    /// id=機械用キー / title=人間向け表示)。UI のフラグ一覧・語りの接地に使い、キーは
    /// `allowed_flags` 宣言必須 ([`Scenario::validate`] が幻フラグへの表示名を弾く)。
    #[serde(default)]
    pub flag_titles: BTreeMap<FlagKey, String>,
    /// **表示から隠すフラグ** (`hidden_stats` のフラグ版)。タイマーの armed フラグ (`x_done` 等)
    /// のような**変数として使う帳簿フラグ**を、提示層 (UI フラグ一覧 / `state_brief` /
    /// 語彙節) が一切出さない宣言。engine 非使用・非検証 — gate/トリガーの評価は不変で効く。
    /// キーは `allowed_flags` 宣言必須。
    #[serde(default)]
    pub hidden_flags: BTreeSet<FlagKey>,
    /// 役職のランダム割り当て (spec 06 Phase A)。宣言があれば [`Self::initial_state`] が
    /// 専用ストリームで shuffle して配る。詳細は [`RoleAssignment`]。
    #[serde(default)]
    pub role_assignment: Option<RoleAssignment>,
    /// **ゲーム的秘匿情報**の属性キー (spec 06 Phase B。役職等)。`hidden_*` (全提示層から
    /// 隠す帳簿) とは別軸の宛先別可視性: **GM=全員分** (秘匿注記付き、ゲームを回すのに必要) /
    /// **プレイヤー UI=本人分のみ** (NPC 分は DTO 段階で落とす) / **登場人物どうし=不可**
    /// (GM の演じ分け=prompt 規律)。engine は宣言を運ぶだけで gate/トリガー評価は不変。
    /// キーはどこかで宣言済み (initial_attributes / CharacterDef.attributes /
    /// role_assignment.key) 必須。
    #[serde(default)]
    pub secret_attributes: BTreeSet<AttrKey>,
    /// **当人にも見えない**属性キー (呪い・自覚のない正体等)。`secret_attributes`
    /// (本人分は見える = 人狼の自役職) より一段強い秘匿: **プレイヤー UI = 本人分ごと落とす** /
    /// **GM = 全員分** (「本人未知」注記付き — 当人にすら明かさない語りの規律は prompt 層) /
    /// 登場人物どうし = 不可。engine は宣言を運ぶだけで gate/トリガー評価は不変。
    /// キーの宣言必須は secret と同じ (initial_attributes / CharacterDef.attributes /
    /// role_assignment.key)。
    #[serde(default)]
    pub hidden_attributes: BTreeSet<AttrKey>,
    /// 投票権の宣言 (spec 06 Phase C)。CastVote はこのいずれかに合致したときだけ受理
    /// (デフォルト拒否)。詳細は [`VoteRule`]。
    #[serde(default)]
    pub vote_rules: Vec<VoteRule>,
    /// `"player"` の stat 糖衣 (後方互換)。min 0 / max なしで宣言扱い。
    #[serde(default)]
    pub initial_stats: BTreeMap<StatKey, i64>,
    /// **表示から隠す stat キー** (内部用の帳簿 stat。spec 04 追補)。タイマー (`record_turn` の刻み) や
    /// repeatable カウンタのような engine 内部値を、提示層 (UI の状態パネル / prompt の state_brief / CLI) が
    /// この集合のキーで skip する。**engine は使わない提示ヒント** (キーの正本性には影響しない・非検証)。
    /// 全 entity に効く (どの entity のこのキーの stat も隠す)。
    #[serde(default)]
    pub hidden_stats: BTreeSet<StatKey>,
    /// `"player"` の初期スキル糖衣 (閉世界宣言)。NPC は [`CharacterDef::skills`]。
    #[serde(default)]
    pub initial_skills: BTreeSet<SkillId>,
    /// `"player"` の初期所持品。[`Scenario::initial_state`] で player に seed される
    /// (場所から拾う/譲渡/持ち越し以外の「最初から所持」経路)。NPC は [`CharacterDef::inventory`]。
    #[serde(default)]
    pub initial_inventory: BTreeSet<ItemId>,
    /// `"player"` の初期文字列属性 (クラス/職業/種族 等)。宣言キーが player の閉世界許可集合になり、
    /// トリガーの set_attribute はこのキーにしか書けない。NPC は [`CharacterDef::attributes`]。
    #[serde(default)]
    pub initial_attributes: BTreeMap<AttrKey, String>,
    /// このシナリオに登場する外部キャラの宣言 (`characters/{id}.yaml` から注入する entity)。
    /// **空なら外部注入しない** — シナリオが宣言した登場人物だけが現れる (全シナリオ共有の混入を防ぐ)。
    /// inline `characters` に在る entity はそちらが優先。
    #[serde(default)]
    pub cast: BTreeSet<EntityId>,
    /// 登場人物 (player 以外)。inline 宣言 + `cast` で指定した外部 `characters/*.yaml` の注入。
    #[serde(default)]
    pub characters: BTreeMap<EntityId, CharacterDef>,
    /// 反応ビート (Phase C)。`when` 成立で `effects` を原子適用し `narration` を注入する。
    #[serde(default)]
    pub triggers: Vec<Trigger>,
    /// authored challenge (技能判定の素性と帰結)。LLM は `AttemptChallenge` で選ぶだけ。
    #[serde(default)]
    pub challenges: BTreeMap<ChallengeId, ChallengeDef>,
    pub locations: BTreeMap<LocationId, Location>,
    /// 単一の達成条件 (名前無し・後方互換)。`goals` を使う時は省略可。
    #[serde(default)]
    pub goal: Option<Gate>,
    /// 名前付き goal (エンディング) の authored 順リスト。分岐する結末を書ける。
    /// 非空ならこちらが優先され、最初に成立した [`GoalDef::id`] が [`Scenario::reached`] の戻り値。
    #[serde(default)]
    pub goals: Vec<GoalDef>,
}

impl Scenario {
    pub fn from_yaml(s: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(s)
    }

    pub fn location(&self, id: &str) -> Option<&Location> {
        self.locations.get(id)
    }

    /// フラグを true にするための gate。未登録なら [`Gate::Always`]。
    pub fn flag_gate(&self, key: &str) -> Gate {
        self.flag_rules.get(key).cloned().unwrap_or(Gate::Always)
    }

    /// 指定 entity がこのシナリオに存在するか (主人公 or 登場人物)。譲渡先の検証に使う。
    pub fn knows_entity(&self, entity: &str) -> bool {
        entity == PLAYER || self.characters.contains_key(entity)
    }

    /// 現在地の**実効 NPC presence** (spec 04)。`Location.present` (場所ベース) に
    /// `GameState.present_overrides` を重ね、このモジュールが知る characters に絞る。**純粋関数** —
    /// 提示層 (顔アイコン行) が主人公を先頭に名前/アイコンを解決する素。override は bool を運ぶだけなので、
    /// このモジュールに未注入の entity への force-present はここで黙って落ちる (override 自体は state に残り、
    /// そのキャラを持つ次のモジュールで現れる)。
    ///
    /// **present は明示宣言** (2026-07-02 改訂): 空 (未宣言含む) なら**誰もいない**。
    /// 旧「空なら全 characters」フォールバックは廃止 — 無人の場所を作るのに全キャラを
    /// set_presence false する羽目になるため。NPC を出す場所には present を必ず書く。
    pub fn present_at(&self, state: &GameState) -> BTreeSet<EntityId> {
        let mut set: BTreeSet<EntityId> = self
            .location(&state.location)
            .map(|loc| loc.present.clone())
            .unwrap_or_default();
        for (entity, &present) in &state.present_overrides {
            if present {
                set.insert(entity.clone());
            } else {
                set.remove(entity);
            }
        }
        // このモジュールが知らない (cast 注入されていない) entity は解決できないので落とす。
        set.retain(|e| self.characters.contains_key(e));
        set
    }

    /// authored challenge を引く (未宣言なら `None`)。
    pub fn challenge(&self, id: &str) -> Option<&ChallengeDef> {
        self.challenges.get(id)
    }

    /// **authored 専権フラグ** — トリガー効果・challenge 帰結 (on_success/on_failure/tier) が
    /// engine 経由で書くフラグ。LLM が set_flag すべきでない (立てても筋書きの先取り＝ノイズ)。
    /// 宣言の走査だけで機械的に判別できる。`filter_authored_only_ops` (op の構造的遮断) のフラグ版。
    pub fn authored_only_flags(&self) -> BTreeSet<FlagKey> {
        let mut set = BTreeSet::new();
        for t in &self.triggers {
            for op in &t.effects {
                if let StateOp::SetFlag { key, .. } = op {
                    set.insert(key.clone());
                }
            }
        }
        for c in self.challenges.values() {
            for outcome in [&c.on_success, &c.on_failure].into_iter().flatten() {
                if let Some(flag) = &outcome.flag {
                    set.insert(flag.clone());
                }
            }
            for tier in c.tiers.values() {
                if let Some(flag) = &tier.flag {
                    set.insert(flag.clone());
                }
            }
        }
        set
    }

    /// **LLM が set_flag してよいフラグの語彙** = `allowed_flags` − [`Self::authored_only_flags`]。
    /// prompt (使えるフラグの列挙) と `FlagNotAllowed` の却下文面 (self-repair の一発修正) の素。
    pub fn usable_flags(&self) -> BTreeSet<FlagKey> {
        let authored = self.authored_only_flags();
        self.allowed_flags.iter().filter(|f| !authored.contains(*f)).cloned().collect()
    }

    /// **静的整合性**を検査する (load 時に呼ぶ)。空 Vec なら健全。
    ///
    /// **lint** — プレイは壊れないが作者の意図どおりに動かない書き方を報せる (非 fatal・表示のみ)。
    /// [`Self::validate`] (整合性の破れ = load 拒否) とは重大度で分ける: fatal にすると配布済み
    /// content が受領側で死ぬ (受領者は直せない) ため、lint は警告として提示層が surface する。
    ///
    /// 現在の lint: **専権フラグへの flag_hint** (二重所有の罠) — ヒントは「GM に set_flag で
    /// 立てさせたい」意図表明だが、トリガー/challenge の effects が書くフラグは GM の usable
    /// 一覧に出ないのでヒントが死ぬ。書き手を GM か engine のどちらか一方に決める。
    pub fn lints(&self) -> Vec<ScenarioError> {
        let mut warns = Vec::new();
        let authored_only = self.authored_only_flags();
        for flag in self.flag_hints.keys() {
            if self.allowed_flags.contains(flag) && authored_only.contains(flag) {
                warns.push(ScenarioError::FlagHintOnAuthoredOnly { flag: flag.clone() });
            }
        }
        warns
    }

    /// PoC-1: 各 challenge の tier が立てる帰結フラグが `allowed_flags` に宣言済みかを見る。
    /// engine は tier 該当時にこのフラグを (flag_rules gate を迂回して) 直書きするので、
    /// 未宣言フラグを許すと閉世界が破れる。apply 中の panic でなく load 時に弾く。
    pub fn validate(&self) -> Vec<ScenarioError> {
        let mut errs = Vec::new();
        for (cid, def) in &self.challenges {
            // tier (極) と通常成否 (on_success/on_failure) が立てるフラグは全て allowed_flags 宣言必須。
            let outcome_flags = def
                .tiers
                .iter()
                .filter_map(|(tname, tier)| tier.flag.as_ref().map(|f| (tname.as_str(), f)))
                .chain(def.on_success.as_ref().and_then(|o| o.flag.as_ref()).map(|f| ("on_success", f)))
                .chain(def.on_failure.as_ref().and_then(|o| o.flag.as_ref()).map(|f| ("on_failure", f)));
            for (label, flag) in outcome_flags {
                if !self.allowed_flags.contains(flag) {
                    errs.push(ScenarioError::ChallengeFlagUndeclared {
                        challenge: cid.clone(),
                        tier: label.to_string(),
                        flag: flag.clone(),
                    });
                }
            }
            // authored 判定主体 (entity) が判定 stat を宣言していることを load 時に確認する
            // (幻主体はプレイ中の UnknownStat 却下でなく load 時に名指しで弾く)。
            if let (Some(e), Some(s)) = (&def.entity, &def.stat) {
                if !self.knows_stat(e, s) {
                    errs.push(ScenarioError::ChallengeStatUndeclared {
                        challenge: cid.clone(),
                        entity: e.clone(),
                        stat: s.clone(),
                    });
                }
            }
            // at_most/at_least は threshold が必須かつ 1..=sides の範囲内であること
            // (欠落=無制限、範囲外=常時発火/絶対不発火の幻値。load 時に弾く)。min/max は threshold 不要。
            for (tname, tier) in &def.tiers {
                if matches!(tier.natural, Natural::AtMost | Natural::AtLeast) {
                    // 欠落は 0 として報告 (1..=sides 外なので同じ経路で弾かれる)。
                    let n = tier.threshold.unwrap_or(0);
                    if n < 1 || n > def.sides {
                        errs.push(ScenarioError::TierThresholdOutOfRange {
                            challenge: cid.clone(),
                            tier: tname.clone(),
                            threshold: n,
                            sides: def.sides,
                        });
                    }
                }
            }
            // 帰結の直接効果 (effects): attempt_challenge の入れ子は無限再帰の芽なので弾く。
            // set_attribute の幻キーもトリガー効果と同じ検査 (trigger 欄に challenge:{id})。
            let effect_lists = def
                .tiers
                .values()
                .map(|t| &t.effects)
                .chain(def.on_success.as_ref().map(|o| &o.effects))
                .chain(def.on_failure.as_ref().map(|o| &o.effects));
            for effects in effect_lists {
                for op in effects {
                    match op {
                        StateOp::AttemptChallenge { .. } => {
                            errs.push(ScenarioError::ChallengeEffectRecursive {
                                challenge: cid.clone(),
                            });
                        }
                        StateOp::SetAttribute { entity, key, .. } => {
                            let declared = if entity == PLAYER {
                                self.initial_attributes.contains_key(key)
                            } else {
                                self.characters
                                    .get(entity)
                                    .is_some_and(|c| c.attributes.contains_key(key))
                            } || self
                                .role_assignment
                                .as_ref()
                                .is_some_and(|ra| &ra.key == key);
                            if !declared {
                                errs.push(ScenarioError::AttributeKeyUndeclared {
                                    trigger: format!("challenge:{cid}"),
                                    entity: entity.clone(),
                                    key: key.clone(),
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        // 世界フラグは許可フラグの部分集合でなければならない (幻の世界フラグを持ち越さない)。
        for flag in &self.global_flags {
            if !self.allowed_flags.contains(flag) {
                errs.push(ScenarioError::GlobalFlagUndeclared { flag: flag.clone() });
            }
        }
        // 場所フラグも許可フラグの部分集合 (幻の場所フラグを campaign 記憶に乗せない)。
        for flag in &self.persistent_flags {
            if !self.allowed_flags.contains(flag) {
                errs.push(ScenarioError::PersistentFlagUndeclared { flag: flag.clone() });
            }
        }
        // 知識フラグのヒントも許可フラグのキーにしか付けられない (幻フラグへのヒントを弾く)。
        // 専権フラグへの死んだヒントは**プレイを壊さない**ので validate (fatal) でなく
        // [`Self::lints`] (警告・表示のみ) が報せる — fatal にすると配布済み content を
        // 受領側で殺す (受領者は直せない)。
        for flag in self.flag_hints.keys() {
            if !self.allowed_flags.contains(flag) {
                errs.push(ScenarioError::FlagHintUndeclared { flag: flag.clone() });
            }
        }
        // フラグの表示名も同様 (幻フラグへの表示名を弾く)。
        for flag in self.flag_titles.keys() {
            if !self.allowed_flags.contains(flag) {
                errs.push(ScenarioError::FlagTitleUndeclared { flag: flag.clone() });
            }
        }
        // 秘匿宣言も同様 (幻フラグの秘匿を弾く)。
        for flag in &self.hidden_flags {
            if !self.allowed_flags.contains(flag) {
                errs.push(ScenarioError::HiddenFlagUndeclared { flag: flag.clone() });
            }
        }
        // 属性の閉世界: トリガーの set_attribute は宣言済みキーにしか書けない (幻属性遮断)。
        // player は initial_attributes、NPC は CharacterDef.attributes が許可キー集合。
        for trig in &self.triggers {
            for op in &trig.effects {
                if let StateOp::SetAttribute { entity, key, .. } = op {
                    let declared = if entity == PLAYER {
                        self.initial_attributes.contains_key(key)
                    } else {
                        self.characters
                            .get(entity)
                            .is_some_and(|c| c.attributes.contains_key(key))
                    };
                    if !declared {
                        errs.push(ScenarioError::AttributeKeyUndeclared {
                            trigger: trig.id.clone(),
                            entity: entity.clone(),
                            key: key.clone(),
                        });
                    }
                }
            }
        }
        // 投票権の宣言整合 (spec 06 Phase C): voter_attribute の幻キーを load 時に弾く。
        for rule in &self.vote_rules {
            if let Some(va) = &rule.voter_attribute {
                let declared = self.initial_attributes.contains_key(&va.key)
                    || self.characters.values().any(|c| c.attributes.contains_key(&va.key))
                    || self.role_assignment.as_ref().is_some_and(|ra| ra.key == va.key);
                if !declared {
                    errs.push(ScenarioError::VoteRuleAttributeUndeclared { key: va.key.clone() });
                }
            }
        }
        // 秘匿属性の宣言整合 (spec 06 Phase B): 幻属性の秘匿を load 時に弾く。
        for key in &self.secret_attributes {
            let declared = self.initial_attributes.contains_key(key)
                || self.characters.values().any(|c| c.attributes.contains_key(key))
                || self.role_assignment.as_ref().is_some_and(|ra| &ra.key == key);
            if !declared {
                errs.push(ScenarioError::SecretAttributeUndeclared { key: key.clone() });
            }
        }
        // 本人未知の秘匿属性も同じ宣言整合 (幻属性の秘匿を load 時に弾く)。
        for key in &self.hidden_attributes {
            let declared = self.initial_attributes.contains_key(key)
                || self.characters.values().any(|c| c.attributes.contains_key(key))
                || self.role_assignment.as_ref().is_some_and(|ra| &ra.key == key);
            if !declared {
                errs.push(ScenarioError::HiddenAttributeUndeclared { key: key.clone() });
            }
        }
        // 役職割り当ての整合 (spec 06 Phase A): 人数一致・幻キャラ・重複配布を load 時に弾く。
        if let Some(ra) = &self.role_assignment {
            let pool_total: u32 = ra.pool.values().sum();
            if pool_total as usize != ra.among.len() {
                errs.push(ScenarioError::RoleAssignmentCountMismatch {
                    pool_total,
                    among: ra.among.len(),
                });
            }
            let mut seen = BTreeSet::new();
            for entity in &ra.among {
                if entity != PLAYER && !self.characters.contains_key(entity) {
                    errs.push(ScenarioError::RoleAssignmentUnknownEntity { entity: entity.clone() });
                }
                if !seen.insert(entity) {
                    errs.push(ScenarioError::RoleAssignmentDuplicateEntity {
                        entity: entity.clone(),
                    });
                }
            }
        }
        // 勝利条件は最低一つ要る (goal 単一 or goals 名前付き)。
        if self.goal.is_none() && self.goals.is_empty() {
            errs.push(ScenarioError::NoGoal);
        }
        errs
    }

    /// **どのエンディング (GoalId) に到達したか**を返す (PoC-2b)。`None` なら未達。
    ///
    /// 戻り値の `GoalId` は次モジュールの **transition 分岐セレクタ**になる
    /// (jammed_ending → 地下、open_ending → 森、等)。
    /// `goals` (名前付き) が非空ならそれを authored 順で評価し、最初に成立した id を返す
    /// (決定論)。`goals` が空なら単一 `goal` を [`DEFAULT_GOAL`] という既定 id で評価する (後方互換)。
    pub fn reached(&self, state: &GameState) -> Option<GoalId> {
        if !self.goals.is_empty() {
            self.goals
                .iter()
                .find(|g| g.when.eval(state))
                .map(|g| g.id.clone())
        } else {
            self.goal
                .as_ref()
                .filter(|g| g.eval(state))
                .map(|_| DEFAULT_GOAL.to_string())
        }
    }

    /// 到達した [`GoalDef`] (id + 結末ナレーション) を返す (`reached` の richer 版)。
    /// 複数 goal の**どれに達したか**と**その語り**を提示層へ渡すための経路。
    /// `goals` (名前付き) が非空のときのみ意味を持つ — 単一 `goal` (後方互換) は GoalDef を
    /// 持たないので `None` (到達判定は `reached`/`is_goal` を使う)。authored 順で最初の一致。
    pub fn reached_goal(&self, state: &GameState) -> Option<&GoalDef> {
        self.goals.iter().find(|g| g.when.eval(state))
    }

    /// **状態を持ち越したまま次の骨格へ遷移する** (campaign keystone, PoC-2a)。
    ///
    /// `self` = 遷移先 (次モジュール) の骨格。`prev` = 直前の状態、`prev_scenario` = 直前の骨格。
    /// 「密室脱出 → 森へ、HP/所持品/好感度を保ったまま」の本体。`initial_state` の双対 —
    /// あちらは初期化、こちらは**持ち越し** (リセットしないことで状態が場所を跨ぐ)。
    ///
    /// - **持ち越す**: 数値 (entities)・所持品・能力・RNG ストリーム・累積ターン・
    ///   **世界フラグ** (`prev_scenario.global_flags` に宣言された分。source が「出ても残る」と宣言)。
    /// - **捨てる**: 局所フラグ・発火済みトリガー (次モジュールの反応は新規)。
    /// - **リセット**: `location` を `self.start` へ。
    /// - 次モジュールが新規宣言する entity/stat/skill は初期化され、既存値は持ち越しが上書きする。
    ///
    /// 帰結 (HP 等) はすべて engine が運ぶ — 生成器/LLM は値を持てない (シーン跨ぎでも不変条件を維持)。
    pub fn transition(&self, prev: &GameState, prev_scenario: &Scenario) -> GameState {
        // 次モジュールの宣言で初期化 (新規 entity/stat/skill を立て、location=self.start, flags/fired 空)。
        let mut s = self.initial_state(prev.rng.seed);
        // RNG ストリームと累積ターンを継続 (監査の連続性)。
        s.rng = prev.rng.clone();
        s.turn = prev.turn;
        // 数値: 既存値で上書き (新規宣言の初期値より持ち越しが優先)。
        for (entity, stats) in &prev.entities {
            for (key, value) in stats {
                s.set_stat(entity, key, *value);
            }
        }
        // 所持品・能力: 丸ごと持ち越し (次モジュールの新規宣言と union)。
        for (entity, items) in &prev.inventory {
            for item in items {
                s.add_to_inventory(entity, item);
            }
        }
        for (entity, skills) in &prev.skills {
            for skill in skills {
                s.grant_skill(entity, skill);
            }
        }
        // 文字列属性: 丸ごと持ち越し (転職等の結果は跨いで生きる。次モジュールの新規宣言を上書き)。
        for (entity, attrs) in &prev.attributes {
            for (key, value) in attrs {
                s.set_attribute(entity, key, value);
            }
        }
        // 登場/退場のオーバーライド: 丸ごと持ち越し (登場させた仲間が次の画面にも同行する、spec 04)。
        s.present_overrides = prev.present_overrides.clone();
        // フラグ: source が global と宣言したものだけ運ぶ (局所は捨てる)。
        for key in &prev_scenario.global_flags {
            if let Some(value) = prev.flags.get(key) {
                s.flags.insert(key.clone(), *value);
            }
        }
        // 真化ターンの記録は生き残ったフラグの分だけ持ち越す (捨てたフラグの帳簿は残さない)。
        s.flag_turns = prev
            .flag_turns
            .iter()
            .filter(|(k, _)| s.flags.contains_key(*k))
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        s
    }

    /// 指定キャラの stat が宣言済か (adjust/scale の対象になれるか)。
    /// 主人公は `initial_stats` 糖衣も宣言とみなす。
    pub fn knows_stat(&self, entity: &str, key: &str) -> bool {
        if entity == PLAYER && self.initial_stats.contains_key(key) {
            return true;
        }
        self.characters
            .get(entity)
            .is_some_and(|c| c.stats.contains_key(key))
    }

    /// 指定キャラ stat の clamp 境界 `(min, max)`。宣言が無ければ下限 0・上限なし。
    pub fn stat_bounds(&self, entity: &str, key: &str) -> (i64, Option<i64>) {
        if let Some(decl) = self.characters.get(entity).and_then(|c| c.stats.get(key)) {
            (decl.min, decl.max)
        } else {
            (0, None) // 主人公の initial_stats 糖衣を含む既定
        }
    }

    /// 開始地点・全キャラの初期 stat から初期状態を作る。
    pub fn initial_state(&self, seed: u64) -> GameState {
        let mut s = GameState::new(self.start.clone(), seed);
        // 主人公の糖衣。
        for (k, v) in &self.initial_stats {
            s.set_stat(PLAYER, k, *v);
        }
        for skill in &self.initial_skills {
            s.grant_skill(PLAYER, skill);
        }
        for item in &self.initial_inventory {
            s.add_to_inventory(PLAYER, item);
        }
        for (k, v) in &self.initial_attributes {
            s.set_attribute(PLAYER, k, v);
        }
        // 登場人物の宣言。
        for (eid, def) in &self.characters {
            for (k, decl) in &def.stats {
                s.set_stat(eid, k, decl.initial);
            }
            for skill in &def.skills {
                s.grant_skill(eid, skill);
            }
            for item in &def.inventory {
                s.add_to_inventory(eid, item);
            }
            for (k, v) in &def.attributes {
                s.set_attribute(eid, k, v);
            }
        }
        // 役職のランダム割り当て (spec 06 Phase A)。seed 派生の専用ストリームで shuffle し、
        // 本流 s.rng は消費しない (配役の有無でプレイ中のダイス列が変わらない)。
        if let Some(ra) = &self.role_assignment {
            // "ROLE_RNG" (ASCII) を seed に混ぜた別系列。決定論 = 同 seed 同配役。
            const ROLE_RNG_LABEL: u64 = 0x524F_4C45_5F52_4E47;
            let mut role_rng = RngState { seed: seed ^ ROLE_RNG_LABEL, cursor: 0 };
            // pool をキー順に展開 (BTreeMap = 決定論) してから Fisher–Yates で混ぜる。
            let mut roles: Vec<&String> = ra
                .pool
                .iter()
                .flat_map(|(role, n)| std::iter::repeat(role).take(*n as usize))
                .collect();
            for i in (1..roles.len()).rev() {
                let j = (role_rng.roll((i + 1) as u32) - 1) as usize;
                roles.swap(i, j);
            }
            for (entity, role) in ra.among.iter().zip(roles) {
                s.set_attribute(entity, &ra.key, role);
                // bookkeeping: 勝敗集計の正本。更新は ResolveVote の専権 (Phase C)。
                s.set_stat(entity, "生存", 1);
            }
            // 役職別の生存カウンタと優位 stat (goal の Gate が単体比較で読めるよう player に置く)。
            // {役職}優位 = 2×生存{役職}数 − 生存者数 (0 以上 = その役職が過半 = パリティ勝利条件)。
            // 更新はどちらも ResolveVote の専権 (Phase C)。
            let total = ra.among.len() as i64;
            s.set_stat(PLAYER, "生存者数", total);
            for (role, n) in &ra.pool {
                s.set_stat(PLAYER, &format!("生存{role}数"), i64::from(*n));
                s.set_stat(PLAYER, &format!("{role}優位"), 2 * i64::from(*n) - total);
            }
        }
        s
    }
}
