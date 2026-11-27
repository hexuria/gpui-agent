//! `schemas/recipe.v1.schema.json` against the parser, on whole documents.
//!
//! The schema is what an editor checks a recipe file with; `recipe
//! validate` is the authority. For every document tried here:
//!
//! - what `recipe validate` accepts, the schema accepts (an editor never
//!   rejects a good recipe);
//! - what the schema accepts, the parser reads (the schema never admits a
//!   shape the parser refuses);
//! - on a step's own fields the two agree exactly.
//!
//! Between "parses" and "validates" the schema may go either way: it states
//! some rules the parser leaves to validation (`id` not empty, `needs`
//! without repeats) and cannot state others (unknown `needs`, cycles).

use std::path::Path;

use gpui_agent_recipe::{Recipe, todo_registry, validate_recipe};
use serde_json::{Map, Value, json};

/// The schema, or `None` outside the workspace: the published crate does
/// not carry the file. In the workspace a missing file fails the test.
fn schema() -> Option<Value> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if !workspace.join("Cargo.toml").exists() {
        return None;
    }
    let text = std::fs::read_to_string(workspace.join("schemas/recipe.v1.schema.json"))
        .expect("the workspace has schemas/recipe.v1.schema.json");
    Some(serde_json::from_str(&text).expect("the schema is JSON"))
}

/// Values a field is set to: every JSON type, the numbers at the bounds the
/// parser has, and nested shapes.
fn probes() -> Vec<Value> {
    vec![
        Value::Null,
        json!(""),
        json!("\u{1}no-such-name"),
        json!(true),
        json!(0),
        json!(1),
        json!(-1),
        json!(1.5),
        json!(u32::MAX),
        json!(u64::from(u32::MAX) + 1),
        json!(u64::MAX),
        json!([]),
        json!(["s"]),
        json!([1]),
        json!([null]),
        json!({}),
        json!({"x": 1}),
    ]
}

/// The smallest step a schema variant allows, with an op the todo
/// registry knows, so the recipe around it validates.
fn smallest_step(variant: &Value) -> Map<String, Value> {
    let op = variant["properties"]["op"]["const"]
        .as_str()
        .expect("each step variant names one op");
    let mut step = Map::new();
    for key in variant["required"].as_array().expect("required list") {
        let key = key.as_str().expect("required key");
        let sample = match (op, key) {
            (_, "op") => json!(op),
            (_, "id") => json!("a"),
            ("invoke", "name") => json!("todo.list"),
            ("keybinding", "scope") => json!("global"),
            ("swipe", "direction") => json!("up"),
            ("system", "action") => json!("back"),
            (_, "timeout_ms" | "index" | "scale") => json!(1),
            _ => json!("x"),
        };
        step.insert(key.into(), sample);
    }
    if op == "scroll_until" {
        step.insert("dy".into(), json!(1));
    }
    step
}

fn recipe_of(step: Map<String, Value>) -> Value {
    json!({"name": "r", "steps": [Value::Object(step)]})
}

/// Every document reached from `doc` by changing one place: a value
/// replaced by each probe, a key removed, an unknown key added, an array's
/// first item repeated.
fn one_change_from(doc: &Value) -> Vec<(String, Value)> {
    fn walk(node: &Value, path: &str, out: &mut Vec<String>) {
        out.push(path.to_string());
        match node {
            Value::Object(map) => {
                for (key, child) in map {
                    walk(child, &format!("{path}/{}", key.replace('/', "~1")), out);
                }
            }
            Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    walk(child, &format!("{path}/{index}"), out);
                }
            }
            _ => {}
        }
    }
    let mut paths = Vec::new();
    walk(doc, "", &mut paths);
    let mut out = Vec::new();
    for path in paths {
        for probe in probes() {
            let mut changed = doc.clone();
            *changed.pointer_mut(&path).expect("path from the walk") = probe.clone();
            out.push((format!("{path} = {probe}"), changed));
        }
        match doc.pointer(&path).expect("path from the walk") {
            Value::Object(map) => {
                for key in map.keys() {
                    let mut changed = doc.clone();
                    let object = changed.pointer_mut(&path).and_then(Value::as_object_mut);
                    object.expect("an object").remove(key);
                    out.push((format!("{path}: without `{key}`"), changed));
                }
                let mut changed = doc.clone();
                let object = changed.pointer_mut(&path).and_then(Value::as_object_mut);
                object.expect("an object").insert("zz".into(), json!(1));
                out.push((format!("{path}: with unknown `zz`"), changed));
            }
            Value::Array(items) if !items.is_empty() => {
                let mut changed = doc.clone();
                let array = changed.pointer_mut(&path).and_then(Value::as_array_mut);
                array.expect("an array").push(items[0].clone());
                out.push((format!("{path}: first item repeated"), changed));
            }
            _ => {}
        }
    }
    out
}

/// A recipe that uses every part of the format and validates against the
/// todo registry.
fn full_recipe() -> Value {
    json!({
        "$schema": "../../schemas/recipe.v1.schema.json",
        "v": 1,
        "name": "full",
        "app": "todo",
        "params": [
            "title",
            {
                "name": "count", "type": "integer", "required": false, "default": 2,
                "enum": [1, 2, 3], "description": "How many",
            },
            {"name": "kind", "pattern": "^[a-z]+$", "default": "task"},
            {"name": "done", "type": "bool", "default": false},
            {"name": "ratio", "type": "number", "default": 0.5},
        ],
        "steps": [
            {"id": "wait", "op": "wait", "timeout_ms": 1000},
            {
                "id": "add", "op": "invoke", "name": "todo.add", "needs": ["wait"],
                "screenshot": true, "args": {"title": "$title"},
            },
            {
                "id": "note", "op": "set_value", "target": "todo-input", "needs": ["wait", "add"],
                "value": "$count $kind $done $ratio",
            },
            {
                "id": "check", "op": "assert", "target": "todo-item-1", "needs": ["note"],
                "name": "x", "checked": false, "exists": true,
            },
        ],
    })
}

struct Verdicts {
    schema: bool,
    parses: bool,
    validates: bool,
}

fn judge(validator: &jsonschema::Validator, doc: &Value) -> Verdicts {
    let parsed = Recipe::from_value(doc);
    Verdicts {
        schema: validator.is_valid(doc),
        parses: parsed.is_ok(),
        validates: parsed.is_ok_and(|recipe| validate_recipe(&recipe, &todo_registry()).is_ok()),
    }
}

#[test]
fn schema_and_parser_agree_on_documents() {
    let Some(schema) = schema() else {
        return;
    };
    let validator = jsonschema::validator_for(&schema).expect("the schema compiles");
    let variants = schema["$defs"]["step"]["oneOf"]
        .as_array()
        .expect("step variants");
    let mut problems = Vec::new();
    let mut tried = 0_usize;

    // Each step variant: its own fields, value by value. Exact agreement.
    let every_key: std::collections::BTreeSet<&String> = variants
        .iter()
        .flat_map(|variant| {
            variant["properties"]
                .as_object()
                .expect("properties")
                .keys()
        })
        .collect();
    for variant in variants {
        let op = variant["properties"]["op"]["const"].as_str().expect("op");
        let base = smallest_step(variant);
        let whole = judge(&validator, &recipe_of(base.clone()));
        if !(whole.schema && whole.validates) {
            problems.push(format!(
                "`{op}`: its smallest step is not a valid recipe (schema {}, validate {})",
                whole.schema, whole.validates
            ));
        }
        for key in &every_key {
            if ["id", "needs", "screenshot", "op"].contains(&key.as_str()) {
                continue;
            }
            let mut values = probes();
            let listed = &variant["properties"][key.as_str()]["enum"];
            values.extend(listed.as_array().into_iter().flatten().cloned());
            for value in values {
                let mut step = base.clone();
                step.insert((*key).clone(), value.clone());
                let got = judge(&validator, &recipe_of(step));
                tried += 1;
                if got.schema != got.parses {
                    problems.push(format!(
                        "`{op}`.`{key}` = {value}: schema {}, parser {}",
                        got.schema, got.parses
                    ));
                }
            }
        }
    }

    // A recipe that uses every part of the format, changed one place at a
    // time: valid => schema accepts => parses.
    let full = full_recipe();
    let whole = judge(&validator, &full);
    if !(whole.schema && whole.validates) {
        problems.push(format!(
            "the full recipe is not valid (schema {}, validate {})",
            whole.schema, whole.validates
        ));
    }
    for (what, doc) in one_change_from(&full) {
        let got = judge(&validator, &doc);
        tried += 1;
        if got.validates && !got.schema {
            problems.push(format!("{what}: validates, but the schema rejects it"));
        }
        if got.schema && !got.parses {
            problems.push(format!(
                "{what}: the schema accepts it, but it does not parse"
            ));
        }
    }

    assert!(tried > 5000, "only {tried} documents were tried");
    assert!(
        problems.is_empty(),
        "{} of {tried} documents disagree:\n{}",
        problems.len(),
        problems.join("\n")
    );
}

/// The one difference a JSON Schema cannot state: its `integer` is any
/// number with no fractional part, so it admits `1.0`; the parser reads a
/// whole-number field only from a JSON integer.
#[test]
fn schema_admits_a_whole_number_written_with_a_decimal_point() {
    let Some(schema) = schema() else {
        return;
    };
    let validator = jsonschema::validator_for(&schema).expect("the schema compiles");
    let doc = json!({"name": "r", "steps": [{"id": "a", "op": "events", "limit": 1.0}]});
    assert!(validator.is_valid(&doc));
    assert!(Recipe::from_value(&doc).is_err());
}

/// Every spelling of an op the parser reads is a step the schema knows.
#[test]
fn schema_knows_the_keybinding_list_spelling() {
    let Some(schema) = schema() else {
        return;
    };
    let validator = jsonschema::validator_for(&schema).expect("the schema compiles");
    let doc = json!({"name": "r", "steps": [{"id": "a", "op": "keybinding.list"}]});
    assert!(validator.is_valid(&doc));
    assert!(Recipe::from_value(&doc).is_ok());
}
