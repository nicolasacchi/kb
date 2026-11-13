//! Holder supervision: systemd units in production, direct processes in CI.
//!
//! The systemd supervisor only ever runs `systemctl --user` (argv only).
//! The direct supervisor spawns `<tmux-bin> -D -S <sock> -f <conf>` with
//! `setsid` (the holder survives the runner) and KEEPS the `Child`
//! handle, `wait()`ing it after a kill so no zombie remains.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use tokio::process::Command;

use super::{kill_group, RunnerError};

/// Context a kill passes to the supervisor's stop path.
#[derive(Debug, Clone, Default)]
pub struct StopCtx {
    /// The pane pid read before touching the server (direct mode only).
    pub pane_pid: Option<u32>,
    /// The tmux server pid (direct mode only).
    pub server_pid: Option<u32>,
}

/// How the runner starts and stops holders and taps.
pub trait Supervisor: Send + Sync {
    /// Start the holder for `id` (`tmux -D` in its own session).
    fn start_holder(&self, id: &str) -> impl Future<Output = Result<(), RunnerError>> + Send;
    /// Start the tap for `id` (no-op when no tap binary is configured).
    fn start_tap(&self, id: &str) -> impl Future<Output = Result<(), RunnerError>> + Send;
    /// Stop the tap and the holder for `id`, reaping strays. Returns
    /// `true` when the SIGKILL path ran (something survived SIGTERM);
    /// the kill RPC derives `forced` from this instead of the pane exit.
    fn stop(
        &self,
        id: &str,
        ctx: StopCtx,
    ) -> impl Future<Output = Result<bool, RunnerError>> + Send;
}

/// The systemd supervisor: one `kb-term@<id>` unit per holder.
pub struct SystemdSupervisor;

impl Supervisor for SystemdSupervisor {
    async fn start_holder(&self, id: &str) -> Result<(), RunnerError> {
        systemctl(&["start", &format!("kb-term@{id}.service")]).await
    }

    async fn start_tap(&self, id: &str) -> Result<(), RunnerError> {
        systemctl(&["start", &format!("kb-tap@{id}.service")]).await
    }

    async fn stop(&self, id: &str, _ctx: StopCtx) -> Result<bool, RunnerError> {
        systemctl(&[
            "stop",
            &format!("kb-tap@{id}.service"),
            &format!("kb-term@{id}.service"),
        ])
        .await?;
        // systemd reaps the cgroup itself (`KillMode=control-group` +
        // `SendSIGKILL=yes`); there is no runner-side SIGKILL path to
        // report, so the answer is always "not needed".
        Ok(false)
    }
}

/// The minimal bus environment `systemctl --user` needs to reach the user
/// bus: `HOME` and `XDG_RUNTIME_DIR` (plus `DBUS_SESSION_BUS_ADDRESS`
/// when the runner inherited one). A cleared environment carries none of
/// these, which breaks `systemctl --user` entirely.
fn bus_env_for<F: Fn(&str) -> Option<String>>(get: F) -> Vec<(String, String)> {
    ["HOME", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS"]
        .into_iter()
        .filter_map(|k| get(k).map(|v| (k.to_string(), v)))
        .collect()
}

/// The bus environment for `systemctl --user`, from this process's env.
fn systemctl_env() -> Vec<(String, String)> {
    bus_env_for(|k| std::env::var(k).ok())
}

async fn systemctl(args: &[&str]) -> Result<(), RunnerError> {
    let mut cmd = Command::new("systemctl");
    cmd.arg("--user").args(args);
    for (k, v) in systemctl_env() {
        cmd.env(k, v);
    }
    let out = tokio::time::timeout(Duration::from_secs(10), cmd.output())
        .await
        .map_err(|_| RunnerError::TmuxFailed("systemctl timed out".to_string()))?
        .map_err(|e| RunnerError::TmuxFailed(format!("spawn systemctl: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(RunnerError::TmuxFailed(format!(
            "systemctl {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// A spawned process the direct supervisor reaps.
struct Held {
    child: tokio::process::Child,
    stdin: Option<tokio::process::ChildStdin>,
}

/// The direct supervisor: holders and taps as plain (setsid) processes.
pub struct DirectSupervisor {
    tmux_bin: String,
    conf: PathBuf,
    run_dir: PathBuf,
    tap_bin: Option<PathBuf>,
    holders: StdMutex<HashMap<String, Held>>,
    taps: StdMutex<HashMap<String, Held>>,
}

impl DirectSupervisor {
    pub fn new(
        tmux_bin: String,
        conf: PathBuf,
        run_dir: PathBuf,
        tap_bin: Option<PathBuf>,
    ) -> Self {
        DirectSupervisor {
            tmux_bin,
            conf,
            run_dir,
            tap_bin,
            holders: StdMutex::new(HashMap::new()),
            taps: StdMutex::new(HashMap::new()),
        }
    }

    pub fn socket(&self, id: &str) -> PathBuf {
        self.run_dir.join("s").join(format!("{id}.sock"))
    }
}

impl Supervisor for DirectSupervisor {
    async fn start_holder(&self, id: &str) -> Result<(), RunnerError> {
        let sock = self.socket(id);
        let mut cmd = Command::new(&self.tmux_bin);
        cmd.arg("-D").arg("-S").arg(&sock).arg("-f").arg(&self.conf);
        cmd.env_clear();
        cmd.env("LANG", "C.UTF-8");
        cmd.env("HOME", std::env::var("HOME").unwrap_or_default());
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // setsid: the holder must survive the runner's exit.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd
            .spawn()
            .map_err(|e| RunnerError::TmuxFailed(format!("spawn holder: {e}")))?;
        self.holders
            .lock()
            .unwrap()
            .insert(id.to_string(), Held { child, stdin: None });
        Ok(())
    }

    async fn start_tap(&self, id: &str) -> Result<(), RunnerError> {
        let Some(tap_bin) = &self.tap_bin else {
            return Ok(());
        };
        let mut cmd = Command::new(tap_bin);
        cmd.arg("--session")
            .arg(id)
            .arg("--run-dir")
            .arg(&self.run_dir);
        cmd.env_clear();
        cmd.env("LANG", "C.UTF-8");
        cmd.env("HOME", std::env::var("HOME").unwrap_or_default());
        cmd.env("KB_TMUX_BIN", &self.tmux_bin);
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| RunnerError::TmuxFailed(format!("spawn tap: {e}")))?;
        let stdin = child.stdin.take();
        self.taps
            .lock()
            .unwrap()
            .insert(id.to_string(), Held { child, stdin });
        Ok(())
    }

    async fn stop(&self, id: &str, ctx: StopCtx) -> Result<bool, RunnerError> {
        // Stop the tap first (it is attached to the holder).
        let tap = self.taps.lock().unwrap().remove(id);
        if let Some(mut held) = tap {
            kill_held(&mut held).await;
        }
        // Direct-mode kill of a holder: SIGTERM the server and the pane
        // process group (the pane child is its own session leader, so
        // pgid == pane_pid), then poll the GROUPS (not just the leader
        // pids): every group that still has a live member after the
        // grace gets SIGKILL. Returns `true` when a SIGKILL was sent.
        //
        // The pane/server signals come from `ctx` BEFORE the holders-map
        // lookup: after a runner restart the map is empty (the holder is
        // a setsid survivor, not re-adopted into the map), and a
        // state-free kill must still reap the adopted session. Only the
        // wait()/reap below needs the held Child.
        if let Some(pane) = ctx.pane_pid {
            kill_group(pane, libc::SIGTERM);
        }
        if let Some(server) = ctx.server_pid {
            kill_group(server, libc::SIGTERM);
        }
        let mut holder = self.holders.lock().unwrap().remove(id);
        let child_pid = holder.as_ref().and_then(|h| h.child.id()).unwrap_or(0);
        if child_pid != 0 {
            kill_group(child_pid, libc::SIGTERM);
        }
        let groups: Vec<u32> = [ctx.pane_pid, ctx.server_pid, Some(child_pid)]
            .into_iter()
            .flatten()
            .filter(|p| *p > 1)
            .collect();
        wait_groups_dead(&groups, holder.as_mut()).await;
        // SIGKILL every group that still has a live member, re-probing
        // right before each signal so an emptied pgid is never signalled.
        // A member that ignored HUP/TERM while its leader exited cleanly
        // is caught here: the probe is per GROUP, not per leader pid.
        let mut sigkill = false;
        for g in &groups {
            if group_alive(*g) {
                kill_group(*g, libc::SIGKILL);
                sigkill = true;
            }
        }
        if let Some(mut held) = holder {
            if sigkill {
                kill_held(&mut held).await;
            } else {
                let _ = held.child.wait().await;
            }
        }
        Ok(sigkill)
    }
}

/// Poll (up to 2 s, 50 ms apart) until every given process group is
/// empty. The held holder Child is reaped with `try_wait` each round (a
/// zombie leader keeps its group "present" until reaped), but its group
/// is still probed like the others.
async fn wait_groups_dead(groups: &[u32], mut holder: Option<&mut Held>) {
    for _ in 0..40 {
        if let Some(h) = holder.as_deref_mut() {
            let _ = h.child.try_wait();
        }
        if !groups.iter().any(|g| group_alive(*g)) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A process group is alive while `kill(-pgid, 0)` succeeds (or answers
/// EPERM) AND at least one member is not a zombie. ESRCH means empty.
/// When `/proc` cannot be scanned the kill probe alone decides.
fn group_alive(pgid: u32) -> bool {
    if pgid <= 1 {
        return false;
    }
    let present = unsafe {
        if libc::kill(-(pgid as i32), 0) == 0 {
            true
        } else {
            std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
        }
    };
    if !present {
        return false;
    }
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return true;
    };
    for ent in dir.flatten() {
        let name = ent.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{name}/stat")) else {
            continue;
        };
        // `pid (comm) state ppid pgrp ...`: comm may contain anything, so
        // split after the LAST `)`.
        let mut f = stat.rsplit(')').next().unwrap_or("").split_whitespace();
        let state = f.next();
        let _ppid = f.next();
        let pgrp = f.next().and_then(|v| v.parse::<u32>().ok());
        if pgrp == Some(pgid) && state != Some("Z") {
            return true;
        }
    }
    false
}

async fn kill_held(held: &mut Held) {
    // Close the control stdin so a control-mode client sees EOF, SIGTERM
    // the process group (unless it already exited — `try_wait` first so
    // an exited child is never signalled), and wait() so no zombie
    // remains.
    drop(held.stdin.take());
    let pid = held.child.id().unwrap_or(0);
    if pid != 0 && matches!(held.child.try_wait(), Ok(None)) {
        kill_group(pid, libc::SIGTERM);
        tokio::time::sleep(Duration::from_millis(200)).await;
        kill_group(pid, libc::SIGKILL);
    }
    let _ = held.child.wait().await;
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::sync::Mutex;

    /// Recorded stop contexts, in order: `(id, pane_pid, server_pid)`.
    type StopCtxs = Vec<(String, Option<u32>, Option<u32>)>;

    /// A fake [`Supervisor`] that records every call.
    #[derive(Default)]
    pub struct FakeSupervisor {
        /// `(method, id)` per call, in order.
        pub calls: Mutex<Vec<(String, String)>>,
        /// Every stop's context, in order: `(id, pane_pid, server_pid)`.
        pub stop_ctxs: Mutex<StopCtxs>,
        /// What `stop` answers (the SIGKILL-path verdict).
        pub stop_sigkill: Mutex<bool>,
    }

    impl FakeSupervisor {
        pub fn new() -> Self {
            FakeSupervisor::default()
        }
    }

    impl Supervisor for FakeSupervisor {
        async fn start_holder(&self, id: &str) -> Result<(), RunnerError> {
            self.calls
                .lock()
                .unwrap()
                .push(("start_holder".to_string(), id.to_string()));
            Ok(())
        }

        async fn start_tap(&self, id: &str) -> Result<(), RunnerError> {
            self.calls
                .lock()
                .unwrap()
                .push(("start_tap".to_string(), id.to_string()));
            Ok(())
        }

        async fn stop(&self, id: &str, ctx: StopCtx) -> Result<bool, RunnerError> {
            self.calls
                .lock()
                .unwrap()
                .push(("stop".to_string(), id.to_string()));
            self.stop_ctxs
                .lock()
                .unwrap()
                .push((id.to_string(), ctx.pane_pid, ctx.server_pid));
            Ok(*self.stop_sigkill.lock().unwrap())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `systemctl --user` needs HOME + XDG_RUNTIME_DIR (+ the bus address
    /// when inherited); a cleared environment breaks the user bus, so the
    /// forwarded set is exactly these three, nothing else.
    #[test]
    fn systemctl_forwards_exactly_the_user_bus_env() {
        let env: Vec<(String, String)> = bus_env_for(|k| match k {
            "HOME" => Some("/home/user".to_string()),
            "XDG_RUNTIME_DIR" => Some("/run/user/1000".to_string()),
            "DBUS_SESSION_BUS_ADDRESS" => Some("unix:path=/run/user/1000/bus".to_string()),
            "PATH" => Some("/usr/bin".to_string()),
            _ => None,
        });
        assert_eq!(
            env,
            vec![
                ("HOME".to_string(), "/home/user".to_string()),
                ("XDG_RUNTIME_DIR".to_string(), "/run/user/1000".to_string()),
                (
                    "DBUS_SESSION_BUS_ADDRESS".to_string(),
                    "unix:path=/run/user/1000/bus".to_string()
                ),
            ],
            "only the bus trio, never PATH or anything else"
        );
        // When the bus address was not inherited it is absent, not empty.
        let env: Vec<(String, String)> = bus_env_for(|k| match k {
            "HOME" => Some("/home/user".to_string()),
            "XDG_RUNTIME_DIR" => Some("/run/user/1000".to_string()),
            _ => None,
        });
        assert_eq!(
            env,
            vec![
                ("HOME".to_string(), "/home/user".to_string()),
                ("XDG_RUNTIME_DIR".to_string(), "/run/user/1000".to_string()),
            ]
        );
    }

    #[test]
    fn unit_files_pin_the_lifecycle() {
        let holder = include_str!("../../share/systemd/kb-term@.service");
        for needle in [
            "Description=kb-term holder %i",
            "Environment=KB_TMUX_BIN=%h/.local/libexec/kb-term/tmux",
            // The holder must exec the pinned tmux by path (it is never
            // on PATH), never a bare `tmux` lookup resolved against the
            // user manager's PATH.
            "ExecStart=/bin/sh -c 'exec \"$KB_TMUX_BIN\" -D -S /run/kb-term/s/%i.sock -f %h/.local/share/kb-term/kb-term.conf'",
            "Restart=no",
            "KillMode=control-group",
            "SendSIGKILL=yes",
            "TimeoutStopSec=10",
            // `tmux -D` runs the server in the foreground, so the unit's
            // main process IS the tmux server (exec'd by the shell): stop
            // terminates it and a server exit ends the unit. The systemd
            // lifecycle itself is not exercised on CI.
            "Type=exec",
        ] {
            assert!(holder.contains(needle), "kb-term@ must contain `{needle}`");
        }
        assert!(
            !holder.contains("Type=simple"),
            "kb-term@ must not use Type=simple"
        );
        assert!(
            !holder.contains("sleep infinity"),
            "kb-term@ must not mask a server exit behind a placeholder process"
        );
        assert!(
            !holder.contains("[Install]"),
            "kb-term@ must not have an [Install] section"
        );

        let tap = include_str!("../../share/systemd/kb-tap@.service");
        for needle in [
            "PartOf=kb-term@%i.service",
            "After=kb-term@%i.service",
            // The tap needs the pinned tmux and the session's harness,
            // discovered from the holder's `@kb_harness` at start.
            "Environment=KB_TMUX_BIN=%h/.local/libexec/kb-term/tmux",
            "ExecStart=/bin/sh -c 'h=$(%h/.local/libexec/kb-term/tmux -S /run/kb-term/s/%i.sock show-options -gqv @kb_harness); exec %h/.local/bin/kb-tap --session %i --run-dir /run/kb-term --harness \"$h\"'",
            "Restart=on-failure",
            "RestartSec=1",
        ] {
            assert!(tap.contains(needle), "kb-tap@ must contain `{needle}`");
        }

        let runner_unit = include_str!("../../share/systemd/kb-termd.service");
        for needle in [
            // Config flows into kb-termd via the unit invocation.
            "Environment=KB_TMUX_BIN=%h/.local/libexec/kb-term/tmux",
            "Environment=KB_TERM_RUN_DIR=/run/kb-term",
            "ExecStart=%h/.local/bin/kb-termd --supervisor systemd",
            "Restart=on-failure",
            "KillMode=process",
            // No tmpfiles dirs, no start (and no silent Restart loop).
            "ConditionPathExists=/run/kb-term",
        ] {
            assert!(
                runner_unit.contains(needle),
                "kb-termd.service must contain `{needle}`"
            );
        }

        let tmpfiles = include_str!("../../share/tmpfiles.d/kb-term.conf");
        for line in [
            "d /run/kb-term         0755 __USER__ __USER__ -",
            "d /run/kb-term/s       0700 __USER__ __USER__ -",
            "d /run/kb-term/runner  0700 __USER__ __USER__ -",
        ] {
            assert!(
                tmpfiles.contains(line),
                "tmpfiles must contain the line `{line}` (three `d` lines)"
            );
        }
        assert!(
            !tmpfiles.contains("/home/"),
            "tmpfiles must not name a real home"
        );
    }
}
