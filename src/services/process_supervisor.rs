//! Fixed-command process supervisor using `tokio::process` with SQLite PID/status persistence.
//! Never executes a shell and never builds commands from raw request input.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::process::{Output, Stdio};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::process::{Child, Command};
use tokio::sync::Mutex as AsyncMutex;

const KV_SCOPE: &str = "process_supervisor";

#[derive(Debug, Clone)]
pub struct FixedCommand {
    pub program: &'static str,
    pub args: Vec<String>,
}

impl FixedCommand {
    pub fn new(program: &'static str, args: Vec<String>) -> Self {
        Self { program, args }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProcessRecord {
    pub id: String,
    pub pid: Option<u32>,
    pub status: String,
    pub program: String,
    pub started_at: Option<String>,
    pub stopped_at: Option<String>,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub already_running: Option<bool>,
}

impl ProcessRecord {
    fn stopped(id: &str, program: &str) -> Self {
        Self {
            id: id.to_owned(),
            pid: None,
            status: "stopped".to_owned(),
            program: program.to_owned(),
            started_at: None,
            stopped_at: None,
            exit_code: None,
            error: None,
            already_running: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("{0}")]
    NotInstalled(String),
    #[error("{0}")]
    SpawnFailed(String),
    #[error("{0}")]
    Timeout(String),
    #[error("{0}")]
    ExitFailed(String),
}

pub struct ProcessSupervisor {
    children: AsyncMutex<HashMap<String, Child>>,
}

static GLOBAL_SUPERVISOR: OnceLock<ProcessSupervisor> = OnceLock::new();

pub fn global() -> &'static ProcessSupervisor {
    GLOBAL_SUPERVISOR.get_or_init(|| ProcessSupervisor {
        children: AsyncMutex::new(HashMap::new()),
    })
}

fn now_iso() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}

pub fn data_dir() -> std::path::PathBuf {
    match std::env::var("DATA_DIR") {
        Ok(d) if !d.trim().is_empty() => std::path::PathBuf::from(d),
        _ => std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".9router"),
    }
}

pub fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn send_sigterm(pid: u32) {
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

fn send_sigkill(pid: u32) {
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

fn persist_record(db: &Arc<StdMutex<Connection>>, record: &ProcessRecord) {
    if let Ok(json) = serde_json::to_string(record)
        && let Ok(conn) = db.lock()
    {
        let _ = crate::db::kv_set(&conn, KV_SCOPE, &record.id, &json);
    }
}

fn load_record(db: &Arc<StdMutex<Connection>>, id: &str) -> Option<ProcessRecord> {
    let raw = db
        .lock()
        .ok()
        .and_then(|conn| crate::db::kv_get(&conn, KV_SCOPE, id).ok().flatten())?;
    serde_json::from_str(&raw).ok()
}

impl ProcessSupervisor {
    pub async fn status(&self, db: &Arc<StdMutex<Connection>>, id: &str) -> ProcessRecord {
        let mut children = self.children.lock().await;
        if let Some(child) = children.get_mut(id) {
            match child.try_wait() {
                Ok(None) => {
                    let pid = child.id();
                    if let Some(mut rec) = load_record(db, id) {
                        rec.status = "running".to_owned();
                        rec.pid = pid;
                        rec.already_running = None;
                        return rec;
                    }
                    return ProcessRecord {
                        id: id.to_owned(),
                        pid,
                        status: "running".to_owned(),
                        program: String::new(),
                        started_at: None,
                        stopped_at: None,
                        exit_code: None,
                        error: None,
                        already_running: None,
                    };
                }
                Ok(Some(exit)) => {
                    children.remove(id);
                    let mut rec = load_record(db, id).unwrap_or_else(|| ProcessRecord::stopped(id, ""));
                    rec.pid = None;
                    rec.exit_code = exit.code();
                    rec.status = if exit.success() { "stopped" } else { "failed" }.to_owned();
                    rec.stopped_at = Some(now_iso());
                    rec.already_running = None;
                    persist_record(db, &rec);
                    return rec;
                }
                Err(_) => {
                    children.remove(id);
                }
            }
        }
        drop(children);

        if let Some(mut rec) = load_record(db, id) {
            if rec.status == "running" {
                if let Some(pid) = rec.pid
                    && is_pid_alive(pid)
                {
                    return rec;
                }
                rec.status = "stopped".to_owned();
                rec.pid = None;
                rec.stopped_at = Some(now_iso());
                persist_record(db, &rec);
            }
            rec.already_running = None;
            return rec;
        }
        ProcessRecord::stopped(id, "")
    }

    pub async fn spawn(
        &self,
        db: &Arc<StdMutex<Connection>>,
        id: &str,
        cmd: FixedCommand,
        log_path: Option<&Path>,
    ) -> Result<ProcessRecord, SupervisorError> {
        let current = self.status(db, id).await;
        if current.status == "running" && current.pid.is_some() {
            let mut rec = current;
            rec.already_running = Some(true);
            return Ok(rec);
        }

        let (stdout_cfg, stderr_cfg) = if let Some(path) = log_path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::OpenOptions::new().create(true).append(true).open(path) {
                Ok(f) => {
                    let f2 = f.try_clone().ok();
                    (
                        Stdio::from(f),
                        f2.map(Stdio::from).unwrap_or_else(Stdio::null),
                    )
                }
                Err(_) => (Stdio::null(), Stdio::null()),
            }
        } else {
            (Stdio::null(), Stdio::null())
        };

        let mut command = Command::new(cmd.program);
        command
            .args(&cmd.args)
            .stdin(Stdio::null())
            .stdout(stdout_cfg)
            .stderr(stderr_cfg)
            .kill_on_drop(false);

        let child = command.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                SupervisorError::NotInstalled(format!("{} is not installed", cmd.program))
            } else {
                SupervisorError::SpawnFailed(format!("Failed to spawn {}: {e}", cmd.program))
            }
        })?;

        let pid = child.id();
        let rec = ProcessRecord {
            id: id.to_owned(),
            pid,
            status: "running".to_owned(),
            program: cmd.program.to_owned(),
            started_at: Some(now_iso()),
            stopped_at: None,
            exit_code: None,
            error: None,
            already_running: Some(false),
        };
        persist_record(db, &rec);
        self.children.lock().await.insert(id.to_owned(), child);
        Ok(rec)
    }

    pub async fn stop(
        &self,
        db: &Arc<StdMutex<Connection>>,
        id: &str,
        grace: Duration,
    ) -> Result<Option<u32>, SupervisorError> {
        let mut child_opt = self.children.lock().await.remove(id);
        let pid_opt = child_opt
            .as_ref()
            .and_then(|c| c.id())
            .or_else(|| load_record(db, id).and_then(|r| r.pid));

        let Some(pid) = pid_opt else {
            let mut rec = load_record(db, id).unwrap_or_else(|| ProcessRecord::stopped(id, ""));
            rec.status = "stopped".to_owned();
            rec.pid = None;
            persist_record(db, &rec);
            return Ok(None);
        };

        let alive = if let Some(ref mut child) = child_opt {
            child.try_wait().ok().flatten().is_none()
        } else {
            is_pid_alive(pid)
        };

        if !alive {
            let mut rec = load_record(db, id).unwrap_or_else(|| ProcessRecord::stopped(id, ""));
            rec.status = "stopped".to_owned();
            rec.pid = None;
            rec.stopped_at = Some(now_iso());
            persist_record(db, &rec);
            return Ok(None);
        }

        send_sigterm(pid);
        let deadline = tokio::time::Instant::now() + grace;
        let mut exited = false;
        while tokio::time::Instant::now() < deadline {
            if let Some(ref mut child) = child_opt {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    exited = true;
                    break;
                }
            } else if !is_pid_alive(pid) {
                exited = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        if !exited {
            if let Some(ref mut child) = child_opt {
                let _ = child.kill().await;
                let _ = child.wait().await;
            } else {
                send_sigkill(pid);
            }
        }

        let mut rec = load_record(db, id).unwrap_or_else(|| ProcessRecord::stopped(id, ""));
        rec.status = "stopped".to_owned();
        rec.pid = None;
        rec.stopped_at = Some(now_iso());
        persist_record(db, &rec);
        Ok(Some(pid))
    }

    pub async fn restart(
        &self,
        db: &Arc<StdMutex<Connection>>,
        id: &str,
        cmd: FixedCommand,
        log_path: Option<&Path>,
        grace: Duration,
    ) -> Result<ProcessRecord, SupervisorError> {
        let _ = self.stop(db, id, grace).await?;
        self.spawn(db, id, cmd, log_path).await
    }

    pub async fn run_once(
        cmd: FixedCommand,
        cwd: Option<&Path>,
        log_path: Option<&Path>,
        timeout: Duration,
    ) -> Result<Output, SupervisorError> {
        let mut command = Command::new(cmd.program);
        command
            .args(&cmd.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(dir) = cwd {
            command.current_dir(dir);
        }
        let child = command.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                SupervisorError::NotInstalled(format!("{} is not installed", cmd.program))
            } else {
                SupervisorError::SpawnFailed(format!("Failed to spawn {}: {e}", cmd.program))
            }
        })?;

        let output = tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .map_err(|_| SupervisorError::Timeout(format!("{} timed out", cmd.program)))?
            .map_err(|e| SupervisorError::SpawnFailed(format!("{} failed: {e}", cmd.program)))?;

        if let Some(path) = log_path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let mut combined = Vec::new();
            combined.extend_from_slice(&output.stdout);
            combined.extend_from_slice(&output.stderr);
            let _ = std::fs::write(path, combined);
        }

        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn supervisor_spawns_persists_and_gracefully_stops_sleep() {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        let db = Arc::new(StdMutex::new(db));
        let sup = ProcessSupervisor {
            children: AsyncMutex::new(HashMap::new()),
        };

        let rec = sup
            .spawn(
                &db,
                "test-sleep",
                FixedCommand::new("sleep", vec!["30".to_owned()]),
                None,
            )
            .await
            .unwrap();
        let pid = rec.pid.expect("sleep should have a pid");
        assert_eq!(rec.status, "running");
        assert_eq!(rec.already_running, Some(false));

        let stored_raw = crate::db::kv_get(&db.lock().unwrap(), KV_SCOPE, "test-sleep")
            .unwrap()
            .expect("sqlite record persisted");
        let stored: ProcessRecord = serde_json::from_str(&stored_raw).unwrap();
        assert_eq!(stored.pid, Some(pid));
        assert_eq!(stored.status, "running");

        let rec2 = sup
            .spawn(
                &db,
                "test-sleep",
                FixedCommand::new("sleep", vec!["30".to_owned()]),
                None,
            )
            .await
            .unwrap();
        assert_eq!(rec2.already_running, Some(true));
        assert_eq!(rec2.pid, Some(pid));

        let stopped_pid = sup
            .stop(&db, "test-sleep", Duration::from_millis(500))
            .await
            .unwrap();
        assert_eq!(stopped_pid, Some(pid));

        let after = sup.status(&db, "test-sleep").await;
        assert_eq!(after.status, "stopped");
        assert_eq!(after.pid, None);

        let stored_after: ProcessRecord = serde_json::from_str(
            &crate::db::kv_get(&db.lock().unwrap(), KV_SCOPE, "test-sleep")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(stored_after.status, "stopped");
        assert_eq!(stored_after.pid, None);
    }
}
