//! M6.4: `gpui-agent run --goal` (feature `instinct`) against a
//! headless `TodoStore` — task outcomes, golden receipts, and the
//! call-budget tripwire.
#![cfg(feature = "instinct")]

mod common;

use std::process::{Command, Output, Stdio};

use serde_json::Value;

use common::Todo;

const TOKEN: &str = "m6-token";

fn run_goal(todo: &Todo, goal: &str) -> (i32, Value) {
    let out: Output = Command::new(env!("CARGO_BIN_EXE_gpui-agent"))
        .args([
            "--addr", &todo.addr, "--token", TOKEN, "run", "--goal", goal, "--json",
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
    let todo = Todo::spawn(TOKEN);
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
    let todo = Todo::spawn(TOKEN);
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
    let todo = Todo::spawn(TOKEN);
    todo.seed(&["Buy milk"]);
    let (code, receipt) = run_goal(&todo, r#"Delete "Walk dog""#);
    assert_eq!(code, 2, "{receipt}");
    assert_eq!(receipt["outcome"], "abstained", "{receipt}");
    assert_eq!(todo.titles(), ["Buy milk"], "nothing may be dispatched");
}

#[test]
fn dangerous_keybinding_never_dispatches() {
    let todo = Todo::spawn(TOKEN);
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
    let todo_a = Todo::spawn(TOKEN);
    let todo_b = Todo::spawn(TOKEN);
    let (_, a) = run_goal(&todo_a, r#"Add a todo "Buy milk""#);
    let (_, b) = run_goal(&todo_b, r#"Add a todo "Buy milk""#);
    assert_eq!(a, b, "same goal + same host behavior → same receipt");
}

#[test]
fn call_budget_stays_flat() {
    // observe(1) + keybindings(1) + per step dispatch+since (2 each)
    // — the tripwire catches accidental re-snapshot loops.
    let todo = Todo::spawn(TOKEN);
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
    let todo = Todo::spawn(TOKEN);
    todo.seed(&["Write docs", "Ship it"]);
    let (code, receipt) = run_goal(&todo, r#"Mark "Write docs" as done"#);
    assert_eq!(code, 0, "{receipt}");
    assert_eq!(todo.done_titles(), ["Write docs"]);
    assert_eq!(todo.titles().len(), 2);
}
