//! `kb daemon` proves liveness with a lifetime flock on `kb-daemon.pid`.
//!
//! These spawn the REAL `kb` binary with `KB_HOME` pointing at a tempdir
//! and a scratch port. The state dir is `<KB_HOME>/state/<daemon-name>/`.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Kill-on-drop guard so a failing assert never leaks a process.
struct Proc {
    child: Child,
    stderr_path: Option<PathBuf>,
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Proc {
    fn stderr(&self) -> String {
        self.stderr_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default()
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn sleeper() -> Proc {
    Proc {
        child: Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn sleep"),
        stderr_path: None,
    }
}

fn kb_cmd(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_kb"));
    cmd.env("KB_HOME", home)
        .env_remove("KB_STATE_DIR")
        .env_remove("KB_CONFIG_DIR")
        .env_remove("KB_CACHE_DIR")
        .env_remove("RUST_LOG")
        .env_remove("KB_LOG_FILE_LEVEL");
    cmd
}

fn pid_file(home: &Path, name: &str) -> PathBuf {
    home.join("state").join(name).join("kb-daemon.pid")
}

fn write_cfg(home: &Path, name: &str, with_corpus: bool) -> (PathBuf, u16) {
    let port = free_port();
    let mut body = format!("[daemon]\nname = \"{name}\"\n");
    if with_corpus {
        let corpus = home.join("corpus");
        std::fs::create_dir_all(&corpus).unwrap();
        std::fs::write(
            corpus.join("smoke.html"),
            "<!doctype html><html><head><title>smoke</title></head><body><h1>smoke</h1></body></html>",
        )
        .unwrap();
        body.push_str(&format!(
            "\n[server]\naddr = \"127.0.0.1:{port}\"\n\n\
             [defaults]\ndisable_embedder_fallback = true\n\n\
             [kb.smoke]\npath = \"{}\"\n",
            corpus.display()
        ));
    }
    let cfg = home.join("kb.toml");
    std::fs::write(&cfg, body).unwrap();
    (cfg, port)
}

fn spawn_daemon(home: &Path, cfg: &Path, tag: &str) -> Proc {
    let stderr_path = home.join(format!("daemon-{tag}-stderr.log"));
    let child = kb_cmd(home)
        .args(["daemon", "--config"])
        .arg(cfg)
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&stderr_path).unwrap())
        .spawn()
        .expect("spawn kb daemon");
    Proc {
        child,
        stderr_path: Some(stderr_path),
    }
}

/// Wait until the daemon accepts TCP connections (the listener is bound
/// right after the lock is taken), panicking if it exits early.
fn wait_ready(d: &mut Proc, port: u16, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Some(status) = d.child.try_wait().unwrap() {
            panic!(
                "{what}: daemon exited early ({status}); stderr:\n{}",
                d.stderr()
            );
        }
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        if Instant::now() >= deadline {
            panic!(
                "{what}: timed out waiting for readiness; stderr:\n{}",
                d.stderr()
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn wait_exit(d: &mut Proc, secs: u64, what: &str) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(status) = d.child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            panic!(
                "{what}: did not exit within {secs}s; stderr:\n{}",
                d.stderr()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `stop` on a pid file nobody holds the lock on must signal nothing.
#[test]
fn kb_daemon_stop_on_an_unlocked_pid_file_signals_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (cfg, _) = write_cfg(tmp.path(), "lock-stop", false);
    let mut child = sleeper();
    let pf = pid_file(tmp.path(), "lock-stop");
    std::fs::create_dir_all(pf.parent().unwrap()).unwrap();
    std::fs::write(&pf, format!("{}\n", child.child.id())).unwrap();

    let out = kb_cmd(tmp.path())
        .args(["daemon", "stop", "--config"])
        .arg(&cfg)
        .output()
        .expect("run kb daemon stop");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "stop must fail; stderr:\n{stderr}");
    assert!(stderr.contains("stale"), "{stderr}");
    assert!(stderr.contains("nothing was signalled"), "{stderr}");

    std::thread::sleep(Duration::from_millis(500));
    assert!(
        child.child.try_wait().unwrap().is_none(),
        "the unrelated process was signalled"
    );
    assert!(pf.exists(), "stop must not unlink the pid file");
}

#[test]
fn daemon_lock_lifecycle_against_the_real_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let (cfg, port) = write_cfg(home, "lock-smoke", true);
    let pf = pid_file(home, "lock-smoke");

    // 1. A pid file naming a live unrelated process.
    let mut child = sleeper();
    std::fs::create_dir_all(pf.parent().unwrap()).unwrap();
    std::fs::write(&pf, format!("{}\n", child.child.id())).unwrap();

    // 2. D1 boots anyway (no lock is held).
    let mut d1 = spawn_daemon(home, &cfg, "d1");
    wait_ready(&mut d1, port, "step 2: D1 past a stale pid file");

    // 3. The file now names D1; the unrelated process is untouched.
    let d1_pid = d1.child.id();
    assert_eq!(
        std::fs::read_to_string(&pf).unwrap().trim(),
        d1_pid.to_string(),
        "step 3: pid file must hold D1's pid"
    );
    assert!(
        child.child.try_wait().unwrap().is_none(),
        "step 3: sleeper died"
    );

    // 4. D2 on the same state dir refuses.
    let mut d2 = spawn_daemon(home, &cfg, "d2");
    let status = wait_exit(&mut d2, 60, "step 4: D2");
    assert!(!status.success(), "step 4: D2 must exit non-zero");
    let err = d2.stderr();
    assert!(
        err.contains("another kb daemon is already running"),
        "step 4: {err}"
    );
    assert!(err.contains(&format!("pid {d1_pid}")), "step 4: {err}");

    // 5. SIGKILL D1.
    d1.child.kill().unwrap();
    d1.child.wait().unwrap();

    // 6. D3 boots: the lock died with D1 even though the file still names it.
    let mut d3 = spawn_daemon(home, &cfg, "d3");
    wait_ready(&mut d3, port, "step 6: D3 after SIGKILL of D1");

    // 7. stop signals D3, sees the lock free, and leaves the file.
    let out = kb_cmd(home)
        .args(["daemon", "stop", "--config"])
        .arg(&cfg)
        .output()
        .expect("run kb daemon stop");
    let stop_err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "step 7: stop failed:\n{stop_err}");
    assert!(stop_err.contains("exited cleanly"), "step 7: {stop_err}");
    wait_exit(&mut d3, 15, "step 7: D3");
    assert!(
        pf.exists(),
        "step 7: the lock path must never unlink the file"
    );

    // 8. The unrelated process lived through all of it.
    assert!(
        child.child.try_wait().unwrap().is_none(),
        "step 8: sleeper died"
    );
}
