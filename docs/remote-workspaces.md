# Remote workspaces

Agentmesh lets a local Claude Code or Codex session operate a workspace on another
enrolled machine. The remote machine runs Agentmesh, without a model or harness.
Tools travel through the existing authenticated iroh connection. Configure your own
relay for networks that require one; public relays and public node discovery are disabled. Native harness
file and shell tools still operate locally; use the `remote_*` MCP tools for remote work.

## Publish and authorize

On the server, publish an existing directory, then authorize a paired machine:

```sh
agentmesh workspace add --name training --path /srv/training
agentmesh peer list
agentmesh workspace grant --name training --peer laptop --write --execute
```

`--peer` accepts an exact, unambiguous machine name or endpoint ID. A grant always
allows reading. `--write` permits file replacement; `--execute` permits shell commands
with the daemon account's permissions. Omit these flags for read-only access.
Repeating `grant` replaces that machine's permissions, so it can also downgrade access.
There are no wildcard grants. Invitations and directory discovery do not grant execution.

```sh
agentmesh workspace list
agentmesh workspace revoke --name training --peer laptop
agentmesh workspace remove --name training
```

Revocation and removal cancel active jobs in the affected scope and reject subsequent
requests. They do not undo changes made by commands. File tools walk relative paths
using directory descriptors and reject symlinks and parent traversal. Shell access
is account access: a working directory is **not** an OS sandbox. Use a dedicated
Unix account or container when a process must be isolated from the rest of the host.

## Use from an agent

Run `agentmesh setup` with the new binary and reconnect existing MCP sessions.
The same tools are available to `claude`, `tclaude`, `codex`, and `tcodex`.
Setup continues to grant automatic use only to the seven communication tools;
new remote tools retain the harness's normal approval policy.

Ask your agent to discover the remote workspaces, inspect files, and run a command.
The MCP tool descriptions explain arguments and limits. Requests identify a machine
and workspace explicitly, so local and remote operations can coexist in one session.

| MCP tool | Operation |
| --- | --- |
| `remote_workspaces` | Discover authorized workspaces on one or all machines. |
| `remote_list_files` | List a workspace directory. |
| `remote_read_file` | Read text and its SHA-256. |
| `remote_write_file` | Create or replace text using the previous hash. |
| `remote_search` | Search for literal text. |
| `remote_execute` | Submit a shell command with a stable request ID. |
| `remote_job` | Read status and output from byte cursors. |
| `remote_jobs` | List this machine's jobs in a workspace. |
| `remote_cancel_job` | Cancel a job owned by this machine. |

The CLI exposes the same operations, with JSON on stdout:

```sh
agentmesh remote list
agentmesh remote read --node server --workspace training --path config.yaml
agentmesh remote execute --node server --workspace training \
  --command 'python3 -m unittest' --request-id test-run-001
agentmesh remote job --node server --job <returned-job-id>
agentmesh remote cancel --node server --job <returned-job-id>
```

Use the same request ID and arguments if command submission returns an uncertain
network error. Jobs belong to the originating machine and can be queried by another
session on that machine. The server owner can inspect or cancel any current workspace
job using `--node local`. Run `agentmesh setup` on both sides when upgrading from
v0.3; a v0.3 peer still supports messaging but cannot handle workspace operations.

## Execution contract

Command submission requires a caller-chosen `request_id`. Retrying the same ID with
the same workspace, command, directory and timeout returns the original job. Reusing
an ID for different inputs fails. The authenticated caller machine owns its jobs;
another machine cannot query or cancel them through its own grant.

Jobs return immediately. Poll their status and stdout/stderr using independent byte
cursors. Output is persisted on the executor, survives client disconnects, and is
bounded to 1 MiB per stream. A read returns at most 64 KiB per stream. Text is UTF-8;
invalid output bytes are replaced only in the returned text, while cursors count raw
bytes. Truncation is reported explicitly. Jobs default to a five-minute timeout,
with a maximum of 24 hours. At most eight jobs run concurrently and 256 records are
retained; an administrator can prune finished records with `agentmesh workspace prune`.

A daemon restart marks unfinished jobs `interrupted`, never resubmits commands, and
preserves their captured output. A hard crash can leave external side effects or
descendants: inspect the remote environment before submitting a new request ID.
Graceful shutdown, cancellation and timeouts kill the job's process group.
If a descendant escapes that group and retains an output pipe, capture stops after
a one-second drain grace period and reports incomplete output. OS-level isolation
is required to prevent processes from escaping their group.

File reads and writes are UTF-8 text, limited to 64 KiB. Replacing an existing file
requires its `expected_sha256` from a read; an omitted hash means create-only.
Repeating a write whose desired content is already present succeeds without rewriting.
Writes replace a file atomically and do not create parent directories.
Search is literal, bounded, and reports whether results were truncated.

The executor starts `/bin/sh -c` with a small environment whitelist; it does not
forward model credentials, SSH agents, or arbitrary daemon environment variables.
Use explicit shell initialization for project environments such as Conda.
Workspace permissions, request identities, commands, completion status and job metadata
are stored privately on the executor. Treat captured output as potentially sensitive.

This release provides MCP tools, not native Codex exec-server remapping, interactive
PTYs, port forwarding, or cross-machine credential synchronization.
