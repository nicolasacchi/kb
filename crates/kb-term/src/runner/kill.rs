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
    /// True when no holder answered and the call only removed the
    /// leftover socket / session dir of a stale entry.
    pub stale_removed: bool,
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
    // The holder must answer at all. A tmux that ANSWERS with an error
    // (no server, refused connection) is a stale entry: its leftovers
    // are reaped. A probe that fails to complete (spawn error, timeout)
    // proves nothing about the holder, so nothing is removed.
    let probe = runner.tmux.run(&sock, &["list-sessions"], None).await;
    match probe {
        Ok(o) if o.ok => {}
        Ok(_) => {
            return if reap_stale_entry(&runner.sessions_dir(), id)? {
                Ok(KillOutcome {
                    exit_code: None,
                    forced: false,
                    stale_removed: true,
                })
            } else {
                Err(RunnerError::NotFound(format!("no holder for {id}")))
            };
        }
        Err(e) => {
            return if stale_leftovers_exist(&runner.sessions_dir(), id) {
                Err(e)
            } else {
                Err(RunnerError::NotFound(format!("no holder for {id}")))
            };
        }
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
    // SIGKILL verdict comes from the supervisor's stop path (does any
    // process GROUP still have a live member after SIGTERM?), so a
    // surviving stray forces `true` even when the pane's leader died
    // cleanly.
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
        stale_removed: false,
    })
}

fn stale_leftovers_exist(sessions_dir: &std::path::Path, id: &str) -> bool {
    std::fs::symlink_metadata(sessions_dir.join(format!("{id}.sock"))).is_ok()
        || std::fs::symlink_metadata(sessions_dir.join(id)).is_ok()
}

/// Remove exactly `<sessions_dir>/<id>.sock` and `<sessions_dir>/<id>/`,
/// the leftovers of a session whose holder no longer answers. The caller
/// has already established that no holder answers and holds the id lock.
///
/// Refuses (nothing removed) when the id fails [`valid_id`], when either
/// entry is a symlink, when the socket entry is not a socket, or when
/// the session entry is not a directory. Both entries are inspected
/// with `symlink_metadata` BEFORE anything is removed, so a refusal is
/// all-or-nothing. Returns `Ok(false)` when neither entry exists.
pub fn reap_stale_entry(sessions_dir: &std::path::Path, id: &str) -> Result<bool, RunnerError> {
    use std::os::unix::fs::FileTypeExt;
    if !valid_id(id) {
        return Err(RunnerError::InvalidId);
    }
    let sock = sessions_dir.join(format!("{id}.sock"));
    let dir = sessions_dir.join(id);
    let lstat = |p: &std::path::Path| match std::fs::symlink_metadata(p) {
        Ok(m) => Ok(Some(m)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(RunnerError::Io(format!(
            "cannot inspect a session entry: {e}"
        ))),
    };
    let sock_meta = lstat(&sock)?;
    let dir_meta = lstat(&dir)?;
    if let Some(m) = &sock_meta {
        if !m.file_type().is_socket() {
            return Err(RunnerError::BadRequest(format!(
                "refusing {id}: the holder entry is not a socket"
            )));
        }
    }
    if let Some(m) = &dir_meta {
        if !m.file_type().is_dir() {
            return Err(RunnerError::BadRequest(format!(
                "refusing {id}: the session entry is not a plain directory"
            )));
        }
    }
    if sock_meta.is_none() && dir_meta.is_none() {
        return Ok(false);
    }
    if sock_meta.is_some() {
        std::fs::remove_file(&sock)
            .map_err(|e| RunnerError::Io(format!("cannot remove the stale socket: {e}")))?;
    }
    if dir_meta.is_some() {
        std::fs::remove_dir_all(&dir)
            .map_err(|e| RunnerError::Io(format!("cannot remove the stale session dir: {e}")))?;
    }
    Ok(true)
}
/// Minimum pause after a key/text send that is not followed by an
/// explicit `WaitMs` (an input-burst or paste heuristic must not see
/// `/exit` and `Enter` as one pasted chunk). A `WaitMs` governs instead
/// where present.
pub const MIN_KEY_GAP_MS: u64 = 150;

/// Send structured exit steps: `Keys`/`Text` via `send-keys`, `WaitMs`
/// as an explicit sleep, and a [`MIN_KEY_GAP_MS`] pause after every send
/// that is followed by another send. Pure of ordering so the `WaitMs`
/// values are pinned by `kill_actions`.
pub async fn run_exit_steps<S: Supervisor>(
    runner: &Runner<S>,
    sock: &std::path::Path,
    steps: &[crate::profiles::OwnedExitStep],
) {
    let actions = kill_actions(steps);
    for (i, action) in actions.iter().enumerate() {
        match action {
            KillAction::Keys(s) => {
                let _ = runner
                    .tmux
                    .run(sock, &["send-keys", "-t", "kb", s], None)
                    .await;
                if matches!(actions.get(i + 1), Some(KillAction::Keys(_))) {
                    tokio::time::sleep(Duration::from_millis(MIN_KEY_GAP_MS)).await;
                }
            }
            KillAction::Sleep(ms) => {
                tokio::time::sleep(Duration::from_millis(*ms as u64)).await;
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
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};

    /// A sessions dir holding a bound unix socket `<id>.sock` and a
    /// `<id>/` dir with one file (what a hand-stopped session leaves).
    fn leftovers(root: &Path, id: &str) -> (PathBuf, PathBuf, PathBuf) {
        let dir = root.join("s");
        std::fs::create_dir_all(dir.join(id)).unwrap();
        std::fs::write(dir.join(id).join("state"), b"x").unwrap();
        let sock = dir.join(format!("{id}.sock"));
        // Binding leaves a real socket inode behind when the listener drops.
        drop(UnixListener::bind(&sock).unwrap());
        (dir.clone(), sock, dir.join(id))
    }

    #[test]
    fn reaping_removes_exactly_the_one_sessions_socket_and_dir() {
        let root = tempfile::tempdir().unwrap();
        let (dir, sock, sdir) = leftovers(root.path(), "abcd1234");
        let (_, other_sock, other_dir) = leftovers(root.path(), "wxyz9876");
        assert!(reap_stale_entry(&dir, "abcd1234").unwrap());
        assert!(!sock.exists() && !sdir.exists(), "the target is gone");
        assert!(
            other_sock.exists() && other_dir.join("state").exists(),
            "a neighbour session is untouched"
        );
        // Nothing left: the id is not found, not silently ok.
        assert!(!reap_stale_entry(&dir, "abcd1234").unwrap());
    }

    #[test]
    fn reaping_refuses_malformed_and_traversal_ids_removing_nothing() {
        let root = tempfile::tempdir().unwrap();
        let (dir, sock, sdir) = leftovers(root.path(), "abcd1234");
        let victim = root.path().join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(root.path().join("victim.sock"), b"keep").unwrap();
        for bad in [
            "",
            "../victim",
            "../s/abcd1234",
            "ABCD1234",
            "abcd123",
            "a/b",
            "abcd1234/..",
        ] {
            assert!(
                matches!(reap_stale_entry(&dir, bad), Err(RunnerError::InvalidId)),
                "{bad:?}"
            );
        }
        assert!(sock.exists() && sdir.exists() && victim.exists());
        assert!(root.path().join("victim.sock").exists());
    }

    #[test]
    fn reaping_refuses_a_symlinked_socket_or_dir_removing_nothing() {
        // Symlinked socket entry: the target and the real dir survive.
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("s");
        std::fs::create_dir_all(dir.join("abcd1234")).unwrap();
        let outside = root.path().join("outside.sock");
        drop(UnixListener::bind(&outside).unwrap());
        symlink(&outside, dir.join("abcd1234.sock")).unwrap();
        assert!(matches!(
            reap_stale_entry(&dir, "abcd1234"),
            Err(RunnerError::BadRequest(_))
        ));
        assert!(outside.exists(), "the symlink target is untouched");
        assert!(dir.join("abcd1234").is_dir(), "all-or-nothing: dir kept");
        assert!(std::fs::symlink_metadata(dir.join("abcd1234.sock")).is_ok());

        // Symlinked session dir: its target's contents survive, and the
        // (valid) socket beside it is kept because the refusal is whole.
        let root = tempfile::tempdir().unwrap();
        let (dir, sock, sdir) = leftovers(root.path(), "abcd1234");
        std::fs::remove_dir_all(&sdir).unwrap();
        let target = root.path().join("precious");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("keep"), b"k").unwrap();
        symlink(&target, &sdir).unwrap();
        assert!(matches!(
            reap_stale_entry(&dir, "abcd1234"),
            Err(RunnerError::BadRequest(_))
        ));
        assert!(target.join("keep").exists());
        assert!(sock.exists(), "all-or-nothing: socket kept");
    }

    #[test]
    fn reaping_refuses_a_regular_file_posing_as_the_socket() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("s");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("abcd1234.sock"), b"not a socket").unwrap();
        assert!(matches!(
            reap_stale_entry(&dir, "abcd1234"),
            Err(RunnerError::BadRequest(_))
        ));
        assert!(dir.join("abcd1234.sock").exists());
    }

    /// A runner whose tmux is `script` (shell body) over a fresh run dir.
    fn scripted_runner(
        root: &Path,
        body: &str,
    ) -> crate::runner::Runner<crate::runner::supervisor::fake::FakeSupervisor> {
        use std::os::unix::fs::PermissionsExt;
        let script = root.join("tmux.sh");
        std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cfg = crate::runner::config::RunnerConfig::test_config(
            root.join("run"),
            script.to_string_lossy().into_owned(),
            PathBuf::from("/dev/null"),
        );
        crate::runner::Runner::with_supervisor(
            cfg,
            crate::runner::supervisor::fake::FakeSupervisor::new(),
        )
    }

    /// The runner's kill op on an id whose holder answers with a tmux error
    /// (no server): the leftovers go, the reply says so, the supervisor
    /// is not asked to stop anything.
    #[tokio::test]
    async fn kill_of_a_stale_id_reaps_its_files_through_the_runner_op() {
        let root = tempfile::tempdir().unwrap();
        let runner = scripted_runner(root.path(), "echo 'no server running' >&2; exit 1");
        let (_, sock, sdir) = leftovers(&runner.cfg.run_dir, "abcd1234");
        let out = kill_session(&runner, "abcd1234", Some(1)).await.unwrap();
        assert!(out.stale_removed && !out.forced && out.exit_code.is_none());
        assert!(!sock.exists() && !sdir.exists());
        assert!(runner.supervisor.calls.lock().unwrap().is_empty());
        // Gone from `ls`, and a second kill is an honest not-found.
        assert!(crate::runner::discovery::list_sessions(&runner)
            .await
            .is_empty());
        assert!(matches!(
            kill_session(&runner, "abcd1234", Some(1)).await,
            Err(RunnerError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn kill_refuses_an_invalid_id_and_a_symlinked_entry() {
        let root = tempfile::tempdir().unwrap();
        let runner = scripted_runner(root.path(), "exit 1");
        let (dir, _, _) = leftovers(&runner.cfg.run_dir, "abcd1234");
        assert!(matches!(
            kill_session(&runner, "../s/abcd1234", Some(1)).await,
            Err(RunnerError::InvalidId)
        ));
        let target = root.path().join("precious");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("keep"), b"k").unwrap();
        symlink(&target, dir.join("wxyz9876")).unwrap();
        assert!(matches!(
            kill_session(&runner, "wxyz9876", Some(1)).await,
            Err(RunnerError::BadRequest(_))
        ));
        assert!(target.join("keep").exists());
        assert!(dir.join("abcd1234.sock").exists(), "other sessions kept");
    }

    /// A tmux that cannot even be spawned proves nothing about the holder:
    /// the entry is kept and the failure surfaces.
    #[tokio::test]
    async fn kill_keeps_the_files_when_the_probe_fails_to_complete() {
        let root = tempfile::tempdir().unwrap();
        let mut runner = scripted_runner(root.path(), "exit 1");
        runner.tmux = crate::runner::tmux::RealTmux::new("/nonexistent/tmux".to_string());
        let (_, sock, sdir) = leftovers(&runner.cfg.run_dir, "abcd1234");
        assert!(matches!(
            kill_session(&runner, "abcd1234", Some(1)).await,
            Err(RunnerError::TmuxFailed(_))
        ));
        assert!(sock.exists() && sdir.exists());
    }

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

    /// A tmux stand-in script: `@kb_exit_steps` answers the JSON file,
    /// `@kb_conf_version`/`list-sessions` succeed, `#{pane_dead}` reports
    /// a clean exit, and every `send-keys` appends `<ns-timestamp> <args>`
    /// to a log. Returns `(runner, log path, dir guard)`.
    fn fake_runner(
        steps: &[crate::profiles::OwnedExitStep],
    ) -> (
        crate::runner::Runner<crate::runner::supervisor::DirectSupervisor>,
        std::path::PathBuf,
        tempfile::TempDir,
    ) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("sends.log");
        let steps_json = dir.path().join("steps.json");
        std::fs::write(&steps_json, serde_json::to_string(steps).unwrap()).unwrap();
        let script = dir.path().join("fake_tmux.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 case \"$*\" in\n\
                 *@kb_exit_steps*) cat {steps}; exit 0 ;;\n\
                 *send-keys*) echo \"$(date +%s%N) $*\" >> {log}; exit 0 ;;\n\
                 *'#{{pane_dead}}'*) printf '1 0\\n'; exit 0 ;;\n\
                 esac\n\
                 exit 0\n",
                steps = steps_json.display(),
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cfg = crate::runner::RunnerConfig::test_config(
            dir.path().join("run"),
            script.to_string_lossy().into_owned(),
            std::path::PathBuf::from("/dev/null"),
        );
        (crate::runner::Runner::direct(cfg), log, dir)
    }

    /// Timestamps (ns) of the send-keys log lines, with their key arg.
    fn send_times(log: &std::path::Path) -> Vec<(u128, String)> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                let ts: u128 = it.next()?.parse().ok()?;
                let key = l.rsplit(' ').next()?.to_string();
                Some((ts, key))
            })
            .collect()
    }

    /// The kill path SLEEPS: omp's `WaitMs(300)` steps put at least 300 ms
    /// between its `C-c` sends, and claude's `/exit` then `Enter` (no
    /// WaitMs) are at least 150 ms apart. Lower bounds only. A `Sleep`
    /// arm that became a no-op, or a missing key gap, fails this.
    #[tokio::test]
    async fn exit_steps_are_paced_by_waitms_and_the_min_key_gap() {
        use crate::profiles::{ExitStep, OwnedExitStep};
        // omp-shaped sequence.
        let omp = OwnedExitStep::from_slice(&[
            ExitStep::Keys(&["C-c"]),
            ExitStep::WaitMs(300),
            ExitStep::Keys(&["C-c"]),
            ExitStep::WaitMs(300),
            ExitStep::Keys(&["C-d"]),
        ]);
        let (runner, log, _dir) = fake_runner(&omp);
        let sock = runner.holder_socket("abcd1234");
        let steps = discovery::exit_steps(&runner, "abcd1234")
            .await
            .expect("fake tmux answers the steps JSON");
        assert_eq!(steps, omp);
        run_exit_steps(&runner, &sock, &steps).await;
        let t = send_times(&log);
        assert_eq!(t.len(), 3, "three sends: {t:?}");
        let ms = |a: u128, b: u128| (b - a) / 1_000_000;
        assert!(ms(t[0].0, t[1].0) >= 300, "C-c to C-c gap: {t:?}");
        assert!(ms(t[1].0, t[2].0) >= 300, "C-c to C-d gap: {t:?}");

        // claude-shaped sequence: Text then Keys, no WaitMs.
        let claude =
            OwnedExitStep::from_slice(&[ExitStep::Text("/exit"), ExitStep::Keys(&["Enter"])]);
        let (runner, log, _dir) = fake_runner(&claude);
        let sock = runner.holder_socket("abcd1234");
        let steps = discovery::exit_steps(&runner, "abcd1234").await.unwrap();
        run_exit_steps(&runner, &sock, &steps).await;
        let t = send_times(&log);
        assert_eq!(t.len(), 2, "two sends: {t:?}");
        assert_eq!(t[0].1, "/exit");
        assert_eq!(t[1].1, "Enter");
        assert!(ms(t[0].0, t[1].0) >= 150, "/exit to Enter gap: {t:?}");
    }

    /// The `forced` wiring: a clean leader exit (`pane_dead` `1 0`) with a
    /// supervisor that reports it had to SIGKILL a surviving group is
    /// `forced: true`; reverting the call site to derive `forced` from
    /// the exit code alone fails this.
    #[tokio::test]
    async fn kill_session_forced_follows_the_supervisor_sigkill_verdict() {
        use crate::runner::supervisor::fake::FakeSupervisor;
        let (runner, _log, dir) = fake_runner(&[]);
        let runner =
            crate::runner::Runner::with_supervisor(runner.cfg.clone(), FakeSupervisor::new());
        *runner.supervisor.stop_sigkill.lock().unwrap() = true;
        std::fs::create_dir_all(runner.sessions_dir()).unwrap();
        let out = kill_session(&runner, "abcd1234", Some(2)).await.unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(out.forced, "a SIGKILLed survivor forces the verdict");
        *runner.supervisor.stop_sigkill.lock().unwrap() = false;
        let out = kill_session(&runner, "abcd1234", Some(2)).await.unwrap();
        assert!(!out.forced, "a clean exit with no SIGKILL is not forced");
        drop(dir);
    }
}
