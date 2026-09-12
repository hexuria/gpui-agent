//! Helpers for the optional **virtual** delivery mode.
//!
//! Virtual input is an in-process GPUI event path (mailbox → UI thread →
//! `Window::dispatch_event` / `dispatch_keystroke`). It never warps the OS
//! pointer and never posts OS HID. This module is host-agnostic: it plans
//! clicks/keys from the semantic tree. Desktop hosts dispatch the plan;
//! headless hosts return [`virtual_unavailable`].

use crate::tree::{Bounds, UiTree};

/// Stable error prefix so agents can branch without parsing prose.
pub const VIRTUAL_UNAVAILABLE: &str = "virtual_unavailable";

pub fn virtual_unavailable(detail: impl Into<String>) -> String {
    format!("{VIRTUAL_UNAVAILABLE}: {}", detail.into())
}

/// Center of a node's bounds, or `virtual_unavailable` when layout is missing.
pub fn hit_point(bounds: Bounds) -> Result<(f32, f32), String> {
    if !bounds.has_area() {
        return Err(virtual_unavailable(
            "target has zero bounds (no layout / hit-test). \
             Headless hosts never fill bounds. On desktop, wait for a painted frame \
             or use delivery=semantic.",
        ));
    }
    Ok(bounds.center())
}

/// Resolve a click target from the semantic tree.
#[derive(Debug, Clone, PartialEq)]
pub struct VirtualPointerClick {
    pub target: String,
    pub x: f32,
    pub y: f32,
}

pub fn plan_click(tree: &UiTree, target: &str) -> Result<VirtualPointerClick, String> {
    let node = tree.require_id(target)?;
    let (x, y) = hit_point(node.bounds)?;
    Ok(VirtualPointerClick {
        target: target.to_string(),
        x,
        y,
    })
}

/// Map a protocol `key` (`Enter`, `Backspace`, …) to a GPUI `Keystroke::parse` token.
///
/// Modifier-free by design (SECURITY I1). Chords such as `cmd-q` belong on
/// [`crate::Op::Keybinding`], not here.
pub fn keystroke_token(key: &str) -> Result<String, String> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err(virtual_unavailable("empty key"));
    }
    if looks_like_modifier_chord(trimmed) {
        return Err(virtual_unavailable(format!(
            "modifiers are not allowed on free-form key `{trimmed}`; use op keybinding"
        )));
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "enter" | "return" => Ok("enter".into()),
        "backspace" => Ok("backspace".into()),
        "tab" => Ok("tab".into()),
        "escape" | "esc" => Ok("escape".into()),
        "space" | " " => Ok("space".into()),
        "delete" | "del" => Ok("delete".into()),
        other if other.chars().count() == 1 => Ok(other.to_string()),
        other => Err(virtual_unavailable(format!(
            "unhandled virtual key `{other}` (first slice: Enter, Backspace, Tab, Escape, Delete, Space, ASCII)"
        ))),
    }
}

fn looks_like_modifier_chord(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.contains('-')
        || lower.starts_with("cmd")
        || lower.starts_with("ctrl")
        || lower.starts_with("alt")
        || lower.starts_with("shift")
        || lower.starts_with("super")
        || lower.starts_with("meta")
        || lower.starts_with("win")
}

/// One GPUI keystroke token per character for `type` (ASCII + space).
pub fn text_keystrokes(text: &str) -> Result<Vec<String>, String> {
    text.chars()
        .map(|ch| {
            if ch == ' ' {
                Ok("space".into())
            } else if ch.is_ascii_graphic() || ch == '\n' || ch == '\t' {
                if ch == '\n' {
                    Ok("enter".into())
                } else if ch == '\t' {
                    Ok("tab".into())
                } else {
                    Ok(ch.to_string())
                }
            } else {
                Err(virtual_unavailable(format!(
                    "virtual type is ASCII-only in this slice (got {ch:?})"
                )))
            }
        })
        .collect()
}

/// Painted agent cursor. Does **not** control the OS pointer.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentCursor {
    pub x: f32,
    pub y: f32,
    pub visible: bool,
    /// Hue in `0.0..1.0` for the in-window overlay (session-colored).
    pub hue: f32,
}

impl Default for AgentCursor {
    fn default() -> Self {
        Self::session_default()
    }
}

impl AgentCursor {
    /// Cyan session color — distinct from a typical OS arrow.
    pub fn session_default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            visible: false,
            hue: 0.54,
        }
    }

    pub fn move_to(&mut self, x: f32, y: f32) {
        self.x = x;
        self.y = y;
        self.visible = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PlatformKind;
    use crate::tree::{UiNode, UiTree};

    fn tree_with(id: &str, bounds: Bounds) -> UiTree {
        let mut node = UiNode::new(id, "button", "Add");
        node.bounds = bounds;
        UiTree {
            app: "t".into(),
            platform: PlatformKind::Desktop,
            ready: true,
            nodes: vec![node],
        }
    }

    #[test]
    fn zero_bounds_are_unavailable() {
        let err = plan_click(&tree_with("todo-add", Bounds::default()), "todo-add").unwrap_err();
        assert!(err.starts_with(VIRTUAL_UNAVAILABLE), "{err}");
        assert!(err.contains("zero bounds"), "{err}");
    }

    #[test]
    fn missing_node_is_not_virtual_unavailable() {
        let err = plan_click(&tree_with("todo-add", Bounds::default()), "nope").unwrap_err();
        assert!(!err.starts_with(VIRTUAL_UNAVAILABLE), "{err}");
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn plan_click_duplicate_id_is_error() {
        let area = Bounds {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        };
        let mut first = UiNode::new("dup", "button", "First");
        first.bounds = area;
        let mut second = UiNode::new("dup", "button", "Second");
        second.bounds = Bounds {
            x: 20.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        };
        let tree = UiTree {
            app: "t".into(),
            platform: PlatformKind::Desktop,
            ready: true,
            nodes: vec![first, second],
        };
        let err = plan_click(&tree, "dup").unwrap_err();
        assert!(
            err.contains("duplicate id"),
            "duplicate click-id must fail closed, not first-match: {err}"
        );
        assert!(!err.starts_with(VIRTUAL_UNAVAILABLE), "{err}");
    }

    #[test]
    fn hit_center() {
        let plan = plan_click(
            &tree_with(
                "todo-add",
                Bounds {
                    x: 10.0,
                    y: 20.0,
                    w: 40.0,
                    h: 10.0,
                },
            ),
            "todo-add",
        )
        .unwrap();
        assert_eq!((plan.x, plan.y), (30.0, 25.0));
    }

    #[test]
    fn key_tokens() {
        assert_eq!(keystroke_token("Enter").unwrap(), "enter");
        assert_eq!(keystroke_token("Backspace").unwrap(), "backspace");
        assert_eq!(keystroke_token("a").unwrap(), "a");
        assert!(
            keystroke_token("F13")
                .unwrap_err()
                .starts_with(VIRTUAL_UNAVAILABLE)
        );
        let cmd_q = keystroke_token("cmd-q").unwrap_err();
        assert!(cmd_q.starts_with(VIRTUAL_UNAVAILABLE), "{cmd_q}");
        assert!(cmd_q.contains("modifiers"), "{cmd_q}");
        assert!(cmd_q.contains("keybinding"), "{cmd_q}");
        assert!(keystroke_token("cmd-q").is_err());
        assert!(keystroke_token("ctrl-c").unwrap_err().contains("modifiers"));
    }

    #[test]
    fn type_tokens() {
        assert_eq!(text_keystrokes("Hi ").unwrap(), ["H", "i", "space"]);
    }

    #[test]
    fn cursor_moves_without_os_claim() {
        let mut cursor = AgentCursor::session_default();
        assert!(!cursor.visible);
        cursor.move_to(8.0, 12.0);
        assert!(cursor.visible);
        assert_eq!((cursor.x, cursor.y), (8.0, 12.0));
    }
}
