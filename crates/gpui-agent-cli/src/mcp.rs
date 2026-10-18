use std::io::{BufReader, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use gpui_agent::client::AgentClient;
use gpui_agent::protocol::{AssertSpec, DeliveryMode, KeybindingScope, Op};
use gpui_agent::{MAX_LINE_BYTES, line_is_blank, read_limited_line_into};
use gpui_agent_recipe::{
    RunError, ScreenshotCapture, compile_plan, parse_recipe_source, registry_from_schema_paths,
    resolve_intent, run_plan_with_screenshots, validate_recipe,
};
use serde_json::{Value, json};

/// Minimal MCP stdio server: `initialize`, `tools/list`, `tools/call`.
///
/// Tools match the generic protocol ops. App-specific verbs are `invoke`
/// names the host registered — they are not separate MCP tools.
///
/// One [`AgentClient`] lives for the process: protocol `rpc` reuses a single
/// TCP session across `tools/call` (no per-tool reconnect).
pub fn run(addr: SocketAddr, token: String, schema_paths: Vec<PathBuf>) -> Result<()> {
    let mut client = AgentClient::connect(addr).with_token(token);

    let mut stdin = BufReader::new(std::io::stdin());
    let mut stdout = std::io::stdout();
    let mut line_buf = Vec::with_capacity(4096);
    while read_limited_line_into(&mut stdin, &mut line_buf, MAX_LINE_BYTES)? {
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
            "tools/call" => tools_call_result(&mut client, &params, &schema_paths),
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
            "wait_until",
            "Poll assert fields on a snapshot until they match or timeout_ms elapses. Distinct from wait (ready/paint only). in_viewport fails closed (in_viewport_unavailable) when bounds are zero or the host is headless.",
            json!({
                "type": "object",
                "properties": {
                    "timeout_ms": { "type": "integer", "description": "Host poll budget in milliseconds." },
                    "target": { "type": "string", "description": "Stable node id (alias: id)." },
                    "id": { "type": "string", "description": "Alias for target." },
                    "name": { "type": "string" },
                    "value": { "type": "string" },
                    "role": { "type": "string" },
                    "checked": { "type": "boolean" },
                    "exists": { "type": "boolean" },
                    "visible": { "type": "boolean", "description": "Host-declared visibility (not exists, not in_viewport)." },
                    "in_viewport": { "type": "boolean", "description": "Non-empty intersection with the painted window clip." }
                },
                "required": ["timeout_ms"]
            }),
        ),
        tool(
            "hello",
            "Handshake: protocol version, app name, platform, ready.",
            json!({ "type": "object", "properties": {} }),
        ),
        tool(
            "snapshot",
            "Read the semantic UI tree (stable ids, roles, names, state). Pass since=<rev> for a diff against a revision you already hold, format=compact for one line per visible node.",
            json!({
                "type": "object",
                "properties": {
                    "since": { "type": "integer", "description": "Return only what changed since this snapshot revision." },
                    "format": { "type": "string", "enum": ["json", "compact"], "description": "json (default) is the full UiTree; compact is one line per visible node." },
                    "fingerprints": { "type": "boolean", "description": "true adds an id→fingerprint map so callers can gate mutating ops with expect_fingerprint." }
                }
            }),
        ),
        tool(
            "screenshot",
            "Observe-only PNG of the app surface (not the full desktop). The host writes `path` on the same machine so the image does not ride NDJSON. Headless / Linux / Windows return screenshot_unavailable. macOS desktop writes this window via screencapture -l (Screen Recording permission). mode=scrolled requires target (named scroller) and stitches tiles; default mode is viewport.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Destination PNG path on the host machine." },
                    "mode": { "type": "string", "enum": ["viewport", "scrolled"], "description": "viewport (default) captures the window. scrolled scrolls target and stitches tiles." },
                    "target": { "type": "string", "description": "Stable scroll-view id. Required when mode=scrolled." },
                    "max_height_px": { "type": "integer", "description": "Safety cap for scrolled capture (max 16384)." }
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
            "Send a key (Enter, Backspace, …) to a widget. No modifiers. Optional delivery=virtual uses GPUI dispatch_keystroke. Modifier chords use the keybinding tool.",
            input_schema_with_delivery(&["target", "key"]),
        ),
        tool(
            "keybinding",
            "Fire a GPUI Action by stable id (the same handler the keymap uses). Never OS HID. scope=focused requires the app window to be focused unless activate=true. scope=global is this app’s global map only. Destructive ids (app.quit) require confirm=true.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Action id (protocol field: binding). Alias: binding." },
                    "binding": { "type": "string", "description": "Alias for id." },
                    "scope": { "type": "string", "enum": ["focused", "global"] },
                    "chord": { "type": "string", "description": "Optional chord metadata / resolve aid." },
                    "confirm": { "type": "boolean", "description": "Required for dangerous Actions such as app.quit." },
                    "activate": { "type": "boolean", "description": "scope=focused only: host-activate this window first." }
                },
                "required": ["scope"]
            }),
        ),
        tool(
            "keybindings",
            "List registered Action-id keybindings: { id, chord, scope, dangerous }.",
            json!({ "type": "object", "properties": {} }),
        ),
        tool(
            "assert",
            "Assert name/value/role/checked/exists/visible/in_viewport on a node from the current snapshot. visible is host-declared; in_viewport is geometry and fails closed when bounds are dishonest.",
            json!({
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": "Stable node id (alias: id)." },
                    "id": { "type": "string", "description": "Alias for target." },
                    "name": { "type": "string" },
                    "value": { "type": "string" },
                    "role": { "type": "string" },
                    "checked": { "type": "boolean" },
                    "exists": { "type": "boolean" },
                    "visible": { "type": "boolean", "description": "Host-declared visibility." },
                    "in_viewport": { "type": "boolean", "description": "Non-empty intersection with the painted window clip." }
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
            "[experimental] Lint a JSON recipe of protocol ops (`.wants` text also accepted). Does not contact the host.",
            json!({
                "type": "object",
                "properties": {
                    "recipe": { "description": "JSON recipe object (canonical) or wants text." },
                    "schema": schema_arg()
                },
                "required": ["recipe"]
            }),
        ),
        tool(
            "recipe_plan",
            "[experimental] Compile a JSON recipe to a wave plan (effects, fingerprint). Does not run it. `.wants` text also accepted.",
            json!({
                "type": "object",
                "properties": {
                    "recipe": { "description": "JSON recipe object (canonical) or wants text." },
                    "set": { "type": "object", "description": "Parameter bindings." },
                    "schema": schema_arg()
                },
                "required": ["recipe"]
            }),
        ),
        tool(
            "recipe_run",
            "[experimental] Validate, plan, and execute a JSON recipe sequentially on one reused TCP session. `.wants` also accepted. This MCP process already required a token; each step still carries it. Pass yes=true if the plan includes shutdown.",
            json!({
                "type": "object",
                "properties": {
                    "recipe": { "description": "JSON recipe object (canonical) or wants text." },
                    "set": { "type": "object", "description": "Parameter bindings." },
                    "yes": { "type": "boolean" },
                    "screenshot_dir": { "type": "string", "description": "After each step (or flagged steps), write an app-surface PNG. Receipt lists paths; headless/Linux/Windows are screenshot_unavailable; macOS desktop uses screencapture -l of this window." },
                    "screenshot_flagged": { "type": "boolean", "description": "Only steps with screenshot: true." },
                    "schema": schema_arg()
                },
                "required": ["recipe"]
            }),
        ),
        tool(
            "recipe_resolve",
            "[experimental] Map a natural-language intent through the local schema registry (fail closed).",
            json!({
                "type": "object",
                "properties": {
                    "intent": { "type": "string" },
                    "schema": schema_arg()
                },
                "required": ["intent"]
            }),
        ),
        #[cfg(feature = "instinct")]
        tool(
            "locate",
            "[experimental, instinct] Read-only: decide which finite action best matches a natural-language query. Prefer `act` with a plain instruction; use `snapshot format=compact since=<rev>` to look.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "k": { "type": "integer", "description": "Max candidates to return (default 5)." },
                    "schema": schema_arg()
                },
                "required": ["query"]
            }),
        ),
        #[cfg(feature = "instinct")]
        tool(
            "act",
            "[experimental, instinct] Execute ONE clause of a natural-language instruction against the observed UI. Abstains (never dispatches) when evidence is insufficient or the action is destructive. Replies {done:{op,id,text?},rev,diff} or {abstain:{reason,candidates},rev}. Prefer this over manual snapshot+click; use `snapshot format=compact since=<rev>` to look.",
            json!({
                "type": "object",
                "properties": {
                    "instruction": { "type": "string", "description": "One clause, e.g. `click Add` or `type \"milk\" into What needs doing?`. `then`/`and then` chains run the first clause only." },
                    "k": { "type": "integer", "description": "Max candidates in an abstain reply (default 5)." },
                    "delivery": { "type": "string", "enum": ["semantic", "virtual"] },
                    "schema": schema_arg()
                },
                "required": ["instruction"]
            }),
        ),
    ]
}

fn schema_arg() -> Value {
    json!({
        "description": "App invoke/id schema JSON path(s). Same as CLI `--schema` / `GPUI_AGENT_SCHEMA`.",
        "oneOf": [
            { "type": "string" },
            { "type": "array", "items": { "type": "string" } }
        ]
    })
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

fn parse_screenshot_mode(args: &Value) -> Result<gpui_agent::ScreenshotMode, String> {
    match args.get("mode") {
        None => Ok(gpui_agent::ScreenshotMode::Viewport),
        Some(Value::String(s)) => s.parse(),
        Some(_) => Err("mode must be a string (`viewport` or `scrolled`)".into()),
    }
}

fn call_tool(
    client: &mut AgentClient,
    params: &Value,
    schema_paths: &[PathBuf],
) -> Result<Value, String> {
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
        "wait_until" => {
            let timeout_ms = args
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .ok_or_else(|| "missing integer `timeout_ms`".to_string())?;
            let spec = parse_assert_spec(&args)?;
            client.wait_until(spec, timeout_ms)?
        }
        "hello" => client.expect_ok(Op::Hello)?,
        "snapshot" => {
            let since = args.get("since").and_then(Value::as_u64);
            let format = match args.get("format").and_then(Value::as_str) {
                None => None,
                Some("json") => Some(gpui_agent::protocol::SnapshotFormat::Json),
                Some("compact") => Some(gpui_agent::protocol::SnapshotFormat::Compact),
                Some(other) => return Err(format!("unknown snapshot format {other}")),
            };
            let fingerprints = args.get("fingerprints").and_then(Value::as_bool);
            if fingerprints == Some(true) {
                client.expect_ok(Op::Snapshot {
                    since,
                    format,
                    fingerprints,
                })?
            } else {
                client.snapshot_with(since, format)?
            }
        }
        "screenshot" => {
            let path = args.string("path")?;
            let mode = parse_screenshot_mode(&args)?;
            let target = args.opt_string("target");
            let max_height_px = args
                .get("max_height_px")
                .and_then(Value::as_u64)
                .map(|n| n as u32);
            if mode.is_scrolled() && target.as_deref().map(str::trim).unwrap_or("").is_empty() {
                return Err("screenshot mode=scrolled requires target".into());
            }
            client.screenshot_with(path, mode, target, max_height_px)?
        }
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
        "keybinding" => {
            let binding = args
                .opt_string("id")
                .or_else(|| args.opt_string("binding"))
                .ok_or_else(|| "missing string `id`".to_string())?;
            let scope: KeybindingScope =
                args.string("scope")?.parse().map_err(|err: String| err)?;
            let chord = args.opt_string("chord");
            let confirm = args
                .get("confirm")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let activate = args
                .get("activate")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            client.keybinding(binding, scope, chord, confirm, activate)?
        }
        "keybindings" | "keybinding.list" => client.keybindings()?,
        "assert" => {
            let spec = parse_assert_spec(&args)?;
            client.assert(spec)?
        }
        "invoke" => {
            let invoke_name = args.string("name")?;
            let invoke_args = args.get("args").cloned().unwrap_or(json!({}));
            client.invoke(invoke_name, invoke_args)?
        }
        "shutdown" => client.expect_ok(Op::Shutdown)?,
        "recipe_validate" => return recipe_validate(&args, schema_paths),
        "recipe_plan" => return recipe_plan(&args, schema_paths),
        "recipe_run" => return recipe_run_tool(client, &args, schema_paths),
        "recipe_resolve" => return recipe_resolve_tool(&args, schema_paths),
        #[cfg(feature = "instinct")]
        "locate" => return instinct::locate_tool(client, &args, schema_paths),
        #[cfg(feature = "instinct")]
        "act" => return instinct::act_tool(client, &args, schema_paths),
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

fn recipe_schema_paths(cli: &[PathBuf], args: &Value) -> Vec<PathBuf> {
    let mut paths = cli.to_vec();
    match args.get("schema") {
        Some(Value::Array(arr)) => {
            for v in arr {
                if let Some(s) = v.as_str() {
                    paths.push(PathBuf::from(s));
                }
            }
        }
        Some(Value::String(s)) => paths.push(PathBuf::from(s)),
        _ => {}
    }
    paths
}

fn recipe_validate(args: &Value, schema_paths: &[PathBuf]) -> Result<Value, String> {
    let recipe = parse_recipe_source(&recipe_text(args)?, "inline", false)?;
    validate_recipe(
        &recipe,
        &registry_from_schema_paths(&recipe_schema_paths(schema_paths, args))?,
    )?;
    serde_json::to_value(serde_json::json!({
        "ok": true,
        "name": recipe.name,
        "steps": recipe.steps.len(),
    }))
    .map_err(|err| err.to_string())
}

fn recipe_plan(args: &Value, schema_paths: &[PathBuf]) -> Result<Value, String> {
    let recipe = parse_recipe_source(&recipe_text(args)?, "inline", false)?;
    let plan = compile_plan(
        &recipe,
        &recipe_set(args),
        &registry_from_schema_paths(&recipe_schema_paths(schema_paths, args))?,
    )?;
    serde_json::to_value(plan).map_err(|err| err.to_string())
}

fn recipe_run_tool(
    client: &mut AgentClient,
    args: &Value,
    schema_paths: &[PathBuf],
) -> Result<Value, String> {
    let recipe = parse_recipe_source(&recipe_text(args)?, "inline", false)?;
    let plan = compile_plan(
        &recipe,
        &recipe_set(args),
        &registry_from_schema_paths(&recipe_schema_paths(schema_paths, args))?,
    )?;
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
    match run_plan_with_screenshots(client, &plan, yes, capture.as_ref()) {
        Ok(receipt) => serde_json::to_value(receipt).map_err(|err| err.to_string()),
        Err(RunError::Step { receipt, error, .. }) => {
            let mut value = serde_json::to_value(*receipt).map_err(|err| err.to_string())?;
            value["error"] = json!(error);
            Err(serde_json::to_string(&value).unwrap_or(error))
        }
        Err(err) => Err(err.to_string()),
    }
}

/// MCP `tools/call` result. Failed recipe steps must be `isError` so a
/// host does not treat `"ok": false` inside a successful tool payload as OK.
fn tools_call_result(client: &mut AgentClient, params: &Value, schema_paths: &[PathBuf]) -> Value {
    match call_tool(client, params, schema_paths) {
        Ok(value) => json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&value).unwrap_or_else(|err| err.to_string())
            }]
        }),
        Err(err) => json!({
            "isError": true,
            "content": [{ "type": "text", "text": err }]
        }),
    }
}

fn recipe_resolve_tool(args: &Value, schema_paths: &[PathBuf]) -> Result<Value, String> {
    let intent = args
        .get("intent")
        .and_then(Value::as_str)
        .ok_or("missing string `intent`")?;
    let result = resolve_intent(
        intent,
        &registry_from_schema_paths(&recipe_schema_paths(schema_paths, args))?,
    )?;
    serde_json::to_value(result).map_err(|err| err.to_string())
}

fn parse_assert_spec(args: &Value) -> Result<AssertSpec, String> {
    let target = args
        .opt_string("target")
        .or_else(|| args.opt_string("id"))
        .ok_or_else(|| "missing string `target`".to_string())?;
    Ok(AssertSpec {
        target,
        name: args.opt_string("name"),
        value: args.opt_string("value"),
        role: args.opt_string("role"),
        checked: args.get("checked").and_then(Value::as_bool),
        exists: args.get("exists").and_then(Value::as_bool),
        visible: args.get("visible").and_then(Value::as_bool),
        in_viewport: args.get("in_viewport").and_then(Value::as_bool),
    })
}

/// M4 `locate`/`act` (feature `instinct`): observe → finite
/// [`ActionSpace`] → [`InstinctPolicy`] decide → revalidate → dispatch.
/// Abstention is a value: `act` never dispatches after an abstain.
#[cfg(feature = "instinct")]
mod instinct {
    use super::*;
    use gpui_agent::dispatch::is_stale_dispatch_error;
    use gpui_agent::keybinding::KeybindingInfo;
    use gpui_agent_policy::{
        ActionKind, ActionSpace, DeterministicTextResolver, InstinctPolicy, OfferedInvoke,
        PolicyOutcome, RankedAction, TextContext, TextError, TextResolver, UiGoal, UiPolicy,
        op_for_decision, split_sequential_clauses, top_candidates,
    };
    use gpui_agent_recipe::invoke_args;

    /// One observation + decision. Returned so `act` can revalidate and
    /// dispatch exactly once.
    struct Observed {
        rev: Option<u64>,
        keybindings: Vec<KeybindingInfo>,
        space: ActionSpace,
    }

    fn offered_invokes(
        args: &Value,
        schema_paths: &[PathBuf],
    ) -> Result<Vec<OfferedInvoke>, String> {
        let paths = recipe_schema_paths(schema_paths, args);
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let registry = registry_from_schema_paths(&paths)?;
        Ok(crate::instinct_wiring::offered_invokes(&registry))
    }

    fn observe(
        client: &mut AgentClient,
        args: &Value,
        schema_paths: &[PathBuf],
    ) -> Result<Observed, String> {
        let snap = client.snapshot()?;
        let tree = snap
            .tree
            .ok_or_else(|| "host returned no ui tree".to_string())?;
        let rev = snap.rev.or(tree.rev);
        let keybindings = client.keybinding_catalog()?;
        let invokes = offered_invokes(args, schema_paths)?;
        let space = ActionSpace::from_tree(&tree, &keybindings, &invokes);
        Ok(Observed {
            rev,
            keybindings,
            space,
        })
    }

    fn decide(
        obs: &Observed,
        clause: &str,
        history: &[gpui_agent_policy::HistoryEntry],
    ) -> Result<PolicyOutcome, String> {
        let goal = UiGoal::new(clause);
        InstinctPolicy::default()
            .decide(&obs.space, &goal, history)
            .map_err(|err| err.to_string())
    }

    /// Same rows as `top_candidates`, serialized under the MCP `act`
    /// envelope's key names (`confidence`, role always present).
    fn candidates_json(obs: &Observed, ranked: &[RankedAction], k: usize) -> Value {
        Value::Array(
            top_candidates(&obs.space, ranked, k)
                .iter()
                .map(|c| {
                    json!({
                        "id": c.id,
                        "op": c.op,
                        "role": c.role,
                        "name": c.name,
                        "confidence": c.confidence_millis,
                    })
                })
                .collect(),
        )
    }

    /// `locate {query, k?=5, schema?}` — read-only decide. Never
    /// dispatches anything.
    pub(crate) fn locate_tool(
        client: &mut AgentClient,
        args: &Value,
        schema_paths: &[PathBuf],
    ) -> Result<Value, String> {
        let query = args.string("query")?;
        let k = args.get("k").and_then(Value::as_u64).unwrap_or(5).min(64) as usize;
        let obs = observe(client, args, schema_paths)?;
        match decide(&obs, &query, &[])? {
            PolicyOutcome::Choice(d) => {
                let action = obs
                    .space
                    .get(&d.action_id)
                    .ok_or_else(|| "chosen action left the space".to_string())?;
                Ok(json!({
                    "answer": "choice",
                    "chosen": {
                        "id": d.action_id.as_str(),
                        "op": d.kind.as_str(),
                        "role": action.role(),
                        "name": d.target_label,
                        "confidence": d.confidence_millis,
                    },
                    "candidates": candidates_json(&obs, &d.target_ranked, k),
                    "rev": obs.rev,
                }))
            }
            PolicyOutcome::Abstain {
                reason,
                target_ranked,
                operation_ranked,
            } => Ok(json!({
                "answer": "abstain",
                "reason": reason,
                "candidates": if target_ranked.is_empty() {
                    candidates_json(&obs, &operation_ranked, k)
                } else {
                    candidates_json(&obs, &target_ranked, k)
                },
                "rev": obs.rev,
            })),
        }
    }

    /// Terminal outcome of one observe→decide→dispatch attempt.
    /// `Stale` means the host refused a fingerprinted dispatch
    /// (`stale_target`/`target_gone`) — nothing was dispatched.
    enum ActStep {
        Done(Value),
        Stale,
    }

    /// `act {instruction, delivery?, schema?}` — executes the FIRST
    /// clause only. Abstains → no dispatch, ever. A `stale_target` /
    /// `target_gone` refusal re-observes and re-decides at most twice
    /// (M5); the old id is never re-dispatched unchanged because every
    /// retry carries the fingerprint of a fresh observation.
    pub(crate) fn act_tool(
        client: &mut AgentClient,
        args: &Value,
        schema_paths: &[PathBuf],
    ) -> Result<Value, String> {
        const MAX_STALE_RETRIES: u8 = 2;
        let instruction = args.string("instruction")?;
        let clauses = split_sequential_clauses(&instruction);
        let clause = clauses
            .first()
            .ok_or_else(|| "instruction has no clause".to_string())?;
        let mut stale_retries = 0u8;
        loop {
            match act_attempt(client, args, schema_paths, clause)? {
                ActStep::Done(value) => return Ok(value),
                ActStep::Stale if stale_retries < MAX_STALE_RETRIES => {
                    stale_retries += 1;
                }
                ActStep::Stale => {
                    return Ok(json!({
                        "abstain": {
                            "reason": "stale_target: target kept changing between decide and dispatch",
                            "candidates": [],
                        },
                    }));
                }
            }
        }
    }

    fn act_attempt(
        client: &mut AgentClient,
        args: &Value,
        schema_paths: &[PathBuf],
        clause: &str,
    ) -> Result<ActStep, String> {
        let delivery = parse_delivery(args)?;
        let k = args.get("k").and_then(Value::as_u64).unwrap_or(5).min(64) as usize;

        let obs = observe(client, args, schema_paths)?;
        let outcome = decide(&obs, clause, &[])?;
        let decision = match outcome {
            PolicyOutcome::Choice(d) => d,
            PolicyOutcome::Abstain {
                reason,
                target_ranked,
                operation_ranked,
            } => {
                return Ok(ActStep::Done(json!({
                    "abstain": {
                        "reason": reason,
                        "candidates": if target_ranked.is_empty() {
                            candidates_json(&obs, &operation_ranked, k)
                        } else {
                            candidates_json(&obs, &target_ranked, k)
                        },
                    },
                    "rev": obs.rev,
                })));
            }
        };
        let action = obs
            .space
            .get(&decision.action_id)
            .ok_or_else(|| "chosen action left the space".to_string())?
            .clone();

        // Controls terminate the clause without a dispatch. `Done` is
        // the one success control; `Blocked`/`Wait` dispatch nothing so
        // callers get the abstain envelope with the control as reason.
        match decision.kind {
            ActionKind::Done => {
                return Ok(ActStep::Done(
                    json!({"done": {"op": "done", "id": "DONE"}, "rev": obs.rev}),
                ));
            }
            ActionKind::Blocked | ActionKind::Wait => {
                return Ok(ActStep::Done(json!({
                    "abstain": {
                        "reason": decision.kind.as_str().to_lowercase(),
                        "candidates": [],
                    },
                    "rev": obs.rev,
                })));
            }
            _ => {}
        }

        // TYPE/SET_VALUE payloads come from the TextResolver — never
        // from Instinct. No resolution → abstain, no dispatch.
        let mut text_payload: Option<String> = None;
        if matches!(decision.kind, ActionKind::Type | ActionKind::SetValue) {
            let ctx = TextContext {
                goal: UiGoal::new(clause),
                field_label: action.label().to_owned(),
                field_role: action.role().unwrap_or("").to_owned(),
                typed: Vec::new(),
                context_fingerprint: obs.rev.unwrap_or(0),
            };
            match DeterministicTextResolver.resolve(&ctx) {
                Ok(res) => text_payload = Some(res.text),
                Err(err) => {
                    let (reason, ranked) = text_abstain(&err, &decision);
                    return Ok(ActStep::Done(json!({
                        "abstain": {
                            "reason": reason,
                            "candidates": candidates_json(&obs, &ranked, k),
                        },
                        "rev": obs.rev,
                    })));
                }
            }
        }

        // M5: node-bound mutating ops carry the build-time fingerprint
        // as `expect_fingerprint`; the host recomputes it on the
        // dispatch path in the same call — that atomic check replaces
        // M4.4's extra snapshot + role/name re-read. The op table is
        // shared with `run --goal` via `op_for_decision`.
        let op = op_for_decision(
            &decision,
            &action,
            text_payload.as_deref(),
            delivery,
            clause,
            &obs.keybindings,
            |name, clause| invoke_args_for(name, clause, args, schema_paths),
        )?;
        match client.expect_ok(op) {
            Ok(_) => {}
            Err(err) if is_stale_dispatch_error(&err) => {
                return Ok(ActStep::Stale);
            }
            Err(err) => return Err(err),
        }
        // Re-observe against the decide-time rev so the reply carries
        // the post-dispatch rev + diff (PRD M4.5). `result` is
        // `{"diff":…}` / `{"unchanged":true}` / `{"full":true}`.
        let mut done = json!({
            "op": decision.kind.as_str(),
            "id": decision.action_id.as_str(),
            "confidence": decision.confidence_millis,
        });
        if let Some(text) = text_payload {
            done["text"] = Value::String(text);
        }
        let mut out = json!({ "done": done });
        match client.snapshot_with(obs.rev, None) {
            Ok(post) => {
                out["rev"] = json!(post.rev);
                if let Some(Value::Object(effect)) = post.result {
                    for (k, v) in effect {
                        out[k] = v;
                    }
                }
            }
            Err(err) => {
                out["rev"] = json!(obs.rev);
                out["diff"] = Value::Null;
                out["diff_error"] = json!(err);
            }
        }
        Ok(ActStep::Done(out))
    }

    fn text_abstain(
        err: &TextError,
        decision: &gpui_agent_policy::PolicyDecision,
    ) -> (String, Vec<RankedAction>) {
        (
            match err {
                TextError::Missing => "text_missing".to_owned(),
                TextError::Ambiguous => "text_ambiguous".to_owned(),
                TextError::Invalid(msg) => format!("text_invalid:{msg}"),
                TextError::Abstain(msg) => format!("text_abstain:{msg}"),
            },
            decision.target_ranked.clone(),
        )
    }

    fn invoke_args_for(
        name: &str,
        clause: &str,
        args: &Value,
        schema_paths: &[PathBuf],
    ) -> Result<Value, String> {
        let paths = recipe_schema_paths(schema_paths, args);
        let registry = registry_from_schema_paths(&paths)?;
        let schema = registry
            .get(name)
            .ok_or_else(|| format!("invoke schema `{name}` not in registry"))?;
        invoke_args(schema, clause)
    }
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
        #[cfg_attr(not(feature = "instinct"), allow(unused_mut))]
        let mut expected = vec![
            "wait",
            "wait_until",
            "hello",
            "snapshot",
            "screenshot",
            "click",
            "type",
            "set_value",
            "key",
            "keybinding",
            "keybindings",
            "assert",
            "invoke",
            "shutdown",
            "recipe_validate",
            "recipe_plan",
            "recipe_run",
            "recipe_resolve",
        ];
        #[cfg(feature = "instinct")]
        expected.extend(["locate", "act"]);
        assert_eq!(names, expected);
        assert!(names.iter().all(|n| !n.starts_with("todo_")));
        for tool in listed {
            let name = tool["name"].as_str().unwrap_or("");
            if name.starts_with("recipe_") {
                let desc = tool["description"].as_str().unwrap_or("");
                assert!(
                    desc.to_ascii_lowercase().contains("experimental"),
                    "{name} should be labeled experimental: {desc}"
                );
            }
        }
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
    fn assert_and_wait_until_schemas_advertise_visible_and_in_viewport() {
        for name in ["assert", "wait_until"] {
            let tool = tools().into_iter().find(|t| t["name"] == name).unwrap();
            let props = &tool["inputSchema"]["properties"];
            assert!(props.get("visible").is_some(), "{name} missing visible");
            assert!(
                props.get("in_viewport").is_some(),
                "{name} missing in_viewport"
            );
        }
        let wait_until = tools()
            .into_iter()
            .find(|t| t["name"] == "wait_until")
            .unwrap();
        assert_eq!(wait_until["inputSchema"]["required"], json!(["timeout_ms"]));
    }

    #[test]
    fn recipe_validate_unknown_invoke_fails_closed() {
        let err = recipe_validate(
            &json!({
                "recipe": {
                    "name": "bad",
                    "steps": [{"id": "x", "op": "invoke", "name": "shell.run"}]
                }
            }),
            &[],
        )
        .unwrap_err();
        assert!(err.contains("unknown invoke"), "{err}");
    }

    #[test]
    fn recipe_validate_honors_schema_paths() {
        let recipe = json!({
            "recipe": {
                "name": "add",
                "steps": [{"id": "x", "op": "invoke", "name": "todo.add", "args": {"title": "Milk"}}]
            }
        });
        let err = recipe_validate(&recipe, &[]).unwrap_err();
        assert!(err.contains("unknown invoke"), "{err}");
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/schemas/todo.json");
        let ok = recipe_validate(&recipe, &[path]).expect("todo.json should allow todo.add");
        assert_eq!(ok["ok"], json!(true));
    }

    #[test]
    fn recipe_validate_empty_and_bad_json_fail_closed() {
        let err = recipe_validate(&json!({ "recipe": "" }), &[]).unwrap_err();
        assert!(err.contains("no steps"), "{err}");
        let err = recipe_validate(&json!({ "recipe": "{" }), &[]).unwrap_err();
        assert!(err.contains("recipe json"), "{err}");
    }

    #[test]
    fn recipe_resolve_shell_like_fails_closed() {
        let err = recipe_resolve_tool(&json!({ "intent": "rm -rf /" }), &[]).unwrap_err();
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
            &[],
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
            &[],
        )
        .unwrap_err();
        assert!(err.contains("yes"), "{err}");
    }

    #[test]
    fn unknown_mcp_tool_fails_closed() {
        let mut client = AgentClient::connect("127.0.0.1:1".parse().unwrap());
        let err = call_tool(
            &mut client,
            &json!({ "name": "todo.add", "arguments": {} }),
            &[],
        )
        .unwrap_err();
        assert!(err.contains("unknown tool"), "{err}");
    }

    #[test]
    fn recipe_run_failed_assert_is_error() {
        use std::sync::{Arc, Mutex};

        use gpui_agent::protocol::PlatformKind;
        use gpui_agent::server::spawn_host;
        use todo_core::TodoStore;

        let store = Arc::new(Mutex::new(TodoStore::new(PlatformKind::Headless)));
        let (addr, shutdown) = spawn_host("127.0.0.1:0".parse().unwrap(), None, store).unwrap();
        let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(3));
        let result = tools_call_result(
            &mut client,
            &json!({
                "name": "recipe_run",
                "arguments": {
                    "recipe": {
                        "name": "fail-assert",
                        "steps": [
                            {"id": "wait", "op": "wait"},
                            {"id": "seen", "op": "assert", "target": "no-such-node"}
                        ]
                    }
                }
            }),
            &[],
        );
        assert_eq!(
            result.get("isError"),
            Some(&json!(true)),
            "failed assert must not look like a successful tools/call: {result}"
        );
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("\"ok\":false") || text.contains("\"ok\": false"),
            "error payload should still carry the receipt: {text}"
        );
        shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[test]
    fn recipe_run_success_is_not_error() {
        use std::sync::{Arc, Mutex};

        use gpui_agent::protocol::PlatformKind;
        use gpui_agent::server::spawn_host;
        use todo_core::TodoStore;

        let store = Arc::new(Mutex::new(TodoStore::new(PlatformKind::Headless)));
        let (addr, shutdown) = spawn_host("127.0.0.1:0".parse().unwrap(), None, store).unwrap();
        let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(3));
        let result = tools_call_result(
            &mut client,
            &json!({
                "name": "recipe_run",
                "arguments": {
                    "recipe": {
                        "name": "ok-hello",
                        "steps": [
                            {"id": "wait", "op": "wait"},
                            {"id": "hello", "op": "hello"}
                        ]
                    }
                }
            }),
            &[],
        );
        assert!(
            result.get("isError").is_none(),
            "successful recipe_run must not set isError: {result}"
        );
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.contains("\"ok\": true") || text.contains("\"ok\":true"),
            "success payload should carry the receipt: {text}"
        );
        shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}
