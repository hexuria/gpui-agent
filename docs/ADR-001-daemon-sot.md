# ADR-001: Daemon is source of truth; GUI is a client

- **Status:** Accepted
- **Date:** 2026-09-09
- **Issue:** [#13](https://github.com/hexuria/gpui-agent/issues/13)
- **Related:** [#10](https://github.com/hexuria/gpui-agent/issues/10) (epic), [#12](https://github.com/hexuria/gpui-agent/issues/12) (daemon), [#14](https://github.com/hexuria/gpui-agent/issues/14) (remote bind)

## Context

The lab has two hosts that both implement `AgentHost` and own a `TodoStore`:

- `apps/todo` — GPUI Kit window with an in-process mailbox
- `apps/todo-headless` — no GPU, same domain logic over TCP

If a human keeps the Mac GUI open while an agent drives the headless daemon, the two stores diverge. Issue #13 asked which sync model matches product reality (OpenGrok / Grok Bot: agent machines run logic; humans may still have a window open).

Options considered:

1. **Single process** — only GUI or only daemon. Simplest; weakens the agent-machine story.
2. **Shared persistence** — SQLite/file both watch. Two writers, schema and lock work, still two hosts.
3. **Daemon is source of truth** — GUI is a protocol client of the daemon.
4. **Bridge** — daemon forwards to the GUI loopback port when a window is detected. Two stores plus a hidden hop.
5. **CRDT / event log** — only if multi-device merge is a hard requirement. It is not.

## Decision

**Option 3.** The headless logic daemon owns domain state and the agent protocol endpoint that agents talk to. The Mac GPUI app does **not** own a competing store for product / agent-machine writes. It connects to the daemon (loopback by default, or an authenticated remote channel — see #14) and renders from snapshots.

When an agent mutates via CLI or recipes against the daemon, an open GUI updates because it is a client of that same source of truth — not because we bridge into a second in-process `AgentHost` store.

## What stays in-process

`AgentHost` inside a GUI (mailbox drain on the UI thread) remains the path for **SDK / widget E2E**: tree ids, virtual delivery, painted bounds. That path must not become a second product mutation store. Use a feature flag or a separate test binary when both are needed.

Product mutations for agent-machine workflows go through the daemon.

## Protocol implications (v1)

- No event-bus / push protocol bump in this ADR.
- GUI v1 is **snapshot poll** plus ordinary ops (`click`, `set_value`, `invoke`, …) over `AgentClient`.
- Screenshot stays honest: headless / daemon remain `screenshot_unavailable`. A real Mac window PNG exists only on the in-process **embedded-host** mailbox path (`screencapture -l` of that window; [#20](https://github.com/hexuria/gpui-agent/pull/20)). The default GUI client does not host the agent port, so it cannot serve a PNG.
- Caps, no OS HID, semantic default, no wire `batch` — unchanged.

## Consequences

- `apps/todo-headless` (or its successor daemon CLI) is the SoT binary agents install.
- `apps/todo` must stop treating its local `TodoStore` as the product store once the GUI-as-client slice lands. Until that slice, two stores remain a known lie — do not paper over it with a silent bridge.
- Remote bind (#14) is a separate decision: authenticated opt-in, fail closed, plaintext TCP+token is lab-only until TLS/SSH/mTLS is specified.
- In-process `spawn_host` / `spawn_mailbox` stay in the SDK for tests and embedders who are not running the product daemon.
