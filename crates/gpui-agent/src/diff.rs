//! Revision diffs and the compact snapshot format (M3).
//!
//! `snapshot since` answers one of: `unchanged`, a [`TreeDiff`] against a
//! revision still in the server ring, or the full tree. `snapshot
//! format:"compact"` is one line per visible node — the token-lean
//! representation LLM drivers use instead of the JSON tree.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::tree::{UiNode, UiTree};

/// Node scalar fields compared between revisions. `children` is not a
/// field here — adds/removes/move are expressed via `added`/`removed`.
const NODE_FIELDS: &[&str] = &[
    "role", "name", "value", "checked", "enabled", "focused", "visible", "bounds", "states",
];

/// A node that exists in the new revision but not the old one.
///
/// `parent` is the id it attaches under (`None` = top level), so a client
/// holding the old tree can graft it without a fresh snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddedNode {
    #[serde(flatten)]
    pub node: UiNode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

/// Same id in both revisions with at least one differing field.
/// `fields` carries only the new values (e.g. `{"checked":true}`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangedNode {
    pub id: String,
    pub fields: serde_json::Map<String, serde_json::Value>,
}

/// Id-keyed diff between two snapshots of the same host.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TreeDiff {
    pub added: Vec<AddedNode>,
    pub removed: Vec<String>,
    pub changed: Vec<ChangedNode>,
}

/// (node, parent-id) for every node in DFS order.
fn index(tree: &UiTree) -> BTreeMap<String, (UiNode, Option<String>)> {
    let mut out = BTreeMap::new();
    let mut stack: Vec<(&UiNode, Option<String>)> =
        tree.nodes.iter().rev().map(|n| (n, None)).collect();
    while let Some((node, parent)) = stack.pop() {
        for child in node.children.iter().rev() {
            stack.push((child, Some(node.id.clone())));
        }
        out.insert(node.id.clone(), (node.clone(), parent));
    }
    out
}

fn scalar_fields(node: &UiNode) -> serde_json::Map<String, serde_json::Value> {
    let mut v = serde_json::to_value(node).unwrap_or_default();
    let obj = v.as_object_mut().map(std::mem::take).unwrap_or_default();
    obj.into_iter()
        .filter(|(k, _)| NODE_FIELDS.contains(&k.as_str()))
        .collect()
}

/// `diff_trees(old, new)` — what a client at `old` needs to reach `new`.
///
/// Ids are stable across revisions by contract; a node that changed
/// parents comes out as `removed` + `added` (a move is not a field edit).
pub fn diff_trees(old: &UiTree, new: &UiTree) -> TreeDiff {
    let before = index(old);
    let after = index(new);
    let mut diff = TreeDiff::default();

    for (id, (node, parent)) in &after {
        match before.get(id) {
            None => diff.added.push(AddedNode {
                node: node.clone(),
                parent: parent.clone(),
            }),
            Some((old_node, old_parent)) => {
                if old_parent != parent {
                    diff.removed.push(id.clone());
                    diff.added.push(AddedNode {
                        node: node.clone(),
                        parent: parent.clone(),
                    });
                    continue;
                }
                let old_fields = scalar_fields(old_node);
                let fields: serde_json::Map<String, serde_json::Value> = scalar_fields(node)
                    .into_iter()
                    .filter(|(k, v)| old_fields.get(k) != Some(v))
                    .collect();
                if !fields.is_empty() {
                    diff.changed.push(ChangedNode {
                        id: id.clone(),
                        fields,
                    });
                }
            }
        }
    }
    for id in before.keys() {
        if !after.contains_key(id) {
            diff.removed.push(id.clone());
        }
    }
    // Deterministic output regardless of map order.
    diff.added.sort_by(|a, b| a.node.id.cmp(&b.node.id));
    diff.removed.sort();
    diff.changed.sort_by(|a, b| a.id.cmp(&b.id));
    diff
}

/// Rebuild the new tree from the old tree and its diff. Inverts
/// [`diff_trees`]: `apply_diff(old, &diff_trees(old, new))` yields `new`
/// (up to top-level ordering — ids are the identity).
pub fn apply_diff(old: &UiTree, diff: &TreeDiff) -> UiTree {
    let mut out = old.clone();
    for id in &diff.removed {
        remove_id(&mut out.nodes, id);
    }
    for change in &diff.changed {
        if let Some(node) = find_mut(&mut out.nodes, &change.id) {
            apply_fields(node, &change.fields);
        }
    }
    // Parents are emitted before children (DFS), so a single pass grafts
    // nested additions correctly.
    let mut pending: Vec<&AddedNode> = diff.added.iter().collect();
    let mut progress = true;
    while !pending.is_empty() && progress {
        progress = false;
        pending.retain(|add| {
            let can_place = match &add.parent {
                None => true,
                Some(p) => find_mut(&mut out.nodes, p).is_some() || p.is_empty(),
            };
            if can_place {
                let node = add.node.clone();
                match &add.parent {
                    Some(p) if !p.is_empty() => {
                        if let Some(parent) = find_mut(&mut out.nodes, p) {
                            parent.children.push(node);
                        }
                    }
                    _ => out.nodes.push(node),
                }
                progress = true;
                false
            } else {
                true
            }
        });
    }
    out
}

fn remove_id(nodes: &mut Vec<UiNode>, id: &str) -> bool {
    if let Some(pos) = nodes.iter().position(|n| n.id == id) {
        nodes.remove(pos);
        return true;
    }
    nodes.iter_mut().any(|n| remove_id(&mut n.children, id))
}

fn find_mut<'a>(nodes: &'a mut [UiNode], id: &str) -> Option<&'a mut UiNode> {
    for node in nodes.iter_mut() {
        if node.id == id {
            return Some(node);
        }
        if let Some(found) = find_mut(&mut node.children, id) {
            return Some(found);
        }
    }
    None
}

fn apply_fields(node: &mut UiNode, fields: &serde_json::Map<String, serde_json::Value>) {
    for (k, v) in fields {
        match k.as_str() {
            "role" => node.role = v.as_str().unwrap_or_default().to_string(),
            "name" => node.name = v.as_str().unwrap_or_default().to_string(),
            "value" => node.value = v.as_str().map(str::to_string),
            "checked" => node.checked = v.as_bool(),
            "enabled" => node.enabled = v.as_bool().unwrap_or(true),
            "focused" => node.focused = v.as_bool().unwrap_or(false),
            "visible" => node.visible = v.as_bool().unwrap_or(true),
            "bounds" => {
                if let Ok(b) = serde_json::from_value(v.clone()) {
                    node.bounds = b;
                }
            }
            "states" => {
                if let Ok(s) = serde_json::from_value(v.clone()) {
                    node.states = s;
                }
            }
            _ => {}
        }
    }
}

const MAX_VALUE_CHARS: usize = 200;

/// `format:"compact"` — one line per **visible** node:
/// `id␠role␠"name"␠[checked|disabled|focused|value="…"]`, depth-indented
/// with two spaces. Values are truncated at 200 chars and password nodes
/// never emit their value.
pub fn compact_lines(tree: &UiTree) -> String {
    let mut out = String::new();
    for node in &tree.nodes {
        compact_node(node, 0, &mut out);
    }
    out
}

fn is_password(node: &UiNode) -> bool {
    node.role == "password" || node.states.iter().any(|s| s == "password")
}

fn compact_node(node: &UiNode, depth: usize, out: &mut String) {
    if !node.visible {
        return;
    }
    for _ in 0..depth {
        out.push_str("  ");
    }
    out.push_str(&node.id);
    out.push(' ');
    out.push_str(&node.role);
    out.push(' ');
    out.push('"');
    out.push_str(&node.name.replace('"', "'"));
    out.push('"');
    let mut flags: Vec<String> = Vec::new();
    if node.checked == Some(true) {
        flags.push("checked".into());
    }
    if !node.enabled {
        flags.push("disabled".into());
    }
    if node.focused {
        flags.push("focused".into());
    }
    if !is_password(node) {
        if let Some(value) = &node.value {
            let mut v: String = value.chars().take(MAX_VALUE_CHARS).collect();
            if value.chars().count() > MAX_VALUE_CHARS {
                v.push('…');
            }
            flags.push(format!("value=\"{}\"", v.replace('"', "'")));
        }
    }
    if !flags.is_empty() {
        out.push_str("  [");
        out.push_str(&flags.join("|"));
        out.push(']');
    }
    out.push('\n');
    for child in &node.children {
        compact_node(child, depth + 1, out);
    }
}
