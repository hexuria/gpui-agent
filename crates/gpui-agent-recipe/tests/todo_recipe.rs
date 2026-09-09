use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_agent::client::AgentClient;
use gpui_agent::protocol::{Op, PlatformKind};
use gpui_agent::server::spawn_host;
use gpui_agent_recipe::{
    compile_plan, parse_recipe, parse_wants, run_plan, todo_registry, validate_recipe, RunError,
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

fn sample(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/recipes")
        .join(name)
}

#[test]
fn advertised_sample_files_run_on_headless() {
    for name in ["todo-crud.json", "todo-crud.wants"] {
        let recipe = parse_recipe(&sample(name)).expect(name);
        validate_recipe(&recipe, &todo_registry()).expect(name);
        let mut set = BTreeMap::new();
        set.insert("title".into(), "Buy milk".into());
        let plan = compile_plan(&recipe, &set, &todo_registry()).expect(name);
        assert_eq!(plan.steps.len(), 5, "{name}");
        assert!(!plan.requires_yes, "{name}");

        let (mut client, shutdown) = spawn_todo();
        let receipt = run_plan(&mut client, &plan, false).expect(name);
        assert!(receipt.ok, "{name} {receipt:?}");
        assert_eq!(receipt.steps.len(), 5, "{name}");
        assert!(receipt.session_reused, "{name}");
        shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[test]
fn hello_and_snapshot_receipts_keep_payloads() {
    let recipe = parse_wants("hello\nsnapshot", "perceive").unwrap();
    let plan = compile_plan(&recipe, &BTreeMap::new(), &todo_registry()).unwrap();
    let (mut client, shutdown) = spawn_todo();
    let receipt = run_plan(&mut client, &plan, false).expect("run");
    assert!(receipt.ok, "{receipt:?}");
    assert_eq!(receipt.steps.len(), 2);
    let hello = receipt.steps[0]
        .hello
        .as_ref()
        .expect("hello payload on first step");
    assert_eq!(hello.protocol, 1);
    assert_eq!(hello.app, "todo");
    let tree = receipt.steps[1]
        .tree
        .as_ref()
        .expect("snapshot tree on second step");
    assert_eq!(tree.app, "todo");
    assert!(
        !tree.nodes.is_empty(),
        "snapshot must not be an empty perceive: {tree:?}"
    );
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[test]
fn toggle_id_param_binds_as_a_number() {
    let json = r#"{
        "name": "toggle-param",
        "params": ["title", "id"],
        "steps": [
            {"id": "wait", "op": "wait"},
            {"id": "add", "op": "invoke", "name": "todo.add", "args": {"title": "$title"}, "needs": ["wait"]},
            {"id": "toggle", "op": "invoke", "name": "todo.toggle", "args": {"id": "$id"}, "needs": ["add"]}
        ]
    }"#;
    let recipe = gpui_agent_recipe::Recipe::from_json(json).unwrap();
    let mut set = BTreeMap::new();
    set.insert("title".into(), "Buy milk".into());
    set.insert("id".into(), "1".into());
    let plan = compile_plan(&recipe, &set, &todo_registry()).unwrap();
    match &plan.steps[2].op {
        Op::Invoke { args, .. } => assert!(args["id"].is_number(), "{args}"),
        other => panic!("{other:?}"),
    }
    let (mut client, shutdown) = spawn_todo();
    let receipt = run_plan(&mut client, &plan, false).expect("run");
    assert!(receipt.ok, "{receipt:?}");
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
}
