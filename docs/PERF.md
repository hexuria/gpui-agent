# Performance notes

Stacked on [PR #4](https://github.com/hexuria/gpui-agent/pull/4)
(`gol/recipes-tmp-perf-e79b`). **Do not merge that PR or this follow-up**
unless you intend to land the experiment.

Numbers: cloud VM, `x86_64`, `rustc 1.98.1`, Criterion `--quick`,
`cargo bench` (release). Median times. SIMD / FxHash / tokio startup
were a separate release probe (`simd-json 0.14`, `tokio 1.53`,
`rustc-hash 2`) so those crates never entered this workspace.

Security caps are unchanged: `MAX_LINE_BYTES = 1 MiB`,
`MAX_CONNECTIONS = 32`, `MAX_MAILBOX_DEPTH = 128`,
`MAX_RECIPE_STEPS = 256`. No OS HID. No `unsafe`.

```bash
cargo bench -p gpui-agent --bench agent_perf
cargo bench -p gpui-agent-recipe --bench recipe_plan
```

## Adopted (measured)

| Change | Bench | Number | Why |
| --- | --- | --- | --- |
| **NDJSON pipeline** (`AgentClient::rpc_pipeline`): write N request lines, then read N responses on the **same** socket. Recipe `run` uses this for independent DAG waves when there is no `--screenshot-dir` / `--record` (those need a turn after each step). Each line still carries token + version. Chunks cap at `MAX_MAILBOX_DEPTH`. | `rpc_pipeline_32_hellos` vs `rpc_session_reuse_32_hellos` | **135 µs** vs **526 µs** (~3.9×) | Hides localhost RTT. Not a new wire `op`. |
| Same, through `recipe run` | `recipe_run_32_hello_wave_pipelined` vs `recipe_run_32_hellos_one_session` | **153 µs** vs **544 µs** (~3.6×) | Linear recipes stay sequential (fail-fast + one-step waves). |
| **Direct `$param` subst** on `Op` fields + byte scan for `$ident` / `${ident}` (no `Vec<char>`, no serde round-trip). Fast path when the string has no `$`. | `apply_params_three_steps` | **358 ns** | Compile/validate path; behavior tests include Unicode around placeholders and invoke `args`. |
| **Index schedule**: Kahn uses `Vec<usize>`; implicit linear recipes skip the graph clone. `apply_params` skipped when `params` is empty. | `compile_40_invoke_adds` / `compile_255_invoke_adds` / `compile_wide_dag_32_leaves` | **82 µs** / **1.19 ms** / **61 µs** | Same waves/fingerprint shape as PR #4 (implicit needs still chained). |
| **`flatten_into` without `node_count` pre-walk** | `tree_flatten_capacity` vs naive vs no-precount | **1.20 µs** vs **4.91 µs** naive; no-precount **1.11 µs** | The extra count pass was slower than letting `Vec` grow. Reuse: `tree_flatten_into_reuse` **1.15 µs**. |
| **Mailbox `take`**: store `MailboxRequest` and `mem::take` | `mailbox_push_take_full` (128) | **23.3 µs** | One pointer swap instead of drain+map. Cap unchanged. |
| **`rpc_op(&Op)` + stack `fmt_u64` ids** | (session benches above) | — | Recipe run no longer `clone`s each `Op`. Wire JSON matches owned `Request` (unit test). |
| **4 KiB encode/line buffers**; todo `tree()` `Vec::with_capacity(items)` | snapshot serialize reuse vs `to_string` | **38.3 µs** vs **37.8 µs** | Reuse already paid off in PR #4; capacity avoids early growth on snapshots. |

PR #4 session reuse vs reconnect is still the big win and is unchanged in kind:

| Bench | This VM |
| --- | --- |
| `rpc_once_reconnect_32_hellos` | **324 ms** |
| `rpc_session_reuse_32_hellos` | **526 µs** (~616×) |
| `handle_request_inprocess_32_hellos` (CPU floor) | **1.80 µs** |

## Tried and rejected

### SIMD JSON (`simd-json`)

Protocol lines are tiny (`hello` ~30 bytes). `serde_json::from_slice`:
**103 ns**. `simd-json` serde on a reused buffer: **355 ns** (hello),
**265 ns** with a fresh `to_vec` (click). A 100-node tree:
`serde_json` **106 µs** vs simd-json **120 µs** (includes copy).

Criterion `protocol_parse_hello` **125 ns**;
`protocol_parse_snapshot_tree_100` **122 µs**. Parse is not the RPC
bottleneck (526 µs / 32 hellos). **No simd-json dep.**

### Async (`tokio`)

Current-thread runtime build **17 µs**; multi-thread (2 workers)
**65 µs**. That is already one sequential hello RTT. One localhost
session is blocking reuse + optional pipeline; an executor does not
overlap waits we do not have. **No tokio in the control plane.**

### Threads / parallel DAG / parallel tree walk

`tree_node_count_100_seq` **465 ns** vs `std::thread::scope` over the
root list **26 µs** (~55× worse). Spawning one thread per row of a
2000-item tree: **46 ms** vs sequential **9.0 µs**. Host dispatch is
`Mutex` (headless) or the GPUI UI thread (mailbox). Parallel waves on
extra TCP connections would fight `MAX_CONNECTIONS` and that mutex.
**Never parallelize UI-thread GPUI work.** Recipe waves pipeline on
**one** connection instead.

### Wire `batch` op

In-process 32 hellos are **1.8 µs**; pipelined NDJSON is **135 µs**. A
new `op: batch` could theoretically approach one RTT, but it is a
protocol bump, needs nested responses, mailbox/virtual expansion, and
fights per-step receipts / screenshots / `--record`. Pipeline of
ordinary lines got the measured win without that. **No batch op.**

### mimalloc / jemalloc

Not adopted, not even behind a flag. The hot path is serialize +
loopback syscalls, not allocator traffic. A global allocator swap
without a process-level A/B is cargo-cult.

### compact_str / smallvec / Arc intern / SoA / field reorder

`UiNode` is **168 bytes** on this 64-bit target (`uinode_layout_stays_compact`).
Reordering `Bounds` vs bools does not shrink it. `SmallVec<[UiNode; 2]>`
would inline ~336 bytes on every leaf. `compact_str` is still 24 stack
bytes; snapshot JSON still emits the bytes. Ids are unique per node;
roles repeat but interning does not shrink the wire tree. SoA would
break the snapshot JSON shape.

### FxHashMap / hashbrown for bounds maps

`tree_apply_bounds_hashmap` **4.84 µs** (100 rows / ~301 nodes) vs
BTreeMap **8.50 µs** vs linear scan **42.5 µs**. Keep `std::HashMap`.
A separate probe (10k keys) showed FxHashMap lookup **7 ns** vs
HashMap **21 ns** — not worth a public-API/`rustc-hash` dep for a
desktop paint of a todo list.

`tree_find_dfs_last_of_100_rows` **811 ns** vs a prebuilt HashMap
**12 ns**. Building the map every snapshot is more expensive than one
assert. No index on `UiTree`.

### Unsafe (`get_unchecked` / `from_utf8_unchecked`)

Schedule indices are in-range by construction; JSON parse dwarfs a
bounds check. UTF-8 is already validated in `read_limited_line_into`.
A second scan is tens of nanoseconds. **No unsafe.**

## CLI recipe path

`cli_validate_plan_wants_40` (**parse + validate + compile**, no host):
**114 µs**. `parse_wants_40_invoke_adds` **18.5 µs**;
`parse_json_40_invoke_adds` **26.3 µs**. Run time is the TCP session
(linear ~544 µs for 32 hellos; pipelined wave ~153 µs). Do not spawn
one CLI process per op (PR #4).

## Release LTO (integrators)

This lab workspace does not set `lto` (compile time). Shipping a
product binary that embeds `gpui-agent`:

```toml
[profile.release]
lto = "thin"
codegen-units = 1
```

Expect a small serialize/parse win, not another 600×. Measure with
`cargo bench` on the real host tree size.

## What we did not touch

Virtual delivery, screenshots, recording, token/loopback gates, mailbox
depth, line/connection caps, OS HID (there is none).
