//! RS-U3 — the store manifest (`<store>/kb-code-store.json`) and the
//! per-store daemon lock (README §5.1, design-internal-store §3.1/§4.2).
//!
//! The manifest ties a directory to the DB row that owns it: `uuid` and
//! `store_key` must match `review_stores` at open, or the store is
//! `broken` with `manifest-mismatch` — a restored DB pointing at a
//! different store's directory (or a copied directory) is never silently
//! adopted. The manifest carries no secret and no path.
//!
//! The lock is an exclusive, non-blocking `flock(2)` on
//! `<root>/<uuid>.lock`, held for the daemon's lifetime once the store is
//! opened or seeded. A second daemon pointed at the same root cannot take
//! it and reports the store `store-locked` instead of racing the first
//! one's fetches. The lock file sits BESIDE the store directory rather
//! than inside it (design §4.2 puts it inside) so the same lock also
//! guards seeding, before `<uuid>.git` exists, and the boot sweep of
//! `.seed-<uuid>.tmp` leftovers (only a directory whose lock is free is
//! stale).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The manifest file name inside a store directory.
pub const MANIFEST_NAME: &str = "kb-code-store.json";
/// Manifest schema string.
pub const MANIFEST_SCHEMA: &str = "kb-code-store/1";

/// `<store>/kb-code-store.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: String,
    pub format: u32,
    pub uuid: String,
    pub store_key: String,
    /// Unix seconds.
    pub created_at: i64,
}

impl Manifest {
    pub fn new(uuid: &str, store_key: &str, created_at: i64) -> Self {
        Self {
            schema: MANIFEST_SCHEMA.into(),
            format: 1,
            uuid: uuid.into(),
            store_key: store_key.into(),
            created_at,
        }
    }
}

/// Why a store directory does not match its DB row.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestProblem {
    #[error("store directory is missing")]
    DirMissing,
    #[error("store manifest is missing")]
    Missing,
    #[error("store manifest is unreadable: {0}")]
    Unreadable(String),
    #[error("store manifest does not match the database ({field})")]
    Mismatch { field: &'static str },
}

impl ManifestProblem {
    /// The `state_json.code` a problem records.
    pub fn code(&self) -> &'static str {
        match self {
            Self::DirMissing => "store-dir-missing",
            Self::Missing | Self::Unreadable(_) | Self::Mismatch { .. } => "manifest-mismatch",
        }
    }
}

/// Write the manifest into `dir` and fsync it.
pub fn write(dir: &Path, m: &Manifest) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(m).map_err(std::io::Error::other)?;
    let path = dir.join(MANIFEST_NAME);
    let mut f = File::create(&path)?;
    f.write_all(&bytes)?;
    f.write_all(b"\n")?;
    f.sync_all()
}

/// Read `dir`'s manifest.
pub fn read(dir: &Path) -> Result<Manifest, ManifestProblem> {
    if !dir.is_dir() {
        return Err(ManifestProblem::DirMissing);
    }
    let bytes = match std::fs::read(dir.join(MANIFEST_NAME)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(ManifestProblem::Missing),
        Err(e) => return Err(ManifestProblem::Unreadable(e.kind().to_string())),
    };
    serde_json::from_slice(&bytes).map_err(|e| ManifestProblem::Unreadable(e.to_string()))
}

/// Check `dir`'s manifest against the DB row's `uuid` + `store_key`.
pub fn check(dir: &Path, uuid: &str, store_key: &str) -> Result<Manifest, ManifestProblem> {
    let m = read(dir)?;
    if m.schema != MANIFEST_SCHEMA {
        return Err(ManifestProblem::Mismatch { field: "schema" });
    }
    if m.uuid != uuid {
        return Err(ManifestProblem::Mismatch { field: "uuid" });
    }
    if m.store_key != store_key {
        return Err(ManifestProblem::Mismatch { field: "store_key" });
    }
    Ok(m)
}

/// `<root>/<uuid>.lock`.
pub fn lock_path(root: &Path, uuid: &str) -> PathBuf {
    root.join(format!("{uuid}.lock"))
}

/// An exclusive `flock` on a store, released on drop.
#[derive(Debug)]
pub struct StoreLock {
    _file: File,
    path: PathBuf,
}

impl StoreLock {
    /// Take the lock without blocking. `Ok(None)` = another process holds
    /// it (`store-locked`).
    pub fn try_acquire(root: &Path, uuid: &str) -> std::io::Result<Option<Self>> {
        let path = lock_path(root, uuid);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        // SAFETY: `file` owns a valid descriptor for the whole call.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(Some(Self { _file: file, path }));
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Ok(None)
        } else {
            Err(err)
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_round_trips_and_mismatches_are_named() {
        let d = tempfile::tempdir().unwrap();
        let m = Manifest::new("u-1", "github.com/acme/widgets", 7);
        write(d.path(), &m).unwrap();
        assert_eq!(
            check(d.path(), "u-1", "github.com/acme/widgets").unwrap(),
            m
        );
        assert_eq!(
            check(d.path(), "u-2", "github.com/acme/widgets"),
            Err(ManifestProblem::Mismatch { field: "uuid" })
        );
        assert_eq!(
            check(d.path(), "u-1", "github.com/acme/gadgets"),
            Err(ManifestProblem::Mismatch { field: "store_key" })
        );
        assert_eq!(
            check(&d.path().join("nope"), "u-1", "k"),
            Err(ManifestProblem::DirMissing)
        );
        std::fs::remove_file(d.path().join(MANIFEST_NAME)).unwrap();
        assert_eq!(check(d.path(), "u-1", "k"), Err(ManifestProblem::Missing));
        std::fs::write(d.path().join(MANIFEST_NAME), b"{not json").unwrap();
        assert!(matches!(
            check(d.path(), "u-1", "k"),
            Err(ManifestProblem::Unreadable(_))
        ));
        assert_eq!(ManifestProblem::Missing.code(), "manifest-mismatch");
        assert_eq!(ManifestProblem::DirMissing.code(), "store-dir-missing");
    }

    #[test]
    fn a_second_holder_cannot_take_the_lock() {
        let d = tempfile::tempdir().unwrap();
        let first = StoreLock::try_acquire(d.path(), "u-1")
            .unwrap()
            .expect("free");
        // flock locks are per open file description, so a second open in
        // the same process contends exactly like a second daemon would.
        assert!(StoreLock::try_acquire(d.path(), "u-1").unwrap().is_none());
        assert!(StoreLock::try_acquire(d.path(), "u-2").unwrap().is_some());
        drop(first);
        assert!(StoreLock::try_acquire(d.path(), "u-1").unwrap().is_some());
    }
}
