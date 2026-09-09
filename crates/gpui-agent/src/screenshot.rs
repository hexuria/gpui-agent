//! Observe-only pixel capture for the **app surface**.
//!
//! This is not OS HID. Headless hosts return [`screenshot_unavailable`] —
//! same honesty as [`virtual_unavailable`](crate::virtual_unavailable).
//! Desktop GPUI on macOS writes a PNG of **this window** via
//! `screencapture -l` (needs Screen Recording). Linux/Windows desktop
//! stays unavailable: GPUI's `Window::render_to_image` is
//! `test-support` only on this pin, and this crate does not capture the
//! full desktop. The host writes the PNG to a local `path` so the image
//! does not ride the 1 MiB NDJSON line.

use std::path::Path;
#[cfg(target_os = "macos")]
use std::process::Command;

use crate::DispatchResult;

/// Stable error prefix so agents can branch without parsing prose.
pub const SCREENSHOT_UNAVAILABLE: &str = "screenshot_unavailable";

/// `result.backend` when the PNG came from macOS `screencapture -l`.
pub const SCREENSHOT_BACKEND_SCREENCAPTURE: &str = "screencapture";

pub fn screenshot_unavailable(detail: impl Into<String>) -> String {
    format!("{SCREENSHOT_UNAVAILABLE}: {}", detail.into())
}

pub fn is_screenshot_unavailable(error: &str) -> bool {
    error == SCREENSHOT_UNAVAILABLE
        || error.starts_with(SCREENSHOT_UNAVAILABLE)
            && error.as_bytes().get(SCREENSHOT_UNAVAILABLE.len()) == Some(&b':')
}

/// Client-supplied destination. Empty is a request error, not
/// [`screenshot_unavailable`] (the host never invents a path).
pub fn require_screenshot_path<'a>(path: Option<&'a str>) -> Result<&'a str, String> {
    match path.map(str::trim).filter(|p| !p.is_empty()) {
        Some(path) => Ok(path),
        None => Err("screenshot requires path".into()),
    }
}

/// Write `png` bytes to `path`. Production hosts pass a real frame;
/// tests may pass [`TEST_PNG`]. Never invent pixels for a missing surface.
pub fn write_png(path: &str, png: &[u8]) -> Result<serde_json::Value, String> {
    let path = require_screenshot_path(Some(path))?;
    let dest = Path::new(path);
    if let Some(parent) = dest.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    std::fs::write(dest, png).map_err(|err| format!("{}: {err}", dest.display()))?;
    Ok(serde_json::json!({ "path": path }))
}

/// 1×1 transparent PNG for tests / mock hosts only. Not a stand-in for
/// a real window capture in production hosts.
pub const TEST_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

/// Argv for `screencapture` of **one** window. Never omits `-l`; never
/// interactive (`-i`/`-w`) or full-screen (`-S`) capture. `window_id` is
/// a host-chosen CGWindowID (not a client string). `path` is the only
/// client-influenced argument and is passed as a single argv slot (no
/// shell).
pub fn screencapture_window_argv(window_id: u32, path: &str) -> Result<Vec<String>, String> {
    let path = require_screenshot_path(Some(path))?;
    if window_id == 0 {
        return Err(screenshot_unavailable(
            "refusing screencapture without a CGWindowID (would not be this app window)",
        ));
    }
    Ok(vec![
        format!("-l{window_id}"),
        "-o".into(),
        "-x".into(),
        path.into(),
    ])
}

/// Capture **this** window to `path` with macOS `screencapture -l`.
///
/// Linux/Windows return [`screenshot_unavailable`] without running a
/// command and without creating a file. On macOS, a missing Screen
/// Recording grant also maps to unavailable (no fake PNG).
pub fn capture_window_via_screencapture(
    window_id: u32,
    path: Option<&str>,
) -> Result<DispatchResult, String> {
    let path = require_screenshot_path(path)?;
    let args = screencapture_window_argv(window_id, path)?;
    #[cfg(target_os = "macos")]
    {
        run_screencapture(&args, path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = args;
        Err(screenshot_unavailable(
            "screencapture -l is macOS-only; this OS has no production GPUI framebuffer export",
        ))
    }
}

#[cfg(target_os = "macos")]
fn run_screencapture(args: &[String], dest: &str) -> Result<DispatchResult, String> {
    let dest_path = Path::new(dest);
    if dest_path.exists() {
        let _ = std::fs::remove_file(dest_path);
    }
    if let Some(parent) = dest_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("create {}: {err}", parent.display()))?;
    }

    let output = Command::new("screencapture")
        .args(args)
        .output()
        .map_err(|err| {
            screenshot_unavailable(format!(
                "screencapture exec failed: {err}. Grant Screen Recording to this app/terminal."
            ))
        })?;

    if !output.status.success() {
        remove_unless_png(dest_path);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        return Err(screenshot_unavailable(format!(
            "screencapture exited {}. Grant Screen Recording in System Settings if this is a permission error.{}",
            output.status,
            if stderr.is_empty() {
                String::new()
            } else {
                format!(" {stderr}")
            }
        )));
    }

    accept_written_png(dest)
}

/// Keep `path` only if it is a real PNG. Otherwise delete it and fail
/// closed — never leave a non-image as a "success".
pub fn accept_written_png(path: &str) -> Result<DispatchResult, String> {
    let dest = Path::new(path);
    let bytes = match std::fs::read(dest) {
        Ok(bytes) => bytes,
        Err(err) => {
            return Err(screenshot_unavailable(format!(
                "screencapture produced no file at {path}: {err}"
            )));
        }
    };
    if !bytes.starts_with(b"\x89PNG") {
        let _ = std::fs::remove_file(dest);
        return Err(screenshot_unavailable(format!(
            "screencapture wrote a non-PNG at {path}; refusing to keep it"
        )));
    }
    Ok(DispatchResult::json(serde_json::json!({
        "path": path,
        "backend": SCREENSHOT_BACKEND_SCREENCAPTURE,
    })))
}

#[cfg(target_os = "macos")]
fn remove_unless_png(path: &Path) {
    match std::fs::read(path) {
        Ok(bytes) if bytes.starts_with(b"\x89PNG") => {}
        Ok(_) | Err(_) => {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_matches() {
        let err = screenshot_unavailable("headless host has no pixel surface");
        assert!(is_screenshot_unavailable(&err));
        assert!(!is_screenshot_unavailable("virtual_unavailable: x"));
        assert!(TEST_PNG.starts_with(b"\x89PNG"));
    }

    #[test]
    fn write_png_roundtrip() {
        let dir = std::env::temp_dir().join(format!("gpui-agent-test-png-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("001-wait.png");
        let value = write_png(path.to_str().unwrap(), TEST_PNG).unwrap();
        assert_eq!(value["path"], path.to_str().unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), TEST_PNG);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_path_is_a_request_error() {
        assert_eq!(
            require_screenshot_path(None).unwrap_err(),
            "screenshot requires path"
        );
        assert_eq!(
            require_screenshot_path(Some("  ")).unwrap_err(),
            "screenshot requires path"
        );
        assert!(write_png("", TEST_PNG).is_err());
    }

    #[test]
    fn screencapture_argv_is_this_window_only() {
        let args = screencapture_window_argv(4242, "/tmp/todo.png").unwrap();
        assert_eq!(args[0], "-l4242");
        assert!(args.contains(&"-o".to_string()));
        assert!(args.contains(&"-x".to_string()));
        assert_eq!(args.last().unwrap(), "/tmp/todo.png");
        let joined = args.join(" ");
        for forbidden in ["-i", "-S", "-w", "-C", "-R", "-W"] {
            assert!(
                !args.iter().any(|a| a == forbidden),
                "argv must not include interactive/full-desktop flag {forbidden}: {joined}"
            );
        }
        assert!(
            !args.iter().any(|a| a == "-l" || a == "-l0"),
            "window id must be glued to -l so it cannot be dropped: {joined}"
        );
    }

    #[test]
    fn screencapture_refuses_window_id_zero() {
        let err = screencapture_window_argv(0, "/tmp/todo.png").unwrap_err();
        assert!(is_screenshot_unavailable(&err), "{err}");
        assert!(err.contains("CGWindowID"), "{err}");
    }

    #[test]
    fn accept_written_png_keeps_real_png() {
        let dir =
            std::env::temp_dir().join(format!("gpui-agent-accept-png-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ok.png");
        std::fs::write(&path, TEST_PNG).unwrap();
        let result = accept_written_png(path.to_str().unwrap()).unwrap();
        let value = result.value.unwrap();
        assert_eq!(value["path"], path.to_str().unwrap());
        assert_eq!(value["backend"], SCREENSHOT_BACKEND_SCREENCAPTURE);
        assert_eq!(std::fs::read(&path).unwrap(), TEST_PNG);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accept_written_png_deletes_non_png() {
        let dir =
            std::env::temp_dir().join(format!("gpui-agent-reject-png-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nope.png");
        std::fs::write(&path, b"not a png").unwrap();
        let err = accept_written_png(path.to_str().unwrap()).unwrap_err();
        assert!(is_screenshot_unavailable(&err), "{err}");
        assert!(
            !path.exists(),
            "non-PNG must not remain at {}",
            path.display()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn capture_without_macos_does_not_invent_a_file() {
        if cfg!(target_os = "macos") {
            // Live capture needs this process's CGWindowID and Screen
            // Recording. Do not call screencapture with a guessed id.
            return;
        }
        let dest = std::env::temp_dir().join(format!(
            "gpui-agent-no-screencapture-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&dest);
        let err = capture_window_via_screencapture(7, dest.to_str()).unwrap_err();
        assert!(is_screenshot_unavailable(&err), "{err}");
        assert!(err.contains("macOS-only"), "{err}");
        assert!(!dest.exists(), "must not invent {}", dest.display());
    }
}
