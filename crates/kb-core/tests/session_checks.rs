//! OL IDEA3 -- the session evidence card over a SYNTHETIC whole-transcript
//! fixture (`session_fixtures/verify-after-edit.jsonl`, invented content, the
//! directory's standing rule), plus the R1-digest isolation pin.
//!
//! The fixture runs: Edit counter.rs -> `flock ... cargo test -p widget 2>&1 |
//! tail -5` (the exit status is `tail`'s, the `test result: ok.` line decides)
//! -> Edit render.rs -> a BACKGROUND `cargo clippy` completed by a
//! `<task-notification>` -> a closing text that names counter.rs only.

use kb_core::sessions::checks::{
    build_evidence, summarize, Basis, Outcome, Verdict, EVIDENCE_VERSION,
};
use kb_core::sessions::{parse_session_html_full, session_digest, session_digest_excerpt};

const FIXED_CAPTURE_TS: &str = "20260101T000000Z";
const FIXED_MTIME_UNIX: i64 = 1_767_225_600;

fn fixture_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/session_fixtures")
        .join(format!("{name}.jsonl"))
}

/// The kb-capture.sh envelope around a fixture's raw JSONL (the same escape
/// chain as `session_digest_snapshots.rs`).
fn build_capture(name: &str, jsonl: &str) -> (String, String) {
    let esc = jsonl
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let html = format!(
        "<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\n\
         <title>Session transcript {FIXED_CAPTURE_TS}</title>\n\
         <meta name=\"kb-category\" content=\"memory-session\">\n\
         <meta name=\"kb-session\" content=\"{name}\">\n\
         </head><body>\n<h1>Session transcript {FIXED_CAPTURE_TS}</h1>\n\
         <pre>{esc}</pre>\n</body></html>\n"
    );
    (html, format!("session-{FIXED_CAPTURE_TS}-{name}.html"))
}

fn parse(name: &str) -> kb_core::sessions::SessionParse {
    let raw = std::fs::read_to_string(fixture_path(name))
        .unwrap_or_else(|e| panic!("read fixture {name}: {e}"));
    let (html, filename) = build_capture(name, &raw);
    parse_session_html_full(&html, &filename, FIXED_MTIME_UNIX)
}

#[test]
fn verify_after_edit_fixture_yields_the_exact_evidence() {
    let p = parse("verify-after-edit");
    let a = &p.activity;
    let ev = build_evidence(&a.check_runs, "claude", a.last_assistant_text.as_deref());

    assert_eq!(ev.v, EVIDENCE_VERSION);
    assert_eq!(ev.edit_count, 2);
    assert_eq!(ev.edits_ignored, 0);
    assert_eq!(ev.last_edit_seq, Some(3));
    assert_eq!(
        ev.last_edit_at,
        kb_core::timeparse::parse_iso_utc("2026-06-10T09:00:40.000Z")
    );
    assert_eq!(ev.first_push_after_edit_seq, None);
    // The closing text names counter.rs, never render.rs.
    assert_eq!(ev.unnamed_edits, vec!["render.rs".to_string()]);
    assert_eq!(ev.unnamed_edits_total, 1);

    assert_eq!(ev.checks_total, 2);
    assert_eq!(ev.checks.len(), 2);
    // The piped `cargo test`: the exit is tail's, the output line decides.
    let t = &ev.checks[0];
    assert_eq!((t.seq, t.label.as_str()), (2, "cargo test"));
    assert!(t
        .cmd
        .starts_with("flock /tmp/widget.lock env CARGO_BUILD_JOBS=2"));
    assert_eq!(t.runners, vec!["cargo-test".to_string()]);
    assert_eq!(
        (t.outcome, t.basis, t.why),
        (Outcome::Pass, Some(Basis::Output), None)
    );
    // The background clippy, settled by its task-notification.
    let c = &ev.checks[1];
    assert_eq!((c.seq, c.label.as_str()), (4, "cargo clippy"));
    assert_eq!(
        (c.outcome, c.basis, c.exit_code),
        (Outcome::Pass, Some(Basis::Notification), Some(0))
    );

    // The test ran BEFORE the last edit; clippy ran after it.
    let s = summarize(&ev);
    assert_eq!(s.verdict, Verdict::Verified);
    assert_eq!(
        s.phrase.as_deref(),
        Some("cargo clippy passed after the last edit")
    );
    assert_eq!(s.checks_after_edit, 1);
    assert_eq!(s.deciding.as_ref().map(|d| d.seq), Some(4));
}

const DIGEST_FIXTURES: &[&str] = &[
    "tiny-noop",
    "tiny-rate-limited",
    "subagent-delegation",
    "ask-user-question",
    "tool-heavy-research",
    "verify-after-edit",
];

// invariant:11 checks never enter the R1 digest
#[test]
fn digest_ignores_check_tracking() {
    for name in DIGEST_FIXTURES {
        let with = parse(name);
        let mut without = parse(name);
        without.activity.check_runs = Default::default();
        assert_eq!(
            session_digest(&with),
            session_digest(&without),
            "{name}: the R1 digest must not read the check tracker"
        );
        assert_eq!(
            session_digest_excerpt(&with),
            session_digest_excerpt(&without),
            "{name}: the digest excerpt must not read the check tracker"
        );
    }
}

#[test]
fn the_digest_fixtures_with_edits_actually_track_something() {
    // The isolation pin above is only meaningful if the tracker saw work.
    let p = parse("verify-after-edit");
    assert!(!p.activity.check_runs.runs.is_empty());
    assert!(!p.activity.check_runs.edits.is_empty());
}
