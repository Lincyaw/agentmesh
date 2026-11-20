mod daemon;
mod local;
mod mcp;
mod network;
mod protocol;
mod setup;
mod store;
mod wake;

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
enum InviteCommand {
    /// Create a single-use invitation, valid for ten minutes by default.
    Create {
        #[arg(long, default_value_t = 600)]
        ttl_seconds: u64,
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
        #[arg(long, conflicts_with = "no_relay")]
        relay_only: bool,
        #[arg(long, default_value = "0.0.0.0:43291")]
        bind_addr: String,
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
    let request = match cli.command {
        Command::Invite(InviteCommand::Create { ttl_seconds }) => {
            setup::ensure_daemon(&dir).await?;
            Request::InviteCreate { ttl_seconds }
        }
        Command::Join {
            code_file,
            config_root,
            name,
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
        }) => {
            return daemon::run(
                dir,
                name,
                relay_url,
                relay_token_file,
                no_relay,
                relay_only,
                bind_addr,
            )
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
