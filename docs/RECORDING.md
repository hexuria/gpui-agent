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

## Scrolled screenshots (`mode=scrolled`)

Default `screenshot` stays **viewport** (this window). Opt-in tall capture:

```text
screenshot
  path?: relative.png
  mode?: "viewport" | "scrolled"   # default viewport
  target?: scroll-view id          # required when mode=scrolled
  max_height_px?: number           # default and max 16384
```

The **host** does the work with a semantic scroll API (no OS HID, no
virtual wheel):

1. Resolve `target` to a scroll container
2. Read viewport / content metrics
3. Set offset → wait for paint → capture a tile (`screencapture -l` of
   this window, then crop to the scroller clip)
4. Stitch tiles into one PNG; restore the original offset even on error

Result metadata may include `tiles`, `target`, `content_height`,
`viewport_height`. The mode name is `scrolled` (not `full_content` /
`stitched`). Virtualized lists only include the loaded range.

Caps fail closed (clear error, not a silent truncated image): content
taller than `max_height_px`, more than 32 tiles, or a huge encoded PNG.

| Host | `mode=scrolled` |
| --- | --- |
| Headless / daemon / Linux / Windows | `screenshot_unavailable`, **no file** |
| macOS embedded-host | Stitched PNG of `target` (todo demo: `todo-list-scroll`) |
| Unknown / unscrollable `target` | `scroll_unavailable` (do not invent tiles) |

Print / multi-page documents: prefer an app exporter (`form.pdf` /
frozen HTML) for content identity. Scrolled PNG is for chrome + layout.
Offscreen GPUI `render_to_image` is **out of MVP** (test-support only
on this pin). Apps expose their own scroll-view ids; bir is not
changed here.
