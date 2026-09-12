//! Allow-listed GPUI **Action** dispatch (`Op::Keybinding` / `Op::Keybindings`).
//!
//! This is **not** free-form virtual `key`. Modifier chords belong here, as
//! Action ids the host registered, never as OS HID and never as unconstrained
//! `cmd-q` on [`crate::keystroke_token`].

use crate::protocol::{KeybindingScope, Op};

/// Stable error prefix so agents can branch without parsing prose.
pub const KEYBINDING_UNAVAILABLE: &str = "keybinding_unavailable";

pub fn keybinding_unavailable(detail: impl Into<String>) -> String {
    format!("{KEYBINDING_UNAVAILABLE}: {}", detail.into())
}

pub fn is_keybinding_unavailable(error: &str) -> bool {
    error == KEYBINDING_UNAVAILABLE
        || error.starts_with(KEYBINDING_UNAVAILABLE)
            && error.as_bytes().get(KEYBINDING_UNAVAILABLE.len()) == Some(&b':')
}

/// One row of `keybindings` / `keybinding.list`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KeybindingInfo {
    pub id: String,
    pub chord: String,
    pub scope: KeybindingScope,
    pub dangerous: bool,
}

impl KeybindingInfo {
    pub fn new(
        id: impl Into<String>,
        chord: impl Into<String>,
        scope: KeybindingScope,
        dangerous: bool,
    ) -> Self {
        Self {
            id: id.into(),
            chord: chord.into(),
            scope,
            dangerous,
        }
    }
}

/// Fields of [`Op::Keybinding`] used by the shared authorize gate.
#[derive(Debug, Clone, Copy)]
pub struct KeybindingRequest<'a> {
    pub binding: &'a str,
    pub chord: Option<&'a str>,
    pub scope: KeybindingScope,
    pub confirm: bool,
    pub activate: bool,
}

impl<'a> KeybindingRequest<'a> {
    pub fn from_op(op: &'a Op) -> Result<Self, String> {
        match op {
            Op::Keybinding {
                binding,
                chord,
                scope,
                confirm,
                activate,
            } => Ok(Self {
                binding,
                chord: chord.as_deref(),
                scope: *scope,
                confirm: *confirm,
                activate: *activate,
            }),
            _ => Err("not a keybinding fire op".into()),
        }
    }
}

/// Process-ending Action ids. Token auth is not enough; `confirm=true` is
/// required even if the host forgot `dangerous: true` on the list row.
pub fn is_quit_binding(binding: &str) -> bool {
    let n = binding.trim().to_ascii_lowercase();
    n == "quit" || n == "app.quit" || n.ends_with(".quit")
}

pub fn is_quit_chord(chord: &str) -> bool {
    matches!(
        normalize_chord(chord).as_str(),
        "cmd-q" | "super-q" | "meta-q" | "win-q"
    )
}

pub fn binding_is_dangerous(entry: &KeybindingInfo) -> bool {
    entry.dangerous || is_quit_binding(&entry.id) || is_quit_chord(&entry.chord)
}

pub fn op_is_confirmed_quit(op: &Op) -> bool {
    match op {
        Op::Keybinding {
            binding,
            confirm: true,
            ..
        } if is_quit_binding(binding) => true,
        _ => false,
    }
}

pub fn keybinding_list_json(catalog: &[KeybindingInfo]) -> serde_json::Value {
    let keybindings: Vec<KeybindingInfo> = catalog
        .iter()
        .cloned()
        .map(|mut row| {
            row.dangerous = binding_is_dangerous(&row);
            row
        })
        .collect();
    serde_json::json!({ "keybindings": keybindings })
}

/// Mailbox reply after `dispatch_action`. `listener_result` is `Some` only if
/// a keymap / `on_action` handler actually ran.
///
/// A no-op Action (wrong context, deferred drop, kit gap) must fail closed.
/// Do **not** call Action bodies from the intercept to synthesize `Some`.
pub fn complete_keybinding_action(
    listener_result: Option<Result<crate::dispatch::DispatchResult, String>>,
) -> Result<crate::dispatch::DispatchResult, String> {
    match listener_result {
        Some(result) => result,
        None => Err(keybinding_unavailable("Action handler did not run")),
    }
}

/// Testable intercept: `dispatch_action` may invoke listeners, which record
/// `Some(result)` into the slot. The intercept itself must not run Action
/// bodies when the slot stays `None`.
pub fn intercept_keybinding_action(
    dispatch_action: impl FnOnce(&mut Option<Result<crate::dispatch::DispatchResult, String>>),
) -> Result<crate::dispatch::DispatchResult, String> {
    let mut listener_result = None;
    dispatch_action(&mut listener_result);
    complete_keybinding_action(listener_result)
}

/// Shared gates every host must apply before dispatching an Action.
///
/// Lookup is `(binding, scope)`. Optional `chord` is a resolve aid and must
/// match the catalog when present. `scope=focused` does not auto-activate.
pub fn authorize_keybinding<'a>(
    catalog: &'a [KeybindingInfo],
    request: KeybindingRequest<'_>,
    app_focused: bool,
) -> Result<&'a KeybindingInfo, String> {
    let binding = request.binding.trim();
    if binding.is_empty() {
        return Err("keybinding requires binding (Action id)".into());
    }

    let by_id: Vec<&KeybindingInfo> = catalog.iter().filter(|row| row.id == binding).collect();
    if by_id.is_empty() {
        return Err(format!("unknown binding `{binding}`"));
    }
    let Some(entry) = by_id.iter().copied().find(|row| row.scope == request.scope) else {
        let actual = by_id
            .iter()
            .map(|row| row.scope.as_str())
            .collect::<Vec<_>>()
            .join("/");
        return Err(format!(
            "binding `{binding}` is registered as {actual}, not {}",
            request.scope
        ));
    };

    if let Some(chord) = request.chord.map(str::trim).filter(|s| !s.is_empty()) {
        if !chords_eq(&entry.chord, chord) {
            return Err(format!(
                "chord `{chord}` does not match binding `{binding}` ({})",
                entry.chord
            ));
        }
    }

    if request.scope == KeybindingScope::Global && request.activate {
        return Err("activate=true is only valid for scope=focused".into());
    }

    if request.scope == KeybindingScope::Focused && !app_focused && !request.activate {
        return Err(keybinding_unavailable("app not focused"));
    }

    if binding_is_dangerous(entry) && !request.confirm {
        return Err(format!(
            "confirm=true is required for dangerous keybinding `{binding}`"
        ));
    }

    Ok(entry)
}

pub fn authorize_keybinding_op<'a>(
    op: &'a Op,
    catalog: &'a [KeybindingInfo],
    app_focused: bool,
) -> Result<&'a KeybindingInfo, String> {
    authorize_keybinding(catalog, KeybindingRequest::from_op(op)?, app_focused)
}

fn normalize_chord(chord: &str) -> String {
    chord
        .trim()
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
        .replace("command", "cmd")
        .replace("control", "ctrl")
}

fn chords_eq(registered: &str, given: &str) -> bool {
    normalize_chord(registered) == normalize_chord(given)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Vec<KeybindingInfo> {
        vec![
            KeybindingInfo::new("todo.focus_input", "cmd-n", KeybindingScope::Focused, false),
            KeybindingInfo::new(
                "todo.go_settings",
                "cmd-shift-s",
                KeybindingScope::Global,
                false,
            ),
            KeybindingInfo::new("app.quit", "cmd-q", KeybindingScope::Global, true),
        ]
    }

    fn fire(
        binding: &str,
        scope: KeybindingScope,
        confirm: bool,
        activate: bool,
        focused: bool,
    ) -> Result<String, String> {
        let cat = catalog();
        authorize_keybinding(
            &cat,
            KeybindingRequest {
                binding,
                chord: None,
                scope,
                confirm,
                activate,
            },
            focused,
        )
        .map(|row| row.id.clone())
    }

    #[test]
    fn focused_without_focus_is_unavailable() {
        let err = fire(
            "todo.focus_input",
            KeybindingScope::Focused,
            false,
            false,
            false,
        )
        .unwrap_err();
        assert!(is_keybinding_unavailable(&err), "{err}");
        assert!(err.contains("app not focused"), "{err}");
    }

    #[test]
    fn focused_with_activate_is_ok_when_unfocused() {
        assert_eq!(
            fire(
                "todo.focus_input",
                KeybindingScope::Focused,
                false,
                true,
                false
            )
            .unwrap(),
            "todo.focus_input"
        );
    }

    #[test]
    fn focused_ok_when_already_focused() {
        assert_eq!(
            fire(
                "todo.focus_input",
                KeybindingScope::Focused,
                false,
                false,
                true
            )
            .unwrap(),
            "todo.focus_input"
        );
    }

    #[test]
    fn global_does_not_require_focus() {
        assert_eq!(
            fire(
                "todo.go_settings",
                KeybindingScope::Global,
                false,
                false,
                false
            )
            .unwrap(),
            "todo.go_settings"
        );
    }

    #[test]
    fn global_rejects_activate() {
        let err = fire(
            "todo.go_settings",
            KeybindingScope::Global,
            false,
            true,
            false,
        )
        .unwrap_err();
        assert!(err.contains("activate=true"), "{err}");
        assert!(!is_keybinding_unavailable(&err), "{err}");
    }

    #[test]
    fn scope_mismatch_does_not_promote_to_global() {
        let err = fire(
            "todo.focus_input",
            KeybindingScope::Global,
            false,
            false,
            true,
        )
        .unwrap_err();
        assert!(err.contains("registered as focused"), "{err}");
        assert!(err.contains("not global"), "{err}");
    }

    #[test]
    fn unknown_binding() {
        let err = fire("nope.action", KeybindingScope::Focused, false, false, true).unwrap_err();
        assert!(err.contains("unknown binding"), "{err}");
    }

    #[test]
    fn quit_requires_confirm_even_with_token_class_auth() {
        let err = fire("app.quit", KeybindingScope::Global, false, false, false).unwrap_err();
        assert!(err.contains("confirm=true"), "{err}");
        assert!(err.contains("app.quit"), "{err}");
        assert!(fire("app.quit", KeybindingScope::Global, true, false, false).is_ok());
    }

    #[test]
    fn catalog_forgetting_dangerous_still_gates_quit() {
        let cat = [KeybindingInfo::new(
            "app.quit",
            "cmd-q",
            KeybindingScope::Global,
            false,
        )];
        let err = authorize_keybinding(
            &cat,
            KeybindingRequest {
                binding: "app.quit",
                chord: None,
                scope: KeybindingScope::Global,
                confirm: false,
                activate: false,
            },
            false,
        )
        .unwrap_err();
        assert!(err.contains("confirm=true"), "{err}");
    }

    #[test]
    fn chord_must_match_when_supplied() {
        let cat = catalog();
        let err = authorize_keybinding(
            &cat,
            KeybindingRequest {
                binding: "todo.go_settings",
                chord: Some("cmd-q"),
                scope: KeybindingScope::Global,
                confirm: false,
                activate: false,
            },
            false,
        )
        .unwrap_err();
        assert!(err.contains("does not match"), "{err}");
        assert!(
            authorize_keybinding(
                &cat,
                KeybindingRequest {
                    binding: "todo.go_settings",
                    chord: Some("cmd-shift-s"),
                    scope: KeybindingScope::Global,
                    confirm: false,
                    activate: false,
                },
                false,
            )
            .is_ok()
        );
    }

    #[test]
    fn list_json_shape() {
        let json = keybinding_list_json(&catalog());
        assert_eq!(json["keybindings"][2]["id"], "app.quit");
        assert_eq!(json["keybindings"][2]["dangerous"], true);
        assert_eq!(json["keybindings"][0]["scope"], "focused");
    }

    #[test]
    fn list_json_uses_binding_is_dangerous_not_stored_flag() {
        let cat = [KeybindingInfo::new(
            "app.quit",
            "cmd-q",
            KeybindingScope::Global,
            false,
        )];
        let json = keybinding_list_json(&cat);
        assert_eq!(json["keybindings"][0]["dangerous"], true);
        assert_eq!(json["keybindings"][0]["id"], "app.quit");
    }

    #[test]
    fn no_op_action_fails_closed_without_listener_result() {
        let err = complete_keybinding_action(None).unwrap_err();
        assert!(is_keybinding_unavailable(&err), "{err}");
        assert!(err.contains("Action handler did not run"), "{err}");
    }

    #[test]
    fn intercept_no_op_does_not_succeed_via_side_door_slot() {
        use crate::dispatch::DispatchResult;

        let mut mutated = false;
        let err = intercept_keybinding_action(|slot| {
            // Action dispatch is a no-op: listener never records `Some`.
            // Dual-write would be: mutated = true; *slot = Some(Ok(...)).
            let _ = slot;
        })
        .unwrap_err();
        assert!(is_keybinding_unavailable(&err), "{err}");
        assert!(
            !mutated,
            "no-op Action must not mutate via intercept fallback"
        );

        let ok = intercept_keybinding_action(|slot| {
            mutated = true;
            *slot = Some(Ok(DispatchResult::json(serde_json::json!({
                "path": "gpui.action"
            }))));
        })
        .unwrap();
        assert!(mutated);
        assert_eq!(ok.value.unwrap()["path"], "gpui.action");
    }
}
