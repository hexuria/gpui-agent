# Recording and screenshots

P3 is **still PNG**, not video. `--record`, ffmpeg, and a ScreenCaptureKit
crate are **out of scope**. This file is the short visual note so PROTOCOL
does not grow a media appendix.

## What agents get today

| Host | `screenshot` / `--screenshot-dir` |
| --- | --- |
| `todo-headless` (daemon) | `screenshot_unavailable`, **no file** |
| Desktop `todo` (default, daemon client) | Same honesty. The GUI does not host the agent port, so it cannot serve a PNG. |
| Desktop `todo --features embedded-host` on Linux / Windows | Same honesty. GPUI `Window::render_to_image` exists only under `test-support` on this gpui-kit pin; this repo does not enable that in production and does **not** capture the full desktop. |
| Desktop `todo --features embedded-host` on macOS | PNG of **this app window** via `screencapture -l <CGWindowID> -o -x <path>`. Needs **Screen Recording** for the terminal (or the `todo` binary). Failure is `screenshot_unavailable`, not a fake `TEST_PNG`. |

The image stays on disk. It does not ride the 1 MiB NDJSON line.

`TEST_PNG` is a 1×1 fixture for tests and mock hosts only.

## What this is not

- Not OS HID. Capture does not warp the cursor (`-C` is not passed).
- Not interactive `screencapture -i` / `-w` (that would be a picker).
- Not a full-desktop grab (`screencapture` without `-l`, or `-S`).
- Not ScreenCaptureKit / `zed-scap` (ask before adding that crate or entitlements).
- Not `--record` / ffmpeg / SVG+PPM semantic frames.

## Secrets in frames

Do not put tokens, passwords, or CI secrets in the painted window.
`snapshot` already returns field `value`s (SECURITY M3); a PNG is the
same class of local observation. See [SECURITY.md](SECURITY.md).

Laptop steps: [TRY_ON_MAC.md](TRY_ON_MAC.md#8-step-screenshots).
