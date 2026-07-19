//! 却下理由を**構造化データ**として表現し、提示層で多言語にレンダリングする。
//!
//! エンジンは「なぜ却下したか」をコード (データ) で返す。日本語/英語などの文面は
//! **提示の関心**であってエンジンに焼かない ── i18n・トーン差し替え・テスト頑健化の土台。
//! ルール (所持/移動/gate の力学) はエンジンの普遍法則として残り、文面だけが言語層に分離する。

use crate::spine::Gate;
use serde::{Deserialize, Serialize};

/// レンダリング言語。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lang {
    Ja,
    En,
}

/// 却下の構造化理由。表示文字列は [`RejectReason::localize`] で言語ごとに生成する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum RejectReason {
    /// 現在地がシナリオに存在しない (state 破損)。
    CurrentLocationMissing { location: String },
    // ItemAlreadyHeld は spec 09-B で撤去 — 既所持への add_item は却下でなく no-op 受理
    // (「念のための再拾得」で delta 全体が落ちる摩擦の除去。複製穴の守りは taken_items)。
    ItemNotHere { item: String },
    /// 取得条件が未達。`requirement` は満たすべき条件そのもの (#42: 「未達」とだけ言うと
    /// LLM が op クラスごと諦める回避学習に入る — 条件を明示して計画修正へ導く)。
    /// `unmet` は `requirement` のうち**現に false の葉条件**だけ (どれがダメかの名指し。
    /// バグか本当に未達かの切り分け材料)。旧セーブ互換のため serde default。
    ItemGateUnmet {
        item: String,
        requirement: Gate,
        #[serde(default)]
        unmet: Vec<Gate>,
    },
    /// 備え付けアイテム (`take: fixed`)。取得は不可だが、その場で使えることを LLM に説明する。
    ItemFixed { item: String },
    /// `take: once` のアイテムを既にこの場所から持ち去っている (再取得=複製の遮断)。
    ItemAlreadyTaken { item: String },
    ItemNotHeld { item: String },
    /// 未宣言フラグ (幻フラグ)。`available` は LLM が set_flag してよい語彙
    /// (`Scenario::usable_flags` = allowed − authored 専権) — self-repair が一発で正しい名前に直せる。
    FlagNotAllowed {
        key: String,
        #[serde(default)]
        available: Vec<String>,
    },
    /// フラグを立てる前提条件 (`flag_rules`) が未達。`requirement` は条件そのもの (#42)。
    /// `unmet` は現に false の葉条件だけ (どれがダメかの名指し)。
    FlagGateUnmet {
        key: String,
        requirement: Gate,
        #[serde(default)]
        unmet: Vec<Gate>,
    },
    NoExit { to: String },
    /// 出口の gate が未達。`requirement` は条件そのもの (#42 — 「未達」だけでは LLM が
    /// move を諦め、語りだけで移動した気になる回避学習の温床だった)。
    /// `unmet` は現に false の葉条件だけ (どれがダメかの名指し)。
    MoveGateUnmet {
        to: String,
        requirement: Gate,
        #[serde(default)]
        unmet: Vec<Gate>,
    },
    DiceSidesInvalid,
    UnknownStat { entity: String, key: String },
    DivideByZero { key: String },
    /// このデルタは `entity` の硬い禁忌 (taboo) を破る (Phase B)。
    TabooViolated { entity: String },
    /// 能力の付与 (grant_skill) は LLM が提案できない (authored トリガーの専権)。メアリー・スー遮断。
    SkillGrantNotAllowed { entity: String, skill: String },
    /// 文字列属性の書き換え (set_attribute) は LLM が提案できない (authored トリガーの専権)。
    /// クラス/種族 等の捏造遮断 (SkillGrantNotAllowed と同型)。
    AttributeSetNotAllowed { entity: String, key: String },
    /// ターンの刻み (record_turn) は LLM が提案できない (authored トリガーの専権)。
    /// タイマー詐称遮断 (SkillGrantNotAllowed と同型)。
    TurnRecordNotAllowed { entity: String, key: String },
    /// 登場/退場 (set_presence) は LLM が提案できない (authored トリガーの専権)。
    /// キャラ勝手登場の捏造遮断 (SkillGrantNotAllowed と同型)。
    PresenceSetNotAllowed { entity: String },
    /// 可変量ダイス (roll_stat) は LLM が提案できない (authored 専権、spec 16)。
    /// ダメージ/SAN 減少の量の捏造遮断 (SkillGrantNotAllowed と同型)。
    StatRollNotAllowed { entity: String, key: String },
    /// 譲渡先がこのシナリオに存在しない entity (幻のキャラには渡せない)。
    UnknownEntity { entity: String },
    /// 投票の voter/target が生存していない (死者は投票できず、されもしない)。
    EntityNotAlive { entity: String },
    /// いまの局面では voter に投票権が無い (`vote_rules` のどれにも合致しない = デフォルト拒否)。
    VoteNotAllowed { voter: String },
    /// この盤面には投票の機構が宣言されていない (`vote_rules` が空)。投票の無いゲームへの
    /// cast_vote を「死者/局面」でなく**機構の不在**として名指し却下する — self-repair が
    /// 一発で cast_vote を落とせる (#31 同型の診断可能性、実プレイ #35)。
    VoteNotDeclared,
    /// 開票 (resolve_vote) は LLM が提案できない (authored トリガーの専権。開票結果の捏造遮断)。
    VoteResolveNotAllowed,
    /// このシナリオに宣言されていない challenge には挑めない (幻チャレンジ遮断)。
    UnknownChallenge { challenge: String },
    /// challenge の前提条件 (`requires` Gate) が未達で、まだ挑めない (挑戦の解禁待ち)。
    /// `requirement` は条件そのもの (#42)。`unmet` は現に false の葉条件だけ (どれがダメかの
    /// 名指し — 「フラグを満たしているのに却下される」がバグか本当に未達かを切り分ける)。
    ChallengeLocked {
        challenge: String,
        requirement: Gate,
        #[serde(default)]
        unmet: Vec<Gate>,
    },
}

// Gate を人間可読の条件文にする (却下理由用、Ja/En)。harness の `gate_brief` (prompt 用)
// と役割は同じだが、こちらは**却下理由の言語層** — self-repair する LLM が「何を満たせば
// 通るか」を読むための文。層が違うので複製を許容する (i18n はこちらだけが持つ)。
fn gate_ja(gate: &Gate) -> String {
    match gate {
        Gate::Always => "条件なし".to_string(),
        Gate::HasItem { entity, item } => format!("{entity} が「{item}」を所持していること"),
        Gate::FlagIs { key, value } => format!("フラグ「{key}」が {value} であること"),
        Gate::LocationIs { at } => format!("「{at}」にいること"),
        Gate::StatAtLeast { entity, key, value } => {
            format!("{entity} の「{key}」が {value} 以上であること")
        }
        Gate::StatAtMost { entity, key, value } => {
            format!("{entity} の「{key}」が {value} 以下であること")
        }
        Gate::HasSkill { entity, skill } => format!("{entity} が能力「{skill}」を持っていること"),
        Gate::AttributeIs { entity, key, value } => {
            format!("{entity} の「{key}」が「{value}」であること")
        }
        Gate::TurnsSince { entity, key, turns } => {
            format!("{entity} の「{key}」から {turns} ターン以上経つこと")
        }
        Gate::HasVoted { entity } => format!("{entity} が投票を済ませていること"),
        Gate::All { of } => {
            let parts: Vec<String> = of.iter().map(gate_ja).collect();
            format!("すべて満たす({})", parts.join(" / "))
        }
        Gate::Any { of } => {
            let parts: Vec<String> = of.iter().map(gate_ja).collect();
            format!("いずれか満たす({})", parts.join(" / "))
        }
    }
}

fn gate_en(gate: &Gate) -> String {
    match gate {
        Gate::Always => "no condition".to_string(),
        Gate::HasItem { entity, item } => format!("{entity} holds '{item}'"),
        Gate::FlagIs { key, value } => format!("flag '{key}' is {value}"),
        Gate::LocationIs { at } => format!("being at '{at}'"),
        Gate::StatAtLeast { entity, key, value } => {
            format!("{entity}'s '{key}' is at least {value}")
        }
        Gate::StatAtMost { entity, key, value } => {
            format!("{entity}'s '{key}' is at most {value}")
        }
        Gate::HasSkill { entity, skill } => format!("{entity} has the skill '{skill}'"),
        Gate::AttributeIs { entity, key, value } => {
            format!("{entity}'s '{key}' is '{value}'")
        }
        Gate::TurnsSince { entity, key, turns } => {
            format!("at least {turns} turns have passed since {entity}'s '{key}'")
        }
        Gate::HasVoted { entity } => format!("{entity} has cast a vote"),
        Gate::All { of } => {
            let parts: Vec<String> = of.iter().map(gate_en).collect();
            format!("all of ({})", parts.join(" / "))
        }
        Gate::Any { of } => {
            let parts: Vec<String> = of.iter().map(gate_en).collect();
            format!("any of ({})", parts.join(" / "))
        }
    }
}

/// 必要条件を描き、`All` の一部だけ未達なら**どの葉が false か**を名指しする。
/// 単一条件 (unmet == requirement) では冗長なので `requirement` だけ返す。
fn requirement_ja(requirement: &Gate, unmet: &[Gate]) -> String {
    // 単一葉の要件 (unmet == [requirement]) や未算出時は名指しても冗長なので要件だけ。
    let trivial = unmet.is_empty() || (unmet.len() == 1 && unmet[0] == *requirement);
    if trivial {
        gate_ja(requirement)
    } else {
        let parts: Vec<String> = unmet.iter().map(gate_ja).collect();
        format!("{}【未達: {}】", gate_ja(requirement), parts.join(" / "))
    }
}

fn requirement_en(requirement: &Gate, unmet: &[Gate]) -> String {
    let trivial = unmet.is_empty() || (unmet.len() == 1 && unmet[0] == *requirement);
    if trivial {
        gate_en(requirement)
    } else {
        let parts: Vec<String> = unmet.iter().map(gate_en).collect();
        format!("{} [unmet: {}]", gate_en(requirement), parts.join(" / "))
    }
}

impl RejectReason {
    /// 指定言語の表示文字列を生成する。新言語の追加はここに一手で閉じる。
    pub fn localize(&self, lang: Lang) -> String {
        match lang {
            Lang::Ja => self.ja(),
            Lang::En => self.en(),
        }
    }

    fn ja(&self) -> String {
        match self {
            RejectReason::CurrentLocationMissing { location } => {
                format!("現在地 '{location}' がシナリオに存在しない")
            }
            RejectReason::ItemNotHere { item } => format!("'{item}' はこの場所には存在しない"),
            RejectReason::ItemGateUnmet { item, requirement, unmet } => {
                format!("'{item}' はまだ取得できない (必要: {})", requirement_ja(requirement, unmet))
            }
            RejectReason::ItemFixed { item } => {
                format!("'{item}' は備え付けで持ち運べない (取得せず、その場で使える)")
            }
            RejectReason::ItemAlreadyTaken { item } => {
                format!("'{item}' は既にここから持ち去られていて、もう無い")
            }
            RejectReason::ItemNotHeld { item } => format!("'{item}' を所持していないので手放せない"),
            RejectReason::FlagNotAllowed { key, available } => {
                if available.is_empty() {
                    format!("フラグ '{key}' は存在しない (このシナリオに set_flag できるフラグは無い)")
                } else {
                    format!("フラグ '{key}' は存在しない (使えるフラグ: {})", available.join(", "))
                }
            }
            RejectReason::FlagGateUnmet { key, requirement, unmet } => {
                format!("フラグ '{key}' はまだ立てられない (必要: {})", requirement_ja(requirement, unmet))
            }
            RejectReason::NoExit { to } => format!("'{to}' への出口は存在しない"),
            RejectReason::MoveGateUnmet { to, requirement, unmet } => {
                format!(
                    "'{to}' へはまだ移動できない (必要: {}。満たせば move は通る — 語りだけで移動した事にしないこと)",
                    requirement_ja(requirement, unmet)
                )
            }
            RejectReason::DiceSidesInvalid => "ダイスの面数は1以上でなければならない".to_string(),
            RejectReason::UnknownStat { entity, key } => {
                format!("{entity} は stat '{key}' を持っていない (NPC の数値なら entity にその NPC を指定すること)")
            }
            RejectReason::DivideByZero { key } => format!("stat '{key}' をゼロで割ることはできない"),
            RejectReason::TabooViolated { entity } => {
                format!("その行動は {entity} の禁忌に反する")
            }
            RejectReason::SkillGrantNotAllowed { entity, skill } => {
                format!("{entity} は能力 '{skill}' をその場で開花できない (能力は筋書きの出来事でのみ目覚める)")
            }
            RejectReason::AttributeSetNotAllowed { entity, key } => {
                format!("{entity} の '{key}' をその場で書き換えられない (属性は筋書きの出来事でのみ変わる)")
            }
            RejectReason::TurnRecordNotAllowed { entity, key } => {
                format!("{entity} の '{key}' にターンを刻めない (時の記録は筋書きの出来事でのみ起きる)")
            }
            RejectReason::PresenceSetNotAllowed { entity } => {
                format!("{entity} をその場で登場/退場させられない (登場は筋書きの出来事でのみ起きる)")
            }
            RejectReason::StatRollNotAllowed { entity, key } => {
                format!("{entity} の '{key}' をダイスで増減させられない (変化量のダイスは筋書きの出来事でのみ振られる。判定は check/check_under か挑戦で行え)")
            }
            RejectReason::UnknownEntity { entity } => {
                format!("'{entity}' はこのシナリオに存在しないので渡せない")
            }
            RejectReason::EntityNotAlive { entity } => {
                format!("{entity} は既に生存していない (死者は投票できず、投票の対象にもならない)")
            }
            RejectReason::VoteNotAllowed { voter } => {
                format!("いまは {voter} が投票できる局面ではない (投票のフェーズと投票権を確認せよ)")
            }
            RejectReason::VoteNotDeclared => {
                "この盤面に投票の仕組みは無い (cast_vote は使えない。意図は別の行動・語りで表せ)"
                    .to_string()
            }
            RejectReason::VoteResolveNotAllowed => {
                "開票はあなたが起こせない (開票は筋書きの出来事でのみ行われる)".to_string()
            }
            RejectReason::UnknownChallenge { challenge } => {
                format!("'{challenge}' という挑戦はこのシナリオに存在しない")
            }
            RejectReason::ChallengeLocked { challenge, requirement, unmet } => {
                format!("'{challenge}' にはまだ挑めない (必要: {})", requirement_ja(requirement, unmet))
            }
        }
    }

    fn en(&self) -> String {
        match self {
            RejectReason::CurrentLocationMissing { location } => {
                format!("current location '{location}' does not exist in the scenario")
            }
            RejectReason::ItemNotHere { item } => format!("'{item}' is not present in this location"),
            RejectReason::ItemGateUnmet { item, requirement, unmet } => {
                format!("'{item}' cannot be taken yet (requires: {})", requirement_en(requirement, unmet))
            }
            RejectReason::ItemFixed { item } => {
                format!("'{item}' is a fixture and cannot be carried (use it where it is, without taking it)")
            }
            RejectReason::ItemAlreadyTaken { item } => {
                format!("'{item}' has already been taken from here and is gone")
            }
            RejectReason::ItemNotHeld { item } => {
                format!("cannot drop '{item}' because you do not hold it")
            }
            RejectReason::FlagNotAllowed { key, available } => {
                if available.is_empty() {
                    format!("flag '{key}' does not exist (no flags can be set in this scenario)")
                } else {
                    format!("flag '{key}' does not exist (available flags: {})", available.join(", "))
                }
            }
            RejectReason::FlagGateUnmet { key, requirement, unmet } => {
                format!("flag '{key}' cannot be set yet (requires: {})", requirement_en(requirement, unmet))
            }
            RejectReason::NoExit { to } => format!("there is no exit to '{to}'"),
            RejectReason::MoveGateUnmet { to, requirement, unmet } => {
                format!(
                    "cannot move to '{to}' yet (requires: {}. once met, move will succeed — do not narrate the move as done)",
                    requirement_en(requirement, unmet)
                )
            }
            RejectReason::DiceSidesInvalid => "a die must have at least 1 side".to_string(),
            RejectReason::UnknownStat { entity, key } => {
                format!("{entity} has no stat '{key}' (for an NPC's stat, set entity to that NPC)")
            }
            RejectReason::DivideByZero { key } => format!("cannot divide stat '{key}' by zero"),
            RejectReason::TabooViolated { entity } => {
                format!("that action violates {entity}'s taboo")
            }
            RejectReason::SkillGrantNotAllowed { entity, skill } => {
                format!("{entity} cannot awaken the skill '{skill}' on a whim (skills awaken only through authored events)")
            }
            RejectReason::AttributeSetNotAllowed { entity, key } => {
                format!("{entity} cannot rewrite '{key}' on a whim (attributes change only through authored events)")
            }
            RejectReason::TurnRecordNotAllowed { entity, key } => {
                format!("{entity} cannot stamp the turn into '{key}' (time is recorded only through authored events)")
            }
            RejectReason::PresenceSetNotAllowed { entity } => {
                format!("{entity} cannot enter or leave the scene on a whim (presence changes only through authored events)")
            }
            RejectReason::StatRollNotAllowed { entity, key } => {
                format!("you cannot roll dice to change {entity}'s '{key}' (variable amounts are rolled only by authored events; use check/check_under or a challenge instead)")
            }
            RejectReason::UnknownEntity { entity } => {
                format!("cannot give to '{entity}' because it does not exist in this scenario")
            }
            RejectReason::ChallengeLocked { challenge, requirement, unmet } => {
                format!("'{challenge}' cannot be attempted yet (requires: {})", requirement_en(requirement, unmet))
            }
            RejectReason::UnknownChallenge { challenge } => {
                format!("there is no challenge '{challenge}' in this scenario")
            }
            RejectReason::EntityNotAlive { entity } => {
                format!("{entity} is no longer alive (the dead can neither vote nor be voted for)")
            }
            RejectReason::VoteNotAllowed { voter } => {
                format!("{voter} cannot vote in the current situation (check the phase and voting rights)")
            }
            RejectReason::VoteNotDeclared => {
                "this scenario has no voting mechanism (cast_vote is unavailable; express the intent through other actions)".to_string()
            }
            RejectReason::VoteResolveNotAllowed => {
                "you cannot resolve the vote (tallying only happens as an authored event)".to_string()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 同じ構造化理由が言語ごとに正しくレンダリングされる (多言語の最小証明)。
    #[test]
    fn localizes_to_each_language() {
        let r = RejectReason::ItemNotHere { item: "master_key".into() };
        assert!(r.localize(Lang::Ja).contains("存在しない"));
        assert!(r.localize(Lang::En).contains("not present"));
        assert!(r.localize(Lang::Ja).contains("master_key"), "id は言語に依らず保持");

        let z = RejectReason::DivideByZero { key: "gold".into() };
        assert!(z.localize(Lang::Ja).contains("ゼロ"));
        assert!(z.localize(Lang::En).contains("zero"));
    }
}
