//! `run --goal` local runner (PRD M6.2): observe (`snapshot since`) →
//! [`ActionSpace`] → policy → fingerprint gate → dispatch → diff-verify.
//!
//! Clause semantics: `split_sequential_clauses` gives `then`/`and then`
//! clauses; each is one policy decision per step. The clause advances
//! when the policy picks `DONE` (the `satisfied_by_history` swap in
//! [`crate::policy`] makes a verified repeat pick DONE) or when a
//! `wait for X` / `wait until X` clause sees X on screen. Every
//! mutating dispatch carries `expect_fingerprint`; a `stale_target` /
//! `target_gone` refusal re-observes and re-decides without consuming
//! the id — up to `max_consecutive_stale` times.

use crate::action_space::{ActionId, ActionKind, ActionSpace, OfferedAction, OfferedInvoke};
use crate::escalate::{CommandEscalation, ESCALATION_PROTOCOL};
use crate::goal::{UiGoal, split_sequential_clauses};
use crate::policy::{HistoryEntry, PolicyDecision, PolicyOutcome, RankedAction, UiPolicy};
use crate::text::{
    DeterministicTextResolver, TextContext, TextError, TextResolution, TextResolver,
};
use gpui_agent::client::AgentClient;
use gpui_agent::keybinding::KeybindingInfo;
use gpui_agent::protocol::{DeliveryMode, Op};
use gpui_agent::tree::{UiNode, UiTree};

/// Step/counter bounds for [`Runner`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunnerCaps {
    /// Hard step bound across the whole run.
    pub max_steps: u32,
    /// `no-effect`/`wrong-effect` verifications before `Blocked`.
    pub max_consecutive_no_effect: u32,
    /// `stale_target`/`target_gone` refusals before `Abstained`.
    pub max_consecutive_stale: u32,
    /// Total wall budget spent inside `WAIT` / `wait for` clauses.
    pub max_wait_ms: u64,
}

impl Default for RunnerCaps {
    fn default() -> Self {
        Self {
            max_steps: 20,
            max_consecutive_no_effect: 3,
            max_consecutive_stale: 2,
            max_wait_ms: 30_000,
        }
    }
}

/// Pause between re-observes inside a `wait for` clause or `WAIT`.
pub const WAIT_POLL_MS: u64 = 250;

/// `kind:"text"` escalation never touches a sensitive field (M7.2):
/// a `password` role or a password-named label keeps the deterministic
/// `Missing` verdict.
fn sensitive_field(role: &str, label: &str) -> bool {
    role.eq_ignore_ascii_case("password") || label.to_lowercase().contains("password")
}

/// How one clause runs (`Act` is the default; port of aui
/// `clause_mode`'s act/wait subset — no `while`/`if present` modes:
/// those exist for browser ad markers gpui has no concept of).
#[derive(Clone, Debug, PartialEq, Eq)]
enum ClauseMode {
    Act,
    /// `wait for X` / `wait until X`: no action; the clause ends when
    /// a node named X is visible, or the wait budget runs out.
    WaitFor(String),
}

fn clause_mode(clause: &str) -> (String, ClauseMode) {
    let trimmed = clause.trim();
    let lower = trimmed.to_lowercase();
    for prefix in ["wait for ", "wait until "] {
        if lower.starts_with(prefix) {
            let mut target = trimmed[prefix.len()..].trim().to_owned();
            loop {
                let stripped = target
                    .strip_suffix(" appears")
                    .or_else(|| target.strip_suffix(" shows"))
                    .or_else(|| target.strip_suffix(" in the list"))
                    .map(str::trim);
                match stripped {
                    Some(next) if next != target => target = next.to_owned(),
                    _ => break,
                }
            }
            let target = target.trim_matches('"').trim();
            if !target.is_empty() {
                return (trimmed.to_owned(), ClauseMode::WaitFor(target.to_owned()));
            }
        }
    }
    (trimmed.to_owned(), ClauseMode::Act)
}

/// `X and wait for Y` / `X wait until Y` → [`X`, `wait … Y`]: a `then`
/// clause may still carry a trailing wait instruction, which becomes
/// its own `WaitFor` clause. Quotes are respected; a `wait` at the
/// clause start stays whole.
fn split_wait(clause: &str) -> Vec<String> {
    let lower = clause.to_lowercase();
    let mut quote = false;
    for (i, c) in clause.char_indices() {
        if c == '"' {
            quote = !quote;
            continue;
        }
        if quote {
            continue;
        }
        for pat in [
            " and wait for ",
            " and wait until ",
            " wait for ",
            " wait until ",
        ] {
            if lower[i..].starts_with(pat) {
                let head = clause[..i].trim_end_matches([' ', ',']);
                let tail = clause[i + if pat.starts_with(" and") { 5 } else { 1 }..].trim_start();
                let mut out = Vec::new();
                if !head.is_empty() {
                    out.push(head.to_owned());
                }
                if !tail.is_empty() {
                    out.push(tail.to_owned());
                }
                if out.len() > 1 {
                    return out;
                }
                return vec![clause.to_owned()];
            }
        }
    }
    vec![clause.to_owned()]
}

/// A clause that names no literal but says "it"/"them" refers to the
/// literal the agent just placed — substitute it quoted so ordinary
/// literal evidence applies ("mark it done" → `mark "X" done`).
fn substitute_pronouns(clause: &str, typed: &[String]) -> String {
    if clause.contains('"') || typed.is_empty() {
        return clause.to_owned();
    }
    let Some(last) = typed.last() else {
        return clause.to_owned();
    };
    let mut out = Vec::new();
    let mut replaced = false;
    for word in clause.split(' ') {
        if !replaced && matches!(word, "it" | "It" | "them" | "Them") {
            out.push(format!("\"{last}\""));
            replaced = true;
        } else {
            out.push(word.to_owned());
        }
    }
    out.join(" ")
}

/// A named candidate reported inside an abstain outcome or receipt.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Candidate {
    pub id: String,
    pub op: String,
    /// Target role at offer time (`""` for entity-bound/controls).
    pub role: String,
    pub name: String,
    pub confidence_millis: i16,
}

/// The best-`k` rows of `ranked` for abstain evidence — shared
/// construction for `run --goal` receipts and MCP `act` replies, which
/// serialize the same row under different key names.
pub fn top_candidates(space: &ActionSpace, ranked: &[RankedAction], k: usize) -> Vec<Candidate> {
    ranked
        .iter()
        .take(k)
        .map(|r| {
            let offered = space.get(&r.id);
            Candidate {
                id: r.id.as_str().to_owned(),
                op: r.kind.as_str().to_owned(),
                role: offered
                    .and_then(|a| a.role())
                    .unwrap_or_default()
                    .to_owned(),
                name: offered
                    .map(|a| a.label().to_owned())
                    .unwrap_or_else(|| r.label.clone()),
                confidence_millis: r.confidence_millis,
            }
        })
        .collect()
}

/// One executed (or refused) step — receipt rows are deterministic:
/// no clocks, no host timings.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct StepRecord {
    pub step: u32,
    pub clause: String,
    pub op: String,
    pub id: String,
    pub name: String,
    pub confidence_millis: i16,
    /// `success` | `state-changed` | `no-effect` | `wrong-effect` |
    /// `stale` | `done` | `wait` — the strings the policy's
    /// `satisfied_by_history`/habituation read back.
    pub verification: String,
}

/// Terminal run state (PRD M6.2 outcomes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    Done,
    Blocked {
        reason: String,
    },
    Abstained {
        reason: String,
        candidates: Vec<Candidate>,
    },
    CapHit,
}

impl RunOutcome {
    /// `run --goal` exit code: 0 done, 2 abstained/blocked, 1 error.
    /// (`CapHit` exits 2: the runner stopped without acting wrongly.)
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Done => 0,
            Self::Blocked { .. } | Self::Abstained { .. } | Self::CapHit => 2,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Blocked { .. } => "blocked",
            Self::Abstained { .. } => "abstained",
            Self::CapHit => "cap_hit",
        }
    }
}

/// What `run --goal` prints — byte-deterministic given the same host
/// behavior (no clocks in the payload).
#[derive(Clone, Debug, serde::Serialize)]
pub struct RunReceipt {
    pub goal: String,
    pub clauses: Vec<String>,
    /// The `outcome` label's enum — kept alongside the serialized
    /// string so callers reuse `RunOutcome::exit_code` instead of
    /// re-matching labels.
    #[serde(skip_serializing)]
    pub outcome_kind: RunOutcome,
    pub outcome: String,
    pub reason: Option<String>,
    pub candidates: Vec<Candidate>,
    pub steps: Vec<StepRecord>,
    pub final_rev: Option<u64>,
    /// Protocol ops the runner sent (snapshot/keybindings/dispatch/
    /// since) — the M6.4 call-budget tripwire.
    pub calls: u32,
    pub engine: EngineInfo,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct EngineInfo {
    pub policy: &'static str,
    pub instinct: &'static str,
}

/// `instinct-*` version requirement from `gpui-agent-policy`'s manifest
/// — pinned by `engine_instinct_matches_manifest` so it cannot drift.
const INSTINCT_VERSION_REQ: &str = "0.1";

/// Resolve invoke-schema args for an `INVOKE:<name>` dispatch — owned
/// by the caller (recipe crate) so this crate stays recipe-free.
pub type InvokeArgs = dyn FnMut(&str, &str) -> Result<serde_json::Value, String>;

pub struct Runner<'a> {
    client: &'a mut AgentClient,
    policy: Box<dyn UiPolicy>,
    text_escalation: Option<CommandEscalation>,
    delivery: DeliveryMode,
    invokes: Vec<OfferedInvoke>,
    invoke_args: &'a mut InvokeArgs,
    caps: RunnerCaps,
    wait_poll_ms: u64,
    /// Counted protocol ops — the M6.4 call-budget tripwire asserts on
    /// this so accidental re-snapshot loops fail loudly.
    pub calls: u32,
}

impl<'a> Runner<'a> {
    pub fn new(
        client: &'a mut AgentClient,
        policy: impl UiPolicy + 'static,
        delivery: DeliveryMode,
        invokes: Vec<OfferedInvoke>,
        invoke_args: &'a mut InvokeArgs,
    ) -> Self {
        Self {
            client,
            policy: Box::new(policy),
            text_escalation: None,
            delivery,
            invokes,
            invoke_args,
            caps: RunnerCaps::default(),
            wait_poll_ms: WAIT_POLL_MS,
            calls: 0,
        }
    }

    /// Optional `kind:"text"` command fallback used only when the
    /// deterministic resolver answers [`TextError::Missing`] (M7.2);
    /// password-role fields are never escalated.
    pub fn with_text_escalation(mut self, escalation: Option<CommandEscalation>) -> Self {
        self.text_escalation = escalation;
        self
    }

    /// `kind:"text"` round-trip after a deterministic `Missing`. Never
    /// escalates sensitive fields — a `Missing` there stays a local
    /// abstain (M7.2).
    fn escalate_text(
        &mut self,
        ctx: &TextContext,
        field_id: &ActionId,
    ) -> Result<TextResolution, TextError> {
        let Some(endpoint) = self.text_escalation.as_mut() else {
            return Err(TextError::Missing);
        };
        if sensitive_field(&ctx.field_role, &ctx.field_label) {
            return Err(TextError::Missing);
        }
        endpoint.resolve_text(
            &ctx.goal,
            field_id.as_str(),
            &ctx.field_label,
            ctx.fingerprint(),
        )
    }

    pub fn with_caps(mut self, caps: RunnerCaps) -> Self {
        self.caps = caps;
        self
    }

    /// Shorter polls for tests / bounded `wait for` loops.
    pub fn with_wait_poll_ms(mut self, ms: u64) -> Self {
        self.wait_poll_ms = ms.max(1);
        self
    }

    fn rpc(&mut self, op: Op) -> Result<gpui_agent::protocol::Response, String> {
        self.calls += 1;
        self.client.expect_ok(op)
    }

    /// Full-snapshot observe (run start, or after a `full` answer).
    fn observe_full(&mut self) -> Result<UiTree, String> {
        let resp = self.rpc(Op::snapshot())?;
        resp.tree
            .ok_or_else(|| "host returned no ui tree".to_string())
    }

    /// `snapshot since <rev>` — `unchanged` keeps `tree`, `diff`
    /// applies it, `full` replaces it. Returns the new rev.
    fn observe_since(
        &mut self,
        tree: &mut UiTree,
        rev: Option<u64>,
    ) -> Result<(bool, Option<u64>), String> {
        let Some(since) = rev else {
            *tree = self.observe_full()?;
            return Ok((true, tree.rev));
        };
        let resp = self.rpc(Op::Snapshot {
            since: Some(since),
            format: None,
            fingerprints: None,
        })?;
        let result = resp.result.unwrap_or_default();
        if result.get("unchanged").is_some() {
            return Ok((false, resp.rev));
        }
        if let Some(diff) = result.get("diff") {
            let diff: gpui_agent::TreeDiff =
                serde_json::from_value(diff.clone()).map_err(|e| e.to_string())?;
            let next = gpui_agent::apply_diff(tree, &diff);
            *tree = next;
            return Ok((true, resp.rev));
        }
        if let Some(next) = resp.tree {
            *tree = next;
            return Ok((true, resp.rev));
        }
        Ok((true, resp.rev))
    }

    /// Diff between two known trees — used after a `full` answer and
    /// for the post-dispatch verification.
    fn diff_is_empty(old: &UiTree, new: &UiTree) -> bool {
        let d = gpui_agent::diff_trees(old, new);
        d.added.is_empty() && d.removed.is_empty() && d.changed.is_empty()
    }

    /// Fetches via `self.rpc` so the op lands on the M6.4 call budget.
    fn keybinding_catalog(&mut self) -> Result<Vec<KeybindingInfo>, String> {
        let resp = self.rpc(Op::Keybindings)?;
        KeybindingInfo::catalog_from(resp.result.as_ref())
    }

    /// A visible node whose name covers `target` (word-subset match,
    /// same as `wait for` evidence — any role).
    fn on_screen(tree: &UiTree, target: &str) -> bool {
        fn covers(hay: &str, needle: &str) -> bool {
            let h: Vec<&str> = hay.split_whitespace().collect();
            needle
                .split_whitespace()
                .all(|w| h.iter().any(|t| t.eq_ignore_ascii_case(w)))
        }
        let mut found = false;
        tree.visit(&mut |node: &UiNode| {
            if node.visible && covers(node.name.as_str(), target) {
                found = true;
            }
        });
        found
    }

    /// Run `goal` to a terminal outcome. Deterministic given the same
    /// host behavior; `Err` is a transport/protocol failure (exit 1).
    pub fn run(&mut self, goal: &str) -> Result<RunReceipt, String> {
        let clauses: Vec<String> = split_sequential_clauses(goal)
            .iter()
            .flat_map(|clause| split_wait(clause))
            .collect();
        let mut history: Vec<HistoryEntry> = Vec::new();
        let mut typed: Vec<String> = Vec::new();
        let mut steps: Vec<StepRecord> = Vec::new();
        let mut no_effect = 0u32;
        let mut stale = 0u32;
        let mut waited_ms = 0u64;
        let mut clause_idx = 0usize;

        let mut tree = self.observe_full()?;
        let mut rev = tree.rev;
        let keybindings = self.keybinding_catalog()?;

        while steps.len() < self.caps.max_steps as usize {
            let clause = match clauses.get(clause_idx) {
                Some(c) => substitute_pronouns(c, &typed),
                // Every clause completed — reached when a WaitFor
                // tail (not a DONE pick) ends the goal.
                None => {
                    return Ok(self.receipt(goal, clauses, RunOutcome::Done, steps, rev));
                }
            };
            let (goal_text, mode) = clause_mode(&clause);
            let space = ActionSpace::from_tree(&tree, &keybindings, &self.invokes);

            // `wait for X` — no policy, no action: advance when X shows.
            if let ClauseMode::WaitFor(target) = &mode {
                if Self::on_screen(&tree, target) {
                    clause_idx += 1;
                    continue;
                }
                if waited_ms >= self.caps.max_wait_ms {
                    return Ok(self.receipt(
                        goal,
                        clauses,
                        RunOutcome::Blocked {
                            reason: format!("wait_for `{target}` exceeded wait budget"),
                        },
                        steps,
                        rev,
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(self.wait_poll_ms));
                waited_ms += self.wait_poll_ms;
                let (changed, next) = self.observe_since(&mut tree, rev)?;
                rev = next.or(rev);
                let _ = changed;
                continue;
            }

            let outcome = self
                .policy
                .decide(&space, &UiGoal::new(&goal_text), &history)
                .map_err(|e| e.to_string())?;
            let decision = match outcome {
                PolicyOutcome::Choice(d) => d,
                PolicyOutcome::Abstain {
                    reason,
                    target_ranked,
                    operation_ranked,
                } => {
                    let ranked = if target_ranked.is_empty() {
                        &operation_ranked
                    } else {
                        &target_ranked
                    };
                    // An ESCALATION_PROTOCOL reason is a hard error from
                    // the endpoint (timeout / off-menu / bad wire), not a
                    // policy abstain — the run is Blocked (M7.2).
                    let outcome = if reason.starts_with(ESCALATION_PROTOCOL) {
                        RunOutcome::Blocked { reason }
                    } else {
                        RunOutcome::Abstained {
                            reason,
                            candidates: top_candidates(&space, ranked, 8),
                        }
                    };
                    return Ok(self.receipt(goal, clauses, outcome, steps, rev));
                }
            };

            match decision.kind {
                ActionKind::Done => {
                    clause_idx += 1;
                    if clause_idx >= clauses.len() {
                        return Ok(self.receipt(goal, clauses, RunOutcome::Done, steps, rev));
                    }
                    continue;
                }
                ActionKind::Blocked => {
                    return Ok(self.receipt(
                        goal,
                        clauses,
                        RunOutcome::Blocked {
                            reason: "policy chose blocked".to_owned(),
                        },
                        steps,
                        rev,
                    ));
                }
                ActionKind::Wait => {
                    if waited_ms >= self.caps.max_wait_ms {
                        return Ok(self.receipt(goal, clauses, RunOutcome::CapHit, steps, rev));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(self.wait_poll_ms));
                    waited_ms += self.wait_poll_ms;
                    let (_, next) = self.observe_since(&mut tree, rev)?;
                    rev = next.or(rev);
                    continue;
                }
                _ => {}
            }

            // Mutating / entity ops. TYPE/SET_VALUE payloads come from
            // the TextResolver — never from Instinct.
            let action = space
                .get(&decision.action_id)
                .ok_or_else(|| "chosen action left the space".to_string())?
                .clone();
            let mut payload: Option<String> = None;
            if matches!(decision.kind, ActionKind::Type | ActionKind::SetValue) {
                let ctx = TextContext {
                    goal: UiGoal::new(&goal_text),
                    field_label: action.label().to_owned(),
                    field_role: action.role().unwrap_or("").to_owned(),
                    typed: typed.clone(),
                    context_fingerprint: rev.unwrap_or(0),
                };
                let resolved = match DeterministicTextResolver.resolve(&ctx) {
                    Ok(res) => Ok(res),
                    // M7.2: escalate only Missing — Ambiguous / Invalid /
                    // Abstain stay local verdicts.
                    Err(TextError::Missing) => self.escalate_text(&ctx, &decision.action_id),
                    Err(err) => Err(err),
                };
                match resolved {
                    Ok(res) => payload = Some(res.text),
                    Err(err) => {
                        let reason = match err {
                            TextError::Missing => "text_missing".to_owned(),
                            TextError::Ambiguous => "text_ambiguous".to_owned(),
                            TextError::Invalid(m) => format!("text_invalid:{m}"),
                            TextError::Abstain(m) => format!("text_abstain:{m}"),
                        };
                        // Escalation-protocol failures are hard errors →
                        // Blocked, matching the choose path.
                        let outcome = if reason.contains(ESCALATION_PROTOCOL) {
                            RunOutcome::Blocked { reason }
                        } else {
                            RunOutcome::Abstained {
                                reason,
                                candidates: top_candidates(&space, &decision.target_ranked, 8),
                            }
                        };
                        return Ok(self.receipt(goal, clauses, outcome, steps, rev));
                    }
                }
            }

            let op = op_for_decision(
                &decision,
                &action,
                payload.as_deref(),
                self.delivery,
                &clause,
                &keybindings,
                &mut *self.invoke_args,
            )?;
            let resp = self.rpc(op);
            let resp = match resp {
                Ok(r) => r,
                Err(err) if gpui_agent::dispatch::is_stale_dispatch_error(&err) => {
                    stale += 1;
                    // `max_consecutive_stale` bounds the *re-decides*
                    // (PRD M5.3 "at most twice"): abort on the stale
                    // arriving after the last allowed re-decide.
                    if stale > self.caps.max_consecutive_stale {
                        return Ok(self.receipt(
                            goal,
                            clauses,
                            RunOutcome::Abstained {
                                reason: format!("stale: {err}"),
                                candidates: top_candidates(&space, &decision.target_ranked, 8),
                            },
                            steps,
                            rev,
                        ));
                    }
                    // Nothing dispatched; re-observe and re-decide.
                    let (_, next) = self.observe_since(&mut tree, rev)?;
                    rev = next.or(rev);
                    continue;
                }
                Err(err) => return Err(err),
            };
            stale = 0;
            let _ = resp;

            // Post-dispatch verify: one `snapshot since` doubles as next
            // step's observation.
            let old_tree = tree.clone();
            let (_, next) = self.observe_since(&mut tree, rev)?;
            rev = next.or(rev);
            let changed = !Self::diff_is_empty(&old_tree, &tree);
            let verification = self.verify(&decision, &action, payload.as_deref(), &tree, changed);
            let effect = matches!(verification, "success" | "state-changed");
            if effect {
                no_effect = 0;
            } else {
                no_effect += 1;
            }
            let n = steps.len() as u32 + 1;
            history.push(HistoryEntry {
                step: n,
                action_id: decision.action_id.clone(),
                kind: decision.kind,
                label: decision.target_label.clone(),
                verification: verification.to_owned(),
            });
            if let Some(text) = payload.clone() {
                typed.push(text);
            }
            steps.push(StepRecord {
                step: n,
                clause: clause.clone(),
                op: decision.kind.as_str().to_owned(),
                id: decision.action_id.as_str().to_owned(),
                name: decision.target_label.clone(),
                confidence_millis: decision.confidence_millis,
                verification: verification.to_owned(),
            });
            if no_effect >= self.caps.max_consecutive_no_effect {
                return Ok(self.receipt(
                    goal,
                    clauses,
                    RunOutcome::Blocked {
                        reason: format!(
                            "{no_effect} consecutive no-effect/wrong-effect actions (last: {})",
                            decision.target_label
                        ),
                    },
                    steps,
                    rev,
                ));
            }
        }
        Ok(self.receipt(goal, clauses, RunOutcome::CapHit, steps, rev))
    }

    /// `TYPE`/`SET_VALUE` verify on the value read back; every other op
    /// verifies on a non-empty diff (PRD M6.2 "consistent with the op").
    fn verify(
        &self,
        decision: &PolicyDecision,
        action: &crate::action_space::OfferedAction,
        payload: Option<&str>,
        tree: &UiTree,
        changed: bool,
    ) -> &'static str {
        if matches!(decision.kind, ActionKind::Type | ActionKind::SetValue) {
            let current = action
                .target()
                .and_then(|id| tree.find(id))
                .and_then(|n| n.value.as_deref());
            return match (payload, current) {
                (Some(want), Some(got)) if got == want => "success",
                (Some(_), _) => "wrong-effect",
                _ if changed => "state-changed",
                _ => "no-effect",
            };
        }
        if changed {
            "state-changed"
        } else {
            "no-effect"
        }
    }

    fn receipt(
        &self,
        goal: &str,
        clauses: Vec<String>,
        outcome: RunOutcome,
        steps: Vec<StepRecord>,
        rev: Option<u64>,
    ) -> RunReceipt {
        let (reason, candidates) = match &outcome {
            RunOutcome::Blocked { reason } => (Some(reason.clone()), Vec::new()),
            RunOutcome::Abstained { reason, candidates } => {
                (Some(reason.clone()), candidates.clone())
            }
            _ => (None, Vec::new()),
        };
        RunReceipt {
            goal: goal.to_owned(),
            clauses,
            outcome_kind: outcome.clone(),
            outcome: outcome.label().to_owned(),
            reason,
            candidates,
            steps,
            final_rev: rev,
            calls: self.calls,
            engine: EngineInfo {
                policy: env!("CARGO_PKG_VERSION"),
                instinct: INSTINCT_VERSION_REQ,
            },
        }
    }
}

/// Build the wire op for a decision — node-bound kinds carry the
/// build-time fingerprint as `expect_fingerprint` (M5). Shared by
/// `Runner` and MCP `act` so the dispatch table can't drift apart.
pub fn op_for_decision(
    decision: &PolicyDecision,
    action: &OfferedAction,
    payload: Option<&str>,
    delivery: DeliveryMode,
    clause: &str,
    keybindings: &[KeybindingInfo],
    mut invoke_args: impl FnMut(&str, &str) -> Result<serde_json::Value, String>,
) -> Result<Op, String> {
    let target = || {
        action
            .target()
            .map(str::to_owned)
            .ok_or_else(|| format!("{} has no dispatch target", decision.action_id))
    };
    let gated = |op: Op| -> Result<Op, String> {
        let fp = action
            .fingerprint()
            .ok_or_else(|| format!("{} has no fingerprint", decision.action_id))?;
        Ok(op.with_fingerprint(fp))
    };
    match decision.kind {
        ActionKind::Click => gated(Op::Click {
            target: target()?,
            delivery,
            expect_fingerprint: None,
        }),
        ActionKind::Type => gated(Op::Type {
            target: target()?,
            text: payload.unwrap_or_default().to_owned(),
            delivery,
            expect_fingerprint: None,
        }),
        ActionKind::SetValue => gated(Op::SetValue {
            target: target()?,
            value: payload.unwrap_or_default().to_owned(),
            expect_fingerprint: None,
        }),
        ActionKind::Key => gated(Op::Key {
            target: target()?,
            key: action.key()?.to_owned(),
            delivery,
            expect_fingerprint: None,
        }),
        ActionKind::Keybinding => {
            let id = target()?;
            let kb = keybindings
                .iter()
                .find(|kb| kb.id == id)
                .ok_or_else(|| format!("keybinding `{id}` not in catalog"))?;
            Ok(Op::Keybinding {
                binding: kb.id.clone(),
                chord: Some(kb.chord.clone()),
                scope: kb.scope,
                confirm: false,
                activate: false,
            })
        }
        ActionKind::Invoke => {
            let name = target()?;
            let args = invoke_args(&name, clause)?;
            Ok(Op::Invoke { name, args })
        }
        other => Err(format!("cannot dispatch kind {}", other.as_str())),
    }
}

#[cfg(test)]
mod tests {
    use super::INSTINCT_VERSION_REQ;

    /// The receipt's `engine.instinct` must equal the manifest's
    /// `instinct-*` requirement — the literal cannot drift silently.
    #[test]
    fn engine_instinct_matches_manifest() {
        let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
            .expect("read gpui-agent-policy manifest");
        for dep in ["instinct-core", "instinct-lexicon", "instinct-text"] {
            let line = format!("{dep} = \"{INSTINCT_VERSION_REQ}\"");
            assert!(
                manifest.contains(&line),
                "manifest missing `{line}` — update INSTINCT_VERSION_REQ"
            );
        }
    }
}
