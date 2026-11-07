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
        let out = StdCommand::new(&self.tmux_bin)
            .arg("-S")
            .arg(sock)
            .args(args)
            .output();
        match out {
            Ok(o) => (
                o.status.success(),
                String::from_utf8_lossy(&o.stdout).trim().to_string(),
            ),
            Err(_) => (false, String::new()),
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
        // failing test never leaks a tmux server.
        if let Ok(rd) = std::fs::read_dir(self.runner.sessions_dir()) {
            for entry in rd.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("sock") {
                    let _ = StdCommand::new(&self.tmux_bin)
                        .arg("-S")
                        .arg(&path)
                        .arg("kill-server")
                        .output();
                }
            }
        }
    }
}
