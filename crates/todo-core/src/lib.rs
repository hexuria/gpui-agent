//! Domain model + `AgentHost` for the demo todo app.
//!
//! Both the GPUI Kit window and the headless host call the same methods, so
//! an agent action (`click todo-add`) and a human click run identical code.

use gpui_agent::dispatch::DispatchResult;
use gpui_agent::host::AgentHost;
use gpui_agent::protocol::{DeliveryMode, HelloInfo, Op, PROTOCOL_VERSION, PlatformKind};
use gpui_agent::tree::{UiNode, UiTree};
use gpui_agent::virtual_unavailable;
use serde::{Deserialize, Serialize};

/// Stable ids for the sample todo app. Other GPUI Kit apps define their own.
pub mod ids {
    pub const INPUT: &str = "todo-input";
    pub const ADD: &str = "todo-add";
    pub const LIST: &str = "todo-list";
    pub const EMPTY: &str = "todo-empty";
    pub const STATUS: &str = "todo-status";
    pub const WINDOW: &str = "todo-window";

    pub fn item(id: u64) -> String {
        format!("todo-item-{id}")
    }

    pub fn toggle(id: u64) -> String {
        format!("todo-toggle-{id}")
    }

    pub fn delete(id: u64) -> String {
        format!("todo-delete-{id}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Todo {
    pub id: u64,
    pub title: String,
    pub done: bool,
}

#[derive(Debug, Clone)]
pub struct TodoStore {
    next_id: u64,
    items: Vec<Todo>,
    draft: String,
    platform: PlatformKind,
    shutdown: bool,
}

impl Default for TodoStore {
    fn default() -> Self {
        Self::new(PlatformKind::Headless)
    }
}

impl TodoStore {
    pub fn new(platform: PlatformKind) -> Self {
        Self {
            next_id: 1,
            items: Vec::new(),
            draft: String::new(),
            platform,
            shutdown: false,
        }
    }

    pub fn items(&self) -> &[Todo] {
        &self.items
    }

    pub fn draft(&self) -> &str {
        &self.draft
    }

    pub fn set_draft(&mut self, draft: impl Into<String>) {
        self.draft = draft.into();
    }

    pub fn wants_shutdown(&self) -> bool {
        self.shutdown
    }

    pub fn add(&mut self, title: impl Into<String>) -> Result<Todo, String> {
        let title = title.into().trim().to_string();
        if title.is_empty() {
            return Err("cannot add an empty todo".into());
        }
        let todo = Todo {
            id: self.next_id,
            title,
            done: false,
        };
        self.next_id += 1;
        self.items.push(todo.clone());
        self.draft.clear();
        Ok(todo)
    }

    pub fn add_from_draft(&mut self) -> Result<Todo, String> {
        self.add(self.draft.clone())
    }

    pub fn toggle(&mut self, id: u64) -> Result<Todo, String> {
        let item = self
            .items
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or_else(|| format!("todo {id} not found"))?;
        item.done = !item.done;
        Ok(item.clone())
    }

    pub fn delete(&mut self, id: u64) -> Result<Todo, String> {
        let idx = self
            .items
            .iter()
            .position(|item| item.id == id)
            .ok_or_else(|| format!("todo {id} not found"))?;
        Ok(self.items.remove(idx))
    }

    pub fn tree(&self) -> UiTree {
        let status_name = match self.items.len() {
            0 => "No todos".to_string(),
            n => {
                let done = self.items.iter().filter(|item| item.done).count();
                format!("{n} todos, {done} done")
            }
        };

        let list_children = if self.items.is_empty() {
            vec![UiNode::new(
                ids::EMPTY,
                "note",
                "No todos yet. Add one above.",
            )]
        } else {
            let mut list_children = Vec::with_capacity(self.items.len());
            for item in &self.items {
                list_children.push(
                    UiNode::new(ids::item(item.id), "listitem", item.title.clone())
                        .with_checked(item.done)
                        .with_children(vec![
                            UiNode::new(ids::toggle(item.id), "checkbox", item.title.clone())
                                .with_checked(item.done),
                            UiNode::new(
                                ids::delete(item.id),
                                "button",
                                format!("Delete {}", item.title),
                            ),
                        ]),
                );
            }
            list_children
        };

        let window = UiNode::new(ids::WINDOW, "window", "Agent Todo")
            .with_child(
                UiNode::new(ids::INPUT, "textbox", "What needs doing?")
                    .with_value(self.draft.clone()),
            )
            .with_child(UiNode::new(ids::ADD, "button", "Add"))
            .with_child(UiNode::new(ids::LIST, "list", "Todos").with_children(list_children))
            .with_child(UiNode::new(ids::STATUS, "status", status_name));

        UiTree {
            app: "todo".into(),
            platform: self.platform,
            ready: true,
            nodes: vec![window],
        }
    }

    fn click(&mut self, target: &str) -> Result<DispatchResult, String> {
        if target == ids::ADD {
            return self.add_from_draft().map(todo_result);
        }
        if let Some(id) = gpui_agent::parse_numbered_id("todo-toggle-", target) {
            return self.toggle(id).map(todo_result);
        }
        if let Some(id) = gpui_agent::parse_numbered_id("todo-delete-", target) {
            return self.delete(id).map(todo_result);
        }
        if let Some(id) = gpui_agent::parse_numbered_id("todo-item-", target) {
            return self.toggle(id).map(todo_result);
        }
        Err(format!("cannot click `{target}`"))
    }

    fn type_into(&mut self, target: &str, text: &str) -> Result<DispatchResult, String> {
        if target != ids::INPUT {
            return Err(format!("`{target}` is not editable"));
        }
        self.draft.push_str(text);
        Ok(DispatchResult::json(
            serde_json::json!({ "value": self.draft }),
        ))
    }

    fn set_value(&mut self, target: &str, value: &str) -> Result<DispatchResult, String> {
        if target != ids::INPUT {
            return Err(format!("`{target}` is not editable"));
        }
        self.draft = value.to_string();
        Ok(DispatchResult::json(
            serde_json::json!({ "value": self.draft }),
        ))
    }

    fn key(&mut self, target: &str, key: &str) -> Result<DispatchResult, String> {
        match (target, key) {
            (ids::INPUT, "Enter") | (ids::ADD, "Enter") => self.add_from_draft().map(todo_result),
            (ids::INPUT, "Backspace") => {
                self.draft.pop();
                Ok(DispatchResult::empty())
            }
            _ => Err(format!("unhandled key `{key}` on `{target}`")),
        }
    }

    fn invoke(&mut self, name: &str, args: &serde_json::Value) -> Result<DispatchResult, String> {
        match name {
            "todo.add" => {
                let title = args
                    .get("title")
                    .and_then(|v| v.as_str())
                    .ok_or("todo.add requires args.title")?;
                self.add(title).map(todo_result)
            }
            "todo.toggle" => {
                let id = arg_id(args)?;
                self.toggle(id).map(todo_result)
            }
            "todo.delete" => {
                let id = arg_id(args)?;
                self.delete(id).map(todo_result)
            }
            "todo.list" => Ok(DispatchResult::json(
                serde_json::to_value(&self.items).unwrap(),
            )),
            other => Err(format!("unknown invoke `{other}`")),
        }
    }
}

fn arg_id(args: &serde_json::Value) -> Result<u64, String> {
    args.get("id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "expected args.id".into())
}

fn todo_result(todo: Todo) -> DispatchResult {
    DispatchResult::json(serde_json::to_value(todo).unwrap())
}

impl AgentHost for TodoStore {
    fn hello(&self) -> HelloInfo {
        HelloInfo {
            protocol: PROTOCOL_VERSION,
            app: "todo".into(),
            platform: self.platform,
            ready: true,
            deliveries: match self.platform {
                PlatformKind::Desktop => vec![DeliveryMode::Semantic, DeliveryMode::Virtual],
                _ => vec![DeliveryMode::Semantic],
            },
            auth: gpui_agent::HelloAuth::None,
        }
    }

    fn snapshot(&self) -> UiTree {
        self.tree()
    }

    fn screenshot(&self, path: Option<&str>) -> Result<DispatchResult, String> {
        let _ = path;
        let detail = match self.platform {
            PlatformKind::Headless => "headless host has no pixel surface",
            PlatformKind::Desktop => {
                "desktop screenshot must run on the GPUI UI thread with a Window \
                 (mailbox intercept). This store has no pixel surface."
            }
            _ => "this host has no pixel surface",
        };
        Err(gpui_agent::screenshot_unavailable(detail))
    }

    fn dispatch(&mut self, op: &Op) -> Result<DispatchResult, String> {
        if op.is_virtual_input() {
            return Err(virtual_unavailable(
                "this host has no GPUI pointer/key pipeline. \
                 Virtual delivery synthesizes in-window events on the desktop GPUI host \
                 after a painted frame. Headless and store-only hosts implement semantic \
                 dispatch only — use delivery=semantic, or run apps/todo with GPUI_AGENT=1.",
            ));
        }
        match op {
            Op::Click { target, .. } => self.click(target),
            Op::Type { target, text, .. } => self.type_into(target, text),
            Op::SetValue { target, value } => self.set_value(target, value),
            Op::Key { target, key, .. } => self.key(target, key),
            Op::Invoke { name, args } => self.invoke(name, args),
            Op::Screenshot { path } => AgentHost::screenshot(self, path.as_deref()),
            Op::Shutdown => {
                self.shutdown = true;
                Ok(DispatchResult::empty())
            }
            Op::Hello | Op::Snapshot | Op::Assert { .. } | Op::Wait { .. } => {
                Ok(DispatchResult::empty())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_agent::dispatch::handle_request;
    use gpui_agent::protocol::{AssertSpec, Request};

    fn add(store: &mut TodoStore, title: &str) -> u64 {
        store.add(title).unwrap().id
    }

    #[test]
    fn crud_through_semantic_ids() {
        let mut store = TodoStore::default();
        store
            .dispatch(&Op::SetValue {
                target: ids::INPUT.into(),
                value: "Buy milk".into(),
            })
            .unwrap();
        store.dispatch(&Op::click(ids::ADD)).unwrap();

        let id = store.items()[0].id;
        assert_eq!(store.items()[0].title, "Buy milk");
        assert!(!store.items()[0].done);

        store.dispatch(&Op::click(ids::toggle(id))).unwrap();
        assert!(store.items()[0].done);

        store.dispatch(&Op::click(ids::delete(id))).unwrap();
        assert!(store.items().is_empty());
    }

    #[test]
    fn virtual_delivery_is_unavailable_on_store_host() {
        let mut store = TodoStore::default();
        let err = store.dispatch(&Op::click_virtual(ids::ADD)).unwrap_err();
        assert!(err.starts_with(gpui_agent::VIRTUAL_UNAVAILABLE), "{err}");
        let hello = store.hello();
        assert_eq!(hello.deliveries, vec![DeliveryMode::Semantic]);
    }

    #[test]
    fn snapshot_carries_stable_ids() {
        let mut store = TodoStore::default();
        let id = add(&mut store, "Write docs");
        let tree = store.tree();
        assert!(tree.find(ids::INPUT).is_some());
        assert!(tree.find(ids::ADD).is_some());
        assert!(tree.find(&ids::item(id)).is_some());
        assert!(tree.find(&ids::toggle(id)).is_some());
        assert!(tree.find(&ids::delete(id)).is_some());
        assert_eq!(tree.find(&ids::item(id)).unwrap().name, "Write docs");
    }

    #[test]
    fn screenshot_is_honestly_unavailable() {
        let mut store = TodoStore::default();
        let dest =
            std::env::temp_dir().join(format!("gpui-agent-todo-no-shot-{}", std::process::id()));
        let _ = std::fs::remove_file(&dest);
        let req = Request::new(
            "s",
            Op::Screenshot {
                path: Some(dest.to_string_lossy().into_owned()),
            },
        );
        let resp = handle_request(&mut store, req, None);
        assert!(!resp.ok, "{resp:?}");
        let err = resp.error.unwrap();
        assert!(gpui_agent::is_screenshot_unavailable(&err), "{err}");
        assert!(err.contains("headless"), "{err}");
        assert!(!dest.exists(), "must not invent {}", dest.display());
    }

    #[test]
    fn desktop_store_screenshot_stays_unavailable_without_a_window() {
        let mut store = TodoStore::new(PlatformKind::Desktop);
        let dest = std::env::temp_dir().join(format!(
            "gpui-agent-desktop-store-no-shot-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&dest);
        let req = Request::new(
            "s",
            Op::Screenshot {
                path: Some(dest.to_string_lossy().into_owned()),
            },
        );
        let resp = handle_request(&mut store, req, None);
        assert!(!resp.ok, "{resp:?}");
        let err = resp.error.unwrap();
        assert!(gpui_agent::is_screenshot_unavailable(&err), "{err}");
        assert!(err.contains("UI thread") || err.contains("Window"), "{err}");
        assert!(!dest.exists(), "must not invent {}", dest.display());
    }

    #[test]
    fn protocol_assert_roundtrip() {
        let mut store = TodoStore::default();
        let id = add(&mut store, "Ship it");
        let req = Request::new(
            "a1",
            Op::Assert {
                spec: AssertSpec {
                    target: ids::item(id),
                    name: Some("Ship it".into()),
                    checked: Some(false),
                    exists: Some(true),
                    ..Default::default()
                },
            },
        );
        let resp = handle_request(&mut store, req, None);
        assert!(resp.ok, "{resp:?}");
    }
}
