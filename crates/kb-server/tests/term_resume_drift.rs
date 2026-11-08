//! kb-term resume-tail drift test: kb-term's pinned copy of the resume
//! table must stay byte-identical (after quoting) to kb-core's ONE
//! table. kb-server already depends on kb-core; the dev-dep on kb-term
//! is the only link.

use kb_core::sessions::resume::NOT_RESUMABLE_UNSAFE;
use kb_core::sessions::{resume_command, HARNESSES};
use kb_term::profiles::{resume_tail, PlanError, HARNESSES as TERM_HARNESSES};

/// Split a command string the way POSIX sh words it: unquoted whitespace
/// separates, single quotes group, `'\''` is a literal quote.
fn posix_unquote_split(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut has = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_q = !in_q;
                has = true;
            }
            '\\' if !in_q => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    has = true;
                }
            }
            c if c.is_whitespace() && !in_q => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        out.push(cur);
    }
    out
}

#[test]
fn kb_term_resume_matches_kb_core_resume_command() {
    let sids = [
        "sid-1",
        "session_0a1b2c",
        "ses_x.y-z:1",
        "spec-mobsheet-001",
    ];
    let cwds: [Option<&str>; 3] = [None, Some("/w/x"), Some("/w/it's here")];
    for &harness in &HARNESSES {
        for &sid in &sids {
            for &cwd in &cwds {
                let from_core = posix_unquote_split(&resume_command(harness, sid, cwd));
                let from_term = resume_tail(harness, sid, cwd).unwrap_or_else(|e| panic!("{e:?}"));
                assert_eq!(
                    from_core, from_term,
                    "harness {harness} sid {sid} cwd {cwd:?}"
                );
            }
        }
    }
}

#[test]
fn kb_term_rejects_what_kb_core_calls_unsafe() {
    let long = "a".repeat(129);
    for sid in ["", "-x", "a b", "a;b", long.as_str()] {
        let sentence = resume_command("omp", sid, None);
        assert_eq!(sentence, NOT_RESUMABLE_UNSAFE, "sid {sid:?}");
        assert_eq!(
            resume_tail("omp", sid, None),
            Err(PlanError::UnsafeToken("resume_sid")),
            "sid {sid:?}"
        );
    }
}

#[test]
fn kb_term_harness_set_equals_kb_core_set() {
    assert_eq!(TERM_HARNESSES.as_slice(), HARNESSES.as_slice());
    let sentence = resume_command("zzz", "sid-1", None);
    assert!(sentence.contains("not resumable"), "got: {sentence}");
    assert_eq!(
        resume_tail("zzz", "sid-1", None),
        Err(PlanError::UnknownHarness)
    );
}
