//! M4.6: `gpui-agent mcp` (feature `instinct`) driven end-to-end
//! against a headless `TodoStore` — `locate`/`act`/`snapshot
//! since+format over the real MCP stdio surface.
#![cfg(feature = "instinct")]

mod common;

use common::Todo;

const TOKEN: &str = "m4-token";

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

fn cli_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_gpui-agent"));
    cmd.env_remove("GPUI_AGENT_TOKEN");
    cmd
}

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Mcp {
    fn spawn(addr: &str, token: &str) -> Self {
        let mut child = cli_bin()
            .args(["--addr", addr, "--token", token, "mcp"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let mut mcp = Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        mcp.rpc("initialize", json!({}));
        mcp
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
        )
        .expect("write request");
        self.stdin.flush().expect("flush");
        let mut line = String::new();
        loop {
            line.clear();
            let n = self.stdout.read_line(&mut line).expect("read response");
            assert!(n > 0, "mcp closed stdout before answering {method}");
            let v: Value = serde_json::from_str(line.trim()).expect("response json");
            if v.get("id").and_then(Value::as_u64) == Some(id) {
                return v;
            }
        }
    }

    /// `tools/call` → the tool payload (`content[0].text` parsed).
    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let resp = self.rpc(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        let result = &resp["result"];
        assert!(
            result.get("isError").is_none(),
            "{name} errored: {}",
            result["content"][0]["text"]
        );
        serde_json::from_str(result["content"][0]["text"].as_str().expect("text"))
            .expect("tool payload json")
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        drop(self.stdin.flush());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn tools_list_includes_locate_and_act() {
    let todo = Todo::spawn(TOKEN);
    let mut mcp = Mcp::spawn(&todo.addr, TOKEN);
    let resp = mcp.rpc("tools/list", json!({}));
    let names: Vec<&str> = resp["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    for want in ["locate", "act", "snapshot", "click"] {
        assert!(names.contains(&want), "missing {want} in {names:?}");
    }
}

#[test]
fn locate_and_act_drive_todo_flow() {
    let todo = Todo::spawn(TOKEN);
    let mut mcp = Mcp::spawn(&todo.addr, TOKEN);

    let located = mcp.call("locate", json!({ "query": "click Add" }));
    assert_eq!(located["answer"], "choice", "{located}");
    assert_eq!(located["chosen"]["id"], "CLICK:todo-add", "{located}");

    // act types the draft, then act clicks Add — two one-shot clauses.
    let typed = mcp.call(
        "act",
        json!({ "instruction": r#"type "Buy milk" into What needs doing?"# }),
    );
    assert_eq!(typed["done"]["op"], "TYPE", "{typed}");
    assert_eq!(typed["done"]["text"], "Buy milk", "{typed}");
    // M4.5 envelope: post-dispatch rev + diff of the dispatch's effect.
    assert!(typed["rev"].is_u64(), "{typed}");
    assert!(
        typed.get("diff").is_some() || typed.get("unchanged").is_some(),
        "{typed}"
    );

    let clicked = mcp.call("act", json!({ "instruction": "click Add" }));
    assert_eq!(clicked["done"]["op"], "CLICK", "{clicked}");
    assert_eq!(clicked["done"]["id"], "CLICK:todo-add", "{clicked}");

    let items = todo.titles();
    assert_eq!(items, ["Buy milk"], "store must hold the new todo");
}

#[test]
fn act_abstains_without_dispatch() {
    let todo = Todo::spawn(TOKEN);
    let mut mcp = Mcp::spawn(&todo.addr, TOKEN);

    // Nothing named "settings gear" → abstain, and no node is touched.
    let out = mcp.call(
        "act",
        json!({ "instruction": "click the nonexistent waffle button" }),
    );
    assert!(
        !out["abstain"]["reason"].as_str().unwrap_or("").is_empty(),
        "{out}"
    );
    assert!(todo.titles().is_empty());

    // Destructive keybinding is offered but never auto-chosen.
    let quit = mcp.call("act", json!({ "instruction": "app quit keybinding" }));
    assert_eq!(quit["abstain"]["reason"], "needs_confirmation", "{quit}");
    // The host is still alive — prove no dispatch happened.
    let hello = mcp.call("hello", json!({}));
    assert_eq!(hello["ok"], true, "{hello}");
}

#[test]
fn snapshot_since_and_format_work() {
    let todo = Todo::spawn(TOKEN);
    let mut mcp = Mcp::spawn(&todo.addr, TOKEN);

    let first = mcp.call("snapshot", json!({}));
    assert_eq!(first["ok"], true, "{first}");
    let rev = first["rev"].as_u64().expect("rev");

    let compact = mcp.call("snapshot", json!({ "format": "compact" }));
    assert_eq!(compact["ok"], true, "{compact}");
    // Compact is a text view, not the UiTree object.
    assert!(
        compact.get("tree").is_none() || compact["tree"].is_null(),
        "{compact}"
    );

    let diff = mcp.call("snapshot", json!({ "since": rev }));
    assert_eq!(diff["ok"], true, "{diff}");
}

#[test]
fn events_tool_polls_the_ring() {
    let todo = Todo::spawn("m4-token");
    let mut mcp = Mcp::spawn(&todo.addr, "m4-token");

    // The app bridges one `info` event per successful `todo.*` invoke.
    mcp.call(
        "invoke",
        json!({ "name": "todo.add", "args": { "title": "Buy milk" } }),
    );
    let poll = mcp.call("events", json!({}));
    let events = poll["events"].as_array().expect("events array");
    assert_eq!(events.len(), 1, "{poll}");
    assert_eq!(events[0]["message"], "invoke todo.add");
    assert_eq!(poll["latest"], 1, "{poll}");
    assert_eq!(poll["lagged"], false, "{poll}");

    // Cursor advance: nothing new after latest.
    let next = mcp.call(
        "events",
        json!({ "since": poll["latest"].as_u64().unwrap() }),
    );
    assert_eq!(next["events"].as_array().expect("events").len(), 0);
}
