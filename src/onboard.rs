//! Portable private enrollment: relay bootstrap followed by an authenticated invitation.
use crate::{daemon, local, network, protocol::*, setup};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    net::IpAddr,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bootstrap {
    version: u8,
    invitation: Invitation,
    relay: Relay,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Relay {
    url: iroh::RelayUrl,
    token: Option<String>,
    resolve: Option<IpAddr>,
}

impl Bootstrap {
    fn validate(&self) -> Result<()> {
        ensure!(self.version == 1, "unsupported bootstrap version");
        network::decode(&network::encode(&self.invitation)?)?;
        let url = &self.relay.url;
        let loopback = url.host_str().is_some_and(|host| {
            host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
        });
        ensure!(
            url.scheme() == "https" || (url.scheme() == "http" && loopback),
            "bootstrap relay requires HTTPS (HTTP is limited to loopback)"
        );
        ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "relay URL cannot contain credentials, query or fragment"
        );
        ensure!(
            self.invitation
                .inviter
                .address
                .relay_urls()
                .any(|peer_url| peer_url == url),
            "inviter does not advertise the bootstrap relay"
        );
        if let Some(token) = &self.relay.token {
            ensure!(
                !token.is_empty() && token.len() <= 4096 && !token.contains(['\r', '\n', '\0']),
                "invalid bootstrap relay credential"
            );
        }
        if let Some(address) = self.relay.resolve {
            crate::relay_dns::pinned(url, address)?;
        }
        Ok(())
    }
}

pub async fn invitation(dir: &Path, ttl_seconds: u64) -> Result<Value> {
    let profile: Value = serde_json::from_slice(&std::fs::read(dir.join("daemon.json"))?)?;
    ensure!(
        profile["no_relay"].as_bool() != Some(true),
        "bootstrap invitations require an enabled self-hosted relay"
    );
    let url = profile["relay_url"]
        .as_str()
        .context("configure a self-hosted relay before creating a bootstrap invitation")?
        .parse()?;
    let resolve = profile["relay_resolve"]
        .as_str()
        .map(str::parse)
        .transpose()?;
    ensure!(
        profile.get("dns_server").is_none() || resolve.is_some(),
        "custom DNS is not portable; configure relay_resolve before creating a bootstrap invitation"
    );
    let token = profile["relay_token_file"]
        .as_str()
        .map(|path| daemon::read_relay_token(Path::new(path)))
        .transpose()?;
    let response = local::call(dir, Request::InviteCreate { ttl_seconds }).await?;
    let invitation = network::decode(
        response["code"]
            .as_str()
            .context("invalid invitation response")?,
    )?;
    let bundle = Bootstrap {
        version: 1,
        invitation,
        relay: Relay {
            url,
            token,
            resolve,
        },
    };
    bundle.validate()?;
    let code = format!(
        "amb1_{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&bundle)?)
    );
    ensure!(
        code.len() <= 8192,
        "bootstrap invitation exceeds size limit"
    );
    Ok(
        json!({"code":code,"expires_at":bundle.invitation.expires_at,"uses":1,"private":true,"includes":"relay credentials and a dedicated workspace grant to the inviting machine"}),
    )
}

pub async fn join(
    dir: &Path,
    config_root: Option<PathBuf>,
    name: Option<String>,
    code: &str,
    service: bool,
) -> Result<Value> {
    ensure!(
        code.trim().len() <= 8192,
        "bootstrap invitation exceeds size limit"
    );
    let encoded = code
        .trim()
        .strip_prefix("amb1_")
        .context("expected an amb1_ bootstrap invitation")?;
    let raw = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| anyhow::anyhow!("invalid bootstrap encoding"))?;
    let bundle: Bootstrap =
        serde_json::from_slice(&raw).map_err(|_| anyhow::anyhow!("invalid bootstrap format"))?;
    bundle.validate()?;
    let home = config_root
        .clone()
        .or_else(dirs::home_dir)
        .context("home directory unavailable")?;
    let profile_path = dir.join("daemon.json");
    let profile = if profile_path.exists() {
        let profile: Value = serde_json::from_slice(&std::fs::read(&profile_path)?)?;
        ensure!(
            profile["relay_url"].as_str().is_some_and(|url| url
                .parse::<iroh::RelayUrl>()
                .is_ok_and(|url| url == bundle.relay.url)),
            "existing installation uses a different relay; use a separate data directory"
        );
        let previous = profile["relay_token_file"]
            .as_str()
            .map(|path| daemon::read_relay_token(Path::new(path)))
            .transpose()?;
        ensure!(
            previous == bundle.relay.token,
            "existing installation uses another relay credential"
        );
        ensure!(
            profile["relay_only"].as_bool() == Some(true)
                && profile["no_relay"].as_bool() != Some(true)
                && profile.get("dns_server").is_none()
                && profile["relay_resolve"]
                    .as_str()
                    .map(str::parse::<IpAddr>)
                    .transpose()?
                    == bundle.relay.resolve,
            "existing relay transport differs; reconfigure it explicitly or use a separate data directory"
        );
        ensure!(
            name.as_ref()
                .is_none_or(|name| profile["name"].as_str() == Some(name.as_str())),
            "existing machine name differs; rename it explicitly"
        );
        profile
    } else {
        json!({"name":name.clone().unwrap_or_else(||"machine".into()),"bind_addr":"0.0.0.0:43291"})
    };
    local::private_dir(dir)?;
    let mut profile = profile;
    if let Some(name) = name {
        validate_text(&name, "machine name", 128)?;
        profile["name"] = json!(name);
    } else if !profile_path.exists() {
        profile["name"] = json!(
            std::env::var("HOSTNAME")
                .ok()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| format!("machine-{}", &uuid::Uuid::new_v4().to_string()[..8]))
        );
    }
    profile["relay_url"] = json!(bundle.relay.url);
    profile["relay_only"] = json!(true);
    if let Some(token) = &bundle.relay.token {
        let path = dir.join("relay-token");
        setup::atomic_write(&path, token.as_bytes())?;
        profile["relay_token_file"] = json!(path);
    }
    if let Some(address) = bundle.relay.resolve {
        profile["relay_resolve"] = json!(address);
    } else {
        profile.as_object_mut().unwrap().remove("relay_resolve");
    }
    // Existing harness configurations are validated by setup before any of them change.
    setup::install(dir, config_root, None, false)?;
    setup::atomic_write(&profile_path, &serde_json::to_vec_pretty(&profile)?)?;
    setup::ensure_daemon(dir).await?;
    // Enrollment must advertise a usable relay address to the controller, not an
    // empty ticket captured while the first relay handshake is still starting.
    local::call(dir, Request::RelayCheck).await?;
    let mut result = local::call(
        dir,
        Request::Join {
            code: network::encode(&bundle.invitation)?,
        },
    )
    .await?;
    let installed_service = if service {
        crate::user_service::install(dir, &home, &profile).await?
    } else {
        json!({"enabled":false,"reason":"disabled by --no-service"})
    };
    if installed_service["enabled"] == true {
        // Service handoff starts a new endpoint. Refresh the inviter's ticket only
        // after that endpoint has its relay address, before reporting readiness.
        local::call(dir, Request::RelayCheck).await?;
        local::call(
            dir,
            Request::NetworkRefresh {
                node: bundle.invitation.inviter.address.id.to_string(),
            },
        )
        .await?;
    }
    let workspace = home.join("agentmesh-workspace");
    local::private_dir(&workspace)?;
    local::call(
        dir,
        Request::WorkspaceAdmin {
            command: WorkspaceAdmin::Add {
                name: "workspace".into(),
                path: workspace
                    .to_str()
                    .context("workspace path must be UTF-8")?
                    .into(),
            },
        },
    )
    .await?;
    let controller = bundle.invitation.inviter.address.id.to_string();
    local::call(
        dir,
        Request::WorkspaceAdmin {
            command: WorkspaceAdmin::Grant {
                name: "workspace".into(),
                peer: controller.clone(),
                write: true,
                execute: true,
            },
        },
    )
    .await?;
    result["workspace"] = json!({"name":"workspace","root":workspace,"controller":controller});
    result["service"] = installed_service;
    result["next"] =
        json!("Launch claude, tclaude, codex or tcodex normally; reconnect existing MCP sessions.");
    Ok(result)
}
