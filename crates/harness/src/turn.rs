//! GM ターンループ。LocalAI `orchestrator.py::_self_repair_loop` と同型。
//!
//! 提案 → 裁定 → 却下なら理由を戻して再生成 → 受理なら原子適用。
//! 正本 (gm_core) が真実を握り、LLM の流暢な嘘はここで構造的に弾かれる。

use gm_core::{
    adjudicate, apply, CheckOutcome, FiredTrigger, GameState, Lang, RejectReason, RollOutcome,
    Scenario, StateDelta, Verdict,
};

use crate::memoria::MemoryFragment;
use llm_client::ChatMessage;

use crate::error::HarnessError;
use crate::proposer::DeltaProposer;
use crate::prompt;

/// 経緯ログの 1 エントリ (chronicle)。「経過を忘れる GM」対策 —
/// GM が毎ターン書く 1 行要約 ([`gm_core::StateDelta`] の `summary`、無ければ narration 冒頭) を
/// 呼び出し側 (CLI/app) が [`chronicle_entry`] で蓄積し、[`run_turn`] の `history` に渡すと
/// 「これまでの経緯」として prompt に還流される (recent_narration=直前 1 ターンの中期記憶版)。
/// 語り素材であって正本状態ではない (Memoria 同様、可変世界状態は持たない)。Serialize 派生は
/// 将来のセーブ同梱用。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TurnLog {
    /// 適用後のターン番号。
    pub turn: u32,
    /// プレイヤーの行動文 (そのターンの入力)。
    pub player: String,
    /// そのターンの経緯 1 行 (GM の summary、fallback は narration 冒頭)。
    pub summary: String,
}

/// 受理ターンから経緯ログの 1 エントリを作る。GM が `summary` を書いていればそれを、
/// 書いていない (弱モデル等) なら narration 冒頭を文字境界安全に切り詰めて使う。
///
/// `beats` は発火した反応ビートの authored narration。**GM は見ていない** (発火は提案後に
/// engine 側で起きる) ので GM の summary には現れない — ここで「出来事」として併記し、
/// 筋書きの出来事が経緯から抜け落ちないようにする。
pub fn chronicle_entry(
    turn: u32,
    player: &str,
    summary: &str,
    narration: &str,
    beats: &[String],
) -> TurnLog {
    let mut summary = if summary.trim().is_empty() {
        let flat = narration.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut head: String = flat.chars().take(80).collect();
        if flat.chars().count() > 80 {
            head.push('…');
        }
        head
    } else {
        summary.trim().to_string()
    };
    let beats: Vec<String> = beats
        .iter()
        .filter(|b| !b.trim().is_empty())
        .map(|b| b.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    if !beats.is_empty() {
        summary.push_str(&format!("／出来事: {}", beats.join("、")));
    }
    TurnLog { turn, player: player.to_string(), summary }
}

/// 次ターンへ持ち越す「直前までの語り」を組む。GM の narration に発火ビート (authored の
/// 筋書きの出来事) を連結する — ビートはプレイヤーには表示されるが GM は見ていないため、
/// ここで継続文脈に含めないと GM が出来事を知らないまま続きを語る (#27 のトリガー版)。
pub fn carryover_narration(narration: &str, beats: &[String]) -> String {
    let beats: Vec<&String> = beats.iter().filter(|b| !b.trim().is_empty()).collect();
    if beats.is_empty() {
        return narration.to_string();
    }
    let mut s = narration.trim_end().to_string();
    s.push_str("\n（直後に筋書きの出来事が起きた）");
    for b in beats {
        s.push('\n');
        s.push_str(b.trim());
    }
    s
}

/// 1 ターンの結末。
#[derive(Debug, Clone)]
pub enum TurnOutcome {
    /// 受理されて state に適用された。
    Accepted {
        narration: String,
        /// GM 自身が書いたこのターンの経緯 1 行 (StateDelta.summary、非検証)。
        /// 呼び出し側が [`chronicle_entry`] で経緯ログに積む素。
        summary: String,
        rolls: Vec<RollOutcome>,
        /// この適用で行われた技能判定の結果。次ターンの語りに還流される。
        checks: Vec<CheckOutcome>,
        /// この適用で発火した反応ビート (Phase C)。`narration` を語りに注入する。
        fired: Vec<FiredTrigger>,
        /// 受理までに要した試行回数 (1 = 一発合格)。
        attempts: u32,
        /// 受理前に却下された各試行の理由 (試行順)。空なら一発合格。提示層が「なぜ筋を通すのに
        /// N 回かかったか」を author に見せる素 (Grok 等で却下が多い時の診断)。
        rejected: Vec<Vec<RejectReason>>,
    },
    /// 最大試行回数まで却下され続けた。**state は無傷**。理由は構造化 (表示は localize)。
    Rejected {
        last_reasons: Vec<RejectReason>,
        attempts: u32,
    },
}

impl TurnOutcome {
    pub fn is_accepted(&self) -> bool {
        matches!(self, TurnOutcome::Accepted { .. })
    }
}

/// 1 ターンを回す。
///
/// `max_attempts` 回まで提案を裁定し、却下されるたびに理由を messages に積んで再生成させる。
/// 受理されたら `state` に原子適用して [`TurnOutcome::Accepted`]、尽きたら [`TurnOutcome::Rejected`]
/// (このとき `state` は一切変わっていない)。
///
/// `recalled_lore` は memoria_bridge: 直前ターンの発火で Memoria から recall された伏線。
/// 今回の語りに「思い出す様子」として織り込ませるため prompt に注入する (空なら注入しない)。
/// `recent_checks` は直前ターンの技能判定の結果。出目は apply 後に確定するので同一ターンの
/// narration に間に合わない → 次ターンの prompt に還流し、GM に結果へ沿って語らせる。
/// `recent_narration` は直前ターンの語り。継続文脈として渡し、既出情景の繰り返しを防ぐ
/// (毎ターン messages を新規構築するので LLM は自分の直前の語りを記憶していない)。
/// `history` は経緯ログ (chronicle)。過去ターンの 1 行要約列を「これまでの経緯」として
/// 注入し、GM が数ターン前の経過を保持する (recent_narration の中期記憶版)。
#[allow(clippy::too_many_arguments)]
pub async fn run_turn<P: DeltaProposer>(
    proposer: &P,
    state: &mut GameState,
    scenario: &Scenario,
    player_action: &str,
    max_attempts: u32,
    lang: Lang,
    recalled_lore: &[MemoryFragment],
    recent_checks: &[CheckOutcome],
    recent_narration: &str,
    history: &[TurnLog],
) -> Result<TurnOutcome, HarnessError> {
    // 盤面と現在状態を毎ターン新規に提示する (state は正本の唯一の真実)。
    // history=過去ターンの経緯、recalled_lore=思い出された伏線、recent_checks=直前判定の結果、
    // recent_narration=直前の語り (継続文脈、繰り返し禁止) を語りに還流する。
    let mut messages = vec![
        ChatMessage::system(format!(
            "{}\n\n{}",
            prompt::GM_SYSTEM,
            prompt::scenario_brief(scenario)
        )),
        ChatMessage::user(format!(
            "{}{}{}{}{}\n\n# プレイヤーの行動\n{}",
            prompt::state_brief(state, scenario),
            prompt::history_note(history),
            prompt::check_outcome_note(recent_checks),
            prompt::recalled_lore_note(recalled_lore),
            prompt::recent_narration_note(recent_narration),
            player_action
        )),
    ];

    let mut last_reasons = Vec::new();
    // 受理前に却下された各試行の理由 (試行順)。受理時に提示層へ渡し「なぜ N 回かかったか」を見せる。
    let mut rejected: Vec<Vec<RejectReason>> = Vec::new();

    for attempt in 1..=max_attempts.max(1) {
        let delta = proposer.propose(&messages).await?;

        match adjudicate(state, scenario, &delta) {
            Verdict::Accept => {
                // adjudicate が通ったので apply は成功するはず。RNG はここで決定論的に振られ、
                // 適用後に発火した反応ビート (Phase C) も返る。
                let out = apply(state, scenario, &delta)
                    .expect("adjudicate 済みなら apply は成功する");
                return Ok(TurnOutcome::Accepted {
                    narration: delta.narration,
                    summary: delta.summary,
                    rolls: out.rolls,
                    checks: out.checks,
                    fired: out.fired,
                    attempts: attempt,
                    rejected,
                });
            }
            Verdict::Reject { reasons } => {
                // 履歴の一貫性のため、LLM が出した提案 (の痕跡) と却下理由を会話に積む。
                push_rejection(&mut messages, &delta, &reasons, lang);
                rejected.push(reasons.clone());
                last_reasons = reasons;
            }
        }
    }

    Ok(TurnOutcome::Rejected {
        last_reasons,
        attempts: max_attempts.max(1),
    })
}

/// 却下された提案を assistant 発話として、修正指示を user 発話として積む (self_repair の核)。
/// 却下理由は `lang` でレンダリングして LLM に戻す。
fn push_rejection(
    messages: &mut Vec<ChatMessage>,
    delta: &StateDelta,
    reasons: &[RejectReason],
    lang: Lang,
) {
    // LLM 自身の直前の提案を会話履歴に残す (何を直すかの参照点になる)。
    let echoed = serde_json::to_string(delta)
        .unwrap_or_else(|_| delta.narration.clone());
    messages.push(ChatMessage::assistant(echoed));
    messages.push(ChatMessage::user(prompt::rejection_feedback(reasons, lang)));
}
