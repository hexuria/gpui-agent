# Scout: instinct + jev for gpui-agent speed, accuracy, and context

Date: 2026-10-17. Read-only scout — no code changes proposed for merge yet.

Sources scouted (all local clones):
- `../instinct` (hexuria/instinct) — deterministic decision engine
- `../ultra-instinct` (hexuria/ultra-instinct) — browser-agent runtime that consumes instinct + jev
- `../typesafe-sdk-rust` (codeitlikemiley/typesafe-sdk-rust) — Rust client for System One (`jev-latest`)

## TL;DR

`gpui-agent` today is a **transport**: it serves pull-based ops (snapshot /
click / assert / invoke) over NDJSON and leaves all deciding and watching to
the caller. The three biggest wins available, in order:

1. **Event/log stream + cached snapshot revision** — the "console.log /
   stream of data" ask. Today there is *no* push channel and `wait_until`
   rebuilds the **entire `UiTree` every ≤10 ms** to poll one assert. A host
   event ring + `tree_changed` notify + `snapshot --since <rev>` diff kills
   the poll loop and gives agents the streaming context browsers give via
   CDP `console`/`Runtime` events.
2. **Action-space + Instinct local policy** — port `aui-policy`'s
   `InstinctPolicy` (~350 lines of logic) so `recipe resolve` / a future
   `gpui-agent run --goal` picks ops deterministically in microseconds
   instead of LLM round-trips. Abstain over guess; escalate only on abstain.
3. **Gate/ticket revalidation + optional `jev` policy** — port
   `aui-guard`'s target-scoped fingerprint check (accuracy: never act on a
   stale node) and `aui-cli`'s `TypesafeTransport` (a closed wire to jev:
   the model may only ever name an offered op/id or abstain).

## What each repo actually is

### instinct (hexuria/instinct)

Small, domain-agnostic **decision engine**, pure Rust, sync, no floats in
the decision path, no clocks/RNG/HashMap — same input + same `DataVersion`
+ same profile = byte-identical `Decision`. Runs in µs–ms on CPU, no
network.

Key API (`instinct/README.md`, `instinct-core`):

- `CandidateSet` + `Scores` + `arbitrate` → `Answer::{Choice, Abstain}` —
  choose among a finite set of ids. `CandidateSet::chosen` is `None` on
  abstain, so **an abstain can never be acted on by mistake**.
- `instinct_lexicon::overlap` — integer text evidence for open-text
  candidates.
- `RuleClassifier` (instinct-rules) — data-driven text→Decision pipeline
  from `ClassifierSpec` (lexicon/rules TOML).
- `Confidence` is 0–1000 integer millis; one threshold-and-margin gate.
  Profile table (`instinct-core/src/profile.rs`): Fast 850/200,
  Standard 750/150 (default), Deep 650/100 (min_confidence/min_margin).
- `DataVersion` — blake3 over domain, data, seeds, profile table, crate +
  Unicode versions; every decision is attributable/replayable
  (`instinct-explain` trails). Same idea fits recipe receipts.
- Consumers pin by **git rev** (`Cargo.toml`), same pattern aui uses.
- Note: instinct contains **no** browser/network/streaming code — it is
  pure `fn` over in-memory data. The event/context work is ours to design.

### ultra-instinct (hexuria/ultra-instinct)

Rust-native **browser-agent runtime** — the reference consumer. The loop
(`AGENTS.md`, `crates/aui-agent/src/agent.rs`):

```
observe → ActionSpace → policy(instinct|jev|…) → gate → ActionTicket
→ execute_ticketed → settle → observe → diff/verify → history
```

- `aui-policy/instinct_policy.rs` — two-head decide: **operation head**
  (click / type_text / select / scroll / wait / done / blocked) then
  **target head**, each a `CandidateSet` + `arbitrate`. Evidence is
  `evidence.rs::score_action` — `instinct_lexicon::overlap` on goal text vs
  label, a `target_phrase` stripper (`Type "rust" into Search` → `Search`),
  exact-label saturation cap, and **habituation** (repeated no-effect/wrong-
  effect lowers the urge by `HABITUATION_STEP = 250` so it won't loop).
- `aui-guard` — hard gate + one-shot `ActionTicket` + `WorldSnapshot`:
  ledger → **fresh observe** → `revalidate` (target-scoped fingerprint:
  front layer, focus, clickable/occluded set within 160px neighborhood) →
  mark consumed **before** dispatch. `WorldChanged`/`TargetGone`/`Target-
  Changed` = *stale* → discard and re-decide; gate refusal = never retried
  silently. "Stale ≠ spent."
- `aui-observe` — id-based `ManifoldDiff` (added/removed/changed with field
  lists) + `SnapshotRing` for temporal matching. Structural similarity
  matching exists for unstable ids — **not needed** for gpui-agent (ids are
  stable by contract).
- `aui-cdp` — CDP transport incl. a **replay transport** for deterministic
  tests; `call_budget.rs` pins the CDP call count per observe as a perf
  tripwire.
- `aui-cli` — `aui run --policy instinct|jev|clef`; `typesafe.rs` is the
  jev adapter.

### typesafe-sdk-rust (codeitlikemiley/typesafe-sdk-rust)

Blocking/async client for `POST /v1/systemone` (`api.typesafe.ai`), model
`jev-latest` (`jev-preview` for box questions). Question kinds: `noul`
(yes-probability), `choice` (label pick + per-choice probabilities),
`score` (rubric), `box` (image region). `TYPESAFE_API_KEY`, `TYPESAFE_MODEL`,
`TYPESAFE_BASE_URL`. The `blocking` feature fits gpui-agent's sync codebase —
**no tokio needed**. The `wire` module can serve a mock System One for CI
(no network in tests).

The aui jev adapter (`crates/aui-cli/src/typesafe.rs`) is the shape to copy:

- One closed request `{goal, actions:[{id,kind,label,role,state}], history}`
  → **one** System One POST carrying an `operation` choice question plus a
  `<kind>_target` choice question per element-bearing op (speculative
  fan-out, one network call per step).
- Reply validation is strict: chosen id must be on the offered menu,
  probabilities must cover exactly the offered set and sum ≈1, choice must
  be argmax. Anything off-menu → hard error, never executed.
- Reply is only `{"choice":{"id","kind"}}` or abstain — the model can never
  emit selectors, coordinates, or scripts.

## gpui-agent today (measured gaps)

| Gap | Evidence |
| --- | --- |
| Pull-only; **no event/log/console stream** | `Op` enum in `protocol.rs` has 13 ops, none push. Apps cannot stream logs/metrics to the agent. |
| `wait_until` polls `host.snapshot()` every ≤10 ms — **full tree rebuild per poll** | `dispatch.rs:191-214`, `server.rs:373-412` (mailbox path posts an Assert per poll → a UI-thread roundtrip per poll on desktop) |
| `assert` also rebuilds the whole tree per call | `dispatch.rs:116` (`assert_tree(&host.snapshot(), …)`) |
| `snapshot` always serializes the whole tree; no diff/subtree/fields | `UiTree`/`UiNode` in `tree.rs`; ~43 µs serialize for 100 nodes (`PERF.md`) — small today, grows with real apps |
| Every decision is the caller's (LLM) problem — no local policy, no resolve beyond a static schema | `recipe resolve` maps prose through a local schema, fail-closed (`docs/RECIPES.md`) |
| No pre-dispatch revalidation — a stale id between snapshot and click clicks whatever the id now means | semantic `dispatch` calls the handler by id directly (`INTEGRATING.md`) |

Done already (don't redo): session reuse ~600×, `rpc_pipeline` for
all-Read waves ~3.7×, `flatten_into`, mailbox `mem::take`, NDJSON buffer
reuse. Rejected by measurement: simd-json, tokio, threads-on-tree, wire
`batch` op, allocator swaps (`PERF.md`).

## Proposals

### 1. Event/log stream — the console.log ask (protocol v2 additive)

Give `AgentHost` a bounded event ring and two ways to read it:

```jsonc
{"v":2,"id":"9","op":"events","since":41}            // poll (cheap)
{"v":2,"id":"10","op":"subscribe","kinds":["log","tree_changed","metric","invoke_result","error"]}
// after subscribe, host may emit unsolicited lines on the same connection:
{"v":2,"event":{"seq":42,"kind":"log","level":"info","target":"todo-list","message":"rendered 128 rows","data":{}}}
```

- Apps bridge `tracing`/`log` into the ring — the literal `console.log`
  analog; `invoke` results and store mutations can also emit.
- **Perf side-effect:** `wait_until` stops rebuilding `UiTree` per 10 ms
  poll — host caches the last built tree + `revision: u64`, bumps on
  change, serves `tree_changed` events. Poll-against-cache is the fallback
  when a host can't notify.
- Keep caps honest: ring ≤ some bound (e.g. 1024), events count toward the
  1 MiB line cap, idle timeout unchanged.
- aui precedent: this is the CDP `Runtime.consoleAPICalled` / event-domain
  equivalent — the piece gpui-agent's README comparison table doesn't have.

### 2. Snapshot revision + `diff` op

- Host stamps each `UiTree` with `rev`. `assert`/`wait_until` evaluate
  against the cached tree while rev is unchanged — zero rebuilds on poll.
- `{"op":"snapshot","since":<rev>}` → `{rev, diff:{added,changed,removed}}`
  instead of the full tree. Port `aui-observe`'s id-based diff — trivial
  here because ids are stable; skip its structural-similarity matching.

### 3. Action-space + Instinct local policy (deterministic speed + accuracy)

New thin consumer crate (e.g. `gpui-agent-run`, or inside
`gpui-agent-recipe::resolve`), mirroring `aui-policy`:

- `UiTree` → `ActionSpace`: each `visible && enabled` node yields offered
  ops by role — button→`CLICK:<id>`, textbox→`TYPE_TEXT:`/`SET_VALUE:`,
  checkbox→`CLICK:`, scroll→`SCROLL_*`, every `keybindings` row →
  `KEYBINDING:<id>`, every `invoke` name → `INVOKE:<name>`; controls
  `WAIT`, `DONE`, `BLOCKED`.
- Port `evidence.rs::score_action` almost verbatim: `instinct_lexicon::
  overlap` on goal vs `node.name` (+role), `target_phrase` stripper for
  `Type "x" into field`, exact-label cap, non-exact ceiling 849.
- Two `CandidateSet`+`arbitrate` heads (operation → target) with
  habituation from step history — directly portable, ~350 lines of logic
  (`instinct_policy.rs:44-382`).
- Win: `recipe resolve` and a `run --goal` loop decide routine steps in
  **µs locally, byte-identical, no network**. Abstain → escalate to the
  LLM/jev — aui has this exact seam: `EscalatingPolicy<L,H>` calls the
  heavier tier **only** on abstain and never promotes the top-ranked
  candidate silently (`aui-policy/src/escalate.rs`).
- Even before any policy work, port the abstain shape into `resolve`:
  score candidates 0–1000, min-confidence + min-margin + exact-tie →
  return a ranked abstain listing candidates instead of silently picking.
- Also gives CI deterministic goldens (`DataVersion`-pinned receipts).
- Pin `instinct-*` by git rev like aui does (rev `a42d16b…` in aui's
  workspace `Cargo.toml`; no crates.io release yet).

### 4. Gate/ticket: revalidate before dispatch (accuracy)

- Add a **derived per-node `fingerprint: u64`** (blake3/xxhash over
  id+role+name+quantized bounds+states+parent) — the `aui-core` region
  fingerprint idea. Enables ticket staleness checks, the `diff` op's
  `changed` list, and receipts that record *which tree* a step ran on.
- Before a mutating op executes, issue a one-shot ticket carrying
  `{op, node_id, node_fingerprint, neighborhood_fingerprint}`; re-read
  just the node (+ neighborhood) before dispatch — not a full snapshot.
  Mismatch → `world_changed` / `target_gone` (stale → re-decide, bounded),
  never a silent mis-click. **Consume the ticket before dispatch** so a
  retry can't double-fire.
- Neighborhood scope = ancestors + same-parent siblings + bounds-nearby
  peers (aui uses `NEIGHBOR_RADIUS_PX = 160`); the front-layer notion maps
  to open `role=dialog` + `visible` nodes — no DOM occlusion machinery
  needed here.
- In recipes this is a cheap `gate` precondition step; as protocol it can
  ride `assert` or a `dispatch` option. `stale ≠ spent`: stale errors
  re-observe and re-decide; gate refusals are never silently retried.

### 5. `jev` remote policy (feature-gated, optional)

- Port `aui-cli/src/typesafe.rs` nearly verbatim onto the gpui action
  space: closed `{goal, actions, history}` request → one System One POST
  (operation + per-kind `*_target` questions — the single-call fan-out) →
  validate (offered ids only, probs cover set, argmax) → choice or abstain.
- `typesafe_sdk::blocking::Client` — sync, no async runtime added.
  Feature `jev`; `TYPESAFE_API_KEY`; mock server in tests via the SDK's
  `wire` module.
- Extra jev surfaces later: `score` questions as step verification
  ("is the goal visibly satisfied"), `noul` gates before destructive ops,
  `box` questions on screenshots (needs `jev-preview` + the existing
  macOS PNG path) for visual grounding.
- Same anti-drift rule as aui: the model can only ever name an offered
  op/id; selectors/coordinates/scripts = hard error.

### 6. Cheap engineering transfers

- **Call-budget tripwire** (`aui-browser/tests/call_budget.rs` analog): pin
  the protocol-op count per recipe run — catches accidental re-snapshot
  loops when the diff/event work lands.
- **Replay transport** (`aui-cdp` pattern): `trait AgentTransport` over
  `Request/Response` with live-TCP and `ReplayTransport` impls reading
  recorded `{"calls":[{request,response}]}` fixtures → recipe/MCP tests
  and benches run with no app at all.
- **Bounded control state** (`aui-browser/compact.rs`): cap `UiNode.value`
  (e.g. ≤200 chars) and any future option lists; never read password
  fields. Keeps the 1 MiB line safe and snapshots cheap.
- **Strict symmetric serde** (`typesafe-sdk` `wire.rs` model): server
  deserialization rejects everything the client refuses to send — tighten
  `protocol.rs` beyond serde defaults.
- One blocking socket per agent; parallel agents = separate connections —
  already true (`MAX_CONNECTIONS`).

### Where gpui-agent is already ahead (don't regress)

Real `screenshot` op (aui has none), HMAC challenge auth, `wait_until`
server-side polling, virtual-input delivery modes, `in_viewport`
fail-closed semantics. Aui's MCP has **no** push/subscription either —
the event stream proposed above would put gpui-agent ahead of both.

## Suggested order

1. Event ring + cached revision (context ask + biggest perf win; additive)
2. `snapshot --since` diff
3. Action-space + `InstinctPolicy` port in resolve/`run --goal`
4. Target-scoped gate revalidation
5. `jev` feature on the same closed wire

## Constraints to keep (repo rules)

No OS HID; loopback + caps + token policy unchanged; `delivery=semantic`
stays default; app-specific verbs stay out of the CLI (a policy layer is
generic — ops and `invoke` names come from the host, not new subcommands);
new ops are additive = protocol v2 minor bump, not v3; ask before product
forks (e.g. making a hosted `jev` a required dep). Instinct stays an
optional policy — `gpui-agent` the protocol doesn't become an agent
framework, same way `aui-mcp` is an optional adapter, not the loop.
