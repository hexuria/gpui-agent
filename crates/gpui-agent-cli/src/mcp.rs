use std::io::{BufReader, Write};
use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Result;
use gpui_agent::client::AgentClient;
use gpui_agent::protocol::{AssertSpec, DeliveryMode, Op};
use gpui_agent::{MAX_LINE_BYTES, line_is_blank, read_limited_line_into};
use serde_json::{Value, json};

/// Minimal MCP stdio server: `initialize`, `tools/list`, `tools/call`.
///
/// Tools match the generic protocol ops. App-specific verbs are `invoke`
/// names the host registered — they are not separate MCP tools.
///
/// One [`AgentClient`] lives for the process: protocol `rpc` reuses a single
/// TCP session across `tools/call` (no per-tool reconnect).
pub fn run(addr: SocketAddr, token: Option<String>) -> Result<()> {
    let mut client = AgentClient::connect(addr);
    if let Some(token) = token {
        client = client.with_token(token);
    }

    let mut stdin = BufReader::new(std::io::stdin());
    let mut stdout = std::io::stdout();
    let mut line_buf = Vec::with_capacity(4096);
    loop {
        match read_limited_line_into(&mut stdin, &mut line_buf, MAX_LINE_BYTES)? {
            true => {}
            false => break,
        }
        if line_is_blank(&line_buf) {
            continue;
        }
        let msg: Value = match serde_json::from_slice(&line_buf) {
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
        other => return Err(format!("unknown tool {other}")),
    };
    serde_json::to_value(resp).map_err(|err| err.to_string())
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
                "click",
                "type",
                "set_value",
                "key",
                "assert",
                "invoke",
                "shutdown",
            ]
        );
        assert!(names.iter().all(|n| !n.starts_with("todo")));
    }

    #[test]
    fn click_schema_advertises_delivery() {
        let click = tools().into_iter().find(|t| t["name"] == "click").unwrap();
        assert_eq!(
            click["inputSchema"]["properties"]["delivery"]["enum"],
            json!(["semantic", "virtual"])
        );
    }
}
