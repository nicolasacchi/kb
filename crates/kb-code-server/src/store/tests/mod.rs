use super::*;

mod annotations;
mod doclens;
mod files_rails_entities;
mod findings;
mod generation_chunks;
mod open;
mod reading_sets;
mod review_stores;
mod scip_analytics;
mod symbols;
mod transcripts;
mod v72_b1;

fn open_temp() -> (tempfile::TempDir, Store) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("index.db")).unwrap();
    (tmp, store)
}

fn sample_rails_edge(dst_path: &str, line: u32) -> crate::frameworks::FrameworkEdge {
    crate::frameworks::FrameworkEdge {
        kind: crate::frameworks::EdgeKind::RenderPartial,
        src_path: String::new(), // overwritten by the caller below
        src_line: Some(line),
        src_symbol: None,
        dst_kind: Some("partial".to_string()),
        dst_path: Some(dst_path.to_string()),
        dst_symbol: None,
        trust: crate::frameworks::Trust::Likely,
        extra_json: None,
    }
}

fn entity_claim(fqn: &str, zeitwerk: Option<&str>) -> crate::entities::EntityDefClaim {
    crate::entities::EntityDefClaim {
        fqn: fqn.to_string(),
        kind: "class".to_string(),
        nesting: crate::entities::NESTING_LEXICAL,
        line_start: 1,
        line_end: 9,
        zeitwerk_fqn: zeitwerk.map(|s| s.to_string()),
    }
}

fn sample_symbol(ordinal: u32, name: &str) -> Symbol {
    Symbol {
        ordinal,
        name: name.to_string(),
        kind: "fn".to_string(),
        line_start: 1,
        line_end: 3,
        col_start: 0,
        col_end: 1,
        container: None,
        signature: None,
        doc: None,
        param_min: None,
        param_max: None,
    }
}

fn occ(ordinal: u32, name: &str, role: &str, line: u32) -> crate::occurrences::Occurrence {
    crate::occurrences::Occurrence {
        ordinal,
        name: name.to_string(),
        role: role.to_string(),
        line,
        col_start: 0,
        col_end: 1,
        source: crate::occurrences::SOURCE_TS.to_string(),
        local_def_ordinal: None,
    }
}

fn sample_turn(
    uuid: &str,
    session_id: &str,
    kind: &'static str,
    ts: i64,
    text: &str,
) -> IndexedTurn {
    IndexedTurn {
        turn: crate::transcripts::parse::ParsedTurn {
            session_id: session_id.to_string(),
            uuid: uuid.to_string(),
            parent_uuid: None,
            ts,
            kind,
            tool_name: None,
            file_paths: Vec::new(),
            is_sidechain: false,
            text: text.to_string(),
        },
        byte_offset: 0,
        byte_len: text.len() as i64,
    }
}

/// A `tool_use` turn variant of [`sample_turn`], carrying a `tool_name`
/// and `file_paths` — `sessiondiff`'s own data source, which
/// `search_transcripts`' tests above never need.
fn sample_tool_use_turn(
    uuid: &str,
    session_id: &str,
    ts: i64,
    tool_name: &str,
    file_paths: &[&str],
) -> IndexedTurn {
    IndexedTurn {
        turn: crate::transcripts::parse::ParsedTurn {
            session_id: session_id.to_string(),
            uuid: uuid.to_string(),
            parent_uuid: None,
            ts,
            kind: "tool_use",
            tool_name: Some(tool_name.to_string()),
            file_paths: file_paths.iter().map(|s| s.to_string()).collect(),
            is_sidechain: false,
            text: format!("{tool_name} edit"),
        },
        byte_offset: 0,
        byte_len: 10,
    }
}

fn sample_turn_with_paths(
    uuid: &str,
    session_id: &str,
    ts: i64,
    file_paths: Vec<String>,
) -> IndexedTurn {
    IndexedTurn {
        turn: crate::transcripts::parse::ParsedTurn {
            session_id: session_id.to_string(),
            uuid: uuid.to_string(),
            parent_uuid: None,
            ts,
            kind: crate::transcripts::parse::KIND_TOOL_USE,
            tool_name: Some("Edit".to_string()),
            file_paths,
            is_sidechain: false,
            text: "Edit ...".to_string(),
        },
        byte_offset: 0,
        byte_len: 8,
    }
}

fn sample_commit_session(confidence: &str, via: &str) -> CommitSessionRow {
    CommitSessionRow {
        confidence: confidence.to_string(),
        via: via.to_string(),
        session_id: Some("sess-1".to_string()),
        kb: Some("memory".to_string()),
        display_name: Some("fixed the gizmo race".to_string()),
        started_at: Some(1_700_000_000),
        resolved_at: 1_700_000_100,
    }
}

fn sample_annotation(id: &str, repo_id: i64, path: &str) -> AnnotationRow {
    AnnotationRow {
        id: id.to_string(),
        repo_id,
        path: path.to_string(),
        anchor: Some(
            r#"{"kind":"selection","css_path":"","offset":1,"snippet":"fn a() {}"}"#.to_string(),
        ),
        anchor_kind: "line".to_string(),
        anchor2: None,
        parent_id: None,
        intent: "note".to_string(),
        body: "why is this here?".to_string(),
        author: "you".to_string(),
        created_at: 1_700_000_000,
        updated_at: 1_700_000_000,
        resolved: false,
        review_id: None,
        ps_number: None,
        side: None,
        set_id: None,
        trail_id: None,
    }
}

fn sample_reply(id: &str, parent: &AnnotationRow, body: &str) -> AnnotationRow {
    AnnotationRow {
        id: id.to_string(),
        repo_id: parent.repo_id,
        path: parent.path.clone(),
        anchor: None,
        anchor_kind: "line".to_string(),
        anchor2: None,
        parent_id: Some(parent.id.clone()),
        intent: "note".to_string(),
        body: body.to_string(),
        author: "you".to_string(),
        created_at: parent.created_at + 1,
        updated_at: parent.created_at + 1,
        resolved: false,
        review_id: parent.review_id,
        ps_number: parent.ps_number,
        side: parent.side.clone(),
        set_id: parent.set_id.clone(),
        trail_id: parent.trail_id.clone(),
    }
}

fn insert_suggestion(store: &Store, annotation_id: &str) {
    store
        .lock()
        .execute(
            "INSERT INTO annotation_suggestions
                (annotation_id, replacement, original, base_blob_sha,
                 applied, created_at, updated_at)
             VALUES (?1, 'new', 'old', 'deadbeef', 0, 1, 1)",
            params![annotation_id],
        )
        .unwrap();
}

fn whole_file_span(path: &str) -> NewReadingSetSpan {
    NewReadingSetSpan {
        path: path.to_string(),
        ..Default::default()
    }
}

fn ranged_span(path: &str, start: i64, end: i64, note: &str) -> NewReadingSetSpan {
    NewReadingSetSpan {
        path: path.to_string(),
        line_start: Some(start),
        line_end: Some(end),
        git_ref: Some("deadbeef".to_string()),
        note: Some(note.to_string()),
    }
}

fn pin(kb: &str, doc: &str, repo: &str) -> DocLensPin {
    DocLensPin {
        kb: kb.to_string(),
        doc_id: doc.to_string(),
        repo: repo.to_string(),
        repo_root: format!("/tmp/{repo}"),
        doc_hash: Some("h1".to_string()),
        pinned_at: 1_754_500_000,
    }
}

fn sample_imported_finding(slug: &str) -> ImportedFinding {
    ImportedFinding {
        slug: slug.to_string(),
        severity: SEVERITY_CONCERN.to_string(),
        category: "Concurrency".to_string(),
        location_kind: LOCATION_KIND_SINGLE.to_string(),
        location_path: "app/models/order.rb".to_string(),
        location_lines: Some(location_lines_json(&[88])),
        location_removed: false,
        title: "Duplicate order rows possible".to_string(),
        rationale: "Verified against db/schema.rb:41".to_string(),
        recommendation: Some("Add a unique index.".to_string()),
        evidence_lang: Some("ruby".to_string()),
        evidence_source: Some("def checkout!\nend".to_string()),
        anchor_kind: crate::annotations::ANCHOR_KIND_LINE.to_string(),
        anchor: serde_json::to_string(&crate::annotations::anchor_for_line(88, "def checkout!"))
            .unwrap(),
        anchor2: None,
        side: Some("new".to_string()),
        act: "issue".to_string(),
        blocking: false,
        cites_json: None,
        fingerprint: None,
        supersedes: Vec::new(),
    }
}

fn setup_review_for_findings(store: &Store) -> (i64, i64) {
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    let review_id = store
        .create_review("r", Some("feat"), "main", "feature", None, 1_000)
        .unwrap();
    (repo_id, review_id)
}

fn insert_manual_finding(
    store: &Store,
    repo_id: i64,
    review_id: i64,
    slug: &str,
) -> NewReviewFinding {
    let new = NewReviewFinding {
        review_id,
        repo_id,
        ps_number: 1,
        slug: slug.to_string(),
        severity: SEVERITY_OK.to_string(),
        category: "Style".to_string(),
        location_kind: LOCATION_KIND_SINGLE.to_string(),
        location_path: "app/models/order.rb".to_string(),
        location_lines: Some(location_lines_json(&[5])),
        location_removed: false,
        title: "A human noticed this too".to_string(),
        rationale: "Spotted while reading the diff.".to_string(),
        recommendation: None,
        evidence_lang: None,
        evidence_source: None,
        anchor_kind: crate::annotations::ANCHOR_KIND_LINE.to_string(),
        anchor: serde_json::to_string(&crate::annotations::anchor_for_line(5, "x")).unwrap(),
        anchor2: None,
        side: Some("new".to_string()),
        author: "you".to_string(),
        import_batch_id: "manual".to_string(),
        origin: FINDING_ORIGIN_MANUAL.to_string(),
        finding_author: Some("you".to_string()),
        act: "issue".to_string(),
        blocking: false,
        cites_json: None,
        fingerprint: None,
    };
    store.insert_review_finding(&new, 900).unwrap();
    new
}

fn analytics_finding(slug: &str, severity: &str, category: &str, path: &str) -> ImportedFinding {
    let mut f = sample_imported_finding(slug);
    f.severity = severity.to_string();
    f.category = category.to_string();
    f.location_path = path.to_string();
    f
}
