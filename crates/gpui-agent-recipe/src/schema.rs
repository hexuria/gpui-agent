use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// TMP-shaped operation contract: effects, required inputs, result hint.
///
/// This is a local mapping schema, not a `tmp-core` dependency. Unknown
/// intents fail closed; nothing here shells out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpSchema {
    pub name: String,
    pub kind: SchemaKind,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<Effect>,
    #[serde(default)]
    pub idempotent: bool,
    #[serde(default)]
    pub verified: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub args: BTreeMap<String, ArgSchema>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaKind {
    /// A first-class protocol op (`click`, `snapshot`, …).
    Protocol,
    /// Host `invoke` name (`todo.add`).
    Invoke,
    /// Stable widget id (`todo-add`).
    Id,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Read,
    Write,
    Exit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgSchema {
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub required: bool,
}

impl OpSchema {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("schema name cannot be empty".into());
        }
        if self
            .name
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'))
        {
            return Err(format!(
                "schema name `{}` must be alphanumeric plus _-. ",
                self.name
            ));
        }
        for req in &self.required {
            if !self.args.contains_key(req) && *req != "target" && *req != "text" && *req != "value"
            {
                return Err(format!(
                    "schema `{}` lists required `{req}` with no args entry",
                    self.name
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
            name: "".into(),
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
}
