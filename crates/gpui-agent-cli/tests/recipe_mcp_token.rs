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
        .args(["recipe", "validate", path.to_str().unwrap()])
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
        "127.0.0.1:0".parse().unwrap(),
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
            recipe.to_str().unwrap(),
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
    let (addr, shutdown) = spawn_host("127.0.0.1:0".parse().unwrap(), None, store).expect("bind");

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
    let (addr, shutdown) =
        spawn_host("127.0.0.1:0".parse().unwrap(), Some("p2-mcp".into()), store).expect("bind");

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
