# Independent review round 2 — `gol/security-remediation-2026-09-11`

Reviewer did not author the remediation. HEAD reviewed: `dae6f694eb83df38218ed75a32ed25d5f4929095` (matches `origin/gol/security-remediation-2026-09-11`). Base: `8857139af12fb033b4dd04eabd8d19b5bfc5ffc6`. Plan: `docs/plans/2026-09-11-security-remediation-plan.md` §1 + 8.1. Round-1 review: `docs/plans/evidence/REVIEW-ROUND-1.md` (REJECT). Evidence: `docs/plans/evidence/2026-09-11-remediation-evidence.md` `(round 2)` blocks. Draft PR: https://github.com/hexuria/gpui-agent/pull/33.

**Section 8.1 verdict: ACCEPT.** Scoped round-2 work is at the sinks it claims. Evidence SHAs are reachable. CI-package count is stably **232**.

**MERGE-READY CANDIDATE — operator may mark draft ready.**

This round re-reviews only tasks that were not CONFIRMED in REVIEW-ROUND-1, plus CONFIRMED tasks with an explicit round-2 follow-up (R2 NO_BRAINER row, R3 confine-before-unavailable, R10 MCP `--schema`, R11 `plan_click`).

Method (§8.1): re-run Verify at HEAD; reconstruct Red on the reachable SHA claimed in each `(round 2)` block (naive parent checkout is 0 tests when the named test landed in the same commit as the fix); diff the sink; grep forbidden moves on `git diff 8857139...HEAD`; hand-break X1–X5; confirm the CI package set is stably `>= 232`.

Round-2 commits used:

| Task | Commit | Git parent | Claimed red tree |
| --- | --- | --- | --- |
| T0 | `623080ef089ce2417489487e9947d775a2b7ca4f` | `d3a1ca8` | n/a |
| R2 | `49eefab057b68bfe0dbd82fd2c3ea29464c231ff` | `623080e` | `623080e` + test only |
| R1 | `abfdfd74e733cd295e197a4a1069d47ec6d190f1` | `49eefab` | `49eefab` + test only |
| R3 | `5b79ae870749b70440c208499036a5a5d4976a26` | `abfdfd7` | `abfdfd7` + test only |
| R5 | `f958d2996b26d703e8dfa481eebdeabade452c0e` | `5b79ae8` | claimed `037eb4e` + test only |
| R4 | `d36ac7ebfbfa87013807ab78d0836a226238debc` | `f958d29` | v1 red: `243ab78`; bad-JSON: `f958d29` |
| R10 | `78df9c1a04521905d528d6734dbaec6309299d7a` | `d36ac7e` | `d36ac7e` + test only |
| R11 | `69ebbf953ad519142ec3c6dc39df56cfffdd16e0` | `78df9c1` | `78df9c1` + test only |
| R15 | `ad5b80833180b4bf5fe0bff9df736052b7aad8b4` | `69ebbf9` | claimed HMAC-strip on `3ad6241` |

`git cat-file -t` on every SHA above, and on all 16 rewritten round-1 `Commit:` objects: **commit**.

---

## Cross-cutting

### Forbidden-move grep (`git diff 8857139...HEAD`)

- Removed `#[test]`: **none**. Allowed R2 rename remains `loopback_bind_does_not_need_token_or_remote_flag` → `loopback_bind_requires_token_client_does_not`.
- Added `#[ignore]`: none.
- Added `#[allow(` in the branch: none. `apps/todo/src/macos_window.rs` `#[allow(unused_imports)]` is **on `main`**.
- Cap constants **unchanged**: `MAX_LINE_BYTES = 1024 * 1024`, `MAX_CONNECTIONS = 32`, `MAX_MAILBOX_DEPTH = 128`, `MAX_RECIPE_STEPS = 256`, `IDLE_TIMEOUT = 30s`. `tokens_match` / `#[inline(never)]` `tokens_match_bytes` is not in the `security.rs` diff vs `main` (byte-identical).
- `.github/workflows/ci.yml`: **additive only** (generate-lockfile, `cargo-audit` 0.22.2, `cargo audit`). Unit-test and recipe steps remain.
- `authorize_client` loopback-without-token still `Ok` (`crates/gpui-agent/src/security.rs:128-135`). `authorize_bind` loopback-without-token is `Err(LoopbackRequiresToken)` (`:115-120`).
- No `tokio`. No `gpui-kit` on `crates/gpui-agent`. Added `hmac = "0.12"`, `sha2 = "0.10"` only.
- R11b skipped (no CSS/XPath grammar; recipe `--role`/`--name` tokens are existing `.wants` parse, not a client locator).
- `PROTOCOL_VERSION = 2`.

### Test inventory

`cargo test -p gpui-agent -p todo-core -p gpui-agent-cli -p gpui-agent-recipe -- --list` → **232** tests. Required names from the plan plus round-2 follow-ups are present (see named greens under each task). Arithmetic: `BASELINE_PASSED 194` + round-1 29 + round-2 9 = **232** (R15 round 2 strengthened an existing test; count stayed 232 after R11).

### Evidence SHAs

Round-1 hole (amended-away `Commit:` objects) is closed. Round-2 `Commit:` fields match `git log` and `git cat-file -t` = `commit`. Naive `git grep 'fn <test>' <git-parent>` is still **ABSENT** for tests that landed in the same commit as the fix; reconstructed reds below insert only the named test onto parent production.

### CI-package stability

First full CI set at HEAD (`cargo test -p gpui-agent -p todo-core -p gpui-agent-cli -p gpui-agent-recipe`):

```
test result: ok. 91 passed; 0 failed; 0 ignored; ...  gpui-agent lib
test result: ok. 30 passed; ...                         gpui-agent bin
test result: ok. 8 passed; ...                          ci_recipe_assert
test result: ok. 9 passed; ...                          recipe_mcp_token
test result: ok. 64 passed; ...                         gpui-agent-recipe lib
test result: ok. 16 passed; ...                         todo_recipe
test result: ok. 13 passed; ...                         todo-core lib
test result: ok. 1 passed; ...                          tcp_crud
```

Sum **232**. Second full run: **same split, exit 0**. `rpc_pipeline_wrong_token_fails_fast` 20 consecutive `--exact` runs: **20 ok / 0 fail**. Round-1 EPIPE flake is not reproduced here.

---

## T0 — Record CI-package baseline / BLOCKED-ENV

**Verdict: BLOCKED-ENV** (honest; `apps/todo` / fontconfig only)

**Why:** Round-1 PARTIAL was (1) missing evidence SHAs and (2) an unverifiable claim that `libfontconfig1-dev` + `cargo check -p todo` succeeded. Round-2 records the honest failure. Independently reproduced on this Ubuntu 24.04.4 VM. Baseline **194** at `8e09eba` was already reproduced in round 1; SHA `8e09ebacc198f3d91d3468fdb302ab3bada26166` now exists.

Red: n/a (docs/evidence).

Verify:

```
$ uname -a
Linux cursor 6.12.94+ … x86_64 GNU/Linux
Ubuntu 24.04.4 LTS

$ sudo apt-get install -y libfontconfig1-dev pkg-config
E: Unable to locate package libfontconfig1-dev
APT_EXIT:100

$ apt-cache search fontconfig
fontconfig - generic font configuration library - support binaries
fontconfig-config - generic font configuration library - configuration
libfontconfig1 - generic font configuration library - runtime
libxft2 - FreeType-based font drawing library for X
```

`apt-cache policy libfontconfig1-dev` prints no candidate (package not in this apt snapshot). `libfontconfig1` runtime **is** installed (`2.15.0-1.1ubuntu2`). `pkg-config --exists fontconfig` → 1. No `fontconfig.pc`.

```
$ cargo check -p todo
error: failed to run custom build command for `yeslogic-fontconfig-sys v6.0.1`
…
Package 'fontconfig', required by 'virtual:world', not found
The system library `fontconfig` required by crate `yeslogic-fontconfig-sys` was not found.
The file `fontconfig.pc` needs to be installed …
TODO_OK_EXIT:101
```

`cargo check -p todo --features embedded-host` → **exit 101**, same `fontconfig.pc` miss.

CI-package set at HEAD: **232**, exit 0 (see Cross-cutting). Caps test:

```
test cap_tests::security_caps_unchanged ... ok
```

Diff at T0 round 2 is evidence-only (`2026-09-11-remediation-evidence.md`). No production change.

This is the allowed merge-ready exception: honest `BLOCKED-ENV` for `apps/todo` fontconfig, not a failed CI-package gate.

---

## R1 — Docs embed-only + hide token env on `--help`

**Verdict: CONFIRMED**

**Why:** Round-1 PARTIAL was clap printing the live `GPUI_AGENT_TOKEN` (`hide_env_values` missing). Round-2 sink is the clap arg at `crates/gpui-agent-cli/src/main.rs:33`: `#[arg(long, env = "GPUI_AGENT_TOKEN", hide_env_values = true)]`. Test `cli_help_hides_token_env_canary` actually spawns `CARGO_BIN_EXE_gpui-agent` (`recipe_mcp_token.rs`). About-text remains embed-only (`main.rs:15-19`). Existing `cli_help_does_not_claim_cdp_attach` not deleted.

Naive Red @ `49eefab`: test **ABSENT**, `running 0 tests`, exit 0.

Reconstructed Red (named test only, no `hide_env_values`):

```
thread 'cli_help_hides_token_env_canary' panicked at crates/gpui-agent-cli/tests/recipe_mcp_token.rs:237:5:
--help must not print the live GPUI_AGENT_TOKEN canary:
…
      --token <TOKEN>
          Shared secret; must match `GPUI_AGENT_TOKEN` on the host when the host has one. Required (non-empty) for `recipe run` and `mcp`
          
          [env: GPUI_AGENT_TOKEN=review-canary-9f3a-TOKEN]
…
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 8 filtered out
```

Matches evidence. Green @ HEAD:

```
test cli_help_hides_token_env_canary ... ok
```

Live X1-canary (plan step 5):

```
$ GPUI_AGENT_TOKEN=review-canary-9f3a-TOKEN ./target/debug/gpui-agent --help
…
      --token <TOKEN>
          …
          [env: GPUI_AGENT_TOKEN]
…
CANARY_IN_HELP: False
DRIVE_ANY: False
AGENTHOST: True
ENV_LINE_HAS_EQ_VALUE: False
HELP_EXIT: 0
```

Root cause at the clap env printer, not a docs-only comment. Help still names the env var; it does not print the canary value.

---

## R2 — NO_BRAINER P2 table (follow-up)

**Verdict: CONFIRMED**

**Why:** Round-1 residual: `docs/NO_BRAINER_PLAN.md:160` still said host `from_env` was optional. Round-2 replaces that row with D1 default-deny + `GPUI_AGENT_INSECURE_NO_TOKEN`. Production `from_env` / `authorize_bind` was already the sink (round-1 CONFIRMED); this follow-up is the leftover table. Test `no_brainer_host_from_env_is_not_optional` reads the doc.

Naive Red @ `623080e`: 0 tests, exit 0.

Reconstructed Red (committed assertion text; workhorse had shortened the panic after red so green would not dump the whole file):

```
thread 'tests::no_brainer_host_from_env_is_not_optional' panicked at crates/todo-core/src/lib.rs:653:9:
P2 table must not say host from_env is still optional after D1 default-deny
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 11 filtered out
```

Parent still contains `| Host \`from_env\` | Still optional. … |`. Green @ HEAD:

```
test tests::no_brainer_host_from_env_is_not_optional ... ok
```

Live X2 (no token, no insecure) still refuses bind — see X1–X5. `git grep 'Still optional'` in product docs is empty (only evidence/review quotes).

---

## R3 — Confine before `screenshot_unavailable`

**Verdict: CONFIRMED**

**Why:** Round-1 residual: `TodoStore::screenshot` ignored `path` and returned `screenshot_unavailable`, so a live screenshot op skipped `confine_screenshot_path`. Round-2 sink is `crates/todo-core/src/lib.rs` `screenshot`: `require_screenshot_path` then `confine_screenshot_path` **before** the unavailable string. `apps/todo` `screenshot_this_window` also confines first (`apps/todo/src/app.rs:811-812`) — compile of that crate is BLOCKED-ENV here; the SDK/host path is what live `todo-headless` hits.

Naive Red @ `abfdfd7`: 0 tests, exit 0.

Reconstructed Red against the real pre-fix sink (`let _ = path`):

```
thread 'tests::screenshot_unconfined_path_fails_before_unavailable' panicked at crates/todo-core/src/lib.rs:676:13:
unconfined path must fail at confine, not unavailable: /etc/passwd.png screenshot_unavailable: headless host has no pixel surface
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 12 filtered out
```

Matches evidence. Green @ HEAD:

```
test tests::screenshot_unconfined_path_fails_before_unavailable ... ok
```

Live X1 / X3 / X4 against tokened `todo-headless` (HMAC hello, `GPUI_AGENT_SCREENSHOT_DIR` isolated):

```
SCREENSHOT '../x.png': {"v":2,"id":"2","ok":false,"error":"screenshot path must be a relative .png name under the host screenshot dir"}
SCREENSHOT '/etc/passwd.png': {"v":2,"id":"3","ok":false,"error":"screenshot path must be a relative .png name under the host screenshot dir"}
SCREENSHOT '-Sc.png': {"v":2,"id":"4","ok":false,"error":"screenshot filename must not start with '-' or '.'"}
SHOTDIR_LIST: []
ETC_PASSWD_PNG_EXISTS: False
RELATIVE_PNG: {"v":2,"id":"6","ok":false,"error":"screenshot_unavailable: headless host has no pixel surface"}
SHOTDIR_AFTER_RELATIVE: []
```

Unconfined paths fail at confine, not unavailable. Relative `.png` still honestly unavailable and does not invent a file. No `/etc/passwd.png`. This is the round-1 live hole, closed.

`cargo check -p todo` still **exit 101** (T0 BLOCKED-ENV). macOS handoff for the `apps/todo` one-line confine remains.

---

## R5 — `write_png` is atomic, not only the stub

**Verdict: CONFIRMED**

**Why:** Round-1 PARTIAL: red was `atomic_write_png not implemented` (stub), and live `-Sc.png` never reached confine. Production `write_png_in` at HEAD calls `confine_screenshot_path_in` then `atomic_write_png` (`screenshot.rs:117-120`). `atomic_write_png` writes `{parent}/.{stem}.{pid}-{n}.tmp` then `rename`. Round-2 adds (1) behavioral `write_png_in_temp_then_rename_replaces_without_predelete` and (2) source test `write_png_in_source_does_not_fs_write_dest_in_place`. Original stub test kept. Live dash filename is now a confine miss (R3).

Naive Red @ round-2 git parent `5b79ae8` and claimed `037eb4e`: named source test **ABSENT**, 0 tests, exit 0.

Reconstructed Red on claimed parent **`037eb4e`** (`write_png_in` still `fs::write(&dest, png)`):

```
thread 'screenshot::tests::write_png_in_source_does_not_fs_write_dest_in_place' panicked at crates/gpui-agent/src/screenshot.rs:460:9:
write_png_in must call atomic_write_png:
pub fn write_png_in(path: &str, png: &[u8], base: &Path) -> Result<serde_json::Value, String> {
    let dest = confine_screenshot_path_in(path, base)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    std::fs::write(&dest, png).map_err(|err| format!("{}: {err}", dest.display()))?;
    Ok(serde_json::json!({ "path": dest.to_string_lossy() }))
}
…
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 73 filtered out
```

That is **in-place `fs::write`**, not a missing-function stub. Matches evidence (body dump is longer here because the next `pub fn` on that tree is `screencapture_window_argv`; evidence elided with `…`).

Cross-check: same test inserted onto round-2 git parent `5b79ae8` (production already `atomic_write_png`) → **exit 0 / 1 passed**. Expected for a test-only follow-up of an already-fixed sink. Not FAKED-RED: evidence claimed `037eb4e`, and that tree fails.

Green @ HEAD:

```
test screenshot::tests::write_png_in_source_does_not_fs_write_dest_in_place ... ok
test screenshot::tests::write_png_in_temp_then_rename_replaces_without_predelete ... ok
```

Live `-Sc.png`: confine, not unavailable (R3 paste). Root cause of the original truncate-in-place write is the `atomic_write_png` sink from R5 round 1; round 2 is the missing assertion plus a `write_png_in` behavior test.

---

## R4 — v1-accepts-token red, close on bad JSON, Broken pipe, hmac/sha2

**Verdict: CONFIRMED**

**Why:** Round-1 FAKED-RED: named test panicked `unsupported protocol version 2 (want 1)` instead of “v1 accepts `Request.token`”. Production sink was already real (`authorize_request` rejects non-empty `token` when the host has a token; challenge + HMAC). Round-2:

1. Reachable v1 reconstruction `docs/plans/evidence/r4-round2-red-v1-authorize-request.rs` (2-arg `authorize_request`, `Request::new` uses tree `PROTOCOL_VERSION`).
2. `bad_json` **breaks** the host/mailbox loop (`server.rs:241-249` `break`, same on mailbox) instead of `continue`.
3. `rpc_pipeline_wrong_token_fails_fast` treats `Broken pipe` / `os error 32` as auth-close.
4. `docs/SECURITY.md` lists `hmac` and `sha2`.

Naive Red @ `243ab78` for the named test: 0 tests, exit 0. Naive Red @ `f958d29` for `bad_json_after_challenge_closes_connection`: 0 tests, exit 0.

Reconstructed v1 red on **`243ab78`** (evidence file applied; `assert_eq!(req.v, PROTOCOL_VERSION)` so this cannot be a v2-vs-1 miss):

```
thread 'dispatch::tests::v2_raw_token_on_wire_is_rejected' panicked at crates/gpui-agent/src/dispatch.rs:272:59:
called `Result::unwrap_err()` on an `Ok` value: ()
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 76 filtered out
```

That is **token accepted**. Matches evidence. Not a version mismatch.

Reconstructed HTTP/non-JSON red on round-2 git parent **`f958d29`** (`continue` on bad json):

```
thread 'server::tests::bad_json_after_challenge_closes_connection' panicked at crates/gpui-agent/src/server.rs:796:9:
non-JSON after challenge must close; later HMAC must not be served: write=Ok(()) read=Ok(142) "{\"v\":2,\"id\":\"1\",\"ok\":true,\"hello\":{…\"ok\":true…}}\n"
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 89 filtered out
```

Matches evidence (HMAC hello still served after `GET / HTTP/1.1`).

Cross-check: named `v2_raw_token_on_wire_is_rejected` **already passes** on `f958d29` (round-2 parent). The extra “must not be a version mismatch” asserts were added to an already-green test. The production close-on-bad-json change is what actually reds on that parent.

Green @ HEAD:

```
test dispatch::tests::v2_raw_token_on_wire_is_rejected ... ok
test server::tests::bad_json_after_challenge_closes_connection ... ok
test server::tests::rpc_pipeline_wrong_token_fails_fast ... ok
```

Live X5 (tokened `todo-headless` `127.0.0.1:18731`):

```
CHALLENGE: {"v":2,"op":"challenge","nonce":"b6d0ee9231884d05ca32ac323045182c61905df0ac064e8a33d9b31f2c62af7e"}
RAW_TOKEN_FIELD: {"v":2,"id":"5","ok":false,"error":"token must not be sent on the wire"}
NONCE_CHANGED: True
REPLAY_OLD_NONCE: {"v":2,"id":"r","ok":false,"error":"invalid automation token"}
HTTP_READ_0: {"v":2,"id":"?","ok":false,"error":"bad json: expected value at line 1 column 1"}
HTTP_READ_1: EOF
```

HTTP after challenge now **closes** (round-1 residual: headers `continue`d). HMAC/token gate still holds. `docs/SECURITY.md:190-191` names `hmac` and `sha2`. No `tokio`. Caps unchanged.

---

## R10 — MCP `--schema` (follow-up)

**Verdict: CONFIRMED**

**Why:** Round-1 residual: MCP recipe tools called `registry_from_schema_paths(&[])` (env-only). Round-2 sink: `Command::Mcp { schema }` (`main.rs`) passed to `mcp::run(..., schema_paths)`; `recipe_validate` / `plan` / `run` / `resolve` merge CLI paths with a tool `schema` arg via `recipe_schema_paths`. Empty paths still reject unknown invoke (`recipe_validate_unknown_invoke_fails_closed` kept). Not a docs-only note.

Naive Red @ `d36ac7e`: 0 tests, exit 0.

Reconstructed Red (`Cli::try_parse_from(["gpui-agent","mcp","--schema",…])` while `Command::Mcp` has no schema field):

```
thread 'tests::mcp_schema_flag_parses_like_recipe' panicked at crates/gpui-agent-cli/src/main.rs:739:9:
mcp --schema must parse like recipe --schema: Some("error: unexpected argument '--schema' found\n\nUsage: gpui-agent mcp\n\nFor more information, try '--help'.\n")
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 28 filtered out
```

Matches evidence. Green @ HEAD:

```
test tests::mcp_schema_flag_parses_like_recipe ... ok
test mcp::tests::recipe_validate_honors_schema_paths ... ok
```

Did not spawn a live stdio MCP session (workhorse deviation; parse + registry sink tests cover the flag and merge). `GPUI_AGENT_SCHEMA` still honored via `registry_from_schema_paths`.

---

## R11 — `plan_click` fail-closed on duplicate ids (follow-up)

**Verdict: CONFIRMED**

**Why:** Round-1 residual: `virtual_input::plan_click` still `tree.find` (first-match) while `assert_tree` used `require_id`. Round-2 sink: `plan_click` calls `tree.require_id(target)?` (`virtual_input.rs:38-39`). Test uses two `dup` nodes with non-zero bounds so first-match would return a click, not `virtual_unavailable`. `find` remains first-match for non-dispatch helpers. R11b skipped.

Naive Red @ `78df9c1`: 0 tests, exit 0.

Reconstructed Red (`plan_click` still `find`):

```
thread 'virtual_input::tests::plan_click_duplicate_id_is_error' panicked at crates/gpui-agent/src/virtual_input.rs:225:44:
called `Result::unwrap_err()` on an `Ok` value: VirtualPointerClick { target: "dup", x: 5.0, y: 5.0 }
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 90 filtered out
```

That is **first-match click**, not zero-bounds. Matches evidence. Green @ HEAD:

```
test virtual_input::tests::plan_click_duplicate_id_is_error ... ok
```

`assert_duplicate_id_is_error` still present (round-1 CONFIRMED `assert_tree` path).

---

## R15 — INTEGRATING test requires HMAC / default-deny / confine

**Verdict: CONFIRMED**

**Why:** Round-1 PARTIAL: `integrating_md_lists_mailbox_and_screenshot` only required `spawn_mailbox` + `GPUI_AGENT_SCREENSHOT_DIR`. Section 8 checklist already listed HMAC / default-deny / confined `.png` (`docs/INTEGRATING.md:153-163`). Round-2 strengthens that test; production INTEGRATING.md is unchanged (`git show ad5b808 --stat` is `todo-core` + evidence). R7 `mailbox_hello_auth_matches_server_token` still present. R11b skipped.

Naive Red @ round-2 git parent `69ebbf9`: named test **exists** (old asserts) and **passes** (docs already have the bullets). Inserting the new HMAC/default-deny/confine asserts onto that parent **also passes** (docs already contain `HMAC-SHA256`, `GPUI_AGENT_INSECURE_NO_TOKEN`, `Confined screenshots` / `relative `.png``). Expected for a test-only commit.

Claimed reconstruction (replace `HMAC-SHA256` with `hmac` on `3ad6241`, original two asserts kept):

```
thread 'tests::integrating_md_lists_mailbox_and_screenshot' panicked at crates/todo-core/src/lib.rs:647:9:
adapter checklist must name HMAC-SHA256
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 10 filtered out
```

Matches evidence. Pre-R15 `0464504` with the full strengthened test fails first on `GPUI_AGENT_SCREENSHOT_DIR` (checklist absent):

```
INTEGRATING.md must mention GPUI_AGENT_SCREENSHOT_DIR
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 10 filtered out
```

Green @ HEAD:

```
test tests::integrating_md_lists_mailbox_and_screenshot ... ok
```

Not FAKED-RED: they did not claim a production edit; they documented HMAC-strip reconstruction; the new assert is what fails when HMAC-SHA256 is removed. The round-1 hole (“test would pass without the HMAC bullets”) is closed.

---

## X1–X5 hand-break transcripts

Host: `GPUI_AGENT=1 GPUI_AGENT_TOKEN=review-canary-9f3a-TOKEN GPUI_AGENT_ADDR=127.0.0.1:18731 GPUI_AGENT_SCREENSHOT_DIR=<temp> ./target/debug/todo-headless serve`.

**X2** (no token, no insecure):

```
STDERR: refusing to start automation: loopback bind requires a non-empty GPUI_AGENT_TOKEN (set GPUI_AGENT_INSECURE_NO_TOKEN=1 only for local demos)
X2_EXIT:2
```

Insecure opt-in (timeout 1s):

```
*** GPUI_AGENT_INSECURE_NO_TOKEN=1 ***
This host accepts unauthenticated loopback control-plane requests.
Any local process can snapshot, click, invoke, and shutdown.
…
auth: none (GPUI_AGENT_INSECURE_NO_TOKEN=1 — any local process can drive this host)
INSECURE_TIMEOUT:1
```

**X1 / X3 / X4** screenshots after valid HMAC hello: see R3 live paste. Confine runs. No write, no pre-delete, no `screencapture` argv. `-Sc.png` is a filename reject.

**X5** raw token + HTTP + replay: see R4 live paste. Connection closes after non-JSON. Canary `--help`: see R1.

---

## Final Verify (HEAD `dae6f69`)

```
cargo test -p gpui-agent -p todo-core -p gpui-agent-cli -p gpui-agent-recipe
```

Pass 1 and pass 2: **232** (`91+30+8+9+64+16+13+1`), 0 failed, 0 ignored. `--list` 232. `rpc_pipeline_wrong_token_fails_fast` 20/20. `cargo clippy -p gpui-agent -p todo-core -p gpui-agent-cli -p gpui-agent-recipe -- -D warnings` → **exit 0**. `cargo check -p todo` **exit 101** (fontconfig) — honest BLOCKED-ENV.

---

## Verdict table (scoped tasks)

| Task | Round-1 | Round-2 |
| --- | --- | --- |
| T0 | PARTIAL | **BLOCKED-ENV** (honest; fontconfig / `apps/todo` only) |
| R1 | PARTIAL | **CONFIRMED** |
| R4 | FAKED-RED | **CONFIRMED** |
| R5 | PARTIAL | **CONFIRMED** |
| R15 | PARTIAL | **CONFIRMED** |
| R2 (NO_BRAINER follow-up) | CONFIRMED + leftover row | **CONFIRMED** |
| R3 (confine before unavailable) | CONFIRMED + live skip | **CONFIRMED** |
| R10 (MCP `--schema`) | CONFIRMED + MCP gap | **CONFIRMED** |
| R11 (`plan_click` fail-closed) | CONFIRMED + `find` on click | **CONFIRMED** |

**8.1: ACCEPT.** MERGE-READY CANDIDATE — operator may mark draft ready.

Do not merge from this review. This agent did not mark the draft ready.

---

## Follow-ups for non-CONFIRMED

- **T0:** Operator should `cargo check -p todo` and `cargo check -p todo --features embedded-host` on a machine that has fontconfig headers (`libfontconfig1-dev` or Xcode), including the round-2 `apps/todo` confine-before-capture line; this VM cannot.
