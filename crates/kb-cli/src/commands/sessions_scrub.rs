//! v0.44 F7b — the secrets-only floor for every remaining capture lane.
//!
//! Three entry points, one scrubber (`kb_core::session_scrub` with
//! `ScrubOptions::secrets_only()`, the same floor `kb sessions capture` and
//! `kb import claude-history` apply):
//!
//! * [`run_filter`] — `kb sessions scrub`: a stdin→stdout JSONL filter. The
//!   codex/opencode bash adapters hand-write their own envelope (a
//!   `kb-harness` meta `kb sessions capture` does not emit), so they pipe the
//!   translated JSONL through this verb before it is embedded. Adapters
//!   FAIL CLOSED when the filter is unavailable: no capture beats an
//!   unscrubbed one.
//! * [`run_rescrub`] — `kb sessions rescrub [--dry-run|--apply]`: re-scrub
//!   captures that are already on disk (anything written before the floor
//!   existed). Dry-run is the default; files are rewritten only on `--apply`
//!   and only when a lane actually changed, so a second run is a no-op.
//! * [`scan_dir`] — the read-only audit `kb doctor --hooks` prints as a
//!   per-lane data-at-rest table.
//!
//! Lanes of one capture artifact (see `sessions_capture.rs`):
//! `transcript` (the `<pre>` JSONL), `digest` (the structured tail blocks:
//! commits + subagents digest) and `sidecar-text` (raw subagent evidence).
//! The `transcript` lane is scrubbed on the UNESCAPED bytes (the exact input
//! the capture path scrubs) and re-escaped; the two tail lanes are scrubbed
//! in place on their stored text — the redaction markers contain neither
//! `"` nor `&<>`, so JSON validity and HTML structure survive.

use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use kb_core::session_scrub::{scrub_transcript, ScrubOptions};
use kb_core::sessions::{CapturedCommit, SubagentDigest};

/// Scrub the one subagents-digest lane of a fresh capture: the agent id and
/// every file path the digest carries. Returns the redacted digests and the
/// redaction count (folded into the caller's `secrets_redacted`).
pub(crate) fn scrub_subagents(agents: Vec<SubagentDigest>) -> (Vec<SubagentDigest>, u32) {
    let opts = ScrubOptions::secrets_only();
    let mut total = 0u32;
    let mut one = |s: &str| -> String {
        let (out, r) = scrub_transcript(s, &opts);
        total += r.total;
        out
    };
    let agents = agents
        .into_iter()
        .map(|mut a| {
            a.agent_id = one(&a.agent_id);
            for f in &mut a.files {
                f.path = one(&f.path);
            }
            a
        })
        .collect();
    (agents, total)
}

/// Scrub the commits block of a fresh capture (v0.44 X4). The block carries
/// text that never came through the transcript scrub: the TRUE subject and
/// author `git show` returned, the trailers (`Signed-off-by:`, `Co-Authored-By:`
/// and any free-form `Key: value` a developer typed — a token pasted into a
/// commit message lands here) and the repo root path. `rescrub` already
/// covered this lane at rest; this is the same floor at write time, so a fresh
/// capture is not "unscrubbed" the moment it lands. Returns the redacted
/// commits and the redaction count.
pub(crate) fn scrub_commits(commits: Vec<CapturedCommit>) -> (Vec<CapturedCommit>, u32) {
    let opts = ScrubOptions::secrets_only();
    let mut total = 0u32;
    let mut one = |s: String| -> String {
        let (out, r) = scrub_transcript(&s, &opts);
        total += r.total;
        out
    };
    let commits = commits
        .into_iter()
        .map(|mut c| {
            c.subject = c.subject.map(&mut one);
            c.author = c.author.map(&mut one);
            c.repo_root = c.repo_root.map(&mut one);
            c.trailers = c.trailers.into_iter().map(&mut one).collect();
            c
        })
        .collect();
    (commits, total)
}

/// `kb sessions scrub` — stdin → stdout, secrets-only.
pub fn run_filter() -> Result<()> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("read stdin")?;
    let (out, _) = scrub_transcript(&input, &ScrubOptions::secrets_only());
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(out.as_bytes()).context("write stdout")?;
    stdout.flush().context("flush stdout")?;
    Ok(())
}

/// Redaction hits still present per lane of one capture.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct LaneHits {
    pub transcript: u32,
    pub digest: u32,
    pub sidecar_text: u32,
}

impl LaneHits {
    pub fn total(&self) -> u32 {
        self.transcript + self.digest + self.sidecar_text
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn html_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

const SIDECAR_MARKER: &str = r#"id="kb-session-sidecar-text""#;

/// Byte spans of a capture's lanes: the main `<pre>` inner text and the
/// start of the sidecar-text section (the tail digest lane is everything
/// between the `</pre>` and that start, or `</body>` when there is none).
struct Spans {
    pre_inner: (usize, usize),
    digest: (usize, usize),
    sidecar: Option<(usize, usize)>,
}

fn locate(html: &str) -> Option<Spans> {
    let open = html.find("<pre>")? + "<pre>".len();
    let close = open + html[open..].find("</pre>")?;
    let tail_start = close + "</pre>".len();
    let body_end = html.rfind("</body>").unwrap_or(html.len()).max(tail_start);
    let sidecar_start = html[tail_start..body_end].find(SIDECAR_MARKER).map(|rel| {
        // Back up to the opening `<` of the element carrying the id.
        html[tail_start..tail_start + rel]
            .rfind('<')
            .map_or(tail_start + rel, |lt| tail_start + lt)
    });
    Some(match sidecar_start {
        Some(s) => Spans {
            pre_inner: (open, close),
            digest: (tail_start, s),
            sidecar: Some((s, body_end)),
        },
        None => Spans {
            pre_inner: (open, close),
            digest: (tail_start, body_end),
            sidecar: None,
        },
    })
}

/// Scrub every lane of one capture's HTML. Returns the (possibly unchanged)
/// HTML and the per-lane redaction counts.
pub fn scrub_capture_html(html: &str) -> (String, LaneHits) {
    let Some(sp) = locate(html) else {
        return (html.to_string(), LaneHits::default());
    };
    let opts = ScrubOptions::secrets_only();
    let mut hits = LaneHits::default();

    // A lane counts only when scrubbing CHANGED its bytes: a rule that matches
    // and rewrites to the identical text is not a pending redaction (v0.49 RI:
    // the report and the rewrite decision reflect real changes only).
    let pre_in = html_unescape(&html[sp.pre_inner.0..sp.pre_inner.1]);
    let (pre, r) = scrub_transcript(&pre_in, &opts);
    hits.transcript = if pre == pre_in { 0 } else { r.total };
    let digest_in = &html[sp.digest.0..sp.digest.1];
    let (digest, r) = scrub_transcript(digest_in, &opts);
    hits.digest = if digest == digest_in { 0 } else { r.total };
    let sidecar = sp.sidecar.map(|(a, b)| {
        let (s, r) = scrub_transcript(&html[a..b], &opts);
        hits.sidecar_text = if s == html[a..b] { 0 } else { r.total };
        s
    });

    if hits.total() == 0 {
        return (html.to_string(), hits);
    }
    let mut out = String::with_capacity(html.len());
    out.push_str(&html[..sp.pre_inner.0]);
    if hits.transcript > 0 {
        out.push_str(&html_escape(&pre));
    } else {
        out.push_str(&html[sp.pre_inner.0..sp.pre_inner.1]);
    }
    out.push_str(&html[sp.pre_inner.1..sp.digest.0]);
    out.push_str(&digest);
    match (sp.sidecar, sidecar) {
        (Some((_, b)), Some(s)) => {
            out.push_str(&s);
            out.push_str(&html[b..]);
        }
        _ => out.push_str(&html[sp.digest.1..]),
    }
    (out, hits)
}

/// `<meta name="kb-harness" content="X">`, else `claude` (the Claude Code
/// envelope carries no harness meta).
fn harness_of(html: &str) -> String {
    let marker = r#"name="kb-harness" content=""#;
    html.find(marker)
        .and_then(|at| {
            let rest = &html[at + marker.len()..];
            rest.find('"').map(|e| rest[..e].to_string())
        })
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "claude".to_string())
}

fn capture_files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("session-") && n.ends_with(".html"))
        })
        .collect();
    out.sort();
    out
}

/// Read-only audit of a sessions corpus dir: per-harness count of captures
/// and the redaction hits still sitting at rest in each lane.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DirAudit {
    /// Captures actually audited.
    pub captures: usize,
    /// Captures on disk (>= `captures`; the difference is `skipped`).
    pub total_captures: usize,
    /// Captures the audit cap left out (oldest by mtime). 0 = exact.
    pub skipped: usize,
    pub unscrubbed_captures: usize,
    pub unreadable: usize,
    /// harness → (captures, pending hits per lane).
    pub by_harness: BTreeMap<String, HarnessAudit>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct HarnessAudit {
    pub captures: usize,
    pub unscrubbed_captures: usize,
    pub hits: LaneHits,
}

/// How many captures `kb doctor --hooks` audits by default. The audit reads
/// and regex-scrubs EVERY lane of each capture it visits (a multi-MB
/// transcript each, on a corpus that grows by a file per session), so an
/// unbounded walk makes a diagnostic cost minutes of IO on a large corpus.
/// The newest [`DEFAULT_AUDIT_LIMIT`] by mtime are audited and the report says
/// how many were left out; `KB_DOCTOR_SCRUB_LIMIT=0` audits all of them, and
/// `kb sessions rescrub` (dry run) is always the full, exact count.
pub const DEFAULT_AUDIT_LIMIT: usize = 500;

/// `$KB_DOCTOR_SCRUB_LIMIT` -> audit cap (`0` = unlimited), default
/// [`DEFAULT_AUDIT_LIMIT`].
pub fn audit_limit_from_env() -> Option<usize> {
    parse_audit_limit(std::env::var("KB_DOCTOR_SCRUB_LIMIT").ok().as_deref())
}

fn parse_audit_limit(raw: Option<&str>) -> Option<usize> {
    match raw.and_then(|v| v.trim().parse::<usize>().ok()) {
        Some(0) => None,
        Some(n) => Some(n),
        None => Some(DEFAULT_AUDIT_LIMIT),
    }
}

/// Full audit (every capture) — tests only; production audits go through
/// [`scan_dir_limited`] so the cap is always an explicit choice.
#[cfg(test)]
pub fn scan_dir(dir: &Path) -> DirAudit {
    scan_dir_limited(dir, None)
}

/// Audit at most `limit` captures, newest first by mtime (`None` = all).
/// `total_captures` always carries the true count on disk and `skipped` how
/// many the cap left out, so a sampled report can never read as exact.
pub fn scan_dir_limited(dir: &Path, limit: Option<usize>) -> DirAudit {
    let mut audit = DirAudit::default();
    let mut files = capture_files(dir);
    audit.total_captures = files.len();
    if let Some(n) = limit.filter(|n| *n < files.len()) {
        files.sort_by_key(|p| {
            std::cmp::Reverse(
                std::fs::metadata(p)
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH),
            )
        });
        audit.skipped = files.len() - n;
        files.truncate(n);
    }
    for path in files {
        let Ok(html) = std::fs::read_to_string(&path) else {
            audit.unreadable += 1;
            continue;
        };
        let (_, hits) = scrub_capture_html(&html);
        let h = audit.by_harness.entry(harness_of(&html)).or_default();
        audit.captures += 1;
        h.captures += 1;
        if hits.total() > 0 {
            audit.unscrubbed_captures += 1;
            h.unscrubbed_captures += 1;
            h.hits.transcript += hits.transcript;
            h.hits.digest += hits.digest;
            h.hits.sidecar_text += hits.sidecar_text;
        }
    }
    audit
}

/// Plain-text per-lane table (one row per harness) for `kb doctor` and the
/// human `rescrub` report.
pub fn render_lane_table(audit: &DirAudit) -> String {
    let mut s =
        String::from("harness      captures  unscrubbed  transcript  digest  sidecar-text\n");
    for (h, a) in &audit.by_harness {
        s.push_str(&format!(
            "{:<12} {:>8}  {:>10}  {:>10}  {:>6}  {:>12}\n",
            h,
            a.captures,
            a.unscrubbed_captures,
            a.hits.transcript,
            a.hits.digest,
            a.hits.sidecar_text
        ));
    }
    s
}

#[derive(Debug, Serialize)]
pub struct RescrubSummary {
    pub dir: String,
    pub dry_run: bool,
    pub scanned: usize,
    /// Captures with at least one redaction pending (rewritten on --apply).
    pub affected: usize,
    pub rewritten: usize,
    pub unreadable: usize,
    pub redactions: LaneHits,
}

pub fn rescrub(dir: &Path, apply: bool) -> Result<RescrubSummary> {
    if !dir.is_dir() {
        bail!("{} is not a directory", dir.display());
    }
    let mut sum = RescrubSummary {
        dir: dir.display().to_string(),
        dry_run: !apply,
        scanned: 0,
        affected: 0,
        rewritten: 0,
        unreadable: 0,
        redactions: LaneHits::default(),
    };
    for path in capture_files(dir) {
        let Ok(html) = std::fs::read_to_string(&path) else {
            sum.unreadable += 1;
            continue;
        };
        sum.scanned += 1;
        let (out, hits) = scrub_capture_html(&html);
        if hits.total() == 0 {
            continue;
        }
        sum.affected += 1;
        sum.redactions.transcript += hits.transcript;
        sum.redactions.digest += hits.digest;
        sum.redactions.sidecar_text += hits.sidecar_text;
        if apply {
            // Atomic replace, same discipline as `kb sessions capture`: the
            // watcher sees `<name>.html.tmp` (not indexable, ignored) and then
            // the rename over `<name>.html`, which it re-indexes (pinned by
            // `watcher::tests::atomic_tmp_rename_over_an_existing_file_*`).
            // The new file would otherwise take the process umask, silently
            // widening (or narrowing) whatever mode the capture was given, so
            // the original permissions are carried over before the rename.
            let tmp = PathBuf::from(format!("{}.tmp", path.display()));
            let perms = std::fs::metadata(&path)
                .with_context(|| format!("stat {}", path.display()))?
                .permissions();
            std::fs::write(&tmp, &out).with_context(|| format!("write {}", tmp.display()))?;
            std::fs::set_permissions(&tmp, perms)
                .with_context(|| format!("restore mode on {}", tmp.display()))?;
            std::fs::rename(&tmp, &path).with_context(|| format!("finalize {}", path.display()))?;
            sum.rewritten += 1;
        }
    }
    Ok(sum)
}

pub fn run_rescrub(dir: Option<PathBuf>, apply: bool, json: bool) -> Result<()> {
    let dir = match dir {
        Some(d) => d,
        None => match std::env::var_os("KB_SESSIONS_DIR") {
            Some(v) if !v.is_empty() => PathBuf::from(v),
            _ => bail!("no sessions corpus: pass --dir <DIR> or set KB_SESSIONS_DIR"),
        },
    };
    let sum = rescrub(&dir, apply)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&sum)?);
    } else {
        println!(
            "{}: scanned {} capture(s), {} with unscrubbed secrets ({} transcript / {} digest / {} sidecar-text redaction(s)){}",
            if apply { "rescrub --apply" } else { "rescrub (dry run)" },
            sum.scanned,
            sum.affected,
            sum.redactions.transcript,
            sum.redactions.digest,
            sum.redactions.sidecar_text,
            if apply {
                format!(" — rewrote {}", sum.rewritten)
            } else if sum.affected > 0 {
                " — re-run with --apply to rewrite".to_string()
            } else {
                String::new()
            }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kb_core::sessions::{render_sidecar_text_block, SubagentFileEntry};

    const GH: &str = "ghp_0123456789abcdefghijklmnopqrstuvwxyzAB";
    const AWS: &str = "AKIAIOSFODNN7EXAMPLE";

    fn envelope(pre: &str, agents: &[SubagentDigest], sidecars: &[(String, String)]) -> String {
        let mut tail = String::new();
        let b = kb_core::sessions::render_subagents_block(agents);
        if !b.is_empty() {
            tail.push_str(&b);
            tail.push('\n');
        }
        if let Some(b) = render_sidecar_text_block(sidecars) {
            tail.push_str(&b);
            tail.push('\n');
        }
        format!(
            "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\n<meta name=\"kb-harness\" content=\"codex\">\n</head><body>\n<pre>{}</pre>\n{tail}</body></html>\n",
            html_escape(pre)
        )
    }

    fn dirty_envelope() -> String {
        let agents = vec![SubagentDigest {
            agent_id: "a1".into(),
            files: vec![SubagentFileEntry {
                path: format!("/tmp/{GH}/x.rs"),
                action: "edit".into(),
            }],
            ..Default::default()
        }];
        let sidecars = vec![("a1".to_string(), format!("secret {AWS} <tag> & more\n"))];
        envelope(&format!("line <b> & {GH}\n"), &agents, &sidecars)
    }

    #[test]
    fn scrub_subagents_redacts_ids_and_paths() {
        let agents = vec![SubagentDigest {
            agent_id: GH.into(),
            files: vec![SubagentFileEntry {
                path: format!("/tmp/{AWS}/x.rs"),
                action: "edit".into(),
            }],
            ..Default::default()
        }];
        let (out, n) = scrub_subagents(agents);
        assert!(n >= 2, "n={n}");
        let json = serde_json::to_string(&out).unwrap();
        assert!(!json.contains(GH) && !json.contains(AWS), "{json}");
    }

    #[test]
    fn scrub_commits_redacts_subject_author_trailers_and_leaves_shas_alone() {
        let c = CapturedCommit {
            kind: "commit".into(),
            sha: Some("abc1234".into()),
            subject: Some(format!("fix: rotate {GH}")),
            resolved: true,
            sha_full: Some("abc1234abc1234abc1234abc1234abc1234abc1".into()),
            repo_root: Some(format!("/work/{AWS}")),
            author: Some(format!("dev <{AWS}@example.com>")),
            parents: Some(1),
            trailers: vec![
                "Signed-off-by: Dev <dev@example.com>".into(),
                format!("Reviewed-by: key {GH}"),
            ],
        };
        let (out, n) = scrub_commits(vec![c]);
        assert!(n >= 4, "n={n}");
        let json = serde_json::to_string(&out).unwrap();
        assert!(!json.contains(GH) && !json.contains(AWS), "{json}");
        assert_eq!(out[0].sha.as_deref(), Some("abc1234"));
        assert_eq!(
            out[0].sha_full.as_deref(),
            Some("abc1234abc1234abc1234abc1234abc1234abc1")
        );
        assert_eq!(out[0].trailers[0], "Signed-off-by: Dev <dev@example.com>");
        assert!(out[0].resolved && out[0].parents == Some(1));
    }

    #[test]
    fn scrub_capture_html_clears_all_three_lanes_and_keeps_structure() {
        let html = dirty_envelope();
        let (out, hits) = scrub_capture_html(&html);
        assert!(
            hits.transcript >= 1 && hits.digest >= 1 && hits.sidecar_text >= 1,
            "{hits:?}"
        );
        assert!(!out.contains(GH) && !out.contains(AWS), "{out}");
        // Round trips and parses still work after the in-place rewrite.
        let pre = kb_core::sessions::recover_jsonl_from_capture(&out).unwrap();
        assert!(pre.contains("line <b> & "), "{pre}");
        assert!(kb_core::sessions::extract_subagents_block(&out).is_some());
        let side = kb_core::sessions::extract_sidecar_text_block(&out);
        assert_eq!(side.len(), 1);
        assert!(side[0].1.contains("<tag> & more"), "{:?}", side[0].1);
        // Idempotent.
        let (again, h2) = scrub_capture_html(&out);
        assert_eq!(h2.total(), 0);
        assert_eq!(again, out);
    }

    #[test]
    fn rescrub_dry_run_reports_without_writing_then_apply_rewrites_once() {
        let tmp = tempfile::tempdir().unwrap();
        let dirty = dirty_envelope();
        let f = tmp.path().join("session-20260301T090000Z-s1.html");
        std::fs::write(&f, &dirty).unwrap();
        let clean = tmp.path().join("session-20260301T090000Z-s2.html");
        std::fs::write(&clean, envelope("nothing here\n", &[], &[])).unwrap();

        let dry = rescrub(tmp.path(), false).unwrap();
        assert!(dry.dry_run);
        assert_eq!((dry.scanned, dry.affected, dry.rewritten), (2, 1, 0));
        assert_eq!(
            std::fs::read_to_string(&f).unwrap(),
            dirty,
            "dry run must not write"
        );

        let applied = rescrub(tmp.path(), true).unwrap();
        assert_eq!((applied.affected, applied.rewritten), (1, 1));
        let after = std::fs::read_to_string(&f).unwrap();
        assert!(!after.contains(GH) && !after.contains(AWS));

        let again = rescrub(tmp.path(), true).unwrap();
        assert_eq!((again.affected, again.rewritten), (0, 0), "idempotent");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), after);
    }

    /// v0.49 RI — a capture whose env secrets were already masked by a
    /// previous scrub (`KEY=[masked]`) is CLEAN: dry run and apply report zero
    /// and do not rewrite it. v0.48 re-matched its own `[masked]` marker, so
    /// every run reported (and `--apply` rewrote) the same captures.
    #[test]
    fn rescrub_is_idempotent_on_env_secret_markers_and_reports_real_changes_only() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("session-20260301T090000Z-s1.html");
        let pre = "{\"t\":\"export AWS_SECRET_ACCESS_KEY=abc123def456 && x\"}\n{\"t\":\"MY_CREDENTIAL=hunter2hunter2\"}\n";
        let sidecars = vec![(
            "a1".to_string(),
            "STORE_CREDENTIAL=zzzzzzzz9 ok\n".to_string(),
        )];
        std::fs::write(&f, envelope(pre, &[], &sidecars)).unwrap();

        let first = rescrub(tmp.path(), true).unwrap();
        assert_eq!((first.affected, first.rewritten), (1, 1));
        assert!(first.redactions.transcript >= 2 && first.redactions.sidecar_text >= 1);
        let after = std::fs::read_to_string(&f).unwrap();
        assert!(after.contains("AWS_SECRET_ACCESS_KEY=[masked]"), "{after}");

        for apply in [false, true] {
            let again = rescrub(tmp.path(), apply).unwrap();
            assert_eq!(
                (again.affected, again.rewritten, again.redactions.total()),
                (0, 0, 0),
                "apply={apply}"
            );
            assert_eq!(std::fs::read_to_string(&f).unwrap(), after);
        }
        let (again, hits) = scrub_capture_html(&after);
        assert_eq!((hits.total(), again), (0, after));
    }

    /// v0.44 X4 — `rescrub --apply` must not change a capture's permission
    /// bits (the rename'd tmp file would otherwise take the process umask).
    #[cfg(unix)]
    #[test]
    fn rescrub_apply_preserves_the_file_mode() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("session-20260301T090000Z-s1.html");
        std::fs::write(&f, dirty_envelope()).unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o640)).unwrap();
        let applied = rescrub(tmp.path(), true).unwrap();
        assert_eq!(applied.rewritten, 1);
        let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640, "mode changed to {mode:o}");
        // and a 0o600 capture stays private (umask 022 would have made it 644)
        let g = tmp.path().join("session-20260301T090000Z-s3.html");
        std::fs::write(&g, dirty_envelope()).unwrap();
        std::fs::set_permissions(&g, std::fs::Permissions::from_mode(0o600)).unwrap();
        rescrub(tmp.path(), true).unwrap();
        assert_eq!(
            std::fs::metadata(&g).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    /// v0.44 X4 — the capped audit visits the NEWEST captures by mtime and
    /// reports the true total and how many it left out.
    #[test]
    fn scan_dir_limited_samples_newest_and_reports_the_remainder() {
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("session-20260101T000000Z-old.html");
        let new = tmp.path().join("session-20260301T000000Z-new.html");
        let mid = tmp.path().join("session-20260201T000000Z-mid.html");
        // the OLD one is the dirty one; a cap of 2 must leave it out
        std::fs::write(&old, dirty_envelope()).unwrap();
        std::fs::write(&mid, envelope("clean\n", &[], &[])).unwrap();
        std::fs::write(&new, envelope("clean\n", &[], &[])).unwrap();
        let t = |secs| std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        for (p, secs) in [(&old, 1_000), (&mid, 2_000), (&new, 3_000)] {
            std::fs::File::options()
                .write(true)
                .open(p)
                .unwrap()
                .set_modified(t(secs))
                .unwrap();
        }
        let capped = scan_dir_limited(tmp.path(), Some(2));
        assert_eq!(
            (capped.captures, capped.total_captures, capped.skipped),
            (2, 3, 1)
        );
        assert_eq!(
            capped.unscrubbed_captures, 0,
            "the dirty OLD file is out of the sample"
        );
        let full = scan_dir_limited(tmp.path(), None);
        assert_eq!(
            (full.captures, full.skipped, full.unscrubbed_captures),
            (3, 0, 1)
        );
        assert_eq!(scan_dir(tmp.path()).captures, 3);
    }

    #[test]
    fn audit_limit_parses_default_unlimited_and_garbage() {
        assert_eq!(parse_audit_limit(None), Some(DEFAULT_AUDIT_LIMIT));
        assert_eq!(parse_audit_limit(Some("0")), None);
        assert_eq!(parse_audit_limit(Some(" 25 ")), Some(25));
        assert_eq!(parse_audit_limit(Some("many")), Some(DEFAULT_AUDIT_LIMIT));
    }

    #[test]
    fn scan_dir_builds_a_per_harness_lane_table() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("session-20260301T090000Z-s1.html"),
            dirty_envelope(),
        )
        .unwrap();
        let audit = scan_dir(tmp.path());
        assert_eq!((audit.captures, audit.unscrubbed_captures), (1, 1));
        let codex = &audit.by_harness["codex"];
        assert!(
            codex.hits.transcript >= 1 && codex.hits.digest >= 1 && codex.hits.sidecar_text >= 1
        );
        let table = render_lane_table(&audit);
        assert!(
            table.contains("codex") && table.contains("sidecar-text"),
            "{table}"
        );
    }
}
