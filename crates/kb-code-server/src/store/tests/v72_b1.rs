//! V72-B1 — the migration-checksum repair (`repair_v3_transcripts_checksum`)
//! and its own regression golden (`migrations.checksums.json`). See the
//! constants next to `repair_v3_transcripts_checksum` for the full
//! defect story.
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
    include_str!("../../../tests/fixtures/migrations.checksums.json");

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
