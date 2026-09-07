//! V75-M1 — `rehearsal/1`: run the migration against a COPY of a real
//! volume and report what it did, before it is allowed near the real one.
//!
//! "Every milestone is a one-way door for the volume. Backup, migration
//! inventory, epoch bump, redeploy dry run and rollback-by-restore are a
//! standing exit item" — the v7 design's own risk register. A backup makes
//! the door reversible; a rehearsal is how you find out, on a throwaway
//! copy, whether you will need it.
//!
//! What it proves, in one pass:
//!
//! * the migration APPLIES to this volume's actual schema history
//!   (including whatever repairs `Store::open` performs on the way);
//! * the epoch moved, and to what;
//! * the backup gate fired and produced a real file;
//! * **no table lost or gained a row** — a re-key adds columns, and a
//!   per-table before/after census is the cheapest honest proof it did
//!   nothing else;
//! * the paged backfill TERMINATES and how long it takes on a volume this
//!   size, which is the number an operator needs before a deploy window.
//!
//! **The synthetic identity.** A copied volume's repositories are usually
//! not on this machine, so `crate::workspace::resolve_and_upsert` cannot
//! run and every `repos.workspace_id` would stay NULL — which would make
//! the backfill a no-op and the timing a lie. The rehearsal therefore
//! stamps a clearly-fake identity onto the COPY's `repos` rows so the
//! backfill genuinely writes every row. That value never touches the
//! source volume, and [`Rehearsal::synthetic_identity`] says so on the
//! wire.
//!
//! The copy itself is made with `VACUUM INTO`, not `cp`: a live volume's
//! WAL may not have checkpointed, and a torn copy would rehearse a
//! migration against a state that never existed.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::{BackfillReport, KeyedTable, REPO_KEYED_TABLES};

pub const SCHEMA: &str = "rehearsal/1";

/// A clearly-fake workspace id. It exists only inside the throwaway copy;
/// the shape is deliberately un-mistakable for a real `ws_` + 12 hex id.
pub const SYNTHETIC_WORKSPACE: &str = "ws_rehearsal!!";
pub const SYNTHETIC_WORKTREE: &str = "(rehearsal)";

#[derive(Debug, thiserror::Error)]
pub enum RehearsalError {
    #[error("no such volume: {0}")]
    NoSource(String),
    #[error("rehearsal io error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("rehearsal could not copy the volume: {0}")]
    Copy(String),
    #[error("rehearsal sqlite error: {0}")]
    Sqlite(String),
    #[error("rehearsal could not open the migrated copy: {0}")]
    Open(String),
}

type Result<T> = std::result::Result<T, RehearsalError>;

/// One table's before/after census.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TableRow {
    pub name: String,
    /// `None` when the table could not be counted (an fts5 contentless
    /// index, a shadow table) — stated, never silently zero.
    pub rows_before: Option<i64>,
    pub rows_after: Option<i64>,
    pub delta: Option<i64>,
    /// From [`REPO_KEYED_TABLES`]; `None` for a table the re-key does not
    /// classify (a blob-keyed derived table, a child table, an fts shadow).
    pub class: Option<&'static str>,
    pub key_column: Option<&'static str>,
    /// Rows carrying the key after the backfill, and rows still without
    /// one. A non-zero `unkeyed` on a keyed table is a PROBLEM.
    pub keyed: Option<i64>,
    pub unkeyed: Option<i64>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Rehearsal {
    pub schema: &'static str,
    pub source: String,
    pub work_dir: String,
    /// `true` when `--keep` left the copy on disk for inspection.
    pub kept: bool,
    pub epoch_before: Option<u32>,
    pub epoch_after: Option<u32>,
    /// The pre-migration snapshot the backup gate took on the COPY —
    /// `None` when the copy was already at or past the re-key epoch, which
    /// is itself a useful answer (this volume has already crossed).
    pub backup: Option<crate::backup::BackupReceipt>,
    pub synthetic_identity: bool,
    pub tables: Vec<TableRow>,
    pub backfill: BackfillReport,
    pub elapsed_ms: u64,
    pub ok: bool,
    /// Every reason `ok` is false, by name. Empty when `ok`.
    pub problems: Vec<String>,
}

fn table_names(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type IN ('table','view') \
             AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .map_err(|e| RehearsalError::Sqlite(e.to_string()))?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| RehearsalError::Sqlite(e.to_string()))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| RehearsalError::Sqlite(e.to_string()))
}

/// `count(*)`, or `None` when the table refuses to be counted. An fts5
/// contentless index is the known case; anything else is reported the same
/// honest way rather than crashing a rehearsal on a volume shape this
/// binary has not seen.
fn count_of(conn: &Connection, table: &str) -> Option<i64> {
    conn.query_row(&format!("SELECT count(*) FROM \"{table}\""), [], |r| {
        r.get(0)
    })
    .ok()
}

fn classified(name: &str) -> Option<&'static KeyedTable> {
    REPO_KEYED_TABLES.iter().find(|t| t.table == name)
}

/// Run the whole rehearsal. `source` is a kb-code `index.db`; nothing about
/// it is ever written.
pub fn rehearse(source: &Path, keep: bool) -> Result<Rehearsal> {
    let started = std::time::Instant::now();
    if !source.is_file() {
        return Err(RehearsalError::NoSource(source.display().to_string()));
    }
    let work = work_dir_for(source);
    std::fs::create_dir_all(&work).map_err(|e| RehearsalError::Io {
        path: work.display().to_string(),
        source: e,
    })?;
    let copy = work.join("index.db");

    // A consistent copy, not `cp` — see the module doc.
    kb_core::storage::backup::vacuum_into(source, &copy)
        .map_err(|e| RehearsalError::Copy(e.to_string()))?;

    let (epoch_before, before): (Option<u32>, Vec<(String, Option<i64>)>) = {
        let conn = Connection::open(&copy).map_err(|e| RehearsalError::Sqlite(e.to_string()))?;
        let epoch = kb_core::sibling::volume_epoch(&conn).ok().flatten();
        let names = table_names(&conn)?;
        let counts = names
            .into_iter()
            .map(|n| {
                let c = count_of(&conn, &n);
                (n, c)
            })
            .collect();
        (epoch, counts)
    };

    let mut problems: Vec<String> = Vec::new();

    // THE migration — the same `Store::open` the daemon boots through, so
    // the backup gate and every repair run exactly as they would live.
    let store = crate::store::Store::open(&copy).map_err(|e| {
        // Leave the copy behind on a failure: it is the evidence.
        RehearsalError::Open(e.to_string())
    })?;
    let backup = crate::backup::read_receipt(&copy);

    // The synthetic identity — see the module doc.
    {
        let conn = Connection::open(&copy).map_err(|e| RehearsalError::Sqlite(e.to_string()))?;
        conn.execute(
            "UPDATE repos SET workspace_id = ?1, worktree_id = ?2 \
             WHERE workspace_id IS NULL OR worktree_id IS NULL",
            rusqlite::params![SYNTHETIC_WORKSPACE, SYNTHETIC_WORKTREE],
        )
        .map_err(|e| RehearsalError::Sqlite(e.to_string()))?;
    }

    // No budget and no pause: a rehearsal wants the WALL CLOCK of the full
    // pass, not one boot's slice of it.
    let backfill = super::run_backfill(
        &store,
        "rehearsal",
        None,
        std::time::Duration::from_millis(0),
    );
    if !backfill.complete {
        problems.push("the backfill did not run to completion".to_string());
    }

    drop(store);

    let (epoch_after, tables) = {
        let conn = Connection::open(&copy).map_err(|e| RehearsalError::Sqlite(e.to_string()))?;
        let epoch = kb_core::sibling::volume_epoch(&conn).ok().flatten();
        let names = table_names(&conn)?;
        let mut rows = Vec::with_capacity(names.len());
        for name in names {
            let after = count_of(&conn, &name);
            let before_n = before
                .iter()
                .find(|(n, _)| *n == name)
                .and_then(|(_, c)| *c);
            let delta = match (before_n, after) {
                (Some(b), Some(a)) => Some(a - b),
                _ => None,
            };
            let kt = classified(&name);
            let key_column = kt.and_then(|t| t.class.key_column());
            let (keyed, unkeyed) = match key_column {
                Some(key) => (
                    conn.query_row(
                        &format!("SELECT count(*) FROM \"{name}\" WHERE {key} IS NOT NULL"),
                        [],
                        |r| r.get::<_, i64>(0),
                    )
                    .ok(),
                    conn.query_row(
                        &format!("SELECT count(*) FROM \"{name}\" WHERE {key} IS NULL"),
                        [],
                        |r| r.get::<_, i64>(0),
                    )
                    .ok(),
                ),
                None => (None, None),
            };
            let mut note = None;
            let existed_before = before.iter().any(|(n, _)| *n == name);
            if !existed_before {
                // A table this migration CREATED. Its "delta" is not a row
                // count that moved; there was nothing to move.
                note = Some("created by this migration".to_string());
            } else if before_n.is_none() || after.is_none() {
                note = Some("not countable (fts5 contentless index or shadow table)".to_string());
            }
            // `refinery_schema_history` gains exactly one row per applied
            // migration — that IS the migration happening, and counting it
            // as a violation of "a re-key adds columns, never rows" would
            // make the rehearsal cry wolf on every successful run.
            let is_bookkeeping = name == "refinery_schema_history";
            if existed_before && !is_bookkeeping && delta.is_some_and(|d| d != 0) {
                problems.push(format!(
                    "{name}: row count moved by {} — a re-key adds columns, never rows",
                    delta.unwrap_or(0)
                ));
            }
            if let Some(u) = unkeyed {
                if u > 0 {
                    // A row whose repo no longer exists in `repos` cannot be
                    // keyed and is NOT a defect — say which it is.
                    problems.push(format!(
                        "{name}: {u} row(s) still unkeyed after the backfill (rows whose \
                         repo is no longer registered cannot be keyed)"
                    ));
                }
            }
            rows.push(TableRow {
                name,
                rows_before: before_n,
                rows_after: after,
                delta,
                class: kt.map(|t| t.class.as_str()),
                key_column,
                keyed,
                unkeyed,
                note,
            });
        }
        (epoch, rows)
    };

    if epoch_after.is_none() {
        problems.push("the migrated copy reports no schema epoch at all".to_string());
    }
    if let (Some(b), Some(a)) = (epoch_before, epoch_after) {
        if a < b {
            problems.push(format!("epoch went BACKWARDS: V{b:04} -> V{a:04}"));
        }
    }

    let work_dir = work.display().to_string();
    if !keep {
        let _ = std::fs::remove_dir_all(&work);
    }

    Ok(Rehearsal {
        schema: SCHEMA,
        source: source.display().to_string(),
        work_dir,
        kept: keep,
        epoch_before,
        epoch_after,
        backup,
        synthetic_identity: true,
        tables,
        backfill,
        elapsed_ms: started.elapsed().as_millis() as u64,
        ok: problems.is_empty(),
        problems,
    })
}

/// A throwaway directory BESIDE the source, so the copy lands on the same
/// filesystem — which is also what makes "did this volume fit" an honest
/// part of the rehearsal.
fn work_dir_for(source: &Path) -> PathBuf {
    let parent = source
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    parent.join(format!(".kbc-rehearsal-{}-{stamp}", std::process::id()))
}
