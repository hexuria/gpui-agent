//! Domain model + `AgentHost` for the demo todo app.
//!
//! Both the GPUI Kit window and the headless host call the same methods, so
//! an agent action (`click todo-add`) and a human click run identical code.

use gpui_agent::dispatch::DispatchResult;
use gpui_agent::host::AgentHost;
use gpui_agent::keybinding::{KeybindingInfo, keybinding_unavailable};
use gpui_agent::protocol::{
    DeliveryMode, HelloInfo, KeybindingScope, Op, PROTOCOL_VERSION, PlatformKind,
};
use gpui_agent::tree::{UiNode, UiTree};
use gpui_agent::virtual_unavailable;
use serde::{Deserialize, Serialize};

/// Stable ids for the sample todo app. Other GPUI Kit apps define their own.
pub mod ids {
    pub const INPUT: &str = "todo-input";
    pub const ADD: &str = "todo-add";
    pub const LIST: &str = "todo-list";
    /// Named scroll view for `screenshot mode=scrolled` (embedded-host demo).
    pub const LIST_SCROLL: &str = "todo-list-scroll";
    pub const EMPTY: &str = "todo-empty";
    pub const STATUS: &str = "todo-status";
    pub const WINDOW: &str = "todo-window";
    pub const NAV: &str = "todo-nav";
    pub const NAV_TODOS: &str = "nav-todos";
    pub const NAV_SETTINGS: &str = "nav-settings";
    /// Collapse/expand the nav sidebar (`visible` on `todo-nav`).
    pub const NAV_TOGGLE: &str = "nav-toggle-sidebar";
    pub const PAGE_TODOS: &str = "page-todos";
    pub const PAGE_SETTINGS: &str = "page-settings";
    pub const SETTINGS_CONFIRM_DELETE: &str = "settings-confirm-delete";

    /// Focused keymap Action: go to Todos and focus the draft field.
    pub const KEY_FOCUS_INPUT: &str = "todo.focus_input";
    /// Global keymap Action: open Settings (no window focus required).
    pub const KEY_GO_SETTINGS: &str = "todo.go_settings";
    /// Destructive global Action: quit. Requires `confirm=true`.
    pub const KEY_QUIT: &str = "app.quit";

    pub const KEY_FOCUS_INPUT_CHORD: &str = "cmd-n";
    pub const KEY_GO_SETTINGS_CHORD: &str = "cmd-shift-s";
    pub const KEY_QUIT_CHORD: &str = "cmd-q";

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    Todos,
    Settings,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Todo {
    pub id: u64,
    pub title: String,
    pub done: bool,
}

/// GUI / client projection of a snapshot. The daemon `TodoStore` remains SoT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoView {
    pub items: Vec<Todo>,
    pub draft: String,
    pub page: Page,
    pub confirm_delete: bool,
    pub sidebar_open: bool,
}

impl Default for TodoView {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            draft: String::new(),
            page: Page::default(),
            confirm_delete: false,
            sidebar_open: true,
        }
    }
}

impl TodoView {
    pub fn from_tree(tree: &UiTree) -> Self {
        let page = if tree.find(ids::PAGE_SETTINGS).is_some() {
            Page::Settings
        } else {
            Page::Todos
        };
        let draft = tree
            .find(ids::INPUT)
            .and_then(|node| node.value.clone())
            .unwrap_or_default();
        let confirm_delete = tree
            .find(ids::SETTINGS_CONFIRM_DELETE)
            .and_then(|node| node.checked)
            .unwrap_or(false);
        let sidebar_open = tree.find(ids::NAV).map(|node| node.visible).unwrap_or(true);
        let mut items = Vec::new();
        tree.visit(&mut |node| {
            if let Some(id) = gpui_agent::parse_numbered_id("todo-item-", &node.id) {
                items.push(Todo {
                    id,
                    title: node.name.clone(),
                    done: node.checked.unwrap_or(false),
                });
            }
        });
        items.sort_by_key(|todo| todo.id);
        Self {
            items,
            draft,
            page,
            confirm_delete,
            sidebar_open,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TodoStore {
    next_id: u64,
    items: Vec<Todo>,
    draft: String,
    page: Page,
    confirm_delete: bool,
    sidebar_open: bool,
    platform: PlatformKind,
    shutdown: bool,
    /// Headless stand-in for OS window focus. Desktop uses `Window::is_window_active`.
    app_focused: bool,
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
            page: Page::Todos,
            confirm_delete: false,
            sidebar_open: true,
            platform,
            shutdown: false,
            app_focused: false,
        }
    }

    pub fn page(&self) -> Page {
        self.page
    }

    pub fn confirm_delete(&self) -> bool {
        self.confirm_delete
    }

    pub fn go(&mut self, page: Page) {
        self.page = page;
    }

    pub fn toggle_confirm_delete(&mut self) {
        self.confirm_delete = !self.confirm_delete;
    }

    pub fn sidebar_open(&self) -> bool {
        self.sidebar_open
    }

    pub fn toggle_sidebar(&mut self) {
        self.sidebar_open = !self.sidebar_open;
    }

    pub fn seed_overflow_demo(&mut self, count: usize) {
        for i in 1..=count {
            let _ = self.add(format!("Scroll demo {i}"));
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

    pub fn set_app_focused(&mut self, focused: bool) {
        self.app_focused = focused;
    }

    pub fn is_app_focused(&self) -> bool {
        self.app_focused
    }

    /// Sample Action-id catalog: one focused, one global, one destructive.
    pub fn keybinding_catalog() -> Vec<KeybindingInfo> {
        vec![
            KeybindingInfo::new(
                ids::KEY_FOCUS_INPUT,
                ids::KEY_FOCUS_INPUT_CHORD,
                KeybindingScope::Focused,
                false,
            ),
            KeybindingInfo::new(
                ids::KEY_GO_SETTINGS,
                ids::KEY_GO_SETTINGS_CHORD,
                KeybindingScope::Global,
                false,
            ),
            KeybindingInfo::new(
                ids::KEY_QUIT,
                ids::KEY_QUIT_CHORD,
                KeybindingScope::Global,
                true,
            ),
        ]
    }

    /// Same bodies the GPUI keymap Action listeners call.
    ///
    /// Headless hosts *are* the handler (no GPUI). Desktop `embedded-host`
    /// must not call this from the mailbox intercept as a fallback after
    /// `dispatch_action` — listeners own mutation, or the fire fails closed.
    pub fn perform_keybinding(&mut self, binding: &str) -> Result<DispatchResult, String> {
        match binding {
            ids::KEY_FOCUS_INPUT => {
                self.page = Page::Todos;
                Ok(DispatchResult::json(serde_json::json!({
                    "id": ids::KEY_FOCUS_INPUT,
                    "scope": "focused",
                    "path": "gpui.action"
                })))
            }
            ids::KEY_GO_SETTINGS => {
                self.page = Page::Settings;
                Ok(DispatchResult::json(serde_json::json!({
                    "id": ids::KEY_GO_SETTINGS,
                    "scope": "global",
                    "path": "gpui.action"
                })))
            }
            ids::KEY_QUIT => {
                self.shutdown = true;
                Ok(DispatchResult::json(serde_json::json!({
                    "id": ids::KEY_QUIT,
                    "scope": "global",
                    "path": "gpui.action",
                    "dangerous": true
                })))
            }
            other => Err(format!("unknown binding `{other}`")),
        }
    }

    fn fire_keybinding(&mut self, op: &Op) -> Result<DispatchResult, String> {
        if self.platform == PlatformKind::Desktop {
            return Err(keybinding_unavailable(
                "desktop keybinding fire must run on the GPUI UI thread \
                 (Action / keymap dispatch). Mailbox intercept missing?",
            ));
        }
        let binding = match op {
            Op::Keybinding {
                binding, activate, ..
            } => {
                if *activate {
                    self.app_focused = true;
                }
                binding.as_str()
            }
            _ => return Err("not a keybinding fire op".into()),
        };
        // Headless: this *is* the Action body (no Window / keymap).
        self.perform_keybinding(binding)
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
        let nav = UiNode::navigation(ids::NAV, "Todo")
            .with_child(UiNode::button(ids::NAV_TODOS, "Todos"))
            .with_child(UiNode::button(ids::NAV_SETTINGS, "Settings"));
        let nav = if self.sidebar_open {
            nav
        } else {
            nav.with_visible_deep(false)
        };

        let page = match self.page {
            Page::Todos => self.todos_page(),
            Page::Settings => self.settings_page(),
        };

        let toggle_name = if self.sidebar_open {
            "Hide sidebar"
        } else {
            "Show sidebar"
        };

        let window = UiNode::window(ids::WINDOW, "Agent Todo")
            .with_child(UiNode::button(ids::NAV_TOGGLE, toggle_name))
            .with_child(nav)
            .with_child(page);

        UiTree {
            app: "todo".into(),
            platform: self.platform,
            ready: true,
            nodes: vec![window],
        }
    }

    fn todos_page(&self) -> UiNode {
        let status_name = match self.items.len() {
            0 => "No todos".to_string(),
            n => {
                let done = self.items.iter().filter(|item| item.done).count();
                format!("{n} todos, {done} done")
            }
        };

        let list_children = if self.items.is_empty() {
            vec![UiNode::note(ids::EMPTY, "No todos yet. Add one above.")]
        } else {
            self.items
                .iter()
                .map(|item| {
                    UiNode::listitem(ids::item(item.id), item.title.clone())
                        .with_checked(item.done)
                        .with_child(
                            UiNode::checkbox(ids::toggle(item.id), item.title.clone())
                                .with_checked(item.done),
                        )
                        .with_child(UiNode::button(
                            ids::delete(item.id),
                            format!("Delete {}", item.title),
                        ))
                })
                .collect()
        };

        UiNode::page(ids::PAGE_TODOS, "Todos")
            .with_child(
                UiNode::textbox(ids::INPUT, "What needs doing?").with_value(self.draft.clone()),
            )
            .with_child(UiNode::button(ids::ADD, "Add"))
            .with_child(
                UiNode::scroll(ids::LIST_SCROLL, "Todo list")
                    .with_child(UiNode::list(ids::LIST, "Todos").with_children(list_children)),
            )
            .with_child(UiNode::status(ids::STATUS, status_name))
    }

    fn settings_page(&self) -> UiNode {
        UiNode::page(ids::PAGE_SETTINGS, "Settings").with_child(
            UiNode::checkbox(ids::SETTINGS_CONFIRM_DELETE, "Confirm before delete")
                .with_checked(self.confirm_delete),
        )
    }

    fn click(&mut self, target: &str) -> Result<DispatchResult, String> {
        if target == ids::NAV_TOGGLE {
            self.toggle_sidebar();
            return Ok(DispatchResult::json(serde_json::json!({
                "sidebar_open": self.sidebar_open
            })));
        }
        if target == ids::NAV_TODOS {
            self.page = Page::Todos;
            return Ok(DispatchResult::empty());
        }
        if target == ids::NAV_SETTINGS {
            self.page = Page::Settings;
            return Ok(DispatchResult::empty());
        }
        if target == ids::SETTINGS_CONFIRM_DELETE {
            self.confirm_delete = !self.confirm_delete;
            return Ok(DispatchResult::json(serde_json::json!({
                "confirm_delete": self.confirm_delete
            })));
        }
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
            "nav.go" => {
                let page = args
                    .get("page")
                    .and_then(|v| v.as_str())
                    .ok_or("nav.go requires args.page")?;
                match page {
                    "todos" => self.page = Page::Todos,
                    "settings" => self.page = Page::Settings,
                    other => return Err(format!("unknown page `{other}`")),
                }
                Ok(DispatchResult::empty())
            }
            "todo.toggle_sidebar" => {
                self.toggle_sidebar();
                Ok(DispatchResult::json(serde_json::json!({
                    "sidebar_open": self.sidebar_open
                })))
            }
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

    fn keybindings(&self) -> Vec<KeybindingInfo> {
        Self::keybinding_catalog()
    }

    fn is_app_focused(&self) -> bool {
        self.app_focused
    }

    fn screenshot(&self, spec: gpui_agent::ScreenshotSpec<'_>) -> Result<DispatchResult, String> {
        let path = gpui_agent::require_screenshot_path(spec.path)?;
        let _dest = gpui_agent::confine_screenshot_path(path)?;
        let detail = match self.platform {
            PlatformKind::Headless => "headless host has no pixel surface (viewport and scrolled)",
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
            Op::Keybinding { .. } => self.fire_keybinding(op),
            Op::Keybindings => Ok(DispatchResult::json(gpui_agent::keybinding_list_json(
                &Self::keybinding_catalog(),
            ))),
            Op::Invoke { name, args } => self.invoke(name, args),
            Op::Screenshot {
                path,
                mode,
                target,
                max_height_px,
            } => AgentHost::screenshot(
                self,
                gpui_agent::ScreenshotSpec::from_op(path, *mode, target, *max_height_px),
            ),
            Op::Shutdown => {
                self.shutdown = true;
                Ok(DispatchResult::empty())
            }
            Op::Hello
            | Op::Snapshot
            | Op::Assert { .. }
            | Op::Wait { .. }
            | Op::WaitUntil { .. } => Ok(DispatchResult::empty()),
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
        assert!(tree.find(ids::NAV_TODOS).is_some());
        assert!(tree.find(ids::NAV_SETTINGS).is_some());
        assert!(tree.find(ids::NAV_TOGGLE).is_some());
        assert!(tree.find(ids::NAV).unwrap().visible);
        assert!(tree.find(ids::PAGE_TODOS).is_some());
        assert!(tree.find(&ids::item(id)).is_some());
        assert!(tree.find(&ids::toggle(id)).is_some());
        assert!(tree.find(&ids::delete(id)).is_some());
        assert_eq!(tree.find(&ids::item(id)).unwrap().name, "Write docs");
        assert_eq!(
            tree.find(ids::INPUT).unwrap().role,
            gpui_agent::role::TEXTBOX
        );
        assert_eq!(
            tree.find(ids::PAGE_TODOS).unwrap().role,
            gpui_agent::role::PAGE
        );
    }

    #[test]
    fn settings_page_is_a_second_screen() {
        let mut store = TodoStore::default();
        add(&mut store, "Keep me");
        store.dispatch(&Op::click(ids::NAV_SETTINGS)).unwrap();
        assert_eq!(store.page(), Page::Settings);
        let tree = store.tree();
        assert!(tree.find(ids::PAGE_SETTINGS).is_some());
        assert_eq!(
            tree.find(ids::PAGE_SETTINGS).unwrap().role,
            gpui_agent::role::PAGE
        );
        assert!(tree.find(ids::SETTINGS_CONFIRM_DELETE).is_some());
        assert!(tree.find(ids::INPUT).is_none());
        assert!(tree.find("todo-item-1").is_none());

        store
            .dispatch(&Op::click(ids::SETTINGS_CONFIRM_DELETE))
            .unwrap();
        assert!(store.confirm_delete());
        store
            .dispatch(&Op::Invoke {
                name: "nav.go".into(),
                args: serde_json::json!({ "page": "todos" }),
            })
            .unwrap();
        assert_eq!(store.page(), Page::Todos);
        assert!(store.tree().find(ids::PAGE_TODOS).is_some());
        assert!(store.tree().find("todo-item-1").is_some());
    }

    #[test]
    fn view_from_tree_roundtrips_items_and_page() {
        let mut store = TodoStore::default();
        add(&mut store, "Milk");
        store.toggle(1).unwrap();
        let view = TodoView::from_tree(&store.tree());
        assert_eq!(view.page, Page::Todos);
        assert_eq!(view.items.len(), 1);
        assert_eq!(view.items[0].title, "Milk");
        assert!(view.items[0].done);
        assert!(view.sidebar_open);
        store.go(Page::Settings);
        let settings = TodoView::from_tree(&store.tree());
        assert_eq!(settings.page, Page::Settings);
        assert!(settings.items.is_empty());
    }

    #[test]
    fn screenshot_is_honestly_unavailable() {
        let mut store = TodoStore::default();
        let name = format!("todo-headless-no-shot-{}.png", std::process::id());
        let dest = gpui_agent::screenshot_base_dir().join(&name);
        let _ = std::fs::remove_file(&dest);
        let req = Request::new("s", Op::screenshot(name));
        let resp = handle_request(&mut store, req, None, None);
        assert!(!resp.ok, "{resp:?}");
        let err = resp.error.unwrap();
        assert!(gpui_agent::is_screenshot_unavailable(&err), "{err}");
        assert!(err.contains("headless"), "{err}");
        assert!(!dest.exists(), "must not invent {}", dest.display());
    }

    #[test]
    fn scrolled_screenshot_is_honestly_unavailable() {
        let mut store = TodoStore::default();
        let name = format!("todo-headless-no-scrolled-{}.png", std::process::id());
        let dest = gpui_agent::screenshot_base_dir().join(&name);
        let _ = std::fs::remove_file(&dest);
        let req = Request::new(
            "s",
            Op::screenshot_scrolled(name, ids::LIST_SCROLL, Some(4096)),
        );
        let resp = handle_request(&mut store, req, None, None);
        assert!(!resp.ok, "{resp:?}");
        let err = resp.error.unwrap();
        assert!(gpui_agent::is_screenshot_unavailable(&err), "{err}");
        assert!(err.contains("headless"), "{err}");
        assert!(!dest.exists(), "must not invent {}", dest.display());
    }

    #[test]
    fn scrolled_screenshot_unconfined_path_fails_before_unavailable() {
        let mut store = TodoStore::default();
        let dest = std::path::Path::new("/tmp/evil-scrolled.png");
        let _ = std::fs::remove_file(dest);
        let req = Request::new(
            "s",
            Op::screenshot_scrolled("/tmp/evil-scrolled.png", ids::LIST_SCROLL, Some(4096)),
        );
        let resp = handle_request(&mut store, req, None, None);
        assert!(!resp.ok, "{resp:?}");
        let err = resp.error.unwrap();
        assert!(
            !gpui_agent::is_screenshot_unavailable(&err),
            "unconfined scrolled path must fail at confine: {err}"
        );
        assert!(err.contains("relative"), "{err}");
        assert!(!dest.exists(), "must not invent {}", dest.display());
    }

    #[test]
    fn tree_exposes_scroll_target_id() {
        let store = TodoStore::default();
        let tree = store.tree();
        let node = tree.find(ids::LIST_SCROLL).expect("scroll id");
        assert_eq!(node.role, gpui_agent::role::SCROLL);
        assert!(tree.find(ids::LIST).is_some());
    }

    #[test]
    fn screenshot_unconfined_path_fails_before_unavailable() {
        let mut store = TodoStore::default();
        for bad in ["/etc/passwd.png", "-Sc.png", "../x.png"] {
            let req = Request::new("s", Op::screenshot(bad));
            let resp = handle_request(&mut store, req, None, None);
            assert!(!resp.ok, "{bad} {resp:?}");
            let err = resp.error.clone().unwrap();
            assert!(
                !gpui_agent::is_screenshot_unavailable(&err),
                "unconfined path must fail at confine, not unavailable: {bad} {err}"
            );
            assert!(
                err.contains("relative")
                    || err.contains("filename")
                    || err.contains("..")
                    || err.contains(".png"),
                "{bad} {err}"
            );
        }
        assert!(
            !std::path::Path::new("/etc/passwd.png").exists(),
            "must not invent /etc/passwd.png"
        );
    }

    #[test]
    fn desktop_store_screenshot_stays_unavailable_without_a_window() {
        let mut store = TodoStore::new(PlatformKind::Desktop);
        let dest = gpui_agent::screenshot_base_dir().join(format!(
            "todo-desktop-store-no-shot-{}.png",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&dest);
        let req = Request::new(
            "s",
            Op::screenshot(format!(
                "todo-desktop-store-no-shot-{}.png",
                std::process::id()
            )),
        );
        let resp = handle_request(&mut store, req, None, None);
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
        let resp = handle_request(&mut store, req, None, None);
        assert!(resp.ok, "{resp:?}");
    }

    #[test]
    fn sidebar_toggle_assert_visible_and_wait_until() {
        let mut store = TodoStore::default();
        assert!(store.sidebar_open());
        assert!(store.tree().find(ids::NAV).unwrap().visible);

        store.dispatch(&Op::click(ids::NAV_TOGGLE)).unwrap();
        assert!(!store.sidebar_open());
        let tree = store.tree();
        let nav = tree.find(ids::NAV).unwrap();
        assert!(!nav.visible);
        assert!(!nav.children[0].visible);

        let hidden = handle_request(
            &mut store,
            Request::new(
                "v1",
                Op::Assert {
                    spec: AssertSpec {
                        target: ids::NAV.into(),
                        exists: Some(true),
                        visible: Some(false),
                        ..Default::default()
                    },
                },
            ),
            None,
            None,
        );
        assert!(hidden.ok, "{hidden:?}");

        let wait = handle_request(
            &mut store,
            Request::new(
                "w1",
                Op::wait_until(
                    AssertSpec {
                        target: ids::NAV.into(),
                        visible: Some(false),
                        ..Default::default()
                    },
                    200,
                ),
            ),
            None,
            None,
        );
        assert!(wait.ok, "{wait:?}");

        store
            .dispatch(&Op::Invoke {
                name: "todo.toggle_sidebar".into(),
                args: serde_json::json!({}),
            })
            .unwrap();
        assert!(store.sidebar_open());
        assert!(store.tree().find(ids::NAV).unwrap().visible);

        let timeout = handle_request(
            &mut store,
            Request::new(
                "w2",
                Op::wait_until(
                    AssertSpec {
                        target: ids::NAV.into(),
                        visible: Some(false),
                        ..Default::default()
                    },
                    40,
                ),
            ),
            None,
            None,
        );
        assert!(!timeout.ok, "{timeout:?}");
        let err = timeout.error.unwrap();
        assert!(err.contains("timed out"), "{err}");
        assert!(err.contains("visible"), "{err}");

        let geo = handle_request(
            &mut store,
            Request::new(
                "vp",
                Op::Assert {
                    spec: AssertSpec {
                        target: ids::NAV_TOGGLE.into(),
                        in_viewport: Some(true),
                        ..Default::default()
                    },
                },
            ),
            None,
            None,
        );
        assert!(!geo.ok, "{geo:?}");
        let err = geo.error.unwrap();
        assert!(gpui_agent::is_in_viewport_unavailable(&err), "{err}");
    }

    #[test]
    fn docs_protocol_mentions_remote_triple() {
        let path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/PROTOCOL.md");
        let text = std::fs::read_to_string(&path).expect("PROTOCOL.md");
        assert!(
            text.contains("GPUI_AGENT_REMOTE"),
            "PROTOCOL.md must document remote bind with GPUI_AGENT_REMOTE"
        );
        assert!(
            text.contains("wait_until") && text.contains("in_viewport"),
            "PROTOCOL.md must document wait_until and in_viewport"
        );
    }

    #[test]
    fn page_settings_readme_role_is_page() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../README.md");
        let text = std::fs::read_to_string(&path).expect("README.md");
        assert!(
            !text.contains("--role window"),
            "page-settings example must not use --role window"
        );
        assert!(
            text.contains("assert --id page-settings --role page"),
            "page-settings example should assert role page"
        );
    }

    #[test]
    fn integrating_md_lists_mailbox_and_screenshot() {
        let path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/INTEGRATING.md");
        let text = std::fs::read_to_string(&path).expect("INTEGRATING.md");
        assert!(
            text.contains("spawn_mailbox"),
            "INTEGRATING.md must mention spawn_mailbox"
        );
        assert!(
            text.contains("GPUI_AGENT_SCREENSHOT_DIR"),
            "INTEGRATING.md must mention GPUI_AGENT_SCREENSHOT_DIR"
        );
        assert!(
            text.contains("HMAC-SHA256"),
            "adapter checklist must name HMAC-SHA256"
        );
        assert!(
            text.contains("GPUI_AGENT_INSECURE_NO_TOKEN"),
            "adapter checklist must name the default-deny opt-in"
        );
        assert!(
            text.contains("Confined screenshots") && text.contains("relative `.png`"),
            "adapter checklist must require confined relative .png paths"
        );
    }

    #[test]
    fn docs_document_keybinding_op_and_confirm() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let protocol = std::fs::read_to_string(root.join("docs/PROTOCOL.md")).unwrap();
        assert!(
            protocol.contains("`keybinding`"),
            "PROTOCOL.md must document the keybinding op"
        );
        assert!(
            protocol.contains("`keybindings`"),
            "PROTOCOL.md must document the keybindings list"
        );
        let security = std::fs::read_to_string(root.join("docs/SECURITY.md")).unwrap();
        assert!(
            security.contains("confirm=true") || security.contains("`confirm=true`"),
            "SECURITY.md must require confirm=true for destructive keybindings"
        );
        assert!(
            security.contains("keybinding"),
            "SECURITY.md must mention keybinding"
        );
        let integrating = std::fs::read_to_string(root.join("docs/INTEGRATING.md")).unwrap();
        assert!(
            integrating.contains("keybinding") && integrating.contains("dispatch"),
            "INTEGRATING.md must tell apps to dispatch the Action the keymap would"
        );
        assert!(
            integrating.contains("must not") && integrating.contains("fallback"),
            "INTEGRATING.md must forbid intercept store fallback after dispatch_action"
        );
        assert!(
            integrating.contains("visible") && integrating.contains("sidebar"),
            "INTEGRATING.md must tell apps how to mark sidebar/modal visible"
        );
    }

    #[test]
    fn keybinding_catalog_has_focused_global_and_dangerous() {
        let cat = TodoStore::keybinding_catalog();
        assert_eq!(cat.len(), 3);
        assert_eq!(cat[0].id, ids::KEY_FOCUS_INPUT);
        assert_eq!(cat[0].scope, KeybindingScope::Focused);
        assert!(!cat[0].dangerous);
        assert_eq!(cat[1].id, ids::KEY_GO_SETTINGS);
        assert_eq!(cat[1].scope, KeybindingScope::Global);
        assert_eq!(cat[2].id, ids::KEY_QUIT);
        assert!(cat[2].dangerous);
    }

    #[test]
    fn keybindings_list_over_handle_request() {
        let mut store = TodoStore::default();
        let resp = handle_request(&mut store, Request::new("1", Op::Keybindings), None, None);
        assert!(resp.ok, "{resp:?}");
        let rows = resp.result.unwrap()["keybindings"]
            .as_array()
            .cloned()
            .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["id"], ids::KEY_FOCUS_INPUT);
        assert_eq!(rows[2]["dangerous"], true);
    }

    #[test]
    fn focused_keybinding_errors_when_app_not_focused() {
        let mut store = TodoStore::default();
        let resp = handle_request(
            &mut store,
            Request::new(
                "1",
                Op::keybinding(ids::KEY_FOCUS_INPUT, KeybindingScope::Focused),
            ),
            None,
            None,
        );
        assert!(!resp.ok, "{resp:?}");
        let err = resp.error.unwrap();
        assert!(gpui_agent::is_keybinding_unavailable(&err), "{err}");
        assert!(err.contains("app not focused"), "{err}");
        assert_eq!(store.page(), Page::Todos);
    }

    #[test]
    fn focused_keybinding_activate_then_fires() {
        let mut store = TodoStore::default();
        store.go(Page::Settings);
        let resp = handle_request(
            &mut store,
            Request::new(
                "1",
                Op::Keybinding {
                    binding: ids::KEY_FOCUS_INPUT.into(),
                    chord: None,
                    scope: KeybindingScope::Focused,
                    confirm: false,
                    activate: true,
                },
            ),
            None,
            None,
        );
        assert!(resp.ok, "{resp:?}");
        assert_eq!(store.page(), Page::Todos);
        assert!(store.is_app_focused());
        assert_eq!(resp.result.unwrap()["path"], "gpui.action");
    }

    #[test]
    fn global_keybinding_fires_without_focus() {
        let mut store = TodoStore::default();
        assert!(!store.is_app_focused());
        let resp = handle_request(
            &mut store,
            Request::new(
                "1",
                Op::keybinding(ids::KEY_GO_SETTINGS, KeybindingScope::Global),
            ),
            None,
            None,
        );
        assert!(resp.ok, "{resp:?}");
        assert_eq!(store.page(), Page::Settings);
        assert!(!store.is_app_focused(), "global must not activate");
    }

    #[test]
    fn window_only_binding_requested_as_global_fails() {
        let mut store = TodoStore::default();
        store.set_app_focused(true);
        let resp = handle_request(
            &mut store,
            Request::new(
                "1",
                Op::keybinding(ids::KEY_FOCUS_INPUT, KeybindingScope::Global),
            ),
            None,
            None,
        );
        assert!(!resp.ok, "{resp:?}");
        let err = resp.error.unwrap();
        assert!(err.contains("registered as focused"), "{err}");
        assert!(err.contains("not global"), "{err}");
    }

    #[test]
    fn quit_keybinding_requires_confirm_and_sets_shutdown() {
        let mut store = TodoStore::default();
        let denied = handle_request(
            &mut store,
            Request::new("1", Op::keybinding(ids::KEY_QUIT, KeybindingScope::Global)),
            None,
            None,
        );
        assert!(!denied.ok, "{denied:?}");
        assert!(
            denied
                .error
                .as_deref()
                .is_some_and(|e| e.contains("confirm=true")),
            "{denied:?}"
        );
        assert!(!store.wants_shutdown());

        let ok = handle_request(
            &mut store,
            Request::new(
                "2",
                Op::Keybinding {
                    binding: ids::KEY_QUIT.into(),
                    chord: Some(ids::KEY_QUIT_CHORD.into()),
                    scope: KeybindingScope::Global,
                    confirm: true,
                    activate: false,
                },
            ),
            None,
            None,
        );
        assert!(ok.ok, "{ok:?}");
        assert!(store.wants_shutdown());
    }

    #[test]
    fn free_form_key_still_rejects_cmd_q() {
        let err = gpui_agent::keystroke_token("cmd-q").unwrap_err();
        assert!(err.contains("modifiers"), "{err}");
        let mut store = TodoStore::default();
        store.set_app_focused(true);
        let resp = store.dispatch(&Op::key(ids::INPUT, "cmd-q"));
        assert!(resp.is_err());
        let virt = store.dispatch(&Op::key_virtual(ids::INPUT, "cmd-q"));
        assert!(
            virt.unwrap_err()
                .starts_with(gpui_agent::VIRTUAL_UNAVAILABLE)
        );
    }

    #[test]
    fn desktop_store_refuses_to_side_door_keybinding_fire() {
        let mut store = TodoStore::new(PlatformKind::Desktop);
        store.set_app_focused(true);
        let err = store
            .dispatch(&Op::keybinding(
                ids::KEY_GO_SETTINGS,
                KeybindingScope::Global,
            ))
            .unwrap_err();
        assert!(gpui_agent::is_keybinding_unavailable(&err), "{err}");
        assert!(err.contains("UI thread"), "{err}");
    }

    #[test]
    fn action_no_op_does_not_green_via_store_side_door() {
        let mut store = TodoStore::new(PlatformKind::Desktop);
        store.set_app_focused(true);

        let err = gpui_agent::intercept_keybinding_action(|slot| {
            // Window::dispatch_action was a no-op (listener never ran).
            // Dual-write would call perform_keybinding here and stuff `Some`
            // into `slot` so the mailbox still looked like `path: gpui.action`.
            let _ = slot;
        })
        .unwrap_err();
        assert!(gpui_agent::is_keybinding_unavailable(&err), "{err}");
        assert!(err.contains("Action handler did not run"), "{err}");
        assert_eq!(
            store.page(),
            Page::Todos,
            "no-op Action must not mutate the store from the intercept"
        );
        assert!(!store.wants_shutdown());
    }

    #[test]
    fn action_listener_owns_mutation_and_reply() {
        let mut store = TodoStore::new(PlatformKind::Desktop);
        store.set_app_focused(true);
        let result = gpui_agent::intercept_keybinding_action(|slot| {
            *slot = Some(store.perform_keybinding(ids::KEY_GO_SETTINGS));
        })
        .unwrap();
        assert_eq!(store.page(), Page::Settings);
        assert_eq!(result.value.unwrap()["path"], "gpui.action");
    }

    #[test]
    fn dual_write_after_no_op_is_what_the_guard_rejects() {
        let mut store = TodoStore::new(PlatformKind::Desktop);
        store.set_app_focused(true);

        // Production intercept: dispatch_action only. Keep DUAL_WRITE false.
        // Enabling it (the old apps/todo intercept) must fail this test.
        const DUAL_WRITE: bool = false;
        let action_ran = false;
        let mut listener_result = None;
        if action_ran {
            listener_result = Some(store.perform_keybinding(ids::KEY_GO_SETTINGS));
        }
        if DUAL_WRITE {
            listener_result = Some(store.perform_keybinding(ids::KEY_GO_SETTINGS));
        }
        let result = gpui_agent::complete_keybinding_action(listener_result);
        assert!(
            result.is_err(),
            "no-op Action must not look green: {result:?}"
        );
        assert_eq!(store.page(), Page::Todos);
    }

    #[test]
    fn embedded_host_dispatch_keybinding_does_not_dual_write() {
        let src = include_str!("../../../apps/todo/src/app.rs");
        let start = src
            .find("fn start_keybinding_fire")
            .expect("start_keybinding_fire in apps/todo");
        let rest = &src[start..];
        let end = rest[1..]
            .find("\n    fn ")
            .map(|i| i + 1)
            .unwrap_or(rest.len());
        let body = &rest[..end];
        assert!(
            !body.contains("perform_keybinding"),
            "start_keybinding_fire must not call perform_keybinding* \
             (that dual-write masks a no-op dispatch_action):\n{body}"
        );
        assert!(
            body.contains("dispatch_action"),
            "start_keybinding_fire must dispatch_action:\n{body}"
        );
        assert!(
            src.contains("complete_keybinding_action"),
            "embedded-host must complete the mailbox reply from listener state"
        );
        assert!(
            src.contains("defer_in"),
            "embedded-host must wait until after deferred Window::dispatch_action"
        );
    }

    #[test]
    fn no_brainer_host_from_env_is_not_optional() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/NO_BRAINER_PLAN.md");
        let text = std::fs::read_to_string(&path).expect("NO_BRAINER_PLAN.md");
        assert!(
            !text.contains("Still optional"),
            "P2 table must not say host from_env is still optional after D1 default-deny"
        );
        assert!(
            text.contains("GPUI_AGENT_INSECURE_NO_TOKEN"),
            "P2 table should name the insecure opt-in"
        );
    }
}
