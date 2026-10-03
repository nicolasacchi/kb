use super::*;

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
