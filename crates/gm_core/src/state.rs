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

/// **authored 専権 op の serde タグ** ── LLM が提案すると [`crate::adjudicate`] が必ず却下する op。
/// これらは authored トリガーの効果 (`apply_ops` 直行) でのみ実行される。`emit_delta` の schema から
/// これらを除外して LLM に**そもそも提案させない** (構造的遮断)。露出したままだと LLM が使い続け、
/// 却下→再生成ループで詰まる (presence は物語で頻出ゆえ特に問題)。adjudicate の却下ケースと対応。
pub const AUTHORED_ONLY_OPS: &[&str] = &[
    "grant_skill",
    "set_attribute",
    "record_turn",
    "set_presence",
    "resolve_vote",
    "roll_stat",
];

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
    /// **投票の器** (spec 06 Phase C)。voter → target。一人一票 (再投票は上書き)、
    /// BTreeMap = 集計順も決定論。**セーブ対象**・`transition` では持ち越さない (票は
    /// フェーズ内の一時状態)。書き込みは CastVote、読み出し+リセットは ResolveVote。
    #[serde(default)]
    pub votes: BTreeMap<EntityId, EntityId>,
    /// フラグが **true に真化したターン**の記録 (`apply` 末尾の差分で一括捕捉 — op /
    /// トリガー効果 / challenge 帰結のどの経路でも刻まれる)。提示層が chronicle (経緯ログ) の
    /// 該当ターン要約と join し「何をして立ったフラグか」を思い出す素。**セーブ対象**。
    #[serde(default)]
    pub flag_turns: BTreeMap<FlagKey, u32>,
    /// 場所から**持ち去った** `take: once` アイテムの記録 (LocationId → 取得済み ItemId 集合)。
    /// 手放して戻っても再取得 (複製) を却下するための世界の事実。**セーブ対象**。
    /// `transition` では持ち越さない (LocationId はモジュール内スコープ。campaign 再訪の持続は
    /// `persistent_flags` と同じ問いで、必要になったら spec 02 同型の機構で扱う)。
    #[serde(default)]
    pub taken_items: BTreeMap<LocationId, BTreeSet<ItemId>>,
    /// **進行中の対決** (spec 18 Phase C)。`attempt_contest` の受理で開き、
    /// [`crate::contest_round`] が 1 交換ずつ進めて決着 (until/max_rounds/goal) で閉じる。
    /// 進行中は上位 (app) が次のターンを回さない (決着まで LLM 非関与)。**セーブ対象**。
    #[serde(default)]
    pub pending_contest: Option<PendingContest>,
    /// **決断待ちの判定** (spec 18 Phase B)。pushable/spendable な challenge が失敗した時、
    /// 帰結 (フラグ/effects/トリガー) を**適用せず凍結**した素性が積まれる。プレイヤーの決断
    /// (受け入れ/プッシュ/差分買い) を `resolve_decision` が確定し、そこで初めて帰結が原子適用
    /// される。先頭から順に解決 (開帳→決断の直列)。**セーブ対象** (serde default = 旧セーブ互換)。
    /// 空でない間、上位 (app) は次のターンを回さない。
    #[serde(default)]
    pub pending_decisions: Vec<PendingDecision>,
}

/// 進行中の対決の帳簿 (spec 18 Phase C)。ラウンドの積算だけを持つ (振り方・帰結は
/// authored 定義から毎ラウンド引き直す = セーブに authored 内容を複製しない)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingContest {
    /// どの contest か (定義を引き直すキー)。
    pub contest: String,
    /// 消化済みラウンド数。
    pub rounds: u32,
    /// player 側の勝ち/負け/引き分けの積算 (digest の素)。
    pub wins: u32,
    pub losses: u32,
    pub ties: u32,
}

fn default_one_u32() -> u32 {
    1
}
fn default_one_i64() -> i64 {
    1
}

/// 決断待ちの判定の凍結素性 (spec 18 Phase B)。**raw_roll (出目そのもの) を必ず持つ** —
/// 差分買いの費用は `出目 − 買いたい成功度の閾値` の計算に生の出目が要る (degree だけでは
/// 導けない、rev2 査読 I-5)。帰結スロットは持たない (challenge 定義から決断確定時に解決 =
/// セーブに authored 内容を複製しない)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingDecision {
    /// どの challenge の判定か (解決時に定義を引き直すキー)。
    pub challenge: String,
    /// 判定主体 (決断が凍結されるのは player のみだが、表示用に運ぶ)。
    pub entity: EntityId,
    /// 判定 stat (表示用。stat 無し challenge は空文字)。
    pub stat: StatKey,
    /// ダイス面数 (additive)。percentile は 100。
    pub sides: u32,
    /// ダイス個数 (既定 1・serde default = 旧セーブ互換)。プッシュの振り直しが同じ式で振るための素性。
    #[serde(default = "default_one_u32")]
    pub count: u32,
    /// 出目の乗数 (既定 1)。同上。
    #[serde(default = "default_one_i64")]
    pub times: i64,
    /// **生の出目** (差分買いの費用計算の基準)。
    pub roll: u32,
    /// additive の修正値 (stat + modifiers)。percentile は目標値への修正合算。
    pub modifier: i64,
    /// additive の合計 (roll + modifier)。percentile は出目そのもの。
    pub total: i64,
    /// additive の DC。percentile は実効目標値。
    pub dc: u32,
    /// percentile の成功度 (凍結時は常に "failure" — fumble は凍結されず final)。additive は None。
    pub degree: Option<String>,
    /// プッシュ済みか (凍結時 false。push 解決は final なので true のまま残ることは無いが、
    /// CheckOutcome への写しと将来の拡張のため素性に持つ)。
    pub pushed: bool,
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
            votes: BTreeMap::new(),
            flag_turns: BTreeMap::new(),
            taken_items: BTreeMap::new(),
            pending_contest: None,
            pending_decisions: Vec::new(),
        }
    }

    /// `take: once` アイテムを既にその場所から持ち去ったか。
    pub fn already_taken(&self, location: &str, item: &str) -> bool {
        self.taken_items.get(location).is_some_and(|s| s.contains(item))
    }

    /// 持ち去りを記録する (エンジン内部用。apply の AddItem からのみ呼ばれる)。
    pub fn record_taken(&mut self, location: &str, item: &str) {
        self.taken_items
            .entry(location.to_string())
            .or_default()
            .insert(item.to_string());
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
    /// このターンの経緯 1 行 (誰が何をして何が起きたか、確定した事実だけ)。narration と同じ
    /// **非検証の語り素材** — engine は解釈しない。harness が経緯ログとして蓄積し、以後の
    /// ターンの prompt に「これまでの経緯」として還流する (GM の中期記憶。後のターンの
    /// 自分自身への引き継ぎ既成事実)。
    #[serde(default)]
    pub summary: String,
    // **単要素をスカラーで書く LLM を救済する** (#64): `"ops": {…}` のように配列を省く出力が
    // 実在し、従来は delta 全体がパース失敗 → 再送 → 内容が失われていた。schema は配列のまま
    // (指示は正しく出す) で、受け側だけ寛容にする (#40/#52 と同族の「壊れた構造化出力の救済」)。
    #[serde(default, deserialize_with = "one_or_many")]
    pub ops: Vec<StateOp>,
    // spec 20 既成事実 (facts) の **GM 書き込み経路は撤去した** (2026-07-21、実測 3 周の結論)。
    // 契機を三度書き直しても 0/45・0/20 の絶対ゼロで、GM は尋ねれば自分の即興を正確に
    // 列挙できるのに提出前の確認は毎ターン脱落した = **語り手に記録係を兼ねさせるのが
    // 構造的に無理**という結論 (failures.md #65)。既成事実はユーザーが設定を宣言する欄になり、
    // GM は読むだけ。機械が書く経路が要るなら、語りと競合しない瞬間 (あらすじ圧縮時の抽出)
    // に別経路で足す — ターン毎の delta には戻さない。
}

/// 配列フィールドを**単要素スカラーでも受ける** (#64)。
///
/// LLM は要素が 1 つのとき配列を省いて書くことが実在する
/// (`"facts": "…"` / `"ops": {…}`)。従来は serde が `invalid type: string, expected a sequence`
/// で落ち、**delta 全体が失われて再送**になっていた (再送時は当該フィールドごと落とすので、
/// 書かれた内容は永久に失われる = 外からは「GM が書かない」に見える)。
/// schema 側は配列のまま (指示は正しく出す)、**受け側だけを寛容にする**
/// — 「パース失敗は raw を保持し再生成の燃料にする」(#40) の一歩手前で救う。
fn one_or_many<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany<T> {
        Many(Vec<T>),
        One(T),
    }
    Ok(match OneOrMany::<T>::deserialize(d)? {
        OneOrMany::Many(v) => v,
        OneOrMany::One(x) => vec![x],
    })
}

impl StateDelta {
    pub fn new(narration: impl Into<String>, ops: Vec<StateOp>) -> Self {
        Self {
            narration: narration.into(),
            summary: String::new(),
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
    /// **d100 ロールアンダー即興判定** (spec 16)。エンジンが 1d100 を振り、目標値 = その entity の
    /// stat 現在値、`roll <= 目標値` で成功。成功度 (degree: critical/extreme/hard/regular/
    /// failure/fumble) もエンジンが決定論で計算する — LLM は出目も成功度も持てない。
    /// 帰結は持たない (成否+degree の surface のみ。機械的帰結は authored challenge で書く)。
    /// `key` (技能 stat) 未宣言は却下。`entity` 省略時は主人公。
    CheckUnder {
        #[serde(default = "default_entity")]
        entity: EntityId,
        key: StatKey,
    },
    /// authored challenge への挑戦。**LLM は challenge を「選ぶ」だけ** — 判定の stat/sides/dc も、
    /// 大失敗/大成功(tier)とその帰結フラグも、すべて [`crate::Scenario`] の authored 定義側にある
    /// (LLM は帰結を持てない＝閉世界)。engine が `1d{sides} + stat修正 vs dc` を振り、natural 値が
    /// tier に該当すれば authored な帰結フラグを直書きする (`resolution: percentile` なら
    /// d100 ロールアンダー + degree 別帰結、spec 16)。未宣言 challenge は却下。`entity` 省略時は主人公。
    AttemptChallenge {
        #[serde(default = "default_entity")]
        entity: EntityId,
        challenge: ChallengeId,
    },
    /// authored contest (対決) の開始 (spec 18 Phase C)。**LLM は対決を「開く」だけ** —
    /// 双方の振り方 (RollSpec)・帰結・決着条件はすべて authored 定義側にある。開始後の
    /// ラウンドは **LLM を介さず** engine とプレイヤーが直接回す ([`crate::contest_round`]) =
    /// 雑魚戦のトークンを消す一括型 (cadence はこの機構、逐次型は従来の challenge)。
    AttemptContest { contest: String },
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
    /// **可変量ダイス** (spec 16)。エンジンが `count × d(sides) + bonus` を振り、`negate` に
    /// 応じて ± を stat へ clamp 適用する (SAN 1d6 減少・1d8 ダメージ)。**authored 専権** —
    /// LLM が提案すると `adjudicate` が却下する (ダメージ量の捏造遮断、GrantSkill と同型)。
    /// trigger/challenge の effects は `apply_ops` 直行なので使える。出目は
    /// [`crate::StatRollOutcome`] として surface (「SAN -4 (1d6=4)」)。`entity` 省略時は主人公。
    RollStat {
        #[serde(default = "default_entity")]
        entity: EntityId,
        key: StatKey,
        count: u32,
        sides: u32,
        #[serde(default)]
        bonus: i64,
        #[serde(default)]
        negate: bool,
    },
    /// **投票の意図** (spec 06 Phase C)。voter が target の処刑/襲撃に票を入れる。
    /// LLM 提案可 — ただし受理は「voter/target 生存 + `vote_rules` のいずれかに合致
    /// (デフォルト拒否)」をエンジンが裁く。一人一票 ([`GameState::votes`] は voter キーの
    /// map = 再投票は上書き)。開票は [`StateOp::ResolveVote`] の専権。`voter` 省略時は主人公。
    CastVote {
        #[serde(default = "default_entity")]
        voter: EntityId,
        target: EntityId,
    },
    /// **開票** (spec 06 Phase C)。**authored トリガーの専権 (効果 op 第5例)** — LLM が提案
    /// すると `adjudicate` が却下する (開票結果の捏造遮断)。エンジンが一箇所で原子適用:
    /// 集計 → 最多得票 (同数は seed 派生 VOTE_RNG で抽選 = 決定論) → 死亡
    /// (生存=0 + presence false) → 役職カウンタ/優位 stat 再計算 → 票リセット。
    ResolveVote,
}
