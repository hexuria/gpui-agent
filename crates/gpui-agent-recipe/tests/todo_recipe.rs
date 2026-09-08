use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_agent::client::AgentClient;
use gpui_agent::protocol::PlatformKind;
use gpui_agent::server::spawn_host;
use gpui_agent_recipe::{
    RunError, compile_plan, parse_wants, run_plan, todo_registry, validate_recipe,
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
