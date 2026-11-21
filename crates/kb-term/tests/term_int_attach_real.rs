//! term-int: the local `kb-term attach` against a REAL runner, a REAL tap and
//! real tmux 3.7c. `kb-term attach` runs inside a second ("driver") tmux pane
//! of a size that differs from the session grid, so the driver pane IS the
//! operator's outer terminal; its `capture-pane` is compared with the
//! session's own `capture-pane` after every step. Content is a plain
//! line-mode shell (no full-screen program), synthetic text only.
//!
//! Gated like the other term-int lanes: `KB_TMUX_BIN` and `KB_TAP_BIN` unset
//! means SKIP, unless `KB_REQUIRE_TMUX=1` (a missing binary then FAILS).
//! Every wait has a hard deadline.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kb_term::runner::config::RunnerConfig;
use kb_term::runner::rpc::dispatch;
use kb_term::runner::server::serve;
use kb_term::runner::supervisor::DirectSupervisor;
use kb_term::runner::Runner;
use kb_term::wire::{Harness, Request, Response};
use tokio::sync::broadcast;

#[allow(dead_code)]
#[path = "integration/common.rs"]
mod common;

/// The outer terminal's size for every test here.
const OUTER: (u16, u16) = (160, 50);

fn require_tap() -> Option<PathBuf> {
    match std::env::var("KB_TAP_BIN") {
        Ok(bin) if !bin.is_empty() => Some(PathBuf::from(bin)),
        _ => {
            if std::env::var("KB_REQUIRE_TMUX").as_deref() == Ok("1") {
                panic!("KB_REQUIRE_TMUX=1 but KB_TAP_BIN is unset");
            }
            println!("SKIP: KB_TAP_BIN unset (term-int lane only)");
            None
        }
    }
}

/// One tmux call on `sock` with a hard deadline; stdout as text.
fn tmux(bin: &str, sock: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new(bin);
    cmd.arg("-S").arg(sock).args(args);
    match common::run_bounded(&mut cmd, Duration::from_secs(10)) {
        Some(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        None => String::new(),
    }
}

/// `capture-pane -p` rows, trailing blanks trimmed.
fn rows_of(text: &str) -> Vec<String> {
    let mut rows: Vec<String> = text.split('\n').map(|r| r.trim_end().to_string()).collect();
    if rows.last().is_some_and(String::is_empty) {
        rows.pop();
    }
    rows
}

/// Kills both tmux servers on drop (the holder is also killed by the
/// runner's own cleanup in production; a failing test must not leak).
struct Cleanup {
    tmux_bin: String,
    holder: PathBuf,
    driver: PathBuf,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        tmux(&self.tmux_bin, &self.driver, &["kill-server"]);
        tmux(&self.tmux_bin, &self.holder, &["kill-server"]);
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    _cleanup: Cleanup,
    _shutdown: broadcast::Sender<()>,
    runner: Arc<Runner<DirectSupervisor>>,
    tmux_bin: String,
    id: String,
    holder: PathBuf,
    driver: PathBuf,
}

impl Rig {
    /// Launch a line-mode shell session at `grid`, put `lines` into it, and
    /// open the driver pane (a plain shell in an `OUTER` tmux window).
    async fn start(tmux_bin: String, tap_bin: PathBuf, grid: (u16, u16), lines: &[String]) -> Rig {
        let dir = tempfile::Builder::new()
            .prefix("kb-term-attnote-")
            .tempdir_in("/tmp")
            .unwrap();
        let run_dir = dir.path().join("run");
        std::fs::create_dir_all(run_dir.join("s")).unwrap();
        std::fs::create_dir_all(run_dir.join("runner")).unwrap();
        let mut cfg =
            RunnerConfig::test_config(run_dir.clone(), tmux_bin.clone(), common::conf_path());
        cfg.require_tap = true;
        cfg.tap_ready_secs = 30;
        cfg.allow_verbatim_probe = true;
        cfg.tap_bin = Some(tap_bin);
        let runner = Arc::new(Runner::direct(cfg));
        let sock_path = run_dir.join("runner").join("runner.sock");
        let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();
        let (shutdown, shutdown_rx) = broadcast::channel::<()>(4);
        tokio::spawn(serve(
            runner.clone(),
            listener,
            sock_path.clone(),
            shutdown_rx,
        ));

        // A line-mode shell with a short prompt. Verbatim argv may not carry
        // `-c`, so the shell is a one-line script.
        let script = dir.path().join("shell.sh");
        std::fs::write(&script, "#!/bin/sh\nPS1='q> '\nexport PS1\nexec sh -i\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let launch = Request::Launch {
            harness: Harness::Omp,
            argv: Some(vec![script.to_string_lossy().into_owned()]),
            resume_sid: None,
            handoff_from: None,
            cwd: dir.path().to_string_lossy().into_owned(),
            cols: Some(grid.0),
            rows: Some(grid.1),
            env: None,
            exit_keys: None,
        };
        let id = match dispatch(&runner, &launch).await {
            Response::Ok { data } => data["id"].as_str().unwrap().to_string(),
            Response::Err { code, message } => panic!("launch failed ({code}): {message}"),
        };
        let holder = runner.holder_socket(&id);
        let driver = dir.path().join("driver.sock");
        let cleanup = Cleanup {
            tmux_bin: tmux_bin.clone(),
            holder: holder.clone(),
            driver: driver.clone(),
        };
        let rig = Rig {
            _dir: dir,
            _cleanup: cleanup,
            _shutdown: shutdown,
            runner,
            tmux_bin,
            id,
            holder,
            driver,
        };
        // Content first, so the attach has something to mirror.
        tokio::time::sleep(Duration::from_millis(500)).await;
        for line in lines {
            tmux(
                &rig.tmux_bin,
                &rig.holder,
                &["send-keys", "-t", "kb", "-l", line],
            );
            tmux(
                &rig.tmux_bin,
                &rig.holder,
                &["send-keys", "-t", "kb", "Enter"],
            );
        }
        if let Some(last) = lines.last() {
            let want = last.rsplit(' ').next().unwrap_or("").to_string();
            rig.wait_until("session shows its content", |r| {
                r.holder_rows().iter().any(|row| row == &want)
            })
            .await;
        }
        // The driver: a plain shell, the attach typed into it like an
        // operator would (that typed line is what a join must clear).
        let (c, r) = (OUTER.0.to_string(), OUTER.1.to_string());
        tmux(
            &rig.tmux_bin,
            &rig.driver,
            &[
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "drv",
                "-x",
                c.as_str(),
                "-y",
                r.as_str(),
                "/bin/sh",
            ],
        );
        rig
    }

    /// The session's rows, scrollback first (`-S -`): the mirror paints the
    /// twin's scrollback and screen from the outer terminal's top row, so for
    /// a session shorter than the screen the outer rows are history +
    /// screen. (tmux reflows a narrowed pane into a different number of
    /// history rows than the twin does when the session is not full, which a
    /// screen-only capture would show as a shifted top.)
    fn holder_rows(&self) -> Vec<String> {
        rows_of(&tmux(
            &self.tmux_bin,
            &self.holder,
            &["capture-pane", "-p", "-S", "-", "-t", "kb"],
        ))
    }

    fn driver_rows(&self) -> Vec<String> {
        rows_of(&tmux(
            &self.tmux_bin,
            &self.driver,
            &["capture-pane", "-p", "-t", "drv"],
        ))
    }

    /// The driver row `i`, padded: capture-pane drops trailing blank rows.
    fn driver_row(&self, i: usize) -> String {
        self.driver_rows().get(i).cloned().unwrap_or_default()
    }

    async fn wait_until(&self, what: &str, f: impl Fn(&Rig) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if f(self) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("timed out waiting for: {what}");
    }

    /// Type the attach command into the driver shell.
    fn type_attach(&self) {
        let bin = std::env::var("CARGO_BIN_EXE_kb-term").expect("CARGO_BIN_EXE_kb-term");
        let sock = self.runner.cfg.run_dir.join("runner").join("runner.sock");
        let line = format!("{bin} attach {} --runner-sock {}", self.id, sock.display());
        self.type_driver(&line);
        tmux(
            &self.tmux_bin,
            &self.driver,
            &["send-keys", "-t", "drv", "Enter"],
        );
    }

    fn type_driver(&self, text: &str) {
        tmux(
            &self.tmux_bin,
            &self.driver,
            &["send-keys", "-t", "drv", "-l", text],
        );
    }

    /// The mismatches between the driver pane and the session, as short
    /// shape lines (row index and the two rows). The driver's LAST row is the
    /// attach's status line and is not compared; rows under the session's
    /// grid must be blank.
    fn mismatches(&self) -> Vec<String> {
        let hold = self.holder_rows();
        let drv = self.driver_rows();
        let last = usize::from(OUTER.1) - 1;
        let mut bad = Vec::new();
        for i in 0..last {
            let h = hold.get(i).map(String::as_str).unwrap_or("");
            let d = drv.get(i).map(String::as_str).unwrap_or("");
            if h != d {
                bad.push(format!("row {i}: session {h:?} / outer {d:?}"));
            }
        }
        bad
    }

    /// After a step: the driver pane equals the session (content rows).
    async fn expect_equal(&self, step: &str) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let bad = self.mismatches();
            if bad.is_empty() {
                return;
            }
            if Instant::now() >= deadline {
                panic!(
                    "step `{step}`: the outer terminal differs from the session on {} row(s):\n{}",
                    bad.len(),
                    bad.join("\n")
                );
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    async fn call_ok(&self, req: Request) {
        if let Response::Err { code, message } = dispatch(&self.runner, &req).await {
            panic!("runner call failed ({code}): {message}");
        }
    }
}

fn gate() -> Option<(String, PathBuf)> {
    let tmux_bin = common::require_tmux()?;
    let tap = require_tap()?;
    Some((tmux_bin, tap))
}

/// A3: the session was launched at a grid that differs from the outer
/// terminal's; the attach takes the outer size AFTER its seed was painted at
/// the old grid. The outer screen must still end equal to the session
/// (rows shifted by a reflowed long line, a missing line and a doubled prompt
/// were the symptoms), and the shell line that started the attach must not
/// survive under the mirror.
#[tokio::test(flavor = "multi_thread")]
async fn attach_taking_a_different_grid_ends_equal_to_the_session() {
    let Some((tmux_bin, tap)) = gate() else {
        return;
    };
    let lines = vec![
        format!("echo {}", "x".repeat(195)),
        "echo alpha".to_string(),
        "echo beta".to_string(),
    ];
    // Launched wider and shorter than the outer terminal.
    let rig = Rig::start(tmux_bin, tap, (201, 48), &lines).await;
    rig.type_attach();
    rig.expect_equal("attach takes a different grid").await;
}

/// A1/A2/A4: the whole ownership walk against a line-mode shell. After each
/// step the outer terminal's content rows equal the session's, a notice only
/// ever lives on the last row, and the notice text says what the next key
/// does.
#[tokio::test(flavor = "multi_thread")]
async fn attach_notices_and_resume_never_corrupt_the_mirrored_rows() {
    let Some((tmux_bin, tap)) = gate() else {
        return;
    };
    let lines = vec![
        "echo one".to_string(),
        "echo two".to_string(),
        "echo three".to_string(),
    ];
    // Same size as the outer terminal: the only differences below are the
    // ones the steps cause.
    let rig = Rig::start(tmux_bin, tap, OUTER, &lines).await;
    let last = usize::from(OUTER.1) - 1;
    rig.type_attach();
    rig.expect_equal("attach").await;
    assert_eq!(
        rig.driver_row(last),
        "",
        "no notice while the attach owns the grid"
    );

    // 1. The web takes the grid (runner TakeSize, owner `kb-term`).
    rig.call_ok(Request::TakeSize {
        id: rig.id.clone(),
        cols: 120,
        rows: 40,
        owner: "kb-term".into(),
        force: false,
    })
    .await;
    rig.wait_until("the web owner status line", |r| {
        r.driver_row(last).contains("web panel owns the grid")
    })
    .await;
    rig.expect_equal("web take").await;

    // 2. A key while the web owns the grid: nothing is typed, nothing is
    // written into the mirrored rows.
    rig.type_driver("k");
    tokio::time::sleep(Duration::from_millis(600)).await;
    rig.expect_equal("key while watching").await;
    assert!(
        rig.driver_row(last).contains("web panel owns the grid"),
        "the status line stays: {:?}",
        rig.driver_row(last)
    );

    // 3. The web releases the grid: the line now says what the next key does.
    rig.call_ok(Request::ReleaseSize {
        id: rig.id.clone(),
        owner: "kb-term".into(),
    })
    .await;
    rig.wait_until("the released status line", |r| {
        r.driver_row(last)
            .contains("press any key to take the grid back (that key is not sent)")
    })
    .await;
    rig.expect_equal("release").await;

    // 4. A key retakes the grid; that key is NOT sent, the screen repaints
    // clean (no residue of the notices).
    rig.type_driver("z");
    rig.wait_until("the attach took the grid back", |r| {
        r.driver_row(last).is_empty()
    })
    .await;
    rig.expect_equal("retake").await;
    assert!(
        !rig.holder_rows().iter().any(|row| row.contains('z')),
        "the retake key is not forwarded: {:?}",
        rig.holder_rows()
    );

    // 5. Typing works again and the mirror follows.
    rig.type_driver("echo done");
    tmux(
        &rig.tmux_bin,
        &rig.driver,
        &["send-keys", "-t", "drv", "Enter"],
    );
    rig.wait_until("the echo reached the session", |r| {
        r.holder_rows().iter().any(|row| row == "done")
    })
    .await;
    rig.expect_equal("echo").await;
}
