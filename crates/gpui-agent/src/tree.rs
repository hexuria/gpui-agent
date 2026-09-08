use serde::{Deserialize, Serialize};

/// Axis-aligned bounds in logical pixels. Hosts that cannot measure
/// layout (headless, or before the first frame) send zeros.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Bounds {
    pub fn has_area(self) -> bool {
        self.w > 0.0 && self.h > 0.0
    }

    pub fn center(self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
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
    #[serde(default)]
    pub bounds: Bounds,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<UiNode>,
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
            bounds: Bounds::default(),
            states: Vec::new(),
            children: Vec::new(),
        }
    }

    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
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
    fn flatten_matches_visit_and_preallocates() {
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
    fn uinode_layout_stays_compact() {
        // AoS + String fields: 168 bytes on 64-bit with current field order.
        // Reordering bools vs Bounds did not shrink this (see docs/PERF.md).
        assert_eq!(std::mem::size_of::<UiNode>(), 168);
        assert_eq!(std::mem::size_of::<Bounds>(), 16);
    }
}
