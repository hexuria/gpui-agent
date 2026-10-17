# Performance notes

P0 of the [no-brainer plan](NO_BRAINER_PLAN.md): session reuse, NDJSON
buffer reuse, tree flatten, mailbox `take`. Recipes reuse that session;
all-Read DAG waves may `rpc_pipeline` (write N lines, then read). Write /
Exit / mixed waves stay sequential.

Numbers: cloud VM, `x86_64`, `rustc 1.98.1`, Criterion `--quick`,
`cargo bench -p gpui-agent --bench agent_perf` (release). Median times
from this branch.

Security caps are unchanged: `MAX_LINE_BYTES = 1 MiB`,
`MAX_CONNECTIONS = 32`, `MAX_MAILBOX_DEPTH = 128`. No OS HID. No
`unsafe`.

```bash
cargo bench -p gpui-agent --bench agent_perf
```

## P0 measured (this branch)

| Bench | Median | Notes |
| --- | --- | --- |
| `rpc_once_reconnect_32_hellos` | **324 ms** | Historical per-op TCP connect |
| `rpc_session_reuse_32_hellos` | **536 µs** | **~605×** vs reconnect |
| `rpc_pipeline_32_hellos` | **145 µs** | **~3.7×** vs sequential session hellos |
| `handle_request_inprocess_32_hellos` | **1.88 µs** | CPU floor, no socket |
| `tree_flatten_naive_intermediate_vecs` | **5.29 µs** | Old per-child `Vec` (100-row tree) |
| `tree_flatten_capacity` | **1.06 µs** | **~5.0×** vs naive |
| `tree_flatten_into_reuse` | **994 ns** | `clear` keeps capacity |
| `mailbox_push_take_full` (128) | **22.4 µs** | `mem::take` |
| `snapshot_to_string` | **43.4 µs** | 100-row tree |
| `snapshot_write_json_line_reuse` | **43.4 µs** | Same ballpark; avoids the extra `String` |

Same shape as the experimental recipe branches (PR #4/#5). The headline
win is session reuse, not serialize.

## What P0 changed

- **`AgentClient::rpc`** keeps one TCP session. MCP already holds one
  client for the process, so `tools/call` no longer reconnects.
  **`rpc_once`** is the reconnect path for benches.
- **NDJSON helpers** (`write_json_line`, `read_limited_line_into`) reuse
  4 KiB encode/line buffers on client and server.
- **`UiTree::flatten_into`** visits once; no `node_count` pre-walk (that
  extra pass lost on #5).
- **Mailbox** stores `MailboxRequest` and `take`s with `mem::take`.
  Depth cap still 128.
- **todo `tree()`** pre-sizes the item list.

## Intentionally not in P0 / P1

| Technique | Where it was tried | Why it is not here |
| --- | --- | --- |
| `rpc_pipeline` writes | this branch / PR #5 | All-Read waves only. Write siblings stay fail-fast. Retry-after-write is Fatal (`4d464c7`). |
| simd-json | PR #5 | Hello parse **slower** than serde_json on tiny lines |
| tokio | PR #5 | Runtime build already ≈ one hello RTT |
| Scoped threads on the tree | PR #5 | 100-node count ~55× **worse** than sequential |
| Wire `batch` op | PR #5 | Protocol bump; pipeline of ordinary lines is enough later |
| mimalloc / compact_str / SmallVec / FxHashMap | PR #5 | JSON + loopback, not allocator- or hash-bound at todo size |

`UiNode` is **168 bytes** on 64-bit (`uinode_layout_stays_compact`).
`visible` fits in the existing bool padding before `Bounds`.

## Integrators

This workspace does not set `lto`. A product binary that embeds
`gpui-agent` may use thin LTO; expect a small serialize/parse win, not
another 600×. Measure with `cargo bench` on the real host tree size.

## M3 — snapshot revision cache (GATE-P)

`AgentHost::revision()` returning `Some(rev)` lets the server reuse the
last built `UiTree` while `rev` is unchanged. `wait_until` and `assert`
polls stop paying the rebuild entirely; mailbox `wait_until` posts a
cheap `revision` probe and only enqueues the assert after `rev` moves.
`None` (the default) keeps the old every-poll rebuild — always correct.

Measured: Apple Silicon, `cargo bench -p gpui-agent --bench agent_perf`,
1000-node tree (3001 nodes with children):

| Bench | Median | Notes |
| --- | --- | --- |
| `snapshot_rebuild_1000_nodes` | **436 µs** | Full `UiTree` build |
| `snapshot_cached_1000_nodes` | **10.5 ns** | Rev hit — **~41,000×** |
| `wait_until_poll_1000_nodes_uncached` | **450 µs** | Rebuild + assert per poll |
| `wait_until_poll_1000_nodes_cached` | **6.9 µs** | Assert only — **~64×** |
| `snapshot_compact_1000_nodes` | **179 µs** | Compact render (1 line/node) |

GATE-P verdicts:
- `wait_until` on an unchanged tree: **0 rebuilds** after the first
  (`wait_until_unchanged_tree_never_rebuilds` pins it — 45 ms of polling
  costs exactly one build).
- `snapshot {since}` at the current rev answers
  `{"unchanged":true}` — the whole response is **< 1 KiB**.
- `since` against a ring-cached rev answers a `diff` (`added` /
  `removed` / `changed`); past 8 revisions it degrades to the full tree
  with `{"full":true}` — never wrong, only bigger.

## M2–M8 — Instinct policy (GATE-R / GATE-L)

`gpui-agent-policy` is the opt-in local decision engine (`instinct`
feature; `jev` adds the optional System One fallback). Everything below
is in [docs/bench/RESULTS.md](bench/RESULTS.md).

| Number | Value |
| --- | --- |
| `resolve` decision latency (GATE-R bar) | **p50 46 µs** ≤ 50 µs |
| Resolve accuracy vs baseline | R1 wrong=0, pass 59.2% vs R0 44.8% |
| `run --goal` local arm (GATE-L, A2) | **16/16, wrong=0** (T09 refuse correct) |
| Escalated decision | one subprocess spawn (M7) or one System One POST (M8) **per abstain only** — a local choice costs zero model calls |

Determinism: the local policy reads no clocks, RNG, env, or unordered
maps in the decision path — same tree + same goal → same decision.
Abstain never becomes a dispatch.

LLM-arm comparisons (A0/A1 GATE-T, A3 GATE-E, A4) are pending owner
API keys; the wire and validation paths are covered by scripted
endpoints, not live models.
