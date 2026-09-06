//! LSC-5 — the Codex CLI rollout adapter (design §4 "Codex — clean turns,
//! ambiguous ending"), a sibling of [`super::super::live`]'s Claude Code
//! adapter: same bounded-tail discipline (reuses
//! [`super::super::live::read_tail_lines`] verbatim, never a full parse),
//! same honest-`None`-on-ambiguity rule, same `why` debug-string
//! convention.
//!
//! Ground truth verified against real `codex-cli 0.146.0` rollouts on this
//! box (`~/.codex/sessions/<yyyy>/<mm>/<dd>/rollout-*.jsonl`, up to 38 MB):
//! turn boundaries ride `event_msg` records whose `payload.type` is
//! `task_started` / `task_complete` / `turn_aborted`. The canonical session
//! id is `session_meta.payload.id` (== `.payload.session_id` == the UUID
//! suffix of the filename, per `kb-capture-codex.sh`'s own extraction) —
//! but `session_meta` is always the FIRST line of the file, so a 256 KiB
//! tail read of a 38 MB rollout never sees it. The id is therefore read
//! from the FILENAME (verified: the trailing UUID always matches
//! `session_meta.payload.id` on every rollout sampled), not the tail.
//!
//! Honest ambiguity (ground truth, verified — codex exits after a
//! headless `exec` run but lingers interactively): a trailing
//! `task_complete` does NOT distinguish "waiting for the next prompt" from
//! "the process is gone". This adapter reports [`Holder::Human`] (so the
//! elapsed axis does the rest) and says exactly that in `why` — it never
//! guesses `Finished`.

use regex::Regex;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::sessions::live::{
    self, clamp_last_activity_unix, derive_state, read_tail_lines, Holder, LivePolicy, LiveSession,
    StateSource,
};

/// This adapter's entry in the closed [`crate::sessions::HARNESSES`] set —
/// named once, referenced everywhere below, pinned by
/// `harness_label_is_a_member_of_the_closed_set` so it can never drift from
/// the set `constants.rs` owns (never a bare string literal at the
/// construction site, per the DCB precedent [`crate::sessions::HARNESS_DEFAULT`]
/// already established for Claude Code).
const HARNESS: &str = "codex";

/// Standard UUID shape (8-4-4-4-12 hex), matched at the END of the rollout
/// filename stem (`rollout-<timestamp-with-dashes>-<uuid>`). Verified
/// against every rollout on this box: the match is always present and
/// always equals `session_meta.payload.id`.
fn uuid_suffix_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$")
            .expect("static regex is valid")
    })
}

/// The three turn-boundary `event_msg.payload.type` values (design §4,
/// verified live). Anything else (`user_message`, `agent_message`,
/// `token_count`, …) is conversational noise for THIS adapter's purposes —
/// still walked for title extraction, never for the holder axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Boundary {
    TaskStarted,
    TaskComplete,
    TurnAborted,
}

impl Boundary {
    fn from_payload_type(s: &str) -> Option<Self> {
        match s {
            "task_started" => Some(Boundary::TaskStarted),
            "task_complete" => Some(Boundary::TaskComplete),
            "turn_aborted" => Some(Boundary::TurnAborted),
            _ => None,
        }
    }
}

/// Classify one Codex rollout file. `None` when the filename carries no
/// recoverable session id, the tail window contains no turn-boundary
/// `event_msg` record at all, or the file can't be stat'd/opened — every
/// case an honest "don't know", mirroring
/// [`super::super::live::classify_claude_transcript`].
pub fn classify_codex_session(
    path: &Path,
    now_unix: i64,
    policy: &LivePolicy,
) -> Option<LiveSession> {
    let stem = path.file_stem()?.to_str()?;
    let session_id = uuid_suffix_re()
        .captures(stem)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_lowercase())?;

    let meta = fs::metadata(path).ok()?;
    let mtime_unix = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(now_unix);
    let last_activity_unix = clamp_last_activity_unix(mtime_unix, now_unix);

    let lines = read_tail_lines(path)?;

    let mut last_boundary: Option<(Boundary, String)> = None;
    let mut cwd: Option<String> = None;
    let mut model: Option<String> = None;
    let mut last_user_message: Option<String> = None;

    for line in &lines {
        let Ok(rec) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match rec.get("type").and_then(Value::as_str).unwrap_or("") {
            "turn_context" => {
                if let Some(c) = rec
                    .get("payload")
                    .and_then(|p| p.get("cwd"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    cwd = Some(c.to_string());
                }
                if let Some(m) = rec
                    .get("payload")
                    .and_then(|p| p.get("model"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    model = Some(m.to_string());
                }
            }
            "event_msg" => {
                let payload = rec.get("payload");
                let ptype = payload
                    .and_then(|p| p.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if let Some(b) = Boundary::from_payload_type(ptype) {
                    let detail = match b {
                        Boundary::TurnAborted => payload
                            .and_then(|p| p.get("reason"))
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                            .to_string(),
                        _ => String::new(),
                    };
                    last_boundary = Some((b, detail));
                } else if ptype == "user_message" {
                    if let Some(m) = payload
                        .and_then(|p| p.get("message"))
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        last_user_message = Some(m.chars().take(70).collect());
                    }
                }
            }
            _ => {}
        }
    }

    let (holder, why) = match last_boundary {
        // Rule: last = task_started with nothing after => the agent holds
        // the turn (an exec is outstanding).
        Some((Boundary::TaskStarted, _)) => (Holder::Agent, "event_msg=task_started".to_string()),
        // Rule (ground truth, verified): task_complete is genuinely
        // ambiguous between "waiting for you" and "the process exited" —
        // codex lingers after an interactive run but exits after a headless
        // `exec`. Honest Human, honest caveat in `why`; never guess Finished.
        Some((Boundary::TaskComplete, _)) => (
            Holder::Human,
            "event_msg=task_complete (waiting-vs-finished ambiguous from file alone)".to_string(),
        ),
        Some((Boundary::TurnAborted, reason)) => (
            Holder::Human,
            format!("event_msg=turn_aborted reason={reason}"),
        ),
        // No turn-boundary event_msg record anywhere in the tail window —
        // honest "don't know", never a guess.
        None => return None,
    };

    let (state, confidence) = derive_state(
        holder,
        last_activity_unix,
        now_unix,
        StateSource::Transcript,
        policy,
    );

    let project = cwd
        .as_deref()
        .and_then(|c| Path::new(c).file_name())
        .map(|n| n.to_string_lossy().into_owned());
    // Deliberately NO path-parent fallback for `project` (unlike the Claude
    // adapter): a rollout's parent directory is a `<dd>` date segment, not a
    // project slug — falling back to it would be an honest-looking lie.

    Some(LiveSession {
        resume: format!("codex resume {session_id}"),
        session_id,
        harness: HARNESS.to_string(),
        holder,
        state,
        source: StateSource::Transcript,
        confidence,
        since_unix: last_activity_unix,
        since_secs: (now_unix - last_activity_unix).max(0),
        project,
        cwd,
        model,
        title: last_user_message,
        transcript_path: path.to_path_buf(),
        why: Some(why),
        version: None,
    })
}

/// Walk `<root>/<yyyy>/<mm>/<dd>/rollout-*.jsonl` (verified real layout)
/// and classify every candidate. Depth is variable (yyyy/mm/dd are three
/// nested levels, unlike Claude's fixed depth-2), so this is a small
/// recursive walk — still bounded by [`live::LIVE_SCAN_CAP`] directory
/// entries considered across the WHOLE walk, and still gated on mtime
/// (against `max_age_secs`) BEFORE a file is ever opened, mirroring
/// [`live::scan_claude_projects`]'s two load-bearing discipline rules.
pub fn scan_codex(
    root: &Path,
    now_unix: i64,
    policy: &LivePolicy,
    max_age_secs: i64,
) -> Vec<LiveSession> {
    let mut out = Vec::new();
    let mut scanned = 0usize;
    walk_dir(
        root,
        now_unix,
        policy,
        max_age_secs,
        &mut out,
        &mut scanned,
        0,
    );
    out
}

/// Codex nests three directory levels (`yyyy/mm/dd`) before the rollout
/// files — cap recursion depth at 4 (root + 3 levels) so a pathological
/// symlink loop or an unexpectedly deep tree can't runaway; real rollouts
/// never nest deeper than that.
const MAX_WALK_DEPTH: u8 = 4;

#[allow(clippy::too_many_arguments)]
fn walk_dir(
    dir: &Path,
    now_unix: i64,
    policy: &LivePolicy,
    max_age_secs: i64,
    out: &mut Vec<LiveSession>,
    scanned: &mut usize,
    depth: u8,
) {
    if depth > MAX_WALK_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if *scanned >= live::LIVE_SCAN_CAP {
            return;
        }
        *scanned += 1;
        let p: PathBuf = entry.path();
        if p.is_dir() {
            walk_dir(&p, now_unix, policy, max_age_secs, out, scanned, depth + 1);
            continue;
        }
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !(name.starts_with("rollout-") && name.ends_with(".jsonl")) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let mtime_unix = modified
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        // Cheap gate BEFORE opening the file.
        if now_unix - mtime_unix > max_age_secs {
            continue;
        }
        if let Some(session) = classify_codex_session(&p, now_unix, policy) {
            out.push(session);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        p
    }

    const SID: &str = "01a025fd-51eb-7cd3-86f2-95261769cb14";

    fn rollout_name() -> String {
        format!("rollout-2026-08-21T22-22-33-{SID}.jsonl")
    }

    #[test]
    fn agent_holds_on_trailing_task_started() {
        let tmp = tempfile::tempdir().unwrap();
        let content = concat!(
            r#"{"type":"turn_context","payload":{"cwd":"/home/user/project/kb","model":"gpt-5.3-codex-spark"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"t1"}}"#,
            "\n",
        );
        let p = write_file(tmp.path(), &rollout_name(), content);
        let got = classify_codex_session(&p, 100_000_000, &LivePolicy::default()).unwrap();
        assert_eq!(got.session_id, SID);
        assert_eq!(got.holder, Holder::Agent);
        assert_eq!(got.harness, "codex");
        assert_eq!(got.project.as_deref(), Some("kb"));
        assert!(got.why.as_deref().unwrap().contains("task_started"));
    }

    /// The ambiguous case, verified against real behaviour: task_complete
    /// must NOT be reported as Finished — Human, with an honest caveat.
    #[test]
    fn human_holds_on_trailing_task_complete_with_honest_ambiguity_caveat() {
        let tmp = tempfile::tempdir().unwrap();
        let content = concat!(
            r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"t1"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"t1"}}"#,
            "\n",
        );
        let p = write_file(tmp.path(), &rollout_name(), content);
        let got = classify_codex_session(&p, 100_000_000, &LivePolicy::default()).unwrap();
        assert_eq!(got.holder, Holder::Human);
        assert!(got
            .why
            .as_deref()
            .unwrap()
            .contains("waiting-vs-finished ambiguous"));
    }

    #[test]
    fn turn_aborted_is_human_with_reason_in_why() {
        let tmp = tempfile::tempdir().unwrap();
        let content = concat!(
            r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"t1"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"turn_aborted","turn_id":"t1","reason":"interrupted"}}"#,
            "\n",
        );
        let p = write_file(tmp.path(), &rollout_name(), content);
        let got = classify_codex_session(&p, 100_000_000, &LivePolicy::default()).unwrap();
        assert_eq!(got.holder, Holder::Human);
        assert!(got.why.as_deref().unwrap().contains("turn_aborted"));
        assert!(got.why.as_deref().unwrap().contains("interrupted"));
    }

    #[test]
    fn robustness_empty_file_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_file(tmp.path(), &rollout_name(), "");
        assert!(classify_codex_session(&p, 100_000_000, &LivePolicy::default()).is_none());
    }

    #[test]
    fn robustness_no_turn_boundary_record_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let content = concat!(
            r#"{"type":"event_msg","payload":{"type":"user_message","message":"hi"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count"}}"#,
            "\n",
        );
        let p = write_file(tmp.path(), &rollout_name(), content);
        assert!(classify_codex_session(&p, 100_000_000, &LivePolicy::default()).is_none());
    }

    #[test]
    fn robustness_torn_trailing_line_is_skipped_not_errored() {
        let tmp = tempfile::tempdir().unwrap();
        let mut content = String::new();
        content.push_str(r#"{"type":"event_msg","payload":{"type":"task_started"}}"#);
        content.push('\n');
        content.push_str(r#"{"type":"event_msg","payload":{"type":"task_comp"#); // torn
        let p = write_file(tmp.path(), &rollout_name(), &content);
        let got = classify_codex_session(&p, 100_000_000, &LivePolicy::default()).unwrap();
        assert_eq!(got.holder, Holder::Agent);
    }

    #[test]
    fn robustness_pure_garbage_returns_none_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_file(tmp.path(), &rollout_name(), "not json\n{{{\ngarbage");
        assert!(classify_codex_session(&p, 100_000_000, &LivePolicy::default()).is_none());
    }

    #[test]
    fn robustness_unrecognisable_filename_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let content = r#"{"type":"event_msg","payload":{"type":"task_started"}}
"#;
        let p = write_file(tmp.path(), "rollout-not-a-uuid.jsonl", content);
        assert!(classify_codex_session(&p, 100_000_000, &LivePolicy::default()).is_none());
    }

    #[test]
    fn scan_walks_yyyy_mm_dd_and_respects_the_cheap_age_gate() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("2026").join("08").join("21");
        fs::create_dir_all(&dir).unwrap();
        write_file(
            &dir,
            &rollout_name(),
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n",
        );
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let got = scan_codex(tmp.path(), now, &LivePolicy::default(), 86_400);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].session_id, SID);

        let excluded = scan_codex(tmp.path(), now, &LivePolicy::default(), -1);
        assert!(excluded.is_empty());
    }

    #[test]
    fn scan_returns_empty_for_a_nonexistent_root_not_an_error_or_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let got = scan_codex(&missing, 100_000_000, &LivePolicy::default(), 86_400);
        assert!(got.is_empty());
    }

    #[test]
    fn harness_label_is_a_member_of_the_closed_set() {
        assert!(crate::sessions::HARNESSES.contains(&HARNESS));
    }
}
