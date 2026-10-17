# Progress log

One line per merged PR: `YYYY-MM-DD  M<n>.<k>  PR #<num>  <outcome>`.

2026-10-17  M1.2-M1.8  PR #61  bench harness + wire-compat + deps-guard merged; R0 baseline in RESULTS.md
2026-10-17  M3.1-M3.8  PR #62  revision cache + since/diff/compact; GATE-P green (poll 450µs→6.9µs)
2026-10-17  M2.1-M2.5  PR #63  gpui-agent-policy (instinct evidence + arbitration) + opt-in `instinct` resolver; GATE-R: R1 wrong=0, pass 59.2% ≥ R0 44.8%, p50 46µs ≤ 50µs.
2026-10-17  M4.1-M4.6  PR #64  ActionSpace + UiPolicy + TextResolver + locate/act/compact MCP; GATE-T blocked (no ANTHROPIC_API_KEY); T08/T09/T10/T14 + todo-headless MCP tests green.
2026-10-17  M5  PR #65  UiNode::fingerprint (FNV-1a, bounds/focus excluded) + expect_fingerprint atomic dispatch gate; act retries stale ≤2; race test green: stale → 0 dispatches.
2026-10-17  M6  —  run --goal runner + clause model + evidence channels; GATE-L: A2 16/16 wrong=0 (T09 refuse correct); A3 blocked on ANTHROPIC_API_KEY
