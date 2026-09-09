# PR verify report (no-brainer drafts #20–#24)

FACTS ONLY. No merges were performed. Verified 2026-09-09 on a **Linux x86_64**
cloud VM (`uname -s -m`), `rustc 1.98.1 (48a229cea 2026-09-01)`.

Base: `origin/main` = `0c5f3817f4e8792b4829e869239d7cf392d6cea4`
(P0–P2 landed; #19 merged).

Each draft was checked out independently (merge-base with `main` is
`0c5f381`; **0 commits behind**, rebase/replay onto current `main` had
**no conflicts**). Tests used a **separate** `CARGO_TARGET_DIR` per PR
so artifacts could not leak across branches.

Command (every PR):

```bash
cargo test -p gpui-agent -p todo-core -p gpui-agent-cli -p gpui-agent-recipe
```

Counts below exclude empty doc-test suites (`0 passed` doc-tests on
`gpui-agent`, `gpui-agent-recipe`, `todo-core`).

## Summary table

| PR | Rebases on main? | Tests | Claim tests | Verdict | Evidence |
| --- | --- | --- | --- | --- | --- |
| [#20](https://github.com/hexuria/gpui-agent/pull/20) P3 Mac PNG | Yes (0 behind, 0 conflicts) at `8c2c4bd` | **165 passed / 0 failed** (56+23+8+57+14+6+1) | Headless/Linux unavailable + no file: **PASS**. Live `screencapture -l` PNG: **not run** | **UNPROVEN ON CI** | Linux honest path proven. Do not treat `cargo test` as a Mac window PNG. Mac evidence: TRY_ON_MAC §8 (below). |
| [#21](https://github.com/hexuria/gpui-agent/pull/21) P4 CI receipt | Yes (0 behind, 0 conflicts) at `9ce2eaf` | **164 passed / 0 failed** (49+23+7+8+57+14+5+1) | Receipt requires `ok` **and** `session_reused` (missing field fails). `ci-recipe.sh` with token **ok**. Token-unset CLI tests **8/8**. | **MERGE-READY** (after verify fixes on the branch) | Original assert skipped missing `session_reused`; tightened + claim tests pushed. `GPUI_AGENT=1 GPUI_AGENT_TOKEN=ci-p4-token ./scripts/ci-recipe.sh` → `ci-recipe ok`. |
| [#22](https://github.com/hexuria/gpui-agent/pull/22) P5 hygiene | Yes (0 behind, 0 conflicts) at `a0e26d3` | **157 passed / 0 failed** (49+23+8+57+14+5+1) — same shape as `main` (docs-only rust) | GitHub: #4/#5 **closed, not merged**. Inventory updated for later drafts #23/#24. | **MERGE-READY** | Not redundant with `main` (`docs/STACK_HYGIENE.md` is new). Closing #4/#5 already happened; this PR is the in-tree record. |
| [#23](https://github.com/hexuria/gpui-agent/pull/23) all-Read pipeline | Yes (0 behind, 0 conflicts) at `414c72c` | **164 passed / 0 failed** (53+23+8+58+16+5+1) | Wide Read records siblings on assert fail; writes sequential; screenshot-dir per-step; EOF after first pipelined reply does not retry until timeout. | **MERGE-READY** | Claim tests named below all `ok`. Criterion **not** re-run; PERF.md 145µs / 536µs **not** re-proven here. |
| [#24](https://github.com/hexuria/gpui-agent/pull/24) MCP isError + `$params` | Yes (0 behind, 0 conflicts) at `764a278` | **162 passed / 0 failed** (49+25+8+60+14+5+1) | Failed `recipe_run` → `isError: true`; success has no `isError`; `$params` named `id` do not rewrite graph; `$placeholder` in `id`/`needs` fails validate. | **MERGE-READY** | Claim tests named below all `ok`. |

**Do not merge any of these PRs as part of this report.** Humans merge after reading the verdicts.

## Recommended merge order (dependencies / conflicts)

Code is independent except **#20 vs #24 both edit** `crates/gpui-agent-cli/src/mcp.rs`
(P3 tool description strings vs `isError` refactor). Pairwise `git merge-tree`
also shows **docs-only** overlap on `README.md` / `docs/NO_BRAINER_PLAN.md` /
`docs/RECIPES.md` / `docs/SECURITY.md` for every other pair.

1. **#24** — smallest fail-closed fix; MERGE-READY; land before #20 to avoid the `mcp.rs` conflict.
2. **#21** — visual-free CI gate; MERGE-READY; once on `main`, later PRs get Actions. Token is only on the recipe step (cargo test does not export `GPUI_AGENT_TOKEN`).
3. **#23** — pipeline; MERGE-READY; no rust overlap with #21/#24.
4. **#20** — only after **Mac evidence** (below). Rebase onto `main` that already contains #24.
5. **#22** — docs inventory; MERGE-READY; rebase last so `STACK_HYGIENE.md` does not fight every other docs hunk.

Museum leftovers **#4 / #5 stay closed**. Do not merge them.

## Security spot-check (all five)

`git diff origin/main...HEAD` for `security.rs`, `virtual_input.rs`,
`mailbox.rs`, `dispatch.rs`: **empty** on every PR.

Caps on #23 (unchanged values): `MAX_LINE_BYTES = 1 MiB`, `MAX_CONNECTIONS = 32`,
`MAX_MAILBOX_DEPTH = 128`. Pipeline still sends ordinary tokened NDJSON lines
(no wire `batch`). #20 `screencapture` argv is `-l<CGWindowID> -o -x <path>`
(window-only; no `-i`/`-S`/`-w`). No OS HID. P2 token policy not relaxed
(`recipe run` / `mcp` still require a token; #21 sets `ci-p4-token` only on
the recipe CI step).

---

## #20 — P3 macOS app-window PNG (`8c2c4bd`)

Branch `gol/no-brainer-p3-screenshot-b20f`. Original `59dee77` plus claim-test
commit `8c2c4bd`.

### Rebase

`git merge-base origin/main HEAD` = `0c5f381`. Ahead 2 / behind 0. No conflicts.

### `cargo test` summary (isolated `/tmp/target-pr20`)

```
gpui-agent lib:            56 passed; 0 failed
gpui-agent-cli bin:        23 passed; 0 failed
recipe_mcp_token:           8 passed; 0 failed
gpui-agent-recipe lib:     57 passed; 0 failed
todo_recipe:               14 passed; 0 failed
todo-core lib:              6 passed; 0 failed
tcp_crud:                   1 passed; 0 failed
```

**165 passed / 0 failed.**

Claim tests that ran on this Linux VM:

```
screenshot::tests::capture_without_macos_does_not_invent_a_file ... ok
screenshot::tests::screencapture_argv_is_this_window_only ... ok
screenshot::tests::screencapture_refuses_window_id_zero ... ok
screenshot::tests::test_png_is_mock_helper_not_a_live_window_capture ... ok
todo-core::tests::desktop_store_screenshot_stays_unavailable_without_a_window ... ok
todo-core::tests::screenshot_is_honestly_unavailable ... ok
```

`capture_without_macos_does_not_invent_a_file` is `#[cfg(not(target_os = "macos"))]`
so a Mac `cargo test` cannot skip-pass it.

### Smoke (headless)

`GPUI_AGENT=1 GPUI_AGENT_ADDR=127.0.0.1:17432 ./scripts/smoke.sh` → **exit 0**.

Screenshot line:

```
==> screenshot is honestly unavailable on headless (no fake PNG)
error: screenshot_unavailable: headless host has no pixel surface
```

Script fails if that op succeeds or if a PNG file is created. It did not.

### What this VM cannot prove

Live macOS `screencapture -l<CGWindowID>` of `apps/todo` with Screen Recording.
`TEST_PNG` / `accept_written_png` are **mock helpers**, not a window grab.
Do not read a Linux `cargo test` green as “Mac PNG works.”

### Mac evidence required before MERGE-READY

On a Mac laptop, `docs/TRY_ON_MAC.md` §8:

1. Build desktop `todo` + CLI. `GPUI_AGENT=1`, loopback, matching token.
2. Grant **Screen Recording** to the terminal / `todo`.
3. `gpui-agent screenshot --out artifacts/steps/mid.png` returns
   `{"ok": true, "result": {"path": "…", "backend": "screencapture"}}`.
4. `mid.png` is a PNG of **the Agent Todo window only** (not the full desktop).
5. Headless still returns `screenshot_unavailable` and writes **no** file.
6. Permission denial still maps to `screenshot_unavailable` (no invented pixels).

Until those six are pasted from a real Mac, verdict stays **UNPROVEN ON CI**.

### Fixes

Claim tests only (no product-code bug found on Linux).

---

## #21 — P4 CI recipe receipt (`9ce2eaf`)

Branch `gol/no-brainer-p4-ci-b20f`. Original `8b621ec` plus verify commits
(require both receipt fields; claim tests; rustfmt; temp-dir race fix;
reverted accidental `Cargo.lock`).

### Rebase

Merge-base `0c5f381`. Ahead 5 / behind 0. No conflicts.

### Bug found and fixed on the PR branch

`scripts/ci-recipe.sh` originally treated **missing** `session_reused` as
skip (`if "session_reused" in receipt and …`). A receipt with only
`"ok": true` would have passed. Receipt structs always serialize the
field today, but the **stated gate** is both fields.

Now `scripts/ci_recipe_assert.py` fails unless `ok is True` **and**
`session_reused is True`.

### `cargo test` summary (isolated `/tmp/target-pr21`)

```
gpui-agent lib:            49 passed; 0 failed
gpui-agent-cli bin:        23 passed; 0 failed
ci_recipe_assert:           7 passed; 0 failed
recipe_mcp_token:           8 passed; 0 failed
gpui-agent-recipe lib:     57 passed; 0 failed
todo_recipe:               14 passed; 0 failed
todo-core lib:              5 passed; 0 failed
tcp_crud:                   1 passed; 0 failed
```

**164 passed / 0 failed.**

Claim tests:

```
ci_workflow_token_is_only_on_the_recipe_step ... ok
ci_recipe_sh_fails_without_token_before_build ... ok
receipt_ok_and_session_reused_passes ... ok
receipt_ok_false_fails ... ok
receipt_session_reused_false_fails ... ok
receipt_without_ok_fails ... ok
receipt_without_session_reused_fails ... ok
recipe_run_without_token_fails_fast ... ok
```

Workflow YAML: `ubuntu-latest` only; no `macos-` runner; `GPUI_AGENT_TOKEN`
is **not** on the `cargo test` step.

Token-unset suite (explicit re-run): `recipe_mcp_token` **8 passed / 0 failed**.

### Live script

```
GPUI_AGENT=1 GPUI_AGENT_TOKEN=ci-p4-token GPUI_AGENT_ADDR=127.0.0.1:17431 ./scripts/ci-recipe.sh
```

Ended with:

```
==> assert receipt
CI receipt assert ok
ci-recipe ok
```

Receipt body included `"ok": true` and `"session_reused": true`. Host
logged `auth: required`. No `--screenshot-dir`. Exit 0.

Empty token still fails before `cargo build` (`ci_recipe_sh_fails_without_token_before_build`).

### Fixes

- **Fixed:** missing `session_reused` no longer skipped.
- **Fixed:** parallel claim tests racing on a temp path named by `json.len()`.
- **Not a leak:** a generated `Cargo.lock` was committed by mistake and
  **removed** (`9ce2eaf`). P4 still has no committed lockfile / `cargo audit`.

---

## #22 — P5 stack hygiene (`a0e26d3`)

Branch `gol/no-brainer-p5-hygiene-b20f`. Docs only. Original `d78b3be` plus
inventory refresh `a0e26d3`.

### Rebase

Merge-base `0c5f381`. Ahead 2 / behind 0. No conflicts.

### GitHub facts (API, 2026-09-09)

| PR | state | merged | closed_at | notes |
| --- | --- | --- | --- | --- |
| #4 | closed | **false** | 2026-09-09T12:27:03Z | base `b50fb8a`, `mergeable_state=dirty` |
| #5 | closed | **false** | 2026-09-09T12:27:03Z | stacked on #4 |
| #7 | closed | false | 2026-09-09T10:50:38Z | superseded by #9 |
| #8 | closed | false | 2026-09-09T11:10:49Z | superseded by #19 |
| #20 | open draft | — | — | keep |
| #21 | open draft | — | — | keep |
| #23 | open draft | — | — | opened after this inventory |
| #24 | open draft | — | — | opened after this inventory |

Museum remotes still present: `gol/recipes-tmp-perf-e79b`,
`gol/recipes-measured-perf-762f`.

First `STACK_HYGIENE.md` still described `rpc_pipeline` / MCP `isError` as
“optional later.” Those are now **#23 / #24**. Inventory updated on this
branch to match GitHub.

### `cargo test` summary (isolated `/tmp/target-pr22`)

```
gpui-agent lib:            49 passed; 0 failed
gpui-agent-cli bin:        23 passed; 0 failed
recipe_mcp_token:           8 passed; 0 failed
gpui-agent-recipe lib:     57 passed; 0 failed
todo_recipe:               14 passed; 0 failed
todo-core lib:              5 passed; 0 failed
tcp_crud:                   1 passed; 0 failed
```

**157 passed / 0 failed.** Same crate test counts as `main` (no rust changes).

### Verdict

**MERGE-READY.** Not CLOSE: `main` does not contain `docs/STACK_HYGIENE.md`.
The *actions* (close #4/#5) are already done; merging this is documentation.

---

## #23 — Pipeline all-Read waves (`414c72c`)

Branch `gol/read-wave-pipeline-b20f`. Original `dbff00c` + `8c57feb` plus
claim tests `ef82d12` / rustfmt `414c72c`.

### Rebase

Merge-base `0c5f381`. Ahead 4 / behind 0. No conflicts.

### `cargo test` summary (isolated `/tmp/target-pr23`)

```
gpui-agent lib:            53 passed; 0 failed
gpui-agent-cli bin:        23 passed; 0 failed
recipe_mcp_token:           8 passed; 0 failed
gpui-agent-recipe lib:     58 passed; 0 failed
todo_recipe:               16 passed; 0 failed
todo-core lib:              5 passed; 0 failed
tcp_crud:                   1 passed; 0 failed
```

**164 passed / 0 failed.**

Claim tests:

```
server::tests::rpc_pipeline_eof_after_first_reply_does_not_retry ... ok
server::tests::rpc_pipeline_wrong_token_fails_fast ... ok
run::tests::pipeline_only_wide_read_waves_without_screenshots ... ok
independent_read_wave_is_pipelined_on_one_session ... ok
independent_read_wave_records_siblings_when_one_fails ... ok
independent_write_wave_stops_before_later_sibling ... ok
mixed_write_and_read_wave_stays_sequential_fail_fast ... ok
screenshot_dir_records_a_receipt_entry_per_step ... ok
```

Sibling-on-assert-fail: receipt length 4 (root + a/b/c); `later` wave does
not start. Write fail-fast: later click sibling does not run. Screenshot-dir
wide hello wave still sets `steps[].screenshot` (pipeline path would be
`None`).

EOF-after-first-reply finishes in **&lt; 800ms** (timeout is 2s) — no
retry-until-timeout after a line is on the wire.

### What was not re-proven

`docs/PERF.md` quotes `rpc_pipeline_32_hellos` **145 µs** vs sequential
session **536 µs**. This verify run **did not** execute `cargo bench`.
Those numbers remain the PR author’s earlier Criterion `--quick` result,
not this report.

### Fixes

Claim tests only. No product-code bug found.

---

## #24 — MCP `isError` + `$params` graph identity (`764a278`)

Branch `gol/mcp-iserror-params-b20f`. Original `86f5d80` plus claim tests
`764a278`.

### Rebase

Merge-base `0c5f381`. Ahead 2 / behind 0. No conflicts.

### `cargo test` summary (isolated `/tmp/target-pr24`)

```
gpui-agent lib:            49 passed; 0 failed
gpui-agent-cli bin:        25 passed; 0 failed
recipe_mcp_token:           8 passed; 0 failed
gpui-agent-recipe lib:     60 passed; 0 failed
todo_recipe:               14 passed; 0 failed
todo-core lib:              5 passed; 0 failed
tcp_crud:                   1 passed; 0 failed
```

**162 passed / 0 failed.**

Claim tests:

```
mcp::tests::recipe_run_failed_assert_is_error ... ok
mcp::tests::recipe_run_success_is_not_error ... ok
recipe::tests::params_named_id_do_not_rewrite_graph_identity ... ok
recipe::tests::placeholder_in_step_id_or_needs_fails_validate ... ok
recipe::tests::placeholder_in_needs_is_not_confused_with_a_real_step_id ... ok
```

Failed assert: `tools/call` result has `isError: true` and receipt text
with `"ok": false`. Success hello recipe: `isError` absent. Param named
`id` substitutes into invoke args only; step `id`/`needs` unchanged.
`$title` in `needs` fails validate even when a real `wait` step exists.

On `main`, `recipe_run_tool` still maps `RunError::Step` to `Ok(value)` —
this PR is the fail-closed fix, not a no-op.

### Fixes

Claim tests only. No product-code bug found.

---

## Environment

| Item | Value |
| --- | --- |
| OS | Linux x86_64 |
| rustc | 1.98.1 (48a229cea 2026-09-01) |
| python3 | 3.12.3 (used by P4 assert) |
| Display / macOS | none — cannot run `screencapture` |
| Criterion | not run |

## Claim-test commits pushed to the PR branches (not `main`)

| PR | extra commits from this verify |
| --- | --- |
| #20 | `8c2c4bd` honest non-macOS screenshot tests |
| #21 | `9aef073` require both receipt fields; `a71482b` rustfmt; `68f30db` unique temp dirs; `9ce2eaf` drop accidental lockfile |
| #22 | `a0e26d3` GitHub inventory (#23/#24 + closed-at facts) |
| #23 | `ef82d12` retry-after-write + screenshot-dir claim tests; `414c72c` rustfmt |
| #24 | `764a278` success `isError` absence + `$placeholder` in needs |
