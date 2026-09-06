//! Consistent snapshot helper for `kb backup` (B1).
//!
//! The pre-B1 backup tar'd the live `index.db` (plus its `-wal`/`-shm`
//! sidecars) while the daemon could be mid-write — capturing a torn,
//! half-applied transaction. `VACUUM INTO` instead reads a
//! transactionally-consistent view under WAL and writes a fresh,
//! defragmented, standalone database file (no WAL/SHM to reconcile), so a
//! snapshot taken while the daemon writes is never torn. It needs no
//! cooperation from the running daemon.

use crate::config::BackupSection;
use crate::Result;
use rusqlite::Connection;
use std::path::Path;
use std::process::Command;

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
}
