//! 正本が所有する可変状態と、LLM が提案するデルタの型。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub type ItemId = String;
pub type LocationId = String;
pub type FlagKey = String;
pub type StatKey = String;
pub type EntityId = String;
/// トリガーの識別子 (発火済み集合 `GameState.fired` のキー)。
pub type TriggerId = String;
/// 能力 (スキル) の識別子。閉世界 — 宣言された SkillId だけが存在する。
pub type SkillId = String;
/// 文字列属性のキー (クラス/職業/種族 等)。閉世界 — 初期宣言された AttrKey だけが存在する。
/// 値は authored な自由文字列。トリガーの set_attribute でのみ書き換わる (LLM は不可)。
pub type AttrKey = String;
/// authored challenge の識別子。閉世界 — 宣言された ChallengeId にしか挑めない。
pub type ChallengeId = String;
/// 名前付き goal (エンディング) の識別子。`reached` が返し、次モジュールの分岐セレクタになる。
pub type GoalId = String;

/// 単一 `goal` (名前無し) を `reached` が返す時の既定 GoalId (後方互換)。
pub const DEFAULT_GOAL: &str = "goal";

/// 主人公の規約的 EntityId。op/gate が entity を省略した時の既定。
pub const PLAYER: &str = "player";

/// op/gate の `entity` 省略時に使う既定値 (serde default)。
pub fn default_entity() -> EntityId {
    PLAYER.to_string()
}

/// ゲームの唯一の真実。エンジンだけが [`crate::apply`] 経由で変更できる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameState {
    pub location: LocationId,
    /// キャラ別の所持物 (閉世界)。`"player"` は世界から拾い (AddItem)、NPC へ渡せる (GiveItem)。
    #[serde(default)]
    pub inventory: BTreeMap<EntityId, BTreeSet<ItemId>>,
    #[serde(default)]
    pub flags: BTreeMap<FlagKey, bool>,
    /// キャラ別の数値の真実 (HP/STR/好感度 等)。算術はエンジンだけが [`crate::apply`] で行う。
    /// `"player"` が主人公。各 entity は [`crate::Scenario`] の宣言で初期化される。
    #[serde(default)]
    pub entities: BTreeMap<EntityId, BTreeMap<StatKey, i64>>,
    pub rng: RngState,
    #[serde(default)]
    pub turn: u32,
    /// 発火済みトリガーの集合 (Phase C)。edge-triggered once のラッチ。**セーブ対象**:
    /// 一度発火した反応ビートは二度と発火しない (`when` が真のままでも latch で抑止)。
    #[serde(default)]
    pub fired: BTreeSet<TriggerId>,
    /// キャラ別の獲得済み能力 (閉世界 capability)。初期=宣言集合、開花は authored トリガーのみ。
    /// 未宣言の能力は存在しない (メアリー・スー遮断)。**セーブ対象**。
    #[serde(default)]
    pub skills: BTreeMap<EntityId, BTreeSet<SkillId>>,
    /// キャラ別の**文字列属性** (クラス/職業/種族 等。第4の可変状態)。初期=宣言集合、
    /// 書き換えは authored トリガーの set_attribute のみ (LLM は提案しても却下)。未宣言キーは
    /// 存在しない (閉世界、load 時 validate)。値は authored な自由文字列。**セーブ対象**。
    #[serde(default)]
    pub attributes: BTreeMap<EntityId, BTreeMap<AttrKey, String>>,
    /// **画面上の在/不在のオーバーライド** (登場/退場。第5の可変状態。spec 04)。`entity → true`
    /// は強制登場、`entity → false` は強制退場。`Location.present` (場所ベース) に重ねて実効 presence を
    /// 決める。書き換えは authored トリガーの set_presence のみ (LLM は提案しても却下)。**セーブ対象**・
    /// `transition` で持ち越す (仲間が同行する)。
    #[serde(default)]
    pub present_overrides: BTreeMap<EntityId, bool>,
}

impl GameState {
    /// 開始地点と RNG seed から初期状態を作る。
    pub fn new(location: impl Into<LocationId>, seed: u64) -> Self {
        Self {
            location: location.into(),
            inventory: BTreeMap::new(),
            flags: BTreeMap::new(),
            entities: BTreeMap::new(),
            rng: RngState { seed, cursor: 0 },
            turn: 0,
            fired: BTreeSet::new(),
            skills: BTreeMap::new(),
            attributes: BTreeMap::new(),
            present_overrides: BTreeMap::new(),
        }
    }

    /// 指定キャラが能力を獲得済みか (閉世界: 宣言/開花した能力のみ true)。
    pub fn has_skill(&self, entity: &str, skill: &str) -> bool {
        self.skills.get(entity).is_some_and(|s| s.contains(skill))
    }

    /// 能力を付与する (エンジン内部用。authored トリガーの grant_skill 効果からのみ呼ばれる)。
    pub fn grant_skill(&mut self, entity: &str, skill: &str) {
        self.skills
            .entry(entity.to_string())
            .or_default()
            .insert(skill.to_string());
    }

    /// 指定キャラが item を所持しているか。`entity` 省略経路 (Gate/op) は既定で `"player"`。
    pub fn has_item(&self, entity: &str, item: &str) -> bool {
        self.inventory.get(entity).is_some_and(|s| s.contains(item))
    }

    /// item を entity の所持物に加える (エンジン内部用)。
    pub fn add_to_inventory(&mut self, entity: &str, item: &str) {
        self.inventory
            .entry(entity.to_string())
            .or_default()
            .insert(item.to_string());
    }

    /// item を entity の所持物から外す (エンジン内部用)。
    pub fn remove_from_inventory(&mut self, entity: &str, item: &str) {
        if let Some(items) = self.inventory.get_mut(entity) {
            items.remove(item);
        }
    }

    /// 未設定フラグは false 扱い。
    pub fn flag(&self, key: &str) -> bool {
        self.flags.get(key).copied().unwrap_or(false)
    }

    /// 指定キャラの stat。未設定は 0 扱い。
    pub fn stat_of(&self, entity: &str, key: &str) -> i64 {
        self.entities
            .get(entity)
            .and_then(|s| s.get(key))
            .copied()
            .unwrap_or(0)
    }

    /// 主人公 (`"player"`) の stat。未設定は 0 扱い。
    pub fn stat(&self, key: &str) -> i64 {
        self.stat_of(PLAYER, key)
    }

    /// キャラの stat を設定する (エンジン内部用。entity が無ければ作る)。
    pub fn set_stat(&mut self, entity: &str, key: &str, value: i64) {
        self.entities
            .entry(entity.to_string())
            .or_default()
            .insert(key.to_string(), value);
    }

    /// 指定キャラの文字列属性。未設定は空文字。`entity` 省略経路 (Gate/op) は既定で `"player"`。
    pub fn attribute_of(&self, entity: &str, key: &str) -> &str {
        self.attributes
            .get(entity)
            .and_then(|a| a.get(key))
            .map_or("", |v| v.as_str())
    }

    /// 文字列属性を設定する (エンジン内部用。authored トリガーの set_attribute / 初期化からのみ)。
    pub fn set_attribute(&mut self, entity: &str, key: &str, value: &str) {
        self.attributes
            .entry(entity.to_string())
            .or_default()
            .insert(key.to_string(), value.to_string());
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum StateOp {
    /// player が現在地からアイテムを拾う (世界 → player)。
    AddItem { item: ItemId },
    /// player がアイテムを手放す。
    RemoveItem { item: ItemId },
    /// アイテムを譲渡する。`from` が所持していなければ却下 (持っていない物は渡せない)。
    /// `to` は既知の entity でなければ却下。`from` 省略時は主人公。
    GiveItem {
        #[serde(default = "default_entity")]
        from: EntityId,
        to: EntityId,
        item: ItemId,
    },
    SetFlag { key: FlagKey, value: bool },
    Move { to: LocationId },
    /// ダイスを振る要求。**結果は含めない** — エンジンが振って裁く。
    RequestRoll { sides: u32, dc: u32 },
    /// 技能判定。エンジンが `1d{sides} + entity の stat 修正` を振り、`total >= dc` で成否を裁く。
    /// LLM は出目も合計も主張できない (op 構造上不可能)。`stat` 未宣言は却下。`entity` 省略時は主人公。
    Check {
        #[serde(default = "default_entity")]
        entity: EntityId,
        stat: StatKey,
        sides: u32,
        dc: u32,
    },
    /// authored challenge への挑戦。**LLM は challenge を「選ぶ」だけ** — 判定の stat/sides/dc も、
    /// 大失敗/大成功(tier)とその帰結フラグも、すべて [`crate::Scenario`] の authored 定義側にある
    /// (LLM は帰結を持てない＝閉世界)。engine が `1d{sides} + stat修正 vs dc` を振り、natural 値が
    /// tier に該当すれば authored な帰結フラグを直書きする。未宣言 challenge は却下。`entity` 省略時は主人公。
    AttemptChallenge {
        #[serde(default = "default_entity")]
        entity: EntityId,
        challenge: ChallengeId,
    },
    /// stat への加減 (+/−)。エンジンが `clamp(current + delta)` を計算する。
    /// LLM は変化量(意図)だけを提案し、結果の値は持てない。`entity` 省略時は主人公。
    AdjustStat {
        #[serde(default = "default_entity")]
        entity: EntityId,
        key: StatKey,
        delta: i64,
    },
    /// stat への乗除 (×/÷)。エンジンが `clamp(current * num / den)` を計算する。
    /// `den == 0` (ゼロ除算) はエンジンが却下するので、LLM は /0 で壊せない。`entity` 省略時は主人公。
    ScaleStat {
        #[serde(default = "default_entity")]
        entity: EntityId,
        key: StatKey,
        num: i64,
        den: i64,
    },
    /// 能力の付与 (開花)。**authored トリガーの専権** — LLM が提案すると `adjudicate` が却下する
    /// (メアリー・スー遮断)。trigger effects は `apply_ops` 直行なので付与できる。`entity` 省略時は主人公。
    GrantSkill {
        #[serde(default = "default_entity")]
        entity: EntityId,
        skill: SkillId,
    },
    /// 文字列属性の書き換え (クラス転職 等)。**authored トリガーの専権** — LLM が提案すると
    /// `adjudicate` が却下する (クラス捏造 = メアリー・スー遮断、GrantSkill と同型)。trigger effects は
    /// `apply_ops` 直行なので書き換えられる。未宣言キーは load 時 validate で弾く。`entity` 省略時は主人公。
    SetAttribute {
        #[serde(default = "default_entity")]
        entity: EntityId,
        key: AttrKey,
        value: String,
    },
    /// **現在ターンを stat に刻む** (タイムスタンプ)。**authored トリガーの専権** — LLM が提案すると
    /// `adjudicate` が却下する (タイマー詐称遮断、GrantSkill/SetAttribute と同型)。trigger effects は
    /// `apply_ops` 直行なので刻める。`Gate::TurnsSince` と対で「〇〇から N ターン後に発火」を組む。
    /// 値は `GameState.turn` の生値 (stat 境界で clamp しない)。`entity` 省略時は主人公。
    RecordTurn {
        #[serde(default = "default_entity")]
        entity: EntityId,
        key: StatKey,
    },
    /// **画面上の登場/退場** (presence のオーバーライド)。**authored トリガーの専権** — LLM が提案すると
    /// `adjudicate` が却下する (キャラ勝手登場の捏造遮断、GrantSkill/SetAttribute と同型)。trigger effects は
    /// `apply_ops` 直行なので登場/退場させられる。`present=true` で強制登場・`false` で強制退場。
    /// `transition` で持ち越すので、ある画面で登場させた仲間が次の画面にも同行する。`entity` 省略時は主人公。
    SetPresence {
        #[serde(default = "default_entity")]
        entity: EntityId,
        present: bool,
    },
}
