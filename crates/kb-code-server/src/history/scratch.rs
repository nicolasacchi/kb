//! SEC-15 — a per-request scratch object directory for `git merge-tree
//! --write-tree`, plus the boot-time orphan sweep.
//!
//! # The finding
//!
//! `merge_check` runs `git merge-tree --write-tree`, which writes the
//! would-be merge result tree into the object database — and until V70-A2
//! that ODB was **the browsed repo's own**. Every conflict check on a
//! "read-first" daemon grew the operator's repo by a tree and some blobs,
//! unbounded, invisible, and never garbage-collected by anything kb-code
//! does. v7 turns one-per-click into an N×M fan-out.
//!
//! # The two environment variables, and why neither works alone
//!
//! * `GIT_OBJECT_DIRECTORY=<scratch>` redirects every object WRITE into
//!   the scratch dir. On its own it also makes the repo's REAL objects
//!   unreadable — git looks for objects in exactly one primary ODB — so
//!   the merge has nothing to merge and fails.
//! * `GIT_ALTERNATE_OBJECT_DIRECTORIES=<real objects>` adds the repo's own
//!   store back as a read-only alternate. Together: reads see everything,
//!   writes land only in the scratch.
//!
//! The real objects dir is resolved with `git rev-parse --git-path
//! objects` rather than assembled as `<root>/.git/objects`, so a LINKED
//! WORKTREE (whose `.git` is a *file* pointing at
//! `…/.git/worktrees/<name>`, and whose objects live in the common dir)
//! resolves correctly instead of naming a path that does not exist.
//!
//! # Where the scratch lives, and why not in the repo
//!
//! Under the daemon's own state dir (`<state>/kb-code/scratch/<id>`),
//! never inside the repo. A scratch dir under the repo would be an
//! untracked file the working-tree walker enumerates (SEC-13) and a write
//! storm for the live-mirror watcher (`crate::mirror`) — two bugs traded
//! for one.
//!
//! # Cleanup: a Drop guard AND a boot sweep
//!
//! [`ScratchOdb`] removes its directory on drop, which covers the ordinary
//! path and the error path. It does NOT cover a kill -9 mid-merge, so
//! [`sweep_orphans`] runs at boot and removes every leftover — the same
//! belt-and-braces shape `kb`'s own comment-attachment GC uses. The sweep
//! is deliberately unconditional (not age-gated): the daemon holding a
//! scratch dir is THIS process, which has not created any yet at boot.

use super::{HistoryError, Result};
use std::path::{Path, PathBuf};

/// The state-dir subdirectory every scratch ODB lives under.
pub const SCRATCH_DIR_NAME: &str = "scratch";

/// V75-M3 — the RFC 7807 `type` URN for
/// [`super::HistoryError::ScratchUnwritable`]. Lives beside its producer,
/// the same convention `security::paths`/`security::secrets` follow for
/// theirs. A caller seeing this knows the refusal is about the DAEMON'S
/// state dir, not about the repo it asked about.
pub const ERR_SCRATCH_UNWRITABLE: &str = "urn:kb:errors:scratch-unwritable";

/// A per-request scratch object directory, removed on drop.
#[derive(Debug)]
pub struct ScratchOdb {
    dir: PathBuf,
    alternates: PathBuf,
}

impl ScratchOdb {
    /// Create `<scratch_root>/<random>` and resolve `repo_root`'s real
    /// objects dir as the alternate.
    pub fn create(repo_root: &Path, scratch_root: &Path) -> Result<Self> {
        let alternates = real_objects_dir(repo_root)?;
        let dir = scratch_root.join(random_id());
        // `objects/` needs its two standard subdirs to be a usable ODB;
        // git creates fan-out dirs itself but expects `info`/`pack` to
        // exist (it creates them lazily too, but making them here keeps
        // the layout obvious to anyone who looks at a leftover).
        //
        // V75-M3 — a failure HERE is the read-only-mount refusal, not a
        // git failure. The browsed repo needs no write access at all
        // (exactly what the redirection above bought), so the only
        // directory a `merge-tree` lane can be blocked on for want of
        // write permission is THIS one, under the daemon's own state dir.
        // `HistoryError::ScratchUnwritable` names it, so a caller looks in
        // the right place; `Spawn` would have read as "git could not
        // start". `merge_check` inherits the same typed refusal by going
        // through this constructor.
        let unwritable = |e: std::io::Error| HistoryError::ScratchUnwritable {
            path: scratch_root.display().to_string(),
            reason: e.to_string(),
        };
        std::fs::create_dir_all(dir.join("info")).map_err(unwritable)?;
        std::fs::create_dir_all(dir.join("pack")).map_err(unwritable)?;
        Ok(Self { dir, alternates })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn alternates(&self) -> &Path {
        &self.alternates
    }
}

impl Drop for ScratchOdb {
    fn drop(&mut self) {
        // Best-effort: a failed removal leaves an orphan the boot sweep
        // collects. Never panics in a Drop.
        if let Err(e) = std::fs::remove_dir_all(&self.dir) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    dir = %self.dir.display(), error = %e,
                    "kb-code: could not remove a scratch object directory"
                );
            }
        }
    }
}

/// `git rev-parse --git-path objects`, absolutised against `repo_root` —
/// see the module doc for why this is not `<root>/.git/objects`.
fn real_objects_dir(repo_root: &Path) -> Result<PathBuf> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--git-path", "objects"])
        .output()
        .map_err(HistoryError::Spawn)?;
    if !out.status.success() {
        return Err(HistoryError::GitFailed {
            status: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    let rel = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if rel.is_empty() {
        return Err(HistoryError::GitFailed {
            status: -1,
            stderr: "git rev-parse --git-path objects returned nothing".to_string(),
        });
    }
    let p = PathBuf::from(&rel);
    Ok(if p.is_absolute() {
        p
    } else {
        repo_root.join(p)
    })
}

/// Remove every leftover scratch directory under `scratch_root`. Called
/// once from `bind_and_spawn`; never fatal to boot.
pub fn sweep_orphans(scratch_root: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(scratch_root) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
            && std::fs::remove_dir_all(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

/// 12 hex chars from `getrandom` — the crate's established id shape
/// (`annotations::new_annotation_id`), no uuid dependency.
fn random_id() -> String {
    let mut bytes = [0u8; 6];
    if getrandom::getrandom(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        bytes[..4].copy_from_slice(&nanos.to_le_bytes());
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn init_repo(dir: &Path) {
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "T"],
        ] {
            Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(&args)
                .output()
                .expect("git runs");
        }
    }

    #[test]
    fn the_alternate_points_at_the_repos_real_objects_dir() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let objects = real_objects_dir(repo.path()).unwrap();
        assert!(objects.ends_with("objects"), "{objects:?}");
        assert!(objects.exists(), "{objects:?}");
    }

    #[test]
    fn the_scratch_dir_is_created_then_removed_on_drop() {
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let root = tempfile::tempdir().unwrap();
        let path = {
            let s = ScratchOdb::create(repo.path(), root.path()).unwrap();
            assert!(s.dir().is_dir());
            assert!(s.dir().starts_with(root.path()));
            s.dir().to_path_buf()
        };
        assert!(!path.exists(), "the Drop guard must remove {path:?}");
    }

    #[test]
    fn the_scratch_dir_is_never_inside_the_repo() {
        // SEC-13/mirror-storm: a scratch dir under the repo would be an
        // untracked file the tree walker lists and the watcher storms on.
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let root = tempfile::tempdir().unwrap();
        let s = ScratchOdb::create(repo.path(), root.path()).unwrap();
        assert!(!s.dir().starts_with(repo.path()));
    }

    /// V75-M3 — the read-only-mount refusal is TYPED. Unix-only: the test
    /// needs a directory whose write bit is genuinely off.
    #[cfg(unix)]
    #[test]
    fn an_unwritable_scratch_root_is_a_typed_refusal_naming_the_directory() {
        use std::os::unix::fs::PermissionsExt;
        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let root = tempfile::tempdir().unwrap();
        let locked = root.path().join("locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();

        let got = ScratchOdb::create(repo.path(), &locked);
        // Restore before asserting so a failure still leaves a removable
        // temp dir behind.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        match got {
            Err(HistoryError::ScratchUnwritable { path, .. }) => {
                assert_eq!(path, locked.display().to_string());
            }
            // Running as root (some container CI images) ignores the mode
            // bits entirely, so there is no refusal to observe on such a
            // host. Skipping is honest; asserting would fail for a reason
            // that has nothing to do with this code.
            Ok(_) => eprintln!(
                "skipped: this uid can create a directory under mode 0500 (root?) — \
                 the ScratchUnwritable path is unobservable here"
            ),
            Err(other) => panic!("expected ScratchUnwritable, got {other:?}"),
        }
    }

    #[test]
    fn the_boot_sweep_removes_orphans() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("aaaa/info")).unwrap();
        std::fs::create_dir_all(root.path().join("bbbb/pack")).unwrap();
        assert_eq!(sweep_orphans(root.path()), 2);
        assert_eq!(sweep_orphans(root.path()), 0);
        // A missing root is not an error.
        assert_eq!(sweep_orphans(&root.path().join("nope")), 0);
    }
}
