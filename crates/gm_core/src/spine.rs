//! シナリオ脊椎 (拘束)。beat/場所のグラフと gate 条件で、即興が筋から外れすぎないよう縛る。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::state::{
    default_entity, ChallengeId, EntityId, FlagKey, GameState, ItemId, LocationId, SkillId,
    StateOp, StatKey, TriggerId, PLAYER,
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
    /// 指定キャラが能力を獲得済みである (能力条件)。閉世界: 宣言/開花した能力のみ true。`entity` 省略時は主人公。
    HasSkill {
        #[serde(default = "default_entity")]
        entity: EntityId,
        skill: SkillId,
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
            Gate::HasSkill { entity, skill } => s.has_skill(entity, skill),
            Gate::All { of } => of.iter().all(|g| g.eval(s)),
            Gate::Any { of } => of.iter().any(|g| g.eval(s)),
        }
    }
}

fn default_gate() -> Gate {
    Gate::Always
}

/// 場所からの出口。`gate` 未達なら [`Gate::Always`]。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exit {
    pub to: LocationId,
    #[serde(default = "default_gate")]
    pub gate: Gate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    #[serde(default)]
    pub description: String,
    /// 拾得可能なアイテム → それを拾うための gate。
    #[serde(default)]
    pub items: BTreeMap<ItemId, Gate>,
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
}

/// tier がどの**自然出目** (修正前の素の `1d{sides}`) で発火するか。
/// `sides` 相対なので die サイズに依存しない (`max` は d6→6, d20→20)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Natural {
    /// 最小目 (`roll == 1`)。大失敗 (fumble) の定番条件。
    Min,
    /// 最大目 (`roll == sides`)。大成功 (crit) の定番条件。
    Max,
}

/// 判定結果の極 (tier)。**作者が定義する** — どの自然出目で発火し、何を帰結フラグに立てるか。
/// `flag` は任意: tier を認識だけして帰結フラグを持たない極も書ける (例: 大成功で語りだけ変える)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierDef {
    /// この tier が発火する自然出目の極。
    pub natural: Natural,
    /// 該当時に engine が直書きする帰結フラグ (任意)。`allowed_flags` 宣言必須 ([`Scenario::validate`])。
    #[serde(default)]
    pub flag: Option<FlagKey>,
}

/// authored challenge。**判定の素性と帰結を作者が握る閉じた定義**。
/// LLM は [`StateOp::AttemptChallenge`] で challenge を**選ぶ**だけで、ここを author できない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChallengeDef {
    /// 修正に使う stat (挑戦する entity が宣言済みであること)。
    pub stat: StatKey,
    pub sides: u32,
    pub dc: u32,
    /// 極 (tier) の定義。キー = tier 名 (`crit_fail` 等)。`CheckOutcome.tier` に surface する。
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
}

/// シナリオ全体。`scenarios/*.yaml` から読み込まれる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scenario {
    #[serde(default)]
    pub title: String,
    pub start: LocationId,
    #[serde(default)]
    pub allowed_flags: BTreeSet<FlagKey>,
    /// **世界フラグ** (campaign 横断で持ち越す)。`allowed_flags` の部分集合。
    /// [`Scenario::transition`] はこれに挙げたフラグだけ次モジュールへ運び、残り (局所) は捨てる。
    #[serde(default)]
    pub global_flags: BTreeSet<FlagKey>,
    /// フラグを true にするための gate。記載なければ [`Gate::Always`]。
    #[serde(default)]
    pub flag_rules: BTreeMap<FlagKey, Gate>,
    /// `"player"` の stat 糖衣 (後方互換)。min 0 / max なしで宣言扱い。
    #[serde(default)]
    pub initial_stats: BTreeMap<StatKey, i64>,
    /// `"player"` の初期スキル糖衣 (閉世界宣言)。NPC は [`CharacterDef::skills`]。
    #[serde(default)]
    pub initial_skills: BTreeSet<SkillId>,
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
    /// 達成でクリアとなる条件。
    pub goal: Gate,
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

    /// authored challenge を引く (未宣言なら `None`)。
    pub fn challenge(&self, id: &str) -> Option<&ChallengeDef> {
        self.challenges.get(id)
    }

    /// **静的整合性**を検査する (load 時に呼ぶ)。空 Vec なら健全。
    ///
    /// PoC-1: 各 challenge の tier が立てる帰結フラグが `allowed_flags` に宣言済みかを見る。
    /// engine は tier 該当時にこのフラグを (flag_rules gate を迂回して) 直書きするので、
    /// 未宣言フラグを許すと閉世界が破れる。apply 中の panic でなく load 時に弾く。
    pub fn validate(&self) -> Vec<ScenarioError> {
        let mut errs = Vec::new();
        for (cid, def) in &self.challenges {
            for (tname, tier) in &def.tiers {
                if let Some(flag) = &tier.flag {
                    if !self.allowed_flags.contains(flag) {
                        errs.push(ScenarioError::ChallengeFlagUndeclared {
                            challenge: cid.clone(),
                            tier: tname.clone(),
                            flag: flag.clone(),
                        });
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
        errs
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
        // フラグ: source が global と宣言したものだけ運ぶ (局所は捨てる)。
        for key in &prev_scenario.global_flags {
            if let Some(value) = prev.flags.get(key) {
                s.flags.insert(key.clone(), *value);
            }
        }
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
        // 登場人物の宣言。
        for (eid, def) in &self.characters {
            for (k, decl) in &def.stats {
                s.set_stat(eid, k, decl.initial);
            }
            for skill in &def.skills {
                s.grant_skill(eid, skill);
            }
        }
        s
    }
}
