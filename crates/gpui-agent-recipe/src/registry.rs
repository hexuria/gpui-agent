use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::schema::{ArgSchema, Effect, OpSchema, SchemaKind};

/// In-process schema registry. No network, no shell data sources.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    schemas: BTreeMap<String, OpSchema>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, schema: OpSchema) -> Result<(), String> {
        schema.validate()?;
        self.schemas.insert(schema.name.clone(), schema);
        Ok(())
    }

    pub fn merge(&mut self, other: Registry) -> Result<(), String> {
        for schema in other.schemas.into_values() {
            if self.schemas.contains_key(&schema.name) {
                return Err(format!("duplicate schema `{}`", schema.name));
            }
            self.insert(schema)?;
        }
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&OpSchema> {
        self.schemas.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &OpSchema> {
        self.schemas.values()
    }
}

fn arg(ty: &str, required: bool) -> ArgSchema {
    ArgSchema {
        ty: ty.into(),
        required,
    }
}

fn protocol(
    name: &str,
    description: &str,
    effects: Vec<Effect>,
    idempotent: bool,
    keywords: &[&str],
    required: &[&str],
) -> OpSchema {
    OpSchema {
        name: name.into(),
        kind: SchemaKind::Protocol,
        description: description.into(),
        effects,
        idempotent,
        verified: true,
        required: required.iter().map(|s| (*s).to_string()).collect(),
        args: BTreeMap::new(),
        result: None,
        keywords: keywords.iter().map(|s| (*s).to_string()).collect(),
    }
}

/// Protocol ops only. App invoke/id schemas load from `--schema` / `GPUI_AGENT_SCHEMA`.
pub fn protocol_registry() -> Registry {
    let mut reg = Registry::new();
    let protocol_ops = [
        protocol(
            "hello",
            "Handshake: protocol version, app name, platform.",
            vec![Effect::Read],
            true,
            &["hello", "handshake"],
            &[],
        ),
        protocol(
            "wait",
            "Block until the host answers hello/ready.",
            vec![Effect::Read],
            true,
            &["wait", "ready"],
            &[],
        ),
        protocol(
            "snapshot",
            "Read the semantic UI tree.",
            vec![Effect::Read],
            true,
            &["snapshot", "tree", "perceive"],
            &[],
        ),
        protocol(
            "screenshot",
            "Write an observe-only PNG of the app surface (not the desktop).",
            vec![Effect::Read],
            true,
            &["screenshot", "png", "frame"],
            &[],
        ),
        protocol(
            "click",
            "Activate a widget by stable id.",
            vec![Effect::Write],
            false,
            &["click", "press", "tap"],
            &["target"],
        ),
        protocol(
            "type",
            "Append text to an editable widget.",
            vec![Effect::Write],
            false,
            &["type", "append"],
            &["target", "text"],
        ),
        protocol(
            "set_value",
            "Replace the value of an editable widget.",
            vec![Effect::Write],
            true,
            &["set", "value", "fill"],
            &["target", "value"],
        ),
        protocol(
            "key",
            "Send a key to a widget.",
            vec![Effect::Write],
            false,
            &["key", "enter", "backspace"],
            &["target", "key"],
        ),
        protocol(
            "assert",
            "Check snapshot fields on a stable id.",
            vec![Effect::Read],
            true,
            &["assert", "check", "verify"],
            &["target"],
        ),
        protocol(
            "invoke",
            "Call a named host command (allow-listed by the app).",
            vec![Effect::Write],
            false,
            &["invoke", "command"],
            &["name"],
        ),
        protocol(
            "shutdown",
            "Ask the host to exit.",
            vec![Effect::Exit],
            false,
            &["shutdown", "exit", "quit"],
            &[],
        ),
    ];
    for schema in protocol_ops {
        reg.insert(schema).expect("protocol schema");
    }
    reg
}

fn todo_app_registry() -> Registry {
    let mut reg = Registry::new();
    let mut add_args = BTreeMap::new();
    add_args.insert("title".into(), arg("string", true));
    reg.insert(OpSchema {
        name: "todo.add".into(),
        kind: SchemaKind::Invoke,
        description: "Create a todo item from a title.".into(),
        effects: vec![Effect::Write],
        idempotent: false,
        verified: true,
        required: vec!["title".into()],
        args: add_args,
        result: Some(json!({"id": "u64", "title": "string", "done": "bool"})),
        keywords: vec![
            "add".into(),
            "create".into(),
            "new".into(),
            "todo".into(),
            "task".into(),
        ],
    })
    .unwrap();

    let mut id_args = BTreeMap::new();
    id_args.insert("id".into(), arg("number", true));
    reg.insert(OpSchema {
        name: "todo.toggle".into(),
        kind: SchemaKind::Invoke,
        description: "Toggle a todo item's done state.".into(),
        effects: vec![Effect::Write],
        idempotent: false,
        verified: true,
        required: vec!["id".into()],
        args: id_args.clone(),
        result: Some(json!({"id": "u64", "title": "string", "done": "bool"})),
        keywords: vec![
            "toggle".into(),
            "check".into(),
            "complete".into(),
            "todo".into(),
        ],
    })
    .unwrap();

    reg.insert(OpSchema {
        name: "todo.delete".into(),
        kind: SchemaKind::Invoke,
        description: "Delete a todo item.".into(),
        effects: vec![Effect::Write],
        idempotent: false,
        verified: true,
        required: vec!["id".into()],
        args: id_args,
        result: Some(json!({"id": "u64", "title": "string", "done": "bool"})),
        keywords: vec!["delete".into(), "remove".into(), "todo".into()],
    })
    .unwrap();

    reg.insert(OpSchema {
        name: "todo.list".into(),
        kind: SchemaKind::Invoke,
        description: "List todo items.".into(),
        effects: vec![Effect::Read],
        idempotent: true,
        verified: true,
        required: vec![],
        args: BTreeMap::new(),
        result: Some(json!([{"id": "u64", "title": "string", "done": "bool"}])),
        keywords: vec!["list".into(), "todos".into(), "items".into()],
    })
    .unwrap();

    for (name, description, keywords) in [
        (
            "todo-input",
            "Draft text field for a new todo.",
            &["input", "draft", "textbox"][..],
        ),
        (
            "todo-add",
            "Add button that commits the draft.",
            &["add", "button"],
        ),
        ("todo-list", "List of todo items.", &["list"]),
        (
            "todo-window",
            "Root window of the sample todo app.",
            &["window"],
        ),
    ] {
        reg.insert(OpSchema {
            name: name.into(),
            kind: SchemaKind::Id,
            description: description.into(),
            effects: vec![Effect::Write],
            idempotent: false,
            verified: true,
            required: vec![],
            args: BTreeMap::new(),
            result: None,
            keywords: keywords.iter().map(|s| (*s).to_string()).collect(),
        })
        .unwrap();
    }

    reg
}

/// Demo registry: protocol ops + sample todo invoke/ids.
///
/// Other apps should ship JSON schemas; this in-process copy keeps unit
/// tests independent of `examples/schemas/todo.json`.
pub fn todo_registry() -> Registry {
    let mut reg = protocol_registry();
    reg.merge(todo_app_registry()).expect("todo schemas");
    reg
}

/// Load invoke/id (or extra protocol) schemas from a JSON file.
///
/// Accepts a top-level array or `{ "schemas": [ ... ] }`. Every row is
/// validated; unknown invoke names stay fail-closed at recipe validate.
pub fn load_schema_file(path: &Path) -> Result<Registry, String> {
    let text = std::fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|err| format!("{}: {err}", path.display()))?;
    let rows = if let Some(arr) = value.as_array() {
        arr.clone()
    } else if let Some(arr) = value.get("schemas").and_then(|v| v.as_array()) {
        arr.clone()
    } else {
        return Err(format!(
            "{}: expected a JSON array or {{ \"schemas\": [...] }}",
            path.display()
        ));
    };
    let mut reg = Registry::new();
    for row in rows {
        let schema: OpSchema =
            serde_json::from_value(row).map_err(|err| format!("{}: {err}", path.display()))?;
        reg.insert(schema)?;
    }
    Ok(reg)
}

/// `protocol_registry()` plus files from `GPUI_AGENT_SCHEMA` and `paths`.
pub fn registry_from_schema_paths(paths: &[PathBuf]) -> Result<Registry, String> {
    let mut reg = protocol_registry();
    let mut all = Vec::new();
    if let Some(env) = std::env::var_os("GPUI_AGENT_SCHEMA") {
        all.extend(std::env::split_paths(&env));
    }
    all.extend(paths.iter().cloned());
    for path in all {
        if path.as_os_str().is_empty() {
            continue;
        }
        reg.merge(load_schema_file(&path)?)?;
    }
    Ok(reg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todo_registry_has_verified_invoke_names() {
        let reg = todo_registry();
        assert!(reg.get("todo.add").unwrap().verified);
        assert_eq!(reg.get("todo.add").unwrap().effects, vec![Effect::Write]);
        assert!(reg.get("shutdown").unwrap().effects.contains(&Effect::Exit));
        assert!(reg.get("click").is_some());
        assert!(reg.get("screenshot").unwrap().idempotent);
        assert!(reg.get("todo-input").is_some());
    }

    #[test]
    fn insert_rejects_non_allowlisted_schema_name() {
        let mut reg = Registry::new();
        let err = reg
            .insert(OpSchema {
                name: "rm -rf".into(),
                kind: SchemaKind::Invoke,
                description: "no".into(),
                effects: vec![Effect::Write],
                idempotent: false,
                verified: false,
                required: vec![],
                args: BTreeMap::new(),
                result: None,
                keywords: vec![],
            })
            .unwrap_err();
        assert!(
            err.contains("alphanumeric") || err.contains("name"),
            "{err}"
        );
        assert!(reg.get("rm -rf").is_none());
    }

    #[test]
    fn recipe_rejects_unknown_invoke_with_protocol_only_registry() {
        let recipe = crate::recipe::parse_recipe_source(
            r#"{
                "name": "x",
                "steps": [{"id": "a", "op": "invoke", "name": "todo.add", "args": {"title": "x"}}]
            }"#,
            "x",
            true,
        )
        .unwrap();
        let err = crate::recipe::validate_recipe(&recipe, &protocol_registry()).unwrap_err();
        assert!(err.contains("unknown invoke"), "{err}");
        assert!(err.contains("todo.add"), "{err}");
    }

    #[test]
    fn empty_registry_rejects_all_invoke() {
        let recipe = crate::recipe::parse_recipe_source(
            r#"{
                "name": "x",
                "steps": [{"id": "a", "op": "invoke", "name": "todo.add", "args": {"title": "x"}}]
            }"#,
            "x",
            true,
        )
        .unwrap();
        let err = crate::recipe::validate_recipe(&recipe, &Registry::new()).unwrap_err();
        assert!(err.contains("unknown invoke"), "{err}");
        assert!(err.contains("todo.add"), "{err}");
    }

    #[test]
    fn recipe_accepts_invoke_from_schema_file() {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/schemas/todo.json");
        let mut reg = protocol_registry();
        reg.merge(load_schema_file(&path).expect("todo.json"))
            .unwrap();
        let recipe = crate::recipe::parse_recipe_source(
            r#"{
                "name": "x",
                "steps": [{"id": "a", "op": "invoke", "name": "todo.add", "args": {"title": "x"}}]
            }"#,
            "x",
            true,
        )
        .unwrap();
        crate::recipe::validate_recipe(&recipe, &reg).expect("schema file should allow todo.add");
        assert!(reg.get("todo.add").unwrap().verified);
    }
}
