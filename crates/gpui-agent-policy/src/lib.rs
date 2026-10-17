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
//! Modules that arrive with later milestones are stubs: `action_space`
//! and `policy`/`text`/`goal`/`runner` (M4 `locate`/`act`/`run`),
//! `escalate` (M7 abstain escalation).
#![forbid(unsafe_code)]

pub mod arbitration;
pub mod evidence;

pub use arbitration::{ArbitrationError, Choice, RankedCandidate, choose};
pub use evidence::{
    EvidenceTarget, NON_EXACT_CAP, OperationKind, UNNAMED_OPERATION_CAP, label_covers_target,
    label_names_target, lexical_overlap, name_in_intent, name_in_intent_lowered,
    overlap_normalized, score_action, score_operation, target_phrase, tokens,
};
pub use instinct_core::{Confidence, Profile};
pub use instinct_text::{NormalizeConfig, Normalized, normalize};
