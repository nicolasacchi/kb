//! Shared helpers for the tmux integration tests.
//!
//! Every test starts with [`require_tmux`]: outside the term-int lane
//! (no `KB_TMUX_BIN`) the test prints `SKIP` and returns; under
//! `KB_REQUIRE_TMUX=1` a missing tmux is a failure. Every pty/socket
//! read has a hard deadline. Tests use random tempdirs under /tmp.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use kb_term::runner::config::RunnerConfig;
use kb_term::runner::supervisor::DirectSupervisor;
use kb_term::runner::Runner;
use kb_term::wire::{Harness, Request};

/// The pinned tmux, or `None` outside the term-int lane. Prints `SKIP`
/// and returns `None`; panics when `KB_REQUIRE_TMUX=1`.
pub fn require_tmux() -> Option<String> {
    match std::env::var("KB_TMUX_BIN") {
        Ok(bin) if !bin.is_empty() => Some(bin),
        _ => {
            if std::env::var("KB_REQUIRE_TMUX").as_deref() == Ok("1") {
                panic!("KB_REQUIRE_TMUX=1 but KB_TMUX_BIN is unset");
            }
            println!("SKIP: KB_TMUX_BIN unset (term-int lane only)");
            None
        }
    }
}

/// The absolute path of the crate's `kb-term.conf` v2.
pub fn conf_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("share")
        .join("kb-term.conf")
}

/// A fixture script by name.
pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Is `pid` gone (missing or zombie; a zombie answers kill(pid, 0))?
pub fn pid_gone(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(state) => {
            state
                .rsplit(')')
                .next()
                .and_then(|s| s.split_whitespace().next())
                == Some("Z")
        }
        Err(_) => true,
    }
}

/// Poll `f` every 20 ms until true or the deadline.
pub async fn poll_until<F, Fut>(deadline: Duration, mut f: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = Instant::now();
    loop {
        if f().await {
            return true;
        }
        if start.elapsed() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// An in-process runner on a tempdir under /tmp (supervisor=direct).
pub struct TestRunner {
    /// The runner (dispatch directly; the socket server is unit-tested).
    pub runner: Runner<DirectSupervisor>,
    /// The temp submission dir (deleted on drop).
    pub dir: tempfile::TempDir,
    /// The pinned tmux binary.
    pub tmux_bin: String,
}

impl TestRunner {
    pub fn new(tmux_bin: String) -> Self {
        Self::with_opts(tmux_bin, true)
    }

    pub fn with_opts(tmux_bin: String, require_tap: bool) -> Self {
        Self::with_tap(tmux_bin, require_tap, true)
    }

    pub fn with_tap(tmux_bin: String, require_tap: bool, with_fake_tap: bool) -> Self {
        // Integration fixtures run through the production dispatch
        // path, so enable verbatim argv via the explicit config flag
        // (test harness only; production kb-termd never sets it and
        // ambient env never gates it).
        let dir = tempfile::Builder::new()
            .prefix("kb-term-test-")
            .tempdir_in("/tmp")
            .unwrap();
        let run_dir = dir.path().join("run");
        std::fs::create_dir_all(run_dir.join("s")).unwrap();
        std::fs::create_dir_all(run_dir.join("runner")).unwrap();
        let mut cfg = RunnerConfig::test_config(run_dir, tmux_bin.clone(), conf_path());
        cfg.require_tap = require_tap;
        cfg.tap_ready_secs = 5;
        cfg.allow_verbatim_probe = true;
        if with_fake_tap {
            cfg.tap_bin = Some(fixture("fake_tap.sh"));
        }
        let runner = Runner::direct(cfg);
        TestRunner {
            runner,
            dir,
            tmux_bin,
        }
    }

    /// The socket of the holder for `id`.
    pub fn sock(&self, id: &str) -> PathBuf {
        self.runner.holder_socket(id)
    }

    /// Direct tmux call (deadline-bounded) for assertions.
    pub fn tmux(&self, sock: &Path, args: &[&str]) -> (bool, String) {
        let mut cmd = StdCommand::new(&self.tmux_bin);
        cmd.arg("-S").arg(sock).args(args);
        let out = run_bounded(&mut cmd, Duration::from_secs(10));
        match out {
            Some(o) => (
                o.status.success(),
                String::from_utf8_lossy(&o.stdout).trim().to_string(),
            ),
            None => (false, String::new()),
        }
    }

    /// A launch request for `argv` in the tempdir (asserts dispatch ok).
    pub async fn launch(&self, argv: Vec<String>) -> (String, u32) {
        let cwd = self.dir.path().to_string_lossy().into_owned();
        let req = launch_req(argv, cwd);
        let resp = kb_term::runner::rpc::dispatch(&self.runner, &req).await;
        match resp {
            kb_term::wire::Response::Ok { data } => {
                let id = data["id"].as_str().unwrap().to_string();
                let pid = data["pane_pid"].as_u64().unwrap() as u32;
                (id, pid)
            }
            kb_term::wire::Response::Err { code, message } => {
                panic!("launch failed ({code}): {message}")
            }
        }
    }

    /// A launch with raw dispatch (for error assertions).
    pub async fn launch_raw(&self, req: &Request) -> kb_term::wire::Response {
        kb_term::runner::rpc::dispatch(&self.runner, req).await
    }
}

pub fn launch_req(argv: Vec<String>, cwd: String) -> Request {
    Request::Launch {
        harness: Harness::Omp,
        argv: Some(argv),
        resume_sid: None,
        cwd,
        cols: Some(120),
        rows: Some(40),
        env: None,
        exit_keys: None,
    }
}

impl Drop for TestRunner {
    fn drop(&mut self) {
        // Cleanup guard: kill every holder still in the tempdir so a
        // failing test never leaks a tmux server. Also clear the legacy
        // ambient probe var (production no longer reads it) so no
        // leaked flag can order-depend a later test in this binary.
        std::env::remove_var("KB_TERM_TEST_PROBE");
        if let Ok(rd) = std::fs::read_dir(self.runner.sessions_dir()) {
            for entry in rd.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("sock") {
                    let mut cmd = StdCommand::new(&self.tmux_bin);
                    cmd.arg("-S").arg(&path).arg("kill-server");
                    let _ = run_bounded(&mut cmd, Duration::from_secs(10));
                }
            }
        }
    }
}

/// Run a command with a hard deadline (10-minute-lane protection): a
/// wedged tmux/test must fail fast, not burn the lane timeout. Returns
/// `None` on timeout or spawn failure (the child is killed and reaped).
pub fn run_bounded(cmd: &mut StdCommand, timeout: Duration) -> Option<std::process::Output> {
    use std::process::Stdio;
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}
