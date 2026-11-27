//! Ordering and parameter edge cases for `.node` references.
#![expect(
    clippy::unwrap_used,
    reason = "a failed setup/assert aborts the test anyway"
)]

use std::collections::BTreeMap;

use gpui_agent::protocol::Op;
use gpui_agent_recipe::{Recipe, compile_plan, todo_registry};

fn ids(json: &str) -> Vec<String> {
    let recipe = Recipe::from_json(json).unwrap();
    compile_plan(&recipe, &BTreeMap::new(), &todo_registry())
        .unwrap()
        .steps
        .into_iter()
        .map(|step| step.id)
        .collect()
}

#[test]
fn one_node_reference_keeps_implicit_file_order() {
    let got = ids(r#"{"name":"x","steps":[
      {"id":"s1","op":"scroll_until","target":"list","until":"row","dy":1},
      {"id":"s2","op":"press","target":"$s1.node"},
      {"id":"s3","op":"click","target":"delete"},
      {"id":"s4","op":"click","target":"confirm"}
    ]}"#);
    assert_eq!(got, ["s1", "s2", "s3", "s4"]);
}

#[test]
fn twelve_steps_keep_file_order_not_text_order() {
    let mut steps = vec![
        r#"{"id":"s1","op":"scroll_until","target":"list","until":"row","dy":1}"#.to_string(),
        r#"{"id":"s2","op":"press","target":"$s1.node"}"#.to_string(),
    ];
    for n in 3..=12 {
        steps.push(format!(r#"{{"id":"s{n}","op":"click","target":"b{n}"}}"#));
    }
    let got = ids(&format!(r#"{{"name":"x","steps":[{}]}}"#, steps.join(",")));
    assert_eq!(got, (1..=12).map(|n| format!("s{n}")).collect::<Vec<_>>());
}

#[test]
fn forward_node_reference_is_refused_in_an_implicit_recipe() {
    let recipe = Recipe::from_json(
        r#"{"name":"x","steps":[
      {"id":"s1","op":"press","target":"$s2.node"},
      {"id":"s2","op":"scroll_until","target":"list","until":"row","dy":1}
    ]}"#,
    )
    .unwrap();
    let error = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap_err();
    assert!(error.to_string().contains("later step"), "{error}");
}

#[test]
fn explicit_needs_still_get_the_implied_node_edge() {
    let recipe = Recipe::from_json(
        r#"{"name":"x","steps":[
      {"id":"s1","op":"scroll_until","target":"list","until":"row","dy":1},
      {"id":"s2","op":"click","target":"other","needs":["s1"]},
      {"id":"s3","op":"press","target":"$s1.node"}
    ]}"#,
    )
    .unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    let s3 = plan.steps.iter().find(|step| step.id == "s3").unwrap();
    assert!(s3.needs.iter().any(|need| need == "s1"), "{s3:?}");
}

#[test]
fn param_named_like_node_suffix_is_not_a_step_reference() {
    let recipe = Recipe::from_json(r#"{"name":"x","params":[{"name":"host"}],"steps":[{"id":"s1","op":"type","target":"url","text":"https://$host.nodejs.org"}]}"#).unwrap();
    let mut set = BTreeMap::new();
    set.insert("host".to_string(), "example".to_string());
    let plan = compile_plan(&recipe, &set, &todo_registry()).unwrap();
    let Op::Type { text, .. } = &plan.steps[0].op else {
        panic!("not type")
    };
    assert_eq!(text, "https://example.nodejs.org");
}

#[test]
fn step_id_that_is_also_a_param_is_refused() {
    let recipe = Recipe::from_json(
        r#"{"name":"x","params":[{"name":"s1"}],"steps":[
      {"id":"s1","op":"scroll_until","target":"list","until":"row","dy":1},
      {"id":"s2","op":"press","target":"$s1.node"}
    ]}"#,
    )
    .unwrap();
    let error = compile_plan(
        &recipe,
        &BTreeMap::from([("s1".to_string(), "x".to_string())]),
        &todo_registry(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("both a step id and a param"),
        "{error}"
    );
}
