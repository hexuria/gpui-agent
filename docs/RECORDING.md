# Experimental window / recipe recording

**Partial yes.** You can record a recipe from start → finish. True
**window-scoped pixels** are a Mac helper script (observe-only). CI
should stay **headless**: receipts first, optional semantic SVG/PPM
frames, never a human desktop for green.

This sits on the experimental recipes + session-reuse work. It does
**not** steal the OS pointer or keyboard. Recording is opt-in observe
only (`--record`). Semantic delivery stays the default.

Laptop try: [TRY_ON_MAC.md](TRY_ON_MAC.md#8-optional-record-a-recipe).
Threat model: [SECURITY.md](SECURITY.md#recipes-experimental).

## Answer (feasibility)

| Approach | Window-only? | CI? | Status here |
| --- | --- | --- | --- |
| **Semantic frames** (tree → SVG + PPM after each step) | N/A (not pixels) | **Yes** — no display, deterministic, text-diff SVG | **Shipped** (`--record`) |
| **macOS window capture** (`screencapture -l` CGWindowID) | **Yes** | No (needs a real display + Screen Recording permission) | **Script** `scripts/record-window.sh` |
| ScreenCaptureKit / AVFoundation | Yes (more code, entitlements) | No | Not vendored |
| Linux Xvfb + ffmpeg crop | Crop, not a real window id | Fragile GPU/Xvfb | Documented only |
| GPUI swapchain / offscreen PNG | Best for CI pixels | Needs GPU + a GPUI export API we do not have in 0.6 | **Ask later** |
| Full-desktop capture | No — leaks other apps | No | **Out of scope** |

**CI/CD recommendation**

1. **Primary gate:** `cargo test -p gpui-agent -p todo-core -p gpui-agent-cli -p gpui-agent-recipe` and `gpui-agent recipe run` against `todo-headless`. Assert `receipt.ok` (and optionally fingerprint in-process).
2. **Optional artifact:** `recipe run --record artifacts/run` on the same headless host. Upload `manifest.json` + `*.svg` (CI-diffable). Mux `*.ppm` with `ffmpeg` only if you want a movie.
3. **Never** require a laptop display, Screen Recording permission, or Xvfb for green.
4. **Human demo / flaky-pixel debug:** Mac desktop `todo` + `scripts/record-window.sh` → `recipe-run.mp4`. Treat that as a review artifact, not a gate.

Why not in-app GPUI frames today? The protocol has no `screenshot` op
yet; headless bounds are zero; published `gpui-kit 0.6` does not give
this lab a small offscreen swapchain dump without pulling GPU + extra
crates. Semantic frames reuse the snapshot we already trust.

## CLI

```bash
# works on this cloud VM / Linux CI (no window)
gpui-agent recipe run examples/recipes/todo-crud.json \
  --set title="Buy milk" \
  --record artifacts/recipe-run

# video filename → frames in artifacts/recipe-run.mp4.frames
gpui-agent recipe run examples/recipes/todo-crud.json \
  --set title="Buy milk" \
  --record artifacts/recipe-run.mp4

# include snapshot field values (off by default; still redacts role=password)
gpui-agent recipe run … --record artifacts/run --record-values

# stub: in-process OS capture is not implemented
gpui-agent recipe run … --record artifacts/run --record-backend os
# → error pointing at scripts/record-window.sh
```

`--record` starts on plan start (writes `recording.flag`), snapshots the
tree after `_start` and after **each** step on the reused TCP session
(still token + caps), and writes `manifest.json` on success **or**
failure (partial frames). Later recipe steps that did not run produce
no frames.

`--record-backend os` always errors in this CLI on purpose: we will not
bundle ScreenCaptureKit or spawn ffmpeg from the generic protocol
binary. Use the script.

## Semantic frames (headless)

Each frame:

| File | What |
| --- | --- |
| `NNNN-step.svg` | Labeled tree (id / role / name). CI can diff as text. |
| `NNNN-step.ppm` | Color strip (mux with ffmpeg; no extra crate). |
| `manifest.json` | Recipe name, fingerprint, frame list, mux hint |

Values are **`«redacted»`** unless `--record-values`. `role=password` is
always redacted. Artifacts stay on the local disk you passed.

```bash
./scripts/mux-record-frames.sh artifacts/recipe-run
# needs ffmpeg; otherwise keep the SVGs
```

## macOS window-only pixels

Requires a display, desktop `todo` (title **Agent Todo**), and Screen
Recording permission for the terminal.

```bash
# terminal 1
GPUI_AGENT=1 ./target/debug/todo

# terminal 2 — window PNGs, optional mp4
./scripts/record-window.sh --out artifacts/recipe-run --title "Agent Todo"

# terminal 3 — same dir so the script sees recording.flag
./target/debug/gpui-agent recipe run examples/recipes/todo-crud.json \
  --set title="Buy milk" --record artifacts/recipe-run
```

`screencapture -l <windowid>` captures **that window only**. The script
does not click or type. It stops when `recording.flag` is removed
(recipe finish) or `--seconds` elapses.

Linux: the script exits with a pointer at `--record-backend semantic`.
Windows: later (not this slice).

## Security

- Recording does **not** bypass loopback, token, line/conn/mailbox, or
  `--yes`. Extra snapshots are normal `Request`s.
- Do not enable `--record-values` in CI if drafts / passwords land on
  the tree (M3). Default redaction is the safe path.
- Do not commit `artifacts/`. Tokens must not be drawn on the window
  chrome.
- OS capture can still show whatever is **painted in that window**
  (including typed secrets). Prefer semantic frames in CI.

## Tests

Recorder start/stop, redaction, `--record` clap plumbing, and a
headless recipe that stops mid-assert (partial frames) run in
`gpui-agent-recipe` / `gpui-agent-cli`. They do not need a GPU.

## Ask later

1. Protocol `screenshot` once desktop GPUI can export a frame.
2. In-process ScreenCaptureKit (entitlements, not a crate default).
3. MCP `recipe_run` `record` path (stdio + local files is awkward).
4. Linux `ffmpeg` + `_NET_WM` window crop behind a documented opt-in.
