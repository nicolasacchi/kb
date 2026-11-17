//! Per-harness launch profiles: `plan()` turns a launch request into the
//! exact argv + complete pane env + unset-after-login list + exit steps.
//!
//! kb-term carries a pinned copy of `kb_core::sessions::HARNESSES` and
//! of the kb-core resume table (it must not depend on kb-core); a drift
//! test in kb-server keeps the two in sync.

pub mod env;
pub mod table;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Pinned copy of `kb_core::sessions::HARNESSES` (same order).
pub const HARNESSES: [&str; 5] = ["claude", "codex", "opencode", "grok", "omp"];

/// The approved private omp overlay (never touch the user's global
/// config): image/recapture pins only.
pub const OMP_OVERLAY: &str = include_str!("../../share/omp/overlay.yml");

/// One step of a harness exit sequence (sent before the cgroup kill,
/// which is the backstop that never depends on these). `WaitMs` is
/// honoured by the kill path; it is not dead data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStep {
    Keys(&'static [&'static str]),
    Text(&'static str),
    WaitMs(u32),
}

/// Owned, JSON-serialisable mirror of [`ExitStep`] for the
/// `@kb_exit_steps` tmux option. [`ExitStep`] borrows `&'static str`
/// literals and cannot be deserialised from JSON; this type carries the
/// same steps as owned `String`s.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OwnedExitStep {
    Keys(Vec<String>),
    Text(String),
    WaitMs(u32),
}

impl OwnedExitStep {
    /// Copy a static step into its owned transport form.
    pub fn from_static(step: &ExitStep) -> Self {
        match step {
            ExitStep::Keys(keys) => Self::Keys(keys.iter().map(|k| k.to_string()).collect()),
            ExitStep::Text(t) => Self::Text(t.to_string()),
            ExitStep::WaitMs(ms) => Self::WaitMs(*ms),
        }
    }

    /// Copy a static slice into owned form.
    pub fn from_slice(steps: &[ExitStep]) -> Vec<Self> {
        steps.iter().map(Self::from_static).collect()
    }
}

/// Everything `plan()` needs. `base_env` is the runner's own env
/// (FILTERED inside `plan()`, never forwarded wholesale);
/// `extra_passthrough` names env vars to copy on top of the allowlist
/// (callers pass `&[]` until a `[terminal]` config key exists).
pub struct PlanRequest<'a> {
    pub harness: &'a str,
    /// `[a-z0-9]{1,16}`.
    pub term_id: &'a str,
    /// `None` = fresh launch.
    pub resume_sid: Option<&'a str>,
    /// Source session id of a cross-harness handoff (`claude`/`omp`
    /// targets only; exclusive with `resume_sid`): the pane runs
    /// `kb sessions handoff <id> --to <harness> --transplant --launch
    /// --cwd <cwd>` instead of the harness binary.
    pub handoff_from: Option<&'a str>,
    /// Absolute, no control chars.
    pub cwd: &'a str,
    pub job_ulid: Option<&'a str>,
    /// `/run/kb-term/s/<id>`; the omp overlay is written below it.
    pub run_dir: &'a Path,
    pub base_env: &'a [(String, String)],
    pub extra_passthrough: &'a [String],
}

/// The complete launch plan for one pane.
#[derive(Debug, Clone)]
pub struct LaunchPlan {
    /// `argv[0]` is the bare binary name (resolved by the pane PATH).
    pub argv: Vec<String>,
    /// Complete pane env (allowlist result + profile env + kb-term
    /// identity), sorted by name.
    pub env: Vec<(String, String)>,
    /// Forbidden exact names + concrete forbidden-prefixed names
    /// present in `base_env` (the wrapper unsets these before exec to
    /// defend server-env inheritance; the pane shell is non-login).
    pub unset_after_login: Vec<String>,
    /// omp only: absolute overlay path (mode 0600).
    pub overlay_path: Option<PathBuf>,
    pub experimental: bool,
    pub exit_steps: &'static [ExitStep],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    UnknownHarness,
    UnsafeToken(&'static str),
    BadCwd,
    ForbiddenEnvName(String),
    BadTermId,
    /// An overlay write failed (disk/permission state, not a bad
    /// request). Callers map this to an internal error, never a
    /// `bad-request`.
    Io(String),
    /// `handoff_from` was set for a harness that is not a handoff target
    /// (only `claude` and `omp` are).
    HandoffUnsupported,
    /// `handoff_from` and `resume_sid` were both set.
    HandoffWithResume,
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanError::UnknownHarness => write!(f, "unknown harness"),
            PlanError::UnsafeToken(field) => write!(f, "unsafe token in {field}"),
            PlanError::BadCwd => write!(f, "cwd must be absolute with no control chars"),
            PlanError::ForbiddenEnvName(name) => {
                write!(f, "forbidden env passthrough name `{name}`")
            }
            PlanError::BadTermId => write!(f, "term id must match [a-z0-9]{{1,16}}"),
            PlanError::Io(msg) => write!(f, "host IO failure: {msg}"),
            PlanError::HandoffUnsupported => {
                write!(f, "handoff_from requires harness claude or omp")
            }
            PlanError::HandoffWithResume => {
                write!(f, "handoff_from and resume_sid are mutually exclusive")
            }
        }
    }
}

impl std::error::Error for PlanError {}

/// A plain token (mirrors kb-core `is_plain_token`): non-empty, at most
/// 128 bytes, starts alphanumeric, then alphanumerics or `.` `_` `:` `-`.
pub(crate) fn is_plain_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.as_bytes()
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b':' | b'-'))
}

/// The exact argv kb-core's resume table prints, as a vector. Used by
/// the drift test and by `plan()` itself.
pub fn resume_tail(harness: &str, sid: &str, cwd: Option<&str>) -> Result<Vec<String>, PlanError> {
    table::resume_tail(harness, sid, cwd)
}

/// Build the launch plan. Only the omp overlay touches the filesystem
/// (written under `run_dir`, mode 0600); everything else is pure.
pub fn plan(req: &PlanRequest<'_>) -> Result<LaunchPlan, PlanError> {
    let row = table::row(req.harness).ok_or(PlanError::UnknownHarness)?;
    if req.term_id.is_empty()
        || req.term_id.len() > 16
        || !req
            .term_id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        return Err(PlanError::BadTermId);
    }
    if !req.cwd.starts_with('/') || req.cwd.chars().any(char::is_control) {
        return Err(PlanError::BadCwd);
    }
    if let Some(sid) = req.resume_sid {
        if !is_plain_token(sid) {
            return Err(PlanError::UnsafeToken("resume_sid"));
        }
    }
    if let Some(sid) = req.handoff_from {
        if !is_plain_token(sid) {
            return Err(PlanError::UnsafeToken("handoff_from"));
        }
        if !matches!(req.harness, "claude" | "omp") {
            return Err(PlanError::HandoffUnsupported);
        }
        if req.resume_sid.is_some() {
            return Err(PlanError::HandoffWithResume);
        }
    }
    if let Some(ulid) = req.job_ulid {
        if !is_plain_token(ulid) {
            return Err(PlanError::UnsafeToken("job_ulid"));
        }
    }
    for name in req.extra_passthrough {
        if !env::validate_passthrough(name) {
            return Err(PlanError::ForbiddenEnvName(name.clone()));
        }
    }
    // omp overlay (written only for omp; never read/write/stat the
    // user's global omp config).
    let overlay_path = if req.harness == "omp" {
        let path = req.run_dir.join("omp-overlay.yml");
        write_overlay(req.run_dir, &path)?;
        Some(path)
    } else {
        None
    };
    let mut out_env: BTreeMap<String, String> = env::filter_env(req.base_env).into_iter().collect();
    for name in req.extra_passthrough {
        if let Some(v) = env::lookup(req.base_env, name) {
            out_env.insert(name.clone(), v);
        }
    }
    // Per-harness profile env (verified knobs; profile wins over
    // passthrough).
    match req.harness {
        "claude" => {
            out_env.insert(
                "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN".to_string(),
                "1".to_string(),
            );
        }
        "omp" => {
            out_env.insert("PI_FORCE_IMAGE_PROTOCOL".to_string(), "kitty".to_string());
            let path = overlay_path
                .clone()
                .expect("omp plan always writes the overlay");
            out_env.insert(
                "PI_CONFIG_FILES".to_string(),
                path.to_string_lossy().into_owned(),
            );
        }
        _ => {}
    }
    out_env.insert("KB_TERM_ID".to_string(), req.term_id.to_string());
    if let Some(ulid) = req.job_ulid {
        out_env.insert("KB_JOB_ULID".to_string(), ulid.to_string());
    }
    let argv = match (req.handoff_from, req.resume_sid) {
        // Fixed tokens plus the validated sid, the closed harness name and
        // the validated absolute cwd; the pane PATH resolves bare `kb`.
        // The target harness's profile env/overlay/exit steps above and
        // below are unchanged: `kb` execs the harness, which inherits them.
        (Some(sid), _) => ["kb", "sessions", "handoff", sid, "--to", req.harness]
            .into_iter()
            .chain(["--transplant", "--launch", "--cwd", req.cwd])
            .map(str::to_string)
            .collect(),
        (None, Some(sid)) => {
            let tail = resume_tail(req.harness, sid, Some(req.cwd))?;
            let mut argv = vec![tail[0].clone()];
            argv.extend(row.fresh_flags.iter().map(|s| s.to_string()));
            argv.extend(tail[1..].iter().cloned());
            argv
        }
        (None, None) => {
            let mut argv = vec![row.bin.to_string()];
            argv.extend(row.fresh_flags.iter().map(|s| s.to_string()));
            argv
        }
    };
    Ok(LaunchPlan {
        argv,
        env: out_env.into_iter().collect(),
        unset_after_login: env::unset_after_login(req.base_env),
        overlay_path,
        experimental: row.experimental,
        exit_steps: row.exit_steps,
    })
}

/// Write the embedded omp overlay under `dir` with mode 0600. IO
/// failures (readonly run dir, full tmpfs) are `PlanError::Io` — host
/// state, not a bad client request.
fn write_overlay(dir: &Path, path: &Path) -> Result<(), PlanError> {
    use std::io::Write;
    let io = |e: std::io::Error| PlanError::Io(e.to_string());
    std::fs::create_dir_all(dir).map_err(io)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(io)?;
    file.write_all(OMP_OVERLAY.as_bytes()).map_err(io)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(io)?;
    Ok(())
}

use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;

#[cfg(test)]
mod handoff_tests {
    use super::*;

    fn base_env() -> Vec<(String, String)> {
        vec![
            ("HOME".into(), "/home/x".into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ]
    }

    fn request<'a>(
        harness: &'a str,
        run_dir: &'a Path,
        env: &'a [(String, String)],
        handoff_from: Option<&'a str>,
    ) -> PlanRequest<'a> {
        PlanRequest {
            harness,
            term_id: "abc123",
            resume_sid: None,
            handoff_from,
            cwd: "/w/x",
            job_ulid: None,
            run_dir,
            base_env: env,
            extra_passthrough: &[],
        }
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(ToString::to_string).collect()
    }

    /// claude and omp: the argv is exactly the fixed `kb sessions handoff`
    /// form, while env keys, overlay and exit steps equal a plain launch of
    /// that harness.
    #[test]
    fn handoff_plan_runs_kb_with_the_target_harness_profile() {
        let env = base_env();
        for harness in ["claude", "omp"] {
            let dir = tempfile::tempdir().unwrap();
            let plain = plan(&request(harness, dir.path(), &env, None)).unwrap();
            let handoff = plan(&request(harness, dir.path(), &env, Some("sid-1.a:b_c"))).unwrap();
            assert_eq!(
                handoff.argv,
                argv(&[
                    "kb",
                    "sessions",
                    "handoff",
                    "sid-1.a:b_c",
                    "--to",
                    harness,
                    "--transplant",
                    "--launch",
                    "--cwd",
                    "/w/x",
                ])
            );
            assert_eq!(handoff.env, plain.env, "{harness}: env keys/values");
            assert_eq!(
                handoff.overlay_path, plain.overlay_path,
                "{harness}: overlay"
            );
            assert_eq!(
                handoff.exit_steps, plain.exit_steps,
                "{harness}: exit steps"
            );
            assert_eq!(handoff.unset_after_login, plain.unset_after_login);
            assert_eq!(handoff.experimental, plain.experimental);
            assert!(handoff
                .env
                .contains(&("KB_TERM_ID".to_string(), "abc123".to_string())));
            for arg in &handoff.argv {
                assert!(!table::FORBIDDEN_ARGV.contains(&arg.as_str()), "{arg}");
            }
        }
        // omp: the private overlay is written and PI_CONFIG_FILES points at it.
        let dir = tempfile::tempdir().unwrap();
        let handoff = plan(&request("omp", dir.path(), &env, Some("sid-1"))).unwrap();
        let overlay = handoff.overlay_path.expect("omp overlay");
        assert!(overlay.is_file(), "overlay written");
        assert!(handoff.env.contains(&(
            "PI_CONFIG_FILES".to_string(),
            overlay.to_string_lossy().into_owned()
        )));
        // claude: its profile env, no overlay.
        let handoff = plan(&request("claude", dir.path(), &env, Some("sid-1"))).unwrap();
        assert!(handoff.overlay_path.is_none());
        assert!(handoff.env.contains(&(
            "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN".to_string(),
            "1".to_string()
        )));
    }

    /// kimi support was removed: a launch plan naming it is an unknown
    /// harness, with or without a handoff or resume sid.
    #[test]
    fn a_removed_kimi_harness_plans_as_unknown() {
        let env = base_env();
        let dir = tempfile::tempdir().unwrap();
        for sid in [None, Some("sid-1")] {
            assert_eq!(
                plan(&request("kimi", dir.path(), &env, sid)).unwrap_err(),
                PlanError::UnknownHarness
            );
        }
        assert_eq!(
            resume_tail("kimi", "sid-1", None).unwrap_err(),
            PlanError::UnknownHarness
        );
    }

    #[test]
    fn handoff_plan_rejects_unsupported_harnesses_resume_and_unsafe_sids() {
        let env = base_env();
        let dir = tempfile::tempdir().unwrap();
        for harness in ["codex", "opencode", "grok"] {
            assert_eq!(
                plan(&request(harness, dir.path(), &env, Some("sid-1"))).unwrap_err(),
                PlanError::HandoffUnsupported,
                "{harness}"
            );
        }
        let mut req = request("claude", dir.path(), &env, Some("sid-1"));
        req.resume_sid = Some("other");
        assert_eq!(plan(&req).unwrap_err(), PlanError::HandoffWithResume);
        let too_long = "s".repeat(129);
        for bad in ["a;b", "-x", "", "a b", too_long.as_str()] {
            assert_eq!(
                plan(&request("omp", dir.path(), &env, Some(bad))).unwrap_err(),
                PlanError::UnsafeToken("handoff_from"),
                "sid {bad:?}"
            );
        }
        // Exactly 128 bytes is still fine.
        let max = "s".repeat(128);
        assert!(plan(&request("omp", dir.path(), &env, Some(max.as_str()))).is_ok());
    }
}
