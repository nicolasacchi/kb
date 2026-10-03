use super::*;

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
