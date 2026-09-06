//! Directory listing at a ref — root or nested path, pure ODB read.

use super::{GitError, GitRepo, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    /// A pinned commit of a git submodule — reported, never descended into.
    Submodule,
}

impl EntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EntryKind::File => "file",
            EntryKind::Dir => "dir",
            EntryKind::Symlink => "symlink",
            EntryKind::Submodule => "submodule",
        }
    }

    fn from_mode(mode: gix::objs::tree::EntryMode) -> Self {
        use gix::objs::tree::EntryKind as GixKind;
        match mode.kind() {
            GixKind::Tree => EntryKind::Dir,
            GixKind::Blob | GixKind::BlobExecutable => EntryKind::File,
            GixKind::Link => EntryKind::Symlink,
            GixKind::Commit => EntryKind::Submodule,
        }
    }

    /// Human label for error messages (`NotADir`/`NotABlob`'s `actual`).
    fn label(self) -> &'static str {
        match self {
            EntryKind::File => "file",
            EntryKind::Dir => "directory",
            EntryKind::Symlink => "symlink",
            EntryKind::Submodule => "submodule",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TreeEntry {
    pub name: String,
    pub kind: EntryKind,
    /// Blob size in bytes — populated for `File`/`Symlink` (both are
    /// backed by a blob object), `None` for `Dir`/`Submodule` (a tree has
    /// no byte size, and a submodule's pinned commit lives in a different
    /// repository this handle never opens).
    pub size: Option<u64>,
    /// Full hex object id of the entry (the blob for files/symlinks, the
    /// tree for dirs, the pinned commit for submodules).
    pub oid: String,
}

fn entry_to_tree_entry(entry: gix::object::tree::EntryRef<'_, '_>) -> Result<TreeEntry> {
    let mode = entry.mode();
    let kind = EntryKind::from_mode(mode);
    let name = entry.filename().to_string();
    let oid = entry.object_id().to_string();
    let size = if mode.is_blob_or_symlink() {
        let header = entry.id().header().map_err(|e| GitError::Odb {
            message: e.to_string(),
        })?;
        Some(header.size())
    } else {
        None
    };
    Ok(TreeEntry {
        name,
        kind,
        size,
        oid,
    })
}

/// Peel `rev` down to its tree, then — if `path` names a subdirectory —
/// navigate to that subtree. `path` of `""` (or all-slashes) means the
/// repo root.
fn resolve_tree<'repo>(repo: &'repo GitRepo, rev: &str, path: &str) -> Result<gix::Tree<'repo>> {
    let id = repo.resolve(rev)?;
    let object = repo.repo.find_object(id).map_err(|e| GitError::Odb {
        message: e.to_string(),
    })?;
    let root = object.peel_to_tree().map_err(|e| GitError::Odb {
        message: e.to_string(),
    })?;

    let norm = path.trim_matches('/');
    if norm.is_empty() {
        return Ok(root);
    }

    let entry = root
        .lookup_entry_by_path(norm)
        .map_err(|e| GitError::Odb {
            message: e.to_string(),
        })?
        .ok_or_else(|| GitError::PathNotFound {
            rev: rev.to_string(),
            path: norm.to_string(),
        })?;

    let kind = EntryKind::from_mode(entry.mode());
    if kind != EntryKind::Dir {
        return Err(GitError::NotADir {
            rev: rev.to_string(),
            path: norm.to_string(),
            actual: kind.label(),
        });
    }

    let object = entry.object().map_err(|e| GitError::Odb {
        message: e.to_string(),
    })?;
    object.try_into_tree().map_err(|e| GitError::Odb {
        message: e.to_string(),
    })
}

pub(super) fn list_tree(repo: &GitRepo, rev: &str, path: &str) -> Result<Vec<TreeEntry>> {
    let tree = resolve_tree(repo, rev, path)?;
    let mut out = Vec::new();
    for entry in tree.iter() {
        let entry = entry.map_err(|e| GitError::Odb {
            message: e.to_string(),
        })?;
        out.push(entry_to_tree_entry(entry)?);
    }
    Ok(out)
}
