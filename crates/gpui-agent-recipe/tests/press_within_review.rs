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
