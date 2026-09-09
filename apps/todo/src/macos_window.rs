//! CGWindowID for this process's NSWindow. Used only as `screencapture -l`.

use std::ffi::c_void;

use gpui_kit::Window;
#[allow(unused_imports)]
use objc::{msg_send, runtime::Object, sel, sel_impl};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// `NSWindow.windowNumber` for the GPUI `Window` currently on the UI thread.
pub fn cgwindow_id(window: &Window) -> Result<u32, String> {
    let handle = HasWindowHandle::window_handle(window).map_err(|err| {
        gpui_agent::screenshot_unavailable(format!("no native window handle: {err}"))
    })?;
    match handle.as_raw() {
        RawWindowHandle::AppKit(appkit) => unsafe { window_number(appkit.ns_view.as_ptr()) },
        other => Err(gpui_agent::screenshot_unavailable(format!(
            "expected AppKit window handle, got {other:?}"
        ))),
    }
}

unsafe fn window_number(ns_view: *mut c_void) -> Result<u32, String> {
    if ns_view.is_null() {
        return Err(gpui_agent::screenshot_unavailable("NSView pointer is null"));
    }
    let view: *mut Object = ns_view.cast();
    // Edition 2024: `unsafe fn` does not make the body an unsafe block.
    let ns_window: *mut Object = unsafe { msg_send![view, window] };
    if ns_window.is_null() {
        return Err(gpui_agent::screenshot_unavailable(
            "NSView has no NSWindow yet",
        ));
    }
    let number: isize = unsafe { msg_send![ns_window, windowNumber] };
    if number <= 0 {
        return Err(gpui_agent::screenshot_unavailable(format!(
            "NSWindow.windowNumber is {number} (need a real on-screen window)"
        )));
    }
    Ok(number as u32)
}
