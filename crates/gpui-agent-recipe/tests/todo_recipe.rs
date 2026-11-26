//! End-to-end recipe runs against a spawned todo host.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_agent::client::AgentClient;
use gpui_agent::dispatch::DispatchResult;
use gpui_agent::host::AgentHost;
use gpui_agent::protocol::PlatformKind;
use gpui_agent::protocol::{HelloInfo, Op};
use gpui_agent::server::spawn_host;
use gpui_agent::tree::UiTree;
use gpui_agent_recipe::{
    RunError, RunOptions, ScreenshotCapture, compile_plan, parse_wants, run_plan, todo_registry,
    validate_recipe,
};
use todo_core::TodoStore;

fn spawn_todo() -> (AgentClient, Arc<std::sync::atomic::AtomicBool>) {
    let store = Arc::new(Mutex::new(TodoStore::new(PlatformKind::Headless)));
    let (addr, shutdown) =
        spawn_host("127.0.0.1:0".parse().expect("valid addr"), None, store).expect("bind");
    let client = AgentClient::connect(addr).with_timeout(Duration::from_secs(3));
    (client, shutdown)
}

#[test]
fn recipe_creates_and_toggles_todos_on_one_session() {
    let wants = r#"
wait
invoke todo.add title="Buy milk"
assert todo-item-1 name="Buy milk" checked=false
invoke todo.toggle id=1
assert todo-item-1 checked=true
"#;
    let recipe = parse_wants(wants, "todo-crud").unwrap();
    validate_recipe(&recipe, &todo_registry()).unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    assert!(!plan.requires_yes);

    let (mut client, shutdown) = spawn_todo();
    let receipt = run_plan(&mut client, &plan, &RunOptions::new()).expect("run");
    assert!(receipt.ok, "{receipt:?}");
    assert_eq!(receipt.steps.len(), 5);
    assert!(
        receipt.session_reused,
        "many ops should share one TCP session"
    );

    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn json_todo_crud_with_matching_token_reuses_session() {
    let json = std::fs::read_to_string(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/recipes/todo-crud.json"),
    )
    .expect("example recipe");
    let recipe = gpui_agent_recipe::Recipe::from_json(&json).unwrap();
    let mut set = BTreeMap::new();
    set.insert("title".into(), "Buy milk".into());
    let plan = compile_plan(&recipe, &set, &todo_registry()).unwrap();

    let store = Arc::new(Mutex::new(TodoStore::new(PlatformKind::Headless)));
    let (addr, shutdown) = spawn_host(
        "127.0.0.1:0".parse().unwrap(),
        Some("p2-secret".into()),
        store,
    )
    .expect("bind");
    let mut client = AgentClient::connect(addr)
        .with_token("p2-secret")
        .with_timeout(Duration::from_secs(3));
    let receipt = run_plan(&mut client, &plan, &RunOptions::new()).expect("run");
    assert!(receipt.ok, "{receipt:?}");
    assert!(
        receipt.session_reused,
        "matching token must still reuse the TCP session"
    );

    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn json_todo_visible_wait_until_reuses_session() {
    let json = std::fs::read_to_string(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/recipes/todo-visible.json"),
    )
    .expect("visible recipe");
    let recipe = gpui_agent_recipe::Recipe::from_json(&json).unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();

    let (mut client, shutdown) = spawn_todo();
    let receipt = run_plan(&mut client, &plan, &RunOptions::new()).expect("run");
    assert!(receipt.ok, "{receipt:?}");
    assert!(receipt.session_reused);
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn json_recipe_params_and_widget_ops() {
    let json = r#"{
        "name": "widget-crud",
        "params": ["title"],
        "steps": [
            {"id": "wait", "op": "wait"},
            {"id": "draft", "op": "set_value", "target": "todo-input", "value": "$title", "needs": ["wait"]},
            {"id": "add", "op": "click", "target": "todo-add", "needs": ["draft"]},
            {"id": "seen", "op": "assert", "target": "todo-item-1", "name": "$title", "checked": false, "needs": ["add"]},
            {"id": "toggle", "op": "click", "target": "todo-toggle-1", "needs": ["seen"]},
            {"id": "done", "op": "assert", "target": "todo-item-1", "checked": true, "needs": ["toggle"]}
        ]
    }"#;
    let recipe = gpui_agent_recipe::Recipe::from_json(json).unwrap();
    let mut set = BTreeMap::new();
    set.insert("title".into(), "Write docs".into());
    let plan = compile_plan(&recipe, &set, &todo_registry()).unwrap();

    let (mut client, shutdown) = spawn_todo();
    let receipt = run_plan(&mut client, &plan, &RunOptions::new()).expect("run");
    assert!(receipt.ok, "{receipt:?}");
    assert!(receipt.session_reused);
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn independent_read_wave_is_pipelined_on_one_session() {
    let recipe = gpui_agent_recipe::Recipe::from_json(
        r#"{
        "name": "wide",
        "steps": [
            {"id": "root", "op": "wait"},
            {"id": "a", "op": "hello", "needs": ["root"]},
            {"id": "b", "op": "hello", "needs": ["root"]},
            {"id": "c", "op": "hello", "needs": ["root"]}
        ]
    }"#,
    )
    .unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    assert_eq!(plan.waves.len(), 2);
    assert_eq!(plan.waves[1].len(), 3);
    assert!(
        plan.waves[1]
            .iter()
            .all(|id| plan.steps.iter().any(|s| s.id == *id
                && s.effects
                    .iter()
                    .all(|e| matches!(e, gpui_agent_recipe::Effect::Read)))),
        "hello wave must be all-Read so the runner pipelines it"
    );

    let (mut client, shutdown) = spawn_todo();
    let receipt = run_plan(&mut client, &plan, &RunOptions::new()).expect("run");
    assert!(receipt.ok, "{receipt:?}");
    assert_eq!(receipt.steps.len(), 4);
    assert!(receipt.session_reused);
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn independent_read_wave_records_siblings_when_one_fails() {
    let recipe = gpui_agent_recipe::Recipe::from_json(
        r#"{
        "name": "wide-fail",
        "steps": [
            {"id": "root", "op": "wait"},
            {"id": "a", "op": "hello", "needs": ["root"]},
            {"id": "b", "op": "assert", "target": "no-such-node", "needs": ["root"]},
            {"id": "c", "op": "hello", "needs": ["root"]},
            {"id": "later", "op": "hello", "needs": ["a"]}
        ]
    }"#,
    )
    .unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    assert_eq!(plan.waves.len(), 3);
    assert_eq!(plan.waves[1].len(), 3);

    let (mut client, shutdown) = spawn_todo();
    let err = run_plan(&mut client, &plan, &RunOptions::new()).unwrap_err();
    match err {
        RunError::Step { id, receipt, .. } => {
            assert_eq!(id, "b");
            assert!(!receipt.ok);
            assert_eq!(
                receipt.steps.len(),
                4,
                "pipelined read siblings already ran: {receipt:?}"
            );
            assert!(receipt.steps[0].ok, "{receipt:?}");
            let wave: Vec<_> = receipt.steps.iter().skip(1).collect();
            assert_eq!(wave.len(), 3);
            let by_id: std::collections::BTreeMap<_, _> =
                wave.iter().map(|s| (s.id.as_str(), s.ok)).collect();
            assert_eq!(by_id.get("a"), Some(&true), "{receipt:?}");
            assert_eq!(by_id.get("b"), Some(&false), "{receipt:?}");
            assert_eq!(
                by_id.get("c"),
                Some(&true),
                "hello sibling still ran after assert fail: {receipt:?}"
            );
            assert!(
                !receipt.steps.iter().any(|s| s.id == "later"),
                "failed read wave must not start the next wave: {receipt:?}"
            );
        }
        other => panic!("unexpected {other}"),
    }
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn independent_write_wave_stops_before_later_sibling() {
    let recipe = gpui_agent_recipe::Recipe::from_json(
        r#"{
        "name": "wide-writes",
        "steps": [
            {"id": "root", "op": "wait"},
            {"id": "a", "op": "click", "target": "no-such-a", "needs": ["root"]},
            {"id": "b", "op": "click", "target": "todo-add", "needs": ["root"]}
        ]
    }"#,
    )
    .unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    assert_eq!(plan.waves.len(), 2);
    assert_eq!(plan.waves[1], vec!["a".to_string(), "b".to_string()]);

    let (mut client, shutdown) = spawn_todo();
    let err = run_plan(&mut client, &plan, &RunOptions::new()).unwrap_err();
    match err {
        RunError::Step { id, receipt, .. } => {
            assert_eq!(id, "a");
            assert!(!receipt.ok);
            assert_eq!(
                receipt.steps.len(),
                2,
                "write-wave fail-fast must not run later siblings: {receipt:?}"
            );
            assert_eq!(receipt.steps[0].id, "root");
            assert!(receipt.steps[0].ok, "{receipt:?}");
            assert_eq!(receipt.steps[1].id, "a");
            assert!(!receipt.steps[1].ok, "{receipt:?}");
            assert!(
                !receipt.steps.iter().any(|s| s.id == "b"),
                "later click must not run: {receipt:?}"
            );
        }
        other => panic!("unexpected {other}"),
    }
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn mixed_write_and_read_wave_stays_sequential_fail_fast() {
    let recipe = gpui_agent_recipe::Recipe::from_json(
        r#"{
        "name": "mixed-wave",
        "steps": [
            {"id": "root", "op": "wait"},
            {"id": "click", "op": "click", "target": "no-such-click", "needs": ["root"]},
            {"id": "hello", "op": "hello", "needs": ["root"]}
        ]
    }"#,
    )
    .unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    assert_eq!(plan.waves.len(), 2);
    assert_eq!(
        plan.waves[1],
        vec!["click".to_string(), "hello".to_string()]
    );

    let (mut client, shutdown) = spawn_todo();
    let err = run_plan(&mut client, &plan, &RunOptions::new()).unwrap_err();
    match err {
        RunError::Step { id, receipt, .. } => {
            assert_eq!(id, "click");
            assert!(!receipt.ok);
            assert_eq!(
                receipt.steps.len(),
                2,
                "mixed wave must not pipeline the hello sibling: {receipt:?}"
            );
            assert!(
                !receipt.steps.iter().any(|s| s.id == "hello"),
                "hello sibling must not run after click fail: {receipt:?}"
            );
        }
        other => panic!("unexpected {other}"),
    }
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn screenshot_dir_records_a_receipt_entry_per_step() {
    let recipe = gpui_agent_recipe::Recipe::from_json(
        r#"{
        "name": "wide-shots",
        "steps": [
            {"id": "root", "op": "wait"},
            {"id": "a", "op": "hello", "needs": ["root"]},
            {"id": "b", "op": "hello", "needs": ["root"]},
            {"id": "c", "op": "hello", "needs": ["root"]}
        ]
    }"#,
    )
    .unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    let dir = std::env::temp_dir().join(format!("gpui-agent-wide-shots-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let capture = ScreenshotCapture::new(dir.clone());

    let (mut client, shutdown) = spawn_todo();
    let receipt = run_plan(
        &mut client,
        &plan,
        &RunOptions::new().with_screenshots(Some(capture)),
    )
    .expect("hello wave succeeds");
    assert!(receipt.ok, "{receipt:?}");
    assert_eq!(
        receipt.screenshots.len(),
        4,
        "screenshot-dir must stay sequential and turn after each step: {receipt:?}"
    );
    assert_eq!(receipt.steps.len(), 4);
    assert!(
        receipt.steps.iter().all(|step| step.screenshot.is_some()),
        "pipeline path sets screenshot: None; screenshot-dir must capture after each step: {receipt:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn recipe_does_not_bypass_token() {
    let store = Arc::new(Mutex::new(TodoStore::new(PlatformKind::Headless)));
    let (addr, shutdown) = spawn_host(
        "127.0.0.1:0".parse().expect("valid addr"),
        Some("secret".into()),
        store,
    )
    .expect("bind");

    let recipe = parse_wants("hello", "auth").unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();

    let mut no_token = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
    let err = run_plan(&mut no_token, &plan, &RunOptions::new()).unwrap_err();
    match err {
        RunError::Step { error, .. } => {
            assert!(
                error.contains("token"),
                "recipe must still hit auth: {error}"
            );
        }
        other => panic!("unexpected {other}"),
    }

    let mut with_token = AgentClient::connect(addr)
        .with_token("secret")
        .with_timeout(Duration::from_secs(2));
    let receipt = run_plan(&mut with_token, &plan, &RunOptions::new()).expect("authed");
    assert!(receipt.ok);

    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn shutdown_recipe_requires_yes() {
    let recipe = parse_wants("hello\nshutdown", "bye").unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    let (mut client, shutdown) = spawn_todo();
    let err = run_plan(&mut client, &plan, &RunOptions::new()).unwrap_err();
    assert!(matches!(err, RunError::NeedsYes));
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn assert_fail_stops_and_keeps_partial_receipt() {
    let wants = r#"
wait
invoke todo.add title="Buy milk"
assert todo-item-1 name="NOPE"
invoke todo.toggle id=1
"#;
    let recipe = parse_wants(wants, "partial").unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    let (mut client, shutdown) = spawn_todo();
    let err = run_plan(&mut client, &plan, &RunOptions::new()).unwrap_err();
    match err {
        RunError::Step { id, receipt, .. } => {
            assert_eq!(id, "s3");
            assert!(!receipt.ok);
            assert_eq!(
                receipt.steps.len(),
                3,
                "must stop before toggle: {receipt:?}"
            );
            assert!(receipt.steps[0].ok, "{receipt:?}");
            assert!(receipt.steps[1].ok, "{receipt:?}");
            assert!(!receipt.steps[2].ok, "{receipt:?}");
            // The invoke acted and was answered; the assert is a read.
            assert_eq!(
                receipt.steps[1].delivery_status,
                Some(gpui_agent::DeliveryStatus::DeliveredUnverified),
                "{receipt:?}"
            );
            assert_eq!(
                receipt.steps[2].error_code.as_deref(),
                Some("assert_failed"),
                "{receipt:?}"
            );
            assert_eq!(receipt.steps[2].delivery_status, None, "{receipt:?}");
        }
        other => panic!("unexpected {other}"),
    }
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn host_down_fails_without_running_later_steps() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);

    let recipe = parse_wants("hello\nsnapshot", "down").unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    let mut client = AgentClient::connect(addr).with_timeout(Duration::from_millis(250));
    let err = run_plan(&mut client, &plan, &RunOptions::new()).unwrap_err();
    match err {
        RunError::Step {
            id, receipt, error, ..
        } => {
            assert_eq!(id, "s1");
            assert!(!receipt.ok);
            assert_eq!(receipt.steps.len(), 1);
            assert!(!receipt.steps[0].ok);
            assert_eq!(
                receipt.steps[0].error_code.as_deref(),
                Some("connect_failed"),
                "{receipt:?}"
            );
            assert!(
                error.contains("connect") || error.contains("Connection refused"),
                "{error}"
            );
        }
        other => panic!("unexpected {other}"),
    }
}

#[test]
fn wrong_token_fails_closed() {
    let store = Arc::new(Mutex::new(TodoStore::new(PlatformKind::Headless)));
    let (addr, shutdown) = spawn_host(
        "127.0.0.1:0".parse().unwrap(),
        Some("correct".into()),
        store,
    )
    .expect("bind");

    let recipe = parse_wants("hello", "auth").unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    let mut client = AgentClient::connect(addr)
        .with_token("wrong")
        .with_timeout(Duration::from_secs(2));
    let err = run_plan(&mut client, &plan, &RunOptions::new()).unwrap_err();
    match err {
        RunError::Step { error, receipt, .. } => {
            assert!(
                error.contains("invalid automation token") || error.contains("token"),
                "{error}"
            );
            assert!(!receipt.ok);
            assert_eq!(receipt.steps.len(), 1);
        }
        other => panic!("unexpected {other}"),
    }

    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn second_recipe_reuses_same_client_session() {
    let hello = parse_wants("hello", "a").unwrap();
    let snap = parse_wants("snapshot", "b").unwrap();
    let hello_plan = compile_plan(&hello, &BTreeMap::new(), &todo_registry()).unwrap();
    let snap_plan = compile_plan(&snap, &BTreeMap::new(), &todo_registry()).unwrap();

    let (mut client, shutdown) = spawn_todo();
    let first = run_plan(&mut client, &hello_plan, &RunOptions::new()).expect("hello");
    assert!(first.ok);
    assert!(
        client.has_session(),
        "first recipe should leave the TCP session open"
    );

    let second = run_plan(&mut client, &snap_plan, &RunOptions::new()).expect("snapshot");
    assert!(second.ok, "{second:?}");
    assert!(
        second.session_reused && client.has_session(),
        "second recipe on the same AgentClient must reuse the connection"
    );

    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn headless_screenshots_are_honestly_unavailable() {
    let recipe = parse_wants("wait\nhello", "shots").unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    let dir =
        std::env::temp_dir().join(format!("gpui-agent-shots-headless-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let capture = ScreenshotCapture::new(dir.clone());

    let (mut client, shutdown) = spawn_todo();
    let receipt = run_plan(
        &mut client,
        &plan,
        &RunOptions::new().with_screenshots(Some(capture)),
    )
    .expect("recipe steps themselves succeed");
    assert!(receipt.ok, "{receipt:?}");
    assert_eq!(receipt.screenshots.len(), 2);
    assert_eq!(receipt.steps.len(), 2);
    for (i, shot) in receipt.screenshots.iter().enumerate() {
        assert!(!shot.ok, "{shot:?}");
        assert!(
            gpui_agent::is_screenshot_unavailable(shot.error.as_deref().unwrap_or("")),
            "{shot:?}"
        );
        assert!(
            !std::path::Path::new(&shot.path).exists(),
            "must not invent {}",
            shot.path
        );
        assert!(receipt.steps[i].screenshot.is_some());
    }
    assert!(
        receipt.screenshots[0].path.ends_with("001-s1.png"),
        "{receipt:?}"
    );
    assert!(
        receipt.screenshots[1].path.ends_with("002-s2.png"),
        "{receipt:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}

struct PngHost {
    inner: TodoStore,
}

impl AgentHost for PngHost {
    fn hello(&self) -> HelloInfo {
        self.inner.hello()
    }

    fn snapshot(&self) -> UiTree {
        self.inner.snapshot()
    }

    fn dispatch(&mut self, op: &Op) -> Result<DispatchResult, String> {
        self.inner.dispatch(op)
    }

    fn screenshot(&self, spec: gpui_agent::ScreenshotSpec<'_>) -> Result<DispatchResult, String> {
        let path = spec
            .path
            .ok_or_else(|| "screenshot requires path".to_string())?;
        gpui_agent::write_png(path, gpui_agent::TEST_PNG).map(DispatchResult::json)
    }
}

/// A host that speaks the protocol by hand and lists no
/// `screenshot_inline`: it is sent a file name, writes the PNG under its own
/// directory and answers with `path` as given (or with none).
fn host_file_shot_host(reported: Option<&'static str>) -> std::net::SocketAddr {
    use gpui_agent::protocol::{HelloAuth, PROTOCOL_VERSION, Request, Response};
    use std::io::{BufRead, BufReader, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test fixture");
    let addr = listener.local_addr().expect("test fixture");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut reader = BufReader::new(stream.try_clone().expect("test fixture"));
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let request: Request = serde_json::from_str(&line).expect("test fixture");
                let mut response = Response::ok(&request.id);
                match request.op {
                    Op::Hello | Op::Wait { .. } => {
                        response.hello = Some(HelloInfo {
                            protocol: PROTOCOL_VERSION,
                            app: "host-file-shots".into(),
                            platform: PlatformKind::Headless,
                            os: gpui_agent::protocol::host_os(),
                            ready: true,
                            deliveries: Vec::new(),
                            auth: HelloAuth::None,
                            bundle_id: None,
                            executable: None,
                            features: Vec::new(),
                            framework: None,
                            device: None,
                        });
                    }
                    Op::Screenshot { path, inline, .. } => {
                        assert!(!inline, "a host without the feature is not asked for bytes");
                        assert_eq!(
                            path.as_deref(),
                            Some("001-wait.png"),
                            "the host is sent the file name only"
                        );
                        response.result = Some(match reported {
                            Some(path) => serde_json::json!({"path": path}),
                            None => serde_json::json!({}),
                        });
                    }
                    _ => response = Response::err(&request.id, "unexpected operation"),
                }
                let response = serde_json::to_string(&response).expect("test fixture");
                if writeln!(stream, "{response}").is_err() {
                    break;
                }
            }
        }
    });
    addr
}

#[test]
fn host_written_step_png_is_reported_where_the_host_says() {
    let recipe = gpui_agent_recipe::Recipe::from_json(
        r#"{"name":"shots","steps":[{"id":"wait","op":"wait"}]}"#,
    )
    .unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    let dir = std::env::temp_dir().join(format!("gpui-agent-shots-host-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let run = |reported| {
        let mut client = AgentClient::connect(host_file_shot_host(reported))
            .with_timeout(Duration::from_secs(3));
        let options = RunOptions::new().with_screenshots(Some(ScreenshotCapture::new(&dir)));
        run_plan(&mut client, &plan, &options).unwrap()
    };

    let receipt = run(Some("/host/shots/001-wait.png"));
    assert!(receipt.ok, "{receipt:?}");
    assert_eq!(receipt.screenshots.len(), 1, "{receipt:?}");
    assert!(receipt.screenshots[0].ok, "{receipt:?}");
    assert_eq!(receipt.screenshots[0].path, "/host/shots/001-wait.png");
    // The client wrote nothing: the file is the host's.
    assert!(!dir.join("001-wait.png").exists());

    // A host that names no path: the receipt falls back to the name it was
    // asked to write, under the client's directory.
    let receipt = run(None);
    assert!(receipt.screenshots[0].ok, "{receipt:?}");
    assert_eq!(
        receipt.screenshots[0].path,
        dir.join("001-wait.png").to_string_lossy()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mocked_host_writes_step_pngs_onto_receipt() {
    let store = Arc::new(Mutex::new(PngHost {
        inner: TodoStore::new(PlatformKind::Headless),
    }));
    let (addr, shutdown) =
        spawn_host("127.0.0.1:0".parse().expect("valid addr"), None, store).expect("bind");
    let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(3));

    let json = r#"{
        "name": "flagged-shots",
        "steps": [
            {"id": "wait", "op": "wait"},
            {"id": "add", "op": "invoke", "name": "todo.add", "args": {"title": "Milk"}, "screenshot": true}
        ]
    }"#;
    let recipe = gpui_agent_recipe::Recipe::from_json(json).unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    let dir = std::env::temp_dir().join(format!("gpui-agent-shots-mock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let every = ScreenshotCapture::new(dir.clone());
    let receipt = run_plan(
        &mut client,
        &plan,
        &RunOptions::new().with_screenshots(Some(every)),
    )
    .unwrap();
    assert!(receipt.ok, "{receipt:?}");
    assert_eq!(receipt.screenshots.len(), 2);
    assert!(receipt.screenshots.iter().all(|s| s.ok), "{receipt:?}");
    // The receipt's path is where the file actually is: in the client's
    // `--screenshot-dir`, not wherever the host happens to write.
    assert_eq!(
        receipt.screenshots[0].path,
        dir.join("001-wait.png").to_string_lossy()
    );
    for shot in &receipt.screenshots {
        assert_eq!(std::fs::read(&shot.path).unwrap(), gpui_agent::TEST_PNG);
    }

    let flagged_dir = dir.join("flagged");
    let flagged = ScreenshotCapture::new(flagged_dir.clone()).with_flagged_only(true);
    let flagged_receipt = run_plan(
        &mut client,
        &plan,
        &RunOptions::new().with_screenshots(Some(flagged)),
    )
    .unwrap();
    assert_eq!(flagged_receipt.screenshots.len(), 1);
    assert!(flagged_receipt.screenshots[0].ok);
    assert!(
        flagged_receipt.screenshots[0].path.ends_with("002-add.png"),
        "{flagged_receipt:?}"
    );
    assert_eq!(
        std::fs::read(flagged_dir.join("002-add.png")).unwrap(),
        gpui_agent::TEST_PNG
    );
    let _ = std::fs::remove_dir_all(&dir);
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn paced_type_sends_one_key_per_request_with_the_pause_between() {
    let recipe = gpui_agent_recipe::Recipe::from_json(
        r#"{"name": "paced", "steps": [
            {"id": "wait", "op": "wait"},
            {"id": "amount", "op": "type", "target": "todo-input", "text": "1,234.56", "key_delay_ms": 20}
        ]}"#,
    )
    .unwrap();
    validate_recipe(&recipe, &todo_registry()).unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    assert_eq!(plan.steps[1].key_delay_ms, Some(20));

    let (mut client, shutdown) = spawn_todo();
    let receipt = run_plan(&mut client, &plan, &RunOptions::new()).expect("run");
    assert!(receipt.ok, "{receipt:?}");
    let typed = &receipt.steps[1];
    assert_eq!(
        typed.result.as_ref().and_then(|r| r.get("value")),
        Some(&serde_json::json!("1,234.56")),
        "every key arrived, in order: {typed:?}"
    );
    // Eight keys, seven pauses of 20 ms between them.
    assert!(typed.elapsed_ms >= 140, "the keys were paced: {typed:?}");

    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn paced_type_stops_at_the_first_refused_key() {
    let recipe = gpui_agent_recipe::Recipe::from_json(
        r#"{"name": "refused", "steps": [
            {"id": "amount", "op": "type", "target": "todo-add", "text": "12", "key_delay_ms": 1000}
        ]}"#,
    )
    .unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    let (mut client, shutdown) = spawn_todo();
    let started = std::time::Instant::now();
    let Err(RunError::Step { id, error, .. }) = run_plan(&mut client, &plan, &RunOptions::new())
    else {
        panic!("typing into a button must fail");
    };
    assert_eq!(id, "amount");
    assert!(error.contains("not editable"), "{error}");
    assert!(
        started.elapsed() < Duration::from_millis(900),
        "no pause after the refused first key"
    );
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}
