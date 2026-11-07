//! kb-term integration tests: the runner against the pinned tmux 3.7c.
//!
//! ONE binary (`cargo test -p kb-term --test integration`, the term-int
//! lane's selector); the test fns are `term_int_*`. Run only where
//! `KB_TMUX_BIN` is set; `workspace-test` skips via [`common::require_tmux`].

mod common;

use std::time::Duration;

use common::{fixture, pid_gone, poll_until, require_tmux, TestRunner};
use kb_term::runner::discovery;
use kb_term::runner::rpc;
use kb_term::runner::{adopt_anchors, check_tmux_version, ensure_layout};
use kb_term::wire::{Request, Response};

/// Boot a runner the way kb-termd does: layout + version + adopt.
async fn boot_runner(tr: &TestRunner) {
    ensure_layout(&tr.runner.cfg).await.unwrap();
    check_tmux_version(&tr.runner.tmux, &tr.runner.cfg.expected_tmux)
        .await
        .unwrap();
    adopt_anchors(&tr.runner).await.unwrap();
}

// 1. The shipped conf loads clean and carries every documented option.
#[tokio::test(flavor = "multi_thread")]
async fn term_int_conf_loads_clean() {
    let Some(tmux_bin) = require_tmux() else {
        return;
    };
    // Load the conf into a throwaway holder (the production `-D -f` form)
    // and read the options back from the live `kb` session.
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("t.sock");
    let conf = common::conf_path();
    run_tmux_file_probe(&tmux_bin, &sock, &conf);
}

// Every assert below fails with the conf absent (window-size reads the
// default `latest`, @kb_conf_version reads empty) or with a conf line
// edited out: window-size manual is proven behaviorally by
// term_int_grid_is_fixed_until_resized, so a query that cannot see it
// (the round-5 `-g -w`, which read back `latest`) is the wrong query,
// not a broken conf. Each option is queried in its own scope: `-g` for
// session/server/user options (the production `show-options -gv`
// idiom), `-w` for the window option, `-p` for the pane options.
// With exactly one session on the server, an unscoped `-w`/`-p`
// resolves to its window/pane.
fn run_tmux_file_probe(tmux_bin: &str, sock: &std::path::Path, conf: &std::path::Path) {
    use std::process::Command;
    // Start the holder (tmux -D) with the shipped conf.
    let mut child = Command::new(tmux_bin)
        .arg("-D")
        .arg("-S")
        .arg(sock)
        .arg("-f")
        .arg(conf)
        .spawn()
        .unwrap();
    // The -D server must answer before source-file/show-options can run.
    let mut up = false;
    for _ in 0..100 {
        let mut cmd = Command::new(tmux_bin);
        cmd.arg("-S").arg(sock).arg("list-sessions");
        match common::run_bounded(&mut cmd, Duration::from_secs(2)) {
            Some(o) if o.status.success() => {
                up = true;
                break;
            }
            _ => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    assert!(up, "the -D holder with the shipped conf must answer");
    // `source-file -n` (no-exec) must exit 0 on the shipped conf.
    let mut cmd = Command::new(tmux_bin);
    cmd.arg("-S")
        .arg(sock)
        .arg("source-file")
        .arg("-n")
        .arg(conf);
    let out = common::run_bounded(&mut cmd, Duration::from_secs(10)).unwrap();
    assert!(out.status.success(), "source-file -n must exit 0");
    // Create the session the window/pane queries resolve to.
    let out = Command::new(tmux_bin)
        .arg("-S")
        .arg(sock)
        .arg("new-session")
        .arg("-d")
        .arg("-s")
        .arg("kb")
        .arg("--")
        .arg("/bin/sh")
        .arg("-c")
        .arg("sleep 30")
        .output()
        .unwrap();
    assert!(out.status.success(), "new-session must succeed");
    let checks: [(&str, &str, &[&str]); 8] = [
        ("@kb_conf_version", "2", &["-g"]),
        ("window-size", "manual", &["-w"]),
        ("exit-empty", "off", &["-g"]),
        ("mouse", "off", &["-g"]),
        ("allow-passthrough", "off", &["-p"]),
        ("history-limit", "50000", &["-g"]),
        ("remain-on-exit", "on", &["-p"]),
        ("extended-keys", "always", &["-g"]),
    ];
    for (opt, want, flags) in checks {
        let mut cmd = Command::new(tmux_bin);
        cmd.arg("-S").arg(sock).arg("show-options");
        for f in flags {
            cmd.arg(*f);
        }
        cmd.arg("-v").arg(opt);
        let out = common::run_bounded(&mut cmd, Duration::from_secs(10)).unwrap();
        let got = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(got, want, "show-options {opt}");
    }
    // No keys bound in root or prefix.
    for table in ["root", "prefix"] {
        let mut cmd = Command::new(tmux_bin);
        cmd.arg("-S")
            .arg(sock)
            .arg("list-keys")
            .arg("-T")
            .arg(table);
        let out = common::run_bounded(&mut cmd, Duration::from_secs(10)).unwrap();
        let got = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert!(
            got.is_empty(),
            "list-keys -T {table} must be empty, got: {got}"
        );
    }
    let _ = Command::new(tmux_bin)
        .arg("-S")
        .arg(sock)
        .arg("kill-server")
        .output();
    let _ = child.kill();
    let _ = child.wait();
}

// 2. exit-empty off keeps the -D server alive with no sessions.
// (Name mandated by the brief's `term_int_*` examples; the `D` is not
// snake_case, so the lint is allowed rather than renaming the pinned test.)
#[allow(non_snake_case)]
#[tokio::test(flavor = "multi_thread")]
async fn term_int_exit_empty_off_keeps_the_D_server_alive() {
    let Some(tmux_bin) = require_tmux() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("d.sock");
    let conf = common::conf_path();
    // Start the holder (tmux -D) with no session at all.
    let mut child = std::process::Command::new(&tmux_bin)
        .arg("-D")
        .arg("-S")
        .arg(&sock)
        .arg("-f")
        .arg(&conf)
        .spawn()
        .unwrap();
    let sock = sock.clone();
    let tmux = tmux_bin.clone();
    let alive = poll_until(Duration::from_secs(5), || {
        let sock = sock.clone();
        let tmux = tmux.clone();
        async move {
            let out = std::process::Command::new(&tmux)
                .arg("-S")
                .arg(&sock)
                .arg("list-sessions")
                .output();
            matches!(out, Ok(o) if o.status.success())
        }
    })
    .await;
    assert!(alive, "the -D server must answer with no session");
    // Create then kill the only session; still answers.
    for args in [
        vec![
            "new-session",
            "-d",
            "-s",
            "kb",
            "--",
            "/bin/sh",
            "-c",
            "sleep 30",
        ],
        vec!["kill-session", "-t", "kb"],
    ] {
        let out = std::process::Command::new(&tmux_bin)
            .arg("-S")
            .arg(&sock)
            .args(&args)
            .output()
            .unwrap();
        assert!(out.status.success(), "tmux {:?}", args);
    }
    let still = poll_until(Duration::from_secs(3), || {
        let sock = sock.clone();
        let tmux = tmux.clone();
        async move {
            let out = std::process::Command::new(&tmux)
                .arg("-S")
                .arg(&sock)
                .arg("display-message")
                .arg("-p")
                .arg("alive")
                .output();
            matches!(out, Ok(o) if o.status.success())
        }
    })
    .await;
    assert!(still, "the -D server must outlive zero sessions");
    let _ = child.kill();
    let _ = child.wait();
}

// 3. wait-for -S before wait-for returns immediately.
#[tokio::test(flavor = "multi_thread")]
async fn term_int_wait_for_signal_before_wait_returns() {
    let Some(tmux_bin) = require_tmux() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("w.sock");
    let conf = common::conf_path();
    let mut child = std::process::Command::new(&tmux_bin)
        .arg("-D")
        .arg("-S")
        .arg(&sock)
        .arg("-f")
        .arg(&conf)
        .spawn()
        .unwrap();
    let sock_owned = sock.clone();
    let tmux_owned = tmux_bin.clone();
    poll_until(Duration::from_secs(5), move || {
        let sock = sock_owned.clone();
        let tmux = tmux_owned.clone();
        async move {
            let out = std::process::Command::new(&tmux)
                .arg("-S")
                .arg(&sock)
                .arg("list-sessions")
                .output();
            matches!(out, Ok(o) if o.status.success())
        }
    })
    .await;
    // Signal BEFORE starting the waiter.
    let out = std::process::Command::new(&tmux_bin)
        .arg("-S")
        .arg(&sock)
        .arg("wait-for")
        .arg("-S")
        .arg("kbterm-go")
        .output()
        .unwrap();
    assert!(out.status.success(), "wait-for -S must succeed");
    // The next wait-for must return within 1 s.
    let start = std::time::Instant::now();
    let out = std::process::Command::new(&tmux_bin)
        .arg("-S")
        .arg(&sock)
        .arg("wait-for")
        .arg("kbterm-go")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "wait-for after -S must return immediately, took {:?}",
        start.elapsed()
    );
    let _ = child.kill();
    let _ = child.wait();
}

// 4. Start order: tap, then anchor, then release. The harness observes
//    exactly one control-mode (tap) and one non-control (anchor) client,
//    and its first instruction runs after the tap attach.
#[tokio::test(flavor = "multi_thread")]
async fn term_int_start_order_tap_then_anchor_then_release() {
    let Some(tmux_bin) = require_tmux() else {
        return;
    };
    let tr = TestRunner::new(tmux_bin.clone());
    boot_runner(&tr).await;
    let probe_out = tr.dir.path().join("probe.out");
    let run_dir = tr.runner.cfg.run_dir.to_string_lossy().into_owned();
    let (id, _pane_pid) = tr
        .launch(vec![
            fixture("probe.sh").to_string_lossy().into_owned(),
            probe_out.to_string_lossy().into_owned(),
            tmux_bin.clone(),
            run_dir,
        ])
        .await;
    // probe.sh records ts, $$, then one control-mode flag per client.
    // The file is created by `>` before content lands, so poll for a
    // complete record (ts + $$ + the tap and anchor flag lines), not
    // mere existence.
    let probe = poll_until(Duration::from_secs(10), || {
        let probe_out = probe_out.clone();
        async move {
            std::fs::read_to_string(&probe_out)
                .map(|s| s.lines().count() >= 4)
                .unwrap_or(false)
        }
    })
    .await;
    assert!(probe, "probe.sh must have run and written its file");
    let text = std::fs::read_to_string(&probe_out).unwrap();
    let mut lines = text.lines();
    let harness_ts: u64 = lines.next().unwrap().trim().parse().unwrap();
    let _harness_pid = lines.next().unwrap();
    let flags: Vec<&str> = lines.map(str::trim).collect();
    assert_eq!(
        flags.iter().filter(|f| **f == "1").count(),
        1,
        "exactly one control-mode client (the tap): {text}"
    );
    assert_eq!(
        flags.iter().filter(|f| **f == "0").count(),
        1,
        "exactly one non-control client (the anchor): {text}"
    );
    // The tap's attach timestamp (fake_tap.sh) precedes the harness.
    let tap_ts = std::fs::read_to_string(
        tr.dir
            .path()
            .join("run")
            .join("s")
            .join(format!("{id}.tap_ts")),
    )
    .unwrap()
    .trim()
    .parse::<u64>()
    .unwrap();
    assert!(
        harness_ts > tap_ts,
        "the harness must run after the tap attach ({harness_ts} > {tap_ts})"
    );
}

// 5. A missing tap times out and cleans up completely.
#[tokio::test(flavor = "multi_thread")]
async fn term_int_tap_timeout_cleans_up() {
    let Some(tmux_bin) = require_tmux() else {
        return;
    };
    let mut tr = TestRunner::with_tap(tmux_bin, true, false);
    boot_runner(&tr).await;
    tr.runner.cfg.tap_ready_secs = 1;
    let req = common::launch_req(
        vec!["/bin/cat".to_string()],
        tr.dir.path().to_string_lossy().into_owned(),
    );
    let resp = tr.launch_raw(&req).await;
    match resp {
        Response::Err { code, message } => {
            assert_eq!(
                code, "tap-timeout",
                "missing tap must be tap-timeout: {message}"
            );
            // The error names the session and the pane pid it held:
            // "tap did not attach for <id> (pane_pid <pid>)".
            let id = message
                .split("for ")
                .nth(1)
                .and_then(|s| s.split_whitespace().next())
                .unwrap_or("")
                .to_string();
            assert!(
                kb_term::runner::ids::valid_id(&id),
                "message must name the id: {message}"
            );
            let pane: u32 = message
                .split("(pane_pid ")
                .nth(1)
                .and_then(|s| s.trim_end_matches(')').parse().ok())
                .unwrap_or(0);
            // No leftover s/<id>* (socket or dir), no probe of any kind.
            let leftovers: Vec<_> = std::fs::read_dir(tr.runner.sessions_dir())
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with(&id))
                .collect();
            assert!(leftovers.is_empty(), "no s/{id}* may remain: {leftovers:?}");
            // The gate pane is gone and the runner reaped its children.
            if pane != 0 {
                assert!(pid_gone(pane), "gate pane {pane} must be gone");
            }
        }
        Response::Ok { data } => panic!("tap-timeout expected, got ok {data}"),
    }
}

// 6. pane_pid == the harness pid (the exec chain).
#[tokio::test(flavor = "multi_thread")]
async fn term_int_pane_pid_is_the_harness_pid() {
    let Some(tmux_bin) = require_tmux() else {
        return;
    };
    let tr = TestRunner::new(tmux_bin.clone());
    boot_runner(&tr).await;
    let out = tr.dir.path().join("pid.out");
    let run_dir = tr.runner.cfg.run_dir.to_string_lossy().into_owned();
    let (id, pane_pid) = tr
        .launch(vec![
            fixture("probe.sh").to_string_lossy().into_owned(),
            out.to_string_lossy().into_owned(),
            tmux_bin,
            run_dir,
        ])
        .await;
    // probe.sh writes ts, $$, then the control-mode flags. The file is
    // created by `>` before content lands, so poll for a complete
    // record (>= 3 lines means the $$ line is fully in), not existence.
    let present = poll_until(Duration::from_secs(10), || {
        let out = out.clone();
        async move {
            std::fs::read_to_string(&out)
                .map(|s| s.lines().count() >= 3)
                .unwrap_or(false)
        }
    })
    .await;
    assert!(present, "probe.sh must have written $$");
    let text = std::fs::read_to_string(&out).unwrap();
    let harness_pid: u32 = text.lines().nth(1).unwrap().trim().parse().unwrap();
    assert_eq!(harness_pid, pane_pid, "pane_pid must be the harness pid");
    let _ = id;
}

// 7. List is state-free.
#[tokio::test(flavor = "multi_thread")]
async fn term_int_list_is_state_free() {
    let Some(tmux_bin) = require_tmux() else {
        return;
    };
    let dir = tempfile::TempDir::new_in("/tmp").unwrap();
    let run_dir = dir.path().join("run");
    std::fs::create_dir_all(run_dir.join("s")).unwrap();
    std::fs::create_dir_all(run_dir.join("runner")).unwrap();
    let cwd = dir.path().to_string_lossy().into_owned();
    // The tap config must be set on the config BEFORE Runner::direct:
    // direct() clones tap_bin into the supervisor at construction, so a
    // later seed.cfg.tap_bin assignment is invisible to start_tap (a
    // no-op) and launch dies tap-timeout. This was the round-5
    // term_int_list_is_state_free failure.
    let mut seed_cfg = kb_term::runner::RunnerConfig::test_config(
        run_dir.clone(),
        tmux_bin.clone(),
        common::conf_path(),
    );
    seed_cfg.tap_bin = Some(fixture("fake_tap.sh"));
    seed_cfg.require_tap = true;
    let seed = kb_term::runner::Runner::direct(seed_cfg);
    ensure_layout(&seed.cfg).await.unwrap();
    check_tmux_version(&seed.tmux, "3.7c").await.unwrap();
    adopt_anchors(&seed).await.unwrap();
    let req = common::launch_req(vec!["/bin/cat".to_string()], cwd.clone());
    let (id, _pane) = match rpc::dispatch(&seed, &req).await {
        Response::Ok { data } => (
            data["id"].as_str().unwrap().to_string(),
            data["pane_pid"].as_u64().unwrap(),
        ),
        Response::Err { code, message } => panic!("launch failed ({code}): {message}"),
    };
    // Seed state.
    let before = discovery::list_sessions(&seed)
        .await
        .into_iter()
        .find(|e| e.id == id)
        .unwrap();
    // Drop A; runner B lists the same sessions (from disk + tmux only).
    drop(seed);
    let fresh = kb_term::runner::Runner::direct(kb_term::runner::RunnerConfig::test_config(
        run_dir.clone(),
        tmux_bin,
        common::conf_path(),
    ));
    ensure_layout(&fresh.cfg).await.unwrap();
    check_tmux_version(&fresh.tmux, "3.7c").await.unwrap();
    adopt_anchors(&fresh).await.unwrap();
    let after = discovery::list_sessions(&fresh)
        .await
        .into_iter()
        .find(|e| e.id == id)
        .expect("runner B must list the session runner A launched");
    assert_eq!(after.harness, before.harness);
    assert_eq!(after.cwd, before.cwd);
    assert_eq!(after.started_unix, before.started_unix);
    assert_eq!(after.cols, before.cols);
    assert_eq!(after.rows, before.rows);
    assert_eq!(after.conf_version, 2, "the holder loaded conf v2");
    // Cleanup guard: kill the holder.
    let sock = run_dir.join("s").join(format!("{id}.sock"));
    let _ = std::process::Command::new(&fresh.cfg.tmux_bin)
        .arg("-S")
        .arg(&sock)
        .arg("kill-server")
        .output();
}

// 8. Runner restart keeps the session and re-anchors it.
#[tokio::test(flavor = "multi_thread")]
async fn term_int_runner_restart_keeps_the_session_and_recreates_the_anchor() {
    let Some(tmux_bin) = require_tmux() else {
        return;
    };
    let dir = tempfile::TempDir::new_in("/tmp").unwrap();
    let run_dir = dir.path().join("run");
    std::fs::create_dir_all(run_dir.join("s")).unwrap();
    std::fs::create_dir_all(run_dir.join("runner")).unwrap();
    let cwd = dir.path().to_string_lossy().into_owned();
    let mut cfg = kb_term::runner::RunnerConfig::test_config(
        run_dir.clone(),
        tmux_bin.clone(),
        common::conf_path(),
    );
    cfg.tap_bin = Some(fixture("fake_tap.sh"));
    cfg.require_tap = true;
    let a = kb_term::runner::Runner::direct(cfg);
    ensure_layout(&a.cfg).await.unwrap();
    check_tmux_version(&a.tmux, "3.7c").await.unwrap();
    adopt_anchors(&a).await.unwrap();
    let req = common::launch_req(vec!["/bin/cat".to_string()], cwd);
    let (id, pane_pid) = match rpc::dispatch(&a, &req).await {
        Response::Ok { data } => (
            data["id"].as_str().unwrap().to_string(),
            data["pane_pid"].as_u64().unwrap() as u32,
        ),
        Response::Err { code, message } => panic!("launch failed ({code}): {message}"),
    };
    let old_anchor = discovery::anchor_pid(&a, &id)
        .await
        .expect("anchor pid set at launch");
    // Drop the runner (closes anchor masters; the session survives).
    drop(a);
    // The dropped runner must not leave a zombie anchor process behind.
    let reaped = poll_until(
        Duration::from_secs(10),
        || async move { pid_gone(old_anchor) },
    )
    .await;
    assert!(reaped, "the old anchor process must be reaped after drop");
    // The old anchor client must disappear from list-clients.
    let tmux = tmux_bin.clone();
    let sock = run_dir.join("s").join(format!("{id}.sock"));
    let gone = poll_until(Duration::from_secs(10), || {
        let tmux = tmux.clone();
        let sock = sock.clone();
        async move {
            let out = std::process::Command::new(&tmux)
                .arg("-S")
                .arg(&sock)
                .arg("list-clients")
                .arg("-F")
                .arg("#{client_pid}")
                .output();
            match out {
                Ok(o) if o.status.success() => !String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .any(|l| l.trim() == old_anchor.to_string()),
                _ => false,
            }
        }
    })
    .await;
    assert!(gone, "the old anchor must be gone after the runner died");
    assert!(
        !pid_gone(pane_pid),
        "the harness must survive the runner restart"
    );
    // A new runner adopts and re-anchors.
    let b = kb_term::runner::Runner::direct(kb_term::runner::RunnerConfig::test_config(
        run_dir.clone(),
        tmux_bin.clone(),
        common::conf_path(),
    ));
    ensure_layout(&b.cfg).await.unwrap();
    check_tmux_version(&b.tmux, "3.7c").await.unwrap();
    adopt_anchors(&b).await.unwrap();
    let new_anchor = discovery::anchor_pid(&b, &id)
        .await
        .expect("new anchor pid");
    assert_ne!(new_anchor, old_anchor, "a fresh anchor must be spawned");
    let listed = poll_until(Duration::from_secs(5), || {
        let tmux = tmux.clone();
        let sock = sock.clone();
        async move {
            let out = std::process::Command::new(&tmux)
                .arg("-S")
                .arg(&sock)
                .arg("list-clients")
                .arg("-F")
                .arg("#{client_pid}")
                .output();
            match out {
                Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .any(|l| l.trim() == new_anchor.to_string()),
                _ => false,
            }
        }
    })
    .await;
    assert!(listed, "the new anchor must be in list-clients");
    let _ = std::process::Command::new(&tmux_bin)
        .arg("-S")
        .arg(&sock)
        .arg("kill-server")
        .output();
}

// 9. Graceful kill via exit keys.
#[tokio::test(flavor = "multi_thread")]
async fn term_int_graceful_kill_via_exit_keys() {
    let Some(tmux_bin) = require_tmux() else {
        return;
    };
    let tr = TestRunner::new(tmux_bin);
    boot_runner(&tr).await;
    let cwd = tr.dir.path().to_string_lossy().into_owned();
    let req = Request::Launch {
        harness: kb_term::wire::Harness::Omp,
        argv: Some(vec!["/bin/cat".to_string()]),
        resume_sid: None,
        cwd,
        cols: Some(120),
        rows: Some(40),
        env: None,
        exit_keys: Some(vec!["C-d".to_string()]),
    };
    let (id, _) = match tr.launch_raw(&req).await {
        Response::Ok { data } => (
            data["id"].as_str().unwrap().to_string(),
            data["pane_pid"].as_u64().unwrap(),
        ),
        Response::Err { code, message } => panic!("launch failed ({code}): {message}"),
    };
    let resp = rpc::dispatch(
        &tr.runner,
        &Request::Kill {
            id: id.clone(),
            grace_secs: Some(5),
        },
    )
    .await;
    match resp {
        Response::Ok { data } => {
            assert_eq!(data["exit_code"], 0, "cat must exit 0 on C-d");
            assert_eq!(data["forced"], false, "graceful kill must not force");
        }
        Response::Err { code, message } => panic!("kill failed ({code}): {message}"),
    }
    assert!(!tr.sock(&id).exists(), "the holder socket must be gone");
    assert!(
        !tr.runner.sessions_dir().join(&id).exists(),
        "the session dir must be gone"
    );
}

// 10. A stray ignoring HUP is reaped (SIGKILL path).
#[tokio::test(flavor = "multi_thread")]
async fn term_int_kill_reaps_a_stray_that_ignores_hup() {
    let Some(tmux_bin) = require_tmux() else {
        return;
    };
    let tr = TestRunner::new(tmux_bin.clone());
    boot_runner(&tr).await;
    let (id, pane_pid) = tr
        .launch(vec![fixture("stubborn.sh").to_string_lossy().into_owned()])
        .await;
    assert!(!pid_gone(pane_pid), "the stubborn harness must be alive");
    let resp = rpc::dispatch(
        &tr.runner,
        &Request::Kill {
            id: id.clone(),
            grace_secs: Some(1),
        },
    )
    .await;
    match resp {
        Response::Ok { data } => {
            assert_eq!(data["forced"], true, "the SIGKILL path must have run");
        }
        Response::Err { code, message } => panic!("kill failed ({code}): {message}"),
    }
    let gone = poll_until(Duration::from_secs(5), || async move { pid_gone(pane_pid) }).await;
    assert!(gone, "the stubborn harness pid must be gone");
    // The holder must be gone too (no socket, no server answering).
    assert!(!tr.sock(&id).exists(), "the holder socket must be gone");
}

// 11. The grid is fixed until resized; the anchor pty tracks resizes.
#[tokio::test(flavor = "multi_thread")]
async fn term_int_grid_is_fixed_until_resized() {
    let Some(tmux_bin) = require_tmux() else {
        return;
    };
    let tr = TestRunner::new(tmux_bin.clone());
    boot_runner(&tr).await;
    let (id, _) = tr.launch(vec!["/bin/cat".to_string()]).await;
    // Attach a second raw client at 50x20 and type; the window stays 120x40.
    let sock = tr.sock(&id);
    let (cols, rows) = window_size(&tr, &sock);
    assert_eq!((cols, rows), (120, 40), "initial grid");
    let (mut master, _slave) = pty_pair(50, 20);
    let mut child = std::process::Command::new(&tmux_bin)
        .arg("-S")
        .arg(&sock)
        .arg("attach-session")
        .arg("-t")
        .arg("kb")
        .env("TERM", "xterm-256color")
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .stdin(master.try_clone().unwrap())
        .stdout(master.try_clone().unwrap())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn raw client");
    use std::io::Write;
    let _ = master.write_all(b"x");
    let _ = std::process::Command::new(&tmux_bin)
        .arg("-S")
        .arg(&sock)
        .arg("send-keys")
        .arg("-t")
        .arg("kb")
        .arg("x")
        .output();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (cols, rows) = window_size(&tr, &sock);
    assert_eq!((cols, rows), (120, 40), "typing must never move the window");
    let _ = child.kill();
    let _ = child.wait();
    // Resize to 100x30; the anchor pty pixel fields must be 900x540.
    let resp = rpc::dispatch(
        &tr.runner,
        &Request::Resize {
            id: id.clone(),
            cols: 100,
            rows: 30,
        },
    )
    .await;
    match resp {
        Response::Ok { .. } => {}
        Response::Err { code, message } => panic!("resize failed ({code}): {message}"),
    }
    let (cols, rows) = window_size(&tr, &sock);
    assert_eq!((cols, rows), (100, 30), "resize must move the window");
    let winsize = tr.runner.anchor_winsize(&id).await.expect("anchor present");
    assert_eq!(winsize, (100, 30, 900, 540), "anchor pty pixel fields");
}

fn window_size(tr: &TestRunner, sock: &std::path::Path) -> (u16, u16) {
    let (ok, out) = tr.tmux(
        sock,
        &[
            "display-message",
            "-p",
            "-t",
            "kb",
            "#{window_width} #{window_height}",
        ],
    );
    assert!(ok, "display-message window size");
    let mut it = out.split_whitespace();
    (
        it.next().unwrap().parse().unwrap(),
        it.next().unwrap().parse().unwrap(),
    )
}

/// A pty pair (master file, slave path-less fd pair) sized cols x rows.
fn pty_pair(cols: u16, rows: u16) -> (std::fs::File, std::fs::File) {
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: cols * 9,
        ws_ypixel: rows * 18,
    };
    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            &ws,
        )
    };
    assert_eq!(rc, 0, "openpty");
    use std::os::unix::io::FromRawFd;
    let master = unsafe { std::fs::File::from_raw_fd(master) };
    let slave = unsafe { std::fs::File::from_raw_fd(slave) };
    (master, slave)
}

// 12. history and read return screen text.
#[tokio::test(flavor = "multi_thread")]
async fn term_int_history_and_read_return_screen_text() {
    let Some(tmux_bin) = require_tmux() else {
        return;
    };
    let tr = TestRunner::new(tmux_bin.clone());
    boot_runner(&tr).await;
    // cat echoes its input; type three known lines into the pane.
    let (id, _) = tr.launch(vec!["/bin/cat".to_string()]).await;
    let sock = tr.sock(&id);
    for line in ["alpha-line-one", "beta-line-two", "gamma-line-three"] {
        let out = std::process::Command::new(&tmux_bin)
            .arg("-S")
            .arg(&sock)
            .arg("send-keys")
            .arg("-t")
            .arg("kb")
            .arg("-l")
            .arg(line)
            .output()
            .unwrap();
        assert!(out.status.success());
        let out = std::process::Command::new(&tmux_bin)
            .arg("-S")
            .arg(&sock)
            .arg("send-keys")
            .arg("-t")
            .arg("kb")
            .arg("Enter")
            .output()
            .unwrap();
        assert!(out.status.success());
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    for req in [
        Request::Read { id: id.clone() },
        Request::History { id: id.clone() },
    ] {
        match rpc::dispatch(&tr.runner, &req).await {
            Response::Ok { data } => {
                assert_eq!(data["truncated"], false, "{req:?} truncated");
                let text = data["text"].as_str().unwrap();
                for line in ["alpha-line-one", "beta-line-two", "gamma-line-three"] {
                    assert!(
                        text.contains(line),
                        "{req:?} must contain {line}, got: {text}"
                    );
                }
            }
            Response::Err { code, message } => panic!("{req:?} failed ({code}): {message}"),
        }
    }
}

// 13. Invalid ids never reach tmux.
#[tokio::test(flavor = "multi_thread")]
async fn term_int_invalid_ids_never_reach_tmux() {
    let Some(tmux_bin) = require_tmux() else {
        return;
    };
    let tr = TestRunner::new(tmux_bin);
    boot_runner(&tr).await;
    for req in [
        Request::Kill {
            id: "../x".to_string(),
            grace_secs: None,
        },
        Request::Read {
            id: "../x".to_string(),
        },
        Request::Resize {
            id: "../x".to_string(),
            cols: 100,
            rows: 30,
        },
    ] {
        match rpc::dispatch(&tr.runner, &req).await {
            Response::Err { code, .. } => assert_eq!(code, "invalid-id", "{req:?}"),
            Response::Ok { data } => panic!("{req:?} must be invalid-id, got ok {data}"),
        }
    }
    // No session was created (no holder for any id).
    let entries: Vec<_> = std::fs::read_dir(tr.runner.sessions_dir())
        .unwrap()
        .flatten()
        .collect();
    assert!(entries.is_empty(), "no holder may be created for bad ids");
}

// 14. The runner refuses a wrong pinned-tmux version.
#[tokio::test(flavor = "multi_thread")]
async fn term_int_runner_rejects_wrong_tmux_version() {
    let Some(tmux_bin) = require_tmux() else {
        return;
    };
    let dir = tempfile::TempDir::new_in("/tmp").unwrap();
    let run_dir = dir.path().join("run");
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_kb-termd"));
    cmd.arg("--run-dir")
        .arg(&run_dir)
        .arg("--tmux-bin")
        .arg(&tmux_bin)
        .arg("--conf")
        .arg(common::conf_path())
        .arg("--supervisor")
        .arg("direct")
        .arg("--require-tap")
        .arg("--expected-tmux")
        .arg("9.9");
    let out = common::run_bounded(&mut cmd, Duration::from_secs(15)).unwrap();
    assert!(!out.status.success(), "a wrong version must exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("tmux-version"),
        "the failure must carry the tmux-version code: {stderr}"
    );
}

// 15. Kill after a runner restart still reaps the pane and the server
//     (the second runner's holders map is empty; stop() must signal from
//     StopCtx without a held Child).
#[tokio::test(flavor = "multi_thread")]
async fn term_int_kill_after_restart_reaps_holder() {
    let Some(tmux_bin) = require_tmux() else {
        return;
    };
    let dir = tempfile::TempDir::new_in("/tmp").unwrap();
    let run_dir = dir.path().join("run");
    std::fs::create_dir_all(run_dir.join("s")).unwrap();
    std::fs::create_dir_all(run_dir.join("runner")).unwrap();
    let cwd = dir.path().to_string_lossy().into_owned();
    let mut cfg = kb_term::runner::RunnerConfig::test_config(
        run_dir.clone(),
        tmux_bin.clone(),
        common::conf_path(),
    );
    cfg.tap_bin = Some(fixture("fake_tap.sh"));
    cfg.require_tap = true;
    let a = kb_term::runner::Runner::direct(cfg);
    ensure_layout(&a.cfg).await.unwrap();
    check_tmux_version(&a.tmux, "3.7c").await.unwrap();
    adopt_anchors(&a).await.unwrap();
    let req = common::launch_req(vec!["/bin/cat".to_string()], cwd);
    let (id, pane_pid) = match rpc::dispatch(&a, &req).await {
        Response::Ok { data } => (
            data["id"].as_str().unwrap().to_string(),
            data["pane_pid"].as_u64().unwrap() as u32,
        ),
        Response::Err { code, message } => panic!("launch failed ({code}): {message}"),
    };
    let (_, server_pid) = discovery::pane_and_server_pids(&a, &id).await;
    let server_pid = server_pid.expect("server pid readable before restart");
    assert!(!pid_gone(pane_pid), "the harness must be alive");
    // Drop runner A; the holder survives as a setsid process nobody owns.
    drop(a);
    // Fresh runner B: its holders map is empty.
    let b = kb_term::runner::Runner::direct(kb_term::runner::RunnerConfig::test_config(
        run_dir.clone(),
        tmux_bin,
        common::conf_path(),
    ));
    ensure_layout(&b.cfg).await.unwrap();
    check_tmux_version(&b.tmux, "3.7c").await.unwrap();
    adopt_anchors(&b).await.unwrap();
    let resp = rpc::dispatch(
        &b,
        &Request::Kill {
            id: id.clone(),
            grace_secs: Some(2),
        },
    )
    .await;
    match resp {
        Response::Ok { .. } => {}
        Response::Err { code, message } => panic!("kill-after-restart failed ({code}): {message}"),
    }
    let gone = poll_until(Duration::from_secs(10), || async move {
        pid_gone(pane_pid) && pid_gone(server_pid)
    })
    .await;
    assert!(
        gone,
        "pane {pane_pid} and server {server_pid} must be reaped"
    );
    assert!(
        !run_dir.join("s").join(format!("{id}.sock")).exists(),
        "the holder socket must be removed"
    );
}
