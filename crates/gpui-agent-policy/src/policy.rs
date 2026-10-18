//! Default deterministic UI policy backed by Instinct.
//!
//! Port of `aui-policy/src/instinct_policy.rs:44-382` (two-head decide:
//! operation head, then target head) and its habituation
//! (`HABITUATION_STEP = 250`). Hard-invalid nodes are already absent
//! from [`ActionSpace`]; this policy never applies hidden/disabled as
//! scores. Dangerous actions are offered but never auto-chosen —
//! choosing one abstains `needs_confirmation`.

use std::collections::BTreeMap;
use std::fmt;

use instinct_core::{
    Affinity, Answer, CandidateId, CandidateSet, Confidence, DriveIndex, Drives, Profile, Scores,
    arbitrate,
};

use crate::action_space::{ActionId, ActionKind, ActionSpace, OfferedAction};
use crate::evidence::{
    EvidenceTarget, GoalEvidence, fold, object_covered, quoted_literals, score_action_ctx,
    score_operation_ctx, tokenize, verb_matches,
};
use crate::goal::UiGoal;

pub const HABITUATION_STEP: i16 = 250;

/// One ranked candidate the policy considered (for trails / escalation).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RankedAction {
    pub id: ActionId,
    pub kind: ActionKind,
    pub label: String,
    /// Instinct confidence millis 0..=1000 (not a probability).
    pub confidence_millis: i16,
}

/// Successful finite choice from a policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyDecision {
    pub action_id: ActionId,
    pub kind: ActionKind,
    pub target_label: String,
    pub confidence_millis: i16,
    pub operation_ranked: Vec<RankedAction>,
    pub target_ranked: Vec<RankedAction>,
}

impl PolicyDecision {
    pub fn is_terminal(&self) -> bool {
        matches!(self.kind, ActionKind::Done | ActionKind::Blocked)
    }
}

/// Policy result: choose, abstain, or hard error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyOutcome {
    Choice(PolicyDecision),
    Abstain {
        reason: String,
        operation_ranked: Vec<RankedAction>,
        target_ranked: Vec<RankedAction>,
    },
}

impl PolicyOutcome {
    pub fn as_choice(&self) -> Option<&PolicyDecision> {
        match self {
            Self::Choice(d) => Some(d),
            Self::Abstain { .. } => None,
        }
    }
}

/// Errors that prevent a decision attempt (not abstention).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyError {
    EmptyGoal,
    EmptyActionSpace,
    Internal(String),
}

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyGoal => f.write_str("agent goal is empty"),
            Self::EmptyActionSpace => f.write_str("action space has no candidates"),
            Self::Internal(msg) => write!(f, "policy internal: {msg}"),
        }
    }
}

impl std::error::Error for PolicyError {}

/// Past executed / observed steps (consumer-owned; policy may use for
/// repetition dampening and done-detection).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryEntry {
    pub step: u32,
    pub action_id: ActionId,
    pub kind: ActionKind,
    pub label: String,
    pub verification: String,
}

impl HistoryEntry {
    /// The `{op, id, effect}` escalation-wire row — shared by the
    /// `choose` request and the jev `state.recent_actions` payload.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "op": self.kind.as_str(),
            "id": self.action_id.as_str(),
            "effect": self.verification,
        })
    }
}

/// Policy over a finite [`ActionSpace`].
pub trait UiPolicy {
    fn decide(
        &mut self,
        space: &ActionSpace,
        goal: &UiGoal,
        history: &[HistoryEntry],
    ) -> Result<PolicyOutcome, PolicyError>;
}

impl<T: UiPolicy + ?Sized> UiPolicy for Box<T> {
    fn decide(
        &mut self,
        space: &ActionSpace,
        goal: &UiGoal,
        history: &[HistoryEntry],
    ) -> Result<PolicyOutcome, PolicyError> {
        (**self).decide(space, goal, history)
    }
}

/// Instinct-backed finite policy.
#[derive(Clone, Debug)]
pub struct InstinctPolicy {
    profile: Profile,
}

impl Default for InstinctPolicy {
    fn default() -> Self {
        Self {
            profile: Profile::Standard,
        }
    }
}

impl InstinctPolicy {
    pub fn new(profile: Profile) -> Self {
        Self { profile }
    }

    pub fn profile(&self) -> Profile {
        self.profile
    }
}

fn evidence_of<'a>(action: &'a OfferedAction) -> EvidenceTarget<'a> {
    EvidenceTarget {
        id: action.id().as_str(),
        label: action.label(),
        role: action.role(),
        kind: action.kind().operation_kind(),
    }
}

impl UiPolicy for InstinctPolicy {
    fn decide(
        &mut self,
        space: &ActionSpace,
        goal: &UiGoal,
        history: &[HistoryEntry],
    ) -> Result<PolicyOutcome, PolicyError> {
        if goal.is_empty() {
            return Err(PolicyError::EmptyGoal);
        }
        if space.is_empty() {
            return Err(PolicyError::EmptyActionSpace);
        }

        let goal_text = goal.as_str();
        let ctx = goal_evidence(goal_text, space);

        // --- Operation head -------------------------------------------------
        let kinds = offered_kinds(space);
        let mut op_scores = BTreeMap::new();
        let mut op_ids = BTreeMap::new();
        for kind in kinds.iter().copied() {
            let best = space
                .targets_of(kind)
                .max_by_key(|a| score_action_ctx(goal_text, &evidence_of(a), &ctx).get());
            let id = if kind.is_control() {
                ActionId::try_new(kind.as_str())
                    .map_err(|e| PolicyError::Internal(e.to_string()))?
            } else {
                best.map(|action| action.id().clone())
                    .ok_or_else(|| PolicyError::Internal(format!("no target for {kind}")))?
            };
            op_scores.insert(
                kind,
                score_operation_ctx(
                    goal_text,
                    kind.operation_kind(),
                    best.map(|a| evidence_of(a)).as_ref(),
                    &ctx,
                ),
            );
            op_ids.insert(kind, id);
        }

        // Clause done-detection: the last dispatched action verified a
        // real effect. If its target is gone (a `Delete "X"` button
        // vanishes with the item) or the action's label covers the
        // clause's object — the step *was* the intent — the clause is
        // satisfied. Chosen before arbitration so a tie at the cap or
        // a sub-threshold top still closes the clause instead of
        // looping or abstaining.
        if let Some(last) = history.last() {
            if satisfied_entry(last) {
                let gone = space.get(&last.action_id).is_none();
                if gone || clause_effect_done(goal_text, &last.label, &ctx, space) {
                    if let Some(done) = space.get_str(ActionKind::Done.as_str()) {
                        return Ok(PolicyOutcome::Choice(PolicyDecision {
                            action_id: done.id().clone(),
                            kind: ActionKind::Done,
                            target_label: done.label().to_owned(),
                            confidence_millis: Confidence::MAX.get(),
                            operation_ranked: Vec::new(),
                            target_ranked: Vec::new(),
                        }));
                    }
                }
            }
        }

        let (op_answer, op_ranked) =
            choose_kinds(&kinds, &op_scores, &op_ids, history, self.profile)?;
        let Some(chosen_kind) = op_answer else {
            return Ok(PolicyOutcome::Abstain {
                reason: "operation abstain".to_owned(),
                operation_ranked: op_ranked,
                target_ranked: Vec::new(),
            });
        };

        if chosen_kind.is_control() {
            let id = ActionId::try_new(chosen_kind.as_str())
                .map_err(|e| PolicyError::Internal(e.to_string()))?;
            let action = space.get(&id).ok_or_else(|| {
                PolicyError::Internal(format!("control `{chosen_kind}` missing from space"))
            })?;
            let confidence_millis = op_ranked
                .iter()
                .find(|ranked| ranked.kind == chosen_kind)
                .map(|ranked| ranked.confidence_millis)
                .ok_or_else(|| PolicyError::Internal("chosen control missing from ranks".into()))?;
            // A control (wait) that just ran with a verified effect
            // satisfied a single-intent goal: DONE, not a loop.
            if !chosen_kind_is_terminal(chosen_kind) && satisfied_by_history(history, action.id()) {
                if let Some(done) = space.get_str(ActionKind::Done.as_str()) {
                    return Ok(PolicyOutcome::Choice(PolicyDecision {
                        action_id: done.id().clone(),
                        kind: ActionKind::Done,
                        target_label: done.label().to_owned(),
                        confidence_millis: Confidence::MAX.get(),
                        operation_ranked: op_ranked,
                        target_ranked: Vec::new(),
                    }));
                }
            }
            return Ok(PolicyOutcome::Choice(PolicyDecision {
                action_id: action.id().clone(),
                kind: chosen_kind,
                target_label: action.label().to_owned(),
                confidence_millis,
                operation_ranked: op_ranked,
                target_ranked: Vec::new(),
            }));
        }

        // --- Target head (same observation; only this kind's targets) -------
        let targets: Vec<&OfferedAction> = space.targets_of(chosen_kind).collect();
        if targets.is_empty() {
            return Ok(PolicyOutcome::Abstain {
                reason: format!("no viable targets for {chosen_kind}"),
                operation_ranked: op_ranked,
                target_ranked: Vec::new(),
            });
        }

        let target_scores: Vec<(&OfferedAction, Confidence)> = targets
            .iter()
            .map(|a| (*a, score_action_ctx(goal_text, &evidence_of(a), &ctx)))
            .collect();

        let (chosen, target_ranked) = choose_targets(&target_scores, history, self.profile)?;

        let Some(action) = chosen else {
            return Ok(PolicyOutcome::Abstain {
                reason: "target abstain".to_owned(),
                operation_ranked: op_ranked,
                target_ranked,
            });
        };

        let confidence_millis = target_ranked
            .iter()
            .find(|ranked| ranked.id == *action.id())
            .map(|ranked| ranked.confidence_millis)
            .ok_or_else(|| PolicyError::Internal("chosen target missing from ranks".into()))?;

        // Destructive offers are listed but never auto-chosen: the
        // runner needs an explicit confirmation path, which the local
        // policy never supplies (PRD M4.1).
        if action.dangerous() {
            return Ok(PolicyOutcome::Abstain {
                reason: "needs_confirmation".to_owned(),
                operation_ranked: op_ranked,
                target_ranked,
            });
        }

        // Repeated-action avoidance: the winner is exactly the action
        // that just executed with a verified effect *and* that effect
        // is the clause's intent (its label covers the object, or a
        // verb-named affordance landed the payload). Repeating it
        // would double-submit; choose DONE instead.
        if satisfied_by_history(history, action.id())
            && clause_effect_done(goal_text, action.label(), &ctx, space)
        {
            if let Some(done) = space.get_str(ActionKind::Done.as_str()) {
                return Ok(PolicyOutcome::Choice(PolicyDecision {
                    action_id: done.id().clone(),
                    kind: ActionKind::Done,
                    target_label: done.label().to_owned(),
                    confidence_millis: Confidence::MAX.get(),
                    operation_ranked: op_ranked,
                    target_ranked,
                }));
            }
        }

        Ok(PolicyOutcome::Choice(PolicyDecision {
            action_id: action.id().clone(),
            kind: action.kind(),
            target_label: action.label().to_owned(),
            confidence_millis,
            operation_ranked: op_ranked,
            target_ranked,
        }))
    }
}

/// Verbs whose clause carries a text payload a field must hold first
/// ("Add a todo "Buy milk"", "type rust into Search").
const PAYLOAD_VERBS: &[&str] = &[
    "add", "create", "new", "insert", "type", "enter", "fill", "input", "submit",
];

/// Clause words that ask for a check/toggle effect on a checkbox.
const TOGGLE_WORDS: &[&str] = &[
    "mark",
    "marked",
    "done",
    "check",
    "checked",
    "complete",
    "completed",
    "toggle",
    "tick",
    "ticked",
];

/// Prepositions whose next word may name a location ("in Settings").
const LOCATIVES: &[&str] = &["in", "on", "inside", "under", "to", "from", "into"];

/// Folded substrings that signal an enable/disable intent.
const TURN_WORDS: &[&str] = &["turnon", "turnedon", "turnoff", "enable", "disable"];

/// Clause-level evidence for `decide`: quoted literals (and whether
/// one still needs a field), the verb-position word, check/toggle
/// intent, locative words, and the sole payload/checkbox targets when
/// the space offers exactly one.
fn goal_evidence(goal: &str, space: &ActionSpace) -> GoalEvidence {
    let literals = quoted_literals(goal);
    let has_literal = !literals.is_empty();
    let unplaced_literal = literals
        .iter()
        .any(|lit| !space.literal_placed(&tokenize(lit)));
    let tokens = tokenize(goal);
    let verb = tokens.first().cloned().unwrap_or_default();

    let type_targets: Vec<&OfferedAction> = space.targets_of(ActionKind::Type).collect();
    let payload_target = if PAYLOAD_VERBS.iter().any(|v| verb == *v)
        && unplaced_literal
        && type_targets.len() == 1
    {
        Some(type_targets[0].id().as_str().to_owned())
    } else {
        None
    };

    let mut checkboxes = space
        .targets_of(ActionKind::Click)
        .filter(|a| a.role() == Some("checkbox"));
    let sole_checkbox = match (checkboxes.next(), checkboxes.next()) {
        (Some(only), None) => Some(only.id().as_str().to_owned()),
        _ => None,
    };

    let folded = fold(goal);
    let toggle_intent = tokens.iter().any(|t| TOGGLE_WORDS.iter().any(|w| t == w))
        || TURN_WORDS.iter().any(|w| folded.contains(w));

    // A locative names a direction word even after its page is shown
    // (it still leaves the clause's object words), but it only earns
    // navigation evidence while the page is not yet visible.
    let locatives: Vec<String> = tokens
        .windows(2)
        .filter(|w| LOCATIVES.iter().any(|p| w[0] == *p))
        .map(|w| w[1].clone())
        .collect();
    let nav_locatives = locatives
        .iter()
        .filter(|w| !space.location_shown(w))
        .cloned()
        .collect();

    GoalEvidence {
        literals,
        unplaced_literal,
        verb,
        toggle_intent,
        locatives,
        nav_locatives,
        payload_target,
        sole_checkbox,
        has_literal,
    }
}

fn chosen_kind_is_terminal(kind: ActionKind) -> bool {
    matches!(kind, ActionKind::Done | ActionKind::Blocked)
}

/// The acted-on label is what the clause asked for — either it covers
/// every object word (a `Settings` click satisfies `open settings`;
/// a `Settings` click does *not* satisfy `turn on X in settings`
/// because `X` is the object, not the locative), or it is the
/// verb-named affordance and the clause's payload has landed inside a
/// node label (the `Add` button after `Buy milk` becomes a list item).
fn clause_effect_done(
    goal: &str,
    last_label: &str,
    ctx: &GoalEvidence,
    space: &ActionSpace,
) -> bool {
    if object_covered(goal, last_label, ctx) {
        return true;
    }
    !ctx.literals.is_empty()
        && verb_matches(&ctx.verb, last_label)
        && ctx
            .literals
            .iter()
            .all(|lit| space.literal_in_label(&tokenize(lit)))
}

/// Verification saw a real effect (`success`/`state-changed`/
/// `navigation`) — the strings the runner records.
fn satisfied_entry(entry: &HistoryEntry) -> bool {
    matches!(
        entry.verification.as_str(),
        "success" | "state-changed" | "navigation"
    )
}

/// The last executed step was `id` and verification saw a real effect.
fn satisfied_by_history(history: &[HistoryEntry], id: &ActionId) -> bool {
    history
        .last()
        .is_some_and(|entry| &entry.action_id == id && satisfied_entry(entry))
}

fn offered_kinds(space: &ActionSpace) -> Vec<ActionKind> {
    let mut kinds = Vec::new();
    for kind in [
        ActionKind::Click,
        ActionKind::Type,
        ActionKind::SetValue,
        ActionKind::Key,
        ActionKind::Keybinding,
        ActionKind::Invoke,
        ActionKind::Done,
        ActionKind::Blocked,
        ActionKind::Wait,
    ] {
        if space.contains_kind(kind) {
            // Target/entity-bound kinds need at least one offered action;
            // controls are always ok.
            if kind.is_control() || space.targets_of(kind).next().is_some() {
                kinds.push(kind);
            }
        }
    }
    kinds
}

fn habituation_weight(id: &ActionId, history: &[HistoryEntry]) -> Confidence {
    let mut weight = 1000_i16;
    for entry in history.iter().rev() {
        if &entry.action_id != id
            || !matches!(entry.verification.as_str(), "no-effect" | "wrong-effect")
        {
            break;
        }
        weight = weight.saturating_sub(HABITUATION_STEP);
        if weight == 0 {
            break;
        }
    }
    Confidence::saturating(i32::from(weight))
}

fn damped_urge(evidence: Confidence, weight: Confidence) -> Confidence {
    Confidence::saturating(i32::from(evidence.get()) * i32::from(weight.get()) / 1000)
}

/// Returns `(Some(kind), ranked)` on choice, `(None, ranked)` on abstain.
fn choose_kinds(
    kinds: &[ActionKind],
    scores: &BTreeMap<ActionKind, Confidence>,
    action_ids: &BTreeMap<ActionKind, ActionId>,
    history: &[HistoryEntry],
    profile: Profile,
) -> Result<(Option<ActionKind>, Vec<RankedAction>), PolicyError> {
    if kinds.is_empty() {
        return Ok((None, Vec::new()));
    }
    if kinds.len() == 1 {
        let kind = kinds[0];
        let evidence = scores.get(&kind).copied().unwrap_or(Confidence::ZERO);
        let action_id = action_ids
            .get(&kind)
            .ok_or_else(|| PolicyError::Internal(format!("missing action id for {kind}")))?;
        let urge = damped_urge(evidence, habituation_weight(action_id, history));
        let ranked = vec![ranked_kind(kind, urge)?];
        if urge < profile.thresholds().min_confidence {
            return Ok((None, ranked));
        }
        return Ok((Some(kind), ranked));
    }

    let ids: Result<Vec<_>, _> = kinds
        .iter()
        .map(|k| CandidateId::new(k.as_str()).map_err(|e| PolicyError::Internal(e.to_string())))
        .collect();
    let set = CandidateSet::new(ids?).map_err(|e| PolicyError::Internal(e.to_string()))?;
    let question = set
        .question("operation")
        .map_err(|e| PolicyError::Internal(e.to_string()))?;
    let drives = Drives::new(&[("goal", Confidence::MAX)])
        .map_err(|e| PolicyError::Internal(e.to_string()))?;
    let mut scores_by_option = Scores::new(&question);
    let mut affinity = Affinity::new(&question, &drives);
    for kind in kinds {
        let idx = set
            .index_of(kind.as_str())
            .ok_or_else(|| PolicyError::Internal(format!("missing operation option for {kind}")))?;
        let evidence = scores.get(kind).copied().unwrap_or(Confidence::ZERO);
        let action_id = action_ids
            .get(kind)
            .ok_or_else(|| PolicyError::Internal(format!("missing action id for {kind}")))?;
        scores_by_option
            .set(idx, evidence)
            .map_err(|e| PolicyError::Internal(e.to_string()))?;
        affinity
            .set(
                DriveIndex::new(0),
                idx,
                habituation_weight(action_id, history),
            )
            .map_err(|e| PolicyError::Internal(e.to_string()))?;
    }
    let arbitration = arbitrate(&scores_by_option, &drives, &affinity, None, profile)
        .map_err(|e| PolicyError::Internal(e.to_string()))?;
    let ranked = ranked_from_answer_kinds(&set, arbitration.answer(), arbitration.urges())?;
    match arbitration.answer() {
        Answer::Choice { option, .. } => {
            let id = set
                .id(*option)
                .ok_or_else(|| PolicyError::Internal("missing chosen kind".into()))?;
            let kind = ActionKind::parse(id.as_str())
                .ok_or_else(|| PolicyError::Internal(format!("bad kind {}", id.as_str())))?;
            Ok((Some(kind), ranked))
        }
        Answer::Abstain { .. } => Ok((None, ranked)),
        other => Err(PolicyError::Internal(format!(
            "unexpected operation answer {other:?}"
        ))),
    }
}

fn choose_targets<'a>(
    scored: &[(&'a OfferedAction, Confidence)],
    history: &[HistoryEntry],
    profile: Profile,
) -> Result<(Option<&'a OfferedAction>, Vec<RankedAction>), PolicyError> {
    if scored.is_empty() {
        return Ok((None, Vec::new()));
    }
    if scored.len() == 1 {
        let (action, conf) = scored[0];
        let urge = damped_urge(conf, habituation_weight(action.id(), history));
        let ranked = vec![ranked_action(action, urge)];
        if urge < profile.thresholds().min_confidence {
            return Ok((None, ranked));
        }
        return Ok((Some(action), ranked));
    }

    let ids: Result<Vec<_>, _> = scored
        .iter()
        .map(|(a, _)| {
            CandidateId::new(a.id().as_str()).map_err(|e| PolicyError::Internal(e.to_string()))
        })
        .collect();
    let set = CandidateSet::new(ids?).map_err(|e| PolicyError::Internal(e.to_string()))?;
    let question = set
        .question("target")
        .map_err(|e| PolicyError::Internal(e.to_string()))?;
    let drives = Drives::new(&[("goal", Confidence::MAX)])
        .map_err(|e| PolicyError::Internal(e.to_string()))?;
    let mut scores_by_option = Scores::new(&question);
    let mut affinity = Affinity::new(&question, &drives);
    for (action, conf) in scored {
        let idx = set
            .index_of(action.id().as_str())
            .ok_or_else(|| PolicyError::Internal("missing target option".into()))?;
        scores_by_option
            .set(idx, *conf)
            .map_err(|e| PolicyError::Internal(e.to_string()))?;
        affinity
            .set(
                DriveIndex::new(0),
                idx,
                habituation_weight(action.id(), history),
            )
            .map_err(|e| PolicyError::Internal(e.to_string()))?;
    }
    let arbitration = arbitrate(&scores_by_option, &drives, &affinity, None, profile)
        .map_err(|e| PolicyError::Internal(e.to_string()))?;
    let ranked =
        ranked_from_answer_targets(&set, arbitration.answer(), arbitration.urges(), scored)?;
    match arbitration.answer() {
        Answer::Choice { option, .. } => {
            let id = set
                .id(*option)
                .ok_or_else(|| PolicyError::Internal("missing chosen target".into()))?;
            let action = scored
                .iter()
                .map(|(a, _)| *a)
                .find(|a| a.id().as_str() == id.as_str())
                .ok_or_else(|| PolicyError::Internal("chosen target not in space".into()))?;
            Ok((Some(action), ranked))
        }
        Answer::Abstain { .. } => Ok((None, ranked)),
        other => Err(PolicyError::Internal(format!(
            "unexpected target answer {other:?}"
        ))),
    }
}

fn ranked_kind(kind: ActionKind, conf: Confidence) -> Result<RankedAction, PolicyError> {
    Ok(RankedAction {
        id: ActionId::try_new(kind.as_str()).map_err(|e| PolicyError::Internal(e.to_string()))?,
        kind,
        label: kind.as_str().to_owned(),
        confidence_millis: conf.get(),
    })
}

fn ranked_action(action: &OfferedAction, conf: Confidence) -> RankedAction {
    RankedAction {
        id: action.id().clone(),
        kind: action.kind(),
        label: action.label().to_owned(),
        confidence_millis: conf.get(),
    }
}

fn ranked_from_answer_kinds(
    set: &CandidateSet,
    answer: &Answer,
    urges: &Scores<'_>,
) -> Result<Vec<RankedAction>, PolicyError> {
    let entries = match answer {
        Answer::Choice { ranked, .. } | Answer::Abstain { ranked, .. } => ranked.entries(),
        _ => return Ok(Vec::new()),
    };
    entries
        .iter()
        .map(|(idx, _)| {
            let id = set
                .id(*idx)
                .ok_or_else(|| PolicyError::Internal("missing ranked operation".into()))?;
            let kind = ActionKind::parse(id.as_str())
                .ok_or_else(|| PolicyError::Internal(format!("bad kind {}", id.as_str())))?;
            let urge = urges
                .get(*idx)
                .ok_or_else(|| PolicyError::Internal("missing operation urge".into()))?;
            ranked_kind(kind, urge)
        })
        .collect()
}

fn ranked_from_answer_targets(
    set: &CandidateSet,
    answer: &Answer,
    urges: &Scores<'_>,
    scored: &[(&OfferedAction, Confidence)],
) -> Result<Vec<RankedAction>, PolicyError> {
    let entries = match answer {
        Answer::Choice { ranked, .. } | Answer::Abstain { ranked, .. } => ranked.entries(),
        _ => return Ok(Vec::new()),
    };
    entries
        .iter()
        .map(|(idx, _)| {
            let id = set
                .id(*idx)
                .ok_or_else(|| PolicyError::Internal("missing ranked target".into()))?;
            let (action, _) = scored
                .iter()
                .find(|(a, _)| a.id().as_str() == id.as_str())
                .ok_or_else(|| PolicyError::Internal("ranked target not in space".into()))?;
            let urge = urges
                .get(*idx)
                .ok_or_else(|| PolicyError::Internal("missing target urge".into()))?;
            Ok(ranked_action(action, urge))
        })
        .collect()
}
