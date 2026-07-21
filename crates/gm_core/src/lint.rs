//! 未知フィールドの lint — 「静かな罠」(serde が未知キーを黙って無視する) の防衛線。
//!
//! 実測 3 件 (2026-07-11〜12): Location 直下の `gate:` (無効な場所に書いた) / challenge の
//! 入れ子ミス (インデントずれで別 challenge の内部フィールド化) / `entry:` typo (`entity:` の誤り)。
//! いずれも「エラーなく、ただ効かない」— serde の寛容さが失敗を隠す。
//!
//! 生 YAML を [`serde_yaml::Value`] として歩き、各文脈 (Scenario 直下 / Location / Trigger /
//! ChallengeDef / Gate / StateOp …) の**既知キー集合**と突き合わせ、未知キーを警告として名指しする
//! (近い既知キーがあれば「〜の誤り？」を添える)。**非 fatal** — 前方互換 (新しい content を古い
//! Kataribe で読む) を殺さないため、load は拒否せず提示層が ⚠ で出す ([`crate::Scenario::lints`] と同じ線引き)。
//!
//! **既知キー集合は手書きしない** — 構造体は「最小 YAML で parse → serialize → 全フィールド名」で
//! 実際の型から導出する (フィールド追加に自動追従 = 表のドリフトが構造的に起きない)。
//! enum (Gate/StateOp) は全バリアント標本の直列化の和集合 + **網羅 match の番人**
//! (バリアント追加時にコンパイルエラーで標本更新を強制する)。

use std::collections::BTreeSet;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_yaml::Value;

use crate::spine::{
    AttrRequirement, ChallengeDef, ChallengeMod, ChallengeOutcome, CharacterDef, Exit, Gate,
    GoalDef, Location, Protagonist, PushCost, RoleAssignment, Scenario, SpendRules, StatDecl,
    ContestDef, RollSpec, TierDef, Trigger, VoteRule,
};
use crate::state::StateOp;

/// scenario YAML の未知フィールドを警告文の列にする (健全なら空)。
/// parse できない YAML は空を返す (エラーは `Scenario::from_yaml` 側が出す — 役割分離)。
pub fn unknown_key_lints(src: &str) -> Vec<String> {
    let Ok(root) = serde_yaml::from_str::<Value>(src) else {
        return Vec::new();
    };
    let tables = Tables::build();
    let mut out = Vec::new();
    walk(&root, Ctx::Scenario, "", &tables, &mut out);
    out
}

/// 文脈 = 「いまどの型の mapping を見ているか」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ctx {
    Scenario,
    Location,
    /// `Location.items` の値 (新形式 `{when, take}`。`kind` を含む mapping は旧形式 = Gate)。
    LocationItem,
    Exit,
    Trigger,
    Challenge,
    Outcome,
    Tier,
    ChallengeMod,
    Goal,
    Character,
    StatDecl,
    Gate,
    Op,
    Protagonist,
    RoleAssignment,
    VoteRule,
    AttrRequirement,
    SpendRules,
    PushCost,
    Contest,
    RollSpec,
}

/// 各文脈の既知キー集合。実際の型から導出する ([`Tables::build`])。
struct Tables {
    scenario: BTreeSet<String>,
    location: BTreeSet<String>,
    location_item: BTreeSet<String>,
    exit: BTreeSet<String>,
    trigger: BTreeSet<String>,
    challenge: BTreeSet<String>,
    outcome: BTreeSet<String>,
    tier: BTreeSet<String>,
    challenge_mod: BTreeSet<String>,
    goal: BTreeSet<String>,
    character: BTreeSet<String>,
    stat_decl: BTreeSet<String>,
    gate: BTreeSet<String>,
    op: BTreeSet<String>,
    protagonist: BTreeSet<String>,
    role_assignment: BTreeSet<String>,
    vote_rule: BTreeSet<String>,
    attr_requirement: BTreeSet<String>,
    spend_rules: BTreeSet<String>,
    push_cost: BTreeSet<String>,
    contest: BTreeSet<String>,
    roll_spec: BTreeSet<String>,
}

/// mapping 1 段の未知キーを警告文の列にする (近い既知キーの提案つき)。`path` は表示用の接頭辞
/// (空なら root)。mapping でなければ空。
///
/// scenario 以外の YAML — `package.yaml` 等、gm_core が知らない配布レイアウト側の型 — を
/// **その型を持つ層が自分で** lint するための部品 ([`struct_keys`] と対で使う)。
/// 再帰はしない: 入れ子は呼び出し側が文脈を知っているので、そちらが段ごとに呼ぶ。
pub fn unknown_keys(value: &Value, known: &BTreeSet<String>, path: &str) -> Vec<String> {
    let Value::Mapping(m) = value else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for k in m.keys() {
        let Some(key) = k.as_str() else { continue };
        if known.contains(key) {
            continue;
        }
        let here = if path.is_empty() { key.to_string() } else { format!("{path}.{key}") };
        out.push(format!(
            "{here}: 不明なフィールド「{key}」は無視されます{}",
            suggest(key, known)
        ));
    }
    out
}

/// 最小 YAML から型 `T` を作り、**シリアライズして全フィールド名**を得る
/// (serde は全フィールドを書き出すので、最小 sample でも既知キーは完全になる)。
pub fn struct_keys<T: DeserializeOwned + Serialize>(minimal_yaml: &str) -> BTreeSet<String> {
    let sample: T = serde_yaml::from_str(minimal_yaml)
        .expect("lint の最小サンプルは必ず parse できる (型のフィールド変更時はここを追従)");
    mapping_keys(&serde_yaml::to_value(&sample).expect("シリアライズは失敗しない"))
}

fn mapping_keys(v: &Value) -> BTreeSet<String> {
    match v {
        Value::Mapping(m) => m
            .keys()
            .filter_map(|k| k.as_str().map(String::from))
            .collect(),
        _ => BTreeSet::new(),
    }
}

/// enum の全バリアント標本を直列化し、キーの**和集合** (タグ `kind`/`op` 込み) を得る。
fn union_keys<T: Serialize>(samples: &[T]) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    for s in samples {
        if let Ok(v) = serde_yaml::to_value(s) {
            set.extend(mapping_keys(&v));
        }
    }
    set
}

/// Gate の全バリアント標本。**バリアントを追加したら [`_gate_exhaustive_guard`] がコンパイルエラーに
/// なるので、ここへ標本を足すこと** (足し忘れると新バリアントのフィールドが偽陽性警告になる)。
fn gate_samples() -> Vec<Gate> {
    let e = || "e".to_string();
    vec![
        Gate::Always,
        Gate::HasItem { entity: e(), item: "i".into() },
        Gate::FlagIs { key: "k".into(), value: true },
        Gate::LocationIs { at: "l".into() },
        Gate::StatAtLeast { entity: e(), key: "s".into(), value: 0 },
        Gate::StatAtMost { entity: e(), key: "s".into(), value: 0 },
        Gate::HasSkill { entity: e(), skill: "s".into() },
        Gate::AttributeIs { entity: e(), key: "a".into(), value: "v".into() },
        Gate::TurnsSince { entity: e(), key: "s".into(), turns: 1 },
        Gate::HasVoted { entity: e() },
        Gate::All { of: Vec::new() },
        Gate::Any { of: Vec::new() },
    ]
}

/// 網羅 match の番人 — Gate にバリアントを足すとここが compile error になり、
/// [`gate_samples`] の更新を強制する (既知キー集合のドリフト防止)。
fn _gate_exhaustive_guard(g: &Gate) {
    match g {
        Gate::Always
        | Gate::HasItem { .. }
        | Gate::FlagIs { .. }
        | Gate::LocationIs { .. }
        | Gate::StatAtLeast { .. }
        | Gate::StatAtMost { .. }
        | Gate::HasSkill { .. }
        | Gate::AttributeIs { .. }
        | Gate::TurnsSince { .. }
        | Gate::HasVoted { .. }
        | Gate::All { .. }
        | Gate::Any { .. } => {}
    }
}

/// StateOp の全バリアント標本 (番人は [`_op_exhaustive_guard`])。
fn op_samples() -> Vec<StateOp> {
    let e = || "e".to_string();
    vec![
        StateOp::AddItem { item: "i".into() },
        StateOp::RemoveItem { item: "i".into() },
        StateOp::GiveItem { from: e(), to: e(), item: "i".into() },
        StateOp::SetFlag { key: "k".into(), value: true },
        StateOp::Move { to: "l".into() },
        StateOp::RequestRoll { sides: 6, dc: 3 },
        StateOp::Check { entity: e(), stat: "s".into(), sides: 20, dc: 10 },
        StateOp::CheckUnder { entity: e(), key: "s".into() },
        StateOp::AttemptChallenge { entity: e(), challenge: "c".into() },
        StateOp::AttemptContest { contest: "c".into() },
        StateOp::AdjustStat { entity: e(), key: "s".into(), delta: 1 },
        StateOp::ScaleStat { entity: e(), key: "s".into(), num: 1, den: 1 },
        StateOp::GrantSkill { entity: e(), skill: "s".into() },
        StateOp::SetAttribute { entity: e(), key: "a".into(), value: "v".into() },
        StateOp::RecordTurn { entity: e(), key: "s".into() },
        StateOp::SetPresence { entity: e(), present: true },
        StateOp::RollStat { entity: e(), key: "s".into(), count: 1, sides: 6, bonus: 0, negate: false },
        StateOp::CastVote { voter: e(), target: e() },
        StateOp::ResolveVote,
    ]
}

/// 網羅 match の番人 — StateOp にバリアントを足すとここが compile error になり、
/// [`op_samples`] の更新を強制する。
fn _op_exhaustive_guard(op: &StateOp) {
    match op {
        StateOp::AddItem { .. }
        | StateOp::RemoveItem { .. }
        | StateOp::GiveItem { .. }
        | StateOp::SetFlag { .. }
        | StateOp::Move { .. }
        | StateOp::RequestRoll { .. }
        | StateOp::Check { .. }
        | StateOp::CheckUnder { .. }
        | StateOp::AttemptChallenge { .. }
        | StateOp::AttemptContest { .. }
        | StateOp::AdjustStat { .. }
        | StateOp::ScaleStat { .. }
        | StateOp::GrantSkill { .. }
        | StateOp::SetAttribute { .. }
        | StateOp::RecordTurn { .. }
        | StateOp::SetPresence { .. }
        | StateOp::RollStat { .. }
        | StateOp::CastVote { .. }
        | StateOp::ResolveVote => {}
    }
}

impl Tables {
    fn build() -> Self {
        Self {
            scenario: struct_keys::<Scenario>("start: room\nlocations: {}"),
            location: struct_keys::<Location>("{}"),
            // LocationItem 新形式 {when, take} (旧形式 = Gate は kind の有無で判別)。
            location_item: ["when", "take"].iter().map(|s| s.to_string()).collect(),
            exit: struct_keys::<Exit>("to: x"),
            trigger: struct_keys::<Trigger>("id: t\nwhen: { kind: always }"),
            challenge: struct_keys::<ChallengeDef>("sides: 1\ndc: 1"),
            outcome: struct_keys::<ChallengeOutcome>("{}"),
            tier: struct_keys::<TierDef>("natural: min"),
            challenge_mod: struct_keys::<ChallengeMod>("when: { kind: always }\nbonus: 0"),
            goal: struct_keys::<GoalDef>("id: g\nwhen: { kind: always }"),
            character: struct_keys::<CharacterDef>("{}"),
            stat_decl: struct_keys::<StatDecl>("initial: 0"),
            gate: union_keys(&gate_samples()),
            op: union_keys(&op_samples()),
            protagonist: struct_keys::<Protagonist>("{}"),
            role_assignment: struct_keys::<RoleAssignment>("key: k\npool: {}\namong: []"),
            vote_rule: struct_keys::<VoteRule>("{}"),
            attr_requirement: struct_keys::<AttrRequirement>("key: k\nvalue: v"),
            spend_rules: struct_keys::<SpendRules>("from: x"),
            push_cost: struct_keys::<PushCost>("from: x\namount: 1"),
            contest: struct_keys::<ContestDef>(
                "opponent: o\nplayer_roll: { sides: 6 }\nopponent_roll: { sides: 6 }",
            ),
            roll_spec: struct_keys::<RollSpec>("{}"),
        }
    }

    fn known(&self, ctx: Ctx) -> &BTreeSet<String> {
        match ctx {
            Ctx::Scenario => &self.scenario,
            Ctx::Location => &self.location,
            Ctx::LocationItem => &self.location_item,
            Ctx::Exit => &self.exit,
            Ctx::Trigger => &self.trigger,
            Ctx::Challenge => &self.challenge,
            Ctx::Outcome => &self.outcome,
            Ctx::Tier => &self.tier,
            Ctx::ChallengeMod => &self.challenge_mod,
            Ctx::Goal => &self.goal,
            Ctx::Character => &self.character,
            Ctx::StatDecl => &self.stat_decl,
            Ctx::Gate => &self.gate,
            Ctx::Op => &self.op,
            Ctx::Protagonist => &self.protagonist,
            Ctx::RoleAssignment => &self.role_assignment,
            Ctx::VoteRule => &self.vote_rule,
            Ctx::AttrRequirement => &self.attr_requirement,
            Ctx::SpendRules => &self.spend_rules,
            Ctx::PushCost => &self.push_cost,
            Ctx::Contest => &self.contest,
            Ctx::RollSpec => &self.roll_spec,
        }
    }
}

/// キー配下をどう歩くか。
enum Child {
    /// この文脈の mapping として直接歩く。
    Direct(Ctx),
    /// 列の各要素を歩く。
    Seq(Ctx),
    /// mapping の**値**をそれぞれ歩く (キーは作者の自由語彙 = 検査しない)。
    MapValues(Ctx),
    /// `Location.items` の値: `kind` を含む mapping = 旧形式 Gate、それ以外 = 新形式 {when, take}。
    ItemMap,
    /// `CharacterDef.stats` の値: mapping = StatDecl、scalar (数値糖衣) = 検査なし。
    StatMap,
    /// 葉 (これ以上構造を知らない)。
    None,
}

fn child_of(ctx: Ctx, key: &str) -> Child {
    match (ctx, key) {
        (Ctx::Scenario, "locations") => Child::MapValues(Ctx::Location),
        (Ctx::Scenario, "triggers") => Child::Seq(Ctx::Trigger),
        (Ctx::Scenario, "challenges") => Child::MapValues(Ctx::Challenge),
        (Ctx::Scenario, "goals") => Child::Seq(Ctx::Goal),
        (Ctx::Scenario, "goal") => Child::Direct(Ctx::Gate),
        (Ctx::Scenario, "characters") => Child::MapValues(Ctx::Character),
        (Ctx::Scenario, "flag_rules") => Child::MapValues(Ctx::Gate),
        (Ctx::Scenario, "protagonist") => Child::Direct(Ctx::Protagonist),
        (Ctx::Scenario, "role_assignment") => Child::Direct(Ctx::RoleAssignment),
        (Ctx::Scenario, "spend_rules") => Child::Direct(Ctx::SpendRules),
        (Ctx::Scenario, "push_cost") => Child::Direct(Ctx::PushCost),
        (Ctx::Scenario, "contests") => Child::MapValues(Ctx::Contest),
        (Ctx::Contest, "requires" | "until") => Child::Direct(Ctx::Gate),
        // player_roll/opponent_roll は文字列 (テンプレート名) or mapping (RollSpec) の両受け。
        // 文字列は walker が mapping でないため素通りし、mapping だけ RollSpec 検査になる。
        (Ctx::Contest, "player_roll" | "opponent_roll") => Child::Direct(Ctx::RollSpec),
        (Ctx::Contest, "on_win" | "on_lose" | "on_tie") => Child::Direct(Ctx::Outcome),
        // キャラの振り方テンプレート: キーはテンプレート名 (データ)、値が RollSpec。
        (Ctx::Character, "rolls") => Child::MapValues(Ctx::RollSpec),
        (Ctx::Scenario, "vote_rules") => Child::Seq(Ctx::VoteRule),
        (Ctx::Location, "items") => Child::ItemMap,
        (Ctx::Location, "exits") => Child::Seq(Ctx::Exit),
        (Ctx::Exit, "gate") => Child::Direct(Ctx::Gate),
        (Ctx::Trigger, "when") => Child::Direct(Ctx::Gate),
        (Ctx::Trigger, "effects") => Child::Seq(Ctx::Op),
        (Ctx::Challenge, "requires") => Child::Direct(Ctx::Gate),
        (Ctx::Challenge, "modifiers") => Child::Seq(Ctx::ChallengeMod),
        // 全帰結スロット (spec 16 の degree 別 + spec 18 の on_push_failure)。
        // 従来 on_success/on_failure のみ = degree スロット内の typo が盲点だった。
        (
            Ctx::Challenge,
            "on_success" | "on_failure" | "on_critical" | "on_extreme" | "on_hard" | "on_fumble"
            | "on_push_failure",
        ) => Child::Direct(Ctx::Outcome),
        (Ctx::Challenge, "tiers") => Child::MapValues(Ctx::Tier),
        (Ctx::Outcome, "effects") | (Ctx::Tier, "effects") => Child::Seq(Ctx::Op),
        (Ctx::ChallengeMod, "when") => Child::Direct(Ctx::Gate),
        (Ctx::Goal, "when") => Child::Direct(Ctx::Gate),
        (Ctx::Character, "stats") => Child::StatMap,
        // initial_stats は素の数値と境界つき宣言 (StatInit) の両受け — mapping 値だけ
        // StatDecl として typo 検査する (Character.stats と同じ StatMap 意味論)。
        (Ctx::Scenario, "initial_stats") => Child::StatMap,
        (Ctx::Character, "taboos") => Child::Seq(Ctx::Gate),
        (Ctx::Gate, "of") => Child::Seq(Ctx::Gate),
        (Ctx::VoteRule, "when") => Child::Direct(Ctx::Gate),
        (Ctx::VoteRule, "voter_attribute") => Child::Direct(Ctx::AttrRequirement),
        _ => Child::None,
    }
}

fn walk(v: &Value, ctx: Ctx, path: &str, t: &Tables, out: &mut Vec<String>) {
    let Value::Mapping(m) = v else { return };
    let known = t.known(ctx);
    for (k, child) in m {
        let Some(key) = k.as_str() else { continue };
        if !known.contains(key) {
            let here = if path.is_empty() { key.to_string() } else { format!("{path}.{key}") };
            out.push(format!(
                "{here}: 不明なフィールド「{key}」は無視されます{}",
                suggest(key, known)
            ));
            continue; // 未知キーの下は文脈が分からないので潜らない
        }
        let sub_path = if path.is_empty() { key.to_string() } else { format!("{path}.{key}") };
        match child_of(ctx, key) {
            Child::Direct(c) => walk(child, c, &sub_path, t, out),
            Child::Seq(c) => {
                if let Value::Sequence(seq) = child {
                    for (i, item) in seq.iter().enumerate() {
                        walk(item, c, &format!("{sub_path}[{i}]"), t, out);
                    }
                }
            }
            Child::MapValues(c) => {
                if let Value::Mapping(map) = child {
                    for (mk, mv) in map {
                        let name = mk.as_str().unwrap_or("?");
                        walk(mv, c, &format!("{sub_path}.{name}"), t, out);
                    }
                }
            }
            Child::ItemMap => {
                if let Value::Mapping(map) = child {
                    for (mk, mv) in map {
                        let name = mk.as_str().unwrap_or("?");
                        let p = format!("{sub_path}.{name}");
                        // 旧形式 (Gate 直書き) は `kind` を含む。新形式は {when, take}。
                        let is_gate =
                            matches!(mv, Value::Mapping(im) if im.contains_key(Value::from("kind")));
                        if is_gate {
                            walk(mv, Ctx::Gate, &p, t, out);
                        } else {
                            walk(mv, Ctx::LocationItem, &p, t, out);
                            if let Value::Mapping(im) = mv {
                                if let Some(w) = im.get(Value::from("when")) {
                                    walk(w, Ctx::Gate, &format!("{p}.when"), t, out);
                                }
                            }
                        }
                    }
                }
            }
            Child::StatMap => {
                if let Value::Mapping(map) = child {
                    for (mk, mv) in map {
                        if mv.is_mapping() {
                            let name = mk.as_str().unwrap_or("?");
                            walk(mv, Ctx::StatDecl, &format!("{sub_path}.{name}"), t, out);
                        }
                    }
                }
            }
            Child::None => {}
        }
    }
}

/// 近い既知キーの提案 (「entry」→「entity」等)。編集距離 2 以下・3 文字以上のキーのみ。
fn suggest(key: &str, known: &BTreeSet<String>) -> String {
    if key.chars().count() < 3 {
        return String::new();
    }
    known
        .iter()
        .map(|k| (levenshtein(key, k), k))
        .filter(|(d, _)| *d <= 2)
        .min()
        .map(|(_, k)| format!("（「{k}」の誤り？）"))
        .unwrap_or_default()
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur.push((prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

// =============================================================================
// PoC: 実測 3 事故 (entry typo / Location 直下 gate / challenge 入れ子) を名指しで捕まえ、
// 健全な総合盤面では偽陽性ゼロであること。
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    /// 【entry typo (実測 2026-07-12, 1ldk)】challenge の `entity:` を `entry:` と書くと serde が
    /// 黙って無視し主体固定が効かない。lint が名指しし「entity の誤り？」を提案する。
    #[test]
    fn lints_entry_typo_with_suggestion() {
        let yaml = "
start: room
challenges:
  hina_work:
    entry: hina
    stat: 主人公❤
    sides: 100
    dc: 100
locations:
  room: { description: d }
";
        let warns = unknown_key_lints(yaml);
        assert_eq!(warns.len(), 1, "{warns:?}");
        assert!(
            warns[0].contains("entry") && warns[0].contains("entity"),
            "typo の名指しと正しいキーの提案: {warns:?}"
        );
        assert!(warns[0].contains("hina_work"), "どの challenge かをパスで示す: {warns:?}");
    }

    /// 【Location 直下 gate (実測 2026-07-11, friday_lemmon)】Location に存在しない `gate:` を
    /// 書いても黙って無視される (出口の gate と混同しやすい)。lint が捕まえる。
    /// 【challenge 入れ子 (実測 2026-07-12, 1ldk)】インデントずれで challenge が別 challenge の
    /// 内部フィールドになると、その id が未知フィールドとして黙殺される。lint が捕まえる。
    #[test]
    fn lints_location_stray_gate_and_nested_challenge() {
        let yaml = "
start: room
challenges:
  sleep:
    sides: 6
    dc: 1
    hina_cafe_work:
      sides: 100
      dc: 10
locations:
  room:
    description: d
    gate: { kind: always }
";
        let warns = unknown_key_lints(yaml);
        assert!(
            warns.iter().any(|w| w.contains("locations.room") && w.contains("gate")),
            "Location 直下の gate を名指し: {warns:?}"
        );
        assert!(
            warns.iter().any(|w| w.contains("challenges.sleep") && w.contains("hina_cafe_work")),
            "入れ子になった challenge を名指し: {warns:?}"
        );
    }

    /// 【偽陽性ゼロ】主要な構造 (goal/goals/trigger/challenge/tier/modifiers/character/
    /// role_assignment/vote_rules/flag_rules/items 新旧形式/protagonist) を使った健全な盤面で
    /// 警告が出ない。型にフィールドを追加してもここは通り続ける (既知キーは型から導出)。
    #[test]
    fn no_false_positives_on_kitchen_sink() {
        let yaml = "
title: t
start: room
world: w
protagonist: { name: n, profile: p }
allowed_flags: [f, g]
global_flags: [f]
persistent_flags: [f]
flag_rules:
  f: { kind: flag_is, key: g, value: true }
flag_hints: { f: hint }
flag_titles: { f: 表示名 }
hidden_flags: [g]
initial_stats: { hp: 10 }
initial_skills: [剣術]
initial_inventory: [鍵]
initial_attributes: { クラス: 見習い }
hidden_stats: [タイマー]
cast: []
characters:
  alice:
    name: アリス
    profile: p
    stats: { 好感度: { initial: 0, min: 0, max: 100 }, 体力: 10 }
    skills: [料理]
    taboos: [{ kind: flag_is, key: f, value: true }]
    inventory: [花]
    attributes: { 職業: 店員 }
role_assignment: { key: 役職, pool: { 人狼: 1 }, among: [alice] }
vote_rules:
  - when: { kind: flag_is, key: f, value: true }
    voter_attribute: { key: 役職, value: 人狼 }
locations:
  room:
    title: 部屋
    description: d
    present: [alice]
    items:
      鍵: { kind: always }
      ジュース: { when: { kind: always }, take: infinite }
    exits:
      - { to: hall, gate: { kind: all, of: [{ kind: has_item, entity: player, item: 鍵 }] } }
  hall: { description: d }
triggers:
  - id: t1
    when: { kind: any, of: [{ kind: stat_at_least, entity: alice, key: 好感度, value: 30 }] }
    effects:
      - { op: set_flag, key: f, value: true }
      - { op: set_presence, entity: alice, present: false }
      - { op: record_turn, entity: player, key: タイマー }
    narration: n
    recall: cue
    repeatable: true
challenges:
  c1:
    description: d
    entity: alice
    stat: 好感度
    requires: { kind: turns_since, entity: player, key: タイマー, turns: 2 }
    modifiers:
      - { when: { kind: attribute_is, entity: alice, key: 職業, value: 店員 }, bonus: -5 }
    sides: 100
    dc: 50
    on_success: { flag: f, effects: [{ op: adjust_stat, entity: alice, key: 好感度, delta: 5 }], narration: n, sound: s.wav }
    on_failure: { effects: [{ op: scale_stat, entity: alice, key: 好感度, num: 1, den: 2 }] }
    tiers:
      crit_fail: { natural: at_most, threshold: 10, flag: g, narration: n }
goals:
  - { id: g1, when: { kind: stat_at_most, entity: alice, key: 好感度, value: 0 }, title: 表示, hint: h, narration: n, visible: false, epilogue_prompt: 余韻を }
goal: { kind: always }
";
        let warns = unknown_key_lints(yaml);
        assert!(warns.is_empty(), "健全な盤面に偽陽性を出さない: {warns:?}");
    }

    /// 【spec 11: 旧形式への epilogue_prompt】旧 `goal:` は素の Gate なので epilogue_prompt の
    /// 置き場が無い — serde は黙って無視する (deny_unknown_fields ではない) が、生 YAML 走査の
    /// この lint が未知キーとして名指しする、という防衛線の前提を回帰固定する。
    /// named goals (GoalDef) 側は既知キーが型から自動導出されるので警告しない
    /// (上の kitchen_sink に epilogue_prompt 入りの goal がある = 同時に担保)。
    #[test]
    fn lints_epilogue_prompt_on_old_form_goal_gate() {
        let yaml = "
start: room
goal: { kind: always, epilogue_prompt: 生存者のその後を語れ }
locations:
  room: { description: d }
";
        let warns = unknown_key_lints(yaml);
        assert_eq!(warns.len(), 1, "{warns:?}");
        assert!(
            warns[0].contains("epilogue_prompt"),
            "旧形式 goal (Gate) への epilogue_prompt を未知キーとして名指し: {warns:?}"
        );
    }

    /// 【壊れた YAML は沈黙】parse 不能なら空 (エラーは from_yaml 側の責務 — 二重報告しない)。
    #[test]
    fn broken_yaml_returns_empty() {
        assert!(unknown_key_lints(": : :").is_empty());
    }
}
