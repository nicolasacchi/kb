//! GC-C1 — golden-fixture regression corpus for session-transcript parsing.
//!
//! `crates/kb-core/src/sessions.rs` had ~44 inline tests, all on hand-written
//! synthetic JSONL strings. The 36 session-memory defects catalogued in
//! `docs/research/session-memory-deep-review-2026-07.html` were found by
//! manual review because no whole-transcript fixtures existed to regression-
//! test against. This suite fixes that: the five `tests/session_fixtures/`
//! transcripts below are SYNTHETIC — their record shapes copy real Claude
//! Code captures, every string in them is invented (that directory's standing
//! rule is "never paste real transcript content into the repo"; see its
//! README) — each parsed through the FULL digest pipeline the daemon runs —
//! `parse_session_html_full` (the kb-capture.sh envelope) → `session_digest` /
//! `session_digest_excerpt` — and pinned with an insta snapshot.
//!
//! Determinism: `session_digest`'s own doc comment claims "no clock, no
//! I/O — deterministic given the bytes", and a grep of sessions.rs /
//! session_bundle.rs / session_scrub.rs for `now()` / `SystemTime` /
//! `Utc::now` turns up nothing — confirmed empirically below by parsing
//! every fixture twice and asserting byte-identical output. Both
//! `started_at` (from the FIXED synthetic filename timestamp — a fixture has
//! no capture filename of its own) and the wrapper's `mtime_unix` are
//! constants here, so no wall-clock value can leak into a snapshot.

use kb_core::sessions::{parse_session_html_full, session_digest, session_digest_excerpt};

/// Every fixture in `tests/session_fixtures/`, named for the digest-pipeline
/// behavior it exercises. See that directory's README for the shape each one
/// models + the properties its consumers assert.
const FIXTURES: &[&str] = &[
    "tiny-noop",
    "tiny-rate-limited",
    "subagent-delegation",
    "ask-user-question",
    "tool-heavy-research",
];

/// Fixed synthetic capture time — NOT wall-clock "now". Any real value works
/// since `session_id` resolution prefers the transcript's own ground-truth
/// `sessionId` field over the filename-derived one; this only backs
/// `started_at` when a fixture's (truncated) JSONL carries no earlier
/// signal.
const FIXED_CAPTURE_TS: &str = "20260101T000000Z";
const FIXED_MTIME_UNIX: i64 = 1_767_225_600; // 2026-01-01T00:00:00Z

fn fixture_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/session_fixtures")
        .join(format!("{name}.jsonl"))
}

/// Reverse of `session_scrub`/`sessions::html_unescape`'s target: the
/// kb-capture.sh hook's own escape chain (`&` first, so the `&` it
/// introduces for `<`/`>` doesn't get double-escaped).
fn capture_escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Build the exact kb-capture.sh HTML envelope (see
/// `plugins/kb-memory/hooks/kb-capture.sh`) around a fixture's raw JSONL, and
/// the matching `session-<ts>-<sid>.html` filename.
fn build_capture(name: &str, jsonl: &str) -> (String, String) {
    let esc = capture_escape(jsonl);
    let html = format!(
        "<!DOCTYPE html>\n\
         <html lang=\"en\"><head><meta charset=\"utf-8\">\n\
         <title>Session transcript {FIXED_CAPTURE_TS}</title>\n\
         <meta name=\"kb-category\" content=\"memory-session\">\n\
         <meta name=\"kb-decay\" content=\"fast\">\n\
         <meta name=\"kb-session\" content=\"{name}\">\n\
         </head><body>\n\
         <h1>Session transcript {FIXED_CAPTURE_TS}</h1>\n\
         <pre>{esc}</pre>\n\
         </body></html>\n"
    );
    let filename = format!("session-{FIXED_CAPTURE_TS}-{name}.html");
    (html, filename)
}

/// The curated, snapshot-friendly summary of one fixture's full-pipeline
/// parse: enough of `SessionParse`/`SessionActivity` to catch a regression
/// in any counted signal, without dumping every raw file path (a full
/// `{:#?}` of `SessionActivity` is mostly noise here).
// Every field here is read only through the derived `Debug` (the snapshot
// body) — rustc's dead-code analysis doesn't see through that, so silence it
// rather than adding a synthetic reader nobody needs.
#[allow(dead_code)]
#[derive(Debug)]
struct DigestReport {
    session_id: String,
    message_count: u32,
    first_user_prompt: Option<String>,
    ai_title: Option<String>,
    cwd: Option<String>,
    all_cwds_count: usize,
    git_branch: Option<String>,
    model: Option<String>,
    token_total: u64,
    tool_calls: u32,
    error_count: u32,
    files_read_count: u32,
    files_edited_count: u32,
    decisions: Vec<(String, String, Option<String>)>,
    commits: Vec<(String, Option<String>, Option<String>)>,
    research: Vec<(String, String)>,
    digest: String,
    digest_excerpt: String,
}

fn digest_report(name: &str) -> DigestReport {
    let raw = std::fs::read_to_string(fixture_path(name))
        .unwrap_or_else(|e| panic!("read fixture {name}: {e}"));
    let (html, filename) = build_capture(name, &raw);
    let parse = parse_session_html_full(&html, &filename, FIXED_MTIME_UNIX);
    let digest = session_digest(&parse);
    let digest_excerpt = session_digest_excerpt(&parse);
    let a = &parse.activity;
    DigestReport {
        session_id: parse.facts.session_id.clone(),
        message_count: parse.facts.message_count,
        first_user_prompt: parse.facts.first_user_prompt.clone(),
        ai_title: a.ai_title.clone(),
        cwd: a.cwd.clone(),
        all_cwds_count: a.all_cwds.len(),
        git_branch: a.git_branch.clone(),
        model: a.model.clone(),
        token_total: a.token_total,
        tool_calls: a.tool_calls,
        error_count: a.error_count,
        files_read_count: a.files_read_count(),
        files_edited_count: a.files_edited_count(),
        decisions: a
            .decisions
            .iter()
            .map(|d| (d.kind.clone(), d.prompt.clone(), d.answer.clone()))
            .collect(),
        commits: a
            .commits
            .iter()
            .map(|c| (c.kind.clone(), c.sha.clone(), c.subject.clone()))
            .collect(),
        research: a
            .research
            .iter()
            .map(|r| (r.kind.clone(), r.query.clone()))
            .collect(),
        digest,
        digest_excerpt,
    }
}

macro_rules! snapshot_fixture {
    ($fn_name:ident, $fixture:literal) => {
        #[test]
        fn $fn_name() {
            let report = digest_report($fixture);
            insta::assert_snapshot!(format!("{report:#?}"));
        }
    };
}

snapshot_fixture!(digest_tiny_noop, "tiny-noop");
snapshot_fixture!(digest_tiny_rate_limited, "tiny-rate-limited");
snapshot_fixture!(digest_subagent_delegation, "subagent-delegation");
snapshot_fixture!(digest_ask_user_question, "ask-user-question");
snapshot_fixture!(digest_tool_heavy_research, "tool-heavy-research");

/// Determinism guard: parsing the same fixture bytes twice (two independent
/// `parse_session_html_full` + `session_digest` passes) must yield
/// byte-identical output. Guards against a clock/RNG/HashMap-ordering
/// regression sneaking into the digest pipeline in the future — see the
/// module doc for why this is currently believed to hold.
#[test]
fn digest_pipeline_is_deterministic_across_repeated_parses() {
    for name in FIXTURES {
        let a = digest_report(name);
        let b = digest_report(name);
        assert_eq!(
            format!("{a:#?}"),
            format!("{b:#?}"),
            "{name}: digest pipeline produced different output on a repeat parse"
        );
    }
}

/// Every fixture must itself be valid JSONL (one JSON value per non-empty
/// line) — the hard requirement for a golden fixture: it must parse the way a
/// transcript does, not just as opaque bytes some regex could break.
#[test]
fn every_fixture_is_valid_jsonl() {
    for name in FIXTURES {
        let raw = std::fs::read_to_string(fixture_path(name)).unwrap();
        for (i, line) in raw.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|e| panic!("{name}:{}: not valid JSON: {e}", i + 1));
        }
    }
}
