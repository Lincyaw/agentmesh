# Validation record

Validated on 20 November 2026 with macOS arm64, Rust, iroh 1.5.0, rmcp 3.11.0, SQLite, and Tokio.

| Check | Result | Evidence |
| --- | --- | --- |
| Rust build and Clippy | Passed | Locked debug/release builds; Clippy with warnings denied. |
| Rust requirement regressions | 5 passed | `cargo test --locked`. |
| Independent iroh nodes and MCP sessions | 7 passed | `python3 tests/test_interop.py`. |
| CLI contracts and relay-token handling | 3 passed | `python3 tests/test_cli.py`. |
| Configuration merging and daemon lifecycle | 4 passed | `python3 tests/test_setup.py`. |
| Invitation enrollment and member synchronization | 2 passed | `python3 tests/test_invites.py`. |
| Native thread isolation | 1 passed | `python3 tests/test_auto_binding.py`; all seven tools reject mismatched or missing thread metadata after binding. |
| Public relay-only transport | Passed | Disabled IP transports, then exchanged messages, replies, groups, and restart deliveries through a public iroh relay. |
| Official Claude Code | MCP connection passed | Claude Code 2.1.328 reported Connected. |
| tclaude | Automatic binding and idle model exchange passed | tclaude 0.1.10 / Claude Code 2.1.303: read, reply, and acknowledgement using setup-generated communication permissions. |
| Official Codex | Automatic binding and idle model exchange passed | Codex 0.169.0-alpha.11.1: two real CLI sessions, target read/reply/ack, other inbox empty. |
| tcodex | Automatic binding, MCP exchange, and queue submission passed | tcodex 0.1.4 / Codex 0.154.0; the configured model returned upstream HTTP 429 in the model-level test. |

## Transport and persistence

The integration suite starts real daemons with separate data directories, private keys, UDP ports, and SQLite databases.
Each daemon connects to independent MCP stdio processes.
The tests use actual iroh QUIC and MCP rather than an in-memory transport.

Coverage includes mutual-trust enforcement, forged-sender rejection, Unicode messages, replies, acknowledgements, local and remote group recipients, duplicate prevention after a lost receipt, and persistent outbox delivery after restarts.
Sessions recover after a 46-second daemon outage exceeding the 45-second lease.
Old bridge processes cannot remove a successor's execution credentials.
Large inboxes, groups, and directories are paginated within byte budgets.
Discovery preserves local results when many remote peers are unreachable.

Relay-only verification disables both endpoints' IP transports:

```sh
AGENTMESH_RELAY_URL=https://use1-1.relay.n0.iroh.link \
AGENTMESH_RELAY_ONLY=1 \
python3 tests/test_interop.py Interoperability.test_iroh_mcp_direct_reply_groups_and_restart
```

These checks use independent processes on one physical host and a public relay.
They do not constitute a deployment across separate physical machines.
Linux release builds and native regression checks run in the release workflow; consult its actual result for a published tag.

An optional self-hosted relay was inspected for TLS and token-based authentication.
Its end-to-end transport check was blocked by hostname resolution, so that custom deployment is not claimed as verified.
No operator credentials or deployment inventory are part of this repository.

## Invitations and native attachment

Invitation checks cover expiration, invalid secrets, recovery by the same admitted machine, rejection of another redemption, automatic fleet synchronization, and communication after the original inviter goes offline.
Independent review also tested concurrent redemption: exactly one machine succeeded, the other was rejected, and the admitted machine could retry.
Changing the expiration inside the code could not bypass the inviting daemon's persisted expiration.

Claude bridges obtain their native session ID, socket, and token directly from their harness environment.
Codex's SessionStart hook calls the owning MCP bridge with native thread metadata on the first turn.
Every subsequent tool call is fenced to that thread.
Normal Codex CLI reminders target its shared daemon; queue acceptance for an independent stdio App Server does not establish that the separate process will wake.

`tests/live_codex_model.py` opens two real CLI terminals, waits for initial turns, sends a random message from another iroh node, and checks body, sender, conversation, reply reference, acknowledgement, and the other session's empty inbox.
`tests/live_claude_model.py` checks the equivalent idle exchange using native Claude inbox delivery, without manual attachment or a test-only tool allowlist.

TCodex's MCP and adapter checks passed independently of model generation.
The user's configured model returned HTTP 429, preventing the model-level acceptance check.
An earlier isolated run used the wrapper's alternative default model and returned unsupported tool calls; that run is not evidence for the configured model.

## Installation and review

The installed configuration was read back through all four CLIs.
Claude and tclaude reported Connected; Codex and tcodex reported their MCP entry enabled.
Setup preserves existing providers, other MCP servers, explicit tool approval policies, Claude deny/ask rules, and inbound holds.
Only the seven Agentmesh communication tools and the specific Codex identity hook receive new default grants.

Daemon upgrades are serialized with a private startup lock.
Configuration, relay-token files, and executable preparation are checked before stopping an older daemon.
Independent review tested malformed JSON, a missing name, an invalid bind address, invalid or conflicting flags, and a non-executable replacement: the old daemon kept its PID and continued responding.
Three concurrent MCP clients also successfully upgraded a v0.2 daemon and registered separate sessions.

Final independent code review: APPROVE, with no remaining blocker or major finding.
