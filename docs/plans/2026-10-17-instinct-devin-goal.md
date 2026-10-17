# Devin goal prompt: Instinct-backed gpui-agent

Paste everything inside the fence below as the Devin session goal. It's written
to run unattended from M0 through M8 and to survive session restarts.

````text
GOAL
Execute the PRD at docs/plans/2026-10-17-instinct-prd.md in hexuria/gpui-agent from
start to finish: milestones M0 → M8 (M8 optional, M9 out of scope), every task ID,
every gate. Ship it as reviewed, green PRs. The outcome: gpui-agent is measurably
faster and cheaper for LLM drivers, makes zero wrong actions from its local policy,
and still works exactly as before with no model and no network.

The PRD is the source of truth. Read it fully before any code, and re-read the
relevant milestone before each one. If this prompt and the PRD disagree, the PRD wins.

REPOS
- hexuria/gpui-agent: the product (read/write).
- hexuria/instinct: M0 only (read/write, PR only; never run `cargo publish`).
- hexuria/ultra-instinct: REFERENCE ONLY. Port code from the aui-* crates by
  reading them. Never add them as dependencies and never push there.
  Key files: crates/aui-policy/src/{instinct_policy.rs,evidence.rs,escalate.rs,
  text.rs,model_text.rs,goal.rs,remote.rs}, crates/aui-cli/src/typesafe.rs,
  crates/aui-observe/src/lib.rs (diff), bench/ (harness shape).
- codeitlikemiley/typesafe-sdk-rust: reference for M8 (System One client).

TRACKING (so nobody has to watch you)
- Each milestone has a GitHub issue "[instinct M<n>] …" in hexuria/gpui-agent with
  a task checklist. When you START a milestone, comment "Starting M<n>" with your
  plan (≤10 lines). When a task merges, tick its box. When a milestone is done,
  comment the gate result with numbers and a link to the bench artifact, then close
  the issue.
- After every merged PR, append a line to docs/bench/PROGRESS.md:
  `YYYY-MM-DD  M<n>.<k>  PR #<num>  <one-line outcome>`.
- RESUME PROTOCOL (a new or restarted session does this first): read the PRD, list
  the open "[instinct M*]" issues and their unchecked boxes, check open PRs on
  `instinct/*` branches, and continue from the first unfinished task. Never redo
  merged work.

WORK LOOP (every task; PRD §7 has the full commands)
1. Sync main, branch `instinct/m<N>-<slug>`.
2. Write the failing tests that encode the task's acceptance criteria first
   (behavior-level, public APIs, no test-only params in production code).
3. Implement the smallest change that passes. Match the surrounding code's style,
   naming and comment density. Conventional commits naming task IDs.
4. LOCAL GATE: run all of these and fix until every one passes:
     cargo fmt --all --check
     cargo clippy --workspace --all-targets --all-features -- -D warnings
     cargo test --workspace --all-features
     cargo test --workspace --no-default-features
     RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
     cargo audit
     ./scripts/ci-recipe.sh
   plus the milestone's bench and gate when it has one.
5. MUTATION GATE (M2 onward): cargo mutants on the crates and modules you changed.
   Kill every surviving mutant in decision, arbitration, evidence, fingerprint,
   diff, gate or escalation-validation code with a new test, or justify it in the
   PR body. Report the caught/total count in the PR.
6. SELF-REVIEW: diff your branch against main and walk PRD §8 line by line, plus:
   error paths tested? determinism (permute inputs → identical output)? any
   abstain that could reach a dispatch? any new dependency reaching
   crates/gpui-agent? any changed bytes for old requests (wire_compat)? Fix
   everything found, then go back to step 4.
7. PR: title with task IDs. Body: what changed, why; test evidence; gate numbers;
   mutation score; "Deviations from PRD" (or "none"); "Decisions I made" (each
   with a one-line reason).
8. CI FIX CYCLE: on red, read the full log, reproduce locally, fix the root
   cause, push. Repeat. Merge on green (squash) unless branch protection needs a
   human review; then leave it ready and continue with the next independent task.
   If the same CI failure survives 3 genuine fix attempts, apply the STUCK RULE.

NON-NEGOTIABLES (a violation means revert and redo)
- I1–I6 from PRD §3: no-model path unchanged, protocol additive (PROTOCOL_VERSION
  stays 2), no instinct/typesafe/tokio/HTTP deps in crates/gpui-agent, abstain is
  never dispatched, deterministic decisions, stale ≠ spent.
- Never skip, #[ignore], weaken or delete a test or assertion to get green.
  Never edit golden files just to match new output unless the PR explains why
  the old golden was wrong.
- Never lower the Instinct profile below Standard, or raise caps, to pass a gate.
- Never edit raw bench JSONL. Results come only from `bench report`.
- Never run `cargo publish`, `just publish`, or push tags other than `bench-baseline`.
- Never set confirm=true for destructive ops in act/runner code.
- No secrets in code, logs, fixtures or PR text. API keys come only from env/secrets.
- Bench LLM arms use exactly model `claude-haiku-5-5` with the PRD §5.2 settings
  on all arms. Use the official `anthropic` Python SDK; don't guess its API, read
  its docs. Spend cap for the whole goal: stop LLM runs and comment on the issue
  if cumulative bench spend reaches $25.

WHEN TO STOP AND ASK (otherwise decide yourself and log the decision in the PR)
Only for PRD §9 human steps:
- M0.4 crates.io publish: label the M0 issue `blocked: human-publish`, comment
  exactly what to run, then CONTINUE with M1 and M3 (and M2 against
  [patch.crates-io], unmerged) while waiting.
- Missing ANTHROPIC_API_KEY when an LLM arm must run: comment on the issue, and
  continue with every non-LLM task.
- A gate fails (PRD §5.5): iterate up to 3 design attempts, each recorded in
  RESULTS.md. GATE-T failing after that: STOP the goal (the PRD stop rule), open
  "[instinct] GATE-T failed — reassess" with data and hypotheses. Any other gate:
  open "[instinct] GATE-<X> failed" and continue only with tasks that don't
  depend on it.
STUCK RULE: if you can't make progress on a task after 3 genuine attempts (not
retries of the same thing), write up what you tried, the evidence and options as
an issue comment, label it `blocked: needs-owner`, and move to the next
independent task. Never sit idle and never thrash.

ORDER
M0 (PR to hexuria/instinct) → M1 → M3 (can run in parallel with M0's human wait)
→ M2 → M4 (GATE-T decides whether to continue) → M5 → M6 → M7 → M8 (optional;
the live run only if TYPESAFE_API_KEY exists).

FINISH
When M7 is closed (and M8 closed or skipped):
1. Update README.md, docs/PROTOCOL.md, docs/INTEGRATING.md, docs/PERF.md and
   docs/RECIPES.md per PRD §8.
2. Final docs/bench/RESULTS.md: baseline vs final for every arm, every gate's
   verdict, USD per passed task, and an honest "what didn't improve" section.
3. Open a final issue "[instinct] Done — release checklist" listing the human
   release steps (publish gpui-agent-policy, `just publish`).
4. Post a summary comment on it: gates, headline numbers (tokens, wall time, pass,
   wrong actions vs baseline), and known limitations.
Done means every non-optional task is ticked, every gate is recorded, CI on main
is green, and the release checklist issue exists.
````

## Notes for the owner
- Before starting Devin: merge the PRD and this file to `main`, add the
  `ANTHROPIC_API_KEY` repo secret, and give Devin access to the four repos listed.
- Devin pauses only for the PRD §9 human steps. Expect `blocked: human-publish`
  on the M0 issue early. Publish the three instinct crates when you see it.
