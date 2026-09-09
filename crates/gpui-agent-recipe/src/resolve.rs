use gpui_agent::protocol::{AssertSpec, Op};
use serde::Serialize;
use serde_json::{Value, json};

use crate::registry::Registry;
use crate::schema::{Effect, SchemaKind};

#[derive(Debug, Clone, Serialize)]
pub struct TokenFill {
    pub name: String,
    pub value: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolveResult {
    pub schema: String,
    pub op: Op,
    pub effects: Vec<Effect>,
    pub confidence: f32,
    pub tokens_filled: Vec<TokenFill>,
    pub idempotent: bool,
}

/// Deterministic, fail-closed intent → schema-backed op.
///
/// No model, no shell. Unknown or ambiguous intents return `Err`.
pub fn resolve_intent(intent: &str, registry: &Registry) -> Result<ResolveResult, String> {
    let trimmed = intent.trim();
    if trimmed.is_empty() {
        return Err("empty intent".into());
    }

    let words = words_of(trimmed);
    let mut scored: Vec<(&str, i32)> = registry
        .iter()
        .map(|schema| (schema.name.as_str(), score(trimmed, &words, schema)))
        .filter(|(_, s)| *s > 0)
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));

    let Some((name, best)) = scored.first().copied() else {
        return Err(format!("unknown intent (fail closed): {trimmed}"));
    };
    if best < 2 {
        return Err(format!("unknown intent (fail closed): {trimmed}"));
    }
    if scored.len() >= 2 && scored[1].1 >= best {
        return Err(format!(
            "ambiguous intent (fail closed): `{trimmed}` matches `{}` and `{}`",
            scored[0].0, scored[1].0
        ));
    }

    let schema = registry.get(name).expect("scored name");
    let (op, tokens_filled) = materialize(trimmed, schema)?;
    Ok(ResolveResult {
        schema: schema.name.clone(),
        op,
        effects: schema.effects.clone(),
        confidence: (best as f32 / 10.0).min(1.0),
        tokens_filled,
        idempotent: schema.idempotent,
    })
}

fn score(intent: &str, words: &[String], schema: &crate::schema::OpSchema) -> i32 {
    let mut score = 0;
    let lower = intent.to_ascii_lowercase();
    if lower.contains(&schema.name.to_ascii_lowercase()) {
        score += 10;
    }
    for keyword in &schema.keywords {
        if words.iter().any(|w| w == keyword) {
            score += 2;
        }
    }
    for word in schema
        .description
        .split(|c: char| !c.is_ascii_alphanumeric())
    {
        let word = word.to_ascii_lowercase();
        if word.len() > 2 && words.iter().any(|w| w == &word) {
            score += 1;
        }
    }
    score
}

fn words_of(intent: &str) -> Vec<String> {
    intent
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '.')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect()
}

fn materialize(
    intent: &str,
    schema: &crate::schema::OpSchema,
) -> Result<(Op, Vec<TokenFill>), String> {
    let mut fills = Vec::new();
    match schema.kind {
        SchemaKind::Protocol => match schema.name.as_str() {
            "hello" => Ok((Op::Hello, fills)),
            "wait" => Ok((Op::Wait { timeout_ms: None }, fills)),
            "snapshot" => Ok((Op::Snapshot, fills)),
            "screenshot" => Ok((Op::Screenshot { path: None }, fills)),
            "shutdown" => Ok((Op::Shutdown, fills)),
            "click" => {
                let target = required_target(intent, &mut fills)?;
                Ok((Op::click(target), fills))
            }
            "set_value" => {
                let target = required_target(intent, &mut fills)?;
                let value = extract_title(intent).ok_or("set_value needs a quoted value")?;
                fills.push(TokenFill {
                    name: "value".into(),
                    value: value.clone(),
                    source: "quoted".into(),
                });
                Ok((Op::SetValue { target, value }, fills))
            }
            "assert" => {
                let target = required_target(intent, &mut fills)?;
                Ok((
                    Op::Assert {
                        spec: AssertSpec {
                            target,
                            ..Default::default()
                        },
                    },
                    fills,
                ))
            }
            other => Err(format!(
                "resolved `{other}` but do not know how to materialize it from prose"
            )),
        },
        SchemaKind::Invoke => {
            let mut args = serde_json::Map::new();
            if schema.required.iter().any(|r| r == "title") {
                let title = extract_title(intent).ok_or_else(|| {
                    format!(
                        "{} requires title (quote it or write titled …)",
                        schema.name
                    )
                })?;
                fills.push(TokenFill {
                    name: "title".into(),
                    value: title.clone(),
                    source: "quoted_or_titled".into(),
                });
                args.insert("title".into(), Value::String(title));
            }
            if schema.required.iter().any(|r| r == "id") {
                let id =
                    extract_id(intent).ok_or_else(|| format!("{} requires id", schema.name))?;
                fills.push(TokenFill {
                    name: "id".into(),
                    value: id.to_string(),
                    source: "number".into(),
                });
                args.insert("id".into(), json!(id));
            }
            Ok((
                Op::Invoke {
                    name: schema.name.clone(),
                    args: Value::Object(args),
                },
                fills,
            ))
        }
        SchemaKind::Id => {
            fills.push(TokenFill {
                name: "target".into(),
                value: schema.name.clone(),
                source: "schema".into(),
            });
            Ok((Op::click(schema.name.clone()), fills))
        }
    }
}

fn required_target(intent: &str, fills: &mut Vec<TokenFill>) -> Result<String, String> {
    let target = extract_id_token(intent).ok_or("click/assert needs a stable id")?;
    fills.push(TokenFill {
        name: "target".into(),
        value: target.clone(),
        source: "token".into(),
    });
    Ok(target)
}

fn extract_id_token(intent: &str) -> Option<String> {
    intent.split_whitespace().find_map(|tok| {
        let tok = tok.trim_matches(|c: char| c == ',' || c == '.');
        if tok.contains('-') && tok.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            Some(tok.to_string())
        } else {
            None
        }
    })
}

fn extract_title(intent: &str) -> Option<String> {
    if let Some(quoted) = first_quoted(intent) {
        return Some(quoted);
    }
    let lower = intent.to_ascii_lowercase();
    for marker in ["titled ", "called ", "named "] {
        if let Some(idx) = lower.find(marker) {
            let rest = intent[idx + marker.len()..].trim();
            if let Some(quoted) = first_quoted(rest) {
                return Some(quoted);
            }
            if !rest.is_empty() {
                return Some(
                    rest.trim_matches(|c: char| c == '.' || c == ',')
                        .to_string(),
                );
            }
        }
    }
    None
}

fn first_quoted(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' || bytes[i] == b'\'' {
            let quote = bytes[i];
            let start = i + 1;
            i += 1;
            while i < bytes.len() && bytes[i] != quote {
                i += 1;
            }
            if i < bytes.len() {
                return Some(input[start..i].to_string());
            }
            return None;
        }
        i += 1;
    }
    None
}

fn extract_id(intent: &str) -> Option<u64> {
    let lower = intent.to_ascii_lowercase();
    if let Some(idx) = lower.find("id") {
        let rest = intent[idx + 2..].trim_start_matches([' ', '=', '#', ':']);
        let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !num.is_empty() {
            return num.parse().ok();
        }
    }
    intent
        .split_whitespace()
        .rev()
        .find_map(|tok| tok.trim_start_matches('#').parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::todo_registry;

    #[test]
    fn resolves_todo_add() {
        let result = resolve_intent("add a todo titled Buy milk", &todo_registry()).unwrap();
        assert_eq!(result.schema, "todo.add");
        match result.op {
            Op::Invoke { name, args } => {
                assert_eq!(name, "todo.add");
                assert_eq!(args["title"], "Buy milk");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn resolves_quoted_title() {
        let result = resolve_intent(r#"todo.add "Write docs""#, &todo_registry()).unwrap();
        assert_eq!(result.schema, "todo.add");
    }

    #[test]
    fn resolves_toggle_id() {
        let result = resolve_intent("toggle todo id 1", &todo_registry()).unwrap();
        assert_eq!(result.schema, "todo.toggle");
        match result.op {
            Op::Invoke { args, .. } => assert_eq!(args["id"], 1),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unknown_intent_fails_closed() {
        let err = resolve_intent("rm -rf /", &todo_registry()).unwrap_err();
        assert!(err.contains("fail closed"), "{err}");
    }

    #[test]
    fn snapshot_intent() {
        let result = resolve_intent("take a snapshot of the tree", &todo_registry()).unwrap();
        assert_eq!(result.schema, "snapshot");
        assert!(result.effects.contains(&Effect::Read));
    }

    #[test]
    fn screenshot_intent() {
        let result = resolve_intent("screenshot png frame", &todo_registry()).unwrap();
        assert_eq!(result.schema, "screenshot");
        assert!(matches!(result.op, Op::Screenshot { .. }));
    }

    #[test]
    fn empty_intent_fails_closed() {
        let err = resolve_intent("   ", &todo_registry()).unwrap_err();
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn shell_like_intents_fail_closed() {
        for intent in [
            "rm -rf /",
            "curl http://evil.example/x",
            "bash -c 'reboot'",
            "frobnicate xyzzy",
        ] {
            let err = resolve_intent(intent, &todo_registry()).unwrap_err();
            assert!(err.contains("fail closed"), "{intent}: {err}");
        }
    }

    #[test]
    fn ambiguous_short_intent_fails_closed() {
        // `todo` ties todo.add / todo.toggle / todo.delete at the same score.
        let err = resolve_intent("todo", &todo_registry()).unwrap_err();
        assert!(err.contains("ambiguous"), "{err}");
    }

    #[test]
    fn invoke_without_required_title_fails() {
        let err = resolve_intent("todo.add", &todo_registry()).unwrap_err();
        assert!(err.contains("title"), "{err}");
    }

    #[test]
    fn invoke_without_required_id_fails() {
        let err = resolve_intent("todo.toggle", &todo_registry()).unwrap_err();
        assert!(err.contains("id"), "{err}");
    }

    #[test]
    fn click_without_stable_id_fails() {
        let err = resolve_intent("click the button", &todo_registry()).unwrap_err();
        assert!(
            err.contains("stable id") || err.contains("fail closed") || err.contains("ambiguous"),
            "{err}"
        );
    }

    #[test]
    fn type_and_key_intents_do_not_materialize_from_prose() {
        for intent in ["type", "key"] {
            let err = resolve_intent(intent, &todo_registry()).unwrap_err();
            assert!(
                err.contains("materialize")
                    || err.contains("fail closed")
                    || err.contains("ambiguous"),
                "{intent}: {err}"
            );
        }
    }
}
