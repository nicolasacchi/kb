use super::*;
use crate::highlight::HighlightClass;

fn open_temp() -> (tempfile::TempDir, Store) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("index.db")).unwrap();
    (tmp, store)
}

#[test]
fn open_runs_migrations_and_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("index.db");
    let _s1 = Store::open(&db_path).unwrap();
    // Re-opening the same file must not fail (refinery no-ops on an
    // already-migrated schema).
    let _s2 = Store::open(&db_path).unwrap();
}

/// kb-sibling/1 — a volume forward-migrated by a NEWER kb-code binary
/// must refuse to open, naming both epochs and the db path. The history
/// row is FABRICATED at `schema_epoch() + 1000` (migrations themselves
/// are immutable); no real migration will ever reach that version.
// invariant:2 kb-sibling/1 schema-epoch boot refuse
#[test]
fn open_refuses_a_volume_whose_schema_epoch_is_ahead_of_this_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("index.db");
    let ahead = schema_epoch() + 1_000;
    {
        let store = Store::open(&db_path).expect("first open migrates normally");
        store
            .lock()
            .execute(
                "INSERT INTO refinery_schema_history (version, name, applied_on, checksum) \
                 VALUES (?1, 'from_a_newer_binary', '', '0')",
                params![ahead],
            )
            .unwrap();
    }
    let err = match Store::open(&db_path) {
        Ok(_) => panic!("an ahead volume must refuse to open"),
        Err(e) => e,
    };
    assert!(matches!(err, StoreError::SchemaEpoch(_)), "{err:?}");
    let msg = err.to_string();
    assert!(msg.contains("refusing to boot"), "{msg}");
    assert!(msg.contains(&format!("V{ahead}")), "{msg}");
    assert!(msg.contains(&format!("V{}", schema_epoch())), "{msg}");
    assert!(msg.contains("index.db"), "{msg}");
}

/// The passing case at EQUAL epoch — a freshly migrated volume sits
/// exactly at the binary epoch and re-opens normally.
#[test]
fn open_proceeds_when_the_volume_epoch_equals_the_binary_epoch() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("index.db");
    let store = Store::open(&db_path).unwrap();
    assert_eq!(
        kb_core::sibling::volume_epoch(&store.lock()).unwrap(),
        Some(schema_epoch()),
    );
    drop(store);
    Store::open(&db_path).expect("re-opening at an equal epoch must boot");
}

/// V72-B1 — the migration-checksum repair (`repair_v3_transcripts_checksum`)
/// and its own regression golden (`migrations.checksums.json`). See the
/// constants next to `repair_v3_transcripts_checksum` for the full
/// defect story.
mod v72_b1 {
    use super::*;

    /// The JSON shape of `tests/fixtures/migrations.checksums.json` —
    /// one row per migration EMBEDDED in this binary. `version` is i32
    /// to match refinery 0.9's widened `Migration::version()` (the JSON
    /// numbers are unchanged — kb's epochs are all small positives).
    #[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq)]
    struct MigrationChecksumRow {
        version: i32,
        name: String,
        checksum: String,
    }

    fn embedded_checksums() -> Vec<MigrationChecksumRow> {
        let mut rows: Vec<MigrationChecksumRow> = embedded::migrations::runner()
            .get_migrations()
            .iter()
            .map(|m| MigrationChecksumRow {
                version: m.version(),
                name: m.name().to_string(),
                checksum: m.checksum().to_string(),
            })
            .collect();
        rows.sort_by_key(|r| r.version);
        rows
    }

    /// The CI golden. Lives under `tests/` (where a reviewer looks for
    /// a fixture) and is read from here (where the only test that can
    /// regenerate it lives) — same convention as `syntax.rs`'s
    /// `PARITY_GOLDEN`.
    const MIGRATIONS_CHECKSUMS_GOLDEN: &str =
        include_str!("../../tests/fixtures/migrations.checksums.json");

    /// (e) — every embedded migration's checksum matches the checked-in
    /// golden. A change here means an APPLIED migration's file content
    /// changed — exactly the class of edit that diverged V3 in the
    /// public scrub. Failing loudly, with the fix spelled out, is the
    /// whole point of this test.
    #[test]
    fn every_embedded_migration_checksum_matches_the_golden() {
        let actual = serde_json::to_string_pretty(&embedded_checksums()).expect("serialize");
        assert_eq!(
            actual.trim_end(),
            MIGRATIONS_CHECKSUMS_GOLDEN.trim_end(),
            "an applied migration's content changed — either revert the edit or ship \
             a repair like V72-B1 and bump this golden deliberately\n{actual}"
        );
    }

    /// Version numbers this project deliberately SKIPPED and will never
    /// use — the debt ledger for [`embedded_migration_versions_are_contiguous`].
    ///
    /// `33` was reserved for V72-H2b's own `derived_status` under the
    /// old pre-assign-a-slot ledger. When that ledger was found unsafe
    /// (below), this unit renumbered to main's max + 1 and left 33
    /// permanently empty. That is INERT and is the point: refinery only
    /// ever refuses an embedded migration that exists BELOW the applied
    /// maximum, so a version that never exists cannot be refused. The
    /// dangerous thing is not the hole — it is filling it.
    ///
    /// This list may SHRINK (never), and must never GROW: adding to it
    /// means a slot was reserved again.
    const PERMANENTLY_SKIPPED_VERSIONS: &[i32] = &[33];

    /// V72-H2b — no embedded migration may be numbered below the
    /// highest one, except for the permanently-skipped versions above.
    ///
    /// The milestone ledger used to pre-assign version numbers to
    /// in-flight units and let them land out of order, on the belief
    /// (written into `V0034__review_docs_and_findings_v2.sql`'s own
    /// header, which is frozen because editing an applied migration's
    /// bytes is itself the trap invariant 11 records) that "refinery
    /// applies by version, so a gap is inert". **A gap is inert; a
    /// gap-FILL is not.** `refinery-core`'s
    /// `traits::get_unapplied_migrations` selects only migrations with
    /// `version > current`, and with `abort_missing` at its default
    /// `true` — which `Store::open` uses — an embedded migration BELOW
    /// the applied maximum is a hard `MissingVersion` error. A volume
    /// already migrated past a reserved slot therefore REFUSES TO BOOT
    /// the moment the gap-fill merges: not a skipped table, a dead
    /// daemon. V72-H2b caught this while holding such a slot.
    ///
    /// This test makes it unrepeatable. It is about the EMBEDDED set
    /// only (what this binary ships), needs no database, and fails at
    /// the one moment a human can still act on it — the PR that adds
    /// the migration.
    #[test]
    fn embedded_migration_versions_are_contiguous() {
        let versions: Vec<i32> = embedded_checksums().iter().map(|r| r.version).collect();
        assert!(!versions.is_empty(), "no embedded migrations at all");
        // `embedded_checksums` already sorts by version.
        let (lo, hi) = (versions[0], *versions.last().unwrap());
        let missing: Vec<i32> = (lo..=hi)
            .filter(|v| !versions.contains(v))
            .filter(|v| !PERMANENTLY_SKIPPED_VERSIONS.contains(v))
            .collect();
        assert!(
            missing.is_empty(),
            "embedded migration versions have gap(s) at {missing:?} (present: {lo}..={hi}) — \
             refinery's abort_missing refuses gap-fills: renumber to main's max + 1"
        );
        // A skipped version must stay skipped: filling one is exactly
        // the boot-refusal this test exists to prevent.
        for v in PERMANENTLY_SKIPPED_VERSIONS {
            assert!(
                !versions.contains(v),
                "V{v:04} is in PERMANENTLY_SKIPPED_VERSIONS but a migration now claims it — \
                 refinery's abort_missing refuses gap-fills: renumber to main's max + 1"
            );
        }
        // Belt and braces: no duplicate version can hide behind the
        // range check above.
        let mut dedup = versions.clone();
        dedup.dedup();
        assert_eq!(
            dedup.len(),
            versions.len(),
            "two embedded migrations share a version: {versions:?}"
        );
    }

    /// A freshly-migrated db, NOT via `Store::open` (so this helper is
    /// unaffected by the repair under test) — the runner's own public
    /// checksums land straight in `refinery_schema_history`.
    fn fresh_migrated_db() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("index.db");
        let mut conn = Connection::open(&db_path).unwrap();
        embedded::migrations::runner().run(&mut conn).unwrap();
        (tmp, db_path)
    }

    fn set_v3_checksum(db_path: &std::path::Path, checksum: &str) {
        Connection::open(db_path)
            .unwrap()
            .execute(
                "UPDATE refinery_schema_history SET checksum = ?1 \
                 WHERE version = 3 AND name = 'transcripts'",
                params![checksum],
            )
            .unwrap();
    }

    fn v3_checksum(db_path: &std::path::Path) -> String {
        Connection::open(db_path)
            .unwrap()
            .query_row(
                "SELECT checksum FROM refinery_schema_history \
                 WHERE version = 3 AND name = 'transcripts'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// (a) — a db carrying the archive-era V3 checksum opens
    /// successfully via `Store::open`, and the row is rewritten to the
    /// public value.
    #[test]
    fn archive_era_checksum_is_repaired_and_open_succeeds() {
        let (_tmp, db_path) = fresh_migrated_db();
        set_v3_checksum(&db_path, V3_TRANSCRIPTS_ARCHIVE_CHECKSUM);
        let store = Store::open(&db_path).expect("open must repair and then succeed");
        drop(store);
        assert_eq!(v3_checksum(&db_path), V3_TRANSCRIPTS_PUBLIC_CHECKSUM);
    }

    /// (b) — a db already carrying the public checksum (the normal,
    /// never-diverged case) is left byte-for-byte untouched.
    #[test]
    fn public_checksum_is_left_untouched() {
        let (_tmp, db_path) = fresh_migrated_db();
        assert_eq!(
            v3_checksum(&db_path),
            V3_TRANSCRIPTS_PUBLIC_CHECKSUM,
            "a fresh migration must already record the public checksum"
        );
        let store = Store::open(&db_path).expect("re-open must succeed");
        drop(store);
        assert_eq!(v3_checksum(&db_path), V3_TRANSCRIPTS_PUBLIC_CHECKSUM);
    }

    /// (c) — an UNRELATED V3 checksum (neither archive-era nor public)
    /// is a real divergence: the repair must not touch it, and
    /// refinery's own `abort_divergent` (the default — never weakened
    /// by this repair) must still surface the error.
    #[test]
    fn an_unrelated_v3_checksum_still_refuses_to_open() {
        let (_tmp, db_path) = fresh_migrated_db();
        set_v3_checksum(&db_path, "1");
        let err = match Store::open(&db_path) {
            Ok(_) => panic!("a real divergence must still refuse to open"),
            Err(e) => e,
        };
        assert!(matches!(err, StoreError::Migration(_)), "{err:?}");
        assert!(err.to_string().contains("V3__transcripts"), "{err}");
        assert_eq!(
            v3_checksum(&db_path),
            "1",
            "an unrelated checksum must be left exactly alone"
        );
    }

    /// (d) — idempotency: opening an already-repaired (or always-public)
    /// volume a second time is a clean no-op repair followed by a
    /// normal boot, same as any other repeated `Store::open`.
    #[test]
    fn repair_is_idempotent_across_repeated_opens() {
        let (_tmp, db_path) = fresh_migrated_db();
        set_v3_checksum(&db_path, V3_TRANSCRIPTS_ARCHIVE_CHECKSUM);
        drop(Store::open(&db_path).expect("first open repairs"));
        assert_eq!(v3_checksum(&db_path), V3_TRANSCRIPTS_PUBLIC_CHECKSUM);
        drop(Store::open(&db_path).expect("second open is a no-op repair"));
        assert_eq!(v3_checksum(&db_path), V3_TRANSCRIPTS_PUBLIC_CHECKSUM);
    }
}

/// V0007 (B2) on a FRESH db: refinery runs the whole chain
/// (V0001..V0007) in one shot, so `doc` is reachable and a symbol
/// carrying `Some(doc)` round-trips normally.
#[test]
fn v0007_migration_applies_cleanly_on_a_fresh_db() {
    let (_tmp, store) = open_temp();
    let mut sym = sample_symbol(0, "documented");
    sym.doc = Some("a doc comment".to_string());
    store
        .replace_symbols("hashA", "rust@1", &[sym.clone()])
        .unwrap();
    assert_eq!(
        store.symbols_for_blob("hashA", "rust@1").unwrap(),
        vec![sym]
    );
}

/// V0007's `ALTER TABLE symbols ADD COLUMN doc TEXT` (no DEFAULT) leaves
/// every PRE-EXISTING row's new column NULL — the same outcome a raw
/// INSERT that never mentions `doc` produces, so this stands in for "a
/// symbols row written before this migration existed" without needing to
/// fake a partial-migration refinery history: either way, an old-shaped
/// row reads back with `doc: None`.
#[test]
fn pre_migration_shaped_symbol_rows_read_back_with_doc_none() {
    let (_tmp, store) = open_temp();
    store
        .lock()
        .execute(
            "INSERT INTO symbols
                (blob_hash, salt, ordinal, name, kind, line_start, line_end, col_start, col_end)
             VALUES ('hashOld', 'rust@1', 0, 'legacy', 'fn', 1, 1, 0, 1)",
            [],
        )
        .unwrap();
    let rows = store.symbols_for_blob("hashOld", "rust@1").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "legacy");
    assert_eq!(rows[0].doc, None);
    assert_eq!(rows[0].signature, None);
}

#[test]
fn upsert_repo_is_idempotent_and_updates_root() {
    let (_tmp, store) = open_temp();
    let id1 = store.upsert_repo("kb", "/tmp/kb").unwrap();
    let id2 = store.upsert_repo("kb", "/tmp/kb-renamed").unwrap();
    assert_eq!(id1, id2);
    assert_eq!(store.repo_id("kb").unwrap(), Some(id1));
    assert_eq!(store.repo_id("nope").unwrap(), None);
}

#[test]
fn files_upsert_and_count_round_trip() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("kb", "/tmp/kb").unwrap();
    store
        .upsert_file(repo_id, "src/lib.rs", "hash1", "rust", 100)
        .unwrap();
    store
        .upsert_file(repo_id, "src/main.rs", "hash2", "rust", 50)
        .unwrap();
    assert_eq!(store.file_count(repo_id).unwrap(), 2);

    let row = store.get_file(repo_id, "src/lib.rs").unwrap().unwrap();
    assert_eq!(row.blob_hash, "hash1");
    assert_eq!(row.size, 100);

    // Re-upserting the same path updates in place, not a new row.
    store
        .upsert_file(repo_id, "src/lib.rs", "hash1-v2", "rust", 200)
        .unwrap();
    assert_eq!(store.file_count(repo_id).unwrap(), 2);
    let row = store.get_file(repo_id, "src/lib.rs").unwrap().unwrap();
    assert_eq!(row.blob_hash, "hash1-v2");
    assert_eq!(row.size, 200);
}

#[test]
fn delete_file_removes_the_row_and_leaves_derived_data_alone() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .upsert_file(repo_id, "src/lib.rs", "hashA", "rust", 10)
        .unwrap();
    store
        .replace_symbols("hashA", "rust@1", &[sample_symbol(0, "foo")])
        .unwrap();
    assert_eq!(store.file_count(repo_id).unwrap(), 1);

    store.delete_file(repo_id, "src/lib.rs").unwrap();
    assert_eq!(store.file_count(repo_id).unwrap(), 0);
    assert!(store.get_file(repo_id, "src/lib.rs").unwrap().is_none());
    // Derived rows are blob-keyed, not path-keyed — deleting the files
    // row must never touch them.
    assert!(store.has_symbols("hashA", "rust@1").unwrap());

    // Deleting an already-gone path is a no-op, not an error.
    store.delete_file(repo_id, "src/lib.rs").unwrap();
}

#[test]
fn delete_file_prunes_file_opens_rows_for_the_same_path() {
    // A4 — unlike blob-keyed symbols/highlights, `file_opens` is keyed
    // by (repo_id, path), same as `files`: an orphaned row there isn't
    // inert, it resurfaces in `recent_file_opens` (no join against
    // `files`) — see `delete_file`'s doc.
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .upsert_file(repo_id, "src/lib.rs", "hashA", "rust", 10)
        .unwrap();
    store.bump_file_open(repo_id, "src/lib.rs", 1_000).unwrap();
    assert_eq!(
        store.last_opened_map(repo_id).unwrap().get("src/lib.rs"),
        Some(&1_000)
    );
    assert_eq!(
        store.recent_file_opens(&[repo_id], 10, None).unwrap(),
        vec![(repo_id, "src/lib.rs".to_string(), 1_000)]
    );

    store.delete_file(repo_id, "src/lib.rs").unwrap();

    assert!(!store
        .last_opened_map(repo_id)
        .unwrap()
        .contains_key("src/lib.rs"));
    assert!(store
        .recent_file_opens(&[repo_id], 10, None)
        .unwrap()
        .is_empty());
}

#[test]
fn delete_file_only_touches_the_named_repo() {
    let (_tmp, store) = open_temp();
    let repo_a = store.upsert_repo("a", "/tmp/a").unwrap();
    let repo_b = store.upsert_repo("b", "/tmp/b").unwrap();
    store
        .upsert_file(repo_a, "same/path.rs", "hashA", "rust", 10)
        .unwrap();
    store
        .upsert_file(repo_b, "same/path.rs", "hashB", "rust", 20)
        .unwrap();

    store.delete_file(repo_a, "same/path.rs").unwrap();
    assert!(store.get_file(repo_a, "same/path.rs").unwrap().is_none());
    assert!(store.get_file(repo_b, "same/path.rs").unwrap().is_some());
}

// ── PRR-N3 R1 fix: rails_edges replace/delete are path-scoped ───────

fn sample_rails_edge(dst_path: &str, line: u32) -> crate::frameworks::FrameworkEdge {
    crate::frameworks::FrameworkEdge {
        kind: crate::frameworks::EdgeKind::RenderPartial,
        src_path: String::new(), // overwritten by the caller below
        src_line: Some(line),
        src_symbol: None,
        dst_kind: Some("partial".to_string()),
        dst_path: Some(dst_path.to_string()),
        dst_symbol: None,
        trust: crate::frameworks::Trust::Likely,
        extra_json: None,
    }
}

#[test]
fn replace_rails_edges_on_a_new_blob_drops_the_previous_blobs_rows_for_the_same_path() {
    // R1 — editing a lens-relevant file must not leave the PREVIOUS
    // blob's rows live: the delete is now `(repo_id, src_path)`
    // scoped, not `(blob_hash, salt)`.
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let path = "app/controllers/x_controller.rb";

    let mut edge_a = sample_rails_edge("app/views/x/old.html.erb", 5);
    edge_a.src_path = path.to_string();
    store
        .replace_rails_edges(repo_id, path, "blobA", "rails-lens/1", &[edge_a])
        .unwrap();
    assert_eq!(
        store.rails_edges_by_src_path(repo_id, path).unwrap().len(),
        1
    );
    assert_eq!(
        store
            .rails_edges_by_dst_path(repo_id, "app/views/x/old.html.erb")
            .unwrap()
            .len(),
        1
    );

    // The SAME path, edited — a new blob with a DIFFERENT edge.
    let mut edge_b = sample_rails_edge("app/views/x/new.html.erb", 9);
    edge_b.src_path = path.to_string();
    store
        .replace_rails_edges(repo_id, path, "blobB", "rails-lens/1", &[edge_b])
        .unwrap();

    let src_rows = store.rails_edges_by_src_path(repo_id, path).unwrap();
    assert_eq!(
        src_rows.len(),
        1,
        "only blob B's row must remain: {src_rows:#?}"
    );
    assert_eq!(
        src_rows[0].dst_path.as_deref(),
        Some("app/views/x/new.html.erb")
    );
    assert_eq!(src_rows[0].src_line, Some(9));

    // The OLD blob's dst is no longer reachable via the reverse index.
    assert!(store
        .rails_edges_by_dst_path(repo_id, "app/views/x/old.html.erb")
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .rails_edges_by_dst_path(repo_id, "app/views/x/new.html.erb")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn replace_rails_edges_never_touches_a_different_paths_rows() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let p1 = "app/controllers/x_controller.rb";
    let p2 = "app/controllers/y_controller.rb";

    let mut e1 = sample_rails_edge("app/views/x/show.html.erb", 1);
    e1.src_path = p1.to_string();
    store
        .replace_rails_edges(repo_id, p1, "blobX", "rails-lens/1", &[e1])
        .unwrap();
    let mut e2 = sample_rails_edge("app/views/y/show.html.erb", 1);
    e2.src_path = p2.to_string();
    store
        .replace_rails_edges(repo_id, p2, "blobY", "rails-lens/1", &[e2])
        .unwrap();

    // Re-index p1 (a fresh edit) — p2's row must be untouched.
    let mut e1b = sample_rails_edge("app/views/x/edited.html.erb", 2);
    e1b.src_path = p1.to_string();
    store
        .replace_rails_edges(repo_id, p1, "blobX2", "rails-lens/1", &[e1b])
        .unwrap();

    assert_eq!(store.rails_edges_by_src_path(repo_id, p2).unwrap().len(), 1);
    assert_eq!(
        store.rails_edges_by_src_path(repo_id, p2).unwrap()[0]
            .dst_path
            .as_deref(),
        Some("app/views/y/show.html.erb")
    );
}

#[test]
fn delete_file_prunes_rails_edges_rows_for_the_same_path() {
    // R1 — deleting a controller must not leave phantom
    // `route_action`/`render_partial` rows that `/api/usages` would
    // report as real usages of a live partial forever.
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let path = "app/controllers/x_controller.rb";
    let mut edge = sample_rails_edge("app/views/x/show.html.erb", 5);
    edge.src_path = path.to_string();
    store
        .upsert_file(repo_id, path, "blobA", "ruby", 10)
        .unwrap();
    store
        .replace_rails_edges(repo_id, path, "blobA", "rails-lens/1", &[edge])
        .unwrap();
    assert_eq!(
        store.rails_edges_by_src_path(repo_id, path).unwrap().len(),
        1
    );

    store.delete_file(repo_id, path).unwrap();

    assert!(store
        .rails_edges_by_src_path(repo_id, path)
        .unwrap()
        .is_empty());
    assert!(store
        .rails_edges_by_dst_path(repo_id, "app/views/x/show.html.erb")
        .unwrap()
        .is_empty());
}

// --- V71-G0 — the entity index ---------------------------------------

fn entity_claim(fqn: &str, zeitwerk: Option<&str>) -> crate::entities::EntityDefClaim {
    crate::entities::EntityDefClaim {
        fqn: fqn.to_string(),
        kind: "class".to_string(),
        nesting: crate::entities::NESTING_LEXICAL,
        line_start: 1,
        line_end: 9,
        zeitwerk_fqn: zeitwerk.map(|s| s.to_string()),
    }
}

#[test]
fn entity_defs_round_trip_and_report_whether_the_indexed_blob_is_still_live() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let path = "app/models/reseller/order.rb";
    store
        .upsert_file(repo_id, path, "blobA", "ruby", 10)
        .unwrap();
    store
        .replace_entity_defs(
            repo_id,
            "",
            path,
            "blobA",
            crate::entities::zeitwerk::STATE_READ,
            &[entity_claim("Reseller::Order", Some("Reseller::Order"))],
        )
        .unwrap();

    let rows = store
        .entity_defs_for_name(repo_id, None, "Reseller::Order", 10)
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].live_blob_hash.as_deref(), Some("blobA"));
    assert_eq!(
        rows[0].zeitwerk_state,
        crate::entities::zeitwerk::STATE_READ
    );
    assert_eq!(rows[0].nesting, crate::entities::NESTING_LEXICAL);

    // The file's content moves on; the claim is still there but is now
    // demonstrably about bytes that are gone.
    store
        .upsert_file(repo_id, path, "blobB", "ruby", 11)
        .unwrap();
    let rows = store
        .entity_defs_for_name(repo_id, None, "Reseller::Order", 10)
        .unwrap();
    assert_eq!(rows[0].blob_hash, "blobA");
    assert_eq!(rows[0].live_blob_hash.as_deref(), Some("blobB"));
}

#[test]
fn entity_defs_are_addressable_by_the_zeitwerk_name_as_well_as_the_nested_one() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let path = "app/models/reseller/order.rb";
    store
        .upsert_file(repo_id, path, "blobA", "ruby", 10)
        .unwrap();
    store
        .replace_entity_defs(
            repo_id,
            "",
            path,
            "blobA",
            crate::entities::zeitwerk::STATE_READ,
            // The tree proves `Order`; the convention says
            // `Reseller::Order`. Both must find the row.
            &[entity_claim("Order", Some("Reseller::Order"))],
        )
        .unwrap();
    assert_eq!(
        store
            .entity_defs_for_name(repo_id, None, "Order", 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .entity_defs_for_name(repo_id, None, "Reseller::Order", 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn the_last_segment_fallback_fires_only_when_nothing_matches_exactly() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    for (path, fqn) in [
        ("app/models/reseller/order.rb", "Reseller::Order"),
        ("app/models/billing/order.rb", "Billing::Order"),
        ("app/models/order.rb", "Order"),
    ] {
        store
            .upsert_file(repo_id, path, "blobA", "ruby", 10)
            .unwrap();
        store
            .replace_entity_defs(
                repo_id,
                "",
                path,
                "blobA",
                crate::entities::zeitwerk::STATE_READ,
                &[entity_claim(fqn, None)],
            )
            .unwrap();
    }
    // `Order` matches a real top-level constant EXACTLY, so the
    // fallback must not fire and drag in the two namespaced ones.
    let exact = store
        .entity_defs_for_name(repo_id, None, "Order", 10)
        .unwrap();
    assert_eq!(exact.len(), 1);
    assert_eq!(exact[0].fqn, "Order");
    // A name that matches nothing exactly falls back to the last
    // segment and returns BOTH namespaced constants — the caller
    // reports them as ambiguous rather than picking one.
    store
        .replace_entity_defs(repo_id, "", "app/models/order.rb", "blobA", "read", &[])
        .unwrap();
    let fallback = store
        .entity_defs_for_name(repo_id, None, "Order", 10)
        .unwrap();
    assert_eq!(fallback.len(), 2);
}

#[test]
fn a_like_wildcard_in_a_constant_name_is_escaped_not_matched() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .replace_entity_defs(
            repo_id,
            "",
            "app/models/a.rb",
            "blobA",
            "read",
            &[entity_claim("Api::OrderXv2", None)],
        )
        .unwrap();
    // `_` is a LIKE wildcard: unescaped, `%::Order_v2` would match
    // `Api::OrderXv2`.
    assert!(store
        .entity_defs_for_name(repo_id, None, "Order_v2", 10)
        .unwrap()
        .is_empty());
}

#[test]
fn replace_entity_defs_is_scoped_to_one_worktree_and_one_path() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .replace_entity_defs(
            repo_id,
            "",
            "a.rb",
            "blobA",
            "read",
            &[entity_claim("A", None)],
        )
        .unwrap();
    store
        .replace_entity_defs(
            repo_id,
            "wt1",
            "a.rb",
            "blobB",
            "read",
            &[entity_claim("A", None)],
        )
        .unwrap();
    store
        .replace_entity_defs(
            repo_id,
            "",
            "b.rb",
            "blobC",
            "read",
            &[entity_claim("B", None)],
        )
        .unwrap();

    // Re-indexing the main worktree's `a.rb` must not disturb the
    // linked worktree's row for the same path — the whole point of
    // putting `worktree` in the key.
    store
        .replace_entity_defs(
            repo_id,
            "",
            "a.rb",
            "blobA2",
            "read",
            &[entity_claim("A", None)],
        )
        .unwrap();
    let all = store.entity_defs_for_name(repo_id, None, "A", 10).unwrap();
    assert_eq!(all.len(), 2, "both checkouts still have their row");
    let wt = store
        .entity_defs_for_name(repo_id, Some("wt1"), "A", 10)
        .unwrap();
    assert_eq!(wt.len(), 1);
    assert_eq!(wt[0].blob_hash, "blobB");
    assert_eq!(
        store
            .entity_defs_for_name(repo_id, None, "B", 10)
            .unwrap()
            .len(),
        1,
        "another path in the same worktree is untouched"
    );
}

#[test]
fn delete_file_prunes_entity_defs_rows_for_the_same_path() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let path = "app/models/order.rb";
    store
        .upsert_file(repo_id, path, "blobA", "ruby", 10)
        .unwrap();
    store
        .replace_entity_defs(
            repo_id,
            "",
            path,
            "blobA",
            "read",
            &[entity_claim("Order", None)],
        )
        .unwrap();
    assert_eq!(
        store
            .entity_defs_for_name(repo_id, None, "Order", 10)
            .unwrap()
            .len(),
        1
    );

    store.delete_file(repo_id, path).unwrap();

    assert!(
        store
            .entity_defs_for_name(repo_id, None, "Order", 10)
            .unwrap()
            .is_empty(),
        "a deleted file must not keep answering ?ent= queries"
    );
}

#[test]
fn delete_file_only_prunes_entity_defs_for_the_named_repo() {
    let (_tmp, store) = open_temp();
    let repo_a = store.upsert_repo("a", "/tmp/a").unwrap();
    let repo_b = store.upsert_repo("b", "/tmp/b").unwrap();
    let path = "app/models/order.rb";
    for repo_id in [repo_a, repo_b] {
        store
            .replace_entity_defs(
                repo_id,
                "",
                path,
                "blobA",
                "read",
                &[entity_claim("Order", None)],
            )
            .unwrap();
    }
    store.delete_file(repo_a, path).unwrap();
    assert!(store
        .entity_defs_for_name(repo_a, None, "Order", 10)
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .entity_defs_for_name(repo_b, None, "Order", 10)
            .unwrap()
            .len(),
        1
    );
}

// --- V71-G0 — kbc-seq/1 ----------------------------------------------

#[test]
fn seq_reading_sets_lists_every_kind_and_filters_by_projection_and_workspace() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .create_reading_set("set_ws", repo_id, "ws", None, &[], 1_000)
        .unwrap();
    store
        .update_reading_set_meta(
            "set_ws",
            None,
            None,
            Some("workspace"),
            None,
            None,
            None,
            1_000,
        )
        .unwrap();
    store
        .create_reading_set(
            "set_tour",
            repo_id,
            "tour1",
            None,
            &[whole_file_span("a.rs")],
            1_000,
        )
        .unwrap();
    store
        .update_reading_set_meta(
            "set_tour",
            None,
            None,
            Some("tour"),
            None,
            None,
            None,
            1_000,
        )
        .unwrap();

    let all = store.seq_reading_sets(repo_id, None, None).unwrap();
    assert_eq!(all.len(), 2);
    let tours = store.seq_reading_sets(repo_id, Some("tour"), None).unwrap();
    assert_eq!(tours.len(), 1);
    assert_eq!(tours[0].projection, "tour");
    assert_eq!(tours[0].size, Some(1));
    assert_eq!(tours[0].source, "reading_sets");

    // Unbound: the workspace filter matches nothing yet.
    assert!(store
        .seq_reading_sets(repo_id, None, Some("set_ws"))
        .unwrap()
        .is_empty());
    assert!(store
        .set_reading_set_workspace("set_tour", Some("set_ws"), 2_000)
        .unwrap());
    let bound = store
        .seq_reading_sets(repo_id, None, Some("set_ws"))
        .unwrap();
    assert_eq!(bound.len(), 1);
    assert_eq!(bound[0].id, "set_tour");
    assert_eq!(bound[0].workspace_id.as_deref(), Some("set_ws"));

    // …and unbinding is expressible, which a COALESCE update could not
    // have said at all.
    store
        .set_reading_set_workspace("set_tour", None, 3_000)
        .unwrap();
    assert!(store
        .seq_reading_sets(repo_id, None, Some("set_ws"))
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .get_reading_set("set_tour")
            .unwrap()
            .unwrap()
            .workspace_id,
        None
    );
}

#[test]
fn set_reading_set_workspace_reports_a_missing_id_as_false() {
    let (_tmp, store) = open_temp();
    assert!(!store
        .set_reading_set_workspace("set_nope", Some("set_ws"), 1)
        .unwrap());
}

#[test]
fn like_escape_neutralises_every_sql_wildcard() {
    assert_eq!(like_escape("Order_v2"), "Order\\_v2");
    assert_eq!(like_escape("100%"), "100\\%");
    assert_eq!(like_escape("a\\b"), "a\\\\b");
    assert_eq!(like_escape("Plain"), "Plain");
}

#[test]
fn delete_file_only_prunes_rails_edges_for_the_named_repo() {
    let (_tmp, store) = open_temp();
    let repo_a = store.upsert_repo("a", "/tmp/a").unwrap();
    let repo_b = store.upsert_repo("b", "/tmp/b").unwrap();
    let path = "app/controllers/x_controller.rb";
    let mut edge_a = sample_rails_edge("app/views/x/show.html.erb", 1);
    edge_a.src_path = path.to_string();
    let mut edge_b = sample_rails_edge("app/views/x/show.html.erb", 1);
    edge_b.src_path = path.to_string();
    store
        .replace_rails_edges(repo_a, path, "blobA", "rails-lens/1", &[edge_a])
        .unwrap();
    store
        .replace_rails_edges(repo_b, path, "blobB", "rails-lens/1", &[edge_b])
        .unwrap();

    store.delete_file(repo_a, path).unwrap();

    assert!(store
        .rails_edges_by_src_path(repo_a, path)
        .unwrap()
        .is_empty());
    assert_eq!(
        store.rails_edges_by_src_path(repo_b, path).unwrap().len(),
        1
    );
}

#[test]
fn symbols_for_repo_joins_files_and_symbols_scoped_per_repo() {
    let (_tmp, store) = open_temp();
    let repo_a = store.upsert_repo("a", "/tmp/a").unwrap();
    let repo_b = store.upsert_repo("b", "/tmp/b").unwrap();

    store
        .upsert_file(repo_a, "lib.rs", "hashA", "rust", 10)
        .unwrap();
    store
        .upsert_file(repo_a, "copy.rs", "hashA", "rust", 10)
        .unwrap(); // same content, second path
    store
        .replace_symbols("hashA", "rust@1", &[sample_symbol(0, "alpha")])
        .unwrap();

    store
        .upsert_file(repo_b, "other.rs", "hashB", "rust", 5)
        .unwrap();
    store
        .replace_symbols("hashB", "rust@1", &[sample_symbol(0, "beta")])
        .unwrap();

    let rows = store.symbols_for_repo(repo_a).unwrap();
    let mut got: Vec<(String, String)> = rows
        .iter()
        .map(|(path, sym)| (path.clone(), sym.name.clone()))
        .collect();
    got.sort();
    // "alpha" appears twice: once per path pointing at the shared blob
    // (unlike symbol_count_for_repo, a listing needs a path per hit).
    assert_eq!(
        got,
        vec![
            ("copy.rs".to_string(), "alpha".to_string()),
            ("lib.rs".to_string(), "alpha".to_string()),
        ]
    );
    assert!(store
        .symbols_for_repo(repo_b)
        .unwrap()
        .iter()
        .all(|(_, s)| s.name == "beta"));
}

#[test]
fn symbols_for_repo_hides_a_stale_salt_row_once_a_current_salt_sibling_exists() {
    // V70-A3X — the actual production bug: a blob re-derived under a
    // NEW salt (grammar/query bump) used to leave the OLD salt's rows
    // visible ALONGSIDE the new ones in this un-salted join, doubling
    // every symbol. `lang::RUST.symbol_salt` is the one genuinely "current"
    // salt `current_salt_cte` knows about; a fake old salt stands in
    // for a pre-bump derivation.
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .upsert_file(repo_id, "lib.rs", "hashA", "rust", 10)
        .unwrap();

    // A fresh derivation lands under the REAL current salt.
    store
        .replace_symbols(
            "hashA",
            crate::lang::RUST.symbol_salt,
            &[sample_symbol(0, "new_name")],
        )
        .unwrap();

    // Simulate the OLD grammar's rows STILL sitting in the table (raw
    // INSERT — bypassing `replace_symbols`'s own write-side purge,
    // as if written by a pre-fix binary and never swept) — this
    // isolates the READ-side filter: `symbols_for_repo` must hide it
    // even though nothing purged it at write time.
    store
        .lock()
        .execute(
            "INSERT INTO symbols (blob_hash, salt, ordinal, name, kind, line_start, \
             line_end, col_start, col_end) VALUES (?1, 'rust@stale-fake', 0, 'old_name', \
             'fn', 1, 1, 0, 1)",
            params!["hashA"],
        )
        .unwrap();

    let rows = store.symbols_for_repo(repo_id).unwrap();
    let names: Vec<&str> = rows.iter().map(|(_, s)| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["new_name"],
        "the stale-salt row must be hidden once a current-salt sibling exists: {names:?}"
    );
}

#[test]
fn symbols_for_repo_still_shows_fixture_only_salts_with_no_current_sibling() {
    // V70-A3X fallback: a repo whose ONLY rows are under a non-current
    // (ad hoc test / not-yet-recognised) salt must still show them —
    // this is what keeps the REST of this crate's "rust@1"-style
    // fixtures byte-identical (see `current_salt_cte`'s doc).
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .upsert_file(repo_id, "lib.rs", "hashA", "rust", 10)
        .unwrap();
    store
        .replace_symbols(
            "hashA",
            "rust@ad-hoc-fixture-salt",
            &[sample_symbol(0, "x")],
        )
        .unwrap();

    let rows = store.symbols_for_repo(repo_id).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1.name, "x");
}

#[test]
fn replace_symbols_purges_only_this_blobs_stale_same_language_rows() {
    // A degenerate/empty file can share ONE blob_hash across DIFFERENT
    // languages (different extensions detecting to different
    // `LangInfo`s over identical bytes) — the purge must be scoped to
    // the SAME language as the incoming salt, never touching a
    // sibling language's CURRENT rows for that same blob_hash.
    let (_tmp, store) = open_temp();
    store
        .replace_symbols(
            "sharedBlob",
            crate::lang::PYTHON.symbol_salt,
            &[sample_symbol(0, "py_fn")],
        )
        .unwrap();
    // An OLD rust salt for the SAME blob (simulating a pre-bump rust
    // derivation that happens to share this content).
    store
        .replace_symbols(
            "sharedBlob",
            "rust@old-fake",
            &[sample_symbol(0, "old_rust_fn")],
        )
        .unwrap();

    // Re-derive rust under its CURRENT salt — must purge the old rust
    // row but leave python's untouched.
    store
        .replace_symbols(
            "sharedBlob",
            crate::lang::RUST.symbol_salt,
            &[sample_symbol(0, "new_rust_fn")],
        )
        .unwrap();

    let rust_rows = store
        .symbols_for_blob("sharedBlob", "rust@old-fake")
        .unwrap();
    assert!(rust_rows.is_empty(), "old rust salt must be purged");
    let new_rust = store
        .symbols_for_blob("sharedBlob", crate::lang::RUST.symbol_salt)
        .unwrap();
    assert_eq!(new_rust.len(), 1);
    assert_eq!(new_rust[0].name, "new_rust_fn");
    let py_rows = store
        .symbols_for_blob("sharedBlob", crate::lang::PYTHON.symbol_salt)
        .unwrap();
    assert_eq!(
        py_rows.len(),
        1,
        "a DIFFERENT language's rows for the same blob_hash must survive"
    );
    assert_eq!(py_rows[0].name, "py_fn");
}

#[test]
fn sweep_stale_salt_derived_prunes_only_genuinely_superseded_rows() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .upsert_file(repo_id, "a.rs", "hashA", "rust", 10)
        .unwrap();
    store
        .replace_symbols(
            "hashA",
            crate::lang::RUST.symbol_salt,
            &[sample_symbol(0, "cur")],
        )
        .unwrap();
    // "hashA" ALSO carries a genuinely stale row — direct INSERT
    // (bypassing `replace_symbols`'s own write-side purge), simulating
    // leftover data from BEFORE this fix shipped, which is exactly what
    // the boot-time sweep exists to catch (the write-side purge alone
    // can't clean up rows a pre-fix binary already wrote).
    store
        .lock()
        .execute(
            "INSERT INTO symbols (blob_hash, salt, ordinal, name, kind, line_start, \
             line_end, col_start, col_end) VALUES ('hashA', 'rust@stale-fake', 0, 'old', \
             'fn', 1, 1, 0, 1)",
            [],
        )
        .unwrap();

    // Untouched repo: a fixture-only blob with NO current-salt sibling
    // at all — the sweep must leave it alone entirely.
    let repo_b = store.upsert_repo("b", "/tmp/b").unwrap();
    store
        .upsert_file(repo_b, "b.rs", "hashB", "rust", 5)
        .unwrap();
    store
        .replace_symbols("hashB", "rust@fixture-only", &[sample_symbol(0, "fixture")])
        .unwrap();

    let counts = store.sweep_stale_salt_derived().unwrap();
    assert_eq!(counts.symbols, 1, "exactly the one genuinely-stale row");
    assert_eq!(counts.highlights, 0);
    assert_eq!(counts.occurrences, 0);

    assert!(store
        .symbols_for_blob("hashA", "rust@stale-fake")
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .symbols_for_blob("hashB", "rust@fixture-only")
            .unwrap()
            .len(),
        1,
        "a fixture-only blob with no current sibling must survive the sweep"
    );
}

/// V72-B0 — the property the boot fix rests on: one page touches ONLY
/// its own slice of `files.blob_hash`, the cursor advances, the walk
/// terminates, and the union over pages equals the un-paged result.
/// A page that silently swept the whole table would put the hours-long
/// transaction straight back onto the store's write mutex.
#[test]
fn sweep_stale_salt_page_is_bounded_resumable_and_totals_to_a_full_sweep() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    // Six blobs, each with one current-salt row and one genuinely stale
    // sibling — deliberately more than the page size used below.
    let hashes: Vec<String> = (0..6).map(|i| format!("hash{i}")).collect();
    for (i, h) in hashes.iter().enumerate() {
        store
            .upsert_file(repo_id, &format!("f{i}.rs"), h, "rust", 10)
            .unwrap();
        store
            .replace_symbols(h, crate::lang::RUST.symbol_salt, &[sample_symbol(0, "cur")])
            .unwrap();
        store
            .lock()
            .execute(
                "INSERT INTO symbols (blob_hash, salt, ordinal, name, kind, line_start, \
                 line_end, col_start, col_end) VALUES (?1, 'rust@stale-fake', 0, 'old', \
                 'fn', 1, 1, 0, 1)",
                params![h],
            )
            .unwrap();
    }

    // Page size 2 over 6 blobs: three full pages, then a short/empty one.
    let mut cursor: Option<String> = None;
    let mut swept = 0u64;
    let mut seen_cursors: Vec<String> = Vec::new();
    let mut pages = 0;
    loop {
        let (counts, next) = store.sweep_stale_salt_page(cursor.as_deref(), 2).unwrap();
        pages += 1;
        assert!(
            counts.symbols <= 2,
            "a page of 2 blobs can never delete more than 2 stale symbol rows, got {}",
            counts.symbols
        );
        swept += counts.symbols;
        match next {
            Some(c) => {
                if let Some(prev) = seen_cursors.last() {
                    assert!(&c > prev, "the cursor must advance strictly: {prev} -> {c}");
                }
                seen_cursors.push(c.clone());
                cursor = Some(c);
            }
            None => break,
        }
        assert!(pages < 20, "the paged sweep must terminate");
    }
    assert_eq!(swept, 6, "every blob's one stale row, exactly once");
    assert!(pages >= 3, "6 blobs at 2 per page must take >= 3 pages");

    for h in &hashes {
        assert!(
            store
                .symbols_for_blob(h, "rust@stale-fake")
                .unwrap()
                .is_empty(),
            "{h}'s stale row must be gone"
        );
        assert_eq!(
            store
                .symbols_for_blob(h, crate::lang::RUST.symbol_salt)
                .unwrap()
                .len(),
            1,
            "{h}'s current-salt row must survive"
        );
    }

    // Idempotent: a second full walk finds nothing left to do.
    assert_eq!(store.sweep_stale_salt_derived().unwrap().total(), 0);
}

// ── V72-H2b — the sweep understands BOTH salt families ───────────────

/// A family is stale only when ITS OWN salt moved. The sweep runs one
/// pass per (table, family) pair, so a highlight row keyed by the
/// current HIGHLIGHT salt must survive even though that string is not
/// in the symbol set at all — the bug a single shared `cur` set would
/// have introduced the moment the two salts diverged.
#[test]
fn the_sweep_keeps_each_familys_current_rows_and_prunes_only_its_own_stale_ones() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .upsert_file(repo_id, "a.rs", "hashA", "rust", 10)
        .unwrap();
    // Current rows for both families, written the production way.
    store
        .replace_symbols(
            "hashA",
            crate::lang::RUST.symbol_salt,
            &[sample_symbol(0, "cur")],
        )
        .unwrap();
    store
        .put_highlights(
            "hashA",
            crate::lang::RUST.highlight_salt,
            &[crate::highlight::Span {
                byte_start: 0,
                byte_len: 2,
                class: crate::highlight::HighlightClass::Keyword,
            }],
        )
        .unwrap();
    // A PRE-SPLIT highlight row: painted under the SYMBOL salt, which
    // is exactly what every mirror on disk carries at the moment this
    // unit deploys. Direct INSERT, bypassing the write-side purge, the
    // same way the V70-A3X test simulates old damage.
    store
        .lock()
        .execute(
            "INSERT INTO highlights (blob_hash, salt, spans) VALUES ('hashA', ?1, X'5B5D')",
            params![crate::lang::RUST.symbol_salt],
        )
        .unwrap();

    let counts = store.sweep_stale_salt_derived().unwrap();
    assert_eq!(
        counts.highlights, 1,
        "the pre-split highlight row is stale FOR ITS FAMILY and goes"
    );
    assert_eq!(counts.symbols, 0, "no symbol row was ever stale here");
    assert_eq!(counts.derived_status, 0);

    // The survivors, by family.
    assert_eq!(
        store
            .highlights_for_blob("hashA", crate::lang::RUST.highlight_salt)
            .unwrap()
            .map(|v| v.len()),
        Some(1),
        "the CURRENT highlight salt's row must survive its own sweep"
    );
    assert_eq!(
        store
            .symbols_for_blob("hashA", crate::lang::RUST.symbol_salt)
            .unwrap()
            .len(),
        1
    );
    assert!(store
        .is_derived(
            "hashA",
            crate::lang::SaltFamily::Symbol,
            crate::lang::RUST.symbol_salt
        )
        .unwrap());
    assert!(store
        .is_derived(
            "hashA",
            crate::lang::SaltFamily::Highlight,
            crate::lang::RUST.highlight_salt
        )
        .unwrap());
}

/// The marker table's own two rules: existence is the gate (`Some(0)`
/// is a real answer), and a write purges only the SAME family's other
/// salts for that blob.
#[test]
fn the_derivation_marker_is_per_family_and_records_a_zero_row_derivation() {
    let (_tmp, store) = open_temp();
    store.replace_symbols("blobZ", "rust@old+q1", &[]).unwrap();
    assert!(store
        .is_derived("blobZ", crate::lang::SaltFamily::Symbol, "rust@old+q1")
        .unwrap());
    assert_eq!(
        store
            .derived_rows("blobZ", crate::lang::SaltFamily::Symbol, "rust@old+q1")
            .unwrap(),
        Some(0),
        "an empty derivation is still a derivation"
    );
    assert!(
        !store.has_symbols("blobZ", "rust@old+q1").unwrap(),
        "and the row-count question still honestly answers no"
    );

    // The HIGHLIGHT family is untouched by a symbol-family write ...
    store
        .put_highlights("blobZ", "rust@old+h1+roles2", &[])
        .unwrap();
    assert!(store
        .is_derived(
            "blobZ",
            crate::lang::SaltFamily::Highlight,
            "rust@old+h1+roles2"
        )
        .unwrap());

    // ... and a symbol-salt bump purges the previous SYMBOL marker
    // without touching the highlight one.
    store.replace_symbols("blobZ", "rust@new+q2", &[]).unwrap();
    assert!(!store
        .is_derived("blobZ", crate::lang::SaltFamily::Symbol, "rust@old+q1")
        .unwrap());
    assert!(store
        .is_derived("blobZ", crate::lang::SaltFamily::Symbol, "rust@new+q2")
        .unwrap());
    assert!(
        store
            .is_derived(
                "blobZ",
                crate::lang::SaltFamily::Highlight,
                "rust@old+h1+roles2"
            )
            .unwrap(),
        "a symbol-salt bump must never erase the highlight family's marker"
    );

    // A DIFFERENT language's marker for the same blob (degenerate
    // content shared across extensions) survives too — the purge is
    // language-prefixed, `lang_prefix_pattern`'s own rule.
    store.replace_symbols("blobZ", "python@x+q1", &[]).unwrap();
    store
        .replace_symbols("blobZ", "rust@newer+q3", &[])
        .unwrap();
    assert!(store
        .is_derived("blobZ", crate::lang::SaltFamily::Symbol, "python@x+q1")
        .unwrap());
}

fn sample_symbol(ordinal: u32, name: &str) -> Symbol {
    Symbol {
        ordinal,
        name: name.to_string(),
        kind: "fn".to_string(),
        line_start: 1,
        line_end: 3,
        col_start: 0,
        col_end: 1,
        container: None,
        signature: None,
        doc: None,
        param_min: None,
        param_max: None,
    }
}

#[test]
fn symbols_cache_hit_and_replace_round_trip() {
    let (_tmp, store) = open_temp();
    assert!(!store.has_symbols("blobA", "rust@1").unwrap());

    let syms = vec![sample_symbol(0, "foo"), sample_symbol(1, "bar")];
    store.replace_symbols("blobA", "rust@1", &syms).unwrap();
    assert!(store.has_symbols("blobA", "rust@1").unwrap());

    let got = store.symbols_for_blob("blobA", "rust@1").unwrap();
    assert_eq!(got, syms);

    // Different salt is a separate cache slot entirely.
    assert!(!store.has_symbols("blobA", "rust@2").unwrap());

    // Replacing overwrites, not appends.
    let syms2 = vec![sample_symbol(0, "baz")];
    store.replace_symbols("blobA", "rust@1", &syms2).unwrap();
    assert_eq!(store.symbols_for_blob("blobA", "rust@1").unwrap(), syms2);
}

#[test]
fn highlights_round_trip_json_blob() {
    let (_tmp, store) = open_temp();
    assert!(!store.has_highlights("blobA", "rust@1").unwrap());
    assert_eq!(store.highlights_for_blob("blobA", "rust@1").unwrap(), None);

    let spans = vec![
        Span {
            byte_start: 0,
            byte_len: 3,
            class: HighlightClass::Keyword,
        },
        Span {
            byte_start: 4,
            byte_len: 2,
            class: HighlightClass::Variable,
        },
    ];
    store.put_highlights("blobA", "rust@1", &spans).unwrap();
    assert!(store.has_highlights("blobA", "rust@1").unwrap());
    assert_eq!(
        store.highlights_for_blob("blobA", "rust@1").unwrap(),
        Some(spans)
    );
}

// --- occurrences (B2) ---------------------------------------------------

fn occ(ordinal: u32, name: &str, role: &str, line: u32) -> crate::occurrences::Occurrence {
    crate::occurrences::Occurrence {
        ordinal,
        name: name.to_string(),
        role: role.to_string(),
        line,
        col_start: 0,
        col_end: 1,
        source: crate::occurrences::SOURCE_TS.to_string(),
        local_def_ordinal: None,
    }
}

#[test]
fn occurrences_insert_has_and_lookup_round_trip() {
    let (_tmp, store) = open_temp();
    assert!(!store.has_occurrences("blobA", "rust@1").unwrap());
    assert_eq!(
        store.occurrences_for_blob("blobA", "rust@1").unwrap(),
        vec![]
    );

    let occs = vec![
        occ(0, "foo", "def", 1),
        occ(1, "foo", "ref", 2),
        occ(2, "bar", "def", 3),
    ];
    store.replace_occurrences("blobA", "rust@1", &occs).unwrap();
    assert!(store.has_occurrences("blobA", "rust@1").unwrap());
    assert_eq!(store.occurrences_for_blob("blobA", "rust@1").unwrap(), occs);

    // Different salt is a separate cache slot, same convention as symbols.
    assert!(!store.has_occurrences("blobA", "rust@2").unwrap());

    // Replacing overwrites, not appends.
    let occs2 = vec![occ(0, "baz", "ref", 1)];
    store
        .replace_occurrences("blobA", "rust@1", &occs2)
        .unwrap();
    assert_eq!(
        store.occurrences_for_blob("blobA", "rust@1").unwrap(),
        occs2
    );
}

#[test]
fn def_occurrences_by_name_filters_to_the_def_role_only() {
    let (_tmp, store) = open_temp();
    store
        .replace_occurrences(
            "blobA",
            "rust@1",
            &[
                occ(0, "run", "def", 1),
                occ(1, "run", "ref", 5),
                occ(2, "run", "import", 8),
            ],
        )
        .unwrap();
    let defs = store
        .def_occurrences_by_name("blobA", "rust@1", "run")
        .unwrap();
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].role, "def");
    assert_eq!(defs[0].line, 1);

    assert!(store
        .def_occurrences_by_name("blobA", "rust@1", "zzz-nope")
        .unwrap()
        .is_empty());
}

#[test]
fn occurrence_at_finds_the_covering_span_only() {
    let (_tmp, store) = open_temp();
    store
        .replace_occurrences(
            "blobA",
            "rust@1",
            &[crate::occurrences::Occurrence {
                ordinal: 0,
                name: "widget".to_string(),
                role: "def".to_string(),
                line: 4,
                col_start: 3,
                col_end: 9,
                source: crate::occurrences::SOURCE_TS.to_string(),
                local_def_ordinal: None,
            }],
        )
        .unwrap();

    // Inside the span.
    let hit = store.occurrence_at("blobA", "rust@1", 4, 5).unwrap();
    assert_eq!(hit.map(|o| o.name), Some("widget".to_string()));

    // Exactly at col_start (inclusive).
    assert!(store
        .occurrence_at("blobA", "rust@1", 4, 3)
        .unwrap()
        .is_some());
    // Exactly at col_end (exclusive) — must miss.
    assert!(store
        .occurrence_at("blobA", "rust@1", 4, 9)
        .unwrap()
        .is_none());
    // Wrong line — must miss.
    assert!(store
        .occurrence_at("blobA", "rust@1", 5, 5)
        .unwrap()
        .is_none());
}

// --- occurrences (scip-sourced, S1) -------------------------------------

#[test]
fn replace_scip_occurrences_is_isolated_from_the_ts_source_and_continues_the_ordinal_space() {
    let (_tmp, store) = open_temp();
    store
        .replace_occurrences(
            "blobA",
            "rust@1",
            &[occ(0, "widget", "def", 1), occ(1, "widget", "ref", 2)],
        )
        .unwrap();

    store
        .replace_scip_occurrences(
            "blobA",
            "rust@1",
            &[crate::store::ScipOccurrenceIn {
                name: "widget".to_string(),
                role: "def".to_string(),
                line: 1,
                col_start: 3,
                col_end: 9,
            }],
        )
        .unwrap();

    let all = store.occurrences_for_blob("blobA", "rust@1").unwrap();
    assert_eq!(all.len(), 3, "got: {all:#?}");
    // The scip row's ordinal continues AFTER the two ts ordinals (0, 1)
    // — no primary-key collision.
    let scip_row = all.iter().find(|o| o.source == "scip").expect("a scip row");
    assert_eq!(scip_row.ordinal, 2);

    // A re-derive of the TS pass (`replace_occurrences`) must NOT touch
    // the scip row.
    store
        .replace_occurrences("blobA", "rust@1", &[occ(0, "widget", "def", 1)])
        .unwrap();
    let after = store.occurrences_for_blob("blobA", "rust@1").unwrap();
    assert_eq!(after.len(), 2, "got: {after:#?}"); // 1 ts + 1 scip
    assert!(after
        .iter()
        .any(|o| o.source == "scip" && o.name == "widget"));

    // Re-ingesting scip occurrences REPLACES the prior scip set, still
    // never touching the ts row.
    store
        .replace_scip_occurrences(
            "blobA",
            "rust@1",
            &[crate::store::ScipOccurrenceIn {
                name: "renamed".to_string(),
                role: "def".to_string(),
                line: 1,
                col_start: 3,
                col_end: 10,
            }],
        )
        .unwrap();
    let final_rows = store.occurrences_for_blob("blobA", "rust@1").unwrap();
    assert_eq!(final_rows.len(), 2, "got: {final_rows:#?}");
    assert!(final_rows
        .iter()
        .any(|o| o.source == "scip" && o.name == "renamed"));
    assert!(!final_rows
        .iter()
        .any(|o| o.name == "widget" && o.source == "scip"));
}

#[test]
fn def_occurrences_by_name_is_scoped_to_ts_scip_def_occurrences_by_name_to_scip() {
    let (_tmp, store) = open_temp();
    store
        .replace_occurrences("blobA", "rust@1", &[occ(0, "widget", "def", 1)])
        .unwrap();
    store
        .replace_scip_occurrences(
            "blobA",
            "rust@1",
            &[crate::store::ScipOccurrenceIn {
                name: "widget".to_string(),
                role: "def".to_string(),
                line: 5,
                col_start: 0,
                col_end: 6,
            }],
        )
        .unwrap();

    let ts_defs = store
        .def_occurrences_by_name("blobA", "rust@1", "widget")
        .unwrap();
    assert_eq!(ts_defs.len(), 1);
    assert_eq!(ts_defs[0].line, 1);

    let scip_defs = store
        .scip_def_occurrences_by_name("blobA", "rust@1", "widget")
        .unwrap();
    assert_eq!(scip_defs.len(), 1);
    assert_eq!(scip_defs[0].line, 5);
}

#[test]
fn occurrence_at_prefers_the_scip_row_when_both_sources_cover_the_same_span() {
    let (_tmp, store) = open_temp();
    store
        .replace_occurrences(
            "blobA",
            "rust@1",
            &[crate::occurrences::Occurrence {
                ordinal: 0,
                name: "widget".to_string(),
                role: "ref".to_string(),
                line: 4,
                col_start: 3,
                col_end: 9,
                source: crate::occurrences::SOURCE_TS.to_string(),
                local_def_ordinal: None,
            }],
        )
        .unwrap();
    store
        .replace_scip_occurrences(
            "blobA",
            "rust@1",
            &[crate::store::ScipOccurrenceIn {
                name: "widget".to_string(),
                role: "def".to_string(),
                line: 4,
                col_start: 3,
                col_end: 9,
            }],
        )
        .unwrap();

    let hit = store
        .occurrence_at("blobA", "rust@1", 4, 5)
        .unwrap()
        .expect("a hit");
    assert_eq!(hit.source, "scip", "the scip row must win the tie-break");
    assert_eq!(hit.role, "def");
}

/// Migration `V0010__occurrences_source.sql`'s own contract: `ALTER
/// TABLE occurrences ADD COLUMN source TEXT NOT NULL DEFAULT 'ts'`
/// backfills every PRE-EXISTING row (inserted before the column
/// existed) to `'ts'` for free, no separate UPDATE. Exercised here by
/// inserting a row with a raw SQL statement that OMITS the `source`
/// column entirely (the exact shape SQLite's own `ALTER TABLE ADD
/// COLUMN ... DEFAULT` backfill produces for a row that predates the
/// column) — reading it back through the normal `Store` API must see
/// `source == "ts"`.
#[test]
fn v0010_defaults_pre_existing_rows_without_a_source_column_to_ts() {
    let (_tmp, store) = open_temp();
    store
        .lock()
        .execute(
            "INSERT INTO occurrences (blob_hash, salt, ordinal, name, role, line, col_start, col_end)
             VALUES ('blobA', 'rust@1', 0, 'legacy', 'def', 1, 0, 6)",
            [],
        )
        .unwrap();
    let rows = store.occurrences_for_blob("blobA", "rust@1").unwrap();
    assert_eq!(rows.len(), 1, "got: {rows:#?}");
    assert_eq!(rows[0].name, "legacy");
    assert_eq!(rows[0].source, "ts");
}

// --- W2.1: generation counter + list_files + file_opens ---------------

#[test]
fn generation_starts_at_zero_and_bumps_on_every_mutation() {
    let (_tmp, store) = open_temp();
    assert_eq!(store.generation(), 0);
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    // upsert_repo itself does not bump — only files/symbols mutations do
    // (the search-lane path/symbol caches only ever key on repo_id
    // contents, never the repos table itself).
    assert_eq!(store.generation(), 0);

    store
        .upsert_file(repo_id, "a.rs", "hashA", "rust", 10)
        .unwrap();
    let g1 = store.generation();
    assert!(g1 > 0);

    store.delete_file(repo_id, "a.rs").unwrap();
    let g2 = store.generation();
    assert!(g2 > g1);

    // V70-A3X: a file OPEN is read-only w.r.t. the files/symbols
    // candidate SET, so it must NOT bump the main `generation` any
    // more — see `bump_file_open`'s doc. It bumps its OWN
    // `opens_generation` counter instead (the next test).
    store.bump_file_open(repo_id, "a.rs", 1_000).unwrap();
    assert_eq!(
        store.generation(),
        g2,
        "a file open must not invalidate the files/symbols path caches"
    );
}

#[test]
fn bump_file_open_bumps_only_opens_generation_not_the_main_generation() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .upsert_file(repo_id, "a.rs", "hashA", "rust", 10)
        .unwrap();
    let gen_before = store.generation();
    let opens_gen_before = store.opens_generation();

    store.bump_file_open(repo_id, "a.rs", 1_000).unwrap();

    assert_eq!(store.generation(), gen_before, "main generation untouched");
    assert!(
        store.opens_generation() > opens_gen_before,
        "opens_generation must advance on a file open"
    );

    // A second open advances it again (monotonic, not a one-shot flag).
    let opens_gen_mid = store.opens_generation();
    store.bump_file_open(repo_id, "a.rs", 2_000).unwrap();
    assert!(store.opens_generation() > opens_gen_mid);
    assert_eq!(store.generation(), gen_before);
}

#[test]
fn list_files_returns_every_row_path_ordered() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .upsert_file(repo_id, "z.rs", "hashZ", "rust", 1)
        .unwrap();
    store
        .upsert_file(repo_id, "a.rs", "hashA", "rust", 2)
        .unwrap();
    let rows = store.list_files(repo_id).unwrap();
    assert_eq!(
        rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
        vec!["a.rs", "z.rs"]
    );
}

#[test]
fn bump_file_open_and_last_opened_map_track_the_latest_open() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store.bump_file_open(repo_id, "a.rs", 1_000).unwrap();
    store.bump_file_open(repo_id, "a.rs", 5_000).unwrap();
    store.bump_file_open(repo_id, "b.rs", 2_000).unwrap();

    let map = store.last_opened_map(repo_id).unwrap();
    assert_eq!(map.get("a.rs"), Some(&5_000));
    assert_eq!(map.get("b.rs"), Some(&2_000));
    assert_eq!(map.get("c.rs"), None);
}

#[test]
fn recent_file_opens_orders_newest_first_across_repos_and_respects_limit() {
    let (_tmp, store) = open_temp();
    let repo_a = store.upsert_repo("a", "/tmp/a").unwrap();
    let repo_b = store.upsert_repo("b", "/tmp/b").unwrap();
    store.bump_file_open(repo_a, "old.rs", 1_000).unwrap();
    store.bump_file_open(repo_b, "newer.rs", 3_000).unwrap();
    store.bump_file_open(repo_a, "old.rs", 2_000).unwrap(); // re-open, newer ts
    store.bump_file_open(repo_a, "newest.rs", 9_000).unwrap();

    let rows = store
        .recent_file_opens(&[repo_a, repo_b], 10, None)
        .unwrap();
    assert_eq!(
        rows,
        vec![
            (repo_a, "newest.rs".to_string(), 9_000),
            (repo_b, "newer.rs".to_string(), 3_000),
            (repo_a, "old.rs".to_string(), 2_000),
        ]
    );

    let limited = store.recent_file_opens(&[repo_a, repo_b], 1, None).unwrap();
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].1, "newest.rs");

    assert!(store.recent_file_opens(&[], 10, None).unwrap().is_empty());
}

#[test]
fn recent_file_opens_path_filter_applies_in_sql_before_limit() {
    // V70-A3X: an unfiltered `limit=1` picks the single newest open
    // ("newest.rs") — a path filter that excludes it must fall through
    // to the next-newest MATCHING row, not just re-check the already
    // narrowed top-1 page (proving the filter runs in the SQL query,
    // not as a post-filter over an already-`LIMIT`-ed result).
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store.bump_file_open(repo_id, "keep/old.rs", 1_000).unwrap();
    store.bump_file_open(repo_id, "newest.rs", 9_000).unwrap();

    let unfiltered = store.recent_file_opens(&[repo_id], 1, None).unwrap();
    assert_eq!(unfiltered, vec![(repo_id, "newest.rs".to_string(), 9_000)]);

    let filtered = store
        .recent_file_opens(&[repo_id], 1, Some("keep/"))
        .unwrap();
    assert_eq!(filtered, vec![(repo_id, "keep/old.rs".to_string(), 1_000)]);

    // Case-insensitive substring, same grammar as `search::grammar`'s
    // `path:` filter.
    let cased = store
        .recent_file_opens(&[repo_id], 10, Some("KEEP/"))
        .unwrap();
    assert_eq!(cased, vec![(repo_id, "keep/old.rs".to_string(), 1_000)]);
}

#[test]
fn ref_and_def_occurrences_by_name_in_repo_hide_stale_salt_duplicates() {
    // V70-A3X — same production bug as symbols, one layer over
    // (`usages.rs`'s "find usages" / Code Vision counts): a stale-salt
    // occurrence row must not double a genuine current-salt hit once
    // both exist for the same blob.
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .upsert_file(repo_id, "a.rs", "hashA", "rust", 10)
        .unwrap();
    store
        .replace_occurrences(
            "hashA",
            crate::lang::RUST.symbol_salt,
            &[occ(0, "widget", "ref", 2)],
        )
        .unwrap();
    // A genuinely stale sibling row for the SAME blob (raw INSERT under
    // a DIFFERENT salt — no PK collision, since salt is part of the key
    // — bypassing `replace_occurrences`'s write-side purge entirely):
    // simulates data left behind from BEFORE this fix shipped, which
    // the READ-side fallback-aware filter must still hide.
    store
        .lock()
        .execute(
            "INSERT INTO occurrences (blob_hash, salt, ordinal, name, role, line, \
             col_start, col_end, source) VALUES ('hashA', 'rust@stale-fake', 0, 'widget', \
             'ref', 1, 0, 1, 'ts')",
            [],
        )
        .unwrap();

    let refs = store
        .ref_occurrences_by_name_in_repo(repo_id, "widget")
        .unwrap();
    assert_eq!(refs.len(), 1, "got {refs:?}");
    assert_eq!(refs[0].1.line, 2, "must be the CURRENT-salt row's data");

    let by_names = store
        .occurrences_by_names_in_repo(repo_id, &["widget".to_string()])
        .unwrap();
    assert_eq!(by_names.len(), 1, "got {by_names:?}");

    // Fallback: a repo whose ONLY occurrence rows are under a
    // non-current salt still shows them (no current sibling exists).
    let repo_b = store.upsert_repo("b", "/tmp/b").unwrap();
    store
        .upsert_file(repo_b, "b.rs", "hashB", "rust", 5)
        .unwrap();
    store
        .replace_occurrences("hashB", "rust@fixture-only", &[occ(0, "gadget", "def", 1)])
        .unwrap();
    let defs = store
        .def_occurrences_by_name_in_repo(repo_b, "gadget")
        .unwrap();
    assert_eq!(
        defs.len(),
        1,
        "fixture-only salt must still surface: {defs:?}"
    );
}

#[test]
fn put_highlights_purges_only_this_blobs_stale_same_language_rows() {
    let (_tmp, store) = open_temp();
    let old_spans = vec![];
    store
        .put_highlights("sharedBlob", "rust@old-fake", &old_spans)
        .unwrap();
    store
        .put_highlights("sharedBlob", crate::lang::PYTHON.symbol_salt, &old_spans)
        .unwrap();

    store
        .put_highlights("sharedBlob", crate::lang::RUST.symbol_salt, &old_spans)
        .unwrap();

    assert!(store
        .highlights_for_blob("sharedBlob", "rust@old-fake")
        .unwrap()
        .is_none());
    assert!(store
        .highlights_for_blob("sharedBlob", crate::lang::RUST.symbol_salt)
        .unwrap()
        .is_some());
    assert!(
        store
            .highlights_for_blob("sharedBlob", crate::lang::PYTHON.symbol_salt)
            .unwrap()
            .is_some(),
        "a DIFFERENT language's highlights for the same blob_hash must survive"
    );
}

// --- W2.3: chunk_status (semantic lane bookkeeping) --------------------

#[test]
fn has_chunks_and_mark_chunked_round_trip() {
    let (_tmp, store) = open_temp();
    assert!(!store.has_chunks("blobA", "rust@1").unwrap());

    store.mark_chunked("blobA", "rust@1", 3).unwrap();
    assert!(store.has_chunks("blobA", "rust@1").unwrap());

    // Different salt is a separate cache slot, same as symbols/highlights.
    assert!(!store.has_chunks("blobA", "rust@2").unwrap());

    // Upsert: re-marking updates chunk_count in place, not a new row
    // (verified indirectly — has_chunks still true, no error on repeat).
    store.mark_chunked("blobA", "rust@1", 5).unwrap();
    assert!(store.has_chunks("blobA", "rust@1").unwrap());
}

#[test]
fn clear_chunk_status_for_blob_removes_every_salt() {
    let (_tmp, store) = open_temp();
    store.mark_chunked("blobA", "rust@1", 2).unwrap();
    store.mark_chunked("blobA", "rust@2", 1).unwrap();
    store.mark_chunked("blobB", "rust@1", 4).unwrap();

    store.clear_chunk_status_for_blob("blobA").unwrap();
    assert!(!store.has_chunks("blobA", "rust@1").unwrap());
    assert!(!store.has_chunks("blobA", "rust@2").unwrap());
    // A different blob's status is untouched.
    assert!(store.has_chunks("blobB", "rust@1").unwrap());

    // Clearing an already-clear blob is a no-op, not an error.
    store.clear_chunk_status_for_blob("blobA").unwrap();
}

#[test]
fn orphaned_chunk_blobs_finds_only_blobs_with_no_owning_file_anywhere() {
    let (_tmp, store) = open_temp();
    let repo_a = store.upsert_repo("a", "/tmp/a").unwrap();
    let repo_b = store.upsert_repo("b", "/tmp/b").unwrap();

    store
        .upsert_file(repo_a, "a.rs", "hashLive", "rust", 10)
        .unwrap();
    store.mark_chunked("hashLive", "rust@1", 2).unwrap();
    // Orphaned: chunked once, but no files row (anywhere) references it.
    store.mark_chunked("hashGone", "rust@1", 1).unwrap();

    let orphans = store.orphaned_chunk_blobs().unwrap();
    assert_eq!(
        orphans,
        vec![("hashGone".to_string(), "rust@1".to_string())]
    );

    // A blob referenced from a DIFFERENT repo must not be reported —
    // blob_hash sharing is global (ADR-2), so the ref-count is too.
    store
        .upsert_file(repo_b, "b.rs", "hashLive", "rust", 10)
        .unwrap();
    store.delete_file(repo_a, "a.rs").unwrap();
    let orphans2 = store.orphaned_chunk_blobs().unwrap();
    assert_eq!(
        orphans2,
        vec![("hashGone".to_string(), "rust@1".to_string())],
        "hashLive still lives in repo b — must not be reported as orphaned"
    );

    // Once EVERY referencing file is gone, it becomes an orphan too.
    store.delete_file(repo_b, "b.rs").unwrap();
    let mut orphans3 = store.orphaned_chunk_blobs().unwrap();
    orphans3.sort();
    assert_eq!(
        orphans3,
        vec![
            ("hashGone".to_string(), "rust@1".to_string()),
            ("hashLive".to_string(), "rust@1".to_string()),
        ]
    );
}

// --- transcripts (W2.5) -------------------------------------------------

fn sample_turn(
    uuid: &str,
    session_id: &str,
    kind: &'static str,
    ts: i64,
    text: &str,
) -> IndexedTurn {
    IndexedTurn {
        turn: crate::transcripts::parse::ParsedTurn {
            session_id: session_id.to_string(),
            uuid: uuid.to_string(),
            parent_uuid: None,
            ts,
            kind,
            tool_name: None,
            file_paths: Vec::new(),
            is_sidechain: false,
            text: text.to_string(),
        },
        byte_offset: 0,
        byte_len: text.len() as i64,
    }
}

#[test]
fn transcript_file_state_round_trips_and_updates_in_place() {
    let (_tmp, store) = open_temp();
    assert!(store.get_transcript_file("proj/a.jsonl").unwrap().is_none());

    let id1 = store
        .upsert_transcript_file_state("proj", "proj/a.jsonl", 111, 0, 1_000)
        .unwrap();
    let row = store.get_transcript_file("proj/a.jsonl").unwrap().unwrap();
    assert_eq!(row.id, id1);
    assert_eq!(row.inode, 111);
    assert_eq!(row.byte_offset, 0);

    // Same src_file → same row, updated in place (not a new row).
    let id2 = store
        .upsert_transcript_file_state("proj", "proj/a.jsonl", 111, 500, 2_000)
        .unwrap();
    assert_eq!(id1, id2);
    let row2 = store.get_transcript_file("proj/a.jsonl").unwrap().unwrap();
    assert_eq!(row2.byte_offset, 500);
    assert_eq!(row2.mtime, 2_000);
}

#[test]
fn insert_transcript_turns_makes_them_searchable_newest_first() {
    let (_tmp, store) = open_temp();
    let file_id = store
        .upsert_transcript_file_state("proj", "proj/a.jsonl", 1, 0, 0)
        .unwrap();
    let turns = vec![
        sample_turn("u1", "s1", "user", 1_000, "an unusual error string alpha"),
        sample_turn(
            "u2",
            "s1",
            "assistant",
            2_000,
            "an unusual error string beta",
        ),
    ];
    store.insert_transcript_turns(file_id, &turns).unwrap();

    let hits = store.search_transcripts("unusual", 10, None, None).unwrap();
    assert_eq!(hits.len(), 2);
    // Newest first: ts=2_000 (uuid u2) before ts=1_000 (uuid u1).
    assert_eq!(hits[0].uuid, "u2");
    assert_eq!(hits[1].uuid, "u1");
    assert_eq!(hits[0].project_dir, "proj");
    assert_eq!(hits[0].src_file, "proj/a.jsonl");
}

#[test]
fn search_transcripts_session_and_kind_filters_narrow_results() {
    let (_tmp, store) = open_temp();
    let file_id = store
        .upsert_transcript_file_state("proj", "proj/a.jsonl", 1, 0, 0)
        .unwrap();
    let turns = vec![
        sample_turn("u1", "s1", "user", 1_000, "widget lookup"),
        sample_turn("u2", "s2", "user", 2_000, "widget lookup"),
        sample_turn("u3", "s1", "assistant", 3_000, "widget lookup"),
    ];
    store.insert_transcript_turns(file_id, &turns).unwrap();

    let by_session = store
        .search_transcripts("widget", 10, Some("s1"), None)
        .unwrap();
    assert_eq!(by_session.len(), 2);
    assert!(by_session.iter().all(|h| h.session_id == "s1"));

    let by_kind = store
        .search_transcripts("widget", 10, None, Some("assistant"))
        .unwrap();
    assert_eq!(by_kind.len(), 1);
    assert_eq!(by_kind[0].uuid, "u3");

    let both = store
        .search_transcripts("widget", 10, Some("s1"), Some("user"))
        .unwrap();
    assert_eq!(both.len(), 1);
    assert_eq!(both[0].uuid, "u1");
}

/// A `tool_use` turn variant of [`sample_turn`], carrying a `tool_name`
/// and `file_paths` — `sessiondiff`'s own data source, which
/// `search_transcripts`' tests above never need.
fn sample_tool_use_turn(
    uuid: &str,
    session_id: &str,
    ts: i64,
    tool_name: &str,
    file_paths: &[&str],
) -> IndexedTurn {
    IndexedTurn {
        turn: crate::transcripts::parse::ParsedTurn {
            session_id: session_id.to_string(),
            uuid: uuid.to_string(),
            parent_uuid: None,
            ts,
            kind: "tool_use",
            tool_name: Some(tool_name.to_string()),
            file_paths: file_paths.iter().map(|s| s.to_string()).collect(),
            is_sidechain: false,
            text: format!("{tool_name} edit"),
        },
        byte_offset: 0,
        byte_len: 10,
    }
}

#[test]
fn transcript_turns_for_session_returns_every_turn_oldest_first_with_file_paths() {
    let (_tmp, store) = open_temp();
    let file_id = store
        .upsert_transcript_file_state("proj", "proj/a.jsonl", 1, 0, 0)
        .unwrap();
    let turns = vec![
        sample_turn("u1", "s1", "user", 3_000, "third"),
        sample_tool_use_turn("u2", "s1", 1_000, "Edit", &["/repo/a.rs"]),
        sample_turn("u3", "s2", "user", 500, "other session"),
        sample_turn("u4", "s1", "assistant", 2_000, "second"),
    ];
    store.insert_transcript_turns(file_id, &turns).unwrap();

    let rows = store.transcript_turns_for_session("s1").unwrap();
    assert_eq!(rows.len(), 3, "only s1's turns, s2's u3 excluded");
    // Oldest first (ts ASC) — the session's own narrative order, the
    // OPPOSITE of search_transcripts' newest-first convention.
    assert_eq!(rows[0].uuid, "u2");
    assert_eq!(rows[0].tool_name.as_deref(), Some("Edit"));
    assert_eq!(rows[0].file_paths, vec!["/repo/a.rs".to_string()]);
    assert_eq!(rows[1].uuid, "u4");
    assert_eq!(rows[2].uuid, "u1");

    assert!(store
        .transcript_turns_for_session("unknown-session")
        .unwrap()
        .is_empty());
}

#[test]
fn delete_transcript_turns_for_file_removes_fts_rows_too() {
    let (_tmp, store) = open_temp();
    let file_id = store
        .upsert_transcript_file_state("proj", "proj/a.jsonl", 1, 0, 0)
        .unwrap();
    store
        .insert_transcript_turns(file_id, &[sample_turn("u1", "s1", "user", 1_000, "gizmo")])
        .unwrap();
    assert_eq!(
        store
            .search_transcripts("gizmo", 10, None, None)
            .unwrap()
            .len(),
        1
    );

    store.delete_transcript_turns_for_file(file_id).unwrap();
    assert!(store
        .search_transcripts("gizmo", 10, None, None)
        .unwrap()
        .is_empty());
}

#[test]
fn transcript_stats_counts_files_turns_and_bytes() {
    let (_tmp, store) = open_temp();
    assert_eq!(
        store.transcript_stats().unwrap(),
        TranscriptStats::default()
    );

    let file_a = store
        .upsert_transcript_file_state("proj", "proj/a.jsonl", 1, 0, 0)
        .unwrap();
    let file_b = store
        .upsert_transcript_file_state("proj", "proj/b.jsonl", 2, 0, 0)
        .unwrap();
    store
        .insert_transcript_turns(file_a, &[sample_turn("u1", "s1", "user", 1_000, "12345")])
        .unwrap();
    store
        .insert_transcript_turns(
            file_b,
            &[
                sample_turn("u2", "s1", "user", 2_000, "1234567890"),
                sample_turn("u3", "s1", "assistant", 3_000, "12"),
            ],
        )
        .unwrap();

    let stats = store.transcript_stats().unwrap();
    assert_eq!(stats.files, 2);
    assert_eq!(stats.turns, 3);
    assert_eq!(stats.indexed_bytes, 5 + 10 + 2);
}

fn sample_turn_with_paths(
    uuid: &str,
    session_id: &str,
    ts: i64,
    file_paths: Vec<String>,
) -> IndexedTurn {
    IndexedTurn {
        turn: crate::transcripts::parse::ParsedTurn {
            session_id: session_id.to_string(),
            uuid: uuid.to_string(),
            parent_uuid: None,
            ts,
            kind: crate::transcripts::parse::KIND_TOOL_USE,
            tool_name: Some("Edit".to_string()),
            file_paths,
            is_sidechain: false,
            text: "Edit ...".to_string(),
        },
        byte_offset: 0,
        byte_len: 8,
    }
}

#[test]
fn transcript_sessions_touching_path_finds_exact_matches_newest_first() {
    let (_tmp, store) = open_temp();
    let file_id = store
        .upsert_transcript_file_state("proj", "proj/a.jsonl", 1, 0, 0)
        .unwrap();
    store
        .insert_transcript_turns(
            file_id,
            &[
                sample_turn_with_paths(
                    "u1",
                    "s-older",
                    1_000,
                    vec!["/repo/src/lib.rs".to_string()],
                ),
                sample_turn_with_paths(
                    "u2",
                    "s-newer",
                    2_000,
                    vec![
                        "/repo/src/lib.rs".to_string(),
                        "/repo/README.md".to_string(),
                    ],
                ),
                // A DIFFERENT, longer path that merely ENDS in the same
                // basename must never match — proves the quote-delimited
                // needle isn't a bare substring scan.
                sample_turn_with_paths(
                    "u3",
                    "s-unrelated",
                    3_000,
                    vec!["/repo/src/other_lib.rs".to_string()],
                ),
            ],
        )
        .unwrap();

    let hits = store
        .transcript_sessions_touching_path("/repo/src/lib.rs", 10)
        .unwrap();
    let sessions: Vec<&str> = hits.iter().map(|h| h.session_id.as_str()).collect();
    assert_eq!(
        sessions,
        vec!["s-newer", "s-older"],
        "newest-first, and the unrelated longer path must not match: {hits:?}"
    );
}

#[test]
fn transcript_sessions_touching_path_is_bounded_by_limit() {
    let (_tmp, store) = open_temp();
    let file_id = store
        .upsert_transcript_file_state("proj", "proj/a.jsonl", 1, 0, 0)
        .unwrap();
    let turns: Vec<IndexedTurn> = (0..5)
        .map(|i| {
            sample_turn_with_paths(
                &format!("u{i}"),
                &format!("s{i}"),
                1_000 + i,
                vec!["/repo/hot_file.rs".to_string()],
            )
        })
        .collect();
    store.insert_transcript_turns(file_id, &turns).unwrap();

    let hits = store
        .transcript_sessions_touching_path("/repo/hot_file.rs", 2)
        .unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].session_id, "s4", "newest first");
}

#[test]
fn transcript_sessions_touching_path_no_match_is_an_honest_empty_vec() {
    let (_tmp, store) = open_temp();
    let file_id = store
        .upsert_transcript_file_state("proj", "proj/a.jsonl", 1, 0, 0)
        .unwrap();
    store
        .insert_transcript_turns(
            file_id,
            &[sample_turn_with_paths(
                "u1",
                "s1",
                1_000,
                vec!["/repo/other.rs".to_string()],
            )],
        )
        .unwrap();

    let hits = store
        .transcript_sessions_touching_path("/repo/nothing_here.rs", 10)
        .unwrap();
    assert!(hits.is_empty());
}

// --- commit_sessions (W3.2 join ladder cache) --------------------------

fn sample_commit_session(confidence: &str, via: &str) -> CommitSessionRow {
    CommitSessionRow {
        confidence: confidence.to_string(),
        via: via.to_string(),
        session_id: Some("sess-1".to_string()),
        kb: Some("memory".to_string()),
        display_name: Some("fixed the gizmo race".to_string()),
        started_at: Some(1_700_000_000),
        resolved_at: 1_700_000_100,
    }
}

#[test]
fn commit_session_round_trips_and_misses_cleanly() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    assert_eq!(store.get_commit_session(repo_id, "deadbeef").unwrap(), None);

    let row = sample_commit_session("exact", "by-commit");
    store
        .upsert_commit_session(repo_id, "deadbeef", &row)
        .unwrap();
    let got = store.get_commit_session(repo_id, "deadbeef").unwrap();
    assert_eq!(got, Some(row));

    // A different sha in the same repo is a separate cache slot.
    assert_eq!(store.get_commit_session(repo_id, "other").unwrap(), None);
}

#[test]
fn commit_session_upsert_replaces_every_column_in_place() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .upsert_commit_session(repo_id, "sha1", &sample_commit_session("none", "no-match"))
        .unwrap();
    assert_eq!(
        store
            .get_commit_session(repo_id, "sha1")
            .unwrap()
            .unwrap()
            .confidence,
        "none"
    );

    // A later re-resolution upgrades none -> fuzzy, replacing the row
    // wholesale (not merging fields).
    let upgraded = CommitSessionRow {
        confidence: "fuzzy".to_string(),
        via: "time-window".to_string(),
        session_id: Some("sess-2".to_string()),
        kb: Some("memory".to_string()),
        display_name: None,
        started_at: Some(1_700_050_000),
        resolved_at: 1_700_060_000,
    };
    store
        .upsert_commit_session(repo_id, "sha1", &upgraded)
        .unwrap();
    assert_eq!(
        store.get_commit_session(repo_id, "sha1").unwrap(),
        Some(upgraded)
    );
}

#[test]
fn commit_session_is_scoped_per_repo() {
    let (_tmp, store) = open_temp();
    let repo_a = store.upsert_repo("a", "/tmp/a").unwrap();
    let repo_b = store.upsert_repo("b", "/tmp/b").unwrap();
    store
        .upsert_commit_session(repo_a, "sha1", &sample_commit_session("exact", "by-commit"))
        .unwrap();
    assert!(store.get_commit_session(repo_a, "sha1").unwrap().is_some());
    assert_eq!(store.get_commit_session(repo_b, "sha1").unwrap(), None);
}

#[test]
fn commit_session_none_row_carries_no_enrichment() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let none_row = CommitSessionRow {
        confidence: "none".to_string(),
        via: "kb-unreachable".to_string(),
        session_id: None,
        kb: None,
        display_name: None,
        started_at: None,
        resolved_at: 1_700_000_000,
    };
    store
        .upsert_commit_session(repo_id, "sha1", &none_row)
        .unwrap();
    let got = store.get_commit_session(repo_id, "sha1").unwrap().unwrap();
    assert_eq!(got.session_id, None);
    assert_eq!(got.kb, None);
    assert_eq!(got.display_name, None);
    assert_eq!(got.started_at, None);
}

// --- annotations (W4.6) -------------------------------------------------

fn sample_annotation(id: &str, repo_id: i64, path: &str) -> AnnotationRow {
    AnnotationRow {
        id: id.to_string(),
        repo_id,
        path: path.to_string(),
        anchor: Some(
            r#"{"kind":"selection","css_path":"","offset":1,"snippet":"fn a() {}"}"#.to_string(),
        ),
        anchor_kind: "line".to_string(),
        anchor2: None,
        parent_id: None,
        intent: "note".to_string(),
        body: "why is this here?".to_string(),
        author: "you".to_string(),
        created_at: 1_700_000_000,
        updated_at: 1_700_000_000,
        resolved: false,
        review_id: None,
        ps_number: None,
        side: None,
        set_id: None,
        trail_id: None,
    }
}

#[test]
fn annotation_insert_list_get_round_trip() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let row = sample_annotation("ann_1", repo_id, "src/lib.rs");
    store.insert_annotation(&row).unwrap();

    assert_eq!(store.get_annotation("ann_1").unwrap(), Some(row.clone()));
    assert_eq!(store.get_annotation("nope").unwrap(), None);

    let listed = store.list_annotations(repo_id, "src/lib.rs").unwrap();
    assert_eq!(listed, vec![row]);
    assert!(store
        .list_annotations(repo_id, "src/other.rs")
        .unwrap()
        .is_empty());
}

#[test]
fn annotation_list_is_scoped_per_path_and_ordered_by_creation() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let mut first = sample_annotation("ann_1", repo_id, "a.rs");
    first.created_at = 100;
    let mut second = sample_annotation("ann_2", repo_id, "a.rs");
    second.created_at = 200;
    let other_path = sample_annotation("ann_3", repo_id, "b.rs");
    // Insert out of chronological order to prove the ORDER BY, not
    // insertion order, decides.
    store.insert_annotation(&second).unwrap();
    store.insert_annotation(&first).unwrap();
    store.insert_annotation(&other_path).unwrap();

    let listed = store.list_annotations(repo_id, "a.rs").unwrap();
    assert_eq!(
        listed.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
        vec!["ann_1", "ann_2"]
    );
}

#[test]
fn annotation_update_patches_only_given_fields_and_reports_existence() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let row = sample_annotation("ann_1", repo_id, "a.rs");
    store.insert_annotation(&row).unwrap();

    // Body only.
    assert!(store
        .update_annotation("ann_1", Some("edited body"), None, None, 1_700_000_100)
        .unwrap());
    let got = store.get_annotation("ann_1").unwrap().unwrap();
    assert_eq!(got.body, "edited body");
    assert!(!got.resolved);
    assert_eq!(got.intent, "note");
    assert_eq!(got.updated_at, 1_700_000_100);

    // Resolved only — body from the previous update must survive.
    assert!(store
        .update_annotation("ann_1", None, Some(true), None, 1_700_000_200)
        .unwrap());
    let got = store.get_annotation("ann_1").unwrap().unwrap();
    assert_eq!(got.body, "edited body");
    assert!(got.resolved);
    assert_eq!(got.updated_at, 1_700_000_200);

    // Intent only — body/resolved from previous updates must survive.
    assert!(store
        .update_annotation("ann_1", None, None, Some("todo"), 1_700_000_250)
        .unwrap());
    let got = store.get_annotation("ann_1").unwrap().unwrap();
    assert_eq!(got.body, "edited body");
    assert!(got.resolved);
    assert_eq!(got.intent, "todo");
    assert_eq!(got.updated_at, 1_700_000_250);

    // Missing id.
    assert!(!store
        .update_annotation("nope", Some("x"), None, None, 1_700_000_300)
        .unwrap());
}

#[test]
fn annotation_update_never_touches_anchor_columns() {
    // The v1 rule ("PATCH never changes anchors"), preserved through
    // D-server — `update_annotation`'s SQL simply has no `anchor*`
    // column in its SET list at all, but pin it with a real round trip
    // anyway (a future careless edit to that SQL would show up here).
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let row = sample_annotation("ann_1", repo_id, "a.rs");
    store.insert_annotation(&row).unwrap();

    store
        .update_annotation(
            "ann_1",
            Some("edited"),
            Some(true),
            Some("flag-for-agent"),
            1_700_000_100,
        )
        .unwrap();
    let got = store.get_annotation("ann_1").unwrap().unwrap();
    assert_eq!(got.anchor, row.anchor);
    assert_eq!(got.anchor_kind, row.anchor_kind);
    assert_eq!(got.anchor2, row.anchor2);
    assert_eq!(got.parent_id, row.parent_id);
}

#[test]
fn annotation_delete_removes_the_row_and_reports_existence() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let row = sample_annotation("ann_1", repo_id, "a.rs");
    store.insert_annotation(&row).unwrap();

    assert!(store.delete_annotation("ann_1").unwrap());
    assert_eq!(store.get_annotation("ann_1").unwrap(), None);
    // Already gone — reports false, not an error.
    assert!(!store.delete_annotation("ann_1").unwrap());
}

// --- D-server: anchor kinds / threads / intents -------------------------

fn sample_reply(id: &str, parent: &AnnotationRow, body: &str) -> AnnotationRow {
    AnnotationRow {
        id: id.to_string(),
        repo_id: parent.repo_id,
        path: parent.path.clone(),
        anchor: None,
        anchor_kind: "line".to_string(),
        anchor2: None,
        parent_id: Some(parent.id.clone()),
        intent: "note".to_string(),
        body: body.to_string(),
        author: "you".to_string(),
        created_at: parent.created_at + 1,
        updated_at: parent.created_at + 1,
        resolved: false,
        review_id: parent.review_id,
        ps_number: parent.ps_number,
        side: parent.side.clone(),
        set_id: parent.set_id.clone(),
        trail_id: parent.trail_id.clone(),
    }
}

#[test]
fn annotation_delete_of_a_parent_cascades_to_its_replies() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let parent = sample_annotation("ann_parent", repo_id, "a.rs");
    store.insert_annotation(&parent).unwrap();
    let reply1 = sample_reply("ann_reply1", &parent, "first reply");
    let reply2 = sample_reply("ann_reply2", &parent, "second reply");
    store.insert_annotation(&reply1).unwrap();
    store.insert_annotation(&reply2).unwrap();
    assert_eq!(store.list_annotations(repo_id, "a.rs").unwrap().len(), 3);

    assert!(store.delete_annotation("ann_parent").unwrap());

    assert_eq!(store.get_annotation("ann_parent").unwrap(), None);
    assert_eq!(store.get_annotation("ann_reply1").unwrap(), None);
    assert_eq!(store.get_annotation("ann_reply2").unwrap(), None);
    assert!(store.list_annotations(repo_id, "a.rs").unwrap().is_empty());
}

#[test]
fn annotation_delete_of_a_reply_only_removes_that_one_row() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let parent = sample_annotation("ann_parent", repo_id, "a.rs");
    store.insert_annotation(&parent).unwrap();
    let reply = sample_reply("ann_reply", &parent, "a reply");
    store.insert_annotation(&reply).unwrap();

    assert!(store.delete_annotation("ann_reply").unwrap());
    assert!(store.get_annotation("ann_parent").unwrap().is_some());
    assert_eq!(store.get_annotation("ann_reply").unwrap(), None);
}

#[test]
fn list_open_annotations_excludes_resolved_and_replies() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();

    let mut open_one = sample_annotation("ann_open", repo_id, "a.rs");
    open_one.created_at = 100;
    let mut resolved_one = sample_annotation("ann_resolved", repo_id, "a.rs");
    resolved_one.created_at = 200;
    resolved_one.resolved = true;
    store.insert_annotation(&open_one).unwrap();
    store.insert_annotation(&resolved_one).unwrap();
    let reply = sample_reply("ann_reply", &open_one, "a reply");
    store.insert_annotation(&reply).unwrap();

    let open = store
        .list_open_annotations(repo_id, None, None, 500)
        .unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].0.id, "ann_open");
    assert_eq!(open[0].1, 1, "one direct reply");
}

#[test]
fn list_open_annotations_filters_by_intent_and_path_prefix() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();

    let mut a = sample_annotation("ann_a", repo_id, "src/lib.rs");
    a.intent = "todo".to_string();
    let mut b = sample_annotation("ann_b", repo_id, "src/main.rs");
    b.intent = "question".to_string();
    let mut c = sample_annotation("ann_c", repo_id, "docs/readme.md");
    c.intent = "todo".to_string();
    store.insert_annotation(&a).unwrap();
    store.insert_annotation(&b).unwrap();
    store.insert_annotation(&c).unwrap();

    let todos = store
        .list_open_annotations(repo_id, Some("todo"), None, 500)
        .unwrap();
    assert_eq!(
        todos.iter().map(|(r, _)| r.id.as_str()).collect::<Vec<_>>(),
        vec!["ann_c", "ann_a"],
        "newest first"
    );

    let under_src = store
        .list_open_annotations(repo_id, None, Some("src/"), 500)
        .unwrap();
    assert_eq!(
        under_src
            .iter()
            .map(|(r, _)| r.id.as_str())
            .collect::<std::collections::HashSet<_>>(),
        std::collections::HashSet::from(["ann_a", "ann_b"])
    );

    let both = store
        .list_open_annotations(repo_id, Some("todo"), Some("src/"), 500)
        .unwrap();
    assert_eq!(both.len(), 1);
    assert_eq!(both[0].0.id, "ann_a");
}

#[test]
fn list_open_annotations_respects_the_limit_for_truncation_detection() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    for i in 0..5 {
        let mut row = sample_annotation(&format!("ann_{i}"), repo_id, "a.rs");
        row.created_at = 1_000 + i;
        store.insert_annotation(&row).unwrap();
    }
    // Ask for 3 (a caller-configured cap of 2 plus one, per this
    // method's own doc) — exactly 3 must come back so the route can
    // tell `rows.len() > 2` and report `truncated: true`.
    let rows = store.list_open_annotations(repo_id, None, None, 3).unwrap();
    assert_eq!(rows.len(), 3);
}

// --- S2-A: unified-inbox annotations lane --------------------------

#[test]
fn list_open_working_tree_annotations_filters_review_scoped_and_intent() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();

    // A working-tree "question" — must be included.
    let mut question = sample_annotation("ann_q", repo_id, "a.rs");
    question.intent = "question".to_string();
    question.created_at = 100;
    question.updated_at = 100;
    // A working-tree "flag-for-agent" — must be included.
    let mut flag = sample_annotation("ann_flag", repo_id, "b.rs");
    flag.intent = "flag-for-agent".to_string();
    flag.created_at = 200;
    flag.updated_at = 200;
    // A working-tree "note" — wrong intent, must be excluded.
    let mut note = sample_annotation("ann_note", repo_id, "c.rs");
    note.intent = "note".to_string();
    note.created_at = 300;
    note.updated_at = 300;
    // A REVIEW-scoped "question" — must be excluded (review_inbox's
    // own lane already counts it; this lane must never double it).
    let mut review_scoped = sample_annotation("ann_review", repo_id, "d.rs");
    review_scoped.intent = "question".to_string();
    review_scoped.review_id = Some(1);
    review_scoped.created_at = 400;
    review_scoped.updated_at = 400;
    // A RESOLVED working-tree "question" — must be excluded.
    let mut resolved = sample_annotation("ann_resolved", repo_id, "e.rs");
    resolved.intent = "question".to_string();
    resolved.resolved = true;
    resolved.created_at = 500;
    resolved.updated_at = 500;
    for row in [&question, &flag, &note, &review_scoped, &resolved] {
        store.insert_annotation(row).unwrap();
    }
    // A reply on `question` — must never appear as its own row, but
    // must count toward `question`'s reply_count.
    let reply = sample_reply("ann_q_reply", &question, "a reply");
    store.insert_annotation(&reply).unwrap();

    let rows = store
        .list_open_working_tree_annotations(repo_id, &["question", "flag-for-agent"], 500)
        .unwrap();
    assert_eq!(
        rows.iter().map(|(r, _)| r.id.as_str()).collect::<Vec<_>>(),
        vec!["ann_flag", "ann_q"],
        "newest updated_at first; note/review-scoped/resolved excluded"
    );
    let (q_row, q_replies) = rows.iter().find(|(r, _)| r.id == "ann_q").unwrap();
    assert_eq!(q_row.intent, "question");
    assert_eq!(*q_replies, 1, "one direct reply counted");
}

#[test]
fn list_open_working_tree_annotations_empty_intents_short_circuits() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let row = sample_annotation("ann_1", repo_id, "a.rs");
    store.insert_annotation(&row).unwrap();

    let rows = store
        .list_open_working_tree_annotations(repo_id, &[], 500)
        .unwrap();
    assert!(rows.is_empty());
}

#[test]
fn list_open_working_tree_annotations_respects_the_limit_for_truncation_detection() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    for i in 0..5 {
        let mut row = sample_annotation(&format!("ann_{i}"), repo_id, "a.rs");
        row.intent = "question".to_string();
        row.created_at = 1_000 + i;
        row.updated_at = 1_000 + i;
        store.insert_annotation(&row).unwrap();
    }
    let rows = store
        .list_open_working_tree_annotations(repo_id, &["question"], 3)
        .unwrap();
    assert_eq!(rows.len(), 3);
}

/// The migration's byte-for-byte pin: a row shaped exactly like a
/// pre-D-server (V0006/W4.6) INSERT — omitting every new column — reads
/// back with `anchor_kind: "line"`, `intent: "note"`,
/// `anchor2`/`parent_id` both `None` (the rebuilt table's own column
/// DEFAULTs), and resolves identically to how it did before this
/// migration existed. Inserting directly (rather than faking a
/// partial-migration refinery history) is this crate's own established
/// precedent — see `pre_migration_shaped_symbol_rows_read_back_with_doc_none`
/// above for the identical technique applied to V0007.
#[test]
fn legacy_v1_shaped_annotation_rows_read_back_as_line_note_and_resolve_identically() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let anchor = crate::annotations::anchor_for_line(2, "fn b() {}");
    let anchor_json = serde_json::to_string(&anchor).unwrap();
    store
        .lock()
        .execute(
            "INSERT INTO annotations
                (id, repo_id, path, anchor, body, author, created_at, updated_at, resolved)
             VALUES ('ann_legacy', ?1, 'src/lib.rs', ?2, 'a v1 comment', 'you', 1000, 1000, 0)",
            params![repo_id, anchor_json],
        )
        .unwrap();

    let row = store.get_annotation("ann_legacy").unwrap().unwrap();
    assert_eq!(row.anchor_kind, "line");
    assert_eq!(row.intent, "note");
    assert_eq!(row.anchor2, None);
    assert_eq!(row.parent_id, None);
    assert_eq!(row.review_id, None);
    assert_eq!(row.ps_number, None);
    assert_eq!(row.side, None);
    assert_eq!(row.anchor.as_deref(), Some(anchor_json.as_str()));

    // Byte-for-byte resolution pin: `crate::annotations::resolve` is
    // completely unchanged by D-server for a `line`-kind anchor.
    let content = "fn a() {}\nfn b() {}\nfn c() {}";
    let resolved = crate::annotations::resolve(content, &anchor);
    assert_eq!(resolved.line, 2);
    assert!(!resolved.stale);

    // Still findable via the ordinary per-path list, alongside a
    // freshly-created D-server row in the SAME file.
    let listed = store.list_annotations(repo_id, "src/lib.rs").unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "ann_legacy");
}

/// V0023 on a fresh db: refinery runs the whole chain, so the new
/// annotation columns and `annotation_suggestions` are reachable.
#[test]
fn v0023_migration_applies_cleanly_on_a_fresh_db() {
    let (_tmp, store) = open_temp();
    let names: Vec<String> = store
        .lock()
        .prepare("PRAGMA table_info(annotations)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    for col in ["review_id", "ps_number", "side"] {
        assert!(
            names.iter().any(|n| n == col),
            "annotations missing {col}: {names:?}"
        );
    }
    let review_cols: Vec<String> = store
        .lock()
        .prepare("PRAGMA table_info(reviews)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    for col in ["verdict", "verdict_note", "verdict_at", "verdict_ps"] {
        assert!(
            review_cols.iter().any(|n| n == col),
            "reviews missing {col}: {review_cols:?}"
        );
    }
    let n: i64 = store
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name = 'annotation_suggestions'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

fn insert_suggestion(store: &Store, annotation_id: &str) {
    store
        .lock()
        .execute(
            "INSERT INTO annotation_suggestions
                (annotation_id, replacement, original, base_blob_sha,
                 applied, created_at, updated_at)
             VALUES (?1, 'new', 'old', 'deadbeef', 0, 1, 1)",
            params![annotation_id],
        )
        .unwrap();
}

#[test]
fn delete_review_cascades_annotations_and_suggestions_in_one_tx() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let review_id = store
        .create_review("r", Some("feat"), "main", "feature", None, 1_000)
        .unwrap();
    store
        .insert_patchset(review_id, 1, "aaa", "bbb", 1_000)
        .unwrap();
    let mut parent = sample_annotation("ann_rev", repo_id, "a.rs");
    parent.review_id = Some(review_id);
    parent.ps_number = Some(1);
    parent.side = Some("new".into());
    store.insert_annotation(&parent).unwrap();
    let reply = sample_reply("ann_rev_reply", &parent, "ok");
    store.insert_annotation(&reply).unwrap();
    insert_suggestion(&store, "ann_rev");
    insert_suggestion(&store, "ann_rev_reply");

    // An unrelated plain annotation must survive.
    store
        .insert_annotation(&sample_annotation("ann_plain", repo_id, "b.rs"))
        .unwrap();

    assert!(store.delete_review(review_id).unwrap());
    assert!(store.get_review(review_id).unwrap().is_none());
    assert!(store.list_patchsets(review_id).unwrap().is_empty());
    assert!(store.get_annotation("ann_rev").unwrap().is_none());
    assert!(store.get_annotation("ann_rev_reply").unwrap().is_none());
    assert!(store
        .get_annotation_suggestion("ann_rev")
        .unwrap()
        .is_none());
    assert!(store
        .get_annotation_suggestion("ann_rev_reply")
        .unwrap()
        .is_none());
    assert!(store.get_annotation("ann_plain").unwrap().is_some());
}

#[test]
fn delete_annotation_cascades_reply_suggestions() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let parent = sample_annotation("ann_parent", repo_id, "a.rs");
    store.insert_annotation(&parent).unwrap();
    let reply = sample_reply("ann_reply", &parent, "a reply");
    store.insert_annotation(&reply).unwrap();
    insert_suggestion(&store, "ann_parent");
    insert_suggestion(&store, "ann_reply");

    assert!(store.delete_annotation("ann_parent").unwrap());
    assert!(store.get_annotation("ann_parent").unwrap().is_none());
    assert!(store.get_annotation("ann_reply").unwrap().is_none());
    assert!(store
        .get_annotation_suggestion("ann_parent")
        .unwrap()
        .is_none());
    assert!(store
        .get_annotation_suggestion("ann_reply")
        .unwrap()
        .is_none());
}

#[test]
fn set_review_verdict_is_a_noop_when_state_and_note_match() {
    let (_tmp, store) = open_temp();
    let id = store
        .create_review("r", None, "main", "feature", None, 1_000)
        .unwrap();
    assert_eq!(
        store
            .set_review_verdict(id, "approve", None, 2_000, 1)
            .unwrap(),
        Some(true)
    );
    let first = store.get_review(id).unwrap().unwrap();
    assert_eq!(first.verdict.as_deref(), Some("approve"));
    assert_eq!(first.verdict_at, Some(2_000));
    assert_eq!(first.verdict_ps, Some(1));

    assert_eq!(
        store
            .set_review_verdict(id, "approve", None, 3_000, 2)
            .unwrap(),
        Some(false)
    );
    let again = store.get_review(id).unwrap().unwrap();
    assert_eq!(again.verdict_at, Some(2_000), "no-op must not restamp at");
    assert_eq!(again.verdict_ps, Some(1), "no-op must not restamp ps");

    assert_eq!(
        store
            .set_review_verdict(id, "approve", Some("lgtm"), 3_000, 1)
            .unwrap(),
        Some(true)
    );
    assert_eq!(store.clear_review_verdict(id).unwrap(), Some(true));
    assert_eq!(store.clear_review_verdict(id).unwrap(), Some(false));
    assert_eq!(store.clear_review_verdict(99).unwrap(), None);
    assert_eq!(
        store.set_review_verdict(99, "comment", None, 1, 1).unwrap(),
        None
    );
}

#[test]
fn upsert_annotation_suggestion_replaces_and_resets_applied() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .insert_annotation(&sample_annotation("ann_s", repo_id, "a.rs"))
        .unwrap();
    store
        .upsert_annotation_suggestion("ann_s", "new", "old", "deadbeef", 10)
        .unwrap();
    assert!(store
        .mark_annotation_suggestion_applied("ann_s", 20, "headsha")
        .unwrap());
    let marked = store.get_annotation_suggestion("ann_s").unwrap().unwrap();
    assert!(marked.applied);
    assert_eq!(marked.applied_at, Some(20));
    assert_eq!(marked.created_at, 10);

    store
        .upsert_annotation_suggestion("ann_s", "newer", "old", "cafe", 30)
        .unwrap();
    let reset = store.get_annotation_suggestion("ann_s").unwrap().unwrap();
    assert!(!reset.applied);
    assert_eq!(reset.applied_at, None);
    assert_eq!(reset.applied_head_sha, None);
    assert_eq!(reset.replacement, "newer");
    assert_eq!(reset.created_at, 10, "created_at survives re-PUT");
    assert_eq!(reset.updated_at, 30);
    assert!(store.delete_annotation_suggestion("ann_s").unwrap());
    assert!(!store.delete_annotation_suggestion("ann_s").unwrap());
}

#[test]
fn apply_annotation_ops_is_atomic_and_reports_noops() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let parent = sample_annotation("ann_p", repo_id, "a.rs");
    store.insert_annotation(&parent).unwrap();

    let report = store
        .apply_annotation_ops(
            &[PreparedAnnotationOp::SetResolved {
                id: "ann_p".into(),
                resolved: true,
            }],
            11,
        )
        .unwrap();
    assert!(report.changed);
    assert_eq!(report.applied, 1);

    let noop = store
        .apply_annotation_ops(
            &[PreparedAnnotationOp::SetResolved {
                id: "ann_p".into(),
                resolved: true,
            }],
            12,
        )
        .unwrap();
    assert!(!noop.changed);
    assert_eq!(noop.applied, 1);

    let before = store.list_annotations(repo_id, "a.rs").unwrap().len();
    let err = store
        .apply_annotation_ops(
            &[
                PreparedAnnotationOp::Insert {
                    row: Box::new(sample_annotation("ann_new", repo_id, "a.rs")),
                    suggestion: None,
                },
                PreparedAnnotationOp::Delete {
                    id: "does-not-exist".into(),
                },
            ],
            13,
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::NotFound(_)));
    assert_eq!(
        store.list_annotations(repo_id, "a.rs").unwrap().len(),
        before,
        "failing op must roll back the insert"
    );
    assert!(store.get_annotation("ann_new").unwrap().is_none());
}

#[test]
fn list_review_annotations_orders_and_filters_resolved_threads() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let review_id = store
        .create_review("r", None, "main", "feature", None, 1_000)
        .unwrap();
    let mut open = sample_annotation("ann_open", repo_id, "a.rs");
    open.review_id = Some(review_id);
    open.ps_number = Some(1);
    open.side = Some("new".into());
    open.created_at = 100;
    let mut resolved = sample_annotation("ann_done", repo_id, "a.rs");
    resolved.review_id = Some(review_id);
    resolved.ps_number = Some(1);
    resolved.side = Some("new".into());
    resolved.resolved = true;
    resolved.created_at = 200;
    store.insert_annotation(&open).unwrap();
    store.insert_annotation(&resolved).unwrap();
    let reply = sample_reply("ann_open_reply", &open, "ack");
    store.insert_annotation(&reply).unwrap();

    let open_only = store.list_review_annotations(review_id, false).unwrap();
    assert_eq!(
        open_only.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        vec!["ann_open", "ann_open_reply"]
    );

    let all = store.list_review_annotations(review_id, true).unwrap();
    assert_eq!(
        all.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        vec!["ann_open", "ann_open_reply", "ann_done"]
    );
}

/// Regression (V72-C2): the batch SELECT once omitted `set_id` — a
/// 16- vs. 17-column mismatch against `annotation_row_from`'s
/// positional `r.get(0..16)` reads (the module doc's own "every SELECT
/// spells out the SAME 17-column order" convention, broken by this one
/// query). That doesn't just drop the field: `r.get(16)` on a
/// 16-column row is a hard `rusqlite::Error::InvalidColumnIndex`, so
/// ANY non-empty result set errored out `review_inbox::compose_rows`
/// (and therefore both `GET /api/reviews/inbox` and `GET /api/inbox`)
/// end to end — caught only at the HTTP layer
/// (`review_inbox_timeline`/`unified_inbox` e2e tests), never at this
/// store layer, since no prior unit test called this fn with a
/// non-empty result. Pins both branches (`include_resolved` true/false)
/// against the SAME fixture `list_review_annotations_orders_and_
/// filters_resolved_threads` uses, plus a `set_id` round trip the
/// singular query already covered.
#[test]
fn list_review_annotations_batch_matches_the_singular_query_incl_set_id() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let review_id = store
        .create_review("r", None, "main", "feature", None, 1_000)
        .unwrap();
    let mut open = sample_annotation("ann_open", repo_id, "a.rs");
    open.review_id = Some(review_id);
    open.ps_number = Some(1);
    open.side = Some("new".into());
    open.set_id = Some("set_abc123456789".into());
    open.created_at = 100;
    let mut resolved = sample_annotation("ann_done", repo_id, "a.rs");
    resolved.review_id = Some(review_id);
    resolved.ps_number = Some(1);
    resolved.side = Some("new".into());
    resolved.resolved = true;
    resolved.created_at = 200;
    store.insert_annotation(&open).unwrap();
    store.insert_annotation(&resolved).unwrap();
    let reply = sample_reply("ann_open_reply", &open, "ack");
    store.insert_annotation(&reply).unwrap();

    let open_only = store
        .list_review_annotations_batch(&[review_id], false)
        .unwrap();
    assert_eq!(
        open_only
            .get(&review_id)
            .unwrap()
            .iter()
            .map(|r| r.id.as_str())
            .collect::<Vec<_>>(),
        vec!["ann_open", "ann_open_reply"]
    );

    let all = store
        .list_review_annotations_batch(&[review_id], true)
        .unwrap();
    let all_rows = all.get(&review_id).unwrap();
    assert_eq!(
        all_rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        vec!["ann_open", "ann_open_reply", "ann_done"]
    );
    let open_row = all_rows.iter().find(|r| r.id == "ann_open").unwrap();
    assert_eq!(open_row.set_id.as_deref(), Some("set_abc123456789"));
}

// --- reading sets (Phase E3) --------------------------------------------

fn whole_file_span(path: &str) -> NewReadingSetSpan {
    NewReadingSetSpan {
        path: path.to_string(),
        ..Default::default()
    }
}

fn ranged_span(path: &str, start: i64, end: i64, note: &str) -> NewReadingSetSpan {
    NewReadingSetSpan {
        path: path.to_string(),
        line_start: Some(start),
        line_end: Some(end),
        git_ref: Some("deadbeef".to_string()),
        note: Some(note.to_string()),
    }
}

#[test]
fn create_list_get_reading_set_round_trip() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let spans = vec![
        whole_file_span("src/lib.rs"),
        ranged_span("src/main.rs", 10, 20, "the entrypoint"),
    ];
    store
        .create_reading_set(
            "set_1",
            repo_id,
            "the ingest path",
            Some("how a request flows in"),
            &spans,
            1_000,
        )
        .unwrap();

    let listed = store.list_reading_sets(repo_id, None).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0.name, "the ingest path");
    assert_eq!(listed[0].1, 2, "span_count");
    assert_eq!(
        listed[0].2, 0,
        "note_count — no annotations scoped to this set"
    );

    let row = store.get_reading_set("set_1").unwrap().unwrap();
    assert_eq!(row.name, "the ingest path");
    assert_eq!(row.description.as_deref(), Some("how a request flows in"));
    assert_eq!(row.created_at, 1_000);
    assert_eq!(row.updated_at, 1_000);
    // V70-A10 — a plain `create_reading_set` row defaults to kind "set"
    // with every workspace-only column `None`.
    assert_eq!(row.kind, "set");
    assert_eq!(row.desk_json, None);
    assert_eq!(row.ref_label, None);
    assert_eq!(row.description_md, None);

    let got_spans = store.reading_set_spans("set_1").unwrap();
    assert_eq!(got_spans.len(), 2);
    assert_eq!(got_spans[0].ordinal, 0);
    assert_eq!(got_spans[0].path, "src/lib.rs");
    assert_eq!(got_spans[0].line_start, None);
    assert_eq!(got_spans[1].ordinal, 1);
    assert_eq!(got_spans[1].path, "src/main.rs");
    assert_eq!(got_spans[1].line_start, Some(10));
    assert_eq!(got_spans[1].line_end, Some(20));
    assert_eq!(got_spans[1].git_ref.as_deref(), Some("deadbeef"));
    assert_eq!(got_spans[1].note.as_deref(), Some("the entrypoint"));

    assert!(store.get_reading_set("set_nope").unwrap().is_none());

    // DCB-W3.C — a set created via the plain `create_reading_set`
    // wrapper carries all four provenance columns as `None` (the
    // wrapper passes four `None`s through to
    // `create_reading_set_with_provenance`) — proves the delegation in
    // §3.2 didn't change this existing call site's behavior.
    assert_eq!(row.source_kb, None);
    assert_eq!(row.source_doc_id, None);
    assert_eq!(row.source_doc_path, None);
    assert_eq!(row.source_doc_hash, None);
}

/// DCB-W3.C — the four `source_*` columns round-trip through
/// `create_reading_set_with_provenance` → `get_reading_set` →
/// `list_reading_sets`, and a `None` stays `None` (never coerced to an
/// empty string).
#[test]
fn create_reading_set_with_provenance_round_trips_the_four_columns() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .create_reading_set_with_provenance(
            "set_from_doc",
            repo_id,
            "materialized",
            None,
            &[whole_file_span("src/lib.rs")],
            1_000,
            Some("platform"),
            Some("9f8b7182d433"),
            Some("docs/checkout.html"),
            Some("deadbeef"),
            "set",
            None,
            None,
            None,
        )
        .unwrap();

    let row = store.get_reading_set("set_from_doc").unwrap().unwrap();
    assert_eq!(row.source_kb.as_deref(), Some("platform"));
    assert_eq!(row.source_doc_id.as_deref(), Some("9f8b7182d433"));
    assert_eq!(row.source_doc_path.as_deref(), Some("docs/checkout.html"));
    assert_eq!(row.source_doc_hash.as_deref(), Some("deadbeef"));

    let listed = store.list_reading_sets(repo_id, None).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0.source_kb.as_deref(), Some("platform"));
    assert_eq!(listed[0].0.source_doc_hash.as_deref(), Some("deadbeef"));

    // A sibling set with no provenance at all (`create_reading_set`,
    // same repo) stays `None` — the two rows don't cross-contaminate.
    store
        .create_reading_set("set_plain", repo_id, "plain", None, &[], 1_000)
        .unwrap();
    let plain = store.get_reading_set("set_plain").unwrap().unwrap();
    assert_eq!(plain.source_kb, None);
}

#[test]
fn create_reading_set_rejects_a_duplicate_name_in_the_same_repo() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .create_reading_set("set_1", repo_id, "dup", None, &[], 1_000)
        .unwrap();
    let err = store
        .create_reading_set("set_2", repo_id, "dup", None, &[], 1_000)
        .unwrap_err();
    assert!(matches!(err, StoreError::NameConflict(n) if n == "dup"));

    // A different repo may reuse the same name freely — the UNIQUE
    // constraint is scoped to (repo_id, name), not name alone.
    let other_repo = store.upsert_repo("r2", "/tmp/r2").unwrap();
    store
        .create_reading_set("set_3", other_repo, "dup", None, &[], 1_000)
        .unwrap();
}

#[test]
fn update_reading_set_meta_coalesces_and_rejects_a_colliding_rename() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .create_reading_set("set_1", repo_id, "one", Some("d1"), &[], 1_000)
        .unwrap();
    store
        .create_reading_set("set_2", repo_id, "two", None, &[], 1_000)
        .unwrap();

    // Description-only update leaves the name untouched.
    let existed = store
        .update_reading_set_meta(
            "set_1",
            None,
            Some("new desc"),
            None,
            None,
            None,
            None,
            2_000,
        )
        .unwrap();
    assert!(existed);
    let row = store.get_reading_set("set_1").unwrap().unwrap();
    assert_eq!(row.name, "one");
    assert_eq!(row.description.as_deref(), Some("new desc"));
    assert_eq!(row.updated_at, 2_000);

    // Renaming to an unknown id is a no-op `false`, not an error.
    assert!(!store
        .update_reading_set_meta("set_nope", Some("x"), None, None, None, None, None, 2_000)
        .unwrap());

    // Renaming "two" to "one" collides with set_1 in the same repo.
    let err = store
        .update_reading_set_meta("set_2", Some("one"), None, None, None, None, None, 2_000)
        .unwrap_err();
    assert!(matches!(err, StoreError::NameConflict(n) if n == "one"));

    // V70-A10 — kind/desk_json/ref/description_md are COALESCE-updated
    // exactly like name/description, and independently of them.
    let existed = store
        .update_reading_set_meta(
            "set_1",
            None,
            None,
            Some("workspace"),
            Some("{\"v\":1}"),
            Some("feature/x"),
            Some("# why"),
            3_000,
        )
        .unwrap();
    assert!(existed);
    let row = store.get_reading_set("set_1").unwrap().unwrap();
    assert_eq!(row.kind, "workspace");
    assert_eq!(row.desk_json.as_deref(), Some("{\"v\":1}"));
    assert_eq!(row.ref_label.as_deref(), Some("feature/x"));
    assert_eq!(row.description_md.as_deref(), Some("# why"));
    // The pre-existing fields untouched by this second update.
    assert_eq!(row.description.as_deref(), Some("new desc"));
}

#[test]
fn replace_reading_set_spans_rewrites_ordinals_contiguously_in_one_tx() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .create_reading_set(
            "set_1",
            repo_id,
            "s",
            None,
            &[whole_file_span("a.rs"), whole_file_span("b.rs")],
            1_000,
        )
        .unwrap();

    let replaced = store
        .replace_reading_set_spans(
            "set_1",
            &[whole_file_span("c.rs"), ranged_span("d.rs", 1, 2, "n")],
            2_000,
        )
        .unwrap();
    assert!(replaced);

    let spans = store.reading_set_spans("set_1").unwrap();
    assert_eq!(spans.len(), 2, "old spans fully replaced, not appended to");
    assert_eq!(
        spans.iter().map(|s| s.ordinal).collect::<Vec<_>>(),
        vec![0, 1],
        "ordinals rewritten contiguously from 0"
    );
    assert_eq!(spans[0].path, "c.rs");
    assert_eq!(spans[1].path, "d.rs");
    assert_eq!(
        store.get_reading_set("set_1").unwrap().unwrap().updated_at,
        2_000
    );

    // Unknown set id: false, no partial write.
    assert!(!store
        .replace_reading_set_spans("set_nope", &[whole_file_span("x.rs")], 3_000)
        .unwrap());
}

#[test]
fn append_reading_set_span_assigns_the_next_ordinal() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .create_reading_set("set_1", repo_id, "s", None, &[], 1_000)
        .unwrap();

    // First append into an EMPTY set gets ordinal 0.
    let ord = store
        .append_reading_set_span("set_1", &whole_file_span("a.rs"), 2_000)
        .unwrap();
    assert_eq!(ord, Some(0));

    let ord = store
        .append_reading_set_span("set_1", &ranged_span("b.rs", 5, 9, "n"), 3_000)
        .unwrap();
    assert_eq!(ord, Some(1));

    let spans = store.reading_set_spans("set_1").unwrap();
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0].path, "a.rs");
    assert_eq!(spans[1].path, "b.rs");
    assert_eq!(
        store.get_reading_set("set_1").unwrap().unwrap().updated_at,
        3_000
    );

    assert_eq!(
        store
            .append_reading_set_span("set_nope", &whole_file_span("x.rs"), 4_000)
            .unwrap(),
        None
    );
}

#[test]
fn delete_reading_set_cascades_to_spans_in_one_tx() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .create_reading_set(
            "set_1",
            repo_id,
            "s",
            None,
            &[whole_file_span("a.rs")],
            1_000,
        )
        .unwrap();

    assert!(store.delete_reading_set("set_1").unwrap());
    assert!(store.get_reading_set("set_1").unwrap().is_none());
    assert!(store.reading_set_spans("set_1").unwrap().is_empty());

    // Deleting an already-gone id is a clean `false`, not an error.
    assert!(!store.delete_reading_set("set_1").unwrap());
}

/// V70-A10 — a workspace ('kind: "workspace"') round-trips its
/// `desk_json`/`ref`/`description_md` sidecar, `list_reading_sets`'s
/// `kind` filter defaults to `'set'` (so a plain `None` filter never
/// sees a workspace row), an explicit `Some("workspace")` finds it, and
/// `note_count` reflects `annotations.set_id` scoped to it.
#[test]
fn workspace_kind_sidecar_and_note_count_round_trip() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .create_reading_set_with_provenance(
            "set_ws1",
            repo_id,
            "feature x",
            None,
            &[whole_file_span("a.rs")],
            1_000,
            None,
            None,
            None,
            None,
            "workspace",
            Some("{\"v\":1,\"preset\":\"read\"}"),
            Some("feature/x"),
            Some("# why this exists"),
        )
        .unwrap();
    // A plain 'set' sibling, same repo.
    store
        .create_reading_set("set_plain", repo_id, "plain", None, &[], 1_000)
        .unwrap();

    let row = store.get_reading_set("set_ws1").unwrap().unwrap();
    assert_eq!(row.kind, "workspace");
    assert_eq!(
        row.desk_json.as_deref(),
        Some("{\"v\":1,\"preset\":\"read\"}")
    );
    assert_eq!(row.ref_label.as_deref(), Some("feature/x"));
    assert_eq!(row.description_md.as_deref(), Some("# why this exists"));

    // Default (no `kind` filter) sees only the plain 'set' row — the
    // pre-A10 listing's behavior is byte-identical.
    let default_listed = store.list_reading_sets(repo_id, None).unwrap();
    assert_eq!(default_listed.len(), 1);
    assert_eq!(default_listed[0].0.id, "set_plain");

    // Explicit `kind = "workspace"` finds ONLY the workspace.
    let ws_listed = store.list_reading_sets(repo_id, Some("workspace")).unwrap();
    assert_eq!(ws_listed.len(), 1);
    assert_eq!(ws_listed[0].0.id, "set_ws1");
    assert_eq!(ws_listed[0].2, 0, "note_count starts at zero");

    // Two annotations scoped to the workspace (a general note + a
    // code-anchored one) bump note_count; a plain annotation on the
    // SAME repo/path with no `set_id` does not.
    let mut general = sample_annotation("ann_general", repo_id, "");
    general.anchor = Some(String::new());
    general.anchor_kind = "set".to_string();
    general.set_id = Some("set_ws1".to_string());
    store.insert_annotation(&general).unwrap();

    let mut anchored = sample_annotation("ann_anchored", repo_id, "a.rs");
    anchored.set_id = Some("set_ws1".to_string());
    store.insert_annotation(&anchored).unwrap();

    let unrelated = sample_annotation("ann_unrelated", repo_id, "a.rs");
    store.insert_annotation(&unrelated).unwrap();

    let ws_listed = store.list_reading_sets(repo_id, Some("workspace")).unwrap();
    assert_eq!(ws_listed[0].2, 2, "note_count counts both workspace notes");

    let by_set = store.list_annotations_by_set("set_ws1").unwrap();
    assert_eq!(by_set.len(), 2);
    assert!(by_set
        .iter()
        .all(|a| a.set_id.as_deref() == Some("set_ws1")));
    assert!(by_set.iter().any(|a| a.id == "ann_general"));
    assert!(by_set.iter().any(|a| a.id == "ann_anchored"));

    // Deleting the workspace cascades to its notes (and NOT the
    // unrelated plain annotation on the same path).
    assert!(store.delete_reading_set("set_ws1").unwrap());
    assert!(store.list_annotations_by_set("set_ws1").unwrap().is_empty());
    assert!(store.get_annotation("ann_general").unwrap().is_none());
    assert!(store.get_annotation("ann_anchored").unwrap().is_none());
    assert!(store.get_annotation("ann_unrelated").unwrap().is_some());
}

// --- doc-lens (DCB W1.C) ---------------------------------------------

fn pin(kb: &str, doc: &str, repo: &str) -> DocLensPin {
    DocLensPin {
        kb: kb.to_string(),
        doc_id: doc.to_string(),
        repo: repo.to_string(),
        repo_root: format!("/tmp/{repo}"),
        doc_hash: Some("h1".to_string()),
        pinned_at: 1_754_500_000,
    }
}

#[test]
fn symbols_named_many_returns_only_the_requested_names_in_order() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .upsert_file(repo_id, "b/two.rb", "hashB", "ruby", 10)
        .unwrap();
    store
        .upsert_file(repo_id, "a/one.rb", "hashA", "ruby", 10)
        .unwrap();
    store
        .replace_symbols(
            "hashA",
            "ruby@1",
            &[sample_symbol(0, "wanted"), sample_symbol(1, "ignored")],
        )
        .unwrap();
    store
        .replace_symbols("hashB", "ruby@1", &[sample_symbol(0, "wanted")])
        .unwrap();

    let got = store
        .symbols_named_many(repo_id, &["wanted".to_string(), "absent".to_string()])
        .unwrap();
    assert_eq!(got.len(), 2, "only the requested names");
    assert!(got.iter().all(|(_, s)| s.name == "wanted"));
    // Ordered `f.path, s.line_start, s.ordinal` — the same determinism
    // contract `symbols_named_in_repo` carries.
    assert_eq!(got[0].0, "a/one.rb");
    assert_eq!(got[1].0, "b/two.rb");

    // A repo that has none of them answers empty, not everything.
    let other = store.upsert_repo("r2", "/tmp/r2").unwrap();
    assert!(store
        .symbols_named_many(other, &["wanted".to_string()])
        .unwrap()
        .is_empty());
}

#[test]
fn symbols_named_many_with_an_empty_name_list_touches_no_sql() {
    let (_tmp, store) = open_temp();
    // A repo_id that does not exist: an implementation that still built
    // and ran a `WHERE name IN ()` statement would error rather than
    // return the documented empty Vec.
    assert!(store.symbols_named_many(-1, &[]).unwrap().is_empty());
}

#[test]
fn doc_lens_pin_round_trip_upsert_get_list_delete() {
    let (_tmp, store) = open_temp();
    assert!(store.get_doc_lens_pin("platform", "d1").unwrap().is_none());

    store
        .put_doc_lens_pin(&pin("platform", "d1", "alpha"))
        .unwrap();
    let got = store.get_doc_lens_pin("platform", "d1").unwrap().unwrap();
    assert_eq!(got.repo, "alpha");
    assert_eq!(got.repo_root, "/tmp/alpha");
    assert_eq!(got.doc_hash.as_deref(), Some("h1"));

    // Last write wins on the (kb, doc_id) PK.
    store
        .put_doc_lens_pin(&pin("platform", "d1", "beta"))
        .unwrap();
    assert_eq!(
        store
            .get_doc_lens_pin("platform", "d1")
            .unwrap()
            .unwrap()
            .repo,
        "beta"
    );

    store
        .put_doc_lens_pin(&pin("platform", "d0", "alpha"))
        .unwrap();
    store
        .put_doc_lens_pin(&pin("research", "d9", "alpha"))
        .unwrap();
    let all = store.list_doc_lens_pins(None).unwrap();
    assert_eq!(
        all.iter()
            .map(|p| (p.kb.as_str(), p.doc_id.as_str()))
            .collect::<Vec<_>>(),
        vec![("platform", "d0"), ("platform", "d1"), ("research", "d9")]
    );
    assert_eq!(store.list_doc_lens_pins(Some("research")).unwrap().len(), 1);

    assert!(store.delete_doc_lens_pin("platform", "d1").unwrap());
    // Idempotent: the second delete removed nothing, but is not an error.
    assert!(!store.delete_doc_lens_pin("platform", "d1").unwrap());
}

#[test]
fn rekey_doc_lens_pin_moves_a_pin_to_a_new_doc_id() {
    let (_tmp, store) = open_temp();
    store
        .put_doc_lens_pin(&pin("platform", "old", "alpha"))
        .unwrap();
    assert!(store.rekey_doc_lens_pin("platform", "old", "new").unwrap());
    assert!(store.get_doc_lens_pin("platform", "old").unwrap().is_none());
    assert_eq!(
        store
            .get_doc_lens_pin("platform", "new")
            .unwrap()
            .unwrap()
            .repo,
        "alpha"
    );
    // No pin to move ⇒ a clean `false`, never an error.
    assert!(!store
        .rekey_doc_lens_pin("platform", "nope", "new2")
        .unwrap());

    // A pre-existing pin on the DESTINATION id loses to the row actually
    // being re-keyed rather than aborting the migration on the PK.
    store
        .put_doc_lens_pin(&pin("platform", "src", "beta"))
        .unwrap();
    assert!(store.rekey_doc_lens_pin("platform", "src", "new").unwrap());
    assert_eq!(
        store
            .get_doc_lens_pin("platform", "new")
            .unwrap()
            .unwrap()
            .repo,
        "beta"
    );
}

#[test]
fn pin_writes_do_not_bump_the_store_generation() {
    // `doc_lens_pins` is invisible to `FileIndex`/`SymbolIndex`'s
    // generation-keyed caches; bumping would throw away every repo's
    // cached path/symbol snapshot on a pin click.
    let (_tmp, store) = open_temp();
    let before = store.generation();
    store
        .put_doc_lens_pin(&pin("platform", "d1", "alpha"))
        .unwrap();
    store.rekey_doc_lens_pin("platform", "d1", "d2").unwrap();
    store.delete_doc_lens_pin("platform", "d2").unwrap();
    assert_eq!(store.generation(), before);

    // Control: a files write DOES bump, so this test can't pass vacuously.
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store.upsert_file(repo_id, "a.rb", "h", "ruby", 1).unwrap();
    assert!(store.generation() > before);
}

// ── PRR-R1: PR binding + findings tests ────────────────────────────

/// V0024's byte-for-byte pin (mirrors `legacy_v1_shaped_annotation_
/// rows_read_back_as_line_note_and_resolve_identically` above): a
/// review created via the EXISTING `create_review` — which never
/// mentions any V0024 column — reads back with every new PR-binding /
/// report / verdict-publish field `None`, exactly as it did before this
/// migration existed.
#[test]
fn legacy_review_row_reads_back_with_pr_binding_report_and_verdict_publish_columns_null() {
    let (_tmp, store) = open_temp();
    let id = store
        .create_review("r", Some("feat"), "main", "feature", None, 1_000)
        .unwrap();

    let binding = store.get_review_pr_binding(id).unwrap().unwrap();
    assert_eq!(binding, ReviewPrBinding::default());

    let report = store.get_review_report(id).unwrap().unwrap();
    assert_eq!(report, ReviewReport::default());

    let (published_at, published_url) = store.get_review_verdict_published(id).unwrap().unwrap();
    assert_eq!(published_at, None);
    assert_eq!(published_url, None);

    // A missing review id is a clean `None`, not an error, at every one
    // of these getters.
    assert!(store.get_review_pr_binding(999).unwrap().is_none());
    assert!(store.get_review_report(999).unwrap().is_none());
    assert!(store.get_review_verdict_published(999).unwrap().is_none());
}

#[test]
fn set_review_pr_binding_round_trips_and_is_independent_of_the_artifact_hint() {
    let (_tmp, store) = open_temp();
    let id = store
        .create_review("r", None, "main", "feature", None, 1_000)
        .unwrap();

    assert!(store
        .set_review_pr_binding(
            id,
            42,
            "acme/widgets",
            Some("deadbeef"),
            Some(r#"{"title":"x"}"#),
            Some(1_100)
        )
        .unwrap());
    let binding = store.get_review_pr_binding(id).unwrap().unwrap();
    assert_eq!(binding.pr_number, Some(42));
    assert_eq!(binding.pr_repo_slug.as_deref(), Some("acme/widgets"));
    assert_eq!(binding.pr_head_sha.as_deref(), Some("deadbeef"));
    assert_eq!(binding.pr_meta_json.as_deref(), Some(r#"{"title":"x"}"#));
    assert_eq!(binding.pr_meta_fetched_at, Some(1_100));
    assert_eq!(
        binding.artifact_hint_kb, None,
        "binding must not touch the hint"
    );

    // Best-effort GitHub enrichment failing at bind time — pr_meta_*
    // stay None, the git-fetch-backed binding itself still lands.
    let id2 = store
        .create_review("r", None, "main", "feature2", None, 1_000)
        .unwrap();
    store
        .set_review_pr_binding(id2, 43, "acme/widgets2", None, None, None)
        .unwrap();
    let binding2 = store.get_review_pr_binding(id2).unwrap().unwrap();
    assert_eq!(binding2.pr_number, Some(43));
    assert_eq!(binding2.pr_head_sha, None);

    // Refreshing metadata later (a re-fetch) leaves pr_number/slug alone.
    store
        .set_review_pr_meta(id2, Some("cafef00d"), Some(r#"{"title":"y"}"#), 1_200)
        .unwrap();
    let refreshed = store.get_review_pr_binding(id2).unwrap().unwrap();
    assert_eq!(refreshed.pr_number, Some(43));
    assert_eq!(refreshed.pr_repo_slug.as_deref(), Some("acme/widgets2"));
    assert_eq!(refreshed.pr_head_sha.as_deref(), Some("cafef00d"));

    // The artifact hint sets/clears independently of the PR binding.
    assert!(store
        .set_review_artifact_hint(id, Some("platform"), Some("doc123"))
        .unwrap());
    let with_hint = store.get_review_pr_binding(id).unwrap().unwrap();
    assert_eq!(with_hint.artifact_hint_kb.as_deref(), Some("platform"));
    assert_eq!(
        with_hint.pr_number,
        Some(42),
        "hint must not touch the binding"
    );
    assert!(store.set_review_artifact_hint(id, None, None).unwrap());
    assert_eq!(
        store
            .get_review_pr_binding(id)
            .unwrap()
            .unwrap()
            .artifact_hint_kb,
        None
    );

    assert!(!store
        .set_review_pr_binding(999, 1, "a/b", None, None, None)
        .unwrap());
    assert!(!store
        .set_review_artifact_hint(999, Some("k"), Some("d"))
        .unwrap());
}

#[test]
fn pr_binding_unique_is_open_only_and_lookup_prefers_open() {
    let (_tmp, store) = open_temp();
    let a = store
        .create_review("r", None, "main", "feature", None, 1_000)
        .unwrap();
    store
        .set_review_pr_binding(a, 7, "acme-app/app", None, None, None)
        .unwrap();
    store.update_review(a, None, Some("closed"), 1_100).unwrap();

    let b = store
        .create_review("r", None, "main", "feature2", None, 1_200)
        .unwrap();
    store
        .set_review_pr_binding(b, 7, "acme-app/app", None, None, None)
        .expect("a closed row must not block a new OPEN binding (V0042)");

    let preferred = store.get_review_by_pr_binding("r", 7).unwrap().unwrap();
    assert_eq!(preferred.id, b, "open review wins over a closed sibling");
    assert_eq!(preferred.state, "open");

    let c = store
        .create_review("r", None, "main", "feature3", None, 1_300)
        .unwrap();
    let err = store
        .set_review_pr_binding(c, 7, "acme-app/app", None, None, None)
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("UNIQUE") || msg.contains("unique") || msg.contains("constraint"),
        "two OPEN reviews must not share a PR: {msg}"
    );

    assert_eq!(store.count_reviews_by_pr_binding("r", 7).unwrap(), 2);
    let bound = store.list_pr_bound_reviews("r").unwrap();
    assert_eq!(bound[0].0, b, "open first");
    assert_eq!(bound[0].1, 7);
}

#[test]
fn set_review_report_replaces_wholesale_and_stamps_updated_at() {
    let (_tmp, store) = open_temp();
    let id = store
        .create_review("r", None, "main", "feature", None, 1_000)
        .unwrap();
    assert!(store
        .set_review_report(id, r#"{"summary":"a"}"#, 1_050)
        .unwrap());
    let report = store.get_review_report(id).unwrap().unwrap();
    assert_eq!(report.report_json.as_deref(), Some(r#"{"summary":"a"}"#));
    assert_eq!(report.report_updated_at, Some(1_050));

    // A later PUT replaces wholesale, not a merge.
    assert!(store
        .set_review_report(id, r#"{"summary":"b"}"#, 1_060)
        .unwrap());
    let report2 = store.get_review_report(id).unwrap().unwrap();
    assert_eq!(report2.report_json.as_deref(), Some(r#"{"summary":"b"}"#));
    assert_eq!(report2.report_updated_at, Some(1_060));

    assert!(!store.set_review_report(999, "{}", 1_000).unwrap());
}

#[test]
fn set_review_verdict_published_round_trips() {
    let (_tmp, store) = open_temp();
    let id = store
        .create_review("r", None, "main", "feature", None, 1_000)
        .unwrap();
    assert!(store
        .set_review_verdict_published(
            id,
            Some("https://github.com/a/b/pull/1#pullrequestreview-1"),
            1_070
        )
        .unwrap());
    let (at, url) = store.get_review_verdict_published(id).unwrap().unwrap();
    assert_eq!(at, Some(1_070));
    assert_eq!(
        url.as_deref(),
        Some("https://github.com/a/b/pull/1#pullrequestreview-1")
    );
}

// -- location-kind -> anchor derivation (design doc §1.4) ---------------

#[test]
fn derive_finding_anchor_single_builds_one_line_selection() {
    let derived = derive_finding_anchor("single", "app/models/order.rb", Some(&[14]), false, |n| {
        assert_eq!(n, 14);
        "  def total".to_string()
    })
    .unwrap();
    assert_eq!(derived.anchor_kind, crate::annotations::ANCHOR_KIND_LINE);
    assert_eq!(derived.anchor2, None);
    assert_eq!(derived.side.as_deref(), Some("new"));
    let anchor: kb_core::review::Anchor = serde_json::from_str(&derived.anchor).unwrap();
    match anchor {
        kb_core::review::Anchor::Selection {
            offset, snippet, ..
        } => {
            assert_eq!(offset, 14);
            assert_eq!(snippet, "def total");
        }
        other => panic!("expected Selection, got {other:?}"),
    }
}

#[test]
fn derive_finding_anchor_range_builds_two_selections_start_and_end() {
    let derived = derive_finding_anchor(
        "range",
        "app/models/order.rb",
        Some(&[13, 30]),
        false,
        |n| format!("line {n}"),
    )
    .unwrap();
    assert_eq!(derived.anchor_kind, crate::annotations::ANCHOR_KIND_RANGE);
    let start: kb_core::review::Anchor = serde_json::from_str(&derived.anchor).unwrap();
    let end: kb_core::review::Anchor =
        serde_json::from_str(derived.anchor2.as_deref().unwrap()).unwrap();
    match (start, end) {
        (
            kb_core::review::Anchor::Selection {
                offset: s,
                snippet: ss,
                ..
            },
            kb_core::review::Anchor::Selection {
                offset: e,
                snippet: es,
                ..
            },
        ) => {
            assert_eq!(s, 13);
            assert_eq!(ss, "line 13");
            assert_eq!(e, 30);
            assert_eq!(es, "line 30");
        }
        other => panic!("expected two Selections, got {other:?}"),
    }
}

#[test]
fn derive_finding_anchor_multi_anchors_only_the_first_line() {
    let mut calls = Vec::new();
    let derived = derive_finding_anchor(
        "multi",
        "app/models/order.rb",
        Some(&[13, 30, 33, 36]),
        false,
        |n| {
            calls.push(n);
            format!("line {n}")
        },
    )
    .unwrap();
    // Documented approximation: only the FIRST line is ever resolved
    // for text — the ladder never even calls `line_text` for 30/33/36.
    assert_eq!(calls, vec![13]);
    assert_eq!(derived.anchor_kind, crate::annotations::ANCHOR_KIND_LINE);
    assert_eq!(derived.anchor2, None);
    let anchor: kb_core::review::Anchor = serde_json::from_str(&derived.anchor).unwrap();
    match anchor {
        kb_core::review::Anchor::Selection { offset, .. } => assert_eq!(offset, 13),
        other => panic!("expected Selection, got {other:?}"),
    }
}

#[test]
fn derive_finding_anchor_whole_file_stores_the_bare_path_not_json() {
    let derived = derive_finding_anchor(
        "whole_file",
        "config/routes.rb",
        None,
        false,
        |_| unreachable!(),
    )
    .unwrap();
    assert_eq!(
        derived.anchor_kind,
        crate::annotations::ANCHOR_KIND_WHOLE_FILE
    );
    assert_eq!(derived.anchor, "config/routes.rb");
    assert_eq!(derived.anchor2, None);
    // Not JSON — a bare path, per the migration's own doc.
    assert!(serde_json::from_str::<kb_core::review::Anchor>(&derived.anchor).is_err());
}

#[test]
fn derive_finding_anchor_removed_forces_side_old_regardless_of_kind() {
    let single =
        derive_finding_anchor("single", "a.rb", Some(&[1]), true, |_| "x".to_string()).unwrap();
    assert_eq!(single.side.as_deref(), Some("old"));

    let whole_file =
        derive_finding_anchor("whole_file", "a.rb", None, true, |_| unreachable!()).unwrap();
    assert_eq!(whole_file.side.as_deref(), Some("old"));

    // Un-removed stays "new" — always an explicit string, never bare
    // `None` (matches `resolve_review_create_scope`'s convention).
    let not_removed =
        derive_finding_anchor("single", "a.rb", Some(&[1]), false, |_| "x".to_string()).unwrap();
    assert_eq!(not_removed.side.as_deref(), Some("new"));
}

#[test]
fn derive_finding_anchor_rejects_malformed_locations() {
    assert!(derive_finding_anchor("single", "a.rb", None, false, |_| String::new()).is_err());
    assert!(
        derive_finding_anchor("single", "a.rb", Some(&[1, 2]), false, |_| String::new()).is_err()
    );
    assert!(derive_finding_anchor("range", "a.rb", Some(&[1]), false, |_| String::new()).is_err());
    assert!(
        derive_finding_anchor("range", "a.rb", Some(&[1, 2, 3]), false, |_| String::new()).is_err()
    );
    assert!(derive_finding_anchor("multi", "a.rb", Some(&[]), false, |_| String::new()).is_err());
    assert!(derive_finding_anchor("multi", "a.rb", None, false, |_| String::new()).is_err());
    assert!(
        derive_finding_anchor("bogus-kind", "a.rb", Some(&[1]), false, |_| String::new()).is_err()
    );
}

// -- vocab validators (severity / disposition / location_kind) ----------

#[test]
fn severity_vocab_accepts_exactly_the_three_severities() {
    for s in ["blocker", "concern", "ok"] {
        assert!(is_valid_severity(s), "{s} should be valid");
    }
    for s in ["Blocker", "info", "", "nit", "praise"] {
        assert!(!is_valid_severity(s), "{s} should be invalid");
    }
}

#[test]
fn disposition_vocab_accepts_exactly_the_four_dispositions() {
    for d in ["agree", "dispute", "waive", "fix-later"] {
        assert!(is_valid_disposition(d), "{d} should be valid");
    }
    for d in ["Agree", "fixed", "", "wontfix"] {
        assert!(!is_valid_disposition(d), "{d} should be invalid");
    }
}

#[test]
fn location_kind_vocab_accepts_exactly_the_four_kinds() {
    for k in ["single", "range", "multi", "whole_file"] {
        assert!(is_valid_location_kind(k), "{k} should be valid");
    }
    for k in ["Single", "whole-file", "", "line"] {
        assert!(!is_valid_location_kind(k), "{k} should be invalid");
    }
}

/// PRR-R1 scope extension.
#[test]
fn finding_origin_vocab_accepts_exactly_the_two_origins() {
    for o in ["import", "manual"] {
        assert!(is_valid_finding_origin(o), "{o} should be valid");
    }
    for o in ["Import", "human", "", "agent"] {
        assert!(!is_valid_finding_origin(o), "{o} should be invalid");
    }
}

// -- findings CRUD + the §4.3 reconciliation matrix ----------------------

fn sample_imported_finding(slug: &str) -> ImportedFinding {
    ImportedFinding {
        slug: slug.to_string(),
        severity: SEVERITY_CONCERN.to_string(),
        category: "Concurrency".to_string(),
        location_kind: LOCATION_KIND_SINGLE.to_string(),
        location_path: "app/models/order.rb".to_string(),
        location_lines: Some(location_lines_json(&[88])),
        location_removed: false,
        title: "Duplicate order rows possible".to_string(),
        rationale: "Verified against db/schema.rb:41".to_string(),
        recommendation: Some("Add a unique index.".to_string()),
        evidence_lang: Some("ruby".to_string()),
        evidence_source: Some("def checkout!\nend".to_string()),
        anchor_kind: crate::annotations::ANCHOR_KIND_LINE.to_string(),
        anchor: serde_json::to_string(&crate::annotations::anchor_for_line(88, "def checkout!"))
            .unwrap(),
        anchor2: None,
        side: Some("new".to_string()),
        act: "issue".to_string(),
        blocking: false,
        cites_json: None,
        fingerprint: None,
        supersedes: Vec::new(),
    }
}

fn setup_review_for_findings(store: &Store) -> (i64, i64) {
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let review_id = store
        .create_review("r", Some("feat"), "main", "feature", None, 1_000)
        .unwrap();
    (repo_id, review_id)
}

#[test]
fn insert_review_finding_creates_a_1to1_annotation_with_intent_finding() {
    let (_tmp, store) = open_temp();
    let (repo_id, review_id) = setup_review_for_findings(&store);
    let new = NewReviewFinding {
        review_id,
        repo_id,
        ps_number: 1,
        slug: "f-dedup-race".to_string(),
        severity: SEVERITY_CONCERN.to_string(),
        category: "Concurrency".to_string(),
        location_kind: LOCATION_KIND_SINGLE.to_string(),
        location_path: "app/models/order.rb".to_string(),
        location_lines: Some(location_lines_json(&[88])),
        location_removed: false,
        title: "Duplicate order rows possible".to_string(),
        rationale: "Verified against db/schema.rb:41".to_string(),
        recommendation: None,
        evidence_lang: None,
        evidence_source: None,
        anchor_kind: crate::annotations::ANCHOR_KIND_LINE.to_string(),
        anchor: serde_json::to_string(&crate::annotations::anchor_for_line(88, "def checkout!"))
            .unwrap(),
        anchor2: None,
        side: Some("new".to_string()),
        author: "claude".to_string(),
        import_batch_id: "batch-1".to_string(),
        origin: FINDING_ORIGIN_IMPORT.to_string(),
        finding_author: None,
        act: "issue".to_string(),
        blocking: false,
        cites_json: None,
        fingerprint: None,
    };
    let (annotation_id, finding_id) = store.insert_review_finding(&new, 1_000).unwrap();
    assert!(finding_id > 0);

    let ann = store.get_annotation(&annotation_id).unwrap().unwrap();
    assert_eq!(ann.intent, crate::annotations::INTENT_FINDING);
    assert_eq!(ann.review_id, Some(review_id));
    assert_eq!(ann.ps_number, Some(1));
    assert_eq!(ann.side.as_deref(), Some("new"));
    assert_eq!(ann.body, "Duplicate order rows possible");
    assert!(!ann.resolved);

    let finding = store
        .get_review_finding(review_id, "f-dedup-race")
        .unwrap()
        .unwrap();
    assert_eq!(finding.annotation_id, annotation_id);
    assert_eq!(finding.severity, "concern");
    assert_eq!(finding.disposition, None);
    assert!(!finding.superseded);
    assert_eq!(finding.published_state, "unpublished");
    assert_eq!(finding.origin, "import");
    assert_eq!(
        finding.author, None,
        "NULL is acceptable for origin=import in v1"
    );
    assert_eq!(
        finding.content_updated_at, None,
        "unset until a re-import refresh"
    );
}

#[test]
fn reconcile_findings_import_new_slug_creates() {
    let (_tmp, store) = open_temp();
    let (repo_id, review_id) = setup_review_for_findings(&store);
    let findings = vec![
        sample_imported_finding("f-a"),
        sample_imported_finding("f-b"),
    ];
    let outcome = store
        .reconcile_findings_import(
            review_id,
            repo_id,
            1,
            "batch-1",
            "claude",
            &findings,
            FindingsImportMode::Full,
            1_000,
        )
        .unwrap();
    assert_eq!(outcome.created, vec!["f-a".to_string(), "f-b".to_string()]);
    assert!(outcome.updated.is_empty());
    assert!(outcome.superseded.is_empty());
    assert!(outcome.unchanged.is_empty());

    let listed = store.list_review_findings(review_id, None, false).unwrap();
    assert_eq!(listed.len(), 2);
}

#[test]
fn reconcile_findings_import_existing_slug_present_again_refreshes_but_preserves_disposition_and_thread(
) {
    let (_tmp, store) = open_temp();
    let (repo_id, review_id) = setup_review_for_findings(&store);
    let first = vec![sample_imported_finding("f-a")];
    store
        .reconcile_findings_import(
            review_id,
            repo_id,
            1,
            "batch-1",
            "claude",
            &first,
            FindingsImportMode::Full,
            1_000,
        )
        .unwrap();
    let before = store.get_review_finding(review_id, "f-a").unwrap().unwrap();

    // A human dispositions it, and asks a question in its thread.
    store
        .set_finding_disposition(review_id, "f-a", "agree", Some("yep"), "you", 1_010)
        .unwrap();
    let reply_id = crate::annotations::new_annotation_id();
    store
        .insert_annotation(&AnnotationRow {
            id: reply_id.clone(),
            repo_id,
            path: before.location_path.clone(),
            anchor: None,
            anchor_kind: "line".to_string(),
            anchor2: None,
            parent_id: Some(before.annotation_id.clone()),
            intent: "note".to_string(),
            body: "why?".to_string(),
            author: "you".to_string(),
            created_at: 1_020,
            updated_at: 1_020,
            resolved: false,
            review_id: Some(review_id),
            ps_number: Some(1),
            side: Some("new".to_string()),
            set_id: None,
            trail_id: None,
        })
        .unwrap();

    // Re-review: same slug, DIFFERENT severity/title/rationale.
    let mut refreshed_input = sample_imported_finding("f-a");
    refreshed_input.severity = SEVERITY_BLOCKER.to_string();
    refreshed_input.title = "Actually a blocker now".to_string();
    refreshed_input.rationale = "New evidence found".to_string();
    let second = vec![refreshed_input];
    let outcome = store
        .reconcile_findings_import(
            review_id,
            repo_id,
            2,
            "batch-2",
            "claude",
            &second,
            FindingsImportMode::Full,
            2_000,
        )
        .unwrap();
    assert_eq!(outcome.updated, vec!["f-a".to_string()]);
    assert!(outcome.created.is_empty());
    assert!(outcome.superseded.is_empty());
    assert!(outcome.unchanged.is_empty());

    let after = store.get_review_finding(review_id, "f-a").unwrap().unwrap();
    assert_eq!(
        after.annotation_id, before.annotation_id,
        "same finding, same annotation row"
    );
    assert_eq!(after.severity, "blocker");
    assert_eq!(after.title, "Actually a blocker now");
    assert_eq!(after.rationale, "New evidence found");
    assert_eq!(after.content_updated_at, Some(2_000));

    // Disposition survives untouched.
    assert_eq!(after.disposition.as_deref(), Some("agree"));
    assert_eq!(after.disposition_note.as_deref(), Some("yep"));
    assert_eq!(after.disposition_by.as_deref(), Some("you"));
    assert_eq!(after.disposition_at, Some(1_010));

    // The thread reply survives, and the annotation's anchor/ps_number
    // (its creation-time position) is NOT eagerly rewritten to ps 2.
    let thread = store
        .list_annotations(repo_id, &before.location_path)
        .unwrap();
    assert!(thread.iter().any(|a| a.id == reply_id), "reply survives");
    let ann_after = store.get_annotation(&after.annotation_id).unwrap().unwrap();
    assert_eq!(
        ann_after.ps_number,
        Some(1),
        "anchor stays pinned to its FIRST-import ps"
    );
    assert_eq!(
        ann_after.body, "Actually a blocker now",
        "display body mirrors the refreshed title"
    );
}

#[test]
fn reconcile_findings_import_existing_slug_absent_soft_supersedes() {
    let (_tmp, store) = open_temp();
    let (repo_id, review_id) = setup_review_for_findings(&store);
    let first = vec![
        sample_imported_finding("f-a"),
        sample_imported_finding("f-b"),
    ];
    store
        .reconcile_findings_import(
            review_id,
            repo_id,
            1,
            "batch-1",
            "claude",
            &first,
            FindingsImportMode::Full,
            1_000,
        )
        .unwrap();

    // Re-review only reproduces f-a — f-b is gone from the new diff.
    let second = vec![sample_imported_finding("f-a")];
    let outcome = store
        .reconcile_findings_import(
            review_id,
            repo_id,
            2,
            "batch-2",
            "claude",
            &second,
            FindingsImportMode::Full,
            2_000,
        )
        .unwrap();
    assert_eq!(outcome.superseded, vec!["f-b".to_string()]);
    // f-a is byte-identical to its first import, so it lands in
    // "unchanged", not "updated".
    assert_eq!(outcome.unchanged, vec!["f-a".to_string()]);

    // Never hard-deleted: invisible by default, still readable via
    // include_superseded=true.
    let default_list = store.list_review_findings(review_id, None, false).unwrap();
    assert_eq!(default_list.len(), 1);
    assert_eq!(default_list[0].slug, "f-a");

    let all_list = store.list_review_findings(review_id, None, true).unwrap();
    assert_eq!(all_list.len(), 2);
    let f_b = all_list.iter().find(|f| f.slug == "f-b").unwrap();
    assert!(f_b.superseded);
    assert_eq!(f_b.superseded_reason.as_deref(), Some("not_in_reimport"));
    assert_eq!(f_b.superseded_at, Some(2_000));

    let direct = store.get_review_finding(review_id, "f-b").unwrap().unwrap();
    assert!(
        direct.superseded,
        "still directly gettable by slug — a tombstone, not an erasure"
    );
}

fn insert_manual_finding(
    store: &Store,
    repo_id: i64,
    review_id: i64,
    slug: &str,
) -> NewReviewFinding {
    let new = NewReviewFinding {
        review_id,
        repo_id,
        ps_number: 1,
        slug: slug.to_string(),
        severity: SEVERITY_OK.to_string(),
        category: "Style".to_string(),
        location_kind: LOCATION_KIND_SINGLE.to_string(),
        location_path: "app/models/order.rb".to_string(),
        location_lines: Some(location_lines_json(&[5])),
        location_removed: false,
        title: "A human noticed this too".to_string(),
        rationale: "Spotted while reading the diff.".to_string(),
        recommendation: None,
        evidence_lang: None,
        evidence_source: None,
        anchor_kind: crate::annotations::ANCHOR_KIND_LINE.to_string(),
        anchor: serde_json::to_string(&crate::annotations::anchor_for_line(5, "x")).unwrap(),
        anchor2: None,
        side: Some("new".to_string()),
        author: "you".to_string(),
        import_batch_id: "manual".to_string(),
        origin: FINDING_ORIGIN_MANUAL.to_string(),
        finding_author: Some("you".to_string()),
        act: "issue".to_string(),
        blocking: false,
        cites_json: None,
        fingerprint: None,
    };
    store.insert_review_finding(&new, 900).unwrap();
    new
}

/// PRR-R1 scope extension (operator-ratified mid-build) — a
/// human-authored ("manual") finding is NEVER superseded by an agent's
/// re-import: the agent's own findings set structurally cannot contain
/// a slug it never generated, and that absence must not tombstone it.
/// Disposition, thread, and every field survive a FULL re-import
/// untouched.
#[test]
fn reconcile_findings_import_never_supersedes_a_manual_finding() {
    let (_tmp, store) = open_temp();
    let (repo_id, review_id) = setup_review_for_findings(&store);
    insert_manual_finding(&store, repo_id, review_id, "f-manual");
    store
        .set_finding_disposition(
            review_id,
            "f-manual",
            "agree",
            Some("good catch"),
            "you",
            910,
        )
        .unwrap();
    let before = store
        .get_review_finding(review_id, "f-manual")
        .unwrap()
        .unwrap();
    assert_eq!(before.origin, "manual");
    assert!(!before.superseded);

    // A full agent re-import that never mentions "f-manual" at all.
    let imported = vec![sample_imported_finding("f-a")];
    let outcome = store
        .reconcile_findings_import(
            review_id,
            repo_id,
            1,
            "batch-1",
            "claude",
            &imported,
            FindingsImportMode::Full,
            1_000,
        )
        .unwrap();
    assert!(
        !outcome.superseded.contains(&"f-manual".to_string()),
        "a manual finding must never appear in the superseded bucket"
    );
    assert_eq!(outcome.created, vec!["f-a".to_string()]);

    let after = store
        .get_review_finding(review_id, "f-manual")
        .unwrap()
        .unwrap();
    assert_eq!(after, before, "byte-identical — untouched by the re-import");

    // A second full re-import, still never mentioning it — still safe.
    store
        .reconcile_findings_import(
            review_id,
            repo_id,
            2,
            "batch-2",
            "claude",
            &imported,
            FindingsImportMode::Full,
            2_000,
        )
        .unwrap();
    assert!(
        !store
            .get_review_finding(review_id, "f-manual")
            .unwrap()
            .unwrap()
            .superseded
    );
}

/// PRR-R3 — the OWED defense-in-depth fix: an import batch whose slug
/// COLLIDES with an existing manual finding must never refresh it
/// (content, disposition, thread — none of it), in ANY mode. The route
/// boundary rejects such a batch wholesale before ever reaching this
/// function, but this test pins the data-layer guard directly, calling
/// `reconcile_findings_import` the way a hypothetical future caller
/// that skipped route-level validation still would.
#[test]
fn reconcile_findings_import_refresh_never_overwrites_a_manual_finding_even_on_slug_collision() {
    let (_tmp, store) = open_temp();
    let (repo_id, review_id) = setup_review_for_findings(&store);
    insert_manual_finding(&store, repo_id, review_id, "f-shared-slug");
    let before = store
        .get_review_finding(review_id, "f-shared-slug")
        .unwrap()
        .unwrap();
    assert_eq!(before.origin, "manual");

    // An import batch that reuses the SAME slug with entirely
    // different content — as if the generator agent happened to mint
    // an identical-looking slug independently.
    let mut colliding = sample_imported_finding("f-shared-slug");
    colliding.title = "A totally different agent-authored title".to_string();
    colliding.severity = SEVERITY_BLOCKER.to_string();
    let outcome = store
        .reconcile_findings_import(
            review_id,
            repo_id,
            1,
            "batch-1",
            "claude",
            &[colliding],
            FindingsImportMode::Full,
            1_000,
        )
        .unwrap();

    // Never counted as created or updated — the write never happened.
    assert!(outcome.created.is_empty());
    assert!(outcome.updated.is_empty());
    assert_eq!(outcome.unchanged, vec!["f-shared-slug".to_string()]);

    let after = store
        .get_review_finding(review_id, "f-shared-slug")
        .unwrap()
        .unwrap();
    assert_eq!(
        after, before,
        "byte-identical — the manual row must be completely untouched \
         by a colliding import, not just its disposition/origin"
    );
}

/// PRR-R1 scope extension — `FindingsImportMode::Additive` never
/// supersedes anything, even an `origin="import"` slug that would have
/// been superseded under `Full`.
#[test]
fn reconcile_findings_import_additive_mode_never_supersedes() {
    let (_tmp, store) = open_temp();
    let (repo_id, review_id) = setup_review_for_findings(&store);
    let first = vec![
        sample_imported_finding("f-a"),
        sample_imported_finding("f-b"),
    ];
    store
        .reconcile_findings_import(
            review_id,
            repo_id,
            1,
            "batch-1",
            "claude",
            &first,
            FindingsImportMode::Full,
            1_000,
        )
        .unwrap();

    // An additive batch mentioning only a brand-new slug — f-a/f-b are
    // absent, but MUST survive because mode=Additive.
    let additive = vec![sample_imported_finding("f-c")];
    let outcome = store
        .reconcile_findings_import(
            review_id,
            repo_id,
            2,
            "batch-2",
            "claude",
            &additive,
            FindingsImportMode::Additive,
            2_000,
        )
        .unwrap();
    assert_eq!(outcome.created, vec!["f-c".to_string()]);
    assert!(
        outcome.superseded.is_empty(),
        "additive mode supersedes nothing"
    );

    let all = store.list_review_findings(review_id, None, false).unwrap();
    let slugs: std::collections::BTreeSet<_> = all.iter().map(|f| f.slug.as_str()).collect();
    assert_eq!(
        slugs,
        std::collections::BTreeSet::from(["f-a", "f-b", "f-c"]),
        "f-a/f-b stay visible — additive mode never tombstones an absent slug"
    );
    assert!(
        !store
            .get_review_finding(review_id, "f-a")
            .unwrap()
            .unwrap()
            .superseded
    );
    assert!(
        !store
            .get_review_finding(review_id, "f-b")
            .unwrap()
            .unwrap()
            .superseded
    );
}

#[test]
fn reconcile_findings_import_unsupersedes_a_slug_that_reappears() {
    let (_tmp, store) = open_temp();
    let (repo_id, review_id) = setup_review_for_findings(&store);
    let one = vec![sample_imported_finding("f-a")];
    store
        .reconcile_findings_import(
            review_id,
            repo_id,
            1,
            "batch-1",
            "claude",
            &one,
            FindingsImportMode::Full,
            1_000,
        )
        .unwrap();
    // Drop it (superseded)...
    store
        .reconcile_findings_import(
            review_id,
            repo_id,
            2,
            "batch-2",
            "claude",
            &[],
            FindingsImportMode::Full,
            2_000,
        )
        .unwrap();
    assert!(
        store
            .get_review_finding(review_id, "f-a")
            .unwrap()
            .unwrap()
            .superseded
    );

    // ...then it comes back in a later re-review (same content).
    let outcome = store
        .reconcile_findings_import(
            review_id,
            repo_id,
            3,
            "batch-3",
            "claude",
            &one,
            FindingsImportMode::Full,
            3_000,
        )
        .unwrap();
    assert_eq!(
        outcome.updated,
        vec!["f-a".to_string()],
        "un-superseding is a real write"
    );
    let back = store.get_review_finding(review_id, "f-a").unwrap().unwrap();
    assert!(!back.superseded);
    assert_eq!(back.superseded_at, None);
    assert_eq!(back.superseded_reason, None);
}

#[test]
fn reconcile_findings_import_a_second_identical_import_reports_unchanged_and_writes_nothing() {
    let (_tmp, store) = open_temp();
    let (repo_id, review_id) = setup_review_for_findings(&store);
    let findings = vec![sample_imported_finding("f-a")];
    store
        .reconcile_findings_import(
            review_id,
            repo_id,
            1,
            "batch-1",
            "claude",
            &findings,
            FindingsImportMode::Full,
            1_000,
        )
        .unwrap();
    let before = store.get_review_finding(review_id, "f-a").unwrap().unwrap();

    let outcome = store
        .reconcile_findings_import(
            review_id,
            repo_id,
            1,
            "batch-2",
            "claude",
            &findings,
            FindingsImportMode::Full,
            2_000,
        )
        .unwrap();
    assert_eq!(outcome.unchanged, vec!["f-a".to_string()]);
    assert!(outcome.updated.is_empty());

    let after = store.get_review_finding(review_id, "f-a").unwrap().unwrap();
    assert_eq!(
        after, before,
        "a true no-op import touches nothing, not even updated_at"
    );
}

#[test]
fn list_review_findings_filters_by_disposition_and_include_superseded() {
    let (_tmp, store) = open_temp();
    let (repo_id, review_id) = setup_review_for_findings(&store);
    let findings = vec![
        sample_imported_finding("f-a"),
        sample_imported_finding("f-b"),
        sample_imported_finding("f-c"),
    ];
    store
        .reconcile_findings_import(
            review_id,
            repo_id,
            1,
            "batch-1",
            "claude",
            &findings,
            FindingsImportMode::Full,
            1_000,
        )
        .unwrap();
    store
        .set_finding_disposition(review_id, "f-a", "agree", None, "you", 1_010)
        .unwrap();
    store
        .set_finding_disposition(review_id, "f-b", "waive", None, "you", 1_010)
        .unwrap();

    let agreed = store
        .list_review_findings(review_id, Some("agree"), false)
        .unwrap();
    assert_eq!(agreed.len(), 1);
    assert_eq!(agreed[0].slug, "f-a");

    let all = store.list_review_findings(review_id, None, false).unwrap();
    assert_eq!(all.len(), 3);
}

#[test]
fn set_and_clear_finding_disposition_report_missing_vs_noop_vs_changed() {
    let (_tmp, store) = open_temp();
    let (repo_id, review_id) = setup_review_for_findings(&store);
    let findings = vec![sample_imported_finding("f-a")];
    store
        .reconcile_findings_import(
            review_id,
            repo_id,
            1,
            "batch-1",
            "claude",
            &findings,
            FindingsImportMode::Full,
            1_000,
        )
        .unwrap();

    // Missing slug -> None.
    assert_eq!(
        store
            .set_finding_disposition(review_id, "nope", "agree", None, "you", 1_000)
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .clear_finding_disposition(review_id, "nope", 1_000)
            .unwrap(),
        None
    );

    // First set -> changed.
    assert_eq!(
        store
            .set_finding_disposition(review_id, "f-a", "waive", Some("later"), "you", 1_010)
            .unwrap(),
        Some(true)
    );
    // Identical re-set -> no-op.
    assert_eq!(
        store
            .set_finding_disposition(review_id, "f-a", "waive", Some("later"), "you", 1_020)
            .unwrap(),
        Some(false)
    );
    // Different note -> changed.
    assert_eq!(
        store
            .set_finding_disposition(
                review_id,
                "f-a",
                "waive",
                Some("actually now"),
                "you",
                1_030
            )
            .unwrap(),
        Some(true)
    );

    // Clear -> changed, then no-op.
    assert_eq!(
        store
            .clear_finding_disposition(review_id, "f-a", 1_040)
            .unwrap(),
        Some(true)
    );
    assert_eq!(
        store
            .clear_finding_disposition(review_id, "f-a", 1_050)
            .unwrap(),
        Some(false)
    );
    let cleared = store.get_review_finding(review_id, "f-a").unwrap().unwrap();
    assert_eq!(cleared.disposition, None);
    assert_eq!(cleared.disposition_note, None);
    assert_eq!(cleared.disposition_by, None);
    assert_eq!(cleared.disposition_at, None);
}

#[test]
fn set_finding_published_records_state_url_and_timestamp() {
    let (_tmp, store) = open_temp();
    let (repo_id, review_id) = setup_review_for_findings(&store);
    let findings = vec![sample_imported_finding("f-a")];
    store
        .reconcile_findings_import(
            review_id,
            repo_id,
            1,
            "batch-1",
            "claude",
            &findings,
            FindingsImportMode::Full,
            1_000,
        )
        .unwrap();
    assert!(store
        .set_finding_published(
            review_id,
            "f-a",
            Some("https://github.com/a/b/pull/1#discussion_r1"),
            1_500
        )
        .unwrap());
    let published = store.get_review_finding(review_id, "f-a").unwrap().unwrap();
    assert_eq!(published.published_state, "published");
    assert_eq!(published.published_at, Some(1_500));
    assert_eq!(
        published.published_url.as_deref(),
        Some("https://github.com/a/b/pull/1#discussion_r1")
    );
    assert!(!store
        .set_finding_published(review_id, "nope", None, 1_500)
        .unwrap());
}

/// V0024 on a fresh db: refinery runs the whole chain, so the new
/// `reviews` columns and `review_findings` are reachable, and a raw
/// insert using the NEW `intent="finding"` / `anchor_kind="whole_file"`
/// vocab values succeeds at the SQL layer with no CHECK constraint in
/// the way (vocab is route-validated only — see the migration's doc).
#[test]
fn v0024_migration_applies_cleanly_and_accepts_the_new_vocab_values_unconstrained() {
    let (_tmp, store) = open_temp();
    let names: Vec<String> = store
        .lock()
        .prepare("PRAGMA table_info(reviews)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    for col in [
        "pr_number",
        "pr_repo_slug",
        "pr_head_sha",
        "pr_meta_json",
        "pr_meta_fetched_at",
        "artifact_hint_kb",
        "artifact_hint_id",
        "report_json",
        "report_updated_at",
        "verdict_published_at",
        "verdict_published_url",
    ] {
        assert!(names.contains(&col.to_string()), "missing column {col}");
    }

    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let review_id = store
        .create_review("r", None, "main", "feature", None, 1_000)
        .unwrap();
    let new = NewReviewFinding {
        review_id,
        repo_id,
        ps_number: 1,
        slug: "f-whole".to_string(),
        severity: SEVERITY_OK.to_string(),
        category: "Style".to_string(),
        location_kind: LOCATION_KIND_WHOLE_FILE.to_string(),
        location_path: "config/routes.rb".to_string(),
        location_lines: None,
        location_removed: false,
        title: "Consider splitting this file".to_string(),
        rationale: "It has grown large.".to_string(),
        recommendation: None,
        evidence_lang: None,
        evidence_source: None,
        anchor_kind: crate::annotations::ANCHOR_KIND_WHOLE_FILE.to_string(),
        anchor: "config/routes.rb".to_string(),
        anchor2: None,
        side: Some("new".to_string()),
        author: "claude".to_string(),
        import_batch_id: "batch-1".to_string(),
        // Also exercises the "manual" origin + a real author string —
        // the SQL layer imposes no CHECK on either.
        origin: FINDING_ORIGIN_MANUAL.to_string(),
        finding_author: Some("carol".to_string()),
        act: "issue".to_string(),
        blocking: false,
        cites_json: None,
        fingerprint: None,
    };
    let (annotation_id, _finding_id) = store.insert_review_finding(&new, 1_000).unwrap();
    let ann = store.get_annotation(&annotation_id).unwrap().unwrap();
    assert_eq!(ann.intent, "finding");
    assert_eq!(ann.anchor_kind, "whole_file");
    assert_eq!(ann.anchor.as_deref(), Some("config/routes.rb"));

    let finding = store
        .get_review_finding(review_id, "f-whole")
        .unwrap()
        .unwrap();
    assert_eq!(finding.origin, "manual");
    assert_eq!(finding.author.as_deref(), Some("carol"));
}

/// `PATCH TABLE review_findings` cascades on review delete (SQL
/// `ON DELETE CASCADE` per the migration), unlike `annotations.review_id`
/// (no SQL FK, cascade is code-owned — V0023's own precedent). This pins
/// that the two mechanisms both actually clean up, even though they get
/// there differently.
#[test]
fn review_findings_cascade_deletes_with_their_review_via_sql_fk() {
    let (_tmp, store) = open_temp();
    let (repo_id, review_id) = setup_review_for_findings(&store);
    let findings = vec![sample_imported_finding("f-a")];
    store
        .reconcile_findings_import(
            review_id,
            repo_id,
            1,
            "batch-1",
            "claude",
            &findings,
            FindingsImportMode::Full,
            1_000,
        )
        .unwrap();
    assert_eq!(
        store
            .list_review_findings(review_id, None, true)
            .unwrap()
            .len(),
        1
    );

    store
        .lock()
        .execute("DELETE FROM reviews WHERE id = ?1", params![review_id])
        .unwrap();
    assert_eq!(
        store
            .list_review_findings(review_id, None, true)
            .unwrap()
            .len(),
        0
    );
}

// ── PRR-N12: scip runs ──────────────────────────────────────────────

#[test]
fn latest_scip_run_is_none_when_never_ingested() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    assert!(store.latest_scip_run(repo_id).unwrap().is_none());
}

#[test]
fn record_scip_run_appends_and_latest_scip_run_reads_the_newest() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();

    store.record_scip_run(repo_id, "sha-one", 100, 3).unwrap();
    let latest = store.latest_scip_run(repo_id).unwrap().unwrap();
    assert_eq!(latest.head_sha, "sha-one");
    assert_eq!(latest.ingested_at, 100);
    assert_eq!(latest.docs_accepted, 3);

    // A second, later run supersedes — `latest_scip_run` never returns
    // the older row once a newer one lands (multiple `scip run`
    // invocations against the same repo are all kept, but only the
    // newest drives `ScipStatus`).
    store.record_scip_run(repo_id, "sha-two", 200, 5).unwrap();
    let latest = store.latest_scip_run(repo_id).unwrap().unwrap();
    assert_eq!(latest.head_sha, "sha-two");
    assert_eq!(latest.ingested_at, 200);
    assert_eq!(latest.docs_accepted, 5);
}

#[test]
fn scip_run_writes_do_not_bump_the_store_generation() {
    // `scip_runs` is invisible to `FileIndex`/`SymbolIndex`'s
    // generation-keyed caches — same posture as `doc_lens_pins`'
    // `pin_writes_do_not_bump_the_store_generation`.
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let before = store.generation();
    store.record_scip_run(repo_id, "sha", 1, 0).unwrap();
    assert_eq!(store.generation(), before);

    // Control: a files write DOES bump, so this test can't pass
    // vacuously.
    store.upsert_file(repo_id, "a.rs", "h", "rust", 1).unwrap();
    assert!(store.generation() > before);
}

#[test]
fn count_files_by_langs_counts_only_the_named_langs_and_short_circuits_on_empty() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store.upsert_file(repo_id, "a.rs", "h1", "rust", 1).unwrap();
    store.upsert_file(repo_id, "b.rs", "h2", "rust", 1).unwrap();
    store.upsert_file(repo_id, "c.rb", "h3", "ruby", 1).unwrap();
    store
        .upsert_file(repo_id, "d.txt", "h4", "unknown", 1)
        .unwrap();

    assert_eq!(
        store
            .count_files_by_langs(repo_id, &["rust".to_string()])
            .unwrap(),
        2
    );
    assert_eq!(
        store
            .count_files_by_langs(repo_id, &["rust".to_string(), "ruby".to_string()])
            .unwrap(),
        3
    );
    assert_eq!(store.count_files_by_langs(repo_id, &[]).unwrap(), 0);
    assert_eq!(
        store
            .count_files_by_langs(repo_id, &["python".to_string()])
            .unwrap(),
        0
    );
}

#[test]
fn count_scip_covered_files_counts_distinct_paths_with_an_scip_source_occurrence() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .upsert_file(repo_id, "a.rs", "hash-a", "rust", 1)
        .unwrap();
    store
        .upsert_file(repo_id, "b.rs", "hash-b", "rust", 1)
        .unwrap();
    let lang = crate::lang::for_id("rust").unwrap();

    // Nothing ingested yet.
    assert_eq!(store.count_scip_covered_files(repo_id).unwrap(), 0);

    store
        .replace_scip_occurrences(
            "hash-a",
            lang.symbol_salt,
            &[ScipOccurrenceIn {
                name: "widget".to_string(),
                role: "def".to_string(),
                line: 1,
                col_start: 0,
                col_end: 6,
            }],
        )
        .unwrap();
    assert_eq!(store.count_scip_covered_files(repo_id).unwrap(), 1);

    // A plain tree-sitter (`source = 'ts'`) occurrence on `b.rs` does
    // NOT count — only `source = 'scip'` rows do.
    store
        .replace_occurrences(
            "hash-b",
            lang.symbol_salt,
            &[crate::occurrences::Occurrence {
                ordinal: 0,
                name: "other".to_string(),
                role: "def".to_string(),
                line: 1,
                col_start: 0,
                col_end: 5,
                source: crate::occurrences::SOURCE_TS.to_string(),
                local_def_ordinal: None,
            }],
        )
        .unwrap();
    assert_eq!(store.count_scip_covered_files(repo_id).unwrap(), 1);
}

// --- PRR-R9: review analytics ------------------------------------------

fn analytics_finding(slug: &str, severity: &str, category: &str, path: &str) -> ImportedFinding {
    let mut f = sample_imported_finding(slug);
    f.severity = severity.to_string();
    f.category = category.to_string();
    f.location_path = path.to_string();
    f
}

#[test]
fn list_findings_for_analytics_filters_by_repo_and_created_at_window_and_includes_superseded() {
    let (_tmp, store) = open_temp();
    let repo_id_a = store.upsert_repo("a", "/tmp/a").unwrap();
    let review_a = store
        .create_review("a", None, "main", "feature", None, 1_000)
        .unwrap();
    let repo_id_b = store.upsert_repo("b", "/tmp/b").unwrap();
    let review_b = store
        .create_review("b", None, "main", "feature", None, 1_000)
        .unwrap();

    store
        .reconcile_findings_import(
            review_a,
            repo_id_a,
            1,
            "batch-a",
            "claude",
            &[analytics_finding(
                "f-a1",
                SEVERITY_BLOCKER,
                "Security",
                "x.rb",
            )],
            FindingsImportMode::Full,
            1_000,
        )
        .unwrap();
    store
        .reconcile_findings_import(
            review_a,
            repo_id_a,
            1,
            "batch-a2",
            "claude",
            &[analytics_finding("f-a2", SEVERITY_OK, "Style", "y.rb")],
            FindingsImportMode::Additive,
            5_000,
        )
        .unwrap();
    store
        .reconcile_findings_import(
            review_b,
            repo_id_b,
            1,
            "batch-b",
            "claude",
            &[analytics_finding(
                "f-b1",
                SEVERITY_CONCERN,
                "Security",
                "x.rb",
            )],
            FindingsImportMode::Full,
            1_000,
        )
        .unwrap();
    // Supersede f-a1 by re-importing batch-a in `Full` mode without it.
    store
        .reconcile_findings_import(
            review_a,
            repo_id_a,
            1,
            "batch-a3",
            "claude",
            &[],
            FindingsImportMode::Full,
            9_000,
        )
        .unwrap();

    // repo filter.
    let repo_a = store
        .list_findings_for_analytics(Some("a"), None, None)
        .unwrap();
    assert_eq!(repo_a.len(), 2, "both a-findings, superseded included");

    // no repo filter -> every repo.
    let all = store.list_findings_for_analytics(None, None, None).unwrap();
    assert_eq!(all.len(), 3);

    // created_at window excludes f-a1 (created_at=1_000).
    let windowed = store
        .list_findings_for_analytics(Some("a"), Some(2_000), None)
        .unwrap();
    assert_eq!(windowed.len(), 1);
    assert_eq!(windowed[0].category, "Style");

    // superseded is surfaced, not dropped.
    let a1 = repo_a
        .iter()
        .find(|r| r.category == "Security")
        .expect("f-a1 present");
    assert!(a1.superseded, "reimport without f-a1 must supersede it");
}

#[test]
fn recurrence_pairs_requires_at_least_min_reviews_distinct_reviews() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let review1 = store
        .create_review("r", None, "main", "f1", None, 1_000)
        .unwrap();
    let review2 = store
        .create_review("r", None, "main", "f2", None, 1_000)
        .unwrap();
    let review3 = store
        .create_review("r", None, "main", "f3", None, 1_000)
        .unwrap();

    // (Security, x.rb) recurs across review1 + review2.
    store
        .reconcile_findings_import(
            review1,
            repo_id,
            1,
            "b1",
            "claude",
            &[analytics_finding(
                "f1",
                SEVERITY_BLOCKER,
                "Security",
                "x.rb",
            )],
            FindingsImportMode::Full,
            1_000,
        )
        .unwrap();
    store
        .reconcile_findings_import(
            review2,
            repo_id,
            1,
            "b2",
            "claude",
            &[analytics_finding(
                "f2",
                SEVERITY_BLOCKER,
                "Security",
                "x.rb",
            )],
            FindingsImportMode::Full,
            1_000,
        )
        .unwrap();
    // (Style, y.rb) appears only once -> below threshold.
    store
        .reconcile_findings_import(
            review3,
            repo_id,
            1,
            "b3",
            "claude",
            &[analytics_finding("f3", SEVERITY_OK, "Style", "y.rb")],
            FindingsImportMode::Full,
            1_000,
        )
        .unwrap();

    let pairs = store
        .recurrence_pairs(Some("r"), None, None, RECURRENCE_MIN_REVIEWS)
        .unwrap();
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0].category, "Security");
    assert_eq!(pairs[0].location_path, "x.rb");
    assert_eq!(pairs[0].review_count, 2);
    assert_eq!(pairs[0].review_ids, {
        let mut v = vec![review1, review2];
        v.sort_unstable();
        v
    });
}

#[test]
fn recurrence_pairs_excludes_superseded_findings_from_the_count() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let review1 = store
        .create_review("r", None, "main", "f1", None, 1_000)
        .unwrap();
    let review2 = store
        .create_review("r", None, "main", "f2", None, 1_000)
        .unwrap();
    store
        .reconcile_findings_import(
            review1,
            repo_id,
            1,
            "b1",
            "claude",
            &[analytics_finding(
                "f1",
                SEVERITY_BLOCKER,
                "Security",
                "x.rb",
            )],
            FindingsImportMode::Full,
            1_000,
        )
        .unwrap();
    store
        .reconcile_findings_import(
            review2,
            repo_id,
            1,
            "b2",
            "claude",
            &[analytics_finding(
                "f2",
                SEVERITY_BLOCKER,
                "Security",
                "x.rb",
            )],
            FindingsImportMode::Full,
            1_000,
        )
        .unwrap();
    // Supersede review2's finding — re-import Full with an empty batch.
    store
        .reconcile_findings_import(
            review2,
            repo_id,
            1,
            "b3",
            "claude",
            &[],
            FindingsImportMode::Full,
            2_000,
        )
        .unwrap();

    let pairs = store
        .recurrence_pairs(Some("r"), None, None, RECURRENCE_MIN_REVIEWS)
        .unwrap();
    assert!(
        pairs.is_empty(),
        "only one non-superseded review left -> below threshold"
    );
}

// --- V77-P3 (Task 2, the E6 finding): `files_by_lang` bytes-desc order --

#[test]
fn files_by_lang_orders_by_bytes_desc_with_a_lang_asc_tiebreak() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    // Alphabetically: bash < ruby < yaml. Bytes-desc must reorder them
    // entirely — the E6 finding was exactly a small-but-early language
    // starving a repo's dominant (biggest-bytes) one out of a shared
    // clock, so the fix must not merely tie-break alphabetically.
    store
        .upsert_file(repo_id, "a.sh", "h1", "bash", 10)
        .unwrap();
    store
        .upsert_file(repo_id, "b.rb", "h2", "ruby", 1_000)
        .unwrap();
    store
        .upsert_file(repo_id, "c.yml", "h3", "yaml", 100)
        .unwrap();
    // A tie: two langs with the SAME total bytes must fall back to
    // `lang` ascending, deterministically.
    store.upsert_file(repo_id, "d.go", "h4", "go", 100).unwrap();

    let rows = store.files_by_lang(repo_id).unwrap();
    let langs: Vec<&str> = rows.iter().map(|(l, _, _)| l.as_str()).collect();
    assert_eq!(
        langs,
        vec!["ruby", "go", "yaml", "bash"],
        "expected bytes DESC (1000, 100, 100, 10), tie broken by lang ASC (go < yaml): {rows:?}"
    );
}

// --- V77-P3 (Task 0): `derived_status_for_current_salts` ----------------

#[test]
fn derived_status_for_current_salts_includes_both_families_and_excludes_stale_salts() {
    let (_tmp, store) = open_temp();
    store
        .mark_derived(
            "hash1",
            crate::lang::SaltFamily::Symbol,
            crate::lang::RUST.symbol_salt,
            1,
        )
        .unwrap();
    store
        .mark_derived(
            "hash1",
            crate::lang::SaltFamily::Highlight,
            crate::lang::RUST.highlight_salt,
            1,
        )
        .unwrap();
    // A STALE salt (not any current language's) must never appear.
    store
        .mark_derived(
            "hash2",
            crate::lang::SaltFamily::Symbol,
            "rust@0.0.0+stale",
            1,
        )
        .unwrap();

    let rows = store.derived_status_for_current_salts().unwrap();
    assert!(
        rows.contains(&(
            "hash1".to_string(),
            crate::lang::SaltFamily::Symbol.as_str().to_string(),
            crate::lang::RUST.symbol_salt.to_string(),
        )),
        "expected the current symbol salt row: {rows:?}"
    );
    assert!(
        rows.contains(&(
            "hash1".to_string(),
            crate::lang::SaltFamily::Highlight.as_str().to_string(),
            crate::lang::RUST.highlight_salt.to_string(),
        )),
        "expected the current highlight salt row: {rows:?}"
    );
    assert!(
        !rows.iter().any(|(h, _, _)| h == "hash2"),
        "a stale salt must never appear in the preload: {rows:?}"
    );
}

#[test]
fn derived_status_for_current_salts_is_empty_on_a_fresh_store() {
    let (_tmp, store) = open_temp();
    assert!(store.derived_status_for_current_salts().unwrap().is_empty());
}

// ── RS-U1 — review store + base model (V0045) ─────────────────────────
//
// `store/review_stores.rs` implements the methods under test here. The
// migration itself is `migrations/V0045__review_store.sql` — see its own
// header for what each column means and the legacy-row semantics
// (`base_set_by` defaults to `'legacy'`; `base_mode`/`base_branch`/
// `base_member`/`base_tip_sha`/`kind` stay NULL for pre-migration rows).

#[test]
fn review_store_crud_round_trips_by_id_uuid_and_key() {
    let (_tmp, store) = open_temp();
    let id = store
        .create_review_store(
            "11111111-1111-1111-1111-111111111111",
            "github.com/acme/widgets",
            "/state/kb-code/git/11111111-1111-1111-1111-111111111111.git",
            Some("https://github.com/acme/widgets.git"),
            Some("pr-slug"),
            1_000,
        )
        .unwrap();

    let by_id = store.get_review_store(id).unwrap().unwrap();
    assert_eq!(by_id.store_key, "github.com/acme/widgets");
    assert_eq!(
        by_id.base_url.as_deref(),
        Some("https://github.com/acme/widgets.git")
    );
    assert_eq!(by_id.base_url_source.as_deref(), Some("pr-slug"));
    // Column DEFAULTs, never guessed by the create call.
    assert_eq!(by_id.cred_kind, "inherit");
    assert_eq!(by_id.forge_verified, "unverified");
    assert_eq!(by_id.state, "absent");
    assert_eq!(by_id.created_at, 1_000);

    let by_uuid = store
        .get_review_store_by_uuid("11111111-1111-1111-1111-111111111111")
        .unwrap()
        .unwrap();
    assert_eq!(by_uuid.id, id);

    let by_key = store
        .get_review_store_by_key("github.com/acme/widgets")
        .unwrap()
        .unwrap();
    assert_eq!(by_key.id, id);

    assert!(store
        .get_review_store_by_key("github.com/nope/nope")
        .unwrap()
        .is_none());

    let listed = store.list_review_stores().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, id);
}

/// D2/README §5.1's registration ladder joins an EXISTING store by
/// `store_key` rather than minting a second one for the same forge
/// project — the UNIQUE index on `store_key` is what makes that a DB-level
/// guarantee rather than only an application-level convention.
#[test]
fn store_key_is_unique_a_second_create_for_the_same_key_errors() {
    let (_tmp, store) = open_temp();
    store
        .create_review_store(
            "uuid-a",
            "github.com/acme/widgets",
            "/a.git",
            None,
            None,
            1_000,
        )
        .unwrap();
    let err = store
        .create_review_store(
            "uuid-b",
            "github.com/acme/widgets",
            "/b.git",
            None,
            None,
            1_000,
        )
        .expect_err("a second store for the same store_key must be refused, not minted");
    assert!(matches!(err, StoreError::Sqlite(_)), "{err:?}");
}

#[test]
fn review_store_state_moves_through_the_seeding_lifecycle() {
    let (_tmp, store) = open_temp();
    let id = store
        .create_review_store("u", "github.com/acme/widgets", "/g.git", None, None, 1_000)
        .unwrap();
    assert_eq!(store.get_review_store(id).unwrap().unwrap().state, "absent");

    assert!(store
        .set_review_store_state(id, "seeding", Some("{\"code\":\"local-fetch\"}"))
        .unwrap());
    let row = store.get_review_store(id).unwrap().unwrap();
    assert_eq!(row.state, "seeding");
    assert_eq!(
        row.state_json.as_deref(),
        Some("{\"code\":\"local-fetch\"}")
    );

    assert!(store.set_review_store_state(id, "ready", None).unwrap());
    assert_eq!(store.get_review_store(id).unwrap().unwrap().state, "ready");

    // A missing id reports false, not an error and not a panic.
    assert!(!store
        .set_review_store_state(id + 999, "ready", None)
        .unwrap());
}

/// `state` is CHECK-constrained at the schema level (the migration
/// header): an invalid value must fail the write, never silently coerce.
#[test]
fn review_store_state_rejects_a_value_outside_the_closed_set() {
    let (_tmp, store) = open_temp();
    let id = store
        .create_review_store("u", "github.com/acme/widgets", "/g.git", None, None, 1_000)
        .unwrap();
    let err = store
        .set_review_store_state(id, "not-a-real-state", None)
        .expect_err("the CHECK constraint must reject an unknown state");
    assert!(matches!(err, StoreError::Sqlite(_)), "{err:?}");
}

#[test]
fn review_store_forge_and_credential_setters_round_trip() {
    let (_tmp, store) = open_temp();
    let id = store
        .create_review_store("u", "github.com/acme/widgets", "/g.git", None, None, 1_000)
        .unwrap();

    assert!(store
        .set_review_store_forge(
            id,
            Some("github"),
            Some("github.com"),
            Some("acme/widgets"),
            "verified"
        )
        .unwrap());
    let row = store.get_review_store(id).unwrap().unwrap();
    assert_eq!(row.forge_kind.as_deref(), Some("github"));
    assert_eq!(row.forge_host.as_deref(), Some("github.com"));
    assert_eq!(row.forge_slug.as_deref(), Some("acme/widgets"));
    assert_eq!(row.forge_verified, "verified");

    assert!(store
        .set_review_store_credential(
            id,
            "gh-cli",
            Some("D12 pinned account"),
            Some("nicolasacchi")
        )
        .unwrap());
    let row = store.get_review_store(id).unwrap().unwrap();
    assert_eq!(row.cred_kind, "gh-cli");
    assert_eq!(row.cred_reason.as_deref(), Some("D12 pinned account"));
    assert_eq!(row.cred_account.as_deref(), Some("nicolasacchi"));
}

#[test]
fn delete_review_store_removes_the_row() {
    let (_tmp, store) = open_temp();
    let id = store
        .create_review_store("u", "github.com/acme/widgets", "/g.git", None, None, 1_000)
        .unwrap();
    assert!(store.delete_review_store(id).unwrap());
    assert!(store.get_review_store(id).unwrap().is_none());
    assert!(
        !store.delete_review_store(id).unwrap(),
        "deleting again is false, not an error"
    );
}

/// The acceptance-gate shape from BUILD-BRIEF.md U1: "rails-01…05 on a
/// copy resolve to ONE store" — `repo_stores.store_id` is deliberately NOT
/// UNIQUE (D2), so two DIFFERENT repos can both point at the same store.
#[test]
fn repo_store_membership_is_not_unique_two_repos_share_one_store() {
    let (_tmp, store) = open_temp();
    let store_id = store
        .create_review_store("u", "github.com/acme/widgets", "/g.git", None, None, 1_000)
        .unwrap();
    let repo_a = store.upsert_repo("widgets-01", "/tmp/widgets-01").unwrap();
    let repo_b = store.upsert_repo("widgets-02", "/tmp/widgets-02").unwrap();

    store.add_repo_to_store(repo_a, store_id).unwrap();
    store.add_repo_to_store(repo_b, store_id).unwrap();

    let members = store.store_members(store_id).unwrap();
    assert_eq!(members, vec![repo_a, repo_b]);

    let row_a = store.repo_store(repo_a).unwrap().unwrap();
    assert_eq!(row_a.store_id, store_id);
    assert_eq!(
        row_a.legacy_refs_state, "present",
        "column DEFAULT on first insert"
    );
    let row_b = store.repo_store(repo_b).unwrap().unwrap();
    assert_eq!(row_b.store_id, store_id);
}

#[test]
fn add_repo_to_store_is_an_upsert_by_repo_a_repo_belongs_to_exactly_one_store() {
    let (_tmp, store) = open_temp();
    let store_1 = store
        .create_review_store(
            "u1",
            "github.com/acme/widgets",
            "/g1.git",
            None,
            None,
            1_000,
        )
        .unwrap();
    let store_2 = store
        .create_review_store("u2", "github.com/acme/other", "/g2.git", None, None, 1_000)
        .unwrap();
    let repo = store.upsert_repo("widgets-01", "/tmp/widgets-01").unwrap();

    store.add_repo_to_store(repo, store_1).unwrap();
    assert_eq!(store.repo_store(repo).unwrap().unwrap().store_id, store_1);

    // A re-point (`store adopt`-shaped) moves the SAME row rather than
    // erroring or creating a second one.
    store.add_repo_to_store(repo, store_2).unwrap();
    assert_eq!(store.repo_store(repo).unwrap().unwrap().store_id, store_2);
    assert_eq!(store.store_members(store_1).unwrap(), Vec::<i64>::new());
    assert_eq!(store.store_members(store_2).unwrap(), vec![repo]);
}

#[test]
fn remove_repo_from_store_only_drops_the_named_members_row() {
    let (_tmp, store) = open_temp();
    let store_id = store
        .create_review_store("u", "github.com/acme/widgets", "/g.git", None, None, 1_000)
        .unwrap();
    let repo_a = store.upsert_repo("a", "/tmp/a").unwrap();
    let repo_b = store.upsert_repo("b", "/tmp/b").unwrap();
    store.add_repo_to_store(repo_a, store_id).unwrap();
    store.add_repo_to_store(repo_b, store_id).unwrap();

    assert!(store.remove_repo_from_store(repo_a).unwrap());
    assert_eq!(store.store_members(store_id).unwrap(), vec![repo_b]);
    assert!(
        !store.remove_repo_from_store(repo_a).unwrap(),
        "already gone: false, not an error"
    );
}

#[test]
fn repo_store_legacy_setters_round_trip() {
    let (_tmp, store) = open_temp();
    let store_id = store
        .create_review_store("u", "github.com/acme/widgets", "/g.git", None, None, 1_000)
        .unwrap();
    let repo = store.upsert_repo("a", "/tmp/a").unwrap();
    store.add_repo_to_store(repo, store_id).unwrap();

    assert!(store
        .set_repo_store_legacy_import(repo, Some("{\"imported\":198}"))
        .unwrap());
    assert!(store
        .set_repo_store_legacy_refs_state(repo, "cleaned")
        .unwrap());

    let row = store.repo_store(repo).unwrap().unwrap();
    assert_eq!(
        row.legacy_import_json.as_deref(),
        Some("{\"imported\":198}")
    );
    assert_eq!(row.legacy_refs_state, "cleaned");
}

/// G2 (verify/store.md): `reviews.repo` is a NAME, not `repos.id` — this
/// is the two-hop join a caller uses to find a review's store, proven
/// through the SAME repo name a review itself carries.
#[test]
fn store_for_repo_name_resolves_the_two_hop_join() {
    let (_tmp, store) = open_temp();
    let store_id = store
        .create_review_store("u", "github.com/acme/widgets", "/g.git", None, None, 1_000)
        .unwrap();
    let repo_id = store.upsert_repo("widgets-01", "/tmp/widgets-01").unwrap();
    store.add_repo_to_store(repo_id, store_id).unwrap();

    let review_id = store
        .create_review("widgets-01", None, "main", "HEAD", None, 1_000)
        .unwrap();
    let review = store.get_review(review_id).unwrap().unwrap();

    let found = store
        .store_for_repo_name(&review.repo)
        .unwrap()
        .expect("the review's own repo name must resolve to its store");
    assert_eq!(found.id, store_id);

    assert!(store.store_for_repo_name("no-such-repo").unwrap().is_none());
}

#[test]
fn review_base_get_returns_none_for_a_missing_review() {
    let (_tmp, store) = open_temp();
    assert!(store.get_review_base(999_999).unwrap().is_none());
}

/// A review created through the pre-existing, unchanged `create_review`
/// (every caller until a later unit wires the base model into review
/// creation) reads back with the column DEFAULTs: `base_set_by = "legacy"`
/// is a REAL value (README §3), everything else NULL.
#[test]
fn a_freshly_created_review_reads_back_as_legacy_until_set_review_base_runs() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("acme", "/tmp/acme").unwrap();
    let review_id = store
        .create_review("acme", None, "main", "HEAD", None, 1_000)
        .unwrap();

    let base = store.get_review_base(review_id).unwrap().unwrap();
    assert_eq!(base.base_mode, None);
    assert_eq!(base.base_branch, None);
    assert_eq!(base.base_member, None);
    assert_eq!(base.base_set_by, "legacy");
    assert_eq!(base.base_status, None);
    assert_eq!(base.objects_state, None);

    assert!(store
        .set_review_base(
            review_id,
            "track",
            Some("main"),
            None,
            "auto",
            Some("{\"state\":\"ok\"}")
        )
        .unwrap());
    let base = store.get_review_base(review_id).unwrap().unwrap();
    assert_eq!(base.base_mode.as_deref(), Some("track"));
    assert_eq!(base.base_branch.as_deref(), Some("main"));
    assert_eq!(base.base_set_by, "auto");
    assert_eq!(base.base_status.as_deref(), Some("{\"state\":\"ok\"}"));

    // `local()` mode names a member repo.
    assert!(store
        .set_review_base(
            review_id,
            "local",
            Some("feature"),
            Some(repo_id),
            "user",
            None
        )
        .unwrap());
    let base = store.get_review_base(review_id).unwrap().unwrap();
    assert_eq!(base.base_mode.as_deref(), Some("local"));
    assert_eq!(base.base_member, Some(repo_id));
    assert_eq!(base.base_set_by, "user");

    assert!(!store
        .set_review_base(999_999, "pin", None, None, "user", None)
        .unwrap());
}

#[test]
fn review_objects_state_setter_round_trips_and_clears() {
    let (_tmp, store) = open_temp();
    let review_id = store
        .create_review("acme", None, "main", "HEAD", None, 1_000)
        .unwrap();
    assert_eq!(
        store
            .get_review_base(review_id)
            .unwrap()
            .unwrap()
            .objects_state,
        None
    );

    assert!(store
        .set_review_objects_state(review_id, Some("objects-missing"))
        .unwrap());
    assert_eq!(
        store
            .get_review_base(review_id)
            .unwrap()
            .unwrap()
            .objects_state
            .as_deref(),
        Some("objects-missing")
    );

    assert!(store.set_review_objects_state(review_id, None).unwrap());
    assert_eq!(
        store
            .get_review_base(review_id)
            .unwrap()
            .unwrap()
            .objects_state,
        None
    );
}

/// A patchset captured through the pre-existing `insert_patchset` (every
/// caller until a later unit wires the base model into capture) reads back
/// as a legacy patchset: `base_tip_sha`/`kind` both NULL, never guessed.
#[test]
fn a_patchset_inserted_the_old_way_reads_back_with_no_base_fields() {
    let (_tmp, store) = open_temp();
    let review_id = store
        .create_review("acme", None, "main", "HEAD", None, 1_000)
        .unwrap();
    store
        .insert_patchset(review_id, 1, "tipsha1", "basesha1", 1_000)
        .unwrap();

    let base = store.get_patchset_base(review_id, 1).unwrap().unwrap();
    assert_eq!(base.base_tip_sha, None);
    assert_eq!(base.kind, None);
    assert!(store.get_patchset_base(review_id, 2).unwrap().is_none());
}

#[test]
fn insert_patchset_with_base_round_trips_the_two_new_columns_and_bumps_review() {
    let (_tmp, store) = open_temp();
    let review_id = store
        .create_review("acme", None, "main", "HEAD", None, 1_000)
        .unwrap();

    let ps_id = store
        .insert_patchset_with_base(
            review_id,
            1,
            "tipsha1",
            "basesha1",
            Some("basetipsha1"),
            Some("base-corrected"),
            2_000,
        )
        .unwrap();
    assert!(ps_id > 0);

    let ps = store.get_patchset(review_id, 1).unwrap().unwrap();
    assert_eq!(ps.tip_sha, "tipsha1");
    assert_eq!(ps.base_sha, "basesha1");
    let base = store.get_patchset_base(review_id, 1).unwrap().unwrap();
    assert_eq!(base.base_tip_sha.as_deref(), Some("basetipsha1"));
    assert_eq!(base.kind.as_deref(), Some("base-corrected"));

    // Same side effect as `insert_patchset`: the parent review's
    // `updated_at` tracks the latest capture.
    assert_eq!(
        store.get_review(review_id).unwrap().unwrap().updated_at,
        2_000
    );
}

/// The golden-relocation shape (BUILD-BRIEF.md U1 "Done when" +
/// BUILDER-RULES §4): apply V0045 on a genuine V0044 fixture and prove
/// every OLD column on a pre-existing review/patchset is byte-identical,
/// the new columns land at their honest legacy defaults, and the crossing
/// wrote `index.db.pre-V0044.bak` (BUILD-BRIEF's literal acceptance line
/// names `.pre-V0045.bak` for a V0044-sourced volume in general prose, but
/// the receipt is always named for the volume's OWN prior epoch — see
/// `backup::snapshot_path` — so a volume that was genuinely AT V0044
/// produces `pre-V0044.bak`, which is what a real upgrade will produce).
#[test]
fn v0045_migration_leaves_a_pre_existing_reviews_row_byte_identical_on_its_old_columns() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("index.db");
    Store::migrate_to_for_test(&path, 44).expect("a genuine V0044 fixture");
    assert_eq!(
        kb_core::sibling::volume_epoch(&Connection::open(&path).unwrap()).unwrap(),
        Some(44),
        "the fixture really is at V0044, one migration short of this binary"
    );

    // Shaped exactly like a pre-V0045 volume: only the columns that exist
    // at V0044, written directly (bypassing every `Store` method, which
    // would require a store already migrated to V0045).
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO repos (name, root) VALUES ('acme', '/tmp/acme')",
            [],
        )
        .unwrap();
        conn.execute(
                "INSERT INTO reviews (repo, title, base_ref, head_ref, session_id, state, created_at, updated_at)
                 VALUES ('acme', 'legacy review', 'cc6561169ccb', 'HEAD', NULL, 'open', 1000, 1000)",
                [],
            )
            .unwrap();
        conn.execute(
            "INSERT INTO review_patchsets (review_id, ps_number, tip_sha, base_sha, captured_at)
             VALUES (1, 1, 'deadbeef', 'cc6561169ccb', 1000)",
            [],
        )
        .unwrap();
    }
    assert!(
        crate::backup::read_receipt(&path).is_none(),
        "nothing has crossed a gated epoch yet"
    );

    // The crossing, through the REAL `Store::open` — not the migration
    // runner in isolation.
    let store = Store::open(&path).expect("V0044 -> V0045 must boot and migrate cleanly");

    // U1's acceptance line: the crossing wrote the pre-migration snapshot.
    let receipt = crate::backup::read_receipt(&path)
        .expect("Store::open must snapshot before crossing a GATED_EPOCHS door");
    assert_eq!(receipt.volume_epoch, Some(44));
    assert!(
        receipt.backup_path.ends_with("index.db.pre-V0044.bak"),
        "{}",
        receipt.backup_path
    );
    assert!(std::path::Path::new(&receipt.backup_path).exists());
    let snap = Connection::open(&receipt.backup_path).unwrap();
    assert_eq!(kb_core::sibling::volume_epoch(&snap).unwrap(), Some(44));

    // Every OLD column, byte-identical.
    let review = store
        .get_review(1)
        .unwrap()
        .expect("the legacy review must still read back");
    assert_eq!(review.repo, "acme");
    assert_eq!(review.title.as_deref(), Some("legacy review"));
    assert_eq!(review.base_ref, "cc6561169ccb");
    assert_eq!(review.head_ref, "HEAD");
    assert_eq!(review.state, "open");
    assert_eq!(review.created_at, 1000);
    assert_eq!(review.updated_at, 1000);
    assert_eq!(review.verdict, None);

    let ps = store
        .get_patchset(1, 1)
        .unwrap()
        .expect("the legacy patchset must still read back");
    assert_eq!(ps.tip_sha, "deadbeef");
    assert_eq!(ps.base_sha, "cc6561169ccb");

    // New columns land at their honest legacy defaults, never a guess.
    let base = store.get_review_base(1).unwrap().unwrap();
    assert_eq!(base.base_mode, None);
    assert_eq!(base.base_branch, None);
    assert_eq!(base.base_member, None);
    assert_eq!(base.base_set_by, "legacy");
    assert_eq!(base.base_status, None);
    assert_eq!(base.objects_state, None);

    let ps_base = store.get_patchset_base(1, 1).unwrap().unwrap();
    assert_eq!(ps_base.base_tip_sha, None);
    assert_eq!(ps_base.kind, None);

    // And the brand-new tables are there, empty.
    assert!(store.list_review_stores().unwrap().is_empty());

    drop(store);
}

/// BUILDER-RULES §"Done when": an old binary refuses a V0045-migrated
/// volume — the SAME `kb_core::sibling::refuse_if_volume_ahead` guard
/// every prior epoch bump has relied on (kb invariant #2), now covering
/// V0045 automatically because `store::schema_epoch()` reads the highest
/// EMBEDDED migration version. No separate constant needed changing for
/// this to hold.
#[test]
fn a_binary_that_only_knows_v0044_refuses_a_v0045_migrated_volume() {
    let (_tmp, store) = open_temp();
    assert!(
        schema_epoch() >= 45,
        "this binary embeds V0045 — if this ever fails, V0045 was renumbered"
    );
    assert_eq!(
        kb_core::sibling::volume_epoch(&store.lock()).unwrap(),
        Some(schema_epoch()),
        "opening the store migrated the volume to this binary's own epoch"
    );

    let pre_v0045 = 44;
    let err = kb_core::sibling::refuse_if_volume_ahead(
        &store.lock(),
        &_tmp.path().join("index.db"),
        pre_v0045,
    )
    .expect_err("a binary that has never heard of V0045 must refuse this volume");
    let msg = err.to_string();
    assert!(msg.contains("refusing to boot"), "{msg}");
    assert!(msg.contains(&format!("V{pre_v0045}")), "{msg}");
}
