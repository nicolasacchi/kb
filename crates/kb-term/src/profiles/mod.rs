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
pub const HARNESSES: [&str; 6] = ["claude", "codex", "opencode", "grok", "kimi", "omp"];

/// The approved private omp overlay (never touch the user's global
/// config): image/recapture pins only.
pub const OMP_OVERLAY: &str = include_str!("../../share/omp/overlay.yml");

/// One step of a harness exit sequence (sent before the cgroup kill,
/// which is the backstop that never depends on these).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStep {
    Keys(&'static [&'static str]),
    Text(&'static str),
    WaitMs(u32),
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
    /// present in `base_env` (the wrapper unsets these after any login
    /// shell runs).
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
        "kimi" => {
            out_env.insert("PI_IMAGE_PROTOCOL".to_string(), "iterm2".to_string());
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
    let argv = match req.resume_sid {
        Some(sid) => {
            let tail = resume_tail(req.harness, sid, Some(req.cwd))?;
            let mut argv = vec![tail[0].clone()];
            argv.extend(row.fresh_flags.iter().map(|s| s.to_string()));
            argv.extend(tail[1..].iter().cloned());
            argv
        }
        None => {
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
