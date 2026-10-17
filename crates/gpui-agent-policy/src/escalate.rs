//! Optional escalation after the local policy abstains (PRD M7).
//!
//! The fallback runs **only** on [`PolicyOutcome::Abstain`] and must
//! answer inside the same offered [`ActionSpace`] — an abstain is never
//! turned into "top candidate wins", and a choice for a `dangerous`
//! offer still lands on `needs_confirmation`.
//!
//! [`CommandEscalation`] spawns `--escalate-cmd <program> [args…]` once
//! per abstain over a closed JSON stdin/stdout wire:
//!
//! Request `choose`:
//! `{"v":1,"kind":"choose","goal","clause","actions":[…],"history":[…],
//!   "abstain":{"reason","ranked":[…]}}` — every action is `{id,op}` plus
//! optional `role`/`name`.
//!
//! Request `text` (field payload on deterministic [`TextError::Missing`]):
//! `{"v":1,"kind":"text","goal","clause","field":{"id","name"}}`
//!
//! Reply (exactly one): `{"choice":{"id","op"}}` | `{"text":"…"}` |
//! `{"abstain":"reason"}`. Off-menu id/op, extra fields, non-JSON,
//! timeout, or transport failure is a hard error — nothing executes; the
//! abstain reason carries [`ESCALATION_PROTOCOL`] so the runner maps it
//! to `Blocked` (exit 2), not to an ordinary abstain.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::action_space::{ActionSpace, OfferedAction};
use crate::goal::UiGoal;
use crate::policy::{
    HistoryEntry, PolicyDecision, PolicyError, PolicyOutcome, RankedAction, UiPolicy,
};
use crate::text::{TextError, TextResolution};

/// Abstain-reason prefix marking an escalation-protocol violation; the
/// runner maps it to `RunOutcome::Blocked`. "hard error" per PRD M7.2.
pub const ESCALATION_PROTOCOL: &str = "escalation:";

/// Timeout for one escalation round-trip (PRD M7.2 default 30 s).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Stdout byte cap per escalation (PRD M7.2 — 64 KiB).
pub const STDOUT_CAP: u64 = 64 * 1024;

/// Max chars accepted from a `text` reply, single line (PRD M7.2).
pub const TEXT_CAP: usize = 256;

/// Why the local policy abstained, forwarded to the endpoint.
#[derive(Clone, Debug, Default)]
pub struct AbstainReport {
    pub reason: String,
    /// Best-ranked candidates at abstain time (`target_ranked` if any,
    /// else `operation_ranked`) — evidence, not an offer to re-score.
    pub ranked: Vec<RankedAction>,
}

impl AbstainReport {
    fn of(outcome: &PolicyOutcome) -> Self {
        match outcome {
            PolicyOutcome::Abstain {
                reason,
                target_ranked,
                operation_ranked,
            } => {
                let ranked = if target_ranked.is_empty() {
                    operation_ranked.clone()
                } else {
                    target_ranked.clone()
                };
                Self {
                    reason: reason.clone(),
                    ranked,
                }
            }
            PolicyOutcome::Choice(_) => Self::default(),
        }
    }
}

/// Try `local` first; on [`PolicyOutcome::Abstain`] call `fallback`
/// exactly once (never on a local choice, never to re-score a winner).
/// Port of `aui-policy/src/escalate.rs` (PRD M7.1).
pub struct EscalatingPolicy<L, H> {
    pub local: L,
    pub fallback: Option<H>,
    calls: u32,
}

impl<L, H> EscalatingPolicy<L, H> {
    pub fn new(local: L, fallback: Option<H>) -> Self {
        Self {
            local,
            fallback,
            calls: 0,
        }
    }

    /// Escalation round-trips performed — eval accounting.
    pub fn calls(&self) -> u32 {
        self.calls
    }
}

impl<L, H> UiPolicy for EscalatingPolicy<L, H>
where
    L: UiPolicy,
    H: UiEscalate,
{
    fn decide(
        &mut self,
        space: &ActionSpace,
        goal: &UiGoal,
        history: &[HistoryEntry],
    ) -> Result<PolicyOutcome, PolicyError> {
        let first = self.local.decide(space, goal, history)?;
        if matches!(first, PolicyOutcome::Choice(_)) {
            return Ok(first);
        }
        let Some(fallback) = self.fallback.as_mut() else {
            return Ok(first);
        };
        let report = AbstainReport::of(&first);
        if ambiguous_contenders(&report.ranked) {
            // Two contenders share kind+label: identical rows in the menu
            // (e.g. two "Buy milk" entries). No endpoint can tell them
            // apart either — a pick here is a coin flip, not evidence.
            // Paying a model for it violates abstain-over-guess.
            return Ok(first);
        }
        self.calls += 1;
        fallback.decide_escalating(space, goal, history, &report)
    }
}

/// True when the ranked contenders contain an identical (kind, label)
/// pair — the menu itself cannot express which of the two is meant.
fn ambiguous_contenders(ranked: &[RankedAction]) -> bool {
    ranked.iter().enumerate().any(|(i, a)| {
        ranked[..i]
            .iter()
            .any(|b| a.kind == b.kind && a.label == b.label)
    })
}

/// Fallback contract: receives the local abstain as evidence. The
/// default ignores it and re-runs `decide`.
pub trait UiEscalate: UiPolicy {
    fn decide_escalating(
        &mut self,
        space: &ActionSpace,
        goal: &UiGoal,
        history: &[HistoryEntry],
        abstain: &AbstainReport,
    ) -> Result<PolicyOutcome, PolicyError> {
        let _ = abstain;
        self.decide(space, goal, history)
    }
}

impl UiEscalate for crate::policy::InstinctPolicy {}

/// One abstain round-trip to `--escalate-cmd` (PRD M7.2). Holds only
/// the command spec — a fresh process per request.
#[derive(Clone, Debug)]
pub struct CommandEscalation {
    program: String,
    args: Vec<String>,
    timeout: Duration,
}

impl CommandEscalation {
    /// `cmd` is the argv the CLI collected for `--escalate-cmd`.
    pub fn new(cmd: &[String]) -> Result<Self, String> {
        let (program, args) = cmd
            .split_first()
            .ok_or_else(|| "--escalate-cmd needs a program".to_owned())?;
        Ok(Self {
            program: program.clone(),
            args: args.to_vec(),
            timeout: DEFAULT_TIMEOUT,
        })
    }

    /// Test / CLI override for the 30 s default.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Spawn the endpoint, write `req`, read up to [`STDOUT_CAP`],
    /// enforce `timeout`. Every failure is [`ESCALATION_PROTOCOL`]
    /// text — a hard error the runner turns into `Blocked`.
    fn call(&self, req: &Value) -> Result<String, String> {
        let bad = |m: &str| format!("{ESCALATION_PROTOCOL} {m}");
        let mut child = Command::new(&self.program)
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| bad(&format!("spawn `{}`: {e}", self.program)))?;
        let Some(mut stdin) = child.stdin.take() else {
            let _ = child.kill();
            return Err(bad("endpoint stdin unavailable"));
        };
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            return Err(bad("endpoint stdout unavailable"));
        };
        let body = req.to_string();
        // Writer + reader off-thread so a stalled write or an over-cap
        // reply cannot deadlock the timeout.
        let (wtx, wrx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = wtx.send(stdin.write_all(body.as_bytes()).map(|_| stdin));
        });
        let (rtx, rrx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let res = stdout.take(STDOUT_CAP + 1).read_to_end(&mut buf);
            let _ = rtx.send(res.map(|_| buf));
        });
        let writer = match wrx.recv_timeout(self.timeout) {
            Ok(Ok(stdin)) => stdin,
            Ok(Err(e)) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(bad(&format!("write: {e}")));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(bad("write timeout"));
            }
        };
        drop(writer); // EOF — the endpoint reads one object then replies.
        let buf = match rrx.recv_timeout(self.timeout) {
            Ok(Ok(buf)) => buf,
            Ok(Err(e)) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(bad(&format!("read: {e}")));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(bad("timeout"));
            }
        };
        let _ = child.wait();
        if buf.len() as u64 > STDOUT_CAP {
            return Err(bad("reply exceeds 64 KiB cap"));
        }
        String::from_utf8(buf).map_err(|_| bad("reply is not UTF-8"))
    }

    /// Build the `choose` request for this abstain (PRD M7.2). `goal`
    /// and `clause` are the deciding clause — the policy layer only
    /// ever sees clause-level goals.
    fn choose_request(
        space: &ActionSpace,
        goal: &UiGoal,
        history: &[HistoryEntry],
        abstain: &AbstainReport,
    ) -> Value {
        let actions: Vec<Value> = space
            .actions()
            .map(|a| {
                let mut action =
                    json!({"id": a.id().as_str(), "op": a.kind().as_str(), "name": a.label()});
                if let Some(role) = a.role() {
                    action["role"] = json!(role);
                }
                action
            })
            .collect();
        let history: Vec<Value> = history
            .iter()
            .map(|h| {
                json!({"op": h.kind.as_str(), "id": h.action_id.as_str(), "effect": h.verification})
            })
            .collect();
        let ranked: Vec<Value> = abstain
            .ranked
            .iter()
            .map(|r| {
                json!({"id": r.id.as_str(), "op": r.kind.as_str(), "name": r.label,
                       "confidence_millis": r.confidence_millis})
            })
            .collect();
        json!({
            "v": 1,
            "kind": "choose",
            "goal": goal.as_str(),
            "clause": goal.as_str(),
            "actions": actions,
            "history": history,
            "abstain": {"reason": abstain.reason, "ranked": ranked},
        })
    }

    /// One `choose` round-trip; violations surface as
    /// [`ESCALATION_PROTOCOL`] strings for the caller to classify.
    pub fn choose(
        &mut self,
        space: &ActionSpace,
        goal: &UiGoal,
        history: &[HistoryEntry],
        abstain: &AbstainReport,
    ) -> Result<PolicyOutcome, String> {
        let req = Self::choose_request(space, goal, history, abstain);
        let reply = self.call(&req)?;
        Self::parse_choose_reply(space, &reply)
    }

    /// Parse and validate a `choose` reply against the offered space:
    /// exactly one top-level key; `choice` carries only `id`/`op`; the
    /// id is offered and its offered op matches; a `dangerous` offer is
    /// never executable, so it becomes `needs_confirmation` like the
    /// local gate.
    pub fn parse_choose_reply(space: &ActionSpace, reply: &str) -> Result<PolicyOutcome, String> {
        let bad = |m: &str| format!("{ESCALATION_PROTOCOL} {m}");
        let value: Value = serde_json::from_str(reply).map_err(|e| bad(&e.to_string()))?;
        let obj = value
            .as_object()
            .ok_or_else(|| bad("reply not an object"))?;
        if obj.len() != 1 {
            return Err(bad("expected exactly one of `choice` or `abstain`"));
        }
        if let Some(reason) = obj.get("abstain") {
            let reason = reason
                .as_str()
                .ok_or_else(|| bad("abstain must be a string"))?;
            return Ok(PolicyOutcome::Abstain {
                reason: format!("escalated abstain: {reason}"),
                operation_ranked: Vec::new(),
                target_ranked: Vec::new(),
            });
        }
        let choice = obj
            .get("choice")
            .and_then(Value::as_object)
            .ok_or_else(|| bad("expected `choice` object"))?;
        if choice.keys().any(|k| k != "id" && k != "op") {
            return Err(bad("choice may only carry `id` and `op`"));
        }
        let id = choice
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("choice.id missing"))?;
        let op = choice
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("choice.op missing"))?;
        let offered: &OfferedAction = space
            .get_str(id)
            .ok_or_else(|| bad(&format!("off-menu id `{id}`")))?;
        if offered.kind().as_str() != op {
            return Err(bad(&format!(
                "id `{id}` is {} not {op}",
                offered.kind().as_str()
            )));
        }
        if offered.dangerous() {
            return Ok(PolicyOutcome::Abstain {
                reason: "needs_confirmation".to_owned(),
                operation_ranked: Vec::new(),
                target_ranked: Vec::new(),
            });
        }
        Ok(PolicyOutcome::Choice(PolicyDecision {
            action_id: offered.id().clone(),
            kind: offered.kind(),
            target_label: offered.label().to_owned(),
            // Endpoint confidence is not comparable to Instinct millis.
            confidence_millis: 0,
            operation_ranked: Vec::new(),
            target_ranked: Vec::new(),
        }))
    }

    /// `kind:"text"` fallback for a deterministic [`TextError::Missing`]:
    /// ask the endpoint for the payload of the chosen field. Password
    /// fields are never sent (PRD M7.2) — callers check before this.
    pub fn resolve_text(
        &mut self,
        goal: &UiGoal,
        field_id: &str,
        field_label: &str,
        context_fingerprint: u64,
    ) -> Result<TextResolution, TextError> {
        let req = json!({
            "v": 1,
            "kind": "text",
            "goal": goal.as_str(),
            "clause": goal.as_str(),
            "field": {"id": field_id, "name": field_label},
        });
        let reply = self.call(&req).map_err(TextError::Invalid)?;
        Self::parse_text_reply(&reply, context_fingerprint)
    }

    /// Parse a `text` reply: `{"text":"…"}` ≤ [`TEXT_CAP`] and single
    /// line; `{"abstain":"…"}` maps back to [`TextError::Missing`] so
    /// the caller abstains as the local tier would.
    pub fn parse_text_reply(
        reply: &str,
        context_fingerprint: u64,
    ) -> Result<TextResolution, TextError> {
        let bad = |m: &str| TextError::Invalid(format!("{ESCALATION_PROTOCOL} {m}"));
        let value: Value = serde_json::from_str(reply).map_err(|e| bad(&e.to_string()))?;
        let obj = value
            .as_object()
            .ok_or_else(|| bad("reply not an object"))?;
        if obj.len() != 1 {
            return Err(bad("expected exactly one of `text` or `abstain`"));
        }
        if let Some(reason) = obj.get("abstain") {
            if reason.as_str().is_none() {
                return Err(bad("abstain must be a string"));
            }
            return Err(TextError::Missing);
        }
        let text = obj
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("expected `text` string"))?;
        if text.len() > TEXT_CAP {
            return Err(bad("text exceeds 256 chars"));
        }
        if text.contains(['\n', '\r']) {
            return Err(bad("text must be a single line"));
        }
        Ok(TextResolution {
            text: text.to_owned(),
            context_fingerprint,
        })
    }
}

impl UiPolicy for CommandEscalation {
    fn decide(
        &mut self,
        space: &ActionSpace,
        goal: &UiGoal,
        history: &[HistoryEntry],
    ) -> Result<PolicyOutcome, PolicyError> {
        self.decide_escalating(space, goal, history, &AbstainReport::default())
    }
}

impl UiEscalate for CommandEscalation {
    fn decide_escalating(
        &mut self,
        space: &ActionSpace,
        goal: &UiGoal,
        history: &[HistoryEntry],
        abstain: &AbstainReport,
    ) -> Result<PolicyOutcome, PolicyError> {
        if goal.as_str().is_empty() {
            return Err(PolicyError::EmptyGoal);
        }
        // A protocol violation is a hard error, not a policy error:
        // surface it as an ESCALATION_PROTOCOL abstain so the runner
        // maps it to Blocked with nothing dispatched.
        match self.choose(space, goal, history, abstain) {
            Ok(outcome) => Ok(outcome),
            Err(protocol) => Ok(PolicyOutcome::Abstain {
                reason: protocol,
                operation_ranked: Vec::new(),
                target_ranked: Vec::new(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action_space::OfferedInvoke;
    use crate::policy::InstinctPolicy;
    use gpui_agent::protocol::PlatformKind;
    use gpui_agent::tree::{Bounds, UiNode, UiTree};

    fn boxed(mut n: UiNode) -> UiNode {
        n.bounds = Bounds {
            x: 0.0,
            y: 0.0,
            w: 40.0,
            h: 20.0,
        };
        n
    }

    /// window → textbox + Add button; plus a dangerous `app.quit`
    /// invoke so the needs_confirmation path has an offer.
    fn space() -> ActionSpace {
        let tree = UiTree {
            app: "todo".to_owned(),
            platform: PlatformKind::Headless,
            ready: true,
            rev: Some(1),
            nodes: vec![UiNode::window("w", "App").with_children(vec![
                boxed(UiNode::textbox("input", "Name")),
                boxed(UiNode::button("add", "Add")),
            ])],
        };
        ActionSpace::from_tree(
            &tree,
            &[],
            &[OfferedInvoke {
                name: "app.quit".to_owned(),
                description: "quit the app".to_owned(),
                dangerous: true,
            }],
        )
    }

    #[test]
    fn choose_reply_valid_choice() {
        let space = space();
        let outcome = CommandEscalation::parse_choose_reply(
            &space,
            r#"{"choice":{"id":"CLICK:add","op":"CLICK"}}"#,
        )
        .expect("valid reply");
        let PolicyOutcome::Choice(d) = outcome else {
            panic!("expected Choice: {outcome:?}")
        };
        assert_eq!(d.action_id.as_str(), "CLICK:add");
        assert_eq!(d.kind, crate::ActionKind::Click);
        assert_eq!(d.confidence_millis, 0);
    }

    #[test]
    fn choose_reply_off_menu_id_is_protocol_error() {
        let space = space();
        let err = CommandEscalation::parse_choose_reply(
            &space,
            r#"{"choice":{"id":"CLICK:ghost","op":"CLICK"}}"#,
        )
        .expect_err("off-menu id must fail");
        assert!(err.starts_with(ESCALATION_PROTOCOL), "{err}");
        assert!(err.contains("off-menu"), "{err}");
    }

    #[test]
    fn choose_reply_mismatched_op_is_protocol_error() {
        let space = space();
        let err = CommandEscalation::parse_choose_reply(
            &space,
            r#"{"choice":{"id":"CLICK:add","op":"TYPE"}}"#,
        )
        .expect_err("wrong op must fail");
        assert!(err.starts_with(ESCALATION_PROTOCOL), "{err}");
    }

    #[test]
    fn choose_reply_extra_fields_is_protocol_error() {
        let space = space();
        for reply in [
            r##"{"choice":{"id":"CLICK:add","op":"CLICK","selector":"#add"}}"##,
            r#"{"choice":{"id":"CLICK:add","op":"CLICK"},"note":"hi"}"#,
        ] {
            let err = CommandEscalation::parse_choose_reply(&space, reply)
                .expect_err("extra field must fail");
            assert!(err.starts_with(ESCALATION_PROTOCOL), "{reply} → {err}");
        }
    }

    #[test]
    fn choose_reply_non_json_is_protocol_error() {
        let space = space();
        let err = CommandEscalation::parse_choose_reply(&space, "CLICK:add")
            .expect_err("non-JSON must fail");
        assert!(err.starts_with(ESCALATION_PROTOCOL), "{err}");
    }

    #[test]
    fn choose_reply_abstain_stays_abstain() {
        let space = space();
        let outcome = CommandEscalation::parse_choose_reply(&space, r#"{"abstain":"not sure"}"#)
            .expect("abstain reply");
        let PolicyOutcome::Abstain { reason, .. } = outcome else {
            panic!("expected Abstain: {outcome:?}")
        };
        assert_eq!(reason, "escalated abstain: not sure");
    }

    #[test]
    fn choose_reply_dangerous_needs_confirmation() {
        let space = space();
        let outcome = CommandEscalation::parse_choose_reply(
            &space,
            r#"{"choice":{"id":"INVOKE:app.quit","op":"INVOKE"}}"#,
        )
        .expect("dangerous pick is an offered action");
        let PolicyOutcome::Abstain { reason, .. } = outcome else {
            panic!("expected Abstain: {outcome:?}")
        };
        assert_eq!(reason, "needs_confirmation");
    }

    #[test]
    fn text_reply_validates_shape() {
        let ok = CommandEscalation::parse_text_reply(r#"{"text":"hello"}"#, 7).expect("text");
        assert_eq!(ok.text, "hello");
        assert_eq!(ok.context_fingerprint, 7);
        // abstain maps back to the Missing verdict
        assert_eq!(
            CommandEscalation::parse_text_reply(r#"{"abstain":"no idea"}"#, 7),
            Err(TextError::Missing)
        );
        // too long / multiline / extra keys / non-object all fail
        let long = format!(r#"{{"text":"{}"}}"#, "x".repeat(TEXT_CAP + 1));
        for bad_reply in [
            long.as_str(),
            "{\"text\":\"a\\nb\"}",
            r#"{"text":"x","note":"y"}"#,
            r#"["text"]"#,
            "hello",
        ] {
            let err =
                CommandEscalation::parse_text_reply(bad_reply, 7).expect_err("invalid text reply");
            match err {
                TextError::Invalid(m) => assert!(m.starts_with(ESCALATION_PROTOCOL), "{m}"),
                other => panic!("expected Invalid: {other:?}"),
            }
        }
    }

    /// Counting fallback so tests can prove when it was (not) called.
    #[derive(Clone)]
    struct Stub {
        outcome: PolicyOutcome,
        calls: u32,
    }

    impl Stub {
        fn new(outcome: PolicyOutcome) -> Self {
            Self { outcome, calls: 0 }
        }
    }

    impl UiPolicy for Stub {
        fn decide(
            &mut self,
            _s: &ActionSpace,
            _g: &UiGoal,
            _h: &[HistoryEntry],
        ) -> Result<PolicyOutcome, PolicyError> {
            Ok(self.outcome.clone())
        }
    }

    impl UiEscalate for Stub {
        fn decide_escalating(
            &mut self,
            _s: &ActionSpace,
            _g: &UiGoal,
            _h: &[HistoryEntry],
            _a: &AbstainReport,
        ) -> Result<PolicyOutcome, PolicyError> {
            self.calls += 1;
            Ok(self.outcome.clone())
        }
    }

    fn abstain(reason: &str) -> PolicyOutcome {
        PolicyOutcome::Abstain {
            reason: reason.to_owned(),
            operation_ranked: Vec::new(),
            target_ranked: Vec::new(),
        }
    }

    fn choice_done() -> PolicyOutcome {
        PolicyOutcome::Choice(PolicyDecision {
            action_id: crate::ActionId::try_new("DONE").expect("id"),
            kind: crate::ActionKind::Done,
            target_label: "done".to_owned(),
            confidence_millis: 800,
            operation_ranked: Vec::new(),
            target_ranked: Vec::new(),
        })
    }

    #[test]
    fn escalating_policy_short_circuits_on_local_choice() {
        let space = space();
        let fallback = Stub::new(abstain("never"));
        let mut policy = EscalatingPolicy::new(Stub::new(choice_done()), Some(fallback));
        let outcome = policy
            .decide(&space, &UiGoal::new("anything"), &[])
            .expect("decide");
        assert!(matches!(outcome, PolicyOutcome::Choice(_)));
        assert_eq!(policy.calls(), 0, "fallback must not run on local choice");
        assert_eq!(policy.fallback.as_ref().unwrap().calls, 0);
    }

    #[test]
    fn escalating_policy_calls_fallback_once_on_abstain() {
        let space = space();
        let mut policy = EscalatingPolicy::new(
            Stub::new(abstain("no winner")),
            Some(Stub::new(choice_done())),
        );
        let outcome = policy
            .decide(&space, &UiGoal::new("anything"), &[])
            .expect("decide");
        assert!(matches!(outcome, PolicyOutcome::Choice(_)));
        assert_eq!(policy.calls(), 1);
        assert_eq!(policy.fallback.as_ref().unwrap().calls, 1);
    }

    #[test]
    fn escalating_policy_fallback_abstain_stays_abstain() {
        let space = space();
        let mut policy = EscalatingPolicy::new(
            Stub::new(abstain("no winner")),
            Some(Stub::new(abstain("me neither"))),
        );
        let outcome = policy
            .decide(&space, &UiGoal::new("anything"), &[])
            .expect("decide");
        let PolicyOutcome::Abstain { reason, .. } = outcome else {
            panic!("expected Abstain: {outcome:?}")
        };
        assert_eq!(reason, "me neither");
    }

    #[test]
    fn escalating_policy_no_fallback_keeps_local_abstain() {
        let space = space();
        let mut policy = EscalatingPolicy::<Stub, Stub>::new(Stub::new(abstain("no winner")), None);
        let outcome = policy
            .decide(&space, &UiGoal::new("anything"), &[])
            .expect("decide");
        let PolicyOutcome::Abstain { reason, .. } = outcome else {
            panic!("expected Abstain: {outcome:?}")
        };
        assert_eq!(reason, "no winner");
        assert_eq!(policy.calls(), 0);
    }

    #[test]
    fn command_escalation_requires_a_program() {
        assert!(CommandEscalation::new(&[]).is_err());
        let esc = CommandEscalation::new(&["prog".to_owned(), "a1".to_owned()]).expect("cmd");
        assert_eq!(esc.program, "prog");
        assert_eq!(esc.args, ["a1"]);
    }

    /// The Instinct local path must be unaffected: an InstinctPolicy
    /// inside EscalatingPolicy still decides normally.
    #[test]
    fn instinct_under_wrapper_still_decides() {
        let space = space();
        let mut policy = EscalatingPolicy::new(InstinctPolicy::default(), None::<Stub>);
        let outcome = policy
            .decide(&space, &UiGoal::new("Click Add"), &[])
            .expect("decide");
        assert!(matches!(
            outcome,
            PolicyOutcome::Choice(_) | PolicyOutcome::Abstain { .. }
        ));
    }

    fn ranked(id: &str, kind: crate::ActionKind, label: &str) -> RankedAction {
        RankedAction {
            id: crate::ActionId::try_new(id).expect("id"),
            kind,
            label: label.to_owned(),
            confidence_millis: 500,
        }
    }

    fn abstain_with(ranked: Vec<RankedAction>) -> PolicyOutcome {
        PolicyOutcome::Abstain {
            reason: "margin".to_owned(),
            operation_ranked: Vec::new(),
            target_ranked: ranked,
        }
    }

    /// Two identical "Buy milk" rows tie locally; the fallback sees the
    /// same identical menu and can only coin-flip — so it must not run.
    #[test]
    fn escalating_policy_skips_fallback_on_identical_contenders() {
        let space = space();
        let dup = vec![
            ranked("CLICK:m1", crate::ActionKind::Click, "Buy milk"),
            ranked("CLICK:m2", crate::ActionKind::Click, "Buy milk"),
        ];
        let mut policy =
            EscalatingPolicy::new(Stub::new(abstain_with(dup)), Some(Stub::new(choice_done())));
        let outcome = policy
            .decide(&space, &UiGoal::new("Delete Buy milk"), &[])
            .expect("decide");
        let PolicyOutcome::Abstain { reason, .. } = outcome else {
            panic!("expected local abstain: {outcome:?}")
        };
        assert_eq!(reason, "margin");
        assert_eq!(policy.calls(), 0, "identical contenders must not escalate");
        assert_eq!(policy.fallback.as_ref().unwrap().calls, 0);
    }

    /// Same label but a different kind is still distinguishable to the
    /// endpoint — escalation proceeds.
    #[test]
    fn escalating_policy_escalates_same_label_different_kind() {
        let space = space();
        let contenders = vec![
            ranked("CLICK:m1", crate::ActionKind::Click, "Buy milk"),
            ranked("TYPE:m2", crate::ActionKind::Type, "Buy milk"),
        ];
        let mut policy = EscalatingPolicy::new(
            Stub::new(abstain_with(contenders)),
            Some(Stub::new(choice_done())),
        );
        let outcome = policy
            .decide(&space, &UiGoal::new("anything"), &[])
            .expect("decide");
        assert!(matches!(outcome, PolicyOutcome::Choice(_)));
        assert_eq!(policy.calls(), 1);
    }
}
