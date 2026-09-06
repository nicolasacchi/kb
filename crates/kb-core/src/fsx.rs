//! Filesystem primitives shared across modules.
//!
//! THE atomic-write implementation — sibling `<file>.tmp.<pid>.<rand>` +
//! rename + best-effort parent-dir fsync. Same-filesystem rename is atomic
//! on POSIX, so a concurrent reader (the watcher, an export walk, a GET
//! handler) sees either the old or the new complete file, never a torn
//! one. The tmp name never ends in an indexable extension (`.html`/`.md`),
//! so the watcher's `is_indexable` filter skips it.
//!
//! Grew out of three per-module copies (`review`, `meta_edit`, `config`)
//! that had drifted: config's used a FIXED tmp name (concurrent writers
//! could clobber each other's staging file) and skipped the dir fsync.

use crate::{Error, Result};
use std::path::Path;

/// Atomically replace `path` with `bytes`, creating the parent dir.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::BadRequest(format!("path {} has no parent dir", path.display())))?;
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        "{}.tmp.{}.{}",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("file"),
        std::process::id(),
        rand_suffix(),
    ));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    // Best-effort parent fsync so the renamed entry survives an immediate
    // crash. Failure here is a durability hint, not a correctness problem
    // — the write itself already landed — so trace it rather than failing.
    if let Err(e) = std::fs::File::open(parent).and_then(|d| d.sync_all()) {
        tracing::debug!(
            dir = %parent.display(),
            error = %e,
            "parent-dir fsync after atomic write failed",
        );
    }
    Ok(())
}

/// Collision-avoidance suffix for staging files: subsec nanos ⊕ pid,
/// hex. Not cryptographic — just unique enough that two writers staging
/// the same target in the same instant don't share a tmp name.
fn rand_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    format!("{:08x}", nanos.wrapping_add(pid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_creates_parents_and_replaces_content() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nested/deep/file.json");
        write_atomic(&target, b"one").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"one");
        write_atomic(&target, b"two").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"two");
        // No staging litter left behind.
        let leftovers: Vec<_> = std::fs::read_dir(target.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "staging files left: {leftovers:?}");
    }

    #[test]
    fn write_atomic_rejects_rootlike_path() {
        assert!(write_atomic(Path::new("/"), b"x").is_err());
    }
}
