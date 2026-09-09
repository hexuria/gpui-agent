use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use gpui_agent::protocol::PlatformKind;
use gpui_agent::security::from_env;
use gpui_agent::server::spawn_host;
use todo_core::TodoStore;

fn main() {
    let config = match from_env() {
        Ok(Some(config)) => config,
        Ok(None) => {
            eprintln!(
                "todo-headless is an automation host. Start it with GPUI_AGENT=1.\n\
                 Example: GPUI_AGENT=1 cargo run -p todo-headless"
            );
            std::process::exit(2);
        }
        Err(err) => {
            eprintln!("refusing to start automation: {err}");
            std::process::exit(2);
        }
    };

    let store = Arc::new(Mutex::new(TodoStore::new(PlatformKind::Headless)));
    let token_set = config.token.is_some();
    let (addr, shutdown) =
        spawn_host(config.addr, config.token, store.clone()).expect("bind agent server");

    eprintln!("gpui-agent listening on {addr} (platform=headless, app=todo)");
    eprintln!("opt-in: GPUI_AGENT=1 · loopback only · protocol v1");
    if token_set {
        eprintln!(
            "auth: required (GPUI_AGENT_TOKEN set; recipe/MCP clients must send the same token)"
        );
    } else {
        eprintln!(
            "auth: none (one-off click/snapshot ok; recipe run and mcp need the same token on host and client)"
        );
    }
    eprintln!("delivery: semantic only (virtual_unavailable — no GPUI event pipeline)");

    while !shutdown.load(std::sync::atomic::Ordering::SeqCst) {
        if store.lock().expect("store").wants_shutdown() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
}
