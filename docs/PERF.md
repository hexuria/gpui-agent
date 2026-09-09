# Performance notes (P0)

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

## Integrators

This workspace does not set `lto`. A product binary that embeds
`gpui-agent` may use thin LTO; expect a small serialize/parse win, not
another 600×. Measure with `cargo bench` on the real host tree size.
