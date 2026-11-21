//! Optional OS user services. Containers without a service manager retain MCP lazy startup.
use crate::{local, protocol::Request, setup};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    path::Path,
    process::{Command, Stdio},
};

fn active(command: &str, args: &[&str]) -> bool {
    Command::new(command)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}
fn command(command: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(command)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()?;
    ensure!(status.success(), "user service command failed: {command}");
    Ok(())
}
fn quote(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t")
    )
}
fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn managed_pid(manager: &str, target: &str) -> Option<u64> {
    let args = if manager == "systemctl" {
        vec!["--user", "show", "--property=MainPID", "--value", target]
    } else {
        vec!["print", target]
    };
    let output = Command::new(manager)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let value = if manager == "systemctl" {
        text.trim()
    } else {
        text.lines()
            .find_map(|line| line.trim().strip_prefix("pid = "))?
    };
    value.parse::<u64>().ok().filter(|pid| *pid > 0)
}

pub async fn install(dir: &Path, home: &Path, profile: &Value) -> Result<Value> {
    let data = dir.to_str().context("data directory must be UTF-8")?;
    let binary = dir.join("bin/agentmesh");
    let binary = binary.to_str().context("binary path must be UTF-8")?;
    let mut args = vec![
        binary.to_owned(),
        "--data-dir".into(),
        data.into(),
        "daemon".into(),
        "run".into(),
        "--name".into(),
        profile["name"]
            .as_str()
            .context("profile has no name")?
            .into(),
    ];
    for field in [
        "bind_addr",
        "relay_url",
        "relay_token_file",
        "relay_resolve",
        "dns_server",
    ] {
        if let Some(value) = profile[field].as_str() {
            args.extend([format!("--{}", field.replace('_', "-")), value.into()]);
        }
    }
    if profile["relay_only"].as_bool() == Some(true) {
        args.push("--relay-only".into());
    }
    let suffix = &crate::network::digest(data)[..12];
    if cfg!(target_os = "linux") && active("systemctl", &["--user", "is-active", "basic.target"]) {
        let label = format!("agentmesh-{suffix}.service");
        let path = home.join(".config/systemd/user").join(&label);
        let content = format!(
            "[Unit]\nDescription=Agentmesh peer daemon\n\n[Service]\nType=simple\nExecStart={}\nRestart=on-failure\nRestartSec=3\nUMask=0077\n\n[Install]\nWantedBy=default.target\n",
            args.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" ")
        );
        setup::atomic_write(&path, content.as_bytes())?;
        command("systemctl", &["--user", "daemon-reload"])?;
        command(
            "systemctl",
            &[
                "--user",
                "enable",
                path.to_str().context("invalid service path")?,
            ],
        )?;
        let running = local::call(dir, Request::Status).await?["pid"].as_u64();
        if managed_pid("systemctl", &label) != running {
            stop(dir).await?;
            command("systemctl", &["--user", "restart", &label])?;
        }
        ready(dir, "systemctl", &label).await?;
        return Ok(json!({"enabled":true,"kind":"systemd-user","name":label}));
    }
    if cfg!(target_os = "macos") {
        let domain = format!("gui/{}", unsafe { libc::geteuid() });
        if active("launchctl", &["print", &domain]) {
            let label = format!("com.agentmesh.{suffix}");
            let path = home
                .join("Library/LaunchAgents")
                .join(format!("{label}.plist"));
            let content = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array>{}</array><key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict><key>ThrottleInterval</key><integer>5</integer><key>StandardOutPath</key><string>{}</string><key>StandardErrorPath</key><string>{}</string></dict></plist>",
                xml(&label),
                args.iter()
                    .map(|s| format!("<string>{}</string>", xml(s)))
                    .collect::<String>(),
                xml(&dir.join("service.log").to_string_lossy()),
                xml(&dir.join("service.log").to_string_lossy())
            );
            setup::atomic_write(&path, content.as_bytes())?;
            let target = format!("{domain}/{label}");
            let running = local::call(dir, Request::Status).await?["pid"].as_u64();
            if managed_pid("launchctl", &target) != running {
                stop(dir).await?;
                if active("launchctl", &["print", &target]) {
                    command("launchctl", &["kickstart", "-k", &target])?;
                } else {
                    command(
                        "launchctl",
                        &[
                            "bootstrap",
                            &domain,
                            path.to_str().context("invalid service path")?,
                        ],
                    )?;
                }
            }
            ready(dir, "launchctl", &target).await?;
            return Ok(json!({"enabled":true,"kind":"launchd-user","name":label}));
        }
    }
    Ok(
        json!({"enabled":false,"reason":"user service manager unavailable; MCP starts the daemon automatically"}),
    )
}

async fn stop(dir: &Path) -> Result<()> {
    local::call(dir, Request::Shutdown).await?;
    for _ in 0..100 {
        if local::call(dir, Request::Status).await.is_err() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    anyhow::bail!("daemon did not stop before user service installation")
}

async fn ready(dir: &Path, manager: &str, target: &str) -> Result<()> {
    // Only the OS service manager may start the replacement. Lazy startup here
    // could race it and accidentally leave an unmanaged daemon holding the lock.
    for _ in 0..200 {
        if local::call(dir, Request::Status).await.is_ok_and(|status| {
            status["version"] == env!("CARGO_PKG_VERSION")
                && status["pid"]
                    .as_u64()
                    .is_some_and(|pid| managed_pid(manager, target) == Some(pid))
        }) {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    anyhow::bail!("user service did not become ready; inspect its service log")
}
