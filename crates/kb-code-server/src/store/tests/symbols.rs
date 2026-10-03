use super::*;
use crate::highlight::HighlightClass;

#[test]
fn symbols_for_repo_joins_files_and_symbols_scoped_per_repo() {
    let (_tmp, store) = open_temp();
    let repo_a = store.upsert_repo("a", "/tmp/a").unwrap();
    let repo_b = store.upsert_repo("b", "/tmp/b").unwrap();

    store
        .upsert_file(repo_a, "lib.rs", "hashA", "rust", 10)
        .unwrap();
    store
        .upsert_file(repo_a, "copy.rs", "hashA", "rust", 10)
        .unwrap(); // same content, second path
    store
        .replace_symbols("hashA", "rust@1", &[sample_symbol(0, "alpha")])
        .unwrap();

    store
        .upsert_file(repo_b, "other.rs", "hashB", "rust", 5)
        .unwrap();
    store
        .replace_symbols("hashB", "rust@1", &[sample_symbol(0, "beta")])
        .unwrap();

    let rows = store.symbols_for_repo(repo_a).unwrap();
    let mut got: Vec<(String, String)> = rows
        .iter()
        .map(|(path, sym)| (path.clone(), sym.name.clone()))
        .collect();
    got.sort();
    // "alpha" appears twice: once per path pointing at the shared blob
    // (unlike symbol_count_for_repo, a listing needs a path per hit).
    assert_eq!(
        got,
        vec![
            ("copy.rs".to_string(), "alpha".to_string()),
            ("lib.rs".to_string(), "alpha".to_string()),
        ]
    );
    assert!(store
        .symbols_for_repo(repo_b)
        .unwrap()
        .iter()
        .all(|(_, s)| s.name == "beta"));
}

#[test]
fn symbols_for_repo_hides_a_stale_salt_row_once_a_current_salt_sibling_exists() {
    // V70-A3X — the actual production bug: a blob re-derived under a
    // NEW salt (grammar/query bump) used to leave the OLD salt's rows
    // visible ALONGSIDE the new ones in this un-salted join, doubling
    // every symbol. `lang::RUST.symbol_salt` is the one genuinely "current"
    // salt `current_salt_cte` knows about; a fake old salt stands in
    // for a pre-bump derivation.
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .upsert_file(repo_id, "lib.rs", "hashA", "rust", 10)
        .unwrap();

    // A fresh derivation lands under the REAL current salt.
    store
        .replace_symbols(
            "hashA",
            crate::lang::RUST.symbol_salt,
            &[sample_symbol(0, "new_name")],
        )
        .unwrap();

    // Simulate the OLD grammar's rows STILL sitting in the table (raw
    // INSERT — bypassing `replace_symbols`'s own write-side purge,
    // as if written by a pre-fix binary and never swept) — this
    // isolates the READ-side filter: `symbols_for_repo` must hide it
    // even though nothing purged it at write time.
    store
        .lock()
        .execute(
            "INSERT INTO symbols (blob_hash, salt, ordinal, name, kind, line_start, \
             line_end, col_start, col_end) VALUES (?1, 'rust@stale-fake', 0, 'old_name', \
             'fn', 1, 1, 0, 1)",
            params!["hashA"],
        )
        .unwrap();

    let rows = store.symbols_for_repo(repo_id).unwrap();
    let names: Vec<&str> = rows.iter().map(|(_, s)| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["new_name"],
        "the stale-salt row must be hidden once a current-salt sibling exists: {names:?}"
    );
}

#[test]
fn symbols_for_repo_still_shows_fixture_only_salts_with_no_current_sibling() {
    // V70-A3X fallback: a repo whose ONLY rows are under a non-current
    // (ad hoc test / not-yet-recognised) salt must still show them —
    // this is what keeps the REST of this crate's "rust@1"-style
    // fixtures byte-identical (see `current_salt_cte`'s doc).
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .upsert_file(repo_id, "lib.rs", "hashA", "rust", 10)
        .unwrap();
    store
        .replace_symbols(
            "hashA",
            "rust@ad-hoc-fixture-salt",
            &[sample_symbol(0, "x")],
        )
        .unwrap();

    let rows = store.symbols_for_repo(repo_id).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1.name, "x");
}

#[test]
fn replace_symbols_purges_only_this_blobs_stale_same_language_rows() {
    // A degenerate/empty file can share ONE blob_hash across DIFFERENT
    // languages (different extensions detecting to different
    // `LangInfo`s over identical bytes) — the purge must be scoped to
    // the SAME language as the incoming salt, never touching a
    // sibling language's CURRENT rows for that same blob_hash.
    let (_tmp, store) = open_temp();
    store
        .replace_symbols(
            "sharedBlob",
            crate::lang::PYTHON.symbol_salt,
            &[sample_symbol(0, "py_fn")],
        )
        .unwrap();
    // An OLD rust salt for the SAME blob (simulating a pre-bump rust
    // derivation that happens to share this content).
    store
        .replace_symbols(
            "sharedBlob",
            "rust@old-fake",
            &[sample_symbol(0, "old_rust_fn")],
        )
        .unwrap();

    // Re-derive rust under its CURRENT salt — must purge the old rust
    // row but leave python's untouched.
    store
        .replace_symbols(
            "sharedBlob",
            crate::lang::RUST.symbol_salt,
            &[sample_symbol(0, "new_rust_fn")],
        )
        .unwrap();

    let rust_rows = store
        .symbols_for_blob("sharedBlob", "rust@old-fake")
        .unwrap();
    assert!(rust_rows.is_empty(), "old rust salt must be purged");
    let new_rust = store
        .symbols_for_blob("sharedBlob", crate::lang::RUST.symbol_salt)
        .unwrap();
    assert_eq!(new_rust.len(), 1);
    assert_eq!(new_rust[0].name, "new_rust_fn");
    let py_rows = store
        .symbols_for_blob("sharedBlob", crate::lang::PYTHON.symbol_salt)
        .unwrap();
    assert_eq!(
        py_rows.len(),
        1,
        "a DIFFERENT language's rows for the same blob_hash must survive"
    );
    assert_eq!(py_rows[0].name, "py_fn");
}

#[test]
fn sweep_stale_salt_derived_prunes_only_genuinely_superseded_rows() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .upsert_file(repo_id, "a.rs", "hashA", "rust", 10)
        .unwrap();
    store
        .replace_symbols(
            "hashA",
            crate::lang::RUST.symbol_salt,
            &[sample_symbol(0, "cur")],
        )
        .unwrap();
    // "hashA" ALSO carries a genuinely stale row — direct INSERT
    // (bypassing `replace_symbols`'s own write-side purge), simulating
    // leftover data from BEFORE this fix shipped, which is exactly what
    // the boot-time sweep exists to catch (the write-side purge alone
    // can't clean up rows a pre-fix binary already wrote).
    store
        .lock()
        .execute(
            "INSERT INTO symbols (blob_hash, salt, ordinal, name, kind, line_start, \
             line_end, col_start, col_end) VALUES ('hashA', 'rust@stale-fake', 0, 'old', \
             'fn', 1, 1, 0, 1)",
            [],
        )
        .unwrap();

    // Untouched repo: a fixture-only blob with NO current-salt sibling
    // at all — the sweep must leave it alone entirely.
    let repo_b = store.upsert_repo("b", "/tmp/b").unwrap();
    store
        .upsert_file(repo_b, "b.rs", "hashB", "rust", 5)
        .unwrap();
    store
        .replace_symbols("hashB", "rust@fixture-only", &[sample_symbol(0, "fixture")])
        .unwrap();

    let counts = store.sweep_stale_salt_derived().unwrap();
    assert_eq!(counts.symbols, 1, "exactly the one genuinely-stale row");
    assert_eq!(counts.highlights, 0);
    assert_eq!(counts.occurrences, 0);

    assert!(store
        .symbols_for_blob("hashA", "rust@stale-fake")
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .symbols_for_blob("hashB", "rust@fixture-only")
            .unwrap()
            .len(),
        1,
        "a fixture-only blob with no current sibling must survive the sweep"
    );
}

/// V72-B0 — the property the boot fix rests on: one page touches ONLY
/// its own slice of `files.blob_hash`, the cursor advances, the walk
/// terminates, and the union over pages equals the un-paged result.
/// A page that silently swept the whole table would put the hours-long
/// transaction straight back onto the store's write mutex.
#[test]
fn sweep_stale_salt_page_is_bounded_resumable_and_totals_to_a_full_sweep() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    // Six blobs, each with one current-salt row and one genuinely stale
    // sibling — deliberately more than the page size used below.
    let hashes: Vec<String> = (0..6).map(|i| format!("hash{i}")).collect();
    for (i, h) in hashes.iter().enumerate() {
        store
            .upsert_file(repo_id, &format!("f{i}.rs"), h, "rust", 10)
            .unwrap();
        store
            .replace_symbols(h, crate::lang::RUST.symbol_salt, &[sample_symbol(0, "cur")])
            .unwrap();
        store
            .lock()
            .execute(
                "INSERT INTO symbols (blob_hash, salt, ordinal, name, kind, line_start, \
                 line_end, col_start, col_end) VALUES (?1, 'rust@stale-fake', 0, 'old', \
                 'fn', 1, 1, 0, 1)",
                params![h],
            )
            .unwrap();
    }

    // Page size 2 over 6 blobs: three full pages, then a short/empty one.
    let mut cursor: Option<String> = None;
    let mut swept = 0u64;
    let mut seen_cursors: Vec<String> = Vec::new();
    let mut pages = 0;
    loop {
        let (counts, next) = store.sweep_stale_salt_page(cursor.as_deref(), 2).unwrap();
        pages += 1;
        assert!(
            counts.symbols <= 2,
            "a page of 2 blobs can never delete more than 2 stale symbol rows, got {}",
            counts.symbols
        );
        swept += counts.symbols;
        match next {
            Some(c) => {
                if let Some(prev) = seen_cursors.last() {
                    assert!(&c > prev, "the cursor must advance strictly: {prev} -> {c}");
                }
                seen_cursors.push(c.clone());
                cursor = Some(c);
            }
            None => break,
        }
        assert!(pages < 20, "the paged sweep must terminate");
    }
    assert_eq!(swept, 6, "every blob's one stale row, exactly once");
    assert!(pages >= 3, "6 blobs at 2 per page must take >= 3 pages");

    for h in &hashes {
        assert!(
            store
                .symbols_for_blob(h, "rust@stale-fake")
                .unwrap()
                .is_empty(),
            "{h}'s stale row must be gone"
        );
        assert_eq!(
            store
                .symbols_for_blob(h, crate::lang::RUST.symbol_salt)
                .unwrap()
                .len(),
            1,
            "{h}'s current-salt row must survive"
        );
    }

    // Idempotent: a second full walk finds nothing left to do.
    assert_eq!(store.sweep_stale_salt_derived().unwrap().total(), 0);
}

// ── V72-H2b — the sweep understands BOTH salt families ───────────────

/// A family is stale only when ITS OWN salt moved. The sweep runs one
/// pass per (table, family) pair, so a highlight row keyed by the
/// current HIGHLIGHT salt must survive even though that string is not
/// in the symbol set at all — the bug a single shared `cur` set would
/// have introduced the moment the two salts diverged.
#[test]
fn the_sweep_keeps_each_familys_current_rows_and_prunes_only_its_own_stale_ones() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .upsert_file(repo_id, "a.rs", "hashA", "rust", 10)
        .unwrap();
    // Current rows for both families, written the production way.
    store
        .replace_symbols(
            "hashA",
            crate::lang::RUST.symbol_salt,
            &[sample_symbol(0, "cur")],
        )
        .unwrap();
    store
        .put_highlights(
            "hashA",
            crate::lang::RUST.highlight_salt,
            &[crate::highlight::Span {
                byte_start: 0,
                byte_len: 2,
                class: crate::highlight::HighlightClass::Keyword,
            }],
        )
        .unwrap();
    // A PRE-SPLIT highlight row: painted under the SYMBOL salt, which
    // is exactly what every mirror on disk carries at the moment this
    // unit deploys. Direct INSERT, bypassing the write-side purge, the
    // same way the V70-A3X test simulates old damage.
    store
        .lock()
        .execute(
            "INSERT INTO highlights (blob_hash, salt, spans) VALUES ('hashA', ?1, X'5B5D')",
            params![crate::lang::RUST.symbol_salt],
        )
        .unwrap();

    let counts = store.sweep_stale_salt_derived().unwrap();
    assert_eq!(
        counts.highlights, 1,
        "the pre-split highlight row is stale FOR ITS FAMILY and goes"
    );
    assert_eq!(counts.symbols, 0, "no symbol row was ever stale here");
    assert_eq!(counts.derived_status, 0);

    // The survivors, by family.
    assert_eq!(
        store
            .highlights_for_blob("hashA", crate::lang::RUST.highlight_salt)
            .unwrap()
            .map(|v| v.len()),
        Some(1),
        "the CURRENT highlight salt's row must survive its own sweep"
    );
    assert_eq!(
        store
            .symbols_for_blob("hashA", crate::lang::RUST.symbol_salt)
            .unwrap()
            .len(),
        1
    );
    assert!(store
        .is_derived(
            "hashA",
            crate::lang::SaltFamily::Symbol,
            crate::lang::RUST.symbol_salt
        )
        .unwrap());
    assert!(store
        .is_derived(
            "hashA",
            crate::lang::SaltFamily::Highlight,
            crate::lang::RUST.highlight_salt
        )
        .unwrap());
}

/// The marker table's own two rules: existence is the gate (`Some(0)`
/// is a real answer), and a write purges only the SAME family's other
/// salts for that blob.
#[test]
fn the_derivation_marker_is_per_family_and_records_a_zero_row_derivation() {
    let (_tmp, store) = open_temp();
    store.replace_symbols("blobZ", "rust@old+q1", &[]).unwrap();
    assert!(store
        .is_derived("blobZ", crate::lang::SaltFamily::Symbol, "rust@old+q1")
        .unwrap());
    assert_eq!(
        store
            .derived_rows("blobZ", crate::lang::SaltFamily::Symbol, "rust@old+q1")
            .unwrap(),
        Some(0),
        "an empty derivation is still a derivation"
    );
    assert!(
        !store.has_symbols("blobZ", "rust@old+q1").unwrap(),
        "and the row-count question still honestly answers no"
    );

    // The HIGHLIGHT family is untouched by a symbol-family write ...
    store
        .put_highlights("blobZ", "rust@old+h1+roles2", &[])
        .unwrap();
    assert!(store
        .is_derived(
            "blobZ",
            crate::lang::SaltFamily::Highlight,
            "rust@old+h1+roles2"
        )
        .unwrap());

    // ... and a symbol-salt bump purges the previous SYMBOL marker
    // without touching the highlight one.
    store.replace_symbols("blobZ", "rust@new+q2", &[]).unwrap();
    assert!(!store
        .is_derived("blobZ", crate::lang::SaltFamily::Symbol, "rust@old+q1")
        .unwrap());
    assert!(store
        .is_derived("blobZ", crate::lang::SaltFamily::Symbol, "rust@new+q2")
        .unwrap());
    assert!(
        store
            .is_derived(
                "blobZ",
                crate::lang::SaltFamily::Highlight,
                "rust@old+h1+roles2"
            )
            .unwrap(),
        "a symbol-salt bump must never erase the highlight family's marker"
    );

    // A DIFFERENT language's marker for the same blob (degenerate
    // content shared across extensions) survives too — the purge is
    // language-prefixed, `lang_prefix_pattern`'s own rule.
    store.replace_symbols("blobZ", "python@x+q1", &[]).unwrap();
    store
        .replace_symbols("blobZ", "rust@newer+q3", &[])
        .unwrap();
    assert!(store
        .is_derived("blobZ", crate::lang::SaltFamily::Symbol, "python@x+q1")
        .unwrap());
}

#[test]
fn symbols_cache_hit_and_replace_round_trip() {
    let (_tmp, store) = open_temp();
    assert!(!store.has_symbols("blobA", "rust@1").unwrap());

    let syms = vec![sample_symbol(0, "foo"), sample_symbol(1, "bar")];
    store.replace_symbols("blobA", "rust@1", &syms).unwrap();
    assert!(store.has_symbols("blobA", "rust@1").unwrap());

    let got = store.symbols_for_blob("blobA", "rust@1").unwrap();
    assert_eq!(got, syms);

    // Different salt is a separate cache slot entirely.
    assert!(!store.has_symbols("blobA", "rust@2").unwrap());

    // Replacing overwrites, not appends.
    let syms2 = vec![sample_symbol(0, "baz")];
    store.replace_symbols("blobA", "rust@1", &syms2).unwrap();
    assert_eq!(store.symbols_for_blob("blobA", "rust@1").unwrap(), syms2);
}

#[test]
fn highlights_round_trip_json_blob() {
    let (_tmp, store) = open_temp();
    assert!(!store.has_highlights("blobA", "rust@1").unwrap());
    assert_eq!(store.highlights_for_blob("blobA", "rust@1").unwrap(), None);

    let spans = vec![
        Span {
            byte_start: 0,
            byte_len: 3,
            class: HighlightClass::Keyword,
        },
        Span {
            byte_start: 4,
            byte_len: 2,
            class: HighlightClass::Variable,
        },
    ];
    store.put_highlights("blobA", "rust@1", &spans).unwrap();
    assert!(store.has_highlights("blobA", "rust@1").unwrap());
    assert_eq!(
        store.highlights_for_blob("blobA", "rust@1").unwrap(),
        Some(spans)
    );
}

// --- occurrences (B2) ---------------------------------------------------

#[test]
fn occurrences_insert_has_and_lookup_round_trip() {
    let (_tmp, store) = open_temp();
    assert!(!store.has_occurrences("blobA", "rust@1").unwrap());
    assert_eq!(
        store.occurrences_for_blob("blobA", "rust@1").unwrap(),
        vec![]
    );

    let occs = vec![
        occ(0, "foo", "def", 1),
        occ(1, "foo", "ref", 2),
        occ(2, "bar", "def", 3),
    ];
    store.replace_occurrences("blobA", "rust@1", &occs).unwrap();
    assert!(store.has_occurrences("blobA", "rust@1").unwrap());
    assert_eq!(store.occurrences_for_blob("blobA", "rust@1").unwrap(), occs);

    // Different salt is a separate cache slot, same convention as symbols.
    assert!(!store.has_occurrences("blobA", "rust@2").unwrap());

    // Replacing overwrites, not appends.
    let occs2 = vec![occ(0, "baz", "ref", 1)];
    store
        .replace_occurrences("blobA", "rust@1", &occs2)
        .unwrap();
    assert_eq!(
        store.occurrences_for_blob("blobA", "rust@1").unwrap(),
        occs2
    );
}

#[test]
fn def_occurrences_by_name_filters_to_the_def_role_only() {
    let (_tmp, store) = open_temp();
    store
        .replace_occurrences(
            "blobA",
            "rust@1",
            &[
                occ(0, "run", "def", 1),
                occ(1, "run", "ref", 5),
                occ(2, "run", "import", 8),
            ],
        )
        .unwrap();
    let defs = store
        .def_occurrences_by_name("blobA", "rust@1", "run")
        .unwrap();
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].role, "def");
    assert_eq!(defs[0].line, 1);

    assert!(store
        .def_occurrences_by_name("blobA", "rust@1", "zzz-nope")
        .unwrap()
        .is_empty());
}

#[test]
fn occurrence_at_finds_the_covering_span_only() {
    let (_tmp, store) = open_temp();
    store
        .replace_occurrences(
            "blobA",
            "rust@1",
            &[crate::occurrences::Occurrence {
                ordinal: 0,
                name: "widget".to_string(),
                role: "def".to_string(),
                line: 4,
                col_start: 3,
                col_end: 9,
                source: crate::occurrences::SOURCE_TS.to_string(),
                local_def_ordinal: None,
            }],
        )
        .unwrap();

    // Inside the span.
    let hit = store.occurrence_at("blobA", "rust@1", 4, 5).unwrap();
    assert_eq!(hit.map(|o| o.name), Some("widget".to_string()));

    // Exactly at col_start (inclusive).
    assert!(store
        .occurrence_at("blobA", "rust@1", 4, 3)
        .unwrap()
        .is_some());
    // Exactly at col_end (exclusive) — must miss.
    assert!(store
        .occurrence_at("blobA", "rust@1", 4, 9)
        .unwrap()
        .is_none());
    // Wrong line — must miss.
    assert!(store
        .occurrence_at("blobA", "rust@1", 5, 5)
        .unwrap()
        .is_none());
}

// --- occurrences (scip-sourced, S1) -------------------------------------

#[test]
fn replace_scip_occurrences_is_isolated_from_the_ts_source_and_continues_the_ordinal_space() {
    let (_tmp, store) = open_temp();
    store
        .replace_occurrences(
            "blobA",
            "rust@1",
            &[occ(0, "widget", "def", 1), occ(1, "widget", "ref", 2)],
        )
        .unwrap();

    store
        .replace_scip_occurrences(
            "blobA",
            "rust@1",
            &[crate::store::ScipOccurrenceIn {
                name: "widget".to_string(),
                role: "def".to_string(),
                line: 1,
                col_start: 3,
                col_end: 9,
            }],
        )
        .unwrap();

    let all = store.occurrences_for_blob("blobA", "rust@1").unwrap();
    assert_eq!(all.len(), 3, "got: {all:#?}");
    // The scip row's ordinal continues AFTER the two ts ordinals (0, 1)
    // — no primary-key collision.
    let scip_row = all.iter().find(|o| o.source == "scip").expect("a scip row");
    assert_eq!(scip_row.ordinal, 2);

    // A re-derive of the TS pass (`replace_occurrences`) must NOT touch
    // the scip row.
    store
        .replace_occurrences("blobA", "rust@1", &[occ(0, "widget", "def", 1)])
        .unwrap();
    let after = store.occurrences_for_blob("blobA", "rust@1").unwrap();
    assert_eq!(after.len(), 2, "got: {after:#?}"); // 1 ts + 1 scip
    assert!(after
        .iter()
        .any(|o| o.source == "scip" && o.name == "widget"));

    // Re-ingesting scip occurrences REPLACES the prior scip set, still
    // never touching the ts row.
    store
        .replace_scip_occurrences(
            "blobA",
            "rust@1",
            &[crate::store::ScipOccurrenceIn {
                name: "renamed".to_string(),
                role: "def".to_string(),
                line: 1,
                col_start: 3,
                col_end: 10,
            }],
        )
        .unwrap();
    let final_rows = store.occurrences_for_blob("blobA", "rust@1").unwrap();
    assert_eq!(final_rows.len(), 2, "got: {final_rows:#?}");
    assert!(final_rows
        .iter()
        .any(|o| o.source == "scip" && o.name == "renamed"));
    assert!(!final_rows
        .iter()
        .any(|o| o.name == "widget" && o.source == "scip"));
}

#[test]
fn def_occurrences_by_name_is_scoped_to_ts_scip_def_occurrences_by_name_to_scip() {
    let (_tmp, store) = open_temp();
    store
        .replace_occurrences("blobA", "rust@1", &[occ(0, "widget", "def", 1)])
        .unwrap();
    store
        .replace_scip_occurrences(
            "blobA",
            "rust@1",
            &[crate::store::ScipOccurrenceIn {
                name: "widget".to_string(),
                role: "def".to_string(),
                line: 5,
                col_start: 0,
                col_end: 6,
            }],
        )
        .unwrap();

    let ts_defs = store
        .def_occurrences_by_name("blobA", "rust@1", "widget")
        .unwrap();
    assert_eq!(ts_defs.len(), 1);
    assert_eq!(ts_defs[0].line, 1);

    let scip_defs = store
        .scip_def_occurrences_by_name("blobA", "rust@1", "widget")
        .unwrap();
    assert_eq!(scip_defs.len(), 1);
    assert_eq!(scip_defs[0].line, 5);
}

#[test]
fn occurrence_at_prefers_the_scip_row_when_both_sources_cover_the_same_span() {
    let (_tmp, store) = open_temp();
    store
        .replace_occurrences(
            "blobA",
            "rust@1",
            &[crate::occurrences::Occurrence {
                ordinal: 0,
                name: "widget".to_string(),
                role: "ref".to_string(),
                line: 4,
                col_start: 3,
                col_end: 9,
                source: crate::occurrences::SOURCE_TS.to_string(),
                local_def_ordinal: None,
            }],
        )
        .unwrap();
    store
        .replace_scip_occurrences(
            "blobA",
            "rust@1",
            &[crate::store::ScipOccurrenceIn {
                name: "widget".to_string(),
                role: "def".to_string(),
                line: 4,
                col_start: 3,
                col_end: 9,
            }],
        )
        .unwrap();

    let hit = store
        .occurrence_at("blobA", "rust@1", 4, 5)
        .unwrap()
        .expect("a hit");
    assert_eq!(hit.source, "scip", "the scip row must win the tie-break");
    assert_eq!(hit.role, "def");
}

/// Migration `V0010__occurrences_source.sql`'s own contract: `ALTER
/// TABLE occurrences ADD COLUMN source TEXT NOT NULL DEFAULT 'ts'`
/// backfills every PRE-EXISTING row (inserted before the column
/// existed) to `'ts'` for free, no separate UPDATE. Exercised here by
/// inserting a row with a raw SQL statement that OMITS the `source`
/// column entirely (the exact shape SQLite's own `ALTER TABLE ADD
/// COLUMN ... DEFAULT` backfill produces for a row that predates the
/// column) — reading it back through the normal `Store` API must see
/// `source == "ts"`.
#[test]
fn v0010_defaults_pre_existing_rows_without_a_source_column_to_ts() {
    let (_tmp, store) = open_temp();
    store
        .lock()
        .execute(
            "INSERT INTO occurrences (blob_hash, salt, ordinal, name, role, line, col_start, col_end)
             VALUES ('blobA', 'rust@1', 0, 'legacy', 'def', 1, 0, 6)",
            [],
        )
        .unwrap();
    let rows = store.occurrences_for_blob("blobA", "rust@1").unwrap();
    assert_eq!(rows.len(), 1, "got: {rows:#?}");
    assert_eq!(rows[0].name, "legacy");
    assert_eq!(rows[0].source, "ts");
}
