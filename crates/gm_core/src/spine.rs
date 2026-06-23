//! シナリオ脊椎 (拘束)。beat/場所のグラフと gate 条件で、即興が筋から外れすぎないよう縛る。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::state::{default_entity, EntityId, FlagKey, GameState, ItemId, LocationId, StatKey, PLAYER};

/// state に対して評価される条件。
///
/// 内部タグ方式 (`kind` フィールド)。serde_yaml 0.9 で素直なマップとして書ける:
/// `{ kind: has_item, item: rusty_key }` / `{ kind: flag_is, key: ..., value: true }`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Gate {
    /// 常に通る。
    Always,
    /// 指定アイテムを所持している。
    HasItem { item: ItemId },
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
    /// すべての子条件が通る (AND)。
    All { of: Vec<Gate> },
    /// いずれかの子条件が通る (OR)。
    Any { of: Vec<Gate> },
}

impl Gate {
    pub fn eval(&self, s: &GameState) -> bool {
        match self {
            Gate::Always => true,
            Gate::HasItem { item } => s.has_item(item),
            Gate::FlagIs { key, value } => s.flag(key) == *value,
            Gate::LocationIs { at } => &s.location == at,
            Gate::StatAtLeast { entity, key, value } => s.stat_of(entity, key) >= *value,
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
    /// 硬い禁忌: これが true になる delta を却下する (Phase B でエンジン強制)。
    #[serde(default)]
    pub taboos: Vec<Gate>,
}

/// シナリオ全体。`scenarios/*.yaml` から読み込まれる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scenario {
    #[serde(default)]
    pub title: String,
    pub start: LocationId,
    #[serde(default)]
    pub allowed_flags: BTreeSet<FlagKey>,
    /// フラグを true にするための gate。記載なければ [`Gate::Always`]。
    #[serde(default)]
    pub flag_rules: BTreeMap<FlagKey, Gate>,
    /// `"player"` の stat 糖衣 (後方互換)。min 0 / max なしで宣言扱い。
    #[serde(default)]
    pub initial_stats: BTreeMap<StatKey, i64>,
    /// 登場人物 (player 以外)。外部 `characters/*.yaml` を読み込んで注入する。
    #[serde(default)]
    pub characters: BTreeMap<EntityId, CharacterDef>,
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
        // 登場人物の宣言。
        for (eid, def) in &self.characters {
            for (k, decl) in &def.stats {
                s.set_stat(eid, k, decl.initial);
            }
        }
        s
    }
}
