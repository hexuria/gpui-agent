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

**A0 — LLM over baseline MCP (claude-haiku-5-5, n=5 × 16 tasks):**
*not run — `ANTHROPIC_API_KEY` not available in the environment. The manual
`bench-llm` workflow (`workflow_dispatch`) runs it once the secret exists;
baseline A0 numbers must be recorded here before A1 is evaluated for GATE-T.*

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

**GATE-T (A1 `claude-haiku-5-5` × `act` vs A0, n=5):** *not run —
`ANTHROPIC_API_KEY` is not available in this environment, and A0's baseline
was never recorded (see Baseline §A0). GATE-T stays open until both arms
run on the `bench-llm` workflow.* The deterministic half is verified
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
- GATE-T's A1 rerun (`wrong_action = 0` with fingerprints on) is still
  blocked on `ANTHROPIC_API_KEY` (see M4).

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
- **GATE-L (A2 local arm, T01–T16, 1 attempt each): 16/16 pass,
  wrong=0.** Includes add/type/add-and-wait, mark-done (exact,
  duplicate-title, 200-row list), delete (exact + missing-target
  refusal: `Delete "Walk dog"` abstains exit 2, state unchanged),
  navigation, sidebar collapse, settings checkbox, quit-refusal, and
  `then`/`wait` chained goals. Protocol calls per run stay flat
  (observe + dispatch + one revision-aware verify-snapshot per step;
  T15 = 6 calls).
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
- GATE-T's A3 arm (`--escalate-cmd`) remains blocked on
  `ANTHROPIC_API_KEY` (see M4/M7).

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
- **GATE-E: blocked — `ANTHROPIC_API_KEY` is unset, so A3 (5 attempts
  vs A0) was not run.** The wire, validation, and blocked/abstain
  outcomes are covered by the scripted-endpoint tests above; A2
  remains 16/16 with the new runner.

<!-- bench:report:start -->
_Generated by `gpui-agent-bench report`; do not edit by hand._

| arm | task | pass | wrong | refused | cap | wall_ms p50 | wall_ms p90 | in_tok p50 | usd/passed |
|---|---|---|---|---|---|---|---|---|---|
| r0 |  | 20/20 | 0 | 20 | 0 | 0.00 | 0.00 | — | — |
| r0 |     | 20/20 | 0 | 20 | 0 | 0.00 | 0.00 | — | — |
| r0 | Add a todo "Buy milk" | 20/20 | 0 | 0 | 0 | 0.02 | 0.03 | — | — |
| r0 | Collapse the sidebar | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | Delete "Write docs" | 0/20 | 0 | 20 | 0 | 0.02 | 0.03 | — | — |
| r0 | I need a new todo item for groceries | 0/20 | 0 | 20 | 0 | 0.02 | 0.05 | — | — |
| r0 | I want to add "Feed cat" as a todo | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | Mark "Write docs" as done | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | activate the settings button | 0/20 | 20 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | add "Pay rent" to my list | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | add a todo titled Buy milk | 20/20 | 0 | 0 | 0 | 0.02 | 0.03 | — | — |
| r0 | addd a toodo pls | 20/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | append "world" to the text field | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | assert the input field is empty | 0/20 | 0 | 20 | 0 | 0.02 | 0.04 | — | — |
| r0 | assert todo-list exists | 0/20 | 20 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | block until the host is ready | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | book a flight to Lisbon | 20/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | buy milk | 20/20 | 0 | 20 | 0 | 0.01 | 0.02 | — | — |
| r0 | call the invoke command sync.now | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | capture a png of the window | 20/20 | 0 | 0 | 0 | 0.02 | 0.04 | — | — |
| r0 | check off "Ship it" | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | check that todo-nav is visible | 0/20 | 20 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | click "submit" | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | click on the delete button | 0/20 | 0 | 20 | 0 | 0.02 | 0.03 | — | — |
| r0 | click the add button | 0/20 | 0 | 20 | 0 | 0.02 | 0.04 | — | — |
| r0 | click todo-add | 0/20 | 20 | 0 | 0 | 0.02 | 0.05 | — | — |
| r0 | clik the buton | 0/20 | 20 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | close the automation host | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | commit my changes to git | 20/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | complete the todo "Buy milk" | 0/20 | 0 | 20 | 0 | 0.02 | 0.03 | — | — |
| r0 | create a new task: call the dentist | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | delete everything everywhere | 20/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | delete the item "Walk dog" | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | delete todo id 4 | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | do the thing | 0/20 | 20 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | drop the task "Pay rent" | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | enter the text "milk" in todo-input | 0/20 | 20 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | erase todo "Buy milk" | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | fill todo-input with "new task" | 0/20 | 20 | 0 | 0 | 0.02 | 0.04 | — | — |
| r0 | fire keybinding quit | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | flip the done state of todo 7 | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | get rid of "Item 42" | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | grab the semantic tree | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | handshake with the host | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | hello | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | help | 20/20 | 0 | 20 | 0 | 0.01 | 0.02 | — | — |
| r0 | hide the nav sidebar | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | hit backspace in the input | 0/20 | 0 | 20 | 0 | 0.02 | 0.04 | — | — |
| r0 | hola como estas | 20/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | how do I bake bread | 20/20 | 0 | 20 | 0 | 0.02 | 0.04 | — | — |
| r0 | invoke a host command | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | invoke nav.go to settings | 0/20 | 0 | 20 | 0 | 0.02 | 0.03 | — | — |
| r0 | key tab on the form | 0/20 | 0 | 20 | 0 | 0.02 | 0.05 | — | — |
| r0 | keybinding toggle-sidebar scope focused | 0/20 | 0 | 20 | 0 | 0.02 | 0.04 | — | — |
| r0 | list all todos | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | list keybindings | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | mark the task "Call Ana" complete | 0/20 | 0 | 20 | 0 | 0.02 | 0.06 | — | — |
| r0 | mark todo 3 done | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | new todo "Write docs" | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | open https://example.com in the browser | 0/20 | 20 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | open up the sidebar again | 20/20 | 0 | 0 | 0 | 0.02 | 0.05 | — | — |
| r0 | perceive the current screen | 20/20 | 0 | 0 | 0 | 0.02 | 0.04 | — | — |
| r0 | play some music | 20/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | please add a todo for me: submit taxes | 0/20 | 0 | 20 | 0 | 0.03 | 0.03 | — | — |
| r0 | please delete the todo called "Ship it" | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | poll until todo-add exists | 0/20 | 20 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | press enter on todo-input | 0/20 | 20 | 0 | 0 | 0.02 | 0.03 | — | — |
| r0 | press the button | 0/20 | 20 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | press the nav-toggle-sidebar button | 0/20 | 20 | 0 | 0 | 0.02 | 0.03 | — | — |
| r0 | print the current todos | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | push the toggle button for todo 2 | 0/20 | 20 | 0 | 0 | 0.03 | 0.03 | — | — |
| r0 | read the UI tree | 20/20 | 0 | 0 | 0 | 0.02 | 0.05 | — | — |
| r0 | remove the todo "Buy milk" | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | replace the value of the textbox with "h | 0/20 | 0 | 20 | 0 | 0.02 | 0.04 | — | — |
| r0 | restart the server on port 8080 | 0/20 | 20 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | rm -rf / | 20/20 | 0 | 20 | 0 | 0.01 | 0.02 | — | — |
| r0 | run invoke health.check | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | run the save shortcut | 0/20 | 0 | 20 | 0 | 0.02 | 0.03 | — | — |
| r0 | save a frame of the app | 20/20 | 0 | 0 | 0 | 0.02 | 0.03 | — | — |
| r0 | say hello to the app | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | screenshot the app to out.png | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | select * from users where id = 1 | 20/20 | 0 | 20 | 0 | 0.02 | 0.03 | — | — |
| r0 | send an email to Ana | 20/20 | 0 | 20 | 0 | 0.02 | 0.03 | — | — |
| r0 | send key enter to todo-input | 0/20 | 0 | 20 | 0 | 0.02 | 0.05 | — | — |
| r0 | set the input to "Pay rent" | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | set value of todo-input to "milk" | 0/20 | 20 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | show me what todos I have | 20/20 | 0 | 0 | 0 | 0.02 | 0.03 | — | — |
| r0 | show my todos | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | show the shortcuts | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | shutdown | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | shutdown the app | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | snap the ui | 0/20 | 20 | 0 | 0 | 0.02 | 0.04 | — | — |
| r0 | snapshot | 20/20 | 0 | 0 | 0 | 0.02 | 0.04 | — | — |
| r0 | snpshot | 20/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | sudo make me a sandwich | 20/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | take a screenshot | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | take a shot of the screen | 0/20 | 20 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | take a snapshot of the UI | 20/20 | 0 | 0 | 0 | 0.02 | 0.04 | — | — |
| r0 | tap todo-item-1 | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | tell the host to exit | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | the quick brown fox jumps over the lazy  | 0/20 | 20 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | todo.add "Walk the dog" | 20/20 | 0 | 0 | 0 | 0.02 | 0.03 | — | — |
| r0 | todo.delete "Old task" | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | todo.list | 20/20 | 0 | 0 | 0 | 0.01 | 0.02 | — | — |
| r0 | todo.toggle id 2 | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | todo.toggle_sidebar | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | toggel the sdiebar | 0/20 | 20 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | toggle the sidebar | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | toggle todo id 1 | 20/20 | 0 | 0 | 0 | 0.02 | 0.05 | — | — |
| r0 | translate this page to French | 20/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | trigger the cmd-q chord | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | turn off the wifi | 0/20 | 20 | 0 | 0 | 0.02 | 0.04 | — | — |
| r0 | type "Buy milk" into todo-input | 0/20 | 20 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | type hello into the input | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | type some text in the draft box | 0/20 | 0 | 20 | 0 | 0.02 | 0.04 | — | — |
| r0 | uncheck "Buy milk" | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | verify page-settings is on screen | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | wait for ready | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | wait until the app is ready | 20/20 | 0 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | wait_until the item appears | 0/20 | 20 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | wait_until todo-list is visible | 0/20 | 20 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | what is on my todo list | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | what is the weather tomorrow | 0/20 | 20 | 0 | 0 | 0.02 | 0.02 | — | — |
| r0 | what keybindings exist | 0/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
| r0 | xyzzy plugh | 20/20 | 0 | 20 | 0 | 0.02 | 0.02 | — | — |
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
| r0 | 1120/2500 | 500 |
| r1 | 1480/2500 | 0 |
<!-- bench:report:end -->
