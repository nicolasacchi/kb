//! V75-M1 — D13's **Workspace**: the shared git object store, and the
//! worktrees that check it out.
//!
//! Two nouns, and conflating them is the defect this module exists to make
//! impossible:
//!
//! * a **Workspace** is one object store — `id` derives from the canonical
//!   `--git-common-dir` plus the root commit (D13: "id = canonical
//!   common-dir + root-commit"). Every blob, every commit and every
//!   git-history derivation belongs to it, no matter which checkout you
//!   read them through.
//! * a **Worktree** is one checkout — `id` is the ADMIN-DIR NAME
//!   (`<common>/worktrees/<id>`), and **the path is a mutable attribute**.
//!   Moving a worktree on disk must not mint a second identity, which is
//!   exactly why the path is not the key.
//!
//! **A third, older word.** D26's kbc-seq/1 already owns the identifier
//! `reading_sets.workspace_id` (V0029), where it names a *reading set* of
//! kind `workspace` — the Desk. That is unrelated to this module. The
//! re-key therefore never adds a `workspace_id` column to a table that
//! could be read as a Desk: the four authored, path-anchored projection
//! tables are WORKTREE-class and gain `worktree_id` instead (see
//! [`crate::rekey`]). `kb-code workspaces` (this module) and `kb-code
//! workspace` (the Desk) are two verbs, one letter apart, and both help
//! texts say so.
//!
//! **No git process is spawned from this file.** Every git read goes
//! through [`crate::history::run_git_raw`], which is already on
//! `tests/security/git_argv_lint.rs`'s pinned `GIT_SPAWNING_FILES` list
//! (invariant 3) — none of the three commands here (`rev-parse
//! --git-common-dir`, `rev-list --max-parents=0 HEAD`, `worktree list
//! --porcelain`) takes a caller-supplied component, and none carries a
//! pathspec.
//!
//! **Nothing here runs on the bind path.** `rev-list --max-parents=0 HEAD`
//! walks history, so resolution is driven from the same paged background
//! task as the re-key backfill (`crate::rekey::spawn_rekey`) — V72-B0(a):
//! no whole-corpus pass may sit between `Store::open` and the listener
//! bind. Until it lands, `GET /api/workspaces` is honestly empty and
//! `GET /api/identity` reports `rekey: "pending"`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::RepoEntry;
use crate::entities::RouteContract;
use crate::store::Store;

/// The reserved id for the MAIN worktree, which has no admin directory of
/// its own (`<root>/.git` IS the common dir).
///
/// Git derives a linked worktree's admin name from its path's basename, so
/// a directory literally named `(main)` could in principle mint a
/// colliding name. Enumeration assigns this id to the main worktree FIRST
/// (`git worktree list --porcelain` always emits it first), so on a
/// collision the main worktree wins and the linked one is disambiguated by
/// [`disambiguate`] rather than silently overwriting it —
/// `tests::a_linked_worktree_named_like_the_main_sentinel_is_disambiguated`
/// pins that.
pub const MAIN_WORKTREE_ID: &str = "(main)";

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("git read failed: {0}")]
    Git(String),
    #[error("workspace io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("kb-code store error: {0}")]
    Store(#[from] crate::store::StoreError),
}

pub type Result<T> = std::result::Result<T, WorkspaceError>;

// ── identity ─────────────────────────────────────────────────────────

/// FNV-1a 64 — hand-rolled because the value is PERSISTED as a primary
/// key. `std::hash::DefaultHasher` is explicitly not stable across Rust
/// releases, so a workspace id minted by one toolchain would stop matching
/// its own rows after a rustc bump. Same constants, same reason, as
/// `lib::salt_set_fingerprint` and `review_doc`'s fingerprint.
fn fnv1a(parts: &[&str]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in p.as_bytes().iter().chain(std::iter::once(&b'\n')) {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

/// D13's workspace id: `ws_` + 12 hex over (canonical common dir, root
/// commit). Pure and total — an absent root commit folds the literal
/// `"-"`, so a repository with no commits still gets a stable id rather
/// than colliding with one that has them.
///
/// The 12-hex shape matches this crate's other minted ids (`set_`, `clm_`,
/// `f-`); collisions are a birthday problem over 2^48 with a handful of
/// workspaces per daemon.
pub fn workspace_id(common_dir: &Path, root_commit: Option<&str>) -> String {
    let common = common_dir.to_string_lossy();
    let root = root_commit.unwrap_or("-");
    let h = fnv1a(&[common.as_ref(), root]);
    format!("ws_{:012x}", h & 0x0000_ffff_ffff_ffff)
}

// ── git reads ────────────────────────────────────────────────────────

fn git(repo_root: &Path, args: &[&str]) -> Result<String> {
    let raw = crate::history::run_git_raw(repo_root, args)
        .map_err(|e| WorkspaceError::Git(e.to_string()))?;
    Ok(String::from_utf8_lossy(&raw).to_string())
}

/// `git rev-parse --git-common-dir`, absolutised against `repo_root` and
/// canonicalised (invariant #27: a symlinked repo root must not mint a
/// second workspace).
///
/// Same relative-vs-absolute handling as
/// `history::scratch::real_objects_dir` — git answers `.git` for a plain
/// checkout and an absolute path for a linked worktree.
pub fn common_dir(repo_root: &Path) -> Result<PathBuf> {
    let out = git(repo_root, &["rev-parse", "--git-common-dir"])?;
    let rel = out.trim();
    if rel.is_empty() {
        return Err(WorkspaceError::Git(
            "git rev-parse --git-common-dir returned nothing".into(),
        ));
    }
    let p = PathBuf::from(rel);
    let abs = if p.is_absolute() {
        p
    } else {
        repo_root.join(p)
    };
    Ok(std::fs::canonicalize(&abs).unwrap_or(abs))
}

/// The root commit reachable from HEAD, or `None` when HEAD is unborn.
///
/// A history with SEVERAL root commits (a merged unrelated history) is
/// resolved to the lexicographically smallest sha, which is the only
/// choice that does not depend on git's traversal order. A shallow clone
/// reports whatever roots it can see, so the id is stable for that clone
/// and `GET /api/workspaces` captions the fact rather than pretending the
/// sha is absolute.
///
/// **This walks history.** `git rev-list --max-parents=0 HEAD` is O(the
/// whole reachable graph), which is why resolution never runs on the bind
/// path (V72-B0(a)) and why [`resolve_and_upsert`] pays it ONCE per volume
/// — the second boot reads the recorded sha out of `workspaces` instead.
/// An error (unborn HEAD, a git too old, an unreadable repo) is `None`,
/// never a fabricated sha.
pub fn root_commit(repo_root: &Path) -> Result<Option<String>> {
    let out = match git(repo_root, &["rev-list", "--max-parents=0", "HEAD"]) {
        Ok(o) => o,
        // An unborn HEAD (a freshly `git init`'d repo) is a legitimate
        // state, not a failure: `rev-list` exits non-zero and we report
        // "no root commit" rather than refusing to identify the workspace.
        Err(_) => return Ok(None),
    };
    let mut roots: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    roots.sort_unstable();
    Ok(roots.first().map(|s| s.to_string()))
}

// ── worktree enumeration ─────────────────────────────────────────────

/// One row of `git worktree list --porcelain`, before an admin name is
/// attached. Deliberately a separate type from the stored row: this is
/// what GIT said, [`WorktreeRow`] is what the daemon recorded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PorcelainEntry {
    pub path: Option<String>,
    pub head_sha: Option<String>,
    pub branch: Option<String>,
    pub bare: bool,
    pub detached: bool,
    pub locked: bool,
    pub lock_reason: Option<String>,
    pub prunable: bool,
    pub prunable_reason: Option<String>,
}

/// PURE parser for `git worktree list --porcelain`. Total: an unknown
/// attribute line is ignored rather than failing the whole enumeration —
/// git has added attributes to this format before (`prunable` in 2.30,
/// `locked` reasons in 2.36) and a daemon that refused to list worktrees
/// because a newer git said something extra would be worse than one that
/// lists what it understood.
///
/// Records are blank-line separated; the first record is always the MAIN
/// worktree.
pub fn parse_worktree_list(porcelain: &str) -> Vec<PorcelainEntry> {
    let mut out = Vec::new();
    let mut cur: Option<PorcelainEntry> = None;
    for line in porcelain.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            if let Some(e) = cur.take() {
                out.push(e);
            }
            continue;
        }
        let (key, rest) = match line.split_once(' ') {
            Some((k, r)) => (k, Some(r)),
            None => (line, None),
        };
        if key == "worktree" {
            if let Some(e) = cur.take() {
                out.push(e);
            }
            cur = Some(PorcelainEntry {
                path: rest.map(|s| s.to_string()),
                ..Default::default()
            });
            continue;
        }
        let Some(e) = cur.as_mut() else { continue };
        match key {
            "HEAD" => e.head_sha = rest.map(|s| s.to_string()),
            // `branch refs/heads/x` — shortened here, once, so no reader
            // re-derives it.
            "branch" => {
                e.branch = rest.map(|s| s.strip_prefix("refs/heads/").unwrap_or(s).to_string())
            }
            "bare" => e.bare = true,
            "detached" => e.detached = true,
            "locked" => {
                e.locked = true;
                e.lock_reason = rest.filter(|s| !s.is_empty()).map(|s| s.to_string());
            }
            "prunable" => {
                e.prunable = true;
                e.prunable_reason = rest.filter(|s| !s.is_empty()).map(|s| s.to_string());
            }
            _ => {}
        }
    }
    if let Some(e) = cur.take() {
        out.push(e);
    }
    out
}

/// Map every linked worktree's ADMIN NAME to the path git recorded for it,
/// read straight from `<common>/worktrees/<name>/gitdir`.
///
/// This is a filesystem read rather than a second git call on purpose: it
/// is the one source that still answers for a PRUNABLE worktree, whose own
/// directory no longer exists (so `git -C <path> rev-parse --git-dir`
/// cannot be asked). Unreadable entries are skipped, never guessed.
pub fn admin_dir_paths(common_dir: &Path) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(common_dir.join("worktrees")) else {
        return map;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else {
            continue;
        };
        let Ok(gitdir) = std::fs::read_to_string(entry.path().join("gitdir")) else {
            continue;
        };
        let recorded = gitdir.trim();
        // `<worktree>/.git` — the file inside the checkout that points
        // back here. The worktree path is its parent.
        let path = recorded
            .strip_suffix("/.git")
            .unwrap_or(recorded)
            .to_string();
        map.insert(name, path);
    }
    map
}

/// Give a linked worktree an id that cannot collide with an id already
/// taken (above all [`MAIN_WORKTREE_ID`]). Deterministic: `name`,
/// `name~1`, `name~2`, …
fn disambiguate(name: &str, taken: &[String]) -> String {
    if !taken.iter().any(|t| t == name) {
        return name.to_string();
    }
    for n in 1..1000 {
        let candidate = format!("{name}~{n}");
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    format!("{name}~overflow")
}

/// How a worktree's own path resolves against the configured `[[repos]]`
/// roots — D13's "known, not mounted" rule.
///
/// * `exact` — the worktree path IS a configured root; reads go straight
///   through that repo entry.
/// * `ancestor` — the worktree path lies INSIDE a configured root.
/// * `absent` — outside every configured root. The worktree is LISTED with
///   that stated (`mounted: false`), never hidden and never browsed.
///
/// Not to be confused with D14/M4's per-FILE `path_resolution`, which
/// answers "does this path exist in the target ref" for the reader. Same
/// three words, different subject; this one is about the worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathResolution {
    Exact,
    Ancestor,
    Absent,
}

impl PathResolution {
    pub fn as_str(self) -> &'static str {
        match self {
            PathResolution::Exact => "exact",
            PathResolution::Ancestor => "ancestor",
            PathResolution::Absent => "absent",
        }
    }
}

/// Resolve `path` against the configured roots, longest-root-wins (the
/// same rule `kb-code doctor`'s cwd→repo oracle uses, for the same reason:
/// nested roots must resolve to the most specific one).
pub fn resolve_path(path: Option<&str>, repos: &[RepoEntry]) -> (PathResolution, Option<String>) {
    let Some(path) = path else {
        return (PathResolution::Absent, None);
    };
    let canon = std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string());
    let mut best: Option<(&RepoEntry, PathResolution)> = None;
    for r in repos {
        let root = r.path.to_string_lossy().trim_end_matches('/').to_string();
        let res = if canon == root {
            PathResolution::Exact
        } else if canon.starts_with(&format!("{root}/")) {
            PathResolution::Ancestor
        } else {
            continue;
        };
        let better = match &best {
            None => true,
            Some((b, _)) => root.len() > b.path.to_string_lossy().trim_end_matches('/').len(),
        };
        if better {
            best = Some((r, res));
        }
    }
    match best {
        Some((r, res)) => (res, Some(r.name.clone())),
        None => (PathResolution::Absent, None),
    }
}

/// One worktree as the daemon recorded it — the wire and storage shape.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorktreeRow {
    pub workspace_id: String,
    pub id: String,
    pub path: Option<String>,
    pub branch: Option<String>,
    pub head_sha: Option<String>,
    pub is_main: bool,
    pub bare: bool,
    pub detached: bool,
    pub locked: bool,
    /// The lock reason VERBATIM. D13's owner oracle is parsed defensively
    /// by a later unit (M2) and can only ever mint `likely`; this unit
    /// stores the bytes and interprets none of them.
    pub lock_reason: Option<String>,
    pub prunable: bool,
    pub prunable_reason: Option<String>,
    pub mounted: bool,
    pub path_resolution: String,
    /// The `[[repos]]` entry this worktree is reachable through, when it is
    /// mounted. `None` for "known, not mounted".
    pub repo: Option<String>,
}

/// Enumerate every worktree of the workspace `repo_root` belongs to.
///
/// PRUNABLE entries are kept in the list (D13's thin slice says reads skip
/// them; it does not say hide them) with `prunable: true` and git's own
/// reason — a worktree whose directory was deleted out from under git is
/// exactly the thing an operator needs to see, and M2's `prune` verb needs
/// something to name.
pub fn enumerate(
    repo_root: &Path,
    workspace_id: &str,
    repos: &[RepoEntry],
) -> Result<Vec<WorktreeRow>> {
    let common = common_dir(repo_root)?;
    let porcelain = git(repo_root, &["worktree", "list", "--porcelain"])?;
    let entries = parse_worktree_list(&porcelain);
    let admin = admin_dir_paths(&common);

    let mut rows: Vec<WorktreeRow> = Vec::with_capacity(entries.len());
    let mut taken: Vec<String> = Vec::with_capacity(entries.len());
    for (i, e) in entries.iter().enumerate() {
        // The first porcelain record is always the main worktree, which has
        // no admin directory. Claiming the sentinel FIRST is what makes a
        // linked worktree that happens to be named `(main)` the one that
        // gets disambiguated.
        let id = if i == 0 {
            MAIN_WORKTREE_ID.to_string()
        } else {
            let name = e
                .path
                .as_deref()
                .and_then(|p| admin_name_for(&admin, p))
                // A linked worktree git listed but whose admin `gitdir`
                // file is unreadable: fall back to the path basename,
                // which is what git itself derived the admin name from.
                .or_else(|| {
                    e.path.as_deref().and_then(|p| {
                        Path::new(p)
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                    })
                })
                .unwrap_or_else(|| format!("worktree-{i}"));
            disambiguate(&name, &taken)
        };
        taken.push(id.clone());
        let (res, repo) = resolve_path(e.path.as_deref(), repos);
        rows.push(WorktreeRow {
            workspace_id: workspace_id.to_string(),
            id,
            path: e.path.clone(),
            branch: e.branch.clone(),
            head_sha: e.head_sha.clone(),
            is_main: i == 0,
            bare: e.bare,
            detached: e.detached,
            locked: e.locked,
            lock_reason: e.lock_reason.clone(),
            prunable: e.prunable,
            prunable_reason: e.prunable_reason.clone(),
            mounted: res != PathResolution::Absent,
            path_resolution: res.as_str().to_string(),
            repo,
        });
    }
    Ok(rows)
}

/// Reverse the admin map: which admin name recorded `path`? Matches on the
/// exact recorded string first, then on both sides canonicalised (git
/// records the path it was given; a caller may hold a symlinked form).
fn admin_name_for(admin: &BTreeMap<String, String>, path: &str) -> Option<String> {
    if let Some((name, _)) = admin.iter().find(|(_, p)| p.as_str() == path) {
        return Some(name.clone());
    }
    let canon = std::fs::canonicalize(path).ok()?;
    admin
        .iter()
        .find(|(_, p)| {
            std::fs::canonicalize(p)
                .map(|c| c == canon)
                .unwrap_or(false)
        })
        .map(|(name, _)| name.clone())
}

// ── resolution + persistence ─────────────────────────────────────────

/// What one repo's resolution produced — returned so the background task
/// can log a census instead of a bare count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub repo: String,
    pub workspace_id: String,
    pub worktree_id: String,
    pub worktrees: usize,
    pub root_commit: Option<String>,
}

/// Resolve every configured repo to its workspace + worktree, upsert both
/// tables, and stamp `repos.workspace_id`/`repos.worktree_id`.
///
/// Idempotent by construction: the workspace id is a pure function of
/// (common dir, root commit), so a second run rewrites the same rows.
/// Cheap on the second boot: the root-commit walk is SKIPPED when a
/// `workspaces` row already exists for that common dir — the expensive
/// part is paid once per volume, not once per boot.
///
/// A repo that cannot be read (deleted between config load and here, a
/// git version too old for `--git-common-dir`) is SKIPPED with a warning;
/// one unreadable repo never costs the others their identity.
pub fn resolve_and_upsert(store: &Store, repos: &[RepoEntry]) -> Vec<Resolved> {
    let now = chrono::Utc::now().timestamp();
    let mut out = Vec::with_capacity(repos.len());
    for r in repos {
        let common = match common_dir(&r.path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(repo = %r.name, error = %e,
                    "kb-code: could not resolve the workspace common dir — skipping");
                continue;
            }
        };
        let common_s = common.to_string_lossy().to_string();
        let known = store.workspace_by_common_dir(&common_s).ok().flatten();
        let root = match &known {
            Some(w) => w.root_commit.clone(),
            None => root_commit(&r.path).unwrap_or(None),
        };
        let ws = workspace_id(&common, root.as_deref());
        if let Err(e) = store.upsert_workspace(&ws, &common_s, root.as_deref(), now) {
            tracing::warn!(repo = %r.name, error = %e, "kb-code: could not record the workspace");
            continue;
        }
        let rows = match enumerate(&r.path, &ws, repos) {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(repo = %r.name, error = %e,
                    "kb-code: could not enumerate worktrees — recording the workspace only");
                Vec::new()
            }
        };
        // Which worktree IS this repo entry? The one whose path resolves
        // `exact` to it, else the main one — never a guess by basename.
        let mine = rows
            .iter()
            .find(|w| w.repo.as_deref() == Some(r.name.as_str()) && w.path_resolution == "exact")
            .or_else(|| rows.iter().find(|w| w.is_main))
            .map(|w| w.id.clone())
            .unwrap_or_else(|| MAIN_WORKTREE_ID.to_string());
        if let Err(e) = store.replace_worktrees(&ws, &rows, now) {
            tracing::warn!(repo = %r.name, error = %e, "kb-code: could not record worktrees");
        }
        if let Err(e) = store.set_repo_identity(&r.name, &ws, &mine) {
            tracing::warn!(repo = %r.name, error = %e, "kb-code: could not stamp the repo identity");
            continue;
        }
        out.push(Resolved {
            repo: r.name.clone(),
            workspace_id: ws,
            worktree_id: mine,
            worktrees: rows.len(),
            root_commit: root,
        });
    }
    out
}

// ── routes ───────────────────────────────────────────────────────────

pub const WORKSPACES_ROUTE: RouteContract = RouteContract {
    path: "/api/workspaces",
    handler: "workspace::workspaces_route",
    required_params: &[],
    params_accept_without: |_| true,
};

/// The two reads this unit adds that a `RouteContract` can describe.
///
/// `GET /api/workspaces/{id}/worktrees` is deliberately ABSENT: a
/// `RouteContract` describes a QUERY-param surface and
/// `params_accept_without` has nothing to say about a path segment — the
/// same reason `boards`' four mutations are absent from `V74_L1_ROUTES`.
/// It is still registered in `router.rs` and covered by its own route
/// test.
pub const V75_M1_ROUTES: &[RouteContract] = &[WORKSPACES_ROUTE, crate::frames::FRAMES_ROUTE];

#[derive(Debug, serde::Serialize)]
pub struct WorkspaceOut {
    pub id: String,
    pub common_dir: String,
    /// `None` when HEAD was unborn or the root walk was over budget — the
    /// id then derives from the common dir alone, and `note` says so.
    pub root_commit: Option<String>,
    pub created_at: i64,
    /// Every `[[repos]]` entry resolved to this workspace, in config order.
    pub repos: Vec<String>,
    pub worktrees: Vec<WorktreeRow>,
    /// Per object-class table, how many rows this workspace owns — the
    /// re-key READ, through the key it added. Absent tables are tables
    /// with no rows for this workspace, never a failure.
    pub derived: BTreeMap<String, i64>,
    pub note: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct WorkspacesResponse {
    pub schema: &'static str,
    /// `pending` | `running` | `done` — the re-key backfill's own state,
    /// the same value `GET /api/identity` reports. `pending` means the
    /// lists below may be empty because resolution has not run yet, NOT
    /// that there are no workspaces.
    pub rekey: &'static str,
    pub workspaces: Vec<WorkspaceOut>,
}

pub async fn workspaces_route(
    axum::extract::State(state): axum::extract::State<crate::state::SharedState>,
) -> std::result::Result<axum::Json<WorkspacesResponse>, crate::routes::ApiError> {
    let rekey = crate::rekey::state_label(&state.rekey);
    let repo_names: Vec<String> = state.repos.iter().map(|r| r.name.clone()).collect();
    let workspaces = crate::store::StoreBlocking::run_blocking(&state.store, move |store| {
        collect_workspaces(store, &repo_names)
    })
    .await?;
    Ok(axum::Json(WorkspacesResponse {
        schema: "kbc-workspace/1",
        rekey,
        workspaces,
    }))
}

#[derive(Debug, serde::Serialize)]
pub struct WorktreesResponse {
    pub schema: &'static str,
    pub rekey: &'static str,
    pub workspace: WorkspaceOut,
}

pub async fn workspace_worktrees_route(
    axum::extract::State(state): axum::extract::State<crate::state::SharedState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> std::result::Result<axum::Json<WorktreesResponse>, crate::routes::ApiError> {
    let rekey = crate::rekey::state_label(&state.rekey);
    let repo_names: Vec<String> = state.repos.iter().map(|r| r.name.clone()).collect();
    let wanted = id.clone();
    let found = crate::store::StoreBlocking::run_blocking(&state.store, move |store| {
        collect_workspaces(store, &repo_names).map(|all| all.into_iter().find(|w| w.id == wanted))
    })
    .await?;
    let Some(workspace) = found else {
        return Err(crate::routes::ApiError::not_found(format!(
            "no such workspace: {id:?}"
        )));
    };
    Ok(axum::Json(WorktreesResponse {
        schema: "kbc-workspace/1",
        rekey,
        workspace,
    }))
}

fn collect_workspaces(
    store: &Store,
    repo_names: &[String],
) -> std::result::Result<Vec<WorkspaceOut>, crate::store::StoreError> {
    let rows = store.list_workspaces()?;
    let mut out = Vec::with_capacity(rows.len());
    for w in rows {
        let worktrees = store.worktrees_for_workspace(&w.id)?;
        let repos: Vec<String> = repo_names
            .iter()
            .filter(|n| {
                store
                    .repo_identity(n)
                    .ok()
                    .flatten()
                    .map(|(ws, _)| ws == w.id)
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        let derived = store.workspace_derived_census(&w.id)?;
        let note = if w.root_commit.is_none() {
            Some(
                "root commit unresolved (unborn HEAD or over the walk budget) — this workspace \
                 id derives from the common dir alone"
                    .to_string(),
            )
        } else {
            None
        };
        out.push(WorkspaceOut {
            id: w.id,
            common_dir: w.common_dir,
            root_commit: w.root_commit,
            created_at: w.created_at,
            repos,
            worktrees,
            derived,
            note,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real shape git emits, including the attributes this parser has
    /// to survive: a bare main, a detached head, a lock with and without a
    /// reason, a prunable entry, and an attribute this binary has never
    /// heard of.
    const PORCELAIN: &str = "\
worktree /repos/acme
HEAD 1111111111111111111111111111111111111111
branch refs/heads/main

worktree /repos/acme-feature
HEAD 2222222222222222222222222222222222222222
branch refs/heads/feature/x

worktree /repos/acme-detached
HEAD 3333333333333333333333333333333333333333
detached
locked

worktree /repos/acme-locked
HEAD 4444444444444444444444444444444444444444
branch refs/heads/wip
locked being reformatted by claude, session 42

worktree /repos/acme-gone
HEAD 5555555555555555555555555555555555555555
branch refs/heads/old
prunable gitdir file points to non-existent location
somethingnewgitinvented yes
";

    #[test]
    fn the_porcelain_parse_reads_every_attribute_and_survives_an_unknown_one() {
        let got = parse_worktree_list(PORCELAIN);
        assert_eq!(got.len(), 5);
        assert_eq!(got[0].path.as_deref(), Some("/repos/acme"));
        assert_eq!(
            got[0].branch.as_deref(),
            Some("main"),
            "refs/heads/ stripped once, here"
        );
        assert!(!got[0].detached && !got[0].locked && !got[0].prunable);

        assert_eq!(got[1].branch.as_deref(), Some("feature/x"));

        assert!(got[2].detached);
        assert_eq!(got[2].branch, None, "a detached head has no branch");
        assert!(got[2].locked);
        assert_eq!(
            got[2].lock_reason, None,
            "`locked` with no reason is still locked"
        );

        assert!(got[3].locked);
        assert_eq!(
            got[3].lock_reason.as_deref(),
            Some("being reformatted by claude, session 42"),
            "the reason is stored VERBATIM — no owner is parsed out of it here"
        );

        assert!(got[4].prunable);
        assert_eq!(
            got[4].prunable_reason.as_deref(),
            Some("gitdir file points to non-existent location")
        );
    }

    #[test]
    fn an_empty_or_trailing_newline_porcelain_is_total() {
        assert!(parse_worktree_list("").is_empty());
        assert_eq!(parse_worktree_list("worktree /a\n").len(), 1);
        assert_eq!(parse_worktree_list("worktree /a").len(), 1);
        // A record with no trailing blank line still closes.
        assert_eq!(parse_worktree_list("worktree /a\nbare").len(), 1);
    }

    #[test]
    fn a_bare_repository_is_recognised() {
        let got = parse_worktree_list("worktree /repos/bare.git\nbare\n");
        assert!(got[0].bare);
    }

    #[test]
    fn the_workspace_id_is_stable_and_depends_on_both_inputs() {
        let a = workspace_id(Path::new("/repos/acme/.git"), Some("abc123"));
        assert_eq!(
            a,
            workspace_id(Path::new("/repos/acme/.git"), Some("abc123"))
        );
        assert_ne!(
            a,
            workspace_id(Path::new("/repos/other/.git"), Some("abc123"))
        );
        assert_ne!(
            a,
            workspace_id(Path::new("/repos/acme/.git"), Some("def456"))
        );
        assert!(a.starts_with("ws_") && a.len() == 15, "{a}");
    }

    /// An unresolved root commit must still be STABLE, and must not
    /// collide with any real one. It folds the literal `"-"`, which is not
    /// a legal object id — so the only string that could collide is `"-"`
    /// itself, and nothing in this crate can produce that as a sha.
    #[test]
    fn an_absent_root_commit_gets_its_own_stable_id() {
        let none = workspace_id(Path::new("/repos/acme/.git"), None);
        assert_eq!(none, workspace_id(Path::new("/repos/acme/.git"), None));
        for sha in [
            "0000000000000000000000000000000000000000",
            "ffffffffffffffffffffffffffffffffffffffff",
            "abc123",
        ] {
            assert_ne!(
                none,
                workspace_id(Path::new("/repos/acme/.git"), Some(sha)),
                "the no-root sentinel collided with {sha}"
            );
        }
    }

    #[test]
    fn a_linked_worktree_named_like_the_main_sentinel_is_disambiguated() {
        let taken = vec![MAIN_WORKTREE_ID.to_string()];
        assert_eq!(disambiguate(MAIN_WORKTREE_ID, &taken), "(main)~1");
        assert_eq!(disambiguate("feature", &taken), "feature");
        let taken2 = vec!["feature".to_string(), "feature~1".to_string()];
        assert_eq!(disambiguate("feature", &taken2), "feature~2");
    }

    fn repo(name: &str, path: &str) -> RepoEntry {
        RepoEntry {
            name: name.to_string(),
            path: PathBuf::from(path),
        }
    }

    #[test]
    fn a_worktree_path_outside_every_configured_root_is_known_but_not_mounted() {
        let repos = vec![repo("acme", "/repos/acme")];
        let (res, name) = resolve_path(Some("/elsewhere/acme-feature"), &repos);
        assert_eq!(res, PathResolution::Absent);
        assert_eq!(name, None);
    }

    #[test]
    fn an_exact_root_beats_an_ancestor_and_the_longest_root_wins() {
        let repos = vec![repo("outer", "/repos"), repo("acme", "/repos/acme")];
        let (res, name) = resolve_path(Some("/repos/acme"), &repos);
        assert_eq!(res, PathResolution::Exact);
        assert_eq!(name.as_deref(), Some("acme"));

        let (res, name) = resolve_path(Some("/repos/acme/sub"), &repos);
        assert_eq!(res, PathResolution::Ancestor);
        assert_eq!(
            name.as_deref(),
            Some("acme"),
            "longest root wins — the same rule kb-code doctor's cwd oracle uses"
        );

        let (res, _) = resolve_path(Some("/repos/other"), &repos);
        assert_eq!(res, PathResolution::Ancestor, "still under /repos");
    }

    /// A prefix match must be on a PATH BOUNDARY: `/repos/acme-old` is not
    /// inside `/repos/acme`.
    #[test]
    fn a_sibling_whose_name_starts_with_a_root_is_not_inside_it() {
        let repos = vec![repo("acme", "/repos/acme")];
        let (res, _) = resolve_path(Some("/repos/acme-old"), &repos);
        assert_eq!(res, PathResolution::Absent);
    }

    #[test]
    fn a_worktree_with_no_path_resolves_absent_rather_than_panicking() {
        assert_eq!(resolve_path(None, &[]).0, PathResolution::Absent);
    }
}
