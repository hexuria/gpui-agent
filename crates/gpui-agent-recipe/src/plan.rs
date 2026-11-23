use std::collections::{BTreeMap, BTreeSet, VecDeque};

use gpui_agent::protocol::Op;
use serde::Serialize;

use crate::error::{RecipeError, RecipeErrorCode, err};
use crate::recipe::{
    Recipe, RecipeStep, apply_params, check_invoke_args, coerce_invoke_args, validate_recipe,
};
use crate::registry::Registry;
use crate::schema::{Effect, SchemaKind};

/// One scheduled recipe step with its wave placement and annotations.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct PlannedStep {
    /// Step id from the recipe.
    pub id: String,
    /// The wire op to send.
    pub op: Op,
    /// Step ids this step waits for.
    pub needs: Vec<String>,
    /// Capture a PNG after this step when `--screenshot-dir` / `--screenshot-flagged`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub screenshot: bool,
    /// Effects the op declares (Read/Write/Exit/…).
    pub effects: Vec<Effect>,
    /// Registry schema the op resolved to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Schema-declared idempotency flag.
    pub idempotent: bool,
}

/// Version of the [`Plan`] JSON shape (`v`).
pub const PLAN_FORMAT_VERSION: u32 = 1;

/// A compiled recipe: ordered steps plus dependency waves.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Plan {
    /// [`PLAN_FORMAT_VERSION`].
    pub v: u32,
    /// Recipe name (bound name when params applied).
    pub name: String,
    /// Optional app the recipe targets.
    pub app: Option<String>,
    /// Steps in schedule order.
    pub steps: Vec<PlannedStep>,
    /// Waves of independent step ids. All-Read waves may
    /// [`rpc_pipeline`](gpui_agent::client::AgentClient::rpc_pipeline);
    /// Write / Exit / mixed waves stay sequential fail-fast. A failed
    /// wave does not start the next one.
    pub waves: Vec<Vec<String>>,
    /// Union of all step effects.
    pub effects: Vec<Effect>,
    /// True when the plan includes an Exit effect — needs `--yes`.
    pub requires_yes: bool,
    /// Content hash of the bound plan (name, step ids, ops, needs):
    /// `fnv1a64:<16 hex>`. Identical on every platform, process, and
    /// toolchain for the same crate version; a correlation id, not a
    /// security digest.
    pub fingerprint: String,
}

/// Whether a recipe's wave schedule changes when steps are reversed.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct OrderCheck {
    /// Forward and reverse waves differ — the recipe is order-dependent.
    pub order_dependent: bool,
    /// Waves from the recipe as written.
    pub forward_waves: Vec<Vec<String>>,
    /// Waves from the reversed recipe.
    pub reverse_waves: Vec<Vec<String>>,
}

/// Validate `recipe`, apply `--set` params, and schedule its steps
/// into dependency waves against `registry` schemas.
///
/// # Errors
/// Any [`validate_recipe`] violation; [`RecipeErrorCode::MissingParam`] /
/// [`RecipeErrorCode::UnknownParam`] when `set` does not match the
/// declared params; [`RecipeErrorCode::InvalidParamValue`] /
/// [`RecipeErrorCode::InvalidArgs`] when a bound invoke arg does not fit
/// its schema type.
pub fn compile_plan(
    recipe: &Recipe,
    set: &BTreeMap<String, String>,
    registry: &Registry,
) -> Result<Plan, RecipeError> {
    validate_recipe(recipe, registry)?;
    let mut bound = apply_params(recipe, set)?;
    for (step, unbound) in bound.steps.iter_mut().zip(&recipe.steps) {
        let (Op::Invoke { name, args }, Op::Invoke { args: raw, .. }) = (&mut step.op, &unbound.op)
        else {
            continue;
        };
        let Some(schema) = registry.get(name) else {
            continue;
        };
        coerce_invoke_args(schema, raw, args)
            .map_err(|m| err(RecipeErrorCode::InvalidParamValue, m).at_step(&step.id))?;
        check_invoke_args(schema, args, true)
            .map_err(|m| err(RecipeErrorCode::InvalidArgs, m).at_step(&step.id))?;
    }
    let name = bound.name.as_str();
    let app = bound.app.clone();
    let steps = bound.steps.as_slice();
    let implicit = steps.iter().all(|step| step.needs.is_empty());
    let (ordered, waves) =
        schedule(steps).map_err(|message| err(RecipeErrorCode::Cycle, message))?;
    let mut planned = Vec::with_capacity(ordered.len());
    let mut effects = BTreeSet::new();
    for &i in &ordered {
        let step = &steps[i];
        let (schema_name, step_effects, idempotent) = annotate(&step.op, registry);
        for effect in &step_effects {
            effects.insert(*effect);
        }
        let needs = if implicit {
            if i == 0 {
                Vec::new()
            } else {
                vec![steps[i - 1].id.clone()]
            }
        } else {
            step.needs.clone()
        };
        planned.push(PlannedStep {
            id: step.id.clone(),
            op: step.op.clone(),
            needs,
            screenshot: step.screenshot,
            effects: step_effects,
            schema: schema_name,
            idempotent,
        });
    }
    let requires_yes = effects.contains(&Effect::Exit);
    let fingerprint = fingerprint_plan(name, &planned);
    Ok(Plan {
        v: PLAN_FORMAT_VERSION,
        name: name.to_string(),
        app,
        steps: planned,
        waves,
        effects: effects.into_iter().collect(),
        requires_yes,
        fingerprint,
    })
}

/// Compile the recipe and its reversal; the result flags
/// order-dependence when the wave sets differ.
///
/// # Errors
/// As [`compile_plan`].
pub fn order_check(
    recipe: &Recipe,
    set: &BTreeMap<String, String>,
    registry: &Registry,
) -> Result<OrderCheck, RecipeError> {
    let forward = compile_plan(recipe, set, registry)?;
    let mut reversed = recipe.clone();
    reversed.steps.reverse();
    let reverse = compile_plan(&reversed, set, registry)?;
    let forward_sets: Vec<BTreeSet<String>> = forward
        .waves
        .iter()
        .map(|w| w.iter().cloned().collect())
        .collect();
    let reverse_sets: Vec<BTreeSet<String>> = reverse
        .waves
        .iter()
        .map(|w| w.iter().cloned().collect())
        .collect();
    Ok(OrderCheck {
        order_dependent: forward_sets != reverse_sets,
        forward_waves: forward.waves,
        reverse_waves: reverse.waves,
    })
}

fn annotate(op: &Op, registry: &Registry) -> (Option<String>, Vec<Effect>, bool) {
    match op {
        Op::Hello => lookup("hello", registry),
        Op::Wait { .. } => lookup("wait", registry),
        Op::WaitUntil { .. } => lookup("wait_until", registry),
        Op::Snapshot { .. } => lookup("snapshot", registry),
        Op::Revision => lookup("revision", registry),
        Op::Click { .. } => lookup("click", registry),
        Op::Type { .. } => lookup("type", registry),
        Op::SetValue { .. } => lookup("set_value", registry),
        Op::Key { .. } => lookup("key", registry),
        Op::Keybinding { binding, .. } => {
            let (name, effects, idempotent) = lookup("keybinding", registry);
            if gpui_agent::is_quit_binding(binding) {
                (name, vec![Effect::Exit], idempotent)
            } else {
                (name, effects, idempotent)
            }
        }
        Op::Keybindings => lookup("keybindings", registry),
        Op::Events { .. } => lookup("events", registry),
        Op::Assert { .. } => lookup("assert", registry),
        Op::Screenshot { .. } => lookup("screenshot", registry),
        Op::Blob { .. } => lookup("blob", registry),
        Op::Press { .. }
        | Op::Scroll { .. }
        | Op::Hover { .. }
        | Op::Drag { .. }
        | Op::Swipe { .. }
        | Op::LongPress { .. }
        | Op::Pinch { .. }
        | Op::Viewport { .. }
        | Op::Paste { .. }
        | Op::System { .. } => lookup(op.feature().unwrap_or("invoke"), registry),
        Op::Shutdown => lookup("shutdown", registry),
        Op::Invoke { name, args } => {
            if let Some(schema) = registry.get(name) {
                if matches!(schema.kind, SchemaKind::Invoke) {
                    return (
                        Some(schema.name.clone()),
                        schema.effects.clone(),
                        schema.idempotent,
                    );
                }
            }
            let _ = args;
            lookup("invoke", registry)
        }
    }
}

fn lookup(name: &str, registry: &Registry) -> (Option<String>, Vec<Effect>, bool) {
    match registry.get(name) {
        Some(schema) => (
            Some(schema.name.clone()),
            schema.effects.clone(),
            schema.idempotent,
        ),
        None => (None, vec![Effect::Write], false),
    }
}

pub(crate) fn schedule(steps: &[RecipeStep]) -> Result<(Vec<usize>, Vec<Vec<String>>), String> {
    if steps.iter().all(|step| step.needs.is_empty()) {
        let ordered: Vec<usize> = (0..steps.len()).collect();
        let waves = steps.iter().map(|step| vec![step.id.clone()]).collect();
        return Ok((ordered, waves));
    }

    let index: BTreeMap<&str, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.as_str(), i))
        .collect();
    let mut indegree = vec![0usize; steps.len()];
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); steps.len()];
    for (i, step) in steps.iter().enumerate() {
        for need in &step.needs {
            let j = *index
                .get(need.as_str())
                .ok_or_else(|| format!("unknown need `{need}`"))?;
            adj[j].push(i);
            indegree[i] += 1;
        }
    }

    let mut ready: VecDeque<usize> = indegree
        .iter()
        .enumerate()
        .filter(|(_, d)| **d == 0)
        .map(|(i, _)| i)
        .collect();
    // Deterministic: sort each wave by step id.
    let mut ordered = Vec::with_capacity(steps.len());
    let mut waves = Vec::new();
    let mut remaining = steps.len();
    while !ready.is_empty() {
        let mut wave: Vec<usize> = ready.drain(..).collect();
        wave.sort_by_key(|&i| steps[i].id.as_str());
        waves.push(wave.iter().map(|&i| steps[i].id.clone()).collect());
        let mut next = Vec::new();
        for i in wave {
            ordered.push(i);
            remaining -= 1;
            for &j in &adj[i] {
                indegree[j] -= 1;
                if indegree[j] == 0 {
                    next.push(j);
                }
            }
        }
        ready.extend(next);
    }
    if remaining != 0 {
        return Err("recipe has a dependency cycle".into());
    }
    Ok((ordered, waves))
}

fn fingerprint_plan(name: &str, steps: &[PlannedStep]) -> String {
    let payload = serde_json::json!({
        "name": name,
        "steps": steps.iter().map(|s| serde_json::json!({
            "id": s.id,
            "op": s.op,
            "needs": s.needs,
        })).collect::<Vec<_>>(),
    });
    let mut canonical = String::new();
    write_canonical(&payload, &mut canonical);
    // FNV-1a 64: fixed constants, so the value never depends on the
    // toolchain's `DefaultHasher`.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in canonical.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("fnv1a64:{hash:016x}")
}

/// Compact JSON with object keys sorted, so the text does not depend on
/// `serde_json`'s map ordering feature. Unset optional op fields are not
/// serialized at all; an explicit `null` (e.g. in invoke args) is kept.
fn write_canonical(value: &serde_json::Value, out: &mut String) {
    use serde_json::Value;
    match value {
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut members: Vec<_> = map.iter().collect();
            members.sort_by(|a, b| a.0.cmp(b.0));
            out.push('{');
            for (i, (key, member)) in members.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(member, out);
            }
            out.push('}');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_keeps_explicit_nulls() {
        let render = |value: serde_json::Value| {
            let mut out = String::new();
            write_canonical(&value, &mut out);
            out
        };
        assert_eq!(
            render(serde_json::json!({"b": {"x": null}, "a": 1})),
            r#"{"a":1,"b":{"x":null}}"#
        );
        assert_ne!(
            render(serde_json::json!({"payload": {"x": null}})),
            render(serde_json::json!({"payload": {}}))
        );
    }
    use crate::recipe::parse_wants;
    use crate::registry::todo_registry;

    #[test]
    fn linear_wants_are_order_dependent() {
        let recipe = parse_wants(
            "set-value todo-input Hi\nclick todo-add\nassert todo-item-1 name=Hi",
            "lin",
        )
        .unwrap();
        let check = order_check(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
        assert!(check.order_dependent);
    }

    #[test]
    fn explicit_dag_is_stable_under_reverse() {
        let recipe = crate::recipe::Recipe::from_json(
            r#"{
            "name": "dag",
            "steps": [
                {"id": "a", "op": "hello"},
                {"id": "b", "op": "snapshot"},
                {"id": "c", "op": "assert", "target": "todo-window", "needs": ["a", "b"]}
            ]
        }"#,
        )
        .unwrap();
        let check = order_check(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
        assert!(!check.order_dependent, "{check:?}");
        assert_eq!(check.forward_waves.len(), 2);
    }

    #[test]
    fn cycle_is_rejected() {
        let recipe = crate::recipe::Recipe::from_json(
            r#"{
            "name": "cycle",
            "steps": [
                {"id": "a", "op": "hello", "needs": ["b"]},
                {"id": "b", "op": "hello", "needs": ["a"]}
            ]
        }"#,
        )
        .unwrap();
        let err = compile_plan(&recipe, &BTreeMap::new(), &todo_registry())
            .unwrap_err()
            .to_string();
        assert!(err.contains("cycle"), "{err}");
    }

    #[test]
    fn shutdown_requires_yes() {
        let recipe = parse_wants("hello\nshutdown", "bye").unwrap();
        let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
        assert!(plan.requires_yes);
        assert!(plan.effects.contains(&Effect::Exit));
    }

    #[test]
    fn quit_keybinding_requires_yes() {
        let recipe =
            parse_wants("keybinding --id app.quit --scope global --confirm", "quit").unwrap();
        let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
        assert!(plan.requires_yes);
        assert!(plan.effects.contains(&Effect::Exit));
    }

    #[test]
    fn compile_plan_missing_params_fail_closed() {
        let recipe = crate::recipe::Recipe::from_json(
            r#"{
            "name": "p",
            "params": ["need_me"],
            "steps": [{"id": "a", "op": "set_value", "target": "todo-input", "value": "$need_me"}]
        }"#,
        )
        .unwrap();
        let err = compile_plan(&recipe, &BTreeMap::new(), &todo_registry())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("need_me") || err.contains("missing --set"),
            "{err}"
        );
    }

    #[test]
    fn compile_plan_copies_screenshot_flag() {
        let recipe = crate::recipe::Recipe::from_json(
            r#"{
            "name": "shots",
            "steps": [
                {"id": "wait", "op": "wait"},
                {"id": "snap", "op": "screenshot", "path": "out.png", "screenshot": true}
            ]
        }"#,
        )
        .unwrap();
        let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
        assert!(!plan.steps[0].screenshot);
        assert!(plan.steps[1].screenshot);
        assert_eq!(plan.steps[1].schema.as_deref(), Some("screenshot"));
        assert!(plan.steps[1].idempotent);
    }

    #[test]
    fn implicit_linear_needs_are_chained() {
        let recipe = parse_wants("hello\nsnapshot", "lin").unwrap();
        let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
        assert_eq!(plan.steps[0].needs, Vec::<String>::new());
        assert_eq!(plan.steps[1].needs, vec!["s1"]);
        assert_eq!(plan.waves.len(), 2);
    }

    #[test]
    fn compile_plan_unknown_invoke_fail_closed() {
        let recipe = crate::recipe::Recipe::from_json(
            r#"{"name":"p","steps":[{"id":"a","op":"invoke","name":"not.a.schema"}]}"#,
        )
        .unwrap();
        let err = compile_plan(&recipe, &BTreeMap::new(), &todo_registry())
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown invoke"), "{err}");
    }

    #[test]
    fn fingerprint_is_a_fixed_function_of_the_bound_plan() {
        let recipe = parse_wants("invoke todo.add title=$t\nsnapshot", "fp").unwrap();
        let set = |v: &str| BTreeMap::from([("t".to_string(), v.to_string())]);
        let plan = compile_plan(&recipe, &set("Milk"), &todo_registry()).unwrap();
        // Golden value: changing it breaks receipts stored by callers.
        assert_eq!(plan.fingerprint, "fnv1a64:d0ee89517c86c524");
        assert_eq!(plan.v, PLAN_FORMAT_VERSION);
        let other = compile_plan(&recipe, &set("Eggs"), &todo_registry()).unwrap();
        assert_ne!(plan.fingerprint, other.fingerprint);
    }

    #[test]
    fn placeholder_args_take_the_schema_type_when_bound() {
        let recipe = parse_wants("invoke todo.toggle id=$id", "ty").unwrap();
        let set = |v: &str| BTreeMap::from([("id".to_string(), v.to_string())]);
        let plan = compile_plan(&recipe, &set("3"), &todo_registry()).unwrap();
        let Op::Invoke { args, .. } = &plan.steps[0].op else {
            panic!("invoke");
        };
        assert_eq!(args["id"], 3);
        let error = compile_plan(&recipe, &set("x"), &todo_registry()).unwrap_err();
        assert_eq!(error.code, RecipeErrorCode::InvalidParamValue);
        assert_eq!(error.step.as_deref(), Some("s1"));
    }
}
