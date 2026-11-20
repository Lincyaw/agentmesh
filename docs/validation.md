# Validation record

Validated on 20 November 2026 with macOS arm64, Rust, iroh 1.5.0, rmcp 3.11.0, SQLite, and Tokio.

| Check | Result | Evidence |
| --- | --- | --- |
| Rust build and Clippy | Passed | Locked debug/release builds; Clippy with warnings denied. |
| Rust requirement regressions | 5 passed | `cargo test --locked`. |
| Independent iroh nodes and MCP sessions | 7 passed | `python3 tests/test_interop.py`. |
| CLI contracts and relay-token handling | 3 passed | `python3 tests/test_cli.py`. |
| Configuration merging and daemon lifecycle | 5 passed | `python3 tests/test_setup.py`. |
| Invitation enrollment and member synchronization | 2 passed | `python3 tests/test_invites.py`. |
| Native thread isolation | 1 passed | `python3 tests/test_auto_binding.py`; all 16 tools reject mismatched or missing thread metadata after binding. |
| Public relay-only transport | Passed | Disabled IP transports, then exchanged messages, replies, groups, and restart deliveries through a public iroh relay. |
| Official Claude Code | MCP connection passed | Claude Code 2.1.328 reported Connected. |
| tclaude | Automatic binding and idle model exchange passed | tclaude 0.1.10 / Claude Code 2.1.303: read, reply, and acknowledgement using setup-generated communication permissions. |
| Official Codex | Automatic binding and idle model exchange passed | Codex 0.169.0-alpha.11.1: two real CLI sessions, target read/reply/ack, other inbox empty. |
| tcodex | Automatic binding, MCP exchange, and queue submission passed | tcodex 0.1.4 / Codex 0.154.0; the configured model returned upstream HTTP 429 in the model-level test. |

## Remote workspace validation (v0.4)

The remote workspace suite passes 10 end-to-end checks using independent iroh nodes
and actual MCP subprocesses:

- Explicit default-deny grants, read/write/execute separation, and cross-machine job ownership.
- All four harness labels expose the same tools and read remote files.
- Unicode text, atomic create/replace, stale-hash rejection, literal search and directory listing.
- Parent traversal, absolute paths, symlinks, special files, oversized text and invalid UTF-8 rejection.
- Lost submission responses and matching retries execute exactly once; conflicting IDs fail.
- Asynchronous status, stdout/stderr cursors, disconnect/reconnect and nonzero exit codes.
- Timeout, cancellation, revocation and process-group cleanup.
- Per-stream output caps, bounded frames, and malformed cursor rejection.
- Restart persistence and interrupted jobs without replay.
- Detached descendants cannot pin pipe capture; UTF-8 survives page and live-write boundaries.

The existing 23 regression checks and one transport privacy check also pass, for **34 checks total**. Formatting,
Clippy with warnings denied, and the optimized macOS build pass. Independent review
found and verified fixes for the inherited-pipe and UTF-8 pagination cases, then
approved the change. Native thread fencing now covers all 16 tools.

These harness-label tests exercise real MCP processes, rather than four separate
model-generation checks. The earlier model validation below applies to messaging.

## Private transport (v0.4)

A real local DNS responder records zero queries for an endpoint using the default
configuration. With an explicitly configured test relay, it records only that relay's
hostname, even when peer tickets and invitations advertise unapproved domain or
literal-IP relay URLs. The endpoint uses the Minimal preset, no address lookup services, and no
public relay map; its DNS resolver disables public fallback nameservers.
`python3 tests/test_transport.py` checks those observable DNS behavior boundaries.

A locally hosted iroh-relay 1.5.0 server binds only to loopback and requires a random
shared token. Both Agentmesh endpoints use `--relay-only`, with all direct IP
transports disabled. The invitation, remote-workspace and messaging suites passed all 19 cases
through that relay, and an incorrect-token rejection check passed, for 20 checks. This exercises a real
relay process and the same seven-plus-nine MCP tools; it does not claim a deployment
across separate physical machines. Reproduce with:

```sh
python3 scripts/check_private_relay.py --relay-binary /absolute/path/to/iroh-relay
```

The loopback-only fixture uses HTTP dev mode for the outer transport. Iroh peer
traffic remains encrypted end to end; deployed relay connections retain normal TLS
certificate verification. Test credentials and relay data are temporary and removed
when the runner exits.

The public-relay result below is historical evidence from v0.3, before this private-only
policy. No public relay is used in v0.4 validation.

## Transport and persistence

The integration suite starts real daemons with separate data directories, private keys, UDP ports, and SQLite databases.
Each daemon connects to independent MCP stdio processes.
The tests use actual iroh QUIC and MCP rather than an in-memory transport.

Coverage includes mutual-trust enforcement, forged-sender rejection, Unicode messages, replies, acknowledgements, local and remote group recipients, duplicate prevention after a lost receipt, and persistent outbox delivery after restarts.
Sessions recover after a 46-second daemon outage exceeding the 45-second lease.
Old bridge processes cannot remove a successor's execution credentials.
Large inboxes, groups, and directories are paginated within byte budgets.
Discovery preserves local results when many remote peers are unreachable.

Historical v0.3 relay-only checks disabled both endpoints' IP transports and passed
through a public relay. Those checks were performed before the private-only v0.4
transport policy and are not repeated for this version.

An optional self-hosted relay was previously inspected for TLS and token-based
authentication. The supplied host became unreachable during isolated offline build preparation;
SSH and relay TLS handshakes time out. Work on that host was stopped at the user's
request; current forced-relay checks use a locally hosted relay. No operator credentials or deployment inventory
are part of this repository. Linux release checks run in the release workflow; consult
its actual result for a published tag.

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
