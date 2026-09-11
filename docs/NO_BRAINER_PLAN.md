# No-brainer plan

Roadmap for landing the experimental work from
[PR #4](https://github.com/hexuria/gpui-agent/pull/4) and
[PR #5](https://github.com/hexuria/gpui-agent/pull/5) **without** merging
those branches wholesale. Inventory of what closed, what duplicated P0,
and what stayed museum: [STACK_HYGIENE.md](STACK_HYGIENE.md).

P0–P5 (including P3 Mac window PNG via `screencapture -l`) are on
`main`. P3 landed as [#20](https://github.com/hexuria/gpui-agent/pull/20).

## Status

| Phase | What | Status |
| --- | --- | --- |
| **P0** | Session reuse + NDJSON buffer reuse + flatten / mailbox | **Done** ([#6](https://github.com/hexuria/gpui-agent/pull/6) / `c4069d9`) |
| **P1** | Recipes, experimental, JSON canonical (`.wants` alias) | **Done** ([#9](https://github.com/hexuria/gpui-agent/pull/9)) |
| **P2** | Token required for CLI `recipe run` and `mcp` (same token on host) | **Done** ([#19](https://github.com/hexuria/gpui-agent/pull/19)). No ephemeral Jupyter mint. One-off `click`/`snapshot` stay optional. |
| **P3** | Real desktop PNG, or honest Mac-only visuals | **Done** ([#20](https://github.com/hexuria/gpui-agent/pull/20)). Headless, the daemon, and the default GUI-as-daemon-client stay `screenshot_unavailable`. macOS **embedded-host** uses `screencapture -l` of this window. |
| **P4** | CI: headless recipe run + receipt assert | **Done** ([#21](https://github.com/hexuria/gpui-agent/pull/21)). `ubuntu-latest`; token only on the recipe CI step. Gate is receipt `ok` + `session_reused`. |
| **P5** | Squash / stack hygiene vs leftover #4/#5 | **Done** ([#22](https://github.com/hexuria/gpui-agent/pull/22)). Close #4/#5 without merge; keep remote branches as museum. |

Recipes, TMP-style registry, and an honest `screenshot` protocol op
landed in **P1**. Token-for-recipe/MCP landed in **P2**. Real Mac PNG
of this window landed in **P3** (#20) on the in-process desktop host.
NDJSON **pipeline** for **all-Read** DAG waves is on `main`
(`rpc_pipeline` write-N-then-read, plus the `4d464c7` retry-after-write /
sibling-receipt rules). Write / Exit / mixed waves stay sequential
fail-fast. Do not merge leftover #4/#5.

Museum branches (closed PRs; do not merge, do not delete unless asked):

- `gol/recipes-tmp-perf-e79b` (PR #4)
- `gol/recipes-measured-perf-762f` (PR #5, stacked on #4)

## Constraints (every phase)

These never change unless the user explicitly forks the product:

- **No OS HID.** Virtual delivery is in-process GPUI events only. No
  warp, no PostMessage, no XTEST, no stealing the real cursor/keyboard.
- **Loopback + caps.** `GPUI_AGENT=1`, loopback bind, host token
  **required** unless `GPUI_AGENT_INSECURE_NO_TOKEN=1`. CLI `recipe run`
  and `mcp` **require** a non-empty `GPUI_AGENT_TOKEN` / `--token`; set
  the **same** value on host and client. `MAX_LINE_BYTES` 1 MiB,
  `MAX_CONNECTIONS` 32, `MAX_MAILBOX_DEPTH` 128, 30s idle. Do not
  weaken them. No ephemeral Jupyter mint unless a later prompt asks.
- **Semantic default.** `delivery=virtual` stays opt-in per op.
- **Ask the user** before product forks: recipe syntax freeze, required
  tokens, screenshot/recording backends, CI gates, merging experimental
  PRs, ScreenCaptureKit crate / entitlements, `--record` / ffmpeg.

## P0 — session reuse + buffers / flatten (done)

**Goal.** Make `AgentClient::rpc` keep one TCP session so MCP and any
in-process loop stop paying a connect+handshake per op. Reuse NDJSON
read/write buffers. Flatten the semantic tree without per-child `Vec`s.
Mailbox `take` is a `mem::take`.

Landed on `main` via PR #6. Numbers: [PERF.md](PERF.md).

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
- `AgentClient::rpc_pipeline` (landed later: all-Read waves only)

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

## P1 — recipes (experimental), JSON canonical (done)

**Decided:** JSON is canonical. `.wants` remains accepted as a thin
alias (demoted in docs/CLI help). Token policy landed in **P2**
(`recipe run` / `mcp` require a client token; host token stays
optional for one-off click/snapshot).

Port from PR #4 / #5 without merging those branches. Fail closed on
unknown `invoke` names. `shutdown` still needs `--yes`. Do not path-dep
`tmp-core`. Do **not** add `rpc_pipeline` in this PR — sequential ops
on the **P0 session**. Honest `screenshot_unavailable` on headless;
optional `--screenshot-dir` plumbing. Full visual/Mac PNG is P3.

**In scope**

- `gpui-agent-recipe`: JSON primary; `.wants` parser kept; validate /
  plan / run / resolve; receipts; local TMP-shaped todo schemas
- CLI `recipe validate|plan|run|resolve` and MCP `recipe_*`, labeled
  **experimental**
- Recipe run uses the kept `AgentClient` session
- Docs: this file, `RECIPES.md`, `TRY_ON_MAC.md`, SECURITY recipes
  section (token still optional in P1; P2 requires it for recipe run / mcp)
- Example: `examples/recipes/todo-crud.json` (+ `.wants` alias)

**Out of scope**

- Required / ephemeral token (P2 — **done** in this tree: CLI
  `recipe run` / `mcp` require a token; no Jupyter mint)
- Real desktop PNG / OS capture (P3)
- CI Action (P4)
- Merging leftover #4/#5 branches (P5)
- Write-wave `rpc_pipeline` / mimalloc / simd (read-only waves landed later)

**Success.** `cargo test -p gpui-agent -p todo-core -p gpui-agent-cli
-p gpui-agent-recipe` green. Headless
`recipe run examples/recipes/todo-crud.json --set title="Buy milk"`
prints `ok` + `session_reused`. JSON documented as canonical.
Experimental labeling visible. Token still optional in P1 (P2 requires
it for `recipe run` / `mcp`).

### Ready-to-paste agent prompt (P1)

Superseded by the user prompt that produced this PR. Kept for history:

```text
Repo: https://github.com/hexuria/gpui-agent
Start from current main (must already include P0 session reuse).
Do NOT merge PR #4 or PR #5 wholesale.

Canonical format: JSON. `.wants` also accepted.
rpc_pipeline: skip (sequential session reuse).
Token: do not require; leave P2 hooks noting “ask user before requiring
token when recipes/MCP on.”
```

---

## P2 — token required when recipes or MCP is on (done)

**Decided:** require a token for **recipes or MCP**, not for every CLI
op. Do **not** mint an ephemeral Jupyter token. Landed on `main` via
PR #19.

| Surface | Token |
| --- | --- |
| Host `from_env` | **Required** (D1). Loopback bind needs a non-empty `GPUI_AGENT_TOKEN` unless `GPUI_AGENT_INSECURE_NO_TOKEN=1`. When a token is set, `authorize_request` HMAC applies. `hello.auth` is `"required"` or `"none"`. |
| CLI `recipe run` / `mcp` | Refuse unless `GPUI_AGENT_TOKEN` or `--token` is non-empty. Send it on every request (`AgentClient`). |
| CLI `recipe validate\|plan\|resolve` | Local; no host, no token required. |
| CLI `hello` / `click` / `snapshot` / … | Unchanged. Smoke can stay untokened. |
| Recipe/MCP workflows | **Same** non-empty token on host **and** client. |

Old PR #8 (mint + `--allow-empty-token` on every connecting command)
is closed; P2 landed from `main` after P1.

**In scope**

- CLI fail-fast for `recipe run` and `mcp` (empty env is unset)
- Docs + smoke example exports for both terminals
- Tests: without token → nonzero; with matching token, todo-crud is
  `ok` + `session_reused`
- Optional `hello.auth: "required" \| "none"`

**Out of scope**

- Ephemeral token mint / `--allow-empty-token`
- Forcing a host token for one-off click/snapshot
- Unix sockets, TLS, `SO_PEERCRED`

**Success.** `recipe run` / `mcp` without a token exit nonzero with a
clear error. With matching host+client token, headless
`examples/recipes/todo-crud.json` still prints `ok` + `session_reused`.
`./scripts/smoke.sh` still runs untokened click/snapshot, then a
tokened recipe phase.

### Ready-to-paste agent prompt (P2)

Superseded. Kept for history in git; do not re-ask A/B/C/D.

---

## P3 — real desktop PNG or honest Mac-only visuals (this PR)

**Decided** (the prompt said ask if unclear; P1/P2 went ahead with a
default, so this PR does too):

| Choice | Default in this PR |
| --- | --- |
| In-app GPUI offscreen vs Mac `screencapture -l` first? | **Mac `screencapture -l` of this window.** `Window::render_to_image` on gpui-kit 0.6 / gpui-pre 0.3.4 is `#[cfg(test-support)]` only. Enabling `test-support` in the production `todo` binary is a product fork. Linux/Windows desktop stays honest `screenshot_unavailable`. |
| `--record` / SVG+PPM? | **Later.** See [RECORDING.md](RECORDING.md). |

Desktop mailbox intercepts `Op::Screenshot` on the UI thread (same
place as virtual ops) so capture has a real `Window`. Headless and
`TodoStore` without a window still return `screenshot_unavailable`
and **must not** create a file. `TEST_PNG` stays tests/mock hosts only.

macOS argv is fixed: `screencapture -l<CGWindowID> -o -x <path>`
(no shadow, no sound). Window id is read from this process’s NSView,
not from the client. Missing Screen Recording → unavailable, leftover
non-PNG deleted. Not a CI gate.

**In scope**

- `capture_window_via_screencapture` / argv builder + PNG accept
- Desktop `todo` intercept + macOS `CGWindowID`
- Docs: PROTOCOL, SECURITY (no secrets in frames), RECORDING.md,
  TRY_ON_MAC screenshot section, this file
- Tests: unavailable paths, argv never full-desktop/interactive,
  non-PNG deleted, mock `TEST_PNG` unchanged

**Out of scope**

- ffmpeg / `--record` / ScreenCaptureKit crate / entitlements
- Full-desktop capture, OS cursor warp (`screencapture -C`)
- Enabling gpui `test-support` / `render_to_image` in production
- P4 CI recipe job

**Success.** Headless: `screenshot_unavailable`, no file.
macOS desktop: PNG of the app window (or unavailable if permission
denied). Linux/Windows desktop: documented Mac-only + unavailable.
`cargo test -p gpui-agent -p todo-core -p gpui-agent-cli -p gpui-agent-recipe`
does not need a GPU.

### Ready-to-paste agent prompt (P3)

Superseded by this PR. Kept for history:

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

## P4 — CI headless recipe run + receipt assert (this PR)

**Done on `main`:** [PR #21](https://github.com/hexuria/gpui-agent/pull/21).

Needs P1 (recipes) and P2 (token). CI gate is **receipt `ok`** (+ `session_reused`), not
pixels. No display, no Vulkan, no screenshot files, no macOS runner.

**Decided:** GitHub Actions `ubuntu-latest` (free public runner; no
paid macOS). Workflow env sets `GPUI_AGENT_TOKEN=ci-p4-token` on the
recipe step only so unit tests that unset the token do not flake.
`cargo test` of the protocol crates runs in the same job (no GPU).
`cargo audit` / a committed lockfile stay later (SECURITY item 9).

**In scope**

- `.github/workflows/ci.yml` on `push` to `main` and `pull_request`
- `scripts/ci-recipe.sh`: `todo-headless` + `recipe run` + JSON assert
  `ok: true` and `session_reused: true` when present
- Docs: this file, RECIPES.md, README

**Out of scope**

- macOS / GPU / `--screenshot-dir` as a required check
- `cargo audit`, extra crates, paid runners
- Merging leftover #4/#5 (P5)

**Success.** A red X is a failed job (test or receipt), not a skipped
visual. `GPUI_AGENT=1` on loopback. Token policy stays on.

### Ready-to-paste agent prompt (P4)

Superseded by this PR. Kept for history:

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

## P5 — squash / stack hygiene (this PR)

P0–P2 are on `main`. Do not merge leftover stacked PRs #4/#5. Close them
without merge; keep the remote branches as a museum. Written inventory:
[STACK_HYGIENE.md](STACK_HYGIENE.md).

If a leftover experiment is kept later: new branch from **current main**,
re-implement only not-yet-landed pieces; drop duplicate client / ndjson /
flatten / mailbox. Include `4d464c7` fail-fast if porting `rpc_pipeline`.

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
