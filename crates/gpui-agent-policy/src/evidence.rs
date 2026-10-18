//! Integer evidence for gpui candidates — a port of
//! `aui-policy/src/evidence.rs` onto `UiNode`-shaped targets. No floats,
//! same thresholds: milli scores 0..=1000, exact-label saturation only,
//! `NON_EXACT_CAP = 849`, `UNNAMED_OPERATION_CAP = 500`.
//!
//! A target's evidence text is its own `name` plus `role` — ancestor
//! names are never concatenated (todo row buttons already carry the
//! title, e.g. "Delete Buy milk").

use instinct_core::Confidence;
use instinct_lexicon::overlap;
use instinct_text::{NormalizeConfig, normalize};

/// Ceiling for a label that is not exactly the goal (or its target
/// phrase): Instinct Standard's margin below `MAX`, still above its
/// min confidence.
pub const NON_EXACT_CAP: i16 = 849;

/// Score ceiling for a target-bound operation the goal did not name when
/// it named another one. Below Instinct Standard's min confidence.
pub const UNNAMED_OPERATION_CAP: i16 = 500;

/// What a candidate can do — the offered operations of an action space.
/// Target-bound kinds (`Click`/`TypeText`/`Select`) name a `UiNode`;
/// control kinds name the page.
///
/// `Select`, `ScrollDown`, and `ScrollUp` are goal-side verb vocabulary
/// only: they appear in the keyword tables (`control_keyword_hit`,
/// `named_target_operations`) so "select …"/"scroll …" goals score and
/// cap correctly, but no `ActionKind` maps to them, so they are never
/// offerable or dispatchable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OperationKind {
    Click,
    TypeText,
    /// Goal-side only — see enum docs.
    Select,
    Done,
    Blocked,
    Wait,
    /// Goal-side only — see enum docs.
    ScrollDown,
    /// Goal-side only — see enum docs.
    ScrollUp,
    /// `set_value` — programmatic field set, distinct from typed input.
    SetValue,
    /// `key` — send a named key (Enter, Escape, …) to a node.
    Key,
    /// `keybinding` — fire a registered Action-id binding.
    Keybinding,
    /// `invoke` — run a registered host invoke schema.
    Invoke,
}

impl OperationKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Click => "click",
            Self::TypeText => "type_text",
            Self::Select => "select",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Wait => "wait",
            Self::ScrollDown => "scroll_down",
            Self::ScrollUp => "scroll_up",
            Self::SetValue => "set_value",
            Self::Key => "key",
            Self::Keybinding => "keybinding",
            Self::Invoke => "invoke",
        }
    }

    /// Controls act on the page, not a node — they are named by the goal
    /// itself ("scroll down", "wait", "done").
    pub const fn is_control(self) -> bool {
        matches!(
            self,
            Self::Done | Self::Blocked | Self::Wait | Self::ScrollDown | Self::ScrollUp
        )
    }
}

/// A scored candidate: stable `id`, evidence `label` (own name + role —
/// no ancestors), `role` for role-keyword evidence, and its operation
/// `kind`.
#[derive(Debug, Clone, Copy)]
pub struct EvidenceTarget<'a> {
    pub id: &'a str,
    pub label: &'a str,
    pub role: Option<&'a str>,
    pub kind: OperationKind,
}

/// Score how well `target` matches `goal`. Returns Instinct confidence
/// millis 0..=1000.
///
/// Label evidence is the better of the whole goal and its *target phrase*
/// (goal minus quoted payload, leading operation verb, and the
/// preposition after them): in `Type "rust" into Search` the verb is
/// operation evidence and `"rust"` is the text resolver's payload; only
/// `Search` names a target.
pub fn score_action(goal: &str, target: &EvidenceTarget<'_>) -> Confidence {
    score_action_ctx(goal, target, &GoalEvidence::default())
}

/// Clause-level evidence the policy computes once per `decide` from
/// the goal text and the observed space. Every channel is additive —
/// it may raise or withhold evidence but never dispatches — and all
/// defaults make [`score_action`]/`[`score_operation`] behave exactly
/// as the context-free form.
#[derive(Clone, Debug, Default)]
pub struct GoalEvidence {
    /// Quoted literals in the clause (unquoted text).
    pub literals: Vec<String>,
    /// A quoted literal appears in no visible node label or value —
    /// the clause still carries an unplaced payload, so verb-position
    /// evidence is withheld until the payload lands.
    pub unplaced_literal: bool,
    /// First content token of the clause (verb position), lowercase.
    pub verb: String,
    /// The clause asks for a check/toggle effect ("mark done",
    /// "turn on", "enable", "check").
    pub toggle_intent: bool,
    /// Words following a locative preposition ("in Settings") —
    /// excluded from the clause's object words regardless of whether
    /// that location is already shown.
    pub locatives: Vec<String>,
    /// Locatives whose page is not yet visible — navigation evidence.
    /// A locative naming the shown page is spent ("in Settings" stops
    /// boosting `nav-settings` once the Settings page is on screen).
    pub nav_locatives: Vec<String>,
    /// Payload intent + exactly one TYPE target — its action id.
    pub payload_target: Option<String>,
    /// The single checkbox target when exactly one is offered.
    pub sole_checkbox: Option<String>,
    /// The clause carries any quoted literal — suppresses `done`
    /// keyword evidence ("mark X as done" is not "I am done").
    pub has_literal: bool,
}

/// `score_action` plus clause-level channels:
///
/// - Each quoted literal is scored as an additional phrase — a literal
///   that *names a node* ("Mark "Write docs" as done") is target
///   evidence, capped at [`NON_EXACT_CAP`] because it is a clause
///   fragment, not the whole intent.
/// - `verb`: the clause's first token appearing in the label is the
///   goal's action word naming this affordance ("Add a todo …" names
///   the `Add` button) — withheld while a payload is unplaced so the
///   field goes before the button.
/// - `toggle_intent`/`sole_checkbox`: check/toggle phrasing raises
///   checkboxes — stronger when the space offers exactly one.
/// - `locatives`: a word after `in`/`on`/`to` that equals a label is
///   navigation evidence ("Turn on … in Settings" opens Settings).
/// - `payload_target`: an unplaced literal plus payload phrasing plus
///   a sole TYPE target makes the field the obvious first step.
pub fn score_action_ctx(goal: &str, target: &EvidenceTarget<'_>, ctx: &GoalEvidence) -> Confidence {
    let mut best = score_action_best_ctx(goal, target, ctx);
    for literal in &ctx.literals {
        let lit = score_action_text_ctx(literal, target, ctx)
            .get()
            .min(NON_EXACT_CAP);
        if lit > best.get() {
            best = Confidence::saturating(i32::from(lit));
        }
    }
    // Only an exact label can saturate: "Skip navigation" contains the
    // goal "Skip" but must stay a margin below the button labelled
    // "Skip". A label token-equal to the whole goal is exact too
    // (`Delete Write docs` names `Delete "Write docs"` whole). A
    // checkbox that covers every object word under toggle intent is
    // exact in effect: `Toggle Item 137` *is* `mark "Item 137" done`,
    // while `Toggle Item 42` stays capped a margin below.
    let exact = exact_label_ctx(goal, target, ctx)
        || tokens_equal(goal, target.label)
        || (ctx.toggle_intent
            && target.role == Some("checkbox")
            && object_covered(goal, target.label, ctx));
    if best == Confidence::MAX && !exact {
        best = Confidence::saturating(i32::from(NON_EXACT_CAP));
    }
    // Verb-position and locative evidence lifts weak matches but may
    // never push a non-exact label past the exact ceiling — otherwise
    // near-identical labels ("Delete task 149" vs "Delete task 150")
    // saturate together and the margin collapses.
    let mut soft = |bonus: i32| {
        let lifted = best.get().saturating_add(bonus as i16);
        best = if exact {
            Confidence::saturating(i32::from(lifted))
        } else {
            Confidence::saturating(i32::from(lifted.min(NON_EXACT_CAP)))
        };
    };
    if !ctx.unplaced_literal && !ctx.verb.is_empty() && verb_matches(&ctx.verb, target.label) {
        // The verb names the affordance; when the label also covers
        // every object word ("collapse the sidebar" → "Hide sidebar")
        // the match is near-exact and earns more.
        soft(if object_covered(goal, target.label, ctx) {
            650
        } else {
            500
        });
    }
    if ctx.nav_locatives.iter().any(|w| eq_fold(w, target.label)) {
        soft(600);
    }
    // Check/toggle intent and the unplaced-payload field are decisive
    // channels: they name the *kind* of affordance the clause wants, so
    // they may reach the ceiling.
    if ctx.toggle_intent && target.role == Some("checkbox") {
        let bonus = if ctx.sole_checkbox.as_deref() == Some(target.id) {
            700
        } else {
            300
        } + if object_covered(goal, target.label, ctx) {
            150
        } else {
            0
        };
        // Uncapped only for the exact target — otherwise the bonus
        // saturates every near-match checkbox and collapses the margin
        // (`Toggle Item 137` must stay above `Toggle Item 42`).
        if exact {
            best = best.saturating_add(Confidence::saturating(bonus));
        } else {
            soft(bonus);
        }
    }
    if target.kind == OperationKind::TypeText && ctx.payload_target.as_deref() == Some(target.id) {
        best = best.saturating_add(Confidence::saturating(850));
    }
    best
}

/// `a` and `b` name the same word when equal or one is a prefix of at
/// least four characters of the other ("confirm" ~ "confirmation").
/// Clause-level channels only — base label evidence stays exact.
fn stem_eq(a: &str, b: &str) -> bool {
    a == b || (a.len().min(b.len()) >= 4 && (a.starts_with(b) || b.starts_with(a)))
}

/// UI affordance names the clause verb may pick: `collapse the
/// sidebar` names the `Hide sidebar` button even though the words
/// differ.
const VERB_SYNONYMS: &[(&str, &[&str])] = &[
    ("collapse", &["hide", "collapse", "minimize"]),
    ("expand", &["show", "expand", "maximize"]),
    ("open", &["open", "show"]),
    ("close", &["close", "hide", "dismiss"]),
    ("quit", &["quit", "exit", "close"]),
    ("delete", &["delete", "remove", "trash"]),
    ("add", &["add", "create", "new"]),
    ("mark", &["mark", "check", "tick"]),
    ("toggle", &["toggle", "flip", "switch"]),
];

/// Effect-state words are the clause's *wanted outcome*, not the
/// object it acts on — "mark X done" acts on `X`, `done` is what the
/// click should make true.
const STATE_WORDS: &[&str] = &[
    "done",
    "on",
    "off",
    "checked",
    "unchecked",
    "complete",
    "completed",
    "enabled",
    "disabled",
    "open",
    "closed",
    "marked",
    "set",
    "toggled",
    "shown",
    "hidden",
];

/// The clause's object words: significant goal tokens plus quoted
/// literals (a quoted `"A"` is the object even though `a` is a
/// stopword), minus the verb position, locative directions, and
/// effect-state words.
fn object_tokens(goal: &str, ctx: &GoalEvidence) -> Vec<String> {
    let mut object: Vec<String> = Vec::new();
    for lit in &ctx.literals {
        for t in tokenize(lit) {
            if !object.contains(&t) {
                object.push(t);
            }
        }
    }
    for t in significant_tokens(goal) {
        if !object.contains(&t) {
            object.push(t);
        }
    }
    object
        .into_iter()
        .filter(|t| {
            !stem_eq(t, &ctx.verb)
                && !ctx.locatives.iter().any(|l| l == t)
                && !STATE_WORDS.iter().any(|s| s == t)
        })
        .collect()
}

/// Every object word of the clause appears in `label` (`stem_eq`),
/// and at least one real object word exists — the label names what
/// the clause acts on, not merely its verb or location.
pub(crate) fn object_covered(goal: &str, label: &str, ctx: &GoalEvidence) -> bool {
    let label_tokens = tokenize(label);
    let object = object_tokens(goal, ctx);
    !object.is_empty()
        && object
            .iter()
            .all(|t| label_tokens.iter().any(|lt| stem_eq(lt, t)))
}

/// The clause's verb-position word names this label directly or via
/// [`VERB_SYNONYMS`].
pub(crate) fn verb_matches(verb: &str, label: &str) -> bool {
    let label_tokens = tokenize(label);
    if label_tokens.iter().any(|t| stem_eq(t, verb)) {
        return true;
    }
    VERB_SYNONYMS.iter().any(|(key, synonyms)| {
        stem_eq(key, verb) && synonyms.iter().any(|s| label_tokens.iter().any(|t| t == s))
    })
}

/// Words that carry no target identity — dropped before the
/// token-equivalence check so `Delete "Write docs"` equals
/// `Delete Write docs` but `Skip` does not equal `Skip navigation`
/// (extra content) and `Write docs` does not equal
/// `Mark "Write docs" as done` (missing intent words).
const COVER_STOP: &[&str] = &[
    "the", "a", "an", "as", "in", "on", "to", "into", "from", "at", "by", "of", "for", "and",
    "then", "it", "them", "that", "this", "with",
];

fn significant_tokens(text: &str) -> Vec<String> {
    tokenize(text)
        .into_iter()
        .filter(|t| !COVER_STOP.iter().any(|s| t == s))
        .collect()
}

/// The goal and label name the same thing: their significant token
/// sets cover each other (`stem_eq` pairwise — "confirm" ~
/// "confirmation"). Such a label earns the exact-label ceiling.
fn tokens_equal(goal: &str, label: &str) -> bool {
    let g = significant_tokens(goal);
    let l = significant_tokens(label);
    !g.is_empty()
        && !l.is_empty()
        && g.iter().all(|t| l.iter().any(|lt| stem_eq(lt, t)))
        && l.iter().all(|t| g.iter().any(|lt| stem_eq(lt, t)))
}

#[cfg(test)]
fn exact_label(goal: &str, target: &EvidenceTarget<'_>) -> bool {
    exact_label_ctx(goal, target, &GoalEvidence::default())
}

fn exact_label_ctx(goal: &str, target: &EvidenceTarget<'_>, ctx: &GoalEvidence) -> bool {
    let label = target.label;
    // Operation keywords count as evidence only outside quotes —
    // `"call"` inside a payload does not name `invoke`.
    let residue;
    let kw_goal: &str = if ctx.has_literal {
        residue = unquoted_text(goal);
        &residue
    } else {
        goal
    };
    eq_fold(goal, label)
        || target_phrase(goal).is_some_and(|p| eq_fold(&p, label))
        || (target.kind.is_control()
            && !done_suppressed(target.kind, ctx)
            && (eq_fold(goal, target.kind.as_str()) || control_keyword_hit(kw_goal, target.kind)))
}

/// `done` keyword evidence is suppressed while the clause carries a
/// quoted literal: "Mark "Write docs" as done" describes the todo's
/// state, not the agent's completion.
fn done_suppressed(kind: OperationKind, ctx: &GoalEvidence) -> bool {
    kind == OperationKind::Done && ctx.has_literal
}

#[cfg(test)]
fn score_action_best(goal: &str, target: &EvidenceTarget<'_>) -> Confidence {
    score_action_best_ctx(goal, target, &GoalEvidence::default())
}

fn score_action_best_ctx(
    goal: &str,
    target: &EvidenceTarget<'_>,
    ctx: &GoalEvidence,
) -> Confidence {
    let whole = score_action_text_ctx(goal, target, ctx);
    match target_phrase(goal) {
        Some(phrase) if phrase != goal => {
            let p = score_action_text_ctx(&phrase, target, ctx);
            if p.get() > whole.get() { p } else { whole }
        }
        _ => whole,
    }
}

/// The goal with `"…"`/`“…”` quoted spans removed — the residue carries
/// the clause's non-literal evidence (verb, object words, locatives).
fn unquoted_text(goal: &str) -> String {
    let mut out = String::with_capacity(goal.len());
    let mut chars = goal.chars();
    while let Some(c) = chars.next() {
        if c == '"' || c == '\u{201c}' {
            let close = if c == '"' { '"' } else { '\u{201d}' };
            for d in chars.by_ref() {
                if d == close {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Operation verbs stripped from the front of a goal for target evidence.
const LEADING_VERBS: &[&str] = &[
    "click", "press", "tap", "type", "enter", "fill", "input", "select", "choose", "pick", "open",
];

/// Connectives dropped right after a stripped verb or quoted payload.
const CONNECTIVES: &[&str] = &["into", "in", "on", "to", "the", "a", "an", "from", "as"];

/// The goal with quoted payloads, a leading operation verb, and the
/// connectives that followed them removed. `None` when nothing is left.
pub fn target_phrase(goal: &str) -> Option<String> {
    // Drop quoted segments ("…" or '…'), marking where they were.
    let mut unquoted = String::with_capacity(goal.len());
    let mut chars = goal.chars();
    while let Some(c) = chars.next() {
        if c == '"' || c == '\u{201c}' {
            let close = if c == '"' { '"' } else { '\u{201d}' };
            for d in chars.by_ref() {
                if d == close {
                    break;
                }
            }
            unquoted.push_str(" \u{0} ");
        } else {
            unquoted.push(c);
        }
    }
    let mut words: Vec<&str> = unquoted.split_whitespace().collect();
    let mut stripped = false;
    if let Some(first) = words.first() {
        if LEADING_VERBS.iter().any(|v| first.eq_ignore_ascii_case(v)) {
            words.remove(0);
            stripped = true;
        }
    }
    let first_word = words.iter().find(|w| **w != "\u{0}").copied();
    let mut out = Vec::new();
    let mut after_marker = stripped;
    let mut had_marker = false;
    for w in words {
        if w == "\u{0}" {
            after_marker = true;
            had_marker = true;
            continue;
        }
        if after_marker && CONNECTIVES.iter().any(|c| w.eq_ignore_ascii_case(c)) {
            continue;
        }
        after_marker = false;
        out.push(w);
    }
    // A quoted span is the clause's payload or named target; a leading
    // word that is not a LEADING_VERB is the clause's own operation
    // verb ("Add "Pay rent"") — it names nothing and must not become
    // the target phrase.
    if had_marker
        && !stripped
        && out
            .first()
            .zip(first_word)
            .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
    {
        out.remove(0);
    }
    let phrase = out.join(" ");
    if phrase.is_empty() {
        None
    } else {
        Some(phrase)
    }
}

#[cfg(test)]
fn score_action_text(goal: &str, target: &EvidenceTarget<'_>) -> Confidence {
    score_action_text_ctx(goal, target, &GoalEvidence::default())
}

fn score_action_text_ctx(
    goal: &str,
    target: &EvidenceTarget<'_>,
    ctx: &GoalEvidence,
) -> Confidence {
    let label = target.label;
    let kind = target.kind;

    if eq_fold(goal, label) {
        return Confidence::MAX;
    }
    if kind.is_control() && eq_fold(goal, kind.as_str()) {
        return Confidence::MAX;
    }

    // Quoted literals are scored by their own channel; their words are
    // not label evidence. A long payload title must not crush the
    // residue's coverage — `add` is 1/3 of `add a todo`, not 1/8 of
    // the goal with the payload folded in.
    let residue;
    let goal: &str = if ctx.has_literal {
        residue = unquoted_text(goal);
        &residue
    } else {
        goal
    };
    // While a payload or named target is still unplaced, the clause's
    // verb names the *future* submit affordance, not current evidence:
    // `add` must not lift the `Add` button before `Pay rent` is typed,
    // and `delete` must not lift `Delete Buy milk` when `Walk dog`
    // does not exist.
    let stripped;
    let goal: &str = if ctx.unplaced_literal {
        let mut toks: Vec<&str> = goal.split_whitespace().collect();
        if toks
            .first()
            .is_some_and(|t| stem_eq(&t.to_lowercase(), &ctx.verb))
        {
            toks.remove(0);
        }
        stripped = toks.join(" ");
        &stripped
    } else {
        goal
    };

    // Control verbs ("done", "wait", "scroll down") pick the control
    // itself. Operation verbs on target-bound kinds ("type", "select")
    // are operation evidence (see `score_operation`), not a cap on label
    // evidence.
    if kind.is_control() && !done_suppressed(kind, ctx) && control_keyword_hit(goal, kind) {
        return Confidence::saturating(920);
    }

    let mut score = Confidence::ZERO;

    if let Some(ov) = lexical_overlap(goal, label) {
        // Overlap measures how much of the label the goal holds. A short
        // label that names a sliver of the goal is scaled by goal
        // coverage.
        let ov = if !sliver_scaled(kind) || covers_most(goal, label) {
            ov
        } else {
            scale_by_coverage(ov, goal, label)
        };
        score = score.saturating_add(ov);
    }

    let g = fold(goal);
    let l = fold(label);
    // A label inside the goal is evidence only when it covers most of
    // the goal: a one-word chip ("kabisado") inside a long title goal
    // is not.
    if !l.is_empty() && g.contains(&l) && (!sliver_scaled(kind) || covers_most(goal, label)) {
        score = score.saturating_add(Confidence::saturating(250));
    } else if !g.is_empty() && l.contains(&g) {
        score = score.saturating_add(Confidence::saturating(200));
    }

    let goal_tokens = tokenize(goal);
    let label_tokens = tokenize(label);
    if !goal_tokens.is_empty() {
        let hits = goal_tokens
            .iter()
            .filter(|t| label_tokens.iter().any(|lt| lt == *t))
            .count();
        if hits == goal_tokens.len() && hits > 0 {
            score = score.saturating_add(Confidence::saturating(300));
        } else if hits > 0 {
            let part = i32::try_from(hits.saturating_mul(150) / goal_tokens.len()).unwrap_or(0);
            score = score.saturating_add(Confidence::saturating(part));
        }
    }

    if let Some(role) = target.role {
        if role_mentioned(goal, role) {
            score = score.saturating_add(Confidence::saturating(80));
        }
    }

    if kind == OperationKind::Done && !done_suppressed(kind, ctx) && done_language(goal) {
        score = score.saturating_add(Confidence::saturating(100));
    }

    score
}

/// Click targets are named by the whole target phrase, so a label that
/// names a sliver of it is scaled down. TYPE/SELECT goals may carry an
/// unquoted payload ("type rust ownership in the Search box") whose
/// words do not name the field, so their labels are not scaled.
fn sliver_scaled(kind: OperationKind) -> bool {
    kind == OperationKind::Click
}

/// `conf` times the fraction of `goal` tokens that occur in `label`.
fn scale_by_coverage(conf: Confidence, goal: &str, label: &str) -> Confidence {
    let goal_tokens = tokenize(goal);
    if goal_tokens.is_empty() {
        return Confidence::ZERO;
    }
    let label_tokens = tokenize(label);
    let hits = goal_tokens
        .iter()
        .filter(|t| label_tokens.contains(t))
        .count();
    let scaled = i64::from(conf.get()) * i64::try_from(hits).unwrap_or(0)
        / i64::try_from(goal_tokens.len()).unwrap_or(1);
    Confidence::saturating(i32::try_from(scaled).unwrap_or(0))
}

/// At least two thirds of `goal`'s tokens occur in `label`.
fn covers_most(goal: &str, label: &str) -> bool {
    let goal_tokens = tokenize(goal);
    if goal_tokens.is_empty() {
        return false;
    }
    let label_tokens = tokenize(label);
    let hits = goal_tokens
        .iter()
        .filter(|t| label_tokens.contains(t))
        .count();
    hits * 3 >= goal_tokens.len() * 2
}

/// The executed `label` names what the clause asked for: it covers at
/// least two thirds of the clause's target phrase (the clause itself
/// when it has none). The agent uses this before treating a step's
/// effect as the clause done.
pub fn label_covers_target(clause: &str, label: &str) -> bool {
    let phrase = target_phrase(clause).unwrap_or_else(|| clause.to_owned());
    covers_most(&phrase, label)
}

/// `label` names the clause's target and little else: it covers two
/// thirds of the target phrase, and the phrase covers two thirds of
/// the label. "Skip" names "click Skip"; "Skip navigation" does not.
pub fn label_names_target(clause: &str, label: &str) -> bool {
    let phrase = target_phrase(clause).unwrap_or_else(|| clause.to_owned());
    covers_most(&phrase, label) && covers_most(label, &phrase)
}

/// Score an operation kind given the goal and the best target (if any)
/// for that kind.
pub fn score_operation(
    goal: &str,
    kind: OperationKind,
    best_target: Option<&EvidenceTarget<'_>>,
) -> Confidence {
    score_operation_ctx(goal, kind, best_target, &GoalEvidence::default())
}

/// [`score_operation`] with clause-level evidence — the target score
/// includes the [`GoalEvidence`] channels and `done` keywords are
/// suppressed while the clause carries a quoted literal.
pub fn score_operation_ctx(
    goal: &str,
    kind: OperationKind,
    best_target: Option<&EvidenceTarget<'_>>,
    ctx: &GoalEvidence,
) -> Confidence {
    // Operation keywords count as evidence only outside quotes — the
    // `call` in `Add a todo titled "Call Ana"` is payload, not `invoke`.
    let residue;
    let kw_goal: &str = if ctx.has_literal {
        residue = unquoted_text(goal);
        &residue
    } else {
        goal
    };
    let mut score = Confidence::ZERO;
    if control_keyword_hit(kw_goal, kind) && !done_suppressed(kind, ctx) {
        score = score.saturating_add(Confidence::saturating(920));
    }
    if eq_fold(goal, kind.as_str()) {
        return Confidence::MAX;
    }
    if let Some(target) = best_target {
        let t = score_action_ctx(goal, target, ctx);
        if t.get() > score.get() {
            score = t;
        }
        // The goal names a different target-bound operation ("type …
        // into Search" offers CLICK and TYPE_TEXT on the same field):
        // an unnamed operation cannot outrank the named one on label
        // evidence alone.
        let named = named_target_operations(kw_goal);
        if !named.is_empty() && !named.contains(&kind) {
            score = Confidence::saturating(i32::from(score.get().min(UNNAMED_OPERATION_CAP)));
        }
    } else if kind.is_control() {
        // Weak prior so controls remain choosable when goal is vague.
        score = score.saturating_add(Confidence::saturating(50));
    }
    score
}

/// All `"…"`/`"…"` quoted literals in `goal`, in order, unquoted.
/// Empty and unterminated segments are skipped.
pub fn quoted_literals(goal: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = 0;
    while cursor < goal.len() {
        let Some((offset, opening)) = goal[cursor..]
            .char_indices()
            .find(|(_, c)| matches!(*c, '"' | '\u{201c}'))
        else {
            break;
        };
        let start = cursor + offset;
        let (closing, content_start) = match opening {
            '"' => ('"', start + 1),
            _ => ('\u{201d}', start + opening.len_utf8()),
        };
        let Some((end_off, _)) = goal[content_start..]
            .char_indices()
            .find(|(_, c)| *c == closing)
        else {
            break;
        };
        let content_end = content_start + end_off;
        if content_end > content_start {
            out.push(goal[content_start..content_end].to_owned());
        }
        cursor = content_end + closing.len_utf8();
    }
    out
}

/// `instinct_lexicon::overlap` on normalized `goal`/`candidate`:
/// fraction of `candidate`'s significant words found in `goal`, millis.
/// `None` when either side fails to normalize — treated as no evidence.
pub fn lexical_overlap(goal: &str, candidate: &str) -> Option<Confidence> {
    let cfg = NormalizeConfig::default();
    let q = normalize(goal, cfg).ok()?;
    overlap_normalized(&q, candidate)
}

/// [`lexical_overlap`] with a pre-normalized query — one `normalize`
/// call serves many candidates (hot loops like schema scoring).
pub fn overlap_normalized(
    query: &instinct_text::Normalized<'_>,
    candidate: &str,
) -> Option<Confidence> {
    let c = normalize(candidate, query.config()).ok()?;
    overlap(query, &c).ok()
}

/// Token-boundary containment: `name` appears in `intent` as a whole
/// identifier (`_`/`-`/alnum adjacent chars make it part of a longer
/// token, so `wait` does not match inside `wait_until`).
pub fn name_in_intent(intent: &str, name: &str) -> bool {
    let hay = intent.to_ascii_lowercase();
    name_in_intent_lowered(&hay, name)
}

/// [`name_in_intent`] with a pre-lowercased haystack — one fold serves
/// many lookups (hot loops like schema scoring).
pub fn name_in_intent_lowered(intent_lower: &str, name: &str) -> bool {
    let hay = intent_lower;
    // Schema/identifier names are conventionally lowercase; fold only
    // when the caller's name actually has uppercase bytes.
    let lowered;
    let name: &str = if name.bytes().any(|b| b.is_ascii_uppercase()) {
        lowered = name.to_ascii_lowercase();
        &lowered
    } else {
        name
    };
    let bytes = hay.as_bytes();
    let boundary = |b: u8| !(b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    let mut start = 0usize;
    while let Some(rel) = hay[start..].find(name) {
        let pos = start + rel;
        let end = pos + name.len();
        let before = pos == 0 || boundary(bytes[pos - 1]);
        let after = end >= bytes.len() || boundary(bytes[end]);
        if before && after {
            return true;
        }
        start = pos + 1;
    }
    false
}

/// Lowercase alphanumeric tokens — public for sibling scorers that need
/// the same tokenization as the evidence functions.
pub fn tokens(text: &str) -> Vec<String> {
    tokenize(text)
}

fn eq_fold(a: &str, b: &str) -> bool {
    fold(a) == fold(b)
}

pub(crate) fn fold(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// Lowercase alphanumeric tokens — port of `aui_core::tokenize`.
pub(crate) fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut buf = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                buf.push(lower);
            }
        } else if !buf.is_empty() {
            tokens.push(std::mem::take(&mut buf));
        }
    }
    if !buf.is_empty() {
        tokens.push(buf);
    }
    tokens
}

fn instruction_tokens(goal: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for clause in goal.split(['.', ';', '!', '?', ',', '\n']) {
        let clause_tokens = tokenize(clause);
        if matches!(
            clause_tokens.first().map(String::as_str),
            Some("if" | "unless" | "otherwise")
        ) {
            continue;
        }
        tokens.extend(clause_tokens);
    }
    tokens
}

fn matches_instruction_tokens(tokens: &[String], keys: &[&[&str]]) -> bool {
    keys.iter().any(|key| {
        tokens.windows(key.len()).any(|window| {
            window
                .iter()
                .zip(key.iter())
                .all(|(token, expected)| token.as_str() == *expected)
        })
    })
}

fn control_keyword_hit(goal: &str, kind: OperationKind) -> bool {
    let keys: &[&[&str]] = match kind {
        OperationKind::Done => &[&["done"], &["finished"], &["complete"], &["completed"]],
        OperationKind::Blocked => &[&["blocked"], &["stuck"], &["impossible"]],
        OperationKind::Wait => &[&["wait"], &["pause"], &["loading"]],
        OperationKind::ScrollDown => &[
            &["scroll", "down"],
            &["scroll", "page"],
            &["page", "down"],
            &["scrolldown"],
            &["pagedown"],
        ],
        OperationKind::ScrollUp => &[
            &["scroll", "up"],
            &["page", "up"],
            &["scrollup"],
            &["pageup"],
        ],
        OperationKind::Click => &[&["click"], &["press"], &["tap"]],
        OperationKind::TypeText => &[&["type"], &["enter"], &["fill"], &["input"]],
        OperationKind::Select => &[&["select"], &["choose"], &["pick"]],
        OperationKind::SetValue => &[&["set"], &["change"], &["update"]],
        OperationKind::Key => &[
            &["press", "enter"],
            &["hit", "enter"],
            &["press", "return"],
            &["send", "enter"],
            &["press", "tab"],
            &["press", "escape"],
            &["press", "backspace"],
            &["press", "space"],
            &["keypress"],
            &["submit"],
        ],
        OperationKind::Keybinding => &[&["shortcut"], &["hotkey"], &["keybinding"]],
        OperationKind::Invoke => &[&["invoke"], &["run"], &["call"], &["execute"], &["trigger"]],
    };
    matches_instruction_tokens(&instruction_tokens(goal), keys)
}

/// Target-bound operations whose verb appears in the goal.
fn named_target_operations(goal: &str) -> Vec<OperationKind> {
    [
        OperationKind::Click,
        OperationKind::TypeText,
        OperationKind::Select,
        OperationKind::SetValue,
        OperationKind::Key,
        OperationKind::Keybinding,
        OperationKind::Invoke,
    ]
    .into_iter()
    .filter(|kind| control_keyword_hit(goal, *kind))
    .collect()
}

fn role_mentioned(goal: &str, role: &str) -> bool {
    let g = fold(goal);
    let key = match role {
        "button" => "button",
        "link" => "link",
        "textbox" | "field" | "combobox" => "text",
        "checkbox" => "checkbox",
        "menuitem" | "option" => "menu",
        "tab" => "tab",
        "dialog" => "dialog",
        "navigation" | "nav" => "nav",
        "listbox" | "list" => "list",
        _ => return false,
    };
    g.contains(key)
}

fn done_language(goal: &str) -> bool {
    matches_instruction_tokens(
        &instruction_tokens(goal),
        &[
            &["done"],
            &["finish"],
            &["finished"],
            &["complete"],
            &["completed"],
            &["success"],
            &["successful"],
            &["submitted"],
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click<'a>(id: &'a str, label: &'a str) -> EvidenceTarget<'a> {
        EvidenceTarget {
            id,
            label,
            role: Some("button"),
            kind: OperationKind::Click,
        }
    }

    #[test]
    fn target_phrase_strips_verb_payload_and_connectives() {
        assert_eq!(target_phrase("Click Go").as_deref(), Some("Go"));
        assert_eq!(
            target_phrase(r#"Type "rust ownership" into Search"#).as_deref(),
            Some("Search")
        );
        assert_eq!(
            target_phrase(r#"Select "Business" in Cabin class"#).as_deref(),
            Some("Cabin class")
        );
        assert_eq!(target_phrase("Sign in").as_deref(), Some("Sign in"));
        assert_eq!(target_phrase("click").as_deref(), None);
    }

    #[test]
    fn exact_label_saturates_only_exact() {
        let exact = score_action("Skip", &click("b1", "Skip"));
        assert_eq!(exact.get(), 1000);
        let near = score_action("Skip", &click("b2", "Skip navigation"));
        assert!(near.get() <= NON_EXACT_CAP, "{}", near.get());
    }

    #[test]
    fn unnamed_operation_capped() {
        // Goal names "type", so click on the same target is capped.
        let target = EvidenceTarget {
            id: "search",
            label: "Search",
            role: Some("textbox"),
            kind: OperationKind::Click,
        };
        let s = score_operation(
            "type \"rust\" into Search",
            OperationKind::Click,
            Some(&target),
        );
        assert!(s.get() <= UNNAMED_OPERATION_CAP, "{}", s.get());
    }

    #[test]
    fn control_verbs_score_the_control() {
        let wait = EvidenceTarget {
            id: "wait",
            label: "Wait",
            role: None,
            kind: OperationKind::Wait,
        };
        let s = score_action("wait for the page", &wait);
        assert!(s.get() >= 920, "{}", s.get());
    }

    #[test]
    fn scroll_keywords_match_adjacent_tokens() {
        assert!(control_keyword_hit(
            "scroll down",
            OperationKind::ScrollDown
        ));
        assert!(control_keyword_hit(
            "Scroll  Down please",
            OperationKind::ScrollDown
        ));
        assert!(control_keyword_hit("page up", OperationKind::ScrollUp));
        assert!(control_keyword_hit("scrolldown", OperationKind::ScrollDown));
        assert!(control_keyword_hit("pagedown", OperationKind::ScrollDown));
        assert!(control_keyword_hit("scrollup", OperationKind::ScrollUp));
    }

    #[test]
    fn conditional_clauses_do_not_trigger_control_language() {
        let goal = "Star the email \"Weekly sync\". If the task cannot be done, stop and say so.";
        assert!(!control_keyword_hit(goal, OperationKind::Done));
        assert!(!done_language(goal));
        assert!(control_keyword_hit(
            "If it is loading, click Retry",
            OperationKind::Click
        ));
        assert!(!control_keyword_hit(
            "If it is loading, click Retry",
            OperationKind::Wait
        ));
    }

    #[test]
    fn label_cover_checks() {
        assert!(label_covers_target("click Skip", "Skip"));
        assert!(label_names_target("click Skip", "Skip"));
        assert!(!label_names_target("click Skip", "Skip navigation"));
    }

    #[test]
    fn operation_kind_names_and_control_classification() {
        let cases = [
            (OperationKind::Click, "click", false),
            (OperationKind::TypeText, "type_text", false),
            (OperationKind::Select, "select", false),
            (OperationKind::Done, "done", true),
            (OperationKind::Blocked, "blocked", true),
            (OperationKind::Wait, "wait", true),
            (OperationKind::ScrollDown, "scroll_down", true),
            (OperationKind::ScrollUp, "scroll_up", true),
        ];
        for (kind, name, control) in cases {
            assert_eq!(kind.as_str(), name, "{kind:?}");
            assert_eq!(kind.is_control(), control, "{kind:?}");
        }
    }

    #[test]
    fn fold_strips_case_and_space() {
        assert!(eq_fold("Sign In", "sign in"));
        assert!(!eq_fold("Sign In", "Sign Out"));
    }

    #[test]
    fn name_in_intent_respects_token_boundaries() {
        assert!(name_in_intent("click the wait button", "wait"));
        assert!(name_in_intent("todo.add please", "todo.add"));
        assert!(name_in_intent("snapshot", "snapshot"));
        assert!(name_in_intent("press TODO.ADD now", "todo.add"));
        // `_`, `-` and alnum neighbours make the name part of a longer
        // token — `wait` must not match inside `wait_until`.
        assert!(!name_in_intent("wait_until the page loads", "wait"));
        assert!(!name_in_intent("address it now", "add"));
        assert!(!name_in_intent("use my-tool today", "tool"));
        assert!(!name_in_intent("_x", "x"));
        assert!(!name_in_intent("snapshots", "snapshot"));
        // A valid later occurrence still matches when an earlier one is
        // boundary-blocked (the scan advances past blocked hits).
        assert!(name_in_intent("snapshotx then snapshot", "snapshot"));
    }

    #[test]
    fn name_in_intent_lowered_matches_lowered_haystack() {
        let hay = "click todo.add then wait_until it settles";
        assert!(name_in_intent_lowered(hay, "todo.add"));
        assert!(!name_in_intent_lowered(hay, "wait"));
        // Mixed-case names fold on demand.
        assert!(name_in_intent_lowered(hay, "TODO.ADD"));
    }

    #[test]
    fn exact_label_has_three_independent_paths() {
        // Whole-goal equality.
        assert!(exact_label("Skip", &click("b1", "Skip")));
        // The stripped target phrase ("click X" → "X").
        assert!(exact_label("Click Skip", &click("b1", "Skip")));
        assert!(!exact_label("Click the Skip button", &click("b1", "Skip")));
        // A control kind named by the goal itself.
        let wait = EvidenceTarget {
            id: "w",
            label: "Zebra",
            role: None,
            kind: OperationKind::Wait,
        };
        assert!(exact_label("wait", &wait));
        let scroll = EvidenceTarget {
            id: "s",
            label: "Zebra",
            role: None,
            kind: OperationKind::ScrollDown,
        };
        assert!(exact_label("scroll down", &scroll));
        // Target-bound kinds are never exact on operation verbs alone.
        assert!(!exact_label("click", &click("b1", "Zebra")));
        assert!(!exact_label("wait", &click("b1", "Zebra")));
    }

    #[test]
    fn score_action_best_prefers_stripped_phrase_when_better() {
        let target = click("search", "Search");
        // The phrase "Search" is exact for this label — better than
        // scoring the whole "type … into Search" goal.
        let best = score_action_best(r#"Type "rust" into Search"#, &target);
        assert_eq!(best.get(), 1000);
        // When the phrase is no better, the whole-goal score stands.
        let vague = click("go", "Go button");
        let whole = score_action_text("press the big Go button now", &vague);
        assert_eq!(
            score_action_best("press the big Go button now", &vague).get(),
            whole.get().max(
                score_action_text(
                    &target_phrase("press the big Go button now").unwrap_or_default(),
                    &vague
                )
                .get()
            )
        );
    }

    #[test]
    fn score_action_text_accumulates_label_channels() {
        let goal = "type milk into the search field";
        let matching = EvidenceTarget {
            id: "a",
            label: "Search",
            role: Some("textbox"),
            kind: OperationKind::TypeText,
        };
        let foreign = EvidenceTarget {
            id: "b",
            label: "Zebra",
            role: Some("textbox"),
            kind: OperationKind::TypeText,
        };
        let m = score_action_text(goal, &matching);
        let f = score_action_text(goal, &foreign);
        // Label in the goal: overlap + containment + token hits all
        // contribute; a foreign label gets none of them.
        assert!(m.get() >= 550, "matching {m:?}");
        assert!(f.get() <= 100, "foreign {f:?}");
    }

    #[test]
    fn score_action_text_empty_goal_scores_nothing() {
        let target = click("b", "Big Button");
        assert!(score_action_text("", &target).get() <= 100);
    }

    #[test]
    fn control_kind_only_saturates_on_its_own_name() {
        let wait = EvidenceTarget {
            id: "w",
            label: "Zebra",
            role: None,
            kind: OperationKind::Wait,
        };
        // "scroll down" is not Wait — the control must not saturate.
        assert!(score_action_text("scroll down", &wait).get() < 500);
        // A click label named "click" does not saturate via the
        // control-name path either.
        let button = click("b", "Big Red Button");
        assert!(score_action_text("click", &button).get() < 920);
    }

    #[test]
    fn score_action_text_click_labels_are_sliver_scaled() {
        // "Save Now" is a sliver of a long type/select-style goal: a
        // Click label is scaled by coverage, a TypeText label is not.
        let goal = "type save now into the long form field please";
        let click_t = EvidenceTarget {
            id: "c",
            label: "Save Now",
            role: None,
            kind: OperationKind::Click,
        };
        let type_t = EvidenceTarget {
            id: "t",
            label: "Save Now",
            role: None,
            kind: OperationKind::TypeText,
        };
        let c = score_action_text(goal, &click_t);
        let t = score_action_text(goal, &type_t);
        assert!(t.get() > c.get(), "type {t:?} vs click {c:?}");
        // Exact value pin: the scaled overlap is conf*hits/goal_tokens —
        // arithmetic mutations in scale_by_coverage must move it.
        let expected_scale = {
            let ov = lexical_overlap(goal, "Save Now").map(|c| i32::from(c.get()));
            let goal_tokens = tokenize(goal).len() as i64;
            let hits = tokenize(goal)
                .iter()
                .filter(|g| tokenize("Save Now").contains(g))
                .count() as i64;
            ov.map(|o| o as i64 * hits / goal_tokens)
        };
        if let Some(scaled) = expected_scale {
            let hits_part = 150 * 2 / tokenize(goal).len() as i64;
            assert_eq!(i64::from(c.get()), scaled + hits_part);
        }
    }

    #[test]
    fn covers_most_requires_two_thirds() {
        // Boundary: hits*3 >= len*2 — 2 of 3 hits is exactly 2/3.
        assert!(covers_most("a b c", "a b"));
        assert!(!covers_most("a b c d", "a b"));
        assert!(!covers_most("", "a"));
        assert!(covers_most("a b c", "a b c"));
        // label_covers_target consults the stripped phrase.
        assert!(label_covers_target("click Save Now", "Save Now"));
        assert!(!label_covers_target("click Save", "Zebra"));
        // label_names_target needs coverage in both directions.
        assert!(label_names_target("click Save Now", "Save Now"));
        assert!(!label_names_target("click Save", "Save Now Plus"));
    }

    #[test]
    fn role_mentioned_maps_roles_to_goal_words() {
        assert!(role_mentioned("press the button", "button"));
        assert!(role_mentioned("open the link", "link"));
        assert!(role_mentioned("type in the text field", "textbox"));
        assert!(role_mentioned("fill the text area", "field"));
        assert!(role_mentioned("the checkbox", "checkbox"));
        assert!(role_mentioned("from the menu", "menuitem"));
        assert!(role_mentioned("from the menu", "option"));
        assert!(role_mentioned("switch tab", "tab"));
        assert!(role_mentioned("the dialog opens", "dialog"));
        assert!(role_mentioned("the nav bar", "navigation"));
        assert!(role_mentioned("the nav bar", "nav"));
        assert!(role_mentioned("a list of rows", "listbox"));
        assert!(role_mentioned("a list of rows", "list"));
        assert!(!role_mentioned("press the button", "link"));
        assert!(!role_mentioned("nothing here", "button"));
        assert!(!role_mentioned("anything", "unmapped-role"));
    }

    #[test]
    fn role_bonus_only_when_role_mentioned() {
        // The textbox role's goal-key is "text" — a folded substring.
        let goal = "type milk into the text field";
        let with = EvidenceTarget {
            id: "a",
            label: "Search",
            role: Some("textbox"),
            kind: OperationKind::TypeText,
        };
        let none = EvidenceTarget { role: None, ..with };
        let wrong = EvidenceTarget {
            role: Some("button"),
            ..with
        };
        assert!(score_action_text(goal, &with).get() > score_action_text(goal, &none).get());
        assert_eq!(
            score_action_text(goal, &wrong).get(),
            score_action_text(goal, &none).get()
        );
    }

    #[test]
    fn done_language_boosts_only_done() {
        let goal = "say done when finished";
        let done = EvidenceTarget {
            id: "d",
            label: "Zebra",
            role: None,
            kind: OperationKind::Done,
        };
        let blocked = EvidenceTarget {
            id: "b",
            label: "Zebra",
            role: None,
            kind: OperationKind::Blocked,
        };
        let d = score_action_text(goal, &done);
        let b = score_action_text(goal, &blocked);
        assert!(d.get() >= 100, "{d:?}");
        // The Done target beats an identical Blocked target only through
        // the done-language channel.
        assert!(d.get() > b.get(), "done {d:?} vs blocked {b:?}");
        assert!(done_language("say done when finished"));
        assert!(!done_language("press the button"));
    }

    #[test]
    fn score_operation_caps_unnamed_target_operations() {
        let search = EvidenceTarget {
            id: "s",
            label: "Search",
            role: Some("textbox"),
            kind: OperationKind::Click,
        };
        // The goal names "type" — CLICK on the same field is capped.
        let capped = score_operation(
            r#"type "rust" into Search"#,
            OperationKind::Click,
            Some(&search),
        );
        assert!(capped.get() <= UNNAMED_OPERATION_CAP, "{capped:?}");
        // No named operation → no cap applies at all.
        let uncapped = score_operation(
            "press the Search thing",
            OperationKind::Click,
            Some(&search),
        );
        assert!(uncapped.get() > 0, "{uncapped:?}");
        // A control with no target keeps only its weak prior.
        let prior = score_operation("hmm", OperationKind::Wait, None);
        assert!(prior.get() <= 50, "{prior:?}");
        // Named operation kind beats the target evidence path.
        let named = score_operation("wait", OperationKind::Wait, None);
        assert!(named.get() >= 920, "{named:?}");
    }

    #[test]
    fn lexical_overlap_counts_significant_words() {
        let ov = lexical_overlap("type milk into the search field", "search");
        assert!(ov.is_some_and(|c| c.get() > 0), "{ov:?}");
        assert_eq!(
            lexical_overlap("type milk", "zebra").map(|c| c.get()),
            Some(0)
        );
        let q = normalize(
            "type milk into the search field",
            NormalizeConfig::default(),
        )
        .expect("normalize");
        assert!(overlap_normalized(&q, "search").is_some_and(|c| c.get() > 0));
    }

    #[test]
    fn tokens_lowercases_and_splits_on_non_alnum() {
        assert_eq!(
            tokens("Type 'Milk' into Search!"),
            ["type", "milk", "into", "search"]
        );
        assert!(tokens("   ").is_empty());
    }
}
