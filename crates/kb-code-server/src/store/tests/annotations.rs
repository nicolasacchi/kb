use super::*;

// --- annotations (W4.6) -------------------------------------------------

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
