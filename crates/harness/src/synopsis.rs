//! あらすじ (spec 10) — GM の長期物語記憶とユーザー可視化。
//!
//! chronicle (中期記憶) の古い経緯を LLM で「章あらすじ」segment へ圧縮し、
//! prompt へ「これまでのあらすじ」として恒久注入する ([`crate::prompt::synopsis_note`])。
//! segment は **append-only** — 一度書いたら不変の確定した語り素材 (chronicle の
//! [`TurnLog`] と同格)。書き直しを許すと「要約の要約」で事実が落ちる複利ドリフトが
//! 起きるため、構造的に断つ。可変世界状態は持たない (正本は `GameState` 専有)。
//!
//! 圧縮の契機は 2 つ (spec 10 rev2):
//! - **あふれ** ([`Synopsis::next_job`]): 未圧縮が [`SYNOPSIS_OVERFLOW_THRESHOLD`] を
//!   超えたら直近 [`SYNOPSIS_KEEP_RECENT`] を温存して圧縮。失敗は skip して次ターン再計算。
//! - **モジュール遷移** ([`Synopsis::on_transition`]): 章の自然な境界で未圧縮全量を圧縮。
//!   失敗は `pending_transition` へ**範囲凍結**し同一範囲でリトライ (範囲を再計算すると
//!   新章のターンが混入し「前章の要約に次章の内容」事故になるため、拡張禁止)。
//!
//! 要約は非検証の言語チャネル (#47 と同族のリスク) なので、入力を chronicle の
//! summary + 機械タグ (spec 08-B の engine 事実) に接地し、発明禁止を指示する
//! ([`SynopsisRequest::system_prompt`])。

use serde::{Deserialize, Serialize};

use crate::error::HarnessError;
use crate::turn::TurnLog;

/// segment 本文の上限 (文字数)。LLM 指示・機械 join とも同上限でカットする。
pub const SYNOPSIS_TEXT_MAX: usize = 400;
/// あふれ閾値: 未圧縮エントリがこれを**超えたら**圧縮する。
pub const SYNOPSIS_OVERFLOW_THRESHOLD: usize = 20;
/// 温存幅: 直近この数のターンは常に未圧縮で残す (history_note の直近層と重なる帯)。
pub const SYNOPSIS_KEEP_RECENT: usize = 10;
/// 遷移時、未圧縮がこれ未満なら LLM を呼ばず機械 join で segment を作る。
pub const SYNOPSIS_MIN_LLM_TURNS: usize = 3;

/// あらすじ 1 章。一度書いたら不変 (append-only)。
///
/// `upto_turn` がこの章の覆う最終ターン (inclusive)。次章の範囲は必ず `upto_turn + 1`
/// から始まる — 境界のオフバイワン (重複・欠落) を構造で封じる。`title` は表示専用
/// (モジュール title は「ターン m〜n」形式の文字列でもあり得るため、識別には使わない)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SynopsisEntry {
    /// この segment が覆う最終ターン (inclusive)。UI のリスト key もこれ。
    pub upto_turn: u32,
    /// 章題 (遷移元モジュール title or「ターン m〜n」)。表示専用。
    pub title: String,
    /// 圧縮された物語 ([`SYNOPSIS_TEXT_MAX`] 字以内)。
    pub text: String,
}

/// 圧縮の契機。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SynopsisTrigger {
    /// 未圧縮のあふれ (単一モジュール長編)。失敗時は次ターンに範囲再計算でよい。
    Overflow,
    /// campaign モジュール遷移 (章の境界)。失敗時は範囲凍結・同一リトライ。
    Transition,
}

/// 圧縮ジョブ = 「どの範囲を、どの章題で」。範囲は **inclusive** `[start, end]`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SynopsisJob {
    /// 範囲の先頭ターン (inclusive)。必ず `前 segment の upto_turn + 1`。
    pub start: u32,
    /// 範囲の末尾ターン (inclusive)。complete でそのまま `upto_turn` になる。
    pub end: u32,
    /// 章題。
    pub title: String,
    /// 契機 (失敗時の分岐に使う)。
    pub trigger: SynopsisTrigger,
}

/// あらすじの器 (segment 列 + 遷移契機の凍結リトライ範囲)。
///
/// 呼び出し側 (app/CLI) が chronicle と並置で保持し、セーブへ丸ごと入れる
/// (`SessionSave.synopsis`、serde default = 旧セーブ互換)。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Synopsis {
    /// 確定した章列 (古い順・append-only)。
    #[serde(default)]
    pub entries: Vec<SynopsisEntry>,
    /// 遷移契機の失敗を凍結したリトライ範囲。消化されるまで新しい圧縮は起こさない
    /// (pending 優先消化 — あふれ契機が凍結範囲を再カバーする事故を構造で防ぐ)。
    #[serde(default)]
    pub pending_transition: Option<SynopsisJob>,
}

impl Synopsis {
    /// 圧縮済み境界 = 最後の segment が覆う最終ターン (無ければ 0)。
    /// 次の圧縮範囲は必ずここ +1 から始まる。
    pub fn compressed_upto(&self) -> u32 {
        self.entries.last().map(|e| e.upto_turn).unwrap_or(0)
    }

    /// 未圧縮の chronicle エントリ (turn > compressed_upto)。
    fn uncompressed<'a>(&self, history: &'a [TurnLog]) -> Vec<&'a TurnLog> {
        let upto = self.compressed_upto();
        history.iter().filter(|l| l.turn > upto).collect()
    }

    /// このターンに回すべき圧縮ジョブ (受理ターン毎に 1 回呼ぶ)。
    ///
    /// 凍結中の遷移ジョブがあれば**同一範囲のまま**それを返す (リトライ。凍結が消化される
    /// まであふれ判定はしない = 1 ターン 1 ジョブ・順序保証)。無ければあふれ判定:
    /// 未圧縮が閾値を超えていたら、直近 [`SYNOPSIS_KEEP_RECENT`] を温存した範囲を返す。
    pub fn next_job(&self, history: &[TurnLog]) -> Option<SynopsisJob> {
        if let Some(pending) = &self.pending_transition {
            return Some(pending.clone());
        }
        let unc = self.uncompressed(history);
        if unc.len() <= SYNOPSIS_OVERFLOW_THRESHOLD {
            return None;
        }
        // 温存幅を残した最後のエントリの turn が end (未圧縮がちょうど KEEP_RECENT 残る)。
        let end = unc[unc.len() - 1 - SYNOPSIS_KEEP_RECENT].turn;
        let start = self.compressed_upto() + 1;
        Some(SynopsisJob {
            start,
            end,
            title: format!("ターン {start}〜{end}"),
            trigger: SynopsisTrigger::Overflow,
        })
    }

    /// モジュール遷移の契機。遷移が確定した時点 (章替わりマーカーを刻む**前**) に呼ぶ。
    ///
    /// - 前回遷移の凍結ジョブが残っていれば、まず**機械 join で強制消化**する
    ///   (二重 pending を作らない。機械 join は LLM 不要で必ず成功する)。
    /// - 未圧縮 0 なら何もしない。[`SYNOPSIS_MIN_LLM_TURNS`] 未満なら LLM を呼ばず
    ///   機械 join で segment を確定し `None` (2 ターンの章に 1 リクエストは割に合わない)。
    /// - それ以外は LLM 用ジョブを返す — 成功なら [`Self::complete`]、失敗なら
    ///   [`Self::abandon`] で凍結すること。
    pub fn on_transition(&mut self, history: &[TurnLog], module_title: &str) -> Option<SynopsisJob> {
        if let Some(pending) = self.pending_transition.take() {
            let text = mechanical_join(&self.logs_in(history, &pending));
            self.push_entry(&pending, &text);
        }
        let unc = self.uncompressed(history);
        let Some(last) = unc.last() else {
            return None; // 未圧縮なし (遷移直後の再遷移など)
        };
        let job = SynopsisJob {
            start: self.compressed_upto() + 1,
            end: last.turn,
            title: module_title.to_string(),
            trigger: SynopsisTrigger::Transition,
        };
        if unc.len() < SYNOPSIS_MIN_LLM_TURNS {
            let text = mechanical_join(&unc);
            self.push_entry(&job, &text);
            return None;
        }
        Some(job)
    }

    /// 要約成功: segment を確定して追記する (400 字カット)。pending だったジョブなら解凍。
    pub fn complete(&mut self, job: &SynopsisJob, text: &str) {
        if self.pending_transition.as_ref() == Some(job) {
            self.pending_transition = None;
        }
        self.push_entry(job, text);
    }

    /// 要約失敗: 遷移契機なら**範囲を凍結**して同一リトライへ (拡張禁止)。
    /// あふれ契機は何もしない (次の受理ターンで再計算 — 章を跨がないので安全)。
    pub fn abandon(&mut self, job: &SynopsisJob) {
        if job.trigger == SynopsisTrigger::Transition {
            self.pending_transition = Some(job.clone());
        }
    }

    /// ジョブの範囲 (inclusive) に入る chronicle エントリ。
    fn logs_in<'a>(&self, history: &'a [TurnLog], job: &SynopsisJob) -> Vec<&'a TurnLog> {
        history.iter().filter(|l| l.turn >= job.start && l.turn <= job.end).collect()
    }

    /// append-only の追記 (境界の後退は defensive に無視)。
    fn push_entry(&mut self, job: &SynopsisJob, text: &str) {
        if job.end <= self.compressed_upto() {
            return; // 既に覆われた範囲 (呼び出しバグ) — 巻き戻さない
        }
        self.entries.push(SynopsisEntry {
            upto_turn: job.end,
            title: job.title.clone(),
            text: truncate_chars(text.trim(), SYNOPSIS_TEXT_MAX),
        });
    }

    /// ジョブから要約リクエストを組む (機械タグ接地 + 前章 tail)。
    pub fn build_request(&self, history: &[TurnLog], job: &SynopsisJob) -> SynopsisRequest {
        SynopsisRequest {
            title: job.title.clone(),
            lines: self.logs_in(history, job).iter().map(|l| grounded_line(l)).collect(),
            prev_tail: self.entries.last().map(|e| tail_of(&e.text)).unwrap_or_default(),
        }
    }
}

/// 要約への入力 (章題 + 接地済み記録行 + 前章の末尾)。
///
/// prompt の文面はここに集約する — 発明禁止・タグ接地・tail 文体限定 (#47 防衛) を
/// Phase A でテスト可能にし、[`Summarizer`] 実装は文面を持たない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SynopsisRequest {
    /// 章題。
    pub title: String,
    /// 記録行 (summary + 機械タグ、古い順)。
    pub lines: Vec<String>,
    /// 直前 segment の末尾 1〜2 文 (文体接続のみ。空なら最初の章)。
    pub prev_tail: String,
}

impl SynopsisRequest {
    /// 要約者への規律 (system)。あらすじは非検証チャネルなので、ここが唯一の防衛線。
    pub fn system_prompt(&self) -> String {
        format!(
            "あなたは TRPG セッションの記録係です。渡された確定記録だけを素材に、\
             この章のあらすじを {SYNOPSIS_TEXT_MAX} 字以内で書いてください。\n\
             規律:\n\
             - 記録に無い新事実を発明しない。解釈や推測を書かない。\n\
             - 固有名 (人物・場所・アイテム) は記録の表記のまま使う。\n\
             - 「前章の末尾」は文体の接続のみに参照し、事実の出典として使わない。\n\
             - 出力はあらすじ本文のみ (前置き・見出し・箇条書きを付けない)。"
        )
    }

    /// 要約対象 (user)。
    pub fn user_prompt(&self) -> String {
        let tail = if self.prev_tail.is_empty() {
            "(なし — 最初の章)".to_string()
        } else {
            self.prev_tail.clone()
        };
        format!(
            "# 章題\n{}\n\n# 前章の末尾 (文体接続用・事実の出典にしない)\n{}\n\n# 確定記録 (古い順)\n{}",
            self.title,
            tail,
            self.lines.join("\n")
        )
    }
}

/// あらすじの要約者。[`crate::DeltaProposer`] と同型の依存性逆転 —
/// 実装は `llm_client::LlmClient` (Phase B)、テストは scripted fake。
#[allow(async_fn_in_trait)] // 本 crate 内でしか実装/消費しないため dyn 化の懸念なし
pub trait Summarizer {
    /// リクエストから章あらすじ本文を返す。失敗は Err (呼び出し側が abandon)。
    async fn summarize(&self, request: &SynopsisRequest) -> Result<String, HarnessError>;
}

/// chronicle 1 行を engine 事実タグ込みで整形する (要約入力の接地素材)。
fn grounded_line(log: &TurnLog) -> String {
    let mut s = format!("T{} プレイヤー「{}」→ {}", log.turn, log.player, log.summary);
    let mut tags: Vec<String> = Vec::new();
    if !log.location.is_empty() {
        tags.push(format!("場所:{}", log.location));
    }
    if !log.present.is_empty() {
        tags.push(format!("同席:{}", log.present.join("・")));
    }
    if !log.flags_set.is_empty() {
        tags.push(format!("成立:{}", log.flags_set.join("・")));
    }
    if !log.items.is_empty() {
        tags.push(format!("持物:{}", log.items.join("・")));
    }
    if !log.checks.is_empty() {
        tags.push(format!("判定:{}", log.checks.join("・")));
    }
    if !tags.is_empty() {
        s.push_str(&format!("〔{}〕", tags.join("／")));
    }
    s
}

/// LLM を使わない機械 join fallback。summary に **location / items タグを併記**する
/// (summary は LLM 産で幻覚があり得る — engine 事実を必ず混ぜて確定化の毒を薄める)。
/// [`SYNOPSIS_TEXT_MAX`] でカット。
pub fn mechanical_join(logs: &[&TurnLog]) -> String {
    let lines: Vec<String> = logs
        .iter()
        .map(|l| {
            let mut s = format!("T{} {}", l.turn, l.summary);
            let mut tags: Vec<String> = Vec::new();
            if !l.location.is_empty() {
                tags.push(l.location.clone());
            }
            if !l.items.is_empty() {
                tags.push(l.items.join("・"));
            }
            if !tags.is_empty() {
                s.push_str(&format!("〔{}〕", tags.join("／")));
            }
            s
        })
        .collect();
    truncate_chars(&lines.join("\n"), SYNOPSIS_TEXT_MAX)
}

/// 文字境界安全な切り詰め (超過時は末尾に …)。
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut head: String = s.chars().take(max.saturating_sub(1)).collect();
    head.push('…');
    head
}

/// 本文の末尾 1〜2 文 (文体接続用の tail)。「。」区切りの最後の 2 文、無ければ末尾 80 字。
fn tail_of(text: &str) -> String {
    let sentences: Vec<&str> = text.split_inclusive('。').collect();
    if sentences.len() >= 2 {
        sentences[sentences.len() - 2..].concat().trim().to_string()
    } else if !sentences.is_empty() {
        truncate_chars(sentences[sentences.len() - 1].trim(), 80)
    } else {
        String::new()
    }
}

// =============================================================================
// PoC (spec 10 Phase A、Red→Green)
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用 TurnLog (turn + summary + 任意タグ)。
    fn log(turn: u32, summary: &str) -> TurnLog {
        TurnLog {
            turn,
            player: format!("行動{turn}"),
            summary: summary.to_string(),
            ..Default::default()
        }
    }
    fn logs(n: u32) -> Vec<TurnLog> {
        (1..=n).map(|t| log(t, &format!("出来事{t}"))).collect()
    }

    /// 【あふれ契機 + 境界接続】21 ターンで発火し、直近 10 を温存した inclusive 範囲を返す。
    /// complete 後の次回範囲は必ず「前回 upto+1」から — オフバイワン (重複・欠落) なし。
    #[test]
    fn overflow_compacts_keeping_recent_and_ranges_connect() {
        let mut syn = Synopsis::default();
        // 20 ターンでは発火しない (閾値は「超えたら」)。
        assert!(syn.next_job(&logs(20)).is_none(), "閾値ちょうどでは圧縮しない");

        let history = logs(21);
        let job = syn.next_job(&history).expect("21 ターンで発火");
        assert_eq!((job.start, job.end), (1, 11), "直近 10 (T12〜21) を温存");
        assert_eq!(job.trigger, SynopsisTrigger::Overflow);
        assert!(job.title.contains("1〜11"), "章題にターン範囲: {}", job.title);
        syn.complete(&job, "第一章のあらすじ。");
        assert_eq!(syn.compressed_upto(), 11);

        // 31 ターンまで進むと未圧縮 20 (T12〜31) = 閾値ちょうど → まだ。32 で発火。
        assert!(syn.next_job(&logs(31)).is_none());
        let history = logs(32);
        let job2 = syn.next_job(&history).expect("未圧縮 21 で再発火");
        assert_eq!(job2.start, 12, "次回 start = 前回 upto_turn+1 (接続保証)");
        assert_eq!(job2.end, 22, "直近 10 (T23〜32) を温存");
        syn.complete(&job2, "第二章のあらすじ。");
        assert_eq!(
            syn.entries.iter().map(|e| e.upto_turn).collect::<Vec<_>>(),
            vec![11, 22],
            "append-only で古い順に積まれる"
        );
    }

    /// 【遷移契機の凍結リトライ】遷移の要約失敗は範囲を凍結し、新章のターンが進んでも
    /// **同一範囲のまま**リトライする — 「前章の要約に次章の内容が混入」事故の遮断。
    #[test]
    fn transition_failure_freezes_range_and_retries_identically() {
        let mut syn = Synopsis::default();
        let mut history = logs(8);
        let job = syn.on_transition(&history, "村の章").expect("8 ターンは LLM 対象");
        assert_eq!((job.start, job.end), (1, 8));
        assert_eq!(job.title, "村の章");
        assert_eq!(job.trigger, SynopsisTrigger::Transition);

        // 要約失敗 → 凍結。
        syn.abandon(&job);
        assert!(syn.pending_transition.is_some(), "遷移失敗は pending へ凍結");

        // 新章のターンが進む (T9〜11)。
        history.extend((9..=11).map(|t| log(t, &format!("新章の出来事{t}"))));
        let retry = syn.next_job(&history).expect("凍結ジョブをリトライ");
        assert_eq!(&retry, &job, "範囲・章題とも同一 (拡張禁止 = T9〜11 は混入しない)");

        // リトライ成功 → 解凍・確定。以後のあふれ判定は凍結範囲の後ろから。
        syn.complete(&retry, "村の章のあらすじ。");
        assert!(syn.pending_transition.is_none(), "成功で解凍");
        assert_eq!(syn.compressed_upto(), 8);
        assert!(syn.next_job(&history).is_none(), "未圧縮 3 ターンでは何も起きない");
    }

    /// 【短章の機械 join + タグ併記】遷移時に未圧縮 3 ターン未満なら LLM を呼ばず
    /// 機械 join で segment を確定する。join には location / items (engine 事実) が併記される
    /// — summary だけだと GM が幻覚した文がそのまま確定化するため。
    #[test]
    fn short_transition_uses_mechanical_join_with_engine_tags() {
        let mut syn = Synopsis::default();
        let mut l1 = log(1, "祠の鍵を拾った");
        l1.location = "shrine".into();
        l1.items = vec!["+祠の鍵".into()];
        let l2 = log(2, "扉へ向かった");
        let history = vec![l1, l2];

        let job = syn.on_transition(&history, "序章");
        assert!(job.is_none(), "3 ターン未満は LLM を呼ばない");
        assert_eq!(syn.entries.len(), 1, "機械 join で即確定");
        let e = &syn.entries[0];
        assert_eq!(e.upto_turn, 2);
        assert_eq!(e.title, "序章");
        assert!(e.text.contains("祠の鍵を拾った"), "summary が入る: {}", e.text);
        assert!(e.text.contains("shrine"), "location タグ併記: {}", e.text);
        assert!(e.text.contains("+祠の鍵"), "items タグ併記: {}", e.text);
    }

    /// 【二重 pending の遮断】凍結が残ったまま次の遷移が来たら、凍結分を機械 join で
    /// 強制消化してから新章のジョブを組む (pending スロットは常に高々 1)。
    #[test]
    fn second_transition_flushes_frozen_pending_mechanically() {
        let mut syn = Synopsis::default();
        let mut history = logs(5);
        let job = syn.on_transition(&history, "第一章").unwrap();
        syn.abandon(&job); // 失敗のまま次の遷移へ

        history.extend((6..=9).map(|t| log(t, &format!("出来事{t}"))));
        let job2 = syn.on_transition(&history, "第二章").expect("新章のジョブ");
        assert_eq!(syn.entries.len(), 1, "凍結分は機械 join で確定済み");
        assert_eq!(syn.entries[0].upto_turn, 5);
        assert_eq!(syn.entries[0].title, "第一章");
        assert_eq!((job2.start, job2.end), (6, 9), "新章は凍結範囲の直後から");
        assert!(syn.pending_transition.is_none());
    }

    /// 【要約入力の接地 (#47 防衛)】リクエストは機械タグを行に併記し、system 指示に
    /// 発明禁止・表記固定・tail 文体限定・本文のみ、を刷り込む。tail は前章の末尾から取る。
    #[test]
    fn synopsis_request_grounds_tags_and_forbids_invention() {
        let mut syn = Synopsis::default();
        syn.entries.push(SynopsisEntry {
            upto_turn: 3,
            title: "序章".into(),
            text: "村に着いた。長老から祠の話を聞いた。".into(),
        });
        let mut l4 = log(4, "祠へ向かった");
        l4.location = "shrine".into();
        l4.present = vec!["alice".into()];
        l4.flags_set = vec!["door_open".into()];
        l4.checks = vec!["player STR 1d20+3=17 vs DC15 成功".into()];
        let history = vec![log(3, "旅立った"), l4, log(5, "祭壇を調べた")];
        let job = SynopsisJob {
            start: 4,
            end: 5,
            title: "祠の章".into(),
            trigger: SynopsisTrigger::Transition,
        };

        let req = syn.build_request(&history, &job);
        assert_eq!(req.lines.len(), 2, "範囲は inclusive [4,5] (T3 は入らない)");
        assert!(req.lines[0].contains("場所:shrine"), "location 接地: {}", req.lines[0]);
        assert!(req.lines[0].contains("同席:alice"), "present 接地");
        assert!(req.lines[0].contains("成立:door_open"), "flag 接地");
        assert!(req.lines[0].contains("判定:"), "check 接地");
        assert!(req.prev_tail.contains("祠の話を聞いた"), "前章末尾が tail: {}", req.prev_tail);

        let sys = req.system_prompt();
        assert!(sys.contains("発明しない"), "発明禁止");
        assert!(sys.contains("記録の表記のまま"), "固有名の表記固定");
        assert!(sys.contains("文体の接続のみ"), "tail の文体限定 (#47 防衛)");
        assert!(sys.contains("あらすじ本文のみ"), "前置き・見出し禁止");
        let user = req.user_prompt();
        assert!(user.contains("祠の章"), "章題");
        assert!(user.contains("事実の出典にしない"), "tail 節にも限定を明記");
    }

    /// 【400 字カット + append-only の防御】complete は本文を文字境界安全に切り詰め、
    /// 既に覆われた範囲への追記 (呼び出しバグ) は黙って無視する (巻き戻さない)。
    #[test]
    fn complete_truncates_and_never_regresses() {
        let mut syn = Synopsis::default();
        let long = "あ".repeat(500);
        let job = SynopsisJob {
            start: 1,
            end: 10,
            title: "章".into(),
            trigger: SynopsisTrigger::Overflow,
        };
        syn.complete(&job, &long);
        assert_eq!(syn.entries[0].text.chars().count(), SYNOPSIS_TEXT_MAX, "400 字で切る");
        assert!(syn.entries[0].text.ends_with('…'));

        // 後退する範囲は無視。
        let stale = SynopsisJob {
            start: 1,
            end: 5,
            title: "古".into(),
            trigger: SynopsisTrigger::Overflow,
        };
        syn.complete(&stale, "巻き戻し");
        assert_eq!(syn.entries.len(), 1, "後退追記は無視される");
        assert_eq!(syn.compressed_upto(), 10);
    }
}
