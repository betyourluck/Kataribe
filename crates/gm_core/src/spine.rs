//! シナリオ脊椎 (拘束)。beat/場所のグラフと gate 条件で、即興が筋から外れすぎないよう縛る。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::state::{FlagKey, GameState, ItemId, LocationId};

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
}
