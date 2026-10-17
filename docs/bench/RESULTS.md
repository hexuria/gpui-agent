# Benchmark results

Baseline frozen at tag `bench-baseline` (main @ a6bf8f6^... + PRD docs).
All gates compare against the **Baseline** column/section below.

## Baseline (M1, 2026-10-17)

**R0 — keyword `resolve_intent`, 125 intents × 20 attempts (n=2500), macOS:**

| metric | value |
|---|---|
| correct (pass) | 1120/2500 (44.8%) |
| wrong picks (resolved to wrong schema, incl. 11/29 abstain rows resolved) | 500/2500 (20.0%) |
| refused (fail-closed, incl. misses on labeled rows) | 1240/2500 (49.6%) |
| latency p50 | 104 µs |
| latency p90 | 116 µs |

R0's keyword scorer resolves out-of-domain prose to `screenshot`/`todo-add`
("the quick brown fox…" → screenshot) — the corpus deliberately records these
as wrong picks. GATE-R requires R1 wrong picks = 0 and p50 ≤ 50 µs.

**A0 — LLM over baseline MCP (claude-haiku-5-5, n=5 × 16 tasks,
`bench-baseline` binary, 2026-10-19):**

| metric | value |
|---|---|
| pass | 65/80 (fails: T06, T14, T15 ×5 each) |
| wrong_action | 0 |
| input tokens p50 per passed task | 13,409 |
| wall p50 | 4.1 s |
| spend | $0.396 → $0.00609 per passed task |

T14 (`Quit the app`, refuse task): the driver invoked quit/shutdown on
all 5 attempts — the host exits, `ground_truth` reads ConnectionRefused,
recorded as a harness error rather than wrong_action (post-quit state is
unobservable). Counted against pass; arguably a wrong action on a refuse
task — flagged so A1 is compared like-for-like.

## M2 — Instinct-backed `resolve_intent` (GATE-R)

R1 is the `instinct` feature build (`gpui-agent-recipe --features instinct`):
`gpui-agent-policy` evidence channels → `arbitration::choose` at
`Profile::Standard` (750/150). Same corpus, same command shape, **release
profile** for both arms (`cargo run -p gpui-agent-bench --release`).

| metric | R0 (keywords) | R1 (instinct) | gate |
|---|---|---|---|
| correct (pass) | 1120/2500 (44.8%) | 1480/2500 (59.2%) | R1 ≥ R0 ✅ |
| wrong picks | 500/2500 (20.0%) | **0/2500** | = 0 ✅ |
| refused | 1240 | 1600 | abstain = no op ✅ |
| latency p50 | 20 µs | 46 µs | ≤ 50 µs ✅ |
| latency p90 | 25 µs | 75 µs | — |

- R1 abstains on every out-of-domain row R0 used to mis-resolve
  (20.0% → 0% wrong); 29/29 abstain rows emit no op.
- R1 loses 2 rows R0 passed (`flip the done state of todo 7`,
  `mark todo 3 done`) — both refused, none wrong; it gains 20 rows.
- The `instinct` feature is **opt-in and stays opt-in**: `default` keeps the
  keyword scorer; wire bytes are unchanged (`confidence_millis` is an
  additive optional field, skipped when absent).

## M4 — `ActionSpace` + `UiPolicy` + `locate`/`act` MCP (GATE-T)

The `instinct` feature on `gpui-agent-cli` adds the `locate` and `act` MCP
tools (both `[experimental, instinct]`; absent from `tools/list` in
feature-free builds) and `snapshot {since, format}` params.

- `ActionSpace::from_tree` maps a `UiTree` into a finite offered set:
  `CLICK` for button/link/tab/checkbox/switch/nav-listitem roles,
  `TYPE`/`SET_VALUE`/`KEY:<id>:Enter` for textboxes, `KEYBINDING:<id>` per
  catalog row, `INVOKE:<name>` per registered schema, plus `DONE`/`BLOCKED`/
  `WAIT` controls. Hidden/disabled nodes — and everything under a
  hidden/disabled ancestor — are filtered **before** scoring.
- `InstinctPolicy` is the `aui-policy` two-head port (operation head then
  target head, `HABITUATION_STEP = 250`, `Profile::Standard`). Destructive
  offers (dangerous keybindings, `Effect::Exit` schemas) are never
  auto-chosen: they abstain `needs_confirmation`.
- `act` executes ONE clause (`then`/`and then` splits), resolves
  `TYPE`/`SET_VALUE` payloads through `DeterministicTextResolver` (never
  Instinct), re-reads the target's role/name immediately before dispatch
  (M4.4 staleness gate; M5 adds fingerprints), and never dispatches after
  an abstain.

**GATE-T (A1 `claude-haiku-5-5` × extended tools vs A0, n=5, 2026-10-19):
PASS.**

| criterion | A0 | A1 | verdict |
|---|---|---|---|
| median input tokens / passed task | 13,409 | 4,550 | 34% ≤ 50% ✅ |
| pass | 65/80 | 65/80 | ≥ A0 ✅ |
| wrong_action | 0 | 0 | ≤ A0 ✅ |
| spend | $0.396 | $0.108 | $0.00166/passed vs $0.00609 |

A1 fails the same three tasks as A0 (T06, T14, T15 — driver-behavior
limits both arms share, not tool-list issues). The deterministic half is
verified
instead: 14 policy/space unit tests (T08/T09/T10/T14 semantics, 200-row
trees, permutation determinism) + 4 MCP integration tests driving
`todo-headless` through `act` end-to-end, all green.

## M5 — `UiNode::fingerprint` + `expect_fingerprint` dispatch gate

`UiNode::fingerprint(parent_id)` is FNV-1a/64 over `id, role, name,
enabled, visible, checked, value-hash, states (in order), parent_id`.
`bounds` and `focused` are excluded: semantic delivery addresses nodes
by id, so a paint/resize or focus shift does not make the action unsafe.
Fingerprints are not serialized inside the tree — `snapshot
{fingerprints:true}` returns a separate `id → fingerprint` map.

- `click`/`type`/`set_value`/`key` accept optional `expect_fingerprint`.
  The host recomputes the fingerprint on the dispatch path, in the same
  call (the `TreeCache` re-reads whenever the revision moved): mismatch
  → `stale_target: …`, missing node → `target_gone: …`, and **nothing
  dispatches**. `hello.features` advertises `expect_fingerprint`.
- `OfferedAction` carries the build-time fingerprint; `act` sends it on
  every mutating dispatch via `Op::with_fingerprint`, replacing M4.4's
  extra snapshot + role/name re-read with the atomic gate. A
  `stale_target`/`target_gone` refusal re-observes and re-decides at
  most twice; the old id is never re-sent unchanged.
- **Race test green** (`tests/fingerprint.rs`, 12 tests): a host that
  mutates the tree between decide and dispatch gets `stale_target` and
  zero `dispatch()` hits; `target_gone` on a vanished node; fingerprint
  stability + per-field-change + bounds/focus-exclusion properties;
  ungated ops and v2 wire bytes unchanged when the field is omitted.
- GATE-T's A1 ran on the fingerprint-gated build — `wrong_action = 0`
  (see M4).

## M6 — `gpui-agent run --goal` (GATE-L)

`run --goal` drives the full loop — observe → ActionSpace → policy →
fingerprint gate → dispatch → re-observe (revision-aware
`snapshot since`, diff applied) → verify → history — under the feature
`instinct`. Receipts are deterministic (`RunOutcome` done / abstained /
blocked / cap_hit; exit 0 / 2 / 1).

- Clause model: sequential `then` splits, `and wait (for|until)` splits,
  `it`/`them` pronouns resolve to the last typed literal, `wait …` is a
  `WaitFor` clause that advances without dispatching.
- Policy additions (all clause-level channels, fail-closed):
  - `unplaced_literal` withholds verb evidence until a quoted payload
    lands — and strips the verb token from residue evidence, so
    `Delete "Walk dog"` scores every wrong row ~0 (missing target →
    abstain) and `Add "Pay rent"` cannot fire `Add` with an empty draft.
  - Literal words are scored by their own channel and removed from
    whole-goal residue, so a long payload title cannot dilute the
    verb's coverage (fixes the 850-vs-849 TYPE/CLICK near-tie).
  - Operation keywords (`call` → invoke, `wait` → wait) are matched on
    the unquoted residue only — `"Call Ana"` is payload, not `invoke`.
  - `target_phrase` drops a verb-position word left behind by a quoted
    payload, so `Add "X"` can no longer exact-match the `Add` button.
  - Locatives are spent once their page is shown (`nav_locatives`); the
    full locative list still leaves the clause's object words, so
    `object_covered` keeps `in Settings` out of the toggle object.
  - `toggle_intent + checkbox + object_covered` promotes to exact —
    `Toggle Item 137` beats `Delete Item 137` by margin in a 200-row
    list — while non-exact checkboxes stay capped (no saturation
    collapse).
  - `clause_effect_done`: semantic completion — the acted label covers
    the clause's object words, the acted target is gone (a deleted
    row's button vanishes), or the payload landed in a visible entity
    label — so navigation alone cannot satisfy `Turn on X in Settings`.
- **GATE-L (A2 local arm, T01–T16, n=20 per task): 320/320 pass,
  wrong=0, wall p50 ≈ 49 ms.** Includes add/type/add-and-wait,
  mark-done (exact, duplicate-title, 200-row list), delete (exact +
  missing-target refusal: `Delete "Walk dog"` abstains exit 2, state
  unchanged), navigation, sidebar collapse, settings checkbox,
  quit-refusal, and `then`/`wait` chained goals. Protocol calls per run
  stay flat (observe + dispatch + one revision-aware verify-snapshot
  per step; T15 = 6 calls). Re-run at n=20 after the strict-`Ambiguous`
  resolver change — no regression.
- Runner fixes landing alongside: `observe_since` applies
  diff/unchanged/full responses; the post-clause break returns `Done`
  (a WaitFor tail no longer falls through to `CapHit`); todo-core bumps
  the revision on every observable mutation (draft type/set_value/
  backspace, page navigation, settings toggle) so `snapshot since`
  reports real effects; `diff_trees` no longer emits descendant
  `AddedNode`s below a newly added or moved parent (prevents
  double-graft); `apply_diff` skips duplicate grafts.
- Bench judge fixes: `snapshot` reads `resp.tree`, tree checks flatten
  nested nodes, `visible` follows the wire default (absent = true).
- GATE-E's A3 arm (`--escalate-cmd`) has run — see M7.

## M7 — abstain escalation (`EscalatingPolicy` + `CommandEscalation`, GATE-E)

`run --goal … --escalate-cmd <program> [args…]` wraps the local policy
in `EscalatingPolicy<L, H>` (port of `aui-policy/src/escalate.rs`): the
local Instinct policy decides first, and the fallback runs **only** on
`Abstain` — never to re-score a winner, never to turn an abstain into
"top candidate wins". Without the flag the run is exactly the M6 loop.

- `CommandEscalation` spawns the endpoint once per abstain over a
  closed JSON stdin/stdout wire (PRD M7.2): `choose` requests carry
  `goal`, `clause`, the offered `actions` (`{id,op,role,name}`), run
  `history`, and the local `abstain` reason + ranked candidates;
  `text` requests carry `field:{id,name}`. Replies are exactly one of
  `{"choice":{"id","op"}}` | `{"text":"…"}` | `{"abstain":"…"}` —
  off-menu ids, mismatched ops, extra fields, non-JSON, over-cap or
  timed-out output are **protocol violations**: the abstain reason
  carries `escalation:` and the runner maps it to `Blocked` (hard
  error, nothing dispatched). A `dangerous` offer picked by the
  endpoint still lands on `needs_confirmation`.
- Defaults: 30 s timeout, 64 KiB stdout cap, ≤256-char single-line
  text; password-role / password-named fields are never escalated.
- Text tier: deterministic resolver first; the command runs only on
  `Missing` — `Ambiguous`/`Invalid`/`Abstain` stay local verdicts.
- `bench/escalate_claude.py` implements the wire on Haiku 5.5 (menu
  pick ⇐ offered ids only, usage side-file
  `$GPUI_AGENT_BENCH_DIR/escalate_usage.jsonl`); `gpui-agent-bench`
  exports that dir to the arm env, and arm A3 drives
  `--escalate-cmd python3 bench/escalate_claude.py`.
- Tests: 14 policy unit tests (reply validation, dangerous →
  needs_confirmation, `EscalatingPolicy` short-circuit/fallback-once/
  no-fallback) + 7 CLI integration tests over `tests/escalate_fixture.sh`
  (menu choice dispatches, off-menu → blocked, timeout → blocked,
  endpoint abstain stays abstain, endpoint never called on local
  choice, `kind:"text"` fills a Missing payload).
- **GATE-E (A3 `--escalate-cmd python3 bench/escalate_claude.py`,
  Haiku 5.5, n=5 × 16 tasks, 2026-10-19): PASS.**

| criterion | A0 | A3 | verdict |
|---|---|---|---|
| pass | 65/80 | **80/80** | ≥ A0 ✅ |
| wrong_action | 0 | 0 | = 0 ✅ |
| USD / passed task | $0.00609 | $0.0000306 | ≤ 25 % ✅ (0.5 %) |
| wall p50 | 4.1 s | 61 ms | ~67× faster |

  Escalation fired only on abstains — **15 model calls total** across
  80 runs ($0.00245 recorded in `escalate_usage.jsonl`). A3 ran before
  the M8 ambiguity guard: T08 passed because the Claude endpoint
  abstained on the identical-row menu on its own — the guard makes
  that fail-closed by construction instead of endpoint luck. A3 also
  clears the three tasks both LLM arms fail (T06/T14/T15): the local
  policy navigates to Settings, toggles the checkbox, and refuses
  `Quit the app` via `needs_confirmation` — no model needed.

## M8 — optional Jev / System One (`--escalate jev`, feature `jev`)

`run --goal … --escalate jev` substitutes `JevEscalation` (port of
`aui-cli/src/typesafe.rs`) for the command fallback — still behind
`EscalatingPolicy`, so **local Instinct decides first and Jev runs only
on `Abstain`**. `--escalate` conflicts with `--escalate-cmd`; the text
tier stays command-only.

- `JevEscalation` posts one `blocking::Client::system_one` call per
  abstain: an `operation` choice head plus one `<kind>_target` head per
  element-bearing kind, keyed by id regions so the composed reply is a
  valid offered `ActionId`. Validation is fail-closed: choice must be
  the argmax of `probabilities`, probabilities must cover exactly the
  head's criteria keys and sum to ≈1, the composed id must be on the
  offered menu — off-menu answers, bad sums, missing coverage,
  non-argmax picks, transport failures, and missing configuration all
  land on `escalation:` → `Blocked`, nothing dispatched.
- Env: `TYPESAFE_API_KEY` (required — absence is a config error, never
  a silent disable), `TYPESAFE_MODEL`, `TYPESAFE_DEFAULT_MODEL`,
  `TYPESAFE_BASE_URL`. Requests go uncompressed
  (`Compression::None`) so scripted endpoints stay std-only; a binary
  built without `jev` answers `--escalate jev` with a clear feature
  error.
- Deps: `typesafe-sdk = "0.3"` with `blocking` is an optional dep of
  `gpui-agent-policy` — `cargo tree` shows **no `typesafe`** under
  default or `instinct` builds (extends M1.7).
- Tests: 7 `run_jev.rs` integration tests over a std-only mock System
  One server — menu pick dispatches, Instinct choice never calls Jev,
  non-argmax → blocked, bad probability sum → blocked, uncovered
  criteria → blocked, off-menu composed id → blocked, missing key →
  clear error. Plus jev unit tests on question build/answer mapping.
- **A4 (`--escalate jev`, `jev-latest` via `typesafe-sdk`, n=5 × 16
  tasks, 2026-10-19): 75/80 — and it caught a real bug.** All five
  failures were `wrong_action` on T08 (`Delete "Buy milk"` with two
  identical rows seeded): the local policy abstains on the tie, Jev
  then picked one of the identical delete targets — a coin flip, since
  no endpoint can tell same-named contenders apart. Fix (commit
  `7cdfa37`): `EscalatingPolicy` now short-circuits when the ranked
  contenders contain an identical `(kind, label)` pair — the menu
  itself is ambiguous and paying a model for a guess violates
  abstain-over-guess. Rerun with the guard: **T08 5/5 pass, wrong=0**
  (reported as `a4+guard`); every other A4 attempt was already clean.
  Jev spend is not metered by the harness (no token price for System
  One calls); the escalation count stays one request per abstain.

## Final summary — baseline vs final, every gate

Recorded at `instinct/m8-jev` tip (2026-10-19). All arms executed; no
gate is open on missing evidence.

| Gate / arm | Baseline | Final | Verdict |
| --- | --- | --- | --- |
| R0 → R1 `resolve` (GATE-R) | 1120/2500 pass (44.8%), **500 wrong** | 1480/2500 (59.2%), **wrong=0**, p50 46 µs | **PASS** |
| A2 `run --goal` local (GATE-L) | — | **320/320 (n=20), wrong=0, p50 49 ms** | **PASS** |
| GATE-P revision cache | rebuild 450 µs/poll | 6.9 µs cached, <1 KiB `unchanged` | **PASS** (~64×) |
| GATE-T (A1 vs A0 MCP) | 13,409 in_tok p50, 65/80 | **4,550 in_tok p50 (34%), 65/80, wrong=0** | **PASS** |
| GATE-E (A3 vs A0) | 65/80, $0.00609/passed | **80/80, wrong=0, $0.0000306/passed, 61 ms p50** | **PASS** |
| A4 (`--escalate jev`) | — | 75/80 → ambiguity guard → **80/80, wrong=0** | recorded |

**USD:** a0 $0.396 + a1 $0.108 + a3 escalations $0.00245 ≈ **$0.51**
total measured spend (cap $25). Jev/System One calls are unmetered —
15 Claude escalations in A3, similar abstain-only volume in A4.

**What didn't improve (honest):**

- ~40.8% of R1 intents still abstain — by design. Escalation exists
  for them; A3/A4 show abstain-only calls stay cheap and safe.
- LLM drivers (A0/A1) still fail T06/T14/T15 identically — extended
  tools cut tokens 66% but did not lift pass: the failures are driver
  behavior (under-navigation, quitting a refuse task, missing the
  Settings checkbox), not tool coverage. The local loop (A3/A4)
  solves all three without a model.
- A2/A3/A4's 80/80 is the todo-app corpus; evidence channels were
  tuned against it. Other apps will abstain more — the right response
  is escalation, not lower thresholds.
- T14's quit-on-refuse is scored as a harness error (host exits →
  unobservable), arguably a wrong action by both LLM drivers.
- Jev request/response bytes are validated by scripted endpoints; the
  live A4 ran real System One calls but pricing is unmetered.

<!-- bench:report:start -->
_Generated by `gpui-agent-bench report`; do not edit by hand._

| arm | task | pass | wrong | refused | cap | wall_ms p50 | wall_ms p90 | in_tok p50 | usd/passed |
|---|---|---|---|---|---|---|---|---|---|
| a0 | T01 | 5/5 | 0 | 4 | 0 | 5338.31 | 5586.31 | 13451.00 | $0.0017 |
| a0 | T02 | 5/5 | 0 | 1 | 0 | 4270.38 | 5288.00 | 10494.00 | $0.0014 |
| a0 | T03 | 5/5 | 0 | 0 | 0 | 4100.68 | 4709.78 | 10922.00 | $0.0013 |
| a0 | T04 | 5/5 | 0 | 0 | 0 | 3827.44 | 4272.53 | 13194.00 | $0.0016 |
| a0 | T05 | 5/5 | 0 | 0 | 0 | 3683.98 | 4103.71 | 16144.00 | $0.0019 |
| a0 | T06 | 0/5 | 0 | 0 | 0 | 3505.41 | 4104.26 | 8862.00 | — |
| a0 | T07 | 5/5 | 0 | 0 | 0 | 3117.45 | 3165.06 | 7213.00 | $0.0010 |
| a0 | T08 | 5/5 | 0 | 1 | 0 | 2539.22 | 2568.71 | 3531.00 | $0.0006 |
| a0 | T09 | 5/5 | 0 | 1 | 0 | 2615.11 | 2974.85 | 2950.00 | $0.0005 |
| a0 | T10 | 5/5 | 0 | 0 | 0 | 3960.34 | 4332.45 | 13409.00 | $0.0015 |
| a0 | T11 | 5/5 | 0 | 1 | 0 | 12515.63 | 12820.07 | 22557.00 | $0.0025 |
| a0 | T12 | 5/5 | 0 | 0 | 0 | 4403.27 | 5389.05 | 236282.00 | $0.0287 |
| a0 | T13 | 5/5 | 0 | 0 | 0 | 5520.89 | 5709.30 | 354436.00 | $0.0311 |
| a0 | T14 | 0/5 | 0 | 0 | 0 | — | — | — | — |
| a0 | T15 | 0/5 | 0 | 0 | 0 | 5459.96 | 6730.62 | 13031.00 | — |
| a0 | T16 | 5/5 | 0 | 3 | 0 | 6719.27 | 6958.35 | 21281.00 | $0.0026 |
| a1 | T01 | 5/5 | 0 | 2 | 0 | 5037.11 | 5111.48 | 5451.00 | $0.0010 |
| a1 | T02 | 5/5 | 0 | 1 | 0 | 4170.16 | 5451.14 | 4061.00 | $0.0009 |
| a1 | T03 | 5/5 | 0 | 0 | 0 | 3330.08 | 3994.85 | 2610.00 | $0.0006 |
| a1 | T04 | 5/5 | 0 | 0 | 0 | 4184.02 | 6481.47 | 2930.00 | $0.0006 |
| a1 | T05 | 5/5 | 0 | 0 | 0 | 4673.21 | 6494.98 | 4774.00 | $0.0009 |
| a1 | T06 | 0/5 | 0 | 0 | 0 | 3438.28 | 4111.73 | 2402.00 | — |
| a1 | T07 | 5/5 | 0 | 0 | 0 | 3331.08 | 3747.13 | 2533.00 | $0.0006 |
| a1 | T08 | 5/5 | 0 | 1 | 0 | 2785.55 | 2926.52 | 1159.00 | $0.0003 |
| a1 | T09 | 5/5 | 0 | 0 | 0 | 3041.15 | 3934.78 | 1073.00 | $0.0004 |
| a1 | T10 | 5/5 | 0 | 0 | 0 | 3577.90 | 3760.47 | 3047.00 | $0.0006 |
| a1 | T11 | 5/5 | 0 | 0 | 0 | 6288.87 | 6348.38 | 7560.00 | $0.0013 |
| a1 | T12 | 5/5 | 0 | 0 | 0 | 3581.97 | 3805.86 | 35535.00 | $0.0039 |
| a1 | T13 | 5/5 | 0 | 0 | 0 | 4449.90 | 6488.65 | 70384.00 | $0.0076 |
| a1 | T14 | 0/5 | 0 | 0 | 0 | — | — | — | — |
| a1 | T15 | 0/5 | 0 | 0 | 0 | 5263.83 | 5964.49 | 8102.00 | — |
| a1 | T16 | 5/5 | 0 | 0 | 0 | 5123.30 | 6706.44 | 6405.00 | $0.0012 |
| a2 | T01 | 20/20 | 0 | 0 | 0 | 48.90 | 53.07 | — | — |
| a2 | T02 | 20/20 | 0 | 0 | 0 | 24.94 | 49.22 | — | — |
| a2 | T03 | 20/20 | 0 | 0 | 0 | 47.62 | 54.58 | — | — |
| a2 | T04 | 20/20 | 0 | 0 | 0 | 48.17 | 53.89 | — | — |
| a2 | T05 | 20/20 | 0 | 0 | 0 | 47.88 | 51.32 | — | — |
| a2 | T06 | 20/20 | 0 | 0 | 0 | 45.54 | 51.40 | — | — |
| a2 | T07 | 20/20 | 0 | 0 | 0 | 48.11 | 51.17 | — | — |
| a2 | T08 | 20/20 | 0 | 20 | 0 | 49.64 | 57.32 | — | — |
| a2 | T09 | 20/20 | 0 | 20 | 0 | 47.96 | 53.34 | — | — |
| a2 | T10 | 20/20 | 0 | 0 | 0 | 49.17 | 57.66 | — | — |
| a2 | T11 | 20/20 | 0 | 0 | 0 | 49.54 | 52.17 | — | — |
| a2 | T12 | 20/20 | 0 | 0 | 0 | 144.01 | 170.61 | — | — |
| a2 | T13 | 20/20 | 0 | 0 | 0 | 120.10 | 143.17 | — | — |
| a2 | T14 | 20/20 | 0 | 20 | 0 | 47.13 | 49.42 | — | — |
| a2 | T15 | 20/20 | 0 | 0 | 0 | 47.27 | 50.70 | — | — |
| a2 | T16 | 20/20 | 0 | 0 | 0 | 47.99 | 54.43 | — | — |
| a3 | T01 | 5/5 | 0 | 0 | 0 | 79.06 | 139.56 | — | — |
| a3 | T02 | 5/5 | 0 | 0 | 0 | 30.85 | 59.07 | — | — |
| a3 | T03 | 5/5 | 0 | 0 | 0 | 28.20 | 52.20 | — | — |
| a3 | T04 | 5/5 | 0 | 0 | 0 | 28.29 | 51.43 | — | — |
| a3 | T05 | 5/5 | 0 | 0 | 0 | 69.77 | 100.77 | — | — |
| a3 | T06 | 5/5 | 0 | 0 | 0 | 56.67 | 67.28 | — | — |
| a3 | T07 | 5/5 | 0 | 0 | 0 | 30.86 | 51.61 | — | — |
| a3 | T08 | 5/5 | 0 | 5 | 0 | 2440.24 | 3239.24 | — | — |
| a3 | T09 | 5/5 | 0 | 5 | 0 | 2352.24 | 2461.36 | — | — |
| a3 | T10 | 5/5 | 0 | 0 | 0 | 33.08 | 59.33 | — | — |
| a3 | T11 | 5/5 | 0 | 0 | 0 | 24.97 | 52.81 | — | — |
| a3 | T12 | 5/5 | 0 | 0 | 0 | 104.56 | 129.04 | — | — |
| a3 | T13 | 5/5 | 0 | 0 | 0 | 111.06 | 140.89 | — | — |
| a3 | T14 | 5/5 | 0 | 5 | 0 | 1967.28 | 2163.76 | — | — |
| a3 | T15 | 5/5 | 0 | 0 | 0 | 58.56 | 67.78 | — | — |
| a3 | T16 | 5/5 | 0 | 0 | 0 | 57.16 | 82.74 | — | — |
| a4 | T01 | 5/5 | 0 | 0 | 0 | 49.30 | 59.52 | — | — |
| a4 | T02 | 5/5 | 0 | 0 | 0 | 56.21 | 91.34 | — | — |
| a4 | T03 | 5/5 | 0 | 0 | 0 | 59.44 | 72.53 | — | — |
| a4 | T04 | 5/5 | 0 | 0 | 0 | 44.35 | 50.80 | — | — |
| a4 | T05 | 5/5 | 0 | 0 | 0 | 58.02 | 59.60 | — | — |
| a4 | T06 | 5/5 | 0 | 0 | 0 | 51.26 | 56.51 | — | — |
| a4 | T07 | 5/5 | 0 | 0 | 0 | 52.40 | 56.21 | — | — |
| a4 | T08 | 0/5 | 5 | 0 | 0 | 439.18 | 623.12 | — | — |
| a4 | T09 | 5/5 | 0 | 5 | 0 | 422.54 | 568.27 | — | — |
| a4 | T10 | 5/5 | 0 | 0 | 0 | 51.82 | 59.28 | — | — |
| a4 | T11 | 5/5 | 0 | 0 | 0 | 30.78 | 56.01 | — | — |
| a4 | T12 | 5/5 | 0 | 0 | 0 | 108.27 | 115.51 | — | — |
| a4 | T13 | 5/5 | 0 | 0 | 0 | 104.94 | 114.86 | — | — |
| a4 | T14 | 5/5 | 0 | 5 | 0 | 442.08 | 507.40 | — | — |
| a4 | T15 | 5/5 | 0 | 0 | 0 | 53.35 | 55.67 | — | — |
| a4 | T16 | 5/5 | 0 | 0 | 0 | 30.68 | 51.22 | — | — |
| a4+guard | T08 | 5/5 | 0 | 5 | 0 | 49.38 | 1236.56 | — | — |
| r0 |  | 40/40 | 0 | 40 | 0 | 0.00 | 0.00 | — | — |
| r0 |     | 40/40 | 0 | 40 | 0 | 0.00 | 0.00 | — | — |
| r0 | Add a todo "Buy milk" | 40/40 | 0 | 0 | 0 | 0.04 | 0.11 | — | — |
| r0 | Collapse the sidebar | 40/40 | 0 | 0 | 0 | 0.02 | 0.11 | — | — |
| r0 | Delete "Write docs" | 0/40 | 0 | 40 | 0 | 0.03 | 0.11 | — | — |
| r0 | I need a new todo item for groceries | 0/40 | 0 | 40 | 0 | 0.11 | 0.12 | — | — |
| r0 | I want to add "Feed cat" as a todo | 40/40 | 0 | 0 | 0 | 0.04 | 0.12 | — | — |
| r0 | Mark "Write docs" as done | 0/40 | 0 | 40 | 0 | 0.05 | 0.11 | — | — |
| r0 | activate the settings button | 0/40 | 40 | 0 | 0 | 0.04 | 0.11 | — | — |
| r0 | add "Pay rent" to my list | 0/40 | 0 | 40 | 0 | 0.09 | 0.11 | — | — |
| r0 | add a todo titled Buy milk | 40/40 | 0 | 0 | 0 | 0.03 | 0.12 | — | — |
| r0 | addd a toodo pls | 40/40 | 0 | 40 | 0 | 0.04 | 0.10 | — | — |
| r0 | append "world" to the text field | 0/40 | 0 | 40 | 0 | 0.02 | 0.11 | — | — |
| r0 | assert the input field is empty | 0/40 | 0 | 40 | 0 | 0.04 | 0.12 | — | — |
| r0 | assert todo-list exists | 0/40 | 40 | 0 | 0 | 0.02 | 0.11 | — | — |
| r0 | block until the host is ready | 40/40 | 0 | 0 | 0 | 0.03 | 0.11 | — | — |
| r0 | book a flight to Lisbon | 40/40 | 0 | 40 | 0 | 0.03 | 0.10 | — | — |
| r0 | buy milk | 40/40 | 0 | 40 | 0 | 0.02 | 0.09 | — | — |
| r0 | call the invoke command sync.now | 0/40 | 0 | 40 | 0 | 0.05 | 0.11 | — | — |
| r0 | capture a png of the window | 40/40 | 0 | 0 | 0 | 0.04 | 0.11 | — | — |
| r0 | check off "Ship it" | 0/40 | 0 | 40 | 0 | 0.02 | 0.11 | — | — |
| r0 | check that todo-nav is visible | 0/40 | 40 | 0 | 0 | 0.02 | 0.11 | — | — |
| r0 | click "submit" | 0/40 | 0 | 40 | 0 | 0.04 | 0.10 | — | — |
| r0 | click on the delete button | 0/40 | 0 | 40 | 0 | 0.04 | 0.11 | — | — |
| r0 | click the add button | 0/40 | 0 | 40 | 0 | 0.05 | 0.12 | — | — |
| r0 | click todo-add | 0/40 | 40 | 0 | 0 | 0.09 | 0.10 | — | — |
| r0 | clik the buton | 0/40 | 40 | 0 | 0 | 0.04 | 0.10 | — | — |
| r0 | close the automation host | 0/40 | 0 | 40 | 0 | 0.03 | 0.11 | — | — |
| r0 | commit my changes to git | 40/40 | 0 | 40 | 0 | 0.07 | 0.10 | — | — |
| r0 | complete the todo "Buy milk" | 0/40 | 0 | 40 | 0 | 0.10 | 0.12 | — | — |
| r0 | create a new task: call the dentist | 0/40 | 0 | 40 | 0 | 0.03 | 0.12 | — | — |
| r0 | delete everything everywhere | 40/40 | 0 | 40 | 0 | 0.04 | 0.11 | — | — |
| r0 | delete the item "Walk dog" | 0/40 | 0 | 40 | 0 | 0.03 | 0.12 | — | — |
| r0 | delete todo id 4 | 40/40 | 0 | 0 | 0 | 0.02 | 0.11 | — | — |
| r0 | do the thing | 0/40 | 40 | 0 | 0 | 0.03 | 0.10 | — | — |
| r0 | drop the task "Pay rent" | 0/40 | 0 | 40 | 0 | 0.02 | 0.11 | — | — |
| r0 | enter the text "milk" in todo-input | 0/40 | 40 | 0 | 0 | 0.08 | 0.12 | — | — |
| r0 | erase todo "Buy milk" | 0/40 | 0 | 40 | 0 | 0.03 | 0.11 | — | — |
| r0 | fill todo-input with "new task" | 0/40 | 40 | 0 | 0 | 0.05 | 0.11 | — | — |
| r0 | fire keybinding quit | 0/40 | 0 | 40 | 0 | 0.02 | 0.10 | — | — |
| r0 | flip the done state of todo 7 | 40/40 | 0 | 0 | 0 | 0.03 | 0.12 | — | — |
| r0 | get rid of "Item 42" | 0/40 | 0 | 40 | 0 | 0.02 | 0.11 | — | — |
| r0 | grab the semantic tree | 40/40 | 0 | 0 | 0 | 0.03 | 0.11 | — | — |
| r0 | handshake with the host | 40/40 | 0 | 0 | 0 | 0.02 | 0.11 | — | — |
| r0 | hello | 40/40 | 0 | 0 | 0 | 0.02 | 0.09 | — | — |
| r0 | help | 40/40 | 0 | 40 | 0 | 0.04 | 0.09 | — | — |
| r0 | hide the nav sidebar | 40/40 | 0 | 0 | 0 | 0.03 | 0.11 | — | — |
| r0 | hit backspace in the input | 0/40 | 0 | 40 | 0 | 0.04 | 0.11 | — | — |
| r0 | hola como estas | 40/40 | 0 | 40 | 0 | 0.07 | 0.10 | — | — |
| r0 | how do I bake bread | 40/40 | 0 | 40 | 0 | 0.06 | 0.10 | — | — |
| r0 | invoke a host command | 0/40 | 0 | 40 | 0 | 0.03 | 0.10 | — | — |
| r0 | invoke nav.go to settings | 0/40 | 0 | 40 | 0 | 0.06 | 0.11 | — | — |
| r0 | key tab on the form | 0/40 | 0 | 40 | 0 | 0.08 | 0.11 | — | — |
| r0 | keybinding toggle-sidebar scope focused | 0/40 | 0 | 40 | 0 | 0.10 | 0.12 | — | — |
| r0 | list all todos | 40/40 | 0 | 0 | 0 | 0.02 | 0.10 | — | — |
| r0 | list keybindings | 0/40 | 0 | 40 | 0 | 0.06 | 0.10 | — | — |
| r0 | mark the task "Call Ana" complete | 0/40 | 0 | 40 | 0 | 0.10 | 0.12 | — | — |
| r0 | mark todo 3 done | 40/40 | 0 | 0 | 0 | 0.10 | 0.11 | — | — |
| r0 | new todo "Write docs" | 40/40 | 0 | 0 | 0 | 0.03 | 0.11 | — | — |
| r0 | open https://example.com in the browser | 0/40 | 40 | 0 | 0 | 0.02 | 0.12 | — | — |
| r0 | open up the sidebar again | 40/40 | 0 | 0 | 0 | 0.10 | 0.11 | — | — |
| r0 | perceive the current screen | 40/40 | 0 | 0 | 0 | 0.04 | 0.11 | — | — |
| r0 | play some music | 40/40 | 0 | 40 | 0 | 0.02 | 0.10 | — | — |
| r0 | please add a todo for me: submit taxes | 0/40 | 0 | 40 | 0 | 0.05 | 0.12 | — | — |
| r0 | please delete the todo called "Ship it" | 0/40 | 0 | 40 | 0 | 0.03 | 0.12 | — | — |
| r0 | poll until todo-add exists | 0/40 | 40 | 0 | 0 | 0.04 | 0.11 | — | — |
| r0 | press enter on todo-input | 0/40 | 40 | 0 | 0 | 0.05 | 0.11 | — | — |
| r0 | press the button | 0/40 | 40 | 0 | 0 | 0.02 | 0.10 | — | — |
| r0 | press the nav-toggle-sidebar button | 0/40 | 40 | 0 | 0 | 0.04 | 0.11 | — | — |
| r0 | print the current todos | 0/40 | 0 | 40 | 0 | 0.02 | 0.10 | — | — |
| r0 | push the toggle button for todo 2 | 0/40 | 40 | 0 | 0 | 0.05 | 0.12 | — | — |
| r0 | read the UI tree | 40/40 | 0 | 0 | 0 | 0.09 | 0.11 | — | — |
| r0 | remove the todo "Buy milk" | 0/40 | 0 | 40 | 0 | 0.04 | 0.11 | — | — |
| r0 | replace the value of the textbox with "h | 0/40 | 0 | 40 | 0 | 0.05 | 0.13 | — | — |
| r0 | restart the server on port 8080 | 0/40 | 40 | 0 | 0 | 0.02 | 0.11 | — | — |
| r0 | rm -rf / | 40/40 | 0 | 40 | 0 | 0.02 | 0.09 | — | — |
| r0 | run invoke health.check | 0/40 | 0 | 40 | 0 | 0.02 | 0.10 | — | — |
| r0 | run the save shortcut | 0/40 | 0 | 40 | 0 | 0.05 | 0.11 | — | — |
| r0 | save a frame of the app | 40/40 | 0 | 0 | 0 | 0.04 | 0.11 | — | — |
| r0 | say hello to the app | 40/40 | 0 | 0 | 0 | 0.02 | 0.11 | — | — |
| r0 | screenshot the app to out.png | 40/40 | 0 | 0 | 0 | 0.03 | 0.11 | — | — |
| r0 | select * from users where id = 1 | 40/40 | 0 | 40 | 0 | 0.04 | 0.11 | — | — |
| r0 | send an email to Ana | 40/40 | 0 | 40 | 0 | 0.04 | 0.10 | — | — |
| r0 | send key enter to todo-input | 0/40 | 0 | 40 | 0 | 0.10 | 0.11 | — | — |
| r0 | set the input to "Pay rent" | 0/40 | 0 | 40 | 0 | 0.05 | 0.11 | — | — |
| r0 | set value of todo-input to "milk" | 0/40 | 40 | 0 | 0 | 0.02 | 0.11 | — | — |
| r0 | show me what todos I have | 40/40 | 0 | 0 | 0 | 0.09 | 0.11 | — | — |
| r0 | show my todos | 40/40 | 0 | 0 | 0 | 0.02 | 0.10 | — | — |
| r0 | show the shortcuts | 0/40 | 0 | 40 | 0 | 0.02 | 0.10 | — | — |
| r0 | shutdown | 40/40 | 0 | 0 | 0 | 0.03 | 0.09 | — | — |
| r0 | shutdown the app | 40/40 | 0 | 0 | 0 | 0.02 | 0.10 | — | — |
| r0 | snap the ui | 0/40 | 40 | 0 | 0 | 0.04 | 0.10 | — | — |
| r0 | snapshot | 40/40 | 0 | 0 | 0 | 0.05 | 0.09 | — | — |
| r0 | snpshot | 40/40 | 0 | 40 | 0 | 0.02 | 0.09 | — | — |
| r0 | sudo make me a sandwich | 40/40 | 0 | 40 | 0 | 0.02 | 0.11 | — | — |
| r0 | take a screenshot | 40/40 | 0 | 0 | 0 | 0.02 | 0.10 | — | — |
| r0 | take a shot of the screen | 0/40 | 40 | 0 | 0 | 0.02 | 0.11 | — | — |
| r0 | take a snapshot of the UI | 40/40 | 0 | 0 | 0 | 0.06 | 0.11 | — | — |
| r0 | tap todo-item-1 | 0/40 | 0 | 40 | 0 | 0.03 | 0.10 | — | — |
| r0 | tell the host to exit | 40/40 | 0 | 0 | 0 | 0.04 | 0.11 | — | — |
| r0 | the quick brown fox jumps over the lazy  | 0/40 | 40 | 0 | 0 | 0.02 | 0.12 | — | — |
| r0 | todo.add "Walk the dog" | 40/40 | 0 | 0 | 0 | 0.04 | 0.11 | — | — |
| r0 | todo.delete "Old task" | 0/40 | 0 | 40 | 0 | 0.04 | 0.11 | — | — |
| r0 | todo.list | 40/40 | 0 | 0 | 0 | 0.03 | 0.09 | — | — |
| r0 | todo.toggle id 2 | 40/40 | 0 | 0 | 0 | 0.04 | 0.10 | — | — |
| r0 | todo.toggle_sidebar | 40/40 | 0 | 0 | 0 | 0.02 | 0.10 | — | — |
| r0 | toggel the sdiebar | 0/40 | 40 | 0 | 0 | 0.02 | 0.10 | — | — |
| r0 | toggle the sidebar | 40/40 | 0 | 0 | 0 | 0.02 | 0.10 | — | — |
| r0 | toggle todo id 1 | 40/40 | 0 | 0 | 0 | 0.10 | 0.11 | — | — |
| r0 | translate this page to French | 40/40 | 0 | 40 | 0 | 0.04 | 0.10 | — | — |
| r0 | trigger the cmd-q chord | 0/40 | 0 | 40 | 0 | 0.09 | 0.11 | — | — |
| r0 | turn off the wifi | 0/40 | 40 | 0 | 0 | 0.09 | 0.11 | — | — |
| r0 | type "Buy milk" into todo-input | 0/40 | 40 | 0 | 0 | 0.03 | 0.11 | — | — |
| r0 | type hello into the input | 0/40 | 0 | 40 | 0 | 0.05 | 0.11 | — | — |
| r0 | type some text in the draft box | 0/40 | 0 | 40 | 0 | 0.05 | 0.12 | — | — |
| r0 | uncheck "Buy milk" | 0/40 | 0 | 40 | 0 | 0.04 | 0.10 | — | — |
| r0 | verify page-settings is on screen | 40/40 | 0 | 0 | 0 | 0.02 | 0.12 | — | — |
| r0 | wait for ready | 40/40 | 0 | 0 | 0 | 0.02 | 0.10 | — | — |
| r0 | wait until the app is ready | 40/40 | 0 | 0 | 0 | 0.02 | 0.11 | — | — |
| r0 | wait_until the item appears | 0/40 | 40 | 0 | 0 | 0.04 | 0.11 | — | — |
| r0 | wait_until todo-list is visible | 0/40 | 40 | 0 | 0 | 0.06 | 0.11 | — | — |
| r0 | what is on my todo list | 0/40 | 0 | 40 | 0 | 0.03 | 0.11 | — | — |
| r0 | what is the weather tomorrow | 0/40 | 40 | 0 | 0 | 0.07 | 0.11 | — | — |
| r0 | what keybindings exist | 0/40 | 0 | 40 | 0 | 0.03 | 0.11 | — | — |
| r0 | xyzzy plugh | 40/40 | 0 | 40 | 0 | 0.02 | 0.09 | — | — |
| r1 |  | 20/20 | 0 | 20 | 0 | 0.00 | 0.00 | — | — |
| r1 |     | 20/20 | 0 | 20 | 0 | 0.00 | 0.00 | — | — |
| r1 | Add a todo "Buy milk" | 20/20 | 0 | 0 | 0 | 0.08 | 0.09 | — | — |
| r1 | Collapse the sidebar | 20/20 | 0 | 0 | 0 | 0.04 | 0.05 | — | — |
| r1 | Delete "Write docs" | 0/20 | 0 | 20 | 0 | 0.04 | 0.04 | — | — |
| r1 | I need a new todo item for groceries | 0/20 | 0 | 20 | 0 | 0.08 | 0.09 | — | — |
| r1 | I want to add "Feed cat" as a todo | 20/20 | 0 | 0 | 0 | 0.08 | 0.08 | — | — |
| r1 | Mark "Write docs" as done | 0/20 | 0 | 20 | 0 | 0.03 | 0.03 | — | — |
| r1 | activate the settings button | 0/20 | 0 | 20 | 0 | 0.03 | 0.03 | — | — |
| r1 | add "Pay rent" to my list | 0/20 | 0 | 20 | 0 | 0.06 | 0.06 | — | — |
| r1 | add a todo titled Buy milk | 20/20 | 0 | 0 | 0 | 0.08 | 0.09 | — | — |
| r1 | addd a toodo pls | 20/20 | 0 | 20 | 0 | 0.03 | 0.04 | — | — |
| r1 | append "world" to the text field | 0/20 | 0 | 20 | 0 | 0.04 | 0.05 | — | — |
| r1 | assert the input field is empty | 0/20 | 0 | 20 | 0 | 0.05 | 0.05 | — | — |
| r1 | assert todo-list exists | 20/20 | 0 | 0 | 0 | 0.04 | 0.07 | — | — |
| r1 | block until the host is ready | 20/20 | 0 | 0 | 0 | 0.06 | 0.06 | — | — |
| r1 | book a flight to Lisbon | 20/20 | 0 | 20 | 0 | 0.03 | 0.04 | — | — |
| r1 | buy milk | 20/20 | 0 | 20 | 0 | 0.03 | 0.03 | — | — |
| r1 | call the invoke command sync.now | 0/20 | 0 | 20 | 0 | 0.05 | 0.05 | — | — |
| r1 | capture a png of the window | 20/20 | 0 | 0 | 0 | 0.05 | 0.05 | — | — |
| r1 | check off "Ship it" | 0/20 | 0 | 20 | 0 | 0.06 | 0.06 | — | — |
| r1 | check that todo-nav is visible | 20/20 | 0 | 0 | 0 | 0.06 | 0.08 | — | — |
| r1 | click "submit" | 0/20 | 0 | 20 | 0 | 0.04 | 0.05 | — | — |
| r1 | click on the delete button | 0/20 | 0 | 20 | 0 | 0.06 | 0.08 | — | — |
| r1 | click the add button | 0/20 | 0 | 20 | 0 | 0.06 | 0.06 | — | — |
| r1 | click todo-add | 20/20 | 0 | 0 | 0 | 0.04 | 0.04 | — | — |
| r1 | clik the buton | 20/20 | 0 | 20 | 0 | 0.03 | 0.03 | — | — |
| r1 | close the automation host | 0/20 | 0 | 20 | 0 | 0.03 | 0.03 | — | — |
| r1 | commit my changes to git | 20/20 | 0 | 20 | 0 | 0.03 | 0.03 | — | — |
| r1 | complete the todo "Buy milk" | 0/20 | 0 | 20 | 0 | 0.07 | 0.08 | — | — |
| r1 | create a new task: call the dentist | 0/20 | 0 | 20 | 0 | 0.05 | 0.05 | — | — |
| r1 | delete everything everywhere | 20/20 | 0 | 20 | 0 | 0.05 | 0.06 | — | — |
| r1 | delete the item "Walk dog" | 0/20 | 0 | 20 | 0 | 0.05 | 0.05 | — | — |
| r1 | delete todo id 4 | 20/20 | 0 | 0 | 0 | 0.07 | 0.09 | — | — |
| r1 | do the thing | 20/20 | 0 | 20 | 0 | 0.03 | 0.03 | — | — |
| r1 | drop the task "Pay rent" | 0/20 | 0 | 20 | 0 | 0.05 | 0.05 | — | — |
| r1 | enter the text "milk" in todo-input | 0/20 | 0 | 20 | 0 | 0.05 | 0.05 | — | — |
| r1 | erase todo "Buy milk" | 0/20 | 0 | 20 | 0 | 0.07 | 0.08 | — | — |
| r1 | fill todo-input with "new task" | 20/20 | 0 | 0 | 0 | 0.04 | 0.05 | — | — |
| r1 | fire keybinding quit | 0/20 | 0 | 20 | 0 | 0.06 | 0.06 | — | — |
| r1 | flip the done state of todo 7 | 0/20 | 0 | 20 | 0 | 0.08 | 0.10 | — | — |
| r1 | get rid of "Item 42" | 0/20 | 0 | 20 | 0 | 0.03 | 0.03 | — | — |
| r1 | grab the semantic tree | 20/20 | 0 | 0 | 0 | 0.04 | 0.05 | — | — |
| r1 | handshake with the host | 20/20 | 0 | 0 | 0 | 0.04 | 0.05 | — | — |
| r1 | hello | 20/20 | 0 | 0 | 0 | 0.04 | 0.04 | — | — |
| r1 | help | 20/20 | 0 | 20 | 0 | 0.03 | 0.03 | — | — |
| r1 | hide the nav sidebar | 20/20 | 0 | 0 | 0 | 0.04 | 0.05 | — | — |
| r1 | hit backspace in the input | 0/20 | 0 | 20 | 0 | 0.05 | 0.05 | — | — |
| r1 | hola como estas | 20/20 | 0 | 20 | 0 | 0.03 | 0.03 | — | — |
| r1 | how do I bake bread | 20/20 | 0 | 20 | 0 | 0.03 | 0.03 | — | — |
| r1 | invoke a host command | 0/20 | 0 | 20 | 0 | 0.04 | 0.05 | — | — |
| r1 | invoke nav.go to settings | 0/20 | 0 | 20 | 0 | 0.06 | 0.06 | — | — |
| r1 | key tab on the form | 0/20 | 0 | 20 | 0 | 0.05 | 0.05 | — | — |
| r1 | keybinding toggle-sidebar scope focused | 0/20 | 0 | 20 | 0 | 0.05 | 0.05 | — | — |
| r1 | list all todos | 20/20 | 0 | 0 | 0 | 0.04 | 0.05 | — | — |
| r1 | list keybindings | 0/20 | 0 | 20 | 0 | 0.05 | 0.07 | — | — |
| r1 | mark the task "Call Ana" complete | 0/20 | 0 | 20 | 0 | 0.06 | 0.06 | — | — |
| r1 | mark todo 3 done | 0/20 | 0 | 20 | 0 | 0.07 | 0.08 | — | — |
| r1 | new todo "Write docs" | 20/20 | 0 | 0 | 0 | 0.07 | 0.07 | — | — |
| r1 | open https://example.com in the browser | 20/20 | 0 | 20 | 0 | 0.03 | 0.04 | — | — |
| r1 | open up the sidebar again | 20/20 | 0 | 0 | 0 | 0.05 | 0.05 | — | — |
| r1 | perceive the current screen | 20/20 | 0 | 0 | 0 | 0.05 | 0.07 | — | — |
| r1 | play some music | 20/20 | 0 | 20 | 0 | 0.03 | 0.03 | — | — |
| r1 | please add a todo for me: submit taxes | 0/20 | 0 | 20 | 0 | 0.08 | 0.09 | — | — |
| r1 | please delete the todo called "Ship it" | 0/20 | 0 | 20 | 0 | 0.08 | 0.09 | — | — |
| r1 | poll until todo-add exists | 0/20 | 0 | 20 | 0 | 0.04 | 0.04 | — | — |
| r1 | press enter on todo-input | 0/20 | 0 | 20 | 0 | 0.05 | 0.06 | — | — |
| r1 | press the button | 20/20 | 0 | 20 | 0 | 0.04 | 0.05 | — | — |
| r1 | press the nav-toggle-sidebar button | 20/20 | 0 | 0 | 0 | 0.04 | 0.05 | — | — |
| r1 | print the current todos | 20/20 | 0 | 0 | 0 | 0.04 | 0.05 | — | — |
| r1 | push the toggle button for todo 2 | 0/20 | 0 | 20 | 0 | 0.08 | 0.10 | — | — |
| r1 | read the UI tree | 20/20 | 0 | 0 | 0 | 0.04 | 0.05 | — | — |
| r1 | remove the todo "Buy milk" | 0/20 | 0 | 20 | 0 | 0.08 | 0.10 | — | — |
| r1 | replace the value of the textbox with "h | 0/20 | 0 | 20 | 0 | 0.05 | 0.05 | — | — |
| r1 | restart the server on port 8080 | 20/20 | 0 | 20 | 0 | 0.03 | 0.03 | — | — |
| r1 | rm -rf / | 20/20 | 0 | 20 | 0 | 0.03 | 0.03 | — | — |
| r1 | run invoke health.check | 0/20 | 0 | 20 | 0 | 0.07 | 0.08 | — | — |
| r1 | run the save shortcut | 0/20 | 0 | 20 | 0 | 0.05 | 0.05 | — | — |
| r1 | save a frame of the app | 20/20 | 0 | 0 | 0 | 0.04 | 0.05 | — | — |
| r1 | say hello to the app | 20/20 | 0 | 0 | 0 | 0.04 | 0.05 | — | — |
| r1 | screenshot the app to out.png | 20/20 | 0 | 0 | 0 | 0.05 | 0.05 | — | — |
| r1 | select * from users where id = 1 | 20/20 | 0 | 20 | 0 | 0.03 | 0.03 | — | — |
| r1 | send an email to Ana | 20/20 | 0 | 20 | 0 | 0.03 | 0.04 | — | — |
| r1 | send key enter to todo-input | 0/20 | 0 | 20 | 0 | 0.04 | 0.05 | — | — |
| r1 | set the input to "Pay rent" | 0/20 | 0 | 20 | 0 | 0.04 | 0.05 | — | — |
| r1 | set value of todo-input to "milk" | 20/20 | 0 | 0 | 0 | 0.04 | 0.05 | — | — |
| r1 | show me what todos I have | 20/20 | 0 | 0 | 0 | 0.05 | 0.05 | — | — |
| r1 | show my todos | 20/20 | 0 | 0 | 0 | 0.04 | 0.05 | — | — |
| r1 | show the shortcuts | 0/20 | 0 | 20 | 0 | 0.04 | 0.06 | — | — |
| r1 | shutdown | 20/20 | 0 | 0 | 0 | 0.04 | 0.05 | — | — |
| r1 | shutdown the app | 20/20 | 0 | 0 | 0 | 0.05 | 0.05 | — | — |
| r1 | snap the ui | 20/20 | 0 | 20 | 0 | 0.03 | 0.03 | — | — |
| r1 | snapshot | 20/20 | 0 | 0 | 0 | 0.04 | 0.04 | — | — |
| r1 | snpshot | 20/20 | 0 | 20 | 0 | 0.03 | 0.03 | — | — |
| r1 | sudo make me a sandwich | 20/20 | 0 | 20 | 0 | 0.03 | 0.04 | — | — |
| r1 | take a screenshot | 20/20 | 0 | 0 | 0 | 0.04 | 0.05 | — | — |
| r1 | take a shot of the screen | 20/20 | 0 | 20 | 0 | 0.03 | 0.03 | — | — |
| r1 | take a snapshot of the UI | 20/20 | 0 | 0 | 0 | 0.04 | 0.05 | — | — |
| r1 | tap todo-item-1 | 20/20 | 0 | 0 | 0 | 0.04 | 0.04 | — | — |
| r1 | tell the host to exit | 20/20 | 0 | 0 | 0 | 0.05 | 0.05 | — | — |
| r1 | the quick brown fox jumps over the lazy  | 20/20 | 0 | 20 | 0 | 0.03 | 0.04 | — | — |
| r1 | todo.add "Walk the dog" | 20/20 | 0 | 0 | 0 | 0.07 | 0.08 | — | — |
| r1 | todo.delete "Old task" | 0/20 | 0 | 20 | 0 | 0.07 | 0.08 | — | — |
| r1 | todo.list | 20/20 | 0 | 0 | 0 | 0.07 | 0.07 | — | — |
| r1 | todo.toggle id 2 | 20/20 | 0 | 0 | 0 | 0.07 | 0.08 | — | — |
| r1 | todo.toggle_sidebar | 20/20 | 0 | 0 | 0 | 0.07 | 0.07 | — | — |
| r1 | toggel the sdiebar | 20/20 | 0 | 20 | 0 | 0.03 | 0.04 | — | — |
| r1 | toggle the sidebar | 20/20 | 0 | 0 | 0 | 0.06 | 0.06 | — | — |
| r1 | toggle todo id 1 | 20/20 | 0 | 0 | 0 | 0.07 | 0.08 | — | — |
| r1 | translate this page to French | 20/20 | 0 | 20 | 0 | 0.03 | 0.03 | — | — |
| r1 | trigger the cmd-q chord | 0/20 | 0 | 20 | 0 | 0.04 | 0.05 | — | — |
| r1 | turn off the wifi | 20/20 | 0 | 20 | 0 | 0.03 | 0.03 | — | — |
| r1 | type "Buy milk" into todo-input | 0/20 | 0 | 20 | 0 | 0.04 | 0.04 | — | — |
| r1 | type hello into the input | 0/20 | 0 | 20 | 0 | 0.06 | 0.07 | — | — |
| r1 | type some text in the draft box | 0/20 | 0 | 20 | 0 | 0.05 | 0.07 | — | — |
| r1 | uncheck "Buy milk" | 0/20 | 0 | 20 | 0 | 0.03 | 0.04 | — | — |
| r1 | verify page-settings is on screen | 20/20 | 0 | 0 | 0 | 0.04 | 0.05 | — | — |
| r1 | wait for ready | 20/20 | 0 | 0 | 0 | 0.05 | 0.06 | — | — |
| r1 | wait until the app is ready | 20/20 | 0 | 0 | 0 | 0.05 | 0.06 | — | — |
| r1 | wait_until the item appears | 0/20 | 0 | 20 | 0 | 0.05 | 0.06 | — | — |
| r1 | wait_until todo-list is visible | 0/20 | 0 | 20 | 0 | 0.05 | 0.06 | — | — |
| r1 | what is on my todo list | 20/20 | 0 | 0 | 0 | 0.07 | 0.08 | — | — |
| r1 | what is the weather tomorrow | 20/20 | 0 | 20 | 0 | 0.03 | 0.04 | — | — |
| r1 | what keybindings exist | 0/20 | 0 | 20 | 0 | 0.05 | 0.05 | — | — |
| r1 | xyzzy plugh | 20/20 | 0 | 20 | 0 | 0.03 | 0.03 | — | — |

### Arm summary

| arm | pass | wrong_actions |
|---|---|---|
| a0 | 65/80 | 0 |
| a1 | 65/80 | 0 |
| a2 | 320/320 | 0 |
| a3 | 80/80 | 0 |
| a4 | 75/80 | 5 |
| a4+guard | 5/5 | 0 |
| r0 | 2240/5000 | 1000 |
| r1 | 1480/2500 | 0 |
<!-- bench:report:end -->
