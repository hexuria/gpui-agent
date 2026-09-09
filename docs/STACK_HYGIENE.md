# Stack hygiene (P5)

History-only note. **Do not merge** [PR #4](https://github.com/hexuria/gpui-agent/pull/4)
or [PR #5](https://github.com/hexuria/gpui-agent/pull/5) onto `main`. No
force-push to `main`. No HID / cap / token regressions.

The experimental stack was landed as smaller PRs instead of those two
branches. Remote heads stay as a museum; they are not deleted.

## What landed vs leftover

| Phase | What | Where |
| --- | --- | --- |
| **P0** | Session reuse, NDJSON buffer reuse, flatten, mailbox `mem::take` | **Merged** [#6](https://github.com/hexuria/gpui-agent/pull/6) |
| **P1** | Recipes (JSON canonical, `.wants` alias), sequential session `rpc`, honest `screenshot` op + `--screenshot-dir` | **Merged** [#9](https://github.com/hexuria/gpui-agent/pull/9) |
| **P2** | Token required for CLI `recipe run` and `mcp` only (same token on host) | **Merged** [#19](https://github.com/hexuria/gpui-agent/pull/19) |
| **P3** | macOS app-window PNG via `screencapture -l`; headless stays `screenshot_unavailable` | **Open** [#20](https://github.com/hexuria/gpui-agent/pull/20) — keep |
| **P4** | CI: headless recipe receipt `"ok"` + `session_reused` on `ubuntu-latest` | **Open** [#21](https://github.com/hexuria/gpui-agent/pull/21) — keep |
| **P5** | This note + close leftover #4/#5 without merge | This PR |

Earlier merged work that is not a no-brainer phase: [#1](https://github.com/hexuria/gpui-agent/pull/1)
(framework-agnostic CLI/MCP), [#2](https://github.com/hexuria/gpui-agent/pull/2)
(virtual input, no OS HID), [#3](https://github.com/hexuria/gpui-agent/pull/3)
(security audit / caps).

## Closed without merging

| PR | Why it must not merge | Disposition |
| --- | --- | --- |
| [#4](https://github.com/hexuria/gpui-agent/pull/4) `[experimental] Recipes, TMP-inspired mapping, and session reuse` | Base is old `main` (`b50fb8a`). Replays P0 session reuse plus recipes, screenshot plumbing, and recording experiments that were sliced into #6 / #9 / later PRs. `mergeable_state` was dirty vs current `main`. | **Closed without merge.** Branch `gol/recipes-tmp-perf-e79b` remains museum. |
| [#5](https://github.com/hexuria/gpui-agent/pull/5) Measured hot-path wins (stacked on #4) | Stacked on #4. Adds `rpc_pipeline` plus rejected simd/tokio/mimalloc/wire `batch`. Already said do not merge #4 or this PR. | **Closed without merge.** Branch `gol/recipes-measured-perf-762f` remains museum. |
| [#7](https://github.com/hexuria/gpui-agent/pull/7) stacked P1 | Superseded by #9. | Already closed. |
| [#8](https://github.com/hexuria/gpui-agent/pull/8) Policy D token (mint + every CLI op) | Superseded by #19. Do not revive mint / `--allow-empty-token`. | Already closed. |

## What was duplicated on #4/#5 and dropped from `main`

Do **not** cherry-pick or rebase these branches onto current `main`.
Duplicate work that already landed (or is in flight as #20/#21):

- Session reuse, NDJSON line buffers, flatten, mailbox `mem::take` (P0 / #6)
- Recipes, TMP-shaped schemas, sequential `rpc` on the kept session (P1 / #9)
- Protocol `screenshot` plumbing / `--screenshot-dir` (P1 / #9); Mac window PNG (P3 / #20)
- Token for `recipe run` / `mcp` (P2 / #19) — not the #8 mint path
- CI recipe receipt assert (P4 / #21)

Also do **not** replay from #4/#5:

- `--record` / ffmpeg / ScreenCaptureKit
- simd-json, tokio, scoped tree threads, mimalloc, compact_str, SmallVec, FxHashMap
- Wire `batch` op

If a leftover experiment is wanted later: **new branch from current
`main`**, re-implement only the not-yet-landed piece. Drop duplicate
client / ndjson / flatten / mailbox.

## Later open PRs (verified 2026-09-09 against GitHub; not this branch)

These landed as separate drafts on current `main` after this inventory
was first written. Keep them; do not fold them into P5.

| PR | Title | Disposition |
| --- | --- | --- |
| [#23](https://github.com/hexuria/gpui-agent/pull/23) | Pipeline all-Read recipe waves; keep writes sequential | **Open draft** (`gol/read-wave-pipeline-b20f`). This is the `rpc_pipeline` follow-on (include `4d464c7` retry-after-write + sibling receipts). |
| [#24](https://github.com/hexuria/gpui-agent/pull/24) | MCP `isError` on failed `recipe_run`; `$params` do not rewrite `id`/`needs` | **Open draft** (`gol/mcp-iserror-params-b20f`). This is the P1 review leftover. |

GitHub facts for the closed leftovers (same day this note was written):

- [#4](https://github.com/hexuria/gpui-agent/pull/4): `state=closed`, `merged=false`, `closed_at=2026-09-09T12:27:03Z`, base still old `main` `b50fb8a`, `mergeable_state=dirty`
- [#5](https://github.com/hexuria/gpui-agent/pull/5): `state=closed`, `merged=false`, `closed_at=2026-09-09T12:27:03Z`, stacked on #4
- [#7](https://github.com/hexuria/gpui-agent/pull/7): already closed (`closed_at=2026-09-09T10:50:38Z`), superseded by #9
- [#8](https://github.com/hexuria/gpui-agent/pull/8): already closed (`closed_at=2026-09-09T11:10:49Z`), superseded by #19

Museum remotes still present: `gol/recipes-tmp-perf-e79b`, `gol/recipes-measured-perf-762f`.

## Optional later (document only; not this PR)

- Write-wave `rpc_pipeline` / simd / tokio / mimalloc / wire `batch` remain rejected.
- `--record` / ffmpeg / ScreenCaptureKit remain later.

## Constraints (unchanged)

No OS HID. Loopback + caps (`GPUI_AGENT=1`, 1 MiB line, 32 conn, 128
mailbox, 30s idle). Semantic default. Token policy is P2 as merged in
#19 — do not re-add ephemeral mint unless asked.
