//! Launch: create a session in the fixed start order.
//!
//! 1. generate the id + take the per-id lock
//! 2. start the holder; poll until the server answers
//! 3. create the session (the pane command is the gate: it waits on the
//!    `kbterm-go` channel so the harness emits no byte before the tap hears)
//! 4. `set-option -g` the `@kb_*` user options (never env/argv/secrets)
//! 5. spawn the anchor (first client: tmux answers pane OSC 10/11 from
//!    the first client's recorded colours, so the anchor attaches
//!    before the tap) and wait for its pid in `list-clients`
//! 6. start the tap and wait for a control-mode client
//! 7. `wait-for -S kbterm-go` releases the harness; read `pane_pid`
//!
//! After step 2, any failure runs a best-effort teardown and returns the
//! error: no stray processes, no leftover `s/<id>*`.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::runner::anchor::Anchor;
use crate::runner::ids::{generate_id, valid_harness, valid_id};
use crate::runner::supervisor::{StopCtx, Supervisor};
use crate::runner::tmux::Tmux;
use crate::runner::{Runner, RunnerConfig, RunnerError};
use crate::wire::{Harness, Request};

/// The documented gate: `wait-for` holds the harness until the runner
/// sends `wait-for -S kbterm-go` after the tap and the anchor are in.
/// The ONLY shell string the runner ever builds.
pub const GATE_SCRIPT: &str =
    "T=$1; S=$2; shift 2; \"$T\" -S \"$S\" wait-for kbterm-go; exec \"$@\"";

/// The default exit keys (the omp sequence; per-harness tables are T10).
pub const DEFAULT_EXIT_KEYS: [&str; 3] = ["C-c", "C-c", "C-d"];

/// Build the exact `new-session` argv for a launch. Pure (no IO) so the
/// gate shape is pinned by a unit test.
///
/// The eight parameters are the documented argv pieces in order; bundling
/// them into a struct would obscure the exact-vector test.
#[allow(clippy::too_many_arguments)]
pub fn gate_argv(
    tmux_bin: &str,
    sock: &Path,
    id: &str,
    cwd: &Path,
    cols: u16,
    rows: u16,
    env: &BTreeMap<String, String>,
    argv: &[String],
) -> Vec<String> {
    let mut out = vec![
        "new-session".to_string(),
        "-d".to_string(),
        "-s".to_string(),
        "kb".to_string(),
        "-x".to_string(),
        cols.to_string(),
        "-y".to_string(),
        rows.to_string(),
        "-c".to_string(),
        cwd.to_string_lossy().into_owned(),
        "-e".to_string(),
        format!("KB_TERM_ID={id}"),
    ];
    for (k, v) in env {
        out.push("-e".to_string());
        out.push(format!("{k}={v}"));
    }
    out.push("--".to_string());
    out.push("/bin/sh".to_string());
    out.push("-c".to_string());
    out.push(GATE_SCRIPT.to_string());
    out.push("kbterm-gate".to_string());
    out.push(tmux_bin.to_string());
    out.push(sock.to_string_lossy().into_owned());
    out.extend(argv.iter().cloned());
    out
}

/// A validated launch request (everything after the pre-tmux checks).
#[derive(Debug, Clone)]
pub struct ValidatedLaunch {
    /// Harness name (wire enum; for the `@kb_harness` option).
    pub harness: String,
    /// The argv to run (first item an absolute path).
    pub argv: Vec<String>,
    /// Resume session id, if any.
    pub resume_sid: Option<String>,
    /// Working directory.
    pub cwd: std::path::PathBuf,
    /// Grid columns.
    pub cols: u16,
    /// Grid rows.
    pub rows: u16,
    /// Extra environment for the harness pane.
    pub env: BTreeMap<String, String>,
    /// Exit keys used by the kill RPC.
    pub exit_keys: Vec<String>,
}

/// The outcome of a successful launch.
#[derive(Debug, Clone)]
pub struct LaunchOutcome {
    /// The session id.
    pub id: String,
    /// The harness pid (`#{pane_pid}`).
    pub pane_pid: u32,
    /// Grid columns.
    pub cols: u16,
    /// Grid rows.
    pub rows: u16,
}

/// Validate a launch request. Every failure is `bad-request` and happens
/// before any tmux or process call (the unit test's fake proves it).
pub fn validate_launch(req: &Request, cfg: &RunnerConfig) -> Result<ValidatedLaunch, RunnerError> {
    let Request::Launch {
        harness,
        argv,
        resume_sid,
        cwd,
        cols,
        rows,
        env,
        exit_keys,
    } = req
    else {
        return Err(RunnerError::BadRequest("not a launch request".to_string()));
    };
    let Some(argv) = argv else {
        return Err(RunnerError::BadRequest("argv is required".to_string()));
    };
    if argv.is_empty() {
        return Err(RunnerError::BadRequest(
            "argv must not be empty".to_string(),
        ));
    }
    if !argv[0].starts_with('/') {
        return Err(RunnerError::BadRequest(
            "argv[0] must be an absolute path".to_string(),
        ));
    }
    let total: usize = argv.iter().map(String::len).sum();
    if total > 32 * 1024 {
        return Err(RunnerError::BadRequest("argv exceeds 32 KiB".to_string()));
    }
    for arg in argv {
        if arg.contains('\0') {
            return Err(RunnerError::BadRequest(
                "argv contains a NUL byte".to_string(),
            ));
        }
    }
    let cwd_path = std::path::Path::new(cwd);
    if !cwd_path.is_absolute() {
        return Err(RunnerError::BadRequest("cwd must be absolute".to_string()));
    }
    if !cwd_path.is_dir() {
        return Err(RunnerError::BadRequest(
            "cwd is not an existing directory".to_string(),
        ));
    }
    let cols = cols.unwrap_or(cfg.default_cols);
    let rows = rows.unwrap_or(cfg.default_rows);
    if !(20..=500).contains(&cols) {
        return Err(RunnerError::BadRequest("cols must be 20..=500".to_string()));
    }
    if !(5..=200).contains(&rows) {
        return Err(RunnerError::BadRequest("rows must be 5..=200".to_string()));
    }
    let env = env.clone().unwrap_or_default();
    if env.len() > 64 {
        return Err(RunnerError::BadRequest(
            "at most 64 env entries".to_string(),
        ));
    }
    for (k, v) in &env {
        if !valid_env_key(k) {
            return Err(RunnerError::BadRequest(format!("bad env key `{k}`")));
        }
        if v.contains('\0') {
            return Err(RunnerError::BadRequest(
                "env value contains a NUL byte".to_string(),
            ));
        }
        if v.len() > 8 * 1024 {
            return Err(RunnerError::BadRequest(
                "env value exceeds 8 KiB".to_string(),
            ));
        }
    }
    let exit_keys = exit_keys.clone().unwrap_or_else(|| {
        DEFAULT_EXIT_KEYS
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    });
    if exit_keys.len() > 8 {
        return Err(RunnerError::BadRequest("at most 8 exit keys".to_string()));
    }
    for key in &exit_keys {
        if !valid_exit_key(key) {
            return Err(RunnerError::BadRequest(format!("bad exit key `{key}`")));
        }
    }
    Ok(ValidatedLaunch {
        harness: harness.as_str().to_string(),
        argv: argv.clone(),
        resume_sid: resume_sid.clone(),
        cwd: cwd_path.to_path_buf(),
        cols,
        rows,
        env,
        exit_keys,
    })
}

fn valid_env_key(k: &str) -> bool {
    let mut chars = k.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn valid_exit_key(k: &str) -> bool {
    (1..=16).contains(&k.len()) && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Launch one session (the full start order). Generic over the tmux driver
/// and the supervisor; the production [`Runner`] calls this with the real
/// driver, and the unit tests with the fakes.
pub async fn launch_session<S: Supervisor>(
    runner: &Runner<S>,
    spec: ValidatedLaunch,
) -> Result<LaunchOutcome, RunnerError> {
    let sessions_dir = runner.sessions_dir();
    let id = generate_id(&sessions_dir)?;
    // The per-id lock: launch and kill of one id never interleave.
    let _lock = runner.id_lock(&id).await;
    let lock = _lock.lock().await;
    match launch_locked(runner, &id, &spec).await {
        Ok(outcome) => {
            drop(lock);
            Ok(outcome)
        }
        Err(e) => {
            drop(lock);
            // Best-effort teardown: stop the units, wait the holder, drop
            // the anchor, remove `s/<id>*`.
            runner.take_anchor(&id).await;
            let _ = runner.supervisor.stop(&id, StopCtx::default()).await;
            remove_session_files(runner, &id).await;
            Err(e)
        }
    }
}

async fn launch_locked<S: Supervisor>(
    runner: &Runner<S>,
    id: &str,
    spec: &ValidatedLaunch,
) -> Result<LaunchOutcome, RunnerError> {
    let sock = runner.holder_socket(id);
    // Step 2: start the holder; poll until the server answers (5 s).
    runner.supervisor.start_holder(id).await?;
    create_session_dir(runner, id).await?;
    let ready = poll_until(
        Duration::from_secs(5),
        Duration::from_millis(50),
        || async {
            let out = runner
                .tmux
                .run(&sock, &["show-options", "-gv", "@kb_conf_version"], None)
                .await;
            matches!(out, Ok(o) if o.ok)
        },
    )
    .await;
    if !ready {
        return Err(RunnerError::TmuxFailed(format!(
            "holder for {id} did not answer"
        )));
    }
    // Step 3: create the session (the pane command is the gate).
    let args = gate_argv(
        &runner.cfg.tmux_bin,
        &sock,
        id,
        &spec.cwd,
        spec.cols,
        spec.rows,
        &spec.env,
        &spec.argv,
    );
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = runner.tmux.run(&sock, &arg_refs, None).await?;
    if !out.ok {
        return Err(RunnerError::TmuxFailed(format!(
            "new-session failed: {}",
            out.stderr.trim()
        )));
    }
    // Step 4: user options (never env, argv or secret-bearing text).
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    set_user_options(runner, &sock, id, spec, started).await?;
    // The pane already exists (the gate holds the harness); name its pid
    // in the tap-timeout error so diagnostics can confirm the teardown.
    let pane_pid_early: u32 = read_pane_pid(runner, &sock).await.unwrap_or_default();
    // Step 5: anchor FIRST (it is the first client in tmux's list, and
    // tmux answers a pane's OSC 10/11 from the first attached client's
    // recorded colours — a later control-mode tap client carries a
    // zeroed bg that would otherwise shadow the anchor's responder
    // answers with black). The anchor must be visible (and have
    // answered tmux's startup queries) in list-clients before release.
    spawn_anchor_and_wait(runner, id, spec.cols, spec.rows).await?;
    // Step 6: tap stage. The tap still attaches before the harness is
    // released (step 7), so it observes output from the first byte
    // exactly as before; only the client order changes.
    if runner.cfg.require_tap {
        runner.supervisor.start_tap(id).await?;
        let ready = wait_for_control_client(
            runner,
            &sock,
            Duration::from_secs(runner.cfg.tap_ready_secs),
        )
        .await;
        if !ready {
            return Err(RunnerError::TapTimeout(format!(
                "tap did not attach for {id} (pane_pid {pane_pid_early})"
            )));
        }
    }
    // Step 7: release the harness.
    let out = runner
        .tmux
        .run(&sock, &["wait-for", "-S", "kbterm-go"], None)
        .await?;
    if !out.ok {
        return Err(RunnerError::TmuxFailed(format!(
            "wait-for -S failed: {}",
            out.stderr.trim()
        )));
    }
    // Step 8: pane_pid (stable since creation; read again as specified).
    let pane_pid = read_pane_pid(runner, &sock).await?;
    Ok(LaunchOutcome {
        id: id.to_string(),
        pane_pid,
        cols: spec.cols,
        rows: spec.rows,
    })
}

/// Read `#{pane_pid}` once.
async fn read_pane_pid<S: Supervisor>(runner: &Runner<S>, sock: &Path) -> Result<u32, RunnerError> {
    let out = runner
        .tmux
        .run(
            sock,
            &["display-message", "-p", "-t", "kb", "#{pane_pid}"],
            None,
        )
        .await?;
    if !out.ok {
        return Err(RunnerError::TmuxFailed(format!(
            "display-message pane_pid failed: {}",
            out.stderr.trim()
        )));
    }
    out.stdout
        .trim()
        .parse()
        .map_err(|_| RunnerError::TmuxFailed(format!("bad pane_pid `{}`", out.stdout.trim())))
}

async fn create_session_dir<S: Supervisor>(
    runner: &Runner<S>,
    id: &str,
) -> Result<(), RunnerError> {
    let dir = runner.sessions_dir().join(id);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| RunnerError::Internal(format!("create {}: {e}", dir.display())))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| RunnerError::Internal(format!("chmod {}: {e}", dir.display())))?;
    Ok(())
}

async fn set_user_options<S: Supervisor>(
    runner: &Runner<S>,
    sock: &Path,
    id: &str,
    spec: &ValidatedLaunch,
    started: u64,
) -> Result<(), RunnerError> {
    let mut opts: Vec<(&str, String)> = vec![
        ("@kb_harness", spec.harness.clone()),
        ("@kb_cwd", spec.cwd.to_string_lossy().into_owned()),
        ("@kb_started", started.to_string()),
        (
            "@kb_exit_keys",
            serde_json::to_string(&spec.exit_keys)
                .map_err(|e| RunnerError::Internal(e.to_string()))?,
        ),
    ];
    if let Some(sid) = &spec.resume_sid {
        opts.push(("@kb_resume_sid", sid.clone()));
    }
    // (step 6 writes `@kb_anchor_pid`; until then it reads as absent and
    // adoption treats that as "recreate".)
    // tmux 3.7c's `set-option` has no `--` terminator: `key -- value`
    // is parsed as option=`key`, value=`--`, extra arg → "too many
    // arguments". Set the value directly after the option name.
    for (key, value) in opts {
        let out = runner
            .tmux
            .run(sock, &["set-option", "-g", key, &value], None)
            .await?;
        if !out.ok {
            return Err(RunnerError::TmuxFailed(format!(
                "set-option {key} failed: {}",
                out.stderr.trim()
            )));
        }
    }
    let _ = id;
    Ok(())
}

/// Poll `f` until it returns true or the deadline passes.
pub async fn poll_until<F, Fut>(deadline: Duration, interval: Duration, mut f: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = tokio::time::Instant::now();
    loop {
        if f().await {
            return true;
        }
        if start.elapsed() >= deadline {
            return false;
        }
        tokio::time::sleep(interval).await;
    }
}

/// Does a control-mode client show up in `list-clients`?
pub async fn control_client_present<S: Supervisor>(runner: &Runner<S>, sock: &Path) -> bool {
    // `# {client_control_mode}` is the believed-valid 3.7c format; when
    // tmux does not know a format it expands lines to empty, so fall back
    // to `#{client_flags}` containing `control-mode`.
    let out = runner
        .tmux
        .run(
            sock,
            &["list-clients", "-F", "#{client_control_mode}"],
            None,
        )
        .await;
    if let Ok(out) = out {
        if out.ok {
            let mut saw_empty = false;
            for line in out.stdout.lines() {
                let line = line.trim();
                if line == "1" {
                    return true;
                } else if line.is_empty() {
                    saw_empty = true;
                }
            }
            if !saw_empty {
                return false;
            }
        }
    }
    let out = runner
        .tmux
        .run(sock, &["list-clients", "-F", "#{client_flags}"], None)
        .await;
    matches!(out, Ok(o) if o.ok && o.stdout.lines().any(|l| l.contains("control-mode")))
}

async fn wait_for_control_client<S: Supervisor>(
    runner: &Runner<S>,
    sock: &Path,
    timeout: Duration,
) -> bool {
    poll_until(timeout, Duration::from_millis(50), || async {
        control_client_present(runner, sock).await
    })
    .await
}

/// Spawn the anchor, store it, write `@kb_anchor_pid`, and wait until its
/// pid shows up in `list-clients` (5 s). Shared with runner-start adopt.
pub async fn spawn_anchor_and_wait<S: Supervisor>(
    runner: &Runner<S>,
    id: &str,
    cols: u16,
    rows: u16,
) -> Result<(), RunnerError> {
    let sock = runner.holder_socket(id);
    let (cell_w, cell_h) = runner.cfg.cell_px_dims();
    let anchor = Anchor::spawn(
        &runner.cfg.tmux_bin,
        &sock,
        cols,
        rows,
        cell_w,
        cell_h,
        &runner.cfg.responder_cfg(),
    )?;
    let pid = anchor.pid();
    runner.store_anchor(id, anchor).await;
    let out = runner
        .tmux
        .run(
            &sock,
            &["set-option", "-g", "@kb_anchor_pid", &pid.to_string()],
            None,
        )
        .await?;
    if !out.ok {
        return Err(RunnerError::TmuxFailed(format!(
            "set-option @kb_anchor_pid failed: {}",
            out.stderr.trim()
        )));
    }
    let appeared = poll_until(
        Duration::from_secs(5),
        Duration::from_millis(50),
        || async {
            let out = runner
                .tmux
                .run(&sock, &["list-clients", "-F", "#{client_pid}"], None)
                .await;
            matches!(out, Ok(o) if o.ok && o.stdout.lines().any(|l| l.trim() == pid.to_string()))
        },
    )
    .await;
    if !appeared {
        return Err(RunnerError::TmuxFailed(format!(
            "anchor {pid} never appeared in list-clients for {id}"
        )));
    }
    // The anchor must have ANSWERED tmux's startup queries before the
    // harness is released (spec 4): wait until the client's termtype
    // is the kb profile (tmux sets it from the XTVERSION reply).
    let answered = poll_until(
        Duration::from_secs(5),
        Duration::from_millis(50),
        || async {
            let out = runner
                .tmux
                .run(
                    &sock,
                    &["list-clients", "-F", "#{client_pid} #{client_termtype}"],
                    None,
                )
                .await;
            matches!(out, Ok(o) if o.ok && o.stdout.lines().any(|l| {
                let mut p = l.split_whitespace();
                p.next() == Some(pid.to_string().as_str())
                    && p.next().unwrap_or("").starts_with("kb-term")
            }))
        },
    )
    .await;
    if !answered {
        return Err(RunnerError::TmuxFailed(format!(
            "anchor {pid} never answered tmux's startup queries for {id}"
        )));
    }
    Ok(())
}

/// Remove `<run-dir>/s/<id>.sock` and `<run-dir>/s/<id>/` (best effort).
pub async fn remove_session_files<S: Supervisor>(runner: &Runner<S>, id: &str) {
    if !valid_id(id) {
        return;
    }
    let _ = tokio::fs::remove_file(runner.holder_socket(id)).await;
    let _ = tokio::fs::remove_dir_all(runner.sessions_dir().join(id)).await;
}

/// The harness by its wire spelling (for `@kb_harness`).
pub fn harness_name(h: Harness) -> &'static str {
    h.as_str()
}

/// Whether `harness` (string) is in the closed set.
pub fn harness_known(harness: &str) -> bool {
    valid_harness(harness)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn gate_argv_is_exactly_the_documented_shape() {
        let env: BTreeMap<String, String> = HashMap::from([
            ("FOO".to_string(), "bar".to_string()),
            ("A_B".to_string(), "x y".to_string()),
        ])
        .into_iter()
        .collect();
        let argv = vec![
            "/usr/bin/omp".to_string(),
            "--flag with space".to_string(),
            "semi;colon".to_string(),
            "quo\"te".to_string(),
        ];
        let got = gate_argv(
            "/pinned/tmux",
            Path::new("/run/s/abc12345.sock"),
            "abc12345",
            Path::new("/work/proj"),
            120,
            40,
            &env,
            &argv,
        );
        assert_eq!(
            got,
            vec![
                "new-session",
                "-d",
                "-s",
                "kb",
                "-x",
                "120",
                "-y",
                "40",
                "-c",
                "/work/proj",
                "-e",
                "KB_TERM_ID=abc12345",
                "-e",
                "A_B=x y",
                "-e",
                "FOO=bar",
                "--",
                "/bin/sh",
                "-c",
                "T=$1; S=$2; shift 2; \"$T\" -S \"$S\" wait-for kbterm-go; exec \"$@\"",
                "kbterm-gate",
                "/pinned/tmux",
                "/run/s/abc12345.sock",
                "/usr/bin/omp",
                "--flag with space",
                "semi;colon",
                "quo\"te",
            ]
        );
    }

    #[test]
    fn launch_rejects_bad_requests() {
        let cfg = RunnerConfig::test_config(
            PathBuf::from("/tmp/kb-term-test"),
            "/pinned/tmux".to_string(),
            PathBuf::from("/dev/null"),
        );
        let base = || Request::Launch {
            harness: Harness::Omp,
            argv: Some(vec!["/bin/cat".to_string()]),
            resume_sid: None,
            cwd: "/tmp".to_string(),
            cols: Some(120),
            rows: Some(40),
            env: None,
            exit_keys: None,
        };
        let mut bad = vec![];
        let mut r = base();
        if let Request::Launch { cwd, .. } = &mut r {
            *cwd = "relative".to_string();
        }
        bad.push(r);
        let mut r = base();
        if let Request::Launch { cwd, .. } = &mut r {
            *cwd = "/definitely/not/a/dir/kb-term".to_string();
        }
        bad.push(r);
        let mut r = base();
        if let Request::Launch { argv, .. } = &mut r {
            *argv = Some(vec![]);
        }
        bad.push(r);
        let mut r = base();
        if let Request::Launch { argv, .. } = &mut r {
            *argv = Some(vec!["cat".to_string()]);
        }
        bad.push(r);
        let mut r = base();
        if let Request::Launch { cols, .. } = &mut r {
            *cols = Some(3);
        }
        bad.push(r);
        let mut r = base();
        if let Request::Launch { env, .. } = &mut r {
            *env = Some(
                [("lower".to_string(), "x".to_string())]
                    .into_iter()
                    .collect(),
            );
        }
        bad.push(r);
        let mut r = base();
        if let Request::Launch { exit_keys, .. } = &mut r {
            *exit_keys = Some(vec!["C-c;rm".to_string()]);
        }
        bad.push(r);
        let mut r = base();
        if let Request::Launch { argv, .. } = &mut r {
            *argv = Some(vec!["/bin/cat\0x".to_string()]);
        }
        bad.push(r);
        // `validate_launch` is pure (takes neither Tmux nor Supervisor),
        // so it cannot spawn anything by construction; the ordering
        // "validation before any tmux/process call" is pinned at the RPC
        // boundary by `bad_launch_precedes_any_tmux_call` below.
        for req in bad {
            let err = validate_launch(&req, &cfg).unwrap_err();
            assert_eq!(err.code(), "bad-request", "request {req:?}");
        }
    }

    /// Validation precedes ANY tmux/process interaction: with a tmux
    /// binary that does not exist, a bad launch must still answer
    /// `bad-request` (not `tmux-failed`) and create no holder. If
    /// validation ever ran after the holder spawn, spawning the bogus
    /// binary would fail first and the code would differ.
    #[tokio::test]
    async fn bad_launch_precedes_any_tmux_call() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("run");
        std::fs::create_dir_all(run_dir.join("s")).unwrap();
        std::fs::create_dir_all(run_dir.join("runner")).unwrap();
        let cfg = RunnerConfig::test_config(
            run_dir.clone(),
            "/definitely/not/a/tmux/kb-term".to_string(),
            std::path::PathBuf::from("/dev/null"),
        );
        let runner = crate::runner::Runner::direct(cfg);
        let req = Request::Launch {
            harness: Harness::Omp,
            argv: Some(vec!["relative-argv".to_string()]),
            resume_sid: None,
            cwd: "/tmp".to_string(),
            cols: Some(120),
            rows: Some(40),
            env: None,
            exit_keys: None,
        };
        match crate::runner::rpc::dispatch(&runner, &req).await {
            crate::wire::Response::Err { code, .. } => assert_eq!(code, "bad-request"),
            crate::wire::Response::Ok { .. } => panic!("bad launch must be bad-request"),
        }
        let entries: Vec<_> = std::fs::read_dir(run_dir.join("s"))
            .unwrap()
            .flatten()
            .collect();
        assert!(entries.is_empty(), "no holder may be spawned");
    }
}
