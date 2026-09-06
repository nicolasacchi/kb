//! Blob content reads at a ref, with a size cap enforced via a cheap ODB
//! header lookup (the oversized blob's bytes are never actually loaded).

use super::{GitError, GitRepo, Result};

/// Library default size cap for `read_blob` — 10 MiB. Callers needing a
/// different cap pass it explicitly to `GitRepo::read_blob`.
pub const DEFAULT_BLOB_SIZE_CAP: u64 = 10 * 1024 * 1024;

pub(super) fn read_blob(repo: &GitRepo, rev: &str, path: &str, max_bytes: u64) -> Result<Vec<u8>> {
    let id = repo.resolve(rev)?;
    let object = repo.repo.find_object(id).map_err(|e| GitError::Odb {
        message: e.to_string(),
    })?;
    let tree = object.peel_to_tree().map_err(|e| GitError::Odb {
        message: e.to_string(),
    })?;

    let norm = path.trim_matches('/');
    if norm.is_empty() {
        return Err(GitError::NotABlob {
            rev: rev.to_string(),
            path: norm.to_string(),
            actual: "directory",
        });
    }

    let entry = tree
        .lookup_entry_by_path(norm)
        .map_err(|e| GitError::Odb {
            message: e.to_string(),
        })?
        .ok_or_else(|| GitError::PathNotFound {
            rev: rev.to_string(),
            path: norm.to_string(),
        })?;

    let mode = entry.mode();
    if !mode.is_blob_or_symlink() {
        // `is_blob_or_symlink()` covers Blob/BlobExecutable/Link; the only
        // remaining tree-entry kinds are Tree (directory) and Commit
        // (submodule pin) — no separate classification needed here.
        let actual = if mode.is_tree() {
            "directory"
        } else {
            "submodule"
        };
        return Err(GitError::NotABlob {
            rev: rev.to_string(),
            path: norm.to_string(),
            actual,
        });
    }

    let header = entry.id().header().map_err(|e| GitError::Odb {
        message: e.to_string(),
    })?;
    let size = header.size();
    if size > max_bytes {
        return Err(GitError::TooLarge {
            rev: rev.to_string(),
            path: norm.to_string(),
            size,
            cap: max_bytes,
        });
    }

    // `Blob` has a `Drop` impl (it returns its buffer to the repo's reuse
    // pool), so its `data` field can't be moved out directly — `take_data()`
    // is gix's sanctioned way to reclaim it.
    let mut blob = entry
        .object()
        .map_err(|e| GitError::Odb {
            message: e.to_string(),
        })?
        .try_into_blob()
        .map_err(|e| GitError::Odb {
            message: e.to_string(),
        })?;
    Ok(blob.take_data())
}

/// The committed blob's object id for `path` at `rev`, or `None` if that
/// path doesn't exist in `rev`'s tree at all (e.g. a brand-new file with no
/// history yet). A pure ODB lookup — `entry.object_id()` is already resolved
/// by the tree walk itself, so no blob content is ever read (same "cheap
/// header before content" discipline `read_blob`'s size-cap check follows).
/// Used by the blame service's clean-vs-dirty cache-key decision
/// (`crate::blame::is_dirty`), which only ever needs to COMPARE hashes, not
/// the bytes behind them.
pub(super) fn blob_oid(repo: &GitRepo, rev: &str, path: &str) -> Result<Option<String>> {
    let id = repo.resolve(rev)?;
    let object = repo.repo.find_object(id).map_err(|e| GitError::Odb {
        message: e.to_string(),
    })?;
    let tree = object.peel_to_tree().map_err(|e| GitError::Odb {
        message: e.to_string(),
    })?;

    let norm = path.trim_matches('/');
    if norm.is_empty() {
        return Ok(None);
    }

    let entry = tree.lookup_entry_by_path(norm).map_err(|e| GitError::Odb {
        message: e.to_string(),
    })?;
    Ok(entry.map(|e| e.object_id().to_string()))
}

/// Read the raw bytes of a blob addressed by its OWN object id (full or
/// abbreviated), refusing anything that is not a blob and refusing to load
/// more than `max_bytes` (the same cheap ODB-header check [`read_blob`]
/// makes).
///
/// V73-K1 — `kbc-review/1`'s `[[code:path:line@sha]]` pins the BLOB the
/// author read, and the carry-forward ladder needs those exact bytes to
/// re-anchor from. Every other read in this module addresses a blob through
/// a REVISION plus a path; this one cannot, because a review document's ref
/// names the content, not the commit it lived in — a commit may have been
/// rewritten under the review while the blob it pointed at is still in the
/// ODB, which is precisely the case a carry-forward exists to survive.
///
/// The caller-supplied id is not a general revspec: the `kbc-review/1`
/// grammar constrains it to 4–64 hex digits before it ever reaches here
/// (`review_doc::refs::is_hexish`), a closed alphabet that cannot express
/// an option, a path or a range. An id that resolves to a non-blob object
/// (a commit sha written where a blob sha belongs, the common author
/// mistake) comes back as `NotABlob` so the caller can say so, rather than
/// as a silent miss.
pub(super) fn read_blob_by_oid(repo: &GitRepo, oid: &str, max_bytes: u64) -> Result<Vec<u8>> {
    let id = repo.resolve(oid)?;
    let object = repo.repo.find_object(id).map_err(|e| GitError::Odb {
        message: e.to_string(),
    })?;
    if object.kind != gix::object::Kind::Blob {
        return Err(GitError::NotABlob {
            rev: oid.to_string(),
            path: String::new(),
            actual: match object.kind {
                gix::object::Kind::Tree => "directory",
                gix::object::Kind::Commit => "commit",
                _ => "tag",
            },
        });
    }
    let mut blob = object.try_into_blob().map_err(|e| GitError::Odb {
        message: e.to_string(),
    })?;
    // Unlike [`read_blob`], the size check happens AFTER the read rather
    // than as a cheap ODB header lookup first: this entry point is
    // addressed by a blob id, not by a tree entry, so there is no
    // already-resolved header to consult. It is a review-document
    // pre-flight, not a hot path, and the cap still bounds what LEAVES this
    // function — the difference is documented rather than silently absorbed.
    let size = blob.data.len() as u64;
    if size > max_bytes {
        return Err(GitError::TooLarge {
            rev: oid.to_string(),
            path: String::new(),
            size,
            cap: max_bytes,
        });
    }
    Ok(blob.take_data())
}
