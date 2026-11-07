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
    /// Stop the tap and the holder for `id`, reaping strays.
    fn stop(&self, id: &str, ctx: StopCtx) -> impl Future<Output = Result<(), RunnerError>> + Send;
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

    async fn stop(&self, id: &str, _ctx: StopCtx) -> Result<(), RunnerError> {
        systemctl(&[
            "stop",
            &format!("kb-tap@{id}.service"),
            &format!("kb-term@{id}.service"),
        ])
        .await
    }
}

async fn systemctl(args: &[&str]) -> Result<(), RunnerError> {
    let mut cmd = Command::new("systemctl");
    cmd.arg("--user").args(args).env_clear();
    cmd.env("HOME", std::env::var("HOME").unwrap_or_default());
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

    async fn stop(&self, id: &str, ctx: StopCtx) -> Result<(), RunnerError> {
        // Stop the tap first (it is attached to the holder).
        let tap = self.taps.lock().unwrap().remove(id);
        if let Some(mut held) = tap {
            kill_held(&mut held).await;
        }
        // Direct-mode kill of a holder: SIGTERM the server and the pane
        // process group (the pane child is its own session leader, so
        // pgid == pane_pid), wait 2 s, then SIGKILL both and reap the
        // Child. `forced` is the caller's diagnosis; this path always
        // SIGKILLs whatever survived the SIGTERM grace.
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
        let holder = self.holders.lock().unwrap().remove(id);
        match holder {
            Some(mut held) => {
                // The holder was setsid'd, so its pgid is the child pid.
                let pid = held.child.id().unwrap_or(0);
                if pid != 0 {
                    kill_group(pid, libc::SIGTERM);
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
                if let Some(pane) = ctx.pane_pid {
                    kill_group(pane, libc::SIGKILL);
                }
                if let Some(server) = ctx.server_pid {
                    kill_group(server, libc::SIGKILL);
                }
                if pid != 0 {
                    kill_group(pid, libc::SIGKILL);
                }
                kill_held(&mut held).await;
            }
            None => {
                // No held Child (kill-after-restart): still run the
                // SIGKILL grace against the pane/server groups, then
                // return.
                tokio::time::sleep(Duration::from_secs(2)).await;
                if let Some(pane) = ctx.pane_pid {
                    kill_group(pane, libc::SIGKILL);
                }
                if let Some(server) = ctx.server_pid {
                    kill_group(server, libc::SIGKILL);
                }
            }
        }
        Ok(())
    }
}

async fn kill_held(held: &mut Held) {
    // Close the control stdin so a control-mode client sees EOF, SIGTERM
    // the process group, and wait() so no zombie remains.
    drop(held.stdin.take());
    let pid = held.child.id().unwrap_or(0);
    if pid != 0 {
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

    /// A fake [`Supervisor`] that records every call.
    #[derive(Default)]
    pub struct FakeSupervisor {
        /// `(method, id)` per call, in order.
        pub calls: Mutex<Vec<(String, String)>>,
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

        async fn stop(&self, id: &str, _ctx: StopCtx) -> Result<(), RunnerError> {
            self.calls
                .lock()
                .unwrap()
                .push(("stop".to_string(), id.to_string()));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn unit_files_pin_the_lifecycle() {
        let holder = include_str!("../../share/systemd/kb-term@.service");
        for needle in [
            "Description=kb-term holder %i",
            "ExecStart=%h/.local/libexec/kb-term/tmux -D -S /run/kb-term/s/%i.sock -f %h/.local/share/kb-term/kb-term.conf",
            "Restart=no",
            "KillMode=control-group",
            "SendSIGKILL=yes",
            "TimeoutStopSec=10",
        ] {
            assert!(holder.contains(needle), "kb-term@ must contain `{needle}`");
        }
        assert!(
            !holder.contains("[Install]"),
            "kb-term@ must not have an [Install] section"
        );

        let tap = include_str!("../../share/systemd/kb-tap@.service");
        for needle in [
            "PartOf=kb-term@%i.service",
            "After=kb-term@%i.service",
            "ExecStart=%h/.local/bin/kb-tap --session %i --run-dir /run/kb-term",
            "Restart=on-failure",
            "RestartSec=1",
        ] {
            assert!(tap.contains(needle), "kb-tap@ must contain `{needle}`");
        }

        let runner_unit = include_str!("../../share/systemd/kb-termd.service");
        for needle in [
            "ExecStart=%h/.local/bin/kb-termd --supervisor systemd",
            "Restart=on-failure",
            "KillMode=process",
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
