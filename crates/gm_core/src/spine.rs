//! シナリオ脊椎 (拘束)。beat/場所のグラフと gate 条件で、即興が筋から外れすぎないよう縛る。

use std::collections::{BTreeMap, BTreeSet};

use indexmap::IndexMap;

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

/// 主人公 stat の初期宣言 — **素の数値と境界つき宣言の両受け** (untagged)。
///
/// `initial_stats` は最初期からの素の i64 マップだったため、NPC (`CharacterDef.stats` =
/// StatDecl) と違い上限が付けられなかった (歴史的非対称)。CoC7 の SAN 上限 99 等で
/// 境界が必要になったので両受けにした — **既存 YAML (`hp: 10`) は無改修で従来どおり**
/// (min 0・max なし)、境界が要るときだけ `{ initial, min, max }` で書く。
/// `Location.items` の旧/新形式両受けと同じ後方互換パターン。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StatInit {
    /// 従来形: `hp: 10` (min 0・max なし)。
    Value(i64),
    /// 境界つき: `SAN: { initial: 60, min: 0, max: 99 }`。
    Decl(StatDecl),
}

impl StatInit {
    pub fn initial(&self) -> i64 {
        match self {
            StatInit::Value(v) => *v,
            StatInit::Decl(d) => d.initial,
        }
    }

    /// clamp 境界 `(min, max)`。従来形は既定 (0, なし) = 挙動不変。
    pub fn bounds(&self) -> (i64, Option<i64>) {
        match self {
            StatInit::Value(_) => (0, None),
            StatInit::Decl(d) => (d.min, d.max),
        }
    }
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
    /// IndexMap で YAML の記述順を保持する (表示順を「書いた順」にする)。
    #[serde(default)]
    pub stats: IndexMap<StatKey, StatDecl>,
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
    pub attributes: IndexMap<AttrKey, String>,
    /// 硬い禁忌: これが true になる delta を却下する (Phase B でエンジン強制)。
    #[serde(default)]
    pub taboos: Vec<Gate>,
    /// **このキャラの振り方テンプレート** (spec 18 Phase C)。「このキャラが振るときはこれ」を
    /// 一度書き、contest の `opponent_roll` が名前で参照する (同じキャラを別シナリオで
    /// 別の contest から使い回せる)。キー = テンプレート名 (例「噛みつき」「回避」)。
    #[serde(default)]
    pub rolls: BTreeMap<String, RollSpec>,
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
    /// 最小目 (ダイスが全部 1 = 素の合計 `roll == count`。1d なら出目 1、3d6 なら合計 3)。
    /// 大失敗 (fumble) の定番条件。`threshold` 不要。
    Min,
    /// 最大目 (全部が最大 = `roll == sides * count`)。大成功 (crit) の定番条件。`threshold` 不要。
    Max,
    /// `roll <= threshold`。下位帯を極にする (d100 で「20 以下は大失敗」等)。`threshold` 必須
    /// (`1..=count*sides`)。**`count >= 2` では合計の下限が `count` なのでそれ未満は不発火**。
    AtMost,
    /// `roll >= threshold`。上位帯を極にする (d100 で「96 以上は大成功」等)。`threshold` 必須
    /// (`1..=count*sides`)。**`count >= 2` では `count` 以下を書くと常時発火**。
    AtLeast,
}

/// 判定結果の極 (tier)。**作者が定義する** — どの自然出目で発火し、何を帰結フラグに立てるか。
/// `flag` は任意: tier を認識だけして帰結フラグを持たない極も書ける (例: 大成功で語りだけ変える)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierDef {
    /// この tier が発火する自然出目の極。
    pub natural: Natural,
    /// `at_most`/`at_least` の閾値 (`1..=count*sides`、[`Scenario::validate`] が検査)。
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
    /// 判定様式 (spec 16)。`additive` (既定) = 従来の `1d{sides}+stat >= dc` /
    /// `percentile` = 1d100 を目標値 (stat 現在値 + modifiers) **以下**で成功、成功度
    /// (degree) をエンジンが計算。percentile では `stat` 必須・`sides`/`dc` は書かない
    /// (validate が形を保証)。フラットフィールド (tier の `natural` と同流儀)。
    #[serde(default)]
    pub resolution: Resolution,
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
    /// **式修正** (spec 19)。stat の代わりに `(CON + SIZ) / 2` のような整数式で
    /// 修正値 (additive) / 目標値 (percentile) を書く。**判定のたびに現在値で評価**される
    /// (CON が削られれば補正も落ちる = 生きた派生値)。参照 stat は判定主体の宣言済みキー必須、
    /// `stat` との併記は不可 (validate `ChallengeExprInvalid`)。除算は切り捨て (CoC 準拠)。
    #[serde(default)]
    pub expr: Option<String>,
    /// additive のダイス面数。**serde default 0** (spec 16 で percentile が省略できるように) —
    /// additive で 0 は load 時 `ChallengeShapeInvalid` (従来の必須性を validate で保証)。
    #[serde(default)]
    pub sides: u32,
    /// additive のダイス個数 (既定 1)。`count: 3, sides: 20` で 3d20 の**合計**が出目になる。
    /// tier (極) は素の合計で判定 (min=全部 1、max=全部最大、threshold は 1..=count×sides)。
    #[serde(default = "default_dice_count")]
    pub count: u32,
    /// 出目の乗数 (既定 1)。`times: 5` で合計 ×5 (CoC 系の 3D6×5 等)。**出目だけに掛かる**
    /// (stat/modifiers の修正は乗算の後に加算)。percentile では書かない。
    #[serde(default = "default_dice_times")]
    pub times: i64,
    /// additive の難易度。percentile では書かない (validate `PercentileChallengeShape`)。
    #[serde(default)]
    pub dc: u32,
    /// 通常成功の帰結 (フラグ + 結末ナレーション、いずれも任意)。`flag` は `allowed_flags` 宣言必須。
    /// percentile では regular 以上の成功の受け皿 (degree 別スロットのフォールバック終点)。
    #[serde(default)]
    pub on_success: Option<ChallengeOutcome>,
    /// 通常失敗の帰結 (フラグ + 結末ナレーション、いずれも任意)。`flag` は `allowed_flags` 宣言必須。
    /// percentile では fumble のフォールバックも兼ねる。
    #[serde(default)]
    pub on_failure: Option<ChallengeOutcome>,
    /// degree=critical の帰結 (percentile 専用・任意)。フォールバック連鎖:
    /// critical は on_critical → on_extreme → on_hard → on_success の順で最初に在るものを使う。
    #[serde(default)]
    pub on_critical: Option<ChallengeOutcome>,
    /// degree=extreme の帰結 (percentile 専用・任意。extreme は on_extreme → on_hard → on_success)。
    #[serde(default)]
    pub on_extreme: Option<ChallengeOutcome>,
    /// degree=hard の帰結 (percentile 専用・任意。hard は on_hard → on_success)。
    #[serde(default)]
    pub on_hard: Option<ChallengeOutcome>,
    /// degree=fumble の帰結 (percentile 専用・任意。fumble は on_fumble → on_failure)。
    #[serde(default)]
    pub on_fumble: Option<ChallengeOutcome>,
    /// 極 (tier) の定義。キー = tier 名 (`crit_fail` 等)。自然出目の min/max で発火。
    /// 通常成否 (on_success/on_failure) と**併存**する。`CheckOutcome.tier` に surface する。
    /// **percentile とは併用不可** (`TierWithPercentile` — 二重クリティカルの曖昧さを load 時に弾く)。
    #[serde(default)]
    pub tiers: BTreeMap<String, TierDef>,
    /// **プッシュ可否** (spec 18 Phase B・opt-in)。true = 失敗した判定を 1 度だけ振り直す決断を
    /// プレイヤーに許す (CoC7 のプッシュロール)。**既定 false** — opt-in にする理由は二つ:
    /// ①既存 content の挙動 (帰結の即時適用・全帰結共通効果の射影 spec 09) を変えない
    /// ②SAN ロール等「一発勝負」が意図の challenge が黙って押せてしまう事故を作らない。
    #[serde(default)]
    pub pushable: Option<bool>,
    /// **差分買い可否** (spec 18 Phase B)。既定 true だが `Scenario.spend_rules` が無ければ
    /// そもそも買えない (scenario 側 opt-in が主・こちらは個別 challenge を締める弁)。
    #[serde(default)]
    pub spendable: Option<bool>,
    /// **プッシュして失敗した時の帰結** (任意)。解決連鎖: on_push_failure → (fumble なら
    /// on_fumble) → on_failure。「押した失敗はより悪い」を authored に書く場所 (CoC7 原典の
    /// 代償はここ — push_cost の stat 支払いは上乗せしたい時だけ)。
    #[serde(default)]
    pub on_push_failure: Option<ChallengeOutcome>,
}

impl ChallengeDef {
    /// 全帰結スロット (通常成否 + degree 別 + push 失敗) の走査。authored_only_flags /
    /// validate (フラグ宣言・効果検査) が共有する — スロット追加時の取りこぼしを一箇所で防ぐ。
    pub fn all_outcomes(&self) -> impl Iterator<Item = (&'static str, &ChallengeOutcome)> {
        [
            ("on_success", &self.on_success),
            ("on_failure", &self.on_failure),
            ("on_critical", &self.on_critical),
            ("on_extreme", &self.on_extreme),
            ("on_hard", &self.on_hard),
            ("on_fumble", &self.on_fumble),
            ("on_push_failure", &self.on_push_failure),
        ]
        .into_iter()
        .filter_map(|(label, o)| o.as_ref().map(|o| (label, o)))
    }
}

/// キャラの「振り方」1 つ (spec 18 Phase C)。additive なら `{count}d{sides}×times + stat + bonus`、
/// percentile (contest 側の宣言) なら `1d100 ≤ stat + bonus`。engine 非解釈の閉じた素性 —
/// LLM は選べず author だけが書く。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollSpec {
    /// 修正に使う stat (その entity が宣言済みであること)。省略 = 純粋な運試し (修正 0)。
    /// percentile では必須 (目標値の素。`expr` でも可)。
    #[serde(default)]
    pub stat: Option<StatKey>,
    /// 式修正 (spec 19)。stat の代わりに `(STR + SIZ) / 4` のような式で修正値/目標値を書く。
    /// `stat` との併記は不可。判定のたびに現在値で評価される。
    #[serde(default)]
    pub expr: Option<String>,
    /// additive の面数。percentile では書かない (無視される)。
    #[serde(default)]
    pub sides: u32,
    /// ダイス個数 (既定 1)。`count: 3, sides: 20` で 3d20 の合計。
    #[serde(default = "default_dice_count")]
    pub count: u32,
    /// 出目の乗数 (既定 1)。合計 ×times (修正 bonus/stat は乗算の後に加算)。
    #[serde(default = "default_dice_times")]
    pub times: i64,
    /// 常時修正 (additive: 出目に加算 / percentile: 目標値に加算)。
    #[serde(default)]
    pub bonus: i64,
}

fn default_dice_count() -> u32 {
    1
}
fn default_dice_times() -> i64 {
    1
}

impl Default for RollSpec {
    /// serde 既定と一致させる (count/times は 1 — std の derive だと 0 になり食い違う)。
    fn default() -> Self {
        Self { stat: None, expr: None, sides: 0, count: 1, times: 1, bonus: 0 }
    }
}

/// contest の振り方の参照: インライン定義 or テンプレート名 (`CharacterDef.rolls` のキー)。
/// テンプレートは「このキャラが振るときはこれ」をキャラファイル (inline キャラならシナリオ)
/// に一度書いて使い回す (2026-07-20 ユーザー決定)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RollRef {
    /// `CharacterDef.rolls` のテンプレート名 (player 側は不可 — player はキャラファイルを持たない)。
    Template(String),
    /// その場で書く振り方。
    Inline(RollSpec),
}

/// authored contest — **決着まで LLM を介さない対決** (spec 18 Phase C・一括型 cadence)。
/// 「1 交換 = 双方が振って比較 → 帰結適用」を `until` が真になるまで繰り返す。
/// ボス戦のように毎交換を GM に語らせたい場面は従来の challenge (逐次型) を使う —
/// **どちらの刻みで戦うかは作者専権** (LLM にもプレイヤーにも選ばせない)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContestDef {
    /// GM への提示文 (どんな対決か。`scenario_brief` に出して GM に開かせる)。
    #[serde(default)]
    pub description: String,
    /// 判定様式。**contest 単位で双方に適用** (様式跨ぎの対抗は構造的に存在しない)。
    #[serde(default)]
    pub resolution: Resolution,
    /// 相手の entity (既知のキャラ必須)。
    pub opponent: EntityId,
    /// player 側の振り方 (通常はインライン)。
    pub player_roll: RollRef,
    /// 相手側の振り方 (インライン or 相手キャラの `rolls` テンプレート名)。
    pub opponent_roll: RollRef,
    /// 開始できる前提条件 (Gate)。偽なら `attempt_contest` を却下。
    #[serde(default)]
    pub requires: Option<Gate>,
    /// 1 交換の帰結 (player 視点)。win = player 勝ち。flag/effects/narration/sound を毎交換適用。
    #[serde(default)]
    pub on_win: Option<ChallengeOutcome>,
    #[serde(default)]
    pub on_lose: Option<ChallengeOutcome>,
    /// 引き分けの帰結 (任意。percentile の同 degree は目標値の高い側が勝つ = CoC7 準拠なので、
    /// 真の引き分けは目標値まで同じ時だけ)。
    #[serde(default)]
    pub on_tie: Option<ChallengeOutcome>,
    /// 決着条件 (毎交換後に評価)。None = 1 交換で終わる単発対抗ロール。
    #[serde(default)]
    pub until: Option<Gate>,
    /// 交換回数の上限 (バックストップ = 無限対決を構造的に断つ)。既定 1。
    /// `until` を書く対決は 20〜30 程度を明示すること。
    #[serde(default = "default_max_rounds")]
    pub max_rounds: u32,
}

fn default_max_rounds() -> u32 {
    1
}

/// 差分買いの支払い規則 (spec 18 Phase B・scenario 単位の opt-in)。
/// 失敗した出目と目標の差分を `from` stat から `rate` 倍で支払い、成功に変える。
/// **engine は stat 名を解釈しない** — `幸運` は CoC7 が宣言した一例で、`所持金`/`霊力` でも
/// 同じ機構が動く (LUCK 固定変数を作らない、2026-07-20 ユーザー決定)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpendRules {
    /// 支払い元 stat (player の宣言済みキー必須 = `SpendStatUndeclared`)。
    pub from: StatKey,
    /// 差分 1 あたりの支払い量 (既定 1 = CoC7 の 1:1)。
    #[serde(default = "default_rate")]
    pub rate: i64,
}

fn default_rate() -> i64 {
    1
}

/// プッシュの代償 (spec 18 Phase B・任意)。既定 None = 原典どおり stat コスト無し
/// (代償は `on_push_failure` の帰結が本線)。書けば「押すのに hp/金を払う」が上乗せされる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushCost {
    /// 支払い元 stat (player の宣言済みキー必須 = `PushCostStatUndeclared`)。
    pub from: StatKey,
    pub amount: i64,
}

/// challenge の判定様式 (spec 16)。フィールドレス enum = YAML は素の文字列 (`resolution: percentile`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    /// 従来の加算式: `1d{sides} + stat修正 >= dc` で成功 (大きいほど良い)。
    #[default]
    Additive,
    /// d100 ロールアンダー: `1d100 <= 目標値 (stat + modifiers)` で成功 (低いほど良い)。
    /// 成功度 (degree) をエンジンが計算する。
    Percentile,
    /// **確定行動** (spec 21): ダイスを振らず `on_success` を必ず適用する。RNG も消費しない。
    ///
    /// 位置づけは「**LLM が起動できる authored 効果の束**」。LLM が authored 効果を起こす経路は
    /// `attempt_challenge` しかなく、`set_flag` → トリガー → **書き戻し**の定石は、その書き戻しで
    /// フラグが [`Scenario::authored_only_flags`] に落ちて LLM が起動できなくなる (起点が LLM の
    /// 時だけリセットが起点の権利を食い潰す)。装備・使用・切替のような**繰り返せる確定行動**は
    /// これで書く。閉世界は不変 — LLM は選ぶだけで、帰結は authored 側にある。
    None,
}

/// 盤面の判定様式スイッチ (spec 16)。engine の意味論には触れない**提示/語彙スイッチ** —
/// percentile なら LLM の op 語彙 (schema) から加算式 `check` を除外し `check_under` を露出
/// (additive では逆)、`scenario_brief` に「## 判定様式」節を接地する。engine は様式違いの
/// op も却下しない (様式は規約であって整合性ではない — 二層目は整合性の破れにだけ使う)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStyle {
    /// 加算式 (既定)。`check` を露出し `check_under` を隠す。
    #[default]
    Additive,
    /// d100 ロールアンダー。`check_under` を露出し `check` を隠す。
    Percentile,
}

/// 既成事実 (spec 20) に対する**ユーザーの書き込み権限**を盤面が縛る宣言 (spec 20 Phase E)。
///
/// 既成事実は検証されないテキストが毎ターン注入され、しかも注入ヘッダが GM に「呼称・約束・意図の
/// 一貫性には従え」と指示する = **ユーザーの既成事実は GM への指示**。TRPG 盤面では作者が設計した
/// 発見の順序を迂回できてしまう (engine は無傷でも語りが誘導される) ので、**誰が虚構を所有するか**
/// を盤面ごとに宣言する (三権分立の「シナリオが縛る」脚)。
///
/// **書き手はユーザーだけ** (2026-07-21 の収縮で GM の書き込み経路を撤去した) — ゆえに
/// `locked` は実質「機能オフ」で、既成事実は一件も生まれず注入する節も出ない。ゲーム性
/// (作者が設計した発見の順序) を守るための既定であって、裏で効いているわけではない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactsPolicy {
    /// **既定**。既成事実の欄を出さない (タブごと非表示)。
    ///
    /// ユーザーの宣言は**GM への指示**として毎ターン注入されるので、作者が設計した発見の
    /// 順序 (謎・段階開示) を語りで迂回できてしまう。宣言を持たない配布物 (= 書庫の既刊
    /// すべて) を安全側に置く (spec 18 の `pushable: false` 既定と同じ判断)。
    #[default]
    Locked,
    /// ユーザーが設定を宣言できる (追加・編集・削除)。キャラクター RP のように、
    /// 呼称・関係・約束をプレイヤーが決めてよい盤面向け。
    Open,
}

impl FactsPolicy {
    /// ユーザーが編集できるか。
    ///
    /// **二値で足りる** (2026-07-21): 当初は「削除のみ (prune)」を中間値に置いたが、あれは
    /// **GM も書く**前提の設計だった (GM の誤記憶を消す = 加算を封じて減算だけ許す非対称)。
    /// GM の書き込み経路を撤去した今、書き手はユーザーだけ — 足せないものは消せないので
    /// prune は空虚な状態になり撤去した (failures.md #66)。
    pub fn allows_write(self) -> bool {
        matches!(self, FactsPolicy::Open)
    }
    /// ユーザーが削除できるか (書けるなら消せる)。
    pub fn allows_delete(self) -> bool {
        self.allows_write()
    }
    /// 既成事実をプレイヤーに見せるか (`locked` は隠す)。
    pub fn is_visible(self) -> bool {
        !matches!(self, FactsPolicy::Locked)
    }
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
    /// `epilogue_prompt` を書いた goal に結末文 (`narration`) が無い (None/空文字/空白のみ =
    /// `trim().is_empty()` 基準)。narration はエピローグ生成失敗時のフォールバックであり、
    /// 「封印か討伐か死か」という結末の意味を生成に伝える接地素材 — 無いと生成失敗時に
    /// バナーだけの幕になる。**lint** (プレイは壊れない = load 拒否しない。spec 11)。
    EpilogueWithoutNarration { goal: GoalId },
    /// Gate の `location_is` が**宣言されていない場所**を指している (spec 21 同梱)。
    /// `state.location` と一致しようがないので、その Gate は**永久に false** — challenge の
    /// `requires` に書けば一度も選べず、出口 gate に書けば通れない。しかもエラーも警告も
    /// 出ないので作者は気づけない。実例: LLM 生成 content の `{ location_is, at: inventory }`
    /// (所持品を場所と誤認)。**lint** — 壊れた盤面でもプレイは続くので load は拒否しない。
    /// `origin` は `challenge:{id}` / `trigger:{id}` / `exit:{from}->{to}` 等の場所名。
    UnknownLocationInGate { origin: String, at: LocationId },
    /// `flag_titles` のキーが `allowed_flags` に宣言されていない (幻フラグへの表示名)。
    FlagTitleUndeclared { flag: FlagKey },
    /// `hidden_flags` のキーが `allowed_flags` に宣言されていない (幻フラグの秘匿)。
    HiddenFlagUndeclared { flag: FlagKey },
    /// `internal_flags` のキーが `allowed_flags` に宣言されていない (幻フラグの帳簿指定)。
    InternalFlagUndeclared { flag: FlagKey },
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
    /// contest の相手 (`opponent`) が既知の entity でない (幻の対戦相手。spec 18 Phase C)。
    ContestOpponentUnknown { contest: String, entity: EntityId },
    /// contest の振り方が解決できない (テンプレート不在 / stat 未宣言 / percentile なのに
    /// stat 無し / additive なのに sides 0。spec 18 Phase C)。
    ContestRollInvalid {
        contest: String,
        entity: EntityId,
        detail: String,
    },
    /// contest の帰結フラグが `allowed_flags` に無い (幻フラグ。spec 18 Phase C)。
    ContestFlagUndeclared { contest: String, flag: FlagKey },
    /// `spend_rules.from` が player の宣言済み stat でない (幻の財布。spec 18 Phase B)。
    SpendStatUndeclared { key: StatKey },
    /// `push_cost.from` が player の宣言済み stat でない (幻の代償元。spec 18 Phase B)。
    PushCostStatUndeclared { key: StatKey },
    /// tier の `at_most`/`at_least` 閾値が `1..=count*sides` の範囲外 (常時発火/絶対不発火の幻値)。
    /// `sides` フィールドには実際に検査した上限 (`count*sides`) が載る。
    TierThresholdOutOfRange {
        challenge: ChallengeId,
        tier: String,
        threshold: u32,
        sides: u32,
    },
    /// additive challenge の `sides` が 0 (spec 16 で serde default 化した従来必須フィールドの
    /// 欠落 = 壊れた挑戦を実行経路に乗せない)。
    ChallengeShapeInvalid { challenge: ChallengeId },
    /// percentile challenge の形が不正 (spec 16): `stat` 欠落 / `sides`・`dc` の指定
    /// (加算式との混同)。`detail` が何が悪いかを名指しする。
    PercentileChallengeShape { challenge: ChallengeId, detail: String },
    /// 確定行動 (`resolution: none`、spec 21) に判定用フィールドが書かれている。
    /// 判定が無い以上どれも無意味なので load 時に弾く (壊れた宣言を実行経路に乗せない)。
    CertainActionShape { challenge: ChallengeId, detail: String },
    /// challenge の式修正 (spec 19) が不正: パース不能 / `stat` と併記 / 参照 stat 未宣言 /
    /// リテラルのゼロ除算。`detail` が何が悪いかを名指しする。
    ChallengeExprInvalid { challenge: ChallengeId, detail: String },
    /// percentile challenge に `tiers` が併記されている (spec 16)。自然出目帯と degree の
    /// 二重クリティカルは authored 意図が曖昧 — percentile の極は degree スロットで書く。
    TierWithPercentile { challenge: ChallengeId },
    /// trigger/challenge effects の `roll_stat` が `count == 0` か `sides == 0` (ゼロダイス)。
    /// `origin` は `trigger:{id}` / `challenge:{id}` の形で場所を名指しする。
    RollStatShapeInvalid { origin: String, key: StatKey },
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
    /// エピローグの**生成指示** (authored、非検証の語り素材。spec 11)。engine は解釈しない。
    /// 到達がセッションの終端 (単発 goal / campaign の辺なし = 判定は呼び出し側) のとき、
    /// harness/app がこの指示 + 旅路の記録 (synopsis/chronicle) で GM にエピローグを 1 回
    /// 書かせる。**本文ではない** (生成本文は提示層の TurnView.epilogue)。
    /// None / 空白のみ = エピローグなし (従来どおり即結末)。`narration` はエピローグの土台
    /// (生成失敗時のフォールバック + 結末の意味の接地素材) なので廃止しない —
    /// 指示だけ書いて結末文が無いと [`ScenarioError::EpilogueWithoutNarration`] が lint 警告する。
    #[serde(default)]
    pub epilogue_prompt: Option<String>,
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
    /// **プレイヤーには隠すが GM は見る秘密のフラグ** (`hidden_attributes` のフラグ版、2026-07-19
    /// 命名整理)。裏で進む真相・隠し進行のような**プレイヤー UI に出したくないが GM は追う**フラグ。
    /// 提示層の扱い: プレイヤー UI = 出さない / `state_brief` = **〔秘匿〕注記付きで GM に見せる** /
    /// set_flag 語彙節 = 出さない (GM に casually 立てさせない)。GM_SYSTEM が「〔秘匿〕は明かすな」を
    /// 刷り込む。engine 非使用・非検証 — gate/トリガーの評価は不変。キーは `allowed_flags` 宣言必須。
    /// (「GM にも見せない engine 帳簿」は [`Self::internal_flags`]。)
    #[serde(default)]
    pub hidden_flags: BTreeSet<FlagKey>,
    /// **GM もプレイヤーも見ない engine 内部の帳簿フラグ** (タイマーの armed フラグ `x_done` 等、
    /// 2026-07-19 命名整理で `hidden_flags` の旧義を分離)。**変数として使う**フラグを全提示層
    /// (UI フラグ一覧 / `state_brief` / 語彙節) が一切出さない宣言。engine 非使用・非検証 —
    /// gate/トリガーの評価は不変で効く。キーは `allowed_flags` 宣言必須。
    /// (「プレイヤーには隠すが GM は見る秘密」は [`Self::hidden_flags`]。)
    #[serde(default)]
    pub internal_flags: BTreeSet<FlagKey>,
    /// 盤面の判定様式 (spec 16、既定 additive)。percentile なら提示層が op 語彙を
    /// check → check_under に入れ替え、「## 判定様式」を接地する。詳細は [`CheckStyle`]。
    #[serde(default)]
    pub check_style: CheckStyle,
    /// 既成事実 (spec 20) のユーザー書き込み権限 (既定 `locked` = 非表示)。
    /// package.yaml の `facts_policy` から注入もできる。詳細は [`FactsPolicy`]。
    #[serde(default)]
    pub facts_policy: FactsPolicy,
    /// 読み上げ (TTS) を**作者が想定しているか** (既定 false)。engine 非使用・非検証の
    /// **提示層宣言** ([`Self::hidden_stats`] と同類) — 正本も prompt も語りも一切変わらず、
    /// true の盤面でだけ提示層が読み上げ操作を出す。
    ///
    /// 意味は「技術的に読めるか」ではない (narration は常に文字列なので全パッケージが読める)。
    /// 既定 false は、宣言を持たない配布物を作者の意図どおり無音に置くため。
    ///
    /// **文体は決めない**: TTS の ON/OFF で語りが変わると chronicle/synopsis に残る記録まで
    /// 再生設定で食い違う。音声前提の会話文体は `world` に書く (作者がパッケージ固有に決め、
    /// TTS はその上の再生手段、と分離する)。package.yaml の `use_tts` から注入もできる。
    #[serde(default)]
    pub use_tts: bool,
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
    pub initial_stats: IndexMap<StatKey, StatInit>,
    /// **プレイヤーには隠すが GM は見る秘密の数値** (2026-07-19 命名整理)。隠し好感度・裏の腐敗値の
    /// ような**プレイヤー UI に出したくないが GM は追う**数値。提示層の扱い: UI の状態パネル = 出さない /
    /// CLI = 出さない / prompt の `state_brief` = **〔秘匿〕注記付きで GM に見せる**。GM_SYSTEM が
    /// 「〔秘匿〕は明かすな」を刷り込む。**engine は使わない提示ヒント** (非検証)。全 entity に効く。
    /// (「GM にも見せない engine 帳簿」は [`Self::internal_stats`]。)
    #[serde(default)]
    pub hidden_stats: BTreeSet<StatKey>,
    /// **GM もプレイヤーも見ない engine 内部の帳簿 stat** (タイマー `record_turn` の刻みや repeatable
    /// カウンタ、2026-07-19 命名整理で `hidden_stats` の旧義を分離)。engine 内部値を全提示層
    /// (UI の状態パネル / prompt の state_brief / CLI) が skip する。**engine は使わない提示ヒント**
    /// (非検証・キーは開集合ゆえ宣言不要)。全 entity に効く。
    /// (「プレイヤーには隠すが GM は見る秘密」は [`Self::hidden_stats`]。)
    #[serde(default)]
    pub internal_stats: BTreeSet<StatKey>,
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
    pub initial_attributes: IndexMap<AttrKey, String>,
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
    /// authored contest (対決) の定義 (spec 18 Phase C)。キー = contest id。
    /// 決着まで LLM を介さない一括型 cadence — 雑魚戦・単発対抗ロール用。
    #[serde(default)]
    pub contests: BTreeMap<String, ContestDef>,
    /// 差分買いの支払い規則 (spec 18 Phase B・opt-in)。None = この盤面に差分買いは無い。
    /// Some で `pushable`/`spendable` な challenge の失敗が「stat を払って成功に変える」決断になる。
    #[serde(default)]
    pub spend_rules: Option<SpendRules>,
    /// プッシュの代償 (spec 18 Phase B・任意)。None = 原典どおり無償
    /// (代償は各 challenge の `on_push_failure`)。
    #[serde(default)]
    pub push_cost: Option<PushCost>,
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

    /// entity の stat を **authored 宣言順 (YAML の記述順)** で返す (表示用)。実行時 `GameState`
    /// は BTreeMap ゆえ順序を持たない — 状態パネルを「書いた順」に並べるため、提示層が
    /// 宣言側 (`initial_stats` / `CharacterDef::stats`、IndexMap で記述順保持) を参照する。
    /// engine の意味論 (gate/validate/seeding) はキー lookup で順序非依存ゆえ影響なし。
    pub fn stat_order(&self, entity: &str) -> Vec<StatKey> {
        if entity == PLAYER {
            self.initial_stats.keys().cloned().collect()
        } else {
            self.characters
                .get(entity)
                .map(|c| c.stats.keys().cloned().collect())
                .unwrap_or_default()
        }
    }

    /// entity の attribute を **authored 宣言順** で返す ([`Self::stat_order`] の attribute 版)。
    pub fn attribute_order(&self, entity: &str) -> Vec<AttrKey> {
        if entity == PLAYER {
            self.initial_attributes.keys().cloned().collect()
        } else {
            self.characters
                .get(entity)
                .map(|c| c.attributes.keys().cloned().collect())
                .unwrap_or_default()
        }
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

    pub fn contest(&self, id: &str) -> Option<&ContestDef> {
        self.contests.get(id)
    }

    /// contest の振り方参照を実体へ解決する (spec 18 Phase C)。Inline はそのまま、
    /// Template は相手キャラの `rolls` から引く (player はキャラファイルを持たないので
    /// Template 不可 = None。validate が load 時に名指しする)。
    pub fn resolve_roll(&self, entity: &str, rref: &RollRef) -> Option<RollSpec> {
        match rref {
            RollRef::Inline(spec) => Some(spec.clone()),
            RollRef::Template(name) => {
                self.characters.get(entity).and_then(|c| c.rolls.get(name)).cloned()
            }
        }
    }

    /// **authored 専権フラグ** — トリガー効果・challenge 帰結 (on_success/on_failure/tier の
    /// `.flag` 欄 **と `effects` 内の `set_flag`**) が engine 経由で書くフラグ。LLM が set_flag
    /// すべきでない (立てても筋書きの先取り＝ノイズ)。宣言の走査だけで機械的に判別できる。
    /// `filter_authored_only_ops` (op の構造的遮断) のフラグ版。
    /// (#51: 当初 challenge 側は `.flag` 欄しか走査しておらず、effects 経由の set_flag が
    /// usable 語彙にも #50 バックストップにも漏れていた — 書くフラグの全経路を舐めること。)
    pub fn authored_only_flags(&self) -> BTreeSet<FlagKey> {
        let mut set = BTreeSet::new();
        // StateOp 列から set_flag の書き先を拾う (trigger/challenge の effects 共通)。
        fn collect_setflags(ops: &[StateOp], set: &mut BTreeSet<FlagKey>) {
            for op in ops {
                if let StateOp::SetFlag { key, .. } = op {
                    set.insert(key.clone());
                }
            }
        }
        for t in &self.triggers {
            collect_setflags(&t.effects, &mut set);
        }
        for c in self.challenges.values() {
            // 全帰結スロット (degree 別 + push 失敗も含む) — 従来 on_success/on_failure のみで
            // percentile の degree スロットが漏れていた (#50 バックストップの穴) のを併せて閉じる。
            for (_, outcome) in c.all_outcomes() {
                if let Some(flag) = &outcome.flag {
                    set.insert(flag.clone());
                }
                collect_setflags(&outcome.effects, &mut set);
            }
            for tier in c.tiers.values() {
                if let Some(flag) = &tier.flag {
                    set.insert(flag.clone());
                }
                collect_setflags(&tier.effects, &mut set);
            }
        }
        // contest の帰結 (spec 18 Phase C) も authored 専権 — GM は set_flag できない。
        for c in self.contests.values() {
            for outcome in [&c.on_win, &c.on_lose, &c.on_tie].into_iter().flatten() {
                if let Some(flag) = &outcome.flag {
                    set.insert(flag.clone());
                }
                collect_setflags(&outcome.effects, &mut set);
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
        // spec 11: エピローグ指示があるのに結末文が無い goal — 生成失敗時のフォールバックが
        // 存在せず「バナーだけの幕」になる。空の定義は trim().is_empty()
        // (空白だけの指示は「書いていない」扱いで沈黙)。
        for g in &self.goals {
            let has_prompt = g.epilogue_prompt.as_deref().is_some_and(|p| !p.trim().is_empty());
            if has_prompt && g.narration.trim().is_empty() {
                warns.push(ScenarioError::EpilogueWithoutNarration { goal: g.id.clone() });
            }
        }
        // spec 21 同梱: 幻の場所を指す location_is (永久に false = 死んだ Gate)。
        // Gate は入れ子 (all/any/not) を取るので再帰で葉まで舐める。
        fn scan_gate(g: &Gate, origin: &str, known: &BTreeSet<LocationId>, out: &mut Vec<ScenarioError>) {
            match g {
                Gate::LocationIs { at } if !known.contains(at) => {
                    out.push(ScenarioError::UnknownLocationInGate {
                        origin: origin.to_string(),
                        at: at.clone(),
                    });
                }
                Gate::All { of } | Gate::Any { of } => {
                    for sub in of {
                        scan_gate(sub, origin, known, out);
                    }
                }
                _ => {}
            }
        }
        let known: BTreeSet<LocationId> = self.locations.keys().cloned().collect();
        for (cid, def) in &self.challenges {
            if let Some(req) = &def.requires {
                scan_gate(req, &format!("challenge:{cid}"), &known, &mut warns);
            }
            for m in &def.modifiers {
                scan_gate(&m.when, &format!("challenge:{cid}"), &known, &mut warns);
            }
        }
        for t in &self.triggers {
            scan_gate(&t.when, &format!("trigger:{}", t.id), &known, &mut warns);
        }
        for (key, gate) in &self.flag_rules {
            scan_gate(gate, &format!("flag_rules:{key}"), &known, &mut warns);
        }
        for (from, loc) in &self.locations {
            for ex in &loc.exits {
                scan_gate(&ex.gate, &format!("exit:{from}->{}", ex.to), &known, &mut warns);
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
            // 判定様式の形 (spec 16): additive は sides 必須 (serde default 0 の欠落を弾く) /
            // percentile は stat 必須・sides/dc 禁止 (加算式との混同を名指し)・tiers 禁止
            // (自然出目帯と degree の二重クリティカルは authored 意図が曖昧)。
            match def.resolution {
                Resolution::Additive => {
                    // sides 0 (欠落) / count 0 (ゼロダイス) / times < 1 (出目が常に 0 以下) は
                    // 壊れた挑戦 — 実行経路に乗せない。
                    if def.sides == 0 || def.count == 0 || def.times < 1 {
                        errs.push(ScenarioError::ChallengeShapeInvalid { challenge: cid.clone() });
                    }
                }
                Resolution::Percentile => {
                    if def.stat.is_none() && def.expr.is_none() {
                        errs.push(ScenarioError::PercentileChallengeShape {
                            challenge: cid.clone(),
                            detail: "stat (目標値に使う技能) か expr (式) が必須".into(),
                        });
                    }
                    if def.sides != 0 || def.dc != 0 {
                        errs.push(ScenarioError::PercentileChallengeShape {
                            challenge: cid.clone(),
                            detail: "sides/dc は書かない (d100 と目標値=stat が様式で確定)".into(),
                        });
                    }
                    if def.count != 1 || def.times != 1 {
                        errs.push(ScenarioError::PercentileChallengeShape {
                            challenge: cid.clone(),
                            detail: "count/times は書かない (percentile は 1d100 固定)".into(),
                        });
                    }
                    if !def.tiers.is_empty() {
                        errs.push(ScenarioError::TierWithPercentile { challenge: cid.clone() });
                    }
                }
                // 確定行動 (spec 21): 判定が無い以上、判定用フィールドはすべて無意味。
                // 書かれていたら「振るつもりで書いたのに振られない」ので load 時に名指しする。
                Resolution::None => {
                    let mut bad = Vec::new();
                    if def.sides != 0 || def.dc != 0 {
                        bad.push("sides/dc");
                    }
                    if def.count != 1 || def.times != 1 {
                        bad.push("count/times");
                    }
                    if def.stat.is_some() || def.expr.is_some() {
                        bad.push("stat/expr");
                    }
                    if !def.modifiers.is_empty() {
                        bad.push("modifiers");
                    }
                    if !def.tiers.is_empty() {
                        bad.push("tiers");
                    }
                    if def.on_failure.is_some() {
                        bad.push("on_failure");
                    }
                    // degree 別スロット (percentile 用) も判定の産物なので無意味。
                    if def.on_critical.is_some()
                        || def.on_extreme.is_some()
                        || def.on_hard.is_some()
                        || def.on_fumble.is_some()
                    {
                        bad.push("degree スロット (on_critical/on_extreme/on_hard/on_fumble)");
                    }
                    // 決断 (spec 18) は「失敗の後にもう一度」の機構 — 失敗が無いので無意味。
                    if def.pushable.is_some() || def.on_push_failure.is_some() {
                        bad.push("pushable/on_push_failure");
                    }
                    if !bad.is_empty() {
                        errs.push(ScenarioError::CertainActionShape {
                            challenge: cid.clone(),
                            detail: format!(
                                "確定行動 (resolution: none) は判定しないので {} は書かない",
                                bad.join(" / ")
                            ),
                        });
                    }
                }
            }
            // tier (極) と全帰結スロット (通常成否 + degree 別 + push 失敗) が立てるフラグは
            // allowed_flags 宣言必須 (走査は all_outcomes に集約 — スロット追加の取りこぼし防止)。
            let outcome_flags = def
                .tiers
                .iter()
                .filter_map(|(tname, tier)| tier.flag.as_ref().map(|f| (tname.as_str(), f)))
                .chain(def.all_outcomes().filter_map(|(label, o)| o.flag.as_ref().map(|f| (label, f))));
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
            // at_most/at_least は threshold が必須かつ 1..=count*sides の範囲内であること
            // (欠落=無制限、範囲外=常時発火/絶対不発火の幻値。load 時に弾く)。min/max は threshold 不要。
            // percentile は tiers 自体を禁止済みなので additive のみ検査 (sides=0 での誤報を避ける)。
            if def.resolution == Resolution::Additive {
                for (tname, tier) in &def.tiers {
                    if matches!(tier.natural, Natural::AtMost | Natural::AtLeast) {
                        // 欠落は 0 として報告 (範囲外なので同じ経路で弾かれる)。判定は素の
                        // **合計** (count 個の和) なので範囲は 1..=count×sides。
                        let n = tier.threshold.unwrap_or(0);
                        let max = def.sides.saturating_mul(def.count.max(1));
                        if n < 1 || n > max {
                            errs.push(ScenarioError::TierThresholdOutOfRange {
                                challenge: cid.clone(),
                                tier: tname.clone(),
                                threshold: n,
                                sides: max,
                            });
                        }
                    }
                }
            }
            // 式修正 (spec 19): パース可能・stat と非併記・参照 stat が判定主体で宣言済み。
            // 主体は authored 固定 (entity) があればそれ、無ければ player を既定として検査する
            // (op の entity 上書きで別主体になった場合は裁定時の UnknownStat が二層目)。
            if let Some(xsrc) = &def.expr {
                if def.stat.is_some() {
                    errs.push(ScenarioError::ChallengeExprInvalid {
                        challenge: cid.clone(),
                        detail: "stat と expr は同時に書けない (どちらか一方)".into(),
                    });
                }
                match crate::expr::parse_expr(xsrc) {
                    Err(e) => errs.push(ScenarioError::ChallengeExprInvalid {
                        challenge: cid.clone(),
                        detail: e,
                    }),
                    Ok(x) => {
                        let subject = def.entity.clone().unwrap_or_else(|| PLAYER.to_string());
                        for key in x.stats() {
                            if !self.knows_stat(&subject, &key) {
                                errs.push(ScenarioError::ChallengeExprInvalid {
                                    challenge: cid.clone(),
                                    detail: format!("式が参照する stat '{key}' が {subject} に宣言されていない"),
                                });
                            }
                        }
                    }
                }
            }
            // 帰結の直接効果 (effects): attempt_challenge の入れ子は無限再帰の芽なので弾く。
            // set_attribute の幻キーもトリガー効果と同じ検査 (trigger 欄に challenge:{id})。
            let effect_lists = def
                .tiers
                .values()
                .map(|t| &t.effects)
                .chain(def.all_outcomes().map(|(_, o)| &o.effects));
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
                        // ゼロダイス (振れない/常に bonus だけ) は書き間違い — load 時に名指し。
                        StateOp::RollStat { key, count, sides, .. } if *count == 0 || *sides == 0 => {
                            errs.push(ScenarioError::RollStatShapeInvalid {
                                origin: format!("challenge:{cid}"),
                                key: key.clone(),
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
        // contest (spec 18 Phase C): 相手・振り方・帰結フラグの閉世界検査。
        for (cid, def) in &self.contests {
            if !self.knows_entity(&def.opponent) {
                errs.push(ScenarioError::ContestOpponentUnknown {
                    contest: cid.clone(),
                    entity: def.opponent.clone(),
                });
            }
            // 双方の振り方が実体へ解決でき、様式に対して健全であること。
            for (entity, rref) in
                [(PLAYER.to_string(), &def.player_roll), (def.opponent.clone(), &def.opponent_roll)]
            {
                let Some(spec) = self.resolve_roll(&entity, rref) else {
                    let name = match rref {
                        RollRef::Template(n) => n.clone(),
                        RollRef::Inline(_) => "(inline)".into(),
                    };
                    errs.push(ScenarioError::ContestRollInvalid {
                        contest: cid.clone(),
                        entity: entity.clone(),
                        detail: format!("振り方テンプレート '{name}' が見つからない (player はインラインのみ)"),
                    });
                    continue;
                };
                if let Some(stat) = &spec.stat {
                    if !self.knows_stat(&entity, stat) {
                        errs.push(ScenarioError::ContestRollInvalid {
                            contest: cid.clone(),
                            entity: entity.clone(),
                            detail: format!("stat '{stat}' が宣言されていない"),
                        });
                    }
                }
                // 式修正 (spec 19): パース可能・stat と非併記・参照 stat 宣言済み。
                if let Some(xsrc) = &spec.expr {
                    if spec.stat.is_some() {
                        errs.push(ScenarioError::ContestRollInvalid {
                            contest: cid.clone(),
                            entity: entity.clone(),
                            detail: "stat と expr は同時に書けない".into(),
                        });
                    }
                    match crate::expr::parse_expr(xsrc) {
                        Err(e) => errs.push(ScenarioError::ContestRollInvalid {
                            contest: cid.clone(),
                            entity: entity.clone(),
                            detail: e,
                        }),
                        Ok(x) => {
                            for key in x.stats() {
                                if !self.knows_stat(&entity, &key) {
                                    errs.push(ScenarioError::ContestRollInvalid {
                                        contest: cid.clone(),
                                        entity: entity.clone(),
                                        detail: format!("式が参照する stat '{key}' が宣言されていない"),
                                    });
                                }
                            }
                        }
                    }
                }
                match def.resolution {
                    Resolution::Percentile if spec.stat.is_none() && spec.expr.is_none() => {
                        errs.push(ScenarioError::ContestRollInvalid {
                            contest: cid.clone(),
                            entity: entity.clone(),
                            detail: "percentile の振り方には stat (か expr) が必須".into(),
                        });
                    }
                    Resolution::Percentile if spec.count != 1 || spec.times != 1 => {
                        errs.push(ScenarioError::ContestRollInvalid {
                            contest: cid.clone(),
                            entity: entity.clone(),
                            detail: "count/times は書かない (percentile は 1d100 固定)".into(),
                        });
                    }
                    Resolution::Additive if spec.sides == 0 || spec.count == 0 || spec.times < 1 => {
                        errs.push(ScenarioError::ContestRollInvalid {
                            contest: cid.clone(),
                            entity: entity.clone(),
                            detail: "additive の振り方は sides/count 1 以上・times 1 以上".into(),
                        });
                    }
                    _ => {}
                }
            }
            // 帰結フラグは allowed_flags 宣言必須 (challenge と同じ閉世界)。
            for outcome in [&def.on_win, &def.on_lose, &def.on_tie].into_iter().flatten() {
                if let Some(flag) = &outcome.flag {
                    if !self.allowed_flags.contains(flag) {
                        errs.push(ScenarioError::ContestFlagUndeclared {
                            contest: cid.clone(),
                            flag: flag.clone(),
                        });
                    }
                }
                // 帰結 effects の attempt_challenge/attempt_contest 入れ子は再帰の芽なので弾く。
                for op in &outcome.effects {
                    if matches!(op, StateOp::AttemptChallenge { .. } | StateOp::AttemptContest { .. }) {
                        errs.push(ScenarioError::ChallengeEffectRecursive { challenge: cid.clone() });
                    }
                }
            }
        }
        // 決断の支払い元 (spec 18 Phase B): player の宣言済み stat 必須 (幻の財布/代償元)。
        if let Some(sr) = &self.spend_rules {
            if !self.knows_stat(PLAYER, &sr.from) {
                errs.push(ScenarioError::SpendStatUndeclared { key: sr.from.clone() });
            }
        }
        if let Some(pc) = &self.push_cost {
            if !self.knows_stat(PLAYER, &pc.from) {
                errs.push(ScenarioError::PushCostStatUndeclared { key: pc.from.clone() });
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
        // 帳簿宣言 (internal_flags) も同様 (幻フラグの帳簿指定を弾く)。internal_stats は
        // hidden_stats と同じく無検証 (stat キーは開集合ゆえ宣言不要)。
        for flag in &self.internal_flags {
            if !self.allowed_flags.contains(flag) {
                errs.push(ScenarioError::InternalFlagUndeclared { flag: flag.clone() });
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
                // 可変量ダイス (spec 16) のゼロダイスも load 時に名指し (challenge 側と同じ検査)。
                if let StateOp::RollStat { key, count, sides, .. } = op {
                    if *count == 0 || *sides == 0 {
                        errs.push(ScenarioError::RollStatShapeInvalid {
                            origin: format!("trigger:{}", trig.id),
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
    /// 主人公は `initial_stats` の境界つき宣言 (StatInit::Decl) を読む — 従来形 (素の数値) は
    /// 既定 (0, なし) のままで挙動不変。
    pub fn stat_bounds(&self, entity: &str, key: &str) -> (i64, Option<i64>) {
        if entity == PLAYER {
            if let Some(init) = self.initial_stats.get(key) {
                return init.bounds();
            }
        }
        if let Some(decl) = self.characters.get(entity).and_then(|c| c.stats.get(key)) {
            (decl.min, decl.max)
        } else {
            (0, None)
        }
    }

    /// 開始地点・全キャラの初期 stat から初期状態を作る。
    pub fn initial_state(&self, seed: u64) -> GameState {
        let mut s = GameState::new(self.start.clone(), seed);
        // 主人公の糖衣 (境界つき宣言は initial を読む)。
        for (k, v) in &self.initial_stats {
            s.set_stat(PLAYER, k, v.initial());
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 【表示順 (stat/attr display order)】stats/attributes は YAML の記述順を保持する
    /// (authored 宣言を IndexMap 化)。BTreeMap 時代はアルファベット順に潰れていた — 状態パネルを
    /// 「書いた順」で表示するための土台。engine の意味論はキー lookup で順序非依存ゆえ不変。
    #[test]
    fn stat_and_attribute_order_follow_yaml_declaration() {
        // わざとアルファベット順でない順で宣言する (hp が先頭なら alpha 順、末尾なら記述順)。
        let yaml = "title: t\nstart: room\n\
            initial_stats: { 観察力: 6, 共感力: 6, hp: 10 }\n\
            initial_attributes: { 役割: 探偵, 年齢: \"17\" }\n\
            locations: { room: {} }\n";
        let sc = Scenario::from_yaml(yaml).expect("parse");
        assert_eq!(
            sc.stat_order(PLAYER),
            vec!["観察力".to_string(), "共感力".to_string(), "hp".to_string()],
            "stat は記述順を保つ (アルファベット順 hp/共感力/観察力 ではない)"
        );
        assert_eq!(
            sc.attribute_order(PLAYER),
            vec!["役割".to_string(), "年齢".to_string()],
            "attribute も記述順を保つ"
        );
    }
}
