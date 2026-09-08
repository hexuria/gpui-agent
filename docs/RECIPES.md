# Experimental recipes + TMP-inspired mapping

This is an **experiment** on a side branch. It does not replace semantic
RPC, does not steal the OS pointer, and does not change protocol v1.

The goal: let an agent **author or reuse a recipe** of gpui-agent ops
(`click` / `set-value` / `assert` / `invoke` / …) that compiles to a
plan and runs with **one CLI (or MCP) invocation** — one process, one
TCP session, many ops — instead of a tool round-trip per action.

## What was borrowed

### From [rwmcp](https://github.com/hexuria/reverse-web-mcp)

Kept the *shape*, not the world-model compiler:

| Idea | Here |
| --- | --- |
| `--wants` file: one predicate/op per line, `#` comments | `*.wants` compiles to a `Recipe` |
| A successful plan is a reusable recipe with `$params` | JSON `Recipe` + `--set key=value` |
| `validate` / `plan` / `run` / receipt | `gpui-agent recipe validate\|plan\|run` |
| Order-check (compile listed vs reversed) | `--order-check` |
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

Both samples assume a **fresh empty** todo list (`todo-item-1` / `id=1`).
Restart the host before a second run.

**Laptop (pull + run only):** [TRY_ON_MAC.md](TRY_ON_MAC.md).

Rules:

- Each step is a **protocol `Op`** plus `id` and optional `needs`.
- If no step has `needs`, the list is an implicit linear chain.
- If any step has `needs`, the graph is an explicit DAG (Kahn waves,
  cycle → error). Wave order is sorted by step id (deterministic).
- `$name` / `${name}` substitution after validate, before compile.
- Max 256 steps (mailbox / DoS cap cousin).
- `invoke` names must appear in the local schema registry (fail closed).
  Protocol ops (`click`, `set_value`, …) are always allowed.

This is **not** a new wire `op`. The server still sees one NDJSON
request per step. The experiment batches on the **client**.

## How an agent authors and runs a recipe (fewer tool calls)

Steady state (zero model calls at run time):

```bash
# terminal 1
GPUI_AGENT=1 cargo run -p todo-headless

# terminal 2 — one process, one connection, many ops
cargo run -p gpui-agent-cli -- recipe run examples/recipes/todo-crud.json --set title="Buy milk"
```

Authoring paths:

1. **Reuse.** Check in `examples/recipes/*.json`. Agent runs it with
   `--set`. No model.
2. **Write once.** Agent emits a `.wants` or JSON recipe in one sample,
   then `recipe validate` / `recipe plan` / `recipe run`.
3. **Resolve then write.** `gpui-agent recipe resolve 'add a todo titled Buy milk'`
   returns a schema-backed `invoke todo.add` (effects, required args,
   result shape). The agent pastes that into a recipe instead of
   guessing CLI flags.

MCP: `recipe_run` accepts the same document inline so a single
`tools/call` replaces N `click` / `assert` calls. Per-op tools remain
for interactive debugging.

Do **not** spawn `gpui-agent` once per op (the old `scripts/smoke.sh`
pattern). That pays process + TCP handshake every time. Recipes and
MCP already share a client; `AgentClient` now reuses the socket.

## Threat model (recipes must not bypass caps)

Recipes are a **confused-deputy amplifier**: they make it cheaper to
do the same things the CLI already can. They do not add privilege.

| Gate | Still true |
| --- | --- |
| Opt-in | Host still needs `GPUI_AGENT=1` (release: `GPUI_AGENT_ALLOW_RELEASE=1`) |
| Bind | CLI still `ensure_loopback` before connect |
| Token | Every recipe step is a normal `Request`; `authorize_request` still runs. Missing/wrong token fails the step and the server still closes. |
| Line / conn / idle / mailbox | Unchanged. Recipe cap 256 is extra, not a replacement. |
| No OS HID | `delivery` defaults to `semantic`. `virtual` is still in-process GPUI. |
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
```
