//! Short refs (`@e1`, `@e2`, …) for the nodes of the last `snapshot --refs`.
//!
//! Host ids can be long or opaque (`n1438`); a ref is short, stable until
//! the next `snapshot --refs`, and accepted wherever a command takes a node
//! id. The table lives in the runtime directory, one file per host address,
//! because each CLI command is its own process.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use gpui_agent::tree::{UiNode, UiTree};

fn table_path(root: &Path, addr: SocketAddr) -> PathBuf {
    let name: String = addr
        .to_string()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    root.join("refs").join(format!("{name}.json"))
}

/// `true` for an argument spelled as a ref.
pub fn is_ref(arg: &str) -> bool {
    arg.strip_prefix("@e")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// A node earns a line when it says something or can be acted on; bare
/// layout containers only pass their children up. Where the host gives
/// geometry (`sized`), a node without any is not on screen.
fn listed(node: &UiNode, sized: bool) -> bool {
    node.visible
        && (!sized || has_size(node))
        && (node.role != "generic" || !node.name.is_empty() || node.value.is_some())
}

fn has_size(node: &UiNode) -> bool {
    node.bounds.w > 0.0 && node.bounds.h > 0.0
}

/// Headless hosts publish no bounds at all.
fn any_sized(nodes: &[UiNode]) -> bool {
    nodes
        .iter()
        .any(|node| has_size(node) || any_sized(&node.children))
}

fn outline(
    nodes: &[UiNode],
    sized: bool,
    depth: usize,
    out: &mut String,
    table: &mut BTreeMap<String, String>,
) {
    for node in nodes {
        if !node.visible {
            continue;
        }
        let mut below = depth;
        if listed(node, sized) {
            let key = format!("e{}", table.len() + 1);
            let _ = write!(
                out,
                "{:indent$}@{key} {}",
                "",
                node.role,
                indent = depth * 2
            );
            if !node.name.is_empty() {
                let _ = write!(out, " {:?}", node.name);
            }
            if let Some(value) = &node.value {
                let _ = write!(out, " value={value:?}");
            }
            if let Some(checked) = node.checked {
                let _ = write!(out, " checked={checked}");
            }
            if !node.enabled {
                out.push_str(" disabled");
            }
            if node.focused {
                out.push_str(" focused");
            }
            out.push('\n');
            table.insert(key, node.id.clone());
            below += 1;
        }
        outline(&node.children, sized, below, out, table);
    }
}

/// The outline of `tree` and the ref table that goes with it.
pub fn render(tree: &UiTree) -> (String, BTreeMap<String, String>) {
    let mut out = String::new();
    let mut table = BTreeMap::new();
    outline(&tree.nodes, any_sized(&tree.nodes), 0, &mut out, &mut table);
    (out, table)
}

pub fn save(
    root: &Path,
    addr: SocketAddr,
    table: &BTreeMap<String, String>,
) -> std::io::Result<()> {
    let path = table_path(root, addr);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_vec(table)?)
}

/// Replaces every ref among `args` with the node id it stands for. The
/// arguments stay as the OS gave them: a path need not be UTF-8.
pub fn resolve(root: &Path, addr: SocketAddr, args: &mut [OsString]) -> Result<(), String> {
    let as_ref = |arg: &OsString| arg.to_str().filter(|arg| is_ref(arg)).map(str::to_owned);
    if !args.iter().any(|arg| as_ref(arg).is_some()) {
        return Ok(());
    }
    let table: BTreeMap<String, String> = std::fs::read(table_path(root, addr))
        .ok()
        .and_then(|body| serde_json::from_slice(&body).ok())
        .ok_or_else(|| {
            format!("no refs saved for {addr}: run `gpui-agent snapshot --refs` first")
        })?;
    for arg in args.iter_mut() {
        let Some(name) = as_ref(arg) else { continue };
        let id = table.get(&name[1..]).ok_or_else(|| {
            format!("unknown ref {name}: it is not in the last `gpui-agent snapshot --refs`")
        })?;
        *arg = OsString::from(id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, role: &str, name: &str, children: Vec<UiNode>) -> UiNode {
        let mut node = UiNode::new(id, role, name);
        node.bounds.w = 10.0;
        node.bounds.h = 10.0;
        node.children = children;
        node
    }

    #[test]
    fn refs_number_the_listed_nodes_and_resolve_back_to_ids() {
        let mut hidden = node("gone", "button", "Hidden", vec![]);
        hidden.visible = false;
        let mut tree = UiTree {
            app: "t".into(),
            platform: gpui_agent::protocol::PlatformKind::Headless,
            ready: true,
            nodes: vec![node(
                "root",
                "generic",
                "",
                vec![
                    node("n12", "button", "Save", vec![]),
                    hidden,
                    node(
                        "list",
                        "scroll",
                        "",
                        vec![node("n40", "note", "Row", vec![])],
                    ),
                ],
            )],
            rev: None,
        };
        let (text, table) = render(&tree);
        assert_eq!(
            text,
            "@e1 button \"Save\"\n@e2 scroll\n  @e3 note \"Row\"\n"
        );

        // A headless host gives no bounds: its nodes are listed all the same.
        fn unsize(nodes: &mut [UiNode]) {
            for node in nodes {
                node.bounds.w = 0.0;
                node.bounds.h = 0.0;
                unsize(&mut node.children);
            }
        }
        let mut headless = tree.clone();
        unsize(&mut headless.nodes);
        assert_eq!(render(&headless).0, text);
        // With geometry, a node that has none is off screen.
        tree.nodes[0].children[0].bounds.w = 0.0;
        assert_eq!(render(&tree).0, "@e1 scroll\n  @e2 note \"Row\"\n");

        let root = std::env::temp_dir().join(format!("gpui-agent-refs-{}", std::process::id()));
        let addr: SocketAddr = "127.0.0.1:17421".parse().unwrap();
        save(&root, addr, &table).unwrap();
        let mut args: Vec<OsString> = vec!["press".into(), "@e3".into(), "@everyone".into()];
        resolve(&root, addr, &mut args).unwrap();
        assert_eq!(args, ["press", "n40", "@everyone"]);
        let mut stale: Vec<OsString> = vec!["@e9".into()];
        assert!(
            resolve(&root, addr, &mut stale)
                .unwrap_err()
                .contains("unknown ref @e9")
        );
        let other: SocketAddr = "127.0.0.1:1".parse().unwrap();
        assert!(
            resolve(&root, other, &mut stale)
                .unwrap_err()
                .contains("no refs saved")
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
