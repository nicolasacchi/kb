use super::*;

// --- transcripts (W2.5) -------------------------------------------------

#[test]
fn transcript_file_state_round_trips_and_updates_in_place() {
    let (_tmp, store) = open_temp();
    assert!(store.get_transcript_file("proj/a.jsonl").unwrap().is_none());

    let id1 = store
        .upsert_transcript_file_state("proj", "proj/a.jsonl", 111, 0, 1_000)
        .unwrap();
    let row = store.get_transcript_file("proj/a.jsonl").unwrap().unwrap();
    assert_eq!(row.id, id1);
    assert_eq!(row.inode, 111);
    assert_eq!(row.byte_offset, 0);

    // Same src_file → same row, updated in place (not a new row).
    let id2 = store
        .upsert_transcript_file_state("proj", "proj/a.jsonl", 111, 500, 2_000)
        .unwrap();
    assert_eq!(id1, id2);
    let row2 = store.get_transcript_file("proj/a.jsonl").unwrap().unwrap();
    assert_eq!(row2.byte_offset, 500);
    assert_eq!(row2.mtime, 2_000);
}

#[test]
fn insert_transcript_turns_makes_them_searchable_newest_first() {
    let (_tmp, store) = open_temp();
    let file_id = store
        .upsert_transcript_file_state("proj", "proj/a.jsonl", 1, 0, 0)
        .unwrap();
    let turns = vec![
        sample_turn("u1", "s1", "user", 1_000, "an unusual error string alpha"),
        sample_turn(
            "u2",
            "s1",
            "assistant",
            2_000,
            "an unusual error string beta",
        ),
    ];
    store.insert_transcript_turns(file_id, &turns).unwrap();

    let hits = store.search_transcripts("unusual", 10, None, None).unwrap();
    assert_eq!(hits.len(), 2);
    // Newest first: ts=2_000 (uuid u2) before ts=1_000 (uuid u1).
    assert_eq!(hits[0].uuid, "u2");
    assert_eq!(hits[1].uuid, "u1");
    assert_eq!(hits[0].project_dir, "proj");
    assert_eq!(hits[0].src_file, "proj/a.jsonl");
}

#[test]
fn search_transcripts_session_and_kind_filters_narrow_results() {
    let (_tmp, store) = open_temp();
    let file_id = store
        .upsert_transcript_file_state("proj", "proj/a.jsonl", 1, 0, 0)
        .unwrap();
    let turns = vec![
        sample_turn("u1", "s1", "user", 1_000, "widget lookup"),
        sample_turn("u2", "s2", "user", 2_000, "widget lookup"),
        sample_turn("u3", "s1", "assistant", 3_000, "widget lookup"),
    ];
    store.insert_transcript_turns(file_id, &turns).unwrap();

    let by_session = store
        .search_transcripts("widget", 10, Some("s1"), None)
        .unwrap();
    assert_eq!(by_session.len(), 2);
    assert!(by_session.iter().all(|h| h.session_id == "s1"));

    let by_kind = store
        .search_transcripts("widget", 10, None, Some("assistant"))
        .unwrap();
    assert_eq!(by_kind.len(), 1);
    assert_eq!(by_kind[0].uuid, "u3");

    let both = store
        .search_transcripts("widget", 10, Some("s1"), Some("user"))
        .unwrap();
    assert_eq!(both.len(), 1);
    assert_eq!(both[0].uuid, "u1");
}

#[test]
fn transcript_turns_for_session_returns_every_turn_oldest_first_with_file_paths() {
    let (_tmp, store) = open_temp();
    let file_id = store
        .upsert_transcript_file_state("proj", "proj/a.jsonl", 1, 0, 0)
        .unwrap();
    let turns = vec![
        sample_turn("u1", "s1", "user", 3_000, "third"),
        sample_tool_use_turn("u2", "s1", 1_000, "Edit", &["/repo/a.rs"]),
        sample_turn("u3", "s2", "user", 500, "other session"),
        sample_turn("u4", "s1", "assistant", 2_000, "second"),
    ];
    store.insert_transcript_turns(file_id, &turns).unwrap();

    let rows = store.transcript_turns_for_session("s1").unwrap();
    assert_eq!(rows.len(), 3, "only s1's turns, s2's u3 excluded");
    // Oldest first (ts ASC) — the session's own narrative order, the
    // OPPOSITE of search_transcripts' newest-first convention.
    assert_eq!(rows[0].uuid, "u2");
    assert_eq!(rows[0].tool_name.as_deref(), Some("Edit"));
    assert_eq!(rows[0].file_paths, vec!["/repo/a.rs".to_string()]);
    assert_eq!(rows[1].uuid, "u4");
    assert_eq!(rows[2].uuid, "u1");

    assert!(store
        .transcript_turns_for_session("unknown-session")
        .unwrap()
        .is_empty());
}

#[test]
fn delete_transcript_turns_for_file_removes_fts_rows_too() {
    let (_tmp, store) = open_temp();
    let file_id = store
        .upsert_transcript_file_state("proj", "proj/a.jsonl", 1, 0, 0)
        .unwrap();
    store
        .insert_transcript_turns(file_id, &[sample_turn("u1", "s1", "user", 1_000, "gizmo")])
        .unwrap();
    assert_eq!(
        store
            .search_transcripts("gizmo", 10, None, None)
            .unwrap()
            .len(),
        1
    );

    store.delete_transcript_turns_for_file(file_id).unwrap();
    assert!(store
        .search_transcripts("gizmo", 10, None, None)
        .unwrap()
        .is_empty());
}

#[test]
fn transcript_stats_counts_files_turns_and_bytes() {
    let (_tmp, store) = open_temp();
    assert_eq!(
        store.transcript_stats().unwrap(),
        TranscriptStats::default()
    );

    let file_a = store
        .upsert_transcript_file_state("proj", "proj/a.jsonl", 1, 0, 0)
        .unwrap();
    let file_b = store
        .upsert_transcript_file_state("proj", "proj/b.jsonl", 2, 0, 0)
        .unwrap();
    store
        .insert_transcript_turns(file_a, &[sample_turn("u1", "s1", "user", 1_000, "12345")])
        .unwrap();
    store
        .insert_transcript_turns(
            file_b,
            &[
                sample_turn("u2", "s1", "user", 2_000, "1234567890"),
                sample_turn("u3", "s1", "assistant", 3_000, "12"),
            ],
        )
        .unwrap();

    let stats = store.transcript_stats().unwrap();
    assert_eq!(stats.files, 2);
    assert_eq!(stats.turns, 3);
    assert_eq!(stats.indexed_bytes, 5 + 10 + 2);
}

#[test]
fn transcript_sessions_touching_path_finds_exact_matches_newest_first() {
    let (_tmp, store) = open_temp();
    let file_id = store
        .upsert_transcript_file_state("proj", "proj/a.jsonl", 1, 0, 0)
        .unwrap();
    store
        .insert_transcript_turns(
            file_id,
            &[
                sample_turn_with_paths(
                    "u1",
                    "s-older",
                    1_000,
                    vec!["/repo/src/lib.rs".to_string()],
                ),
                sample_turn_with_paths(
                    "u2",
                    "s-newer",
                    2_000,
                    vec![
                        "/repo/src/lib.rs".to_string(),
                        "/repo/README.md".to_string(),
                    ],
                ),
                // A DIFFERENT, longer path that merely ENDS in the same
                // basename must never match — proves the quote-delimited
                // needle isn't a bare substring scan.
                sample_turn_with_paths(
                    "u3",
                    "s-unrelated",
                    3_000,
                    vec!["/repo/src/other_lib.rs".to_string()],
                ),
            ],
        )
        .unwrap();

    let hits = store
        .transcript_sessions_touching_path("/repo/src/lib.rs", 10)
        .unwrap();
    let sessions: Vec<&str> = hits.iter().map(|h| h.session_id.as_str()).collect();
    assert_eq!(
        sessions,
        vec!["s-newer", "s-older"],
        "newest-first, and the unrelated longer path must not match: {hits:?}"
    );
}

#[test]
fn transcript_sessions_touching_path_is_bounded_by_limit() {
    let (_tmp, store) = open_temp();
    let file_id = store
        .upsert_transcript_file_state("proj", "proj/a.jsonl", 1, 0, 0)
        .unwrap();
    let turns: Vec<IndexedTurn> = (0..5)
        .map(|i| {
            sample_turn_with_paths(
                &format!("u{i}"),
                &format!("s{i}"),
                1_000 + i,
                vec!["/repo/hot_file.rs".to_string()],
            )
        })
        .collect();
    store.insert_transcript_turns(file_id, &turns).unwrap();

    let hits = store
        .transcript_sessions_touching_path("/repo/hot_file.rs", 2)
        .unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].session_id, "s4", "newest first");
}

#[test]
fn transcript_sessions_touching_path_no_match_is_an_honest_empty_vec() {
    let (_tmp, store) = open_temp();
    let file_id = store
        .upsert_transcript_file_state("proj", "proj/a.jsonl", 1, 0, 0)
        .unwrap();
    store
        .insert_transcript_turns(
            file_id,
            &[sample_turn_with_paths(
                "u1",
                "s1",
                1_000,
                vec!["/repo/other.rs".to_string()],
            )],
        )
        .unwrap();

    let hits = store
        .transcript_sessions_touching_path("/repo/nothing_here.rs", 10)
        .unwrap();
    assert!(hits.is_empty());
}

// --- commit_sessions (W3.2 join ladder cache) --------------------------

#[test]
fn commit_session_round_trips_and_misses_cleanly() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    assert_eq!(store.get_commit_session(repo_id, "deadbeef").unwrap(), None);

    let row = sample_commit_session("exact", "by-commit");
    store
        .upsert_commit_session(repo_id, "deadbeef", &row)
        .unwrap();
    let got = store.get_commit_session(repo_id, "deadbeef").unwrap();
    assert_eq!(got, Some(row));

    // A different sha in the same repo is a separate cache slot.
    assert_eq!(store.get_commit_session(repo_id, "other").unwrap(), None);
}

#[test]
fn commit_session_upsert_replaces_every_column_in_place() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store
        .upsert_commit_session(repo_id, "sha1", &sample_commit_session("none", "no-match"))
        .unwrap();
    assert_eq!(
        store
            .get_commit_session(repo_id, "sha1")
            .unwrap()
            .unwrap()
            .confidence,
        "none"
    );

    // A later re-resolution upgrades none -> fuzzy, replacing the row
    // wholesale (not merging fields).
    let upgraded = CommitSessionRow {
        confidence: "fuzzy".to_string(),
        via: "time-window".to_string(),
        session_id: Some("sess-2".to_string()),
        kb: Some("memory".to_string()),
        display_name: None,
        started_at: Some(1_700_050_000),
        resolved_at: 1_700_060_000,
    };
    store
        .upsert_commit_session(repo_id, "sha1", &upgraded)
        .unwrap();
    assert_eq!(
        store.get_commit_session(repo_id, "sha1").unwrap(),
        Some(upgraded)
    );
}

#[test]
fn commit_session_is_scoped_per_repo() {
    let (_tmp, store) = open_temp();
    let repo_a = store.upsert_repo("a", "/tmp/a").unwrap();
    let repo_b = store.upsert_repo("b", "/tmp/b").unwrap();
    store
        .upsert_commit_session(repo_a, "sha1", &sample_commit_session("exact", "by-commit"))
        .unwrap();
    assert!(store.get_commit_session(repo_a, "sha1").unwrap().is_some());
    assert_eq!(store.get_commit_session(repo_b, "sha1").unwrap(), None);
}

#[test]
fn commit_session_none_row_carries_no_enrichment() {
    let (_tmp, store) = open_temp();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let none_row = CommitSessionRow {
        confidence: "none".to_string(),
        via: "kb-unreachable".to_string(),
        session_id: None,
        kb: None,
        display_name: None,
        started_at: None,
        resolved_at: 1_700_000_000,
    };
    store
        .upsert_commit_session(repo_id, "sha1", &none_row)
        .unwrap();
    let got = store.get_commit_session(repo_id, "sha1").unwrap().unwrap();
    assert_eq!(got.session_id, None);
    assert_eq!(got.kb, None);
    assert_eq!(got.display_name, None);
    assert_eq!(got.started_at, None);
}
