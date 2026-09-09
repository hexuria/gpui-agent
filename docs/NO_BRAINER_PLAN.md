# No-brainer plan

Roadmap for landing the experimental work from
[PR #4](https://github.com/hexuria/gpui-agent/pull/4) and
[PR #5](https://github.com/hexuria/gpui-agent/pull/5) **without** merging
those branches wholesale.

P0 is [PR #6](https://github.com/hexuria/gpui-agent/pull/6). P1 is the
stacked recipe crate. Later phases stay **documented here** until a
human picks them. Do not implement P2–P5 on a P1 branch.

## Status

| Phase | What | Status |
| --- | --- | --- |
| **P0** | Session reuse + NDJSON buffer reuse + flatten / mailbox | [PR #6](https://github.com/hexuria/gpui-agent/pull/6) |
| **P1** | Recipes, experimental; JSON canonical (`.wants` thin alias) | **This stacked PR** |
| **P2** | Token required / ephemeral when recipes or MCP is on | Not started. Details **UNDECIDED — ask the user** |
| **P3** | Real desktop PNG, or honest Mac-only visuals | Not started |
| **P4** | CI: headless recipe run + receipt assert | Not started (needs P1) |
| **P5** | Squash / stack hygiene vs leftover #4/#5 | Not started |

Recipes, TMP-style registry, screenshot protocol, recording, and NDJSON
**pipeline** (`rpc_pipeline` / DAG waves) stay **out of P0**. Pipeline is
a later phase: it has no protocol bump but is easy to review-mix with
recipes. Prefer it with P1 (or a tiny follow-up after P1), not here.

Reference-only branches (do not merge as-is):

- `gol/recipes-tmp-perf-e79b` (PR #4)
- `gol/recipes-measured-perf-762f` (PR #5, stacked on #4)

## Constraints (every phase)

These never change unless the user explicitly forks the product:

- **No OS HID.** Virtual delivery is in-process GPUI events only. No
  warp, no PostMessage, no XTEST, no stealing the real cursor/keyboard.
- **Loopback + caps.** `GPUI_AGENT=1`, loopback bind, optional token
  (until P2), `MAX_LINE_BYTES` 1 MiB, `MAX_CONNECTIONS` 32,
  `MAX_MAILBOX_DEPTH` 128, 30s idle. Do not weaken them.
- **Semantic default.** `delivery=virtual` stays opt-in per op.
- **Ask the user** before product forks: recipe syntax freeze, required
  tokens, screenshot/recording backends, CI gates, merging experimental
  PRs.

## P0 — session reuse + buffers / flatten (this PR)

**Goal.** Make `AgentClient::rpc` keep one TCP session so MCP and any
in-process loop stop paying a connect+handshake per op. Reuse NDJSON
read/write buffers. Flatten the semantic tree without per-child `Vec`s.
Mailbox `take` is a `mem::take`.

**In scope**

- `AgentClient` live session; `rpc_once` for benches / comparison
- `crates/gpui-agent/src/ndjson.rs` (`write_json_line`,
  `read_limited_line_into`)
- `UiTree::flatten_into` / `visit` (no `node_count` pre-walk)
- Mailbox stores `MailboxRequest` and `mem::take`s the queue
- Criterion: session vs reconnect, flatten, mailbox, snapshot serialize
- Docs: this file, `docs/PERF.md` (P0 numbers only)

**Out of scope**

- `gpui-agent-recipe`, CLI `recipe`, MCP `recipe_*`
- Protocol `screenshot`, recording, TMP registry
- `AgentClient::rpc_pipeline`

**Success.** Tests green for `gpui-agent`, `todo-core`, `gpui-agent-cli`.
`rpc_session_reuse_32_hellos` is hundreds of times faster than
`rpc_once_reconnect_32_hellos`. Existing `./scripts/smoke.sh` still
works. No recipe crate on the branch.

**Verify**

```bash
cargo test -p gpui-agent -p todo-core -p gpui-agent-cli
cargo bench -p gpui-agent --bench agent_perf -- --quick
./scripts/smoke.sh
```

Numbers: [PERF.md](PERF.md).

---

## P1 — recipes (experimental), one canonical format

**This stacked PR.** JSON is canonical. `.wants` is a **thin alias**
that compiles to the same `Recipe` (not a second product). Execution is
sequential `rpc` on the P0 session — **no** `rpc_pipeline`. Local schema
registry only (no `tmp-core`, no shell data sources). Unknown `invoke`
fails closed. `shutdown` needs `--yes`.

If you want `.wants` deleted, say so; it is sugar, not a freeze.

### Ready-to-paste agent prompt (P1)

```text
Repo: https://github.com/hexuria/gpui-agent
Start from current main (must already include P0 session reuse).
Do NOT merge PR #4 or PR #5 wholesale.

## STOP — ask the user first
Recipe format is UNDECIDED. Before any code, ask:
1. Canonical format: JSON only, .wants only, or JSON canonical + .wants as a thin alias?
2. Is rpc_pipeline / DAG waves in this PR, or sequential session reuse only?
3. Sample recipes: todo CRUD only, or also a generic hello/snapshot fixture?

Do not invent a third syntax. Do not add TMP cloud registry or shell data sources.

## Goal
Experimental `gpui-agent-recipe` + CLI/MCP `recipe validate|plan|run` so an
agent can run many protocol ops in one process on the existing AgentClient
session. This does not replace semantic RPC, does not steal OS HID, and
must not weaken caps/token/loopback.

## Success
- cargo test includes -p gpui-agent-recipe
- recipe run on todo-headless prints a receipt with ok + session_reused
- unknown invoke and shell-like resolve fail closed
- shutdown in a recipe requires --yes
- docs/RECIPES.md + a short README pointer (recipes are experimental)
- No screenshot/recording protocol unless the user added P3 to this PR

## Constraints
No OS HID. Loopback + 1 MiB / 32 conn / 128 mailbox. Semantic default.
Ask the user on product forks (syntax freeze, tmp-core path-dep).
Reference-only: PR #4 gol/recipes-tmp-perf-e79b, PR #5 gol/recipes-measured-perf-762f.
```

---

## P2 — token required / ephemeral when recipes or MCP is on

**UNDECIDED details — ask the user.** Security audit H1: token is
optional. Recipes/MCP make an open loopback socket cheaper to drive.

Options to present (do not pick silently):

1. Document-only (status quo) + louder README
2. Require `GPUI_AGENT_TOKEN` when MCP or `recipe run` starts (CLI-side)
3. Server generates an ephemeral token at bind, prints once (Jupyter model)
4. Both 2 and 3, with an explicit `--allow-empty-token` for local smoke

Do not break `./scripts/smoke.sh` without a documented env example.

### Ready-to-paste agent prompt (P2)

```text
Repo: https://github.com/hexuria/gpui-agent
Start from main. P0 session reuse is already merged.

## STOP — ask the user first
Token policy is UNDECIDED (security H1). Ask which of:
(A) docs only
(B) CLI/MCP refuse to talk if GPUI_AGENT_TOKEN is unset
(C) server mints ephemeral token at bind and prints it once
(D) B+C with --allow-empty-token for smoke scripts
Also ask: does recipe run (if present) follow the same rule?

## Goal
Implement only the chosen policy. Update docs/SECURITY.md H1, README,
smoke scripts, and tests. Do not add Unix sockets, TLS, or SO_PEERCRED
unless the user asked (those are larger than P2).

## Success
- Chosen policy has tests (refuse / mint / allow-empty)
- smoke.sh still documented and green under that policy
- Caps, loopback, no OS HID unchanged
- No recipe/screenshot work unless already on main

## Constraints
No OS HID. Do not weaken line/conn/mailbox caps. Semantic default.
Ask before product forks (required vs optional token in shipping apps).
```

---

## P3 — real desktop PNG or honest Mac-only visuals

Headless must stay honest: `screenshot_unavailable`, **no fake PNG**.
Desktop PNG of the **app surface** (not the full desktop) is the AI-useful
path. Mac `screencapture -l` is observe-only and not a CI gate.

Do not land a protocol `screenshot` op that invents pixels. Video/`--record`
is secondary; do not block P3 on ffmpeg or ScreenCaptureKit.

### Ready-to-paste agent prompt (P3)

```text
Repo: https://github.com/hexuria/gpui-agent
Start from main.

## Goal
Observe-only visuals for agents:
- Protocol/CLI screenshot writes a PNG of the app surface on the host
  machine (path on disk; image does not ride the 1 MiB NDJSON line).
- Headless (and any host without a pixel export) returns
  screenshot_unavailable and MUST NOT create a file.
- Optional Mac helper: screencapture -l for that window only (no HID,
  needs Screen Recording permission). Not a CI gate.
- Do not add full-desktop capture, OS cursor warp, or a fake TEST_PNG
  on the headless success path. A test double is OK only behind cfg(test)
  and must not be what AgentHost::screenshot returns in production.

## Ask the user if unclear
- In-app GPUI offscreen PNG vs Mac window script first?
- Is --record / SVG+PPM in this PR or later?

## Success
- Headless: error screenshot_unavailable, no file
- Desktop (when implemented) or documented Mac-only limitation
- docs/RECORDING.md or a short PROTOCOL section; SECURITY: no secrets in frames
- Tests for unavailable + (if present) mocked PNG write

## Constraints
No OS HID. Loopback/caps/token unchanged. Semantic default.
Ask before product forks (entitlements, ScreenCaptureKit crate).
Reference-only screenshot work lives on PR #4; re-implement cleanly if it fights.
```

---

## P4 — CI headless recipe run + receipt assert

Needs P1. CI gate is **receipt `ok`**, not pixels. Optional semantic
SVG/PPM frames must not be the required check.

### Ready-to-paste agent prompt (P4)

```text
Repo: https://github.com/hexuria/gpui-agent
Start from main. P1 recipes must already exist; if they do not, stop.

## Goal
CI (GitHub Actions or the repo's existing check) runs a headless
todo-headless + `gpui-agent recipe run` (or the canonical P1 command)
and asserts the receipt JSON has "ok": true (and session_reused if that
field exists). No display, no Vulkan, no screenshot files required.

## Success
- Workflow file in-repo, deterministic, uses GPUI_AGENT=1 on loopback
- Failure is a failed job, not a skipped visual
- Does not require Mac, ffmpeg, or GPU
- Docs: one paragraph in RECIPES.md or README pointing at the job

## Constraints
No OS HID. Do not open non-loopback. Do not disable the token policy
from P2 if it already landed — set a test token in the workflow instead.
Semantic default. Ask the user before adding paid runners or extra crates.
```

---

## P5 — squash / stack hygiene

After P0 (and whatever of P1–P4 landed), do not merge leftover stacked
PRs #4/#5 onto main. Close or retarget them. If recipes still live only
on those branches, rebase **onto current main** and drop commits that
duplicate P0.

### Ready-to-paste agent prompt (P5)

```text
Repo: https://github.com/hexuria/gpui-agent
Start from main. Read docs/NO_BRAINER_PLAN.md.

## Goal
History hygiene only (unless the user also names a code phase):
- List open PRs #4, #5, and any recipe/screenshot stacks.
- If P0 is on main: those PRs must NOT merge as-is (they replay session
  reuse plus recipes). Comment on each with the rebase plan; do not merge.
- If the user wants a leftover experiment kept: new branch from main,
  cherry-pick or re-implement only the not-yet-landed pieces (recipes,
  screenshot, pipeline). Drop duplicate client/ndjson/flatten/mailbox.
- Squash noisy agent fixup commits on the surviving branch if the user
  wants a short reviewable history.

## Success
- Written plan in the PR body or a short docs note: what closed, what
  rebased, what duplicated P0 and was dropped
- No force-push to main
- No HID / cap / token regressions

## Constraints
Do not merge PR #4 or #5 onto main. Ask before closing PRs the user
still wants as a museum branch. No OS HID. Ask on product forks.
```

---

## Explicitly later / not a phase number

- Unix socket + `SO_PEERCRED` (SECURITY M2)
- Path-dep on `tmp-core`
- Wire `batch` op (rejected on #5; pipeline of ordinary lines is enough)
- simd-json, tokio, scoped threads on the tree, mimalloc (rejected; see
  [PERF.md](PERF.md) and PR #5)
- WASM / mobile `AgentHost`
- AccessKit auto-export of ids
