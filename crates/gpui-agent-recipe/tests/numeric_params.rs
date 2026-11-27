//! Numeric `$param` binding in recipe fields.

use std::collections::BTreeMap;

use gpui_agent::protocol::Op;
use gpui_agent_recipe::{Recipe, compile_plan, todo_registry};

fn recipe_with(step: &str) -> Recipe {
    let params = if step.contains("key_delay_ms") {
        r#""delay""#
    } else if step.contains("max_steps") {
        r#""steps""#
    } else if step.contains('y') {
        r#""x","y","hold""#
    } else {
        r#""x""#
    };
    Recipe::from_json(&format!(
        r#"{{"name":"n","params":[{params}],"steps":[{step}]}}"#
    ))
    .expect("recipe parses")
}

fn set(values: &[(&str, &str)]) -> BTreeMap<String, String> {
    values
        .iter()
        .map(|(key, value)| ((*key).into(), (*value).into()))
        .collect()
}

#[test]
fn numeric_params_bind_before_validation() {
    let recipe = recipe_with(
        r#"{"id":"a","op":"long_press","target":"field","x":"$x","y":"$y","duration_ms":"$hold"}"#,
    );
    let plan = compile_plan(
        &recipe,
        &set(&[("x", "12.5"), ("y", "8"), ("hold", "750")]),
        &todo_registry(),
    )
    .unwrap();
    let Op::LongPress {
        x, y, duration_ms, ..
    } = plan.steps[0].op
    else {
        panic!("not long press");
    };
    assert_eq!(x, Some(12.5));
    assert_eq!(y, Some(8.0));
    assert_eq!(duration_ms, Some(750));
}

#[test]
fn unbound_numeric_param_is_refused_naming_it() {
    let recipe =
        recipe_with(r#"{"id":"a","op":"long_press","target":"field","x":"$x","duration_ms":1}"#);
    let err = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap_err();
    assert!(err.to_string().contains('x'), "{err}");
}

#[test]
fn non_number_numeric_param_is_refused() {
    let recipe =
        recipe_with(r#"{"id":"a","op":"long_press","target":"field","x":"$x","duration_ms":1}"#);
    let err = compile_plan(&recipe, &set(&[("x", "abc")]), &todo_registry()).unwrap_err();
    assert!(err.to_string().contains("abc"), "{err}");
}

#[test]
fn bound_values_are_still_validated() {
    for (json, values, wanted) in [
        (
            r#"{"id":"a","op":"type","target":"t","text":"x","key_delay_ms":"$delay"}"#,
            vec![("delay", "0")],
            "key_delay_ms",
        ),
        (
            r#"{"id":"a","op":"scroll_until","target":"list","until":"row","dy":1,"max_steps":"$steps"}"#,
            vec![("steps", "51")],
            "max_steps",
        ),
        (
            r#"{"id":"a","op":"long_press","target":"field","x":"$x","duration_ms":1}"#,
            vec![("x", "NaN")],
            "finite",
        ),
    ] {
        let recipe = recipe_with(json);
        let err = compile_plan(&recipe, &set(&values), &todo_registry()).unwrap_err();
        assert!(err.to_string().contains(wanted), "{err}");
    }
}
