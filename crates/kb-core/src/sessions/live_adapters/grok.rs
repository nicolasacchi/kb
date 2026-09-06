//! LSC-5 — the Grok Build adapter (design §4 "Grok — a real event log, and
//! a trap"), a sibling of [`super::super::live`]'s Claude Code adapter.
//!
//! Layout verified against real `~/.grok/sessions/<url-encoded-cwd>/
//! <session-uuid>/` directories on this box (cross-checked against
//! `kb-capture-grok.sh`, the existing capture adapter's own extraction):
//! `events.jsonl` (turn boundaries: `turn_started` / `turn_ended` /
//! `phase_changed` / `permission_requested` / `permission_resolved`) and a
//! small `summary.json` (`info.id`, `info.cwd`, `generated_title`,
//! `current_model_id`). The session id is the directory's own name — it
//! equals `summary.json`'s `info.id` on every real session sampled, but the
//! directory name is read FIRST (structural, no parse needed) and
//! `summary.json` is best-effort enrichment only (title/model/cwd), never
//! required for classification.
//!
//! Two verified traps, both honoured here by omission:
//! - `~/.grok/active_sessions.json` is empty for the headless `-p` (via
//!   grokclaude) invocation shape this operator actually uses — it serves
//!   an unused leader/dashboard architecture. This adapter never reads it.
//! - `.grokclaude/jobs/<ulid>/meta.json`'s `status` field has a proven
//!   three-week-stale `"running"` bug. This adapter never reads the
//!   grokclaude job tier at all — it walks Grok's OWN session store
//!   directly, which is exactly what the design's pull signal calls for.
//!
//! `permission_requested`/`permission_resolved` carry no id to pair on
//! (verified: real events include only `tool_name` + `wait_ms`), so
//! "blocked" is a per-`tool_name` open/close counter over the tail window —
//! honest best-effort, surfaced in `why`, never a fabricated precise match.

use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::sessions::live::{
    self, clamp_last_activity_unix, derive_state, read_tail_lines, Holder, LivePolicy, LiveSession,
    StateSource,
};

/// This adapter's entry in the closed [`crate::sessions::HARNESSES`] set —
/// see `codex.rs`'s identical convention/rationale.
const HARNESS: &str = "grok";

/// `summary.json` is a small, bounded metadata sidecar (observed ~0.7 KiB
/// on this box) — NOT a growing transcript, so a whole-file read is
/// appropriate, but still capped defensively rather than trusted blindly.
const SUMMARY_MAX_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Boundary {
    TurnStarted,
    TurnEnded,
}

/// Best-effort per-`tool_name` open/close counter for `permission_requested`
/// / `permission_resolved` pairs (no shared id exists to pair on precisely —
/// see the module docs). Tracks insertion order of still-open tool names so
/// `why` can name the most-recently-opened one deterministically.
#[derive(Default)]
struct PermissionTracker {
    counts: HashMap<String, i32>,
    open_order: Vec<String>,
}

impl PermissionTracker {
    fn requested(&mut self, tool: &str) {
        let c = self.counts.entry(tool.to_string()).or_insert(0);
        if *c <= 0 {
            self.open_order.push(tool.to_string());
        }
        *c += 1;
    }

    fn resolved(&mut self, tool: &str) {
        if let Some(c) = self.counts.get_mut(tool) {
            *c -= 1;
            if *c <= 0 {
                self.open_order.retain(|t| t != tool);
            }
        }
    }

    /// The most-recently-opened tool name still pending, if any.
    fn still_blocked(&self) -> Option<&str> {
        self.open_order.last().map(String::as_str)
    }
}

/// Read summary.json's `info.cwd`, `generated_title` (falling back to
/// `session_summary`), and `current_model_id`. Best-effort: any read/parse
/// failure yields all-`None` rather than propagating an error — this file
/// is enrichment, never load-bearing for classification.
fn read_summary(dir: &Path) -> (Option<String>, Option<String>, Option<String>) {
    let path = dir.join("summary.json");
    let Ok(meta) = fs::metadata(&path) else {
        return (None, None, None);
    };
    if meta.len() > SUMMARY_MAX_BYTES {
        return (None, None, None);
    }
    let Ok(text) = fs::read_to_string(&path) else {
        return (None, None, None);
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return (None, None, None);
    };
    let cwd = v
        .get("info")
        .and_then(|i| i.get("cwd"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let title = v
        .get("generated_title")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            v.get("session_summary")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .map(str::to_string);
    let model = v
        .get("current_model_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    (cwd, title, model)
}

/// Classify one Grok session directory. `None` when there is no
/// `events.jsonl` at all, the tail window has no turn-boundary record, or
/// the directory can't be stat'd — honest "don't know", never a guess.
pub fn classify_grok_session(
    session_dir: &Path,
    now_unix: i64,
    policy: &LivePolicy,
) -> Option<LiveSession> {
    let session_id = session_dir.file_name()?.to_str()?.to_string();
    let events_path = session_dir.join("events.jsonl");
    let meta = fs::metadata(&events_path).ok()?;
    let mtime_unix = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(now_unix);
    let last_activity_unix = clamp_last_activity_unix(mtime_unix, now_unix);

    let lines = read_tail_lines(&events_path)?;

    let mut last_boundary: Option<(Boundary, String)> = None;
    let mut perms = PermissionTracker::default();

    for line in &lines {
        let Ok(rec) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match rec.get("type").and_then(Value::as_str).unwrap_or("") {
            "turn_started" => last_boundary = Some((Boundary::TurnStarted, String::new())),
            "turn_ended" => {
                let outcome = rec
                    .get("outcome")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                last_boundary = Some((Boundary::TurnEnded, outcome));
            }
            "permission_requested" => {
                let tool = rec
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool");
                perms.requested(tool);
            }
            "permission_resolved" => {
                let tool = rec
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool");
                perms.resolved(tool);
            }
            _ => {}
        }
    }

    let (holder, why) = if let Some(tool) = perms.still_blocked() {
        // Ground truth: a pending permission_requested with no matching
        // permission_resolved means the agent is BLOCKED, not that the
        // human holds the turn — the design renders blocked inside the
        // waiting lane via `why`, never a new LiveState variant.
        (
            Holder::Agent,
            format!("blocked: permission pending (tool={tool})"),
        )
    } else {
        match last_boundary {
            Some((Boundary::TurnStarted, _)) => (Holder::Agent, "event=turn_started".to_string()),
            Some((Boundary::TurnEnded, outcome)) => {
                (Holder::Human, format!("event=turn_ended outcome={outcome}"))
            }
            // No turn boundary AND no pending permission anywhere in the
            // tail window — genuinely no signal.
            None => return None,
        }
    };

    let (state, confidence) = derive_state(
        holder,
        last_activity_unix,
        now_unix,
        StateSource::Transcript,
        policy,
    );

    let (cwd, title, model) = read_summary(session_dir);
    let project = cwd
        .as_deref()
        .and_then(|c| Path::new(c).file_name())
        .map(|n| n.to_string_lossy().into_owned());
    // Deliberately no fallback to the parent (URL-encoded-cwd) directory
    // name for `project` — decoding it would need a new dependency (see
    // the module docs) and `summary.json`'s plaintext `info.cwd` already
    // gives the honest answer when present.

    Some(LiveSession {
        resume: format!("grok --resume {session_id}"),
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
        title,
        transcript_path: events_path,
        why: Some(why),
        version: None,
    })
}

/// Walk `<root>/<url-encoded-cwd>/<session-uuid>/` (depth 2, mirroring
/// [`live::scan_claude_projects`]'s shape one level deeper) and classify
/// every candidate. Same two discipline rules: bounded by
/// [`live::LIVE_SCAN_CAP`] total directory entries considered, and the
/// cheap mtime gate runs BEFORE `events.jsonl` is ever opened.
#[allow(clippy::explicit_counter_loop)]
pub fn scan_grok(
    root: &Path,
    now_unix: i64,
    policy: &LivePolicy,
    max_age_secs: i64,
) -> Vec<LiveSession> {
    let mut out = Vec::new();
    let mut scanned = 0usize;
    let Ok(cwd_dirs) = fs::read_dir(root) else {
        return out;
    };
    'outer: for cwd_entry in cwd_dirs.flatten() {
        let cwd_path = cwd_entry.path();
        if !cwd_path.is_dir() {
            continue;
        }
        let Ok(session_dirs) = fs::read_dir(&cwd_path) else {
            continue;
        };
        for sess_entry in session_dirs.flatten() {
            if scanned >= live::LIVE_SCAN_CAP {
                break 'outer;
            }
            scanned += 1;
            let sess_path: PathBuf = sess_entry.path();
            if !sess_path.is_dir() {
                continue;
            }
            let events_path = sess_path.join("events.jsonl");
            let Ok(meta) = fs::metadata(&events_path) else {
                continue;
            };
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
            if let Some(session) = classify_grok_session(&sess_path, now_unix, policy) {
                out.push(session);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(dir: &Path, name: &str, content: &str) {
        let p = dir.join(name);
        let mut f = fs::File::create(p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    fn make_session_dir(root: &Path, cwd_enc: &str, sid: &str) -> PathBuf {
        let dir = root.join(cwd_enc).join(sid);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn agent_holds_on_trailing_turn_started() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "%2Fhome%2Fuser%2Fproject%2Fkb", "sid-a");
        write_file(
            &dir,
            "events.jsonl",
            "{\"type\":\"turn_started\"}\n{\"type\":\"phase_changed\",\"phase\":\"streaming_text\"}\n",
        );
        write_file(
            &dir,
            "summary.json",
            r#"{"info":{"id":"sid-a","cwd":"/home/user/project/kb"},"generated_title":"t","current_model_id":"grok-4.6"}"#,
        );
        let got = classify_grok_session(&dir, 100_000_000, &LivePolicy::default()).unwrap();
        assert_eq!(got.holder, Holder::Agent);
        assert_eq!(got.harness, "grok");
        assert_eq!(got.project.as_deref(), Some("kb"));
        assert_eq!(got.model.as_deref(), Some("grok-4.6"));
        assert!(got.why.as_deref().unwrap().contains("turn_started"));
    }

    #[test]
    fn human_holds_on_trailing_turn_ended() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "%2Ftmp", "sid-b");
        write_file(
            &dir,
            "events.jsonl",
            "{\"type\":\"turn_started\"}\n{\"type\":\"turn_ended\",\"outcome\":\"completed\"}\n",
        );
        let got = classify_grok_session(&dir, 100_000_000, &LivePolicy::default()).unwrap();
        assert_eq!(got.holder, Holder::Human);
        assert!(got.why.as_deref().unwrap().contains("turn_ended"));
        assert!(got.why.as_deref().unwrap().contains("completed"));
    }

    /// Ground truth blocked case: an unresolved `permission_requested`
    /// overrides even a trailing `turn_ended` — the agent is stuck, not the
    /// human waiting.
    #[test]
    fn unresolved_permission_request_is_agent_but_flagged_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "%2Ftmp", "sid-c");
        write_file(
            &dir,
            "events.jsonl",
            concat!(
                "{\"type\":\"turn_started\"}\n",
                "{\"type\":\"permission_requested\",\"tool_name\":\"run_terminal_command\"}\n",
            ),
        );
        let got = classify_grok_session(&dir, 100_000_000, &LivePolicy::default()).unwrap();
        assert_eq!(got.holder, Holder::Agent);
        assert!(got.why.as_deref().unwrap().contains("blocked"));
        assert!(got.why.as_deref().unwrap().contains("run_terminal_command"));
    }

    /// A RESOLVED permission request must not be reported as blocked.
    #[test]
    fn resolved_permission_request_is_not_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "%2Ftmp", "sid-d");
        write_file(
            &dir,
            "events.jsonl",
            concat!(
                "{\"type\":\"turn_started\"}\n",
                "{\"type\":\"permission_requested\",\"tool_name\":\"grep\"}\n",
                "{\"type\":\"permission_resolved\",\"tool_name\":\"grep\",\"decision\":\"allow\"}\n",
                "{\"type\":\"turn_ended\",\"outcome\":\"completed\"}\n",
            ),
        );
        let got = classify_grok_session(&dir, 100_000_000, &LivePolicy::default()).unwrap();
        assert_eq!(got.holder, Holder::Human);
        assert!(!got.why.as_deref().unwrap().contains("blocked"));
    }

    #[test]
    fn robustness_empty_events_file_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "%2Ftmp", "sid-e");
        write_file(&dir, "events.jsonl", "");
        assert!(classify_grok_session(&dir, 100_000_000, &LivePolicy::default()).is_none());
    }

    #[test]
    fn robustness_no_events_file_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("%2Ftmp").join("sid-f");
        fs::create_dir_all(&dir).unwrap();
        assert!(classify_grok_session(&dir, 100_000_000, &LivePolicy::default()).is_none());
    }

    #[test]
    fn robustness_no_recognisable_turn_record_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "%2Ftmp", "sid-g");
        write_file(
            &dir,
            "events.jsonl",
            "{\"type\":\"phase_changed\",\"phase\":\"streaming_text\"}\n",
        );
        assert!(classify_grok_session(&dir, 100_000_000, &LivePolicy::default()).is_none());
    }

    #[test]
    fn robustness_torn_trailing_line_is_skipped_not_errored() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "%2Ftmp", "sid-h");
        let mut content = String::new();
        content.push_str("{\"type\":\"turn_started\"}\n");
        content.push_str("{\"type\":\"turn_e"); // torn
        write_file(&dir, "events.jsonl", &content);
        let got = classify_grok_session(&dir, 100_000_000, &LivePolicy::default()).unwrap();
        assert_eq!(got.holder, Holder::Agent);
    }

    #[test]
    fn robustness_pure_garbage_returns_none_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "%2Ftmp", "sid-i");
        write_file(&dir, "events.jsonl", "not json\n{{{\ngarbage");
        assert!(classify_grok_session(&dir, 100_000_000, &LivePolicy::default()).is_none());
    }

    /// The confirmed operator trap: `active_sessions.json` must never be
    /// consulted, even when present and sitting right next to `sessions/`.
    #[test]
    fn scan_never_reads_active_sessions_json() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("sessions");
        let dir = make_session_dir(&root, "%2Ftmp", "sid-j");
        write_file(&dir, "events.jsonl", "{\"type\":\"turn_started\"}\n");
        // A malformed active_sessions.json sitting next to `sessions/` —
        // if this adapter ever opened it, the scan would panic/degrade.
        write_file(tmp.path(), "active_sessions.json", "not even json");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let got = scan_grok(&root, now, &LivePolicy::default(), 86_400);
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn scan_cheap_gate_and_cap_and_missing_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("sessions");
        let dir = make_session_dir(&root, "%2Ftmp", "sid-k");
        write_file(&dir, "events.jsonl", "{\"type\":\"turn_started\"}\n");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let excluded = scan_grok(&root, now, &LivePolicy::default(), -1);
        assert!(excluded.is_empty());
        let included = scan_grok(&root, now, &LivePolicy::default(), 86_400);
        assert_eq!(included.len(), 1);

        let missing = tmp.path().join("does-not-exist");
        assert!(scan_grok(&missing, now, &LivePolicy::default(), 86_400).is_empty());
    }

    #[test]
    fn harness_label_is_a_member_of_the_closed_set() {
        assert!(crate::sessions::HARNESSES.contains(&HARNESS));
    }
}
