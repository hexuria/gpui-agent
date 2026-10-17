//! M7.4: `gpui-agent run --goal … --escalate-cmd` (feature `instinct`)
//! against a headless `TodoStore` and a scripted endpoint
//! (`escalate_fixture.sh`): correct choice, off-menu → blocked,
//! timeout → blocked, abstain stays abstain, and no call when the
//! local policy chooses.
#![cfg(feature = "instinct")]

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use gpui_agent::AgentHost;
use gpui_agent::protocol::PlatformKind;
use gpui_agent::server::spawn_host;
use serde_json::{Value, json};
use todo_core::TodoStore;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/escalate_fixture.sh");

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
            Some("m7-token".into()),
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

    fn draft(&self) -> String {
        self.store.lock().unwrap().draft().to_owned()
    }
}

impl Drop for Todo {
    fn drop(&mut self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Scratch dir per test for the endpoint log + reply files.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("m7-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self(dir)
    }

    fn write(&self, name: &str, body: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, body).expect("write reply");
        path
    }

    fn log(&self) -> PathBuf {
        self.0.join("calls.log")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[allow(clippy::too_many_arguments)]
fn run_goal(
    todo: &Todo,
    goal: &str,
    choose: Option<&PathBuf>,
    text: Option<&PathBuf>,
    log: &PathBuf,
    timeout_ms: Option<u64>,
    sleep: Option<u64>,
) -> (i32, Value) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_gpui-agent"));
    cmd.args([
        "--addr", &todo.addr, "--token", "m7-token", "run", "--goal", goal, "--json",
    ]);
    let choose = choose
        .cloned()
        .unwrap_or_else(|| log.with_file_name("none.choose"));
    let text = text
        .cloned()
        .unwrap_or_else(|| log.with_file_name("none.text"));
    cmd.arg("--escalate-cmd")
        .arg("sh")
        .arg(FIXTURE)
        .env("ESC_REPLY_CHOOSE", &choose)
        .env("ESC_REPLY_TEXT", &text)
        .env("ESC_LOG", log);
    if let Some(ms) = timeout_ms {
        cmd.arg("--escalate-timeout-ms").arg(ms.to_string());
    }
    if let Some(s) = sleep {
        cmd.env("ESC_SLEEP", s.to_string());
    }
    let out: Output = cmd
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

/// A clause the local policy abstains on: the named row does not exist.
const ABSTAIN_GOAL: &str = r#"Delete "Walk dog""#;

#[test]
fn escalated_choice_from_the_menu_dispatches() {
    let todo = Todo::spawn();
    todo.seed(&["Buy milk"]);
    let scratch = Scratch::new("choice");
    let reply = scratch.write("reply.json", r#"{"choice":{"id":"DONE","op":"DONE"}}"#);
    let (code, receipt) = run_goal(
        &todo,
        ABSTAIN_GOAL,
        Some(&reply),
        None,
        &scratch.log(),
        None,
        None,
    );
    assert_eq!(code, 0, "{receipt}");
    assert_eq!(receipt["outcome"], "done", "{receipt}");
    let log = std::fs::read_to_string(scratch.log()).expect("endpoint was called");
    assert!(log.contains("\"kind\":\"choose\""), "{log}");
    assert!(log.contains("\"abstain\""), "{log}");
}

#[test]
fn escalated_menu_click_really_dispatches() {
    let todo = Todo::spawn();
    todo.seed(&["Buy milk"]);
    let scratch = Scratch::new("click");
    // The endpoint keeps answering CLICK:nav-settings — a real
    // dispatch on every escalated step until the run caps out.
    let reply = scratch.write(
        "reply.json",
        r#"{"choice":{"id":"CLICK:nav-settings","op":"CLICK"}}"#,
    );
    let (code, receipt) = run_goal(
        &todo,
        ABSTAIN_GOAL,
        Some(&reply),
        None,
        &scratch.log(),
        None,
        None,
    );
    assert_eq!(code, 2, "{receipt}");
    // The endpoint's repeated pick keeps dispatching until the run's
    // no-effect tripwire or step cap stops it — either is a bounded
    // stop, and both prove escalated picks really dispatch.
    let outcome = receipt["outcome"].as_str().unwrap_or("");
    assert!(
        outcome == "cap_hit" || outcome == "blocked",
        "bounded stop expected: {receipt}"
    );
    let steps = receipt["steps"].as_array().expect("steps");
    assert_eq!(steps[0]["op"], "CLICK", "{receipt}");
    assert_eq!(steps[0]["id"], "CLICK:nav-settings", "{receipt}");
    assert_eq!(steps[0]["confidence_millis"], 0, "escalated pick");
    assert!(steps.len() > 1, "endpoint answers every abstain");
    let calls = std::fs::read_to_string(scratch.log()).expect("endpoint log");
    assert!(calls.lines().count() >= 2, "{calls}");
}

#[test]
fn off_menu_choice_is_a_hard_error() {
    let todo = Todo::spawn();
    todo.seed(&["Buy milk"]);
    let scratch = Scratch::new("offmenu");
    let reply = scratch.write(
        "reply.json",
        r#"{"choice":{"id":"CLICK:ghost","op":"CLICK"}}"#,
    );
    let (code, receipt) = run_goal(
        &todo,
        ABSTAIN_GOAL,
        Some(&reply),
        None,
        &scratch.log(),
        None,
        None,
    );
    assert_eq!(code, 2, "{receipt}");
    assert_eq!(receipt["outcome"], "blocked", "{receipt}");
    let reason = receipt["reason"].as_str().unwrap_or("");
    assert!(reason.contains("escalation:"), "{receipt}");
    assert!(reason.contains("off-menu"), "{receipt}");
    assert_eq!(todo.titles(), ["Buy milk"], "nothing may be dispatched");
}

#[test]
fn endpoint_timeout_blocks_the_run() {
    let todo = Todo::spawn();
    todo.seed(&["Buy milk"]);
    let scratch = Scratch::new("timeout");
    let reply = scratch.write("reply.json", r#"{"choice":{"id":"DONE","op":"DONE"}}"#);
    let (code, receipt) = run_goal(
        &todo,
        ABSTAIN_GOAL,
        Some(&reply),
        None,
        &scratch.log(),
        Some(400),
        Some(5),
    );
    assert_eq!(code, 2, "{receipt}");
    assert_eq!(receipt["outcome"], "blocked", "{receipt}");
    assert!(
        receipt["reason"].as_str().unwrap_or("").contains("timeout"),
        "{receipt}"
    );
}

#[test]
fn endpoint_abstain_remains_an_abstain() {
    let todo = Todo::spawn();
    todo.seed(&["Buy milk"]);
    let scratch = Scratch::new("abstain");
    let reply = scratch.write("reply.json", r#"{"abstain":"no idea"}"#);
    let (code, receipt) = run_goal(
        &todo,
        ABSTAIN_GOAL,
        Some(&reply),
        None,
        &scratch.log(),
        None,
        None,
    );
    assert_eq!(code, 2, "{receipt}");
    assert_eq!(receipt["outcome"], "abstained", "{receipt}");
    assert!(
        receipt["reason"]
            .as_str()
            .unwrap_or("")
            .contains("escalated abstain: no idea"),
        "{receipt}"
    );
}

#[test]
fn endpoint_is_never_called_when_instinct_chooses() {
    let todo = Todo::spawn();
    let scratch = Scratch::new("nocall");
    let reply = scratch.write("reply.json", r#"{"choice":{"id":"DONE","op":"DONE"}}"#);
    let (code, receipt) = run_goal(
        &todo,
        r#"Add a todo "Buy milk""#,
        Some(&reply),
        None,
        &scratch.log(),
        None,
        None,
    );
    assert_eq!(code, 0, "{receipt}");
    assert_eq!(receipt["outcome"], "done", "{receipt}");
    assert_eq!(todo.titles(), ["Buy milk"]);
    assert!(
        !scratch.log().exists(),
        "endpoint log must not exist — Instinct chose locally"
    );
}

#[test]
fn text_escalation_fills_a_missing_payload() {
    let todo = Todo::spawn();
    let scratch = Scratch::new("text");
    // `Type in the what needs doing box` names the textbox but has no
    // quoted literal → deterministic resolver answers Missing → the
    // endpoint supplies the payload over kind:"text".
    let choose = scratch.write("choose.json", r#"{"abstain":"done"}"#);
    let text = scratch.write("text.json", r#"{"text":"escalated title"}"#);
    let (code, receipt) = run_goal(
        &todo,
        "Type in the what needs doing box",
        Some(&choose),
        Some(&text),
        &scratch.log(),
        None,
        None,
    );
    let log = std::fs::read_to_string(scratch.log()).expect("endpoint was called");
    assert!(log.contains("\"kind\":\"text\""), "{log}");
    assert!(
        log.contains("\"field\":{\"id\":\"TYPE:todo-input\""),
        "text request names the field: {log}"
    );
    assert!(
        todo.draft().starts_with("escalated title"),
        "escalated payload reached the field: {:?} / {receipt}",
        todo.draft()
    );
    assert_ne!(code, 1, "no protocol error: {receipt}");
}
