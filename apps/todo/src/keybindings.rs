//! Sample GPUI Actions + keymap for the todo embedded-host.
//!
//! `todo.focus_input` is window-scoped (`cmd-n`). `todo.go_settings` and
//! `app.quit` are on the app global map. Agent `Op::Keybinding` fires those
//! Actions through `Window::dispatch_action` / `App::dispatch_action` —
//! never OS HID and never a store fallback in the intercept.

use gpui_kit::*;
use todo_core::ids;

gpui_kit::actions!(todo, [FocusInput, GoSettings, Quit]);

pub fn bind_app_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(ids::KEY_FOCUS_INPUT_CHORD, FocusInput, Some("todo")),
        KeyBinding::new(ids::KEY_GO_SETTINGS_CHORD, GoSettings, None),
        KeyBinding::new(ids::KEY_QUIT_CHORD, Quit, None),
    ]);
}

pub fn action_for_binding(binding: &str) -> Option<Box<dyn Action>> {
    match binding {
        ids::KEY_FOCUS_INPUT => Some(Box::new(FocusInput)),
        ids::KEY_GO_SETTINGS => Some(Box::new(GoSettings)),
        ids::KEY_QUIT => Some(Box::new(Quit)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_chords_parse_as_gpui_keystrokes() {
        assert!(Keystroke::parse(ids::KEY_FOCUS_INPUT_CHORD).is_ok());
        assert!(Keystroke::parse(ids::KEY_GO_SETTINGS_CHORD).is_ok());
        assert!(Keystroke::parse(ids::KEY_QUIT_CHORD).is_ok());
        assert!(
            gpui_agent::keystroke_token("cmd-q").is_err(),
            "free-form key must still reject cmd-q"
        );
    }

    #[test]
    fn action_ids_map_to_gpui_actions() {
        assert_eq!(
            action_for_binding(ids::KEY_FOCUS_INPUT).unwrap().name(),
            FocusInput.name()
        );
        assert_eq!(
            action_for_binding(ids::KEY_GO_SETTINGS).unwrap().name(),
            GoSettings.name()
        );
        assert_eq!(
            action_for_binding(ids::KEY_QUIT).unwrap().name(),
            Quit.name()
        );
        assert!(action_for_binding("nope").is_none());
    }
}
