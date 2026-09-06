//! Branch + tag listing.

use super::{GitError, GitRepo, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RefKind {
    Branch,
    Tag,
}

impl RefKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RefKind::Branch => "branch",
            RefKind::Tag => "tag",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RefInfo {
    /// Short name, e.g. `"main"` or `"v1.0"`. For a remote branch this is
    /// the full name minus the `refs/remotes/<remote>/` prefix (so
    /// `refs/remotes/origin/foo` → `"foo"`), never gix's `shorten()` which
    /// would leave the remote in the name.
    pub name: String,
    /// Full ref name, e.g. `"refs/heads/main"` or `"refs/tags/v1.0"`.
    pub full_name: String,
    pub kind: RefKind,
    /// The commit (or other object) this ref ultimately resolves to, after
    /// peeling annotated tags — full hex sha.
    pub target_sha: String,
    /// `true` if this is the branch HEAD currently points at (always
    /// `false` for tags, and for every ref when HEAD is detached/unborn).
    pub is_head: bool,
    /// `Some(remote)` for `refs/remotes/<remote>/…`. `None` for local
    /// branches and tags. Skipped on the wire when `None` so `/api/refs`
    /// stays byte-identical for its existing local-only listing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
}

/// Local branches + tags. Remote-tracking refs are **not** included —
/// existing callers (`/api/refs`, stack detection, the CLI picker) stay
/// byte-identical. Use [`list_remote_branches`] when remotes are wanted.
pub(super) fn list_refs(repo: &GitRepo) -> Result<Vec<RefInfo>> {
    let head_full_name: Option<String> = repo
        .repo
        .head_name()
        .ok()
        .flatten()
        .map(|n| n.as_bstr().to_string());

    let platform = repo.repo.references().map_err(|e| GitError::Refs {
        message: e.to_string(),
    })?;

    let mut out = Vec::new();

    for r in platform.local_branches().map_err(|e| GitError::Refs {
        message: e.to_string(),
    })? {
        let mut r = r.map_err(|e| GitError::Refs {
            message: e.to_string(),
        })?;
        let full_name = r.name().as_bstr().to_string();
        let name = r.name().shorten().to_string();
        let is_head = head_full_name.as_deref() == Some(full_name.as_str());
        let target = r.peel_to_id().map_err(|e| GitError::Refs {
            message: e.to_string(),
        })?;
        out.push(RefInfo {
            name,
            full_name,
            kind: RefKind::Branch,
            target_sha: target.to_string(),
            is_head,
            remote: None,
        });
    }

    for r in platform.tags().map_err(|e| GitError::Refs {
        message: e.to_string(),
    })? {
        let mut r = r.map_err(|e| GitError::Refs {
            message: e.to_string(),
        })?;
        let full_name = r.name().as_bstr().to_string();
        let name = r.name().shorten().to_string();
        let target = r.peel_to_id().map_err(|e| GitError::Refs {
            message: e.to_string(),
        })?;
        out.push(RefInfo {
            name,
            full_name,
            kind: RefKind::Tag,
            target_sha: target.to_string(),
            is_head: false,
            remote: None,
        });
    }

    Ok(out)
}

/// `refs/remotes/<remote>/<name>` entries via gix's `remote_branches()`.
/// Skips each `<remote>/HEAD` symref. Short name is the full name minus
/// `refs/remotes/<remote>/` (so `origin/foo` → name `"foo"`, remote
/// `Some("origin")`).
pub(super) fn list_remote_branches(repo: &GitRepo) -> Result<Vec<RefInfo>> {
    let platform = repo.repo.references().map_err(|e| GitError::Refs {
        message: e.to_string(),
    })?;

    let mut out = Vec::new();
    for r in platform.remote_branches().map_err(|e| GitError::Refs {
        message: e.to_string(),
    })? {
        let mut r = r.map_err(|e| GitError::Refs {
            message: e.to_string(),
        })?;
        let full_name = r.name().as_bstr().to_string();
        let Some((remote, name)) = parse_remote_ref(&full_name) else {
            continue;
        };
        let target = r.peel_to_id().map_err(|e| GitError::Refs {
            message: e.to_string(),
        })?;
        out.push(RefInfo {
            name,
            full_name,
            kind: RefKind::Branch,
            target_sha: target.to_string(),
            is_head: false,
            remote: Some(remote),
        });
    }
    Ok(out)
}

/// Split `refs/remotes/<remote>/<name>` → `(remote, name)`. Returns
/// `None` for a malformed name or the `<remote>/HEAD` symref (that entry
/// is the default-branch pointer, not a branch the operator can check
/// out).
fn parse_remote_ref(full_name: &str) -> Option<(String, String)> {
    let rest = full_name.strip_prefix("refs/remotes/")?;
    let (remote, name) = rest.split_once('/')?;
    if remote.is_empty() || name.is_empty() || name == "HEAD" {
        return None;
    }
    Some((remote.to_string(), name.to_string()))
}

/// Default branch: `refs/remotes/origin/HEAD` as a symbolic ref → target
/// minus `refs/remotes/origin/`. Falls back to [`GitRepo::head_info`]'s
/// `branch` (today's heuristic) when the origin HEAD symref is missing,
/// not symbolic, or doesn't point under `refs/remotes/origin/`.
pub fn default_branch(repo: &GitRepo) -> Option<String> {
    if let Some(name) = origin_head_branch(repo) {
        return Some(name);
    }
    repo.head_info().ok().and_then(|h| h.branch)
}

fn origin_head_branch(repo: &GitRepo) -> Option<String> {
    let r = repo
        .repo
        .try_find_reference("refs/remotes/origin/HEAD")
        .ok()
        .flatten()?;
    let target = r.target();
    let full = target.try_name()?.as_bstr().to_string();
    full.strip_prefix("refs/remotes/origin/")
        .filter(|n| !n.is_empty() && *n != "HEAD")
        .map(str::to_string)
}
