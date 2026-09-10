use crate::DispatchResult;
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
    /// Write `path` on this machine. Headless hosts must return
    /// [`crate::screenshot_unavailable`] instead of inventing pixels.
    /// Desktop GPUI should intercept `Op::Screenshot` on the UI thread
    /// (real `Window`) and call
    /// [`crate::capture_window_via_screencapture`] on macOS. Linux /
    /// Windows desktop stays unavailable: `Window::render_to_image` is
    /// `test-support` only on this gpui-kit pin.
    fn screenshot(&self, path: Option<&str>) -> Result<DispatchResult, String> {
        let _ = path;
        Err(crate::screenshot_unavailable(
            "this host has no pixel surface",
        ))
    }
}
