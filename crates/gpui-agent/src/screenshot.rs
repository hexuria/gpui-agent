//! Observe-only pixel capture for the **app surface**.
//!
//! This is not OS HID. Headless hosts return [`screenshot_unavailable`] —
//! same honesty as [`virtual_unavailable`](crate::virtual_unavailable).
//! A painted GPUI window on macOS may write a PNG of **this window** via
//! `screencapture -l` (needs Screen Recording). Headless hosts, a GUI
//! that is only a daemon client, and Linux/Windows desktop stay
//! unavailable: GPUI's `Window::render_to_image` is `test-support` only
//! on this pin, and this crate does not capture the full desktop. The
//! host writes the PNG to a local `path` so the image does not ride the
//! 1 MiB NDJSON line.

use std::path::{Component, Path, PathBuf};
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
pub fn require_screenshot_path(path: Option<&str>) -> Result<&str, String> {
    match path.map(str::trim).filter(|p| !p.is_empty()) {
        Some(path) => Ok(path),
        None => Err("screenshot requires path".into()),
    }
}

/// Host-chosen directory for PNG writes.
///
/// `GPUI_AGENT_SCREENSHOT_DIR` if set and non-empty, otherwise
/// `{temp_dir}/gpui-agent-screenshots/`.
pub fn screenshot_base_dir() -> PathBuf {
    std::env::var("GPUI_AGENT_SCREENSHOT_DIR")
        .ok()
        .map(|raw| raw.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("gpui-agent-screenshots"))
}

/// Resolve a client screenshot name against [`screenshot_base_dir`].
pub fn confine_screenshot_path(client: &str) -> Result<PathBuf, String> {
    confine_screenshot_path_in(client, &screenshot_base_dir())
}

/// Resolve `client` against `base`. Relative `.png` names only; no `..`.
pub fn confine_screenshot_path_in(client: &str, base: &Path) -> Result<PathBuf, String> {
    let client = require_screenshot_path(Some(client))?;
    let raw = Path::new(client);
    if raw.is_absolute() {
        return Err(SCREENSHOT_PATH_CONFINE_ERR.into());
    }
    let mut rel = PathBuf::new();
    for c in raw.components() {
        match c {
            Component::CurDir => {}
            Component::Normal(part) => {
                let s = part
                    .to_str()
                    .ok_or_else(|| SCREENSHOT_PATH_CONFINE_ERR.to_string())?;
                if s.is_empty() || s == "." || s == ".." || s.starts_with('~') {
                    return Err(SCREENSHOT_PATH_CONFINE_ERR.into());
                }
                rel.push(s);
            }
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                return Err(SCREENSHOT_PATH_CONFINE_ERR.into());
            }
        }
    }
    if rel.as_os_str().is_empty() {
        return Err("screenshot requires path".into());
    }
    let file = rel
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| SCREENSHOT_PATH_CONFINE_ERR.to_string())?;
    if !file.ends_with(".png") || file.len() < 5 {
        return Err("screenshot path must end with .png".into());
    }
    if file.starts_with('-') || file.starts_with('.') {
        return Err("screenshot filename must not start with '-' or '.'".into());
    }
    let dest = base.join(&rel);
    if !dest.starts_with(base) {
        return Err(SCREENSHOT_PATH_CONFINE_ERR.into());
    }
    Ok(dest)
}

const SCREENSHOT_PATH_CONFINE_ERR: &str =
    "screenshot path must be a relative .png name under the host screenshot dir";

/// Write `png` bytes to a confined `path`. Production hosts pass a real
/// frame; tests may pass [`TEST_PNG`]. Never invent pixels for a missing surface.
pub fn write_png(path: &str, png: &[u8]) -> Result<serde_json::Value, String> {
    write_png_in(path, png, &screenshot_base_dir())
}

/// [`write_png`] against an explicit base directory (tests).
pub fn write_png_in(path: &str, png: &[u8], base: &Path) -> Result<serde_json::Value, String> {
    let dest = confine_screenshot_path_in(path, base)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    std::fs::write(&dest, png).map_err(|err| format!("{}: {err}", dest.display()))?;
    Ok(serde_json::json!({ "path": dest.to_string_lossy() }))
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
    let dest = confine_screenshot_path(path)?;
    let dest_str = dest
        .to_str()
        .ok_or_else(|| "screenshot path is not utf-8".to_string())?;
    let args = screencapture_window_argv(window_id, dest_str)?;
    #[cfg(target_os = "macos")]
    {
        run_screencapture(&args, dest_str)
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
        let value = write_png_in("001-wait.png", TEST_PNG, &dir).unwrap();
        let dest = dir.join("001-wait.png");
        assert_eq!(value["path"], dest.to_str().unwrap());
        assert_eq!(std::fs::read(&dest).unwrap(), TEST_PNG);
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
        let base = std::env::temp_dir().join(format!("gpui-agent-argv-{}", std::process::id()));
        let dest = confine_screenshot_path_in("todo.png", &base).unwrap();
        let args = screencapture_window_argv(4242, dest.to_str().unwrap()).unwrap();
        assert_eq!(args[0], "-l4242");
        assert!(args.contains(&"-o".to_string()));
        assert!(args.contains(&"-x".to_string()));
        assert_eq!(args.last().unwrap(), dest.to_str().unwrap());
        assert!(
            !args.last().unwrap().starts_with('-'),
            "confined dest must not look like a flag"
        );
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
    fn test_png_is_mock_helper_not_a_live_window_capture() {
        // TEST_PNG is for tests/mock hosts (`write_png`). Live Mac capture
        // is `screencapture -l` (TRY_ON_MAC §8) and is not proven here.
        assert!(TEST_PNG.starts_with(b"\x89PNG"));
        assert_eq!(TEST_PNG.len(), 67, "1×1 fixture, not a window grab");
        assert_ne!(SCREENSHOT_BACKEND_SCREENCAPTURE, "test_png");
    }

    /// Mock/desktop helper: keep a real PNG that some other path already wrote.
    /// This is not proof that macOS `screencapture -l` ran.
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

    /// Linux/Windows CI must not claim a Mac window PNG. Gated so a Mac
    /// `cargo test` cannot skip-pass this by returning early.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn capture_without_macos_does_not_invent_a_file() {
        let name = format!("gpui-agent-no-screencapture-{}.png", std::process::id());
        let dest = screenshot_base_dir().join(&name);
        let _ = std::fs::remove_file(&dest);
        let err = capture_window_via_screencapture(7, Some(&name)).unwrap_err();
        assert!(is_screenshot_unavailable(&err), "{err}");
        assert!(err.contains("macOS-only"), "{err}");
        assert!(
            !err.to_ascii_lowercase().contains("backend"),
            "must not claim a screencapture PNG on this OS: {err}"
        );
        assert!(!dest.exists(), "must not invent {}", dest.display());
    }

    #[test]
    fn screenshot_rejects_absolute_path() {
        let err = confine_screenshot_path("/tmp/evil.png").unwrap_err();
        assert!(
            err.contains("relative"),
            "absolute paths must be rejected: {err}"
        );
    }

    #[test]
    fn screenshot_rejects_dotdot() {
        let base =
            std::env::temp_dir().join(format!("gpui-agent-confine-dot-{}", std::process::id()));
        let err = confine_screenshot_path_in("../etc/passwd.png", &base).unwrap_err();
        assert!(err.contains("..") || err.contains("relative"), "{err}");
    }

    #[test]
    fn screenshot_rejects_non_png_extension() {
        let base =
            std::env::temp_dir().join(format!("gpui-agent-confine-ext-{}", std::process::id()));
        let err = confine_screenshot_path_in("notes.txt", &base).unwrap_err();
        assert!(err.contains(".png"), "{err}");
    }

    #[test]
    fn screenshot_relative_png_writes_under_base() {
        let base =
            std::env::temp_dir().join(format!("gpui-agent-confine-ok-{}", std::process::id()));
        let dest = confine_screenshot_path_in("shot.png", &base).unwrap();
        assert!(dest.starts_with(&base), "{}", dest.display());
        assert_eq!(dest.file_name().unwrap(), "shot.png");
        let value = write_png_in("shot.png", TEST_PNG, &base).unwrap();
        assert_eq!(value["path"], dest.to_str().unwrap());
        assert_eq!(std::fs::read(&dest).unwrap(), TEST_PNG);
        let _ = std::fs::remove_dir_all(&base);
    }
}
