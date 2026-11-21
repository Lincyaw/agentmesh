use crate::{local, protocol::*, store::Database};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use fs2::FileExt;
use futures::{StreamExt, stream};
use iroh::{Endpoint, RelayConfig, RelayMap, RelayMode, SecretKey, endpoint::presets};
use serde_json::{Value, json};
use std::{
    fs::OpenOptions,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    net::UnixListener,
    sync::{Notify, Semaphore},
};

pub struct State {
    pub endpoint: Endpoint,
    pub db: Database,
    pub name: String,
    pub changed: Notify,
    pub executor: crate::execution::Executor,
    allowed_relay: Option<iroh::RelayUrl>,
    relay_only: bool,
    shutdown: Notify,
}

pub(crate) fn read_relay_token(path: &Path) -> Result<String> {
    use std::os::unix::fs::MetadataExt;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .context("cannot open relay token file")?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0,
        "relay token file must be private and owned by current user"
    );
    use std::io::Read;
    let mut contents = String::new();
    Read::by_ref(&mut file)
        .take(8193)
        .read_to_string(&mut contents)?;
    ensure!(
        contents.len() <= 8192,
        "relay token file exceeds size limit"
    );
    let token = contents.trim().to_string();
    ensure!(
        !token.is_empty() && token.len() <= 4096 && !token.contains(['\r', '\n', '\0']),
        "relay token file must contain one nonempty token of at most 4096 bytes"
    );
    Ok(token)
}

pub struct Options {
    pub dir: PathBuf,
    pub name: String,
    pub relay: Option<String>,
    pub relay_token_file: Option<PathBuf>,
    pub no_relay: bool,
    pub relay_only: bool,
    pub bind_addr: String,
    pub dns_server: Option<std::net::SocketAddr>,
    pub relay_resolve: Option<std::net::IpAddr>,
}

pub async fn run(options: Options) -> Result<()> {
    let Options {
        dir,
        name,
        relay,
        relay_token_file,
        no_relay,
        relay_only,
        bind_addr,
        dns_server,
        relay_resolve,
    } = options;
    validate_text(&name, "machine name", 128)?;
    ensure!(
        relay_resolve.is_none() || (relay.is_some() && dns_server.is_none()),
        "relay-resolve requires a relay URL and conflicts with dns-server"
    );
    ensure!(
        !relay_only || relay.is_some(),
        "relay-only mode requires an explicit relay URL"
    );
    local::private_dir(&dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(dir.join("daemon.lock"))?;
    lock.try_lock_exclusive()
        .context("a daemon already owns this data directory")?;
    let key_path = dir.join("identity.key");
    let key = if key_path.exists() {
        let data = std::fs::read(&key_path)?;
        SecretKey::from_bytes(
            &data
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid identity key length"))?,
        )
    } else {
        let key = SecretKey::generate();
        use std::io::Write;
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&key_path)?
            .write_all(&key.to_bytes())?;
        key
    };
    let allowed_relay = if no_relay {
        None
    } else {
        relay
            .as_deref()
            .map(str::parse::<iroh::RelayUrl>)
            .transpose()?
    };
    let relay_mode = if no_relay {
        RelayMode::Disabled
    } else if let Some(url) = relay {
        let mut config = RelayConfig::new(url.parse()?, None);
        if let Some(path) = relay_token_file {
            let token = read_relay_token(&path)?;
            config = config.with_auth_token(token);
        }
        RelayMode::Custom(RelayMap::from_iter([config]))
    } else {
        RelayMode::Disabled
    };
    // Peers exchange explicit addresses in invitations/tickets. Do not publish
    // node metadata or resolve peers through the public iroh.link directory.
    let resolver = iroh::dns::DnsResolver::builder().disable_fallback();
    let resolver = match dns_server {
        Some(server) => resolver.add_nameserver_config(
            iroh::dns::NameserverConfig::udp(server.ip()).with_port(server.port()),
        ),
        None => resolver.with_system_defaults(),
    }
    .build();
    let resolver = if let Some(address) = relay_resolve {
        crate::relay_dns::pinned(
            allowed_relay
                .as_ref()
                .context("relay-resolve requires an enabled relay")?,
            address,
        )?
    } else {
        resolver
    };
    let builder = Endpoint::builder(presets::Minimal)
        .clear_address_lookup()
        .dns_resolver(resolver)
        .secret_key(key)
        .relay_mode(relay_mode)
        .alpns(vec![ALPN.to_vec()])
        .clear_ip_transports();
    let builder = if relay_only {
        builder
    } else {
        builder.bind_addr(bind_addr.as_str())?
    };
    let endpoint = builder.bind().await?;
    let db = Database::open(&dir.join("mailbox.sqlite"), endpoint.id().to_string())?;
    let state = Arc::new(State {
        allowed_relay,
        relay_only,
        executor: crate::execution::Executor::open(&dir)?,
        endpoint,
        db,
        name,
        changed: Notify::new(),
        shutdown: Notify::new(),
    });
    let socket = dir.join("daemon.sock");
    if socket.exists() {
        std::fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    eprintln!(
        "{}",
        json!({"event":"ready","node":state.endpoint.id().to_string(),"socket":socket})
    );
    let peer_state = state.clone();
    let peer_task = tokio::spawn(async move { accept_peers(peer_state).await });
    let network_state = state.clone();
    let network_task = tokio::spawn(async move { network_loop(network_state).await });
    let delivery_state = state.clone();
    let delivery_task = tokio::spawn(async move { delivery_loop(delivery_state).await });
    let wake_state = state.clone();
    let wake_task = tokio::spawn(async move { wake_loop(wake_state).await });
    let permits = Arc::new(Semaphore::new(128));
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            _=state.shutdown.notified()=>break,
            _=tokio::signal::ctrl_c()=>break,
            _=terminate.recv()=>break,
            accepted=listener.accept()=>{
                let(mut socket,_)=accepted?;
                let Ok(permit)=permits.clone().try_acquire_owned()else{continue};
                let state=state.clone();
                tokio::spawn(async move{
                    let _permit=permit;
                    let result=tokio::time::timeout(Duration::from_secs(22),async{
                        let bytes=local::read_frame(&mut socket).await?;
                        let request=serde_json::from_slice::<Request>(&bytes)?;
                        state.handle(request).await
                    }).await.unwrap_or_else(|_|Err(anyhow::anyhow!("request timed out")));
                    let stopping=result.as_ref().is_ok_and(|value|value.get("stopping")==Some(&json!(true)));
                    if let Ok(mut bytes)=serde_json::to_vec(&Response::from_result(result)) {
                        bytes.push(b'\n');
                        if let Err(e)=socket.write_all(&bytes).await {eprintln!("local response failed: {e}");}
                    }
                    if stopping{state.shutdown.notify_one();}
                });
            }
        }
    }
    state.executor.shutdown().await;
    peer_task.abort();
    network_task.abort();
    delivery_task.abort();
    wake_task.abort();
    state.endpoint.close().await;
    std::fs::remove_file(socket)?;
    Ok(())
}

impl State {
    async fn sync_peer(&self, peer: Ticket, network: Network) -> Result<()> {
        let expected = peer.address.id;
        let value = self
            .remote(
                peer,
                RemoteRequest::NetworkSync {
                    network,
                    member: self.ticket(),
                },
            )
            .await?;
        let mut received: Network = serde_json::from_value(value)?;
        let current = self.ticket();
        received
            .members
            .retain(|p| p.address.id != current.address.id);
        received.members.push(current);
        ensure!(
            received.members.iter().any(|p| p.address.id == expected),
            "sync response omitted its sender"
        );
        self.db.run(move |db| db.merge_network(received)).await?;
        Ok(())
    }
    async fn authorize_lease(&self, lease: Option<ExecutionLease>) -> Result<()> {
        if let Some(lease) = lease {
            self.db
                .run(move |db| db.authorize(&lease.id, &lease.execution))
                .await?;
        }
        Ok(())
    }

    async fn resolve_machine(&self, name: &str) -> Result<Ticket> {
        let local = self.ticket();
        let mut peers = self.db.run(|db| db.peers()).await?;
        peers.push(local.clone());
        if name == "local" {
            return Ok(local);
        }
        let mut found = peers
            .into_iter()
            .filter(|p| p.address.id.to_string() == name || p.name == name);
        let peer = found
            .next()
            .context("machine not found; use peer list or an endpoint ID")?;
        ensure!(
            found.next().is_none(),
            "ambiguous machine name; use an endpoint ID"
        );
        Ok(peer)
    }

    async fn workspace_call(
        &self,
        node: String,
        request: WorkspaceOperation,
        administrator: bool,
    ) -> Result<Value> {
        let peer = self.resolve_machine(&node).await?;
        if peer.address.id == self.endpoint.id() {
            self.executor
                .handle(self.endpoint.id().to_string(), request, administrator)
                .await
        } else {
            self.remote(peer, RemoteRequest::Workspace { request })
                .await
        }
    }

    async fn workspaces(&self, node: Option<String>, administrator: bool) -> Result<Value> {
        let peers = if let Some(node) = node {
            vec![self.resolve_machine(&node).await?]
        } else {
            let mut peers = self.db.run(|db| db.peers()).await?;
            peers.push(self.ticket());
            peers
        };
        let mut waiting = peers
            .iter()
            .map(|p| p.address.id.to_string())
            .collect::<std::collections::BTreeSet<_>>();
        let mut requests = stream::iter(peers.into_iter().map(|p| async move {
            let node = p.address.id.to_string();
            let result = self
                .workspace_call(node.clone(), WorkspaceOperation::List, administrator)
                .await;
            (node, p.name, result)
        }))
        .buffer_unordered(8);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
        let mut machines = vec![];
        let mut unreachable = vec![];
        let mut bytes = 0;
        let mut truncated = false;
        while let Ok(Some((node, name, result))) =
            tokio::time::timeout_at(deadline, requests.next()).await
        {
            waiting.remove(&node);
            match result {
                Ok(result) => {
                    let entry = json!({"node":node,"name":name,"workspaces":result["workspaces"]});
                    bytes += serde_json::to_vec(&entry)?.len();
                    if bytes < MAX_FRAME / 2 {
                        machines.push(entry);
                    } else {
                        truncated = true;
                    }
                }
                Err(error) => {
                    unreachable.push(json!({"node":node,"name":name,"error":error.to_string()}))
                }
            }
        }
        for node in waiting {
            unreachable.push(json!({"node":node,"error":"discovery deadline exceeded"}));
        }
        Ok(json!({"machines":machines,"unreachable":unreachable,"truncated":truncated}))
    }
    fn ticket(&self) -> Ticket {
        Ticket {
            version: 1,
            name: self.name.clone(),
            address: self.endpoint.addr(),
        }
    }
    async fn join(&self, code: String) -> Result<Value> {
        let invitation = crate::network::decode(&code)?;
        ensure!(
            invitation.inviter.address.id != self.endpoint.id(),
            "cannot join this machine's own invitation"
        );
        let existing = self.db.run(|db| db.network()).await?;
        ensure!(
            existing.is_none_or(|network| network
                .members
                .iter()
                .any(|member| member.address.id == invitation.inviter.address.id)),
            "machine already belongs to a network; use a separate data directory for another network"
        );
        let response = self
            .remote(
                invitation.inviter.clone(),
                RemoteRequest::Join {
                    secret: invitation.secret,
                    member: self.ticket(),
                },
            )
            .await?;
        let network: Network = serde_json::from_value(response)?;
        ensure!(
            network
                .members
                .iter()
                .any(|p| p.address.id == invitation.inviter.address.id)
                && network
                    .members
                    .iter()
                    .any(|p| p.address.id == self.endpoint.id()),
            "incomplete network membership"
        );
        let members = self
            .db
            .run(move |db| Ok(db.merge_network(network)?.members.len()))
            .await?;
        self.changed.notify_one();
        Ok(json!({"joined":true,"machines":members,"inviter":invitation.inviter.name}))
    }
    async fn discover(&self, node: Option<String>, after: Option<String>) -> Result<Value> {
        ensure!(
            after.is_none() || node.is_some(),
            "directory cursor requires a node"
        );
        if let Some(ref node) = node {
            node.parse::<iroh::EndpointId>()?;
        }
        let local_id = self.endpoint.id().to_string();
        let local = if node.as_ref().is_none_or(|n| n == &local_id) {
            let after = after.clone();
            self.db.run(move |db| db.directory(after)).await?
        } else {
            DirectoryPage {
                sessions: vec![],
                next_cursor: None,
            }
        };
        let mut peers = self.db.run(|db| db.peers()).await?;
        if let Some(ref node) = node {
            peers.retain(|p| p.address.id.to_string() == *node);
            ensure!(node == &local_id || !peers.is_empty(), "node is not paired");
        }
        let mut result = json!({"sessions":local.sessions,"unreachable":[],"continuations":[]});
        if let Some(cursor) = local.next_cursor {
            result["continuations"]
                .as_array_mut()
                .unwrap()
                .push(json!({"node":local_id,"after":cursor}));
        }
        let mut bytes = serde_json::to_vec(&result)?.len();
        let mut waiting = peers
            .iter()
            .map(|p| p.address.id.to_string())
            .collect::<std::collections::BTreeSet<_>>();
        let cursor = after.clone();
        let mut requests = stream::iter(peers.into_iter().map(|peer| {
            let after = cursor.clone();
            async move {
                let id = peer.address.id.to_string();
                let reply = async {
                    let page: DirectoryPage = serde_json::from_value(
                        self.remote(peer, RemoteRequest::Directory { after })
                            .await?,
                    )?;
                    ensure!(
                        page.sessions.len() <= 200
                            && page.sessions.iter().all(|s| split_address(&s.address)
                                .is_ok_and(|(node, sid)| node == id && sid == s.id)),
                        "peer returned invalid session addresses"
                    );
                    Ok::<_, anyhow::Error>(page)
                }
                .await;
                (id, reply)
            }
        }))
        .buffer_unordered(8);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
        while let Ok(Some((id, reply))) = tokio::time::timeout_at(deadline, requests.next()).await {
            waiting.remove(&id);
            match reply {
                Ok(page) => {
                    let mut last = after.clone();
                    let mut clipped = false;
                    for session in page.sessions {
                        let value = serde_json::to_value(&session)?;
                        let size = serde_json::to_vec(&value)?.len() + 1;
                        if bytes + size > MAX_FRAME / 2 {
                            clipped = true;
                            break;
                        }
                        bytes += size;
                        last = Some(session.id);
                        result["sessions"].as_array_mut().unwrap().push(value);
                    }
                    let next = if clipped {
                        Some(last)
                    } else {
                        page.next_cursor.map(Some)
                    };
                    if let Some(next) = next {
                        result["continuations"]
                            .as_array_mut()
                            .unwrap()
                            .push(json!({"node":id,"after":next}));
                    }
                }
                Err(error) => result["unreachable"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"node":id,"error":error.to_string()})),
            }
        }
        for id in waiting {
            result["unreachable"]
                .as_array_mut()
                .unwrap()
                .push(json!({"node":id,"error":"discovery deadline exceeded"}));
        }
        Ok(result)
    }

    async fn handle(&self, request: Request) -> Result<Value> {
        match request {
            Request::NetworkRefresh { node } => {
                let peer = self.resolve_machine(&node).await?;
                let network = self
                    .db
                    .run(|db| db.network())
                    .await?
                    .context("network membership required")?;
                self.sync_peer(peer, network).await?;
                Ok(json!({"synced":true}))
            }
            Request::WorkspaceAdmin { mut command } => {
                match &mut command {
                    WorkspaceAdmin::Grant { peer, .. } | WorkspaceAdmin::Revoke { peer, .. } => {
                        *peer = self.resolve_machine(peer).await?.address.id.to_string();
                    }
                    _ => (),
                }
                self.executor.admin(command).await
            }
            Request::WorkspaceList { node, lease } => {
                let administrator = lease.is_none();
                self.authorize_lease(lease).await?;
                self.workspaces(node, administrator).await
            }
            Request::WorkspaceCall {
                node,
                request,
                lease,
            } => {
                let administrator = lease.is_none();
                self.authorize_lease(lease).await?;
                self.workspace_call(node, request, administrator).await
            }
            Request::Shutdown => Ok(json!({"stopping":true})),
            Request::RelayCheck => {
                tokio::time::timeout(Duration::from_secs(12),self.endpoint.online()).await.context("no relay handshake completed within 12 seconds; check relay URL, TLS trust and protocol version")?;
                Ok(json!({"connected":true,"address":self.endpoint.addr()}))
            }
            Request::Status => Ok(
                json!({"version":env!("CARGO_PKG_VERSION"),"node":self.endpoint.id().to_string(),"name":self.name,"pid":std::process::id(),"address":self.endpoint.addr(),"public_discovery":false,"public_dns_fallback":false}),
            ),
            Request::Ticket => {
                let ticket = Ticket {
                    version: 1,
                    name: self.name.clone(),
                    address: self.endpoint.addr(),
                };
                Ok(
                    json!({"ticket":URL_SAFE_NO_PAD.encode(serde_json::to_vec(&ticket)?),"node":self.endpoint.id().to_string()}),
                )
            }
            Request::PeerAdd { ticket } => {
                ensure!(ticket.len() < 32768, "ticket too large");
                let ticket: Ticket = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(ticket)?)?;
                self.db
                    .run(move |db| {
                        db.add_peer(ticket)?;
                        Ok(json!({"paired":true}))
                    })
                    .await
            }
            Request::Peers => {
                self.db
                    .run(|db| Ok(serde_json::to_value(db.peers()?)?))
                    .await
            }
            Request::InviteCreate { ttl_seconds } => {
                let ticket = self.ticket();
                let invitation = self
                    .db
                    .run(move |db| db.create_invite(ticket, ttl_seconds))
                    .await?;
                Ok(
                    json!({"code":crate::network::encode(&invitation)?,"expires_at":invitation.expires_at,"uses":1}),
                )
            }
            Request::Join { code } => self.join(code).await,
            Request::Directory { node, after } => self.discover(node, after).await,
            Request::Register { id, name, harness } => {
                self.db
                    .run(move |db| Ok(serde_json::to_value(db.register(id, name, harness)?)?))
                    .await
            }
            Request::Heartbeat { id, execution } => {
                self.db
                    .run(move |db| {
                        db.heartbeat(&id, &execution)?;
                        Ok(json!({"renewed":true}))
                    })
                    .await
            }
            Request::Retire { id, execution } => {
                self.db
                    .run(move |db| {
                        db.retire(&id, &execution)?;
                        Ok(json!({"retired":true}))
                    })
                    .await
            }
            Request::Attach { id, target } => {
                crate::wake::validate(&target)?;
                self.db
                    .run(move |db| {
                        db.attach(&id, target)?;
                        Ok(json!({"attached":true}))
                    })
                    .await
            }
            Request::Send(input) => {
                let result = self.db.run(move |db| db.enqueue(input)).await;
                self.changed.notify_one();
                result
            }
            Request::Inbox {
                id,
                execution,
                limit,
            } => {
                self.db
                    .run(move |db| db.inbox(&id, &execution, limit))
                    .await
            }
            Request::Ack {
                id,
                execution,
                messages,
            } => {
                self.db
                    .run(move |db| Ok(json!({"acknowledged":db.ack(&id,&execution,messages)?})))
                    .await
            }
            Request::Groups { after } => self.db.run(move |db| db.groups(after)).await,
            Request::GroupSet { name, members } => {
                self.db
                    .run(move |db| {
                        db.group_set(&name, members)?;
                        Ok(json!({"saved":true}))
                    })
                    .await
            }
            Request::Deliveries => self.db.run(|db| db.deliveries()).await,
        }
    }
    async fn remote(&self, mut peer: Ticket, request: RemoteRequest) -> Result<Value> {
        // A custom home relay map does not constrain relay URLs advertised by peers.
        // Filter every dial, including persisted tickets, invitations and membership sync.
        peer.address.addrs.retain(|address| match address {
            iroh::TransportAddr::Relay(url) => self.allowed_relay.as_ref() == Some(url),
            iroh::TransportAddr::Ip(_) => !self.relay_only,
            _ => false,
        });
        ensure!(
            !peer.address.addrs.is_empty(),
            "peer has no address permitted by the local transport policy"
        );
        tokio::time::timeout(Duration::from_secs(8), async {
            let conn = self.endpoint.connect(peer.address, ALPN).await?;
            let (mut send, mut recv) = conn.open_bi().await?;
            let bytes = serde_json::to_vec(&request)?;
            ensure!(bytes.len() <= MAX_FRAME, "request too large");
            send.write_all(&bytes).await?;
            send.finish()?;
            let response: Response = serde_json::from_slice(&recv.read_to_end(MAX_FRAME).await?)?;
            response.into_result()
        })
        .await
        .context("peer request timed out")?
    }
}

async fn accept_peers(state: Arc<State>) {
    let permits = Arc::new(Semaphore::new(64));
    while let Some(incoming) = state.endpoint.accept().await {
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            incoming.refuse();
            continue;
        };
        let state = state.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let result: Result<()> = async {
                let conn = tokio::time::timeout(Duration::from_secs(10), incoming).await??;
                let peer = conn.remote_id().to_string();
                let check = peer.clone();
                let trusted = state.db.run(move |db| db.trusted(&check)).await?;
                loop {
                    let (mut send, mut recv) =
                        match tokio::time::timeout(Duration::from_secs(30), conn.accept_bi()).await
                        {
                            Ok(Ok(stream)) => stream,
                            _ => break,
                        };
                    let result = tokio::time::timeout(Duration::from_secs(10), async {
                        let request: RemoteRequest =
                            serde_json::from_slice(&recv.read_to_end(MAX_FRAME).await?)?;
                        match request {
                            RemoteRequest::Workspace { request } => {
                                ensure!(trusted, "unpaired peer");
                                state.executor.handle(peer.clone(), request, false).await
                            }
                            RemoteRequest::Join { secret, member } => {
                                ensure!(
                                    member.address.id.to_string() == peer,
                                    "member identity does not match connection"
                                );
                                let own = state.ticket();
                                state
                                    .db
                                    .run(move |db| {
                                        let network = db.redeem_invite(&secret, member)?;
                                        Ok(serde_json::to_value(db.merge_network(Network {
                                            key: network.key,
                                            members: vec![own],
                                        })?)?)
                                    })
                                    .await
                            }
                            RemoteRequest::NetworkSync {
                                mut network,
                                member,
                            } => {
                                ensure!(
                                    member.address.id.to_string() == peer,
                                    "member identity does not match connection"
                                );
                                let own = state.ticket();
                                state
                                    .db
                                    .run(move |db| {
                                        let existing =
                                            db.network()?.context("network membership required")?;
                                        ensure!(
                                            crate::network::same_key(&existing.key, &network.key),
                                            "invalid network credential"
                                        );
                                        ensure!(
                                            network.members.len() <= 256,
                                            "too many network members"
                                        );
                                        network.members.retain(|p| {
                                            p.address.id != member.address.id
                                                && p.address.id != own.address.id
                                        });
                                        network.members.extend([member, own]);
                                        Ok(serde_json::to_value(db.merge_network(network)?)?)
                                    })
                                    .await
                            }
                            RemoteRequest::Directory { after } => {
                                ensure!(trusted, "unpaired peer");
                                state
                                    .db
                                    .run(move |db| Ok(serde_json::to_value(db.directory(after)?)?))
                                    .await
                            }
                            RemoteRequest::Deliver { message } => {
                                ensure!(trusted, "unpaired peer");
                                let peer = peer.clone();
                                state
                                    .db
                                    .run(move |db| {
                                        db.receive(&peer, message)?;
                                        Ok(json!({"stored":true}))
                                    })
                                    .await
                            }
                        }
                    })
                    .await
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("remote request timed out")));
                    send.write_all(&serde_json::to_vec(&Response::from_result(result))?)
                        .await?;
                    send.finish()?;
                }
                Ok(())
            }
            .await;
            if let Err(e) = result {
                eprintln!("peer connection failed: {e:#}");
            }
        });
    }
}

async fn delivery_loop(state: Arc<State>) {
    loop {
        if let Err(e) = deliver_pending(&state).await {
            eprintln!("delivery error: {e:#}");
        }
        tokio::select! {_=state.changed.notified()=>{},_=tokio::time::sleep(Duration::from_secs(1))=>{}}
    }
}

async fn deliver_pending(state: &State) -> Result<()> {
    let messages = state.db.run(|db| db.pending()).await?;
    let peers = state.db.run(|db| db.peers()).await?;
    let mut jobs = stream::iter(messages.into_iter().map(|message| {
        let peers = &peers;
        async move {
            let result: Result<()> = async {
                let (node, _) = split_address(&message.to)?;
                if node == state.endpoint.id().to_string() {
                    let local = message.clone();
                    let sender = node.to_string();
                    state.db.run(move |db| db.receive(&sender, local)).await?;
                } else {
                    let peer = peers
                        .iter()
                        .find(|p| p.address.id.to_string() == node)
                        .cloned()
                        .context("peer no longer paired")?;
                    state
                        .remote(
                            peer,
                            RemoteRequest::Deliver {
                                message: message.clone(),
                            },
                        )
                        .await?;
                }
                Ok(())
            }
            .await;
            state
                .db
                .run(move |db| db.finish_delivery(&message, result.err().map(|e| format!("{e:#}"))))
                .await
        }
    }))
    .buffer_unordered(16);
    while let Some(result) = jobs.next().await {
        result?;
    }
    Ok(())
}

async fn wake_loop(state: Arc<State>) {
    loop {
        let result: Result<()> = async {
            let batches = state.db.run(|db| db.claim_wake()).await?;
            let mut jobs = stream::iter(batches.into_iter().map(|(session, target, ids)| {
                let state = &state;
                async move {
                    let (outcome, error) = crate::wake::post(target, &ids).await;
                    if let Some(error) = error {
                        eprintln!("wake {session}: {outcome}: {error}");
                    }
                    state
                        .db
                        .run(move |db| db.finish_wake(&session, &ids, outcome))
                        .await
                }
            }))
            .buffer_unordered(8);
            while let Some(result) = jobs.next().await {
                result?;
            }
            Ok(())
        }
        .await;
        if let Err(e) = result {
            eprintln!("wake error: {e:#}");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn network_loop(state: Arc<State>) {
    loop {
        let own = state.ticket();
        if own.address.is_empty() {
            // Keep known routes during endpoint startup instead of gossiping an
            // empty self ticket before the authenticated relay handshake finishes.
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }
        let network = state
            .db
            .run(move |db| {
                let Some(network) = db.network()? else {
                    return Ok(None);
                };
                Ok(Some(db.merge_network(Network {
                    key: network.key,
                    members: vec![own],
                })?))
            })
            .await;
        if let Ok(Some(network)) = network {
            let peers: Vec<_> = network
                .members
                .iter()
                .filter(|p| p.address.id != state.endpoint.id())
                .cloned()
                .collect();
            let mut jobs = stream::iter(peers.into_iter().map(|peer| {
                let state = state.clone();
                let network = network.clone();
                async move { state.sync_peer(peer, network).await }
            }))
            .buffer_unordered(8);
            while jobs.next().await.is_some() {}
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
