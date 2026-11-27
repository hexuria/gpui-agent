//! Recipe-only bounded `scroll_until` runs against a fake scroll host.
#![expect(
    clippy::unwrap_used,
    reason = "a failed setup/assert aborts the test anyway"
)]

use std::collections::BTreeMap;

use gpui_agent::dispatch::DispatchResult;
use gpui_agent::host::AgentHost;
use gpui_agent::protocol::{
    DeliveryMode, HelloAuth, HelloInfo, Op, PROTOCOL_VERSION, PlatformKind,
};
use gpui_agent::testing::TestHost;
use gpui_agent::tree::{UiNode, UiTree};
use gpui_agent_recipe::{
    Recipe, RecipeErrorCode, RunError, RunOptions, compile_plan, parse_wants, run_plan,
    todo_registry,
};

#[derive(Default)]
struct ScrollHost {
    scrolls: usize,
    reveal_at: Option<usize>,
}

impl AgentHost for ScrollHost {
    fn hello(&self) -> HelloInfo {
        HelloInfo {
            protocol: PROTOCOL_VERSION,
            app: "scroll".into(),
            platform: PlatformKind::Headless,
            os: gpui_agent::protocol::host_os(),
            ready: true,
            deliveries: vec![DeliveryMode::Semantic],
            auth: HelloAuth::None,
            bundle_id: None,
            executable: None,
            features: vec!["scroll".into()],
            framework: None,
            device: None,
        }
    }

    fn snapshot(&self) -> UiTree {
        let mut list = UiNode::new("list", "scroll", "List");
        list.children
            .push(UiNode::new("row-0", "listitem", "Row 0"));
        if self.reveal_at.is_some_and(|at| self.scrolls >= at) {
            list.children
                .push(UiNode::new("row-target", "listitem", "Target row"));
        }
        UiTree {
            app: "scroll".into(),
            platform: PlatformKind::Headless,
            ready: true,
            nodes: vec![list],
            rev: Some(u64::try_from(self.scrolls).unwrap_or(u64::MAX)),
        }
    }

    fn dispatch(&mut self, op: &Op) -> Result<DispatchResult, String> {
        if matches!(op, Op::Scroll { .. }) {
            self.scrolls += 1;
        }
        Ok(DispatchResult::empty())
    }
}

fn host(reveal_at: Option<usize>) -> TestHost<ScrollHost> {
    TestHost::spawn(
        ScrollHost {
            scrolls: 0,
            reveal_at,
        },
        None,
    )
    .unwrap()
}

fn json_recipe(max_steps: u64) -> Recipe {
    Recipe::from_json(&format!(
        r#"{{"name":"scroll","steps":[{{"id":"s1","op":"scroll_until","target":"list","until":"row-target","dy":120,"max_steps":{max_steps}}}]}}"#
    ))
    .unwrap()
}

fn run(
    host: &TestHost<ScrollHost>,
    recipe: &Recipe,
) -> Result<gpui_agent_recipe::Receipt, RunError> {
    let plan = compile_plan(recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    let mut client = host.client();
    run_plan(&mut client, &plan, &RunOptions::new())
}

#[test]
fn found_after_two_scroll_steps() {
    let host = host(Some(2));
    let receipt = run(&host, &json_recipe(5)).expect("run");
    assert_eq!(receipt.steps[0].result.as_ref().unwrap()["steps"], 2);
    assert_eq!(host.host().lock().unwrap().scrolls, 2);
}

#[test]
fn never_found_is_a_bounded_failure_with_revision() {
    let host = host(None);
    let Err(RunError::Step { error, .. }) = run(&host, &json_recipe(2)) else {
        panic!("must be bounded");
    };
    assert!(error.contains("after 2 scroll steps"), "{error}");
    assert!(error.contains("last snapshot revision 2"), "{error}");
    assert_eq!(host.host().lock().unwrap().scrolls, 2);
}

#[test]
fn already_visible_scrolls_zero_times() {
    let host = host(Some(0));
    let receipt = run(&host, &json_recipe(5)).expect("run");
    assert_eq!(receipt.steps[0].result.as_ref().unwrap()["steps"], 0);
    assert_eq!(host.host().lock().unwrap().scrolls, 0);
}

#[test]
fn wants_and_json_compile_to_the_same_plan() {
    let json = compile_plan(&json_recipe(7), &BTreeMap::new(), &todo_registry()).unwrap();
    let wants_recipe = parse_wants(
        "scroll_until list row-target --dy 120 --max-steps 7",
        "scroll",
    )
    .unwrap();
    let wants = compile_plan(&wants_recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    assert_eq!(json.fingerprint, wants.fingerprint);
    assert_eq!(json.steps[0].until.as_deref(), Some("row-target"));
    assert_eq!(json.steps[0].max_steps, Some(7));
}

#[test]
fn max_steps_over_fifty_is_refused_at_parse() {
    let err = Recipe::from_json(
        r#"{"name":"x","steps":[{"id":"s","op":"scroll_until","target":"list","until":"row","dy":1,"max_steps":51}]}"#,
    )
    .unwrap_err();
    assert_eq!(err.code, RecipeErrorCode::Parse);
    assert!(err.to_string().contains("1 to 50"), "{err}");
    let err = parse_wants("scroll_until list row --dy 1 --max-steps 51", "x")
        .unwrap_err()
        .to_string();
    assert!(err.contains("1 to 50"), "{err}");
}

#[test]
fn wants_scroll_maps_to_the_json_scroll_op() {
    let recipe = parse_wants("scroll list --dy 120 --dx -4", "s").unwrap();
    match &recipe.steps[0].op {
        Op::Scroll { target, dx, dy, .. } => {
            assert_eq!(target.as_deref(), Some("list"));
            assert_eq!(*dx, -4.0);
            assert_eq!(*dy, 120.0);
        }
        other => panic!("unexpected {other:?}"),
    }
}
