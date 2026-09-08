# Step screenshots + optional recipe recording

**Step PNGs are the reliable path for AI validation between recipe
steps.** Video is nice-to-have. This is observe-only: no OS HID.

Laptop try: [TRY_ON_MAC.md](TRY_ON_MAC.md#8-step-screenshots-for-ai).
Threat model: [SECURITY.md](SECURITY.md#recipes-experimental).

## How an AI uses this

1. Run a recipe (or a few manual protocol ops).
2. After a step — or between manual ops — look at the **app-only PNG**
   named on the receipt (`001-wait.png`, `002-add.png`, …).
3. Decide the next click / assert from **that frame plus the semantic
   snapshot**, not a full-desktop grab.
4. Treat `screenshot_unavailable` as honest: headless has no pixels.
   Do not invent a PNG. CI still greens on the receipt.

```bash
# PRIMARY — every step (or flagged steps) writes an intended path
gpui-agent recipe run examples/recipes/todo-crud.json \
  --set title="Buy milk" \
  --screenshot-dir artifacts/steps/

# one-shot between manual steps (same protocol op)
gpui-agent screenshot --out artifacts/steps/mid.png
```

The host writes the file on the **same machine** so the image does not
ride the 1 MiB NDJSON line. Receipts list `{ path, ok, error }` for
AI/CI. Headless lists `screenshot_unavailable` and creates **no** file.

## Answer (feasibility)

| Approach | App-only? | CI? | Status here |
| --- | --- | --- | --- |
| **Protocol `screenshot`** + `--screenshot-dir` | Yes (host surface) | **Receipt + optional PNG.** Headless is honest `screenshot_unavailable` | **Shipped** (PRIMARY) |
| **One-shot CLI / MCP `screenshot`** | Yes | Same honesty | **Shipped** |
| **Semantic frames** (tree → SVG + PPM) | N/A (not pixels) | Yes — text-diff SVG | **Shipped** (`--record`, SECONDARY) |
| **macOS window PNG** (`screencapture -l`) | **Yes** | No (display + Screen Recording) | Scripts `screenshot-window.sh` / `record-window.sh` |
| ScreenCaptureKit / AVFoundation | Yes | No | Not vendored |
| GPUI swapchain / offscreen PNG | Best for CI pixels | Needs a GPUI export API we do not have in 0.6 | Host returns unavailable until then |
| Full-desktop capture | No — leaks other apps | No | **Out of scope** |

**CI/CD recommendation**

1. **Primary gate:** `cargo test -p gpui-agent -p todo-core -p gpui-agent-cli -p gpui-agent-recipe` and `gpui-agent recipe run` against `todo-headless`. Assert `receipt.ok`.
2. **Visual gate:** receipts. Optional PNGs when a host can write them. On headless, `screenshots[].ok == false` and `error` starts with `screenshot_unavailable` — that is success for the honesty check, not a fake frame.
3. **Optional movie:** `--record artifacts/run` (semantic SVG/PPM) or a Mac `record-window.sh` mp4. Never a green-build requirement.
4. **Never** require a laptop display, Screen Recording permission, or Xvfb for green.

## Step screenshots (PRIMARY)

Wire-up:

| Mechanism | Behavior |
| --- | --- |
| `recipe run --screenshot-dir DIR` | After **every** step, `screenshot` with `DIR/NNN-id.png` (1-based, not `_start`) |
| `--screenshot-flagged` | Only steps with JSON `"screenshot": true` or wants `--screenshot` |
| Recipe step `"op": "screenshot", "path": "…"` | A real step; fails if the host cannot write |
| `gpui-agent screenshot --out FILE.png` | One-shot mid-flight check |
| MCP tool `screenshot` `{ "path": "…" }` | Same op |

Names are deterministic: `001-wait.png`, `002-add.png`. The receipt
repeats those paths on `steps[].screenshot` and top-level `screenshots`.

JSON:

```json
{ "id": "add", "op": "click", "target": "todo-add", "screenshot": true }
```

Wants:

```
wait --screenshot
click --screenshot todo-add
screenshot --out artifacts/steps/manual.png
```

Headless / current desktop GPUI: the host returns
`screenshot_unavailable: …` and **does not write a file**. A mock host
in tests may write `TEST_PNG` (1×1) — that is not a stand-in in
production.

When a host *can* export the app surface (later GPUI offscreen, or
Mac `screencapture -l` via `scripts/screenshot-window.sh`), the same
paths become real PNGs. Prefer in-app/offscreen over a desktop grab.

## Screenrecord (SECONDARY)

Optional `--record out.mp4` (or a frames directory) from start → finish
when you want a movie. Default backend is **semantic** (headless-safe
SVG + PPM). `--record-backend os` is a stub that points at
`scripts/record-window.sh`. Mux is optional ffmpeg; CI should not gate
on the mp4.

```bash
gpui-agent recipe run examples/recipes/todo-crud.json \
  --set title="Buy milk" \
  --record artifacts/recipe-run

# video filename → frames in artifacts/recipe-run.mp4.frames
gpui-agent recipe run … --record artifacts/recipe-run.mp4

gpui-agent recipe run … --record artifacts/run --record-values
```

`--record` writes `recording.flag`, snapshots the tree after `_start`
and after each step, then `manifest.json`. Values are `«redacted»`
unless `--record-values`. `role=password` is always redacted.

```bash
./scripts/mux-record-frames.sh artifacts/recipe-run
```

## macOS window-only pixels

Needs a display, desktop `todo` (title **Agent Todo**), and Screen
Recording permission. Observe-only.

```bash
# one PNG between manual steps (when protocol screenshot is unavailable)
./scripts/screenshot-window.sh --out artifacts/steps/manual.png --title "Agent Todo"

# continuous window PNGs / optional mp4 while a recipe runs
./scripts/record-window.sh --out artifacts/recipe-run --title "Agent Todo"
```

`screencapture -l` is **that window only**. The scripts do not click or
type. Linux: they exit with a pointer at `--screenshot-dir` /
`--record-backend semantic`.

## Security

- Screenshots and recording do **not** bypass loopback, token,
  line/conn/mailbox, or `--yes`. Extra `screenshot` / `snapshot` RPCs
  are normal `Request`s.
- **Do not put secrets in frames.** Tokens must not be painted on the
  window. Default semantic `--record` redacts `value`; password roles
  stay redacted. App-surface PNGs can still show whatever is on screen
  (typed secrets included).
- Do not commit `artifacts/`.
- Full-desktop capture is out of scope (other apps, notifications).

## Tests

Receipt lists screenshot paths; the unavailable path is honest (error
prefix, no file); `--screenshot-dir` / `--screenshot-flagged` clap and
JSON `screenshot: true` are covered even when frames are mocked (`TEST_PNG`)
on CI. Semantic `--record` start/stop stays in `gpui-agent-recipe`.

## Ask later

1. In-app GPUI offscreen PNG once 0.6 (or a later kit) exports a frame.
2. In-process ScreenCaptureKit (entitlements, not a crate default).
3. MCP `recipe_run` `record` path (stdio + local files is awkward).
4. Linux `ffmpeg` + `_NET_WM` window crop behind a documented opt-in.
