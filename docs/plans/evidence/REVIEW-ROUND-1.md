# Independent review round 1 — `gol/security-remediation-2026-09-11`

Reviewer did not author the remediation. HEAD reviewed: `7f19e5375c6d47c65e01e1b8a4a3dcaaf3325ffc` (matches `origin/gol/security-remediation-2026-09-11`). Base: `8857139af12fb033b4dd04eabd8d19b5bfc5ffc6`. Plan: `docs/plans/2026-09-11-security-remediation-plan.md`. Evidence: `docs/plans/evidence/2026-09-11-remediation-evidence.md`. Draft PR: https://github.com/hexuria/gpui-agent/pull/33.

**Section 8.1 verdict: REJECT** (fixes at several sinks are real; evidence discipline and two residual holes are not).

Method: every named Verify/Red was re-run or reconstructed. `git grep 'fn <test>' <parent>` for every task: **ABSENT**. Checking out the parent commit and running the named test is therefore **exit 0 / 0 tests**, not the panic pasted in evidence. Evidence `Commit:` SHAs are **not in this clone** (`git cat-file -t` → missing) — they were amended away. Reconstructed reds below insert only the named test (or the plan’s specified stub) onto parent production.

Task commits used (git log, not evidence SHAs):

| Task | Commit | Parent |
| --- | --- | --- |
| T0 | `8e09eba` | `8857139` |
| R2 | `82a6d22` | `8e09eba` |
| R3 | `0931363` | `82a6d22` |
| R6 | `037eb4e` | `0931363` |
| R5 | `dc6faea` | `037eb4e` |
| R7 | `f565031` | `dc6faea` |
| R8 | `a91548e` | `f565031` |
| R1 | `243ab78` | `a91548e` |
| R4 | `4414843` | `243ab78` |
| R9 | `a607982` | `4414843` |
| R10 | `e228918` | `a607982` |
| R11 | `7bbea1c` | `e228918` |
| R12 | `12eed9a` | `7bbea1c` |
| R13 | `ada2a71` | `12eed9a` |
| R14 | `0464504` | `ada2a71` |
| R15 | `3ad6241` | `0464504` |

---

## Cross-cutting

### Forbidden-move grep (`git diff 8857139...HEAD`)

- Removed `#[test]`: none. One allowed R2 rename: `loopback_bind_does_not_need_token_or_remote_flag` → `loopback_bind_requires_token_client_does_not`.
- Added `#[ignore]`: none.
- Added `#[allow(`: none in the branch diff.
- Cap constants: **unchanged**. `MAX_LINE_BYTES = 1024 * 1024`, `MAX_CONNECTIONS = 32`, `MAX_MAILBOX_DEPTH = 128`, `MAX_RECIPE_STEPS = 256`, `IDLE_TIMEOUT = 30s`. `tokens_match` XOR-fold + `#[inline(never)]` is byte-identical to `main`.
- `.github/workflows/ci.yml`: **additive only** (generate-lockfile, install cargo-audit 0.22.2, `cargo audit`). Existing unit-test and recipe steps remain.
- `authorize_client` loopback-without-token still `Ok` (`crates/gpui-agent/src/security.rs:128-135`). `authorize_bind` loopback-without-token is `Err(LoopbackRequiresToken)` (`:115-120`).
- No `tokio`, no `gpui-kit` on `crates/gpui-agent`. Added `hmac = "0.12"`, `sha2 = "0.10"` only.
- R11b skipped (no CSS/XPath / `--role/--name` resolver).

### Test inventory

`cargo test -p gpui-agent -p todo-core -p gpui-agent-cli -p gpui-agent-recipe -- --list` → **223** tests. Required names from the plan are all present (see list paste under T0 / final Verify). Arithmetic: `BASELINE_PASSED 194 + 29 new = 223`.

### Evidence SHAs

Every evidence `Commit:` object is **missing** from git. HANDOFF admits amend. That is incomplete evidence, not a substitute for reachable commits.

### Flake on claimed-green suite

First `cargo clean -p gpui-agent -p todo-core -p gpui-agent-cli -p gpui-agent-recipe` then CI set:

```
test server::tests::rpc_pipeline_wrong_token_fails_fast ... FAILED
wrong token must not run the wave: Broken pipe (os error 32)
test result: FAILED. 86 passed; 1 failed; 0 ignored; ...
```

Re-run of that test: **1 fail (EPIPE) / 7 pass**. Second full CI set: **223 passed** (`87+28+8+8+64+16+11+1`). The test accepts `"token" | "connection closed" | "reset"` but not `Broken pipe` (`server.rs:577-581`). Workhorse Verify “223 / exit 0” is not always true.

---

## T0 — Record CI-package test baseline

**Verdict: PARTIAL**

**Why:** CI-package count **194** is independently reproduced at `8e09eba`. `apps/todo` compile is **not** reproducible here. Evidence SHA `f64a054…` does not exist.

Naive Red: n/a (baseline).

Verify (worktree `/tmp/review-t0` @ `8e09eba`):

```
test result: ok. 67 passed; 0 failed; 0 ignored; ...
test result: ok. 26 passed; ...
test result: ok. 7 passed; ...
test result: ok. 8 passed; ...
test result: ok. 61 passed; ...
test result: ok. 16 passed; ...
test result: ok. 8 passed; ...
test result: ok. 1 passed; ...
```

Sum **194**. Matches evidence binary split exactly.

Caps at HEAD:

```
test cap_tests::security_caps_unchanged ... ok
MAX_LINE_BYTES = 1024 * 1024
MAX_CONNECTIONS = 32
MAX_MAILBOX_DEPTH = 128
MAX_RECIPE_STEPS = 256
IDLE_TIMEOUT = 30s
```

`cargo check -p todo` / `--features embedded-host` in this VM:

```
error: failed to run custom build command for `yeslogic-fontconfig-sys v6.0.1`
Package 'fontconfig' was not found ...
TODO_EMBED_EXIT:101
TODO_OK_EXIT:101
```

`sudo apt-get install -y libfontconfig1-dev pkg-config` → `E: Unable to locate package libfontconfig1-dev`. Apt offers `libfontconfig1` runtime only. **BLOCKED-ENV** for `apps/todo` here. Workhorse T0 claimed install + check succeeded (`BLOCKED-ENV: none`). Different snapshot or unverifiable paste; cannot confirm.

Diff at T0 is docs-only (plan + evidence). No production change. Root-cause N/A.

---

## R2 — Default-deny host token

**Verdict: CONFIRMED**

**Why:** Production sink is `from_env` → `authorize_bind_with_insecure` (`security.rs:82-83`, `:109-120`), used by `todo-headless` `serve()` (`apps/todo-headless/src/main.rs:53-65`). Not a caller-only patch. `AgentServer::bind(..., None)` still allowed for in-process tests (plan).

Naive Red @ `8e09eba`: test **ABSENT**, `running 0 tests`, exit 0.

Reconstructed Red (named test’s `unwrap_err()` on parent `authorize_bind`; variant `LoopbackRequiresToken` does not exist yet so the evidence’s `matches!` form does not compile):

```
thread 'security::tests::loopback_bind_requires_token_client_does_not' panicked at crates/gpui-agent/src/security.rs:230:54:
called `Result::unwrap_err()` on an `Ok` value: 127.0.0.1:17421
test result: FAILED. 0 passed; 1 failed; ... 67 filtered out
```

Matches evidence panic text.

Green @ HEAD:

```
test security::tests::loopback_bind_requires_token_client_does_not ... ok
```

Live X2 (no token, no insecure flag):

```
refusing to start automation: loopback bind requires a non-empty GPUI_AGENT_TOKEN (set GPUI_AGENT_INSECURE_NO_TOKEN=1 only for local demos)
X2_EXIT:2
```

Insecure opt-in banner (timeout 1s):

```
*** GPUI_AGENT_INSECURE_NO_TOKEN=1 ***
This host accepts unauthenticated loopback control-plane requests.
Any local process can snapshot, click, invoke, and shutdown.
auth: none (GPUI_AGENT_INSECURE_NO_TOKEN=1 — any local process can drive this host)
```

Residual (not a failed sink): `docs/NO_BRAINER_PLAN.md:160` historical P2 table still says `Host from_env | Still optional` after R2 only updated the invariants bullet (`:39-42`).

---

## R3 — Confine screenshot paths

**Verdict: CONFIRMED**

**Why:** Root cause at the SDK write/exec sink, not one caller. `write_png_in` calls `confine_screenshot_path_in` then `atomic_write_png` (`screenshot.rs:117-120`). `capture_window_via_screencapture` confines before argv (`:221-227`). Recipe wire name is `file_name()` only (`crates/gpui-agent-recipe/src/run.rs:230-237`). `apps/todo` `screenshot_this_window` passes through `capture_window_via_screencapture` (`apps/todo/src/app.rs:811-815`).

Naive Red @ `82a6d22`: test ABSENT, would be 0 tests.

Reconstructed Red against the **real pre-fix sink** `write_png("/tmp/evil.png")` (stronger than evidence’s passthrough helper):

```
thread 'screenshot::tests::screenshot_rejects_absolute_path' panicked at crates/gpui-agent/src/screenshot.rs:316:56:
called `Result::unwrap_err()` on an `Ok` value: Object {"path": String("/tmp/evil.png")}
test result: FAILED. 0 passed; 1 failed; ... 68 filtered out
```

(Wrote `/tmp/evil.png` TEST_PNG; deleted after.) Evidence red was `confine_screenshot_path` returning `Ok("/tmp/evil.png")` — that helper did not exist on parent; they added a passthrough then tightened it. The production `fs::write` of client paths is what actually closed.

Green @ HEAD:

```
test screenshot::tests::screenshot_rejects_absolute_path ... ok
test screenshot::tests::screenshot_rejects_dotdot ... ok
test screenshot::tests::screenshot_rejects_non_png_extension ... ok
test screenshot::tests::screenshot_relative_png_writes_under_base ... ok
test screenshot::tests::capture_without_macos_does_not_invent_a_file ... ok
```

Live X1 against **tokened `todo-headless`** does **not** hit confine: `TodoStore::screenshot` ignores `path` (`todo-core/src/lib.rs:394-405`) and returns `screenshot_unavailable`. Transcript under X1–X5. No files created. That is a probe-host gap, not a missing SDK fix.

---

## R6 — No pre-delete of screenshot destinations

**Verdict: CONFIRMED**

**Why:** Parent `run_screencapture` (`0931363` `screenshot.rs:187-188`) still did `if dest_path.exists() { remove_file }`. HEAD `prepare_screencapture_dest` (`:252-260`) only `create_dir_all`s the parent. `run_screencapture` (`:264-266`) calls that helper and does not unlink before exec. Test is of the helper (plan’s specified shape), and the helper is on the macOS exec path.

Naive Red: test ABSENT.

Reconstructed Red (helper that still unlinks, as evidence described):

```
thread 'screenshot::tests::screencapture_prepare_does_not_predelete_existing_file' panicked at crates/gpui-agent/src/screenshot.rs:451:41:
called `Result::unwrap()` on an `Err` value: Os { code: 2, kind: NotFound, message: "No such file or directory" }
test result: FAILED. 0 passed; 1 failed; ... 72 filtered out
```

Matches evidence. Green @ HEAD: same test `ok`. Linux never executes `run_screencapture` (`cfg(macos)`).

---

## R5 — Temp-then-rename + dash-safe argv

**Verdict: PARTIAL**

**Why:** `atomic_write_png` (`screenshot.rs:139-174`) is the write sink and is used by `write_png_in`. Capture writes to `png_write_temp_path` then `rename` (`:223-238`). `screencapture_window_argv` rejects a last slot starting with `-` (`:194-197`); confine also rejects `-` filenames (`:97-98`). That is the X4 sink for the SDK path.

Red is a **stub**, not old `fs::write`. Evidence panic `"atomic_write_png not implemented"` reconstructed:

```
called `Result::unwrap()` on an `Err` value: "atomic_write_png not implemented"
test result: FAILED. 0 passed; 1 failed; ... 73 filtered out
```

Plan allowed “function missing”. It does **not** prove the old truncate-in-place write. Green test does prove replace-without-leftover-tmp once implemented.

```
test screenshot::tests::write_png_temp_then_rename_replaces_without_predelete ... ok
test screenshot::tests::screencapture_argv_rejects_leading_dash_filename ... ok
```

Live `-Sc.png` on headless: `screenshot_unavailable` (path ignored), so X4 is not live-proven on `todo-headless`.

---

## R7 — Mailbox `hello.auth` matches server token

**Verdict: CONFIRMED**

**Why:** Stamp is on the TCP mailbox path after `mailbox.wait` (`server.rs:338-340`), not a CLI check. `authorize_request` still runs before `wait` (`:328-332`). Drain may still `handle_request(..., None, None)` (`:710`).

Naive Red: test ABSENT.

Reconstructed Red (R7 test copied onto `dc6faea`, no stamp):

```
assertion `left == right` failed
  left: None
  right: Required
test server::tests::mailbox_hello_auth_matches_server_token ... FAILED
... 75 filtered out
```

Matches evidence. Green @ HEAD: test listed and passed in the 223 run.

---

## R8 — `smoke-desktop.sh` matches ADR-001

**Verdict: CONFIRMED**

**Why:** Script rewrite is real (`scripts/smoke-desktop.sh`): builds/starts `todo-headless` first, CRUD against headless, GUI optional, cites ADR-001, does not `--features embedded-host`. Not a one-line string insert.

Naive Red: test ABSENT.

Reconstructed Red (assert `script.contains("todo-headless")` on parent script that drove the GUI):

```
test tests::smoke_desktop_script_starts_todo_headless ... FAILED
```

Parent script ended `desktop smoke ok: GPUI Kit window driven without CDP` and had no `todo-headless`. Plan allowed a grep test; the production script change matches the Do list. Did not re-run full `./scripts/smoke-desktop.sh` (GPU-less; plan says that is not BLOCKED-ENV if the file test passed).

---

## R1 — Docs: embed-only, not CDP attach

**Verdict: PARTIAL**

**Why:** About-text at `crates/gpui-agent-cli/src/main.rs:15-19` no longer says `Drive any GPUI Kit app`. Test uses `Cli::command().render_long_help()` (`:341-350`), **not** spawning `gpui-agent --help` (plan Red). That is the stated deviation.

Naive Red: test ABSENT.

Reconstructed Red on `a91548e` (old about string still present): test FAILED (help dump included the old claim).

**Cosmetic hole / canary (plan step 5):** clap prints the live env value. No `hide_env_values`.

```
GPUI_AGENT_TOKEN=review-canary-9f3a-TOKEN ./target/debug/gpui-agent --help
[env: GPUI_AGENT_TOKEN=review-canary-9f3a-TOKEN]
```

`cli_help_does_not_claim_cdp_attach` still **passes** with that env set. Help is embed-only, but the canary leaks. That is not a CDP-claim miss; it is an untested token leak on `--help`.

---

## R4 — Protocol v2 HMAC-SHA256

**Verdict: FAKED-RED**

**Why (production is not cosmetic):** Sink is `authorize_request` (`dispatch.rs:39-43`) plus `begin_session` challenge (`server.rs:188-200`) and `WireRequest` that serializes `auth` only (`client.rs:147-155`). Live X5: challenge is 64 hex chars; raw `token` is rejected; replay HMAC from a previous nonce is `invalid automation token`; nonces change per connection. `hmac`/`sha2` only; no `tokio`.

**Why FAKED-RED:** Naive Red @ `243ab78`:

```
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 76 filtered out
```

Exit 0. Evidence claimed exit 101.

The named test calls 3-arg `authorize_request`. On R1 that function is 2-arg and **accepts** a matching `Request.token`. Inserting the named test **does not compile** (`this function takes 2 arguments but 3 arguments were supplied`). Evidence panic was:

```
unsupported protocol version 2 (want 1)
```

That is a **version mismatch**, not “server still accepts `token`”. Reconstructing the 2-arg behavior:

```
test dispatch::tests::reviewer_old_code_accepts_raw_token_on_wire ... ok
```

Old code **accepts** the raw token. The pasted red never showed that. After the fix, the same named test can go from “wrong error (v2 vs v1)” to “right error (token must not be sent)” without the red having tested the finding.

Green @ HEAD (real):

```
test dispatch::tests::v2_raw_token_on_wire_is_rejected ... ok
test dispatch::tests::v2_challenge_hmac_accepts_matching_token ... ok
test dispatch::tests::v2_hmac_from_wrong_nonce_is_rejected ... ok
test server::tests::v2_untokened_server_does_not_send_challenge ... ok
```

Live (tokened `todo-headless` 127.0.0.1:18721):

```
CHALLENGE: {"v": 2, "op": "challenge", "nonce": "71869a9a8d3ecbb589dd588e308c87339636fe76af5f96daf2a7249b468e1ff4"}
RAW_TOKEN_FIELD: {"v":2,"id":"5","ok":false,"error":"token must not be sent on the wire"}
NONCE_CHANGED: True
REPLAY_OLD_NONCE: {"v":2,"id":"r","ok":false,"error":"invalid automation token"}
```

HTTP-framed after challenge (nc/python):

```
HTTP_READ_0: {"v":2,"id":"?","ok":false,"error":"bad json: expected value at line 1 column 1"}
HTTP_READ_1: {"v":2,"id":"?","ok":false,"error":"bad json: expected value at line 1 column 1"}
HTTP_READ_2: {"v":2,"id":"http1","ok":false,"error":"token must not be sent on the wire"}
HTTP_READ_3: EOF
```

HTTP headers do **not** close the socket (`continue` on bad json, `server.rs:241-249`). HMAC/token gate still holds. Residual: a later valid HMAC line on the same connection would be served.

Flaky `rpc_pipeline_wrong_token_fails_fast` (EPIPE vs allowed “reset”) is an R4 test gap. `docs/SECURITY.md:189` still says `gpui-agent` depends only on `serde`, `serde_json`, `thiserror` after hmac/sha2.

---

## R9 — `Wait.timeout_ms` polls `hello.ready`

**Verdict: CONFIRMED**

**Why:** `handle_request` splits `Wait { None }` vs `Some(ms)` (`dispatch.rs:86-89`). Timeout path returns `Response::err(..., "wait timed out")` without claiming ready (`:134-155`). Not a docs-only change.

Naive Red: test ABSENT.

Reconstructed Red on R4 tree (`Wait` still aliased to Hello):

```
thread 'dispatch::tests::wait_times_out_when_not_ready' panicked at crates/gpui-agent/src/dispatch.rs:350:9:
Response { v: 2, id: "1", ok: true, error: None, hello: Some(HelloInfo { ..., ready: false, ... }) }
test result: FAILED. 0 passed; 1 failed; ... 80 filtered out
```

Matches evidence. Green: `wait_times_out_when_not_ready`, `wait_none_is_immediate_hello`, `wait_succeeds_when_host_becomes_ready` all listed and passed in the 223 run.

---

## R10 — Protocol-only recipe registry + schema files

**Verdict: CONFIRMED**

**Why:** CLI/MCP default is `registry_from_schema_paths` → `protocol_registry()` (`registry.rs:318-332`, `recipe_cmd.rs:59`). `todo_registry()` kept and still used by in-process tests. `examples/schemas/todo.json` exists. `scripts/ci-recipe.sh:56` passes `--schema examples/schemas/todo.json`. Live CLI:

```
./target/debug/gpui-agent recipe validate examples/recipes/todo-crud.json
error: unknown invoke `todo.add` (not in schema registry; fail closed)
NOSCHEMA:1

./target/debug/gpui-agent recipe validate examples/recipes/todo-crud.json --schema examples/schemas/todo.json
{ "ok": true, "name": "todo-crud", ... }
SCHEMA:0
```

Naive Red: test ABSENT.

Reconstructed Red (`protocol_registry()` as clone of `todo_registry()`, plan’s specified fake-red):

```
called `Result::unwrap_err()` on an `Ok` value: ()
test registry::tests::recipe_rejects_unknown_invoke_with_protocol_only_registry ... FAILED
... 61 filtered out
```

Matches evidence. MCP recipe tools call `registry_from_schema_paths(&[])` so they honor `GPUI_AGENT_SCHEMA` but have **no `--schema` flag** (CLI recipe does). Small gap, not a failed default.

---

## R11 — Fail-closed duplicate ids

**Verdict: CONFIRMED**

**Why:** `assert_tree` uses `require_id` (`dispatch.rs:161`). `UiTree::require_id` / `find_all` / `duplicate_ids` added (`tree.rs:229-258`). `find` remains first-match (plan: deprecated for dispatch). `virtual_input::plan_click` still `tree.find` (`virtual_input.rs:39-41`) — title mentioned click-id; Do section allowed semantic/host find. R11b skipped.

Naive Red: test ABSENT.

Reconstructed Red on `e228918` (`assert_tree` still `find`):

```
called `Result::unwrap_err()` on an `Ok` value: ()
test dispatch::tests::assert_duplicate_id_is_error ... FAILED
... 83 filtered out
```

Matches evidence. Required tests listed: `find_all_returns_every_duplicate_in_dfs_order`, `require_id_zero_is_not_found`, `require_id_many_is_duplicate_error`, `assert_duplicate_id_is_error`.

---

## R12 — Doc drift pack

**Verdict: CONFIRMED**

**Why:** Parent `PROTOCOL.md` (R11) still said “The server and the CLI refuse non-loopback addresses” and had **no** `GPUI_AGENT_REMOTE` string (R4 did not add it). R12 replaces that with the remote triple (`docs/PROTOCOL.md:8-12`). README `page-settings --role window` → `--role page`. `todo-headless` comment is `from_env`. Behaviour unchanged except docs.

Naive Red: test ABSENT.

Reconstructed Red:

```
PROTOCOL.md must document remote bind with GPUI_AGENT_REMOTE
test tests::docs_protocol_mentions_remote_triple ... FAILED
... 8 filtered out
```

Matches evidence. Also added `page_settings_readme_role_is_page`. Residual: `SECURITY.md:189` hmac/sha2 sentence still wrong (overlaps R14).

---

## R13 — Lockfile + `cargo audit` (additive CI)

**Verdict: CONFIRMED**

**Why:** CI YAML adds three steps after rust-cache; does not remove unit tests or recipe. Test is a string grep (plan allowed).

Naive Red: test ABSENT.

Reconstructed Red on `12eed9a`:

```
test ci_workflow_runs_cargo_audit ... FAILED
```

(YAML dump in panic had unit tests / recipe, no `cargo audit`.)

Local Verify:

```
cargo generate-lockfile
Locking ... packages to latest compatible versions
cargo audit
Scanning Cargo.lock for vulnerabilities (895 crate dependencies)
warning: 5 allowed warnings found
AUDIT:0
```

`Cargo.lock` left untracked (this review did not commit it). Pinned `cargo-audit` 0.22.2. Did not use `--deny warnings`. Matches stated deviations.

---

## R14 — Clippy `result_large_err` + SECURITY I1

**Verdict: CONFIRMED**

**Why:** `authorize_request` returns `Result<(), Box<Response>>` (`dispatch.rs:28`) rather than a silent `#[allow]`. Call sites updated. Extra `mcp.rs` `while_let_loop` rewrite so four-package `-D warnings` passes (stated deviation). I1 workspace-unsafe sentence was already tightened in R12 (`SECURITY.md:196-197`).

Red @ `ada2a71`:

```
error: the `Err`-variant returned from this function is very large
  --> crates/gpui-agent/src/dispatch.rs:28:6
28 | ) -> Result<(), Response> {
   = note: `-D clippy::result-large-err` implied by `-D warnings`
error: could not compile `gpui-agent` (lib) due to 1 previous error
```

Matches evidence. Green @ HEAD:

```
cargo clippy -p gpui-agent -p todo-core -p gpui-agent-cli -p gpui-agent-recipe -- -D warnings
Finished `dev` profile ... 
CLIPPY_HEAD:0
```

No new required test (clippy was the red). Passing count unchanged (222 → 222 in evidence; HEAD 223 includes R15).

---

## R15 — INTEGRATING adapter checklist

**Verdict: PARTIAL**

**Why:** Section 8 checklist exists with the seven required bullets (`docs/INTEGRATING.md:153-163`), including `spawn_mailbox`, `GPUI_AGENT_SCREENSHOT_DIR`, v2 HMAC, default-deny. R7 test still present. R11b skipped.

The test only asserts two substrings (`todo-core/src/lib.rs:633-645`). `spawn_mailbox` was already in INTEGRATING at R14 (`0464504` line 52). Red failed only on `GPUI_AGENT_SCREENSHOT_DIR`:

```
INTEGRATING.md must mention GPUI_AGENT_SCREENSHOT_DIR
test tests::integrating_md_lists_mailbox_and_screenshot ... FAILED
... 10 filtered out
```

Matches evidence. The checklist itself is real, not a one-word fake, but the test would pass if those two strings appeared without the HMAC/default-deny/confine bullets.

---

## X1–X5 hand-break transcripts

Host: `GPUI_AGENT=1 GPUI_AGENT_TOKEN=review-canary-9f3a-TOKEN GPUI_AGENT_ADDR=127.0.0.1:18721 ./target/debug/todo-headless serve`.

**X2** (no token, no insecure): refuse bind, exit 2 — see R2.

**X1 / X3 / X4** screenshots (after valid HMAC hello):

```
SCREENSHOT '../x.png': {"v":2,"id":"2","ok":false,"error":"screenshot_unavailable: headless host has no pixel surface"}
SCREENSHOT '/etc/passwd.png': {"v":2,"id":"3","ok":false,"error":"screenshot_unavailable: headless host has no pixel surface"}
SCREENSHOT '-Sc.png': {"v":2,"id":"4","ok":false,"error":"screenshot_unavailable: headless host has no pixel surface"}
SHOTDIR_LIST: []
ETC_PASSWD_PNG_EXISTS: False
```

No write, no pre-delete, no `screencapture` argv. Confine/atomic/pre-delete are **not** executed on this host. SDK unit tests are the actual X1/X3/X4 evidence.

**X5** raw token + HTTP + replay: see R4 live paste. Canary `--help` leak: see R1.

---

## Final Verify (HEAD, after `cargo clean -p` of the four crates)

First run: **fail** `rpc_pipeline_wrong_token_fails_fast` (Broken pipe). Second `--no-fail-fast` run:

```
test result: ok. 87 passed; 0 failed; 0 ignored; ...  gpui-agent lib
test result: ok. 28 passed; ...                         gpui-agent bin
test result: ok. 8 passed; ...                          ci_recipe_assert
test result: ok. 8 passed; ...                          recipe_mcp_token
test result: ok. 64 passed; ...                         gpui-agent-recipe lib
test result: ok. 16 passed; ...                         todo_recipe
test result: ok. 11 passed; ...                         todo-core lib
test result: ok. 1 passed; ...                          tcp_crud
```

Sum **223** (`>= 194 + 29`). `--list` also 223. Required names present. `cargo clippy … -- -D warnings` exit 0. `cargo check -p todo` **exit 101** (fontconfig) — BLOCKED-ENV in this VM.

---

## Verdict table

| Task | Verdict |
| --- | --- |
| T0 | PARTIAL |
| R2 | CONFIRMED |
| R3 | CONFIRMED |
| R6 | CONFIRMED |
| R5 | PARTIAL |
| R7 | CONFIRMED |
| R8 | CONFIRMED |
| R1 | PARTIAL |
| R4 | FAKED-RED |
| R9 | CONFIRMED |
| R10 | CONFIRMED |
| R11 | CONFIRMED |
| R12 | CONFIRMED |
| R13 | CONFIRMED |
| R14 | CONFIRMED |
| R15 | PARTIAL |

**8.1: REJECT.** Do not mark PR #33 ready. Production sinks for D1–D4 and most of D5 are in place; evidence SHAs are gone, R4’s named red does not test token-on-wire, `--help` prints the live token, and the CI-package suite is not stably green.

---

## Required follow-ups for the workhorse

Each item is one sentence and names a task id.

- **T0:** Record `BLOCKED-ENV` for `cargo check -p todo` when `libfontconfig1-dev` is not installable, or pin the exact package name/snapshot that made T0’s check succeed.
- **R1:** Add `hide_env_values = true` (or equivalent) on `--token` / `GPUI_AGENT_TOKEN` so `gpui-agent --help` cannot print a canary, and assert that in a test that actually sets a canary and spawns the binary.
- **R4:** Replace the named red with one that fails because v1 `authorize_request` **accepts** `Request.token`, not because `v` is 2 vs 1; keep that test in git history or a reachable pre-fix commit.
- **R4:** Treat `Broken pipe (os error 32)` as an auth-close success in `rpc_pipeline_wrong_token_fails_fast` so the CI package set is stably `>= 223`.
- **R4:** Stop HTTP/non-JSON lines from staying on an authenticated connection forever, or document that only HMAC failure closes (headers currently `continue`).
- **R4 / R12:** Update `docs/SECURITY.md` “depends only on serde, serde_json, thiserror” to include `hmac` and `sha2`.
- **R2:** Fix the leftover P2 table row in `docs/NO_BRAINER_PLAN.md` that still says host `from_env` is optional.
- **R3:** Make `todo-headless` / `TodoStore::screenshot` reject unconfined paths **before** returning `screenshot_unavailable`, so a live screenshot op cannot skip `confine_screenshot_path`.
- **R5:** Keep the atomic-write test but also assert `write_png` (not only the stub) no longer `fs::write`s the dest in place; live `-Sc.png` should fail at confine, not only at “no pixel surface”.
- **R11:** Fail-closed duplicate ids in `virtual_input::plan_click` (`find` → `require_id`) if the title’s “click-id resolution” is in scope; otherwise document that virtual clicks are still first-match.
- **R15:** Strengthen `integrating_md_lists_mailbox_and_screenshot` to require the HMAC / default-deny / confine checklist lines, not only `spawn_mailbox` + `GPUI_AGENT_SCREENSHOT_DIR`.
- **evidence:** Rewrite every `Commit:` field to the reachable git-log SHA (or stop amending after filling evidence) so a reviewer can `git show` the claimed red tree.
- **R10:** Thread `--schema` into MCP recipe tools the same way as the CLI, or document that MCP is env-only (`GPUI_AGENT_SCHEMA`).
