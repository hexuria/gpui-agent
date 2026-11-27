//! Validation of recipes after `$param` binding.

use std::collections::BTreeMap;

use gpui_agent_recipe::{Recipe, compile_plan, todo_registry};

fn set(values: &[(&str, &str)]) -> BTreeMap<String, String> {
    values
        .iter()
        .map(|(key, value)| ((*key).into(), (*value).into()))
        .collect()
}

#[test]
fn escaped_dollar_and_dollar_in_bound_values_survive_validation() {
    let recipe = Recipe::from_json(
        r#"{"name":"bound","params":["pw"],"steps":[
            {"id":"literal","op":"type","target":"$$literal","text":"x"},
            {"id":"value","op":"type","target":"field","text":"$pw","needs":["literal"]}
        ]}"#,
    )
    .unwrap();
    compile_plan(&recipe, &set(&[("pw", "a$bc")]), &todo_registry()).expect("bound plan");
}

#[test]
fn undeclared_placeholders_are_still_refused_before_binding() {
    let recipe = Recipe::from_json(
        r#"{"name":"bad","steps":[{"id":"a","op":"type","target":"field","text":"$x"}]}"#,
    )
    .unwrap();
    let err = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap_err();
    assert!(err.to_string().contains("$x"), "{err}");
}

#[test]
fn scroll_until_numeric_param_is_not_seen_as_zero_delta() {
    let recipe = Recipe::from_json(
        r#"{"name":"scroll","params":["d"],"steps":[{"id":"s","op":"scroll_until","target":"list","until":"row","dy":"$d","max_steps":2}]}"#,
    )
    .unwrap();
    let plan = compile_plan(&recipe, &set(&[("d", "120")]), &todo_registry()).expect("bound plan");
    let gpui_agent::protocol::Op::Scroll { dy, .. } = plan.steps[0].op else {
        panic!("not scroll");
    };
    assert_eq!(dy, 120.0);
    let err = compile_plan(&recipe, &set(&[("d", "0")]), &todo_registry()).unwrap_err();
    assert!(err.to_string().contains("non-zero"), "{err}");
}
