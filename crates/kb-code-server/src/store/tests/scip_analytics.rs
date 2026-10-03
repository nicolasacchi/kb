use super::*;

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
