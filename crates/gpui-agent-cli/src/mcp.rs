use std::io::{BufReader, Write};
use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Result;
use gpui_agent::client::AgentClient;
use gpui_agent::protocol::{AssertSpec, DeliveryMode, Op};
use gpui_agent::{MAX_LINE_BYTES, read_limited_line};
use gpui_agent_recipe::{
    RunError, ScreenshotCapture, compile_plan, parse_recipe_source, resolve_intent,
    run_plan_with_extras, todo_registry, validate_recipe,
};
use serde_json::{Value, json};

/// Minimal MCP stdio server: `initialize`, `tools/list`, `tools/call`.
///
/// Tools match the generic protocol ops. App-specific verbs are `invoke`
/// names the host registered — they are not separate MCP tools.
pub fn run(addr: SocketAddr, token: Option<String>) -> Result<()> {
    let mut client = AgentClient::connect(addr);
    if let Some(token) = token {
        client = client.with_token(token);
    }

    let mut stdin = BufReader::new(std::io::stdin());
    let mut stdout = std::io::stdout();
    loop {
        let line = match read_limited_line(&mut stdin, MAX_LINE_BYTES)? {
            Some(line) => line,
            None => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(err) => {
                write_msg(
                    &mut stdout,
                    json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":err.to_string()}}),
                )?;
                continue;
            }
        };
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "gpui-agent", "version": "0.1.0" }
            }),
            "notifications/initialized" => {
                continue;
            }
            "tools/list" => json!({ "tools": tools() }),
            "tools/call" => match call_tool(&mut client, &params) {
                Ok(value) => json!({
                    "content": [{ "type": "text", "text": serde_json::to_string_pretty(&value)? }]
                }),
                Err(err) => json!({
                    "isError": true,
                    "content": [{ "type": "text", "text": err }]
                }),
            },
            "ping" => json!({}),
            other => {
                write_msg(
                    &mut stdout,
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("unknown method {other}")}}),
                )?;
                continue;
            }
        };
        write_msg(
            &mut stdout,
            json!({"jsonrpc":"2.0","id":id,"result":result}),
        )?;
    }
    Ok(())
}

fn write_msg(stdout: &mut std::io::Stdout, value: Value) -> Result<()> {
    writeln!(stdout, "{}", serde_json::to_string(&value)?)?;
    stdout.flush()?;
    Ok(())
}

pub(crate) fn tools() -> Vec<Value> {
    vec![
        tool(
            "wait",
            "Block until the host answers hello/ready.",
            json!({
                "type": "object",
                "properties": {
                    "timeout_ms": { "type": "integer", "description": "Client connect retry budget in milliseconds." }
                }
            }),
        ),
        tool(
            "hello",
            "Handshake: protocol version, app name, platform, ready.",
            json!({ "type": "object", "properties": {} }),
        ),
        tool(
            "snapshot",
            "Read the semantic UI tree (stable ids, roles, names, state).",
            json!({ "type": "object", "properties": {} }),
        ),
        tool(
            "screenshot",
            "Observe-only PNG of the app surface (not the full desktop). The host writes `path` on the same machine so the image does not ride NDJSON. Headless returns screenshot_unavailable instead of a fake image.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Destination PNG path on the host machine." }
                },
                "required": ["path"]
            }),
        ),
        tool(
            "click",
            "Activate a widget by stable id. delivery=semantic (default) calls the handler; delivery=virtual synthesizes in-window GPUI pointer events (never OS HID).",
            input_schema_with_delivery(&["target"]),
        ),
        tool(
            "type",
            "Append text to an editable widget. Optional delivery=virtual types through GPUI keystrokes after focusing the target.",
            input_schema_with_delivery(&["target", "text"]),
        ),
        tool(
            "set_value",
            "Replace the value of an editable widget.",
            object_schema(&["target", "value"]),
        ),
        tool(
            "key",
            "Send a key (Enter, Backspace, …) to a widget. Optional delivery=virtual uses GPUI dispatch_keystroke.",
            input_schema_with_delivery(&["target", "key"]),
        ),
        tool(
            "assert",
            "Assert name/value/role/checked/exists on a node from the current snapshot.",
            json!({
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": "Stable node id (alias: id)." },
                    "id": { "type": "string", "description": "Alias for target." },
                    "name": { "type": "string" },
                    "value": { "type": "string" },
                    "role": { "type": "string" },
                    "checked": { "type": "boolean" },
                    "exists": { "type": "boolean" }
                },
                "required": []
            }),
        ),
        tool(
            "invoke",
            "Call a named host command the app registered (app-specific verbs belong here, not as extra tools).",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Host command, e.g. demo.reset" },
                    "args": { "type": "object", "description": "JSON object of arguments." }
                },
                "required": ["name"]
            }),
        ),
        tool(
            "shutdown",
            "Ask the host to exit.",
            json!({ "type": "object", "properties": {} }),
        ),
        tool(
            "recipe_validate",
            "Lint a JSON or line-based recipe of protocol ops. Does not contact the host.",
            json!({
                "type": "object",
                "properties": {
                    "recipe": { "description": "Recipe JSON object or wants text." }
                },
                "required": ["recipe"]
            }),
        ),
        tool(
            "recipe_plan",
            "Compile a recipe to a wave plan (effects, fingerprint). Does not run it.",
            json!({
                "type": "object",
                "properties": {
                    "recipe": { "description": "Recipe JSON object or wants text." },
                    "set": { "type": "object", "description": "Parameter bindings." }
                },
                "required": ["recipe"]
            }),
        ),
        tool(
            "recipe_run",
            "Validate, plan, and execute a recipe on one reused TCP session. Still requires token/caps. Pass yes=true if the plan includes shutdown.",
            json!({
                "type": "object",
                "properties": {
                    "recipe": { "description": "Recipe JSON object or wants text." },
                    "set": { "type": "object", "description": "Parameter bindings." },
                    "yes": { "type": "boolean" },
                    "screenshot_dir": { "type": "string", "description": "After each step (or flagged steps), write an app-surface PNG. Receipt lists paths; headless is screenshot_unavailable." },
                    "screenshot_flagged": { "type": "boolean", "description": "Only steps with screenshot: true." }
                },
                "required": ["recipe"]
            }),
        ),
        tool(
            "recipe_resolve",
            "Map a natural-language intent through the local schema registry (fail closed).",
            json!({
                "type": "object",
                "properties": {
                    "intent": { "type": "string" }
                },
                "required": ["intent"]
            }),
        ),
    ]
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema
    })
}

fn object_schema(required: &[&str]) -> Value {
    let mut properties = serde_json::Map::new();
    for key in required {
        properties.insert((*key).into(), json!({ "type": "string" }));
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required
    })
}

fn input_schema_with_delivery(required: &[&str]) -> Value {
    let mut schema = object_schema(required);
    schema["properties"]["delivery"] = json!({
        "type": "string",
        "enum": ["semantic", "virtual"],
        "description": "semantic (default) calls the widget handler. virtual synthesizes GPUI pointer/key events in-process; never warps the OS cursor."
    });
    schema
}

fn parse_delivery(args: &Value) -> Result<DeliveryMode, String> {
    match args.get("delivery") {
        None => Ok(DeliveryMode::Semantic),
        Some(Value::String(s)) => s.parse(),
        Some(_) => Err("delivery must be a string (`semantic` or `virtual`)".into()),
    }
}

fn call_tool(client: &mut AgentClient, params: &Value) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("missing tool name")?;
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let resp = match name {
        "wait" => {
            if let Some(ms) = args.get("timeout_ms").and_then(Value::as_u64) {
                client.set_timeout(Duration::from_millis(ms));
            }
            client.wait_ready()?
        }
        "hello" => client.expect_ok(Op::Hello)?,
        "snapshot" => client.snapshot()?,
        "screenshot" => client.screenshot(args.string("path")?)?,
        "click" => client.click_with_delivery(args.string("target")?, parse_delivery(&args)?)?,
        "type" => client.type_with_delivery(
            args.string("target")?,
            args.string("text")?,
            parse_delivery(&args)?,
        )?,
        "set_value" => client.set_value(args.string("target")?, args.string("value")?)?,
        "key" => client.key_with_delivery(
            args.string("target")?,
            args.string("key")?,
            parse_delivery(&args)?,
        )?,
        "assert" => {
            let target = args
                .opt_string("target")
                .or_else(|| args.opt_string("id"))
                .ok_or_else(|| "missing string `target`".to_string())?;
            let spec = AssertSpec {
                target,
                name: args.opt_string("name"),
                value: args.opt_string("value"),
                role: args.opt_string("role"),
                checked: args.get("checked").and_then(Value::as_bool),
                exists: args.get("exists").and_then(Value::as_bool),
            };
            client.assert(spec)?
        }
        "invoke" => {
            let invoke_name = args.string("name")?;
            let invoke_args = args.get("args").cloned().unwrap_or(json!({}));
            client.invoke(invoke_name, invoke_args)?
        }
        "shutdown" => client.expect_ok(Op::Shutdown)?,
        "recipe_validate" => return recipe_validate(&args),
        "recipe_plan" => return recipe_plan(&args),
        "recipe_run" => return recipe_run_tool(client, &args),
        "recipe_resolve" => return recipe_resolve_tool(&args),
        other => return Err(format!("unknown tool {other}")),
    };
    serde_json::to_value(resp).map_err(|err| err.to_string())
}

fn recipe_text(args: &Value) -> Result<String, String> {
    match args.get("recipe") {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(other) => serde_json::to_string(other).map_err(|err| err.to_string()),
        None => Err("missing `recipe`".into()),
    }
}

fn recipe_set(args: &Value) -> std::collections::BTreeMap<String, String> {
    let mut map = std::collections::BTreeMap::new();
    if let Some(Value::Object(obj)) = args.get("set") {
        for (k, v) in obj {
            map.insert(
                k.clone(),
                match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                },
            );
        }
    }
    map
}

fn recipe_validate(args: &Value) -> Result<Value, String> {
    let recipe = parse_recipe_source(&recipe_text(args)?, "inline", false)?;
    validate_recipe(&recipe, &todo_registry())?;
    serde_json::to_value(serde_json::json!({
        "ok": true,
        "name": recipe.name,
        "steps": recipe.steps.len(),
    }))
    .map_err(|err| err.to_string())
}

fn recipe_plan(args: &Value) -> Result<Value, String> {
    let recipe = parse_recipe_source(&recipe_text(args)?, "inline", false)?;
    let plan = compile_plan(&recipe, &recipe_set(args), &todo_registry())?;
    serde_json::to_value(plan).map_err(|err| err.to_string())
}

fn recipe_run_tool(client: &mut AgentClient, args: &Value) -> Result<Value, String> {
    let recipe = parse_recipe_source(&recipe_text(args)?, "inline", false)?;
    let plan = compile_plan(&recipe, &recipe_set(args), &todo_registry())?;
    let yes = args.get("yes").and_then(Value::as_bool).unwrap_or(false);
    let flagged = args
        .get("screenshot_flagged")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let dir = args.get("screenshot_dir").and_then(Value::as_str);
    if flagged && dir.is_none() {
        return Err("screenshot_flagged requires screenshot_dir".into());
    }
    let capture = dir.map(|dir| ScreenshotCapture {
        dir: std::path::PathBuf::from(dir),
        flagged_only: flagged,
    });
    match run_plan_with_extras(client, &plan, yes, None, capture.as_ref()) {
        Ok(receipt) => serde_json::to_value(receipt).map_err(|err| err.to_string()),
        Err(RunError::Step { receipt, error, .. }) => {
            let mut value = serde_json::to_value(receipt).map_err(|err| err.to_string())?;
            value["error"] = json!(error);
            Ok(value)
        }
        Err(err) => Err(err.to_string()),
    }
}

fn recipe_resolve_tool(args: &Value) -> Result<Value, String> {
    let intent = args
        .get("intent")
        .and_then(Value::as_str)
        .ok_or("missing string `intent`")?;
    let result = resolve_intent(intent, &todo_registry())?;
    serde_json::to_value(result).map_err(|err| err.to_string())
}

trait ArgsExt {
    fn string(&self, key: &str) -> Result<String, String>;
    fn opt_string(&self, key: &str) -> Option<String>;
}

impl ArgsExt for Value {
    fn string(&self, key: &str) -> Result<String, String> {
        self.get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("missing string `{key}`"))
    }

    fn opt_string(&self, key: &str) -> Option<String> {
        self.get(key).and_then(Value::as_str).map(str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_tools_are_generic_protocol_ops() {
        let listed = tools();
        let names: Vec<&str> = listed
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        assert_eq!(
            names,
            [
                "wait",
                "hello",
                "snapshot",
                "screenshot",
                "click",
                "type",
                "set_value",
                "key",
                "assert",
                "invoke",
                "shutdown",
                "recipe_validate",
                "recipe_plan",
                "recipe_run",
                "recipe_resolve",
            ]
        );
        assert!(names.iter().all(|n| !n.starts_with("todo_")));
    }

    #[test]
    fn click_schema_advertises_delivery() {
        let click = tools().into_iter().find(|t| t["name"] == "click").unwrap();
        assert_eq!(
            click["inputSchema"]["properties"]["delivery"]["enum"],
            json!(["semantic", "virtual"])
        );
    }

    #[test]
    fn recipe_validate_unknown_invoke_fails_closed() {
        let err = recipe_validate(&json!({
            "recipe": {
                "name": "bad",
                "steps": [{"id": "x", "op": "invoke", "name": "shell.run"}]
            }
        }))
        .unwrap_err();
        assert!(err.contains("unknown invoke"), "{err}");
    }

    #[test]
    fn recipe_validate_empty_and_bad_json_fail_closed() {
        let err = recipe_validate(&json!({ "recipe": "" })).unwrap_err();
        assert!(err.contains("no steps"), "{err}");
        let err = recipe_validate(&json!({ "recipe": "{" })).unwrap_err();
        assert!(err.contains("recipe json"), "{err}");
    }

    #[test]
    fn recipe_resolve_shell_like_fails_closed() {
        let err = recipe_resolve_tool(&json!({ "intent": "rm -rf /" })).unwrap_err();
        assert!(err.contains("fail closed"), "{err}");
    }

    #[test]
    fn recipe_run_screenshot_flagged_requires_dir() {
        let mut client = AgentClient::connect("127.0.0.1:1".parse().unwrap());
        let err = recipe_run_tool(
            &mut client,
            &json!({
                "recipe": "hello",
                "screenshot_flagged": true
            }),
        )
        .unwrap_err();
        assert!(err.contains("screenshot_dir"), "{err}");
    }

    #[test]
    fn recipe_run_shutdown_requires_yes() {
        let mut client = AgentClient::connect("127.0.0.1:1".parse().unwrap());
        let err = recipe_run_tool(
            &mut client,
            &json!({
                "recipe": "hello\nshutdown",
                "yes": false
            }),
        )
        .unwrap_err();
        assert!(err.contains("yes"), "{err}");
    }

    #[test]
    fn unknown_mcp_tool_fails_closed() {
        let mut client = AgentClient::connect("127.0.0.1:1".parse().unwrap());
        let err =
            call_tool(&mut client, &json!({ "name": "todo.add", "arguments": {} })).unwrap_err();
        assert!(err.contains("unknown tool"), "{err}");
    }
}
