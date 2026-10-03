use super::*;

// --- doc-lens (DCB W1.C) ---------------------------------------------

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
