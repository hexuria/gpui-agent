# PRD: Instinct-backed gpui-agent — faster, cheaper, still accurate

- **Status:** Approved for execution · **Date:** 2026-10-17 · **Owner:** codeitlikemiley
- **Executor:** Devin (autonomous). Use the goal prompt in
  [`2026-10-17-instinct-devin-goal.md`](2026-10-17-instinct-devin-goal.md).
- **Tracking:** one GitHub issue per milestone in `hexuria/gpui-agent`, titled
  `[instinct M<n>] …`. Every task below has an ID (`M2.3`); PRs and commits
  name the IDs they close. This file is the source of truth. If an issue
  disagrees with this file, this file wins. Issues (label `instinct`):
  M0 #51 · M1 #52 · M2 #53 · M3 #54 · M4 #55 · M5 #56 · M6 #57 · M7 #58 ·
  M8 #59 (optional).
- **Inputs:** the scout report
  [`2026-10-17-instinct-jev-scout.md`](2026-10-17-instinct-jev-scout.md)
  (§2 below lists its corrections) and the reference implementation
  `hexuria/ultra-instinct` (the `aui-*` crates).

---

## 1. Problem

Today gpui-agent's only realistic driver is an LLM talking to it over MCP. Each step
costs tokens and a model round trip:

1. The LLM calls `snapshot`, which returns the **whole** `UiTree` as JSON. On a
   200-row list this dominates input tokens.
2. The LLM reads the tree, picks a node id, then calls `click` / `type` / `set_value`.
3. The LLM calls `snapshot` again to see what changed, or `wait_until`. Server-side,
   `wait_until` rebuilds the full tree every ≤10 ms poll
   (`crates/gpui-agent/src/dispatch.rs:191-214`). On desktop that's one UI-thread
   mailbox round trip per poll (`server.rs:373-412`).
4. Nothing checks freshness. If the tree changes between the snapshot and the
   click, the click lands on whatever that id points to now.

`recipe resolve` (`crates/gpui-agent-recipe/src/resolve.rs:28-63`) already
contains a hand-rolled, weaker version of Instinct: keyword scoring, a fixed
threshold of 2, and fail-closed only on **exact** ties, with no margin.

## 2. What the scout got right and wrong (verified 2026-10-17)

| Scout claim | Verdict |
|---|---|
| `wait_until` and `assert` rebuild the full tree; line refs | ✅ correct (`Op` has 14 variants, not 13) |
| Only `aui-policy` uses Instinct; profiles 850/200, 750/150, 650/100; `HABITUATION_STEP=250`; `NON_EXACT_CAP=849`; `NEIGHBOR_RADIUS_PX=160` | ✅ correct |
| "Abstain escalates via `EscalatingPolicy` — exactly how aui does instinct → jev" | ⚠️ **Overstated.** `EscalatingPolicy` exists (`aui-policy/src/escalate.rs`), but only tests use it (`aui-agent/tests/remote_e2e.rs`). The shipped `ultra-instinct run` takes **one** policy, and with `--features jev` the default is jev alone (`aui-cli/src/run_cmd.rs:174-177`). Instinct-first with escalation on abstain has never been benchmarked. This PRD does it. |
| "`typesafe_sdk::blocking` — no tokio needed" | ❌ **Wrong.** `typesafe-sdk-rust/Cargo.toml`: `blocking = ["tokio/rt", "tokio/net"]`. `PERF.md` rejected tokio for gpui-agent, so jev stays behind a non-default feature and never enters `gpui-agent` core. |
| Instinct can be pinned by git rev | ❌ **Blocks publishing.** `gpui-agent`, `-recipe` and `-cli` publish to crates.io, which rejects git-only deps. Decision: publish the instinct crates first (M0). |
| (missed) Text payloads are **not** Instinct | aui splits this out (`aui-policy/src/text.rs:1`): Instinct picks *which field*, and a `TextResolver` (quoted literal first, model only on failure) supplies *what to type*. |
| (missed) Multi-step goals | `aui-policy/src/goal.rs::split_sequential_clauses` (`then` / `and then` outside quotes). |
| (missed) The MCP path is where the tokens go | The biggest saving is an `act` / `locate` MCP tool that resolves a phrase locally, plus diff and compact snapshots. A new `run --goal` loop that nobody calls today saves nothing on its own. |

## 3. Goals, non-goals, invariants

### Goals
- **G1 Cheaper:** fewer LLM input/output tokens and model calls per completed task.
- **G2 Faster:** less wall time per task. No tree rebuild when nothing changed.
- **G3 Accurate:** pass rate ≥ today's, and **zero wrong actions** from the local
  policy. Instinct abstains instead of guessing.
- **G4 Proven:** every claim is backed by a reproducible benchmark against a frozen
  baseline of the current implementation.

### Non-goals
- No OS-level HID input, no remote bind, and no change to auth or caps policy.
- No app-specific verbs in the CLI. Ops and `invoke` names come from the host or schema.
- No LLM planner inside gpui-agent. The local tier is Instinct, and any model is
  an external, optional escalation.
- No hosted dependency on the default install path.

### Invariants (enforced by tests; a PR that breaks one is rejected)
- **I1 Works with no model.** Default build, no API keys, no network: every existing
  op, MCP tool, recipe and CLI command behaves byte-for-byte as before (wire-compat
  tests in M1.6), and the new Instinct features work fully offline.
- **I2 Additive protocol.** `PROTOCOL_VERSION` stays `2`. New request fields are
  optional; new response fields are optional or omitted. Old requests such as
  `{"op":"snapshot"}` parse and answer exactly as today. Hosts advertise new
  abilities in `hello.features` (new, `skip_serializing_if = "Vec::is_empty"`).
- **I3 Core stays lean.** `crates/gpui-agent` gains no dependency on instinct,
  typesafe-sdk, tokio, or any HTTP client. CI enforces this (M1.7).
- **I4 Abstain is never acted on.** A local abstain never becomes a dispatch.
  Escalation may only return an id from the offered set; anything else is a hard
  error and nothing executes.
- **I5 Deterministic.** Same tree, goal, history and profile give a byte-identical
  decision. No floats in the decision path, no HashMap iteration order, no clocks
  or RNG.
- **I6 Stale ≠ spent.** A stale-target refusal is re-decided (bounded), never
  silently retried with the old id.

## 4. Architecture

```
                 ┌──────────────── gpui-agent-cli (published) ───────────────┐
 LLM ── MCP ───▶ │ existing tools (unchanged)                                │
                 │ + snapshot{since,format}  + locate  + act   (feature      │
                 │ `instinct`, default ON)                                   │
 shell ────────▶ │ + `gpui-agent run --goal …` [--escalate-cmd] [--policy]   │
                 └───────┬───────────────────────────────┬───────────────────┘
                         │                               │
          ┌──────────────▼─────────────┐   ┌─────────────▼──────────────────┐
          │ gpui-agent-policy (NEW,    │   │ gpui-agent-recipe (published)   │
          │ published)                 │   │ resolve_intent: feature         │
          │ ActionSpace, evidence,     │◀──│ `instinct` → arbitrate          │
          │ InstinctPolicy, Escalating,│   └─────────────────────────────────┘
          │ TextResolver, clauses,     │
          │ Runner, CommandEscalation, │
          │ feature `jev` (off)        │
          └──────────────┬─────────────┘
                         │ depends on
          ┌──────────────▼─────────────┐   instinct-core / -text / -lexicon
          │ gpui-agent (core)          │   (crates.io, published in M0)
          │ + UiTree.rev, cache, diff, │
          │ + UiNode::fingerprint,     │
          │ + expect_fingerprint gate  │
          └────────────────────────────┘
```

Feature matrix:

| Crate | Feature | Default | Pulls in |
|---|---|---|---|
| `gpui-agent` | none new | — | nothing new (I3) |
| `gpui-agent-policy` | `jev` | off | `typesafe-sdk` (blocking → tokio) |
| `gpui-agent-recipe` | `instinct` | off at first, flipped to on in M2.6 if the gate passes | `gpui-agent-policy` |
| `gpui-agent-cli` | `instinct` | **on** | `gpui-agent-policy` |
| `gpui-agent-cli` | `jev` | off | `gpui-agent-policy/jev` |

## 5. Benchmark specification (used by every milestone)

### 5.1 Arms

| Arm | Driver | Tools / path | Model |
|---|---|---|---|
| **R0** | resolve micro-bench | current `resolve_intent` | none |
| **R1** | resolve micro-bench | Instinct-backed `resolve_intent` | none |
| **A0** | LLM over MCP | **baseline**: current MCP tools, frozen at tag `bench-baseline` | Haiku 5.5 |
| **A1** | LLM over MCP | A0 tools + `snapshot{since,format:"compact"}` + `locate` + `act` | Haiku 5.5 |
| **A2** | `gpui-agent run --goal` | Instinct only, no escalation | none |
| **A3** | `gpui-agent run --goal --escalate-cmd bench/escalate_claude.py` | Instinct, Haiku only on abstain | Haiku 5.5 |
| **A4** (optional, M8) | `run --goal --policy instinct --escalate jev` | Instinct, jev on abstain | jev |

### 5.2 LLM settings (A0, A1, A3: identical)
- Model `claude-haiku-5-5`, official `anthropic` Python SDK, manual tool-use loop,
  `tool_choice` auto, `max_tokens` 16000, `output_config.effort` pinned to
  `"medium"`, default adaptive thinking.
- Same system prompt and goal text for every arm (`bench/prompts/system.md` plus a
  shared suffix: "Use only this app. If the task is impossible or ambiguous, stop
  without changing anything and say why.").
- Prompt caching enabled identically on all arms (top-level `cache_control`). Tool
  list and system prompt are byte-stable across attempts.
- Record each response's `usage.input_tokens`, `output_tokens`,
  `cache_read_input_tokens` and `cache_creation_input_tokens`.
- Price table in `bench/prices.toml`, with source and date. Haiku 5.5 on
  2026-10-06: $0.10 / MTok input, $0.50 / MTok output for prompts ≤100K tokens;
  cache-read and over-100K rates go in the file from the official pricing page at
  run time. Cost is computed from that file, never hard-coded.

### 5.3 Task suite (`bench/tasks.toml`, run against `todo-headless`)

Each task has: `id`, `goal`, `seed` (todos created via `invoke todo.add` by the
**harness** before handing over), `expect` (final state), and `kind` ∈
{`do`, `refuse`}.

| ID | Goal (verbatim) | Seed | Pass condition |
|---|---|---|---|
| T01 | `Add a todo "Buy milk"` | none | exactly one new item "Buy milk", not done |
| T02 | `Add a todo titled "Call Ana, re: Q3 (urgent)"` | none | exact title, punctuation preserved |
| T03 | `Mark "Write docs" as done` | Write docs, Ship it | Write docs done; Ship it unchanged |
| T04 | `Delete "Write docs"` | Write docs, Ship it | Write docs gone; Ship it unchanged |
| T05 | `Mark "A" done then mark "C" done` | A, B, C | A, C done; B not |
| T06 | `Open Settings` | none | page-settings visible |
| T07 | `Collapse the sidebar` | none | todo-nav not visible |
| T08 | `Delete "Buy milk"` | Buy milk, Buy milk | **refuse**: no state change |
| T09 | `Delete "Walk dog"` | Buy milk | **refuse**: no state change |
| T10 | `Delete "Buy milk"` | Buy milk, Buy milk powder | only "Buy milk" deleted (exact label beats partial) |
| T11 | `Add "Pay rent" and wait until it shows in the list` | none | item present; agent observed it |
| T12 | `Mark "Item 137" done` | Item 1…Item 200 | only Item 137 done (large-tree token stress) |
| T13 | `Delete "Item 42"` | Item 1…Item 200 | only Item 42 gone |
| T14 | `Quit the app` | none | **refuse**: destructive keybinding needs confirm; app still running |
| T15 | `Turn on delete confirmation in Settings` | none | settings-confirm-delete checked |
| T16 | `Add "Groceries" then mark it done` | none | Groceries exists and is done |

### 5.4 Ground truth and metrics
- **Ground truth is the app state**, read by the harness via `invoke todo.list`
  plus `snapshot` after the attempt. What the agent says it did is ignored, except
  that a `refuse` task needs a refusal or abstain outcome and an unchanged state.
- **wrong_action** = any state change outside `expect`, or any mutation on a
  `refuse` task. This is the accuracy metric that matters most.
- Per attempt (JSONL in `bench/results/<run-id>/<arm>.jsonl`): `task, arm,
  attempt, pass, wrong_action, refused, wall_ms, protocol_ops,
  bytes_to_model, model_calls, input_tokens, output_tokens,
  cache_read_tokens, cache_write_tokens, usd, git_sha, host_os`.
- **Caps** (enforced by the harness from outside): 20 actions, 30 model calls,
  120 s wall. Hitting a cap is a fail (`cap_hit`). A fresh `todo-headless` runs on
  a fresh port and token for every attempt.
- **n:** LLM arms n=5 per task; local arms n=20 (timing). Report pass `k/n`,
  wrong-action count, median and p90 for wall time and tokens, and mean USD per
  **passed** task.
- `bench report` writes `docs/bench/RESULTS.md` (arms × tasks table plus a
  summary) and never edits raw JSONL.

### 5.5 Targets (keep/kill gates, not promises)

| Gate | Comparison | Pass condition |
|---|---|---|
| **GATE-R** (M2) | R1 vs R0 on the ≥120-intent corpus | R1 wrong picks = 0; R1 correct ≥ R0 correct; p50 latency ≤ 50 µs |
| **GATE-P** (M3) | rev cache on vs off | `wait_until` on an unchanged tree does **0** rebuilds (counter); `snapshot since` on an unchanged tree returns `unchanged` in ≤ 1 KiB |
| **GATE-T** (M4) | A1 vs A0 | median input tokens per passed task ≤ **50 %** of A0; pass ≥ A0; wrong_action ≤ A0 |
| **GATE-L** (M6) | A2 | wrong_action = **0** across all tasks and attempts; passes ≥ 10/16 tasks with the rest abstaining; median wall per task ≤ 100 ms headless |
| **GATE-E** (M7) | A3 vs A0 | pass ≥ A0; wrong_action = 0; USD per passed task ≤ 25 % of A0 |

If **GATE-T** fails, stop after M4: write the findings into `docs/bench/RESULTS.md`
and open an issue titled `[instinct] GATE-T failed — reassess` instead of
continuing to M5+. Every other gate failure is iterated on (§7 loop) up to 3
design attempts, then escalated the same way.

## 6. Milestones and tasks

Order: M0 → M1 → M2 → M3 → M4 → M5 → M6 → M7 → M8 (optional) → M9 (optional).
M0 has a human publish step; if it is pending, M1 and M3 can proceed in parallel.
M2 and M4 onward need M0 published. Until then, develop against `[patch.crates-io]`
pointing at a local or git checkout, but never merge a manifest that only works
with the patch.

### M0 — Publish instinct crates (repo `hexuria/instinct`)
- **M0.1** Make `instinct-core`, `instinct-text` and `instinct-lexicon` publishable:
  set `publish = true` on those three only (the workspace stays `publish = false`),
  version `0.1.0`, add `version = "0.1.0"` to their path deps, and fill
  `description`, `license`, `repository`, `readme`, `keywords`, `categories`.
  Every other instinct crate stays unpublished.
- **M0.2** `cargo publish --dry-run -p` each, in order core → text → lexicon, and
  `cargo package --list` for each, with no stray files. `cargo test --workspace`
  stays green.
- **M0.3** Add a CHANGELOG entry and README "Install" section, and record in the
  ADR folder (or `DECISIONS.md`) that 0.1.0 decisions get a different
  `DataVersion` than git-rev builds because the crate version is hashed.
- **M0.4 (HUMAN)** The repo owner runs `cargo publish` for the three crates. Devin
  opens the PR, then sets the M0 issue to `blocked: human-publish` and moves on to
  M1 and M3.
- Names verified free on crates.io (2026-10-17): `instinct-core`, `instinct-text`,
  `instinct-lexicon`, `gpui-agent-policy`.
- **Done when:** `cargo add instinct-core@0.1` resolves from crates.io.

### M1 — Baseline freeze and bench harness
- **M1.1** Create tag `bench-baseline` on current `main` before any product change
  in this PRD. All A0 runs build the CLI from this tag.
- **M1.2** `crates/gpui-agent-bench` (`publish = false`): a Rust binary
  `gpui-agent-bench` with subcommands `resolve` (R0/R1), `local` (A2), `report`.
  It spawns `todo-headless` on a free port with a random token, seeds via
  `AgentClient`, runs, and reads ground truth.
- **M1.3** `bench/` (Python ≥3.12, `uv`, pinned `uv.lock`): `bench/run.py` for the
  LLM arms (A0/A1/A3). It spawns `gpui-agent mcp` over stdio, exposes the MCP
  `tools/list` to Claude as tools, and runs the manual loop and caps from §5.2–5.4.
  Shared `bench/tasks.toml`, `bench/prices.toml`, `bench/prompts/`. Arm-to-binary
  mapping lives in `bench/arms.toml`.
- **M1.4** Labeled intent corpus `bench/intents.toml` with ≥120 rows
  `{intent, expect: <schema name> | "abstain"}`. It covers every schema in
  `examples/schemas/todo.json` plus protocol ops, paraphrases, near-duplicates,
  typos, out-of-domain prose, and ≥20 rows that must abstain.
- **M1.5** Run R0 and A0 (n=5) and commit `docs/bench/RESULTS.md` with the
  **baseline** section. This is the number every later gate compares against.
- **M1.6** Wire-compat golden tests in `crates/gpui-agent/tests/wire_compat.rs`:
  record request/response pairs for every existing op from `bench-baseline` and
  assert byte-identical responses after any change. This protects I1 and I2.
- **M1.7** CI job `deps-guard`: fail if `cargo tree -p gpui-agent -e normal`
  contains `instinct`, `typesafe`, `tokio`, `reqwest`, `ureq` or `hyper`.
  This protects I3.
- **M1.8** CI job `bench-local`: run R0 (and R1 once it exists) plus A2 (once it
  exists) on every PR, with no network, and upload the JSONL as an artifact. The
  LLM arms are manual only (`workflow_dispatch`, secret `ANTHROPIC_API_KEY`).
- **Done when:** baseline numbers are committed and both CI jobs are green.

### M2 — Instinct-backed `resolve` (GATE-R)
- **M2.1** Create crate `crates/gpui-agent-policy` (published, `#![forbid(unsafe_code)]`,
  depends on `gpui-agent` and the instinct crates at `0.1`). Add the module
  skeleton `arbitration`, `evidence`, `action_space`, `policy`, `text`, `goal`,
  `runner`, `escalate`.
- **M2.2** `evidence`: port `aui-policy/src/evidence.rs` (`score_action`,
  `target_phrase`, the exact-label saturation, `NON_EXACT_CAP = 849`,
  `UNNAMED_OPERATION_CAP = 500`) onto gpui labels. A target's evidence text is its
  own `name` plus `role`. Ancestor names are **not** concatenated: todo row buttons
  already carry the title, e.g. `Delete Buy milk`.
- **M2.3** `arbitration::choose(candidates: &[(id, Confidence)], profile) -> Answer`,
  a thin wrapper over `CandidateSet` + `Scores` + `arbitrate`, shaped like
  `aui-policy/src/instinct_policy.rs:255-300`. Default profile `Standard`
  (750/150).
- **M2.4** In `gpui-agent-recipe` behind feature `instinct`: `resolve_intent` scores
  each schema with `instinct_lexicon::overlap` (intent vs `name` + `keywords` +
  `description`) and decides with `arbitration::choose`. On abstain it returns
  `Err` with a **ranked** list (top 5: name and confidence millis). On choice,
  `ResolveResult.confidence` stays `f32` for compatibility, and a new optional
  field `confidence_millis: Option<u16>` is added. The old scorer stays the
  default path when the feature is off.
- **M2.5** Tests: every corpus row as a table test, a determinism test (permute
  registry order 10× and get identical output), and a property test (proptest)
  that an abstain never produces an `Op`.
- **M2.6** Run R0 vs R1. If GATE-R passes, make `instinct` a default feature of
  `gpui-agent-recipe` and record the result.
- **Done when:** GATE-R passes and is recorded in RESULTS.md.

### M3 — Tree revision cache and diff (GATE-P)
- **M3.1** Core: add `fn revision(&self) -> Option<u64> { None }` to `AgentHost`
  (additive default) and `UiTree.rev: Option<u64>` (serde `default`,
  `skip_serializing_if = "Option::is_none"`).
- **M3.2** Server: keep a `TreeCache { rev, tree: Arc<UiTree> }` plus a ring of the
  last 8 trees by rev. `Assert`, `WaitUntil` and `Snapshot` reuse the cache when
  `host.revision() == Some(cached.rev)`. A `None` revision means rebuild every
  time, exactly as today. Add a rebuild counter visible to tests (`cfg(test)` or
  a `testing.rs` hook; no production-only test seam).
- **M3.3** `todo-core`: bump the revision on every state mutation (one counter;
  every `&mut self` path that changes items, draft, page, sidebar or settings).
  Desktop `apps/todo`: bump wherever it calls `cx.notify()` for model changes.
  Document the contract in `docs/INTEGRATING.md`: "bump revision on every change
  that can alter the snapshot; when unsure, return `None`".
- **M3.4** Wire: `snapshot` accepts optional `since: u64` and
  `format: "json" | "compact"`, and responses carry `rev` when known.
  - `since == current` → `{"rev":N,"unchanged":true}`
  - `since` still in the ring → `{"rev":N,"diff":{"added":[UiNode…],"removed":[id…],"changed":[{"id","fields":{…new values…}}]}}`.
    Diff by id over the flattened tree. `added` nodes carry `parent`.
  - otherwise the full tree plus `"full":true`.
  - `format:"compact"` returns one line per visible node:
    `id␠role␠"name"␠[checked|disabled|focused|value="…"]`, depth-indented, with
    values ≤ 200 chars and never password field values.
  Advertise `"rev"`, `"snapshot_since"` and `"snapshot_compact"` in
  `hello.features`.
- **M3.5** Desktop mailbox `wait_until`: with a known revision, check
  `host.revision()` on the TCP thread via a cheap mailbox `Revision` probe and post
  the assert only when the revision changed.
- **M3.6** Tests: diff property tests (apply(diff(a, b), a) == b over generated
  trees), the rebuild counter is 0 for `wait_until` on an unchanged tree, wire
  compat still byte-identical for requests without the new fields, and a criterion
  bench added to `agent_perf.rs` for a 1000-node `wait_until`.
- **Done when:** GATE-P passes and the `PERF.md` numbers are updated.

### M4 — `locate`, `act`, compact MCP (GATE-T, the main token win)
- **M4.1** `action_space`: map `UiTree` → offered actions. Hidden or disabled nodes
  are **filtered** before scoring, never penalized. Op mapping:
  `button|link|tab|listitem(nav)|checkbox|switch` → `CLICK:<id>`;
  `textbox` → `TYPE:<id>` and `SET_VALUE:<id>`; textbox submit → `KEY:<id>:Enter`;
  each `keybindings` row → `KEYBINDING:<action>`, plus each registered invoke
  schema → `INVOKE:<name>` (only when a schema registry is loaded); controls
  `DONE`, `BLOCKED`, `WAIT`. Destructive actions (keybindings where
  `gpui_agent::keybinding::binding_is_dangerous` is true, schemas whose
  `effects` contain `Effect::Exit`, and any op `binding_is_dangerous` would
  require `confirm=true` for) are marked and **never auto-chosen**: choosing one
  returns abstain `needs_confirmation`. The runner and `act` never set
  `confirm=true` themselves.
- **M4.2** `policy::InstinctPolicy`: port the two-head decide (operation head, then
  target head) and habituation (`HABITUATION_STEP = 250`) from
  `aui-policy/src/instinct_policy.rs:44-382`. Same `BrowserPolicy`-shaped trait,
  named `UiPolicy`.
- **M4.3** `text::TextResolver`: a deterministic resolver that takes the quoted
  literal from the clause, exact and unaltered. No quote, or more than one
  candidate quote → `TextError::Missing` / `Ambiguous` → the step abstains. Port
  the trait shape from `aui-policy/src/text.rs`.
- **M4.4** Gate prerequisite for `act`: `act` verifies the target with M5's
  `expect_fingerprint` once M5 lands. Until then it re-reads the node by id
  immediately before dispatch and aborts on role or name change.
- **M4.5** MCP tools in `gpui-agent-cli` (feature `instinct`):
  - `locate {query, k?=5}` → `{"answer":"choice"|"abstain","candidates":[{"id","role","name","op","confidence"}]}`.
    Read-only.
  - `act {instruction, delivery?}` → runs **one** clause:
    `{"done":{"op","id","text?"},"rev":N,"diff":{…}}` or
    `{"abstain":{"reason","candidates":[top5]}}`. Never dispatches on abstain.
  - `snapshot` gains `since` and `format`.
  - Tool descriptions are short and teach the cheap path: "Prefer `act` with a
    plain instruction; use `snapshot format=compact since=<rev>` to look."
    Existing tool definitions stay byte-identical.
- **M4.6** Tests: action-space golden fixtures (todo trees, including 200 rows),
  determinism and permutation tests, T08/T09/T10/T14 semantics as unit tests
  (abstain, abstain, exact wins, needs_confirmation), and an MCP integration test
  driving `todo-headless` through `act`.
- **M4.7** Run A1 (n=5) vs A0 and record. Check **GATE-T** and apply §5.5's
  stop rule if it fails.
- **Done when:** GATE-T passes and is recorded.

### M5 — Target fingerprint gate (accuracy)
- **M5.1** Core: `UiNode::fingerprint(&self, parent_id: Option<&str>) -> u64`,
  FNV-1a 64 over `id, role, name, enabled, visible, checked, value-hash, states
  (in order), parent_id`. Bounds are excluded because semantic delivery doesn't
  depend on them; document why. The fingerprint is not serialized by default;
  `snapshot` exposes it only with `fingerprints:true`.
- **M5.2** Wire: mutating ops (`click`, `type`, `set_value`, `key`) accept optional
  `expect_fingerprint: u64`. The host checks it on the dispatch path **in the same
  call** (on the UI thread for mailbox hosts) before dispatching. Mismatch →
  error `stale_target: …`; missing node → `target_gone: …`. Nothing dispatches.
  Advertise `"expect_fingerprint"` in `hello.features`.
- **M5.3** `act` and the runner always send `expect_fingerprint`. On
  `stale_target` / `target_gone` they re-observe and re-decide at most twice
  (`max_consecutive_stale = 2`), and never resend the old id unchanged (I6).
- **M5.4** Tests: a race test (mutate the tree between decide and dispatch and get
  a stale error with no state change), property tests on fingerprint stability
  (same node gives the same fingerprint across runs and platforms; any field
  change gives a different one), and wire compat for ops without the field.
- **Done when:** the race test is green and the A1 rerun shows wrong_action = 0.

### M6 — `gpui-agent run --goal` local runner (GATE-L)
- **M6.1** `goal::split_sequential_clauses`: port from `aui-policy/src/goal.rs`,
  with the same documented limits.
- **M6.2** `runner::Runner`: loop observe (`snapshot since`) → action space →
  policy → gate → dispatch → diff. Advance the clause on a non-empty diff consistent
  with the op. Mark no-effect in history (habituation). Caps: `max_steps 20`,
  `max_consecutive_no_effect 3`, `max_consecutive_stale 2`, `max_wait`.
  Outcomes: `Done | Blocked{reason} | Abstained{reason, candidates} | CapHit`.
- **M6.3** CLI `gpui-agent run --goal "<text>" [--profile fast|standard|deep]
  [--json]`. It prints a receipt (steps, ops, decisions with confidence millis,
  instinct crate versions, final rev). Exit codes: 0 done, 2 abstained or blocked,
  1 error.
- **M6.4** Tests: every T-task as a `todo-headless` integration test with the
  expected outcome, golden receipts (byte-identical across two runs), and a
  call-budget tripwire asserting protocol op counts per task (catches accidental
  re-snapshot loops).
- **M6.5** Run A2 (n=20) and record. Check GATE-L.
- **Done when:** GATE-L passes and is recorded.

### M7 — Escalation on abstain (GATE-E)
- **M7.1** `escalate::EscalatingPolicy<L, H>`: port `aui-policy/src/escalate.rs`
  semantics exactly. The fallback runs **only** on abstain and must return a choice
  from the same offered set.
- **M7.2** `escalate::CommandEscalation`: spawn `--escalate-cmd <program> [args…]`
  per abstain. Use a closed JSON wire over stdin/stdout, a timeout (default 30 s),
  and a stdout cap of 64 KiB.
  - Request: `{"v":1,"kind":"choose","goal","clause","actions":[{"id","op","role","name","state"}],"history":[{"op","id","effect"}],"abstain":{"reason","ranked":[…]}}`
    or `{"v":1,"kind":"text","goal","clause","field":{"id","name"}}`.
  - Reply: `{"choice":{"id","op"}}` | `{"text":"…"}` | `{"abstain":"reason"}`.
  - Validation: off-menu id or op, extra fields, non-JSON, or timeout → hard error;
    nothing executes and the outcome is `Blocked`. Text replies are ≤ 256 chars,
    single line, and never written into password fields.
  - Model text is a separate resolver tier (`ModelTextResolver` shape from
    `aui-policy/src/model_text.rs`): deterministic first, command only on
    `Missing`.
- **M7.3** `bench/escalate_claude.py`: implements the wire with Haiku 5.5 (same
  settings as §5.2) and logs usage to the attempt JSONL via a side file.
- **M7.4** Tests: a scripted escalator fixture (correct choice, off-menu → hard
  error, timeout → blocked, abstain stays abstain), and an assertion that the
  escalator is never invoked when Instinct chooses.
- **M7.5** Run A3 (n=5) vs A0 and record. Check GATE-E.
- **Done when:** GATE-E passes and is recorded.

### M8 (optional) — `jev` policy
- **M8.1** Feature `jev` on `gpui-agent-policy` and `gpui-agent-cli`.
  `typesafe-sdk` with `blocking`. Port `aui-cli/src/typesafe.rs`: one System One
  POST per step with an `operation` choice question plus a `<op>_target` choice
  question per element-bearing op. Strict validation: offered ids only,
  probabilities cover exactly the offered set and sum to ≈1, choice is the argmax.
- **M8.2** CLI `--escalate jev` (needs `TYPESAFE_API_KEY`). Without the key it is a
  clear config error, never silent.
- **M8.3** Tests use the SDK `wire` mock server. No network in CI. Check that the
  default build's `cargo tree` has no `typesafe` (extends M1.7).
- **M8.4** If `TYPESAFE_API_KEY` is available in the environment, run A4 and
  record it. Otherwise record "not run: no key". **Don't** block on this.

### M9 (optional) — Event ring for context

Bounded, host-owned event stream so agents can see *what the app said happened*
(log lines, lifecycle notices) without scraping stderr. Poll-only; no push
subscription in this milestone.

**Semantics.** `EventRing` (in `gpui-agent`, no new deps — I3): a
`Mutex<VecDeque<AgentEvent>>` with capacity `EVENT_RING_CAPACITY = 1024`.
`AgentEvent { seq, level, target, message }` — `seq` is a `u64` allocated
monotonically from 1 at `push`; **there is no clock field** (I5: same app
behavior yields the same stream). `level` is `trace|debug|info|warn|error`;
`target` is the emitting component (module path / subsystem name); `message` is
the pre-rendered line. `push` past capacity evicts the **oldest** event.

**Ownership.** The host owns the ring. `AgentHost` gains one defaulted method:

```rust
fn event_ring(&self) -> Option<&EventRing> { None }
```

A host that emits events holds an `EventRing` and returns `Some(&ring)`; every
existing impl compiles unchanged (I2). Bridging `log`/`tracing` is the app's
job — `ring.push(level, target, message)` directly, or a thin `Layer` in the
app's own crate. `gpui-agent` never depends on tracing/log.

**Wire (additive, `PROTOCOL_VERSION` stays 2).**

```json
{"op":"events","since":12,"limit":100}
→ {"ok":true,"result":{"events":[{seq,level,target,message}…],
   "latest":13,"lagged":false}}
```

- `since` omitted → all retained events; `since:N` → events with `seq > N`.
- `limit` defaults to 256, hard-capped at 1024.
- `lagged: true` when `since + 1` is below the ring's base seq — the caller
  *knows* it dropped events rather than silently seeing a gap.
- `latest` is the highest allocated seq (0 when empty), so a poller can advance
  its cursor without re-receiving.
- On a host with no ring: `{"ok":false,"error":"events unsupported on this
  host"}` — fail-closed and explicit, like `screenshot_unavailable`.
- `hello.features` gains `"events"` iff `event_ring()` returns `Some` — clients
  detect support without a probe error.

**Consumers.**
- `AgentClient::events(since, limit)` → typed `EventPoll`.
- CLI `gpui-agent events [--since N] [--limit N]` prints the poll as JSON.
- MCP tool `events {since?, limit?}` → `{events, latest, lagged}` — **ungated**:
  it is a protocol op like `snapshot`, not a policy feature.
- `dispatch` handles `Op::Events` before host mutation ops; it never touches the
  decision path, the fingerprint gate, or `execute_ticketed`.

**Non-goals (explicit).** No `subscribe`/push lines (follow-up). No runner or
receipt coupling (follow-up). No persistence across restarts. No cross-process
fan-out. Apps that emit nothing return `Some(ring)` with an empty stream —
that is valid, and `"events"` is still advertised.

**M9 tasks**
- **M9.1** `event.rs`: `AgentEvent`, `EventLevel`, `EventPoll`, `EventRing`
  (`push`/`since`/`latest`/`is_empty`/`capacity` bounded), `EVENT_RING_CAPACITY`.
- **M9.2** `host.rs` + `dispatch.rs` + `protocol.rs`: `event_ring()` default,
  `Op::Events`, `hello.features` advertising, unsupported-host error.
- **M9.3** `AgentClient::events` + CLI `events` subcommand + MCP `events` tool.
- **M9.4** `todo-headless` (test app) pushes one `info` event per
  `todo.*` invoke — the e2e fixture proving the bridge recipe.
- **M9.5** Tests: ring order/eviction/`lagged`/seq monotonicity; protocol
  round-trip; unsupported host error; features advertisement; CLI + MCP
  integration; `hello`/non-event ops byte-identical.
- **Done when:** all tests pass, no decision-path code touches `EventRing`,
  and the events op is inert for hosts without a ring.

## 7. Engineering workflow (every task)

1. Branch from latest `main`: `instinct/m<N>-<slug>`. One PR per milestone;
   split it when the diff exceeds ~800 changed lines (excluding fixtures and
   lockfiles).
2. Before coding, write or extend the failing tests that encode the task's
   acceptance criteria (behavior-level, through public APIs).
3. Implement the smallest change that passes. Match the surrounding code style
   and comment density.
4. Local gate, all must pass:
   ```
   cargo fmt --all --check
   cargo clippy --workspace --all-targets --all-features -- -D warnings
   cargo test --workspace --all-features
   cargo test --workspace --no-default-features   # I1: no-model path
   cargo doc --workspace --no-deps                # RUSTDOCFLAGS="-D warnings"
   cargo audit                                    # same version as CI (0.22.2)
   ./scripts/ci-recipe.sh
   ```
   plus the milestone's bench and gate when it has one.
5. Mutation check on new logic: `cargo mutants -p gpui-agent-policy` (and the
   touched modules of core). Every surviving mutant in decision, arbitration,
   fingerprint or diff code is either killed with a new test or justified in the
   PR body.
6. Self-review with the checklist in §8, fix what it finds, and rerun step 4.
7. Open the PR with: task IDs closed, what changed, test evidence (commands plus
   summary), bench and gate numbers with links to the JSONL artifact, and
   deviations from this PRD with reasons. Use conventional commit titles
   (`feat(policy): …`, `test(core): …`, `bench: …`).
8. Wait for CI. On red: read the logs, reproduce locally, fix, push. Never skip,
   `#[ignore]`, loosen or delete a test to get green. Merge only on green CI (or
   leave it for the owner if branch protection requires review), then tick the
   tasks in the milestone issue.

## 8. Definition of done and review checklist

- [ ] Acceptance criteria of every task ID in the PR are met and tested.
- [ ] Invariants I1–I6 hold: wire-compat, deps-guard, determinism and stale tests are green.
- [ ] No `unwrap()` / `expect()` on runtime input in library code. `expect`
      only on proven invariants, with a message naming the invariant.
- [ ] New public types have rustdoc. Errors are typed (`thiserror`), and
      wire error strings are stable prefixes (`stale_target:`, `target_gone:`,
      `needs_confirmation:`).
- [ ] No new `unsafe`. New crates carry `#![forbid(unsafe_code)]`.
- [ ] No test-only parameters or branches in production APIs. Test hooks live in
      `testing.rs` or `cfg(test)`.
- [ ] Every bug found and fixed got a regression test first.
- [ ] Docs updated: `PROTOCOL.md` (new fields and features), `INTEGRATING.md`
      (revision contract), `PERF.md` (numbers), `RECIPES.md` (resolve),
      `README.md` (MCP `act`/`locate`, `run --goal`).
- [ ] Bench numbers committed to `docs/bench/RESULTS.md` with run id and git sha.

## 9. Human-only steps (Devin pauses only for these)

| Step | Who | When |
|---|---|---|
| `cargo publish` instinct-core, -text, -lexicon | owner | M0.4 |
| `ANTHROPIC_API_KEY` as repo secret and local env for LLM arms | owner | before M1.5 |
| `TYPESAFE_API_KEY` | owner | M8.4, optional |
| crates.io publish of `gpui-agent-policy` and the version bump via `just publish` | owner | after M7 |
| GATE-T or a gate failing three times | owner decides | §5.5 |

Everything else is in scope for the executor without asking.

## 10. Risks

| Risk | Mitigation |
|---|---|
| Instinct abstains too often on terse UI labels, so A2 coverage is low | That's acceptable by design: A3 escalates. Tune evidence (role words, exact-label caps) against the corpus, never by lowering the profile below Standard by default. |
| Hosts that never implement `revision()` see no speedup | Fallback is today's behavior (I1). The todo apps show the contract. |
| Haiku performs differently with new tool descriptions | Both arms share the system prompt. Existing tool definitions stay byte-identical, and A1 only adds tools. |
| Bench flakiness | Fresh daemon per attempt, harness-side caps, n=5 or n=20, and medians plus p90 instead of single runs. |
| crates.io publish delayed | M1 and M3 proceed. M2 develops behind `[patch.crates-io]` but doesn't merge until published. |

## 11. Deviations from this PRD (as implemented)

Decisions that diverged from the text above, each deliberate and verified:

- **Feature defaults stay opt-in.** §4's matrix (CLI `instinct` default-on,
  recipe flipped on in M2.6) was superseded by the explicit backward-compat
  requirement: existing users get zero behavior change unless they build with
  `--features instinct`/`jev`. GATE-R passed; the flip was never applied.
- **Ambiguity guard (M7+).** When two ranked contenders share the same
  `(kind, label)` — e.g. two identical `Buy milk` rows — no fallback can
  disambiguate them either, so `EscalatingPolicy` keeps the local abstain
  and never calls the endpoint. Stricter than M7.1's "fallback runs on every
  abstain"; proven necessary by the live A4 T08 bug (5/5 wrong picks).
- **`snapshot` tool description/schema rewritten** — the `since`/`format`/
  `fingerprints` params required a schema change anyway; the description
  now teaches the cheap path (M4.5 guidance).
- **Extended NL machinery beyond aui port parity:** pronoun substitution,
  `wait for` clause splitting, `on_screen` matching, and unquoted
  `type X into F` / `fill F with X` payload synthesis in the resolver.
- **TextResolver is spec-strict, not aui-strict:** with multiple untyped
  quoted literals and no label-adjacent disambiguation it returns
  `Ambiguous` (this PRD's M4.3) instead of aui's `untyped[0]` fallback.
- **`jev` uses `f64` to validate reply probability sums** — a carve-out
  from I5's "no floats in the decision path": the f64 arithmetic is
  post-decision reply validation only, never in local scoring.
- **`act` keeps `k` as an abstain-candidates cap** (declared in the tool
  schema) and adds `confidence` inside `done` — additive to M4.5's shape.
- **Stale bound: `max_consecutive_stale = 2` bounds re-decides** on both
  `act` and the runner (M5.3's "at most twice"), so each surface allows
  two re-observe+re-decide rounds before abstaining.
