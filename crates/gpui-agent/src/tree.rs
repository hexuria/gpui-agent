use serde::{Deserialize, Serialize};

/// Well-known role strings for semantic trees.
///
/// Wire format stays a string so apps can add their own. These constants
/// (and the `UiNode` constructors below) cut boilerplate for the common set.
pub mod role {
    pub const WINDOW: &str = "window";
    pub const PAGE: &str = "page";
    pub const NAVIGATION: &str = "navigation";
    pub const BUTTON: &str = "button";
    pub const TEXTBOX: &str = "textbox";
    pub const LIST: &str = "list";
    pub const LIST_ITEM: &str = "listitem";
    pub const SCROLL: &str = "scroll";
    pub const CHECKBOX: &str = "checkbox";
    pub const NOTE: &str = "note";
    pub const STATUS: &str = "status";
    pub const DIALOG: &str = "dialog";
}

/// Axis-aligned bounds in logical pixels. Hosts that cannot measure
/// layout (headless, or before the first frame) send zeros.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Stable error prefix for geometry-based `in_viewport` asserts.
///
/// Headless hosts and zero bounds must use this instead of inventing a clip.
pub const IN_VIEWPORT_UNAVAILABLE: &str = "in_viewport_unavailable";

pub fn in_viewport_unavailable(detail: impl Into<String>) -> String {
    format!("{IN_VIEWPORT_UNAVAILABLE}: {}", detail.into())
}

pub fn is_in_viewport_unavailable(error: &str) -> bool {
    error == IN_VIEWPORT_UNAVAILABLE
        || error.starts_with(IN_VIEWPORT_UNAVAILABLE)
            && error.as_bytes().get(IN_VIEWPORT_UNAVAILABLE.len()) == Some(&b':')
}

impl Bounds {
    pub fn has_area(self) -> bool {
        self.w > 0.0 && self.h > 0.0
    }

    pub fn center(self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// Non-empty overlap of two axis-aligned rects. Zero-area rects never
    /// intersect (callers should treat that as unavailable, not `false`).
    pub fn intersects(self, other: Self) -> bool {
        if !self.has_area() || !other.has_area() {
            return false;
        }
        self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiNode {
    pub id: String,
    pub role: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub focused: bool,
    /// Host-declared visibility. Omitted on the wire deserializes as
    /// `true` so old snapshots stay valid (`serde` default). Serializers
    /// omit `true` to keep trees compact.
    #[serde(default = "default_visible", skip_serializing_if = "is_visible")]
    pub visible: bool,
    #[serde(default)]
    pub bounds: Bounds,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<UiNode>,
}

fn default_visible() -> bool {
    true
}

fn is_visible(value: &bool) -> bool {
    *value
}

impl UiNode {
    pub fn new(id: impl Into<String>, role: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            role: role.into(),
            name: name.into(),
            value: None,
            checked: None,
            enabled: true,
            focused: false,
            visible: true,
            bounds: Bounds::default(),
            states: Vec::new(),
            children: Vec::new(),
        }
    }

    pub fn window(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(id, role::WINDOW, name)
    }

    pub fn page(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(id, role::PAGE, name)
    }

    pub fn navigation(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(id, role::NAVIGATION, name)
    }

    pub fn button(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(id, role::BUTTON, name)
    }

    pub fn textbox(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(id, role::TEXTBOX, name)
    }

    pub fn list(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(id, role::LIST, name)
    }

    pub fn listitem(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(id, role::LIST_ITEM, name)
    }

    pub fn scroll(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(id, role::SCROLL, name)
    }

    pub fn checkbox(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(id, role::CHECKBOX, name)
    }

    pub fn note(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(id, role::NOTE, name)
    }

    pub fn status(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(id, role::STATUS, name)
    }

    pub fn dialog(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(id, role::DIALOG, name)
    }

    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
        self
    }

    pub fn with_focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn with_visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    /// Set `visible` on this node and, when hiding, on every descendant.
    ///
    /// Prefer keeping a closed sidebar/modal in the tree with
    /// `visible=false` so agents can assert “known but not showing”.
    pub fn with_visible_deep(mut self, visible: bool) -> Self {
        self.visible = visible;
        if !visible {
            self.children = self
                .children
                .into_iter()
                .map(|child| child.with_visible_deep(false))
                .collect();
        }
        self
    }

    pub fn with_bounds(mut self, bounds: Bounds) -> Self {
        self.bounds = bounds;
        self
    }

    pub fn with_checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self.states.push(if checked {
            "checked".into()
        } else {
            "unchecked".into()
        });
        self
    }

    pub fn with_child(mut self, child: UiNode) -> Self {
        self.children.push(child);
        self
    }

    pub fn with_children(mut self, children: Vec<UiNode>) -> Self {
        self.children = children;
        self
    }

    pub fn find(&self, id: &str) -> Option<&UiNode> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter().find_map(|child| child.find(id))
    }

    pub fn find_all<'a>(&'a self, id: &str) -> Vec<&'a UiNode> {
        let mut out = Vec::new();
        self.collect_id(id, &mut out);
        out
    }

    fn collect_id<'a>(&'a self, id: &str, out: &mut Vec<&'a UiNode>) {
        if self.id == id {
            out.push(self);
        }
        for child in &self.children {
            child.collect_id(id, out);
        }
    }

    /// Visit this node and descendants without allocating intermediate vecs.
    pub fn visit<'a, F: FnMut(&'a UiNode)>(&'a self, f: &mut F) {
        f(self);
        for child in &self.children {
            child.visit(f);
        }
    }

    pub fn node_count(&self) -> usize {
        1 + self.children.iter().map(UiNode::node_count).sum::<usize>()
    }

    pub fn flatten(&self) -> Vec<&UiNode> {
        let mut out = Vec::new();
        self.flatten_into(&mut out);
        out
    }

    /// Clear `out` and fill it with this node and descendants, reusing capacity.
    ///
    /// Does **not** pre-walk `node_count`: that extra pass was slower than
    /// letting `Vec` grow (see `tree_flatten_no_precount` in benches).
    pub fn flatten_into<'a>(&'a self, out: &mut Vec<&'a UiNode>) {
        out.clear();
        self.visit(&mut |node| out.push(node));
    }

    pub fn apply_bounds_map(&mut self, map: &std::collections::HashMap<String, Bounds>) {
        if let Some(bounds) = map.get(&self.id) {
            self.bounds = *bounds;
        }
        for child in &mut self.children {
            child.apply_bounds_map(map);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiTree {
    pub app: String,
    pub platform: crate::protocol::PlatformKind,
    pub ready: bool,
    pub nodes: Vec<UiNode>,
}

impl UiTree {
    pub fn find(&self, id: &str) -> Option<&UiNode> {
        self.nodes.iter().find_map(|node| node.find(id))
    }

    pub fn find_all<'a>(&'a self, id: &str) -> Vec<&'a UiNode> {
        let mut out = Vec::new();
        for node in &self.nodes {
            node.collect_id(id, &mut out);
        }
        out
    }

    pub fn duplicate_ids(&self) -> Vec<String> {
        let mut counts = std::collections::BTreeMap::<String, usize>::new();
        self.visit(&mut |node| {
            *counts.entry(node.id.clone()).or_insert(0) += 1;
        });
        counts
            .into_iter()
            .filter(|(_, n)| *n > 1)
            .map(|(id, _)| id)
            .collect()
    }

    pub fn ids_are_unique(&self) -> bool {
        self.duplicate_ids().is_empty()
    }

    pub fn require_id(&self, id: &str) -> Result<&UiNode, String> {
        let found = self.find_all(id);
        match found.len() {
            0 => Err(format!("node `{id}` not found")),
            1 => Ok(found[0]),
            n => Err(format!("duplicate id `{id}` ({n} nodes)")),
        }
    }

    pub fn visit<'a, F: FnMut(&'a UiNode)>(&'a self, f: &mut F) {
        for node in &self.nodes {
            node.visit(f);
        }
    }

    pub fn node_count(&self) -> usize {
        self.nodes.iter().map(UiNode::node_count).sum()
    }

    pub fn flatten(&self) -> Vec<&UiNode> {
        let mut out = Vec::new();
        self.flatten_into(&mut out);
        out
    }

    /// Flatten into `out`, clearing it first so callers can reuse the buffer.
    ///
    /// Skips a `node_count` pre-pass; reuse still wins because `clear` keeps
    /// capacity after the first call.
    pub fn flatten_into<'a>(&'a self, out: &mut Vec<&'a UiNode>) {
        out.clear();
        self.visit(&mut |node| out.push(node));
    }

    pub fn apply_bounds_map(&mut self, map: &std::collections::HashMap<String, Bounds>) {
        for node in &mut self.nodes {
            node.apply_bounds_map(map);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatten_matches_visit_and_reuses_capacity() {
        let tree = UiTree {
            app: "t".into(),
            platform: crate::protocol::PlatformKind::Headless,
            ready: true,
            nodes: vec![
                UiNode::new("a", "window", "A")
                    .with_child(UiNode::new("b", "button", "B"))
                    .with_child(
                        UiNode::new("c", "button", "C").with_child(UiNode::new("d", "note", "D")),
                    ),
            ],
        };
        assert_eq!(tree.node_count(), 4);
        let flat: Vec<&str> = tree.flatten().iter().map(|n| n.id.as_str()).collect();
        assert_eq!(flat, ["a", "b", "c", "d"]);
        let mut visited = Vec::new();
        tree.visit(&mut |n| visited.push(n.id.as_str()));
        assert_eq!(visited, flat);

        let mut reuse = Vec::with_capacity(8);
        tree.flatten_into(&mut reuse);
        let reused: Vec<&str> = reuse.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(reused, flat);
        let cap = reuse.capacity();
        tree.flatten_into(&mut reuse);
        assert!(reuse.capacity() >= cap);
        assert_eq!(reuse.len(), 4);
    }

    #[test]
    fn typed_role_constructors_match_constants() {
        assert_eq!(UiNode::window("w", "W").role, role::WINDOW);
        assert_eq!(UiNode::page("p", "P").role, role::PAGE);
        assert_eq!(UiNode::navigation("n", "N").role, role::NAVIGATION);
        assert_eq!(UiNode::button("b", "B").role, role::BUTTON);
        assert_eq!(UiNode::textbox("t", "T").role, role::TEXTBOX);
        assert_eq!(UiNode::checkbox("c", "C").with_focused(true).focused, true);
        assert_eq!(UiNode::scroll("s", "S").role, role::SCROLL);
        assert!(!UiNode::note("n", "N").with_enabled(false).enabled);
        assert_eq!(UiNode::dialog("d", "About").role, role::DIALOG);
        assert!(UiNode::button("b", "B").visible);
        assert!(!UiNode::button("b", "B").with_visible(false).visible);
    }

    #[test]
    fn uinode_layout_stays_compact() {
        // AoS + String fields: 168 bytes on 64-bit with current field order.
        // `visible` sits in the existing bool padding before Bounds.
        // Reordering bools vs Bounds did not shrink this (see docs/PERF.md).
        assert_eq!(std::mem::size_of::<UiNode>(), 168);
        assert_eq!(std::mem::size_of::<Bounds>(), 16);
    }

    fn dup_tree() -> UiTree {
        UiTree {
            app: "t".into(),
            platform: crate::protocol::PlatformKind::Headless,
            ready: true,
            nodes: vec![
                UiNode::window("root", "Root")
                    .with_child(UiNode::button("dup", "First"))
                    .with_child(
                        UiNode::list("list", "List").with_child(UiNode::button("dup", "Second")),
                    ),
            ],
        }
    }

    #[test]
    fn find_all_returns_every_duplicate_in_dfs_order() {
        let tree = dup_tree();
        let names: Vec<&str> = tree
            .find_all("dup")
            .iter()
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(names, ["First", "Second"]);
        assert_eq!(tree.duplicate_ids(), vec!["dup".to_string()]);
        assert!(!tree.ids_are_unique());
    }

    #[test]
    fn require_id_zero_is_not_found() {
        let tree = dup_tree();
        let err = tree.require_id("missing").unwrap_err();
        assert!(err.contains("not found"), "{err}");
        assert!(err.contains("missing"), "{err}");
    }

    #[test]
    fn require_id_many_is_duplicate_error() {
        let tree = dup_tree();
        let err = tree.require_id("dup").unwrap_err();
        assert!(err.contains("duplicate id"), "{err}");
        assert!(err.contains('2') || err.contains("2 nodes"), "{err}");
        assert_eq!(tree.require_id("root").unwrap().name, "Root");
    }

    #[test]
    fn omitted_visible_deserializes_as_true() {
        let json = serde_json::json!({
            "id": "legacy",
            "role": "button",
            "name": "Old"
        });
        let node: UiNode = serde_json::from_value(json).unwrap();
        assert!(node.visible);
        let hidden = UiNode::button("h", "Hidden").with_visible(false);
        let value = serde_json::to_value(&hidden).unwrap();
        assert_eq!(value["visible"], false);
        let shown = UiNode::button("s", "Shown");
        let shown_json = serde_json::to_value(&shown).unwrap();
        assert!(
            shown_json.get("visible").is_none(),
            "visible=true must omit on the wire: {shown_json}"
        );
    }

    #[test]
    fn with_visible_deep_hides_descendants() {
        let node = UiNode::navigation("nav", "Nav")
            .with_child(UiNode::button("nav-a", "A"))
            .with_visible_deep(false);
        assert!(!node.visible);
        assert!(!node.children[0].visible);
    }

    #[test]
    fn bounds_intersect_requires_area() {
        let a = Bounds {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        };
        let b = Bounds {
            x: 5.0,
            y: 5.0,
            w: 10.0,
            h: 10.0,
        };
        let c = Bounds {
            x: 20.0,
            y: 20.0,
            w: 10.0,
            h: 10.0,
        };
        assert!(a.intersects(b));
        assert!(!a.intersects(c));
        assert!(!Bounds::default().intersects(a));
        assert!(!a.intersects(Bounds::default()));
    }
}
