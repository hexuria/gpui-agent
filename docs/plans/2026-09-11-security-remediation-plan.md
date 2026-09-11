# gpui-agent — Consolidated Security & Architecture Remediation Plan

Date: 2026-09-11
Target revision: `main` @ `8857139af12fb033b4dd04eabd8d19b5bfc5ffc6` (Merge PR #32)
Inputs consolidated:

- **CS** — Claude Security whole-repository scan, 14 panel-verified findings (`CLAUDE-SECURITY-20260911-101146/CLAUDE-SECURITY-RESULTS.md`, stamp `verified`). Referenced as CS-F1 … CS-F14.
- **G** — Grok forensic scan (`gpui-agent-forensic-scan.md`, read-only review of the same revision). Referenced as G-P0.1 … G-P2.6 and G-test-table.

This document is written to be handed verbatim to an implementation agent ("the workhorse") and then to an independent review agent ("the reviewer"). Both must follow Section 1 exactly.

---

## 1. Operating rules for the workhorse (non-negotiable)

### 1.1 Branch and commits

- Create branch `gol/security-remediation-2026-09-11` from `main`. Never commit to `main`. Never force-push. Never rewrite history after a task is marked done.
- One commit per task (T0, R1, R2 …). Commit subject: `<task-id>: <one line>`. Body: the finding ids it closes.
- Do not push and do not open a PR unless the operator's kickoff prompt says so.

### 1.2 Evidence discipline — every task ends with an EVIDENCE block

Append to `docs/plans/evidence/2026-09-11-remediation-evidence.md` (create it in T0). Each task's block must contain, in this order:

1. `Task:` id and title.
2. `Commit:` full sha of the task commit.
3. `Red:` the *exact* command that demonstrates the new test failing **before** the fix, with exit code and the failing assertion lines pasted verbatim (max 40 lines, use `…` for elision, never paraphrase). Use `git stash` / `git stash pop` or check out the previous commit to produce this; state which.
4. `Green:` the same command after the fix, exit code, and the `test result:` line pasted verbatim.
5. `Verify:` every command listed under the task's **Verify** heading, each with exit code and the relevant output lines pasted verbatim.
6. `Diff:` output of `git diff --stat <previous-task-commit>..<this-commit>`.
7. `Deviations:` anything done differently from this plan and why, or `none`.

A task without a complete EVIDENCE block is not done. A "Red" that passes is not a red: the test is not testing the fix, so rewrite it.

### 1.3 Forbidden moves (the reviewer greps for every one of these)

- Deleting, renaming away, or adding `#[ignore]` to any existing test. If existing behaviour is *intentionally* changed by a task (only R2 and R4 do this), the task says which test to flip and the flipped test must assert the new behaviour with the same or stronger specificity.
- Loosening any existing cap: `MAX_LINE_BYTES` (1 MiB), `MAX_CONNECTIONS` (32), `MAX_MAILBOX_DEPTH` (128), `MAX_RECIPE_STEPS` (256), the 30 s idle timeout, `authorize_bind` / `authorize_client` / `authorize_request` semantics, `tokens_match`.
- Adding `#[allow(...)]` without a one-line justification comment on the line above, or adding any `allow` to silence a test.
- Editing `.github/workflows/ci.yml` other than additively (new steps are fine; removing or weakening a step is not).
- Marking a task done when a verification command failed, was skipped, or was "equivalent" to something else. If a command cannot run in your environment, write `BLOCKED-ENV:` with the exact error and leave the task open (see 1.5).
- Writing evidence output from memory. Paste terminal output only.
- Adding `gpui-kit` to `crates/gpui-agent`, adding `tokio`, adding a CSS/XPath selector grammar, adding todo or BIR verbs to `Op` or CLI subcommands, moving any auth check to CLI-only, mapping any op onto a shell. (G "Out of scope" list, adopted.)

### 1.4 Baseline that must not regress

Recorded in T0. The CI package set is `-p gpui-agent -p todo-core -p gpui-agent-cli -p gpui-agent-recipe` (`.github/workflows/ci.yml` line 31). G measured **196 tests, 0 ignored, 0 failed** on that set at this revision; T0 records your own number. Every task's Verify re-runs the set and the passing count must be `>=` baseline plus the tests that task adds.

### 1.5 Environment limits (be honest about them)

`apps/todo` pulls `gpui-kit`, which needs `fontconfig` and a Vulkan ICD. On the Linux VM G used, `cargo check --workspace` failed in `yeslogic-fontconfig-sys`. Rules:

- Before T0, try once: `sudo apt-get install -y libfontconfig1-dev pkg-config` (or the distro equivalent). If it works, `cargo check -p todo --features embedded-host` is part of every Verify that touches `apps/todo`. If it does not, record `BLOCKED-ENV` in T0 and every task touching `apps/todo` additionally lists its `apps/todo` changes under a `## macOS handoff` section in the evidence file, for the operator to compile locally with `cargo check -p todo --features embedded-host` and `cargo check -p todo`.
- Anything that executes `screencapture` is macOS-only. Unit tests must be written so the path/argv logic is tested on every OS (they already are: `screencapture_window_argv` is pure). Never fake a PNG.

### 1.6 Order

Phase 0 → Phase 1 in the listed order → Phase 2 → Phase 3 → Phase 4 (review). Do not start Phase 2 (R4) if the operator's kickoff prompt says `D2=veto`; see Decisions. (This kickoff: D2=implement protocol v2.)

---

## 2. Decisions taken in this plan (operator overrides already applied in kickoff)

| Id | Decision | Why | Override |
| --- | --- | --- | --- |
| D1 | **Host refuses to bind without a token by default.** A new env `GPUI_AGENT_INSECURE_NO_TOKEN=1` restores the old untokened loopback for local demos and prints a loud banner. All smoke scripts set a token. | CS-F3/F7/F11 and G-H1 agree untokened loopback is a full local control plane. G's caution ("don't break `scripts/smoke.sh`") is honoured by updating the scripts, not by keeping the hole. Closed PR #8 (mint-at-bind) is *not* revived: no token is invented, the operator must supply one. | `D1=warn-only` keeps the old default and only adds the banner + docs. |
| D2 | **Protocol v2: per-connection nonce + HMAC-SHA256 challenge-response replaces the raw token on the wire.** | It is the only fix that closes CS-F4, CS-F8 and CS-F12 together (token never crosses the wire, port-squatting yields nothing replayable). Adds two pure-Rust deps (`hmac`, `sha2`) to `crates/gpui-agent`. | `D2=veto` skips R4; R4-fallback (backoff + docs) runs instead. |
| D3 | **Screenshot paths are confined to a host-chosen base directory** (`GPUI_AGENT_SCREENSHOT_DIR`, default `<temp_dir>/gpui-agent-screenshots/`), relative names only, `.png` only, no pre-delete, temp-then-rename. | CS-F1/F5/F6. Absolute client paths are the whole bug. | none recommended |
| D4 | **Recipe registry becomes protocol-only by default; app invoke schemas load from a JSON file** (`--schema` / `GPUI_AGENT_SCHEMA`). Todo schemas move to `examples/schemas/todo.json`. | G-P1.1 (biggest modularity gap). Stays fail-closed: unknown invoke still rejected. | `D4=skip` defers R10. |
| D5 | Selector work is limited to **Phase 1 helpers + fail-closed duplicate ids** (G-P1.2 Phase 1). The `--role/--name` client-side resolver is optional (R11b). | Ambiguity policy must be fail-closed before locators exist. | `R11b=do` makes it required. |

THIS RUN: D1=default-deny, D2=implement v2, D3=confine screenshots, D4=protocol-only+schemas, D5=Phase 1 only (skip R11b unless everything else finished).

---

## 3. Consolidated findings register (X1–X15)

| Id | Sources | Severity | Surface | Finding (one line) | Closed by |
| --- | --- | --- | --- | --- | --- |
| X1 | CS-F1, CS-F5 | High | `screenshot.rs` `write_png` / `create_dir_all` | Client `path` is an arbitrary filesystem destination (write + mkdir). | R3 |
| X2 | CS-F3, CS-F7, CS-F11, G-H1 | High | `security.rs` `from_env` / `authorize_bind` | Loopback host binds with no token; any local process is a confused deputy. | R2 |
| X3 | CS-F6 | High | `screenshot.rs` `run_screencapture` | Pre-delete of the client path is arbitrary file deletion even if capture fails. | R6 |
| X4 | CS-F2 | Medium | `screencapture_window_argv` | Client path is the last argv slot and may start with `-` (flag injection into `screencapture`). | R5 |
| X5 | CS-F4, CS-F8, CS-F12 | High | `Request.token` on the wire | Raw token on every NDJSON line: replayable, visible to port-squatters. | R4 |
| X6 | CS-F9, G-P0.2 | Medium | `apps/todo` mailbox drain + `handle_stream_mailbox` | `hello.auth` is always `"none"` on the mailbox UI drain (`handle_request(..., None)`). | R7 |
| X7 | G-P1.3 | Medium | `scripts/smoke-desktop.sh` | Builds default `todo` (daemon client) but expects it to listen. | R8 |
| X8 | G-P0.1 | Medium | README / CLI help / INTEGRATING | Docs oversell “drive any GPUI Kit app” without an embed contract. | R1 |
| X9 | CS-F10, G-P2.1 | Medium | `dispatch.rs` `Op::Wait` | `timeout_ms` is ignored; Wait is immediate Hello. | R9 |
| X10 | G-P1.1 | High | `todo_registry()` always used by CLI/MCP | Recipes cannot validate a second app; protocol-only default is the modularity fix. | R10 |
| X11 | CS-F13, G-P1.2 | High | `UiTree::find` first-match | Duplicate ids are silent; 0/1/N is not fail-closed. | R11 |
| X12 | G-P2.2 | Low | PROTOCOL/README/SECURITY/todo-headless | Doc drift (remote claim, `page-settings` role, Wait, unsafe, `SecurityPolicy`). | R12 |
| X13 | G-P2.6 | Medium | no `Cargo.lock`; no `cargo audit` in CI | No committed/CI lockfile, no advisory gate. | R13 |
| X14 | G clippy, SECURITY I1 | Low | `authorize_request` `result_large_err`; I1 “no unsafe” | Clippy warning; SECURITY I1 claims no `unsafe` while `apps/todo` has objc `unsafe`. | R14 |
| X15 | G-P1.4, P1.5 | Medium | INTEGRATING.md | No copy-paste GPUI adapter checklist; mailbox+token test named but missing. | R15 |

CS-F14 is folded into X10 (registry). Remaining G items (P2.3 MCP framing, P2.4 password redaction, P2.5 notify/poll) stay documented, not implemented.

---

## 4. T0 — Baseline

**Title:** Record CI-package test baseline and environment limits.

**Closes:** none (instrumentation).

**Do:**

1. Branch already `gol/security-remediation-2026-09-11` from `8857139`.
2. Write this plan to `docs/plans/2026-09-11-security-remediation-plan.md` if missing.
3. Create `docs/plans/evidence/2026-09-11-remediation-evidence.md` with a header and the T0 block.
4. Once: `sudo apt-get install -y libfontconfig1-dev pkg-config`. Then `cargo check -p todo --features embedded-host` and `cargo check -p todo`. If either fails, `BLOCKED-ENV` and macOS handoff for later `apps/todo` tasks.
5. Run and record:

```
cargo test -p gpui-agent -p todo-core -p gpui-agent-cli -p gpui-agent-recipe
```

Count `test result: ok. N passed; 0 failed; 0 ignored`. That N is `BASELINE_PASSED`.

**Red:** n/a (baseline; no new test). State that explicitly.

**Green:** n/a.

**Verify:**

- The cargo test command above (exit 0, N recorded).
- `cargo check -p todo --features embedded-host` and `cargo check -p todo` (or BLOCKED-ENV).
- Caps still at 1 MiB / 32 / 128 / 256 (existing tests).

**Commit:** `T0: record security-remediation baseline`

---

## 5. Phase 1 (order: R2 → R3 → R6 → R5 → R7 → R8 → R1)

### R2 — Default-deny host token (D1)

**Title:** Host refuses to bind without a token unless `GPUI_AGENT_INSECURE_NO_TOKEN=1`.

**Closes:** X2 (CS-F3, CS-F7, CS-F11, G-H1).

**Do:**

- Add `SecurityError` variant for loopback-without-token (name it clearly, e.g. `LoopbackRequiresToken`).
- `authorize_bind`: on loopback, require a non-empty token. This **strengthens** bind policy. `authorize_client` **stays** allowing loopback without a token (client may still talk to an insecure host).
- `from_env`: if token missing/empty:
  - if `GPUI_AGENT_INSECURE_NO_TOKEN` is truthy **and** addr is loopback: bind with `token: None` and print a loud banner to stderr (`GPUI_AGENT_INSECURE_NO_TOKEN=1` plus “any local process can snapshot/click/invoke/shutdown”).
  - else return the new error.
- Do **not** mint a token (PR #8 stays closed).
- **Flip** `security::tests::loopback_bind_does_not_need_token_or_remote_flag` to assert `authorize_bind(loopback, None, false)` is `Err(LoopbackRequiresToken)` (or equivalent) **and** `authorize_bind(loopback, Some("t"), false)` is `Ok`. Keep `authorize_client(loopback, None, false)` succeeding. Rename the test to `loopback_bind_requires_token_client_does_not`.
- Add `insecure_no_token_env_allows_untokened_loopback_from_env` **or** a pure helper `allow_insecure_loopback(token, insecure_flag) -> bool` tested without mutating process env if env tests are racy. Prefer a pure function `bind_token_policy(token, insecure, loopback) -> Result` unit-tested, called from `from_env`.
- Smoke scripts (`scripts/smoke.sh`, `scripts/smoke-desktop.sh`, `scripts/smoke-daemon.sh`) and `todo-headless` banner: set/require `GPUI_AGENT_TOKEN`. `scripts/ci-recipe.sh` already requires a token.
- Docs: SECURITY.md H1 “partially mitigated” → default-deny; INTEGRATING, README, PROTOCOL, NO_BRAINER_PLAN, TRY_ON_MAC, RECIPES: untokened loopback is opt-in via the insecure env, not the default.
- `AgentServer::bind` / `spawn_host(..., None)` remain valid for in-process tests (they do not go through `from_env`). Do not break `tcp_crud`, `TestHost`, or server tests that pass `None`.

**Red:** `cargo test -p gpui-agent --lib loopback_bind_requires_token_client_does_not -- --exact` (or the flipped test name) **before** the production change, with only the new assertions present. Must fail on `authorize_bind(loopback, None, false)` still being `Ok`.

**Green:** same command, `test result: ok`.

**Verify:**

```
cargo test -p gpui-agent -p todo-core -p gpui-agent-cli -p gpui-agent-recipe
```

Passing count `>= BASELINE_PASSED` (net: renamed test + ≥1 new test; flipped test still counts as 1).

**Commit:** `R2: default-deny host token unless GPUI_AGENT_INSECURE_NO_TOKEN`

---

### R3 — Confine screenshot paths (D3 part 1)

**Title:** Relative `.png` names only, under `GPUI_AGENT_SCREENSHOT_DIR`.

**Closes:** X1 (CS-F1, CS-F5).

**Do:**

- Add `screenshot_base_dir() -> PathBuf`: `GPUI_AGENT_SCREENSHOT_DIR` if non-empty, else `{temp_dir}/gpui-agent-screenshots/`.
- Add `confine_screenshot_path(client: &str) -> Result<PathBuf, String>`:
  - Trim; empty → existing `"screenshot requires path"`.
  - Reject absolute paths (`Path::is_absolute` or Windows prefixes).
  - Reject any `..` component, empty components, prefixes `~`.
  - Reject names whose final component does not end with `.png` (lowercase exact).
  - Reject a final component that starts with `-` or `.` (other than `.png` as extension).
  - Join with base; after `clean`/`normalize` of components, the result must still start with the base.
  - `create_dir_all` **only** for the host base (and confined parents under the base), never for an unconfined client parent.
- `require_screenshot_path` may stay as the empty check; production `write_png` and `capture_window_via_screencapture` **must** call `confine_screenshot_path` and write/exec using the confined `PathBuf`.
- Response `result.path` is the confined host path (string).
- Recipe `capture_screenshot`: send **only** the file name (`001-step.png`), not an absolute `--screenshot-dir` join. Receipt may keep the operator's intended join for display **or** use `result.path` from the host. Do not send `/tmp/...`.
- Adapt existing screenshot tests that currently pass `/tmp/todo.png` or `temp_dir()` absolute paths: they must now use a relative name and, where they assert a file appears, set `GPUI_AGENT_SCREENSHOT_DIR` to an isolated temp dir (or pass the confined output). This is **not** a security-policy flip of R2/R4; it is required so tests assert the new confinement. Do not delete those tests; keep the same specificity (argv still has `-l{id}`, no `-i`/`-S`/`-w`).
- New tests (names required):
  - `screenshot_rejects_absolute_path`
  - `screenshot_rejects_dotdot`
  - `screenshot_rejects_non_png_extension`
  - `screenshot_relative_png_writes_under_base`
- Never fake a PNG. `TEST_PNG` remains tests/mock only.

**Red:** `cargo test -p gpui-agent --lib screenshot_rejects_absolute_path -- --exact` before confinement is enforced (test added, production still accepts `/tmp/x.png`). Must fail.

**Green:** same command after confinement.

**Verify:** CI package set. Passing `>=` previous + new tests. Existing `capture_without_macos_does_not_invent_a_file` still passes (use a relative dest; still must not invent a file).

**Commit:** `R3: confine screenshot paths to host base dir`

---

### R6 — No pre-delete of screenshot destinations

**Title:** Do not `remove_file` the destination before `screencapture`.

**Closes:** X3 (CS-F6).

**Do:**

- Delete the `if dest_path.exists() { remove_file }` block in `run_screencapture`.
- `accept_written_png` may still delete a **non-PNG** it just wrote (fail closed on garbage). Do not delete an existing destination before the command runs.
- Extract a testable helper used by macOS and unit-tested on all OS, e.g. `prepare_screencapture_dest(path) -> Result<(), String>` that must **not** unlink `path` if it already exists. Test: write a sentinel file, call the helper, file still exists with the same bytes.
- Test name: `screencapture_prepare_does_not_predelete_existing_file`.

**Red:** that test against current `run_screencapture` / current helper that still deletes.

**Green:** same command.

**Verify:** CI package set.

**Commit:** `R6: do not pre-delete screenshot destinations`

---

### R5 — Temp-then-rename + argv path is not a flag

**Title:** Write PNG via adjacent temp file then rename; confined path cannot start with `-`.

**Closes:** X4 (CS-F2) and D3 “temp-then-rename”.

**Do:**

- `write_png` / successful capture: write bytes to `{parent}/.{stem}.{pid}-{n}.tmp` in the **same directory** as the confined dest, `fsync` best-effort, then `rename` onto dest. Unix rename replaces atomically; that is not a pre-delete.
- If rename fails, remove the temp, leave dest untouched.
- `screencapture_window_argv`: last slot is the confined path as a single argv. After R3, relative names under base are absolute-at-exec time (the confined `PathBuf` is absolute under temp). Ensure the string passed to argv does **not** start with `-`. Test `screencapture_argv_rejects_leading_dash_filename` (already rejected by confine).
- Test `write_png_temp_then_rename_replaces_without_predelete`: dest exists with old bytes; write_png new TEST_PNG; dest is TEST_PNG; no leftover `.tmp`.
- Keep `screencapture_argv_is_this_window_only` assertions.

**Red:** `write_png_temp_then_rename_replaces_without_predelete` before rename helper exists (e.g. if implementation still `fs::write` directly, probe a hook; or test a new `atomic_write_png` that is not yet used). Prefer testing `atomic_write_png` as a public(crate) fn: red = function missing / old write_png overwrites via truncate without temp.

**Green:** same command.

**Verify:** CI package set. `cargo check -p todo --features embedded-host` if R5 touches `apps/todo` (it should not; confinement stays in `crates/gpui-agent`). If `apps/todo` `screenshot_this_window` still calls `require_screenshot_path` then `capture_window_via_screencapture`, that path must use confine — update `apps/todo` to pass through the SDK helpers only.

**Commit:** `R5: atomic screenshot write and dash-safe argv`

---

### R7 — Mailbox `hello.auth` matches the server token

**Title:** Mailbox TCP path stamps `hello.auth` from the server token.

**Closes:** X6 (CS-F9, G-P0.2).

**Do:**

- After `mailbox.wait` in `handle_stream_mailbox`, if `resp.hello` is `Some`, set `hello.auth = HelloAuth::from_token_configured(token)`. Keep `authorize_request` on the TCP thread before `mailbox.wait`. Do **not** move auth to CLI.
- Optional: `apps/todo` drain may still call `handle_request(..., None)` for dispatch; the TCP-thread stamp is the source of truth for clients.
- Test `mailbox_hello_auth_matches_server_token` in `server.rs` (no `Window`): `spawn_mailbox` with `Some("secret")`, drain thread that replies via `handle_request(..., None)`, client `with_token("secret")` Hello → `auth == Required`.
- Do not weaken mailbox depth cap.

**Red:** that test before the stamp (auth is `None`).

**Green:** same command.

**Verify:** CI package set. If `apps/todo` is touched, `cargo check -p todo --features embedded-host`.

**Commit:** `R7: mailbox hello.auth matches server token`

---

### R8 — `smoke-desktop.sh` matches ADR-001

**Title:** Desktop smoke starts the daemon SoT, not an unlistening GUI.

**Closes:** X7 (G-P1.3).

**Do:**

- `scripts/smoke-desktop.sh` must:
  1. `export GPUI_AGENT_TOKEN` (same as R2).
  2. Build `gpui-agent-cli`, `todo-headless`, and `todo` (default features — GUI is the daemon **client**).
  3. Start `todo-headless` first; wait via CLI `wait`.
  4. Start `todo` (GUI) only if a display is available; CLI CRUD talks to **headless** (SoT), not to the GUI port.
  5. If GUI cannot start (no display/Vulkan), still pass the headless CRUD portion and print that the window was skipped — **or** fail clearly. Prefer: require display for the GUI process but drive protocol against headless (ADR-001). Document in the script header.
- Do **not** silently `--features embedded-host` as the only path (that contradicts ADR-001). Embedded-host remains the Mac screenshot path in TRY_ON_MAC.
- Comment in the script citing ADR-001.
- Optional unit test is not required if it needs GPU. A comment + script change is the fix. If you add `embedded_host_listens_default_gui_does_not`, it must live where it can compile without GPU (e.g. assert default `todo` Cargo.toml `embedded-host` is off).

**Red:** n/a for a script-only change unless you add `default_todo_feature_embedded_host_is_off` that already passes — then add a **new** failing assertion first, e.g. script contains `todo-headless`. Grep test: `smoke_desktop_script_starts_todo_headless` as a CLI crate test reading the script file. Red: assertion `script.contains("todo-headless")` before the script edit.

**Green:** same test.

**Verify:** CI package set. Do not run the full desktop smoke on a GPU-less VM unless lavapipe is present; if `todo` cannot start, that is not BLOCKED-ENV for R8 if the script+file test passed. Record whether `./scripts/smoke-desktop.sh` was executed.

**Commit:** `R8: smoke-desktop starts daemon SoT per ADR-001`

---

### R1 — Docs: embed-only, not CDP attach

**Title:** Stop claiming the CLI attaches to an arbitrary GPUI Kit process.

**Closes:** X8 (G-P0.1).

**Do:**

- CLI crate docs / `--help` (`Drive any GPUI Kit app`) → the host must implement `AgentHost`, stable ids, and start the server under `GPUI_AGENT=1`.
- README / INTEGRATING / PROTOCOL: same. No CDP-like attach.
- No code behaviour change.

**Red:** a test `cli_help_does_not_claim_cdp_attach` (or similar) that runs `gpui-agent --help` and asserts help does **not** contain `Drive any GPUI Kit app` and **does** mention `AgentHost` or `embed`. Add the test before editing the help string.

**Green:** same command after help text change.

**Verify:** CI package set.

**Commit:** `R1: docs and CLI help are embed-only`

---

## 6. Phase 2 — R4 protocol v2 (D2=implement, not fallback)

### R4 — Per-connection nonce + HMAC-SHA256

**Title:** Protocol v2 challenge-response; raw token never on the wire.

**Closes:** X5 (CS-F4, CS-F8, CS-F12).

**Do:**

- `PROTOCOL_VERSION = 2`.
- Add deps **only** `hmac` and `sha2` to `crates/gpui-agent` (no `tokio`). Hex encode/decode by hand (no extra crate).
- **Challenge:** when the host has a non-empty token, immediately after accept (after `prepare_stream`), the server writes one NDJSON line and flushes:

```json
{"v":2,"op":"challenge","nonce":"<64 hex chars>"}
```

Nonce: 32 bytes from `/dev/urandom` on Unix; on other OS, fail bind/auth closed if CSPRNG is unavailable (or use a documented `getrandom` via `std` if available). Do not use time-only seeds.

- **Response:** client must **not** send `token`. Instead each request (or the session after handshake) sends `auth` = hex(`HMAC-SHA256(key=token_bytes, msg=nonce_bytes)`). Same MAC for the life of the connection is acceptable (per-connection nonce). Replay on a **new** connection fails because nonce changes.
- If a v2 request includes a non-empty `token` field while the host has a token: reject (`"token must not be sent on the wire"`) and close.
- `authorize_request` stays the single gate. Extend it to take the session nonce:

```rust
pub fn authorize_request(
    req: &Request,
    expected_token: Option<&str>,
    session_nonce: Option<&[u8]>,
) -> Result<(), Response>
```

Semantics **strengthened**, not loosened: version must be 2; when `expected_token` is `Some`, require matching HMAC over `session_nonce` (nonce `Some` and 32 bytes); when `expected_token` is `None`, do not require `auth` (insecure / in-process tests).

- `AgentClient`: if `self.token` is `Some`, on `LiveSession::open` read the challenge, verify `v == PROTOCOL_VERSION` and `op=challenge`, keep nonce, send `auth` HMAC, never serialize raw token. If token is `None`, do not expect a challenge (untokened test servers).
- Untokened servers (tests, insecure bind) do **not** send a challenge.
- **Flip** tests that encode the raw secret on the wire or assume v1:
  - `client::tests::wire_request_matches_owned_request_json` — v2 `auth` not `token` when authenticating; untokened wire has neither.
  - `dispatch` `accepts_matching_token` / `rejects_missing_token` — use HMAC + nonce helper; missing auth fails.
  - `protocol::tests` JSON that hard-codes `"v":1` as a **live** protocol version should expect 2 for `PROTOCOL_VERSION`. Serde round-trips of old documents may still parse `v:1` then fail authorize.
  - `server::tests::rpc_pipeline_eof_after_first_reply_does_not_retry` hard-codes `{"v":1,...}` as a **peer reply**; bump the mock reply `v` to `PROTOCOL_VERSION` so the client parser still exercises EOF. This is an R4 flip of a fixture, not a cap loosen.
  - `hello_auth_roundtrip` `protocol:1` in a JSON literal without `protocol` field default is HelloInfo, not Request.v — HelloInfo.protocol should follow `PROTOCOL_VERSION` in constructors.
- New tests (required names):
  - `v2_challenge_hmac_accepts_matching_token`
  - `v2_raw_token_on_wire_is_rejected`
  - `v2_hmac_from_wrong_nonce_is_rejected` (port-squat / replay)
  - `v2_untokened_server_does_not_send_challenge`
- `tokens_match` remains for any leftover byte compare; HMAC uses the `hmac` crate. Do not remove `tokens_match` tests.
- Docs: PROTOCOL.md becomes v2; show challenge then requests with `auth`. SECURITY.md: token not on the wire; remote still plaintext metadata.
- `todo-headless` banner `protocol v1` → v2.
- Caps unchanged. No `gpui-kit` on the SDK crate.

**Red:** `cargo test -p gpui-agent --lib v2_raw_token_on_wire_is_rejected -- --exact` with the test written against current v1 (server still accepts `token` field). Must fail.

**Green:** same after v2.

**Verify:** CI package set. Passing `>=` previous + new tests − 0. Flipped tests still present (same or stronger assertions).

**Commit:** `R4: protocol v2 HMAC-SHA256 challenge-response`

---

## 7. Phase 3 (order: R9 → R10 → R11 → R12 → R13 → R14 → R15)

### R9 — `Wait.timeout_ms` actually waits

**Title:** `Op::Wait` polls `hello.ready` until ready or timeout.

**Closes:** X9 (CS-F10, G-P2.1).

**Do:**

- In `handle_request`, `Op::Wait { timeout_ms: None }` stays immediate hello (current).
- `Op::Wait { timeout_ms: Some(ms) }`: poll `host.hello().ready` until true or `ms` elapsed (sleep ≤ 10 ms between polls). If still not ready: `Response::err` containing `wait timed out` (and do not claim ready).
- Test `wait_times_out_when_not_ready`: host with `ready: false`, `timeout_ms: Some(50)` → `ok == false`, error contains `timed out`.
- Test `wait_none_is_immediate_hello`: ready or not, `None` returns ok hello without sleeping a long time (if not ready, still ok with `ready: false` — documents current client `wait_ready` retry).
- Test `wait_succeeds_when_host_becomes_ready` using `Arc<AtomicBool>` flipped by another thread.

**Red:** `wait_times_out_when_not_ready` against current Wait≈Hello (would return ok).

**Green:** same command.

**Verify:** CI package set.

**Commit:** `R9: Wait.timeout_ms polls hello.ready`

---

### R10 — Protocol-only registry + schema files (D4)

**Title:** Default recipe registry is protocol ops only; load app schemas from JSON.

**Closes:** X10 (G-P1.1, CS-F14).

**Do:**

- `protocol_registry() -> Registry` with only `SchemaKind::Protocol` ops (the current protocol_* list).
- `Registry::merge(&mut self, other: Registry) -> Result<(), String>` (fail closed on invalid insert).
- `todo_registry()` becomes `protocol_registry()` + merge of todo invoke/id schemas (keep the function; **do not** delete tests that call `todo_registry()`).
- `load_schema_file(path) -> Result<Registry, String>`: JSON array or `{ "schemas": [ OpSchema, ... ] }`. Validate every row.
- CLI/MCP: default registry = `protocol_registry()`. `--schema PATH` (repeatable) and `GPUI_AGENT_SCHEMA` (os path-list, `:` on Unix) merge invoke/id schemas. `recipe validate|plan|run|resolve` all use this.
- Move todo invoke/id schemas to `examples/schemas/todo.json`. CI recipe + smoke recipe phases pass `--schema examples/schemas/todo.json` (or env).
- Tests (required names):
  - `recipe_rejects_unknown_invoke_with_protocol_only_registry` (`todo.add` / `nav.go` / `prefs.set` fail closed)
  - `recipe_accepts_invoke_from_schema_file`
  - `empty_registry_rejects_all_invoke` if distinct from protocol_registry (protocol registry still has the `invoke` **protocol** op schema; unknown **names** fail at validate). Clarify: validate of a step `op: invoke, name: todo.add` fails without that invoke schema.
- Do not add todo verbs to `Op` or CLI subcommands.
- `todo_registry_has_verified_invoke_names` stays.

**Red:** `recipe_rejects_unknown_invoke_with_protocol_only_registry` before CLI default switch is not required to fail the library helper if `protocol_registry()` does not exist yet — add `protocol_registry()` returning todo-full by mistake would make red fail to be red. Red: test uses `protocol_registry()` and expects `todo.add` missing; implement `protocol_registry` as clone of `todo_registry` first so the test fails, then strip invokes.

**Green:** same command.

**Verify:** CI package set. `scripts/ci-recipe.sh` must pass `--schema` (update script). Existing recipe unit tests that pass `&todo_registry()` remain green.

**Commit:** `R10: protocol-only recipe registry loads app schemas`

---

### R11 — Phase 1 selectors: unique ids, fail-closed N (D5)

**Title:** `find_all` / uniqueness; assert/click-id resolution fail-closed on duplicates.

**Closes:** X11 (CS-F13, G-P1.2 Phase 1). **Skip R11b** (`--role/--name` CLI resolver) unless all other tasks including R15 are done.

**Do:**

- `UiNode::find_all` / `UiTree::find_all(id) -> Vec<&UiNode>` (DFS order).
- `UiTree::duplicate_ids() -> Vec<String>` (ids with count > 1).
- `UiTree::ids_are_unique() -> bool`.
- `UiTree::require_id(id) -> Result<&UiNode, String>`: 0 → `node `{id}` not found`; N>1 → error containing `duplicate id` and the count or candidate hint. 1 → that node.
- `assert_tree` uses `require_id` instead of `find`. Existing unique-id tests still pass. New test `assert_duplicate_id_is_error`.
- Keep `find` as first-match **deprecated for dispatch**; do not use it in `assert_tree`. Semantic host click in `todo-core` may still string-match ids (app domain). SDK tree lookup for assert is fail-closed.
- Tests:
  - `find_all_returns_every_duplicate_in_dfs_order`
  - `require_id_zero_is_not_found`
  - `require_id_many_is_duplicate_error`
  - `assert_duplicate_id_is_error`
- No CSS/XPath. No protocol `match` object. No R11b unless leftover time after R15.

**Red:** `assert_duplicate_id_is_error` against current `find` first-match (assert succeeds).

**Green:** same command.

**Verify:** CI package set.

**Commit:** `R11: fail-closed duplicate ids in tree lookup`

---

### R12 — Doc drift pack

**Title:** Align PROTOCOL/README/SECURITY/todo-headless comments with code.

**Closes:** X12 (G-P2.2).

**Do:**

- PROTOCOL.md: remote bind is allowed with `GPUI_AGENT_REMOTE=1` + token (not “refuse all non-loopback”). Protocol v2 after R4.
- README: `page-settings` role is `page`, not `window` (todo-core constructors). Example `assert --id page-settings --role page`.
- Wait: document that `timeout_ms` polls `ready` (R9).
- SECURITY.md I1: do not claim the workspace has no `unsafe`; `apps/todo` `macos_window.rs` has objc `unsafe` for `windowNumber` only.
- `todo-headless` comment `SecurityPolicy::from_env` → `from_env`.
- No behaviour change except comments/docs.

**Red:** `page_settings_readme_role_is_page` as a doc test is heavy. Prefer a tiny test in `todo-core` already having page role, plus a `docs_protocol_mentions_remote_triple` that reads `docs/PROTOCOL.md` and asserts it contains `GPUI_AGENT_REMOTE`. Add test before the PROTOCOL edit so red is “file does not mention GPUI_AGENT_REMOTE” — **wait**, PROTOCOL might already mention it in later sections. Grep first. If already present, red on README `--role window` for page-settings: test reads README.md and asserts it does **not** contain ``--role window`` after the fix; red = currently contains it.

**Green:** same.

**Verify:** CI package set.

**Commit:** `R12: fix protocol and README doc drift`

---

### R13 — Lockfile + `cargo audit` (additive CI)

**Title:** CI generates a lockfile and runs `cargo audit`.

**Closes:** X13 (G-P2.6).

**Do:**

- **Do not** require committing `Cargo.lock` if the repo gitignores it; **do** add CI steps after checkout/toolchain:
  1. `cargo generate-lockfile`
  2. Install `cargo-audit` (or `cargo install cargo-audit --locked` with a pinned version).
  3. `cargo audit` (fail on vulnerabilities; unmaintained warnings in GPUI Kit stack must not fail the job unless `cargo audit` defaults to that — use `--deny warnings` **only if** it does not fail on the known unmaintained GPUI deps; prefer default deny-vulnerabilities).
- Additive only: do not remove the existing test or recipe steps.
- If `cargo audit` cannot be installed: `BLOCKED-ENV` with the error; still add the CI step so review can see intent, **or** leave the task open per 1.5. Prefer a working step.

**Red:** a test is awkward for CI YAML. Add `ci_workflow_runs_cargo_audit` in `gpui-agent-cli` tests that reads `.github/workflows/ci.yml` and asserts it contains `cargo audit`. Red before YAML edit.

**Green:** same test.

**Verify:** CI package set. Locally run `cargo generate-lockfile && cargo audit` if install works; paste output. Do not commit `Cargo.lock` unless you also stop gitignoring it — default is CI-generate only.

**Commit:** `R13: CI generate-lockfile and cargo audit`

---

### R14 — Clippy `result_large_err` + SECURITY I1 honesty

**Title:** Box `authorize_request` error payload; docs match `unsafe` reality.

**Closes:** X14.

**Do:**

- If clippy `result_large_err` fires on `authorize_request`: return `Result<(), Box<Response>>` **or** keep `Response` by value if that **is** the API — do not add `#[allow(clippy::result_large_err)]` without a one-line justification on the line above. Prefer `Box<Response>` only if it does not churn every call site painfully; alternatively leave the Result as-is and add the justified allow citing “Response is the on-wire error”. Reviewer accepts either with justification.
- SECURITY.md I1 already partially notes objc unsafe; make the “no unsafe in gpui-agent crate” vs “workspace unsafe only in macos_window” sentence exact after R12 if still wrong.
- `cargo clippy -p gpui-agent -p todo-core -p gpui-agent-cli -p gpui-agent-recipe -- -D warnings` should pass. If a **pre-existing** warning unrelated to this task fails `-D warnings`, fix only what this task owns or record BLOCKED-ENV — do not `allow` to silence tests.

**Red:** `cargo clippy -p gpui-agent -- -D warnings` before the fix (expect `result_large_err` or I1-unrelated). If clippy is already clean, add no allow; the red must be a new test `security_md_does_not_claim_workspace_has_no_unsafe` reading SECURITY.md.

**Green:** clippy or the doc test.

**Verify:** CI package set + clippy command.

**Commit:** `R14: clippy Result size and unsafe wording`

---

### R15 — INTEGRATING adapter checklist + mailbox token test named in G

**Title:** INTEGRATING copy-paste GPUI checklist; P1.5 mailbox test already added in R7.

**Closes:** X15 (G-P1.4, leftover P1.5).

**Do:**

- INTEGRATING.md: numbered checklist to copy mailbox drain, virtual dispatch, macOS screenshot intercept, `spawn_mailbox` vs `spawn_host`, default-deny token, confined screenshots, protocol v2 HMAC. Do **not** add a `gpui-agent-gpui` crate or `gpui-kit` on the SDK.
- Confirm R7 test `mailbox_hello_auth_matches_server_token` exists (P1.5). If R7 already added it, R15 only docs.
- Test `integrating_md_lists_mailbox_and_screenshot` reads the doc for `spawn_mailbox` and `GPUI_AGENT_SCREENSHOT_DIR`.

**Red:** that test before the INTEGRATING bullets.

**Green:** same.

**Verify:** CI package set. Skip R11b.

**Commit:** `R15: INTEGRATING adapter checklist`

---

## 8. HANDOFF then STOP

When R15 is committed and evidence is complete, write `docs/plans/evidence/HANDOFF.md` (this is **not** a substitute for evidence blocks). Then **stop**. Do not self-review. Do not open a PR.

`HANDOFF.md` must contain:

- Branch name
- `BASELINE_PASSED` and final CI-package passed count
- Commit list (`git log --oneline 8857139..HEAD`)
- Every `BLOCKED-ENV` (exact error) or `none`
- Every deviation from this plan
- Whether R11b was skipped (expected: yes)
- Pointer to `docs/plans/evidence/2026-09-11-remediation-evidence.md`
- Confirmation: no push required; no PR

### 8.1 Reviewer spawn prompt (for the operator)

> You are an independent reviewer of hexuria/gpui-agent branch `gol/security-remediation-2026-09-11`. Read `docs/plans/2026-09-11-security-remediation-plan.md` Section 1 and the evidence file. Do not trust the workhorse. Grep for every forbidden move in §1.3. Re-run the CI package tests. Check each EVIDENCE block has pasted red-before-green. Check flipped tests (R2, R4 only). Confirm caps unchanged. Confirm D1–D5. Write a verdict: ACCEPT / REJECT with findings.

---

## 9. Caps and invariants (reviewer cheat sheet)

| Cap / API | Value |
| --- | --- |
| `MAX_LINE_BYTES` | 1 MiB |
| `MAX_CONNECTIONS` | 32 |
| `MAX_MAILBOX_DEPTH` | 128 |
| `MAX_RECIPE_STEPS` | 256 |
| Idle timeout | 30 s |
| `tokens_match` | XOR-fold, `#[inline(never)]` |
| `authorize_client` loopback without token | still allowed |
| `authorize_bind` loopback without token | **denied** after R2 unless insecure env |
| Protocol version | **2** after R4 |
| Token on v2 wire | forbidden |
| Screenshot dest | relative `.png` under host base |
| Recipe default registry | protocol-only |
| CSS/XPath / tokio / gpui-kit on SDK | forbidden |

---

## 10. Out of scope (do not do)

From G, adopted: BIR/todo verbs on the wire or CLI; protocol `batch` op; CSS selectors; mint-at-bind; OS HID; full-desktop screenshot; invoke→shell; CLI-only auth; tokio; simd-json; AccessKit prerequisite; merging museum PRs #4/#5; `gpui-kit` on `crates/gpui-agent`; R11b unless everything else is done.
