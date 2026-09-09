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
    RunError, ScreenshotCapture, compile_plan, parse_wants, run_plan, run_plan_with_screenshots,
    todo_registry, validate_recipe,
};
use todo_core::TodoStore;

fn spawn_todo() -> (AgentClient, Arc<std::sync::atomic::AtomicBool>) {
    let store = Arc::new(Mutex::new(TodoStore::new(PlatformKind::Headless)));
    let (addr, shutdown) = spawn_host("127.0.0.1:0".parse().unwrap(), None, store).expect("bind");
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
    let receipt = run_plan(&mut client, &plan, false).expect("run");
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
    let receipt = run_plan(&mut client, &plan, false).expect("run");
    assert!(receipt.ok, "{receipt:?}");
    assert!(
        receipt.session_reused,
        "matching token must still reuse the TCP session"
    );

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
    let receipt = run_plan(&mut client, &plan, false).expect("run");
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
    let receipt = run_plan(&mut client, &plan, false).expect("run");
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
    let err = run_plan(&mut client, &plan, false).unwrap_err();
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
    let err = run_plan(&mut client, &plan, false).unwrap_err();
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
    let err = run_plan(&mut client, &plan, false).unwrap_err();
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
    let capture = ScreenshotCapture {
        dir: dir.clone(),
        flagged_only: false,
    };

    let (mut client, shutdown) = spawn_todo();
    let receipt = run_plan_with_screenshots(&mut client, &plan, false, Some(&capture))
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
    let (addr, shutdown) =
        spawn_host("127.0.0.1:0".parse().unwrap(), Some("secret".into()), store).expect("bind");

    let recipe = parse_wants("hello", "auth").unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();

    let mut no_token = AgentClient::connect(addr).with_timeout(Duration::from_secs(2));
    let err = run_plan(&mut no_token, &plan, false).unwrap_err();
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
    let receipt = run_plan(&mut with_token, &plan, false).expect("authed");
    assert!(receipt.ok);

    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn shutdown_recipe_requires_yes() {
    let recipe = parse_wants("hello\nshutdown", "bye").unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    let (mut client, shutdown) = spawn_todo();
    let err = run_plan(&mut client, &plan, false).unwrap_err();
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
    let err = run_plan(&mut client, &plan, false).unwrap_err();
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
    let err = run_plan(&mut client, &plan, false).unwrap_err();
    match err {
        RunError::Step { id, receipt, error } => {
            assert_eq!(id, "s1");
            assert!(!receipt.ok);
            assert_eq!(receipt.steps.len(), 1);
            assert!(!receipt.steps[0].ok);
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
    let err = run_plan(&mut client, &plan, false).unwrap_err();
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
    let first = run_plan(&mut client, &hello_plan, false).expect("hello");
    assert!(first.ok);
    assert!(
        client.has_session(),
        "first recipe should leave the TCP session open"
    );

    let second = run_plan(&mut client, &snap_plan, false).expect("snapshot");
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
    let capture = ScreenshotCapture {
        dir: dir.clone(),
        flagged_only: false,
    };

    let (mut client, shutdown) = spawn_todo();
    let receipt = run_plan_with_screenshots(&mut client, &plan, false, Some(&capture))
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

    fn screenshot(&self, path: Option<&str>) -> Result<DispatchResult, String> {
        let path = path.ok_or_else(|| "screenshot requires path".to_string())?;
        gpui_agent::write_png(path, gpui_agent::TEST_PNG).map(DispatchResult::json)
    }
}

#[test]
fn mocked_host_writes_step_pngs_onto_receipt() {
    let store = Arc::new(Mutex::new(PngHost {
        inner: TodoStore::new(PlatformKind::Headless),
    }));
    let (addr, shutdown) = spawn_host("127.0.0.1:0".parse().unwrap(), None, store).expect("bind");
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

    let every = ScreenshotCapture {
        dir: dir.clone(),
        flagged_only: false,
    };
    let receipt = run_plan_with_screenshots(&mut client, &plan, false, Some(&every)).unwrap();
    assert!(receipt.ok, "{receipt:?}");
    assert_eq!(receipt.screenshots.len(), 2);
    assert!(receipt.screenshots.iter().all(|s| s.ok), "{receipt:?}");
    let wait_png = dir.join("001-wait.png");
    let add_png = dir.join("002-add.png");
    assert_eq!(std::fs::read(&wait_png).unwrap(), gpui_agent::TEST_PNG);
    assert_eq!(std::fs::read(&add_png).unwrap(), gpui_agent::TEST_PNG);

    let flagged_dir = dir.join("flagged");
    let flagged = ScreenshotCapture {
        dir: flagged_dir.clone(),
        flagged_only: true,
    };
    let flagged_receipt =
        run_plan_with_screenshots(&mut client, &plan, false, Some(&flagged)).unwrap();
    assert_eq!(flagged_receipt.screenshots.len(), 1);
    assert!(flagged_receipt.screenshots[0].ok);
    assert!(
        flagged_receipt.screenshots[0].path.ends_with("002-add.png"),
        "{flagged_receipt:?}"
    );
    assert!(!flagged_dir.join("001-wait.png").exists());
    assert_eq!(
        std::fs::read(flagged_dir.join("002-add.png")).unwrap(),
        gpui_agent::TEST_PNG
    );

    let _ = std::fs::remove_dir_all(&dir);
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}
