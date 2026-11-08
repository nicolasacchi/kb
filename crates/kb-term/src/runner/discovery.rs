//! Discovery: state-free listing of sessions.
//!
//! The runner has no database. [`list_sessions`] globs
//! `<run-dir>/s/*.sock`; for each stem passing [`valid_id`] it reads the
//! `@kb_*` user options and the pane facts. A socket that does not answer
//! within 2 s is a stale entry (`{id, stale: true}`) and is NOT deleted
//! here. Never exposes argv or env.

use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use crate::runner::ids::valid_id;
use crate::runner::supervisor::Supervisor;
use crate::runner::tmux::Tmux;
use crate::runner::{Runner, RunnerError};

/// One session as the list RPC reports it. All facts are `Option` because
/// a stale socket answers nothing; non-stale entries fill them in.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionEntry {
    /// Session id.
    pub id: String,
    /// Harness name (`@kb_harness`).
    pub harness: Option<String>,
    /// Working directory (`@kb_cwd`).
    pub cwd: Option<String>,
    /// Start time as unix seconds (`@kb_started`).
    pub started_unix: Option<i64>,
    /// Resume session id, if any.
    pub resume_sid: Option<String>,
    /// Pane pid.
    pub pane_pid: Option<u32>,
    /// Whether the pane is dead.
    pub pane_dead: bool,
    /// Exit code, if the pane is dead.
    pub exit_code: Option<i32>,
    /// Grid columns.
    pub cols: Option<u16>,
    /// Grid rows.
    pub rows: Option<u16>,
    /// Whether bracketed paste is enabled (reported for diagnostics).
    pub bracket_paste_flag: bool,
    /// Grid owner, if any (T9 fills this; always null in T8).
    pub grid_owner: Option<String>,
    /// `kb-term.conf` version the holder loaded.
    pub conf_version: u32,
    /// The anchor client pid, if known.
    pub anchor_pid: Option<u32>,
    /// Whether the socket did not answer.
    pub stale: bool,
}

impl Serialize for SessionEntry {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = s.serialize_map(Some(15))?;
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("harness", &self.harness)?;
        map.serialize_entry("cwd", &self.cwd)?;
        map.serialize_entry("started_unix", &self.started_unix)?;
        map.serialize_entry("resume_sid", &self.resume_sid)?;
        map.serialize_entry("pane_pid", &self.pane_pid)?;
        map.serialize_entry("pane_dead", &self.pane_dead)?;
        map.serialize_entry("exit_code", &self.exit_code)?;
        map.serialize_entry("cols", &self.cols)?;
        map.serialize_entry("rows", &self.rows)?;
        map.serialize_entry("bracket_paste_flag", &self.bracket_paste_flag)?;
        map.serialize_entry("grid_owner", &self.grid_owner)?;
        map.serialize_entry("conf_version", &self.conf_version)?;
        map.serialize_entry("anchor_pid", &self.anchor_pid)?;
        map.serialize_entry("stale", &self.stale)?;
        map.end()
    }
}

/// Glob `<run-dir>/s/*.sock` and describe every holder.
pub async fn list_sessions<S: Supervisor>(runner: &Runner<S>) -> Vec<SessionEntry> {
    let dir = runner.sessions_dir();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name())
        .filter_map(|n| n.into_string().ok())
        .filter_map(|n| n.strip_suffix(".sock").map(ToString::to_string))
        .filter(|id| valid_id(id))
        .collect();
    ids.sort();
    let mut out = Vec::new();
    for id in ids {
        out.push(describe(runner, &id, &dir).await);
    }
    out
}

async fn describe<S: Supervisor>(runner: &Runner<S>, id: &str, _dir: &Path) -> SessionEntry {
    let sock = runner.holder_socket(id);
    let entry = SessionEntry {
        id: id.to_string(),
        harness: None,
        cwd: None,
        started_unix: None,
        resume_sid: None,
        pane_pid: None,
        pane_dead: false,
        exit_code: None,
        cols: None,
        rows: None,
        bracket_paste_flag: false,
        grid_owner: None,
        conf_version: 0,
        anchor_pid: None,
        stale: true,
    };
    // First probe: anything that does not answer in 2 s is stale.
    let probe = runner
        .tmux
        .run(
            &sock,
            &["show-options", "-gqv", "@kb_harness"],
            Some(Duration::from_secs(2)),
        )
        .await;
    if !matches!(&probe, Ok(o) if o.ok) {
        return entry;
    }
    let mut entry = SessionEntry {
        stale: false,
        ..entry
    };
    entry.harness = show_option(runner, &sock, "@kb_harness").await;
    entry.cwd = show_option(runner, &sock, "@kb_cwd").await;
    entry.started_unix = show_option(runner, &sock, "@kb_started")
        .await
        .and_then(|s| s.parse().ok());
    entry.resume_sid = show_option(runner, &sock, "@kb_resume_sid").await;
    entry.anchor_pid = show_option(runner, &sock, "@kb_anchor_pid")
        .await
        .and_then(|s| s.parse().ok());
    entry.conf_version = show_option(runner, &sock, "@kb_conf_version")
        .await
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    entry.grid_owner = show_option(runner, &sock, "@kb_owner_grid").await;
    // Pane facts.
    let out = runner
        .tmux
        .run(
            &sock,
            &[
                "display-message",
                "-p",
                "-t",
                "kb",
                "#{pane_pid} #{pane_dead} #{pane_dead_status} #{window_width} #{window_height} #{bracket_paste_flag}",
            ],
            None,
        )
        .await;
    if let Ok(out) = out {
        if out.ok {
            let parts: Vec<&str> = out.stdout.split_whitespace().collect();
            if parts.len() >= 5 {
                entry.pane_pid = parts[0].parse().ok();
                entry.pane_dead = parts[1] == "1";
                if entry.pane_dead {
                    entry.exit_code = parts[2].parse().ok();
                }
                entry.cols = parts[3].parse().ok();
                entry.rows = parts[4].parse().ok();
                entry.bracket_paste_flag = parts.get(5) == Some(&"1");
            }
        }
    }
    entry
}

async fn show_option<S: Supervisor>(runner: &Runner<S>, sock: &Path, key: &str) -> Option<String> {
    let out = runner
        .tmux
        .run(sock, &["show-options", "-gqv", key], None)
        .await
        .ok()?;
    if out.ok && !out.stdout.trim().is_empty() {
        Some(out.stdout.trim().to_string())
    } else {
        None
    }
}

/// The pids of every client attached to the holder (for adopt checks).
pub async fn client_pids<S: Supervisor>(runner: &Runner<S>, id: &str) -> Vec<u32> {
    let sock = runner.holder_socket(id);
    let out = runner
        .tmux
        .run(&sock, &["list-clients", "-F", "#{client_pid}"], None)
        .await;
    match out {
        Ok(out) if out.ok => out
            .stdout
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect(),
        _ => Vec::new(),
    }
}

/// Read the `@kb_exit_keys` option as a key list (kill RPC).
pub async fn exit_keys<S: Supervisor>(
    runner: &Runner<S>,
    id: &str,
) -> Result<Vec<String>, RunnerError> {
    let sock = runner.holder_socket(id);
    let out = runner
        .tmux
        .run(&sock, &["show-options", "-gqv", "@kb_exit_keys"], None)
        .await?;
    if !out.ok {
        return Err(RunnerError::NotFound(format!("no holder for {id}")));
    }
    let keys: Vec<String> = serde_json::from_str(out.stdout.trim())
        .unwrap_or_else(|_| vec!["C-c".to_string(), "C-c".to_string(), "C-d".to_string()]);
    Ok(keys)
}

/// Read the `@kb_anchor_pid` option (kill/adopt paths).
pub async fn anchor_pid<S: Supervisor>(runner: &Runner<S>, id: &str) -> Option<u32> {
    show_option(runner, &runner.holder_socket(id), "@kb_anchor_pid")
        .await
        .and_then(|s| s.parse().ok())
}

/// The pane pid + server pid of a live holder (kill/diagnostics).
pub async fn pane_and_server_pids<S: Supervisor>(
    runner: &Runner<S>,
    id: &str,
) -> (Option<u32>, Option<u32>) {
    let sock = runner.holder_socket(id);
    let pane = runner
        .tmux
        .run(
            &sock,
            &["display-message", "-p", "-t", "kb", "#{pane_pid}"],
            None,
        )
        .await;
    let pane = match pane {
        Ok(out) if out.ok => out.stdout.trim().parse().ok(),
        _ => None,
    };
    let server = runner
        .tmux
        .run(&sock, &["display-message", "-p", "#{pid}"], None)
        .await;
    let server = match server {
        Ok(out) if out.ok => out.stdout.trim().parse().ok(),
        _ => None,
    };
    (pane, server)
}

/// `capture-pane` for the read/history RPCs (interim).
pub async fn capture_pane<S: Supervisor>(
    runner: &Runner<S>,
    id: &str,
    history: bool,
) -> Result<String, RunnerError> {
    let sock = runner.holder_socket(id);
    let mut args = vec!["capture-pane", "-p", "-J"];
    if history {
        args.extend(["-S", "-"]);
    }
    args.extend(["-t", "kb"]);
    let out = runner.tmux.run(&sock, &args, None).await?;
    if !out.ok {
        return Err(RunnerError::NotFound(format!("no pane for {id}")));
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_item_never_serialises_argv_or_env() {
        let entry = SessionEntry {
            id: "abcd1234".to_string(),
            harness: Some("omp".to_string()),
            cwd: Some("/work/proj".to_string()),
            started_unix: Some(1_234_567_890),
            resume_sid: None,
            pane_pid: Some(4242),
            pane_dead: false,
            exit_code: None,
            cols: Some(120),
            rows: Some(40),
            bracket_paste_flag: false,
            grid_owner: None,
            conf_version: 2,
            anchor_pid: Some(99),
            stale: false,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(!json.contains("argv"), "list items must not carry argv");
        assert!(!json.contains("env"), "list items must not carry env");
        assert!(json.contains("\"harness\":\"omp\""));
        assert!(json.contains("\"anchor_pid\":99"));
        assert!(json.contains("\"conf_version\":2"));
    }
}
