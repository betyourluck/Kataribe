//! # gm_core — TRPG-GM の正本 (state machine)
//!
//! 三権分立の「**エンジン=裁定**」脚。LLM は数値の真実を持たない。
//! HP/所持品/ダイス/フラグ/位置の唯一の真実をここが決定論的に握り、
//! LLM が提案する [`StateDelta`] を [`adjudicate`] が裁く。
//!
//! 設計の掟:
//! - **純粋性**: [`adjudicate`] は `state` を一切変更しない。
//! - **原子性**: 1つでも不正な op があればデルタ全体を却下し、`state` は無傷。
//! - **決定論**: ダイスは seeded RNG。同じ seed/cursor は同じ目を返し、監査可能。

pub mod engine;
pub mod reason;
pub mod lint;
pub mod expr;
pub mod spine;
pub mod state;

pub use engine::{
    adjudicate, apply, contest_round, decision_options, is_goal, percentile_degree,
    resolve_decision, ApplyOutcome, BuyOption, CheckOutcome, ContestEnd, ContestError,
    ContestRound, DecisionChoice, DecisionError, DecisionOptions, DecisionResolution,
    FiredTrigger, RollOutcome, StatRollOutcome, Verdict,
};
pub use reason::{Lang, RejectReason};
pub use lint::unknown_key_lints;
pub use expr::{parse_expr, Expr};
pub use spine::{
    ChallengeDef, CharacterDef, CheckStyle, ContestDef, Exit, Gate, GoalDef, ImageMode, Location,
    LocationItem, Natural, Protagonist, Resolution, RoleAssignment, RollRef, RollSpec, Scenario,
    ScenarioError, StatDecl, TakeMode, TierDef, Trigger,
};
pub use state::{
    default_entity, AttrKey, ChallengeId, EntityId, FlagKey, GameState, GoalId, ItemId, LocationId,
    PendingContest, PendingDecision, RngState, SkillId, StatKey, StateDelta, StateOp, TriggerId,
    AUTHORED_ONLY_OPS, DEFAULT_GOAL, PLAYER,
};
