//! Review coverage for target-relative press aiming.

use std::collections::BTreeMap;

use gpui_agent::dispatch::DispatchResult;
use gpui_agent::host::AgentHost;
use gpui_agent::protocol::{HelloAuth, HelloInfo, Op, PROTOCOL_VERSION, PlatformKind};
use gpui_agent::testing::TestHost;
use gpui_agent::tree::{UiNode, UiTree};
use gpui_agent_recipe::{
    Effect, OpSchema, Recipe, RunOptions, SchemaKind, compile_plan, protocol_registry, run_plan,
};

#[derive(Default)]
struct PressHost {
    dispatched: usize,
}

impl AgentHost for PressHost {
    fn hello(&self) -> HelloInfo {
        HelloInfo {
            protocol: PROTOCOL_VERSION,
            app: "press-host".into(),
            platform: PlatformKind::Headless,
            os: gpui_agent::protocol::host_os(),
            ready: true,
            deliveries: Vec::new(),
            auth: HelloAuth::None,
            bundle_id: None,
            executable: None,
            features: vec!["press".into()],
            framework: None,
            device: None,
        }
    }

    fn snapshot(&self) -> UiTree {
        UiTree {
            app: "press-host".into(),
            platform: PlatformKind::Headless,
            ready: true,
            nodes: vec![UiNode::new("button", "button", "Button")],
            rev: None,
        }
    }

    fn dispatch(&mut self, op: &Op) -> Result<DispatchResult, String> {
        if matches!(op, Op::Press { .. }) {
            self.dispatched += 1;
        }
        Ok(DispatchResult::empty())
    }
}

#[derive(Default)]
struct NoPressHost(PressHost);
impl AgentHost for NoPressHost {
    fn hello(&self) -> HelloInfo {
        let mut hello = PressHost::default().hello();
        hello.features.clear();
        hello
    }
    fn snapshot(&self) -> UiTree {
        PressHost::default().snapshot()
    }
    fn dispatch(&mut self, op: &Op) -> Result<DispatchResult, String> {
        self.0.dispatch(op)
    }
}

#[test]
fn missing_press_within_is_preflighted_before_any_recipe_op() {
    let recipe = Recipe::from_json(
        r#"{"name":"aim","steps":[
            {"id":"plain","op":"press","target":"button"},
            {"id":"aimed","op":"press","target":"button","within":{"x":1,"y":2},"needs":["plain"]}
        ]}"#,
    )
    .unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &protocol_registry()).unwrap();
    let host = TestHost::spawn(PressHost::default(), None).unwrap();
    let state = host.host();
    let mut client = host.client();

    let error = run_plan(&mut client, &plan, &RunOptions::new()).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("before the first wave"), "{text}");
    assert!(text.contains("aimed"), "{text}");
    assert!(text.contains("press_within"), "{text}");
    assert_eq!(
        state.lock().unwrap().dispatched,
        0,
        "no recipe op may reach the host"
    );
}

#[test]
fn press_with_and_without_within_uses_the_base_op_schema() {
    let mut registry = protocol_registry();
    let mut press = OpSchema::new("press", SchemaKind::Protocol, "press");
    press.effects = vec![Effect::Write];
    press.idempotent = true;
    press.verified = true;
    press.required = vec!["target".into()];
    registry.insert(press).unwrap();
    let recipe = Recipe::from_json(
        r#"{"name":"presses","steps":[
            {"id":"root","op":"wait"},
            {"id":"plain","op":"press","target":"button","needs":["root"]},
            {"id":"aimed","op":"press","target":"button","within":{"x":1,"y":2},"needs":["root"]}
        ]}"#,
    )
    .unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &registry).unwrap();
    let plain_step = plan.steps.iter().find(|step| step.id == "plain").unwrap();
    let aimed_step = plan.steps.iter().find(|step| step.id == "aimed").unwrap();
    assert_eq!(plain_step.schema.as_deref(), Some("press"));
    assert_eq!(aimed_step.schema.as_deref(), Some("press"));
    assert_eq!(plain_step.effects, aimed_step.effects);
    assert_eq!(plain_step.idempotent, aimed_step.idempotent);
    assert_eq!(
        plan.waves,
        vec![
            vec!["root".to_string()],
            vec!["aimed".to_string(), "plain".to_string()]
        ]
    );
}

#[test]
fn capability_preflight_is_an_unsupported_op_error() {
    let recipe = Recipe::from_json(
        r#"{"name":"aim","steps":[{"id":"aimed","op":"press","target":"button","within":{"x":1,"y":2}}]}"#,
    )
    .unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &protocol_registry()).unwrap();
    let host = TestHost::spawn(PressHost::default(), None).unwrap();
    let mut client = host.client();
    let error = run_plan(&mut client, &plan, &RunOptions::new()).unwrap_err();
    assert!(error.to_string().starts_with("unsupported_op:"), "{error}");
}

#[test]
fn modern_host_missing_a_legacy_fallback_op_is_preflighted() {
    let recipe = Recipe::from_json(
        r#"{"name":"plain","steps":[{"id":"plain","op":"press","target":"button"}]}"#,
    )
    .unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &protocol_registry()).unwrap();
    let host = TestHost::spawn(NoPressHost(PressHost::default()), None).unwrap();
    let state = host.host();
    let mut client = host.client();
    let error = run_plan(&mut client, &plan, &RunOptions::new()).unwrap_err();
    assert!(
        error.to_string().contains("before the first wave"),
        "{error}"
    );
    assert!(error.to_string().contains("plain"), "{error}");
    assert_eq!(state.lock().unwrap().0.dispatched, 0);
}

#[test]
fn hello_transport_failure_is_not_a_capability_refusal() {
    let recipe = Recipe::from_json(
        r#"{"name":"aim","steps":[{"id":"aimed","op":"press","target":"button","within":{"x":1,"y":2}}]}"#,
    )
    .unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &protocol_registry()).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let mut client =
        gpui_agent::AgentClient::connect(addr).with_timeout(std::time::Duration::from_millis(100));
    let error = run_plan(&mut client, &plan, &RunOptions::new()).unwrap_err();
    assert!(!error.to_string().starts_with("unsupported_op:"), "{error}");
    assert!(
        !error.to_string().contains("unsupported recipe steps"),
        "{error}"
    );
}

#[test]
fn failed_preflight_hello_keeps_the_transport_error_code() {
    let recipe = Recipe::from_json(
        r#"{"name":"aim","steps":[{"id":"aimed","op":"press","target":"button","within":{"x":1,"y":2}}]}"#,
    )
    .unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &protocol_registry()).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let mut client =
        gpui_agent::AgentClient::connect(addr).with_timeout(std::time::Duration::from_millis(100));
    let error = run_plan(&mut client, &plan, &RunOptions::new()).unwrap_err();
    assert_eq!(
        gpui_agent::error_code_of(&error.to_string()),
        gpui_agent::error_code::CONNECT_FAILED,
        "{error}"
    );
}
