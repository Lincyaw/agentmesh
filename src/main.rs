mod daemon;
mod execution;
mod local;
mod mcp;
mod network;
mod onboard;
mod protocol;
mod relay_dns;
mod setup;
mod store;
mod user_service;
mod wake;
mod workspace_files;

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use protocol::*;
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Peer-to-peer mailboxes for Claude Code and Codex CLI sessions"
)]
struct Cli {
    #[arg(long, global = true, env = "AGENTMESH_DATA_DIR")]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Publish directories and manage access on this machine.
    #[command(subcommand)]
    Workspace(WorkspaceCommand),
    /// Operate an authorized workspace over iroh. All output is JSON.
    #[command(subcommand)]
    Remote(RemoteCommand),
    #[command(subcommand)]
    Invite(InviteCommand),
    /// Join using a private invitation read from stdin, a file, or AGENTMESH_INVITE_CODE.
    Join {
        #[arg(long)]
        code_file: Option<PathBuf>,
        #[arg(long)]
        config_root: Option<PathBuf>,
        #[arg(long)]
        name: Option<String>,
        /// Skip OS user-service installation when redeeming a bootstrap invitation.
        #[arg(long)]
        no_service: bool,
    },
    /// Configure the shared Claude/tclaude and Codex/tcodex MCP entries.
    Setup {
        #[arg(long)]
        config_root: Option<PathBuf>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        config_only: bool,
    },
    #[command(subcommand)]
    Relay(RelayCommand),
    #[command(subcommand)]
    Daemon(Daemon),
    #[command(subcommand)]
    Peer(Peer),
    #[command(subcommand)]
    Session(SessionCommand),
    #[command(subcommand)]
    Mcp(Mcp),
    #[command(subcommand)]
    Message(MessageCommand),
    #[command(subcommand)]
    Group(GroupCommand),
    /// Print a harness MCP configuration without modifying existing settings.
    Config {
        #[arg(long,value_parser=["claude","tclaude","codex","tcodex","generic"])]
        harness: String,
    },
    Status,
}

#[derive(Subcommand)]
enum WorkspaceCommand {
    /// Publish an existing directory without granting remote access.
    Add {
        #[arg(long)]
        name: String,
        #[arg(long)]
        path: PathBuf,
    },
    /// List locally published workspaces and grants.
    List,
    /// Replace a paired machine's grant; reading is always included.
    Grant {
        #[arg(long)]
        name: String,
        #[arg(long)]
        peer: String,
        #[arg(long)]
        write: bool,
        /// Allow commands with this daemon account's permissions (not sandboxed).
        #[arg(long)]
        execute: bool,
    },
    /// Revoke access and cancel this machine's active jobs in the workspace.
    Revoke {
        #[arg(long)]
        name: String,
        #[arg(long)]
        peer: String,
    },
    /// Unpublish a workspace and cancel active jobs; leaves workspace files intact.
    Remove {
        #[arg(long)]
        name: String,
    },
    /// Delete completed job records and output. Removed request IDs can run again.
    Prune,
}

#[derive(Subcommand)]
enum RemoteCommand {
    /// Discover authorized workspaces. Omit --node to query all paired machines.
    List {
        #[arg(long)]
        node: Option<String>,
    },
    /// List up to 200 entries without following symlinks.
    Files {
        #[arg(long)]
        node: String,
        #[arg(long)]
        workspace: String,
        #[arg(long, default_value = ".")]
        path: String,
    },
    /// Read a UTF-8 file (64 KiB maximum), including its SHA-256.
    Read {
        #[arg(long)]
        node: String,
        #[arg(long)]
        workspace: String,
        #[arg(long)]
        path: String,
    },
    /// Atomically create or replace a UTF-8 file. Read first to obtain its hash.
    Write {
        #[arg(long)]
        node: String,
        #[arg(long)]
        workspace: String,
        #[arg(long)]
        path: String,
        #[arg(long)]
        content_file: PathBuf,
        #[arg(long)]
        expected_sha256: Option<String>,
    },
    /// Search UTF-8 files for literal text, with bounded results.
    Search {
        #[arg(long)]
        node: String,
        #[arg(long)]
        workspace: String,
        #[arg(long, default_value = ".")]
        path: String,
        #[arg(long)]
        query: String,
    },
    /// Start a job; retry the SAME request ID after an uncertain response.
    Execute {
        #[arg(long)]
        node: String,
        #[arg(long)]
        workspace: String,
        #[arg(long, default_value = ".")]
        path: String,
        #[arg(long)]
        command: String,
        #[arg(long)]
        request_id: String,
        #[arg(long, default_value_t = 300)]
        timeout_seconds: u64,
    },
    /// Read job status and output from byte cursors. Use --node local for administration.
    Job {
        #[arg(long)]
        node: String,
        #[arg(long)]
        job: String,
        #[arg(long, default_value_t = 0)]
        stdout_cursor: u64,
        #[arg(long, default_value_t = 0)]
        stderr_cursor: u64,
    },
    /// List the most recent 100 jobs belonging to this machine.
    Jobs {
        #[arg(long)]
        node: String,
        #[arg(long)]
        workspace: String,
    },
    /// Cancel a job and kill its process group.
    Cancel {
        #[arg(long)]
        node: String,
        #[arg(long)]
        job: String,
    },
}

#[derive(Subcommand)]
enum InviteCommand {
    /// Create a single-use invitation, valid for ten minutes by default.
    Create {
        #[arg(long, default_value_t = 600)]
        ttl_seconds: u64,
        /// Include private relay configuration and a dedicated controller workspace grant.
        #[arg(long)]
        bootstrap: bool,
    },
}

#[derive(Subcommand)]
enum RelayCommand {
    /// Wait up to 12 seconds for an authenticated relay connection.
    Check,
}
#[derive(Subcommand)]
enum Daemon {
    /// Gracefully stop the local daemon without affecting remote peers.
    Stop,
    /// Run the foreground daemon. Use launchd/systemd to supervise it.
    Run {
        #[arg(long)]
        name: String,
        #[arg(long, conflicts_with = "no_relay")]
        relay_url: Option<String>,
        #[arg(long, env = "AGENTMESH_RELAY_TOKEN_FILE", requires = "relay_url")]
        relay_token_file: Option<PathBuf>,
        #[arg(long)]
        no_relay: bool,
        #[arg(long, conflicts_with = "no_relay", requires = "relay_url")]
        relay_only: bool,
        #[arg(long, default_value = "0.0.0.0:43291")]
        bind_addr: String,
        /// Use this DNS server for relay hostnames instead of system DNS. No public fallback.
        #[arg(long)]
        dns_server: Option<std::net::SocketAddr>,
        /// Pin the configured relay hostname to this IP, retaining TLS verification.
        #[arg(long, requires="relay_url", conflicts_with_all=["dns_server","no_relay"])]
        relay_resolve: Option<std::net::IpAddr>,
    },
}
#[derive(Subcommand)]
enum Peer {
    /// Export a public pairing ticket without secrets.
    Ticket,
    /// Trust a public ticket. Both machines must import each other's tickets.
    Add {
        #[arg(long, conflicts_with = "ticket_file")]
        ticket: Option<String>,
        #[arg(long, required_unless_present = "ticket")]
        ticket_file: Option<PathBuf>,
    },
    List,
}
#[derive(Subcommand)]
enum SessionCommand {
    /// Discover local sessions and sessions on paired machines.
    List {
        #[arg(long)]
        node: Option<String>,
        #[arg(long, requires = "node")]
        after: Option<String>,
    },
    /// Attach a live MCP session. Run Claude attachment from its own Bash tool.
    Attach {
        #[arg(long)]
        session: String,
        #[arg(long,value_parser=["claude","tclaude","codex","tcodex"])]
        harness: String,
        #[arg(long)]
        thread: Option<String>,
        #[arg(long)]
        codex_binary: Option<PathBuf>,
    },
}
#[derive(Subcommand)]
enum Mcp {
    /// Serve MCP over stdio. Each invocation owns one distinct session.
    Serve {
        #[arg(long)]
        session: Option<String>,
        #[arg(long, default_value = "session")]
        name: String,
        #[arg(long,default_value="generic",value_parser=["claude","tclaude","codex","tcodex","generic"])]
        harness: String,
    },
}
#[derive(Subcommand)]
enum MessageCommand {
    /// Send using a session's private execution credential file.
    Send {
        #[arg(long)]
        credential_file: PathBuf,
        #[arg(long, required_unless_present = "group", conflicts_with = "group")]
        to: Vec<String>,
        #[arg(long)]
        group: Option<String>,
        #[arg(long)]
        body_file: PathBuf,
        #[arg(long)]
        conversation: Option<String>,
        #[arg(long)]
        reply_to: Option<String>,
    },
    Inbox {
        #[arg(long)]
        credential_file: PathBuf,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    Ack {
        #[arg(long)]
        credential_file: PathBuf,
        #[arg(long, required = true)]
        message: Vec<String>,
    },
    Deliveries,
}
#[derive(Subcommand)]
enum GroupCommand {
    Set {
        #[arg(long)]
        name: String,
        #[arg(long, required = true)]
        member: Vec<String>,
    },
    List {
        #[arg(long)]
        after: Option<String>,
    },
}

#[tokio::main]
async fn main() {
    unsafe {
        libc::umask(0o077);
    }
    let cli = Cli::parse();
    if let Err(error) = execute(cli).await {
        eprintln!("{}", json!({"ok":false,"error":format!("{error:#}")}));
        std::process::exit(1);
    }
}

async fn execute(cli: Cli) -> Result<()> {
    let dir = cli.data_dir.unwrap_or_else(|| {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("agentmesh")
    });
    let dir = std::path::absolute(dir)?;
    if matches!(&cli.command, Command::Workspace(_) | Command::Remote(_))
        && dir.join("daemon.json").exists()
    {
        setup::ensure_daemon(&dir).await?;
    }
    let request = match cli.command {
        Command::Workspace(command) => {
            let command = match command {
                WorkspaceCommand::Add { name, path } => WorkspaceAdmin::Add {
                    name,
                    path: path.to_str().context("path must be UTF-8")?.into(),
                },
                WorkspaceCommand::List => WorkspaceAdmin::List,
                WorkspaceCommand::Grant {
                    name,
                    peer,
                    write,
                    execute,
                } => WorkspaceAdmin::Grant {
                    name,
                    peer,
                    write,
                    execute,
                },
                WorkspaceCommand::Revoke { name, peer } => WorkspaceAdmin::Revoke { name, peer },
                WorkspaceCommand::Remove { name } => WorkspaceAdmin::Remove { name },
                WorkspaceCommand::Prune => WorkspaceAdmin::Prune,
            };
            Request::WorkspaceAdmin { command }
        }
        Command::Remote(RemoteCommand::List { node }) => {
            Request::WorkspaceList { node, lease: None }
        }
        Command::Remote(command) => {
            use WorkspaceOperation as Op;
            let (node, request) = match command {
                RemoteCommand::Files {
                    node,
                    workspace,
                    path,
                } => (node, Op::ListFiles { workspace, path }),
                RemoteCommand::Read {
                    node,
                    workspace,
                    path,
                } => (node, Op::ReadFile { workspace, path }),
                RemoteCommand::Write {
                    node,
                    workspace,
                    path,
                    content_file,
                    expected_sha256,
                } => (
                    node,
                    Op::WriteFile {
                        workspace,
                        path,
                        content: std::fs::read_to_string(content_file)?,
                        expected_sha256,
                    },
                ),
                RemoteCommand::Search {
                    node,
                    workspace,
                    path,
                    query,
                } => (
                    node,
                    Op::Search {
                        workspace,
                        path,
                        query,
                    },
                ),
                RemoteCommand::Execute {
                    node,
                    workspace,
                    path,
                    command,
                    request_id,
                    timeout_seconds,
                } => (
                    node,
                    Op::Execute {
                        workspace,
                        path,
                        command,
                        request_id,
                        timeout_seconds,
                    },
                ),
                RemoteCommand::Job {
                    node,
                    job,
                    stdout_cursor,
                    stderr_cursor,
                } => (
                    node,
                    Op::Job {
                        job,
                        stdout_cursor,
                        stderr_cursor,
                    },
                ),
                RemoteCommand::Jobs { node, workspace } => (node, Op::Jobs { workspace }),
                RemoteCommand::Cancel { node, job } => (node, Op::CancelJob { job }),
                RemoteCommand::List { .. } => unreachable!(),
            };
            Request::WorkspaceCall {
                node,
                request,
                lease: None,
            }
        }
        Command::Invite(InviteCommand::Create {
            ttl_seconds,
            bootstrap,
        }) => {
            setup::ensure_daemon(&dir).await?;
            if bootstrap {
                let result = onboard::invitation(&dir, ttl_seconds).await?;
                println!("{}", serde_json::to_string_pretty(&result)?);
                return Ok(());
            }
            Request::InviteCreate { ttl_seconds }
        }
        Command::Join {
            code_file,
            config_root,
            name,
            no_service,
        } => {
            use std::io::Read;
            let code = if let Some(path) = code_file {
                std::fs::read_to_string(path)?
            } else if let Ok(code) = std::env::var("AGENTMESH_INVITE_CODE") {
                code
            } else {
                if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                    eprintln!("Paste invitation code, then press Enter:");
                }
                let mut code = String::new();
                std::io::BufRead::read_line(
                    &mut std::io::BufReader::new(std::io::stdin().take(8193)),
                    &mut code,
                )?;
                code
            };
            if code.trim().starts_with("amb1_") {
                let result = onboard::join(&dir, config_root, name, &code, !no_service).await?;
                println!("{}", serde_json::to_string_pretty(&result)?);
                return Ok(());
            }
            network::decode(&code)?;
            setup::install(&dir, config_root, name, false)?;
            setup::ensure_daemon(&dir).await?;
            Request::Join { code }
        }
        Command::Daemon(Daemon::Stop) => Request::Shutdown,
        Command::Setup {
            config_root,
            name,
            dry_run,
            config_only,
        } => {
            let result = setup::install(&dir, config_root, name, dry_run)?;
            if !dry_run && !config_only {
                setup::ensure_daemon(&dir).await?;
            }
            println!("{}", serde_json::to_string_pretty(&result)?);
            return Ok(());
        }
        Command::Daemon(Daemon::Run {
            name,
            relay_url,
            relay_token_file,
            no_relay,
            relay_only,
            bind_addr,
            dns_server,
            relay_resolve,
        }) => {
            return daemon::run(daemon::Options {
                dir,
                name,
                relay: relay_url,
                relay_token_file,
                no_relay,
                relay_only,
                bind_addr,
                dns_server,
                relay_resolve,
            })
            .await;
        }
        Command::Mcp(Mcp::Serve {
            session,
            name,
            harness,
        }) => return mcp::serve(dir, session, name, harness).await,
        Command::Config { harness } => {
            let binary = std::env::current_exe()?;
            let args = vec![
                "--data-dir".to_string(),
                dir.to_string_lossy().into(),
                "mcp".into(),
                "serve".into(),
                "--harness".into(),
                harness.clone(),
            ];
            if ["codex", "tcodex"].contains(&harness.as_str()) {
                println!(
                    "[mcp_servers.agentmesh]\ncommand = {}\nargs = {}",
                    serde_json::to_string(&binary)?,
                    serde_json::to_string(&args)?
                );
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &json!({"mcpServers":{"agentmesh":{"command":binary,"args":args}}})
                    )?
                );
            }
            return Ok(());
        }
        Command::Status => Request::Status,
        Command::Relay(RelayCommand::Check) => Request::RelayCheck,
        Command::Peer(Peer::Ticket) => Request::Ticket,
        Command::Peer(Peer::List) => Request::Peers,
        Command::Peer(Peer::Add {
            ticket,
            ticket_file,
        }) => Request::PeerAdd {
            ticket: match ticket {
                Some(s) => s,
                None => std::fs::read_to_string(ticket_file.unwrap())?
                    .trim()
                    .to_string(),
            },
        },
        Command::Session(SessionCommand::List { node, after }) => {
            Request::Directory { node, after }
        }
        Command::Session(SessionCommand::Attach {
            session,
            harness,
            thread,
            codex_binary,
        }) => {
            let target = if ["claude", "tclaude"].contains(&harness.as_str()) {
                ensure!(
                    thread.is_none() && codex_binary.is_none(),
                    "Codex options do not apply to Claude"
                );
                WakeTarget::Claude {
                    socket: std::env::var("CLAUDE_CODE_MESSAGING_SOCKET")
                        .context("run attachment from the target Claude session's Bash tool")?,
                    token: std::env::var("CLAUDE_CODE_MESSAGING_TOKEN").ok(),
                }
            } else {
                let binary = codex_binary
                    .context("provide --codex-binary with an absolute executable path")?;
                WakeTarget::Codex {
                    binary: binary.to_string_lossy().into(),
                    thread: thread.context("provide --thread with the target Codex thread UUID")?,
                    home: std::env::var("CODEX_HOME").ok(),
                }
            };
            Request::Attach {
                id: session,
                target,
            }
        }
        Command::Group(GroupCommand::Set { name, member }) => Request::GroupSet {
            name,
            members: member,
        },
        Command::Group(GroupCommand::List { after }) => Request::Groups { after },
        Command::Message(MessageCommand::Deliveries) => Request::Deliveries,
        Command::Message(MessageCommand::Send {
            credential_file,
            to,
            group,
            body_file,
            conversation,
            reply_to,
        }) => {
            let reg = credential(&credential_file)?;
            Request::Send(Outgoing {
                from: reg.session.id,
                execution: reg.execution,
                to,
                group,
                body: std::fs::read_to_string(body_file)?,
                conversation,
                reply_to,
            })
        }
        Command::Message(MessageCommand::Inbox {
            credential_file,
            limit,
        }) => {
            let reg = credential(&credential_file)?;
            Request::Inbox {
                id: reg.session.id,
                execution: reg.execution,
                limit,
            }
        }
        Command::Message(MessageCommand::Ack {
            credential_file,
            message,
        }) => {
            let reg = credential(&credential_file)?;
            Request::Ack {
                id: reg.session.id,
                execution: reg.execution,
                messages: message,
            }
        }
    };
    let value: Value = local::call(&dir, request).await?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn credential(path: &std::path::Path) -> Result<Registration> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        !metadata.file_type().is_symlink()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0,
        "credential file must be private and owned by current user"
    );
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}
