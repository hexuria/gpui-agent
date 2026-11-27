# Recipes

A recipe is a JSON file of gpui-agent protocol ops (`click`, `set_value`,
`assert`, `invoke`, …). `gpui-agent recipe run` validates it, compiles it
to a plan, and runs every step in **one** process on **one** TCP session,
then prints a receipt. It replaces a tool round-trip per action.

Recipes are a client feature. The host still sees one ordinary NDJSON
request per step ([PROTOCOL.md](PROTOCOL.md)); there is no wire `batch`
op, and no cap or auth check is skipped
([SECURITY.md](SECURITY.md#recipes)).

## Stability

Recipes are stable as of **0.4.0**. Within recipe format `v: 1`, the
following are frozen. Additions arrive only as new optional fields, new
error codes, or new commands; nothing listed here is renamed or removed
without a new format version or a new major crate version.

| Surface | Frozen |
| --- | --- |
| Recipe file | Fields and rules in [Recipe format](#recipe-format); `schemas/recipe.v1.schema.json` |
| `.wants` | Line grammar in [`.wants`](#wants) |
| CLI | `recipe validate`, `recipe plan`, `recipe run`, their flags, exit status, and what is printed on stdout |
| MCP | `recipe_validate`, `recipe_plan`, `recipe_run`, their arguments and results |
| Plan JSON | Fields in [Plan](#plan), versioned by `v` |
| Receipt JSON | Fields in [Receipt](#receipt), versioned by `v` |
| Errors | The `error_code` strings in [Errors](#errors) |
| Fingerprint | The algorithm in [Plan](#plan) |
| Rust API | Public items of `gpui-agent-recipe`, under semver |

Not frozen:

- Human-readable `error` text. Match on `error_code`.
- `recipe resolve` / `recipe_resolve` and `resolve_intent`: scoring is
  heuristic and stays labeled experimental.
- Timing fields' values, and the order of keys in printed JSON.

A recipe that validates under one 0.4+ release validates under later
ones, provided the app schema it is checked against is unchanged. A
recipe with an unsupported `v` is rejected with
`recipe_unsupported_version`, never run best-effort.

Rust callers: `RecipeErrorCode`, `RunError`, `SchemaKind`, `Effect` and
every public struct are `#[non_exhaustive]`. Add a wildcard arm when
matching, and read struct fields rather than writing a struct literal.
Build with `Recipe::new`, `RecipeStep::new`, `OpSchema::new`,
`ArgSchema::new`, `ScreenshotCapture::new`, `RunOptions::new` and
`Param::named`, then set fields; a recipe or schema file is read with
`Recipe::from_json` / `load_schema_file`.

## Recipe format

```json
{
  "$schema": "../../schemas/recipe.v1.schema.json",
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

Top level:

| Field | Required | Rule |
| --- | --- | --- |
| `v` | No (default `1`) | Must be `1` |
| `name` | Yes | Non-empty |
| `app` | No | Informational; copied to the plan |
| `params` | No | Bare names or [typed entries](#typed-params); names unique, matching `[A-Za-z_][A-Za-z0-9_]*`; each must be used by a step |
| `steps` | Yes | 1 to 256 steps |
| `$schema` | No | Editor hint; ignored |

Each step:

| Field | Required | Rule |
| --- | --- | --- |
| `id` | Yes | Non-empty, unique, no `$param` |
| `op` | Yes | A protocol op name, snake_case (`set_value`, `wait_until`, …) |
| `needs` | No | Ids of other steps; no duplicates, no self, no `$param`, no cycle |
| `screenshot` | No | `true` marks the step for `--screenshot-flagged` |
| `key_delay_ms` | No | `type` only: [pace](#paced-typing) the text, this many milliseconds (1 to 10000) between two keys, 120000 ms of pauses at most |
| op fields | Per op | The same fields the op takes on the wire ([PROTOCOL.md](PROTOCOL.md)) |

**Unknown fields are errors**, at the top level and in a step. A step may
only carry the fields its own op reads, so `"need"` or `"tagret"` fails
with `recipe_unknown_field` instead of being ignored.

`schemas/recipe.v1.schema.json` is the JSON Schema (2020-12) for this
structure, for editors and pre-checks. `recipe validate` is the
authority: it also checks ids, `needs`, params, and invoke args, which a
JSON Schema cannot. One more thing a JSON Schema cannot say: its
`integer` admits a whole number written with a decimal point (`1.0`),
and a whole-number step field here must be written without one (`1`).

File handling: a `.json` path is always parsed as JSON. Any other path
is `.wants`, unless the text starts with `{`. Path `-` reads stdin.

### Params

- Write `$name` or `${name}` inside any string op field: targets, text,
  values, assert fields, and strings nested in `invoke` args.
- `$$` is a literal `$`. `"$$5"` is the text `$5`; `"$$title"` is the
  text `$title`.
- A `$` not followed by a name or `{name}` is a literal `$`.
- Every placeholder must be declared in `params`, and every declared
  param must be used. Both fail `validate` with `recipe_invalid_param`.
- `plan` and `run` need a value for every required param
  (`recipe_missing_param`) and reject values for names the recipe does
  not declare (`recipe_unknown_param`). Repeating `--set` for one name is
  an error.
- Values are substituted as text. One exception: an `invoke` arg written
  as exactly one placeholder (`"id": "$id"`) whose schema type is
  `number`, `integer`, or `bool` is converted to that type, so
  `--set id=1` sends `1`, not `"1"`. A value that does not convert fails
  with `recipe_invalid_param_value`.
- Step `id` and `needs` are graph identity and are never substituted.

### Typed params

A `params` entry is a bare name (a required string) or an object:

```json
"params": [
  "title",
  {"name": "count", "type": "integer", "default": 1},
  {"name": "kind", "enum": ["task", "note"], "description": "What to add"},
  {"name": "amount", "pattern": "^[0-9]+\\.00$", "required": false}
]
```

| Key | Rule |
| --- | --- |
| `name` | Required |
| `type` | `string` (default), `number`, `integer`, `bool` |
| `required` | Default `true`, or `false` when `default` is set. `true` with a `default` is rejected |
| `default` | Used when the caller gives no value; must itself pass the checks |
| `enum` | Non-empty list; the value must equal one entry. `number` and `integer` entries compare as numbers (`1`, `1.0` and `1e0` are one value); the rest compare as text |
| `pattern` | `string` params only. Regular expression that must match somewhere in the value; anchor with `^…$` |
| `description` | Free text for people and form builders |

- The input schema `validate` prints carries `pattern` as ECMAScript
  reads it, which is what JSON Schema validators run. Three things are
  spelled out there, because this tool reads them more narrowly: `.` is
  any character but a line feed, and `\s` / `\S` are the six ASCII
  whitespace characters (a no-break space is not one). A pattern in
  syntax only this tool reads (an inline flag such as `(?i)`,
  `(?P<name>…)`, `\A`, `\z`, `[[:alpha:]]`), or a class with a `-` that
  is not first, last, or between two single characters, is left out of
  the schema and still checked at `plan` / `run`. The exported pattern is
  meant for a validator that compiles it with the Unicode (`u`) flag, as
  Ajv does by default.
- A value that fails `type`, `enum`, or `pattern` fails `plan` / `run`
  with `recipe_invalid_param_value`. A bad declaration fails `validate`
  with `recipe_invalid_param`; an unknown key in the object fails with
  `recipe_parse`.
- An optional param with no `default` and no value is absent: every
  `invoke` arg written as exactly its placeholder (`"amount": "$amount"`,
  at any depth in `args`) is left out. Such a param may not be used
  anywhere else; `validate` rejects that.
  If the left-out arg is one the invoke schema requires, `plan` / `run`
  fail with `recipe_invalid_args`.
- `type` checks the value. It does not change how the value is sent:
  see the `invoke` arg conversion above.
- `recipe validate` prints `input_schema`, a JSON Schema (2020-12) object
  built from `params`, for callers that collect or check the values
  before running.

### Paced typing

A `type` step sends its text as one request. With `key_delay_ms` the
client sends it one character at a time instead: one ordinary `type`
request per character, `key_delay_ms` apart, as a person types.

- **Nothing new on the wire.** The host sees ordinary `type` requests, so a
  paced step runs against any host that takes `type`.
- **The target is focused once.** The first key goes to the step's
  target. Before every later key the client reads a snapshot, and the key
  goes to:
  - the node the key before went to, while it has the focus;
  - the next text box after that node among its parent's children, when
    the focus moved on to it: the next cell of a code box whose focus
    moves on with each digit. Never a box before it or further on, never
    when the parent is a window, a page, a dialog or the tree's root (a
    screen of fields), and never after a `\n` or a `\t` key;
  - the target, while the focus has not moved from where it was before
    the first key (a host whose `type` does not focus its target), or no
    node has ever reported it.
  Anywhere else (an autocomplete or a dialog took it, a full box moved it
  to the next field of a screen, a line break or a tab moved it, it left
  the code box) the step stops as a partial delivery: `the focus left
  <target> for <node>`. So a host that focuses
  only an unfocused `type` target presses the box once, and its caret
  stays after the typed text. NSH's strict hosts do (and an NSH code box
  is one text field over its cells); a host that presses its target on
  every `type` moves the caret on every key.
- **No fingerprint on the later keys.** Between the snapshot and the key
  the focus could still move (an app's own timer); the later keys carry
  no `expect_fingerprint`, since the value a controlled input echoes back
  may change in that window. Each snapshot is a whole tree (a diff
  against `since` would be cheaper on a large form).
- **All or nothing before the first key.** A `virtual` step whose text has
  a character with no keystroke (only ASCII, space, `\n` and `\t` have one)
  fails before any key is sent, as one virtual `type` does.
- **A refusal part way is a partial delivery.** The step stops at the
  first key the host refuses. After the first key the error says how many
  keys reached the app (`paced type stopped at key 3 of 4: the first 2
  reached the app ...`) and the status is `delivery_uncertain`: snapshot
  before retrying, the box already holds those keys. Its receipt `result`
  is the last key's.
- **The pace is a floor.** Each key waits for the reply and a snapshot
  before the next; a slow host makes the pace slower, never faster.
- **Bounded.** `key_delay_ms` is 1 to 10000 (anything else is refused when
  the file is read, `recipe_parse`; on any op but `type`,
  `recipe_unknown_field`), and the pauses of one step together at most
  120000 ms (`MAX_PACED_TYPE_MS`; `plan` / `run` refuse a longer one with
  `recipe_invalid_step` before anything is sent).
- **A key is a Unicode scalar value, not a grapheme:** a combining mark or
  a joined emoji goes as its parts (`\r\n` stays one key). Type such text
  without pacing.
- Only the first key carries the step's `expect_fingerprint`: each key
  changes the node's value, and with it the fingerprint.
- `key_delay_ms` is not part of the plan [fingerprint](#plan): it changes
  how the text is delivered, not what the plan does.
- A 0.4.0 client refuses the field (`recipe_unknown_field`): a recipe that
  paces needs gpui-agent-cli 0.4.1 or later.

### Scheduling

- If **no** step has `needs`, the steps run in file order, one per wave.
- If **any** step has `needs`, the recipe is an explicit DAG. Steps
  whose dependencies are done form a wave; within a wave, steps are
  ordered by id. File order is not used.
- A wave of two or more steps that only read is sent pipelined (N
  requests written, then N replies read). If one fails, its siblings
  have already run; the next wave does not start.
- Every other wave (any write, any exit, or a run with screenshots) is
  sequential and fail-fast: the first failure stops the run, and later
  steps do not execute.
- `recipe plan --order-check` compiles the steps in listed and reversed
  order and reports whether the waves differ (`order_dependent`).

### `invoke` and app schemas

Protocol ops are always allowed. `invoke` is allowed only for names
registered in an app schema file, loaded with `--schema PATH`
(repeatable) or `GPUI_AGENT_SCHEMA` (OS path list). MCP recipe tools
also take a `schema` argument. Sample: `examples/schemas/todo.json`.

`validate` checks each `invoke` step against its schema:

- the name is registered with `"kind": "invoke"`
  (else `recipe_unknown_invoke`);
- every arg in `required`, or marked `"required": true`, is present;
- if the schema has `args`, no arg is passed that is named neither there
  nor in `required`. `"args": {}` therefore means the invoke takes none.
  A schema that leaves `args` out does not list its args, and their
  names are not checked. `"args": null` is neither and is refused;
- each arg has its declared type (`string`, `number`, `integer`, `bool`,
  `object`, `array`; other type names are not checked). An arg that
  still contains a placeholder is type-checked after binding.

Violations are `recipe_invalid_args`.

### Confirmation

- A plan containing `shutdown`, or a quit keybinding, has
  `requires_yes: true`. `recipe run` refuses it without `--yes` (MCP:
  `yes: true`) with `recipe_unconfirmed`, before contacting the host.
- A quit keybinding step must also carry `"confirm": true`.

## `.wants`

A line-based alias for a recipe with no `needs` (file order). It
compiles to the same structure; step ids are `s1`, `s2`, …, the name is
the file stem, and `params` is collected from the placeholders used.

```
# bind $title with --set title="Buy milk"
wait
invoke todo.add title=$title
assert todo-item-1 name=$title checked=false
invoke todo.toggle id=1
assert todo-item-1 checked=true
```

- One op per line. `#` starts a comment; blank lines are skipped.
- `"` or `'` keeps spaces in one token: `title="Buy milk"`. `\"` escapes
  inside quotes. An unterminated quote is an error.
- `click` / `type` / `key` accept `--delivery semantic|virtual`.
- `click` / `type` / `set-value` / `key` accept `--fp <n>` (JSON:
  `expect_fingerprint`): the step is refused with `stale_target` unless
  the target's fingerprint is still `n`.
- `press <target>` and `long_press <target> [--duration-ms <n>]` accept
  `--within <x>,<y>` (JSON: `within: {"x": x, "y": y}`): aim that many
  logical points from the target's top-left instead of its centre. `x` and
  `y` are finite, non-negative numbers. This is a field capability, not a
  new wire op: before the first wave the client reads `hello.features` once
  and refuses the whole run if any step needs a missing non-legacy feature.
  It sends `within` only to a host listing `press_within`; against any other
  host it refuses before sending, because a 0.4.1 host would ignore the
  field and press the centre. `AgentClient::as_written()` deliberately skips
  all local refusals, including this preflight, and would send `within` to
  an older host exactly as written; use it only when that wire behaviour is
  intended. The target-independent `x` / `y` point fields remain ignored
  when `target` is set and are not reused
  for this aim.
- `type` accepts `--key-delay-ms <n>` (JSON: `key_delay_ms`): the text
  is [paced](#paced-typing), `n` milliseconds between two keys.
- `keybinding --id … --scope focused|global`, with `--confirm` for quit
  and `--activate` for focused only. `keybindings` lists the catalog.
- `assert <target>` takes `name=` / `value=` / `role=` / `checked=` /
  `--absent` / `--visible` / `--in-viewport`.
- `wait_until --timeout-ms N` takes the same fields as `assert`.
- `--screenshot` on a line marks the step.
- An unknown op, a missing argument, or a bad assert field is
  `recipe_parse`, with the line number in the message.

## Commands

| Command | Host | Token | Does |
| --- | --- | --- | --- |
| `recipe validate <path>` | No | No | Parse and check every rule above |
| `recipe plan <path> [--set k=v]… [--order-check]` | No | No | Validate, bind params, print the [plan](#plan) |
| `recipe run <path> [--set k=v]… [--yes] [--receipt-out FILE] [--screenshot-dir DIR [--screenshot-flagged]]` | Yes | Yes | Plan, run, print the [receipt](#receipt) |
| `recipe resolve '<intent>'` | No | No | Experimental. Map prose to a schema-backed op, or fail |

All take `--schema PATH`. `recipe run` requires a non-empty
`GPUI_AGENT_TOKEN` or `--token`, the same value as the host.

```bash
export GPUI_AGENT_TOKEN=dev-secret

gpui-agent recipe validate examples/recipes/todo-crud.json \
  --schema examples/schemas/todo.json
gpui-agent recipe plan examples/recipes/todo-crud.json \
  --schema examples/schemas/todo.json --set title="Buy milk"
gpui-agent recipe run examples/recipes/todo-crud.json \
  --schema examples/schemas/todo.json --set title="Buy milk"
```

Output contract:

| Outcome | Exit | stdout | stderr |
| --- | --- | --- | --- |
| `validate` ok | 0 | `{"ok": true, "name", "steps", "params" (names), "input_schema", "explicit_needs"}` | |
| `plan` ok | 0 | Plan JSON (or order-check JSON) | |
| `run` ok | 0 | Receipt JSON, `ok: true` | |
| Rejected before running | 1 | Error envelope | `error: <text>` |
| `run`, a step failed | 1 | Partial receipt JSON, `ok: false` | `error: <text>` |

`--receipt-out FILE` writes the same receipt JSON to a file, on success
and on step failure.

MCP tools `recipe_validate`, `recipe_plan`, and `recipe_run` take
`recipe` (a JSON object, or recipe text), `set` (object of param name to
value), and `schema`; `recipe_run` also takes `yes`, `screenshot_dir`,
and `screenshot_flagged`. Results are the same JSON as the CLI's stdout.
Every failure is a tool result with `isError: true` whose text is the
error envelope, or for a failed step the partial receipt with top-level
`error` and `error_code` added.

### Screenshots

With `--screenshot-dir DIR`, the run asks the host for an app-surface
PNG after each step (with `--screenshot-flagged`, only steps marked
`screenshot`). Files are `001-<id>.png`, `002-<id>.png`, … and are
listed in the receipt. A host that lists the `screenshot_inline` feature
sends the PNG over the wire and the file is written in `DIR` on the
client. An older host writes the file itself, under its own screenshot
directory, and the receipt records the path that host reports. A host that cannot capture (headless, the daemon,
Linux and Windows desktop) reports `screenshot_unavailable` in the
receipt and writes no file; the run still succeeds. See
[RECORDING.md](RECORDING.md).

## Plan

Printed by `recipe plan`; `Plan` in Rust.

| Field | Meaning |
| --- | --- |
| `v` | Plan format version, `1` |
| `name`, `app` | From the recipe |
| `steps[]` | In execution order: `id`, bound `op`, `needs`, `effects` (`read` / `write` / `exit`), `schema`, `idempotent`, `screenshot`, `key_delay_ms` (when set) |
| `waves[]` | Step ids per wave |
| `requires_yes` | The plan has an exit effect |
| `fingerprint` | Identity of the bound plan |

`needs` in a plan includes the implicit previous-step edge of a recipe
without `needs`.

The fingerprint is `fnv1a64:` followed by 16 lowercase hex digits:
FNV-1a 64 over the canonical JSON of `{name, steps: [{id, op, needs}]}`
in execution order, where canonical means compact with object keys sorted
(unset optional op fields are absent; an explicit `null` is kept). It is the same on every platform and
toolchain, and changes when the name, a step, a bound value, or the
order changes. It is a correlation id for receipts and caches, not a
security digest.

## Receipt

Printed by `recipe run`; `Receipt` in Rust.

| Field | Meaning |
| --- | --- |
| `v` | Receipt format version, `1` |
| `ok` | Every step succeeded |
| `recipe` | Recipe name |
| `fingerprint` | The plan's fingerprint |
| `session_reused` | The client still held its TCP session after the last step |
| `steps[]` | Steps that ran: `id`, `ok`, `elapsed_ms`, `result`; on failure `error` and `error_code`; for ops that act, `delivery_status`; optional `screenshot` |
| `screenshots[]` | `path`, `ok`, `error` for each requested capture |
| `elapsed_ms` | Wall time of the run |

When a step fails the receipt is partial: steps that ran are listed, the
failed step is last (or among its read-only wave), and later steps are
absent.

## Errors

A recipe rejected before anything runs produces this envelope:

```json
{ "ok": false, "error_code": "recipe_unknown_field", "error": "recipe json: step `b` has unknown field `need` for op `snapshot`", "step": "b" }
```

`step` is present when the error is about one step. In Rust this is
`RecipeError { code, message, step }`, and `lint_recipe` returns every
violation rather than the first.

| `error_code` | Raised by | Meaning |
| --- | --- | --- |
| `recipe_io` | all | A recipe or schema file could not be read |
| `recipe_parse` | all | Not valid JSON / `.wants`, a JSON key written twice in one object (in a recipe file, recipe text, or an MCP recipe tool's request), or a field has the wrong type |
| `recipe_unknown_field` | all | A key the recipe format or the step's op does not have |
| `recipe_unsupported_version` | validate | `v` is not `1` |
| `recipe_invalid` | validate | Empty name, no steps, or more than 256 steps |
| `recipe_invalid_step` | validate | Step id empty, duplicated, or containing a `$param` |
| `recipe_invalid_needs` | validate | `needs` entry unknown, duplicated, self, or containing a `$param` |
| `recipe_cycle` | validate | `needs` form a cycle |
| `recipe_invalid_param` | validate, `--set` | Param name invalid, duplicated, unused, or undeclared; bad typed-param declaration; `--set` malformed or repeated |
| `recipe_unknown_invoke` | validate | `invoke` name is not a registered invoke schema |
| `recipe_invalid_args` | validate, plan | `invoke` args missing, unknown, or of the wrong type |
| `recipe_unconfirmed` | validate, run | Quit keybinding without `confirm`, or an exit plan without `--yes` |
| `recipe_missing_param` | plan, run | A required param has no value |
| `recipe_unknown_param` | plan, run | A value for a param the recipe does not declare |
| `recipe_invalid_param_value` | plan, run | A value fails its param's `type`, `enum`, or `pattern`, or does not convert to the arg's schema type |
| `recipe_invalid_schema` | all | A `--schema` file is invalid or redefines a name |
| `recipe_unresolved` | resolve | The intent did not map to exactly one op |

`plan` and `run` validate first, so they can raise every `validate`
code. A step that fails **during** a run carries the protocol's own
codes (`assert_failed`, `not_found`, `timeout`, `connect_failed`, …) in
the receipt, not a `recipe_*` code.

## Rust API

```rust
use std::collections::BTreeMap;
use gpui_agent_recipe::{RunOptions, compile_plan, parse_recipe, run_plan};

let recipe = parse_recipe("examples/recipes/todo-crud.json".as_ref())?;
let set = BTreeMap::from([("title".to_string(), "Buy milk".to_string())]);
let plan = compile_plan(&recipe, &set, &registry)?;   // validates first
let receipt = run_plan(&mut client, &plan, &RunOptions::new())?;
```

- Parse with `parse_recipe`, `parse_recipe_source`, `Recipe::from_json`,
  or `Recipe::from_value`. Plain `serde_json::from_str::<Recipe>` does
  not reject unknown step fields.
- `validate_recipe` returns the first violation; `lint_recipe` all of
  them.
- `run_plan` returns `RunError::Step { id, error, error_code, receipt }`
  with the partial receipt, or `RunError::NeedsYes`.

Migrating from 0.3:

| 0.3 | 0.4 |
| --- | --- |
| `Result<_, String>` | `Result<_, RecipeError>`; use `.code`, or `.to_string()` for the old text |
| `run_plan(client, plan, yes)` | `run_plan(client, plan, &RunOptions::new().with_yes(yes))` |
| `run_plan_with_screenshots(…, Some(&capture))` | `RunOptions::new().with_screenshots(Some(capture))` |
| Exhaustive `match` on `RunError`, `SchemaKind`, `Effect` | Add a `_` arm |
| Fingerprint: 16 hex digits, toolchain-dependent | `fnv1a64:<16 hex>`; stored 0.3 fingerprints do not match |
| Unused / undeclared `--set`, unknown fields, unused params accepted | Rejected; fix the recipe |

## Security

Recipes add no privilege: they make it cheaper to do what the CLI can
already do with the same token. Treat `recipe run` and MCP `recipe_run`
as equivalent to holding the token.

| Gate | Still true |
| --- | --- |
| Opt-in | Host needs `GPUI_AGENT=1` (release: `GPUI_AGENT_ALLOW_RELEASE=1`) |
| Bind | Loopback by default; remote needs `--allow-remote` and a token |
| Token | `recipe run` and `mcp` refuse to start without a client token. Every step is a normal request and is authorized on its own |
| Caps | Line, connection, idle, and mailbox caps are unchanged. The 256-step cap is extra |
| No OS input | `delivery` defaults to `semantic`; `virtual` is in-process. Screenshots are observe-only and never the desktop |
| No shell | Validate, plan, run, and resolve never spawn a process. `invoke` is an in-process host callback, and only for names in the schema |
| Exit | Plans with an exit effect require `--yes` |

Full threat model: [SECURITY.md](SECURITY.md#recipes).

## Tests and CI

```bash
cargo test -p gpui-agent-recipe -p gpui-agent-cli
GPUI_AGENT=1 GPUI_AGENT_TOKEN=ci-p4-token ./scripts/ci-recipe.sh
```

`ci-recipe.sh` runs `examples/recipes/todo-crud.json` against
`todo-headless` and fails unless the receipt has `ok: true` and
`session_reused: true`. It runs in `.github/workflows/ci.yml`. Benches:
`cargo bench -p gpui-agent-recipe --bench recipe_plan -- --quick`
([PERF.md](PERF.md)).

Both samples assume a fresh, empty todo list (`todo-item-1`, `id=1`);
restart the host before a second run.

## Non-goals

- A wire `batch` op, or parallel execution on several connections.
- An LLM planner, goal solver, or world-model compiler.
- Resume from a partial receipt, idempotency keys, or retries.
- Shell or filesystem steps.
