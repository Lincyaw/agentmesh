use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const ALPN: &[u8] = b"agentmesh/messages/1";
pub const MAX_FRAME: usize = 2 * 1024 * 1024;
pub const LEASE_MS: i64 = 45_000;

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_millis() as i64
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Ticket {
    pub version: u8,
    pub name: String,
    pub address: iroh::EndpointAddr,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invitation {
    pub version: u8,
    pub inviter: Ticket,
    pub secret: String,
    pub expires_at: i64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Network {
    pub key: String,
    pub members: Vec<Ticket>,
}

impl std::fmt::Debug for Network {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Network")
            .field("members", &self.members)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub address: String,
    pub name: String,
    pub harness: String,
    pub online: bool,
    pub wake: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Registration {
    pub session: Session,
    pub execution: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DirectoryPage {
    pub sessions: Vec<Session>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WakeTarget {
    Claude {
        socket: String,
        token: Option<String>,
    },
    Codex {
        binary: String,
        thread: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        home: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub id: String,
    pub from: String,
    pub to: String,
    pub body: String,
    pub conversation: String,
    pub reply_to: Option<String>,
    pub sent_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outgoing {
    pub from: String,
    pub execution: String,
    pub to: Vec<String>,
    pub group: Option<String>,
    pub body: String,
    pub conversation: Option<String>,
    pub reply_to: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Shutdown,
    Status,
    RelayCheck,
    Ticket,
    PeerAdd {
        ticket: String,
    },
    Peers,
    InviteCreate {
        ttl_seconds: u64,
    },
    Join {
        code: String,
    },
    Directory {
        node: Option<String>,
        after: Option<String>,
    },
    Register {
        id: Option<String>,
        name: String,
        harness: String,
    },
    Heartbeat {
        id: String,
        execution: String,
    },
    Retire {
        id: String,
        execution: String,
    },
    Attach {
        id: String,
        target: WakeTarget,
    },
    Send(Outgoing),
    Inbox {
        id: String,
        execution: String,
        limit: usize,
    },
    Ack {
        id: String,
        execution: String,
        messages: Vec<String>,
    },
    Groups {
        after: Option<String>,
    },
    GroupSet {
        name: String,
        members: Vec<String>,
    },
    Deliveries,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum RemoteRequest {
    Join { secret: String, member: Ticket },
    NetworkSync { network: Network, member: Ticket },
    Directory { after: Option<String> },
    Deliver { message: Message },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn from_result(result: anyhow::Result<Value>) -> Self {
        match result {
            Ok(result) => Self {
                ok: true,
                result: Some(result),
                error: None,
            },
            Err(error) => Self {
                ok: false,
                result: None,
                error: Some(format!("{error:#}")),
            },
        }
    }
    pub fn into_result(self) -> anyhow::Result<Value> {
        if !self.ok {
            anyhow::bail!(
                "{}",
                self.error.unwrap_or_else(|| "invalid response".into())
            );
        }
        self.result
            .ok_or_else(|| anyhow::anyhow!("response has no result"))
    }
}

pub fn split_address(address: &str) -> anyhow::Result<(&str, &str)> {
    let (node, session) = address
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("expected endpoint-id/session-uuid"))?;
    node.parse::<iroh::EndpointId>()?;
    uuid::Uuid::parse_str(session)?;
    Ok((node, session))
}

pub fn validate_text(value: &str, label: &str, max: usize) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.trim().is_empty() && value.len() <= max && !value.contains('\0'),
        "invalid {label}: expected 1..={max} bytes without NUL"
    );
    Ok(())
}
