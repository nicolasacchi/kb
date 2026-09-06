//! SEC-13 — path containment for working-tree reads.
//!
//! `routes::safe_rel_path` rejects `..`, absolute paths and Windows
//! prefixes **lexically**. That is a necessary first check and it stays
//! exactly where it is — but it is not a containment proof: `repo.path
//! .join(rel)` then goes through `std::fs`, which FOLLOWS SYMLINKS. A repo
//! containing `docs/secrets -> /home/user/.ssh` (a plain file the repo
//! itself can commit, or that any agent/build script can drop into the
//! working tree) makes `?path=docs/secrets/id_rsa` a lexically-clean,
//! escape-free request that reads outside the repo.
//!
//! This module applies invariant #27's own discipline — *"stored
//! `Doc.path` is canonical; the indexer's `prepare_doc` stores
//! `paths::canonical_abs(path)` and `reconcile` walks the canonical root,
//! so symlinked source roots don't desync"* — at READ time: canonicalise
//! BOTH sides and assert the joined path is still under the root.
//!
//! # Why the deepest-existing-ancestor walk
//!
//! `Path::canonicalize` fails with `ENOENT` on a path that does not exist,
//! and "not found in the working tree" is an ordinary, expected outcome of
//! `GET /api/file` (a caller browsing a ref that has a file `HEAD` does
//! not). Canonicalising the deepest ancestor that DOES exist and appending
//! the remaining literal components gives an answer for a missing leaf
//! without ever inventing one: every symlink on the existing part is
//! resolved, and the non-existent tail cannot itself be a symlink (it does
//! not exist). A missing file therefore still reaches the caller as its
//! own honest 404 rather than being masked by a containment 403.
//!
//! # What this does NOT do
//!
//! It does not make the read TOCTOU-proof — the path could be replaced by
//! a symlink between the check and the `std::fs::read`. Closing that would
//! need `openat2(RESOLVE_BENEATH)`, a Linux-only syscall with no portable
//! wrapper in this crate's dependency graph. The threat this guard
//! addresses is a *resident* symlink (committed, or dropped by a build
//! script), not a racing attacker who already has write access to the
//! repo — and an attacker with that access can simply commit the secret.
//! Stated here so the gap is a recorded scope decision, not an oversight.

use crate::routes::ApiError;
use axum::http::StatusCode;
use std::path::{Path, PathBuf};

/// `urn:kb:errors:path-outside-repo`.
pub const ERR_PATH_OUTSIDE_REPO: &str = "urn:kb:errors:path-outside-repo";

/// Canonicalise `root` and `root.join(rel)` and assert containment.
///
/// Returns the path to actually open — the ORIGINAL join, not the
/// canonical form, so downstream error messages and the mirror's own
/// path-keyed state keep using the configured spelling of the repo root
/// (invariant #27 canonicalises for COMPARISON; the stored/served path
/// shape is a separate contract this guard must not change).
pub fn contained_abs_path(root: &Path, rel: &str) -> Result<PathBuf, ApiError> {
    let joined = root.join(rel);
    let canonical_root = canonicalize_lenient(root);
    let canonical_target = canonicalize_lenient(&joined);
    if canonical_target.starts_with(&canonical_root) {
        Ok(joined)
    } else {
        Err(outside_repo(rel))
    }
}

/// The typed refusal — 403, `urn:kb:errors:path-outside-repo`. Names the
/// repo-relative path the caller asked for and nothing about where it
/// actually resolved to (a containment refusal must not become a
/// filesystem oracle for paths outside the repo).
pub fn outside_repo(rel: &str) -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        format!("{rel}: resolves outside the repository root"),
    )
    .with_problem_type(ERR_PATH_OUTSIDE_REPO)
}

/// `canonicalize`, degrading to the deepest existing ancestor plus the
/// literal remainder — see the module doc.
fn canonicalize_lenient(path: &Path) -> PathBuf {
    if let Ok(c) = path.canonicalize() {
        return c;
    }
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    let mut cursor = path.to_path_buf();
    while let Some(parent) = cursor.parent().map(Path::to_path_buf) {
        let Some(name) = cursor.file_name().map(|n| n.to_os_string()) else {
            break;
        };
        suffix.push(name);
        if let Ok(c) = parent.canonicalize() {
            let mut out = c;
            for part in suffix.iter().rev() {
                out.push(part);
            }
            return out;
        }
        if parent.as_os_str().is_empty() {
            break;
        }
        cursor = parent;
    }
    // Nothing on the chain exists (a fully synthetic path) — the literal
    // form is the best answer available, and a containment check against a
    // canonical root will simply refuse it unless it is genuinely under it.
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_relative_path_is_contained() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/main.rs"), b"fn main() {}").unwrap();
        let got = contained_abs_path(tmp.path(), "src/main.rs").unwrap();
        assert_eq!(got, tmp.path().join("src/main.rs"));
    }

    #[test]
    fn a_missing_file_is_contained_so_the_route_can_404_honestly() {
        let tmp = tempfile::tempdir().unwrap();
        // No file on disk: containment must PASS (the caller gets the
        // route's own 404), not be masked by a 403.
        let got = contained_abs_path(tmp.path(), "nope/deeper/missing.rs").unwrap();
        assert_eq!(got, tmp.path().join("nope/deeper/missing.rs"));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_escape_is_refused() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("id_rsa"), b"PRIVATE KEY").unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), repo.path().join("escape")).unwrap();

        let err = contained_abs_path(repo.path(), "escape/id_rsa").unwrap_err();
        assert_eq!(err.status_code(), StatusCode::FORBIDDEN);
        assert_eq!(err.problem_type(), Some(ERR_PATH_OUTSIDE_REPO));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_that_stays_inside_the_repo_is_allowed() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("real")).unwrap();
        std::fs::write(repo.path().join("real/app.rb"), b"# ok").unwrap();
        std::os::unix::fs::symlink(repo.path().join("real"), repo.path().join("link")).unwrap();
        assert!(contained_abs_path(repo.path(), "link/app.rb").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_repo_root_does_not_false_positive() {
        // Invariant #27's motivating shape: the configured root is itself a
        // symlink (WSL/bind mounts). Canonicalising BOTH sides is what
        // keeps an ordinary read inside it from looking like an escape.
        let real = tempfile::tempdir().unwrap();
        std::fs::write(real.path().join("a.rs"), b"//").unwrap();
        let holder = tempfile::tempdir().unwrap();
        let link = holder.path().join("repo-link");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();
        assert!(contained_abs_path(&link, "a.rs").is_ok());
    }
}
