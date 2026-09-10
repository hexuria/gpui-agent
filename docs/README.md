# Docs

Epic [#10](https://github.com/hexuria/gpui-agent/issues/10) (SDK, daemon, GUI-as-client, remote bind, packaging) landed on `main` in [#30](https://github.com/hexuria/gpui-agent/pull/30). Children: #12 daemon, #13 ADR, #14 remote, #15 packaging, #17 tree, #18 leftover-PR hygiene (#16 / #11 already closed by #30).

| Doc | What |
| --- | --- |
| [ADR-001-daemon-sot.md](ADR-001-daemon-sot.md) | Daemon is source of truth; GUI is a client |
| [SDK.md](SDK.md) | Embeddable SDK cookbook (`prelude`, `TestHost`, tree builders) |
| [INSTALL.md](INSTALL.md) | Install CLI + daemon; no GPUI |
| [SECURITY.md](SECURITY.md) | Trust model, caps, remote bind, recipes |
| [INTEGRATING.md](INTEGRATING.md) | `AgentHost` checklist |
| [PROTOCOL.md](PROTOCOL.md) | Wire format v1 |
| [RECORDING.md](RECORDING.md) | Screenshot backends (Mac window vs honest unavailable) |
| [RECIPES.md](RECIPES.md) | Experimental recipes |
| [TRY_ON_MAC.md](TRY_ON_MAC.md) | Laptop copy-paste |
| [PERF.md](PERF.md) | Session reuse numbers |
| [NO_BRAINER_PLAN.md](NO_BRAINER_PLAN.md) | P0–P5 roadmap |
| [STACK_HYGIENE.md](STACK_HYGIENE.md) | Leftover experiment PRs |
