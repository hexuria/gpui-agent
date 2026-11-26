//! Paced `type` (`key_delay_ms`) against a host whose inputs behave as a
//! real app's do: a `type` aimed at an input that does not have the focus
//! focuses it first (a press, which puts the caret where the press lands),
//! and a code box moves the focus on to its next cell with each digit.

use std::collections::BTreeMap;

use gpui_agent::dispatch::DispatchResult;
use gpui_agent::host::AgentHost;
use gpui_agent::protocol::{
    DeliveryMode, HelloAuth, HelloInfo, Op, PROTOCOL_VERSION, PlatformKind,
};

use gpui_agent::DeliveryStatus;
use gpui_agent::testing::TestHost;
use gpui_agent::tree::{UiNode, UiTree};
use gpui_agent_recipe::{
    Recipe, RecipeErrorCode, RunError, RunOptions, compile_plan, todo_registry,
};

/// One amount box, six code cells and a box that refuses an `x`.
#[derive(Default)]
struct Inputs {
    amount: String,
    /// Where the next character goes in `amount`.
    caret: usize,
    cells: [Option<char>; 6],
    strict: String,
    focused: Option<String>,
    /// Every `type` the host received.
    typed: Vec<(String, String, Option<u64>)>,
    /// How many times a `type` had to focus its target.
    focus_presses: usize,
}

const CELLS: usize = 6;

fn cell(index: usize) -> String {
    format!("code-{index}")
}

impl Inputs {
    fn focus(&mut self, target: &str) {
        if self.focused.as_deref() != Some(target) {
            self.focus_presses += 1;
            self.focused = Some(target.to_owned());
            // A press lands in the middle of the box: on a filled amount
            // that is before its first character, as the macOS money box did.
            if target == "amount" {
                self.caret = 0;
            }
        }
    }
}

impl AgentHost for Inputs {
    fn hello(&self) -> HelloInfo {
        HelloInfo {
            protocol: PROTOCOL_VERSION,
            framework: None,
            device: None,
            app: "inputs".into(),
            platform: PlatformKind::Desktop,
            os: gpui_agent::protocol::host_os(),
            ready: true,
            deliveries: vec![DeliveryMode::Semantic, DeliveryMode::Virtual],
            auth: HelloAuth::None,
            bundle_id: None,
            executable: None,
            features: Vec::new(),
        }
    }

    fn snapshot(&self) -> UiTree {
        let input = |id: &str, value: String| {
            let mut node = UiNode::textbox(id, id);
            node.value = Some(value);
            node.focused = self.focused.as_deref() == Some(id);
            node
        };
        let mut nodes = vec![
            input("amount", self.amount.clone()),
            input("strict", self.strict.clone()),
        ];
        let mut group = UiNode::new("code", "group", "Code");
        for (index, digit) in self.cells.iter().enumerate() {
            group.children.push(input(
                &cell(index),
                digit.map(String::from).unwrap_or_default(),
            ));
        }
        nodes.push(group);
        UiTree {
            app: "inputs".into(),
            platform: PlatformKind::Desktop,
            ready: true,
            nodes,
            rev: None,
        }
    }

    fn dispatch(&mut self, op: &Op) -> Result<DispatchResult, String> {
        let Op::Type {
            target,
            text,
            expect_fingerprint,
            ..
        } = op
        else {
            return Ok(DispatchResult::empty());
        };
        self.typed
            .push((target.clone(), text.clone(), *expect_fingerprint));
        match target.as_str() {
            "amount" => {
                self.focus(target);
                self.amount.insert_str(self.caret, text);
                self.caret += text.len();
            }
            "strict" => {
                if text.contains('x') {
                    return Err(format!("invalid_input: `strict` takes no x (got {text:?})"));
                }
                self.focus(target);
                self.strict.push_str(text);
            }
            id => {
                let index = (0..CELLS)
                    .find(|index| cell(*index) == id)
                    .ok_or_else(|| format!("not_found: `{id}`"))?;
                self.focus(target);
                self.cells[index] = text.chars().next();
                if index + 1 < CELLS {
                    self.focused = Some(cell(index + 1));
                }
            }
        }
        Ok(DispatchResult::json(serde_json::json!({ "value": text })))
    }
}

fn paced(target: &str, text: &str, delivery: Option<&str>) -> Recipe {
    let delivery = delivery.map_or(String::new(), |mode| format!(r#", "delivery": "{mode}""#));
    Recipe::from_json(&format!(
        r#"{{"name": "paced", "steps": [
            {{"id": "keys", "op": "type", "target": "{target}", "text": "{text}", "key_delay_ms": 1{delivery}}}
        ]}}"#
    ))
    .expect("a valid paced recipe")
}

fn run(host: &TestHost<Inputs>, recipe: &Recipe) -> Result<gpui_agent_recipe::Receipt, RunError> {
    let plan =
        compile_plan(recipe, &BTreeMap::new(), &todo_registry()).expect("a valid paced recipe");
    run_plan_on(host, &plan)
}

fn run_plan_on(
    host: &TestHost<Inputs>,
    plan: &gpui_agent_recipe::Plan,
) -> Result<gpui_agent_recipe::Receipt, RunError> {
    let mut client = host.client();
    gpui_agent_recipe::run_plan(&mut client, plan, &RunOptions::new())
}

#[test]
fn a_filled_text_box_keeps_its_caret_after_the_typed_text() {
    let host = TestHost::spawn(Inputs::default(), None).unwrap();
    run(&host, &paced("amount", "1234.56", None)).expect("run");
    let inputs = host.host();
    let inputs = inputs.lock().unwrap();
    // Re-pressing the box for every key would have put each key at the
    // front: "65.4321".
    assert_eq!(inputs.amount, "1234.56");
    assert_eq!(inputs.focus_presses, 1, "the box was focused once");
}

#[test]
fn a_code_box_that_moves_its_focus_on_gets_one_digit_per_cell() {
    let host = TestHost::spawn(Inputs::default(), None).unwrap();
    run(&host, &paced("code-0", "123456", None)).expect("run");
    let inputs = host.host();
    let inputs = inputs.lock().unwrap();
    let cells: String = inputs
        .cells
        .iter()
        .map(|digit| digit.unwrap_or('_'))
        .collect();
    assert_eq!(cells, "123456", "each digit in the cell that had the focus");
    let targets: Vec<&str> = inputs
        .typed
        .iter()
        .map(|(target, ..)| target.as_str())
        .collect();
    assert_eq!(
        targets,
        ["code-0", "code-1", "code-2", "code-3", "code-4", "code-5"]
    );
}

#[test]
fn a_refusal_in_the_middle_reports_a_partial_delivery() {
    let host = TestHost::spawn(Inputs::default(), None).unwrap();
    let Err(RunError::Step {
        id, error, receipt, ..
    }) = run(&host, &paced("strict", "12x4", None))
    else {
        panic!("the third key must be refused");
    };
    assert_eq!(id, "keys");
    assert!(error.contains("stopped at key 3 of 4"), "{error}");
    assert!(error.contains("the first 2 reached the app"), "{error}");
    let step = receipt.steps.last().unwrap();
    assert_eq!(
        step.delivery_status,
        Some(DeliveryStatus::DeliveryUncertain)
    );
    let inputs = host.host();
    let inputs = inputs.lock().unwrap();
    assert_eq!(
        inputs.strict, "12",
        "the keys before the refused one are in the box"
    );
    assert_eq!(inputs.typed.len(), 3, "nothing after the refused key");
}

#[test]
fn a_refused_first_key_is_not_delivered() {
    let host = TestHost::spawn(Inputs::default(), None).unwrap();
    let Err(RunError::Step { error, .. }) = run(&host, &paced("strict", "x1", None)) else {
        panic!("the first key must be refused");
    };
    assert!(!error.contains("stopped at key"), "{error}");
    assert_eq!(host.host().lock().unwrap().typed.len(), 1);
}

#[test]
fn virtual_paced_typing_refuses_a_character_with_no_keystroke_before_any_key() {
    let host = TestHost::spawn(Inputs::default(), None).unwrap();
    let Err(RunError::Step { error, receipt, .. }) =
        run(&host, &paced("amount", "12\u{2603}", Some("virtual")))
    else {
        panic!("a snowman has no keystroke");
    };
    assert!(error.contains("ASCII"), "{error}");
    assert_eq!(
        receipt.steps.last().unwrap().delivery_status,
        Some(DeliveryStatus::NotDelivered)
    );
    assert!(
        host.host().lock().unwrap().typed.is_empty(),
        "no key was sent"
    );
}

#[test]
fn only_the_first_key_carries_the_fingerprint_gate() {
    let fingerprint = Inputs::default()
        .snapshot()
        .fingerprint_of("amount")
        .unwrap();
    let host = TestHost::spawn(Inputs::default(), None).unwrap();
    let recipe = Recipe::from_json(&format!(
        r#"{{"name": "gated", "steps": [
            {{"id": "keys", "op": "type", "target": "amount", "text": "12", "key_delay_ms": 1, "expect_fingerprint": {fingerprint}}}
        ]}}"#
    ))
    .unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    // The first key's value change makes the second key's gate stale: a
    // gate on every key would fail the step here.
    run_plan_on(&host, &plan).expect("run");
    let gates: Vec<Option<u64>> = host
        .host()
        .lock()
        .unwrap()
        .typed
        .iter()
        .map(|(.., gate)| *gate)
        .collect();
    assert_eq!(gates, [Some(fingerprint), None]);
    assert_eq!(host.host().lock().unwrap().amount, "12");
}

#[test]
fn a_paced_type_longer_than_two_minutes_is_refused_before_it_runs() {
    let text = "x".repeat(14);
    let recipe = Recipe::from_json(&format!(
        r#"{{"name": "slow", "steps": [{{"id": "keys", "op": "type", "target": "amount", "text": "{text}", "key_delay_ms": 10000}}]}}"#
    ))
    .unwrap();
    let err = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap_err();
    assert_eq!(err.code, RecipeErrorCode::InvalidStep, "{err}");
    let recipe = Recipe::from_json(&format!(
        r#"{{"name": "ok", "steps": [{{"id": "keys", "op": "type", "target": "amount", "text": "{}", "key_delay_ms": 10000}}]}}"#,
        "x".repeat(13)
    ))
    .unwrap();
    compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).expect("12 pauses of 10 s fit");
}
