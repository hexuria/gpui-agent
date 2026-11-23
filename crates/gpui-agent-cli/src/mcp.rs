use std::io::{BufReader, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use gpui_agent::client::AgentClient;
use gpui_agent::protocol::{AssertSpec, DeliveryMode, KeybindingScope, Op};
use gpui_agent::{CallFailure, OutlineOptions, WaitFor, error_code};
use gpui_agent::{MAX_LINE_BYTES, line_is_blank, read_limited_line_into};
use gpui_agent_recipe::{
    RecipeError, RecipeErrorCode, RunError, RunOptions, ScreenshotCapture, compile_plan,
    parse_recipe_source, registry_from_schema_paths, resolve_intent, run_plan, validate_recipe,
};
use serde_json::{Value, json};

/// Minimal MCP stdio server: `initialize`, `tools/list`, `tools/call`.
///
/// Tools match the generic protocol ops. App-specific verbs are `invoke`
/// names the host registered — they are not separate MCP tools.
///
/// One [`AgentClient`] lives for the process: protocol `rpc` reuses a single
/// TCP session across `tools/call` (no per-tool reconnect).
pub(crate) fn run(
    addr: SocketAddr,
    token: String,
    timeout: Option<std::time::Duration>,
    schema_paths: &[PathBuf],
) -> Result<()> {
    let mut client = AgentClient::connect(addr).with_token(token);
    if let Some(timeout) = timeout {
        client = client.with_timeout(timeout);
    }

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
                    &json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":err.to_string()}}),
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
                "serverInfo": { "name": "gpui-agent", "version": env!("CARGO_PKG_VERSION") }
            }),
            "notifications/initialized" => {
                continue;
            }
            "tools/list" => json!({ "tools": tools() }),
            "tools/call" => match unique_recipe_keys(&params, &line_buf) {
                Ok(()) => tools_call_result(&mut client, &params, schema_paths),
                Err(refused) => tool_error(refused),
            },
            "ping" => json!({}),
            other => {
                write_msg(
                    &mut stdout,
                    &json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("unknown method {other}")}}),
                )?;
                continue;
            }
        };
        write_msg(
            &mut stdout,
            &json!({"jsonrpc":"2.0","id":id,"result":result}),
        )?;
    }
    Ok(())
}

fn write_msg(stdout: &mut std::io::Stdout, value: &Value) -> Result<()> {
    writeln!(stdout, "{}", serde_json::to_string(value)?)?;
    stdout.flush()?;
    Ok(())
}

// Attach, detach and session listing are CLI-only: an MCP process already
// holds one address and token for its lifetime.
#[expect(
    clippy::too_many_lines,
    reason = "one json! literal per tool; splitting splits the tool list"
)]
pub(crate) fn tools() -> Vec<Value> {
    let mut tools = vec![
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
            "Read the semantic UI tree (stable ids, roles, names, state). Pass since=<rev> for a diff against a revision you already hold, format=compact for one line per visible node, depth and root for an outline of part of the tree.",
            json!({
                "type": "object",
                "properties": {
                    "since": { "type": "integer", "description": "Return only what changed since this snapshot revision." },
                    "format": { "type": "string", "enum": ["json", "compact"], "description": "json (default) is the full UiTree; compact is one line per visible node." },
                    "fingerprints": { "type": "boolean", "description": "true adds an id→fingerprint map so callers can gate mutating ops with expect_fingerprint." },
                    "depth": { "type": "integer", "minimum": 1, "description": "Keep this many levels (1 = the roots alone). A node whose children were cut reports children_count." },
                    "root": { "type": "string", "description": "Keep only the subtree of this node id." }
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
            with_no_read_back(input_schema_with_delivery(&["target"])),
        ),
        tool(
            "type",
            "Append text to an editable widget. Optional delivery=virtual types through GPUI keystrokes after focusing the target.",
            with_no_read_back(input_schema_with_delivery(&["target", "text"])),
        ),
        tool(
            "set_value",
            "Replace the value of an editable widget.",
            with_no_read_back(object_schema(&["target", "value"])),
        ),
        tool(
            "press",
            "Press (tap) a node by id, or a point with x and y, as the platform delivers it. Fails when the target is hidden, disabled or covered. Needs the host feature `press`.",
            with_no_read_back(
                json!({ "type": "object", "properties": { "target": { "type": "string" }, "x": { "type": "number" }, "y": { "type": "number" } }, "required": [] }),
            ),
        ),
        tool(
            "scroll",
            "Scroll a scrollable node, or what is under x,y, by dx,dy logical points.",
            json!({ "type": "object", "properties": { "target": { "type": "string" }, "x": { "type": "number" }, "y": { "type": "number" }, "dx": { "type": "number" }, "dy": { "type": "number" } }, "required": [] }),
        ),
        tool(
            "hover",
            "Move the pointer over a node or point (hosts with a pointer).",
            json!({ "type": "object", "properties": { "target": { "type": "string" }, "x": { "type": "number" }, "y": { "type": "number" } }, "required": [] }),
        ),
        tool(
            "drag",
            "Drag node `from` onto node `to`, or drag `target` (or the point x,y) by dx,dy.",
            json!({ "type": "object", "properties": { "from": { "type": "string" }, "to": { "type": "string" }, "target": { "type": "string" }, "x": { "type": "number" }, "y": { "type": "number" }, "dx": { "type": "number" }, "dy": { "type": "number" } }, "required": [] }),
        ),
        tool(
            "swipe",
            "One-finger swipe (up, down, left, right) across a node, or the surface when target is omitted.",
            json!({ "type": "object", "properties": { "direction": { "type": "string" }, "target": { "type": "string" }, "distance": { "type": "number" } }, "required": ["direction"] }),
        ),
        tool(
            "long_press",
            "Press and hold a node or point.",
            with_no_read_back(
                json!({ "type": "object", "properties": { "target": { "type": "string" }, "x": { "type": "number" }, "y": { "type": "number" }, "duration_ms": { "type": "integer" } }, "required": [] }),
            ),
        ),
        tool(
            "pinch",
            "Two-finger pinch on a node: scale above 1 spreads, below 1 pinches in.",
            json!({ "type": "object", "properties": { "target": { "type": "string" }, "scale": { "type": "number" } }, "required": ["scale"] }),
        ),
        tool(
            "viewport",
            "Resize the app's content area to width,height points or a host preset (phone, tablet, desktop).",
            json!({ "type": "object", "properties": { "width": { "type": "number" }, "height": { "type": "number" }, "preset": { "type": "string" } }, "required": [] }),
        ),
        tool(
            "paste",
            "Insert text into an editable node as one paste.",
            with_no_read_back(
                json!({ "type": "object", "properties": { "target": { "type": "string" }, "text": { "type": "string" } }, "required": ["target", "text"] }),
            ),
        ),
        tool(
            "system",
            "Device action: back, home, rotate, keyboard_show, keyboard_hide, background, foreground, open_url. `value` is the orientation for rotate or the URL for open_url.",
            json!({ "type": "object", "properties": { "action": { "type": "string" }, "value": { "type": "string" } }, "required": ["action"] }),
        ),
        tool(
            "key",
            "Send a key (Enter, Backspace, …) to a widget. No modifiers. Optional delivery=virtual uses GPUI dispatch_keystroke. Modifier chords use the keybinding tool.",
            with_no_read_back(input_schema_with_delivery(&["target", "key"])),
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
            "events",
            "Poll the host's bounded event stream: {events:[{seq,level,target,message}], latest, lagged}. `latest` is the cursor for the next `since`; `lagged` means events were dropped to capacity. Hosts without an event ring answer an error.",
            json!({
                "type": "object",
                "properties": {
                    "since": { "type": "integer", "description": "Only events with seq > this (default: all retained)." },
                    "limit": { "type": "integer", "description": "Max events (default 256, cap 1024)." }
                }
            }),
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
            "Lint a JSON recipe of protocol ops (`.wants` text also accepted). Does not contact the host.",
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
            "Compile a JSON recipe to a wave plan (effects, fingerprint). Does not run it. `.wants` text also accepted.",
            json!({
                "type": "object",
                "properties": {
                    "recipe": { "description": "JSON recipe object (canonical) or wants text." },
                    "set": { "type": "object", "description": "Param name to value. Every required param needs one; an optional param may be left out and takes its default. No key that is not a declared param." },
                    "schema": schema_arg()
                },
                "required": ["recipe"]
            }),
        ),
        tool(
            "recipe_run",
            "Validate, plan, and execute a JSON recipe sequentially on one reused TCP session. `.wants` also accepted. This MCP process already required a token; each step still carries it. Pass yes=true if the plan includes shutdown.",
            json!({
                "type": "object",
                "properties": {
                    "recipe": { "description": "JSON recipe object (canonical) or wants text." },
                    "set": { "type": "object", "description": "Param name to value. Every required param needs one; an optional param may be left out and takes its default. No key that is not a declared param." },
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
    ];
    for tool in &mut tools {
        let name = tool["name"].as_str().unwrap_or_default().to_owned();
        let name = name.as_str();
        if FINGERPRINTED_TOOLS.contains(&name) {
            tool["inputSchema"]["properties"]["expect_fingerprint"] = json!({
                "type": ["integer", "string"],
                "minimum": 0,
                "pattern": "^[0-9]+$",
                "description": "The target's fingerprint from snapshot fingerprints=true. The host refuses with stale_target, before acting, when the target changed since."
            });
        }
        if ACTION_TOOLS.contains(&name) {
            let properties = &mut tool["inputSchema"]["properties"];
            properties["wait_for"] = json!({
                "type": "string",
                "description": "After the action, wait until this node id exists, then return the new tree in compact form as `compact`."
            });
            properties["wait_for_gone"] = json!({
                "type": "string",
                "description": "After the action, wait until this node id is gone, then return the new tree in compact form as `compact`."
            });
            properties["wait_timeout_ms"] = json!({
                "type": "integer",
                "description": "Bound for wait_for / wait_for_gone in milliseconds (default 5000)."
            });
        }
    }
    tools
}

/// Tools whose op carries `expect_fingerprint`.
const FINGERPRINTED_TOOLS: &[&str] = &["click", "type", "set_value", "key", "press"];

/// Tools that act on the app, and so take `wait_for` / `wait_for_gone`.
const ACTION_TOOLS: &[&str] = &[
    "click",
    "type",
    "set_value",
    "key",
    "press",
    "scroll",
    "hover",
    "drag",
    "swipe",
    "long_press",
    "pinch",
    "viewport",
    "paste",
    "system",
    "keybinding",
    "invoke",
];

/// The default bound of `wait_for` / `wait_for_gone`, as the CLI's
/// `--wait-timeout-ms`.
const DEFAULT_WAIT_TIMEOUT_MS: u64 = 5000;

fn schema_arg() -> Value {
    json!({
        "description": "App invoke/id schema JSON path(s). Same as CLI `--schema` / `GPUI_AGENT_SCHEMA`.",
        "oneOf": [
            { "type": "string" },
            { "type": "array", "items": { "type": "string" } }
        ]
    })
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "the schema is a json! literal per tool — a reference adds noise, not cost"
)]
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

/// Read-back is on by default for target actions; `no_read_back: true`
/// skips the before/after snapshots.
fn with_no_read_back(mut schema: Value) -> Value {
    schema["properties"]["no_read_back"] = json!({
        "type": "boolean",
        "description": "Skip the read-back. By default the target is snapshotted before and after; the reply carries `after` and `delivery_status` (delivered_verified when the target changed)."
    });
    schema
}

fn read_back(args: &Value) -> bool {
    !args
        .get("no_read_back")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// The optional `expect_fingerprint` of a guarded action.
fn expect_fingerprint(args: &Value) -> Result<Option<u64>, String> {
    match args.get("expect_fingerprint") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value))
            if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            value
                .parse()
                .map(Some)
                .map_err(|_| "`expect_fingerprint` is outside the u64 range".to_string())
        }
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            "`expect_fingerprint` must be a non-negative integer or decimal string".to_string()
        }),
    }
}

fn stringify_fingerprints(mut value: Value) -> Value {
    if let Some(fingerprints) = value.get_mut("fingerprints").and_then(Value::as_object_mut) {
        for fingerprint in fingerprints.values_mut() {
            if let Some(number) = fingerprint.as_u64() {
                *fingerprint = Value::String(number.to_string());
            }
        }
    }
    value
}

/// The optional `wait_for` / `wait_for_gone` of an action tool, with its
/// bound.
fn requested_wait(name: &str, args: &Value) -> Result<Option<(WaitFor, u64)>, String> {
    let wait = match (
        args.opt_string("wait_for"),
        args.opt_string("wait_for_gone"),
    ) {
        (Some(_), Some(_)) => return Err("give wait_for or wait_for_gone, not both".into()),
        (Some(id), None) => WaitFor::Present(id),
        (None, Some(id)) => WaitFor::Gone(id),
        (None, None) => {
            if args.get("wait_timeout_ms").is_some() {
                return Err("wait_timeout_ms needs wait_for or wait_for_gone".into());
            }
            return Ok(None);
        }
    };
    if !ACTION_TOOLS.contains(&name) {
        return Err(format!(
            "wait_for and wait_for_gone follow an action tool ({}), not {name}",
            ACTION_TOOLS.join(", ")
        ));
    }
    let timeout_ms = match args.get("wait_timeout_ms") {
        None => DEFAULT_WAIT_TIMEOUT_MS,
        Some(value) => value
            .as_u64()
            .ok_or_else(|| "`wait_timeout_ms` must be a non-negative integer".to_string())?,
    };
    Ok(Some((wait, timeout_ms)))
}

/// `snapshot` with `depth` and/or `root`: the host's full tree cut in the
/// client, as JSON (`tree` is the outline) or compact text.
#[expect(
    clippy::fn_params_excessive_bools,
    reason = "the fingerprints flag mirrors the CLI switch"
)]
fn outline_snapshot(
    client: &mut AgentClient,
    args: &Value,
    since: Option<u64>,
    format: Option<gpui_agent::protocol::SnapshotFormat>,
    fingerprints: bool,
) -> Result<Value, String> {
    if since.is_some() {
        return Err(fail(CallFailure::with_code(
            "depth and root cut a full snapshot; they do not apply to since",
            error_code::BAD_REQUEST,
            None,
        )));
    }
    let mut options = OutlineOptions::new();
    options.root = args.opt_string("root");
    options.depth = match args.get("depth") {
        None => None,
        Some(value) => Some(
            value
                .as_u64()
                .map(|n| usize::try_from(n).unwrap_or(usize::MAX))
                .ok_or_else(|| "`depth` must be a positive integer".to_string())?,
        ),
    };
    let mut resp = client
        .call(&Op::Snapshot {
            since: None,
            format: None,
            fingerprints: fingerprints.then_some(true),
        })
        .map_err(fail)?;
    let tree = resp.tree.take().ok_or("the reply carried no tree")?;
    let outline = gpui_agent::outline(&tree, &options)
        .map_err(|err| fail(CallFailure::with_code(err, error_code::BAD_REQUEST, None)))?;
    if !options.is_full() {
        resp.rev = None;
    }
    if let Some(fps) = resp.fingerprints.as_mut() {
        outline.retain_fingerprints(fps);
    }
    let mut value = serde_json::to_value(&resp).map_err(|err| err.to_string())?;
    if matches!(format, Some(gpui_agent::protocol::SnapshotFormat::Compact)) {
        value["result"] = json!({ "compact": outline.compact_lines() });
    } else {
        value["tree"] = serde_json::to_value(&outline).map_err(|err| err.to_string())?;
    }
    Ok(stringify_fingerprints(value))
}

/// A failed request as the tool error text: the same JSON envelope the
/// CLI prints on stdout.
fn fail(failure: CallFailure) -> String {
    serde_json::to_string(&failure.envelope()).unwrap_or(failure.error)
}

/// [`fail`] for a request made through a string-returning client API: the
/// text is classified by [`gpui_agent::error_code_of`].
fn fail_text(error: String) -> String {
    fail(CallFailure::from_message(error))
}

/// Tool error text: an envelope or recipe receipt passes through; any
/// other text was produced before a request reached the host (bad tool
/// arguments, an unknown tool, a recipe that does not compile), so it
/// becomes a `bad_request` envelope. A text that already names a code
/// keeps it.
fn tool_error_text(error: String) -> String {
    if serde_json::from_str::<serde_json::Map<String, Value>>(&error).is_ok() {
        return error;
    }
    let code = match gpui_agent::error_code_of(&error) {
        error_code::HOST_ERROR => error_code::BAD_REQUEST,
        code => code,
    };
    fail(CallFailure::with_code(error, code, None))
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

fn wait_reply_timeout(reply_timeout: Duration, bound_ms: Option<u64>) -> Duration {
    let bound = bound_ms
        .map(|ms| Duration::from_millis(ms.saturating_add(2_000)))
        .unwrap_or_default();
    reply_timeout.max(bound)
}

#[expect(
    clippy::too_many_lines,
    reason = "one arm per MCP tool; splitting splits the tool table"
)]
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
    let wait = requested_wait(name, &args)?;
    let resp = match name {
        "wait" => {
            // The wait's bound holds for this call only: the tools after it
            // get the server's reply timeout back.
            let reply_timeout = client.timeout();
            let bound_ms = args.get("timeout_ms").and_then(Value::as_u64);
            client.set_timeout(wait_reply_timeout(reply_timeout, bound_ms));
            let ready = client.call(&Op::Wait { timeout_ms: None });
            client.set_timeout(reply_timeout);
            ready.map_err(fail)?
        }
        "wait_until" => {
            let timeout_ms = args
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .ok_or_else(|| "missing integer `timeout_ms`".to_string())?;
            let spec = parse_assert_spec(&args)?;
            let reply_timeout = client.timeout();
            client.set_timeout(wait_reply_timeout(reply_timeout, Some(timeout_ms)));
            let ready = client.call(&Op::WaitUntil { timeout_ms, spec });
            client.set_timeout(reply_timeout);
            ready.map_err(fail)?
        }
        "hello" => client.call(&Op::Hello).map_err(fail)?,
        "snapshot" => {
            let since = args.get("since").and_then(Value::as_u64);
            let format = match args.get("format").and_then(Value::as_str) {
                None => None,
                Some("json") => Some(gpui_agent::protocol::SnapshotFormat::Json),
                Some("compact") => Some(gpui_agent::protocol::SnapshotFormat::Compact),
                Some(other) => return Err(format!("unknown snapshot format {other}")),
            };
            let fingerprints = args.get("fingerprints").and_then(Value::as_bool);
            if args.get("depth").is_some() || args.get("root").is_some() {
                return outline_snapshot(client, &args, since, format, fingerprints == Some(true));
            }
            let resp = client
                .call(&Op::Snapshot {
                    since,
                    format,
                    fingerprints,
                })
                .map_err(fail)?;
            return serde_json::to_value(resp)
                .map(stringify_fingerprints)
                .map_err(|err| err.to_string());
        }
        "screenshot" => {
            let path = args.string("path")?;
            let mode = parse_screenshot_mode(&args)?;
            let target = args.opt_string("target");
            let max_height_px = args
                .get("max_height_px")
                .and_then(Value::as_u64)
                .map(|n| u32::try_from(n).unwrap_or(u32::MAX));
            if mode.is_scrolled() && target.as_deref().map_or("", str::trim).is_empty() {
                return Err("screenshot mode=scrolled requires target".into());
            }
            client
                .call(&Op::Screenshot {
                    path: Some(path),
                    mode,
                    target,
                    max_height_px,
                    inline: false,
                })
                .map_err(fail)?
        }
        "click" => {
            let op = Op::Click {
                target: args.string("target")?,
                delivery: parse_delivery(&args)?,
                expect_fingerprint: expect_fingerprint(&args)?,
            };
            client.act(&op, read_back(&args)).map_err(fail)?
        }
        "type" => {
            let op = Op::Type {
                target: args.string("target")?,
                text: args.string("text")?,
                delivery: parse_delivery(&args)?,
                expect_fingerprint: expect_fingerprint(&args)?,
            };
            client.act(&op, read_back(&args)).map_err(fail)?
        }
        "set_value" => {
            let op = Op::SetValue {
                target: args.string("target")?,
                value: args.string("value")?,
                expect_fingerprint: expect_fingerprint(&args)?,
            };
            client.act(&op, read_back(&args)).map_err(fail)?
        }
        "key" => {
            let op = Op::Key {
                target: args.string("target")?,
                key: args.string("key")?,
                delivery: parse_delivery(&args)?,
                expect_fingerprint: expect_fingerprint(&args)?,
            };
            client.act(&op, read_back(&args)).map_err(fail)?
        }
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
            client
                .call(&Op::Keybinding {
                    binding,
                    chord,
                    scope,
                    confirm,
                    activate,
                })
                .map_err(fail)?
        }
        "keybindings" | "keybinding.list" => client.keybindings().map_err(fail_text)?,
        "events" => {
            let since = args.get("since").and_then(Value::as_u64);
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .map(|v| u32::try_from(v.min(u64::from(u32::MAX))).unwrap_or(u32::MAX));
            let poll = client.events(since, limit).map_err(fail_text)?;
            return serde_json::to_value(poll).map_err(|e| e.to_string());
        }
        "assert" => {
            let spec = parse_assert_spec(&args)?;
            client.call(&Op::Assert { spec }).map_err(fail)?
        }
        "invoke" => {
            let op = Op::Invoke {
                name: args.string("name")?,
                args: args.get("args").cloned().unwrap_or(json!({})),
            };
            client.call(&op).map_err(fail)?
        }
        "shutdown" => client.call(&Op::Shutdown).map_err(fail)?,
        "press" | "scroll" | "hover" | "drag" | "swipe" | "long_press" | "pinch" | "viewport"
        | "paste" | "system" => {
            // The tool arguments are the op's own wire fields, so the MCP
            // surface cannot drift from the protocol or the CLI.
            let read_back = read_back(&args);
            let press_fingerprint = if name == "press" {
                expect_fingerprint(&args)?
            } else {
                None
            };
            if name == "press"
                && press_fingerprint.is_some()
                && args.get("target").and_then(Value::as_str).is_none()
            {
                return Err(fail(CallFailure::with_code(
                    "expect_fingerprint needs a target",
                    error_code::BAD_REQUEST,
                    None,
                )));
            }
            let Value::Object(mut wire) = args else {
                return Err(format!("{name} takes an object"));
            };
            if let Some(fingerprint) = press_fingerprint {
                wire.insert("expect_fingerprint".into(), json!(fingerprint));
            }
            for key in [
                "no_read_back",
                "wait_for",
                "wait_for_gone",
                "wait_timeout_ms",
            ] {
                wire.remove(key);
            }
            wire.insert("op".into(), json!(name));
            let op: Op = serde_json::from_value(Value::Object(wire))
                .map_err(|err| format!("{name}: {err}"))?;
            client.act(&op, read_back).map_err(fail)?
        }
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
    let Some((wait, timeout_ms)) = wait else {
        return serde_json::to_value(resp).map_err(|err| err.to_string());
    };
    // One call: the action, the wait for the state it should produce, and
    // the tree that state shows, compact. A wait that fails reports the
    // action's delivery status, since the action itself went through.
    let reply_timeout = client.timeout();
    client.set_timeout(wait_reply_timeout(reply_timeout, Some(timeout_ms)));
    let waited = client.wait_after(&wait, timeout_ms);
    client.set_timeout(reply_timeout);
    let compact = waited.map_err(|failure| {
        fail(CallFailure {
            delivery_status: resp.delivery_status,
            ..failure
        })
    })?;
    let mut value = serde_json::to_value(resp).map_err(|err| err.to_string())?;
    value["compact"] = json!(compact);
    Ok(stringify_fingerprints(value))
}

fn recipe_text(args: &Value) -> Result<String, String> {
    match args.get("recipe") {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(other) => serde_json::to_string(other).map_err(|err| err.to_string()),
        None => Err("missing `recipe`".into()),
    }
}

/// A [`RecipeError`] as tool error text: `{error, error_code, step?}`.
#[expect(clippy::needless_pass_by_value, reason = "used as a `map_err` adapter")]
fn recipe_fail(error: RecipeError) -> String {
    error.envelope().to_string()
}

fn recipe_set(args: &Value) -> Result<std::collections::BTreeMap<String, String>, String> {
    let mut map = std::collections::BTreeMap::new();
    match args.get("set") {
        None | Some(Value::Null) => {}
        Some(Value::Object(obj)) => {
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
        Some(_) => {
            return Err(recipe_fail(RecipeError::new(
                RecipeErrorCode::InvalidParam,
                "`set` must be an object of param name to value",
            )));
        }
    }
    Ok(map)
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

fn recipe_registry(
    schema_paths: &[PathBuf],
    args: &Value,
) -> Result<gpui_agent_recipe::Registry, String> {
    registry_from_schema_paths(&recipe_schema_paths(schema_paths, args)).map_err(recipe_fail)
}

fn recipe_source(args: &Value) -> Result<gpui_agent_recipe::Recipe, String> {
    parse_recipe_source(&recipe_text(args)?, "inline", false).map_err(recipe_fail)
}

fn recipe_validate(args: &Value, schema_paths: &[PathBuf]) -> Result<Value, String> {
    let recipe = recipe_source(args)?;
    validate_recipe(&recipe, &recipe_registry(schema_paths, args)?).map_err(recipe_fail)?;
    Ok(json!({
        "ok": true,
        "name": recipe.name,
        "steps": recipe.steps.len(),
        "params": recipe.param_names(),
        "input_schema": recipe.input_schema(),
        "explicit_needs": recipe.uses_explicit_needs(),
    }))
}

fn recipe_plan(args: &Value, schema_paths: &[PathBuf]) -> Result<Value, String> {
    let recipe = recipe_source(args)?;
    let plan = compile_plan(
        &recipe,
        &recipe_set(args)?,
        &recipe_registry(schema_paths, args)?,
    )
    .map_err(recipe_fail)?;
    serde_json::to_value(plan).map_err(|err| err.to_string())
}

fn recipe_run_tool(
    client: &mut AgentClient,
    args: &Value,
    schema_paths: &[PathBuf],
) -> Result<Value, String> {
    let recipe = recipe_source(args)?;
    let plan = compile_plan(
        &recipe,
        &recipe_set(args)?,
        &recipe_registry(schema_paths, args)?,
    )
    .map_err(recipe_fail)?;
    let yes = args.get("yes").and_then(Value::as_bool).unwrap_or(false);
    let flagged = args
        .get("screenshot_flagged")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let dir = args.get("screenshot_dir").and_then(Value::as_str);
    if flagged && dir.is_none() {
        return Err("screenshot_flagged requires screenshot_dir".into());
    }
    let capture = dir.map(|dir| ScreenshotCapture::new(dir).with_flagged_only(flagged));
    let options = RunOptions::new().with_yes(yes).with_screenshots(capture);
    match run_plan(client, &plan, &options) {
        Ok(receipt) => serde_json::to_value(receipt).map_err(|err| err.to_string()),
        Err(RunError::Step {
            receipt,
            error,
            error_code,
            ..
        }) => {
            let mut value = serde_json::to_value(*receipt).map_err(|err| err.to_string())?;
            value["error"] = json!(error);
            value["error_code"] = json!(error_code);
            Err(serde_json::to_string(&value).unwrap_or(error))
        }
        Err(RunError::NeedsYes) => Err(recipe_fail(RecipeError::new(
            RecipeErrorCode::Unconfirmed,
            RunError::NeedsYes.to_string(),
        ))),
        Err(err) => Err(err.to_string()),
    }
}

/// MCP `tools/call` result. Failed recipe steps must be `isError` so a
/// host does not treat `"ok": false` inside a successful tool payload as OK.
/// The text is compact JSON: the model reads it as tokens, and the
/// indentation of a nested tree is most of a pretty-printed snapshot (a
/// todo window's 15.7 kB snapshot is 4.7 kB compact).
fn tools_call_result(client: &mut AgentClient, params: &Value, schema_paths: &[PathBuf]) -> Value {
    match call_tool(client, params, schema_paths) {
        Ok(value) => json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&value).unwrap_or_else(|err| err.to_string())
            }]
        }),
        Err(err) => tool_error(err),
    }
}

fn tool_error(error: String) -> Value {
    json!({
        "isError": true,
        "content": [{ "type": "text", "text": tool_error_text(error) }]
    })
}

/// Refuse a recipe tool's request whose line writes a key twice in one
/// object. A recipe sent as a JSON object is already down to the last of
/// the two in `params`; the line still has both.
fn unique_recipe_keys(params: &Value, line: &[u8]) -> Result<(), String> {
    let recipe_tool = params
        .get("name")
        .and_then(Value::as_str)
        .is_some_and(|name| name.starts_with("recipe_"));
    if !recipe_tool {
        return Ok(());
    }
    let text = std::str::from_utf8(line).map_err(|err| err.to_string())?;
    gpui_agent_recipe::reject_duplicate_keys(text).map_err(recipe_fail)
}

fn recipe_resolve_tool(args: &Value, schema_paths: &[PathBuf]) -> Result<Value, String> {
    let intent = args
        .get("intent")
        .and_then(Value::as_str)
        .ok_or("missing string `intent`")?;
    let result =
        resolve_intent(intent, &recipe_registry(schema_paths, args)?).map_err(recipe_fail)?;
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
    use super::{
        AgentClient, ArgsExt, CallFailure, PathBuf, Result, Value, error_code, fail, fail_text,
        json, parse_delivery, recipe_schema_paths, registry_from_schema_paths,
    };
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
        let registry = registry_from_schema_paths(&paths).map_err(|err| err.to_string())?;
        Ok(crate::instinct_wiring::offered_invokes(&registry))
    }

    fn observe(
        client: &mut AgentClient,
        args: &Value,
        schema_paths: &[PathBuf],
    ) -> Result<Observed, String> {
        let snap = client.snapshot().map_err(fail_text)?;
        // The host answered without a tree: its fault, not the caller's.
        let tree = snap.tree.ok_or_else(|| {
            fail(CallFailure::with_code(
                "host returned no ui tree".to_owned(),
                error_code::HOST_ERROR,
                None,
            ))
        })?;
        let rev = snap.rev.or(tree.rev);
        let keybindings = client.keybinding_catalog().map_err(fail_text)?;
        let invokes = offered_invokes(args, schema_paths)?;
        let secret_redaction = client
            .features()
            .map_err(fail_text)?
            .iter()
            .any(|feature| feature == "secret_redaction");
        let space = ActionSpace::from_tree_with_secret_redaction(
            &tree,
            &keybindings,
            &invokes,
            secret_redaction,
        );
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
        let k = usize::try_from(args.get("k").and_then(Value::as_u64).unwrap_or(5).min(64))
            .unwrap_or(usize::MAX);
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

    #[expect(
        clippy::too_many_lines,
        reason = "one linear observe-decide-verify attempt"
    )]
    fn act_attempt(
        client: &mut AgentClient,
        args: &Value,
        schema_paths: &[PathBuf],
        clause: &str,
    ) -> Result<ActStep, String> {
        let delivery = parse_delivery(args)?;
        let k = usize::try_from(args.get("k").and_then(Value::as_u64).unwrap_or(5).min(64))
            .unwrap_or(usize::MAX);

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
            Err(err) => {
                let code = gpui_agent::error_code_of(&err);
                let delivery = gpui_agent::DeliveryStatus::of_failure(code);
                return Err(fail(CallFailure::with_code(err, code, Some(delivery))));
            }
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
        let registry = registry_from_schema_paths(&paths).map_err(|err| err.to_string())?;
        let schema = registry
            .get(name)
            .ok_or_else(|| format!("invoke schema `{name}` not in registry"))?;
        invoke_args(schema, clause).map_err(|err| err.to_string())
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
        #[cfg_attr(
            not(feature = "instinct"),
            expect(unused_mut, reason = "instinct-only push keeps the same list mutable")
        )]
        let mut expected = vec![
            "wait",
            "wait_until",
            "hello",
            "snapshot",
            "screenshot",
            "click",
            "type",
            "set_value",
            "press",
            "scroll",
            "hover",
            "drag",
            "swipe",
            "long_press",
            "pinch",
            "viewport",
            "paste",
            "system",
            "key",
            "keybinding",
            "keybindings",
            "events",
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
                assert_eq!(
                    desc.to_ascii_lowercase().contains("experimental"),
                    name == "recipe_resolve",
                    "only recipe_resolve is experimental: {name}: {desc}"
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
    fn mcp_wait_reply_timeout_covers_the_bound_and_preserves_longer_values() {
        assert_eq!(
            wait_reply_timeout(Duration::from_secs(8), Some(20_000)),
            Duration::from_secs(22)
        );
        assert_eq!(
            wait_reply_timeout(Duration::from_secs(30), Some(20_000)),
            Duration::from_secs(30)
        );
        assert_eq!(
            wait_reply_timeout(Duration::from_secs(8), None),
            Duration::from_secs(8)
        );
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

    #[test]
    fn target_action_schemas_advertise_no_read_back() {
        for name in [
            "click",
            "type",
            "set_value",
            "key",
            "press",
            "paste",
            "long_press",
        ] {
            let tool = tools().into_iter().find(|t| t["name"] == name).unwrap();
            assert_eq!(
                tool["inputSchema"]["properties"]["no_read_back"]["type"],
                json!("boolean"),
                "{name}"
            );
        }
    }

    #[test]
    fn legacy_snapshot_since_uses_a_full_masked_tree() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let mut secret =
            gpui_agent::tree::UiNode::new("secret", "textbox", "Password").with_value("hunter2");
        secret.states.push("secret".into());
        let raw_fingerprint = secret.fingerprint(None);
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut requests = Vec::new();
            for _ in 0..2 {
                let mut line = String::new();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                let request: Value = serde_json::from_str(&line).unwrap();
                requests.push(request.clone());
                let mut response = match request["op"].as_str() {
                    Some("hello") => json!({
                        "ok": true,
                        "hello": {
                            "protocol": gpui_agent::protocol::PROTOCOL_VERSION,
                            "app": "legacy",
                            "platform": "headless",
                            "ready": true,
                            "features": []
                        }
                    }),
                    Some("snapshot") if request.get("since").is_some() => json!({
                        "ok": true,
                        "result": {
                            "diff": {
                                "added": [],
                                "removed": [],
                                "changed": [{"id": "secret", "value": "hunter2"}]
                            }
                        },
                        "fingerprints": {"secret": raw_fingerprint, "plain": 456}
                    }),
                    Some("snapshot") => json!({
                        "ok": true,
                        "rev": 2,
                        "tree": {
                            "app": "legacy",
                            "platform": "headless",
                            "ready": true,
                            "nodes": [
                                {
                                    "id": "secret",
                                    "role": "textbox",
                                    "name": "Password",
                                    "value": "hunter2",
                                    "states": ["secret"],
                                    "enabled": true
                                },
                                {
                                    "id": "plain",
                                    "role": "button",
                                    "name": "Submit",
                                    "enabled": true
                                }
                            ]
                        },
                        "fingerprints": {"secret": raw_fingerprint, "plain": 456}
                    }),
                    op => panic!("unexpected op: {op:?}"),
                };
                response["v"] = json!(gpui_agent::protocol::PROTOCOL_VERSION);
                response["id"] = request["id"].clone();
                writeln!(stream, "{response}").unwrap();
                stream.flush().unwrap();
            }
            requests
        });
        let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(3));
        let result = tools_call_result(
            &mut client,
            &json!({
                "name": "snapshot",
                "arguments": { "since": 1, "fingerprints": true }
            }),
            &[],
        );
        assert!(result.get("isError").is_none(), "{result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(!text.contains("hunter2"), "{text}");
        let snapshot: Value = serde_json::from_str(text).unwrap();
        assert_eq!(snapshot["result"]["full"], json!(true), "{snapshot}");
        assert_eq!(snapshot["tree"]["nodes"][0]["value"], json!("•••••••"));
        assert!(
            snapshot["fingerprints"].get("secret").is_none(),
            "{snapshot}"
        );
        assert_eq!(snapshot["fingerprints"]["plain"], json!("456"));
        let requests = server.join().unwrap();
        assert_eq!(requests[0]["op"], "hello");
        assert_eq!(requests[1]["op"], "snapshot");
        assert!(requests[1].get("since").is_none(), "{requests:?}");
        assert!(requests[1].get("format").is_none(), "{requests:?}");
    }

    fn spawn_todo_client() -> (AgentClient, std::sync::Arc<std::sync::atomic::AtomicBool>) {
        use std::sync::{Arc, Mutex};

        use gpui_agent::protocol::PlatformKind;
        use gpui_agent::server::spawn_host;
        use todo_core::TodoStore;

        let store = Arc::new(Mutex::new(TodoStore::new(PlatformKind::Headless)));
        let (addr, shutdown) = spawn_host("127.0.0.1:0".parse().unwrap(), None, store).unwrap();
        let client = AgentClient::connect(addr).with_timeout(Duration::from_secs(3));
        (client, shutdown)
    }

    fn tool_error_envelope(result: &Value) -> Value {
        assert_eq!(result.get("isError"), Some(&json!(true)), "{result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        let envelope: Value = serde_json::from_str(text).expect("envelope is JSON");
        assert_eq!(envelope["ok"], json!(false), "{envelope}");
        assert!(envelope["error"].is_string(), "{envelope}");
        envelope
    }

    fn coded_error_client() -> (AgentClient, std::thread::JoinHandle<()>) {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: gpui_agent::protocol::Request = serde_json::from_str(&line).unwrap();
            let mut response =
                gpui_agent::protocol::Response::err(&request.id, "opaque host failure");
            response.error_code = Some("host_specific_code".into());
            response.hint = Some("host supplied recovery hint".into());
            writeln!(stream, "{}", serde_json::to_string(&response).unwrap()).unwrap();
            stream.flush().unwrap();
        });
        (
            AgentClient::connect(addr).with_timeout(Duration::from_secs(3)),
            server,
        )
    }

    #[test]
    fn legacy_tools_preserve_host_error_metadata() {
        for (name, arguments) in [
            ("snapshot", json!({})),
            ("screenshot", json!({ "path": "ignored.png" })),
            ("wait", json!({ "timeout_ms": 100 })),
            (
                "wait_until",
                json!({ "target": "missing", "timeout_ms": 100 }),
            ),
            (
                "keybinding",
                json!({ "id": "custom.binding", "scope": "global" }),
            ),
        ] {
            let (mut client, server) = coded_error_client();
            let result = tools_call_result(
                &mut client,
                &json!({ "name": name, "arguments": arguments }),
                &[],
            );
            let envelope = tool_error_envelope(&result);
            assert_eq!(
                envelope["error_code"],
                json!("host_specific_code"),
                "{name}: {envelope}"
            );
            assert_eq!(
                envelope["hint"],
                json!("host supplied recovery hint"),
                "{name}: {envelope}"
            );
            if name == "keybinding" {
                assert!(
                    envelope["delivery_status"].is_string(),
                    "{name}: {envelope}"
                );
            }
            server.join().unwrap();
        }
    }

    #[test]
    fn failed_request_carries_the_envelope() {
        let (mut client, shutdown) = spawn_todo_client();
        let result = tools_call_result(
            &mut client,
            &json!({ "name": "assert", "arguments": { "target": "no-such-node" } }),
            &[],
        );
        let envelope = tool_error_envelope(&result);
        assert_eq!(envelope["error_code"], json!("not_found"), "{envelope}");
        assert!(envelope["hint"].is_string(), "{envelope}");
        assert!(envelope.get("delivery_status").is_none(), "{envelope}");
        shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[test]
    fn unreachable_host_action_is_not_delivered() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let mut client = AgentClient::connect(addr).with_timeout(Duration::from_millis(500));
        let result = tools_call_result(
            &mut client,
            &json!({ "name": "click", "arguments": { "target": "todo-add" } }),
            &[],
        );
        let envelope = tool_error_envelope(&result);
        assert_eq!(
            envelope["error_code"],
            json!("connect_failed"),
            "{envelope}"
        );
        assert_eq!(
            envelope["delivery_status"],
            json!("not_delivered"),
            "{envelope}"
        );
    }

    #[test]
    fn action_reads_back_by_default_and_no_read_back_skips_it() {
        let (mut client, shutdown) = spawn_todo_client();
        let verified = call_tool(
            &mut client,
            &json!({ "name": "type", "arguments": { "target": "todo-input", "text": "Milk" } }),
            &[],
        )
        .unwrap();
        assert_eq!(
            verified["delivery_status"],
            json!("delivered_verified"),
            "{verified}"
        );
        assert_eq!(verified["after"]["value"], json!("Milk"), "{verified}");
        assert_eq!(verified["after"]["changed"], json!(true), "{verified}");

        let unverified = call_tool(
            &mut client,
            &json!({
                "name": "type",
                "arguments": { "target": "todo-input", "text": "!", "no_read_back": true }
            }),
            &[],
        )
        .unwrap();
        assert_eq!(
            unverified["delivery_status"],
            json!("delivered_unverified"),
            "{unverified}"
        );
        assert!(unverified.get("after").is_none(), "{unverified}");
        shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[test]
    fn tool_results_are_compact_json() {
        let (mut client, shutdown) = spawn_todo_client();
        let result = tools_call_result(&mut client, &json!({ "name": "snapshot" }), &[]);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(!text.contains('\n'), "{text}");
        let value: Value = serde_json::from_str(text).unwrap();
        assert_eq!(serde_json::to_string(&value).unwrap(), text);
        shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[test]
    fn guarded_and_waiting_tools_advertise_their_arguments() {
        let tools = tools();
        let props = |name: &str| {
            tools
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap_or_else(|| panic!("no tool {name}"))["inputSchema"]["properties"]
                .clone()
        };
        for name in FINGERPRINTED_TOOLS {
            assert_eq!(
                props(name)["expect_fingerprint"]["type"],
                json!(["integer", "string"]),
                "{name}"
            );
        }
        for name in ACTION_TOOLS {
            let props = props(name);
            assert_eq!(props["wait_for"]["type"], "string", "{name}");
            assert_eq!(props["wait_for_gone"]["type"], "string", "{name}");
            assert_eq!(props["wait_timeout_ms"]["type"], "integer", "{name}");
        }
        assert!(props("snapshot").get("wait_for").is_none());
        assert_eq!(
            props("press")["expect_fingerprint"]["type"],
            json!(["integer", "string"])
        );
        assert_eq!(props("snapshot")["depth"]["type"], "integer");
        assert_eq!(props("snapshot")["root"]["type"], "string");
    }

    #[test]
    fn expect_fingerprint_guards_each_target_tool() {
        let (mut client, shutdown) = spawn_todo_client();
        let snap = call_tool(
            &mut client,
            &json!({ "name": "snapshot", "arguments": { "fingerprints": true } }),
            &[],
        )
        .unwrap();
        assert!(snap["fingerprints"]["todo-input"].is_string(), "{snap}");
        let input = snap["fingerprints"]["todo-input"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap();
        let add = snap["fingerprints"]["todo-add"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap();
        for (name, args) in [
            ("type", json!({ "target": "todo-input", "text": "x" })),
            ("set_value", json!({ "target": "todo-input", "value": "x" })),
            ("key", json!({ "target": "todo-input", "key": "Enter" })),
            ("click", json!({ "target": "todo-add" })),
        ] {
            let mut stale = args.clone();
            stale["expect_fingerprint"] = json!(u64::MAX);
            let result = tools_call_result(
                &mut client,
                &json!({ "name": name, "arguments": stale }),
                &[],
            );
            let envelope = tool_error_envelope(&result);
            assert_eq!(envelope["error_code"], "stale_target", "{name}: {envelope}");
            assert_eq!(envelope["delivery_status"], "not_delivered", "{name}");
        }
        let typed = call_tool(
            &mut client,
            &json!({
                "name": "type",
                "arguments": { "target": "todo-input", "text": "Milk", "expect_fingerprint": input.to_string() }
            }),
            &[],
        )
        .unwrap();
        assert_eq!(typed["delivery_status"], "delivered_verified", "{typed}");
        // The add button did not change, so its fingerprint still holds.
        call_tool(
            &mut client,
            &json!({ "name": "click", "arguments": { "target": "todo-add", "expect_fingerprint": add } }),
            &[],
        )
        .unwrap();
        let bad = call_tool(
            &mut client,
            &json!({ "name": "click", "arguments": { "target": "todo-add", "expect_fingerprint": "not-a-number" } }),
            &[],
        )
        .unwrap_err();
        assert!(bad.contains("expect_fingerprint"), "{bad}");
        shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[test]
    fn press_accepts_decimal_string_fingerprints() {
        let (mut client, shutdown) = spawn_todo_client();
        call_tool(
            &mut client,
            &json!({
                "name": "set_value",
                "arguments": { "target": "todo-input", "value": "Milk" }
            }),
            &[],
        )
        .unwrap();
        let snap = call_tool(
            &mut client,
            &json!({ "name": "snapshot", "arguments": { "fingerprints": true } }),
            &[],
        )
        .unwrap();
        let fingerprint = snap["fingerprints"]["todo-add"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap();
        let fingerprint_text = fingerprint.to_string();

        call_tool(
            &mut client,
            &json!({
                "name": "press",
                "arguments": {
                    "target": "todo-add",
                    "expect_fingerprint": fingerprint_text
                }
            }),
            &[],
        )
        .unwrap();

        let stale = tools_call_result(
            &mut client,
            &json!({
                "name": "press",
                "arguments": {
                    "target": "todo-add",
                    "expect_fingerprint": (fingerprint ^ 1).to_string()
                }
            }),
            &[],
        );
        let envelope = tool_error_envelope(&stale);
        assert_eq!(envelope["error_code"], "stale_target", "{envelope}");
        assert_eq!(envelope["delivery_status"], "not_delivered", "{envelope}");
        shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[test]
    fn wait_for_returns_the_new_tree_in_one_call() {
        let (mut client, shutdown) = spawn_todo_client();
        let added = call_tool(
            &mut client,
            &json!({
                "name": "invoke",
                "arguments": { "name": "todo.add", "args": { "title": "Milk" }, "wait_for": "todo-item-1" }
            }),
            &[],
        )
        .unwrap();
        let compact = added["compact"].as_str().unwrap();
        assert!(
            compact.contains("todo-item-1 listitem \"Milk\""),
            "{compact}"
        );

        let gone = call_tool(
            &mut client,
            &json!({
                "name": "invoke",
                "arguments": { "name": "todo.delete", "args": { "id": 1 }, "wait_for_gone": "todo-item-1" }
            }),
            &[],
        )
        .unwrap();
        assert!(!gone["compact"].as_str().unwrap().contains("todo-item-1"));

        // The generic wire path drops the wait fields before decoding the op.
        let scrolled = call_tool(
            &mut client,
            &json!({
                "name": "set_value",
                "arguments": { "target": "todo-input", "value": "x", "wait_for": "todo-input", "wait_timeout_ms": 500 }
            }),
            &[],
        )
        .unwrap();
        assert!(
            scrolled["compact"]
                .as_str()
                .unwrap()
                .contains("value=\"x\""),
            "{scrolled}"
        );

        let result = tools_call_result(
            &mut client,
            &json!({
                "name": "paste",
                "arguments": { "target": "todo-input", "text": "y", "wait_for": "todo-item-9", "wait_timeout_ms": 100 }
            }),
            &[],
        );
        let envelope = tool_error_envelope(&result);
        assert_eq!(envelope["error_code"], "timeout", "{envelope}");
        assert_eq!(
            envelope["delivery_status"], "delivered_verified",
            "{envelope}"
        );

        for (name, args) in [
            ("snapshot", json!({ "wait_for": "a" })),
            (
                "click",
                json!({ "target": "todo-add", "wait_for": "a", "wait_for_gone": "b" }),
            ),
            (
                "click",
                json!({ "target": "todo-add", "wait_timeout_ms": 5 }),
            ),
        ] {
            let err = call_tool(
                &mut client,
                &json!({ "name": name, "arguments": args }),
                &[],
            )
            .unwrap_err();
            assert!(err.contains("wait"), "{name}: {err}");
        }
        shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[test]
    fn snapshot_depth_and_root_outline_the_tree() {
        let (mut client, shutdown) = spawn_todo_client();
        let one = call_tool(
            &mut client,
            &json!({ "name": "snapshot", "arguments": { "depth": 1, "fingerprints": true } }),
            &[],
        )
        .unwrap();
        assert!(one.get("rev").is_none(), "{one}");
        assert!(one["tree"].get("rev").is_none(), "{one}");
        let window = &one["tree"]["nodes"][0];
        assert!(window.get("children").is_none(), "{window}");
        assert!(window["children_count"].as_u64().unwrap() > 0, "{window}");
        let fps = one["fingerprints"].as_object().unwrap();
        assert_eq!(fps.len(), 1, "{fps:?}");
        assert!(fps.values().all(Value::is_string), "{fps:?}");

        let page = call_tool(
            &mut client,
            &json!({ "name": "snapshot", "arguments": { "root": "todo-input", "format": "compact" } }),
            &[],
        )
        .unwrap();
        let compact = page["result"]["compact"].as_str().unwrap();
        assert!(compact.starts_with("todo-input textbox"), "{compact}");
        assert_eq!(compact.lines().count(), 1, "{compact}");

        for args in [
            json!({ "root": "nope" }),
            json!({ "depth": 0 }),
            json!({ "depth": 1, "since": 1 }),
        ] {
            let result = tools_call_result(
                &mut client,
                &json!({ "name": "snapshot", "arguments": args }),
                &[],
            );
            let envelope = tool_error_envelope(&result);
            assert_eq!(envelope["error_code"], "bad_request", "{args}: {envelope}");
        }
        shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[test]
    fn keybinding_reports_delivery_status() {
        let (mut client, shutdown) = spawn_todo_client();
        let result = call_tool(
            &mut client,
            &json!({
                "name": "keybinding",
                "arguments": {
                    "id": todo_core::ids::KEY_GO_SETTINGS,
                    "scope": "global"
                }
            }),
            &[],
        )
        .unwrap();

        assert_eq!(
            result["delivery_status"],
            json!("delivered_unverified"),
            "{result}"
        );
        shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[test]
    fn wait_until_timeout_carries_the_envelope() {
        let (mut client, shutdown) = spawn_todo_client();
        let result = tools_call_result(
            &mut client,
            &json!({
                "name": "wait_until",
                "arguments": { "target": "nope", "timeout_ms": 100 }
            }),
            &[],
        );
        let envelope = tool_error_envelope(&result);
        assert_eq!(envelope["error_code"], json!("timeout"), "{envelope}");
        assert!(envelope["hint"].is_string(), "{envelope}");
        shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[test]
    fn string_api_tools_carry_the_envelope_when_unreachable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let mut client = AgentClient::connect(addr).with_timeout(Duration::from_millis(300));
        for (name, arguments) in [
            ("snapshot", json!({})),
            ("snapshot", json!({ "fingerprints": true })),
            ("keybindings", json!({})),
            ("events", json!({})),
            ("wait", json!({ "timeout_ms": 100 })),
        ] {
            let result = tools_call_result(
                &mut client,
                &json!({ "name": name, "arguments": arguments }),
                &[],
            );
            let envelope = tool_error_envelope(&result);
            assert_eq!(
                envelope["error_code"],
                json!("connect_failed"),
                "{name}: {envelope}"
            );
        }
    }

    #[test]
    fn bad_arguments_are_a_bad_request_envelope() {
        let mut client = AgentClient::connect("127.0.0.1:1".parse().unwrap());
        let result = tools_call_result(
            &mut client,
            &json!({ "name": "click", "arguments": {} }),
            &[],
        );
        let envelope = tool_error_envelope(&result);
        assert_eq!(envelope["error_code"], json!("bad_request"), "{envelope}");
        assert!(envelope.get("delivery_status").is_none(), "{envelope}");
    }

    #[test]
    fn click_on_a_missing_target_is_not_found_and_not_delivered() {
        let (mut client, shutdown) = spawn_todo_client();
        for no_read_back in [false, true] {
            let result = tools_call_result(
                &mut client,
                &json!({
                    "name": "click",
                    "arguments": { "target": "no-such-node", "no_read_back": no_read_back }
                }),
                &[],
            );
            let envelope = tool_error_envelope(&result);
            assert_eq!(envelope["error_code"], json!("not_found"), "{envelope}");
            assert_eq!(
                envelope["delivery_status"],
                json!("not_delivered"),
                "{envelope}"
            );
            assert!(envelope["hint"].is_string(), "{envelope}");
        }
        shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[test]
    fn press_rejects_fingerprint_without_target_before_sending() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client =
            AgentClient::connect(addr).with_timeout(std::time::Duration::from_millis(100));
        let error = call_tool(
            &mut client,
            &json!({
                "name": "press",
                "arguments": {
                    "x": 12.0,
                    "y": 34.0,
                    "expect_fingerprint": "123"
                }
            }),
            &[],
        )
        .unwrap_err();
        let envelope: Value = serde_json::from_str(&error).expect("failure envelope");
        assert_eq!(envelope["error_code"], "bad_request", "{envelope}");
        assert_eq!(
            envelope["error"], "expect_fingerprint needs a target",
            "{envelope}"
        );
        assert!(matches!(
            listener.accept(),
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock
        ));
    }
}
