use super::*;

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
