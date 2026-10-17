//! M4.6: `gpui-agent mcp` (feature `instinct`) driven end-to-end
//! against a headless `TodoStore` — `locate`/`act`/`snapshot
//! since+format over the real MCP stdio surface.
#![cfg(feature = "instinct")]

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

use gpui_agent::protocol::PlatformKind;
use gpui_agent::server::spawn_host;
use serde_json::{Value, json};
use todo_core::TodoStore;

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

fn spawn_todo() -> (
    String,
    Arc<std::sync::atomic::AtomicBool>,
    Arc<Mutex<TodoStore>>,
) {
    let store = Arc::new(Mutex::new(TodoStore::new(PlatformKind::Headless)));
    let (addr, shutdown) = spawn_host(
        "127.0.0.1:0".parse().unwrap(),
        Some("m4-token".into()),
        store.clone(),
    )
    .expect("bind");
    (addr.to_string(), shutdown, store)
}

#[test]
fn tools_list_includes_locate_and_act() {
    let (addr, shutdown, _store) = spawn_todo();
    let mut mcp = Mcp::spawn(&addr, "m4-token");
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
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn locate_and_act_drive_todo_flow() {
    let (addr, shutdown, store) = spawn_todo();
    let mut mcp = Mcp::spawn(&addr, "m4-token");

    let located = mcp.call("locate", json!({ "query": "click Add" }));
    assert_eq!(located["answer"], "choice", "{located}");
    assert_eq!(located["chosen"]["id"], "CLICK:todo-add", "{located}");

    // act types the draft, then act clicks Add — two one-shot clauses.
    let typed = mcp.call(
        "act",
        json!({ "instruction": r#"type "Buy milk" into What needs doing?"# }),
    );
    assert_eq!(typed["answer"], "done", "{typed}");
    assert_eq!(typed["op"], "TYPE", "{typed}");
    assert_eq!(typed["text"], "Buy milk", "{typed}");

    let clicked = mcp.call("act", json!({ "instruction": "click Add" }));
    assert_eq!(clicked["answer"], "done", "{clicked}");
    assert_eq!(clicked["op"], "CLICK", "{clicked}");
    assert_eq!(clicked["id"], "CLICK:todo-add", "{clicked}");

    let items: Vec<String> = store
        .lock()
        .unwrap()
        .items()
        .iter()
        .map(|t| t.title.clone())
        .collect();
    assert_eq!(items, ["Buy milk"], "store must hold the new todo");
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn act_abstains_without_dispatch() {
    let (addr, shutdown, store) = spawn_todo();
    let mut mcp = Mcp::spawn(&addr, "m4-token");

    // Nothing named "settings gear" → abstain, and no node is touched.
    let out = mcp.call(
        "act",
        json!({ "instruction": "click the nonexistent waffle button" }),
    );
    assert_eq!(out["answer"], "abstain", "{out}");
    assert!(!out["reason"].as_str().unwrap_or("").is_empty(), "{out}");
    assert!(store.lock().unwrap().items().is_empty());

    // Destructive keybinding is offered but never auto-chosen.
    let quit = mcp.call("act", json!({ "instruction": "app quit keybinding" }));
    assert_eq!(quit["answer"], "abstain", "{quit}");
    assert_eq!(quit["reason"], "needs_confirmation", "{quit}");
    // The host is still alive — prove no dispatch happened.
    let hello = mcp.call("hello", json!({}));
    assert_eq!(hello["ok"], true, "{hello}");
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn snapshot_since_and_format_work() {
    let (addr, shutdown, _store) = spawn_todo();
    let mut mcp = Mcp::spawn(&addr, "m4-token");

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
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}
