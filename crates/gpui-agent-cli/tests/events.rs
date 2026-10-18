//! M9: `gpui-agent events` + the `events` op end-to-end against a
//! headless `TodoStore` — the app bridges `todo.*` invokes into its
//! `EventRing`, and the CLI polls it.

use std::process::{Command, Output};

use serde_json::Value;

mod common;
use common::Todo;

const TOKEN: &str = "m9-token";

fn cli(todo: &Todo, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gpui-agent"))
        .args(["--addr", &todo.addr, "--token", TOKEN])
        .args(args)
        .env_remove("GPUI_AGENT_TOKEN")
        .output()
        .expect("spawn gpui-agent")
}

fn cli_json(todo: &Todo, args: &[&str]) -> Value {
    let out = cli(todo, args);
    assert!(
        out.status.success(),
        "gpui-agent {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("json stdout")
}

#[test]
fn events_streams_app_records() {
    let todo = Todo::spawn(TOKEN);

    // The app pushes one `info` event per successful invoke.
    cli(&todo, &["invoke", "todo.add", "--arg", "title=Buy milk"]);
    cli(&todo, &["invoke", "todo.add", "--arg", "title=Eggs"]);

    let poll = cli_json(&todo, &["events"]);
    assert_eq!(poll["latest"], 2, "{poll}");
    assert_eq!(poll["lagged"], false, "{poll}");
    let events = poll["events"].as_array().expect("events array");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["seq"], 1);
    assert_eq!(events[0]["level"], "info");
    assert_eq!(events[0]["target"], "todo");
    assert_eq!(events[0]["message"], "invoke todo.add");

    // --since=1 returns only the newer event; latest is the cursor.
    let poll = cli_json(&todo, &["events", "--since", "1"]);
    let events = poll["events"].as_array().expect("events array");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["seq"], 2);

    // --limit pages.
    let poll = cli_json(&todo, &["events", "--limit", "1"]);
    assert_eq!(poll["events"].as_array().expect("events").len(), 1);
}

#[test]
fn hello_advertises_events() {
    let todo = Todo::spawn(TOKEN);
    let hello = cli_json(&todo, &["hello"]);
    assert!(
        hello["hello"]["features"]
            .as_array()
            .expect("features")
            .iter()
            .any(|f| f == "events"),
        "{hello}"
    );
}
