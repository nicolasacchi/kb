//! RS-U2 — the bounded, timed, process-GROUP-killed subprocess runner that
//! [`super::git::StoreGit`] and [`super::cred::GhCli`] share.
//!
//! This file spawns nothing by name: it takes an already-built
//! [`std::process::Command`] and owns only the lifecycle around it —
//!
//! * the child is started in its OWN process group (`process_group(0)`),
//!   so a timeout can `killpg` git AND every helper it forked
//!   (`git-remote-https`, `ssh`, a credential-helper shell) in one signal,
//!   rather than killing `git` and orphaning a hung `git-remote-https`;
//! * stdout and stderr are drained on their own threads into BOUNDED
//!   buffers (the tail beyond the cap is read and discarded, so a chatty
//!   child can never block on a full pipe or grow daemon memory without
//!   bound);
//! * the deadline is enforced by polling `try_wait`, and the group is
//!   killed while the child is still UNREAPED — a zombie's pid cannot be
//!   recycled, so the `killpg` can never land on an unrelated group.
//!
//! Synchronous on purpose (callers run it under `spawn_blocking` /
//! `run_blocking`, like every other git subprocess in this crate).

use std::io::{Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long a finished child's pipes may stay open (a grandchild still
/// holding them) before the group is killed to unblock the readers.
const PIPE_DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Inputs for one [`run`].
#[derive(Debug, Clone)]
pub(crate) struct RunSpec {
    pub timeout: Duration,
    pub stdout_cap: usize,
    pub stderr_cap: usize,
    pub stdin: Option<Vec<u8>>,
}

/// What a [`run`] observed. `status` is `None` only when the child had to
/// be killed (timeout).
#[derive(Debug)]
pub(crate) struct Captured {
    pub status: Option<ExitStatus>,
    pub stdout: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr: Vec<u8>,
    pub stderr_truncated: bool,
    pub timed_out: bool,
    pub elapsed: Duration,
}

#[derive(Default)]
struct Sink {
    buf: Vec<u8>,
    truncated: bool,
}

fn spawn_reader<R: Read + Send + 'static>(
    mut r: R,
    cap: usize,
) -> (Arc<Mutex<Sink>>, JoinHandle<()>) {
    let sink = Arc::new(Mutex::new(Sink::default()));
    let s2 = Arc::clone(&sink);
    let h = std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match r.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut s = s2.lock().unwrap_or_else(|p| p.into_inner());
                    let room = cap.saturating_sub(s.buf.len());
                    if room >= n {
                        s.buf.extend_from_slice(&chunk[..n]);
                    } else {
                        s.buf.extend_from_slice(&chunk[..room]);
                        s.truncated = true;
                    }
                }
            }
        }
    });
    (sink, h)
}

fn take(sink: &Arc<Mutex<Sink>>) -> (Vec<u8>, bool) {
    let mut s = sink.lock().unwrap_or_else(|p| p.into_inner());
    (std::mem::take(&mut s.buf), s.truncated)
}

/// SIGKILL every process in the group led by `pgid`.
pub(crate) fn kill_group(pgid: u32) {
    let Ok(pgid) = libc::pid_t::try_from(pgid) else {
        return;
    };
    if pgid <= 1 {
        return; // never signal init / "every process we can"
    }
    // SAFETY: plain syscall; a stale/absent group just returns ESRCH.
    unsafe {
        libc::killpg(pgid, libc::SIGKILL);
    }
}

fn wait_join(handles: Vec<JoinHandle<()>>, deadline: Instant) -> bool {
    loop {
        if handles.iter().all(|h| h.is_finished()) {
            for h in handles {
                let _ = h.join();
            }
            return true;
        }
        if Instant::now() >= deadline {
            // Detached: the threads end when the (killed) writers close.
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Spawn `cmd` in a fresh process group and run it to completion or to
/// `spec.timeout`, whichever comes first. Overwrites `cmd`'s stdio.
pub(crate) fn run(cmd: &mut Command, spec: &RunSpec) -> std::io::Result<Captured> {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0)
        .stdin(if spec.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let start = Instant::now();
    let mut child: Child = cmd.spawn()?;
    let pgid = child.id();

    let (out_sink, out_h) =
        spawn_reader(child.stdout.take().expect("piped stdout"), spec.stdout_cap);
    let (err_sink, err_h) =
        spawn_reader(child.stderr.take().expect("piped stderr"), spec.stderr_cap);
    let mut handles = vec![out_h, err_h];
    if let (Some(mut stdin), Some(bytes)) = (child.stdin.take(), spec.stdin.clone()) {
        handles.push(std::thread::spawn(move || {
            // A child that exits without reading stdin gives EPIPE; that is
            // its decision to make, not an error of ours.
            let _ = stdin.write_all(&bytes);
        }));
    }

    let deadline = start + spec.timeout;
    let mut sleep = Duration::from_millis(2);
    let (status, timed_out) = loop {
        match child.try_wait()? {
            Some(st) => break (Some(st), false),
            None if Instant::now() >= deadline => {
                // Still unreaped here, so `pgid` cannot have been recycled.
                kill_group(pgid);
                let _ = child.wait();
                break (None, true);
            }
            None => {
                std::thread::sleep(sleep);
                sleep = (sleep * 2).min(Duration::from_millis(25));
            }
        }
    };

    if !wait_join(handles, Instant::now() + PIPE_DRAIN_GRACE) {
        // A descendant outlived the leader and still holds a pipe. It is a
        // member of the group (so the group id is still in use and cannot
        // have been recycled) — kill it to release the readers.
        kill_group(pgid);
    }

    let (stdout, stdout_truncated) = take(&out_sink);
    let (stderr, stderr_truncated) = take(&err_sink);
    Ok(Captured {
        status,
        stdout,
        stdout_truncated,
        stderr,
        stderr_truncated,
        timed_out,
        elapsed: start.elapsed(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(timeout_ms: u64) -> RunSpec {
        RunSpec {
            timeout: Duration::from_millis(timeout_ms),
            stdout_cap: 16,
            stderr_cap: 1024,
            stdin: None,
        }
    }

    fn alive(pid: i32) -> bool {
        // A zombie is dead for our purposes (it holds no resources and
        // will be reaped by init).
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(s) => {
                let state = s.rsplit(')').next().unwrap_or("").trim_start();
                !state.starts_with('Z')
            }
            Err(_) => false,
        }
    }

    #[test]
    fn a_timeout_kills_the_whole_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("bg.pid");
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(format!(
            "sleep 60 & echo $! > {}; sleep 60",
            pidfile.display()
        ));
        let t0 = Instant::now();
        let got = run(&mut cmd, &spec(400)).unwrap();
        assert!(got.timed_out);
        assert!(got.status.is_none());
        assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
        let bg: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        while alive(bg) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !alive(bg),
            "the backgrounded grandchild survived the group kill"
        );
    }

    #[test]
    fn stdout_is_bounded_and_the_child_is_drained() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("head -c 200000 /dev/zero; echo done >&2");
        let got = run(&mut cmd, &spec(10_000)).unwrap();
        assert!(got.status.unwrap().success());
        assert_eq!(got.stdout.len(), 16);
        assert!(got.stdout_truncated);
        assert_eq!(String::from_utf8_lossy(&got.stderr).trim(), "done");
    }

    #[test]
    fn stdin_is_fed() {
        let mut cmd = Command::new("cat");
        let mut s = spec(10_000);
        s.stdin = Some(b"hello".to_vec());
        let got = run(&mut cmd, &s).unwrap();
        assert_eq!(got.stdout, b"hello");
    }
}
