# Experimental recipes + TMP-inspired mapping

This is an **experimental** P1 slice. It does not replace semantic RPC,
does not steal the OS pointer, and does not change protocol v2.

The goal: let an agent **author or reuse a recipe** of gpui-agent ops
(`click` / `set-value` / `assert` / `invoke` / …) that compiles to a
plan and runs with **one CLI (or MCP) invocation** — one process, one
TCP session, many ops — instead of a tool round-trip per action.

**Canonical format: JSON.** Line-based `.wants` is still accepted as a
thin alias. Laptop (pull + run only): [TRY_ON_MAC.md](TRY_ON_MAC.md).
Threat model vs PR #3 caps: [SECURITY.md](SECURITY.md#recipes-experimental).
Wire ops stay one NDJSON request each: [PROTOCOL.md](PROTOCOL.md).
Roadmap: [NO_BRAINER_PLAN.md](NO_BRAINER_PLAN.md) (P0–P5 are on `main`,
including **P3** Mac window PNG [#20](https://github.com/hexuria/gpui-agent/pull/20)
and **P4** CI recipe receipt [#21](https://github.com/hexuria/gpui-agent/pull/21)).

## What was borrowed

### From [rwmcp](https://github.com/hexuria/reverse-web-mcp)

Kept the *shape*, not the world-model compiler:

| Idea | Here |
| --- | --- |
| `--wants` file: one predicate/op per line, `#` comments | `*.wants` compiles to a `Recipe` (alias) |
| A successful plan is a reusable recipe with `$params` | JSON `Recipe` + `--set key=value` |
| `validate` / `plan` / `run` / receipt | `gpui-agent recipe validate\|plan\|run` |
| Order-check (compile listed vs reversed) | `recipe plan --order-check` |
| Exit-effect gate (`--yes`) | Recipes that include `shutdown` refuse until `--yes` |
| Falsifiable receipt (what ran, ok/err, timing) | `Receipt` JSON, optional `--receipt-out` |
| One command instead of a session | Connection reuse in `AgentClient` (P0) |

Left out on purpose (non-goals):

- Full intent-graph / OpenAPI world-model compiler
- LLM planner / `--goal` / `--answer-with-model`
- Event-bus scheduler, idempotency keys, resume-from-ledger
- Parallel multi-connection execution (one socket; all-Read waves may
  pipeline lines, writes stay sequential — not a thread pool)
- Surfaces, effectors, CDP driver

### From [tmp](https://github.com/codeitlikemiley/tmp) (Tool Mapping Protocol)

Kept the *mapping contract*, not the product:

| Idea | Here |
| --- | --- |
| Schema-backed intent → verified operation | `OpSchema` + `resolve_intent` |
| Deterministic resolve / compile (no model in the core) | Local keyword scoring; fail closed |
| Effects + required inputs + result shape **before** run | Printed on `recipe plan` / `recipe resolve` |
| Allow-list; unknown intent does nothing | Unknown `invoke` names fail validate |
| Embeddable `tmp-core`-shaped types | Local `Registry` in `gpui-agent-recipe` |

Left out:

- `tmp-agent` Axum server / registry cloud / publish-install
- Help-text scraping, git/cargo resolvers, **shell `data_source.command`**
- Workflow YAML runner, TUI verifier
- A hard path-dep on `tmp-core` (its resolvers exec commands — the
  wrong default for a UI control plane that must never map onto a shell)

If `tmp-core` later exposes a no-exec schema+resolve feature, we can
feature-gate a path dependency. Until then, local schemas are the
honest slice.

## Recipe format (JSON canonical; `.wants` also accepted)

JSON is first-class because the wire protocol is already JSON, rwmcp
recipes are JSON, and an agent can emit a whole DAG in **one** model
sample. YAML would add a crate for no gain. Line-based `.wants` exists
so a human or agent can write a sequence without braces.

```json
{
  "v": 2,
  "name": "todo-crud",
  "app": "todo",
  "params": ["title"],
  "steps": [
    { "id": "wait", "op": "wait" },
    { "id": "add", "op": "invoke", "name": "todo.add", "args": { "title": "$title" }, "needs": ["wait"] },
    { "id": "seen", "op": "assert", "target": "todo-item-1", "name": "$title", "checked": false, "needs": ["add"] }
  ]
}
```

Equivalent wants file (`examples/recipes/todo-crud.wants`):

```
# bind $title with --set title="Buy milk"
wait
invoke todo.add title=$title
assert todo-item-1 name=$title checked=false
invoke todo.toggle id=1
assert todo-item-1 checked=true
```

`.json` paths are always parsed as JSON (even if the file does not start
with `{`). Anything else is `.wants`, unless the text itself starts with
`{`. Path `-` reads stdin.

Both samples assume a **fresh empty** todo list (`todo-item-1` / `id=1`).
Restart the host before a second run.

### `.wants` lines

- One protocol op per line. `#` starts a comment; blank lines are skipped.
- Quotes (`"` / `'`) keep spaces in one token. `title="Buy milk"` and
  `set-value todo-input "Buy milk"` are both valid. `\"` escapes inside
  quotes. An unterminated quote is an error.
- `click` / `type` / `key` accept `--delivery semantic|virtual`.
- `assert` accepts a target plus `name=` / `checked=` / `--absent` / …
- Unknown ops, missing required args, and bad assert fields fail parse.

## Commands (`validate` / `plan` / `run` / `resolve`)

These subcommands and the matching MCP tools are labeled
**experimental** in `--help` / tool descriptions. MCP itself requires a
token to start; `recipe_run` then sends it on every step.

| Command | Host? | Token? | What it does |
| --- | --- | --- | --- |
| `recipe validate <path>` | No | No | Parse + lint (version, ids, `needs`, declared `$params`, invoke allow-list) |
| `recipe plan <path> [--set k=v] [--order-check]` | No | No | Bind params, schedule waves, print effects / fingerprint / `requires_yes` |
| `recipe run <path> [--set k=v] [--yes] [--receipt-out FILE] [--screenshot-dir DIR]` | Yes | **Yes** (`GPUI_AGENT_TOKEN` or `--token`, same as host) | Compile, then run ops on one reused TCP session (all-Read waves may pipeline) |
| `recipe resolve '…'` | No | No | Map prose through the local schema registry (fail closed) |

MCP tools with the same jobs: `recipe_validate`, `recipe_plan`,
`recipe_run` (pass `yes: true` for shutdown), `recipe_resolve`. They
are batching helpers, not app-specific verbs. Per-op tools stay for
interactive debugging.

```bash
# both terminals — recipe run / mcp require this (same value as the host)
export GPUI_AGENT_TOKEN=dev-secret

# no host
gpui-agent recipe validate examples/recipes/todo-crud.json \
  --schema examples/schemas/todo.json
gpui-agent recipe plan examples/recipes/todo-crud.json \
  --schema examples/schemas/todo.json --set title="Buy milk"
gpui-agent recipe resolve 'add a todo titled Buy milk' \
  --schema examples/schemas/todo.json

# host required — JSON is the documented path
gpui-agent recipe run examples/recipes/todo-crud.json \
  --schema examples/schemas/todo.json --set title="Buy milk"

# AI mid-run: intended PNGs after every step
# headless / daemon / default GUI client / Linux / Windows: receipt lists screenshot_unavailable (no files)
# macOS embedded-host todo: real PNG of this window (Screen Recording)
gpui-agent recipe run examples/recipes/todo-crud.json \
  --schema examples/schemas/todo.json --set title="Buy milk" \
  --screenshot-dir artifacts/steps/
```

Between **manual** steps, the same observe-only op:

```bash
gpui-agent screenshot --out artifacts/steps/mid.png
```

JSON steps may set `"screenshot": true`; wants lines may take
`--screenshot`. With `--screenshot-flagged`, only those steps are
captured. Names are `001-wait.png`, `002-add.png`, … and appear on the
receipt. Headless, the daemon, the default GUI-as-daemon-client, and
Linux/Windows desktop GPUI return `screenshot_unavailable` and **do not
invent a file**. macOS `todo --features embedded-host` writes **this
window** via `screencapture -l`. See [RECORDING.md](RECORDING.md).

Do **not** spawn `gpui-agent` once per op (the old `scripts/smoke.sh`
pattern). That pays process + TCP handshake every time.

## `$params`

- Declare names in JSON `params` (wants files collect `$name` / `${name}`
  automatically).
- An undeclared `$placeholder` fails **validate**.
- A declared param without `--set name=…` fails **plan/run**.
- Substitution runs after validate, before compile (`$title` and
  `${title}`). Direct in-place subst (no serde round-trip).
- `$params` substitute **op payloads only** (targets, text, invoke args,
  assert names). Step `id` and `needs` are graph identity and are not
  rewritten — even if a param is named `id`. A `$placeholder` in `id`
  or `needs` fails **validate**.

## DAG / `needs`

- Each step is a protocol `Op` plus `id` and optional `needs`.
- If **no** step has `needs`, the list is an implicit linear chain.
- If **any** step has `needs`, the graph is an explicit DAG (Kahn waves;
  a cycle is an error). Wave order is sorted by step id (deterministic).
- Unknown `needs`, a step that needs itself, duplicate ids, empty ids /
  name, empty `steps`, `v` ≠ 1, or more than **256** steps fail validate.
- All-Read waves (len > 1, every step is only `Effect::Read`) use
  `rpc_pipeline`: write N request lines, then read N replies. A failed
  observe still records siblings that already ran; the **next wave**
  does not start. Extra reads are wasted work, not app mutations.
- Write, Exit, mixed waves, and `--screenshot-dir` stay **sequential
  fail-fast** on one socket: a failed step does not run later siblings
  or later waves. Empty / unknown effects are treated as Write.
- No wire `batch` op. Each line is still a normal tokened request.

## Receipts

`recipe run` prints a JSON receipt (and optionally `--receipt-out`):

| Field | Meaning |
| --- | --- |
| `ok` | Every step succeeded |
| `recipe` | Recipe name |
| `fingerprint` | Hash of the compiled plan (`DefaultHasher` — fine in one process, not a cross-version lock) |
| `session_reused` | `AgentClient` still held a live TCP session after the last step |
| `steps[]` | Per-step `id` / `ok` / `error` / `elapsed_ms` / `result` / optional `screenshot` |
| `screenshots[]` | Intended PNG paths (`path` / `ok` / `error`). Unavailable hosts do not fake a file. |
| `elapsed_ms` | Wall time for the run |

A mid-recipe failure (assert miss, host down, bad token) stops the run
and returns a **partial** receipt: earlier steps stay, later steps do
not run. Plans with `Effect::Exit` never start unless `--yes` is set.

## Schema allow-list + resolve

The default CLI/MCP registry is **protocol ops only**. App invoke/id
schemas load from `--schema PATH` (repeatable) and `GPUI_AGENT_SCHEMA`
(OS path list). Sample todo schemas live in
`examples/schemas/todo.json`. `todo_registry()` still exists for in-process
tests (protocol + those invoke/id names).

```
gpui-agent recipe validate examples/recipes/todo-crud.json \
  --schema examples/schemas/todo.json
```

- `invoke` **names** must be registered as `SchemaKind::Invoke`. Unknown
  names fail closed. Invoking a protocol name (`click`) is rejected
  (“not an invoke schema”).
- Schema names are `[A-Za-z0-9_.-]`. Spaces / shell punctuation cannot
  be registered.
- `recipe resolve` scores keywords + names, then materializes an `Op`.
  Empty, unknown, ambiguous, or shell-like intents (`rm`, `curl`,
  `bash`, …) return an error. Missing required `title` / `id` also
  fails. Resolve never shells out.

Protocol ops (`click`, `set_value`, …) are always valid **as** those
ops. The allow-list is for `invoke` and for prose → schema mapping.

## Session reuse

`AgentClient::rpc` opens one TCP session and keeps it (P0). A recipe
(and a second recipe on the **same** client) reuses that socket. Each
step is still a normal `Request` (token, version, caps).

`rpc_once` is the old per-op reconnect path, kept for benches and
comparison tests. It drops the session after one exchange.

This is **not** a new wire `op`. The server still sees one NDJSON
request per step. The experiment batches on the **client**.

## `--yes` for shutdown

`shutdown` is `Effect::Exit`. `recipe plan` sets `requires_yes`.
`recipe run` without `--yes` (MCP: `yes: true`) returns
`NeedsYes` and does not contact the host. A bare `gpui-agent shutdown`
is unchanged — the gate is on the **recipe** path so a reused recipe
cannot exit the app by accident.

## Threat model (recipes must not bypass caps)

Recipes are a **confused-deputy amplifier**: they make it cheaper to
do the same things the CLI already can. They do not add privilege.

| Gate | Still true |
| --- | --- |
| Opt-in | Host still needs `GPUI_AGENT=1` (release: `GPUI_AGENT_ALLOW_RELEASE=1`) |
| Bind | CLI still `ensure_loopback` before connect |
| Token | CLI `recipe run` and `mcp` **refuse to start** without a non-empty `GPUI_AGENT_TOKEN` or `--token` (P2). Every recipe step is a normal `Request`; `authorize_request` still runs. Missing/wrong token fails the step and the server still closes. Host bind is default-deny; `GPUI_AGENT_INSECURE_NO_TOKEN=1` is the only untokened loopback. **Set the same token on host and client**. `hello.auth` is `"required"` \| `"none"`. |
| Line / conn / idle / mailbox | Unchanged. Recipe cap 256 is extra, not a replacement. |
| No OS HID | `delivery` defaults to `semantic`. `virtual` is still in-process GPUI. `--screenshot-dir` is observe-only (macOS **embedded-host**: this window; never the desktop). |
| No shell | Resolve/plan/run never call `Command`. `invoke` is still an in-process host callback. Unknown invoke names are rejected. |
| Shutdown | Plans with `Effect::Exit` require `--yes` (rwmcp-style). |

What a recipe **cannot** do:

- Bind or send a token off-box
- Skip version / token checks by batching
- Turn `invoke` into a shell (the host allow-list is still the law)
- Grow a request line past `MAX_LINE_BYTES`
- Open more than `MAX_CONNECTIONS` (one recipe = one client session)

What it **can** do (intentionally): drive the UI as whoever holds the
token, including `snapshot` field values and `shutdown` (with `--yes`).
Same as M4 in [SECURITY.md](SECURITY.md). Treat `recipe run` / MCP
`recipe_run` as equivalent to holding the token.

## Edge-case coverage

Deterministic tests live in `gpui-agent-recipe` (unit + headless
`spawn_host` integration) and in CLI/MCP parse tests. They cover, at
least:

- Parse: empty file / comments-only, empty `steps`, bad JSON, unknown
  op, missing `$params`, cyclic / unknown / self `needs`, duplicate
  ids, `$params` in step `id`/`needs`, version ≠ 1, 257 steps
- `.wants` tokenizer: quotes, comments, blanks, invalid lines
- Resolve: shell-like intents, unknown verbs, ambiguous titles,
  missing required args
- Run: assert fail mid-recipe (partial receipt), host down, wrong
  token, CLI `recipe run` / `mcp` without token fail fast; shutdown
  without `--yes`; screenshot paths on the receipt;
  headless `screenshot_unavailable` without a fake PNG; mocked host
  writes `TEST_PNG` when `--screenshot-dir` / flagged steps are set
- Session: second recipe on one `AgentClient` stays connected;
  `rpc_once` reconnects
- Security: unknown invoke, non-allowlisted schema name, MCP
  validate/resolve/run-without-yes; MCP `recipe_run` failed assert is
  `tools/call` `isError` (receipt still in the error text)

```bash
cargo test -p gpui-agent -p todo-core -p gpui-agent-cli -p gpui-agent-recipe
```

## CI (P4)

GitHub Actions (`.github/workflows/ci.yml`) on `ubuntu-latest` runs
those tests, then `scripts/ci-recipe.sh`: `todo-headless` +
`gpui-agent recipe run examples/recipes/todo-crud.json` with
`GPUI_AGENT=1` on loopback and a **test token** (`ci-p4-token`, same
value on host and client). The job fails unless the receipt JSON has
`"ok": true` and `"session_reused": true`. No display, no GPU, no
screenshot files. A failed recipe is a failed check, not a skipped
visual.

```bash
GPUI_AGENT=1 GPUI_AGENT_TOKEN=ci-p4-token ./scripts/ci-recipe.sh
```

## Performance (this crate)

P0 session reuse is already on `main` ([PERF.md](PERF.md)). This crate
adds:

- Direct `$param` substitution (no serde round-trip, no `Vec<char>`)
- Index-only `compile_plan` (no `RecipeStep` clones in Kahn; skip
  `apply_params` when `params` is empty)

All-Read DAG waves use `rpc_pipeline` (write N lines, then read). Write /
Exit / mixed waves and `--screenshot-dir` stay sequential fail-fast.

Criterion benches:

```bash
cargo bench -p gpui-agent --bench agent_perf -- --quick
cargo bench -p gpui-agent-recipe --bench recipe_plan -- --quick
```

## Ask later (open product choices)

1. **Freeze the recipe syntax?** Keep dual JSON + wants, drop wants, or
   adopt rwmcp predicates (`invoice(…).exists`) if a world model lands.
2. **Path-dep `tmp-core`?** Only if we can take schema/resolve without
   shell resolvers. Default remains local schemas.
3. **Ephemeral Jupyter mint?** Not in P2. Host token stays optional.
   Recipe/MCP already require a client token. Ask before minting at bind.
4. **Wire `batch` / pipeline writes?** Read-only waves already pipeline.
   A wire `batch` op is still rejected (ordinary lines are enough).
   Independent **Write** siblings stay sequential so a failed click does
   not run the next one.
5. **Parallel waves on multiple connections?** Rejected: mutex / UI
   thread / `MAX_CONNECTIONS`.
6. **App-specific registries on disk?** Today the demo todo schema is
   baked in. A `schemas/*.json` directory is the obvious next step.
7. **Fingerprint stability?** Today it is `DefaultHasher` of the
   compiled ops — fine for one process, not a cross-version lock.
8. **Real desktop PNG (P3).** Landed as
   [PR #20](https://github.com/hexuria/gpui-agent/pull/20). Headless /
   daemon / default GUI client stay honest (`screenshot_unavailable`).
   macOS **embedded-host** uses `screencapture -l`. `--record` /
   SVG+PPM / ScreenCaptureKit stay later.
9. **CI receipt assert (P4).** Gate is receipt `ok` (+ `session_reused`), not pixels.
   Landed as [PR #21](https://github.com/hexuria/gpui-agent/pull/21).

## Layout

```
crates/gpui-agent-recipe   Recipe / schema / resolve / plan / receipt
crates/gpui-agent          Session reuse, ndjson buffers, tree/mailbox, screenshot op
crates/gpui-agent-cli      experimental recipe validate|plan|run|resolve + MCP tools
examples/recipes/          Sample todo CRUD (JSON canonical + .wants alias)
docs/RECIPES.md            This note
docs/TRY_ON_MAC.md         Pull + run recipes on a laptop (headless first)
docs/RECORDING.md          PNG vs --record (P3)
docs/SECURITY.md           Caps + recipe threat model
docs/NO_BRAINER_PLAN.md    P0–P5 roadmap
.github/workflows/ci.yml   Headless cargo test + recipe receipt
scripts/ci-recipe.sh       Local/CI recipe receipt assert
docs/STACK_HYGIENE.md      Leftover #4/#5 closed without merge
```
