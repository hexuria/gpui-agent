use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use gpui_agent::protocol::{AssertSpec, DeliveryMode, Op};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::registry::Registry;
use crate::schema::SchemaKind;

/// Recipe document version (not the wire protocol version).
pub const RECIPE_FORMAT_VERSION: u32 = 1;
/// Cap so one recipe cannot flood the mailbox / connection.
pub const MAX_RECIPE_STEPS: usize = 256;

/// JSON recipe: a named DAG (or implicit sequence) of protocol ops.
///
/// JSON is the canonical form because the wire protocol is already JSON,
/// rwmcp recipes are JSON, and agents can emit it in one sample. A
/// line-based `.wants` file compiles to the same struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recipe {
    #[serde(default = "recipe_v1")]
    pub v: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<String>,
    pub steps: Vec<RecipeStep>,
}

fn recipe_v1() -> u32 {
    RECIPE_FORMAT_VERSION
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeStep {
    pub id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
    #[serde(flatten)]
    pub op: Op,
}

impl Recipe {
    pub fn from_json(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|err| format!("recipe json: {err}"))
    }

    pub fn uses_explicit_needs(&self) -> bool {
        self.steps.iter().any(|step| !step.needs.is_empty())
    }
}

/// Parse a file: `.json` as structured recipe, anything else as `.wants`.
/// Path `-` reads stdin.
pub fn parse_recipe(path: &Path) -> Result<Recipe, String> {
    let text = if path.as_os_str() == "-" {
        std::io::read_to_string(std::io::stdin()).map_err(|err| err.to_string())?
    } else {
        std::fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?
    };
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("recipe")
        .to_string();
    let hint = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    parse_recipe_source(&text, &name, hint == "json")
}

pub fn parse_recipe_source(text: &str, name: &str, force_json: bool) -> Result<Recipe, String> {
    let trimmed = text.trim();
    if force_json || trimmed.starts_with('{') {
        Recipe::from_json(trimmed)
    } else {
        parse_wants(trimmed, name)
    }
}

/// rwmcp-style `--wants`: one protocol op per line, `#` comments.
pub fn parse_wants(text: &str, name: &str) -> Result<Recipe, String> {
    let mut steps = Vec::new();
    let mut params = BTreeSet::new();
    for (idx, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let tokens = tokenize(line)?;
        if tokens.is_empty() {
            continue;
        }
        let id = format!("s{}", steps.len() + 1);
        let step =
            parse_wants_step(&tokens, id).map_err(|err| format!("line {}: {err}", idx + 1))?;
        collect_placeholders_in_op(&step.op, &mut params);
        steps.push(step);
    }
    if steps.is_empty() {
        return Err("recipe has no steps".into());
    }
    Ok(Recipe {
        v: RECIPE_FORMAT_VERSION,
        name: name.into(),
        app: None,
        params: params.into_iter().collect(),
        steps,
    })
}

pub fn apply_params(recipe: &Recipe, set: &BTreeMap<String, String>) -> Result<Recipe, String> {
    for name in &recipe.params {
        if !set.contains_key(name) {
            return Err(format!("missing --set {name}=…"));
        }
    }
    let mut out = recipe.clone();
    for step in &mut out.steps {
        step.op = substitute_op(&step.op, set)?;
        for need in &mut step.needs {
            *need = substitute_string(need, set)?;
        }
        step.id = substitute_string(&step.id, set)?;
    }
    Ok(out)
}

pub fn validate_recipe(recipe: &Recipe, registry: &Registry) -> Result<(), String> {
    if recipe.v != RECIPE_FORMAT_VERSION {
        return Err(format!(
            "unsupported recipe version {} (want {RECIPE_FORMAT_VERSION})",
            recipe.v
        ));
    }
    if recipe.name.trim().is_empty() {
        return Err("recipe name cannot be empty".into());
    }
    if recipe.steps.is_empty() {
        return Err("recipe has no steps".into());
    }
    if recipe.steps.len() > MAX_RECIPE_STEPS {
        return Err(format!(
            "recipe has {} steps (max {MAX_RECIPE_STEPS})",
            recipe.steps.len()
        ));
    }

    let mut ids = BTreeSet::new();
    for step in &recipe.steps {
        if step.id.trim().is_empty() {
            return Err("step id cannot be empty".into());
        }
        if !ids.insert(step.id.clone()) {
            return Err(format!("duplicate step id `{}`", step.id));
        }
    }
    for step in &recipe.steps {
        for need in &step.needs {
            if !ids.contains(need) {
                return Err(format!("step `{}` needs unknown `{need}`", step.id));
            }
            if need == &step.id {
                return Err(format!("step `{}` cannot need itself", step.id));
            }
        }
        validate_placeholders_declared(&step.op, &recipe.params)?;
        if let Op::Invoke { name, .. } = &step.op {
            match registry.get(name) {
                Some(schema) if matches!(schema.kind, SchemaKind::Invoke) => {}
                Some(_) => {
                    return Err(format!(
                        "invoke `{name}` is registered but is not an invoke schema"
                    ));
                }
                None => {
                    return Err(format!(
                        "unknown invoke `{name}` (not in schema registry; fail closed)"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_placeholders_declared(op: &Op, params: &[String]) -> Result<(), String> {
    let mut found = BTreeSet::new();
    collect_placeholders_in_op(op, &mut found);
    for name in found {
        if !params.iter().any(|p| p == &name) {
            return Err(format!(
                "placeholder ${name} is not declared in recipe.params"
            ));
        }
    }
    Ok(())
}

fn collect_placeholders_in_op(op: &Op, out: &mut BTreeSet<String>) {
    if let Ok(value) = serde_json::to_value(op) {
        collect_placeholders_in_value(&value, out);
    }
}

fn collect_placeholders_in_value(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::String(s) => collect_placeholders(s, out),
        Value::Array(xs) => {
            for x in xs {
                collect_placeholders_in_value(x, out);
            }
        }
        Value::Object(map) => {
            for v in map.values() {
                collect_placeholders_in_value(v, out);
            }
        }
        _ => {}
    }
}

fn collect_placeholders(s: &str, out: &mut BTreeSet<String>) {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' {
            if let Some((name, next)) = parse_placeholder(&chars, i) {
                out.insert(name);
                i = next;
                continue;
            }
        }
        i += 1;
    }
}

fn parse_placeholder(chars: &[char], dollar: usize) -> Option<(String, usize)> {
    let i = dollar + 1;
    if i >= chars.len() {
        return None;
    }
    if chars[i] == '{' {
        let end = chars[i + 1..].iter().position(|c| *c == '}')? + i + 1;
        let name: String = chars[i + 1..end].iter().collect();
        if is_ident(&name) {
            return Some((name, end + 1));
        }
        return None;
    }
    let mut end = i;
    while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_') {
        end += 1;
    }
    if end == i {
        return None;
    }
    let name: String = chars[i..end].iter().collect();
    if is_ident(&name) {
        Some((name, end))
    } else {
        None
    }
}

fn is_ident(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn substitute_op(op: &Op, set: &BTreeMap<String, String>) -> Result<Op, String> {
    let mut value = serde_json::to_value(op).map_err(|err| err.to_string())?;
    substitute_value(&mut value, set)?;
    serde_json::from_value(value).map_err(|err| format!("after $param substitution: {err}"))
}

fn substitute_value(value: &mut Value, set: &BTreeMap<String, String>) -> Result<(), String> {
    match value {
        Value::String(s) => *s = substitute_string(s, set)?,
        Value::Array(xs) => {
            for x in xs {
                substitute_value(x, set)?;
            }
        }
        Value::Object(map) => {
            for v in map.values_mut() {
                substitute_value(v, set)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn substitute_string(input: &str, set: &BTreeMap<String, String>) -> Result<String, String> {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$'
            && let Some((name, next)) = parse_placeholder(&chars, i)
        {
            let value = set
                .get(&name)
                .ok_or_else(|| format!("missing --set {name}=…"))?;
            out.push_str(value);
            i = next;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    Ok(out)
}

fn tokenize(line: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut buf = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' => {
                let quote = c;
                loop {
                    match chars.next() {
                        Some(ch) if ch == quote => break,
                        Some('\\') => {
                            if let Some(next) = chars.next() {
                                buf.push(next);
                            }
                        }
                        Some(ch) => buf.push(ch),
                        None => return Err("unterminated quote".into()),
                    }
                }
                if chars.peek().is_none_or(|ch| ch.is_whitespace()) {
                    tokens.push(std::mem::take(&mut buf));
                }
            }
            ch if ch.is_whitespace() => {
                if !buf.is_empty() {
                    tokens.push(std::mem::take(&mut buf));
                }
            }
            ch => buf.push(ch),
        }
    }
    if !buf.is_empty() {
        tokens.push(buf);
    }
    Ok(tokens)
}

fn parse_wants_step(tokens: &[String], id: String) -> Result<RecipeStep, String> {
    let op_name = tokens[0].replace('-', "_");
    let op = match op_name.as_str() {
        "wait" => Op::Wait { timeout_ms: None },
        "hello" => Op::Hello,
        "snapshot" => Op::Snapshot,
        "shutdown" => Op::Shutdown,
        "click" => {
            let (delivery, rest) = take_delivery(&tokens[1..])?;
            let target = rest
                .first()
                .ok_or_else(|| "click needs a target".to_string())?
                .clone();
            Op::Click { target, delivery }
        }
        "type" => {
            let (delivery, rest) = take_delivery(&tokens[1..])?;
            if rest.len() < 2 {
                return Err("type needs target and text".into());
            }
            Op::Type {
                target: rest[0].clone(),
                text: rest[1].clone(),
                delivery,
            }
        }
        "set_value" => {
            if tokens.len() < 3 {
                return Err("set-value needs target and value".into());
            }
            Op::SetValue {
                target: tokens[1].clone(),
                value: tokens[2].clone(),
            }
        }
        "key" => {
            let (delivery, rest) = take_delivery(&tokens[1..])?;
            if rest.len() < 2 {
                return Err("key needs target and key name".into());
            }
            Op::Key {
                target: rest[0].clone(),
                key: rest[1].clone(),
                delivery,
            }
        }
        "assert" => Op::Assert {
            spec: parse_assert_spec(&tokens[1..])?,
        },
        "invoke" => {
            let name = tokens
                .get(1)
                .ok_or_else(|| "invoke needs a name".to_string())?
                .clone();
            let mut map = serde_json::Map::new();
            for token in &tokens[2..] {
                let (key, raw) = token
                    .split_once('=')
                    .ok_or_else(|| format!("expected KEY=VALUE, got {token}"))?;
                let value =
                    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()));
                map.insert(key.to_string(), value);
            }
            Op::Invoke {
                name,
                args: Value::Object(map),
            }
        }
        other => return Err(format!("unknown op `{other}`")),
    };
    Ok(RecipeStep {
        id,
        needs: Vec::new(),
        op,
    })
}

fn take_delivery(tokens: &[String]) -> Result<(DeliveryMode, &[String]), String> {
    if tokens.first().map(String::as_str) == Some("--delivery") {
        let raw = tokens
            .get(1)
            .ok_or_else(|| "--delivery needs semantic or virtual".to_string())?;
        let delivery: DeliveryMode = raw.parse()?;
        Ok((delivery, &tokens[2..]))
    } else {
        Ok((DeliveryMode::Semantic, tokens))
    }
}

fn parse_assert_spec(tokens: &[String]) -> Result<AssertSpec, String> {
    let mut spec = AssertSpec::default();
    let mut i = 0;
    while i < tokens.len() {
        let token = &tokens[i];
        if token == "--id" || token == "--target" {
            i += 1;
            spec.target = tokens
                .get(i)
                .ok_or_else(|| "missing assert target".to_string())?
                .clone();
        } else if token == "--name" {
            i += 1;
            spec.name = Some(
                tokens
                    .get(i)
                    .ok_or_else(|| "missing --name".to_string())?
                    .clone(),
            );
        } else if token == "--value" {
            i += 1;
            spec.value = Some(
                tokens
                    .get(i)
                    .ok_or_else(|| "missing --value".to_string())?
                    .clone(),
            );
        } else if token == "--role" {
            i += 1;
            spec.role = Some(
                tokens
                    .get(i)
                    .ok_or_else(|| "missing --role".to_string())?
                    .clone(),
            );
        } else if token == "--checked" {
            if tokens.get(i + 1).map(String::as_str) == Some("true") {
                spec.checked = Some(true);
                i += 1;
            } else if tokens.get(i + 1).map(String::as_str) == Some("false") {
                spec.checked = Some(false);
                i += 1;
            } else {
                spec.checked = Some(true);
            }
        } else if token == "--exists" {
            if tokens.get(i + 1).map(String::as_str) == Some("false") {
                spec.exists = Some(false);
                i += 1;
            } else {
                spec.exists = Some(true);
                if tokens.get(i + 1).map(String::as_str) == Some("true") {
                    i += 1;
                }
            }
        } else if token == "--absent" {
            spec.exists = Some(false);
        } else if let Some((key, raw)) = token.split_once('=') {
            apply_assert_kv(&mut spec, key, raw)?;
        } else if spec.target.is_empty() && !token.starts_with('-') {
            spec.target = token.clone();
        } else {
            return Err(format!("bad assert token `{token}`"));
        }
        i += 1;
    }
    if spec.target.is_empty() {
        return Err("assert needs a target".into());
    }
    Ok(spec)
}

fn apply_assert_kv(spec: &mut AssertSpec, key: &str, raw: &str) -> Result<(), String> {
    match key {
        "name" => spec.name = Some(raw.into()),
        "value" => spec.value = Some(raw.into()),
        "role" => spec.role = Some(raw.into()),
        "checked" => {
            spec.checked = Some(parse_bool(raw)?);
        }
        "exists" => spec.exists = Some(parse_bool(raw)?),
        "target" | "id" => spec.target = raw.into(),
        other => return Err(format!("unknown assert field `{other}`")),
    }
    Ok(())
}

fn parse_bool(raw: &str) -> Result<bool, String> {
    match raw {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(format!("expected true/false, got {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::todo_registry;

    #[test]
    fn json_recipe_roundtrip() {
        let json = r#"{
            "name": "demo",
            "params": ["title"],
            "steps": [
                {"id": "set", "op": "set_value", "target": "todo-input", "value": "$title"},
                {"id": "add", "op": "click", "target": "todo-add", "needs": ["set"]}
            ]
        }"#;
        let recipe = Recipe::from_json(json).unwrap();
        validate_recipe(&recipe, &todo_registry()).unwrap();
        assert_eq!(recipe.steps.len(), 2);
        assert_eq!(recipe.steps[1].needs, vec!["set"]);
    }

    #[test]
    fn wants_attached_quotes_become_one_token() {
        let recipe = parse_wants(r#"invoke todo.add title="Buy milk""#, "q").unwrap();
        match &recipe.steps[0].op {
            Op::Invoke { args, .. } => assert_eq!(args["title"], "Buy milk"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn wants_parse_and_params() {
        let wants = r#"
# create a todo
set-value todo-input $title
click todo-add
assert todo-item-1 name=$title checked=false
"#;
        let recipe = parse_wants(wants, "crud").unwrap();
        assert_eq!(recipe.params, vec!["title"]);
        validate_recipe(&recipe, &todo_registry()).unwrap();
        let mut set = BTreeMap::new();
        set.insert("title".into(), "Buy milk".into());
        let bound = apply_params(&recipe, &set).unwrap();
        match &bound.steps[0].op {
            Op::SetValue { value, .. } => assert_eq!(value, "Buy milk"),
            other => panic!("{other:?}"),
        }
        match &bound.steps[2].op {
            Op::Assert { spec } => assert_eq!(spec.name.as_deref(), Some("Buy milk")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unknown_invoke_fails_closed() {
        let recipe = Recipe::from_json(
            r#"{
            "name": "bad",
            "steps": [{"id": "x", "op": "invoke", "name": "shell.run", "args": {"cmd": "rm"}}]
        }"#,
        )
        .unwrap();
        let err = validate_recipe(&recipe, &todo_registry()).unwrap_err();
        assert!(err.contains("unknown invoke"), "{err}");
    }

    #[test]
    fn undeclared_placeholder_is_rejected() {
        let recipe = Recipe::from_json(
            r#"{
            "name": "bad",
            "steps": [{"id": "x", "op": "set_value", "target": "todo-input", "value": "$title"}]
        }"#,
        )
        .unwrap();
        let err = validate_recipe(&recipe, &todo_registry()).unwrap_err();
        assert!(err.contains("not declared"), "{err}");
    }

    #[test]
    fn empty_source_is_rejected() {
        let err = parse_recipe_source("", "empty", false).unwrap_err();
        assert!(err.contains("no steps"), "{err}");
        let err = parse_recipe_source("   \n# only comments\n\n", "c", false).unwrap_err();
        assert!(err.contains("no steps"), "{err}");
    }

    #[test]
    fn empty_json_steps_are_rejected() {
        let recipe = Recipe::from_json(r#"{"name":"x","steps":[]}"#).unwrap();
        let err = validate_recipe(&recipe, &todo_registry()).unwrap_err();
        assert!(err.contains("no steps"), "{err}");
    }

    #[test]
    fn bad_json_is_rejected() {
        let err = parse_recipe_source("{", "bad", true).unwrap_err();
        assert!(err.contains("recipe json"), "{err}");
        let err = parse_recipe_source("not-json", "bad", true).unwrap_err();
        assert!(err.contains("recipe json"), "{err}");
    }

    #[test]
    fn empty_file_via_parse_recipe() {
        let path = std::env::temp_dir().join("gpui-agent-empty.wants");
        std::fs::write(&path, "").unwrap();
        let err = parse_recipe(&path).unwrap_err();
        assert!(err.contains("no steps"), "{err}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unknown_wants_op_is_rejected() {
        let err = parse_wants("frobnicate todo-add", "x").unwrap_err();
        assert!(err.contains("unknown op"), "{err}");
    }

    #[test]
    fn missing_required_params_at_apply() {
        let recipe = Recipe::from_json(
            r#"{
            "name": "p",
            "params": ["title"],
            "steps": [{"id": "x", "op": "set_value", "target": "todo-input", "value": "$title"}]
        }"#,
        )
        .unwrap();
        validate_recipe(&recipe, &todo_registry()).unwrap();
        let err = apply_params(&recipe, &BTreeMap::new()).unwrap_err();
        assert!(err.contains("missing --set title"), "{err}");
    }

    #[test]
    fn duplicate_step_ids_are_rejected() {
        let recipe = Recipe::from_json(
            r#"{
            "name": "dup",
            "steps": [
                {"id": "a", "op": "hello"},
                {"id": "a", "op": "snapshot"}
            ]
        }"#,
        )
        .unwrap();
        let err = validate_recipe(&recipe, &todo_registry()).unwrap_err();
        assert!(err.contains("duplicate"), "{err}");
    }

    #[test]
    fn empty_step_id_and_empty_name_are_rejected() {
        let recipe = Recipe::from_json(r#"{"name":"","steps":[{"id":"a","op":"hello"}]}"#).unwrap();
        let err = validate_recipe(&recipe, &todo_registry()).unwrap_err();
        assert!(err.contains("name"), "{err}");

        let recipe = Recipe::from_json(r#"{"name":"x","steps":[{"id":"","op":"hello"}]}"#).unwrap();
        let err = validate_recipe(&recipe, &todo_registry()).unwrap_err();
        assert!(err.contains("step id"), "{err}");
    }

    #[test]
    fn unknown_and_self_needs_are_rejected() {
        let missing = Recipe::from_json(
            r#"{"name":"x","steps":[{"id":"a","op":"hello","needs":["ghost"]}]}"#,
        )
        .unwrap();
        let err = validate_recipe(&missing, &todo_registry()).unwrap_err();
        assert!(err.contains("unknown"), "{err}");

        let sel =
            Recipe::from_json(r#"{"name":"x","steps":[{"id":"a","op":"hello","needs":["a"]}]}"#)
                .unwrap();
        let err = validate_recipe(&sel, &todo_registry()).unwrap_err();
        assert!(err.contains("itself"), "{err}");
    }

    #[test]
    fn unsupported_recipe_version() {
        let recipe =
            Recipe::from_json(r#"{"v":99,"name":"x","steps":[{"id":"a","op":"hello"}]}"#).unwrap();
        let err = validate_recipe(&recipe, &todo_registry()).unwrap_err();
        assert!(err.contains("unsupported recipe version"), "{err}");
    }

    #[test]
    fn too_many_steps_are_rejected() {
        let steps: Vec<serde_json::Value> = (0..=MAX_RECIPE_STEPS)
            .map(|i| serde_json::json!({"id": format!("s{i}"), "op": "hello"}))
            .collect();
        let recipe =
            Recipe::from_json(&serde_json::json!({"name":"big","steps": steps}).to_string())
                .unwrap();
        let err = validate_recipe(&recipe, &todo_registry()).unwrap_err();
        assert!(err.contains("max"), "{err}");
    }

    #[test]
    fn invoke_protocol_name_is_not_an_invoke_schema() {
        let recipe = Recipe::from_json(
            r#"{"name":"x","steps":[{"id":"a","op":"invoke","name":"click","args":{}}]}"#,
        )
        .unwrap();
        let err = validate_recipe(&recipe, &todo_registry()).unwrap_err();
        assert!(err.contains("not an invoke schema"), "{err}");
    }

    #[test]
    fn wants_skips_comments_and_blanks() {
        let recipe = parse_wants("\n# heading\n\nhello\n  # mid\n\nsnapshot\n", "c").unwrap();
        assert_eq!(recipe.steps.len(), 2);
        assert!(matches!(recipe.steps[0].op, Op::Hello));
        assert!(matches!(recipe.steps[1].op, Op::Snapshot));
    }

    #[test]
    fn wants_unterminated_quote_is_invalid() {
        let err = parse_wants(r#"set-value todo-input "no-end"#, "q").unwrap_err();
        assert!(err.contains("unterminated quote"), "{err}");
    }

    #[test]
    fn wants_invalid_lines() {
        assert!(parse_wants("click", "x").unwrap_err().contains("target"));
        assert!(parse_wants("assert", "x").unwrap_err().contains("target"));
        assert!(parse_wants("invoke", "x").unwrap_err().contains("name"));
        assert!(
            parse_wants("set-value todo-input", "x")
                .unwrap_err()
                .contains("set-value")
        );
        assert!(
            parse_wants("type todo-input", "x")
                .unwrap_err()
                .contains("type")
        );
        assert!(parse_wants("assert todo-item-1 noflag", "x").is_err());
    }

    #[test]
    fn wants_delivery_and_assert_flags() {
        let recipe = parse_wants(
            "click --delivery virtual todo-add\nassert --id todo-item-1 --absent\nassert todo-x checked=true exists=false",
            "d",
        )
        .unwrap();
        match &recipe.steps[0].op {
            Op::Click { target, delivery } => {
                assert_eq!(target, "todo-add");
                assert_eq!(*delivery, DeliveryMode::Virtual);
            }
            other => panic!("{other:?}"),
        }
        match &recipe.steps[1].op {
            Op::Assert { spec } => {
                assert_eq!(spec.target, "todo-item-1");
                assert_eq!(spec.exists, Some(false));
            }
            other => panic!("{other:?}"),
        }
        match &recipe.steps[2].op {
            Op::Assert { spec } => {
                assert_eq!(spec.checked, Some(true));
                assert_eq!(spec.exists, Some(false));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn json_extension_forces_json_even_without_brace() {
        let path = std::env::temp_dir().join("gpui-agent-not-object.json");
        std::fs::write(&path, "[]").unwrap();
        let err = parse_recipe(&path).unwrap_err();
        assert!(err.contains("recipe json"), "{err}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unknown_json_op_is_rejected() {
        let err = parse_recipe_source(
            r#"{"name":"x","steps":[{"id":"a","op":"explode"}]}"#,
            "x.json",
            true,
        )
        .unwrap_err();
        assert!(err.contains("recipe json"), "{err}");
    }

    #[test]
    fn brace_and_bare_params_substitute() {
        let recipe = Recipe::from_json(
            r#"{
            "name": "p",
            "params": ["title"],
            "steps": [
                {"id": "a", "op": "set_value", "target": "todo-input", "value": "${title}"},
                {"id": "b", "op": "set_value", "target": "todo-input", "value": "Hi $title"}
            ]
        }"#,
        )
        .unwrap();
        validate_recipe(&recipe, &todo_registry()).unwrap();
        let mut set = BTreeMap::new();
        set.insert("title".into(), "Buy milk".into());
        let bound = apply_params(&recipe, &set).unwrap();
        match &bound.steps[0].op {
            Op::SetValue { value, .. } => assert_eq!(value, "Buy milk"),
            other => panic!("{other:?}"),
        }
        match &bound.steps[1].op {
            Op::SetValue { value, .. } => assert_eq!(value, "Hi Buy milk"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn wants_standalone_quoted_value() {
        let recipe = parse_wants(r#"set-value todo-input "Buy milk""#, "q").unwrap();
        match &recipe.steps[0].op {
            Op::SetValue { target, value } => {
                assert_eq!(target, "todo-input");
                assert_eq!(value, "Buy milk");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn wants_escaped_quotes_inside_quotes() {
        let recipe = parse_wants(r#"set-value todo-input "Say \"hi\"""#, "q").unwrap();
        match &recipe.steps[0].op {
            Op::SetValue { value, .. } => assert_eq!(value, r#"Say "hi""#),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn wants_key_and_wait_and_bad_delivery() {
        let recipe = parse_wants("wait\nkey todo-input Enter", "k").unwrap();
        assert!(matches!(recipe.steps[0].op, Op::Wait { .. }));
        match &recipe.steps[1].op {
            Op::Key { target, key, .. } => {
                assert_eq!(target, "todo-input");
                assert_eq!(key, "Enter");
            }
            other => panic!("{other:?}"),
        }
        let err = parse_wants("click --delivery", "x").unwrap_err();
        assert!(err.contains("--delivery"), "{err}");
        let err = parse_wants("assert todo-item-1 foo=bar", "x").unwrap_err();
        assert!(err.contains("unknown assert field"), "{err}");
    }
}
