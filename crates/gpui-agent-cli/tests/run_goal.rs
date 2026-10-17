//! M6.4: `gpui-agent run --goal` (feature `instinct`) against a
//! headless `TodoStore` — task outcomes, golden receipts, and the
//! call-budget tripwire.
#![cfg(feature = "instinct")]

use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use gpui_agent::AgentHost;
use gpui_agent::protocol::PlatformKind;
use gpui_agent::server::spawn_host;
use serde_json::{Value, json};
use todo_core::TodoStore;

struct Todo {
    addr: String,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
    store: Arc<Mutex<TodoStore>>,
}

impl Todo {
    fn spawn() -> Self {
        let store = Arc::new(Mutex::new(TodoStore::new(PlatformKind::Headless)));
        let (addr, shutdown) = spawn_host(
            "127.0.0.1:0".parse().unwrap(),
            Some("m6-token".into()),
            store.clone(),
        )
        .expect("bind");
        Self {
            addr: addr.to_string(),
            shutdown,
            store,
        }
    }

    fn seed(&self, titles: &[&str]) {
        let mut store = self.store.lock().unwrap();
        for title in titles {
            store
                .dispatch(&gpui_agent::Op::Invoke {
                    name: "todo.add".into(),
                    args: json!({ "title": title }),
                })
                .expect("seed add");
        }
    }

    fn titles(&self) -> Vec<String> {
        self.store
            .lock()
            .unwrap()
            .items()
            .iter()
            .map(|t| t.title.clone())
            .collect()
    }

    fn done_titles(&self) -> Vec<String> {
        self.store
            .lock()
            .unwrap()
            .items()
            .iter()
            .filter(|t| t.done)
            .map(|t| t.title.clone())
            .collect()
    }
}

impl Drop for Todo {
    fn drop(&mut self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

fn run_goal(todo: &Todo, goal: &str) -> (i32, Value) {
    let out: Output = Command::new(env!("CARGO_BIN_EXE_gpui-agent"))
        .args([
            "--addr", &todo.addr, "--token", "m6-token", "run", "--goal", goal, "--json",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_remove("GPUI_AGENT_TOKEN")
        .output()
        .expect("spawn gpui-agent run");
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let receipt: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("run --goal did not print a json receipt (exit {code}): {e}\n{stdout}")
    });
    (code, receipt)
}

#[test]
fn add_then_done_exits_zero_and_mutates_state() {
    let todo = Todo::spawn();
    let (code, receipt) = run_goal(&todo, r#"Add a todo "Buy milk""#);
    assert_eq!(code, 0, "{receipt}");
    assert_eq!(receipt["outcome"], "done", "{receipt}");
    assert_eq!(todo.titles(), ["Buy milk"]);
    assert!(!receipt["steps"].as_array().unwrap().is_empty());
    assert_eq!(
        receipt["engine"]["policy"],
        env!("CARGO_PKG_VERSION"),
        "receipt names the policy version"
    );
}

#[test]
fn multi_clause_goal_advances_clauses() {
    let todo = Todo::spawn();
    // 'Add "Groceries" then mark it done' — the second clause runs on
    // the post-add observation.
    let (code, receipt) = run_goal(&todo, r#"Add "Groceries" then mark it done"#);
    assert_eq!(code, 0, "{receipt}");
    assert_eq!(receipt["outcome"], "done", "{receipt}");
    assert_eq!(todo.titles(), ["Groceries"]);
    assert_eq!(todo.done_titles(), ["Groceries"]);
    let clauses: Vec<&str> = receipt["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["clause"].as_str())
        .collect();
    // The receipt records the clause as it ran — pronoun substitution
    // rewrites `it` to the typed literal.
    assert!(
        clauses
            .iter()
            .any(|c| c.contains("mark") && c.contains("Groceries")),
        "second clause must run: {clauses:?}"
    );
}

#[test]
fn missing_target_abstains_exit_two_no_state_change() {
    let todo = Todo::spawn();
    todo.seed(&["Buy milk"]);
    let (code, receipt) = run_goal(&todo, r#"Delete "Walk dog""#);
    assert_eq!(code, 2, "{receipt}");
    assert_eq!(receipt["outcome"], "abstained", "{receipt}");
    assert_eq!(todo.titles(), ["Buy milk"], "nothing may be dispatched");
}

#[test]
fn dangerous_keybinding_never_dispatches() {
    let todo = Todo::spawn();
    let (code, receipt) = run_goal(&todo, "app quit keybinding");
    assert_eq!(code, 2, "{receipt}");
    assert_eq!(receipt["outcome"], "abstained", "{receipt}");
    assert!(
        receipt["reason"]
            .as_str()
            .unwrap_or("")
            .contains("confirmation")
            || receipt["reason"].as_str().unwrap_or("").contains("abstain"),
        "{receipt}"
    );
    // Host still alive: a second run still gets a receipt.
    let (code2, _) = run_goal(&todo, "hello unreachable waffle");
    assert_eq!(code2, 2);
}

#[test]
fn receipts_are_byte_identical_across_runs() {
    let todo_a = Todo::spawn();
    let todo_b = Todo::spawn();
    let (_, a) = run_goal(&todo_a, r#"Add a todo "Buy milk""#);
    let (_, b) = run_goal(&todo_b, r#"Add a todo "Buy milk""#);
    assert_eq!(a, b, "same goal + same host behavior → same receipt");
}

#[test]
fn call_budget_stays_flat() {
    // observe(1) + keybindings(1) + per step dispatch+since (2 each)
    // — the tripwire catches accidental re-snapshot loops.
    let todo = Todo::spawn();
    let (_, receipt) = run_goal(&todo, r#"Add a todo "Buy milk""#);
    let calls = receipt["calls"].as_u64().expect("calls in receipt");
    let steps = receipt["steps"].as_array().unwrap().len() as u64;
    assert!(
        calls <= 2 + steps * 2 + 2,
        "calls {calls} vs steps {steps} — re-observe loop crept in"
    );
}

#[test]
fn mark_done_dispatches_the_right_toggle() {
    let todo = Todo::spawn();
    todo.seed(&["Write docs", "Ship it"]);
    let (code, receipt) = run_goal(&todo, r#"Mark "Write docs" as done"#);
    assert_eq!(code, 0, "{receipt}");
    assert_eq!(todo.done_titles(), ["Write docs"]);
    assert_eq!(todo.titles().len(), 2);
}
