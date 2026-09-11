//! V76-R3b (D13 M2) — one worktree oracle, the loopback lifecycle verbs,
//! the readiness card that emits the fix, and the owner-oracle parse.
//!
//! **One oracle.** [`classify`] is the only function that answers "what is
//! this path?". `routes::repo_is_worktree` and
//! [`entities::worktree_key_for`](crate::entities::worktree_key_for) call
//! it; [`GitRepo::is_worktree`](crate::git::GitRepo::is_worktree) stays the
//! gix primitive (`git_dir != common_dir`) so a handle does not spawn git
//! or walk history, and a fixture with a main checkout, two linked
//! worktrees (one moved), a bare repo and a plain directory pins that the
//! three former answers agree with this one. `workspace::enumerate` takes
//! a linked worktree's id from it so the M1 table cannot drift.
//!
//! **Lifecycle is the working-tree lane.** Create / lock / unlock /
//! repair / prune / delete ride the SAME loopback-only sub-router
//! `checkout.rs` already uses. The daemon never provisions a worktree on
//! behalf of an agent beyond these verbs, and it never spawns anything
//! but git (invariant 10). Removal is ONLY for worktrees this daemon
//! recorded creating (`created_by_daemon`), behind a loss preview and an
//! explicit confirm.
//!
//! **Readiness detects, never fixes.** Each issue carries the exact
//! command to run. Holder × silence stay independent axes: the lock-reason
//! parse is `likely` at best and a parse failure renders
//! `"locked — owner unknown"`; lock age is not an input.

use std::path::{Path, PathBuf};
use std::process::Command;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::config::RepoEntry;
use crate::entities::RouteContract;
use crate::git::Revspec;
use crate::routes::ApiError;
use crate::state::SharedState;
use crate::store::StoreBlocking;
use crate::workspace::{self, WorktreeRow, MAIN_WORKTREE_ID};

// ── identity ─────────────────────────────────────────────────────────

/// What [`classify`] decided a path is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorktreeKind {
    Main,
    Linked,
    Bare,
    #[serde(rename = "not-a-repo")]
    NotARepo,
}

impl WorktreeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            WorktreeKind::Main => "main",
            WorktreeKind::Linked => "linked",
            WorktreeKind::Bare => "bare",
            WorktreeKind::NotARepo => "not-a-repo",
        }
    }
}

/// The one worktree identity. `workspace_id` is D13's fold over
/// (canonical common-dir, root commit); `worktree_id` is the admin-dir
/// name (or [`MAIN_WORKTREE_ID`] for main/bare). Path is deliberately
/// NOT here — it is a mutable attribute of the row, not of the identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorktreeIdentity {
    pub workspace_id: String,
    pub worktree_id: String,
    pub kind: WorktreeKind,
    pub common_dir: Option<PathBuf>,
    pub admin_dir: Option<PathBuf>,
}

impl WorktreeIdentity {
    fn not_a_repo() -> Self {
        Self {
            workspace_id: String::new(),
            worktree_id: String::new(),
            kind: WorktreeKind::NotARepo,
            common_dir: None,
            admin_dir: None,
        }
    }
}

/// Parse a linked worktree's `.git` *file* into the admin directory it
/// points at. Relative `gitdir:` values are resolved against the file's
/// parent (the checkout).
fn admin_dir_from_gitfile(git_file: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(git_file).ok()?;
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("gitdir:") else {
            continue;
        };
        let raw = rest.trim();
        if raw.is_empty() {
            continue;
        }
        let p = PathBuf::from(raw);
        let abs = if p.is_absolute() {
            p
        } else {
            git_file.parent()?.join(p)
        };
        return Some(std::fs::canonicalize(&abs).unwrap_or(abs));
    }
    None
}

fn looks_bare(path: &Path) -> bool {
    path.join("HEAD").is_file()
        && (path.join("objects").is_dir() || path.join("commondir").is_file())
        && !path.join(".git").exists()
}

fn worktree_id_from_admin(admin: &Path) -> String {
    admin
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "worktree".to_string())
}

/// The one oracle. Total: a missing path, a plain directory, a broken
/// `.git` file and an unreadable gitdir all become [`WorktreeKind::NotARepo`]
/// (or `linked` with a best-effort admin name) rather than an error a
/// caller has to branch on.
pub fn classify(repo_root: &Path) -> WorktreeIdentity {
    if !repo_root.exists() {
        return WorktreeIdentity::not_a_repo();
    }
    let git_file = repo_root.join(".git");
    if git_file.is_file() {
        let admin = admin_dir_from_gitfile(&git_file);
        let worktree_id = admin
            .as_ref()
            .map(|p| worktree_id_from_admin(p))
            .unwrap_or_else(|| {
                repo_root
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "worktree".to_string())
            });
        let common = workspace::common_dir(repo_root).ok();
        let workspace_id = match &common {
            Some(c) => {
                let root = workspace::root_commit(repo_root).ok().flatten();
                workspace::workspace_id(c, root.as_deref())
            }
            None => String::new(),
        };
        return WorktreeIdentity {
            workspace_id,
            worktree_id,
            kind: WorktreeKind::Linked,
            common_dir: common,
            admin_dir: admin,
        };
    }
    if git_file.is_dir() {
        let common = workspace::common_dir(repo_root)
            .ok()
            .or_else(|| std::fs::canonicalize(&git_file).ok());
        let workspace_id = match &common {
            Some(c) => {
                let root = workspace::root_commit(repo_root).ok().flatten();
                workspace::workspace_id(c, root.as_deref())
            }
            None => String::new(),
        };
        return WorktreeIdentity {
            workspace_id,
            worktree_id: MAIN_WORKTREE_ID.to_string(),
            kind: WorktreeKind::Main,
            common_dir: common,
            admin_dir: None,
        };
    }
    if looks_bare(repo_root) {
        let common = std::fs::canonicalize(repo_root).ok();
        let workspace_id = match &common {
            Some(c) => {
                let root = workspace::root_commit(repo_root).ok().flatten();
                workspace::workspace_id(c, root.as_deref())
            }
            None => String::new(),
        };
        return WorktreeIdentity {
            workspace_id,
            worktree_id: MAIN_WORKTREE_ID.to_string(),
            kind: WorktreeKind::Bare,
            common_dir: common,
            admin_dir: None,
        };
    }
    WorktreeIdentity::not_a_repo()
}

/// Linked-worktree key for `entity_defs.worktree`: the admin-dir name,
/// or `""` for main / bare / not-a-repo (the historical empty-string
/// sentinel `worktree_key_for` already stored).
pub fn linked_admin_name(repo_root: &Path) -> String {
    let ident = classify(repo_root);
    match ident.kind {
        WorktreeKind::Linked => ident.worktree_id,
        _ => String::new(),
    }
}

pub fn is_linked(repo_root: &Path) -> bool {
    classify(repo_root).kind == WorktreeKind::Linked
}

// ── owner oracle ─────────────────────────────────────────────────────

/// A lock-reason parse. Trust is `likely` at best — the reason is a
/// free-text git attribute, not a signed identity. A failure is
/// `"locked — owner unknown"`, never a guessed holder. Silence (how
/// long the lock has been held) is NOT an input: holder × silence stay
/// independent axes, same as the live-sessions cockpit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LockOwner {
    pub display: String,
    pub trust: &'static str,
    pub holder: Option<String>,
    pub session: Option<String>,
}

const OWNER_UNKNOWN: &str = "locked — owner unknown";
const HARNESSES: &[&str] = &[
    "claude", "codex", "grok", "kimi", "omp", "cursor", "opencode",
];

pub fn parse_lock_owner(reason: Option<&str>) -> LockOwner {
    let Some(raw) = reason.map(str::trim).filter(|s| !s.is_empty()) else {
        return LockOwner {
            display: OWNER_UNKNOWN.to_string(),
            trust: "unknown",
            holder: None,
            session: None,
        };
    };
    let lower = raw.to_ascii_lowercase();
    let session = extract_session(&lower, raw);
    let holder = extract_holder(&lower, raw);
    if holder.is_none() && session.is_none() {
        return LockOwner {
            display: OWNER_UNKNOWN.to_string(),
            trust: "unknown",
            holder: None,
            session: None,
        };
    }
    let display = match (&holder, &session) {
        (Some(h), Some(s)) => format!("likely: {h} session {s}"),
        (Some(h), None) => format!("likely: {h}"),
        (None, Some(s)) => format!("likely: session {s}"),
        (None, None) => OWNER_UNKNOWN.to_string(),
    };
    LockOwner {
        display,
        trust: "likely",
        holder,
        session,
    }
}

fn extract_session(lower: &str, raw: &str) -> Option<String> {
    for key in ["session ", "session:", "session=", "sid ", "sid:", "sid="] {
        if let Some(idx) = lower.find(key) {
            let rest = &raw[idx + key.len()..];
            let tok = rest
                .split(|c: char| c.is_ascii_whitespace() || c == ',' || c == ';')
                .find(|s| !s.is_empty())?;
            if tok.len() >= 4 {
                return Some(tok.to_string());
            }
        }
    }
    for h in HARNESSES {
        let prefix = format!("{h}/");
        if let Some(idx) = lower.find(&prefix) {
            let rest = &raw[idx + prefix.len()..];
            let tok = rest
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                .next()
                .unwrap_or("");
            if tok.len() >= 4 {
                return Some(tok.to_string());
            }
        }
    }
    None
}

fn extract_holder(lower: &str, raw: &str) -> Option<String> {
    for h in HARNESSES {
        if let Some(idx) = lower.find(h) {
            let before_ok = idx == 0
                || raw
                    .as_bytes()
                    .get(idx - 1)
                    .map(|b| !b.is_ascii_alphanumeric())
                    .unwrap_or(true);
            if before_ok {
                return Some((*h).to_string());
            }
        }
    }
    if let Some(idx) = lower.find("by ") {
        let rest = &raw[idx + 3..];
        let tok = rest
            .split(|c: char| c.is_ascii_whitespace() || c == ',' || c == ';')
            .find(|s| !s.is_empty())?;
        if tok.len() >= 2 {
            return Some(tok.to_ascii_lowercase());
        }
    }
    None
}

// ── errors / git ─────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("{0}")]
    Refused(String),
    #[error("failed to spawn git: {0}")]
    Spawn(std::io::Error),
    #[error("git {} failed: {stderr}", .args.join(" "))]
    GitFailed { args: Vec<String>, stderr: String },
    #[error("invalid revspec: {0:?}")]
    BadRevspec(String),
    #[error("{0}")]
    NotFound(String),
    #[error("kb-code store error: {0}")]
    Store(#[from] crate::store::StoreError),
    #[error("workspace error: {0}")]
    Workspace(#[from] workspace::WorkspaceError),
}

impl From<crate::git::RevspecError> for WorktreeError {
    fn from(e: crate::git::RevspecError) -> Self {
        WorktreeError::BadRevspec(e.0)
    }
}

impl From<WorktreeError> for ApiError {
    fn from(e: WorktreeError) -> Self {
        match e {
            WorktreeError::NotFound(m) => ApiError::not_found(m),
            WorktreeError::Refused(m) => ApiError::new(StatusCode::FORBIDDEN, m),
            WorktreeError::BadRevspec(s) => {
                ApiError::bad_request(format!("invalid revspec: {s:?}"))
            }
            WorktreeError::GitFailed { stderr, .. } => ApiError::bad_request(stderr),
            WorktreeError::Spawn(err) => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("spawn git: {err}"),
            ),
            WorktreeError::Store(err) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, format!("store: {err}"))
            }
            WorktreeError::Workspace(err) => ApiError::bad_request(err.to_string()),
        }
    }
}

fn run_git(repo_root: &Path, args: &[&str]) -> Result<String, WorktreeError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .map_err(WorktreeError::Spawn)?;
    if !out.status.success() {
        return Err(WorktreeError::GitFailed {
            args: args.iter().map(|s| s.to_string()).collect(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A git invocation whose last caller-supplied value is a PATH. `--` is
/// pushed immediately before it so a path that looks like an option
/// cannot be option-parsed (invariant 3).
fn run_git_path(
    repo_root: &Path,
    before: &[&str],
    path: &Path,
    after: &[&str],
) -> Result<String, WorktreeError> {
    let path_s = path.to_string_lossy();
    let mut args: Vec<&str> = Vec::with_capacity(before.len() + 2 + after.len());
    args.extend_from_slice(before);
    args.push("--");
    args.push(path_s.as_ref());
    args.extend_from_slice(after);
    run_git(repo_root, &args)
}

fn canonicalize_lenient(path: &Path) -> PathBuf {
    let mut cur = path.to_path_buf();
    let mut missing = Vec::new();
    loop {
        if let Ok(c) = std::fs::canonicalize(&cur) {
            let mut out = c;
            for part in missing.into_iter().rev() {
                out.push(part);
            }
            return out;
        }
        match cur.file_name() {
            Some(name) => {
                missing.push(name.to_os_string());
                match cur.parent() {
                    Some(p) if p != cur => cur = p.to_path_buf(),
                    _ => break,
                }
            }
            None => break,
        }
    }
    path.to_path_buf()
}

/// Create is allowed only under a configured `[[repos]]` root, or as a
/// sibling of one (the natural `git worktree add` layout). Anything else
/// is refused BY NAME, listing the roots that would have been accepted.
pub fn path_is_allowed(path: &Path, repos: &[RepoEntry]) -> Result<(), WorktreeError> {
    let target = canonicalize_lenient(path);
    let mut named: Vec<String> = Vec::new();
    for r in repos {
        let root = canonicalize_lenient(&r.path);
        named.push(root.display().to_string());
        if target == root || target.starts_with(&root) {
            return Ok(());
        }
        if let Some(parent) = root.parent() {
            named.push(parent.display().to_string());
            if target.parent() == Some(parent) {
                return Ok(());
            }
        }
    }
    Err(WorktreeError::Refused(format!(
        "path {} is outside every configured root ({})",
        path.display(),
        named.join(", ")
    )))
}

// ── readiness ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadinessIssue {
    pub code: &'static str,
    pub message: String,
    pub command: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Readiness {
    pub schema: &'static str,
    pub workspace_id: String,
    pub worktree_id: String,
    pub ready: bool,
    pub issues: Vec<ReadinessIssue>,
    pub owner: Option<LockOwner>,
}

pub const READINESS_SCHEMA: &str = "kbc-worktree-readiness/1";

/// Detect unreadiness. Never mutates. `indexed_files` is `Some` when this
/// worktree is a mounted `[[repos]]` entry (so a zero means the mirror has
/// not landed yet) and `None` when the question does not apply.
pub fn detect_readiness(row: &WorktreeRow, indexed_files: Option<u64>) -> Vec<ReadinessIssue> {
    let mut issues = Vec::new();
    let path = row.path.as_deref().map(Path::new);

    if let Some(p) = path {
        if p.exists() && !p.join(".git").exists() && !row.bare {
            issues.push(ReadinessIssue {
                code: "missing-git-link",
                message: "the checkout has no .git file or directory".into(),
                command: format!("git worktree repair -- {}", p.display()),
            });
        }
        if row.path.as_deref().is_some() && !row.is_main && !row.bare {
            let ident = classify(p);
            if ident.kind == WorktreeKind::Linked {
                match &ident.admin_dir {
                    Some(admin) if !admin.exists() => {
                        issues.push(ReadinessIssue {
                            code: "stale-admin-dir",
                            message: format!("admin dir {} is missing", admin.display()),
                            command: format!("git worktree repair -- {}", p.display()),
                        });
                    }
                    Some(admin) => {
                        let gitdir = admin.join("gitdir");
                        if let Ok(recorded) = std::fs::read_to_string(&gitdir) {
                            let recorded = recorded.trim();
                            let expected = p.join(".git");
                            let expected_s = expected.to_string_lossy();
                            if !recorded.is_empty()
                                && recorded != expected_s.as_ref()
                                && std::fs::canonicalize(recorded).ok()
                                    != std::fs::canonicalize(&expected).ok()
                            {
                                issues.push(ReadinessIssue {
                                    code: "stale-admin-dir",
                                    message: format!(
                                        "admin gitdir records {recorded}, checkout is at {}",
                                        p.display()
                                    ),
                                    command: format!("git worktree repair -- {}", p.display()),
                                });
                            }
                        }
                    }
                    None => {}
                }
            }
        }
    } else if !row.prunable {
        issues.push(ReadinessIssue {
            code: "missing-git-link",
            message: "git reported no path for this worktree".into(),
            command: "git worktree list --porcelain".into(),
        });
    }

    if row.detached && !row.bare {
        let switch = row
            .branch
            .as_deref()
            .map(|b| format!("git switch {b}"))
            .unwrap_or_else(|| "git switch <branch>".to_string());
        issues.push(ReadinessIssue {
            code: "detached-head",
            message: "HEAD is detached".into(),
            command: switch,
        });
    }

    if row.locked
        && row
            .lock_reason
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
    {
        let target = row.path.as_deref().unwrap_or(row.id.as_str());
        issues.push(ReadinessIssue {
            code: "lock-without-reason",
            message: "locked with no reason — owner unknown".into(),
            command: format!("git worktree lock --reason=\"<why>\" -- {target}"),
        });
    }

    if let Some(p) = path.filter(|p| p.exists()) {
        if !row.bare {
            if let Ok(n) = behind_upstream(p) {
                if n > 0 {
                    issues.push(ReadinessIssue {
                        code: "branch-behind",
                        message: format!("branch is {n} commit(s) behind its upstream"),
                        command: "git merge --ff-only @{upstream}".into(),
                    });
                }
            }
            if let Ok(dirty) = crate::checkout::dirty_paths(p) {
                if !dirty.is_empty() {
                    issues.push(ReadinessIssue {
                        code: "uncommitted",
                        message: format!("{} uncommitted path(s)", dirty.len()),
                        command: "git status --porcelain".into(),
                    });
                }
            }
        }
    }

    if row.mounted {
        if let Some(0) = indexed_files {
            issues.push(ReadinessIssue {
                code: "not-indexed",
                message: "the live mirror has not indexed this checkout yet".into(),
                command: "GET /api/repos (wait for file_count > 0) or restart kb-code-server"
                    .into(),
            });
        }
    }

    issues
}

/// `HEAD` vs `@{upstream}` — daemon-minted argv, not a caller-supplied
/// revspec (`Revspec` refuses `@{` by design). `None`/`Err` means there
/// is no upstream, which is not unreadiness.
fn behind_upstream(repo_root: &Path) -> Result<u64, WorktreeError> {
    let out = run_git(repo_root, &["rev-list", "--count", "HEAD..@{upstream}"])?;
    Ok(out.trim().parse().unwrap_or(0))
}

fn indexed_files_for(state: &SharedState, row: &WorktreeRow) -> Option<u64> {
    let repo = row.repo.as_deref()?;
    let repo_id = *state.repo_ids.get(repo)?;
    state.store.file_count(repo_id).ok()
}

// ── loss preview ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct LossPreview {
    pub schema: &'static str,
    pub workspace_id: String,
    pub worktree_id: String,
    pub path: Option<String>,
    pub created_by_daemon: bool,
    pub uncommitted: Vec<String>,
    pub unpushed: Vec<String>,
    pub stashes: Vec<String>,
    pub manual_remove: String,
}

pub const LOSS_PREVIEW_SCHEMA: &str = "kbc-worktree-loss/1";

pub fn loss_preview(row: &WorktreeRow) -> LossPreview {
    let path = row.path.as_deref().map(Path::new);
    let uncommitted = path
        .and_then(|p| crate::checkout::dirty_paths(p).ok())
        .unwrap_or_default();
    let unpushed = path
        .and_then(|p| unpushed_commits(p).ok())
        .unwrap_or_default();
    let stashes = path.and_then(|p| stash_list(p).ok()).unwrap_or_default();
    let manual = match row.path.as_deref() {
        Some(p) => format!("git worktree remove -- {p}"),
        None => format!("git worktree remove -- {}", row.id),
    };
    LossPreview {
        schema: LOSS_PREVIEW_SCHEMA,
        workspace_id: row.workspace_id.clone(),
        worktree_id: row.id.clone(),
        path: row.path.clone(),
        created_by_daemon: row.created_by_daemon,
        uncommitted,
        unpushed,
        stashes,
        manual_remove: manual,
    }
}

fn unpushed_commits(repo_root: &Path) -> Result<Vec<String>, WorktreeError> {
    let out = match run_git(
        repo_root,
        &["log", "--oneline", "--decorate=no", "@{upstream}..HEAD"],
    ) {
        Ok(o) => o,
        Err(_) => return Ok(Vec::new()),
    };
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

fn stash_list(repo_root: &Path) -> Result<Vec<String>, WorktreeError> {
    let out = run_git(repo_root, &["stash", "list"])?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

// ── lifecycle ────────────────────────────────────────────────────────

fn main_checkout_path(
    store: &crate::store::Store,
    workspace_id: &str,
) -> Result<PathBuf, WorktreeError> {
    let rows = store.worktrees_for_workspace(workspace_id)?;
    rows.iter()
        .find(|w| w.is_main)
        .and_then(|w| w.path.as_deref())
        .map(PathBuf::from)
        .ok_or_else(|| {
            WorktreeError::NotFound(format!(
                "workspace {workspace_id} has no main worktree path"
            ))
        })
}

fn refresh_workspace(
    store: &crate::store::Store,
    repos: &[RepoEntry],
    workspace_id: &str,
    main_path: &Path,
) -> Result<Vec<WorktreeRow>, WorktreeError> {
    let rows = workspace::enumerate(main_path, workspace_id, repos)?;
    let now = chrono::Utc::now().timestamp();
    store.replace_worktrees(workspace_id, &rows, now)?;
    Ok(store.worktrees_for_workspace(workspace_id)?)
}

pub fn create_worktree(
    store: &crate::store::Store,
    repos: &[RepoEntry],
    workspace_id: &str,
    path: &Path,
    branch: Option<&Revspec>,
    new_branch: Option<&Revspec>,
) -> Result<WorktreeRow, WorktreeError> {
    path_is_allowed(path, repos)?;
    if path.exists() {
        return Err(WorktreeError::Refused(format!(
            "path {} already exists",
            path.display()
        )));
    }
    match (branch, new_branch) {
        (Some(_), Some(_)) => {
            return Err(WorktreeError::Refused(
                "pass branch or new_branch, not both".into(),
            ));
        }
        (None, None) => {
            return Err(WorktreeError::Refused(
                "pass branch (existing) or new_branch (create)".into(),
            ));
        }
        _ => {}
    }
    let main = main_checkout_path(store, workspace_id)?;
    if let Some(nb) = new_branch {
        run_git_path(&main, &["worktree", "add", "-b", nb.as_str()], path, &[])?;
    } else if let Some(b) = branch {
        run_git_path(&main, &["worktree", "add"], path, &[b.as_str()])?;
    }
    let _ = refresh_workspace(store, repos, workspace_id, &main)?;
    let ident = classify(path);
    let id = if ident.kind == WorktreeKind::Linked {
        ident.worktree_id
    } else {
        path.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "worktree".to_string())
    };
    store.mark_worktree_created_by_daemon(workspace_id, &id)?;
    let rows = store.worktrees_for_workspace(workspace_id)?;
    if let Some(w) = rows.iter().find(|w| w.id == id) {
        return Ok(w.clone());
    }
    if let Some(w) = rows_find_by_path(&rows, path) {
        store.mark_worktree_created_by_daemon(workspace_id, &w.id)?;
        let mut w = w;
        w.created_by_daemon = true;
        return Ok(w);
    }
    Err(WorktreeError::NotFound(format!(
        "created worktree {id} not in the table"
    )))
}

fn rows_find_by_path(rows: &[WorktreeRow], path: &Path) -> Option<WorktreeRow> {
    let want = canonicalize_lenient(path);
    rows.iter()
        .find(|w| {
            w.path
                .as_deref()
                .map(|p| canonicalize_lenient(Path::new(p)) == want)
                .unwrap_or(false)
        })
        .cloned()
}

pub fn lock_worktree(
    store: &crate::store::Store,
    repos: &[RepoEntry],
    row: &WorktreeRow,
    reason: Option<&str>,
) -> Result<WorktreeRow, WorktreeError> {
    let main = main_checkout_path(store, &row.workspace_id)?;
    let path = row
        .path
        .as_deref()
        .ok_or_else(|| WorktreeError::Refused("worktree has no path to lock".into()))?;
    let path = Path::new(path);
    match reason.map(str::trim).filter(|s| !s.is_empty()) {
        Some(r) => {
            let flag = format!("--reason={r}");
            run_git_path(&main, &["worktree", "lock", &flag], path, &[])?;
        }
        None => {
            run_git_path(&main, &["worktree", "lock"], path, &[])?;
        }
    }
    let rows = refresh_workspace(store, repos, &row.workspace_id, &main)?;
    rows.into_iter()
        .find(|w| w.id == row.id)
        .ok_or_else(|| WorktreeError::NotFound(format!("worktree {} vanished", row.id)))
}

pub fn unlock_worktree(
    store: &crate::store::Store,
    repos: &[RepoEntry],
    row: &WorktreeRow,
) -> Result<WorktreeRow, WorktreeError> {
    let main = main_checkout_path(store, &row.workspace_id)?;
    let path = row
        .path
        .as_deref()
        .ok_or_else(|| WorktreeError::Refused("worktree has no path to unlock".into()))?;
    run_git_path(&main, &["worktree", "unlock"], Path::new(path), &[])?;
    let rows = refresh_workspace(store, repos, &row.workspace_id, &main)?;
    rows.into_iter()
        .find(|w| w.id == row.id)
        .ok_or_else(|| WorktreeError::NotFound(format!("worktree {} vanished", row.id)))
}

pub fn repair_worktree(
    store: &crate::store::Store,
    repos: &[RepoEntry],
    row: &WorktreeRow,
    new_path: Option<&Path>,
) -> Result<WorktreeRow, WorktreeError> {
    let main = main_checkout_path(store, &row.workspace_id)?;
    match new_path {
        Some(p) => {
            path_is_allowed(p, repos)?;
            run_git_path(&main, &["worktree", "repair"], p, &[])?;
        }
        None => {
            run_git(&main, &["worktree", "repair"])?;
        }
    }
    let rows = refresh_workspace(store, repos, &row.workspace_id, &main)?;
    rows.into_iter()
        .find(|w| w.id == row.id)
        .ok_or_else(|| WorktreeError::NotFound(format!("worktree {} vanished", row.id)))
}

#[derive(Debug, Clone, Serialize)]
pub struct PruneResult {
    pub schema: &'static str,
    pub dry_run: bool,
    pub workspaces: Vec<PruneWorkspace>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PruneWorkspace {
    pub workspace_id: String,
    pub output: String,
    pub prunable: Vec<WorktreeRow>,
}

pub const PRUNE_SCHEMA: &str = "kbc-worktree-prune/1";

pub fn prune_worktrees(
    store: &crate::store::Store,
    repos: &[RepoEntry],
    dry_run: bool,
) -> Result<PruneResult, WorktreeError> {
    let workspaces = store.list_workspaces()?;
    let mut out = Vec::new();
    for ws in workspaces {
        let Ok(main) = main_checkout_path(store, &ws.id) else {
            continue;
        };
        let args: &[&str] = if dry_run {
            &["worktree", "prune", "--verbose", "--dry-run"]
        } else {
            &["worktree", "prune", "--verbose"]
        };
        let output = run_git(&main, args).unwrap_or_default();
        let rows = refresh_workspace(store, repos, &ws.id, &main)?;
        let prunable = rows.into_iter().filter(|w| w.prunable).collect();
        out.push(PruneWorkspace {
            workspace_id: ws.id,
            output,
            prunable,
        });
    }
    Ok(PruneResult {
        schema: PRUNE_SCHEMA,
        dry_run,
        workspaces: out,
    })
}

pub fn remove_worktree(
    store: &crate::store::Store,
    repos: &[RepoEntry],
    row: &WorktreeRow,
    confirm: &str,
    preview_seen: bool,
) -> Result<LossPreview, WorktreeError> {
    if row.is_main {
        return Err(WorktreeError::Refused(
            "the main worktree cannot be removed through the daemon".into(),
        ));
    }
    if !row.created_by_daemon {
        let preview = loss_preview(row);
        return Err(WorktreeError::Refused(format!(
            "worktree {} was not created by this daemon — run {} yourself",
            row.id, preview.manual_remove
        )));
    }
    if !preview_seen {
        return Err(WorktreeError::Refused(format!(
            "read GET /api/worktrees/{}/loss-preview?workspace_id={} first, then retry with preview_seen=true",
            row.id, row.workspace_id
        )));
    }
    if confirm != row.id {
        return Err(WorktreeError::Refused(format!(
            "confirm must equal the worktree id ({})",
            row.id
        )));
    }
    let preview = loss_preview(row);
    let main = main_checkout_path(store, &row.workspace_id)?;
    let path = row
        .path
        .as_deref()
        .ok_or_else(|| WorktreeError::Refused("worktree has no path to remove".into()))?;
    run_git_path(&main, &["worktree", "remove"], Path::new(path), &[])?;
    let _ = refresh_workspace(store, repos, &row.workspace_id, &main)?;
    Ok(preview)
}

// ── lookup ───────────────────────────────────────────────────────────

fn lookup_row(
    store: &crate::store::Store,
    workspace_id: Option<&str>,
    id: &str,
) -> Result<WorktreeRow, WorktreeError> {
    if let Some(ws) = workspace_id {
        return store
            .worktree_by_pk(ws, id)?
            .ok_or_else(|| WorktreeError::NotFound(format!("no such worktree {id} in {ws}")));
    }
    let matches = store.worktrees_with_id(id)?;
    match matches.len() {
        0 => Err(WorktreeError::NotFound(format!("no such worktree {id}"))),
        1 => Ok(matches.into_iter().next().unwrap()),
        n => Err(WorktreeError::Refused(format!(
            "worktree id {id:?} is ambiguous across {n} workspaces — pass workspace_id"
        ))),
    }
}

// ── inbox ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct InboxItem {
    pub kind: &'static str,
    pub workspace_id: String,
    pub worktree_id: String,
    pub path: Option<String>,
    pub summary: String,
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<LockOwner>,
}

/// Surfaced-never-scored worktrees lane. Degrades honestly when the
/// workspace table is empty (re-key pending, or a daemon with no
/// resolved repos yet) rather than pretending there is nothing to see.
pub fn compose_inbox_lane(state: &SharedState) -> serde_json::Value {
    let workspaces = match state.store.list_workspaces() {
        Ok(w) => w,
        Err(e) => {
            return serde_json::json!({
                "available": false,
                "reason": "store-error",
                "detail": e.to_string(),
                "items": [],
                "truncated": false,
            });
        }
    };
    if workspaces.is_empty() {
        return serde_json::json!({
            "available": false,
            "reason": "empty-table",
            "items": [],
            "truncated": false,
        });
    }
    let open_reviews = state.store.list_open_reviews().unwrap_or_default();
    let mut items: Vec<InboxItem> = Vec::new();
    for ws in &workspaces {
        let rows = match state.store.worktrees_for_workspace(&ws.id) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for row in rows {
            if row.locked
                && row
                    .lock_reason
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or("")
                    .is_empty()
            {
                let owner = parse_lock_owner(row.lock_reason.as_deref());
                items.push(InboxItem {
                    kind: "locked-without-reason",
                    workspace_id: row.workspace_id.clone(),
                    worktree_id: row.id.clone(),
                    path: row.path.clone(),
                    summary: owner.display.clone(),
                    command: Some(format!(
                        "git worktree lock --reason=\"<why>\" -- {}",
                        row.path.as_deref().unwrap_or(&row.id)
                    )),
                    review_id: None,
                    owner: Some(owner),
                });
            }
            if row.prunable {
                items.push(InboxItem {
                    kind: "prunable",
                    workspace_id: row.workspace_id.clone(),
                    worktree_id: row.id.clone(),
                    path: row.path.clone(),
                    summary: row
                        .prunable_reason
                        .clone()
                        .unwrap_or_else(|| "prunable".into()),
                    command: Some("git worktree prune --dry-run".into()),
                    review_id: None,
                    owner: None,
                });
            }
            let indexed = indexed_files_for(state, &row);
            for issue in detect_readiness(&row, indexed) {
                if issue.code == "lock-without-reason" {
                    continue;
                }
                items.push(InboxItem {
                    kind: "unready",
                    workspace_id: row.workspace_id.clone(),
                    worktree_id: row.id.clone(),
                    path: row.path.clone(),
                    summary: format!("{}: {}", issue.code, issue.message),
                    command: Some(issue.command),
                    review_id: None,
                    owner: None,
                });
            }
            if !row.is_main {
                if let Some(branch) = row.branch.as_deref() {
                    for rev in &open_reviews {
                        if rev.head_ref == branch
                            || rev.head_ref == format!("refs/heads/{branch}")
                            || rev.head_ref.ends_with(&format!("/{branch}"))
                        {
                            items.push(InboxItem {
                                kind: "open-review",
                                workspace_id: row.workspace_id.clone(),
                                worktree_id: row.id.clone(),
                                path: row.path.clone(),
                                summary: format!(
                                    "linked worktree on {branch} has open review {}",
                                    rev.id
                                ),
                                command: None,
                                review_id: Some(rev.id),
                                owner: None,
                            });
                        }
                    }
                }
            }
        }
    }
    let truncated = items.len() > crate::unified_inbox::LANE_CAP;
    items.truncate(crate::unified_inbox::LANE_CAP);
    serde_json::json!({
        "available": true,
        "reason": null,
        "items": items,
        "truncated": truncated,
    })
}

// ── routes ───────────────────────────────────────────────────────────

pub const SCHEMA: &str = "kbc-worktree/1";

pub const WORKTREES_LIST_ROUTE: RouteContract = RouteContract {
    path: "/api/worktrees",
    handler: "worktrees::list_route",
    required_params: &[],
    params_accept_without: |_| true,
};

/// Query-param GETs this unit adds. Path-param reads (`/{id}/readiness`,
/// `/{id}/loss-preview`) and the loopback mutations are absent: a
/// `RouteContract` describes a query-param surface, same reason
/// `workspace::V75_M1_ROUTES` omits `GET /api/workspaces/{id}/worktrees`.
pub const V76_R3B_ROUTES: &[RouteContract] = &[WORKTREES_LIST_ROUTE];

#[derive(Debug, Deserialize)]
pub struct WorkspaceQuery {
    pub workspace_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateBody {
    pub workspace_id: String,
    pub path: String,
    pub branch: Option<String>,
    pub new_branch: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LockBody {
    pub workspace_id: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UnlockBody {
    pub workspace_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RepairBody {
    pub workspace_id: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DeleteBody {
    pub workspace_id: Option<String>,
    pub confirm: String,
    pub preview_seen: bool,
}

#[derive(Debug, Deserialize)]
pub struct PruneQuery {
    pub dry_run: Option<String>,
}

fn query_flag(v: Option<&str>) -> Option<bool> {
    v.map(|s| matches!(s, "1" | "true"))
}

fn json_row(row: &WorktreeRow) -> serde_json::Value {
    let owner = if row.locked {
        Some(parse_lock_owner(row.lock_reason.as_deref()))
    } else {
        None
    };
    serde_json::json!({
        "workspace_id": row.workspace_id,
        "id": row.id,
        "path": row.path,
        "branch": row.branch,
        "head_sha": row.head_sha,
        "is_main": row.is_main,
        "bare": row.bare,
        "detached": row.detached,
        "locked": row.locked,
        "lock_reason": row.lock_reason,
        "owner": owner,
        "prunable": row.prunable,
        "prunable_reason": row.prunable_reason,
        "mounted": row.mounted,
        "path_resolution": row.path_resolution,
        "repo": row.repo,
        "created_by_daemon": row.created_by_daemon,
    })
}

pub async fn list_route(
    State(state): State<SharedState>,
    Query(q): Query<WorkspaceQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let wanted = q.workspace_id.clone();
    let rows = state
        .store
        .run_blocking(move |store| {
            if let Some(ws) = wanted {
                store.worktrees_for_workspace(&ws)
            } else {
                store.list_all_worktrees()
            }
        })
        .await?;
    Ok(Json(serde_json::json!({
        "schema": SCHEMA,
        "worktrees": rows.iter().map(json_row).collect::<Vec<_>>(),
    })))
}

pub async fn readiness_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<WorkspaceQuery>,
) -> Result<Json<Readiness>, ApiError> {
    let ws = q.workspace_id.clone();
    let id_c = id.clone();
    let repo_ids = state.repo_ids.clone();
    let row = state
        .store
        .run_blocking(move |store| {
            let row = lookup_row(store, ws.as_deref(), &id_c)?;
            let indexed = row.repo.as_deref().and_then(|name| {
                let repo_id = *repo_ids.get(name)?;
                store.file_count(repo_id).ok()
            });
            Ok::<_, WorktreeError>((row, indexed))
        })
        .await
        .map_err(ApiError::from)?;
    let (row, indexed) = row;
    let issues = detect_readiness(&row, indexed);
    let owner = if row.locked {
        Some(parse_lock_owner(row.lock_reason.as_deref()))
    } else {
        None
    };
    Ok(Json(Readiness {
        schema: READINESS_SCHEMA,
        workspace_id: row.workspace_id,
        worktree_id: row.id,
        ready: issues.is_empty(),
        issues,
        owner,
    }))
}

pub async fn loss_preview_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<String>,
    Query(q): Query<WorkspaceQuery>,
) -> Result<Json<LossPreview>, ApiError> {
    let ws = q.workspace_id.clone();
    let id_c = id.clone();
    let row = state
        .store
        .run_blocking(move |store| lookup_row(store, ws.as_deref(), &id_c))
        .await
        .map_err(ApiError::from)?;
    Ok(Json(loss_preview(&row)))
}

pub async fn create_route(
    State(state): State<SharedState>,
    Json(body): Json<CreateBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let branch = body
        .branch
        .as_deref()
        .map(Revspec::parse)
        .transpose()
        .map_err(WorktreeError::from)?;
    let new_branch = body
        .new_branch
        .as_deref()
        .map(Revspec::parse)
        .transpose()
        .map_err(WorktreeError::from)?;
    let path = PathBuf::from(&body.path);
    let ws = body.workspace_id.clone();
    let repos = state.repos.clone();
    let row = state
        .store
        .run_blocking(move |store| {
            create_worktree(
                store,
                &repos,
                &ws,
                &path,
                branch.as_ref(),
                new_branch.as_ref(),
            )
        })
        .await
        .map_err(ApiError::from)?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "schema": SCHEMA,
            "before": null,
            "after": json_row(&row),
        })),
    ))
}

pub async fn lock_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<LockBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ws = body.workspace_id.clone();
    let id_c = id.clone();
    let reason = body.reason.clone();
    let repos = state.repos.clone();
    let (before, after) = state
        .store
        .run_blocking(move |store| {
            let row = lookup_row(store, ws.as_deref(), &id_c)?;
            let before = json_row(&row);
            let after = lock_worktree(store, &repos, &row, reason.as_deref())?;
            Ok::<_, WorktreeError>((before, json_row(&after)))
        })
        .await
        .map_err(ApiError::from)?;
    Ok(Json(serde_json::json!({
        "schema": SCHEMA,
        "before": before,
        "after": after,
    })))
}

pub async fn unlock_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<UnlockBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ws = body.workspace_id.clone();
    let id_c = id.clone();
    let repos = state.repos.clone();
    let (before, after) = state
        .store
        .run_blocking(move |store| {
            let row = lookup_row(store, ws.as_deref(), &id_c)?;
            let before = json_row(&row);
            let after = unlock_worktree(store, &repos, &row)?;
            Ok::<_, WorktreeError>((before, json_row(&after)))
        })
        .await
        .map_err(ApiError::from)?;
    Ok(Json(serde_json::json!({
        "schema": SCHEMA,
        "before": before,
        "after": after,
    })))
}

pub async fn repair_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<RepairBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ws = body.workspace_id.clone();
    let id_c = id.clone();
    let new_path = body.path.clone();
    let repos = state.repos.clone();
    let (before, after) = state
        .store
        .run_blocking(move |store| {
            let row = lookup_row(store, ws.as_deref(), &id_c)?;
            let before = json_row(&row);
            let path = new_path.as_deref().map(Path::new);
            let after = repair_worktree(store, &repos, &row, path)?;
            Ok::<_, WorktreeError>((before, json_row(&after)))
        })
        .await
        .map_err(ApiError::from)?;
    Ok(Json(serde_json::json!({
        "schema": SCHEMA,
        "before": before,
        "after": after,
    })))
}

pub async fn prune_route(
    State(state): State<SharedState>,
    Query(q): Query<PruneQuery>,
) -> Result<Json<PruneResult>, ApiError> {
    let dry_run = query_flag(q.dry_run.as_deref()).unwrap_or(true);
    let repos = state.repos.clone();
    let result = state
        .store
        .run_blocking(move |store| prune_worktrees(store, &repos, dry_run))
        .await
        .map_err(ApiError::from)?;
    Ok(Json(result))
}

pub async fn delete_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<DeleteBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ws = body.workspace_id.clone();
    let id_c = id.clone();
    let confirm = body.confirm.clone();
    let preview_seen = body.preview_seen;
    let repos = state.repos.clone();
    let preview = state
        .store
        .run_blocking(move |store| {
            let row = lookup_row(store, ws.as_deref(), &id_c)?;
            let before = loss_preview(&row);
            let after = remove_worktree(store, &repos, &row, &confirm, preview_seen)?;
            Ok::<_, WorktreeError>((before, after))
        })
        .await
        .map_err(ApiError::from)?;
    Ok(Json(serde_json::json!({
        "schema": SCHEMA,
        "before": preview.0,
        "after": null,
        "removed": preview.1,
    })))
}

// ── tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RepoEntry;
    use crate::git::GitRepo;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git -C {} {:?} failed: {}",
            dir.display(),
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
    }

    /// Main checkout + two linked worktrees (one then moved with `mv`),
    /// a bare clone, and a plain directory. The three former oracles
    /// must agree with [`classify`] on every one of them.
    fn fixture() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        init_repo(&main);
        std::fs::write(main.join("README.md"), "root\n").unwrap();
        git(&main, &["add", "-A"]);
        git(&main, &["commit", "-q", "-m", "root"]);

        let wt_a = tmp.path().join("wt-a");
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "branch-a",
                wt_a.to_str().unwrap(),
            ],
        );

        let wt_b = tmp.path().join("wt-b");
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "branch-b",
                wt_b.to_str().unwrap(),
            ],
        );
        let wt_moved = tmp.path().join("wt-moved");
        std::fs::rename(&wt_b, &wt_moved).unwrap();

        let bare = tmp.path().join("bare.git");
        git(
            tmp.path(),
            &[
                "clone",
                "-q",
                "--bare",
                main.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        );

        std::fs::create_dir_all(tmp.path().join("plain")).unwrap();
        tmp
    }

    /// The three M1 implementations, inlined so this test cannot silently
    /// agree with itself after those call sites are migrated onto
    /// [`classify`].
    fn former_oracles(path: &Path) -> (bool, Option<bool>, String) {
        let stat_is_file = path.join(".git").is_file();
        let gix_repo = GitRepo::open(path).ok();
        let gix = gix_repo.as_ref().map(|g| g.git_dir() != g.common_dir());
        let key = gix_repo
            .filter(|g| g.git_dir() != g.common_dir())
            .and_then(|g| {
                g.git_dir()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default();
        (stat_is_file, gix, key)
    }

    #[test]
    fn classify_agrees_with_the_three_former_oracles_on_the_fixture() {
        let fx = fixture();
        let main = fx.path().join("main");
        let wt_a = fx.path().join("wt-a");
        let wt_moved = fx.path().join("wt-moved");
        let bare = fx.path().join("bare.git");
        let plain = fx.path().join("plain");

        let c_main = classify(&main);
        assert_eq!(c_main.kind, WorktreeKind::Main);
        assert_eq!(c_main.worktree_id, MAIN_WORKTREE_ID);
        assert!(c_main.admin_dir.is_none());
        assert!(c_main.workspace_id.starts_with("ws_"));
        let (stat, gix, key) = former_oracles(&main);
        assert!(!stat, "main .git is a directory");
        assert_eq!(gix, Some(false));
        assert_eq!(key, "");
        assert_eq!(is_linked(&main), stat);
        assert_eq!(is_linked(&main), gix.unwrap());
        assert_eq!(linked_admin_name(&main), key);

        let c_a = classify(&wt_a);
        assert_eq!(c_a.kind, WorktreeKind::Linked);
        assert_eq!(c_a.worktree_id, "wt-a");
        assert!(c_a.admin_dir.is_some());
        assert_eq!(c_a.workspace_id, c_main.workspace_id);
        let (stat, gix, key) = former_oracles(&wt_a);
        assert!(stat);
        assert_eq!(gix, Some(true));
        assert_eq!(key, "wt-a");
        assert_eq!(is_linked(&wt_a), stat);
        assert_eq!(is_linked(&wt_a), gix.unwrap());
        assert_eq!(linked_admin_name(&wt_a), key);

        let c_moved = classify(&wt_moved);
        assert_eq!(
            c_moved.kind,
            WorktreeKind::Linked,
            "a moved linked worktree keeps its identity"
        );
        assert_eq!(
            c_moved.worktree_id, "wt-b",
            "admin-dir name, not the new basename"
        );
        assert_eq!(c_moved.workspace_id, c_main.workspace_id);
        let (stat, gix, key) = former_oracles(&wt_moved);
        assert!(stat);
        assert_eq!(gix, Some(true));
        assert_eq!(key, "wt-b");
        assert_eq!(linked_admin_name(&wt_moved), key);

        let c_bare = classify(&bare);
        assert_eq!(c_bare.kind, WorktreeKind::Bare);
        assert_eq!(c_bare.worktree_id, MAIN_WORKTREE_ID);
        let (stat, gix, key) = former_oracles(&bare);
        assert!(!stat);
        assert_eq!(gix, Some(false), "bare is not a linked worktree");
        assert_eq!(key, "");

        let c_plain = classify(&plain);
        assert_eq!(c_plain.kind, WorktreeKind::NotARepo);
        assert!(c_plain.workspace_id.is_empty());
        let (stat, gix, key) = former_oracles(&plain);
        assert!(!stat);
        assert!(gix.is_none() || gix == Some(false));
        assert_eq!(key, "");
        assert!(!is_linked(&plain));
    }

    #[test]
    fn lock_reason_parse_is_likely_at_best_and_does_not_take_silence() {
        let unknown = parse_lock_owner(None);
        assert_eq!(unknown.display, OWNER_UNKNOWN);
        assert_eq!(unknown.trust, "unknown");

        let empty = parse_lock_owner(Some("   "));
        assert_eq!(empty.display, OWNER_UNKNOWN);

        let gibberish = parse_lock_owner(Some("do not touch"));
        assert_eq!(gibberish.display, OWNER_UNKNOWN);
        assert_eq!(gibberish.trust, "unknown");

        let claude = parse_lock_owner(Some("being reformatted by claude, session 55b9abcd"));
        assert_eq!(claude.trust, "likely");
        assert_eq!(claude.holder.as_deref(), Some("claude"));
        assert_eq!(claude.session.as_deref(), Some("55b9abcd"));
        assert!(claude.display.contains("likely"));

        let slash = parse_lock_owner(Some("grok/01M28NZNWN65"));
        assert_eq!(slash.trust, "likely");
        assert_eq!(slash.holder.as_deref(), Some("grok"));

        // The parse takes only the reason string — no timestamp argument
        // exists, so holder × silence cannot collapse into one axis.
        let _ = parse_lock_owner;
    }

    #[test]
    fn path_outside_configured_roots_is_refused_by_name() {
        let repos = vec![RepoEntry {
            name: "acme".into(),
            path: PathBuf::from("/repos/acme-app"),
        }];
        let err = path_is_allowed(Path::new("/elsewhere/nope"), &repos).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("/elsewhere/nope"), "{msg}");
        assert!(msg.contains("/repos/acme-app"), "{msg}");
    }

    #[test]
    fn path_under_a_configured_root_or_as_a_sibling_is_allowed() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("acme-app");
        std::fs::create_dir_all(&main).unwrap();
        let repos = vec![RepoEntry {
            name: "acme".into(),
            path: main.clone(),
        }];
        path_is_allowed(&main.join("inside"), &repos).unwrap();
        path_is_allowed(&tmp.path().join("wt-feature"), &repos).unwrap();
    }

    #[test]
    fn readiness_emits_the_command_for_each_detection() {
        let row = WorktreeRow {
            workspace_id: "ws_x".into(),
            id: "feature".into(),
            path: Some("/repos/acme-app-feature".into()),
            branch: None,
            head_sha: None,
            is_main: false,
            bare: false,
            detached: true,
            locked: true,
            lock_reason: None,
            prunable: false,
            prunable_reason: None,
            mounted: true,
            path_resolution: "exact".into(),
            repo: Some("acme".into()),
            created_by_daemon: false,
        };
        let issues = detect_readiness(&row, Some(0));
        let codes: Vec<_> = issues.iter().map(|i| i.code).collect();
        assert!(codes.contains(&"detached-head"), "{codes:?}");
        assert!(codes.contains(&"lock-without-reason"), "{codes:?}");
        assert!(codes.contains(&"not-indexed"), "{codes:?}");
        assert!(
            issues
                .iter()
                .any(|i| i.code == "detached-head" && i.command.contains("git switch")),
            "{issues:?}"
        );
        assert!(
            issues
                .iter()
                .any(|i| i.code == "lock-without-reason" && i.command.contains("git worktree lock")),
            "{issues:?}"
        );
    }

    #[test]
    fn loss_preview_names_the_manual_remove_command() {
        let row = WorktreeRow {
            workspace_id: "ws_x".into(),
            id: "feature".into(),
            path: Some("/repos/acme-app-feature".into()),
            branch: Some("feature".into()),
            head_sha: None,
            is_main: false,
            bare: false,
            detached: false,
            locked: false,
            lock_reason: None,
            prunable: false,
            prunable_reason: None,
            mounted: true,
            path_resolution: "exact".into(),
            repo: Some("acme".into()),
            created_by_daemon: false,
        };
        let p = loss_preview(&row);
        assert_eq!(
            p.manual_remove,
            "git worktree remove -- /repos/acme-app-feature"
        );
        assert!(!p.created_by_daemon);
    }
}
