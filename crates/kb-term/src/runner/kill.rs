//! Kill: exit keys, wait for `pane_dead`, stop the holder, clean up.
//!
//! Shutdown is exit keys then a whole-holder stop (cgroup under systemd,
//! process-group kill under direct). `kill-server` alone is NEVER the
//! stop mechanism: twice in the spikes a stray harness ignored SIGHUP
//! and held its session lock. A bare continue is never sent.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::runner::discovery;
use crate::runner::ids::valid_id;
use crate::runner::supervisor::{StopCtx, Supervisor};
use crate::runner::{Runner, RunnerError};

/// The outcome of a kill.
#[derive(Debug, Clone)]
pub struct KillOutcome {
    /// The pane's exit code, if it died inside the grace.
    pub exit_code: Option<i32>,
    /// Whether the SIGKILL path ran.
    pub forced: bool,
}

/// Clamp a kill grace to 0..=60 seconds (`None` uses the config default).
pub fn clamp_grace(requested: Option<u64>, default: u64) -> u64 {
    requested.unwrap_or(default).clamp(0, 60)
}

/// Kill one session. The id is validated before any tmux call.
pub async fn kill_session<S: Supervisor>(
    runner: &Runner<S>,
    id: &str,
    grace_secs: Option<u64>,
) -> Result<KillOutcome, RunnerError> {
    if !valid_id(id) {
        return Err(RunnerError::InvalidId);
    }
    let _lock = runner.id_lock(id).await;
    let _guard = _lock.lock().await;
    let grace = clamp_grace(grace_secs, runner.cfg.kill_grace_secs);
    // Read the exit keys; default to the omp sequence (T10 refines).
    let keys = discovery::exit_keys(runner, id)
        .await
        .unwrap_or_else(|_| vec!["C-c".to_string(), "C-c".to_string(), "C-d".to_string()]);
    let sock = runner.holder_socket(id);
    // The holder must answer at all, or this is `not-found`.
    let alive = runner
        .tmux
        .run(&sock, &["list-sessions"], None)
        .await
        .map(|o| o.ok)
        .unwrap_or(false);
    if !alive {
        return Err(RunnerError::NotFound(format!("no holder for {id}")));
    }
    for key in &keys {
        let _ = runner
            .tmux
            .run(&sock, &["send-keys", "-t", "kb", key], None)
            .await;
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    // Poll pane_dead up to the grace.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(grace);
    let mut exit_code: Option<i32> = None;
    loop {
        let out = runner
            .tmux
            .run(
                &sock,
                &[
                    "display-message",
                    "-p",
                    "-t",
                    "kb",
                    "#{pane_dead} #{pane_dead_status}",
                ],
                None,
            )
            .await;
        if let Ok(out) = out {
            if out.ok {
                let parts: Vec<&str> = out.stdout.split_whitespace().collect();
                if parts.first() == Some(&"1") {
                    exit_code = parts.get(1).and_then(|s| s.parse().ok());
                    break;
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Read pane/server pids BEFORE touching the server (direct mode needs
    // them for the process-group signals); systemd ignores them.
    let (pane_pid, server_pid) = discovery::pane_and_server_pids(runner, id).await;
    runner.take_anchor(id).await;
    runner
        .supervisor
        .stop(
            id,
            StopCtx {
                pane_pid,
                server_pid,
            },
        )
        .await?;
    crate::runner::launch::remove_session_files(runner, id).await;
    Ok(KillOutcome {
        exit_code,
        // `forced` is true when the pane did not die inside the grace and
        // the supervisor's SIGKILL path had to run.
        forced: exit_code.is_none(),
    })
}

/// Read `pane_dead` once (for diagnostics in tests and RPCs).
pub async fn pane_dead<S: Supervisor>(runner: &Runner<S>, id: &str) -> bool {
    let sock = runner.holder_socket(id);
    let out = runner
        .tmux
        .run(
            &sock,
            &["display-message", "-p", "-t", "kb", "#{pane_dead}"],
            None,
        )
        .await;
    matches!(out, Ok(o) if o.ok && o.stdout.trim() == "1")
}

/// Current unix time (for `@kb_started`).
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_grace_is_clamped() {
        assert_eq!(clamp_grace(Some(999), 5), 60);
        assert_eq!(clamp_grace(Some(0), 5), 0);
        assert_eq!(clamp_grace(Some(30), 5), 30);
        assert_eq!(clamp_grace(None, 5), 5);
        assert_eq!(clamp_grace(None, 999), 60);
    }
}
