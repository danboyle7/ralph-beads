use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use chrono::Local;
use serde_json::{json, Value};

use crate::cli::Paths;

#[derive(Debug, Clone)]
pub(crate) struct RunState {
    pub(crate) run_id: String,
    pub(crate) status: String,
    pub(crate) started_at: String,
    pub(crate) updated_at: String,
    pub(crate) current_issue: Option<String>,
    pub(crate) iteration: usize,
    pub(crate) total_iterations: usize,
    pub(crate) mode: String,
    pub(crate) pid: u32,
}

pub(crate) struct RunLockGuard {
    path: PathBuf,
}

impl Drop for RunLockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub(crate) struct RunStateGuard {
    pub(crate) paths: Paths,
    pub(crate) run_id: String,
}

impl Drop for RunStateGuard {
    fn drop(&mut self) {
        if let Some(state) = read_run_state(&self.paths) {
            if state.run_id == self.run_id && state.status.eq_ignore_ascii_case("running") {
                let _ = mark_run_state_finished(&self.paths, &self.run_id, "error");
            }
        }
    }
}

pub(crate) fn acquire_run_lock(paths: &Paths) -> Result<RunLockGuard> {
    fs::create_dir_all(&paths.ralph_dir).with_context(|| {
        format!(
            "failed to create Ralph directory at {}",
            paths.ralph_dir.display()
        )
    })?;

    let payload = || {
        json!({
            "pid": std::process::id(),
            "started_at": Local::now().to_rfc3339(),
            "project_dir": paths.project_dir.display().to_string(),
        })
        .to_string()
    };

    for attempt in 0..=1 {
        let open_result = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&paths.lock_file);
        match open_result {
            Ok(mut file) => {
                file.write_all(payload().as_bytes())
                    .context("failed to write run lock file")?;
                return Ok(RunLockGuard {
                    path: paths.lock_file.clone(),
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && attempt == 0 => {
                let lock_content = fs::read_to_string(&paths.lock_file).unwrap_or_default();
                let lock_pid = parse_lock_pid(&lock_content);
                let can_recover = lock_pid.map(|pid| !process_is_running(pid)).unwrap_or(true);
                if can_recover {
                    let _ = fs::remove_file(&paths.lock_file);
                    continue;
                }
                bail!(
                    "Another Ralph process appears active (lock: {}, pid: {}). If this is stale, remove the lock file and retry.",
                    paths.lock_file.display(),
                    lock_pid.unwrap_or(0)
                );
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to acquire run lock at {}",
                        paths.lock_file.display()
                    )
                });
            }
        }
    }

    bail!(
        "failed to acquire run lock at {}; a stale lock may still exist",
        paths.lock_file.display()
    )
}

pub(crate) fn write_run_state(paths: &Paths, state: &RunState) -> Result<()> {
    fs::create_dir_all(&paths.ralph_dir)
        .with_context(|| format!("failed to create {}", paths.ralph_dir.display()))?;
    let payload = json!({
        "run_id": state.run_id,
        "status": state.status,
        "started_at": state.started_at,
        "updated_at": state.updated_at,
        "current_issue": state.current_issue,
        "iteration": state.iteration,
        "total_iterations": state.total_iterations,
        "mode": state.mode,
        "pid": state.pid,
    });
    fs::write(&paths.state_file, serde_json::to_string_pretty(&payload)?)
        .with_context(|| format!("failed to write {}", paths.state_file.display()))
}

pub(crate) fn read_run_state(paths: &Paths) -> Option<RunState> {
    let content = fs::read_to_string(&paths.state_file).ok()?;
    let value: Value = serde_json::from_str(&content).ok()?;
    Some(RunState {
        run_id: value
            .get("run_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        status: value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        started_at: value
            .get("started_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        updated_at: value
            .get("updated_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        current_issue: value
            .get("current_issue")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        iteration: value
            .get("iteration")
            .and_then(Value::as_u64)
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(0),
        total_iterations: value
            .get("total_iterations")
            .and_then(Value::as_u64)
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(0),
        mode: value
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("loop")
            .to_string(),
        pid: value
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0),
    })
}

pub(crate) fn update_run_state_progress(
    paths: &Paths,
    run_id: &str,
    current_issue: Option<String>,
    iteration: usize,
    total_iterations: usize,
    status: &str,
) -> Result<()> {
    let previous = read_run_state(paths);
    let started_at = previous
        .as_ref()
        .map(|state| state.started_at.clone())
        .unwrap_or_else(|| Local::now().to_rfc3339());
    let mode = previous
        .as_ref()
        .map(|state| state.mode.clone())
        .unwrap_or_else(|| "loop".to_string());
    write_run_state(
        paths,
        &RunState {
            run_id: run_id.to_string(),
            status: status.to_string(),
            started_at,
            updated_at: Local::now().to_rfc3339(),
            current_issue,
            iteration,
            total_iterations,
            mode,
            pid: std::process::id(),
        },
    )
}

pub(crate) fn mark_run_state_finished(paths: &Paths, run_id: &str, status: &str) -> Result<()> {
    let previous = read_run_state(paths);
    let started_at = previous
        .as_ref()
        .map(|state| state.started_at.clone())
        .unwrap_or_else(|| Local::now().to_rfc3339());
    let mode = previous
        .as_ref()
        .map(|state| state.mode.clone())
        .unwrap_or_else(|| "loop".to_string());
    let total_iterations = previous
        .as_ref()
        .map(|state| state.total_iterations)
        .unwrap_or(0);
    write_run_state(
        paths,
        &RunState {
            run_id: run_id.to_string(),
            status: status.to_string(),
            started_at,
            updated_at: Local::now().to_rfc3339(),
            current_issue: None,
            iteration: total_iterations,
            total_iterations,
            mode,
            pid: std::process::id(),
        },
    )
}

fn parse_lock_pid(content: &str) -> Option<u32> {
    let value: Value = serde_json::from_str(content).ok()?;
    value
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
}

fn process_is_running(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    match output {
        Ok(output) if output.status.success() => {
            !String::from_utf8_lossy(&output.stdout).trim().is_empty()
        }
        _ => false,
    }
}
