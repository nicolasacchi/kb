//! V75-M1 — the re-key's teeth.
//!
//! The walk in [`every_repo_keyed_table_is_classified`] is the reason this
//! unit is worth its migration: a table added later with a `repo_id`/
//! `repo` column fails the build until its author says which identity owns
//! its rows. Everything else here proves the mechanism that stamps them.

use super::*;
use crate::store::Store;
use rusqlite::Connection;

fn open_temp() -> (tempfile::TempDir, Store, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("index.db");
    let store = Store::open(&path).expect("open kb-code store");
    (tmp, store, path)
}

/// A second connection onto the same volume, for fixture writes the
/// `Store` API deliberately has no verb for. WAL admits it, and the tests
/// are sequential.
fn raw(path: &std::path::Path) -> Connection {
    Connection::open(path).unwrap()
}

fn table_columns(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info(\"{table}\")"))
        .unwrap();
    stmt.query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
}

fn all_tables(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' \
             AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .unwrap();
    stmt.query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
}

/// fts5 creates shadow tables (`<name>_data`, `<name>_idx`, …) that are
/// implementation detail, never something the re-key classifies.
fn is_fts_shadow(name: &str) -> bool {
    ["_data", "_idx", "_content", "_docsize", "_config"]
        .iter()
        .any(|s| name.ends_with(s))
        && name.starts_with("transcript_fts")
}

/// **The classification walk.** Both directions: an undeclared repo-keyed
/// table fails by name, and a declared table that no longer exists fails
/// by name.
#[test]
fn every_repo_keyed_table_is_classified() {
    let (_tmp, _store, path) = open_temp();
    let conn = raw(&path);

    let mut undeclared: Vec<String> = Vec::new();
    for table in all_tables(&conn) {
        if is_fts_shadow(&table) || table == "refinery_schema_history" {
            continue;
        }
        let cols = table_columns(&conn, &table);
        let has_repo_key = cols.iter().any(|c| c == "repo_id" || c == "repo");
        // `repos` carries neither, and is declared `meta` anyway because it
        // is the canonical HOLDER of both identities.
        let declared = REPO_KEYED_TABLES.iter().any(|t| t.table == table);
        if has_repo_key && !declared {
            undeclared.push(table);
        }
    }
    assert!(
        undeclared.is_empty(),
        "table(s) carry a repo key but declare no re-key class: {undeclared:?}\n\
         Add each to `rekey::REPO_KEYED_TABLES` as object | worktree | meta, and (for the \
         first two) an ALTER + trigger in the migration that creates it. An object row is a \
         function of the OBJECT STORE (a blob, a commit, git history); a worktree row is a \
         property of a PATH ON DISK in one checkout."
    );

    let live = all_tables(&conn);
    let missing: Vec<&str> = REPO_KEYED_TABLES
        .iter()
        .map(|t| t.table)
        .filter(|t| !live.iter().any(|l| l == t))
        .collect();
    assert!(
        missing.is_empty(),
        "declared in REPO_KEYED_TABLES but not in the schema: {missing:?}"
    );
}

#[test]
fn every_keyed_table_carries_its_key_column() {
    let (_tmp, _store, path) = open_temp();
    let conn = raw(&path);
    for t in keyed_tables() {
        let key = t.class.key_column().unwrap();
        let cols = table_columns(&conn, t.table);
        assert!(
            cols.iter().any(|c| c == key),
            "{}: declared {} but V0040 never added the column",
            t.table,
            key
        );
    }
    // And `repos` holds both, since every trigger reads them from there.
    let repos = table_columns(&conn, "repos");
    assert!(repos.iter().any(|c| c == "workspace_id"));
    assert!(repos.iter().any(|c| c == "worktree_id"));
}

/// The value comes from a trigger, not from Rust — so the trigger is the
/// thing that must exist for every keyed table.
#[test]
fn every_keyed_table_has_a_trigger_that_writes_its_key() {
    let (_tmp, _store, path) = open_temp();
    let conn = raw(&path);
    let mut stmt = conn
        .prepare("SELECT tbl_name, sql FROM sqlite_master WHERE type = 'trigger'")
        .unwrap();
    let triggers: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    for t in keyed_tables() {
        let key = t.class.key_column().unwrap();
        let found = triggers
            .iter()
            .find(|(tbl, _)| tbl == t.table)
            .unwrap_or_else(|| {
                panic!(
                    "{}: no AFTER INSERT trigger — a future INSERT would leave {key} NULL \
                     forever, which is exactly what the trigger exists to make impossible",
                    t.table
                )
            });
        assert!(
            found.1.contains(key),
            "{}: its trigger does not write {key}: {}",
            t.table,
            found.1
        );
        assert!(
            found.1.contains("FROM repos"),
            "{}: its trigger does not read the identity off `repos`",
            t.table
        );
    }
}

/// The backfill interpolates table and column names into SQL. They come
/// from a `const` in this binary and can never come from a request — this
/// asserts the shape anyway, so the day someone makes the list dynamic the
/// build says no.
#[test]
fn every_declared_table_name_is_a_plain_identifier() {
    for t in REPO_KEYED_TABLES {
        assert!(
            !t.table.is_empty()
                && t.table
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
            "{:?} is not a plain identifier",
            t.table
        );
        assert!(
            matches!(t.repo_column, "repo_id" | "repo" | ""),
            "{}: unknown repo column {:?}",
            t.table,
            t.repo_column
        );
        assert!(
            t.why.len() > 30,
            "{}: a classification with no reason is not a classification",
            t.table
        );
    }
}

/// The debt ledger and the classification must agree, both ways — a
/// widened read left off the ledger would read as done, and a ledger entry
/// for a worktree-class table would be a promise nobody meant.
#[test]
fn the_not_widened_ledger_is_exactly_the_object_class() {
    let mut object: Vec<&str> = REPO_KEYED_TABLES
        .iter()
        .filter(|t| t.class == RepoKeyClass::Object)
        .map(|t| t.table)
        .collect();
    object.sort_unstable();
    let mut ledger: Vec<&str> = READS_NOT_WIDENED.to_vec();
    ledger.sort_unstable();
    assert_eq!(
        object, ledger,
        "READS_NOT_WIDENED must name every object-class table and nothing else — see this \
         module's doc for why no read widens onto workspace_id in this unit"
    );
}

#[test]
fn no_two_declarations_claim_the_same_table() {
    let mut seen: Vec<&str> = Vec::new();
    for t in REPO_KEYED_TABLES {
        assert!(!seen.contains(&t.table), "{} declared twice", t.table);
        seen.push(t.table);
    }
}

#[test]
fn the_state_label_is_the_three_words_the_wire_promises() {
    use std::sync::atomic::AtomicU8;
    assert_eq!(state_label(&AtomicU8::new(STATE_PENDING)), "pending");
    assert_eq!(state_label(&AtomicU8::new(STATE_RUNNING)), "running");
    assert_eq!(state_label(&AtomicU8::new(STATE_DONE)), "done");
    assert_eq!(
        state_label(&AtomicU8::new(99)),
        "pending",
        "unknown is never `done`"
    );
}

// ── the trigger, live ────────────────────────────────────────────────

fn register(store: &Store, name: &str, ws: &str, wt: &str) -> i64 {
    let id = store.upsert_repo(name, &format!("/tmp/{name}")).unwrap();
    store.set_repo_identity(name, ws, wt).unwrap();
    id
}

#[test]
fn a_row_inserted_after_resolution_is_keyed_by_the_trigger() {
    let (_tmp, store, path) = open_temp();
    let repo_id = register(&store, "acme", "ws_aaaaaaaaaaaa", "(main)");
    let conn = raw(&path);
    // object class
    conn.execute(
        "INSERT INTO path_stats (repo_id, path, revisions) VALUES (?1, 'a.rb', 3)",
        rusqlite::params![repo_id],
    )
    .unwrap();
    let got: Option<String> = conn
        .query_row("SELECT workspace_id FROM path_stats", [], |r| r.get(0))
        .unwrap();
    assert_eq!(got.as_deref(), Some("ws_aaaaaaaaaaaa"));

    // worktree class
    conn.execute(
        "INSERT INTO files (repo_id, path, blob_hash, lang, size) \
         VALUES (?1, 'a.rb', 'deadbeef', 'ruby', 10)",
        rusqlite::params![repo_id],
    )
    .unwrap();
    let got: Option<String> = conn
        .query_row("SELECT worktree_id FROM files", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        got.as_deref(),
        Some("(main)"),
        "a worktree-class row takes the WORKTREE id, never the workspace one"
    );
}

/// The two tables keyed by repo NAME rather than `repos.id` go through the
/// other half of the trigger.
#[test]
fn a_name_keyed_table_is_keyed_too() {
    let (_tmp, store, path) = open_temp();
    register(&store, "acme", "ws_bbbbbbbbbbbb", "feature");
    let conn = raw(&path);
    conn.execute(
        "INSERT INTO bookmarks (repo, path, line, created_at, updated_at) \
         VALUES ('acme', 'a.rb', 1, 0, 0)",
        [],
    )
    .unwrap();
    let got: Option<String> = conn
        .query_row("SELECT worktree_id FROM bookmarks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(got.as_deref(), Some("feature"));
}

/// Before resolution has run, `repos.workspace_id` is NULL and the trigger
/// writes NULL — which is exactly the `rekey: "pending"` state, and the
/// reason the backfill exists.
#[test]
fn a_row_inserted_before_resolution_is_left_unkeyed_for_the_backfill() {
    let (_tmp, store, path) = open_temp();
    let repo_id = store.upsert_repo("acme", "/tmp/acme").unwrap();
    let conn = raw(&path);
    conn.execute(
        "INSERT INTO path_stats (repo_id, path, revisions) VALUES (?1, 'a.rb', 3)",
        rusqlite::params![repo_id],
    )
    .unwrap();
    let got: Option<String> = conn
        .query_row("SELECT workspace_id FROM path_stats", [], |r| r.get(0))
        .unwrap();
    assert_eq!(got, None);
}

// ── the paged backfill ───────────────────────────────────────────────

fn path_stats_row(conn: &Connection, repo_id: i64, n: usize) {
    conn.execute(
        "INSERT INTO path_stats (repo_id, path, revisions) VALUES (?1, ?2, 1)",
        rusqlite::params![repo_id, format!("f{n}.rb")],
    )
    .unwrap();
}

#[test]
fn the_backfill_is_paged_resumable_and_idempotent() {
    let (_tmp, store, path) = open_temp();
    let repo_id = store.upsert_repo("acme", "/tmp/acme").unwrap();
    let conn = raw(&path);
    for n in 0..25 {
        path_stats_row(&conn, repo_id, n);
    }
    // Resolution lands only now, so all 25 rows predate the key.
    store
        .set_repo_identity("acme", "ws_cccccccccccc", "(main)")
        .unwrap();
    let t = REPO_KEYED_TABLES
        .iter()
        .find(|t| t.table == "path_stats")
        .unwrap();

    // Page 1 of 10 keys ten rows and does NOT report done.
    let (keyed, done) = store.rekey_backfill_page(t, "fp1", 10).unwrap();
    assert_eq!(keyed, 10);
    assert!(!done);
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM path_stats WHERE workspace_id IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 10, "a page is a page, not a whole-table transaction");

    // Resume from the persisted cursor.
    let (keyed, done) = store.rekey_backfill_page(t, "fp1", 10).unwrap();
    assert_eq!(keyed, 10);
    assert!(!done);
    let (keyed, done) = store.rekey_backfill_page(t, "fp1", 10).unwrap();
    assert_eq!(keyed, 5);
    assert!(done, "a SHORT page is the last page");

    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM path_stats WHERE workspace_id = 'ws_cccccccccccc'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 25);

    // Idempotent: a completed table is free on every later call.
    let (keyed, done) = store.rekey_backfill_page(t, "fp1", 10).unwrap();
    assert_eq!(keyed, 0);
    assert!(done);
}

#[test]
fn a_changed_repo_identity_restarts_a_completed_table() {
    let (_tmp, store, path) = open_temp();
    let repo_id = store.upsert_repo("acme", "/tmp/acme").unwrap();
    let conn = raw(&path);
    for n in 0..3 {
        path_stats_row(&conn, repo_id, n);
    }
    store
        .set_repo_identity("acme", "ws_dddddddddddd", "(main)")
        .unwrap();
    let t = REPO_KEYED_TABLES
        .iter()
        .find(|t| t.table == "path_stats")
        .unwrap();
    let (_, done) = store.rekey_backfill_page(t, "fp1", 100).unwrap();
    assert!(done);
    assert!(store.rekey_is_done().is_ok());

    // A repo is added/moved: new fingerprint, and the table is walked
    // again. Nothing is re-written (every row is already keyed), which is
    // what makes the restart cheap.
    conn.execute(
        "UPDATE path_stats SET workspace_id = NULL WHERE path = 'f0.rb'",
        [],
    )
    .unwrap();
    let (keyed, done) = store.rekey_backfill_page(t, "fp2", 100).unwrap();
    assert!(done);
    assert_eq!(keyed, 1, "only the row that lost its key is rewritten");
}

#[test]
fn run_backfill_walks_every_keyed_table_and_reports_a_census() {
    let (_tmp, store, path) = open_temp();
    let repo_id = store.upsert_repo("acme", "/tmp/acme").unwrap();
    let conn = raw(&path);
    for n in 0..4 {
        path_stats_row(&conn, repo_id, n);
    }
    conn.execute(
        "INSERT INTO files (repo_id, path, blob_hash, lang, size) \
         VALUES (?1, 'a.rb', 'x', 'ruby', 1)",
        rusqlite::params![repo_id],
    )
    .unwrap();
    store
        .set_repo_identity("acme", "ws_eeeeeeeeeeee", "(main)")
        .unwrap();

    let report = run_backfill(&store, "fp", None, std::time::Duration::ZERO);
    assert!(report.complete);
    assert_eq!(report.tables_done, report.tables_total);
    assert_eq!(report.rows_keyed, 5, "4 path_stats + 1 files");
    assert!(store.rekey_is_done().unwrap());

    // And the ONE read that goes through the new key.
    let census = store.workspace_derived_census("ws_eeeeeeeeeeee").unwrap();
    assert_eq!(census.get("path_stats").copied(), Some(4));
    assert!(
        !census.contains_key("files"),
        "the census is object-class only — `files` is a property of a checkout"
    );
    assert!(
        !census.contains_key("comments"),
        "a table with no rows is absent, never a zero"
    );
}

/// The resolution function falls back to the caller's own repo id while
/// the identity is unresolved, which is what keeps every future caller
/// byte-identical during `rekey: "pending"`.
#[test]
fn the_repo_id_set_falls_back_to_the_caller_and_widens_only_within_one_workspace() {
    let (_tmp, store, _path) = open_temp();
    let a = store.upsert_repo("a", "/tmp/a").unwrap();
    let b = store.upsert_repo("b", "/tmp/b").unwrap();
    let c = store.upsert_repo("c", "/tmp/c").unwrap();
    assert_eq!(store.workspace_repo_ids(a).unwrap(), vec![a]);

    store.set_repo_identity("a", "ws_1", "(main)").unwrap();
    store.set_repo_identity("b", "ws_1", "feature").unwrap();
    store.set_repo_identity("c", "ws_2", "(main)").unwrap();
    assert_eq!(store.workspace_repo_ids(a).unwrap(), vec![a, b]);
    assert_eq!(store.workspace_repo_ids(b).unwrap(), vec![a, b]);
    assert_eq!(store.workspace_repo_ids(c).unwrap(), vec![c]);
}

// ── the epoch, both directions ───────────────────────────────────────

/// The one-way door, proven. A volume this binary migrated carries the
/// re-key epoch, and a binary that predates it refuses rather than booting
/// green and failing later on a column it does not know — the 13.5 h kbc
/// outage, structurally prevented.
#[test]
fn an_older_binary_refuses_a_rekeyed_volume() {
    let (_tmp, _store, path) = open_temp();
    let conn = raw(&path);
    let volume = kb_core::sibling::volume_epoch(&conn).unwrap();
    assert_eq!(
        volume,
        Some(crate::store::schema_epoch()),
        "opening the store migrated the volume to this binary's epoch"
    );
    assert!(
        crate::store::schema_epoch() >= crate::backup::REKEY_EPOCH,
        "this binary embeds the re-key migration"
    );

    let previous = crate::backup::REKEY_EPOCH - 1;
    let err = kb_core::sibling::refuse_if_volume_ahead(&conn, &path, previous)
        .expect_err("a pre-re-key binary must refuse this volume");
    let msg = err.to_string();
    assert!(msg.contains("refusing to boot"), "{msg}");
    assert!(msg.contains(&format!("V{previous}")), "{msg}");

    // And the same binary that wrote it boots fine.
    assert!(
        kb_core::sibling::refuse_if_volume_ahead(&conn, &path, crate::store::schema_epoch())
            .is_ok()
    );
}

/// A fresh volume is a first boot, never a crossing — so the overwhelming
/// majority of `Store::open` calls (including every test above) write no
/// snapshot at all and are byte-identical to pre-V75-M1.
#[test]
fn a_fresh_volume_takes_no_pre_migration_snapshot() {
    let (_tmp, _store, path) = open_temp();
    assert!(crate::backup::read_receipt(&path).is_none());
}

// ── the migration treatment, on a SYNTHETIC pre-re-key volume ────────
//
// `Store::migrate_to_for_test` builds a volume that genuinely stopped one
// migration short of this binary — a real refinery history, not a
// fabricated row — which is the only honest fixture for the two things a
// one-way door owes: a backup taken before the crossing, and a rehearsal
// that proves the crossing on a copy.

/// A volume migrated to exactly `REKEY_EPOCH - 1`, i.e. everything this
/// binary embeds except the re-key itself.
fn pre_rekey_volume() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("index.db");
    Store::migrate_to_for_test(&path, crate::backup::REKEY_EPOCH - 1).expect("pre-re-key volume");
    let conn = raw(&path);
    assert_eq!(
        kb_core::sibling::volume_epoch(&conn).unwrap(),
        Some(crate::backup::REKEY_EPOCH - 1),
        "the fixture really is one migration short"
    );
    (tmp, path)
}

/// The gate, through the REAL `Store::open` — not the helper in isolation.
/// This is the test that would have caught the defect the first local
/// rehearsal found: a backup module that exists and is never called.
#[test]
fn opening_a_pre_rekey_volume_snapshots_it_before_migrating() {
    let (_tmp, path) = pre_rekey_volume();
    assert!(
        crate::backup::read_receipt(&path).is_none(),
        "nothing has crossed yet"
    );

    let store = Store::open(&path).expect("the crossing boot succeeds");

    let receipt = crate::backup::read_receipt(&path).expect(
        "Store::open must take a pre-migration snapshot when it carries a volume across the \
         re-key epoch — the gate is only worth anything if it is WIRED",
    );
    assert_eq!(receipt.volume_epoch, Some(crate::backup::REKEY_EPOCH - 1));
    assert!(std::path::Path::new(&receipt.backup_path).exists());
    assert!(receipt.bytes > 0);

    // The snapshot is a volume at the OLD epoch — i.e. a real rollback
    // target for a binary that predates this one.
    let snap = raw(std::path::Path::new(&receipt.backup_path));
    assert_eq!(
        kb_core::sibling::volume_epoch(&snap).unwrap(),
        Some(crate::backup::REKEY_EPOCH - 1)
    );

    // And the live volume did cross.
    let conn = raw(&path);
    assert_eq!(
        kb_core::sibling::volume_epoch(&conn).unwrap(),
        Some(crate::store::schema_epoch())
    );
    drop(store);
}

/// `kb-code rehearse-migration` on a synthetic pre-re-key volume: the
/// whole verb, on the shape it exists for.
#[test]
fn the_rehearsal_crosses_a_synthetic_pre_rekey_volume_and_reports_it() {
    let (_tmp, path) = pre_rekey_volume();
    // A row in each class, so the backfill has something to key and the
    // census has something to count. Written directly, because at this
    // epoch the key columns and their triggers do not exist yet — which is
    // exactly the pre-re-key state the backfill is for.
    {
        let conn = raw(&path);
        conn.execute(
            "INSERT INTO repos (name, root) VALUES ('acme', '/tmp/acme')",
            [],
        )
        .unwrap();
        let repo_id: i64 = conn
            .query_row("SELECT id FROM repos WHERE name = 'acme'", [], |r| r.get(0))
            .unwrap();
        conn.execute(
            "INSERT INTO path_stats (repo_id, path, revisions) VALUES (?1, 'a.rb', 2)",
            rusqlite::params![repo_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO files (repo_id, path, blob_hash, lang, size) \
             VALUES (?1, 'a.rb', 'deadbeef', 'ruby', 12)",
            rusqlite::params![repo_id],
        )
        .unwrap();
    }

    let r = super::rehearsal::rehearse(&path, false).expect("the rehearsal runs");
    assert_eq!(r.epoch_before, Some(crate::backup::REKEY_EPOCH - 1));
    assert_eq!(r.epoch_after, Some(crate::store::schema_epoch()));
    assert!(
        r.backup.is_some(),
        "a rehearsed crossing exercises the backup gate too"
    );
    assert!(r.backfill.complete);
    assert_eq!(r.backfill.tables_done, r.backfill.tables_total);
    assert_eq!(
        r.backfill.rows_keyed, 2,
        "one object-class row and one worktree-class row, keyed against the SYNTHETIC identity \
         the rehearsal stamps on the copy"
    );
    assert!(r.ok, "problems: {:?}", r.problems);

    // The census must be honest about what it could and could not count,
    // and must not read a table this migration CREATED as a row-count
    // violation (the first local rehearsal reported exactly that, plus a
    // "not countable" note on a brand-new table).
    let worktrees = r
        .tables
        .iter()
        .find(|t| t.name == "worktrees")
        .expect("the new table is in the census");
    assert_eq!(worktrees.note.as_deref(), Some("created by this migration"));
    let history = r
        .tables
        .iter()
        .find(|t| t.name == "refinery_schema_history")
        .expect("refinery's own table is counted");
    assert!(
        history.delta.is_some_and(|d| d > 0),
        "the migration really did apply"
    );

    // The source volume is untouched: still at the old epoch, no work dir.
    let conn = raw(&path);
    assert_eq!(
        kb_core::sibling::volume_epoch(&conn).unwrap(),
        Some(crate::backup::REKEY_EPOCH - 1),
        "a rehearsal never writes the source"
    );
    assert!(
        !std::path::Path::new(&r.work_dir).exists(),
        "the copy is deleted without --keep"
    );
}
