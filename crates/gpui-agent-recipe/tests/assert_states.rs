//! Asserting exact node states from recipes.

use std::collections::BTreeMap;

use gpui_agent::protocol::Op;
use gpui_agent_recipe::plan::compile_plan;
use gpui_agent_recipe::recipe::{Recipe, parse_wants};
use gpui_agent_recipe::todo_registry;

fn assert_spec(recipe: &Recipe) -> gpui_agent::protocol::AssertSpec {
    let plan = compile_plan(recipe, &BTreeMap::new(), &todo_registry()).expect("plan");
    let Op::Assert { spec } = &plan.steps[0].op else {
        panic!("not assert");
    };
    spec.clone()
}

#[test]
fn json_and_wants_parse_state_and_states() {
    let json = Recipe::from_json(
        r#"{"name":"s","steps":[{"id":"a","op":"assert","target":"field","state":"masked","states":["selection: 0-9","field: amount"]}]}"#,
    )
    .unwrap();
    let spec = assert_spec(&json);
    assert_eq!(spec.state.as_deref(), Some("masked"));
    assert_eq!(spec.states, ["selection: 0-9", "field: amount"]);

    let wants = parse_wants(r#"assert field state="selection: 0-9""#, "s").unwrap();
    let spec = assert_spec(&wants);
    assert_eq!(spec.state.as_deref(), Some("selection: 0-9"));
}

#[test]
fn wait_until_uses_the_same_state_fields() {
    let recipe = Recipe::from_json(
        r#"{"name":"s","steps":[{"id":"a","op":"wait_until","target":"field","states":["masked"],"timeout_ms":100}]}"#,
    )
    .unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    let Op::WaitUntil { spec, .. } = &plan.steps[0].op else {
        panic!("not wait_until");
    };
    assert_eq!(spec.states, ["masked"]);
}

#[test]
fn absent_state_fields_do_not_change_the_plan_fingerprint() {
    let plain = Recipe::from_json(
        r#"{"name":"s","steps":[{"id":"a","op":"assert","target":"field","name":"Amount"}]}"#,
    )
    .unwrap();
    let empty = Recipe::from_json(
        r#"{"name":"s","steps":[{"id":"a","op":"assert","target":"field","name":"Amount","states":[]}]}"#,
    )
    .unwrap();
    let a = compile_plan(&plain, &BTreeMap::new(), &todo_registry()).unwrap();
    let b = compile_plan(&empty, &BTreeMap::new(), &todo_registry()).unwrap();
    assert_eq!(a.fingerprint, b.fingerprint);
}
