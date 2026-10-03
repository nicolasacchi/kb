use super::*;

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
