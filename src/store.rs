use crate::protocol::*;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub struct Database(Arc<Mutex<Store>>);

impl Database {
    pub fn open(path: &Path, node: String) -> Result<Self> {
        Ok(Self(Arc::new(Mutex::new(Store::open(path, node)?))))
    }
    pub async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Store) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let store = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = store
                .lock()
                .map_err(|_| anyhow::anyhow!("database lock poisoned"))?;
            f(&mut guard)
        })
        .await?
    }
}

pub struct Store {
    conn: Connection,
    node: String,
}

impl Store {
    fn open(path: &Path, node: String) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
          CREATE TABLE IF NOT EXISTS peers(id TEXT PRIMARY KEY,ticket TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY,name TEXT NOT NULL,harness TEXT NOT NULL,execution TEXT NOT NULL,seen INTEGER NOT NULL,wake TEXT);
          CREATE TABLE IF NOT EXISTS inbox(sender TEXT NOT NULL,id TEXT NOT NULL,recipient TEXT NOT NULL,body TEXT NOT NULL,ack INTEGER NOT NULL DEFAULT 0,native TEXT,received INTEGER NOT NULL,PRIMARY KEY(sender,id,recipient));
          CREATE TABLE IF NOT EXISTS outbox(id TEXT NOT NULL,recipient TEXT NOT NULL,body TEXT NOT NULL,status TEXT NOT NULL DEFAULT 'pending',attempts INTEGER NOT NULL DEFAULT 0,next_attempt INTEGER NOT NULL DEFAULT 0,error TEXT,PRIMARY KEY(id,recipient));
          CREATE INDEX IF NOT EXISTS pending_outbox ON outbox(status,next_attempt);
          CREATE TABLE IF NOT EXISTS groups(name TEXT PRIMARY KEY,members TEXT NOT NULL);")?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS network_config(id INTEGER PRIMARY KEY CHECK(id=1),key TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS network_members(id TEXT PRIMARY KEY,ticket TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS invitations(digest TEXT PRIMARY KEY,expires INTEGER NOT NULL,claimed_by TEXT);")?;
        conn.execute(
            "UPDATE inbox SET native='uncertain' WHERE native='dispatching'",
            [],
        )?;
        Ok(Self { conn, node })
    }
    pub fn add_peer(&mut self, ticket: Ticket) -> Result<()> {
        ensure!(ticket.version == 1, "unsupported ticket version");
        validate_text(&ticket.name, "peer name", 128)?;
        let id = ticket.address.id.to_string();
        ensure!(id != self.node, "cannot pair with self");
        ensure!(
            serde_json::to_vec(&ticket)?.len() <= 4096,
            "peer ticket exceeds 4096 bytes"
        );
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM peers", [], |r| r.get(0))?;
        ensure!(
            count < 256 || self.trusted(&id)?,
            "at most 256 directly paired machines are supported"
        );
        self.conn.execute(
            "INSERT INTO peers VALUES(?,?) ON CONFLICT(id) DO UPDATE SET ticket=excluded.ticket",
            params![id, serde_json::to_string(&ticket)?],
        )?;
        Ok(())
    }
    pub fn peers(&self) -> Result<Vec<Ticket>> {
        let mut stmt = self.conn.prepare("SELECT ticket FROM peers ORDER BY id")?;
        let texts = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        texts
            .into_iter()
            .map(|s| Ok(serde_json::from_str(&s)?))
            .collect()
    }
    pub fn network(&self) -> Result<Option<Network>> {
        let key: Option<String> = self
            .conn
            .query_row("SELECT key FROM network_config WHERE id=1", [], |r| {
                r.get(0)
            })
            .optional()?;
        let Some(key) = key else { return Ok(None) };
        let mut stmt = self
            .conn
            .prepare("SELECT ticket FROM network_members ORDER BY id")?;
        let members = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|s| Ok(serde_json::from_str(&s)?))
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(Network { key, members }))
    }
    pub fn merge_network(&mut self, network: Network) -> Result<Network> {
        ensure!(
            network.members.len() <= 256,
            "network supports at most 256 machines"
        );
        crate::network::validate_key(&network.key)?;
        let old = self.network()?;
        if let Some(ref old) = old {
            ensure!(
                crate::network::same_key(&old.key, &network.key),
                "machine already belongs to a different network"
            );
        }
        let mut members = std::collections::BTreeMap::new();
        for member in old
            .into_iter()
            .flat_map(|n| n.members)
            .chain(network.members)
        {
            crate::network::validate_ticket(&member)?;
            members.insert(member.address.id.to_string(), member);
        }
        ensure!(
            members.len() <= 256,
            "network supports at most 256 machines"
        );
        let existing = self
            .peers()?
            .into_iter()
            .map(|p| p.address.id.to_string())
            .collect::<std::collections::BTreeSet<_>>();
        let total = existing
            .iter()
            .cloned()
            .chain(members.keys().filter(|id| **id != self.node).cloned())
            .collect::<std::collections::BTreeSet<_>>();
        ensure!(
            total.len() <= 256,
            "at most 256 paired machines are supported"
        );
        let transaction = self.conn.savepoint()?;
        transaction.execute(
            "INSERT INTO network_config VALUES(1,?) ON CONFLICT(id) DO NOTHING",
            [&network.key],
        )?;
        for (id, ticket) in &members {
            let encoded = serde_json::to_string(ticket)?;
            transaction.execute("INSERT INTO network_members VALUES(?,?) ON CONFLICT(id) DO UPDATE SET ticket=excluded.ticket", params![id,encoded])?;
            if *id != self.node {
                transaction.execute("INSERT INTO peers VALUES(?,?) ON CONFLICT(id) DO UPDATE SET ticket=excluded.ticket", params![id,encoded])?;
            }
        }
        transaction.commit()?;
        Ok(Network {
            key: network.key,
            members: members.into_values().collect(),
        })
    }
    pub fn create_invite(&mut self, ticket: Ticket, ttl_seconds: u64) -> Result<Invitation> {
        ensure!(
            (1..=86400).contains(&ttl_seconds),
            "invitation lifetime must be 1..=86400 seconds"
        );
        let network = self.network()?.unwrap_or_else(|| Network {
            key: crate::network::random_secret(),
            members: vec![],
        });
        self.merge_network(Network {
            key: network.key,
            members: vec![ticket.clone()],
        })?;
        let invitation = Invitation {
            version: 1,
            inviter: ticket,
            secret: crate::network::random_secret(),
            expires_at: now() + ttl_seconds as i64 * 1000,
        };
        self.conn
            .execute("DELETE FROM invitations WHERE expires<?", [now()])?;
        self.conn.execute(
            "INSERT INTO invitations VALUES(?,?,NULL)",
            params![
                crate::network::digest(&invitation.secret),
                invitation.expires_at
            ],
        )?;
        Ok(invitation)
    }
    pub fn redeem_invite(&mut self, secret: &str, member: Ticket) -> Result<Network> {
        crate::network::validate_key(secret)?;
        let digest = crate::network::digest(secret);
        let id = member.address.id.to_string();
        let valid = self.conn.query_row("SELECT 1 FROM invitations WHERE digest=? AND expires>? AND (claimed_by IS NULL OR claimed_by=?)",params![digest,now(),id], |_| Ok(())).optional()?.is_some();
        ensure!(valid, "invitation is invalid, expired or already used");
        self.conn.execute_batch("SAVEPOINT redeem_invitation")?;
        let result = (|| {
            let network = self.network()?.context("network not initialized")?;
            let network = self.merge_network(Network {
                key: network.key,
                members: vec![member],
            })?;
            self.conn.execute(
                "UPDATE invitations SET claimed_by=? WHERE digest=?",
                params![id, digest],
            )?;
            Ok(network)
        })();
        if result.is_err() {
            self.conn.execute_batch("ROLLBACK TO redeem_invitation")?;
        }
        self.conn.execute_batch("RELEASE redeem_invitation")?;
        result
    }
    pub fn trusted(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM peers WHERE id=?", [id], |_| Ok(()))
            .optional()?
            .is_some())
    }
    pub fn register(
        &mut self,
        id: Option<String>,
        name: String,
        harness: String,
    ) -> Result<Registration> {
        validate_text(&name, "session name", 128)?;
        ensure!(
            ["claude", "tclaude", "codex", "tcodex", "generic"].contains(&harness.as_str()),
            "harness must be claude, tclaude, codex, tcodex or generic"
        );
        let id = id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        uuid::Uuid::parse_str(&id)?;
        let live = self
            .conn
            .query_row("SELECT seen FROM sessions WHERE id=?", [&id], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?;
        ensure!(
            live.is_none_or(|seen| seen <= now() - LEASE_MS),
            "session already has a live execution"
        );
        let execution = uuid::Uuid::new_v4().to_string();
        self.conn.execute("INSERT INTO sessions VALUES(?,?,?,?,?,NULL) ON CONFLICT(id) DO UPDATE SET name=excluded.name,harness=excluded.harness,execution=excluded.execution,seen=excluded.seen,wake=NULL",params![id,name,harness,execution,now()])?;
        Ok(Registration {
            session: Session {
                address: format!("{}/{}", self.node, id),
                id,
                name,
                harness,
                online: true,
                wake: false,
            },
            execution,
        })
    }
    pub fn authorize(&self, id: &str, execution: &str) -> Result<()> {
        let valid = self
            .conn
            .query_row(
                "SELECT 1 FROM sessions WHERE id=? AND execution=? AND seen>?",
                params![id, execution, now() - LEASE_MS],
                |_| Ok(()),
            )
            .optional()?;
        ensure!(
            valid.is_some(),
            "session execution expired or replaced; reconnect MCP"
        );
        Ok(())
    }
    pub fn heartbeat(&mut self, id: &str, execution: &str) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE sessions SET seen=? WHERE id=? AND execution=? AND seen>0",
            params![now(), id, execution],
        )?;
        ensure!(
            changed == 1,
            "session execution retired or replaced; reconnect MCP"
        );
        Ok(())
    }
    pub fn retire(&mut self, id: &str, execution: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET seen=0,wake=NULL WHERE id=? AND execution=?",
            params![id, execution],
        )?;
        Ok(())
    }
    pub fn attach(&mut self, id: &str, target: WakeTarget) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE sessions SET wake=? WHERE id=? AND seen>?",
            params![serde_json::to_string(&target)?, id, now() - LEASE_MS],
        )?;
        ensure!(n == 1, "no live session with that ID");
        self.conn.execute(
            "UPDATE inbox SET native=NULL WHERE recipient=? AND native='unavailable'",
            [id],
        )?;
        Ok(())
    }
    pub fn directory(&self, after: Option<String>) -> Result<DirectoryPage> {
        if let Some(ref cursor) = after {
            uuid::Uuid::parse_str(cursor)?;
        }
        let mut stmt = self.conn.prepare(
            "SELECT id,name,harness,seen,wake IS NOT NULL FROM sessions WHERE id>? ORDER BY id LIMIT 201",
        )?;
        let mut sessions: Vec<Session> = stmt
            .query_map([after.unwrap_or_default()], |r| {
                let id: String = r.get(0)?;
                Ok(Session {
                    id: id.clone(),
                    address: format!("{}/{}", self.node, id),
                    name: r.get(1)?,
                    harness: r.get(2)?,
                    online: r.get::<_, i64>(3)? > now() - LEASE_MS,
                    wake: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let next_cursor = if sessions.len() > 200 {
            sessions.truncate(200);
            sessions.last().map(|s| s.id.clone())
        } else {
            None
        };
        Ok(DirectoryPage {
            sessions,
            next_cursor,
        })
    }
    pub fn groups(&self, after: Option<String>) -> Result<Value> {
        let mut stmt = self
            .conn
            .prepare("SELECT name,members FROM groups WHERE name>? ORDER BY name LIMIT 33")?;
        let rows = stmt
            .query_map([after.unwrap_or_default()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut groups = Vec::new();
        let mut bytes = 0;
        let mut more = false;
        for (name, members) in rows {
            let value = json!({"name":name,"members":serde_json::from_str::<Value>(&members)?});
            let size = serde_json::to_vec(&value)?.len() + 1;
            if groups.len() == 32 || bytes + size > MAX_FRAME / 2 {
                more = true;
                break;
            }
            bytes += size;
            groups.push(value);
        }
        let next_cursor = if more {
            groups.last().map(|g| g["name"].clone())
        } else {
            None
        };
        Ok(json!({"groups":groups,"next_cursor":next_cursor}))
    }
    pub fn group_set(&mut self, name: &str, members: Vec<String>) -> Result<()> {
        validate_text(name, "group name", 128)?;
        ensure!(
            !members.is_empty() && members.len() <= 256,
            "group requires 1..=256 members"
        );
        let mut unique = std::collections::BTreeSet::new();
        for member in members {
            self.validate_recipient(&member)?;
            unique.insert(member);
        }
        self.conn.execute("INSERT INTO groups VALUES(?,?) ON CONFLICT(name) DO UPDATE SET members=excluded.members",params![name,serde_json::to_string(&unique)?])?;
        Ok(())
    }
    fn validate_recipient(&self, address: &str) -> Result<()> {
        let (node, id) = split_address(address)?;
        if node == self.node {
            ensure!(
                self.conn
                    .query_row("SELECT 1 FROM sessions WHERE id=?", [id], |_| Ok(()))
                    .optional()?
                    .is_some(),
                "unknown local recipient"
            );
        } else {
            ensure!(self.trusted(node)?, "recipient machine is not paired");
        }
        Ok(())
    }
    pub fn enqueue(&mut self, input: Outgoing) -> Result<Value> {
        let Outgoing {
            from,
            execution,
            mut to,
            group,
            body,
            conversation,
            reply_to,
        } = input;
        self.authorize(&from, &execution)?;
        validate_text(&body, "message body", 64 * 1024)?;
        if let Some(ref value) = conversation {
            validate_text(value, "conversation", 256)?;
        }
        if let Some(ref value) = reply_to {
            uuid::Uuid::parse_str(value)?;
        }
        ensure!(
            group.is_none() || to.is_empty(),
            "choose recipients or a group, not both"
        );
        if let Some(ref group) = group {
            let members: String = self
                .conn
                .query_row("SELECT members FROM groups WHERE name=?", [group], |r| {
                    r.get(0)
                })
                .optional()?
                .context("group not found")?;
            to = serde_json::from_str(&members)?;
        }
        let to = to.into_iter().collect::<std::collections::BTreeSet<_>>();
        ensure!(
            !to.is_empty() && to.len() <= 256,
            "requires 1..=256 recipients"
        );
        for address in &to {
            self.validate_recipient(address)?;
        }
        let id = uuid::Uuid::new_v4().to_string();
        let conversation = conversation.unwrap_or_else(|| {
            group
                .map(|g| format!("group:{}/{g}", self.node))
                .unwrap_or_else(|| id.clone())
        });
        let sender = format!("{}/{}", self.node, from);
        let tx = self.conn.transaction()?;
        for target in &to {
            let msg = Message {
                id: id.clone(),
                from: sender.clone(),
                to: target.clone(),
                body: body.clone(),
                conversation: conversation.clone(),
                reply_to: reply_to.clone(),
                sent_at: now(),
            };
            tx.execute(
                "INSERT INTO outbox(id,recipient,body) VALUES(?,?,?)",
                params![id, target, serde_json::to_string(&msg)?],
            )?;
        }
        tx.commit()?;
        Ok(json!({"id":id,"conversation":conversation,"recipients":to,"status":"queued"}))
    }
    pub fn receive(&mut self, peer: &str, msg: Message) -> Result<()> {
        uuid::Uuid::parse_str(&msg.id)?;
        let (source, _) = split_address(&msg.from)?;
        ensure!(
            source == peer,
            "sender identity does not match authenticated peer"
        );
        let (target, id) = split_address(&msg.to)?;
        ensure!(target == self.node, "message addressed to another machine");
        validate_text(&msg.body, "message body", 64 * 1024)?;
        validate_text(&msg.conversation, "conversation", 256)?;
        if let Some(ref reply) = msg.reply_to {
            uuid::Uuid::parse_str(reply)?;
        }
        self.validate_recipient(&msg.to)?;
        let content = serde_json::to_string(&msg)?;
        let old = self
            .conn
            .query_row(
                "SELECT body FROM inbox WHERE sender=? AND id=? AND recipient=?",
                params![peer, msg.id, id],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        if let Some(old) = old {
            ensure!(old == content, "message ID reused with different content");
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO inbox(sender,id,recipient,body,received) VALUES(?,?,?,?,?)",
            params![peer, msg.id, id, content, now()],
        )?;
        Ok(())
    }
    pub fn pending(&self) -> Result<Vec<Message>> {
        let mut stmt=self.conn.prepare("SELECT body FROM outbox WHERE status='pending' AND next_attempt<=? ORDER BY rowid LIMIT 64")?;
        let rows = stmt
            .query_map([now()], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|s| Ok(serde_json::from_str(&s)?))
            .collect()
    }
    pub fn finish_delivery(&mut self, msg: &Message, error: Option<String>) -> Result<()> {
        if let Some(error) = error {
            self.conn.execute("UPDATE outbox SET attempts=attempts+1,next_attempt=?+MIN(30000,1000*(1<<MIN(attempts,5))),error=? WHERE id=? AND recipient=?",params![now(),error,msg.id,msg.to])?;
        } else {
            self.conn.execute(
                "UPDATE outbox SET status='delivered',error=NULL WHERE id=? AND recipient=?",
                params![msg.id, msg.to],
            )?;
        }
        Ok(())
    }
    pub fn inbox(&self, id: &str, execution: &str, limit: usize) -> Result<Value> {
        self.authorize(id, execution)?;
        ensure!((1..=100).contains(&limit), "limit must be 1..=100");
        let mut stmt=self.conn.prepare("SELECT body,native FROM inbox WHERE recipient=? AND ack=0 ORDER BY received,rowid LIMIT ?")?;
        let rows = stmt
            .query_map(params![id, limit as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut messages = Vec::new();
        let mut bytes = 0;
        for (body, wake) in rows {
            let item = json!({"message":serde_json::from_str::<Value>(&body)?,"wake_outcome":wake});
            let size = serde_json::to_vec(&item)?.len() + 1;
            if bytes + size > MAX_FRAME / 2 {
                break;
            }
            bytes += size;
            messages.push(item);
        }
        Ok(Value::Array(messages))
    }
    pub fn ack(&mut self, id: &str, execution: &str, messages: Vec<String>) -> Result<usize> {
        self.authorize(id, execution)?;
        ensure!(messages.len() <= 100, "at most 100 acknowledgements");
        let tx = self.conn.transaction()?;
        let mut changed = 0;
        for message in messages {
            uuid::Uuid::parse_str(&message)?;
            changed += tx.execute(
                "UPDATE inbox SET ack=1 WHERE recipient=? AND id=? AND ack=0",
                params![id, message],
            )?;
        }
        tx.commit()?;
        Ok(changed)
    }
    pub fn deliveries(&self) -> Result<Value> {
        let mut stmt = self.conn.prepare(
            "SELECT id,recipient,status,attempts,error FROM outbox ORDER BY rowid DESC LIMIT 100",
        )?;
        Ok(Value::Array(stmt.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"recipient":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?,"attempts":r.get::<_,i64>(3)?,"error":r.get::<_,Option<String>>(4)?})))?.collect::<rusqlite::Result<_>>()?))
    }
    pub fn claim_wake(&mut self) -> Result<Vec<(String, WakeTarget, Vec<String>)>> {
        let mut stmt=self.conn.prepare("SELECT DISTINCT s.id,s.wake FROM sessions s JOIN inbox i ON s.id=i.recipient WHERE s.seen>? AND s.wake IS NOT NULL AND i.ack=0 AND i.native IS NULL LIMIT 16")?;
        let sessions = stmt
            .query_map([now() - LEASE_MS], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        let mut batches = vec![];
        for (id, target) in sessions {
            let tx = self.conn.transaction()?;
            let ids = tx
                .prepare(
                    "SELECT id FROM inbox WHERE recipient=? AND ack=0 AND native IS NULL LIMIT 32",
                )?
                .query_map([&id], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for msg in &ids {
                tx.execute(
                    "UPDATE inbox SET native='dispatching' WHERE recipient=? AND id=?",
                    params![id, msg],
                )?;
            }
            tx.commit()?;
            batches.push((id, serde_json::from_str(&target)?, ids));
        }
        Ok(batches)
    }
    pub fn finish_wake(&mut self, id: &str, messages: &[String], outcome: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        for message in messages {
            tx.execute(
                "UPDATE inbox SET native=? WHERE recipient=? AND id=? AND native='dispatching'",
                params![outcome, id, message],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> Store {
        Store::open(
            Path::new(":memory:"),
            iroh::SecretKey::generate().public().to_string(),
        )
        .unwrap()
    }

    #[test]
    fn req002_reject_forged_sender_and_changed_message_replay() {
        let mut db = database();
        let recipient = db
            .register(None, "recipient".into(), "generic".into())
            .unwrap();
        let sender = iroh::SecretKey::generate().public().to_string();
        let msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            from: format!("{}/{}", sender, uuid::Uuid::new_v4()),
            to: recipient.session.address,
            body: "first".into(),
            conversation: "conversation".into(),
            reply_to: None,
            sent_at: now(),
        };
        assert!(db.receive(&db.node.clone(), msg.clone()).is_err());
        db.receive(&sender, msg.clone()).unwrap();
        db.receive(&sender, msg.clone()).unwrap();
        let mut changed = msg;
        changed.body = "changed".into();
        assert!(db.receive(&sender, changed).is_err());
    }

    #[test]
    fn req003_renew_expired_execution_but_fence_retired_and_replaced() {
        let mut db = database();
        let reg = db
            .register(None, "session".into(), "generic".into())
            .unwrap();
        db.conn
            .execute("UPDATE sessions SET seen=?", [now() - 2 * LEASE_MS])
            .unwrap();
        assert!(db.authorize(&reg.session.id, &reg.execution).is_err());
        db.heartbeat(&reg.session.id, &reg.execution).unwrap();
        db.authorize(&reg.session.id, &reg.execution).unwrap();
        db.retire(&reg.session.id, &reg.execution).unwrap();
        assert!(db.heartbeat(&reg.session.id, &reg.execution).is_err());
        let successor = db
            .register(
                Some(reg.session.id.clone()),
                "successor".into(),
                "generic".into(),
            )
            .unwrap();
        assert!(db.heartbeat(&reg.session.id, &reg.execution).is_err());
        db.retire(&reg.session.id, &reg.execution).unwrap();
        db.authorize(&successor.session.id, &successor.execution)
            .unwrap();
    }

    #[test]
    fn req004_escaped_large_inbox_is_readable_in_bounded_batches() {
        let mut db = database();
        let reg = db
            .register(None, "session".into(), "generic".into())
            .unwrap();
        let node = db.node.clone();
        for _ in 0..20 {
            db.receive(
                &node,
                Message {
                    id: uuid::Uuid::new_v4().to_string(),
                    from: reg.session.address.clone(),
                    to: reg.session.address.clone(),
                    body: "\u{1}".repeat(64_000),
                    conversation: "large".into(),
                    reply_to: None,
                    sent_at: now(),
                },
            )
            .unwrap();
        }
        let mut total = 0;
        loop {
            let page = db.inbox(&reg.session.id, &reg.execution, 100).unwrap();
            assert!(serde_json::to_vec(&page).unwrap().len() < MAX_FRAME);
            let messages = page.as_array().unwrap();
            if messages.is_empty() {
                break;
            }
            total += messages.len();
            db.ack(
                &reg.session.id,
                &reg.execution,
                messages
                    .iter()
                    .map(|m| m["message"]["id"].as_str().unwrap().to_string())
                    .collect(),
            )
            .unwrap();
        }
        assert_eq!(total, 20);
    }

    #[test]
    fn req003_directory_can_page_beyond_two_hundred_sessions() {
        let mut db = database();
        for i in 0..205 {
            db.register(None, format!("session {i}"), "generic".into())
                .unwrap();
        }
        let first = db.directory(None).unwrap();
        assert_eq!(first.sessions.len(), 200);
        let second = db.directory(first.next_cursor).unwrap();
        assert_eq!(second.sessions.len(), 5);
        assert!(second.next_cursor.is_none());
        assert!(first.sessions.last().unwrap().id < second.sessions.first().unwrap().id);
    }

    #[test]
    fn req005_large_groups_list_is_paginated_under_frame_budget() {
        let mut db = database();
        let members = (0..256)
            .map(|i| {
                db.register(None, format!("member {i}"), "generic".into())
                    .unwrap()
                    .session
                    .address
            })
            .collect::<Vec<_>>();
        for i in 0..90 {
            db.group_set(&format!("group-{i:03}"), members.clone())
                .unwrap();
        }
        let mut after = None;
        let mut total = 0;
        loop {
            let page = db.groups(after).unwrap();
            assert!(serde_json::to_vec(&page).unwrap().len() < MAX_FRAME);
            let groups = page["groups"].as_array().unwrap();
            assert!(groups.len() <= 32);
            total += groups.len();
            after = page["next_cursor"].as_str().map(str::to_string);
            if after.is_none() {
                break;
            }
        }
        assert_eq!(total, 90);
    }
}
