# Progress log

One line per merged PR: `YYYY-MM-DD  M<n>.<k>  PR #<num>  <outcome>`.

2026-10-17  M1.2-M1.8  PR #61  bench harness + wire-compat + deps-guard merged; R0 baseline in RESULTS.md
2026-10-17  M3.1-M3.8  PR #62  revision cache + since/diff/compact; GATE-P green (poll 450µs→6.9µs)
2026-10-17  M2.1-M2.5  PR #63  gpui-agent-policy (instinct evidence + arbitration) + opt-in `instinct` resolver; GATE-R: R1 wrong=0, pass 59.2% ≥ R0 44.8%, p50 46µs ≤ 50µs.
2026-10-17  M4.1-M4.6  PR #64  ActionSpace + UiPolicy + TextResolver + locate/act/compact MCP; GATE-T blocked (no ANTHROPIC_API_KEY); T08/T09/T10/T14 + todo-headless MCP tests green.
2026-10-17  M5  PR #65  UiNode::fingerprint (FNV-1a, bounds/focus excluded) + expect_fingerprint atomic dispatch gate; act retries stale ≤2; race test green: stale → 0 dispatches.
2026-10-17  M6  —  run --goal runner + clause model + evidence channels; GATE-L: A2 16/16 wrong=0 (T09 refuse correct); A3 blocked on ANTHROPIC_API_KEY
2026-10-17  M7  —  EscalatingPolicy + CommandEscalation (closed wire, 30s/64KiB caps, off-menu→blocked, dangerous→needs_confirmation, text tier on Missing); A2 still 16/16; GATE-E blocked on ANTHROPIC_API_KEY
2026-10-17  M8  —  optional `jev` feature: JevEscalation (aui typesafe.rs port) behind EscalatingPolicy, --escalate jev; fail-closed System One validation; 7 unit + 7 integration tests; A4 not run (no TYPESAFE_API_KEY)
2026-10-19  gates —  live runs with owner-provided keys: A0 baseline 65/80 ($0.396, 13.4k in_tok p50); A1 65/80 wrong=0 → GATE-T PASS (4,550 in_tok p50 = 34% of A0); A3 80/80 wrong=0, 61ms p50, $0.0000306/passed → GATE-E PASS; A4 75/80 — Jev coin-flipped T08's identical-row menu (5 wrong) → ambiguity guard added: identical (kind,label) contenders short-circuit before any endpoint → a4+guard T08 5/5 wrong=0. Harness fixes: v2 challenge/HMAC auth, pydantic tool-use ser, explicit api_key, report skips side-files. Total LLM spend ≈ $0.51.
