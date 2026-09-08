use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_agent::client::AgentClient;
use gpui_agent::protocol::{AssertSpec, DeliveryMode, Op, PlatformKind};
use gpui_agent::server::spawn_host;
use todo_core::TodoStore;
use todo_core::ids;

#[test]
fn agent_can_create_toggle_delete_over_tcp() {
    let store = Arc::new(Mutex::new(TodoStore::new(PlatformKind::Headless)));
    let (addr, shutdown) =
        spawn_host("127.0.0.1:0".parse().unwrap(), None, store.clone()).expect("bind");

    let mut client = AgentClient::connect(addr).with_timeout(Duration::from_secs(3));
    client.wait_ready().expect("hello");

    client
        .set_value(ids::INPUT, "Buy milk")
        .expect("type title");
    client.click(ids::ADD).expect("add");

    let snap = client.snapshot().expect("snapshot");
    let tree = snap.tree.expect("tree");
    let item = tree.find("todo-item-1").expect("todo-item-1");
    assert_eq!(item.name, "Buy milk");
    assert_eq!(item.checked, Some(false));

    client.click("todo-toggle-1").expect("toggle");
    client
        .assert(AssertSpec {
            target: "todo-item-1".into(),
            checked: Some(true),
            name: Some("Buy milk".into()),
            ..Default::default()
        })
        .expect("checked");

    client.click("todo-delete-1").expect("delete");
    client
        .assert(AssertSpec {
            target: "todo-item-1".into(),
            exists: Some(false),
            ..Default::default()
        })
        .expect("gone");

    let list = client.invoke("todo.list", serde_json::json!({})).unwrap();
    assert_eq!(list.result.unwrap(), serde_json::json!([]));

    let virt = client.rpc(Op::click_virtual(ids::ADD)).expect("rpc");
    assert!(!virt.ok);
    let err = virt.error.expect("virtual error");
    assert!(err.starts_with(gpui_agent::VIRTUAL_UNAVAILABLE), "{err}");

    let hello = client.expect_ok(Op::Hello).unwrap().hello.unwrap();
    assert_eq!(hello.deliveries, vec![DeliveryMode::Semantic]);

    client.expect_ok(Op::Shutdown).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    assert!(
        shutdown.load(std::sync::atomic::Ordering::SeqCst)
            || store.lock().unwrap().wants_shutdown()
    );
}
