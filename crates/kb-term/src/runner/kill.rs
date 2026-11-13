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
use crate::runner::tmux::Tmux;
use crate::runner::{Runner, RunnerError};

/// The outcome of a kill.
#[derive(Debug, Clone)]
pub struct KillOutcome {
    /// The pane's exit code, if it died inside the grace.
    pub exit_code: Option<i32>,
    /// Whether the SIGKILL path ran (or the pane never died in the grace).
    pub forced: bool,
}

/// Clamp a kill grace to 0..=60 seconds (`None` uses the config default).
pub fn clamp_grace(requested: Option<u64>, default: u64) -> u64 {
    requested.unwrap_or(default).clamp(0, 60)
}

/// The `forced` verdict: the pane never died in the grace, or the
/// supervisor's stop path had to SIGKILL a survivor. A dead pane with an
/// empty/unparsable status is not forced by itself.
pub fn forced_kill(pane_died: bool, sigkill_ran: bool) -> bool {
    !pane_died || sigkill_ran
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
    // Read structured exit steps (profile path, honours `WaitMs`); fall
    // back to the flat key list (explicit caller keys) with a 150 ms
    // inter-key pause. `WaitMs` is the profile's own timing, not dead
    // data: omp waits 300 ms between its `C-c`s.
    let steps = discovery::exit_steps(runner, id).await;
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
    if let Some(steps) = steps {
        run_exit_steps(runner, &sock, &steps).await;
    } else {
        let keys = discovery::exit_keys(runner, id)
            .await
            .unwrap_or_else(|_| vec!["C-c".to_string(), "C-c".to_string(), "C-d".to_string()]);
        for key in &keys {
            let _ = runner
                .tmux
                .run(&sock, &["send-keys", "-t", "kb", key], None)
                .await;
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }
    // Poll pane_dead up to the grace.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(grace);
    let mut exit_code: Option<i32> = None;
    // Records the pane's own dead-ness inside the grace, independently of
    // whether its status string parsed.
    let mut pane_died = false;
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
                    pane_died = true;
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
    // A dead pane with an empty/unparsable `pane_dead_status` is NOT a
    // forced kill (`pane_died` is true, `exit_code` stays `None`). The
    // SIGKILL verdict comes from the supervisor's stop path (did
    // something survive SIGTERM?), so a surviving stray forces `true`
    // even when the pane itself died cleanly.
    let (pane_pid, server_pid) = discovery::pane_and_server_pids(runner, id).await;
    runner.take_anchor(id).await;
    // Hidden viewers go with the session.
    runner.close_viewers_for(id).await;
    let sigkill_ran = runner
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
        forced: forced_kill(pane_died, sigkill_ran),
    })
}
/// Send structured exit steps: `Keys`/`Text` via `send-keys`, `WaitMs`
/// as an explicit sleep. Pure of ordering so the `WaitMs` values are
/// pinned by `kill_actions`.
pub async fn run_exit_steps<S: Supervisor>(
    runner: &Runner<S>,
    sock: &std::path::Path,
    steps: &[crate::profiles::OwnedExitStep],
) {
    for action in kill_actions(steps) {
        match action {
            KillAction::Keys(s) => {
                let _ = runner
                    .tmux
                    .run(sock, &["send-keys", "-t", "kb", &s], None)
                    .await;
            }
            KillAction::Sleep(ms) => {
                tokio::time::sleep(Duration::from_millis(ms as u64)).await;
            }
        }
    }
}

/// One kill-path action derived from an exit step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KillAction {
    Keys(String),
    Sleep(u32),
}

/// Flatten structured steps into kill actions, preserving every
/// `WaitMs` as a `Sleep`. This is what makes profile waits live: the
/// omp sequence yields `Sleep(300)` between its `C-c`s, not a fixed
/// 150 ms.
pub fn kill_actions(steps: &[crate::profiles::OwnedExitStep]) -> Vec<KillAction> {
    let mut out = Vec::new();
    for step in steps {
        match step {
            crate::profiles::OwnedExitStep::Keys(keys) => {
                out.extend(keys.iter().map(|k| KillAction::Keys(k.clone())));
            }
            crate::profiles::OwnedExitStep::Text(t) => out.push(KillAction::Keys(t.clone())),
            crate::profiles::OwnedExitStep::WaitMs(ms) => out.push(KillAction::Sleep(*ms)),
        }
    }
    out
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

    /// `forced` follows the SIGKILL path, not the pane exit code: a dead
    /// pane with an empty status is not forced, a clean exit is not
    /// forced, and a surviving stray forces `true` even after a clean
    /// pane exit.
    #[test]
    fn forced_follows_the_sigkill_path_not_the_pane_exit() {
        assert!(!forced_kill(true, false), "clean exit, no SIGKILL");
        assert!(
            forced_kill(false, false),
            "pane alive past grace (systemd stop)"
        );
        assert!(forced_kill(false, true), "stubborn pane, SIGKILL ran");
        assert!(
            forced_kill(true, true),
            "dead pane + surviving stray still forces"
        );
    }

    /// `WaitMs` is honoured, not dead: the omp profile steps flatten to
    /// two `Sleep(300)` actions (the spec waits), and the JSON
    /// round-trip preserves them so the kill path can sleep 300 ms
    /// between its `C-c`s instead of a fixed 150 ms.
    #[test]
    fn kill_actions_honour_waitms() {
        let static_steps = [
            crate::profiles::ExitStep::Keys(&["C-c"]),
            crate::profiles::ExitStep::WaitMs(300),
            crate::profiles::ExitStep::Keys(&["C-c"]),
            crate::profiles::ExitStep::WaitMs(300),
            crate::profiles::ExitStep::Keys(&["C-d"]),
        ];
        let steps = crate::profiles::OwnedExitStep::from_slice(&static_steps);
        assert_eq!(
            kill_actions(&steps),
            vec![
                KillAction::Keys("C-c".to_string()),
                KillAction::Sleep(300),
                KillAction::Keys("C-c".to_string()),
                KillAction::Sleep(300),
                KillAction::Keys("C-d".to_string()),
            ]
        );
        let json = serde_json::to_string(&steps).unwrap();
        let back: Vec<crate::profiles::OwnedExitStep> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, steps, "exit steps JSON round-trip");
        assert_eq!(kill_actions(&back), kill_actions(&steps));
    }
}
