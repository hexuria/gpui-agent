//! Deterministic, local policy for `gpui-agent` — Instinct-backed.
//!
//! Same architecture as aui's `observe → action-space → policy → gate`
//! loop, minus the browser: an intent or goal is scored against a closed
//! menu of candidates, [`arbitration::choose`] gates it through Instinct
//! (`CandidateSet` + `Scores` + `arbitrate`), and an `Abstain` answer
//! can never be dispatched. No floats in the decision path, no network,
//! no model — deterministic decisions in microseconds.
//!
//! `evidence` is a port of `aui-policy/src/evidence.rs`: integer milli
//! scores (`0..=1000`), exact-label saturation, `NON_EXACT_CAP`,
//! `UNNAMED_OPERATION_CAP`, and `target_phrase` (goal minus quoted
//! payload, leading verb, connectives). A target's evidence text is its
//! own `name` + `role` — ancestor names are not concatenated.
//!
//! `action_space`/`policy`/`text`/`goal` are the M4 observe → decide
//! surface: `ActionSpace::from_tree` maps a `UiTree` into offered
//! actions, `UiPolicy`/`InstinctPolicy` is the two-head decide ported
//! from `aui-policy/src/instinct_policy.rs`, `TextResolver` supplies
//! TYPE/SET_VALUE payloads, and `split_sequential_clauses` cuts
//! `then`/`and then` goals into single intents. `escalate` (M7) adds an
//! optional abstain-only fallback over a closed command wire — the
//! local policy still decides first and an abstain is never re-scored.
#![forbid(unsafe_code)]

pub mod action_space;
pub mod arbitration;
pub mod escalate;
pub mod evidence;
pub mod goal;
pub mod policy;
pub mod runner;
pub mod text;

pub use action_space::{
    ActionId, ActionKind, ActionSpace, ActionSpaceError, OfferedAction, OfferedInvoke,
};
pub use arbitration::{ArbitrationError, Choice, RankedCandidate, choose};
pub use escalate::{
    AbstainReport, CommandEscalation, DEFAULT_TIMEOUT, ESCALATION_PROTOCOL, EscalatingPolicy,
    STDOUT_CAP, UiEscalate,
};
pub use evidence::{
    EvidenceTarget, NON_EXACT_CAP, OperationKind, UNNAMED_OPERATION_CAP, label_covers_target,
    label_names_target, lexical_overlap, name_in_intent, name_in_intent_lowered,
    overlap_normalized, score_action, score_operation, target_phrase, tokens,
};
pub use goal::{UiGoal, split_sequential_clauses};
pub use instinct_core::{Confidence, Profile};
pub use instinct_text::{NormalizeConfig, Normalized, normalize};
pub use policy::{
    HABITUATION_STEP, HistoryEntry, InstinctPolicy, PolicyDecision, PolicyError, PolicyOutcome,
    RankedAction, UiPolicy,
};
pub use runner::{
    Candidate, EngineInfo, InvokeArgs, RunOutcome, RunReceipt, Runner, RunnerCaps, StepRecord,
    WAIT_POLL_MS,
};
pub use text::{DeterministicTextResolver, TextContext, TextError, TextResolution, TextResolver};
