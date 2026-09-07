//! V75-M1 — the pre-migration **backup gate**.
//!
//! A schema epoch is a ONE-WAY DOOR for a volume. `kb_core::sibling::
//! refuse_if_volume_ahead` (kb invariant #2) makes that safe in one
//! direction — an older binary refuses to boot rather than silently
//! regressing a migrated volume — but it offers the operator exactly one
//! remedy, and that remedy is "restore the state backup matching epoch
//! V\<n\>". The 13.5 h kbc outage was that sentence being true with no such
//! backup in existence.
//!
//! So: the FIRST boot that would carry a volume across [`REKEY_EPOCH`]
//! takes a `VACUUM INTO` snapshot beside the database, named for the epoch
//! it can restore to, and records a receipt. If the snapshot cannot be
//! written — no space, a read-only mount, a permissions change — the boot
//! is REFUSED with the reason, rather than migrating a volume nobody can
//! roll back.
//!
//! Three deliberate shapes:
//!
//! * **`VACUUM INTO`, not a file copy.** It reads through one consistent
//!   transaction, so a WAL that has not checkpointed is included; a naive
//!   `cp index.db` of a live WAL database restores to a torn state. The
//!   engine is `kb_core::storage::backup::vacuum_into` — kb's own
//!   `kb backup` uses it, and reusing it means one implementation of
//!   "snapshot a sqlite volume", not two.
//! * **Automatic, then refusing.** The alternative — refuse first and make
//!   the operator run a verb — is a boot loop for anyone who deploys
//!   without reading a changelog. Taking it automatically is the same
//!   posture kb-code already has for the scratch-ODB sweep: do the safe
//!   thing, say so loudly, and refuse only when the safe thing is
//!   impossible.
//! * **One override, and it is an env var, not a flag.** This runs inside
//!   `Store::open`, before any argument parsing this crate controls;
//!   [`OVERRIDE_ENV`] lets an operator who has their own snapshot
//!   (a filesystem-level one, a container volume clone) proceed. It logs a
//!   WARNING naming itself every time, because a silent bypass of a
//!   one-way-door guard is the guard not existing.
//!
//! `kb-code backup` takes the same snapshot on demand and writes the same
//! receipt, so an operator can front-run the gate before a deploy window.
//! It is a LOCAL FILE operation with no route and no daemon: there is no
//! new mutation surface here, nothing for the audit ledger to record, and
//! nothing reachable from a browser.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

/// The migration version at which the Workspace re-key lands (V0040). A
/// boot that carries a volume from BELOW this to at-or-above it is the
/// crossing this gate guards.
///
/// Deliberately a literal rather than `store::schema_epoch()`: the gate
/// must keep firing for exactly this crossing after V0041, V0042 … land,
/// and must NOT re-fire for every later migration (a routine additive
/// migration is not a one-way door of this size). A future migration that
/// IS one adds its own constant beside this one.
pub const REKEY_EPOCH: u32 = 40;

/// The receipt file, under the same directory as the database
/// (`<state>/kb-code/`).
pub const BACKUP_MARKER: &str = "backup.marker";

/// Set to `1`/`true` to proceed across [`REKEY_EPOCH`] when the snapshot
/// cannot be written. Logs a warning naming itself on every boot it is
/// honoured.
pub const OVERRIDE_ENV: &str = "KB_CODE_I_HAVE_A_BACKUP";

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("kb-code backup io error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("kb-code backup failed: {0}")]
    Snapshot(String),
    #[error(
        "refusing to migrate {db} across schema epoch V{epoch:04}: could not write the \
         pre-migration snapshot ({reason}). A schema epoch is a one-way door — an older \
         binary will refuse this volume afterwards and the only remedy is a restore. Free \
         space or fix permissions beside the database and restart, run `kb-code backup` \
         yourself, or set {env}=1 if you already hold a snapshot."
    )]
    Refused {
        db: String,
        epoch: u32,
        reason: String,
        env: &'static str,
    },
}

/// What was snapshotted, when, and from which epoch it restores.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BackupReceipt {
    pub schema: String,
    pub db_path: String,
    pub backup_path: String,
    /// The volume's epoch AT THE TIME of the snapshot — i.e. the epoch a
    /// restore of this file lands you on. `None` for a volume with no
    /// migration history yet.
    pub volume_epoch: Option<u32>,
    pub bytes: u64,
    pub taken_at: i64,
}

pub const RECEIPT_SCHEMA: &str = "kbc-backup/1";

/// `<db dir>/backup.marker`.
pub fn marker_path(db_path: &Path) -> PathBuf {
    db_dir(db_path).join(BACKUP_MARKER)
}

fn db_dir(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `<db>.pre-V0037.bak` — the epoch in the name is the one a restore
/// lands on, which is the number the operator has to match against a
/// binary.
pub fn snapshot_path(db_path: &Path, volume_epoch: Option<u32>) -> PathBuf {
    let name = db_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "index.db".to_string());
    let epoch = volume_epoch
        .map(|e| format!("V{e:04}"))
        .unwrap_or_else(|| "V0000".to_string());
    db_dir(db_path).join(format!("{name}.pre-{epoch}.bak"))
}

/// Read the receipt beside `db_path`, if one is there and parses. A
/// corrupt receipt reads as ABSENT — it may cost a redundant snapshot,
/// never a skipped one.
pub fn read_receipt(db_path: &Path) -> Option<BackupReceipt> {
    let text = std::fs::read_to_string(marker_path(db_path)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Is `receipt` a usable snapshot of THIS volume at THIS epoch? Requires
/// the recorded epoch to match and the file to still exist and be
/// non-empty — a receipt whose snapshot was deleted is not a backup.
pub fn is_fresh(receipt: &BackupReceipt, db_path: &Path, volume_epoch: Option<u32>) -> bool {
    receipt.volume_epoch == volume_epoch
        && receipt.db_path == db_path.display().to_string()
        && std::fs::metadata(&receipt.backup_path)
            .map(|m| m.len() > 0)
            .unwrap_or(false)
}

/// Take a snapshot of `db_path` and write the receipt beside it. Safe to
/// call while the daemon holds the database open: `VACUUM INTO` runs in a
/// read transaction.
pub fn take(db_path: &Path, volume_epoch: Option<u32>) -> Result<BackupReceipt, BackupError> {
    let dest = snapshot_path(db_path, volume_epoch);
    // `VACUUM INTO` refuses an existing destination. Removing one at the
    // SAME epoch is safe by construction — it is a snapshot of the same
    // schema generation of the same volume, which is what we are about to
    // write again.
    if dest.exists() {
        std::fs::remove_file(&dest).map_err(|e| BackupError::Io {
            path: dest.display().to_string(),
            source: e,
        })?;
    }
    kb_core::storage::backup::vacuum_into(db_path, &dest)
        .map_err(|e| BackupError::Snapshot(e.to_string()))?;
    let bytes = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    if bytes == 0 {
        return Err(BackupError::Snapshot(format!(
            "{} is empty after VACUUM INTO",
            dest.display()
        )));
    }
    let receipt = BackupReceipt {
        schema: RECEIPT_SCHEMA.to_string(),
        db_path: db_path.display().to_string(),
        backup_path: dest.display().to_string(),
        volume_epoch,
        bytes,
        taken_at: chrono::Utc::now().timestamp(),
    };
    let marker = marker_path(db_path);
    let text = serde_json::to_string_pretty(&receipt)
        .map_err(|e| BackupError::Snapshot(format!("serialize receipt: {e}")))?;
    std::fs::write(&marker, text + "\n").map_err(|e| BackupError::Io {
        path: marker.display().to_string(),
        source: e,
    })?;
    Ok(receipt)
}

/// Take a snapshot of the volume at `db_path`, naming it for the epoch
/// the volume is CURRENTLY on — the entry point `kb-code backup` uses.
///
/// Reads the epoch itself rather than taking it as an argument, so the
/// CLI needs no sqlite dependency of its own and there is exactly one
/// place that decides what a snapshot is called.
pub fn take_at_current_epoch(db_path: &Path) -> Result<BackupReceipt, BackupError> {
    let conn = Connection::open(db_path)
        .map_err(|e| BackupError::Snapshot(format!("open {}: {e}", db_path.display())))?;
    let epoch = kb_core::sibling::volume_epoch(&conn).ok().flatten();
    drop(conn);
    take(db_path, epoch)
}

/// Whether this boot would carry `volume_epoch` across [`REKEY_EPOCH`].
///
/// A volume with NO history (`None`) is a first boot: there is nothing to
/// lose and nothing to restore, so it is never a crossing.
pub fn crosses_rekey(volume_epoch: Option<u32>, binary_epoch: u32) -> bool {
    match volume_epoch {
        None => false,
        Some(v) => v < REKEY_EPOCH && binary_epoch >= REKEY_EPOCH,
    }
}

/// The gate itself. Call on a freshly-opened connection AFTER
/// `refuse_if_volume_ahead` and BEFORE the refinery runner.
///
/// Returns the receipt when a snapshot was taken or an existing fresh one
/// was reused, `None` when this boot is not a crossing (the overwhelmingly
/// common case, and a byte-identical no-op).
pub fn ensure_for_epoch_crossing(
    conn: &Connection,
    db_path: &Path,
    binary_epoch: u32,
) -> Result<Option<BackupReceipt>, BackupError> {
    let volume_epoch = kb_core::sibling::volume_epoch(conn).ok().flatten();
    if !crosses_rekey(volume_epoch, binary_epoch) {
        return Ok(None);
    }
    if let Some(existing) = read_receipt(db_path) {
        if is_fresh(&existing, db_path, volume_epoch) {
            tracing::info!(
                backup = %existing.backup_path,
                epoch = ?volume_epoch,
                "kb-code: reusing the existing pre-migration snapshot"
            );
            return Ok(Some(existing));
        }
    }
    match take(db_path, volume_epoch) {
        Ok(receipt) => {
            tracing::warn!(
                backup = %receipt.backup_path,
                bytes = receipt.bytes,
                from_epoch = ?volume_epoch,
                to_epoch = binary_epoch,
                "kb-code: took a pre-migration snapshot before crossing the Workspace re-key \
                 epoch — an older binary will refuse this volume afterwards"
            );
            Ok(Some(receipt))
        }
        Err(e) => {
            let overridden = std::env::var(OVERRIDE_ENV)
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            if overridden {
                tracing::warn!(
                    error = %e,
                    env = OVERRIDE_ENV,
                    "kb-code: could not write the pre-migration snapshot, but {} is set — \
                     migrating anyway on the operator's word",
                    OVERRIDE_ENV
                );
                return Ok(None);
            }
            Err(BackupError::Refused {
                db: db_path.display().to_string(),
                epoch: REKEY_EPOCH,
                reason: e.to_string(),
                env: OVERRIDE_ENV,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every epoch below is expressed RELATIVE to [`REKEY_EPOCH`]. The
    /// first cut of these tests hard-coded 37/38 and went red the moment
    /// the unit renumbered its migration behind a sibling's — a test that
    /// pins the constant it is testing to a literal is a test that fails
    /// for the wrong reason.
    const BEFORE: u32 = REKEY_EPOCH - 1;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn db_with_epoch(dir: &Path, epoch: Option<u32>) -> PathBuf {
        let path = dir.join("index.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE payload (x TEXT); INSERT INTO payload VALUES ('hi');")
            .unwrap();
        if let Some(e) = epoch {
            conn.execute_batch(
                "CREATE TABLE refinery_schema_history (
                     version INTEGER PRIMARY KEY, name TEXT, applied_on TEXT, checksum TEXT);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO refinery_schema_history VALUES (?1, 'x', '', '0')",
                [e],
            )
            .unwrap();
        }
        path
    }

    #[test]
    fn a_first_boot_is_not_a_crossing() {
        assert!(!crosses_rekey(None, REKEY_EPOCH));
    }

    #[test]
    fn only_the_step_over_the_rekey_epoch_is_a_crossing() {
        assert!(crosses_rekey(Some(BEFORE), REKEY_EPOCH));
        assert!(crosses_rekey(Some(1), REKEY_EPOCH + 2));
        // Already past it: a later additive migration is not this door.
        assert!(!crosses_rekey(Some(REKEY_EPOCH), REKEY_EPOCH + 1));
        assert!(!crosses_rekey(Some(REKEY_EPOCH + 1), REKEY_EPOCH + 2));
        // An older binary never migrates forward at all (and
        // `refuse_if_volume_ahead` has already refused this case).
        assert!(!crosses_rekey(Some(BEFORE), BEFORE));
    }

    #[test]
    fn the_gate_is_a_no_op_when_the_volume_is_already_past_the_epoch() {
        let dir = tmp();
        let path = db_with_epoch(dir.path(), Some(REKEY_EPOCH));
        let conn = Connection::open(&path).unwrap();
        let out = ensure_for_epoch_crossing(&conn, &path, REKEY_EPOCH).unwrap();
        assert!(out.is_none(), "no crossing, no snapshot");
        assert!(!marker_path(&path).exists(), "and no receipt written");
    }

    #[test]
    fn a_crossing_snapshots_the_volume_and_writes_a_receipt_naming_the_epoch() {
        let dir = tmp();
        let path = db_with_epoch(dir.path(), Some(BEFORE));
        let conn = Connection::open(&path).unwrap();
        let receipt = ensure_for_epoch_crossing(&conn, &path, REKEY_EPOCH)
            .unwrap()
            .expect("a crossing takes a snapshot");
        assert_eq!(receipt.volume_epoch, Some(BEFORE));
        assert!(
            receipt
                .backup_path
                .ends_with(&format!("index.db.pre-V{BEFORE:04}.bak")),
            "{}",
            receipt.backup_path
        );
        assert!(receipt.bytes > 0);
        assert!(std::path::Path::new(&receipt.backup_path).exists());
        // The snapshot is a real, openable volume carrying the same rows.
        let restored = Connection::open(&receipt.backup_path).unwrap();
        let n: i64 = restored
            .query_row("SELECT count(*) FROM payload", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        // And the receipt round-trips.
        let read = read_receipt(&path).expect("receipt on disk");
        assert_eq!(read, receipt);
        assert!(is_fresh(&read, &path, Some(BEFORE)));
    }

    #[test]
    fn a_second_crossing_boot_reuses_the_existing_snapshot() {
        let dir = tmp();
        let path = db_with_epoch(dir.path(), Some(BEFORE));
        let conn = Connection::open(&path).unwrap();
        let first = ensure_for_epoch_crossing(&conn, &path, REKEY_EPOCH)
            .unwrap()
            .unwrap();
        let second = ensure_for_epoch_crossing(&conn, &path, REKEY_EPOCH)
            .unwrap()
            .unwrap();
        assert_eq!(first, second, "the receipt is reused, not re-minted");
    }

    #[test]
    fn a_receipt_whose_snapshot_was_deleted_is_not_fresh() {
        let dir = tmp();
        let path = db_with_epoch(dir.path(), Some(BEFORE));
        let conn = Connection::open(&path).unwrap();
        let receipt = ensure_for_epoch_crossing(&conn, &path, REKEY_EPOCH)
            .unwrap()
            .unwrap();
        std::fs::remove_file(&receipt.backup_path).unwrap();
        assert!(
            !is_fresh(&receipt, &path, Some(BEFORE)),
            "a receipt is not a backup; the file is"
        );
    }

    #[test]
    fn a_receipt_from_a_different_epoch_is_not_fresh() {
        let dir = tmp();
        let path = db_with_epoch(dir.path(), Some(BEFORE));
        let conn = Connection::open(&path).unwrap();
        let receipt = ensure_for_epoch_crossing(&conn, &path, REKEY_EPOCH)
            .unwrap()
            .unwrap();
        assert!(!is_fresh(&receipt, &path, Some(BEFORE - 7)));
    }

    /// The refusal is the whole point of the gate: a volume that cannot be
    /// snapshotted must not be carried across the door. Provoked by
    /// putting a DIRECTORY where the snapshot file must go.
    #[test]
    fn a_snapshot_that_cannot_be_written_refuses_the_boot() {
        let dir = tmp();
        let path = db_with_epoch(dir.path(), Some(BEFORE));
        let conn = Connection::open(&path).unwrap();
        std::fs::create_dir(snapshot_path(&path, Some(BEFORE))).unwrap();
        let err = ensure_for_epoch_crossing(&conn, &path, REKEY_EPOCH)
            .expect_err("an unwritable snapshot refuses");
        let msg = err.to_string();
        assert!(msg.contains("one-way door"), "{msg}");
        assert!(msg.contains(OVERRIDE_ENV), "{msg}");
        assert!(msg.contains(&format!("V{REKEY_EPOCH:04}")), "{msg}");
    }
}
