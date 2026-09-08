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

    pub fn flatten(&self) -> Vec<&UiNode> {
        let mut out = vec![self];
        for child in &self.children {
            out.extend(child.flatten());
        }
        out
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

    pub fn flatten(&self) -> Vec<&UiNode> {
        self.nodes.iter().flat_map(UiNode::flatten).collect()
    }

    pub fn apply_bounds_map(&mut self, map: &std::collections::HashMap<String, Bounds>) {
        for node in &mut self.nodes {
            node.apply_bounds_map(map);
        }
    }
}
