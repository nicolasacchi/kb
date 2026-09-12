//! `kb-sibling/1` — the contract two kb daemons (kb and kb-code) use to
//! recognise each other and to refuse a schema they can't understand.
//!
//! Two halves, both deliberately HARD:
//!
//! 1. **Schema-epoch boot guard** ([`refuse_if_volume_ahead`]) — every
//!    sqlite volume a daemon opens carries refinery's own
//!    `refinery_schema_history`; its `MAX(version)` is the VOLUME epoch, and
//!    the highest version among the binary's EMBEDDED migrations is the
//!    BINARY epoch. Volume > binary means an older binary has been pointed
//!    at state a newer one already forward-migrated: refinery happily
//!    no-ops (it only ever applies FORWARD), the daemon boots, `/healthz`
//!    goes green, and every read that touches a column the old binary
//!    doesn't know about fails at request time. That is exactly the 13.5 h
//!    kbc outage (2026-08, a deploy rollback pairing an old binary with a
//!    forward-migrated volume). The guard turns it into a refused boot with
//!    both epochs named.
//! 2. **Version Hello** ([`SIBLING_PROTOCOL`]/[`SIBLING_MAJOR`]) — carried
//!    on each daemon's `GET /api/identity` beside its own
//!    [`schema_epoch`](crate::storage::sqlite::schema_epoch), so a sibling
//!    client can handshake BEFORE its first real call and fail closed on a
//!    mismatch instead of discovering it mid-request. rust-analyzer's
//!    advisory-only handshake is the counter-example: advisory IS the
//!    documented failure mode, so both halves of this contract are hard.
//!
//! This module is deliberately pure + tiny: constants, two reads, one
//! comparison. It knows nothing about HTTP — the Hello fields are assembled
//! by each daemon's identity route, which is the only place that knows its
//! own name.

use rusqlite::Connection;
use std::path::Path;

/// Wire value of `sibling_protocol` on both daemons' `/api/identity`. A
/// sibling client compares it EXACTLY — a different string is a different
/// contract, not a newer one.
pub const SIBLING_PROTOCOL: &str = "kb-sibling/1";

/// Wire value of `sibling_major`. Bumping the major means the previous
/// major's clients must fail closed rather than guess.
pub const SIBLING_MAJOR: u32 = 1;

/// refinery's bookkeeping table — the same default name both crates'
/// `embed_migrations!` runners write to (neither calls
/// `set_migration_table_name`).
const HISTORY_TABLE: &str = "refinery_schema_history";

/// A volume whose schema is NEWER than the binary that just opened it.
/// Boot refusal, never a warning — see the module doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaEpochRefusal {
    pub db_path: String,
    pub volume_epoch: u32,
    pub binary_epoch: u32,
}

impl std::fmt::Display for SchemaEpochRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "refusing to boot: schema epoch V{volume} on disk at {path} is NEWER than \
             this binary's V{binary} — this binary is older than the on-disk schema; \
             deploy a binary >= epoch V{volume} or restore the state backup matching \
             epoch V{binary}",
            volume = self.volume_epoch,
            binary = self.binary_epoch,
            path = self.db_path,
        )
    }
}

impl std::error::Error for SchemaEpochRefusal {}

/// Highest version among a binary's EMBEDDED migrations — its schema epoch.
/// Read from the runner rather than hardcoded so it can never drift from
/// `crates/*/migrations/` (which are immutable and only ever grow).
/// `0` for a migration set that is somehow empty, which compares safely
/// against any volume.
pub fn binary_epoch(runner: &refinery::Runner) -> u32 {
    runner
        .get_migrations()
        .iter()
        // refinery 0.9 widened `version()` to i32 (the int8-versions prep);
        // kb's embedded migrations are V<prefix>__ named and always
        // positive, so a negative can only be a bug — clamp it to 0, which
        // compares safely low against any volume epoch.
        .map(|m| u32::try_from(m.version()).unwrap_or(0))
        .max()
        .unwrap_or(0)
}

/// `MAX(version)` in the on-disk refinery history, or `None` when this
/// volume has never been migrated (a brand-new file, or one whose history
/// table doesn't exist yet — the first-boot case, which is never a refusal).
pub fn volume_epoch(conn: &Connection) -> rusqlite::Result<Option<u32>> {
    let exists: bool = conn.query_row(
        "SELECT count(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [HISTORY_TABLE],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(None);
    }
    // `MAX()` over an empty table is one NULL row, hence the Option.
    let max: Option<i64> = conn.query_row(
        &format!("SELECT MAX(version) FROM {HISTORY_TABLE}"),
        [],
        |row| row.get(0),
    )?;
    Ok(max.map(|v| u32::try_from(v.max(0)).unwrap_or(u32::MAX)))
}

/// The guard itself: call it on a freshly-opened connection BEFORE running
/// migrations, so a refused boot never touches the volume at all.
///
/// A read failure against `refinery_schema_history` is NOT a refusal — it
/// is passed through to the caller, who is about to run migrations on that
/// same connection and will surface the real error there.
pub fn refuse_if_volume_ahead(
    conn: &Connection,
    db_path: &Path,
    binary_epoch: u32,
) -> Result<(), SchemaEpochRefusal> {
    let Ok(Some(volume)) = volume_epoch(conn) else {
        return Ok(());
    };
    if volume > binary_epoch {
        return Err(SchemaEpochRefusal {
            db_path: db_path.display().to_string(),
            volume_epoch: volume,
            binary_epoch,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fabricate refinery's own bookkeeping table with one row at
    /// `version` — the tests never touch a real migration (those are
    /// immutable), they synthesise the history a newer binary would leave.
    fn fake_history(conn: &Connection, version: i64) {
        conn.execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS {HISTORY_TABLE} (
                 version INT4 PRIMARY KEY,
                 name VARCHAR(255),
                 applied_on VARCHAR(255),
                 checksum VARCHAR(255)
             );"
        ))
        .unwrap();
        conn.execute(
            &format!(
                "INSERT INTO {HISTORY_TABLE} (version, name, applied_on, checksum) \
                 VALUES (?1, 'fabricated', '', '0')"
            ),
            [version],
        )
        .unwrap();
    }

    #[test]
    fn a_volume_with_no_history_table_is_a_first_boot_not_a_refusal() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(volume_epoch(&conn).unwrap(), None);
        assert!(refuse_if_volume_ahead(&conn, Path::new("/tmp/index.db"), 42).is_ok());
    }

    #[test]
    fn an_empty_history_table_is_also_not_a_refusal() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE {HISTORY_TABLE} (version INT4 PRIMARY KEY, name VARCHAR(255), \
             applied_on VARCHAR(255), checksum VARCHAR(255));"
        ))
        .unwrap();
        assert_eq!(volume_epoch(&conn).unwrap(), None);
        assert!(refuse_if_volume_ahead(&conn, Path::new("/tmp/index.db"), 1).is_ok());
    }

    #[test]
    fn equal_epochs_boot_and_an_older_volume_boots_too() {
        let conn = Connection::open_in_memory().unwrap();
        fake_history(&conn, 40);
        assert_eq!(volume_epoch(&conn).unwrap(), Some(40));
        assert!(refuse_if_volume_ahead(&conn, Path::new("/tmp/index.db"), 40).is_ok());
        assert!(refuse_if_volume_ahead(&conn, Path::new("/tmp/index.db"), 41).is_ok());
    }

    #[test]
    fn a_newer_volume_refuses_and_names_both_epochs_and_the_path() {
        let conn = Connection::open_in_memory().unwrap();
        fake_history(&conn, 1_040);
        let err =
            refuse_if_volume_ahead(&conn, Path::new("/srv/kb/index.db"), 40).expect_err("refusal");
        assert_eq!(err.volume_epoch, 1_040);
        assert_eq!(err.binary_epoch, 40);
        let msg = err.to_string();
        assert!(msg.contains("refusing to boot"), "{msg}");
        assert!(msg.contains("V1040"), "{msg}");
        assert!(msg.contains("V40"), "{msg}");
        assert!(msg.contains("/srv/kb/index.db"), "{msg}");
    }

    /// The MAX, not the last-inserted row — refinery's history is not
    /// ordered by insertion in general (a repaired/backfilled row can land
    /// out of order).
    #[test]
    fn volume_epoch_is_the_max_version_not_the_last_row() {
        let conn = Connection::open_in_memory().unwrap();
        fake_history(&conn, 9);
        fake_history(&conn, 31);
        fake_history(&conn, 12);
        assert_eq!(volume_epoch(&conn).unwrap(), Some(31));
    }

    #[test]
    fn the_hello_constants_are_the_frozen_wire_values() {
        assert_eq!(SIBLING_PROTOCOL, "kb-sibling/1");
        assert_eq!(SIBLING_MAJOR, 1);
    }
}
