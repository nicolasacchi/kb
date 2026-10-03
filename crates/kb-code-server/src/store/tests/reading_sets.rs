use super::*;

// --- reading sets (Phase E3) --------------------------------------------

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
