use crate::DispatchResult;
use crate::keybinding::KeybindingInfo;
use crate::protocol::{HelloInfo, Op};
use crate::tree::UiTree;

/// A host is the platform-specific half of the control plane.
///
/// Desktop GPUI Kit apps, the headless test binary, and (later) a WASM or
/// mobile shell all implement this trait and speak the same protocol.
pub trait AgentHost: Send {
    fn hello(&self) -> HelloInfo;
    fn snapshot(&self) -> UiTree;
    fn dispatch(&mut self, op: &Op) -> Result<DispatchResult, String>;

    /// Observe-only PNG of the **app surface** (not the desktop).
    ///
    /// Write `spec.path` on this machine. Headless hosts must return
    /// [`crate::screenshot_unavailable`] instead of inventing pixels —
    /// including `mode=scrolled`. Desktop GPUI should intercept
    /// `Op::Screenshot` on the UI thread (real `Window`) and call
    /// [`crate::capture_window_via_screencapture`] on macOS (viewport)
    /// or the semantic scroll API (scrolled: set offset → wait paint →
    /// tile → stitch → restore). Linux / Windows desktop stays
    /// unavailable: `Window::render_to_image` is `test-support` only
    /// on this gpui-kit pin. Offscreen render is out of MVP.
    fn screenshot(
        &self,
        spec: crate::scroll_capture::ScreenshotSpec<'_>,
    ) -> Result<DispatchResult, String> {
        let _ = spec;
        Err(crate::screenshot_unavailable(
            "this host has no pixel surface",
        ))
    }

    /// Action-id keymap rows this host will fire. Empty means no `keybinding`
    /// fire will succeed (`unknown binding`).
    fn keybindings(&self) -> Vec<KeybindingInfo> {
        Vec::new()
    }

    /// Whether this app currently has OS / window focus.
    ///
    /// Used for `scope=focused`. Default fail-closed (`false`). Desktop
    /// mailbox intercepts should pass `Window::is_window_active` instead of
    /// this when they dispatch GPUI Actions themselves.
    fn is_app_focused(&self) -> bool {
        false
    }
}
