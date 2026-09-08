# Run this PR on your Mac / laptop

Coding stays on the cloud agent. Local is **pull + run only**.

Typical checkout: `/Volumes/goldcoders/OSS/gpui-agent`.

This verifies the experimental recipes on PR #4 (`gol/recipes-tmp-perf-e79b`).
Do **not** merge the PR from the laptop. **Headless is enough.** The desktop
`todo` window needs a display; skip it unless you want to watch the same
protocol drive a GPUI window.

Format, threat model, and `--yes` / session-reuse notes:
[RECIPES.md](RECIPES.md). Caps that recipes must not bypass:
[SECURITY.md](SECURITY.md#recipes-experimental). Recording (CI frames vs
Mac window): [RECORDING.md](RECORDING.md).

## 1. Fetch the PR branch (do not merge)

```bash
cd /Volumes/goldcoders/OSS/gpui-agent
git fetch origin
git checkout gol/recipes-tmp-perf-e79b
git pull origin gol/recipes-tmp-perf-e79b
```

Or:

```bash
cd /Volumes/goldcoders/OSS/gpui-agent
gh pr checkout 4
```

`rust-toolchain.toml` pins **1.98.1**. First `cargo` on this branch may
download that toolchain. You do not need to install GPUI system libs on
macOS for **headless** (no window).

## 2. Build once

```bash
cd /Volumes/goldcoders/OSS/gpui-agent
cargo build -p gpui-agent-cli -p todo -p todo-headless
```

Binaries:

- `target/debug/gpui-agent` — CLI
- `target/debug/todo-headless` — no window (use this)
- `target/debug/todo` — GPUI window (display required)

## 3. Terminal 1 — start the host

Default bind is **`127.0.0.1:17421`**. Automation is off unless
`GPUI_AGENT=1`.

```bash
cd /Volumes/goldcoders/OSS/gpui-agent
export GPUI_AGENT=1
export GPUI_AGENT_ADDR=127.0.0.1:17421
# optional, recommended on a shared machine:
# export GPUI_AGENT_TOKEN=dev-secret
./target/debug/todo-headless
```

You should see:

```text
gpui-agent listening on 127.0.0.1:17421 (platform=headless, app=todo)
opt-in: GPUI_AGENT=1 · loopback only · protocol v1
```

**Desktop instead** (needs a real display; not required to verify recipes):

```bash
export GPUI_AGENT=1
export GPUI_AGENT_ADDR=127.0.0.1:17421
# export GPUI_AGENT_TOKEN=dev-secret   # same value in both terminals
./target/debug/todo
```

If the port is already taken (`Address already in use`):

```bash
lsof -iTCP:17421 -sTCP:LISTEN
# shut down the old host, or pick another loopback port in BOTH terminals:
# export GPUI_AGENT_ADDR=127.0.0.1:17422
```

## 4. Terminal 2 — recipes (same repo, same addr, same token)

```bash
cd /Volumes/goldcoders/OSS/gpui-agent
export GPUI_AGENT_ADDR=127.0.0.1:17421
# export GPUI_AGENT_TOKEN=dev-secret   # required if terminal 1 set it
CLI=./target/debug/gpui-agent
```

`validate` / `plan` / `resolve` do **not** need a host (and do not open
a socket):

```bash
$CLI recipe validate examples/recipes/todo-crud.json
$CLI recipe validate examples/recipes/todo-crud.wants
$CLI recipe plan examples/recipes/todo-crud.json --set title="Buy milk"
$CLI recipe plan examples/recipes/todo-crud.wants --set title="Buy milk" --order-check
$CLI recipe resolve 'add a todo titled Buy milk'
```

`run` needs the host. Both sample recipes assume an **empty** todo list
(they hard-code `todo-item-1` / `id=1`). Use a freshly started host. If
terminal 1 set `GPUI_AGENT_TOKEN`, export the **same** value here (or
pass `--token`).

```bash
$CLI recipe run examples/recipes/todo-crud.json --set title="Buy milk"
```

### Success looks like this

Cloud verification on this branch (headless, 2026-09-08):

```json
{
  "ok": true,
  "recipe": "todo-crud",
  "fingerprint": "227ccedf49740b90",
  "session_reused": true,
  "steps": [
    { "id": "wait", "ok": true, "elapsed_ms": 7 },
    { "id": "add", "ok": true, "elapsed_ms": 0, "result": { "done": false, "id": 1, "title": "Buy milk" } },
    { "id": "seen", "ok": true, "elapsed_ms": 0 },
    { "id": "toggle", "ok": true, "elapsed_ms": 0, "result": { "done": true, "id": 1, "title": "Buy milk" } },
    { "id": "done", "ok": true, "elapsed_ms": 0 }
  ],
  "elapsed_ms": 7
}
```

Check: `"ok": true`, `"session_reused": true`, five steps, last assert
passes, `add` / `toggle` results show id `1`. `fingerprint` is a
process-local hash (`DefaultHasher`); your Mac may print a different
hex and that is fine.

To run the `.wants` file too, **restart the host** first (same ids):

```bash
# terminal 1: Ctrl-C the old host (or shutdown from terminal 2), then start it again
$CLI recipe run examples/recipes/todo-crud.wants --set title="Buy milk"
```

That receipt should also be `"ok": true` and `"session_reused": true`
(step ids will be `s1`…`s5` instead of `wait`/`add`/…).

## 5. Optional: old per-click CLI vs one recipe

Same host, after a **fresh** start (empty list):

```bash
$CLI wait
$CLI set-value todo-input "Buy milk"
$CLI click todo-add
$CLI assert --id todo-item-1 --name "Buy milk" --checked false
$CLI click todo-toggle-1
$CLI assert --id todo-item-1 --checked true
```

That is six process spawns. The recipe above is the same CRUD in **one**
process and **one** TCP session.

## 6. Shut down

```bash
$CLI shutdown
```

If you set `GPUI_AGENT_TOKEN`, pass `--token` or export the same var.
A recipe that includes `shutdown` will refuse unless you also pass
`--yes`.

Leave terminal 1 with Ctrl-C only if `shutdown` already exited the host.

## 7. Security smoke (optional)

These should **fail** (nonzero exit). They are safe to run: none of them
maps onto a shell.

```bash
# unknown invoke — fail closed, no host needed
printf '%s\n' '{"name":"bad","steps":[{"id":"x","op":"invoke","name":"shell.run","args":{}}]}' \
  | $CLI recipe validate -

# resolve never shells out
$CLI recipe resolve 'rm -rf /'
# expect: unknown intent (fail closed)

# shutdown in a recipe without --yes (does not contact the host)
printf '%s\n' $'hello\nshutdown' | $CLI recipe run -
# expect: pass --yes

# token required when the host has GPUI_AGENT_TOKEN set
# (run from a shell that does NOT export the token)
env -u GPUI_AGENT_TOKEN $CLI recipe run examples/recipes/todo-crud.json --set title="Milk"
# expect: "automation token required"

# non-loopback refused (token would not leave the machine)
$CLI --addr 8.8.8.8:17421 hello
# expect: refusing non-loopback agent address
```

## If something fails

| Symptom | Fix |
| --- | --- |
| `automation is disabled` | Host was started without `GPUI_AGENT=1` |
| `connect … failed` | Host not up, or `GPUI_AGENT_ADDR` differs between terminals |
| `automation token required` / `invalid automation token` | Export the same `GPUI_AGENT_TOKEN` in both terminals, or pass `--token` |
| `node \`todo-item-1\` exists` / name mismatch | Host still has todos from a previous run — `shutdown` and start a fresh host |
| `unknown invoke` | Recipe used a name not in the local schema (only demo `todo.*` + protocol ops) |
| Desktop window won't start | Expected on a display-less session — use `todo-headless` |
| Recipe syntax questions | [RECIPES.md](RECIPES.md) |

## Tests (optional on the laptop)

```bash
cargo test -p gpui-agent -p todo-core -p gpui-agent-cli -p gpui-agent-recipe
```

That suite includes the recipe parse / resolve / run / session-reuse
edge cases and semantic `--record` start/stop. It does not need a display.

## 8. Optional: record a recipe

**CI / this laptop without watching the window** — semantic frames (no
GPU). Values are redacted unless you pass `--record-values`.

```bash
mkdir -p artifacts
$CLI recipe run examples/recipes/todo-crud.json --set title="Buy milk" \
  --record artifacts/recipe-run
ls artifacts/recipe-run
# manifest.json  0000-_start.svg  0000-_start.ppm  …
```

Expect the same `"ok": true` receipt. `recording.flag` should be gone
when the run finishes. Mux only if you have ffmpeg:

```bash
./scripts/mux-record-frames.sh artifacts/recipe-run
```

`--record-backend os` is a stub and should **error** (use the script
below for real pixels).

**Desktop window only (display + Screen Recording permission):**

```bash
# terminal 1
GPUI_AGENT=1 ./target/debug/todo

# terminal 2
./scripts/record-window.sh --out artifacts/recipe-run --title "Agent Todo"

# terminal 3 (fresh empty list)
$CLI recipe run examples/recipes/todo-crud.json --set title="Buy milk" \
  --record artifacts/recipe-run
```

`screencapture -l` is that window only — not the full desktop, and it
does not click or type. Headless is enough to verify recipes; this is
a demo artifact.
