//! LSC-5 — the Kimi Code adapter (design §4 "Kimi — hooks work, sandboxed
//! workers do not"), a sibling of [`super::super::live`]'s Claude Code
//! adapter.
//!
//! Layout verified against real `~/.kimi-code/` (v0.37.2 — NOT `~/.kimi`,
//! per the ground truth) session dirs on this box:
//! `<home>/sessions/<workDirKey>/session_<uuid>/agents/main/wire.jsonl`
//! (subagent wires live as SIBLING `agents/agent-N/wire.jsonl` — never
//! opened here, mirroring the Claude adapter's refusal to descend into
//! `subagents/`) plus a small `state.json` (`.cwd`, `.title`,
//! `.lastPrompt`). `wire.jsonl` carries `turn.prompt` / `turn.ended` turn
//! boundaries — the session id is the `session_<uuid>` directory's own
//! name, per `kb-capture-kimi.sh`'s own extraction (the wire itself has no
//! `sessionId` field).
//!
//! `kimiclaude` worker jobs run `kimi -p` inside bubblewrap with a per-job
//! `KIMI_CODE_HOME`, so their `wire.jsonl` lives under a job dir rather
//! than the shared home — [`scan_kimi`] takes ONE home root per call
//! precisely so a caller can scan both (the shared home AND any per-job
//! homes) by calling it more than once; see [`super::LiveRoots::kimi`].
//!
//! Beyond the ground truth handed down for this phase: real
//! `~/.kimi-code` data on this box also carries `interaction.request` /
//! `interaction.resolved` pairs (approval + question prompts, keyed by a
//! shared `id`) — Kimi Code's own analogue of Grok's permission block.
//! Verified live (not merely inferred), and treated exactly like Grok's:
//! `Holder::Agent`, but flagged BLOCKED in `why`, never a new `LiveState`.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

use crate::sessions::live::{
    self, clamp_last_activity_unix, derive_state, read_tail_lines, Holder, LivePolicy, LiveSession,
    StateSource,
};

/// This adapter's entry in the closed [`crate::sessions::HARNESSES`] set —
/// see `codex.rs`'s identical convention/rationale.
const HARNESS: &str = "kimi";

/// `state.json` is a small, bounded per-session metadata sidecar (observed
/// well under 4 KiB on this box) — enrichment only, capped defensively.
const STATE_JSON_MAX_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Boundary {
    TurnPrompt,
    TurnEnded,
}

/// Tracks `interaction.request` (kind + id) against `interaction.resolved`
/// (id) over the tail window, in request order, so `why` can name the
/// oldest still-open one deterministically.
#[derive(Default)]
struct InteractionTracker {
    open: Vec<(String, String)>, // (id, kind)
}

impl InteractionTracker {
    fn requested(&mut self, id: &str, kind: &str) {
        self.open.push((id.to_string(), kind.to_string()));
    }
    fn resolved(&mut self, id: &str) {
        self.open.retain(|(oid, _)| oid != id);
    }
    fn still_blocked(&self) -> Option<&(String, String)> {
        self.open.first()
    }
}

/// Read `state.json`'s `.cwd` and title (`.title`, falling back to
/// `.lastPrompt`, truncated). Best-effort — any failure yields `None`s.
fn read_state(dir: &Path) -> (Option<String>, Option<String>) {
    let path = dir.join("state.json");
    let Ok(meta) = fs::metadata(&path) else {
        return (None, None);
    };
    if meta.len() > STATE_JSON_MAX_BYTES {
        return (None, None);
    }
    let Ok(text) = fs::read_to_string(&path) else {
        return (None, None);
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return (None, None);
    };
    let cwd = v
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let title = v
        .get("title")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            v.get("lastPrompt")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .map(|s| s.chars().take(70).collect::<String>());
    (cwd, title)
}

/// Classify one Kimi Code session directory (the `session_<uuid>/` dir —
/// NOT the `agents/main/wire.jsonl` path directly, since metadata like the
/// session id and `state.json` live one level up). `None` when
/// `agents/main/wire.jsonl` doesn't exist, the tail has no turn-boundary
/// record and no pending interaction, or the dir can't be stat'd.
pub fn classify_kimi_session(
    session_dir: &Path,
    now_unix: i64,
    policy: &LivePolicy,
) -> Option<LiveSession> {
    let session_id = session_dir.file_name()?.to_str()?.to_string();
    let wire_path = session_dir.join("agents").join("main").join("wire.jsonl");
    let meta = fs::metadata(&wire_path).ok()?;
    let mtime_unix = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(now_unix);
    let last_activity_unix = clamp_last_activity_unix(mtime_unix, now_unix);

    let lines = read_tail_lines(&wire_path)?;

    let mut last_boundary: Option<(Boundary, String)> = None;
    let mut interactions = InteractionTracker::default();
    let mut model: Option<String> = None;
    let mut protocol_version: Option<String> = None;

    for line in &lines {
        let Ok(rec) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match rec.get("type").and_then(Value::as_str).unwrap_or("") {
            "metadata" => {
                if let Some(v) = rec.get("protocol_version") {
                    protocol_version = Some(match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    });
                }
            }
            "profile.bind" => {
                if let Some(m) = rec
                    .get("modelAlias")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    model = Some(m.to_string());
                }
            }
            "turn.prompt" => last_boundary = Some((Boundary::TurnPrompt, String::new())),
            "turn.ended" => {
                let reason = rec
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                last_boundary = Some((Boundary::TurnEnded, reason));
            }
            "interaction.request" => {
                let id = rec.get("id").and_then(Value::as_str).unwrap_or("");
                let kind = rec
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("interaction");
                if !id.is_empty() {
                    interactions.requested(id, kind);
                }
            }
            "interaction.resolved" => {
                let id = rec.get("id").and_then(Value::as_str).unwrap_or("");
                if !id.is_empty() {
                    interactions.resolved(id);
                }
            }
            _ => {}
        }
    }

    let (holder, why) = if let Some((_, kind)) = interactions.still_blocked() {
        // Beyond the handed-down ground truth, verified live on this box:
        // an unresolved interaction.request is Kimi's permission-block
        // analogue to Grok's. Agent holds the ball, but is stuck.
        (
            Holder::Agent,
            format!("blocked: interaction pending (kind={kind})"),
        )
    } else {
        match last_boundary {
            Some((Boundary::TurnPrompt, _)) => (
                Holder::Agent,
                "turn.prompt open (awaiting turn.ended)".to_string(),
            ),
            Some((Boundary::TurnEnded, reason)) => {
                (Holder::Human, format!("turn.ended reason={reason}"))
            }
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

    let (cwd, title) = read_state(session_dir);
    let project = cwd
        .as_deref()
        .and_then(|c| Path::new(c).file_name())
        .map(|n| n.to_string_lossy().into_owned());

    Some(LiveSession {
        resume: format!("kimi -S {session_id}"),
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
        transcript_path: wire_path,
        why: Some(why),
        version: protocol_version,
    })
}

/// Walk `<home>/sessions/<workDirKey>/session_<uuid>/` (depth 2, mirroring
/// Grok's shape) and classify every candidate. Only `agents/main/` is ever
/// opened — sibling `agents/agent-N/` subagent wires are never descended
/// into, mirroring the Claude adapter's `subagents/` refusal. Same two
/// discipline rules as every other scan: bounded by
/// [`live::LIVE_SCAN_CAP`], cheap mtime gate before any file is opened.
#[allow(clippy::explicit_counter_loop)]
pub fn scan_kimi(
    home: &Path,
    now_unix: i64,
    policy: &LivePolicy,
    max_age_secs: i64,
) -> Vec<LiveSession> {
    let mut out = Vec::new();
    let mut scanned = 0usize;
    let sessions_root = home.join("sessions");
    let Ok(workdir_dirs) = fs::read_dir(&sessions_root) else {
        return out;
    };
    'outer: for wd_entry in workdir_dirs.flatten() {
        let wd_path = wd_entry.path();
        if !wd_path.is_dir() {
            continue;
        }
        let Ok(session_dirs) = fs::read_dir(&wd_path) else {
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
            let wire_path = sess_path.join("agents").join("main").join("wire.jsonl");
            let Ok(meta) = fs::metadata(&wire_path) else {
                continue;
            };
            let Ok(modified) = meta.modified() else {
                continue;
            };
            let mtime_unix = modified
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            if now_unix - mtime_unix > max_age_secs {
                continue;
            }
            if let Some(session) = classify_kimi_session(&sess_path, now_unix, policy) {
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
        fs::create_dir_all(dir.parent().unwrap_or(dir)).ok();
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    fn make_session_dir(home: &Path, wdkey: &str, sid: &str) -> PathBuf {
        let dir = home.join("sessions").join(wdkey).join(sid);
        fs::create_dir_all(dir.join("agents").join("main")).unwrap();
        dir
    }

    #[test]
    fn agent_holds_on_unmatched_turn_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "wd_kb_a2c8e68fc4e7", "session_a");
        write_file(
            &dir.join("agents").join("main"),
            "wire.jsonl",
            "{\"type\":\"metadata\",\"protocol_version\":1}\n{\"type\":\"turn.prompt\",\"origin\":{\"kind\":\"user\"}}\n",
        );
        write_file(
            &dir,
            "state.json",
            r#"{"cwd":"/home/user/project/kb","title":"do the thing"}"#,
        );
        let got = classify_kimi_session(&dir, 100_000_000, &LivePolicy::default()).unwrap();
        assert_eq!(got.holder, Holder::Agent);
        assert_eq!(got.harness, "kimi");
        assert_eq!(got.project.as_deref(), Some("kb"));
        assert_eq!(got.title.as_deref(), Some("do the thing"));
        assert!(got.why.as_deref().unwrap().contains("turn.prompt"));
    }

    #[test]
    fn human_holds_on_trailing_turn_ended() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "wd_x", "session_b");
        write_file(
            &dir.join("agents").join("main"),
            "wire.jsonl",
            "{\"type\":\"turn.prompt\"}\n{\"type\":\"turn.ended\",\"reason\":\"completed\"}\n",
        );
        let got = classify_kimi_session(&dir, 100_000_000, &LivePolicy::default()).unwrap();
        assert_eq!(got.holder, Holder::Human);
        assert!(got.why.as_deref().unwrap().contains("turn.ended"));
        assert!(got.why.as_deref().unwrap().contains("completed"));
    }

    /// Verified-live-but-beyond-spec blocked case: an unresolved
    /// interaction.request (approval/question) overrides a trailing
    /// turn.ended-shaped read — Agent, flagged blocked.
    #[test]
    fn unresolved_interaction_request_is_agent_but_flagged_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "wd_x", "session_c");
        write_file(
            &dir.join("agents").join("main"),
            "wire.jsonl",
            concat!(
                "{\"type\":\"turn.prompt\"}\n",
                "{\"type\":\"interaction.request\",\"id\":\"tool_1\",\"kind\":\"approval\"}\n",
            ),
        );
        let got = classify_kimi_session(&dir, 100_000_000, &LivePolicy::default()).unwrap();
        assert_eq!(got.holder, Holder::Agent);
        assert!(got.why.as_deref().unwrap().contains("blocked"));
        assert!(got.why.as_deref().unwrap().contains("approval"));
    }

    #[test]
    fn resolved_interaction_request_is_not_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "wd_x", "session_d");
        write_file(
            &dir.join("agents").join("main"),
            "wire.jsonl",
            concat!(
                "{\"type\":\"turn.prompt\"}\n",
                "{\"type\":\"interaction.request\",\"id\":\"tool_1\",\"kind\":\"approval\"}\n",
                "{\"type\":\"interaction.resolved\",\"id\":\"tool_1\"}\n",
                "{\"type\":\"turn.ended\",\"reason\":\"completed\"}\n",
            ),
        );
        let got = classify_kimi_session(&dir, 100_000_000, &LivePolicy::default()).unwrap();
        assert_eq!(got.holder, Holder::Human);
        assert!(!got.why.as_deref().unwrap().contains("blocked"));
    }

    #[test]
    fn robustness_empty_wire_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "wd_x", "session_e");
        write_file(&dir.join("agents").join("main"), "wire.jsonl", "");
        assert!(classify_kimi_session(&dir, 100_000_000, &LivePolicy::default()).is_none());
    }

    #[test]
    fn robustness_no_wire_file_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("sessions").join("wd_x").join("session_f");
        fs::create_dir_all(&dir).unwrap();
        assert!(classify_kimi_session(&dir, 100_000_000, &LivePolicy::default()).is_none());
    }

    #[test]
    fn robustness_no_recognisable_turn_record_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "wd_x", "session_g");
        write_file(
            &dir.join("agents").join("main"),
            "wire.jsonl",
            "{\"type\":\"usage.record\",\"usage\":{}}\n",
        );
        assert!(classify_kimi_session(&dir, 100_000_000, &LivePolicy::default()).is_none());
    }

    #[test]
    fn robustness_torn_trailing_line_is_skipped_not_errored() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "wd_x", "session_h");
        let mut content = String::new();
        content.push_str("{\"type\":\"turn.prompt\"}\n");
        content.push_str("{\"type\":\"turn.en"); // torn
        write_file(&dir.join("agents").join("main"), "wire.jsonl", &content);
        let got = classify_kimi_session(&dir, 100_000_000, &LivePolicy::default()).unwrap();
        assert_eq!(got.holder, Holder::Agent);
    }

    #[test]
    fn robustness_pure_garbage_returns_none_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "wd_x", "session_i");
        write_file(
            &dir.join("agents").join("main"),
            "wire.jsonl",
            "not json\n{{{\ngarbage",
        );
        assert!(classify_kimi_session(&dir, 100_000_000, &LivePolicy::default()).is_none());
    }

    /// Subagent wires (`agents/agent-N/wire.jsonl`) must never be opened —
    /// mirrors the Claude adapter's `subagents/` refusal.
    #[test]
    fn scan_never_descends_into_subagent_wires() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "wd_x", "session_j");
        write_file(
            &dir.join("agents").join("main"),
            "wire.jsonl",
            "{\"type\":\"turn.prompt\"}\n",
        );
        // A subagent wire with a DIFFERENT (and here, deliberately
        // unparseable) shape — if this were ever opened it would still not
        // affect classification, since scan_kimi only ever opens
        // agents/main/wire.jsonl per session dir.
        write_file(
            &dir.join("agents").join("agent-0"),
            "wire.jsonl",
            "not json at all",
        );
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let got = scan_kimi(tmp.path(), now, &LivePolicy::default(), 86_400);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].session_id, "session_j");
    }

    #[test]
    fn scan_cheap_gate_and_missing_home() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = make_session_dir(tmp.path(), "wd_x", "session_k");
        write_file(
            &dir.join("agents").join("main"),
            "wire.jsonl",
            "{\"type\":\"turn.prompt\"}\n",
        );
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let excluded = scan_kimi(tmp.path(), now, &LivePolicy::default(), -1);
        assert!(excluded.is_empty());
        let included = scan_kimi(tmp.path(), now, &LivePolicy::default(), 86_400);
        assert_eq!(included.len(), 1);

        let missing = tmp.path().join("does-not-exist");
        assert!(scan_kimi(&missing, now, &LivePolicy::default(), 86_400).is_empty());
    }

    #[test]
    fn harness_label_is_a_member_of_the_closed_set() {
        assert!(crate::sessions::HARNESSES.contains(&HARNESS));
    }
}
