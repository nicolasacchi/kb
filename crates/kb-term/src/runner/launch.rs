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

use crate::profiles::{self, PlanRequest};
use crate::runner::anchor::Anchor;
use crate::runner::ids::{generate_id, valid_harness, valid_id};
use crate::runner::supervisor::Supervisor;
use crate::runner::tmux::Tmux;
use crate::runner::{Runner, RunnerConfig, RunnerError};
use crate::wire::{Harness, Request};

/// The documented gate: `wait-for` holds the harness until the runner
/// sends `wait-for -S kbterm-go` after the tap and the anchor are in.
/// The ONLY shell string the runner ever builds. It runs under the
/// non-login `/bin/sh -c` (no `-l`): no user profile is sourced for a
/// pane. The unset prelude + prefix snippet prepended to this script
/// defend server-env inheritance (a tmux server inherits its first
/// client's env); they do not defend a login-profile threat the
/// launcher cannot have.
pub const GATE_SCRIPT: &str =
    "T=$1; S=$2; shift 2; \"$T\" -S \"$S\" wait-for kbterm-go; exec \"$@\"";

/// The default exit keys (the omp sequence; per-harness tables are T10).
pub const DEFAULT_EXIT_KEYS: [&str; 3] = ["C-c", "C-c", "C-d"];

/// Maximum `resume_sid` byte length (validation `bad-request` above this).
pub const MAX_RESUME_SID_LEN: usize = 256;

/// Build the exact `new-session` argv for a launch. Pure (no IO) so the
/// gate shape is pinned by a unit test.
///
/// The `-e` entries come from the caller `env` map alone: `KB_TERM_ID`
/// is NOT hardcoded here because the plan env already carries it (one
/// source), so the argv never repeats it.
#[allow(clippy::too_many_arguments)]
pub fn gate_argv(
    tmux_bin: &str,
    sock: &Path,
    _id: &str,
    cwd: &Path,
    cols: u16,
    rows: u16,
    env: &BTreeMap<String, String>,
    argv: &[String],
    script: &str,
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
    ];
    for (k, v) in env {
        out.push("-e".to_string());
        out.push(format!("{k}={v}"));
    }
    out.push("--".to_string());
    out.push("/bin/sh".to_string());
    out.push("-c".to_string());
    out.push(script.to_string());
    out.push("kbterm-gate".to_string());
    out.push(tmux_bin.to_string());
    out.push(sock.to_string_lossy().into_owned());
    out.extend(argv.iter().cloned());
    out
}
/// Build the pane wrapper script: server-env defence prelude + prefix
/// snippet + the gate. Pure (no IO) so the defence is pinned by a unit
/// test that does not need tmux; the production launch calls this exact
/// helper, so a test that fails without the defence proves the
/// production path emits it.
pub fn build_gate_script(unset_after_login: &[String]) -> Result<String, String> {
    let prelude = profiles::env::shell_unset_prelude(unset_after_login)?;
    Ok(format!(
        "{prelude}{} {GATE_SCRIPT}",
        profiles::env::shell_unset_prefixes_snippet()
    ))
}

/// A validated launch request (everything after the pre-tmux checks).
#[derive(Debug, Clone)]
pub struct ValidatedLaunch {
    /// Harness name (wire enum; for the `@kb_harness` option).
    pub harness: String,
    /// The caller's argv, if any: `None` from kb-server by design (the
    /// runner builds argv from the harness profile). Empty means "use
    /// the plan argv" — the plan governs unless verbatim mode is set.
    /// `Some` entries are validated (absolute `argv[0]`, harness-named,
    /// forbidden scan).
    pub argv: Vec<String>,
    /// Resume session id, if any.
    pub resume_sid: Option<String>,
    /// Working directory.
    pub cwd: std::path::PathBuf,
    /// Grid columns.
    pub cols: u16,
    /// Grid rows.
    pub rows: u16,
    /// Extra environment for the pane, merged on top of the profile
    /// env (see `merge_caller_env`).
    pub env: BTreeMap<String, String>,
    /// Exit keys used by the kill RPC. `None` means "no opinion": the
    /// harness profile's exit steps win. `Some` always wins, even when
    /// it equals the default the wire protocol folds `None` into.
    pub exit_keys: Option<Vec<String>>,
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
    // kb-server sends `argv: None` by design (the runner builds argv
    // from the harness profile). `None` (and an empty `Some`) therefore
    // validates as "use the plan argv"; only a non-empty `Some` is
    // validated as verbatim caller argv. Production always uses the
    // plan argv, so a wire caller cannot run an alternate program.
    let argv: Vec<String> = argv.clone().unwrap_or_default();
    if !argv.is_empty() {
        if !argv[0].starts_with('/') {
            return Err(RunnerError::BadRequest(
                "argv[0] must be an absolute path".to_string(),
            ));
        }
        if let Some(bad) = argv
            .iter()
            .find(|a| profiles::table::FORBIDDEN_ARGV.contains(&a.as_str()))
        {
            return Err(RunnerError::BadRequest(format!(
                "argv element `{bad}` is forbidden"
            )));
        }
        let names_harness = std::path::Path::new(&argv[0])
            .file_name()
            .is_some_and(|f| f.to_str() == Some(harness.as_str()));
        if !names_harness && !cfg.allow_verbatim_probe {
            return Err(RunnerError::BadRequest(format!(
                "argv[0] must name the harness binary ({})",
                harness.as_str()
            )));
        }
        let total: usize = argv.iter().map(String::len).sum();
        if total > 32 * 1024 {
            return Err(RunnerError::BadRequest("argv exceeds 32 KiB".to_string()));
        }
        for arg in &argv {
            if arg.contains('\0') {
                return Err(RunnerError::BadRequest(
                    "argv contains a NUL byte".to_string(),
                ));
            }
        }
    }
    // `resume_sid` is written verbatim into `@kb_resume_sid` and echoed in
    // list; bound it (~256 B) so it can never grow past the line cap.
    if let Some(sid) = resume_sid {
        if sid.len() > MAX_RESUME_SID_LEN {
            return Err(RunnerError::BadRequest(format!(
                "resume_sid exceeds {MAX_RESUME_SID_LEN} bytes"
            )));
        }
        if sid.contains('\0') {
            return Err(RunnerError::BadRequest(
                "resume_sid contains a NUL byte".to_string(),
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
    // `None` is kept as `None` (the profile exit steps win); only
    // caller-supplied keys are validated and stored, so explicit keys
    // that happen to equal the wire default still win downstream.
    let exit_keys = exit_keys.clone();
    if let Some(keys) = &exit_keys {
        if keys.len() > 8 {
            return Err(RunnerError::BadRequest("at most 8 exit keys".to_string()));
        }
        for key in keys {
            if !valid_exit_key(key) {
                return Err(RunnerError::BadRequest(format!("bad exit key `{key}`")));
            }
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
            // Best-effort teardown: read the pane/server pids BEFORE
            // stopping (the server must still answer), then stop the
            // units, drop the anchor, remove `s/<id>*`. An empty StopCtx
            // would leave a stubborn pane alive (its SIGTERMs would go
            // to nobody); the pids let direct mode SIGKILL it.
            runner.take_anchor(&id).await;
            let ctx = teardown_stop_ctx(runner, &id).await;
            let _ = runner.supervisor.stop(&id, ctx).await;
            remove_session_files(runner, &id).await;
            Err(e)
        }
    }
}

/// The supervisor stop context for a teardown: the pane and server pids
/// read best-effort from the holder (each `None` when the server no
/// longer answers).
pub async fn teardown_stop_ctx<S: Supervisor>(
    runner: &Runner<S>,
    id: &str,
) -> crate::runner::supervisor::StopCtx {
    let (pane_pid, server_pid) = crate::runner::discovery::pane_and_server_pids(runner, id).await;
    crate::runner::supervisor::StopCtx {
        pane_pid,
        server_pid,
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
    // Step 3: build the launch plan (per-harness profiles) and create
    // the session (the pane command is the gate).
    let plan = build_launch_plan(runner, id, spec)?;
    let plan_env: BTreeMap<String, String> = plan.env.iter().cloned().collect();
    let script = build_gate_script(&plan.unset_after_login)
        .map_err(|bad| RunnerError::Internal(format!("bad env name in unset prelude: {bad}")))?;
    // The plan argv governs the launch: `validate_launch` rejects any
    // caller argv[0] that does not name the harness binary unless
    // `allow_verbatim_probe` is set (test harness only), in which case
    // an integration fixture runs verbatim (still under the profile env
    // and exit keys). `spec.argv` empty (kb-server sends `argv: None`,
    // "use the plan") always yields the plan argv, even in verbatim
    // mode. Caller extras are otherwise discarded, so no wire-supplied
    // argv can smuggle `--kb-off` or a bare continue past the profile.
    let launch_argv = if spec.argv.is_empty()
        || is_profile_binary(&spec.argv, &plan.argv)
        || !runner.cfg.allow_verbatim_probe
    {
        plan.argv.clone()
    } else {
        spec.argv.clone()
    };
    let args = gate_argv(
        &runner.cfg.tmux_bin,
        &sock,
        id,
        &spec.cwd,
        spec.cols,
        spec.rows,
        &plan_env,
        &launch_argv,
        &script,
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
    let exit_keys = effective_exit_keys(spec, &plan);
    set_user_options(
        runner,
        &sock,
        id,
        spec,
        &exit_keys,
        recorded_exit_steps(spec, &plan),
        started,
    )
    .await?;
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
    exit_keys: &[String],
    exit_steps: Option<&'static [profiles::ExitStep]>,
    started: u64,
) -> Result<(), RunnerError> {
    let mut opts: Vec<(&str, String)> = vec![
        ("@kb_harness", spec.harness.clone()),
        ("@kb_cwd", spec.cwd.to_string_lossy().into_owned()),
        ("@kb_started", started.to_string()),
        (
            "@kb_exit_keys",
            serde_json::to_string(exit_keys).map_err(|e| RunnerError::Internal(e.to_string()))?,
        ),
    ];
    // Profile steps (with their waits) are recorded only when the caller
    // gave no exit keys; the kill path prefers `@kb_exit_steps`, so
    // recording them next to explicit keys would override the caller.
    if let Some(steps) = exit_steps {
        opts.push((
            "@kb_exit_steps",
            serde_json::to_string(&profiles::OwnedExitStep::from_slice(steps))
                .map_err(|e| RunnerError::Internal(e.to_string()))?,
        ));
    }
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
/// Build the launch profile for one launch from the runner's own env
/// (held implicitly: `profiles::plan` filters it through the allowlist),
/// then merge the validated caller env on top (`merge_caller_env`). The
/// plan env stays authoritative: the profile env and the kb-term
/// identity pair are never overridden by caller entries.
fn build_launch_plan<S: Supervisor>(
    runner: &Runner<S>,
    id: &str,
    spec: &ValidatedLaunch,
) -> Result<profiles::LaunchPlan, RunnerError> {
    let base_env: Vec<(String, String)> = std::env::vars().collect();
    let run_dir = runner.sessions_dir().join(id);
    let cwd = spec.cwd.to_string_lossy().into_owned();
    let plan = profiles::plan(&PlanRequest {
        harness: &spec.harness,
        term_id: id,
        resume_sid: spec.resume_sid.as_deref(),
        cwd: &cwd,
        job_ulid: None,
        run_dir: &run_dir,
        base_env: &base_env,
        extra_passthrough: &[],
    })
    .map_err(|e| match e {
        profiles::PlanError::Io(msg) => RunnerError::Internal(format!("launch profile: {msg}")),
        other => RunnerError::BadRequest(format!("launch profile: {other}")),
    })?;
    merge_caller_env(plan, &spec.env)
}

/// Merge validated caller env (`Request::Launch.env`) into an
/// already-built plan. The plan env (allowlist + profile env + kb-term
/// identity) is authoritative for the pane: caller entries are added
/// (re-sorted) but never override the identity pair or a profile-owned
/// name (`PI_*`, `CLAUDE_*`), never name a tmux-owned or never-forward
/// name, and never reintroduce a forbidden outer-terminal name — those
/// are `bad-request`, not silently dropped.
fn merge_caller_env(
    mut plan: profiles::LaunchPlan,
    extra: &BTreeMap<String, String>,
) -> Result<profiles::LaunchPlan, RunnerError> {
    for (k, v) in extra {
        if !profiles::env::is_valid_env_identifier(k)
            || profiles::env::is_forbidden_name(k)
            || profiles::env::TMUX_OWNED.contains(&k.as_str())
            || profiles::env::NEVER_FORWARD.contains(&k.as_str())
            || profiles::env::is_profile_owned(k)
        {
            return Err(RunnerError::BadRequest(format!(
                "env key `{k}` may not be forwarded"
            )));
        }
        if let Some(pos) = plan.env.iter().position(|(n, _)| n == k) {
            plan.env[pos].1 = v.clone();
        } else {
            plan.env.push((k.clone(), v.clone()));
        }
    }
    plan.env.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(plan)
}

/// The effective exit keys for a launch: explicit caller keys always
/// win; only `None` (no caller opinion) falls back to the harness
/// profile's exit steps (flattened to the key strings the kill RPC
/// sends). Branching on `Option::is_some` — not on equality with the
/// wire default — means explicit keys that happen to equal the default
/// still win.
fn effective_exit_keys(spec: &ValidatedLaunch, plan: &profiles::LaunchPlan) -> Vec<String> {
    match &spec.exit_keys {
        None => flattened_exit_keys(plan.exit_steps),
        Some(keys) => keys.clone(),
    }
}

/// The structured exit steps to record in `@kb_exit_steps`: the profile
/// steps when the caller gave no exit keys, `None` (option left unset, so
/// the kill path sends the caller's keys) when it did.
fn recorded_exit_steps(
    spec: &ValidatedLaunch,
    plan: &profiles::LaunchPlan,
) -> Option<&'static [profiles::ExitStep]> {
    spec.exit_keys.is_none().then_some(plan.exit_steps)
}

/// Flatten exit steps to the key strings recorded for `@kb_exit_keys`
/// (waits live in `@kb_exit_steps`; the kill path honours them).
fn flattened_exit_keys(steps: &[profiles::ExitStep]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for step in steps {
        match step {
            profiles::ExitStep::Keys(keys) => out.extend(keys.iter().map(|k| k.to_string())),
            profiles::ExitStep::Text(text) => out.push(text.to_string()),
            profiles::ExitStep::WaitMs(_) => {}
        }
    }
    out
}
/// Whether the caller's `argv[0]` names the profile binary (its file
/// name equals the plan's bare binary, e.g. `/usr/bin/omp` -> `omp`).
fn is_profile_binary(caller_argv: &[String], plan_argv: &[String]) -> bool {
    let Some(first) = caller_argv.first() else {
        return false;
    };
    let Some(bin) = plan_argv.first() else {
        return false;
    };
    std::path::Path::new(first)
        .file_name()
        .is_some_and(|f| f.to_str() == Some(bin.as_str()))
}
#[cfg(test)]
mod profile_binary_tests {
    use super::*;

    #[test]
    fn profile_binary_matches_only_the_profile_name() {
        let plan_argv = vec!["omp".to_string()];
        assert!(is_profile_binary(&["/usr/bin/omp".to_string()], &plan_argv));
        assert!(is_profile_binary(
            &["/home/u/.local/bin/omp".to_string()],
            &plan_argv
        ));
        assert!(!is_profile_binary(
            &["/usr/bin/python3".to_string()],
            &plan_argv
        ));
        assert!(!is_profile_binary(
            &["/usr/bin/omp2".to_string()],
            &plan_argv
        ));
        assert!(!is_profile_binary(&[], &plan_argv));
    }
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
        // `KB_TERM_ID` rides in the env map (the plan env); gate_argv
        // never hardcodes it, so the vector carries it exactly once.
        let env: BTreeMap<String, String> = HashMap::from([
            ("FOO".to_string(), "bar".to_string()),
            ("A_B".to_string(), "x y".to_string()),
            ("KB_TERM_ID".to_string(), "abc12345".to_string()),
        ])
        .into_iter()
        .collect();
        let argv = vec![
            "/usr/bin/omp".to_string(),
            "--flag with space".to_string(),
            "semi;colon".to_string(),
            "quo\"te".to_string(),
        ];
        let script =
            "unset FOO; T=$1; S=$2; shift 2; \"$T\" -S \"$S\" wait-for kbterm-go; exec \"$@\"";
        let got = gate_argv(
            "/pinned/tmux",
            Path::new("/run/s/abc12345.sock"),
            "abc12345",
            Path::new("/work/proj"),
            120,
            40,
            &env,
            &argv,
            script,
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
                "A_B=x y",
                "-e",
                "FOO=bar",
                "-e",
                "KB_TERM_ID=abc12345",
                "--",
                "/bin/sh",
                "-c",
                "unset FOO; T=$1; S=$2; shift 2; \"$T\" -S \"$S\" wait-for kbterm-go; exec \"$@\"",
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
            argv: Some(vec!["/usr/bin/omp".to_string()]),
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

    /// `resume_sid` is bounded at 256 B: a 257-B sid is `bad-request`
    /// (not silently truncated and not written to `@kb_resume_sid`),
    /// while exactly 256 B validates.
    #[test]
    fn validate_rejects_overlong_resume_sid() {
        let cfg = cfg();
        let mut req = launch_with(&["/usr/bin/omp"]);
        if let Request::Launch { resume_sid, .. } = &mut req {
            *resume_sid = Some("s".repeat(257));
        }
        let err = validate_launch(&req, &cfg).unwrap_err();
        assert_eq!(err.code(), "bad-request");
        let mut req = launch_with(&["/usr/bin/omp"]);
        if let Request::Launch { resume_sid, .. } = &mut req {
            *resume_sid = Some("s".repeat(256));
        }
        assert!(
            validate_launch(&req, &cfg).is_ok(),
            "exactly 256 B must validate"
        );
    }

    /// The launch teardown context carries the pane and server pids read
    /// best-effort (a stubborn pane is then SIGKILLable); an empty
    /// StopCtx would send SIGTERMs to nobody and leak it.
    #[tokio::test]
    async fn teardown_stop_ctx_reads_pane_and_server_pids() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake_tmux.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             case \"$*\" in\n\
             *'#{pane_pid}'*) printf '4242\\n' ;;\n\
             *'#{pid}'*) printf '5151\\n' ;;\n\
             esac\n\
             exit 0\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let run_dir = dir.path().join("run");
        let cfg = RunnerConfig::test_config(
            run_dir,
            script.to_string_lossy().into_owned(),
            PathBuf::from("/dev/null"),
        );
        let runner = crate::runner::Runner::direct(cfg);
        let ctx = teardown_stop_ctx(&runner, "abcd1234").await;
        assert_eq!(
            ctx.pane_pid,
            Some(4242),
            "teardown ctx must carry the pane pid"
        );
        assert_eq!(
            ctx.server_pid,
            Some(5151),
            "teardown ctx must carry the server pid"
        );
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

    fn cfg() -> RunnerConfig {
        RunnerConfig::test_config(
            PathBuf::from("/tmp/kb-term-test"),
            "/pinned/tmux".to_string(),
            PathBuf::from("/dev/null"),
        )
    }

    fn cfg_probe() -> RunnerConfig {
        let mut c = cfg();
        c.allow_verbatim_probe = true;
        c
    }

    fn launch_with(argv: &[&str]) -> Request {
        Request::Launch {
            harness: Harness::Omp,
            argv: Some(argv.iter().map(ToString::to_string).collect()),
            resume_sid: None,
            cwd: "/tmp".to_string(),
            cols: Some(120),
            rows: Some(40),
            env: None,
            exit_keys: None,
        }
    }

    /// The profile governs the launch: a caller argv that does not
    /// name the harness binary is `bad-request`, and forbidden argv
    /// elements never pass — even under verbatim mode, so a verbatim
    /// argv cannot smuggle `--kb-off` or `-c` through validation.
    #[test]
    fn validate_rejects_argv_not_naming_the_harness() {
        let err = validate_launch(&launch_with(&["/tmp/copy-of-omp"]), &cfg()).unwrap_err();
        assert_eq!(err.code(), "bad-request");
        let err = validate_launch(&launch_with(&["/usr/bin/omp", "--kb-off"]), &cfg()).unwrap_err();
        assert_eq!(err.code(), "bad-request");
        assert!(
            validate_launch(&launch_with(&["/usr/bin/omp"]), &cfg()).is_ok(),
            "an argv[0] naming the harness binary validates"
        );
    }
    /// kb-server sends `argv: None` by design (the runner builds argv
    /// from the harness profile). `validate_launch` must accept it
    /// (empty ValidatedLaunch argv → plan argv in `launch_locked`), and
    /// the committed `req_launch.json` golden bytes must validate too.
    #[test]
    fn validate_accepts_missing_argv_from_kb_server() {
        let req = Request::Launch {
            harness: Harness::Omp,
            argv: None,
            resume_sid: None,
            cwd: "/tmp".to_string(),
            cols: Some(120),
            rows: Some(40),
            env: None,
            exit_keys: None,
        };
        let v = validate_launch(&req, &cfg()).unwrap();
        assert!(v.argv.is_empty(), "None argv means use the plan");
        let golden = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/req_launch.json");
        let bytes = std::fs::read(golden).unwrap();
        let mut req: Request = serde_json::from_slice(&bytes).unwrap();
        // The golden's cwd is a synthetic path that does not exist on the
        // test host; point it at a real dir so only the shape is judged.
        if let Request::Launch { cwd, .. } = &mut req {
            *cwd = "/tmp".to_string();
        }
        assert!(
            validate_launch(&req, &cfg()).is_ok(),
            "golden bytes validate"
        );
        // An empty `Some` behaves like `None`.
        let req = Request::Launch {
            harness: Harness::Omp,
            argv: Some(vec![]),
            resume_sid: None,
            cwd: "/tmp".to_string(),
            cols: Some(120),
            rows: Some(40),
            env: None,
            exit_keys: None,
        };
        let v = validate_launch(&req, &cfg()).unwrap();
        assert!(v.argv.is_empty());
    }

    /// Only `allow_verbatim_probe` (test harness only) lets a fixture
    /// program run verbatim, and the forbidden scan applies there too.
    /// The ambient env never gates it: with the flag off the same
    /// argv is `bad-request` even when the legacy env var is set.
    #[test]
    fn validate_probe_flag_allows_fixtures_but_not_forbidden_argv() {
        std::env::set_var("KB_TERM_TEST_PROBE", "1");
        let err = validate_launch(
            &launch_with(&["/usr/bin/python3", "-I", "/fixtures/probe.py"]),
            &cfg(),
        )
        .unwrap_err();
        assert_eq!(
            err.code(),
            "bad-request",
            "ambient env must not honour verbatim argv"
        );
        std::env::remove_var("KB_TERM_TEST_PROBE");
        let ok = validate_launch(
            &launch_with(&["/usr/bin/python3", "-I", "/fixtures/probe.py"]),
            &cfg_probe(),
        );
        assert!(
            ok.is_ok(),
            "verbatim argv validates under the explicit flag"
        );
        let err = validate_launch(
            &launch_with(&["/usr/bin/python3", "--kb-off"]),
            &cfg_probe(),
        )
        .unwrap_err();
        assert_eq!(err.code(), "bad-request");
        let err = validate_launch(&launch_with(&["/usr/bin/python3", "-c", "x"]), &cfg_probe())
            .unwrap_err();
        assert_eq!(err.code(), "bad-request");
    }

    /// The pane wrapper script emitted by the production helper carries
    /// the server-env defence: `unset` prelude + prefix snippet + gate.
    /// Without the defence the forbidden names would survive server-env
    /// inheritance; with it they are unset before exec.
    #[test]
    fn gate_script_contains_unset_defence() {
        let names = vec![
            "KITTY_WINDOW_ID".to_string(),
            "TERM_PROGRAM".to_string(),
            "GHOSTTY_FOO".to_string(),
        ];
        let script = build_gate_script(&names).unwrap();
        assert!(
            script.contains("unset KITTY_WINDOW_ID"),
            "prelude: {script}"
        );
        assert!(script.contains("GHOSTTY_"), "snippet: {script}");
        assert!(script.contains(GATE_SCRIPT), "gate: {script}");
        assert!(
            !script.contains("-l"),
            "non-login wrapper must not invoke a login shell: {script}"
        );
        // The gate argv itself is the non-login shell, never `-l`.
        let env: BTreeMap<String, String> = BTreeMap::new();
        let got = gate_argv(
            "/pinned/tmux",
            Path::new("/run/s/abc12345.sock"),
            "abc12345",
            Path::new("/tmp"),
            120,
            40,
            &env,
            &["/usr/bin/omp".to_string()],
            &script,
        );
        let sh_pos = got.iter().position(|a| a == "/bin/sh").unwrap();
        assert_eq!(&got[sh_pos + 1], "-c", "gate uses /bin/sh -c");
        assert!(!got.iter().any(|a| a == "-l"), "gate never uses -l");
    }

    /// Explicit caller exit keys always win — even when they equal the
    /// wire default the protocol folds `None` into. Only `None` falls
    /// back to the harness profile's exit steps.
    #[test]
    fn effective_exit_keys_explicit_default_still_wins() {
        let plan = profiles::plan(&PlanRequest {
            harness: "claude",
            term_id: "abc123",
            resume_sid: None,
            cwd: "/tmp",
            job_ulid: None,
            run_dir: std::path::Path::new("/tmp/kb-term-test"),
            base_env: &[],
            extra_passthrough: &[],
        })
        .unwrap();
        let spec = ValidatedLaunch {
            harness: "claude".to_string(),
            argv: vec!["claude".to_string()],
            resume_sid: None,
            cwd: PathBuf::from("/tmp"),
            cols: 120,
            rows: 40,
            env: BTreeMap::new(),
            exit_keys: None,
        };
        assert_eq!(
            effective_exit_keys(&spec, &plan),
            vec!["/exit".to_string(), "Enter".to_string()],
            "None falls back to the claude profile steps"
        );
        let explicit = ValidatedLaunch {
            exit_keys: Some(DEFAULT_EXIT_KEYS.iter().map(ToString::to_string).collect()),
            ..spec
        };
        assert_eq!(
            effective_exit_keys(&explicit, &plan),
            vec!["C-c".to_string(), "C-c".to_string(), "C-d".to_string()],
            "explicit default-equal keys still win over the profile"
        );
        // The kill path prefers `@kb_exit_steps`, so explicit keys must
        // leave it unrecorded or the profile steps would override them.
        assert!(
            recorded_exit_steps(&explicit, &plan).is_none(),
            "explicit keys: no profile steps recorded"
        );
        assert!(
            recorded_exit_steps(
                &ValidatedLaunch {
                    exit_keys: None,
                    ..explicit
                },
                &plan
            )
            .is_some(),
            "no caller keys: the profile steps are recorded"
        );
    }

    /// The plan env is authoritative: caller env merges on top but can
    /// never override the identity pair, a profile-owned name, or
    /// forward a forbidden, tmux-owned, or identity name.
    #[test]
    fn merge_caller_env_keeps_plan_authoritative() {
        let plan = || {
            profiles::plan(&PlanRequest {
                harness: "claude",
                term_id: "abc123",
                resume_sid: None,
                cwd: "/tmp",
                job_ulid: None,
                run_dir: std::path::Path::new("/tmp/kb-term-test"),
                base_env: &[],
                extra_passthrough: &[],
            })
            .unwrap()
        };
        let mut extra = BTreeMap::new();
        extra.insert("FOO".to_string(), "bar".to_string());
        let merged = merge_caller_env(plan(), &extra).unwrap();
        assert!(merged.env.contains(&("FOO".to_string(), "bar".to_string())));
        assert!(merged
            .env
            .contains(&("KB_TERM_ID".to_string(), "abc123".to_string())));
        let mut sorted = merged.env.clone();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(merged.env, sorted, "merged env stays sorted by name");
        for bad_key in [
            "KB_TERM_ID",
            "KITTY_WINDOW_ID",
            "TERM",
            "KB_SESSION_ID",
            "PI_FORCE_IMAGE_PROTOCOL",
            "PI_CONFIG_FILES",
            "PI_IMAGE_PROTOCOL",
            "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN",
        ] {
            let mut extra = BTreeMap::new();
            extra.insert(bad_key.to_string(), "x".to_string());
            assert_eq!(
                merge_caller_env(plan(), &extra).unwrap_err().code(),
                "bad-request",
                "key {bad_key}"
            );
        }
    }
}
