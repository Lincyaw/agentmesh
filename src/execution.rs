//! Explicit workspace grants and executor-owned durable jobs, independent of mail delivery.
use crate::protocol::{WorkspaceAdmin as Admin, WorkspaceOperation as Operation};
use crate::{protocol::*, workspace_files as files};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    sync::Notify,
};

const OUTPUT_LIMIT: u64 = 1024 * 1024;

#[derive(Clone)]
pub struct Executor {
    db: Arc<Mutex<Connection>>,
    dir: PathBuf,
    // Serialize grant changes, file changes, and process admission. Running processes
    // and their output never hold this lock.
    gate: Arc<tokio::sync::Mutex<()>>,
    running: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
    stopping: Arc<AtomicBool>,
}

struct Workspace {
    id: String,
    root: String,
    write: bool,
    execute: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct Submission {
    workspace: String,
    path: String,
    command: String,
    request_id: String,
    timeout_seconds: u64,
}

impl Executor {
    pub fn open(dir: &Path) -> Result<Self> {
        let db = Connection::open(dir.join("execution.sqlite"))?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS workspaces(id TEXT PRIMARY KEY,name TEXT NOT NULL UNIQUE,root TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS grants(workspace TEXT NOT NULL,peer TEXT NOT NULL,write_access INTEGER NOT NULL,execute_access INTEGER NOT NULL,PRIMARY KEY(workspace,peer));
            CREATE TABLE IF NOT EXISTS jobs(id TEXT PRIMARY KEY,peer TEXT NOT NULL,workspace TEXT NOT NULL,request_id TEXT NOT NULL,submission TEXT NOT NULL,status TEXT NOT NULL,created INTEGER NOT NULL,finished INTEGER,exit_code INTEGER,error TEXT,stdout_truncated INTEGER NOT NULL DEFAULT 0,stderr_truncated INTEGER NOT NULL DEFAULT 0,UNIQUE(peer,request_id));
            UPDATE jobs SET status='interrupted',finished=strftime('%s','now')*1000,error='executor restarted; command was not replayed' WHERE status IN ('starting','running','cancelling');")?;
        let output = dir.join("jobs");
        crate::local::private_dir(&output)?;
        Ok(Self {
            db: Arc::new(Mutex::new(db)),
            dir: output,
            gate: Default::default(),
            running: Default::default(),
            stopping: Default::default(),
        })
    }

    async fn database<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = db
                .lock()
                .map_err(|_| anyhow::anyhow!("execution database lock poisoned"))?;
            f(&mut guard)
        })
        .await?
    }

    pub async fn admin(&self, operation: Admin) -> Result<Value> {
        let _guard = self.gate.lock().await;
        let this = self.clone();
        self.database(move |db| match operation {
            Admin::Add { name, path } => {
                validate_text(&name, "workspace name", 128)?;
                ensure!(Path::new(&path).is_absolute(), "workspace path must be absolute");
                let root = std::fs::canonicalize(&path)?.to_str().context("workspace path must be UTF-8")?.to_owned();
                files::directory(&root, ".")?;
                let existing: Option<String> = db.query_row("SELECT root FROM workspaces WHERE name=?", [&name], |r| r.get(0)).optional()?;
                if let Some(existing) = existing {
                    ensure!(existing == root, "workspace name already refers to another root; remove it first");
                } else {
                    let count: i64 = db.query_row("SELECT COUNT(*) FROM workspaces", [], |r| r.get(0))?;
                    ensure!(count < 32, "at most 32 workspaces are supported");
                    db.execute("INSERT INTO workspaces VALUES(?,?,?)", params![uuid::Uuid::new_v4().to_string(), name, root])?;
                }
                Ok(json!({"name":name,"root":root,"saved":true}))
            }
            Admin::List => {
                let mut query = db.prepare("SELECT id,name,root FROM workspaces ORDER BY name")?;
                let rows = query.query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
                let mut workspaces = vec![];
                for (id,name,root) in rows {
                    let mut grants = db.prepare("SELECT peer,write_access,execute_access FROM grants WHERE workspace=? ORDER BY peer")?;
                    let grants = grants.query_map([id], |r| Ok(json!({"peer":r.get::<_,String>(0)?,"read":true,"write":r.get::<_,bool>(1)?,"execute":r.get::<_,bool>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
                    workspaces.push(json!({"name":name,"root":root,"grants":grants}));
                }
                Ok(json!({"workspaces":workspaces}))
            }
            Admin::Grant { name, peer, write, execute } => {
                let ws = workspace(db, &peer, &name, true)?;
                db.execute("INSERT INTO grants VALUES(?,?,?,?) ON CONFLICT(workspace,peer) DO UPDATE SET write_access=excluded.write_access,execute_access=excluded.execute_access", params![ws.id,peer,write,execute])?;
                if !execute { this.cancel_scope(db, &ws.id, Some(&peer))?; }
                Ok(json!({"name":name,"peer":peer,"read":true,"write":write,"execute":execute}))
            }
            Admin::Revoke { name, peer } => {
                let ws = workspace(db, &peer, &name, true)?;
                db.execute("DELETE FROM grants WHERE workspace=? AND peer=?", params![ws.id,peer])?;
                this.cancel_scope(db, &ws.id, Some(&peer))?;
                Ok(json!({"revoked":true}))
            }
            Admin::Remove { name } => {
                let ws = workspace(db, "", &name, true)?;
                this.cancel_scope(db, &ws.id, None)?;
                let tx = db.transaction()?;
                tx.execute("DELETE FROM grants WHERE workspace=?", [&ws.id])?;
                tx.execute("DELETE FROM workspaces WHERE id=?", [&ws.id])?;
                tx.commit()?;
                Ok(json!({"removed":true}))
            }
            Admin::Prune => {
                let mut query = db.prepare("SELECT id FROM jobs WHERE status NOT IN ('starting','running','cancelling')")?;
                let ids = query.query_map([], |r| r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                drop(query);
                for id in &ids {
                    for stream in ["stdout","stderr"] {
                        let path = this.dir.join(format!("{id}.{stream}"));
                        match std::fs::remove_file(path) { Ok(()) => (), Err(e) if e.kind() == std::io::ErrorKind::NotFound => (), Err(e) => return Err(e.into()) }
                    }
                    db.execute("DELETE FROM jobs WHERE id=?", [id])?;
                }
                Ok(json!({"pruned":ids.len(),"warning":"pruned request IDs may execute again if reused"}))
            }
        }).await
    }

    fn cancel_scope(&self, db: &Connection, workspace: &str, peer: Option<&str>) -> Result<()> {
        let mut query = db.prepare("SELECT id FROM jobs WHERE workspace=? AND (? IS NULL OR peer=?) AND status IN ('starting','running','cancelling')")?;
        let ids = query
            .query_map(params![workspace, peer, peer], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for id in ids {
            self.signal_cancel(db, &id)?;
        }
        Ok(())
    }

    fn signal_cancel(&self, db: &Connection, id: &str) -> Result<()> {
        if let Some(notify) = self
            .running
            .lock()
            .map_err(|_| anyhow::anyhow!("job registry poisoned"))?
            .get(id)
        {
            db.execute("UPDATE jobs SET status='cancelling' WHERE id=? AND status IN ('starting','running')", [id])?;
            notify.notify_one();
        }
        Ok(())
    }

    pub async fn handle(
        &self,
        peer: String,
        operation: Operation,
        administrator: bool,
    ) -> Result<Value> {
        if let Operation::Execute {
            workspace,
            path,
            command,
            request_id,
            timeout_seconds,
        } = operation
        {
            // Dropping an RPC after a timeout must not cancel admission halfway through.
            let this = self.clone();
            return tokio::spawn(async move {
                this.start(
                    peer,
                    Submission {
                        workspace,
                        path,
                        command,
                        request_id,
                        timeout_seconds,
                    },
                    administrator,
                )
                .await
            })
            .await?;
        }
        let _guard = self.gate.lock().await;
        let this = self.clone();
        self.database(move |db| match operation {
            Operation::List => {
                let mut query = db.prepare("SELECT w.name,COALESCE(g.write_access,0),COALESCE(g.execute_access,0) FROM workspaces w LEFT JOIN grants g ON g.workspace=w.id AND g.peer=? WHERE ? OR g.peer IS NOT NULL ORDER BY w.name")?;
                let items = query.query_map(params![peer,administrator], |r| Ok(json!({"name":r.get::<_,String>(0)?,"read":true,"write":administrator || r.get::<_,bool>(1)?,"execute":administrator || r.get::<_,bool>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(json!({"workspaces":items}))
            }
            Operation::ListFiles { workspace: name, path } => files::list(&workspace(db,&peer,&name,administrator)?.root,&path),
            Operation::ReadFile { workspace: name, path } => files::read(&workspace(db,&peer,&name,administrator)?.root,&path),
            Operation::Search { workspace: name, path, query } => files::search(&workspace(db,&peer,&name,administrator)?.root,&path,&query),
            Operation::WriteFile { workspace: name, path, content, expected_sha256 } => {
                let ws = workspace(db,&peer,&name,administrator)?;
                ensure!(ws.write, "workspace write permission required");
                files::write(&ws.root,&path,&content,expected_sha256.as_deref())
            }
            Operation::Jobs { workspace: name } => {
                let ws = workspace(db,&peer,&name,administrator)?;
                let mut query = db.prepare("SELECT id FROM jobs WHERE workspace=? AND (? OR peer=?) ORDER BY created DESC,id LIMIT 100")?;
                let ids = query.query_map(params![ws.id,administrator,peer], |r| r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                let mut jobs = vec![];
                let mut bytes = 0;
                let mut truncated = ids.len() == 100;
                for id in ids {
                    let job = job_summary(db,&id)?;
                    bytes += serde_json::to_vec(&job)?.len();
                    if bytes > MAX_FRAME / 2 { truncated = true; break; }
                    jobs.push(job);
                }
                Ok(json!({"jobs":jobs,"limit":100,"truncated":truncated}))
            }
            Operation::Job { job, stdout_cursor, stderr_cursor } => {
                authorize_job(db,&peer,&job,administrator)?;
                let mut summary = job_summary(db,&job)?;
                let complete = !matches!(summary["status"].as_str(),Some("starting" | "running" | "cancelling"));
                summary["stdout"] = output(&this.dir,&job,"stdout",stdout_cursor,complete)?;
                summary["stderr"] = output(&this.dir,&job,"stderr",stderr_cursor,complete)?;
                Ok(summary)
            }
            Operation::CancelJob { job } => {
                authorize_job(db,&peer,&job,administrator)?;
                this.signal_cancel(db,&job)?;
                job_summary(db,&job)
            }
            Operation::Execute { .. } => unreachable!(),
        }).await
    }

    async fn start(&self, peer: String, input: Submission, administrator: bool) -> Result<Value> {
        let _guard = self.gate.lock().await;
        ensure!(
            !self.stopping.load(Ordering::SeqCst),
            "executor is stopping"
        );
        validate_text(&input.command, "command", 16384)?;
        validate_text(&input.request_id, "request_id", 128)?;
        ensure!(
            (1..=86400).contains(&input.timeout_seconds),
            "timeout_seconds must be 1..=86400"
        );
        ensure!(
            !input.request_id.is_empty()
                && input.request_id.len() <= 128
                && input
                    .request_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "request_id must contain 1..=128 ASCII letters, digits, '-' or '_'"
        );
        let submission = serde_json::to_string(&input)?;
        let candidate = uuid::Uuid::new_v4().to_string();
        let id = candidate.clone();
        let request = input.clone();
        let (id,cwd,existing) = self.database(move |db| {
            let ws = workspace(db,&peer,&request.workspace,administrator)?;
            ensure!(ws.execute,"workspace execute permission required");
            let previous: Option<(String,String)> = db.query_row("SELECT id,submission FROM jobs WHERE peer=? AND request_id=?",params![peer,request.request_id], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((old_id,old_submission)) = previous {
                ensure!(old_submission == submission,"request_id already used for different inputs");
                let original_ws: String = db.query_row("SELECT workspace FROM jobs WHERE id=?",[&old_id],|r|r.get(0))?;
                ensure!(original_ws == ws.id,"original workspace was removed; use a new request_id");
                return Ok((old_id,None,true));
            }
            let count: i64 = db.query_row("SELECT COUNT(*) FROM jobs",[],|r|r.get(0))?;
            ensure!(count < 256,"job history is full; ask the executor owner to run workspace prune");
            let active: i64 = db.query_row("SELECT COUNT(*) FROM jobs WHERE status IN ('starting','running','cancelling')",[],|r|r.get(0))?;
            ensure!(active < 8,"executor already has eight active jobs");
            let cwd = files::directory(&ws.root,&request.path)?;
            db.execute("INSERT INTO jobs(id,peer,workspace,request_id,submission,status,created) VALUES(?,?,?,?,?,'starting',?)",params![id,peer,ws.id,request.request_id,submission,now()])?;
            Ok((id,Some(cwd),false))
        }).await?;
        if existing {
            return self.database(move |db| job_summary(db, &id)).await;
        }
        let cwd = cwd.expect("new job has a directory");
        let stdout = self.output_file(&id, "stdout");
        let stderr = self.output_file(&id, "stderr");
        let spawned = (|| -> Result<_> {
            let stdout = stdout?;
            let stderr = stderr?;
            let mut cmd = tokio::process::Command::new("/bin/sh");
            cmd.arg("-c")
                .arg(&input.command)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .env_clear()
                .kill_on_drop(true)
                .process_group(0);
            for key in [
                "PATH", "HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "TMPDIR",
            ] {
                if let Some(value) = std::env::var_os(key) {
                    cmd.env(key, value);
                }
            }
            if std::env::var_os("PATH").is_none() {
                cmd.env("PATH", "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin");
            }
            unsafe {
                cmd.pre_exec(move || {
                    if libc::fchdir(cwd.as_raw_fd()) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            Ok((cmd.spawn()?, stdout, stderr))
        })();
        match spawned {
            Ok((child, stdout_file, stderr_file)) => {
                let pid = child.id().context("spawned job has no pid")?;
                let process_group = ProcessGroup(Some(pid as i32));
                let cancel = Arc::new(Notify::new());
                self.running
                    .lock()
                    .map_err(|_| anyhow::anyhow!("job registry poisoned"))?
                    .insert(id.clone(), cancel.clone());
                let this = self.clone();
                let job = id.clone();
                tokio::spawn(async move {
                    this.monitor(
                        job,
                        JobProcess {
                            child,
                            process_group,
                            cancel,
                            timeout: input.timeout_seconds,
                            stdout_file,
                            stderr_file,
                        },
                    )
                    .await;
                });
            }
            Err(error) => {
                let job = id.clone();
                let error = error.to_string();
                self.database(move |db| {
                    db.execute(
                        "UPDATE jobs SET status='failed',error=?,finished=? WHERE id=?",
                        params![error, now(), job],
                    )?;
                    Ok(())
                })
                .await?;
            }
        }
        self.database(move |db| job_summary(db, &id)).await
    }

    fn output_file(&self, id: &str, stream: &str) -> Result<File> {
        Ok(OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.dir.join(format!("{id}.{stream}")))?)
    }

    async fn monitor(&self, id: String, mut process: JobProcess) {
        let stdout = process.child.stdout.take().expect("piped stdout");
        let stderr = process.child.stderr.take().expect("piped stderr");
        let stdout_task = tokio::spawn(drain(stdout, process.stdout_file));
        let stderr_task = tokio::spawn(drain(stderr, process.stderr_file));
        let job = id.clone();
        let _ = self
            .database(move |db| {
                db.execute(
                    "UPDATE jobs SET status='running' WHERE id=? AND status='starting'",
                    [job],
                )?;
                Ok(())
            })
            .await;
        let (status, code, mut error) = tokio::select! {
            exit = process.child.wait() => match exit {
                Ok(exit) => (if exit.success() {"succeeded"} else {"failed"},exit.code(),None),
                Err(error) => ("failed",None,Some(error.to_string())),
            },
            _ = process.cancel.notified() => ("cancelled",None,None),
            _ = tokio::time::sleep(Duration::from_secs(process.timeout)) => ("timed_out",None,None),
        };
        process.process_group.kill();
        let _ = process.child.wait().await;
        let (stdout_result, stderr_result) =
            tokio::join!(finish_drain(stdout_task), finish_drain(stderr_task));
        let stdout_truncated = *stdout_result.as_ref().unwrap_or(&true);
        let stderr_truncated = *stderr_result.as_ref().unwrap_or(&true);
        if stdout_result.is_err() || stderr_result.is_err() {
            error = Some("output capture incomplete: I/O failure or inherited pipe remained open after the process ended".into());
        }
        let job = id.clone();
        if let Err(error) = self.database(move |db| {
            db.execute("UPDATE jobs SET status=?,finished=?,exit_code=?,error=?,stdout_truncated=?,stderr_truncated=? WHERE id=?",params![status,now(),code,error,stdout_truncated,stderr_truncated,job])?;
            Ok(())
        }).await { eprintln!("cannot finalize job {id}: {error:#}"); }
        if let Ok(mut running) = self.running.lock() {
            running.remove(&id);
        }
    }

    pub async fn shutdown(&self) {
        let _guard = self.gate.lock().await;
        self.stopping.store(true, Ordering::SeqCst);
        if let Ok(running) = self.running.lock() {
            for cancel in running.values() {
                cancel.notify_one();
            }
        }
        drop(_guard);
        for _ in 0..100 {
            if self.running.lock().is_ok_and(|jobs| jobs.is_empty()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

fn workspace(db: &Connection, peer: &str, name: &str, administrator: bool) -> Result<Workspace> {
    validate_text(name, "workspace name", 128)?;
    db.query_row("SELECT w.id,w.root,COALESCE(g.write_access,0),COALESCE(g.execute_access,0) FROM workspaces w LEFT JOIN grants g ON g.workspace=w.id AND g.peer=? WHERE w.name=? AND (? OR g.peer IS NOT NULL)",params![peer,name,administrator],|r|Ok(Workspace {id:r.get(0)?,root:r.get(1)?,write:administrator || r.get(2)?,execute:administrator || r.get(3)?})).optional()?.context("workspace not found or not authorized")
}

fn authorize_job(db: &Connection, peer: &str, job: &str, administrator: bool) -> Result<()> {
    uuid::Uuid::parse_str(job)?;
    let visible = db.query_row("SELECT 1 FROM jobs j JOIN workspaces w ON w.id=j.workspace LEFT JOIN grants g ON g.workspace=w.id AND g.peer=? WHERE j.id=? AND (? OR (j.peer=? AND g.peer IS NOT NULL))",params![peer,job,administrator,peer], |_|Ok(())).optional()?.is_some();
    ensure!(visible, "job not found or not authorized");
    Ok(())
}

fn job_summary(db: &Connection, id: &str) -> Result<Value> {
    db.query_row("SELECT id,peer,submission,status,created,finished,exit_code,error,stdout_truncated,stderr_truncated FROM jobs WHERE id=?",[id],|r|Ok(json!({"id":r.get::<_,String>(0)?,"peer":r.get::<_,String>(1)?,"submission":serde_json::from_str::<Value>(&r.get::<_,String>(2)?).unwrap_or(Value::Null),"status":r.get::<_,String>(3)?,"created_at":r.get::<_,i64>(4)?,"finished_at":r.get::<_,Option<i64>>(5)?,"exit_code":r.get::<_,Option<i32>>(6)?,"error":r.get::<_,Option<String>>(7)?,"stdout_truncated":r.get::<_,bool>(8)?,"stderr_truncated":r.get::<_,bool>(9)?}))).optional()?.context("job not found")
}

fn output(dir: &Path, id: &str, stream: &str, cursor: u64, complete: bool) -> Result<Value> {
    let mut file = match File::open(dir.join(format!("{id}.{stream}"))) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ensure!(cursor == 0, "invalid output cursor");
            return Ok(json!({"text":"","next_cursor":0,"bytes":0}));
        }
        Err(error) => return Err(error.into()),
    };
    let size = file.metadata()?.len();
    ensure!(cursor <= size, "output cursor exceeds captured bytes");
    file.seek(SeekFrom::Start(cursor))?;
    let mut bytes = vec![];
    file.take((files::TEXT_LIMIT as u64).min(size - cursor))
        .read_to_end(&mut bytes)?;
    // Keep a split code point for the next page (or the next poll while a writer
    // is still producing bytes). Invalid complete byte sequences remain lossy text.
    let mut parsed = 0;
    while let Err(error) = std::str::from_utf8(&bytes[parsed..]) {
        parsed += error.valid_up_to();
        if let Some(length) = error.error_len() {
            parsed += length;
        } else {
            if !complete || cursor + (bytes.len() as u64) < size {
                bytes.truncate(parsed);
            }
            break;
        }
    }
    Ok(
        json!({"text":String::from_utf8_lossy(&bytes),"next_cursor":cursor+bytes.len() as u64,"bytes":size}),
    )
}

struct JobProcess {
    child: tokio::process::Child,
    process_group: ProcessGroup,
    cancel: Arc<Notify>,
    timeout: u64,
    stdout_file: File,
    stderr_file: File,
}

struct ProcessGroup(Option<i32>);
impl ProcessGroup {
    fn kill(&mut self) {
        if let Some(pid) = self.0.take() {
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
}
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

async fn drain(mut input: impl AsyncRead + Unpin, file: File) -> Result<bool> {
    let mut output = tokio::fs::File::from_std(file);
    let mut captured = 0;
    let mut truncated = false;
    let mut buffer = [0; 8192];
    loop {
        let size = input.read(&mut buffer).await?;
        if size == 0 {
            break;
        }
        let keep = (OUTPUT_LIMIT - captured).min(size as u64) as usize;
        if keep < size {
            truncated = true;
        }
        output.write_all(&buffer[..keep]).await?;
        captured += keep as u64;
    }
    output.flush().await?;
    output.sync_data().await?;
    Ok(truncated)
}

async fn finish_drain(mut task: tokio::task::JoinHandle<Result<bool>>) -> Result<bool> {
    match tokio::time::timeout(Duration::from_secs(1), &mut task).await {
        Ok(result) => result.context("output drain task failed")?,
        Err(_) => {
            task.abort();
            let _ = task.await;
            anyhow::bail!("output pipe remained open after process-group termination")
        }
    }
}
