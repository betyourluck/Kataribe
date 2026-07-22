//! 既成事実 (spec 20) — **プレイヤーが宣言し、GM が守る設定**のリスト。
//!
//! **名前が規律**: 「メモ」(走り書き = 暫定) でも「約束事」(相互の取り決め = 交渉の余地) でもなく
//! 「既成事実」— ここに書かれた行はプレイヤーが一方的に確定させた事項で、以後の語りは無条件に従う。
//! 推理途中の仮説を書く場所ではない。GM の書き込み経路を撤去した (下記) 以上、この欄は
//! 相互合意ではなく**一方的宣言**であり、名前はその非対称を符号化する。
//!
//! **正本 (GameState) の外**に置く語り素材 (chronicle/Memoria と同じ境界)。
//!
//! ## GM の書き込み経路は撤去した (2026-07-21、実測 3 周の結論)
//!
//! 当初は GM (LLM) が `StateDelta.facts` で行を提案する設計だったが、契機の書き方を
//! 三度変えても **0/45・0/20 の絶対ゼロ**だった。開発者モードで問うと GM は自分の即興
//! (「東大理三」「妹の雫と 3 万円」) を正確に列挙でき、書くべきだったとも認めるのに、
//! 提出前の確認は毎ターン脱落する — **語り手に記録係を兼ねさせるのが構造的に無理**
//! という結論 (failures.md #65)。よって既成事実は**ユーザーが設定を宣言する欄**とし、
//! GM は読むだけにした。機械が書く経路が要るなら、語りと競合しない瞬間
//! (あらすじ圧縮時の抽出) に別経路で足す — ターン毎の delta には戻さない。

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

/// 上限件数。超過は誤用のサイン (chronicle に書くべきもの) なので増やさない。
pub const FACTS_MAX: usize = 20;
/// 1 行の上限 (Unicode スカラー = Rust `chars()`)。超過は機械カット (spec 10 の 400 字カットと同じ非対話流儀)。
pub const FACT_LINE_CHARS: usize = 60;

/// 新規追加のスコア。編集で `SCORE_USER_EDIT` ずつ加算される。
pub const SCORE_USER_NEW: u32 = 4;
/// 編集・同文統合の加点 (手を触れた行ほど退場しにくくなる)。
pub const SCORE_USER_EDIT: u32 = 3;

/// 既成事実の出所。現在ユーザーだけが書くが、**旧セーブに `gm` が入りうる**ので
/// バリアントは残す (GM 書き込み経路の撤去前に書かれた行との互換)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactOrigin {
    Gm,
    User,
}

/// 既成事実の 1 行。`id` はセッション内一意・不変 (採番 = 現存 max+1)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactEntry {
    pub id: u64,
    pub origin: FactOrigin,
    pub text: String,
    /// 誕生したターン (表示用)。
    pub turn: u32,
    /// 参照スコア。編集で上がり、満杯時は低い行から退場する。
    pub score: u32,
}

/// 60 字カット (Unicode スカラー基準)。
fn truncate_line(text: &str) -> String {
    text.trim().chars().take(FACT_LINE_CHARS).collect()
}

/// dedup キー: trim + NFKC 正規化後の完全一致 (全角半角・末尾空白の差で別行にしない)。
fn dedup_key(text: &str) -> String {
    text.trim().nfkc().collect()
}

fn next_id(list: &[FactEntry]) -> u64 {
    list.iter().map(|m| m.id).max().unwrap_or(0) + 1
}

/// 犠牲者選定: 最低スコア帯の中で最古 (id 最小) の index。空なら None。
fn victim_index(list: &[FactEntry]) -> Option<usize> {
    let min_score = list.iter().map(|m| m.score).min()?;
    list.iter()
        .enumerate()
        .filter(|(_, m)| m.score == min_score)
        .min_by_key(|(_, m)| m.id)
        .map(|(i, _)| i)
}

/// ユーザーの追加 (score 4 で誕生)。満杯なら犠牲者を 1 件退場させ、その行を返す
/// (UI トースト用 — 退場を silent にしない)。同文が既にあれば追加でなく強化に読み替える。
pub fn apply_user_add(
    list: &mut Vec<FactEntry>,
    text: &str,
    turn: u32,
) -> (Option<u64>, Option<FactEntry>) {
    let text = truncate_line(text);
    if text.is_empty() {
        return (None, None);
    }
    let key = dedup_key(&text);
    if let Some(existing) = list.iter_mut().find(|m| dedup_key(&m.text) == key) {
        existing.score += SCORE_USER_EDIT;
        existing.origin = FactOrigin::User;
        return (Some(existing.id), None);
    }
    let mut evicted = None;
    if list.len() >= FACTS_MAX {
        if let Some(i) = victim_index(list) {
            evicted = Some(list.remove(i));
        }
    }
    let id = next_id(list);
    list.push(FactEntry { id, origin: FactOrigin::User, text, turn, score: SCORE_USER_NEW });
    (Some(id), evicted)
}

/// ユーザーの編集 (+3)。編集結果が他行と同文 (dedup 一致) になったら、
/// **編集対象を削除し既存側へ +3 統合** (スコア分散を防ぐ)。統合先/編集先の id を返す。
pub fn apply_user_edit(list: &mut Vec<FactEntry>, id: u64, text: &str) -> Option<u64> {
    let text = truncate_line(text);
    if text.is_empty() {
        return None;
    }
    let key = dedup_key(&text);
    let target = list.iter().position(|m| m.id == id)?;
    if list.iter().any(|m| m.id != id && dedup_key(&m.text) == key) {
        list.remove(target);
        let other = list.iter_mut().find(|m| dedup_key(&m.text) == key)?;
        other.score += SCORE_USER_EDIT;
        other.origin = FactOrigin::User;
        return Some(other.id);
    }
    let m = &mut list[target];
    m.text = text;
    m.score += SCORE_USER_EDIT;
    m.origin = FactOrigin::User;
    Some(m.id)
}

/// ユーザーの削除。消せたら true。
pub fn apply_user_delete(list: &mut Vec<FactEntry>, id: u64) -> bool {
    let before = list.len();
    list.retain(|m| m.id != id);
    list.len() != before
}

/// 表示・注入の共通並び: **スコア降順 (同点は id 昇順)**。LLM はリスト先頭を重視するので
/// UI と注入の並びを一致させる (消えかけが下に集まる退場予告も両側で再現される)。
pub fn sorted_for_display(list: &[FactEntry]) -> Vec<&FactEntry> {
    let mut v: Vec<&FactEntry> = list.iter().collect();
    v.sort_by(|a, b| b.score.cmp(&a.score).then(a.id.cmp(&b.id)));
    v
}

/// prompt 注入節。従属規律ヘッダ (抑止+保護の対 — #47 信頼チャネル対策) + スコア降順の全行。
/// **空リストなら None** — GM は書けないので、空の節を出す意味がない (書き込み経路の撤去に伴い、
/// コールドスタートの促し文も撤去した)。
pub fn facts_note(list: &[FactEntry]) -> Option<String> {
    if list.is_empty() {
        return None;
    }
    let mut s = String::from(
        "\n\n# 既成事実 (プレイヤーが宣言した設定)\n\
         これはプレイヤーがこの物語について宣言した取り決めである。**以後の語りで必ず守ること。** \
         ただし世界の状態そのものではない — state と矛盾するときは常に state が正しく、\
         既成事実を根拠に state に無い所持・能力・出来事を確定させてはならない。\
         呼称・設定・関係・約束のような語りの一貫性については、これに従うこと。\n",
    );
    for m in sorted_for_display(list) {
        s.push_str("- ");
        s.push_str(&m.text);
        s.push('\n');
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 【60 字カット】超過分は機械的に落ちる (chars 基準)。
    #[test]
    fn lines_are_truncated_to_sixty_chars() {
        let mut list = Vec::new();
        let long = "あ".repeat(80);
        apply_user_add(&mut list, &long, 1);
        assert_eq!(list[0].text.chars().count(), FACT_LINE_CHARS);
    }

    /// 【NFKC dedup + 編集の統合】同文は 2 件併存させず、手を触れた分だけ強化される。
    #[test]
    fn dedup_merges_instead_of_duplicating() {
        let mut list = Vec::new();
        apply_user_add(&mut list, "妹の名前はサキ", 1);
        assert_eq!(list[0].score, SCORE_USER_NEW);
        // 全角/半角・空白差は NFKC+trim で同一視される → 追加でなく強化。
        apply_user_add(&mut list, " 妹の名前はサキ ", 2);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].score, SCORE_USER_NEW + SCORE_USER_EDIT);

        // 編集で他行と同文にすると 1 件へ畳まれる。
        apply_user_add(&mut list, "別の設定", 3);
        let other = list.iter().find(|m| m.text == "別の設定").unwrap().id;
        let merged = apply_user_edit(&mut list, other, "妹の名前はサキ").unwrap();
        assert_ne!(merged, other, "編集対象は消えて既存へ統合される");
        assert_eq!(list.len(), 1);
    }

    /// 【満杯時】犠牲者は最低スコア帯の最古。ユーザー操作は常に成立し、退場行を返す
    /// (UI トーストで可視化 = silent な退場を作らない)。
    #[test]
    fn full_list_evicts_lowest_score_oldest_and_reports_it() {
        let mut list = Vec::new();
        for i in 0..FACTS_MAX {
            apply_user_add(&mut list, &format!("設定{i}"), 1);
        }
        // 1 件だけ編集して守る (score 4 → 7)。
        let protected = list[0].id;
        apply_user_edit(&mut list, protected, "守りたい設定");
        let oldest_unprotected = list.iter().filter(|m| m.id != protected).map(|m| m.id).min().unwrap();

        let (id, evicted) = apply_user_add(&mut list, "新しい設定", 2);
        assert!(id.is_some());
        assert_eq!(evicted.unwrap().id, oldest_unprotected, "最低スコア帯の最古が退場");
        assert!(list.iter().any(|m| m.id == protected), "編集済みの行は生き残る");
        assert_eq!(list.len(), FACTS_MAX);
    }

    /// 【権限は二値】`locked` (既定・非表示) と `open` (ユーザーが宣言する) だけ。
    ///
    /// 当初あった「削除のみ (prune)」は **GM も書く**前提の中間値だった (誤記憶を消せるよう
    /// 加算を封じて減算だけ許す非対称)。GM の書き込み経路を撤去した今、書き手はユーザー
    /// だけなので**足せないものは消せない** = 空虚な状態になり撤去した (failures.md #66)。
    #[test]
    fn facts_policy_is_binary_locked_or_open() {
        use gm_core::FactsPolicy;
        assert_eq!(FactsPolicy::default(), FactsPolicy::Locked, "宣言なしは非表示");
        assert!(!FactsPolicy::Locked.allows_write() && !FactsPolicy::Locked.is_visible());
        assert!(FactsPolicy::Open.allows_write() && FactsPolicy::Open.is_visible());
        // 書けるなら消せる (中間値は無い)。
        assert_eq!(FactsPolicy::Open.allows_delete(), FactsPolicy::Open.allows_write());
        assert_eq!(FactsPolicy::Locked.allows_delete(), FactsPolicy::Locked.allows_write());

        // package.yaml の宣言が全モジュールへ注入される。
        let sc = gm_core::Scenario::from_yaml(concat!(
            "title: t\nstart: r\n",
            "locations:\n  r: { description: d, items: {}, exits: [] }\n",
            "goal: { kind: always }\n"
        ))
        .unwrap();
        assert_eq!(sc.facts_policy, FactsPolicy::Locked);
        let mut sc2 = sc.clone();
        let manifest: crate::PackageManifest =
            serde_yaml::from_str("entry: x.yaml\nfacts_policy: open\n").unwrap();
        crate::inject_package(&mut sc2, &manifest);
        assert_eq!(sc2.facts_policy, FactsPolicy::Open);
    }

    /// 【読み上げの可否は作者宣言】`use_tts` は既定 false (宣言を持たない配布物 = 書庫の
    /// 既刊すべてを作者の意図どおり無音に置く) で、package.yaml の宣言が全モジュールを支配する
    /// (セッション単位 = 章の途中で再生設定が変わらない)。
    ///
    /// **engine 非使用の提示層宣言**であることも固定する — `use_tts` を true にしても
    /// prompt (`scenario_brief`) は 1 バイトも変わらない。TTS で語りが変わると
    /// chronicle/synopsis に残る記録まで再生設定で食い違うため (文体は `world` の役目)。
    #[test]
    fn use_tts_defaults_off_and_package_declaration_wins_without_touching_prompt() {
        let base = concat!(
            "title: t\nstart: r\n",
            "locations:\n  r: { description: d, items: {}, exits: [] }\n",
            "goal: { kind: always }\n"
        );
        let sc = gm_core::Scenario::from_yaml(base).unwrap();
        assert!(!sc.use_tts, "宣言なしは無音 (既刊を作者の意図どおりに置く)");

        // package 宣言が全モジュールを支配する。
        let mut injected = sc.clone();
        let manifest: crate::PackageManifest =
            serde_yaml::from_str("entry: x.yaml\nuse_tts: true\n").unwrap();
        crate::inject_package(&mut injected, &manifest);
        assert!(injected.use_tts, "package の宣言が scenario の既定を上書きする");

        // scenario 直書きも効く (package を持たない単発シナリオ)。
        let direct = gm_core::Scenario::from_yaml(&format!("{base}use_tts: true\n")).unwrap();
        assert!(direct.use_tts);

        // 提示層宣言ゆえ prompt は不変 = 語りが再生設定に依存しない。
        assert_eq!(
            crate::prompt::scenario_brief(&sc),
            crate::prompt::scenario_brief(&injected),
            "use_tts は prompt を変えない (文体は world の役目・TTS は再生手段)"
        );
    }

    /// 【注入】従属規律 (抑止+保護の対) + スコア降順。**空なら節を出さない**
    /// (GM は書けないので、空の節に意味がない = 書き込み経路の撤去に伴う収縮)。
    #[test]
    fn facts_note_grounds_subordination_and_sorts_by_score() {
        assert!(facts_note(&[]).is_none(), "空リストは節を出さない");

        let mut list = Vec::new();
        apply_user_add(&mut list, "低スコアの設定", 1);
        apply_user_add(&mut list, "高スコアの設定", 1);
        let hi = list.iter().find(|m| m.text == "高スコアの設定").unwrap().id;
        apply_user_edit(&mut list, hi, "高スコアの設定"); // +3

        let note = facts_note(&list).unwrap();
        // 出所の明示 (プレイヤーの宣言であることが GM の重み付けに効く)。
        assert!(note.contains("プレイヤーが宣言した設定"), "出所を明示: {note}");
        // 保護: 守れ。
        assert!(note.contains("必ず守ること"), "保護: {note}");
        // 抑止: state が優先・付与禁止 (#47 信頼チャネル対策)。
        assert!(note.contains("state と矛盾するときは常に state が正しく"), "抑止: {note}");
        assert!(note.contains("確定させてはならない"), "抑止 (付与禁止): {note}");
        // スコア降順。
        let hi_pos = note.find("高スコアの設定").unwrap();
        let lo_pos = note.find("低スコアの設定").unwrap();
        assert!(hi_pos < lo_pos, "スコア降順で注入: {note}");
    }
}
