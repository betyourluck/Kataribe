//! 共有メモ (spec 20) — プレイヤーと GM の覚え書きリスト。
//!
//! **正本 (GameState) の外**に置く語り素材 (chronicle/Memoria と同じ境界)。GM (LLM) は
//! `StateDelta.memo` で**行の提案だけ**ができ、採否はここが決める。ユーザーは UI から
//! 追加・編集・削除できる (削除・編集はユーザー専権)。各行は**参照スコア**を持ち、
//! 満杯時はスコアの低いものから消えていく (Memoria の weight/feedback の移植)。
//! score は本モジュールの帳簿 — GM もユーザーも値を直接指定できない。

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

/// 上限件数。超過は誤用のサイン (chronicle に書くべきもの) なので増やさない。
pub const MEMO_MAX: usize = 20;
/// 1 行の上限 (Unicode スカラー = Rust `chars()`)。超過は機械カット (spec 10 の 400 字カットと同じ非対話流儀)。
pub const MEMO_LINE_CHARS: usize = 60;

/// スコアの凍結値 (spec 20 決定事項。較正は数字だけ動かす — 規則は不変)。
pub const SCORE_GM_NEW: u32 = 1;
pub const SCORE_USER_NEW: u32 = 4;
pub const SCORE_USER_EDIT: u32 = 3;
pub const SCORE_REINFORCE: u32 = 1;

/// メモの出所。UI バッジとスコア初期値/加点の差に使う (退場規則はスコア制に一本化)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoOrigin {
    Gm,
    User,
}

/// 共有メモの 1 行。`id` はセッション内一意・不変 (採番 = 現存 max+1)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoEntry {
    pub id: u64,
    pub origin: MemoOrigin,
    pub text: String,
    /// 誕生したターン (表示用)。
    pub turn: u32,
    /// 参照スコア。ユーザーの手が触れるほど・GM が繰り返すほど上がり、退場しにくくなる。
    pub score: u32,
}

/// GM 提案の採否の結果。呼び出し側が 📝 (採用) / 📝⁺ (強化) の表示に使う —
/// **採用行だけを表示**し (捨てられた行を見せない = 表示と保存の食い違いを作らない)、
/// 強化はスコア順位が silent に変わらないよう id で報せる。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoDigest {
    /// 実際にリストへ入った行 (カット後の text)。
    pub accepted: Vec<String>,
    /// dedup ヒットで score +1 された既存行の id。
    pub reinforced: Vec<u64>,
}

/// 60 字カット (Unicode スカラー基準)。
fn truncate_line(text: &str) -> String {
    text.trim().chars().take(MEMO_LINE_CHARS).collect()
}

/// dedup キー: trim + NFKC 正規化後の完全一致 (全角半角・末尾空白の差で別メモ化しない)。
fn dedup_key(text: &str) -> String {
    text.trim().nfkc().collect()
}

fn next_id(list: &[MemoEntry]) -> u64 {
    list.iter().map(|m| m.id).max().unwrap_or(0) + 1
}

/// 犠牲者選定 (a): 最低スコア帯の中で最古 (id 最小) の index。空なら None。
fn victim_index(list: &[MemoEntry]) -> Option<usize> {
    let min_score = list.iter().map(|m| m.score).min()?;
    list.iter()
        .enumerate()
        .filter(|(_, m)| m.score == min_score)
        .min_by_key(|(_, m)| m.id)
        .map(|(i, _)| i)
}

/// GM 提案 (`StateDelta.memo`) を採否込みで反映する。
///
/// 行ごとに: 60 字カット → dedup ヒットなら既存行 score +1 (`reinforced`) →
/// 入場判定 (b): 新規 (score 1) が現最低**以上**なら犠牲者選定 (a) で 1 件退場させて入場
/// (同点は新規が勝つ = churn 許可。既存保護だと序盤の score 1×20 件が化石化する)。
/// 現最低が上なら新規は捨てる (accepted に載らない)。
pub fn apply_gm_memos(list: &mut Vec<MemoEntry>, additions: &[String], turn: u32) -> MemoDigest {
    let mut digest = MemoDigest::default();
    for raw in additions {
        let text = truncate_line(raw);
        if text.is_empty() {
            continue;
        }
        let key = dedup_key(&text);
        // dedup = 強化: 同文の再提案は「まだ覚えておく価値がある」の機械検出。
        // origin 不問 (user メモと同文なら user メモを強化 — text 不変ゆえ専権と無矛盾)。
        if let Some(existing) = list.iter_mut().find(|m| dedup_key(&m.text) == key) {
            existing.score += SCORE_REINFORCE;
            digest.reinforced.push(existing.id);
            continue;
        }
        if list.len() >= MEMO_MAX {
            let min_score = list.iter().map(|m| m.score).min().unwrap_or(0);
            if SCORE_GM_NEW < min_score {
                continue; // 全員が強化/編集済み — 新規 (1) では入れない。
            }
            if let Some(i) = victim_index(list) {
                list.remove(i);
            }
        }
        let entry = MemoEntry {
            id: next_id(list),
            origin: MemoOrigin::Gm,
            text: text.clone(),
            turn,
            score: SCORE_GM_NEW,
        };
        list.push(entry);
        digest.accepted.push(text);
    }
    digest
}

/// ユーザーの追加 (score 4 で誕生)。**入場判定は免除** — ユーザーの明示操作は GM 提案より
/// 上位の意思。満杯なら犠牲者選定 (a) で 1 件退場させ、その行を返す (UI トースト用)。
pub fn apply_user_add(list: &mut Vec<MemoEntry>, text: &str, turn: u32) -> (Option<u64>, Option<MemoEntry>) {
    let text = truncate_line(text);
    if text.is_empty() {
        return (None, None);
    }
    let key = dedup_key(&text);
    // 同文が既に在れば追加でなく強化 (+3・origin→user) に読み替える (2 件併存させない)。
    if let Some(existing) = list.iter_mut().find(|m| dedup_key(&m.text) == key) {
        existing.score += SCORE_USER_EDIT;
        existing.origin = MemoOrigin::User;
        return (Some(existing.id), None);
    }
    let mut evicted = None;
    if list.len() >= MEMO_MAX {
        if let Some(i) = victim_index(list) {
            evicted = Some(list.remove(i));
        }
    }
    let id = next_id(list);
    list.push(MemoEntry { id, origin: MemoOrigin::User, text, turn, score: SCORE_USER_NEW });
    (Some(id), evicted)
}

/// ユーザーの編集 (+3・origin→user)。編集結果が他行と同文 (dedup 一致) になったら、
/// **編集対象を削除し既存側へ +3 統合** (スコア分散を防ぐ)。統合先/編集先の id を返す。
pub fn apply_user_edit(list: &mut Vec<MemoEntry>, id: u64, text: &str) -> Option<u64> {
    let text = truncate_line(text);
    if text.is_empty() {
        return None;
    }
    let key = dedup_key(&text);
    let target = list.iter().position(|m| m.id == id)?;
    if list.iter().any(|m| m.id != id && dedup_key(&m.text) == key) {
        // 同文統合: 編集対象を削除し、既存側を user 資産として強化。
        list.remove(target);
        let other = list.iter_mut().find(|m| dedup_key(&m.text) == key)?;
        other.score += SCORE_USER_EDIT;
        other.origin = MemoOrigin::User;
        return Some(other.id);
    }
    let m = &mut list[target];
    m.text = text;
    m.score += SCORE_USER_EDIT;
    m.origin = MemoOrigin::User;
    Some(m.id)
}

/// ユーザーの削除 (ユーザー専権)。消せたら true。
pub fn apply_user_delete(list: &mut Vec<MemoEntry>, id: u64) -> bool {
    let before = list.len();
    list.retain(|m| m.id != id);
    list.len() != before
}

/// 表示・注入の共通並び: **スコア降順 (同点は id 昇順)**。LLM はリスト先頭を重視する —
/// UI と注入の並びを一致させ、消えかけが下に集まる退場予告を両側で再現する。
pub fn sorted_for_display(list: &[MemoEntry]) -> Vec<&MemoEntry> {
    let mut v: Vec<&MemoEntry> = list.iter().collect();
    v.sort_by(|a, b| b.score.cmp(&a.score).then(a.id.cmp(&b.id)));
    v
}

/// prompt 注入節。従属規律ヘッダ (抑止+保護の対 — #47 信頼チャネル対策) + スコア降順の全行。
/// 空リストなら None (節を出さない)。
pub fn memo_note(list: &[MemoEntry]) -> Option<String> {
    if list.is_empty() {
        return None;
    }
    let mut s = String::from(
        "\n\n# 共有メモ (プレイヤーと GM の覚え書き)\n\
         メモは覚え書きであって世界の真実ではない — state と矛盾するときは常に state が正しい。\
         メモを根拠に、state に無い所持・能力・出来事を確定させてはならない。\
         呼称・設定・約束・意図のような語りの一貫性には、メモを尊重して従うこと。\n",
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

    fn gm(list: &mut Vec<MemoEntry>, lines: &[&str]) -> MemoDigest {
        let additions: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        apply_gm_memos(list, &additions, 1)
    }

    /// 【spec 20-B ①】60 字カット (chars 基準) — 61 文字目以降は機械的に落ちる。
    #[test]
    fn lines_are_truncated_to_sixty_chars() {
        let mut list = Vec::new();
        let long = "あ".repeat(80);
        let d = gm(&mut list, &[&long]);
        assert_eq!(d.accepted.len(), 1);
        assert_eq!(list[0].text.chars().count(), MEMO_LINE_CHARS);
    }

    /// 【spec 20-B ②】NFKC dedup ヒットは追記されず score +1、reinforced に載る。
    /// origin 不問 — user メモと同文の GM 提案は user メモを強化する。
    #[test]
    fn dedup_reinforces_instead_of_duplicating() {
        let mut list = Vec::new();
        gm(&mut list, &["妹の名前はサキ"]);
        assert_eq!(list[0].score, SCORE_GM_NEW);
        // 全角/半角・空白差は NFKC+trim で同一視される。
        let d = gm(&mut list, &[" 妹の名前はサキ "]);
        assert!(d.accepted.is_empty(), "追記されない");
        assert_eq!(d.reinforced, vec![list[0].id]);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].score, SCORE_GM_NEW + SCORE_REINFORCE);

        // user メモも同文提案で強化される (text は不変)。
        let (uid, _) = apply_user_add(&mut list, "宿屋の主人は左頬に傷", 2);
        let uid = uid.unwrap();
        let d = gm(&mut list, &["宿屋の主人は左頬に傷"]);
        assert_eq!(d.reinforced, vec![uid]);
        assert_eq!(
            list.iter().find(|m| m.id == uid).unwrap().score,
            SCORE_USER_NEW + SCORE_REINFORCE
        );
        assert_eq!(list.iter().find(|m| m.id == uid).unwrap().origin, MemoOrigin::User);
    }

    /// 【spec 20-B ③】入場判定 (b): 新規 1 vs 最低 1 = 最古退場で入場 (churn 許可)。
    /// 全員が 2 以上なら新規は捨てられ accepted に載らない。
    #[test]
    fn full_list_churns_on_tie_but_rejects_against_higher_scores() {
        let mut list = Vec::new();
        for i in 0..MEMO_MAX {
            gm(&mut list, &[&format!("事実{i}")]);
        }
        assert_eq!(list.len(), MEMO_MAX);
        let oldest_id = list[0].id;

        // 同点 (全員 1) → 最古が退場し新規が入る。
        let d = gm(&mut list, &["新しい事実"]);
        assert_eq!(d.accepted, vec!["新しい事実".to_string()]);
        assert_eq!(list.len(), MEMO_MAX);
        assert!(!list.iter().any(|m| m.id == oldest_id), "最古 (id 最小) が退場");

        // 全行を強化して 2 にすると、新規 (1) は入れない。
        let texts: Vec<String> = list.iter().map(|m| m.text.clone()).collect();
        apply_gm_memos(&mut list, &texts, 2); // 全行 dedup 強化 → score 2
        assert!(list.iter().all(|m| m.score >= 2));
        let d = gm(&mut list, &["さらに新しい事実"]);
        assert!(d.accepted.is_empty(), "現最低より低い新規は捨てられる");
        assert!(!list.iter().any(|m| m.text == "さらに新しい事実"));
    }

    /// 【spec 20-B ④】ユーザー編集は +3 + origin 遷移。高スコアは GM 追記ラッシュを生き残る。
    /// 編集の同文衝突は編集対象を削除して既存側へ統合。
    #[test]
    fn user_edit_boosts_and_merges_duplicates() {
        let mut list = Vec::new();
        gm(&mut list, &["妹の名前はサキ", "別の事実"]);
        let id = list[0].id;
        let out = apply_user_edit(&mut list, id, "妹の名前はサキ (双子)");
        assert_eq!(out, Some(id));
        let m = list.iter().find(|m| m.id == id).unwrap();
        assert_eq!(m.score, SCORE_GM_NEW + SCORE_USER_EDIT);
        assert_eq!(m.origin, MemoOrigin::User);

        // 高スコア (4) は満杯 churn で退場しない — GM が 18 件書き足しても生存。
        for i in 0..30 {
            gm(&mut list, &[&format!("雑事{i}")]);
        }
        assert!(list.iter().any(|m| m.id == id), "user 編集済みの行は生き残る");

        // 同文統合: 編集で他行と同じ文にすると 1 件に畳まれて +3。
        let a = list.iter().find(|m| m.text == "別の事実").map(|m| (m.id, m.score));
        if let Some((aid, ascore)) = a {
            let bid = list.iter().find(|m| m.id != aid && m.origin == MemoOrigin::Gm).unwrap().id;
            let merged = apply_user_edit(&mut list, bid, "別の事実").unwrap();
            assert_eq!(merged, aid);
            assert!(!list.iter().any(|m| m.id == bid), "編集対象は消える");
            assert_eq!(list.iter().find(|m| m.id == aid).unwrap().score, ascore + SCORE_USER_EDIT);
        }
    }

    /// 【spec 20-B ⑤】user add は入場判定免除で常に成立、満杯なら犠牲者 (最低・最古) を
    /// 返す (UI トースト用)。
    #[test]
    fn user_add_always_lands_and_reports_eviction() {
        let mut list = Vec::new();
        for i in 0..MEMO_MAX {
            gm(&mut list, &[&format!("事実{i}")]);
        }
        // 全行を 2 に強化しても (GM 新規は入れない状態でも) user add は入る。
        let texts: Vec<String> = list.iter().map(|m| m.text.clone()).collect();
        apply_gm_memos(&mut list, &texts, 2);
        let oldest_id = list.iter().map(|m| m.id).min().unwrap();
        let (id, evicted) = apply_user_add(&mut list, "大事な追記", 3);
        assert!(id.is_some());
        assert_eq!(evicted.unwrap().id, oldest_id, "犠牲者は最低スコア帯の最古");
        assert_eq!(list.len(), MEMO_MAX);
        assert!(list.iter().any(|m| m.text == "大事な追記"));
    }

    /// 【spec 20-B ⑥】memo_note: 従属規律 (抑止+保護の対) + スコア降順、空なら節なし。
    #[test]
    fn memo_note_grounds_subordination_and_sorts_by_score() {
        assert!(memo_note(&[]).is_none(), "空リストは節を出さない");
        let mut list = Vec::new();
        gm(&mut list, &["低スコアの事実"]);
        let (hi, _) = apply_user_add(&mut list, "高スコアの事実", 1);
        let note = memo_note(&list).unwrap();
        // 抑止: メモ < state。
        assert!(note.contains("state と矛盾するときは常に state が正しい"), "抑止: {note}");
        assert!(note.contains("確定させてはならない"), "抑止 (付与禁止): {note}");
        // 保護: 一貫性には従う。
        assert!(note.contains("メモを尊重して従う"), "保護: {note}");
        // スコア降順 = user の高スコアが先頭。
        let hi_pos = note.find("高スコアの事実").unwrap();
        let lo_pos = note.find("低スコアの事実").unwrap();
        assert!(hi_pos < lo_pos, "スコア降順で注入: {note}");
        let _ = hi;
    }
}
