use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::hash::{Hash, Hasher};

use gpui_agent::protocol::Op;
use serde::Serialize;

use crate::recipe::{Recipe, RecipeStep, apply_params, validate_recipe};
use crate::registry::Registry;
use crate::schema::{Effect, SchemaKind};

#[derive(Debug, Clone, Serialize)]
pub struct PlannedStep {
    pub id: String,
    pub op: Op,
    pub needs: Vec<String>,
    /// Capture a PNG after this step when `--screenshot-dir` / `--screenshot-flagged`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub screenshot: bool,
    pub effects: Vec<Effect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub idempotent: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub name: String,
    pub app: Option<String>,
    pub steps: Vec<PlannedStep>,
    /// Parallel-ready waves (ids). Execution is still sequential on one
    /// TCP session; waves document what *could* run together.
    pub waves: Vec<Vec<String>>,
    pub effects: Vec<Effect>,
    pub requires_yes: bool,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrderCheck {
    pub order_dependent: bool,
    pub forward_waves: Vec<Vec<String>>,
    pub reverse_waves: Vec<Vec<String>>,
}

pub fn compile_plan(
    recipe: &Recipe,
    set: &BTreeMap<String, String>,
    registry: &Registry,
) -> Result<Plan, String> {
    validate_recipe(recipe, registry)?;
    let bound = if recipe.params.is_empty() {
        None
    } else {
        Some(apply_params(recipe, set)?)
    };
    let name = bound
        .as_ref()
        .map(|b| b.name.as_str())
        .unwrap_or(&recipe.name);
    let app = bound
        .as_ref()
        .map(|b| b.app.clone())
        .unwrap_or_else(|| recipe.app.clone());
    let steps = match &bound {
        Some(bound) => bound.steps.as_slice(),
        None => recipe.steps.as_slice(),
    };
    let implicit = steps.iter().all(|step| step.needs.is_empty());
    let (ordered, waves) = schedule(steps)?;
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
        name: name.to_string(),
        app,
        steps: planned,
        waves,
        effects: effects.into_iter().collect(),
        requires_yes,
        fingerprint,
    })
}

pub fn order_check(
    recipe: &Recipe,
    set: &BTreeMap<String, String>,
    registry: &Registry,
) -> Result<OrderCheck, String> {
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
        Op::Snapshot => lookup("snapshot", registry),
        Op::Click { .. } => lookup("click", registry),
        Op::Type { .. } => lookup("type", registry),
        Op::SetValue { .. } => lookup("set_value", registry),
        Op::Key { .. } => lookup("key", registry),
        Op::Assert { .. } => lookup("assert", registry),
        Op::Screenshot { .. } => lookup("screenshot", registry),
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

fn schedule(steps: &[RecipeStep]) -> Result<(Vec<usize>, Vec<Vec<String>>), String> {
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
    let bytes = serde_json::to_vec(&payload).unwrap_or_default();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let err = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap_err();
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
    fn compile_plan_missing_params_fail_closed() {
        let recipe = crate::recipe::Recipe::from_json(
            r#"{
            "name": "p",
            "params": ["need_me"],
            "steps": [{"id": "a", "op": "set_value", "target": "todo-input", "value": "$need_me"}]
        }"#,
        )
        .unwrap();
        let err = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap_err();
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
        assert!(plan.steps[0].needs.is_empty());
        assert_eq!(plan.steps[1].needs, vec!["s1"]);
        assert_eq!(plan.waves.len(), 2);
    }

    #[test]
    fn compile_plan_unknown_invoke_fail_closed() {
        let recipe = crate::recipe::Recipe::from_json(
            r#"{"name":"p","steps":[{"id":"a","op":"invoke","name":"not.a.schema"}]}"#,
        )
        .unwrap();
        let err = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap_err();
        assert!(err.contains("unknown invoke"), "{err}");
    }
}
