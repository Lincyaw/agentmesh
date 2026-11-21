# One-command enrollment

On a machine already connected to your private relay, create a private bootstrap invitation:

```sh
agentmesh invite create --bootstrap --ttl-seconds 3600
```

On the new machine:

```sh
curl -fsSL https://raw.githubusercontent.com/Lincyaw/agentmesh/v0.5.0/scripts/install.sh | sh -s -- --join
```

Paste the returned `amb1_` code when prompted. For unattended installation, supply it
through `AGENTMESH_INVITE_CODE` or `--code-file /private/path/invitation.txt`.
The installer never accepts invitation secrets as process arguments.

The command downloads a platform-specific binary, checks its SHA-256, and then:

1. Configures Claude Code, tclaude, Codex, and tcodex, preserving existing providers
   and unrelated settings with private backups.
2. Configures the private relay and its authentication token, forcing relay-only
   transport with public discovery and public DNS fallback disabled.
3. Redeems the expiring, single-use invitation and establishes mutual membership.
4. Publishes `~/agentmesh-workspace` as `workspace` and grants read, write and execute
   access to the **inviting machine**, so its sessions can connect immediately.
5. Installs a user service where systemd or launchd is available. Containers without
   a user service manager retain automatic daemon startup when MCP connects.

Run any supported CLI normally afterward. Existing sessions need an MCP reconnect.
The installer configures harness integrations; it does not install or authenticate
the four model-provider CLIs themselves.

The inviter must remain online during redemption. Each new machine needs its own
invitation. Repeating installation on the same admitted machine is safe while the
code remains valid. A new identity cannot reuse it. Existing installations with a
different relay, credential, transport, or explicit name are preserved and rejected
with an actionable error; use a separate data directory to join another network.

The bootstrap code contains the shared relay credential. Keep it private and share
it only with machines you trust. The network admission is single-use and expires;
the relay's shared credential remains valid independently of that expiry. Public
peer tickets and membership synchronization never include that credential.

Commands run as the remote Unix user. The dedicated workspace is a file-tool scope,
not an OS sandbox for shell commands. Revoke a controller's access locally with:

```sh
agentmesh workspace revoke --name workspace --peer <inviting-machine-id>
```

## Relay DNS without an extra daemon

When the relay certificate hostname needs an explicit address, a daemon profile can use:

```json
{
  "name": "server",
  "relay_url": "https://relay.example.com:9443",
  "relay_token_file": "/private/path/relay-token",
  "relay_only": true,
  "relay_resolve": "192.0.2.10"
}
```

The binary resolves only that relay hostname to the pinned IP internally. TLS still
checks the original hostname. No Python DNS helper, `/etc/hosts` edit, public DNS
service or public discovery is needed. `relay_resolve` and `dns_server` are mutually
exclusive. Configure and restart the inviter with this profile before exporting a
portable bootstrap invitation; a local `dns_server` address cannot be shared with
another machine.

## Supported platforms and prerequisites

On headless Linux, a user service stays active after logout when user lingering is
enabled; otherwise it follows the OS user-session lifecycle. MCP can restart it
when a CLI connects.

The installer supports Linux x64/ARM64 and macOS ARM64. It needs a POSIX shell,
`curl`, `tar`, and `sha256sum` or `shasum`; no Rust or Python is required.
Use `--no-service` for isolated environments. The installer writes the CLI into
`~/.local/bin` and MCP configuration uses an absolute stable binary path.

The installation endpoint is a GitHub release download, separate from the private
iroh relay that carries agent traffic. A download checksum mismatch fails before
Agentmesh is installed or any harness configuration is changed.
