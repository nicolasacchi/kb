use super::*;

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
