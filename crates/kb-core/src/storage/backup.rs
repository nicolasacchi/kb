//! Consistent snapshot helper for `kb backup` (B1).
//!
//! The pre-B1 backup tar'd the live `index.db` (plus its `-wal`/`-shm`
//! sidecars) while the daemon could be mid-write — capturing a torn,
//! half-applied transaction. `VACUUM INTO` instead reads a
//! transactionally-consistent view under WAL and writes a fresh,
//! defragmented, standalone database file (no WAL/SHM to reconcile), so a
//! snapshot taken while the daemon writes is never torn. It needs no
//! cooperation from the running daemon.
//!
//! [`write_kb_export`] packs that snapshot into
//! `<exports>/<kb>-YYYYMMDD-HHMMSS.tar.gz` for the daemon's
//! `[backup] schedule_hours` task. It invokes `tar` directly and must not
//! exec the `kb` binary.

use crate::config::BackupSection;
use crate::paths::KbPaths;
use crate::types::KbName;
use crate::Result;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

/// Write a transactionally-consistent copy of the sqlite database at
/// `src` to `dest` via `VACUUM INTO`.
///
/// Opens a fresh connection and does NOT run migrations (that would
/// mutate the live DB). `dest` must not already exist — sqlite refuses to
/// overwrite an existing file. The destination path is escaped into the
/// statement as a single-quoted literal (`VACUUM INTO` predates bound
/// parameters on some sqlite builds); callers pass a controlled staging
/// path, but the escape keeps a stray apostrophe from breaking the SQL.
pub fn vacuum_into(src: &Path, dest: &Path) -> Result<()> {
    let dest_str = dest.to_str().ok_or_else(|| {
        crate::Error::Storage(format!("backup: non-utf8 destination {}", dest.display()))
    })?;
    let conn = Connection::open(src)
        .map_err(|e| crate::Error::Storage(format!("backup: open {}: {e}", src.display())))?;
    let sql = format!("VACUUM INTO '{}'", dest_str.replace('\'', "''"));
    conn.execute(&sql, [])
        .map_err(|e| crate::Error::Storage(format!("backup: VACUUM INTO {dest_str}: {e}")))?;
    Ok(())
}

/// Outcome of an attempted GC-B4 off-host copy. `run_remote_copy` always
/// returns `None` when `[backup]` isn't fully configured — callers can
/// treat `None` as "nothing to report".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteCopyOutcome {
    /// The command exited 0.
    Ok,
    /// The command ran and exited non-zero, or failed to spawn at all.
    /// `message` is loggable/printable as-is (never contains the
    /// destination's credentials — those live in `remote_dest`/argv,
    /// which the operator controls and is responsible for scrubbing from
    /// their own logs if sensitive).
    Failed { message: String },
}

/// Best-effort off-host copy of a completed local backup (GC-B4).
///
/// Runs `cfg.remote_cmd` (argv, `{src}`/`{dest}` substituted) via
/// `std::process::Command` — no shell, so no quoting/injection surface.
/// Returns `None` when `[backup]` isn't fully configured (the common
/// case — the feature is opt-in). A failure to spawn or a non-zero exit
/// is reported as `RemoteCopyOutcome::Failed`, never as an `Err` —
/// **the remote copy is best-effort and must never fail the backup
/// itself**; callers surface the outcome loudly (log + CLI exit
/// message) but keep the backup's own success.
pub fn run_remote_copy(cfg: &BackupSection, src: &Path) -> Option<RemoteCopyOutcome> {
    let argv = cfg.build_argv(src)?;
    // `is_configured`/`build_argv` guarantee argv is non-empty.
    let (program, args) = argv
        .split_first()
        .expect("build_argv yields non-empty argv");
    Some(match Command::new(program).args(args).output() {
        Ok(out) if out.status.success() => {
            tracing::info!(program, dest = %cfg.remote_dest.as_deref().unwrap_or(""), "backup: off-host copy succeeded");
            RemoteCopyOutcome::Ok
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let message = format!(
                "`{program}` exited {}: {}",
                out.status,
                stderr.trim().lines().next().unwrap_or("(no stderr)")
            );
            tracing::warn!(program, %message, "backup: off-host copy FAILED");
            RemoteCopyOutcome::Failed { message }
        }
        Err(e) => {
            let message = format!("failed to spawn `{program}`: {e}");
            tracing::warn!(program, %message, "backup: off-host copy FAILED");
            RemoteCopyOutcome::Failed { message }
        }
    })
}

/// Drop guard so a failed or cancelled export does not leave
/// `<exports>/.staging-*` behind.
struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `true` when a scheduled export of `kb` should not write a tarball.
///
/// Skip when `index_db` is missing (nothing to snapshot), or when the
/// newest `<exports>/<kb>-YYYYMMDD-HHMMSS.tar.gz` — by mtime, not by
/// name — is at least as new as the index and its `-wal` sidecar. A
/// missing exports dir or no matching tarball is not a skip: the first
/// tick must write one. Unreadable index metadata is not a skip either
/// (rewrite rather than drop a possible write). `-shm` is ignored: a
/// reader can touch it without a commit.
pub fn should_skip_scheduled_backup(index_db: &Path, exports: &Path, kb: &str) -> bool {
    if !index_db.is_file() {
        return true;
    }
    match newest_scheduled_tarball_mtime(exports, kb) {
        Some(since) => !index_written_since(index_db, since),
        None => false,
    }
}

/// Write one restore-compatible export tarball for `kb`.
///
/// Layout matches `kb backup`: `<kb>/index.db` (via [`vacuum_into`]),
/// `<kb>/lance/` and `<kb>/.review/` when present, and a sibling
/// `slates/` when the daemon has one. `tar` is executed directly — this
/// function must not shell out to the `kb` binary. A missing index is an
/// error (this does not create one). The caller decides whether an existing
/// index is unchanged and should be skipped.
pub async fn write_kb_export(paths: &KbPaths, kb: &KbName) -> Result<PathBuf> {
    // `Connection::open` creates a missing file. Refuse first so a scheduled
    // tick cannot invent an empty live index as a side effect of snapshotting.
    let sqlite_src = paths.kb_sqlite(kb);
    if !sqlite_src.is_file() {
        return Err(crate::Error::Storage(format!(
            "backup: kb {kb} has no index at {}",
            sqlite_src.display()
        )));
    }
    std::fs::create_dir_all(&paths.exports)?;
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let out = paths.exports.join(format!("{kb}-{stamp}.tar.gz"));
    let staging = paths
        .exports
        .join(format!(".staging-{kb}-{}-{stamp}", std::process::id()));
    let _cleanup = RemoveOnDrop(staging.clone());

    let staged_kb = staging.join(kb.as_str());
    let lance_src = paths.kb_lance(kb);
    let review_src = paths.kb_review_dir(kb);
    let slates_src = paths.state.join("slates");
    let staged_sqlite = staged_kb.join("index.db");
    let staged_lance = staged_kb.join("lance");
    let staged_review = staged_kb.join(".review");
    let staged_slates = staging.join("slates");

    let stage_sqlite = sqlite_src.clone();
    let stage_lance = lance_src.clone();
    let stage_review = review_src.clone();
    let stage_slates = slates_src.clone();
    let stage_kb = staged_kb.clone();
    let stage_sqlite_dest = staged_sqlite.clone();
    let stage_lance_dest = staged_lance.clone();
    let stage_review_dest = staged_review.clone();
    let stage_slates_dest = staged_slates.clone();
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&stage_kb)?;
        vacuum_into(&stage_sqlite, &stage_sqlite_dest)?;
        if stage_lance.exists() {
            copy_dir(&stage_lance, &stage_lance_dest)?;
        }
        if stage_review.exists() {
            copy_dir(&stage_review, &stage_review_dest)?;
        }
        if stage_slates.exists() {
            copy_dir(&stage_slates, &stage_slates_dest)?;
        }
        Ok::<(), crate::Error>(())
    })
    .await
    .map_err(|e| crate::Error::Storage(format!("backup staging task failed: {e}")))??;

    if staged_lance.exists() {
        let store = crate::storage::lance::Storage::open(&staged_lance, None)
            .await
            .map_err(|e| {
                crate::Error::Storage(format!(
                    "lance snapshot is inconsistent ({e}); a commit may have landed mid-copy"
                ))
            })?;
        store.count_rows().await.map_err(|e| {
            crate::Error::Storage(format!("lance snapshot failed validation ({e})"))
        })?;
    }

    let include_slates = staged_slates.exists();
    let tar_staging = staging.clone();
    let tar_out = out.clone();
    let tar_kb = kb.as_str().to_string();
    tokio::task::spawn_blocking(move || tar_tree(&tar_staging, &tar_out, &tar_kb, include_slates))
        .await
        .map_err(|e| crate::Error::Storage(format!("backup tar task failed: {e}")))??;
    Ok(out)
}

fn newest_scheduled_tarball_mtime(exports: &Path, kb: &str) -> Option<SystemTime> {
    let entries = std::fs::read_dir(exports).ok()?;
    let mut newest = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_scheduled_tarball_name(name, kb) {
            continue;
        }
        let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        newest = Some(match newest {
            Some(prev) if prev >= mtime => prev,
            _ => mtime,
        });
    }
    newest
}

/// `<kb>-YYYYMMDD-HHMMSS.tar.gz`. The stamp shape keeps `notes` from
/// matching `notes-extra-YYYYMMDD-HHMMSS.tar.gz`, and ignores `--out`
/// names and `.staging-*` dirs.
fn is_scheduled_tarball_name(name: &str, kb: &str) -> bool {
    if kb.is_empty() {
        return false;
    }
    let Some(rest) = name.strip_prefix(kb) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix('-') else {
        return false;
    };
    let Some(stamp) = rest.strip_suffix(".tar.gz") else {
        return false;
    };
    let b = stamp.as_bytes();
    b.len() == 15
        && b[8] == b'-'
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[9..].iter().all(u8::is_ascii_digit)
}

fn index_written_since(index_db: &Path, since: SystemTime) -> bool {
    match index_write_mtime(index_db) {
        Some(mtime) => mtime > since,
        None => true,
    }
}

fn index_write_mtime(index_db: &Path) -> Option<SystemTime> {
    let wal = sidecar(index_db, "-wal");
    newest_mtime([index_db, wal.as_path()])
}

fn sidecar(index_db: &Path, suffix: &str) -> PathBuf {
    let mut os = index_db.as_os_str().to_owned();
    os.push(suffix);
    PathBuf::from(os)
}

fn newest_mtime<'a>(paths: impl IntoIterator<Item = &'a Path>) -> Option<SystemTime> {
    let mut newest = None;
    for path in paths {
        let Ok(mtime) = std::fs::metadata(path).and_then(|m| m.modified()) else {
            continue;
        };
        newest = Some(match newest {
            Some(prev) if prev >= mtime => prev,
            _ => mtime,
        });
    }
    newest
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    for entry in walkdir::WalkDir::new(src) {
        let entry =
            entry.map_err(|e| std::io::Error::other(format!("walk {}: {e}", src.display())))?;
        let rel = entry
            .path()
            .strip_prefix(src)
            .map_err(|e| std::io::Error::other(format!("strip {}: {e}", entry.path().display())))?;
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Pack `staging` with `tar` directly. Not a shell, and not the `kb` binary.
fn tar_tree(staging: &Path, out: &Path, kb: &str, include_slates: bool) -> Result<()> {
    let mut cmd = Command::new("tar");
    cmd.arg("-czf")
        .arg(out)
        .arg("-C")
        .arg(staging)
        .arg("--")
        .arg(kb);
    if include_slates {
        cmd.arg("slates");
    }
    let status = cmd.status().map_err(|e| {
        crate::Error::Storage(format!("tar invocation failed (is `tar` installed?): {e}"))
    })?;
    if !status.success() {
        let _ = std::fs::remove_file(out);
        return Err(crate::Error::Storage(format!("tar exited {status}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vacuum_into_produces_a_consistent_openable_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("index.db");
        // A real, migrated DB with a row to copy.
        {
            let mut db = crate::storage::sqlite::Db::open(&src).unwrap();
            db.history_record_search("hello", 1_700_000_000, "operator")
                .unwrap();
        }
        let dest = tmp.path().join("snapshot.db");
        vacuum_into(&src, &dest).unwrap();
        assert!(dest.exists(), "snapshot not written");

        // The copy opens, passes integrity_check, and carries the row.
        let snap = Connection::open(&dest).unwrap();
        let integrity: String = snap
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(integrity, "ok", "snapshot failed integrity_check");
        let n: i64 = snap
            .query_row("SELECT count(*) FROM history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "snapshot lost the history row");
    }

    #[test]
    fn run_remote_copy_is_none_when_unconfigured() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("kb.tar.gz");
        std::fs::write(&src, b"tarball bytes").unwrap();
        assert_eq!(run_remote_copy(&BackupSection::default(), &src), None);
    }

    #[test]
    fn run_remote_copy_succeeds_and_substitutes_src_dest() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("kb.tar.gz");
        std::fs::write(&src, b"tarball bytes").unwrap();
        let dest = tmp.path().join("off-host-copy.tar.gz");
        let cfg = BackupSection {
            remote_cmd: Some(vec!["cp".into(), "{src}".into(), "{dest}".into()]),
            remote_dest: Some(dest.to_string_lossy().into_owned()),
            schedule_hours: None,
        };
        let outcome = run_remote_copy(&cfg, &src);
        assert_eq!(outcome, Some(RemoteCopyOutcome::Ok));
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            b"tarball bytes",
            "remote {{dest}} did not receive the src bytes"
        );
    }

    #[test]
    fn run_remote_copy_surfaces_nonzero_exit_as_failed_without_erroring() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("kb.tar.gz");
        std::fs::write(&src, b"tarball bytes").unwrap();
        let script = tmp.path().join("fail-uploader.sh");
        std::fs::write(&script, "#!/bin/sh\necho boom-from-uploader >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cfg = BackupSection {
            remote_cmd: Some(vec![
                script.to_string_lossy().into_owned(),
                "{src}".into(),
                "{dest}".into(),
            ]),
            remote_dest: Some("remote:bucket/path".into()),
            schedule_hours: None,
        };
        match run_remote_copy(&cfg, &src) {
            Some(RemoteCopyOutcome::Failed { message }) => {
                assert!(
                    message.contains("boom-from-uploader"),
                    "failure message should surface stderr: {message}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn run_remote_copy_surfaces_spawn_error_as_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("kb.tar.gz");
        std::fs::write(&src, b"tarball bytes").unwrap();
        let cfg = BackupSection {
            remote_cmd: Some(vec!["/no/such/uploader-binary-xyz".into(), "{src}".into()]),
            remote_dest: Some("remote:bucket/path".into()),
            schedule_hours: None,
        };
        match run_remote_copy(&cfg, &src) {
            Some(RemoteCopyOutcome::Failed { message }) => {
                assert!(
                    message.contains("failed to spawn"),
                    "expected a spawn-error message: {message}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn vacuum_into_refuses_to_overwrite_existing_dest() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("index.db");
        crate::storage::sqlite::Db::open(&src).unwrap();
        let dest = tmp.path().join("exists.db");
        std::fs::write(&dest, b"in the way").unwrap();
        assert!(
            vacuum_into(&src, &dest).is_err(),
            "VACUUM INTO must refuse an existing destination"
        );
    }

    fn set_mtime(path: &Path, when: SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    fn epoch_plus(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs)
    }

    #[test]
    fn scheduled_backup_skips_only_when_index_is_not_newer_than_its_tarball() {
        let tmp = tempfile::tempdir().unwrap();
        let exports = tmp.path().join("exports");
        std::fs::create_dir(&exports).unwrap();
        let index = tmp.path().join("index.db");
        std::fs::write(&index, b"db").unwrap();
        let older = epoch_plus(1_700_000_000);
        let newer = epoch_plus(1_700_003_600);
        set_mtime(&index, older);

        assert!(
            should_skip_scheduled_backup(&tmp.path().join("missing.db"), &exports, "notes"),
            "no index → nothing to snapshot"
        );
        assert!(
            !should_skip_scheduled_backup(&index, &exports, "notes"),
            "index with no tarball must be written"
        );

        // A sibling kb, a manual --out name, and a staging dir are not this kb's tarball.
        std::fs::write(exports.join("notes-extra-20260102-000000.tar.gz"), b"other").unwrap();
        std::fs::write(exports.join("notes-manual.tar.gz"), b"manual").unwrap();
        std::fs::create_dir(exports.join(".staging-notes-1-20260101-000000")).unwrap();
        assert!(
            !should_skip_scheduled_backup(&index, &exports, "notes"),
            "unrelated exports must not count as notes' newest tarball"
        );

        let by_name = exports.join("notes-20260102-000000.tar.gz");
        let by_mtime = exports.join("notes-20260101-000000.tar.gz");
        std::fs::write(&by_name, b"older-bytes").unwrap();
        std::fs::write(&by_mtime, b"newer-bytes").unwrap();
        set_mtime(&by_name, older);
        set_mtime(&by_mtime, newer);
        set_mtime(&index, older);
        assert!(
            should_skip_scheduled_backup(&index, &exports, "notes"),
            "newest tarball is by mtime, and the index is not newer"
        );

        set_mtime(&index, newer);
        assert!(
            should_skip_scheduled_backup(&index, &exports, "notes"),
            "equal mtime is not a write since the tarball"
        );

        set_mtime(&index, epoch_plus(1_700_003_601));
        assert!(
            !should_skip_scheduled_backup(&index, &exports, "notes"),
            "an index write after the newest tarball must not be skipped"
        );
    }

    #[test]
    fn scheduled_backup_treats_wal_mtime_as_an_index_write() {
        let tmp = tempfile::tempdir().unwrap();
        let exports = tmp.path().join("exports");
        std::fs::create_dir(&exports).unwrap();
        let index = tmp.path().join("index.db");
        let wal = tmp.path().join("index.db-wal");
        std::fs::write(&index, b"db").unwrap();
        std::fs::write(&wal, b"wal").unwrap();
        let tar = exports.join("notes-20260102-000000.tar.gz");
        std::fs::write(&tar, b"tar").unwrap();
        set_mtime(&index, epoch_plus(1_700_000_000));
        set_mtime(&tar, epoch_plus(1_700_000_010));
        set_mtime(&wal, epoch_plus(1_700_000_020));
        assert!(
            !should_skip_scheduled_backup(&index, &exports, "notes"),
            "a WAL newer than the tarball is a write the main db mtime can miss"
        );
    }

    #[tokio::test]
    async fn write_kb_export_packs_restore_layout_without_the_kb_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::paths::KbPaths::rooted_at(tmp.path(), "daemon");
        let kb = crate::types::KbName::new("notes").unwrap();
        let missing = write_kb_export(&paths, &kb).await.unwrap_err();
        assert!(
            missing.to_string().contains("no index"),
            "missing index must fail closed, got {missing}"
        );
        assert!(
            !paths.kb_sqlite(&kb).exists(),
            "a refused export must not create the live index"
        );

        std::fs::create_dir_all(paths.kb_state(&kb)).unwrap();
        {
            let mut db = crate::storage::sqlite::Db::open(&paths.kb_sqlite(&kb)).unwrap();
            db.history_record_search("hello", 1_700_000_000, "operator")
                .unwrap();
        }
        std::fs::create_dir_all(paths.kb_review_dir(&kb)).unwrap();
        std::fs::write(paths.kb_review_dir(&kb).join("c_abc.json"), b"{}").unwrap();
        let slate = paths.state.join("slates").join("proj");
        std::fs::create_dir_all(&slate).unwrap();
        std::fs::write(slate.join("meta.json"), b"{}").unwrap();

        let out = write_kb_export(&paths, &kb).await.unwrap();
        assert_eq!(out.parent(), Some(paths.exports.as_path()));
        let name = out.file_name().unwrap().to_str().unwrap();
        assert!(
            is_scheduled_tarball_name(name, "notes"),
            "writer must use the scheduled name the skipper recognizes, got {name}"
        );

        let extract = tmp.path().join("extract");
        std::fs::create_dir(&extract).unwrap();
        let status = Command::new("tar")
            .arg("-xzf")
            .arg(&out)
            .arg("-C")
            .arg(&extract)
            .status()
            .unwrap();
        assert!(status.success(), "tar extract failed");
        assert!(extract.join("notes/index.db").is_file());
        assert!(extract.join("notes/.review/c_abc.json").is_file());
        assert!(extract.join("slates/proj/meta.json").is_file());
        let snap = Connection::open(extract.join("notes/index.db")).unwrap();
        let n: i64 = snap
            .query_row("SELECT count(*) FROM history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "tarball lost the vacuumed history row");

        for ent in std::fs::read_dir(&paths.exports).unwrap() {
            let n = ent.unwrap().file_name();
            let n = n.to_str().unwrap();
            assert!(!n.starts_with(".staging-"), "staging left behind: {n}");
        }

        // The name this writer just produced is what the skipper keys on.
        let ancient = epoch_plus(1_000);
        set_mtime(&paths.kb_sqlite(&kb), ancient);
        let wal = {
            let mut os = paths.kb_sqlite(&kb).into_os_string();
            os.push("-wal");
            PathBuf::from(os)
        };
        if wal.exists() {
            set_mtime(&wal, ancient);
        }
        assert!(
            should_skip_scheduled_backup(&paths.kb_sqlite(&kb), &paths.exports, "notes"),
            "an index older than the tarball this writer just produced must be skipped"
        );
    }
}
