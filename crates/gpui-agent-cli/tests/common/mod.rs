//! Shared harness for the headless-todo integration tests. Each test
//! binary gets its own copy — `dead_code` is allowed because not every
//! binary uses every helper.
#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gpui_agent::AgentHost;
use gpui_agent::protocol::PlatformKind;
use gpui_agent::server::spawn_host;
use serde_json::json;
use todo_core::TodoStore;

/// A spawned headless todo host; `token` doubles as the HMAC secret and
/// a per-test-file marker in the spawned process's args.
pub struct Todo {
    pub addr: String,
    shutdown: Arc<AtomicBool>,
    store: Arc<Mutex<TodoStore>>,
}

impl Todo {
    pub fn spawn(token: &str) -> Self {
        let store = Arc::new(Mutex::new(TodoStore::new(PlatformKind::Headless)));
        let (addr, shutdown) = spawn_host(
            "127.0.0.1:0".parse().unwrap(),
            Some(token.to_owned()),
            store.clone(),
        )
        .expect("bind");
        Self {
            addr: addr.to_string(),
            shutdown,
            store,
        }
    }

    pub fn seed(&self, titles: &[&str]) {
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

    pub fn titles(&self) -> Vec<String> {
        self.store
            .lock()
            .unwrap()
            .items()
            .iter()
            .map(|t| t.title.clone())
            .collect()
    }

    pub fn draft(&self) -> String {
        self.store.lock().unwrap().draft().to_owned()
    }

    pub fn done_titles(&self) -> Vec<String> {
        self.store
            .lock()
            .unwrap()
            .items()
            .iter()
            .filter(|t| t.done)
            .map(|t| t.title.clone())
            .collect()
    }
}

impl Drop for Todo {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}
