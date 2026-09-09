# Run experimental recipes on your Mac / laptop

Coding stays on the cloud agent. Local is **pull + run only**.

Typical checkout: `/Volumes/goldcoders/OSS/gpui-agent`.

This verifies **P3** (macOS window PNG via `screencapture -l`; headless
stays honest) on current `main` plus this PR. P0–P2 (session reuse,
recipes, token-for-recipe/MCP) are already on `main`. **Headless is
enough** to check recipes and unavailable screenshots. The desktop
`todo` window needs a display **and Screen Recording** if you want a
real PNG.

Format, threat model, and `--yes` / session-reuse notes:
[RECIPES.md](RECIPES.md). Caps that recipes must not bypass:
[SECURITY.md](SECURITY.md#recipes-experimental).

## 1. Fetch the PR branch (do not merge)

```bash
cd /Volumes/goldcoders/OSS/gpui-agent
git fetch origin
git checkout gol/no-brainer-p3-screenshot-b20f
git pull origin gol/no-brainer-p3-screenshot-b20f
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

Confirm experimental labeling and the token note:

```bash
./target/debug/gpui-agent --help | grep -i experimental
./target/debug/gpui-agent --help | grep -i token
./target/debug/gpui-agent recipe --help
```

## 3. Terminal 1 — start the host

Default bind is **`127.0.0.1:17421`**. Automation is off unless
`GPUI_AGENT=1`.

**Recipe / MCP workflows must set the same token on host and client.**
One-off `click` / `snapshot` / `hello` still work without a token if
the host has none. This laptop path uses a token because you are about
to `recipe run`.

```bash
cd /Volumes/goldcoders/OSS/gpui-agent
export GPUI_AGENT=1
export GPUI_AGENT_ADDR=127.0.0.1:17421
export GPUI_AGENT_TOKEN=dev-secret
./target/debug/todo-headless
```

You should see:

```text
gpui-agent listening on 127.0.0.1:17421 (platform=headless, app=todo)
opt-in: GPUI_AGENT=1 · loopback only · protocol v1
auth: required (GPUI_AGENT_TOKEN set; recipe/MCP clients must send the same token)
```

**Desktop instead** (needs a real display; not required to verify recipes):

```bash
export GPUI_AGENT=1
export GPUI_AGENT_ADDR=127.0.0.1:17421
export GPUI_AGENT_TOKEN=dev-secret
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
export GPUI_AGENT_TOKEN=dev-secret
CLI=./target/debug/gpui-agent
```

`validate` / `plan` / `resolve` do **not** need a host (and do not need
a token). JSON is the documented path; `.wants` is an alias:

```bash
$CLI recipe validate examples/recipes/todo-crud.json
$CLI recipe validate examples/recipes/todo-crud.wants
$CLI recipe plan examples/recipes/todo-crud.json --set title="Buy milk"
$CLI recipe plan examples/recipes/todo-crud.wants --set title="Buy milk" --order-check
$CLI recipe resolve 'add a todo titled Buy milk'
```

`run` needs the host **and** a non-empty token. Both sample recipes
assume an **empty** todo list (they hard-code `todo-item-1` / `id=1`).
Use a freshly started host.

```bash
$CLI recipe run examples/recipes/todo-crud.json --set title="Buy milk"
```

### Success looks like this

Expect `"ok": true` and `"session_reused": true`:

```json
{
  "ok": true,
  "recipe": "todo-crud",
  "fingerprint": "…",
  "session_reused": true,
  "steps": [
    { "id": "wait", "ok": true },
    { "id": "add", "ok": true, "result": { "done": false, "id": 1, "title": "Buy milk" } },
    { "id": "seen", "ok": true },
    { "id": "toggle", "ok": true, "result": { "done": true, "id": 1, "title": "Buy milk" } },
    { "id": "done", "ok": true }
  ]
}
```

`fingerprint` is a process-local hash (`DefaultHasher`); your Mac may
print a different hex and that is fine.

To run the `.wants` file too, **restart the host** first (same ids):

```bash
$CLI recipe run examples/recipes/todo-crud.wants --set title="Buy milk"
```

That receipt should also be `"ok": true` and `"session_reused": true`
(step ids will be `s1`…`s5` instead of `wait`/`add`/…).

## 5. Optional: old per-click CLI vs one recipe

Same host, after a **fresh** start (empty list). These one-off commands
do **not** require a client token *unless the host has one* — here the
host does, so keep `GPUI_AGENT_TOKEN=dev-secret` exported:

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

`hello` includes `"auth": "required"` when the host has a token:

```bash
$CLI hello
# "auth": "required"
```

## 6. Shut down

```bash
$CLI shutdown
```

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
# expect: pass --yes  (still needs a token; fails on --yes first if token is set)

# non-loopback refused (token would not leave the machine)
$CLI --addr 8.8.8.8:17421 hello
# expect: refusing non-loopback agent address
```

`recipe run` and `mcp` **always** fail without a token (P2), even if
the host is untokened:

```bash
env -u GPUI_AGENT_TOKEN $CLI recipe run examples/recipes/todo-crud.json --set title="Milk"
# expect: recipe run and mcp require a non-empty GPUI_AGENT_TOKEN or --token

env -u GPUI_AGENT_TOKEN $CLI mcp </dev/null
# expect: same error, nonzero (does not hang on stdin)
```

## If something fails

| Symptom | Fix |
| --- | --- |
| `automation is disabled` | Host was started without `GPUI_AGENT=1` |
| `connect … failed` | Host not up, or `GPUI_AGENT_ADDR` differs between terminals |
| `recipe run and mcp require a non-empty GPUI_AGENT_TOKEN` | Export `GPUI_AGENT_TOKEN` (or pass `--token`) in the **client** terminal |
| `automation token required` / `invalid automation token` | Export the **same** `GPUI_AGENT_TOKEN` in both terminals |
| `node \`todo-item-1\` exists` / name mismatch | Host still has todos from a previous run — `shutdown` and start a fresh host |
| `unknown invoke` | Recipe used a name not in the local schema (only demo `todo.*` + protocol ops) |
| Desktop window won't start | Expected on a display-less session — use `todo-headless` |
| `screenshot_unavailable` … Screen Recording | Grant Screen Recording to the terminal/`todo`, then retry. Linux/Windows desktop is Mac-only for real PNG. |
| Recipe syntax questions | [RECIPES.md](RECIPES.md) |

## Tests (optional on the laptop)

```bash
cargo test -p gpui-agent -p todo-core -p gpui-agent-cli -p gpui-agent-recipe
```

That suite includes the recipe parse / resolve / run / session-reuse
edge cases, CLI fail-fast without a token, and step-screenshot receipt
plumbing. It does not need a display.

## 8. Step screenshots

**Headless stays honest.** The receipt lists intended paths; no fake
PNG is written. Keep `GPUI_AGENT_TOKEN` exported.

```bash
mkdir -p artifacts/steps
$CLI recipe run examples/recipes/todo-crud.json --set title="Buy milk" \
  --screenshot-dir artifacts/steps
```

On `todo-headless` expect `"ok": true` and `screenshots[]` with
`001-wait.png` … `error` starting with `screenshot_unavailable` and
**no** PNG files. That honesty is correct. Do not treat missing files
as a CI failure.

One-shot between **manual** clicks (same protocol):

```bash
$CLI screenshot --out artifacts/steps/mid.png
# headless: error screenshot_unavailable (no fake file)
```

### macOS desktop (real PNG of this window)

Needs a display. Grant **Screen Recording** to the terminal (or the
`todo` binary) in System Settings → Privacy & Security. First capture
can show a permission dialog; grant it, then retry. This is
observe-only (`screencapture -l` of the Agent Todo window). It does
not warp the cursor and does not grab the full desktop.

```bash
# terminal 1
export GPUI_AGENT=1
export GPUI_AGENT_ADDR=127.0.0.1:17421
export GPUI_AGENT_TOKEN=dev-secret
./target/debug/todo

# terminal 2 (same exports)
mkdir -p artifacts/steps
$CLI screenshot --out artifacts/steps/mid.png
# success: {"ok": true, "result": {"path": "…", "backend": "screencapture"}}
# open artifacts/steps/mid.png — should be the Agent Todo window only
```

If you see `screenshot_unavailable` mentioning Screen Recording, the
grant did not stick — that is still correct (no invented pixels).
Linux/Windows desktop is Mac-only for real PNG in P3; same unavailable
error. See [RECORDING.md](RECORDING.md). `--record` is not in this PR.
