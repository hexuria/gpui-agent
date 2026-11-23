use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{RecipeError, RecipeErrorCode, err};

/// TMP-shaped operation contract: effects, required inputs, result hint.
///
/// This is a local mapping schema, not a `tmp-core` dependency. Unknown
/// intents fail closed; nothing here shells out. In code, start from
/// [`OpSchema::new`] and set the fields that apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct OpSchema {
    /// Stable schema name (`todo.add`, `click`, `shutdown`).
    pub name: String,
    /// How `name` is dispatched: protocol op, `invoke`, or widget id.
    pub kind: SchemaKind,
    /// Human/agent-facing description used in intent matching.
    pub description: String,
    /// Declared side effects; empty is treated as Write (fail closed
    /// — it cannot join a pipelined read wave).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<Effect>,
    /// True when replaying the op is safe.
    #[serde(default)]
    pub idempotent: bool,
    /// True when the schema was verified against a real app.
    #[serde(default)]
    pub verified: bool,
    /// Arg names that must be filled before dispatch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required: Vec<String>,
    /// Per-arg type specs for `Invoke` schemas.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub args: BTreeMap<String, ArgSchema>,
    /// Expected result shape (documentation/eval only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Intent-matching keywords beyond the name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
}

/// How a schema name maps onto the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SchemaKind {
    /// A first-class protocol op (`click`, `snapshot`, …).
    Protocol,
    /// Host `invoke` name (`todo.add`).
    Invoke,
    /// Stable widget id (`todo-add`).
    Id,
}

/// A schema's declared side effects (fail closed: unknown = Write).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Effect {
    /// Observation only.
    Read,
    /// Mutates app state.
    Write,
    /// Terminates the host.
    Exit,
}

/// Type/required spec for one `Invoke` arg.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ArgSchema {
    /// Wire type name (`string`, `number`, `bool`, …).
    #[serde(rename = "type")]
    pub ty: String,
    /// True when the arg must be filled.
    #[serde(default)]
    pub required: bool,
}

impl ArgSchema {
    /// An optional arg of wire type `ty`.
    pub fn new(ty: impl Into<String>) -> Self {
        Self {
            ty: ty.into(),
            required: false,
        }
    }
}

impl OpSchema {
    /// A schema with only its name, kind and description: no declared
    /// effects (treated as Write), no args, not idempotent, not verified.
    pub fn new(name: impl Into<String>, kind: SchemaKind, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind,
            description: description.into(),
            effects: Vec::new(),
            idempotent: false,
            verified: false,
            required: Vec::new(),
            args: BTreeMap::new(),
            result: None,
            keywords: Vec::new(),
        }
    }

    /// Reject empty/invalid names and `required` entries with no
    /// `args` spec. Duplicate names are the registry's job:
    /// [`Registry::insert`](crate::registry::Registry::insert) replaces,
    /// [`Registry::merge`](crate::registry::Registry::merge) rejects.
    ///
    /// # Errors
    /// [`RecipeErrorCode::InvalidSchema`] naming the broken invariant.
    pub fn validate(&self) -> Result<(), RecipeError> {
        if self.name.trim().is_empty() {
            return Err(err(
                RecipeErrorCode::InvalidSchema,
                "schema name cannot be empty",
            ));
        }
        if self
            .name
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'))
        {
            return Err(err(
                RecipeErrorCode::InvalidSchema,
                format!("schema name `{}` must be alphanumeric plus _-. ", self.name),
            ));
        }
        for req in &self.required {
            if !self.args.contains_key(req)
                && !matches!(
                    req.as_str(),
                    "target" | "text" | "value" | "key" | "name" | "binding" | "scope"
                )
            {
                return Err(err(
                    RecipeErrorCode::InvalidSchema,
                    format!(
                        "schema `{}` lists required `{req}` with no args entry",
                        self.name
                    ),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_name() {
        let schema = OpSchema {
            name: String::new(),
            kind: SchemaKind::Protocol,
            description: "x".into(),
            effects: vec![],
            idempotent: true,
            verified: true,
            required: vec![],
            args: BTreeMap::new(),
            result: None,
            keywords: vec![],
        };
        assert!(schema.validate().is_err());
    }

    #[test]
    fn rejects_invalid_schema_name_chars() {
        let schema = OpSchema {
            name: "todo add".into(),
            kind: SchemaKind::Invoke,
            description: "x".into(),
            effects: vec![],
            idempotent: false,
            verified: false,
            required: vec![],
            args: BTreeMap::new(),
            result: None,
            keywords: vec![],
        };
        assert!(schema.validate().is_err());
    }

    #[test]
    fn rejects_required_without_args_entry() {
        let schema = OpSchema {
            name: "custom.op".into(),
            kind: SchemaKind::Invoke,
            description: "x".into(),
            effects: vec![],
            idempotent: false,
            verified: false,
            required: vec!["payload".into()],
            args: BTreeMap::new(),
            result: None,
            keywords: vec![],
        };
        let err = schema.validate().unwrap_err().to_string();
        assert!(err.contains("payload"), "{err}");
    }
}
