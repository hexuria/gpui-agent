# Experimental recipes + TMP-inspired mapping

This is an **experiment** on a side branch. It does not replace semantic
RPC, does not steal the OS pointer, and does not change protocol v1.

The goal: let an agent **author or reuse a recipe** of gpui-agent ops
(`click` / `set-value` / `assert` / `invoke` / …) that compiles to a
plan and runs with **one CLI (or MCP) invocation** — one process, one
TCP session, many ops — instead of a tool round-trip per action.

**Laptop (pull + run only):** [TRY_ON_MAC.md](TRY_ON_MAC.md).
Threat model vs PR #3 caps: [SECURITY.md](SECURITY.md#recipes-experimental).
Wire ops stay one NDJSON request each: [PROTOCOL.md](PROTOCOL.md).
Step PNGs for AI mid-run (plus optional video): [RECORDING.md](RECORDING.md).

## What was borrowed

### From [rwmcp](https://github.com/hexuria/reverse-web-mcp)

Kept the *shape*, not the world-model compiler:

| Idea | Here |
| --- | --- |
| `--wants` file: one predicate/op per line, `#` comments | `*.wants` compiles to a `Recipe` |
| A successful plan is a reusable recipe with `$params` | JSON `Recipe` + `--set key=value` |
| `validate` / `plan` / `run` / receipt | `gpui-agent recipe validate\|plan\|run` |
| Order-check (compile listed vs reversed) | `recipe plan --order-check` |
| Exit-effect gate (`--yes`) | Recipes that include `shutdown` refuse until `--yes` |
| Falsifiable receipt (what ran, ok/err, timing) | `Receipt` JSON, optional `--receipt-out` |
| One command instead of a session | Connection reuse in `AgentClient` |

Left out on purpose (non-goals):

- Full intent-graph / OpenAPI world-model compiler
- LLM planner / `--goal` / `--answer-with-model`
- Event-bus scheduler, idempotency keys, resume-from-ledger
- Parallel multi-connection execution (waves are *documented*, then
  run sequentially on one socket — the measurable win is session reuse
  vs N CLI processes, not a thread pool)
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

## Recipe format (JSON canonical; wants as sugar)

JSON is the canonical form because the wire protocol is already JSON,
rwmcp recipes are JSON, and an agent can emit a whole DAG in **one**
model sample. YAML would add a crate for no gain. Line-based `.wants`
exists so a human or agent can write a sequence without braces.

```json
{
  "v": 1,
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

| Command | Host? | What it does |
| --- | --- | --- |
| `recipe validate <path>` | No | Parse + lint (version, ids, `needs`, declared `$params`, invoke allow-list) |
| `recipe plan <path> [--set k=v] [--order-check]` | No | Bind params, schedule waves, print effects / fingerprint / `requires_yes` |
| `recipe run <path> [--set k=v] [--yes] [--receipt-out FILE] [--screenshot-dir DIR] [--record PATH]` | Yes | Compile, then execute each `Op` on **one** reused TCP session. Step PNGs + optional video: [RECORDING.md](RECORDING.md) |
| `recipe resolve '…'` | No | Map prose through the local schema registry (fail closed) |

MCP tools with the same jobs: `recipe_validate`, `recipe_plan`,
`recipe_run` (pass `yes: true` for shutdown), `recipe_resolve`. They
are batching helpers, not app-specific verbs. Per-op tools stay for
interactive debugging.

```bash
# no host
gpui-agent recipe validate examples/recipes/todo-crud.json
gpui-agent recipe plan examples/recipes/todo-crud.json --set title="Buy milk"
gpui-agent recipe resolve 'add a todo titled Buy milk'

# host required
gpui-agent recipe run examples/recipes/todo-crud.json --set title="Buy milk"

# AI mid-run: intended PNGs after every step (headless lists screenshot_unavailable)
gpui-agent recipe run examples/recipes/todo-crud.json --set title="Buy milk" \
  --screenshot-dir artifacts/steps/
```

Between **manual** steps, the same observe-only op:

```bash
gpui-agent screenshot --out artifacts/steps/mid.png
```

JSON steps may set `"screenshot": true`; wants lines may take
`--screenshot`. With `--screenshot-flagged`, only those steps are
captured. Names are `001-wait.png`, `002-add.png`, … and appear on the
receipt. See [RECORDING.md](RECORDING.md).

Do **not** spawn `gpui-agent` once per op (the old `scripts/smoke.sh`
pattern). That pays process + TCP handshake every time.

## `$params`

- Declare names in JSON `params` (wants files collect `$name` / `${name}`
  automatically).
- An undeclared `$placeholder` fails **validate**.
- A declared param without `--set name=…` fails **plan/run**.
- Substitution runs after validate, before compile (`$title` and
  `${title}`).

## DAG / `needs`

- Each step is a protocol `Op` plus `id` and optional `needs`.
- If **no** step has `needs`, the list is an implicit linear chain.
- If **any** step has `needs`, the graph is an explicit DAG (Kahn waves;
  a cycle is an error). Wave order is sorted by step id (deterministic).
- Unknown `needs`, a step that needs itself, duplicate ids, empty ids /
  name, empty `steps`, `v` ≠ 1, or more than **256** steps fail validate.
- Waves are **documentation only**. Execution is still sequential on one
  socket.

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

The demo registry is baked into `gpui-agent-recipe` (`todo_registry()`):
protocol ops, `todo.add|toggle|delete|list`, and a few stable ids.
Other apps should ship their own schemas later; there is no registry
cloud and no `tmp-core` path-dep.

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

`AgentClient::rpc` opens one TCP session and keeps it. A recipe (and a
second recipe on the **same** client) reuses that socket. Each step is
still a normal `Request` (token, version, caps).

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
| Token | Every recipe step is a normal `Request`; `authorize_request` still runs. Missing/wrong token fails the step and the server still closes. |
| Line / conn / idle / mailbox | Unchanged. Recipe cap 256 is extra, not a replacement. |
| No OS HID | `delivery` defaults to `semantic`. `virtual` is still in-process GPUI. `--screenshot-dir` / `--record` are observe-only. |
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
  ids, version ≠ 1, 257 steps
- `.wants` tokenizer: quotes, comments, blanks, invalid lines
- Resolve: shell-like intents, unknown verbs, ambiguous titles,
  missing required args
- Run: assert fail mid-recipe (partial receipt), host down, wrong
  token, shutdown without `--yes`; screenshot paths on the receipt;
  headless `screenshot_unavailable` without a fake PNG; mocked host
  writes `TEST_PNG` when `--screenshot-dir` / flagged steps are set
- Session: second recipe on one `AgentClient` stays connected;
  `rpc_once` reconnects
- Security: unknown invoke, non-allowlisted schema name, MCP
  validate/resolve/run-without-yes

```bash
cargo test -p gpui-agent -p todo-core -p gpui-agent-cli -p gpui-agent-recipe
```

## Performance work in this experiment

1. **Session reuse** (`AgentClient`): first connect is retried; later
   `rpc` calls share the socket. `rpc_once` keeps the old per-op
   reconnect path for benches.
2. **NDJSON buffers**: `write_json_line` / `read_limited_line_into`
   reuse `Vec<u8>`; parse with `serde_json::from_slice`.
3. **Tree walk**: `visit` + `node_count` so `flatten` does one
   allocation instead of one vec per subtree.
4. **Mailbox `take`**: pre-size the output vec.
5. **Docs**: do not spawn a CLI process per op.

Criterion benches live in:

- `crates/gpui-agent/benches/agent_perf.rs` — parse, snapshot
  serialize, flatten, mailbox, session vs reconnect
- `crates/gpui-agent-recipe/benches/recipe_plan.rs` — compile

```bash
cargo bench -p gpui-agent --bench agent_perf
cargo bench -p gpui-agent-recipe --bench recipe_plan
```

## Ask later (open product choices)

1. **Freeze the recipe syntax?** Keep dual JSON + wants, drop wants, or
   adopt rwmcp predicates (`invoice(…).exists`) if a world model lands.
2. **Path-dep `tmp-core`?** Only if we can take schema/resolve without
   shell resolvers. Default remains local schemas.
3. **Required token?** H1 in SECURITY.md. Recipes make an open socket
   more dangerous; an ephemeral printed token pairs well with this.
4. **Wire `batch` op?** Would cut per-op authorize + serialize cost
   further, but is a protocol bump. Client-side session reuse is enough
   for this experiment.
5. **Parallel waves on multiple connections?** Needs mailbox fairness
   and a story for `MAX_CONNECTIONS`. Not worth it for headless CRUD.
6. **App-specific registries on disk?** Today the demo todo schema is
   baked in. A `schemas/*.json` directory is the obvious next step.
7. **Fingerprint stability?** Today it is `DefaultHasher` of the
   compiled ops — fine for one process, not a cross-version lock.
8. **MCP `recipe_*` tools vs CLI-only?** Both shipped; product may
   want resolve+plan only (run stays an explicit CLI `--yes`).

## Layout

```
crates/gpui-agent-recipe   Recipe / schema / resolve / plan / receipt
crates/gpui-agent          Session reuse, ndjson buffers, tree/mailbox
crates/gpui-agent-cli      recipe validate|plan|run|resolve + MCP tools
examples/recipes/          Sample todo CRUD (JSON + wants)
docs/RECIPES.md            This note
docs/TRY_ON_MAC.md         Pull this branch and run it on a laptop
docs/RECORDING.md          Step PNGs (AI mid-run) + optional video
docs/SECURITY.md           Caps + recipe threat model
```
