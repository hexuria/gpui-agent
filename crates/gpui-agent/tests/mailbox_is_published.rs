//! A host served through `spawn_mailbox` (how GPUI desktop apps run the agent, since the window
//! answers on the main thread) is listed like a `serve_host` one, so `--connect` finds it (#47).
//!
//! Its own test binary, because the records go to the directory `GPUI_AGENT_RUNTIME_DIR` names
//! and the environment is shared by every test in a process.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use gpui_agent::discovery::{self, HostRecord, RUNTIME_DIR_ENV};
use gpui_agent::mailbox::AgentMailbox;
use gpui_agent::server;

fn found(root: &Path, id: &str) -> Option<HostRecord> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Ok(record) = discovery::resolve_id(root, id) {
            return Some(record);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_mailbox_host_is_found_by_its_executable_and_by_the_name_it_is_given() {
    let root = std::env::temp_dir().join(format!("gpui-agent-mailbox-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    // SAFETY: this is the only test in this binary, and it sets the variable before it starts any
    // thread that could read the environment.
    unsafe { std::env::set_var(RUNTIME_DIR_ENV, &root) };
    let any = "127.0.0.1:0".parse().unwrap();

    let (addr, stop) =
        server::spawn_mailbox(any, None, AgentMailbox::new(), Duration::from_secs(1)).unwrap();
    let exe = discovery::detect_identity().executable;
    let record =
        found(&root, &exe).expect("an unnamed mailbox host is listed under its executable");
    assert_eq!(record.addr, addr.to_string());

    let (named, stop_named) = server::spawn_mailbox_named(
        any,
        None,
        AgentMailbox::new(),
        Duration::from_secs(1),
        "todo-gpui",
    )
    .unwrap();
    let record = found(&root, "todo-gpui").expect("a named mailbox host is listed under its name");
    assert_eq!(record.addr, named.to_string());
    assert_eq!(record.app, "todo-gpui");

    // Stopping the server takes its record away with it.
    stop_named.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(3);
    while discovery::resolve_id(&root, "todo-gpui").is_ok() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        discovery::resolve_id(&root, "todo-gpui").is_err(),
        "a stopped host is not listed"
    );

    stop.store(true, Ordering::SeqCst);
    let _ = std::fs::remove_dir_all(&root);
}
