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

/// 1 ターンの結末。
#[derive(Debug, Clone)]
pub enum TurnOutcome {
    /// 受理されて state に適用された。
    Accepted {
        narration: String,
        rolls: Vec<RollOutcome>,
        /// この適用で行われた技能判定の結果。次ターンの語りに還流される。
        checks: Vec<CheckOutcome>,
        /// この適用で発火した反応ビート (Phase C)。`narration` を語りに注入する。
        fired: Vec<FiredTrigger>,
        /// 受理までに要した試行回数 (1 = 一発合格)。
        attempts: u32,
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
) -> Result<TurnOutcome, HarnessError> {
    // 盤面と現在状態を毎ターン新規に提示する (state は正本の唯一の真実)。
    // recalled_lore=思い出された伏線、recent_checks=直前判定の結果、recent_narration=直前の語り
    // (継続文脈、繰り返し禁止) を語りに還流する。
    let mut messages = vec![
        ChatMessage::system(format!(
            "{}\n\n{}",
            prompt::GM_SYSTEM,
            prompt::scenario_brief(scenario)
        )),
        ChatMessage::user(format!(
            "{}{}{}{}\n\n# プレイヤーの行動\n{}",
            prompt::state_brief(state),
            prompt::check_outcome_note(recent_checks),
            prompt::recalled_lore_note(recalled_lore),
            prompt::recent_narration_note(recent_narration),
            player_action
        )),
    ];

    let mut last_reasons = Vec::new();

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
                    rolls: out.rolls,
                    checks: out.checks,
                    fired: out.fired,
                    attempts: attempt,
                });
            }
            Verdict::Reject { reasons } => {
                // 履歴の一貫性のため、LLM が出した提案 (の痕跡) と却下理由を会話に積む。
                push_rejection(&mut messages, &delta, &reasons, lang);
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
