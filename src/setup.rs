use crate::{daemon, local, protocol::Request};

struct ReapedChild(Option<std::process::Child>);

impl Drop for ReapedChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            std::thread::spawn(move || {
                if let Err(error) = child.wait() {
                    eprintln!("daemon child cleanup failed: {error}");
                }
            });
        }
    }
}
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs::OpenOptions,
    io::Write,
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

fn read(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    ensure!(
        !std::fs::symlink_metadata(path)?.file_type().is_symlink(),
        "configuration is a symlink; resolve its target explicitly before setup: {}",
        path.display()
    );
    Ok(Some(std::fs::read_to_string(path)?))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("configuration has no parent")?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".agentmesh-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if temporary.exists() {
        std::fs::remove_file(temporary)?;
    }
    result
}

fn entry(binary: &Path, dir: &Path, harness: &str) -> Value {
    json!({"command":binary,"args":["--data-dir",dir,"mcp","serve","--harness",harness]})
}

pub fn harness_binary(harness: &str) -> Option<PathBuf> {
    let variable = if harness == "tcodex" {
        "AGENTMESH_TCODEX_BINARY"
    } else {
        "AGENTMESH_CODEX_BINARY"
    };
    if let Some(path) = std::env::var_os(variable) {
        return Some(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|p| p.join(harness))
            .find(|p| p.is_file())
    }) {
        return Some(path);
    }
    let home = dirs::home_dir()?;
    let local = home.join(".local/bin").join(harness);
    if local.is_file() {
        return Some(local);
    }
    if harness == "codex" {
        let app = PathBuf::from(
            "/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex",
        );
        if app.is_file() {
            return Some(app);
        }
    }
    let mut versions = std::fs::read_dir(home.join(".nvm/versions/node"))
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect::<Vec<_>>();
    versions.sort();
    versions
        .into_iter()
        .rev()
        .map(|p| p.join("bin").join(harness))
        .find(|p| p.is_file())
}

fn resolved_path(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return Ok(path.canonicalize()?);
    }
    let parent = path.parent().context("configuration path has no parent")?;
    Ok(resolved_path(parent)?.join(path.file_name().context("invalid configuration path")?))
}

pub fn install(
    dir: &Path,
    config_root: Option<PathBuf>,
    name: Option<String>,
    dry_run: bool,
) -> Result<Value> {
    let root = config_root.unwrap_or_else(|| dirs::home_dir().expect("home directory unavailable"));
    let binary = dir.join("bin/agentmesh");
    let targets = [
        (root.join(".claude.json"), "claude"),
        (root.join(".codex/config.toml"), "codex"),
        (root.join(".tclaude/.claude.json"), "tclaude"),
        (root.join(".tcodex/config.toml"), "tcodex"),
    ];
    let mut plans = Vec::new();
    for (path, harness) in targets {
        let old = read(&path)?;
        let bytes = if harness.ends_with("claude") {
            prepare_claude(old.as_deref(), &binary, dir, harness)?
        } else {
            prepare_codex(old.as_deref(), &binary, dir, harness, &path)?
        };
        plans.push((path, harness, old, bytes));
    }
    for path in [
        root.join(".claude/settings.json"),
        root.join(".tclaude/settings.json"),
    ] {
        let old = read(&path)?;
        let mut settings: Value = old
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .context("invalid Claude settings JSON; setup made no changes")?
            .unwrap_or_else(|| json!({}));
        ensure!(settings.is_object(), "Claude settings must be an object");
        if settings.get("crossSessionInbound").is_none() {
            settings["crossSessionInbound"] = json!("accept");
        }
        if settings.get("permissions").is_none() {
            settings["permissions"] = json!({});
        }
        ensure!(
            settings["permissions"].is_object(),
            "Claude permissions must be an object"
        );
        if settings["permissions"].get("allow").is_none() {
            settings["permissions"]["allow"] = json!([]);
        }
        let allow = settings["permissions"]["allow"]
            .as_array_mut()
            .context("Claude permissions.allow must be an array")?;
        for tool in mailbox_tools() {
            let rule = json!(format!("mcp__agentmesh__{tool}"));
            if !allow.contains(&rule) {
                allow.push(rule);
            }
        }
        let bytes = serde_json::to_vec_pretty(&settings)?;
        plans.push((path, "inbound_policy", old, bytes));
    }
    let configurations = plans
        .iter()
        .map(|(path, harness, _, _)| json!({"path":path,"harnesses":[harness]}))
        .collect::<Vec<_>>();
    let mut result = json!({"dry_run":dry_run,"binary":binary,"configurations":configurations,"backups":[],"next":"Restart or reconnect MCP in existing sessions. New sessions register automatically."});
    if dry_run {
        return Ok(result);
    }
    daemon::private_dir(dir)?;
    std::fs::create_dir_all(binary.parent().unwrap())?;
    let current = std::env::current_exe()?;
    if current != binary {
        let staged = binary.with_extension(format!("{}.new", uuid::Uuid::new_v4()));
        std::fs::copy(&current, &staged)?;
        std::fs::rename(staged, &binary)?;
    }
    for (path, _, old, new) in plans {
        if old.as_ref().is_some_and(|s| s.as_bytes() == new) {
            continue;
        }
        if let Some(old) = old {
            let backup = path.with_extension(format!("agentmesh-backup-{}", uuid::Uuid::new_v4()));
            atomic_write(&backup, old.as_bytes())?;
            result["backups"]
                .as_array_mut()
                .unwrap()
                .push(json!(backup));
        }
        atomic_write(&path, &new)?;
    }
    let profile = dir.join("daemon.json");
    if !profile.exists() {
        let name = name.unwrap_or_else(|| {
            Command::new("hostname")
                .output()
                .ok()
                .filter(|r| r.status.success())
                .map(|r| String::from_utf8_lossy(&r.stdout).trim().to_string())
                .unwrap_or_else(|| "machine".into())
        });
        atomic_write(
            &profile,
            &serde_json::to_vec(&json!({"name":name,"bind_addr":"0.0.0.0:43291"}))?,
        )?;
    }
    Ok(result)
}

fn prepare_claude(old: Option<&str>, binary: &Path, dir: &Path, harness: &str) -> Result<Vec<u8>> {
    let mut document: Value = match old {
        Some(s) => {
            serde_json::from_str(s).context("invalid Claude-family JSON; setup made no changes")?
        }
        None => json!({}),
    };
    ensure!(
        document.is_object(),
        "Claude-family configuration must be an object"
    );
    if document.get("mcpServers").is_none() {
        document["mcpServers"] = json!({});
    }
    ensure!(
        document["mcpServers"].is_object(),
        "Claude-family mcpServers must be an object"
    );
    if let Some(existing) = document["mcpServers"].get("agentmesh") {
        ensure!(
            existing.get("command").and_then(Value::as_str) == binary.to_str(),
            "{harness} already has an unmanaged agentmesh entry; setup made no changes"
        );
    }
    document["mcpServers"]["agentmesh"] = entry(binary, dir, harness);
    Ok(serde_json::to_vec_pretty(&document)?)
}

fn prepare_codex(
    old: Option<&str>,
    binary: &Path,
    dir: &Path,
    harness: &str,
    config: &Path,
) -> Result<Vec<u8>> {
    let mut document = old
        .unwrap_or("")
        .parse::<toml_edit::DocumentMut>()
        .context("invalid Codex-family TOML; setup made no changes")?;
    ensure!(
        document
            .get("mcp_servers")
            .is_none_or(|item| item.as_table_like().is_some()),
        "Codex-family mcp_servers must be a table; setup made no changes"
    );
    if let Some(existing) = document
        .get("mcp_servers")
        .and_then(|table| table.get("agentmesh"))
    {
        ensure!(
            existing.get("command").and_then(toml_edit::Item::as_str) == binary.to_str(),
            "{harness} already has an unmanaged agentmesh entry; setup made no changes"
        );
    }
    document["mcp_servers"]["agentmesh"]["command"] =
        toml_edit::value(binary.to_string_lossy().as_ref());
    let mut args = toml_edit::Array::new();
    for value in [
        "--data-dir",
        dir.to_str().context("data directory must be UTF-8")?,
        "mcp",
        "serve",
        "--harness",
        harness,
    ] {
        args.push(value);
    }
    document["mcp_servers"]["agentmesh"]["args"] = toml_edit::value(args);
    if document["mcp_servers"]["agentmesh"]
        .get("required")
        .is_none()
    {
        document["mcp_servers"]["agentmesh"]["required"] = toml_edit::value(true);
    }
    ensure!(
        document["mcp_servers"]["agentmesh"]
            .get("env")
            .is_none_or(|v| v.as_table_like().is_some()),
        "Codex MCP env must be a table"
    );
    if let Some(native) = harness_binary(harness) {
        ensure!(
            native.is_absolute() && native.is_file(),
            "native harness binary must be an existing absolute path"
        );
        document["mcp_servers"]["agentmesh"]["env"]["AGENTMESH_CODEX_BINARY"] =
            toml_edit::value(native.to_string_lossy().as_ref());
    }
    document["mcp_servers"]["agentmesh"]["env"]["AGENTMESH_CODEX_HOME"] =
        toml_edit::value(config.parent().unwrap().to_string_lossy().as_ref());
    ensure!(
        document["mcp_servers"]["agentmesh"]
            .get("tools")
            .is_none_or(|v| v.as_table_like().is_some()),
        "Codex MCP tools must be a table"
    );
    for tool in mailbox_tools() {
        ensure!(
            document["mcp_servers"]["agentmesh"]
                .get("tools")
                .and_then(|t| t.get(tool))
                .is_none_or(|v| v.as_table_like().is_some()),
            "Codex MCP tool settings must be a table"
        );
        if document["mcp_servers"]["agentmesh"]
            .get("tools")
            .and_then(|t| t.get(tool))
            .and_then(|t| t.get("approval_mode"))
            .is_none()
        {
            document["mcp_servers"]["agentmesh"]["tools"][tool]["approval_mode"] =
                toml_edit::value("approve");
        }
    }
    ensure!(
        document
            .get("hooks")
            .is_none_or(|v| v.as_table_like().is_some()),
        "Codex hooks must be a table"
    );
    if document.get("hooks").is_none() {
        document["hooks"] = toml_edit::Item::Table(toml_edit::Table::new());
    } else if let Some(inline) = document["hooks"].as_inline_table() {
        document["hooks"] = toml_edit::Item::Table(inline.clone().into_table());
    }
    ensure!(
        document
            .get("features")
            .is_none_or(|v| v.as_table_like().is_some()),
        "Codex features must be a table"
    );
    ensure!(
        document
            .get("hooks")
            .and_then(|h| h.get("state"))
            .is_none_or(|v| v.as_table_like().is_some()),
        "Codex hook state must be a table"
    );
    if document
        .get("hooks")
        .and_then(|h| h.get("SessionStart"))
        .is_none()
    {
        document["hooks"]["SessionStart"] =
            toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    if let Some(values) = document["hooks"]["SessionStart"].as_array() {
        let mut groups = toml_edit::ArrayOfTables::new();
        for value in values.iter() {
            groups.push(
                value
                    .as_inline_table()
                    .context("Codex SessionStart entries must be tables")?
                    .clone()
                    .into_table(),
            );
        }
        document["hooks"]["SessionStart"] = toml_edit::Item::ArrayOfTables(groups);
    }
    let groups = document["hooks"]["SessionStart"]
        .as_array_of_tables_mut()
        .context("Codex SessionStart hooks must be an array of tables")?;
    let ours = "hooks = [{type = 'mcp_tool', server = 'agentmesh', tool = 'whoami', input = {}}]"
        .parse::<toml_edit::DocumentMut>()?;
    let index = groups.iter().position(|group| {
        group.len() == 1
            && group
                .get("hooks")
                .and_then(toml_edit::Item::as_array)
                .is_some_and(|hooks| {
                    hooks.len() == 1
                        && hooks
                            .get(0)
                            .and_then(toml_edit::Value::as_inline_table)
                            .is_some_and(|h| {
                                h.len() == 4
                                    && h.get("type").and_then(toml_edit::Value::as_str)
                                        == Some("mcp_tool")
                                    && h.get("server").and_then(toml_edit::Value::as_str)
                                        == Some("agentmesh")
                                    && h.get("tool").and_then(toml_edit::Value::as_str)
                                        == Some("whoami")
                                    && h.get("input")
                                        .and_then(toml_edit::Value::as_inline_table)
                                        .is_some_and(|i| i.is_empty())
                            })
                })
    });
    let index = index.unwrap_or_else(|| {
        let index = groups.len();
        groups.push(ours.as_table().clone());
        index
    });
    let key = format!(
        "{}:session_start:{index}:0",
        resolved_path(config)?.display()
    );
    // Codex hashes normalized hook identity as canonical JSON, with the default timeout resolved.
    // Only this installer-owned identity hook is admitted; unrelated hooks keep their trust state.
    let identity = json!({"event_name":"session_start","hooks":[{"type":"mcp_tool","server":"agentmesh","tool":"whoami","input":{},"timeout":600}]});
    let hash = format!("sha256:{}", crate::network::digest(&identity.to_string()));
    ensure!(
        document["hooks"]
            .get("state")
            .and_then(|s| s.get(&key))
            .is_none_or(|v| v.as_table_like().is_some()),
        "Codex hook state entry must be a table"
    );
    document["hooks"]["state"][&key]["trusted_hash"] = toml_edit::value(hash);
    document["hooks"]["state"][&key]["enabled"] = toml_edit::value(true);
    document["features"]["hooks"] = toml_edit::value(true);
    Ok(document.to_string().into_bytes())
}

fn mailbox_tools() -> [&'static str; 7] {
    [
        "whoami",
        "list_sessions",
        "check_inbox",
        "acknowledge_messages",
        "send_message",
        "list_groups",
        "set_group",
    ]
}

pub async fn ensure_daemon(dir: &Path) -> Result<()> {
    if local::call(dir, Request::Status)
        .await
        .is_ok_and(|status| status["version"] == env!("CARGO_PKG_VERSION"))
    {
        return Ok(());
    }
    let binary = dir.join("bin/agentmesh");
    ensure!(
        binary.is_file(),
        "daemon is unavailable; run agentmesh setup or daemon run"
    );
    let startup_lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(dir.join("startup.lock"))?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    loop {
        match fs2::FileExt::try_lock_exclusive(&startup_lock) {
            Ok(()) => break,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(50)).await
            }
            Err(error) => return Err(error).context("could not acquire daemon startup lock"),
        }
    }
    let profile: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("daemon.json"))?)?;
    let name = profile["name"]
        .as_str()
        .context("daemon profile missing name")?;
    ensure!(
        profile.get("bind_addr").is_none_or(|v| v.is_string()),
        "daemon profile bind_addr must be a string"
    );
    let address = profile["bind_addr"].as_str().unwrap_or("0.0.0.0:43291");
    crate::protocol::validate_text(name, "machine name", 128)?;
    address
        .parse::<std::net::SocketAddr>()
        .context("invalid daemon bind address")?;
    for field in ["relay_url", "relay_token_file"] {
        ensure!(
            profile.get(field).is_none_or(|v| v.is_string()),
            "daemon profile {field} must be a string"
        );
    }
    for field in ["no_relay", "relay_only"] {
        ensure!(
            profile.get(field).is_none_or(|v| v.is_boolean()),
            "daemon profile {field} must be a boolean"
        );
    }
    if let Some(url) = profile["relay_url"].as_str() {
        url.parse::<iroh::RelayUrl>().context("invalid relay URL")?;
    }
    ensure!(
        !(profile["no_relay"].as_bool() == Some(true)
            && (profile.get("relay_url").is_some()
                || profile["relay_only"].as_bool() == Some(true))),
        "no_relay conflicts with relay settings"
    );
    ensure!(
        profile.get("relay_token_file").is_none() || profile.get("relay_url").is_some(),
        "relay token requires relay URL"
    );
    if let Some(path) = profile["relay_token_file"].as_str() {
        daemon::read_relay_token(Path::new(path))?;
    }
    ensure!(
        std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&binary)?.permissions()) & 0o111
            != 0,
        "installed daemon binary is not executable"
    );
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(dir.join("daemon.log"))?;
    let mut command = Command::new(binary);
    command.args([
        "--data-dir",
        dir.to_str().context("invalid data directory")?,
        "daemon",
        "run",
        "--name",
        name,
        "--bind-addr",
        address,
    ]);
    for (field, flag) in [
        ("relay_url", "--relay-url"),
        ("relay_token_file", "--relay-token-file"),
    ] {
        if let Some(value) = profile[field].as_str() {
            command.args([flag, value]);
        }
    }
    for (field, flag) in [("no_relay", "--no-relay"), ("relay_only", "--relay-only")] {
        if profile[field].as_bool() == Some(true) {
            command.arg(flag);
        }
    }
    command
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0);
    if let Ok(status) = local::call(dir, Request::Status).await {
        if status["version"] == env!("CARGO_PKG_VERSION") {
            return Ok(());
        }
        local::call(dir, Request::Shutdown)
            .await
            .context("could not stop previous daemon version")?;
        for _ in 0..100 {
            if local::call(dir, Request::Status).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        ensure!(
            local::call(dir, Request::Status).await.is_err(),
            "previous daemon has not stopped; retry setup"
        );
    }
    let mut child = ReapedChild(Some(
        command.spawn().context("failed to start local daemon")?,
    ));
    for _ in 0..100 {
        if local::call(dir, Request::Status).await.is_ok() {
            return Ok(());
        }
        let _status = child.0.as_mut().unwrap().try_wait()?;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if child.0.as_mut().unwrap().try_wait()?.is_none() {
        child.0.as_mut().unwrap().kill()?;
    }
    anyhow::bail!("daemon startup timed out; inspect daemon.log")
}
