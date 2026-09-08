//! Observe-only pixel capture for the **app surface**.
//!
//! This is not OS HID. Headless hosts (and desktop GPUI until it can
//! export a frame) return [`screenshot_unavailable`] — same honesty as
//! [`virtual_unavailable`](crate::virtual_unavailable). The host writes
//! the PNG to a local `path` so the image does not ride the 1 MiB
//! NDJSON line.

use std::path::Path;

/// Stable error prefix so agents can branch without parsing prose.
pub const SCREENSHOT_UNAVAILABLE: &str = "screenshot_unavailable";

pub fn screenshot_unavailable(detail: impl Into<String>) -> String {
    format!("{SCREENSHOT_UNAVAILABLE}: {}", detail.into())
}

pub fn is_screenshot_unavailable(error: &str) -> bool {
    error == SCREENSHOT_UNAVAILABLE
        || error.starts_with(SCREENSHOT_UNAVAILABLE)
            && error.as_bytes().get(SCREENSHOT_UNAVAILABLE.len()) == Some(&b':')
}

/// Write `png` bytes to `path`. Production hosts pass a real frame;
/// tests may pass [`TEST_PNG`]. Never invent pixels for a missing surface.
pub fn write_png(path: &str, png: &[u8]) -> Result<serde_json::Value, String> {
    if path.trim().is_empty() {
        return Err("screenshot requires path".into());
    }
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
}
