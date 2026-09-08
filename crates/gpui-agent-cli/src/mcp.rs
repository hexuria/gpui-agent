use std::io::{BufRead, Write};
use std::net::SocketAddr;

use anyhow::Result;
use gpui_agent::client::AgentClient;
use gpui_agent::protocol::{AssertSpec, Op};
use serde_json::{Value, json};

/// Minimal MCP stdio server: `initialize`, `tools/list`, `tools/call`.
pub fn run(addr: SocketAddr, token: Option<String>) -> Result<()> {
    let mut client = AgentClient::connect(addr);
    if let Some(token) = token {
        client = client.with_token(token);
    }

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(err) => {
                write_msg(&mut stdout, json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":err.to_string()}}))?;
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
        write_msg(&mut stdout, json!({"jsonrpc":"2.0","id":id,"result":result}))?;
    }
    Ok(())
}

fn write_msg(stdout: &mut std::io::Stdout, value: Value) -> Result<()> {
    writeln!(stdout, "{}", serde_json::to_string(&value)?)?;
    stdout.flush()?;
    Ok(())
}

fn tools() -> Vec<Value> {
    vec![
        tool("snapshot", "Read the semantic UI tree (ids, roles, names, state)."),
        tool("click", "Click a widget by stable id."),
        tool("set_value", "Set the value of an editable widget."),
        tool("assert", "Assert name/value/checked/exists on a node."),
        tool("todo_add", "Create a todo by title."),
        tool("todo_toggle", "Toggle a todo by numeric id."),
        tool("todo_delete", "Delete a todo by numeric id."),
        tool("todo_list", "List todos from the host."),
    ]
}

fn tool(name: &str, description: &str) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": { "type": "object", "additionalProperties": true }
    })
}

fn call_tool(client: &mut AgentClient, params: &Value) -> Result<Value, String> {
    let name = params.get("name").and_then(Value::as_str).ok_or("missing tool name")?;
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let resp = match name {
        "snapshot" => client.snapshot()?,
        "click" => {
            let target = args.string("target")?;
            client.click(target)?
        }
        "set_value" => {
            client.set_value(args.string("target")?, args.string("value")?)?
        }
        "assert" => {
            let spec = AssertSpec {
                target: args.string("target")?,
                name: args.opt_string("name"),
                value: args.opt_string("value"),
                role: args.opt_string("role"),
                checked: args.get("checked").and_then(Value::as_bool),
                exists: args.get("exists").and_then(Value::as_bool),
            };
            client.assert(spec)?
        }
        "todo_add" => client.invoke("todo.add", json!({ "title": args.string("title")? }))?,
        "todo_toggle" => client.invoke("todo.toggle", json!({ "id": args.id()? }))?,
        "todo_delete" => client.invoke("todo.delete", json!({ "id": args.id()? }))?,
        "todo_list" => client.invoke("todo.list", json!({}))?,
        "hello" => client.expect_ok(Op::Hello)?,
        other => return Err(format!("unknown tool {other}")),
    };
    serde_json::to_value(resp).map_err(|err| err.to_string())
}

trait ArgsExt {
    fn string(&self, key: &str) -> Result<String, String>;
    fn opt_string(&self, key: &str) -> Option<String>;
    fn id(&self) -> Result<u64, String>;
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

    fn id(&self) -> Result<u64, String> {
        self.get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| "missing id".into())
    }
}
