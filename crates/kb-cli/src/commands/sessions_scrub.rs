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
use kb_core::sessions::SubagentDigest;

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

    let (pre, r) = scrub_transcript(&html_unescape(&html[sp.pre_inner.0..sp.pre_inner.1]), &opts);
    hits.transcript = r.total;
    let (digest, r) = scrub_transcript(&html[sp.digest.0..sp.digest.1], &opts);
    hits.digest = r.total;
    let sidecar = sp.sidecar.map(|(a, b)| {
        let (s, r) = scrub_transcript(&html[a..b], &opts);
        hits.sidecar_text = r.total;
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
    pub captures: usize,
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

pub fn scan_dir(dir: &Path) -> DirAudit {
    let mut audit = DirAudit::default();
    for path in capture_files(dir) {
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
            // Atomic replace, same discipline as `kb sessions capture`.
            let tmp = PathBuf::from(format!("{}.tmp", path.display()));
            std::fs::write(&tmp, &out).with_context(|| format!("write {}", tmp.display()))?;
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
