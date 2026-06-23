//! 正本が所有する可変状態と、LLM が提案するデルタの型。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub type ItemId = String;
pub type LocationId = String;
pub type FlagKey = String;

/// ゲームの唯一の真実。エンジンだけが [`crate::apply`] 経由で変更できる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameState {
    pub location: LocationId,
    #[serde(default)]
    pub inventory: BTreeSet<ItemId>,
    #[serde(default)]
    pub flags: BTreeMap<FlagKey, bool>,
    pub rng: RngState,
    #[serde(default)]
    pub turn: u32,
}

impl GameState {
    /// 開始地点と RNG seed から初期状態を作る。
    pub fn new(location: impl Into<LocationId>, seed: u64) -> Self {
        Self {
            location: location.into(),
            inventory: BTreeSet::new(),
            flags: BTreeMap::new(),
            rng: RngState { seed, cursor: 0 },
            turn: 0,
        }
    }

    pub fn has_item(&self, item: &str) -> bool {
        self.inventory.contains(item)
    }

    /// 未設定フラグは false 扱い。
    pub fn flag(&self, key: &str) -> bool {
        self.flags.get(key).copied().unwrap_or(false)
    }
}

/// 決定論的な乱数状態。`seed` と `cursor` のみで再現でき、監査可能。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RngState {
    pub seed: u64,
    pub cursor: u64,
}

impl RngState {
    /// 1d{sides} を振り (1..=sides)、cursor を1進める。
    /// splitmix64 ベース。`sides` は1以上を前提。
    pub fn roll(&mut self, sides: u32) -> u32 {
        debug_assert!(sides >= 1, "sides must be >= 1");
        let mut z = self
            .seed
            .wrapping_add(self.cursor.wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        self.cursor += 1;
        (z % sides as u64) as u32 + 1
    }
}

/// LLM が毎ターン返す唯一の出力形。`ops` 以外の経路で state を変えることはできない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateDelta {
    #[serde(default)]
    pub narration: String,
    #[serde(default)]
    pub ops: Vec<StateOp>,
}

impl StateDelta {
    pub fn new(narration: impl Into<String>, ops: Vec<StateOp>) -> Self {
        Self {
            narration: narration.into(),
            ops,
        }
    }
}

/// 状態変更の最小単位。内部タグ `"op"` 付きで LLM の structured output に対応。
///
/// 例: `{"op":"add_item","item":"rusty_key"}`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum StateOp {
    AddItem { item: ItemId },
    RemoveItem { item: ItemId },
    SetFlag { key: FlagKey, value: bool },
    Move { to: LocationId },
    /// ダイスを振る要求。**結果は含めない** — エンジンが振って裁く。
    RequestRoll { sides: u32, dc: u32 },
}
