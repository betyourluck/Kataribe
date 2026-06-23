//! 却下理由を**構造化データ**として表現し、提示層で多言語にレンダリングする。
//!
//! エンジンは「なぜ却下したか」をコード (データ) で返す。日本語/英語などの文面は
//! **提示の関心**であってエンジンに焼かない ── i18n・トーン差し替え・テスト頑健化の土台。
//! ルール (所持/移動/gate の力学) はエンジンの普遍法則として残り、文面だけが言語層に分離する。

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
    ItemAlreadyHeld { item: String },
    ItemNotHere { item: String },
    ItemGateUnmet { item: String },
    ItemNotHeld { item: String },
    FlagNotAllowed { key: String },
    FlagGateUnmet { key: String },
    NoExit { to: String },
    MoveGateUnmet { to: String },
    DiceSidesInvalid,
    UnknownStat { key: String },
    DivideByZero { key: String },
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
            RejectReason::ItemAlreadyHeld { item } => format!("'{item}' は既に所持している"),
            RejectReason::ItemNotHere { item } => format!("'{item}' はこの場所には存在しない"),
            RejectReason::ItemGateUnmet { item } => {
                format!("'{item}' はまだ取得できない (前提条件が未達)")
            }
            RejectReason::ItemNotHeld { item } => format!("'{item}' を所持していないので手放せない"),
            RejectReason::FlagNotAllowed { key } => format!("フラグ '{key}' は許可されていない"),
            RejectReason::FlagGateUnmet { key } => format!("フラグ '{key}' を立てる前提条件が未達"),
            RejectReason::NoExit { to } => format!("'{to}' への出口は存在しない"),
            RejectReason::MoveGateUnmet { to } => format!("'{to}' への移動条件が未達"),
            RejectReason::DiceSidesInvalid => "ダイスの面数は1以上でなければならない".to_string(),
            RejectReason::UnknownStat { key } => {
                format!("stat '{key}' はこのシナリオに宣言されていない")
            }
            RejectReason::DivideByZero { key } => format!("stat '{key}' をゼロで割ることはできない"),
        }
    }

    fn en(&self) -> String {
        match self {
            RejectReason::CurrentLocationMissing { location } => {
                format!("current location '{location}' does not exist in the scenario")
            }
            RejectReason::ItemAlreadyHeld { item } => format!("'{item}' is already in your inventory"),
            RejectReason::ItemNotHere { item } => format!("'{item}' is not present in this location"),
            RejectReason::ItemGateUnmet { item } => {
                format!("'{item}' cannot be taken yet (prerequisite unmet)")
            }
            RejectReason::ItemNotHeld { item } => {
                format!("cannot drop '{item}' because you do not hold it")
            }
            RejectReason::FlagNotAllowed { key } => format!("flag '{key}' is not allowed"),
            RejectReason::FlagGateUnmet { key } => format!("prerequisite to set flag '{key}' is unmet"),
            RejectReason::NoExit { to } => format!("there is no exit to '{to}'"),
            RejectReason::MoveGateUnmet { to } => format!("the condition to move to '{to}' is unmet"),
            RejectReason::DiceSidesInvalid => "a die must have at least 1 side".to_string(),
            RejectReason::UnknownStat { key } => {
                format!("stat '{key}' is not declared in this scenario")
            }
            RejectReason::DivideByZero { key } => format!("cannot divide stat '{key}' by zero"),
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
