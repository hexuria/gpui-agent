//! MCP recipe token exchange over the stdio surface.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use gpui_agent::protocol::PlatformKind;
use gpui_agent::server::spawn_host;
use todo_core::TodoStore;

fn cli_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_gpui-agent"));
    cmd.env_remove("GPUI_AGENT_TOKEN");
    cmd
}

fn stderr_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn todo_crud_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/recipes/todo-crud.json")
}

fn todo_schema_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/schemas/todo.json")
}

#[test]
fn recipe_run_without_token_fails_fast() {
    let output = cli_bin()
        .args(["recipe", "run", "/no-such-recipe.json", "--set", "title=x"])
        .output()
        .expect("spawn");
    assert!(
        !output.status.success(),
        "recipe run without token must be nonzero"
    );
    let err = stderr_of(&output);
    assert!(
        err.contains("GPUI_AGENT_TOKEN") || err.contains("token"),
        "{err}"
    );
    assert!(
        !err.contains("no-such-recipe"),
        "must fail before opening the recipe: {err}"
    );
    // Rejected before running: the envelope is on stdout all the same.
    let envelope: serde_json::Value =
        serde_json::from_str(&stdout_of(&output)).expect("envelope on stdout");
    assert_eq!(envelope["ok"], false, "{envelope}");
    assert_eq!(envelope["error_code"], "unauthorized", "{envelope}");
    assert!(err.contains(envelope["error"].as_str().unwrap()), "{err}");
}

/// A remote address without a token is refused before the recipe is read;
/// that refusal carries an envelope too.
#[test]
fn recipe_run_remote_without_token_prints_an_envelope() {
    for extra in [&["--allow-remote"][..], &[]] {
        let output = cli_bin()
            .args(["--addr", "192.0.2.1:9"])
            .args(extra)
            .args(["recipe", "run", "/no-such-recipe.json"])
            .output()
            .expect("spawn");
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let envelope: serde_json::Value =
            serde_json::from_str(&stdout_of(&output)).expect("envelope on stdout");
        assert_eq!(envelope["ok"], false, "{envelope}");
        assert_eq!(envelope["error_code"], "unauthorized", "{envelope}");
        let err = stderr_of(&output);
        assert!(err.contains(envelope["error"].as_str().unwrap()), "{err}");
        assert!(!err.contains("no-such-recipe"), "{err}");
    }
}

/// Any refusal before the recipe is read carries an envelope: here a
/// session that does not exist.
#[test]
fn recipe_run_with_a_missing_session_prints_an_envelope() {
    let output = cli_bin()
        .args(["--session", "no-such-session-9f3a", "--token", "t"])
        .args(["recipe", "run", "/no-such-recipe.json"])
        .output()
        .expect("spawn");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let envelope: serde_json::Value =
        serde_json::from_str(&stdout_of(&output)).expect("envelope on stdout");
    assert_eq!(envelope["ok"], false, "{envelope}");
    assert_eq!(envelope["error_code"], "bad_request", "{envelope}");
    let err = stderr_of(&output);
    assert!(err.contains(envelope["error"].as_str().unwrap()), "{err}");
    assert!(err.contains("no-such-session-9f3a"), "{err}");
    assert!(!err.contains("no-such-recipe"), "{err}");
}

/// A recipe sent to MCP as a JSON object is checked for a key written
/// twice, as recipe text is: the request line still has both.
#[test]
fn mcp_recipe_object_with_a_duplicate_key_is_refused() {
    let mut child = cli_bin()
        .args(["--addr", "127.0.0.1:1", "--token", "t", "mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mcp");
    {
        let mut stdin = child.stdin.take().expect("stdin");
        for (id, recipe) in [
            (
                1,
                r#"{"name":"d","steps":[{"id":"a","op":"hello"}],"steps":[{"id":"b","op":"hello"}]}"#,
            ),
            (2, r#"{"name":"d","steps":[{"id":"a","op":"hello"}]}"#),
        ] {
            writeln!(
                stdin,
                r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"recipe_validate","arguments":{{"recipe":{recipe}}}}}}}"#
            )
            .expect("write call");
        }
        stdin.flush().ok();
    }
    let output = child.wait_with_output().expect("mcp exit");
    let replies: Vec<serde_json::Value> = stdout_of(&output)
        .lines()
        .map(|line| serde_json::from_str(line).expect("json-rpc line"))
        .collect();
    assert_eq!(replies.len(), 2, "{replies:?}");
    let text = |reply: &serde_json::Value| -> serde_json::Value {
        serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    };
    assert_eq!(replies[0]["result"]["isError"], true, "{}", replies[0]);
    let refused = text(&replies[0]);
    assert_eq!(refused["error_code"], "recipe_parse", "{refused}");
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("duplicate field `steps`"),
        "{refused}"
    );
    assert!(
        replies[1]["result"].get("isError").is_none(),
        "{}",
        replies[1]
    );
    assert_eq!(text(&replies[1])["ok"], true, "{}", replies[1]);
}

#[test]
fn recipe_run_flagged_without_dir_prints_an_envelope() {
    let output = cli_bin()
        .args(["--addr", "127.0.0.1:1", "--token", "t"])
        .args(["recipe", "run", "/no-such-recipe.json"])
        .arg("--screenshot-flagged")
        .output()
        .expect("spawn");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let envelope: serde_json::Value =
        serde_json::from_str(&stdout_of(&output)).expect("envelope on stdout");
    assert_eq!(envelope["ok"], false, "{envelope}");
    assert_eq!(envelope["error_code"], "bad_request", "{envelope}");
    assert!(
        envelope["error"]
            .as_str()
            .unwrap()
            .contains("--screenshot-dir"),
        "{envelope}"
    );
}

#[test]
fn recipe_run_empty_env_token_fails_fast() {
    let output = cli_bin()
        .env("GPUI_AGENT_TOKEN", "")
        .args(["recipe", "run", "/no-such-recipe.json", "--set", "title=x"])
        .output()
        .expect("spawn");
    assert!(!output.status.success());
    let err = stderr_of(&output);
    assert!(
        err.contains("GPUI_AGENT_TOKEN") || err.contains("token"),
        "{err}"
    );
}

#[test]
fn mcp_without_token_fails_fast() {
    let output = cli_bin()
        .args(["mcp"])
        .stdin(Stdio::null())
        .output()
        .expect("spawn");
    assert!(
        !output.status.success(),
        "mcp without token must be nonzero (got {:?})\n{}",
        output.status,
        stderr_of(&output)
    );
    let err = stderr_of(&output);
    assert!(
        err.contains("GPUI_AGENT_TOKEN") || err.contains("token"),
        "{err}"
    );
}

#[test]
fn mcp_empty_flag_token_fails_fast() {
    let output = cli_bin()
        .args(["--token", "", "mcp"])
        .stdin(Stdio::null())
        .output()
        .expect("spawn");
    assert!(!output.status.success());
    let err = stderr_of(&output);
    assert!(
        err.contains("GPUI_AGENT_TOKEN") || err.contains("token"),
        "{err}"
    );
}

#[test]
fn recipe_validate_without_token_still_works() {
    let path = todo_crud_path();
    let output = cli_bin()
        .args([
            "recipe",
            "validate",
            path.to_str().expect("test fixture"),
            "--schema",
            todo_schema_path().to_str().expect("test fixture"),
        ])
        .output()
        .expect("spawn");
    assert!(
        output.status.success(),
        "validate is local and does not need a token\n{}",
        stderr_of(&output)
    );
    let stdout = stdout_of(&output);
    assert!(stdout.contains("\"ok\": true"), "{stdout}");
}

#[test]
fn recipe_run_with_matching_token_is_ok_and_reuses_session() {
    let store = Arc::new(Mutex::new(TodoStore::new(PlatformKind::Headless)));
    let (addr, shutdown) = spawn_host(
        "127.0.0.1:0".parse().expect("test fixture"),
        Some("p2-secret".into()),
        store,
    )
    .expect("bind");

    let recipe = todo_crud_path();
    let output = cli_bin()
        .args([
            "--addr",
            &addr.to_string(),
            "--token",
            "p2-secret",
            "recipe",
            "run",
            recipe.to_str().expect("test fixture"),
            "--schema",
            todo_schema_path().to_str().expect("test fixture"),
            "--set",
            "title=Buy milk",
        ])
        .output()
        .expect("spawn");

    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);

    let stdout = stdout_of(&output);
    let stderr = stderr_of(&output);
    assert!(output.status.success(), "stderr={stderr}\nstdout={stdout}");
    assert!(stdout.contains("\"ok\": true"), "{stdout}");
    assert!(stdout.contains("\"session_reused\": true"), "{stdout}");
}

#[test]
fn hello_without_token_still_allowed_when_host_has_none() {
    let store = Arc::new(Mutex::new(TodoStore::new(PlatformKind::Headless)));
    let (addr, shutdown) =
        spawn_host("127.0.0.1:0".parse().expect("test fixture"), None, store).expect("bind");

    let output = cli_bin()
        .args(["--addr", &addr.to_string(), "hello"])
        .output()
        .expect("spawn");

    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);

    assert!(
        output.status.success(),
        "one-off hello must not require a client token\n{}",
        stderr_of(&output)
    );
    let stdout = stdout_of(&output);
    assert!(stdout.contains("\"auth\": \"none\""), "{stdout}");
}

#[test]
fn mcp_with_token_starts_and_answers_initialize() {
    let store = Arc::new(Mutex::new(TodoStore::new(PlatformKind::Headless)));
    let (addr, shutdown) = spawn_host(
        "127.0.0.1:0".parse().expect("test fixture"),
        Some("p2-mcp".into()),
        store,
    )
    .expect("bind");

    let mut child = cli_bin()
        .args(["--addr", &addr.to_string(), "--token", "p2-mcp", "mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mcp");

    {
        let mut stdin = child.stdin.take().expect("stdin");
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{}}}}"#
        )
        .expect("write initialize");
        stdin.flush().ok();
        drop(stdin);
    }

    let output = child.wait_with_output().expect("mcp exit");
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);

    assert!(
        output.status.success(),
        "mcp with token should exit 0 after stdin EOF\n{}",
        stderr_of(&output)
    );
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("gpui-agent") || stdout.contains("protocolVersion"),
        "{stdout}"
    );
}

#[test]
fn cli_help_hides_token_env_canary() {
    const CANARY: &str = "review-canary-9f3a-TOKEN";
    let output = cli_bin()
        .env("GPUI_AGENT_TOKEN", CANARY)
        .arg("--help")
        .output()
        .expect("spawn");
    assert!(
        output.status.success(),
        "gpui-agent --help must succeed\n{}",
        stderr_of(&output)
    );
    let text = format!("{}{}", stdout_of(&output), stderr_of(&output));
    assert!(
        !text.contains(CANARY),
        "--help must not print the live GPUI_AGENT_TOKEN canary:\n{text}"
    );
    assert!(
        !text.contains("Drive any GPUI Kit app"),
        "help must not claim CDP-like attach:\n{text}"
    );
    assert!(
        text.contains("AgentHost") || text.to_ascii_lowercase().contains("embed"),
        "help must mention AgentHost or embed:\n{text}"
    );
}
