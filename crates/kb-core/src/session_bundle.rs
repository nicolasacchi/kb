//! `kb-session-bundle/1` — the portable bundle format for cross-machine
//! `claude -r` resume (Session Portability, SP1).
//!
//! A bundle is a plain zip (`<sid>.kbsession.zip`) of:
//!   - `manifest.json` — this module's [`BundleManifest`]: metadata for
//!     placement (cwd → project slug), git verification, and provenance.
//!   - `session.jsonl` — the byte-identical recovered transcript
//!     ([`crate::sessions::recover_jsonl_from_capture`]); the resume payload.
//!
//! Deterministic + LLM-free. The manifest is **decoupled** from the internal
//! [`crate::sessions::SessionActivity`] types on purpose: the wire schema must
//! not be hostage to internal refactors. Both the CLI (`kb sessions export`)
//! and the daemon export route build the SAME shape from this module, so a
//! bundle produced by either is byte-compatible.

use serde::{Deserialize, Serialize};

use crate::session_scrub::{scrub_transcript, ScrubOptions, ScrubReport};
use crate::sessions::{parse_session_activity, SessionActivity};

/// Schema tag stamped into every manifest — bump on a breaking field change.
pub const BUNDLE_SCHEMA: &str = "kb-session-bundle/1";

/// The canonical bundle entry names (kept flat + self-describing).
pub const MANIFEST_ENTRY: &str = "manifest.json";
pub const TRANSCRIPT_ENTRY: &str = "session.jsonl";

/// Derive the `~/.claude/projects/<slug>` folder name Claude Code uses for a
/// working directory: **every non-`[A-Za-z0-9]` character → `-`**, with NO
/// run-collapsing, NO case-folding, NO trimming.
///
/// Verified empirically against every local project dir (0 mismatches):
/// `/home/user/project/kb` → `-home-user-project-kb`; a `/.hidden` segment keeps
/// its dot as a literal extra `-` (`/x/.research` → `-x--research`). A
/// collapsing slugify (squeezing consecutive `-`) writes the WRONG folder and
/// the rehydrated session becomes invisible to `claude -r`, so the
/// non-collapsing behaviour is golden-pinned below.
pub fn claude_project_slug(abs_cwd: &str) -> String {
    abs_cwd
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// One file the session touched, flattened for the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFile {
    pub path: String,
    /// `"read" | "write" | "edit"` (the [`crate::sessions::FileAction`] token).
    pub action: String,
}

/// A steering moment (AskUserQuestion answer / plan approval).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestDecision {
    pub kind: String,
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

/// A detected VCS action (commit/push/tag) — SHA best-effort, never truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestCommit {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

/// A detected research / tool-usage signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestResearch {
    pub kind: String,
    pub query: String,
}

/// Where the bundle came from — provenance for audit + rehydrate messaging.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BundleOrigin {
    /// The kb (corpus) the capture was exported from, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kb: Option<String>,
    /// The source-relative capture filename (`session-<ts>-<sid>.html`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_relative: Option<String>,
    /// The kb version that produced the bundle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exporter_version: Option<String>,
}

/// `manifest.json` — everything needed to place + verify a session on a new
/// machine, plus provenance. Field-for-field stable (golden-pinned); optional
/// scalars are omitted when absent so the JSON stays legible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleManifest {
    /// Always [`BUNDLE_SCHEMA`].
    pub schema: String,
    /// The authoritative Claude Code session id (ground truth). Drives the
    /// `session.jsonl` filename on rehydrate + the `claude -r <id>` command.
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_user_prompt: Option<String>,
    /// The modal working directory — the placement key (→ project slug).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Every distinct cwd the session visited, first-seen order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub all_cwds: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    /// origin remote URL at export time (from the local repo; `None` if the
    /// repo is gone on the exporting box). Populated in SP2.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_remote: Option<String>,
    /// HEAD sha at export time (local repo). Populated in SP2.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_head_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Claude Code `version` seen in the transcript.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cc_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
    pub message_count: u32,
    pub token_total: u64,
    pub tool_calls: u32,
    pub error_count: u32,
    pub files_read_count: u32,
    pub files_edited_count: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<ManifestFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<ManifestDecision>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<ManifestCommit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub research: Vec<ManifestResearch>,
    pub origin: BundleOrigin,
    /// Whether the transcript in this bundle went through the SP2 redaction
    /// pass. `false` = verbatim (same-account default).
    pub scrubbed: bool,
    /// Number of redactions applied when `scrubbed` (0 otherwise).
    pub redactions_applied: u32,
}

impl BundleManifest {
    /// Build the manifest core from a parsed [`SessionActivity`].
    ///
    /// `session_id` is the caller-resolved canonical id (the activity's own
    /// `session_id`, falling back to the capture filename/meta for transcripts
    /// that predate the field). `started_at` is the capture's filename stamp
    /// (the transcript body doesn't carry it). `cc_version` is scanned by the
    /// caller from the JSONL (not part of `SessionActivity`). git_* start
    /// `None` (the transcript carries neither) — the exporter fills them from
    /// the local repo in SP2. `scrubbed`/`redactions_applied` default to the
    /// unredacted state; SP2's scrub pass overwrites them.
    pub fn from_activity(
        session_id: String,
        started_at: Option<i64>,
        cc_version: Option<String>,
        origin: BundleOrigin,
        act: &SessionActivity,
    ) -> Self {
        Self {
            schema: BUNDLE_SCHEMA.to_string(),
            session_id,
            title: act.ai_title.clone(),
            first_user_prompt: act.first_user_prompt.clone(),
            cwd: act.cwd.clone(),
            all_cwds: act.all_cwds.clone(),
            git_branch: act.git_branch.clone(),
            git_remote: None,
            git_head_sha: None,
            model: act.model.clone(),
            cc_version,
            started_at,
            ended_at: act.ended_at,
            message_count: act.message_count,
            token_total: act.token_total,
            tool_calls: act.tool_calls,
            error_count: act.error_count,
            files_read_count: act.files_read_count(),
            files_edited_count: act.files_edited_count(),
            files: act
                .files
                .iter()
                .map(|f| ManifestFile {
                    path: f.path.clone(),
                    action: f.action.as_str().to_string(),
                })
                .collect(),
            decisions: act
                .decisions
                .iter()
                .map(|d| ManifestDecision {
                    kind: d.kind.clone(),
                    prompt: d.prompt.clone(),
                    answer: d.answer.clone(),
                })
                .collect(),
            commits: act
                .commits
                .iter()
                .map(|c| ManifestCommit {
                    kind: c.kind.clone(),
                    sha: c.sha.clone(),
                    subject: c.subject.clone(),
                })
                .collect(),
            research: act
                .research
                .iter()
                .map(|r| ManifestResearch {
                    kind: r.kind.clone(),
                    query: r.query.clone(),
                })
                .collect(),
            origin,
            scrubbed: false,
            redactions_applied: 0,
        }
    }
}

/// First non-empty string value of `field` across the JSONL records (one JSON
/// value per line). The ground-truth `sessionId` + the Claude Code `version`
/// are recovered this way — shared by the CLI export and the daemon route so
/// both derive the manifest identically.
pub fn first_transcript_field(jsonl: &str, field: &str) -> Option<String> {
    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(s) = v.get(field).and_then(|x| x.as_str()) {
                let t = s.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
    }
    None
}

/// Earliest event `timestamp` (unix secs) across the JSONL — the TRUE session
/// start. The capture filename stamp is written at Stop (≈ the END), so it's
/// the wrong anchor; `None` when no record carries a parseable ts (the caller
/// falls back to the filename stamp).
pub fn earliest_event_ts(jsonl: &str) -> Option<i64> {
    let mut earliest: Option<i64> = None;
    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(ts) = v.get("timestamp").and_then(|x| x.as_str()) {
                if let Some(u) = crate::timeparse::parse_iso_utc(ts) {
                    earliest = Some(earliest.map_or(u, |c: i64| c.min(u)));
                }
            }
        }
    }
    earliest
}

/// A fully-assembled bundle payload: the manifest (struct + serialized JSON)
/// and the transcript, both already redacted per the scrub options. The zip is
/// left to the caller (client-side in kb-cli, a streamed response in kb-server)
/// so this stays a pure, IO-free function both share — the "CLI/server/manifest
/// never diverge" guarantee.
pub struct AssembledBundle {
    pub manifest: BundleManifest,
    /// `manifest.json` contents — serialized, then scrubbed too when redacting
    /// (the manifest carries cwd/prompt leaks). Stays valid JSON.
    pub manifest_json: String,
    /// `session.jsonl` contents — the (optionally redacted) transcript.
    pub transcript: String,
    /// What the scrub pass removed (empty when not scrubbing).
    pub report: ScrubReport,
}

/// Build a bundle from a recovered raw JSONL transcript. Pure + deterministic:
/// parses the activity, shapes the manifest (stamping the caller-supplied
/// canonical id / start / cc-version / origin / git provenance), then applies
/// the scrub layers to BOTH the transcript and the serialized manifest when any
/// layer is on. Callers zip `manifest_json` + `transcript` into the
/// `.kbsession.zip`.
#[allow(clippy::too_many_arguments)]
pub fn assemble_bundle(
    jsonl: &str,
    session_id: String,
    started_at: Option<i64>,
    cc_version: Option<String>,
    origin: BundleOrigin,
    git_remote: Option<String>,
    git_head_sha: Option<String>,
    scrub: &ScrubOptions,
) -> AssembledBundle {
    let activity = parse_session_activity(jsonl);
    let mut manifest =
        BundleManifest::from_activity(session_id, started_at, cc_version, origin, &activity);
    manifest.git_remote = git_remote;
    manifest.git_head_sha = git_head_sha;

    let (transcript, report) = if scrub.any() {
        let (scrubbed, report) = scrub_transcript(jsonl, scrub);
        manifest.scrubbed = true;
        manifest.redactions_applied = report.total;
        (scrubbed, report)
    } else {
        (jsonl.to_string(), ScrubReport::default())
    };

    // Serialize AFTER stamping scrubbed/redactions_applied, then scrub the JSON
    // too (cwd/prompt/paths leak) — markers carry no `"`, so it stays valid.
    let manifest_json = serde_json::to_string_pretty(&manifest).unwrap_or_default();
    let manifest_json = if scrub.any() {
        scrub_transcript(&manifest_json, scrub).0
    } else {
        manifest_json
    };

    AssembledBundle {
        manifest,
        manifest_json,
        transcript,
        report,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_matches_claude_code_non_collapsing() {
        // The canonical case + the load-bearing NON-collapsing edges.
        assert_eq!(
            claude_project_slug("/home/user/project/kb"),
            "-home-user-project-kb"
        );
        // A leading `/` → leading `-`; a `/.hidden` segment → a literal `--`
        // (the `/` and the `.` are BOTH non-alphanumeric). A collapsing
        // slugify would wrongly yield `-x-research`.
        assert_eq!(claude_project_slug("/x/.research"), "-x--research");
        // Consecutive separators are NOT squeezed.
        assert_eq!(claude_project_slug("/a//b"), "-a--b");
        // Dots, underscores, spaces all map to `-`; digits + case preserved.
        assert_eq!(
            claude_project_slug("/Users/Bob/My_Proj v2.0"),
            "-Users-Bob-My-Proj-v2-0"
        );
        // Non-ASCII → `-` (matches a JS `[^A-Za-z0-9]` replace).
        assert_eq!(claude_project_slug("/p/caffè"), "-p-caff-");
    }

    #[test]
    fn manifest_from_activity_maps_and_round_trips() {
        let act = SessionActivity {
            session_id: Some("abc-123".into()),
            ai_title: Some("Do the thing".into()),
            cwd: Some("/home/u/proj".into()),
            all_cwds: vec!["/home/u/proj".into(), "/home/u/proj/web".into()],
            git_branch: Some("main".into()),
            first_user_prompt: Some("investigate qwortzle".into()),
            ended_at: Some(1_700_000_100),
            message_count: 12,
            token_total: 4200,
            tool_calls: 7,
            model: Some("claude-opus-4-8".into()),
            error_count: 1,
            ..Default::default()
        };
        let origin = BundleOrigin {
            source_relative: Some("session-20261101T101010Z-abc-123.html".into()),
            exporter_version: Some("0.14.0".into()),
            ..Default::default()
        };
        let m = BundleManifest::from_activity(
            "abc-123".into(),
            Some(1_700_000_000),
            Some("1.2.3".into()),
            origin,
            &act,
        );
        assert_eq!(m.schema, BUNDLE_SCHEMA);
        assert_eq!(m.session_id, "abc-123");
        assert_eq!(m.cwd.as_deref(), Some("/home/u/proj"));
        assert_eq!(m.all_cwds.len(), 2);
        assert_eq!(m.git_branch.as_deref(), Some("main"));
        assert!(m.git_remote.is_none() && m.git_head_sha.is_none());
        assert_eq!(m.cc_version.as_deref(), Some("1.2.3"));
        assert!(!m.scrubbed && m.redactions_applied == 0);

        // Serde round-trip is lossless.
        let json = serde_json::to_string_pretty(&m).unwrap();
        let back: BundleManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
        // Optional-empty fields are omitted; required scalars are present.
        assert!(json.contains("\"schema\": \"kb-session-bundle/1\""));
        assert!(json.contains("\"message_count\": 12"));
        assert!(!json.contains("git_remote")); // None → omitted
    }

    #[test]
    fn assemble_bundle_scrubs_transcript_and_manifest_and_stamps_git() {
        let jsonl = "{\"sessionId\":\"s\",\"cwd\":\"/home/user/p\"}\n\
                     {\"t\":\"key sk-abcdefghijklmnopqrstuvwx\"}\n";
        let opts = ScrubOptions {
            secrets: true,
            paths: true,
            entropy: false,
        };
        let b = assemble_bundle(
            jsonl,
            "s".into(),
            Some(1),
            None,
            BundleOrigin::default(),
            None,
            Some("deadbeef".into()),
            &opts,
        );
        assert!(b.manifest.scrubbed && b.report.total > 0);
        assert_eq!(b.manifest.git_head_sha.as_deref(), Some("deadbeef"));
        // Transcript redacted.
        assert!(!b.transcript.contains("sk-abcdef"));
        assert!(!b.transcript.contains("/home/user"));
        assert!(b.transcript.contains("/home/[user]"));
        // manifest_json is valid JSON, scrubbed (cwd anonymised), consistent.
        let m: BundleManifest = serde_json::from_str(&b.manifest_json).unwrap();
        assert!(m.scrubbed && m.redactions_applied == b.report.total);
        assert!(m.cwd.unwrap().contains("[user]"));

        // No-scrub path → transcript verbatim, empty report.
        let plain = assemble_bundle(
            jsonl,
            "s".into(),
            Some(1),
            None,
            BundleOrigin::default(),
            None,
            None,
            &ScrubOptions::default(),
        );
        assert_eq!(plain.transcript, jsonl);
        assert!(!plain.manifest.scrubbed && plain.report.total == 0);
    }

    #[test]
    fn transcript_field_and_earliest_ts_derivation() {
        let jsonl = "{\"sessionId\":\"sid-1\",\"version\":\"2.1.0\",\"timestamp\":\"2026-01-02T10:00:00Z\"}\n\
                     {\"timestamp\":\"2026-01-01T09:00:00Z\"}\n\
                     {\"no_ts\":1}\n";
        assert_eq!(
            first_transcript_field(jsonl, "sessionId").as_deref(),
            Some("sid-1")
        );
        assert_eq!(
            first_transcript_field(jsonl, "version").as_deref(),
            Some("2.1.0")
        );
        assert!(first_transcript_field(jsonl, "absent").is_none());
        // The MIN timestamp wins (true start), not the first/last record.
        assert_eq!(
            earliest_event_ts(jsonl),
            crate::timeparse::parse_iso_utc("2026-01-01T09:00:00Z")
        );
        assert!(earliest_event_ts("{\"no_ts\":1}\n").is_none());
    }
}
