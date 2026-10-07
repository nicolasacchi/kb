//! kb-code W0.4 — `kb sessions capture`: builds the SAME
//! `session-<ts>-<sid>.html` capture artifact the Stop-hook
//! (`plugins/kb-memory/hooks/kb-capture.sh`) writes, but in Rust — so it can
//! resolve a session's detected git commits (full sha / true subject /
//! author / parent count / trailers, `kb_core::vcs::resolve_commit`) via ONE
//! `git show -s` per sha before the artifact is written, something a bash
//! heredoc can't do. The resolution rides the envelope as an additive tail
//! block (`kb_core::sessions::render_commits_block`) so the daemon's
//! enrichment — which parses the ARTIFACT, never the live repo — can persist
//! it (`SessionCaptureHook`, `crates/kb-core/src/enrich.rs`).
//!
//! Filesystem-only, same division as `kb import claude-history`
//! (`commands::import`): this verb never talks to the daemon — the watcher
//! indexes the written file like any other artifact. Distinct from the
//! daemon-facing `kb capture` quick-capture verb (`commands::capture`),
//! which POSTs to a running daemon's `capture/` staging folder.
//!
//! `kb-capture.sh` shells out to this verb when the `kb` binary is on PATH,
//! falling back to its own bash heredoc (no commit resolution) on any
//! failure here or when `kb` is absent — this verb is additive, never
//! load-bearing for the capture pipeline's baseline behaviour.
//!
//! W0.5 — alongside commit resolution, this verb also walks the session's
//! sidecar directory (`<transcript-dir>/<session-id>/subagents/agent-*.jsonl`
//! — the on-disk convention `kb import claude-history`'s main walk already
//! knows to exclude `subagents/workflows/**` from) and rides the parse as a
//! second additive tail block (`kb_core::sessions::render_subagents_block`),
//! so the daemon's `SessionCaptureHook` can attribute subagent file activity
//! and compute sidecar-primary aggregate stats (covers ASYNC delegations the
//! parent transcript's own `toolUseResult` never carries numbers for). W0.6
//! reuses the SAME walk ([`collect_sidecars`](super::import::collect_sidecars))
//! to also ride each sidecar's raw text as a third additive tail block
//! (`kb_core::sessions::render_sidecar_text_block`) — searchable evidence
//! (BM25/`kb recollect`), distinct from the structured digest.
//!
//! W5/R10 — [`collect_extra_sidecar_sources`](super::import::collect_extra_sidecar_sources)
//! extends that SAME sidecar-text list (same caps, same block) with two more
//! sources: `subagents/workflows/<wf_id>/journal.jsonl` (a workflow-launched
//! subagent's ONLY durable evidence, verified live — it has no sibling
//! `agent-*.jsonl` the way a direct Task-tool delegation does) and any
//! TaskOutput truncation-marker path still present on disk at capture time.
//! Neither feeds the STRUCTURED subagents digest block (that stays scoped
//! to direct `agent-*.jsonl` sidecars only) — evidence-only, matching the
//! sidecar-text block's existing "raw evidence, not the digest" contract.
//!
//! W5.2 (trust-review rec (b)) — every stored transcript LANE (the main
//! `<pre>` JSONL and the W0.6 sidecar-text evidence blocks) is passed
//! through `kb_core::session_scrub::scrub_transcript` with
//! `ScrubOptions::secrets_only()` (the safe floor: known token shapes +
//! labeled secrets, `paths`/`entropy` stay off) right before it is
//! escaped/embedded. Always on, no new config knob — a call-site fix, not a
//! configurable subsystem. The scrub runs on the FULL raw bytes, before any
//! truncation budgeting (`collect_extra_sidecar_sources`'s caps,
//! `render_sidecar_text_block`'s per-agent budget), so a secret straddling a
//! truncation boundary can never survive as an unmatched fragment; it never
//! touches `sessionId` (no secrets-layer rule matches that key or a
//! UUID-shaped value), so canonical-id recovery off the STORED (now
//! redacted) artifact is unaffected. Metadata extraction above this point —
//! `parse_session_activity`, git commit resolution, the sidecar walk itself —
//! still reads the UNSCRUBBED bytes; only what gets EMBEDDED in the artifact
//! is redacted.

use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

use kb_core::sessions::CapturedCommit;
use kb_core::vcs::resolve_commit;

use super::import::{
    collect_extra_sidecar_sources, collect_sidecars, expand_tilde, sanitize_sid,
    scrub_capture_lanes, wrap_envelope_spooled,
};

/// The env var kb-capture.sh resolves its target from — mirrored exactly so
/// the shell wrapper needs no new configuration to shell out to this verb.
const SESSIONS_DIR_ENV: &str = "KB_SESSIONS_DIR";

/// What one capture run did, for `--json` + tests.
#[derive(Debug, Clone, Serialize)]
pub struct CaptureSummary {
    /// The canonical (unsanitised) session id.
    pub session_id: String,
    pub path: String,
    /// `true` when an existing `session-*-<sid>.html` was reused (a
    /// re-capture of a live session) rather than a fresh filename minted.
    pub reused_existing: bool,
    pub commits_detected: usize,
    pub commits_resolved: usize,
    /// W0.5 — number of `agent-*.jsonl` sidecars found and folded into the
    /// subagents digest block (0 when the session has no sidecar dir, or it
    /// exists but is empty). W0.6 reuses the same walk for the sidecar-text
    /// block, so this count also covers how many agents contributed raw
    /// evidence text.
    pub subagents_captured: usize,
    /// W0.6 — `true` when at least one sidecar's raw text exceeded its
    /// [`kb_core::sessions::SIDECAR_TEXT_AGENT_CAP_BYTES`] /
    /// [`kb_core::sessions::SIDECAR_TEXT_TOTAL_CAP_BYTES`] budget and was
    /// HEAD/TAIL-truncated in the sidecar-text tail block. `false` when
    /// there are no sidecars at all (nothing to truncate).
    pub sidecar_text_truncated: bool,
    /// W5.2 — total redaction count from `kb_core::session_scrub` across
    /// EVERY stored transcript lane (the main transcript + all sidecar-text
    /// entries), so a `--json` consumer can see at a glance whether anything
    /// was caught without grepping the artifact for `[redacted:…]` markers.
    /// `0` when the secrets-only pass found nothing to redact.
    pub secrets_redacted: u32,
    /// `true` when a REPLAYED snapshot was older than the capture already in
    /// the corpus and was therefore NOT written (the existing file is
    /// untouched). Only [`replay_spool`] can produce this.
    pub skipped_stale: bool,
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    transcript: PathBuf,
    session_id: Option<String>,
    cwd: Option<PathBuf>,
    out: Option<PathBuf>,
    json: bool,
    allow_oversized: bool,
    stamp: Option<String>,
) -> Result<()> {
    let out_dir = resolve_out_dir(out)?;
    if let Some(st) = stamp.as_deref() {
        if !valid_compact_stamp(st) {
            bail!("--stamp must look like YYYYMMDDTHHMMSSZ (UTC), got {st:?}");
        }
    }
    let summary = capture_with(
        &transcript,
        session_id.as_deref(),
        cwd.as_deref(),
        &out_dir,
        allow_oversized,
        stamp.as_deref(),
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        // W5.2 — only surface the redaction count when something was
        // actually caught (same "silent when zero" style as the truncation/
        // reuse suffixes below).
        let redacted_suffix = if summary.secrets_redacted > 0 {
            format!(" [{} secret(s) redacted]", summary.secrets_redacted)
        } else {
            String::new()
        };
        println!(
            "captured {} -> {} ({} commit(s), {} resolved · {} subagent(s)){}{}{}",
            summary.session_id,
            summary.path,
            summary.commits_detected,
            summary.commits_resolved,
            summary.subagents_captured,
            redacted_suffix,
            if summary.sidecar_text_truncated {
                " [sidecar text truncated]"
            } else {
                ""
            },
            if summary.reused_existing {
                " [reused existing capture]"
            } else {
                ""
            },
        );
    }
    Ok(())
}

/// v0.44 X6 — the private spool the capture hooks write a RAW transcript to
/// when `kb sessions capture` fails. It lives OUTSIDE every corpus (never
/// indexed, never watched): the old bash fallback embedded the raw bytes in
/// the corpus unscrubbed. `$KB_CAPTURE_SPOOL` wins; else
/// `$KB_CACHE_DIR/capture-spool`; else `$XDG_CACHE_HOME/kb/capture-spool`;
/// else `$HOME/.cache/kb/capture-spool` (the exact order kb-capture.sh uses).
pub fn spool_dir() -> Option<PathBuf> {
    let env = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if let Some(p) = env("KB_CAPTURE_SPOOL") {
        return Some(p);
    }
    if let Some(p) = env("KB_CACHE_DIR") {
        return Some(p.join("capture-spool"));
    }
    if let Some(p) = env("XDG_CACHE_HOME") {
        return Some(p.join("kb").join("capture-spool"));
    }
    env("HOME").map(|h| h.join(".cache").join("kb").join("capture-spool"))
}

/// True when `item` still carries the mtime read before it was processed (a
/// producer that re-parks a newer snapshot over it changes the mtime).
fn item_unchanged(item: &Path, snap: Option<std::time::SystemTime>) -> bool {
    std::fs::metadata(item).and_then(|m| m.modified()).ok() == snap
}

/// Outcome of one `--replay-spool` run.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ReplaySummary {
    pub replayed: usize,
    pub failed: usize,
    /// Items dropped because the corpus already held a capture published after
    /// the item's snapshot (a stale replay never overwrites a fresher capture).
    pub dropped_stale: usize,
}

/// Replay every spooled item (`<key>.jsonl` + optional `<key>.meta` carrying
/// `session_id=` / `cwd=` lines) through [`capture`] — the scrubbing path —
/// and delete each on success. A failure leaves the item for the next run.
pub async fn replay_spool(spool: &Path, out_dir: &Path) -> Result<ReplaySummary> {
    let mut sum = ReplaySummary::default();
    let rd = match std::fs::read_dir(spool) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(sum),
        Err(e) => return Err(e).with_context(|| format!("reading spool {}", spool.display())),
    };
    let mut items: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("jsonl"))
        .collect();
    items.sort();
    for jsonl in items {
        let meta = jsonl.with_extension("meta");
        let (mut sid, mut cwd, mut stamp) = (None, None, None);
        if let Ok(m) = std::fs::read_to_string(&meta) {
            for line in m.lines() {
                match line.split_once('=') {
                    Some(("session_id", v)) if !v.is_empty() => sid = Some(v.to_string()),
                    Some(("cwd", v)) if !v.is_empty() => cwd = Some(PathBuf::from(v)),
                    // v0.45 N4 - the adapter's true session-start stamp, so a
                    // replayed capture keeps its start time in the filename.
                    // An invalid value is ignored (never fails the replay).
                    Some(("stamp", v)) if valid_compact_stamp(v) => stamp = Some(v.to_string()),
                    _ => {}
                }
            }
        }
        // A cwd that no longer exists must not fail the replay (it is only a
        // commit-resolution hint).
        let cwd = cwd.filter(|c| c.is_dir());
        // The snapshot's own age: the item is a copy made when the adapter
        // parked it, so its mtime is the instant the snapshot was taken. A
        // capture published after that is fresher and must win.
        let snap = std::fs::metadata(&jsonl).and_then(|m| m.modified()).ok();
        match capture_inner(
            &jsonl,
            sid.as_deref(),
            cwd.as_deref(),
            out_dir,
            false,
            stamp.as_deref(),
            snap,
        )
        .await
        {
            Ok(done) => {
                if done.skipped_stale {
                    eprintln!(
                        "kb sessions capture --replay-spool: dropped stale spool item {} \
                         (the corpus already holds a fresher capture of session {})",
                        jsonl.display(),
                        done.session_id
                    );
                    sum.dropped_stale += 1;
                } else {
                    sum.replayed += 1;
                }
                // A producer may have re-parked a NEWER snapshot over this item
                // (`mv -f`) while we processed the old one: delete only what we
                // actually read (same mtime as at the stat), else leave the
                // fresher item for the next replay.
                if !item_unchanged(&jsonl, snap) {
                    continue;
                }
                let _ = std::fs::remove_file(&jsonl);
                let _ = std::fs::remove_file(&meta);
                // v0.45 N10 - parked subagent sidecars (`<spool>/<sid>/
                // subagents`, written by the omp hook) were folded into the
                // capture above; drop them with the item. Plain ids only, so a
                // crafted session_id can never point this at another path.
                if let Some(s) = sid.as_deref().filter(|s| {
                    !s.is_empty()
                        && s.chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                }) {
                    let _ = std::fs::remove_dir_all(spool.join(s));
                }
            }
            Err(e) => {
                eprintln!(
                    "kb sessions capture --replay-spool: {}: {e:#}",
                    jsonl.display()
                );
                sum.failed += 1;
            }
        }
    }
    Ok(sum)
}

pub async fn run_replay_spool(out: Option<PathBuf>, json: bool) -> Result<()> {
    let out_dir = resolve_out_dir(out)?;
    let Some(spool) = spool_dir() else {
        bail!("no capture spool dir: set KB_CAPTURE_SPOOL or HOME");
    };
    let sum = replay_spool(&spool, &out_dir).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&sum)?);
    } else {
        println!(
            "spool: {} replayed, {} dropped stale, {} failed ({})",
            sum.replayed,
            sum.dropped_stale,
            sum.failed,
            spool.display()
        );
    }
    if sum.failed > 0 {
        bail!("{} spooled capture(s) could not be replayed", sum.failed);
    }
    Ok(())
}

/// Resolve the target sessions-corpus dir: `--out` wins; else
/// `$KB_SESSIONS_DIR` (exactly how kb-capture.sh resolves its target); else a
/// hard error naming both.
fn resolve_out_dir(out: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = out {
        return Ok(expand_tilde(p));
    }
    match std::env::var_os(SESSIONS_DIR_ENV) {
        Some(v) if !v.is_empty() => Ok(expand_tilde(PathBuf::from(v))),
        _ => bail!(
            "no target sessions corpus: pass --out <DIR> or set {SESSIONS_DIR_ENV} \
             (the same env var kb-capture.sh uses)"
        ),
    }
}

/// `YYYYMMDDTHHMMSSZ`: 8 digits, `T`, 6 digits, `Z`, with in-range fields.
pub(crate) fn valid_compact_stamp(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 16 || b[8] != b'T' || b[15] != b'Z' {
        return false;
    }
    if !b[..8].iter().chain(&b[9..15]).all(u8::is_ascii_digit) {
        return false;
    }
    let n = |r: std::ops::Range<usize>| s[r].parse::<u32>().unwrap_or(99);
    (1..=12).contains(&n(4..6))
        && (1..=31).contains(&n(6..8))
        && n(9..11) < 24
        && n(11..13) < 60
        && n(13..15) < 61
}

/// The whole capture, printing-free — `run` renders this (and tests assert
/// on it directly).
#[cfg(test)]
async fn capture(
    transcript: &Path,
    session_id_hint: Option<&str>,
    cwd_hint: Option<&Path>,
    out_dir: &Path,
    allow_oversized: bool,
) -> Result<CaptureSummary> {
    capture_with(
        transcript,
        session_id_hint,
        cwd_hint,
        out_dir,
        allow_oversized,
        None,
    )
    .await
}

/// [`capture`] plus `stamp`: the compact UTC timestamp to use as the filename
/// ts of a NEW file (an adapter passes the session's true start time). Ignored
/// when an existing capture is reused - the original name's ts is load-bearing
/// for `started_at` (invariant #11) and is never renamed.
async fn capture_with(
    transcript: &Path,
    session_id_hint: Option<&str>,
    cwd_hint: Option<&Path>,
    out_dir: &Path,
    allow_oversized: bool,
    stamp: Option<&str>,
) -> Result<CaptureSummary> {
    capture_inner(
        transcript,
        session_id_hint,
        cwd_hint,
        out_dir,
        allow_oversized,
        stamp,
        None,
    )
    .await
}

/// Per-session publish lock directory: `$KB_CAPTURE_LOCKS`, else
/// `$KB_CACHE_DIR/capture-locks`, else `$XDG_CACHE_HOME/kb/capture-locks`, else
/// `$HOME/.cache/kb/capture-locks` - the SAME directory the shell adapters
/// keep their per-session locks in (kb-hook-lib.sh `hook_capture_lock_dir`),
/// under a distinct `publish-` file prefix.
fn publish_lock_dir() -> Option<PathBuf> {
    let env = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if let Some(p) = env("KB_CAPTURE_LOCKS") {
        return Some(p);
    }
    if let Some(p) = env("KB_CACHE_DIR") {
        return Some(p.join("capture-locks"));
    }
    if let Some(p) = env("XDG_CACHE_HOME") {
        return Some(p.join("kb").join("capture-locks"));
    }
    env("HOME").map(|h| h.join(".cache").join("kb").join("capture-locks"))
}

/// Exclusive advisory lock held across ONE capture's write + rename (a
/// millisecond-scale section, NOT the adapter's long per-session lock). It
/// makes "is the file I am about to replace fresher than my snapshot?" and the
/// rename a single step against every other writer of the same session, so a
/// replay can never rename a stale snapshot over a capture that landed between
/// its check and its rename. Best effort: with no resolvable lock dir the
/// caller proceeds unlocked (the check still runs). Released on drop.
struct PublishLock {
    _file: Option<std::fs::File>,
}

impl PublishLock {
    fn acquire(sid: &str) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let Some(dir) = publish_lock_dir() else {
                return Self { _file: None };
            };
            if std::fs::create_dir_all(&dir).is_err() {
                return Self { _file: None };
            }
            let f = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(dir.join(format!("publish-{sid}.plock")));
            match f {
                Ok(f) => {
                    // SAFETY: flock on a valid, owned fd.
                    let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) };
                    Self {
                        _file: if rc == 0 { Some(f) } else { None },
                    }
                }
                Err(_) => Self { _file: None },
            }
        }
        #[cfg(not(unix))]
        {
            let _ = sid;
            Self { _file: None }
        }
    }
}

/// `capture_with` plus `stale_before`: when set (a REPLAYED spool item, whose
/// value is the snapshot's own mtime), an existing capture of the session that
/// was published AFTER that instant is strictly fresher - the snapshot is
/// dropped (`skipped_stale`), never written. A direct capture passes `None`:
/// it is always the newest view of the transcript.
async fn capture_inner(
    transcript: &Path,
    session_id_hint: Option<&str>,
    cwd_hint: Option<&Path>,
    out_dir: &Path,
    allow_oversized: bool,
    stamp: Option<&str>,
    stale_before: Option<std::time::SystemTime>,
) -> Result<CaptureSummary> {
    // 2026-08-21 ci-host incident, defect 1: a 292MB raw Codex rollout (77%
    // base64 screenshots) entered capture through THIS path — bypassing
    // the harness adapter's jq whitelist entirely — and OOM-looped the
    // daemon. The main-transcript `<pre>` this writes is a byte-identical
    // `claude -r` resume contract (invariant #11,
    // `kb_core::sessions::recover_jsonl_from_capture`), so an oversized
    // transcript is REFUSED here, before it's ever read into memory —
    // never truncated.
    let meta_len = std::fs::metadata(transcript)
        .with_context(|| format!("stat transcript {}", transcript.display()))?
        .len();
    if kb_core::sessions::transcript_size_verdict(
        meta_len,
        kb_core::sessions::CAPTURE_MAX_TRANSCRIPT_BYTES,
        allow_oversized,
    ) == kb_core::sessions::TranscriptSizeVerdict::Refuse
    {
        bail!(
            "transcript {} is {meta_len} bytes, over the \
             {}-byte capture cap (kb_core::sessions::CAPTURE_MAX_TRANSCRIPT_BYTES) \
             — a RAW transcript this large is refused, never truncated (the \
             main transcript <pre> is a byte-identical `claude -r` resume \
             contract). A harness adapter (kb-capture-codex.sh etc.) produces \
             a bounded translation instead of raw passthrough; pass \
             --allow-oversized to capture it verbatim anyway.",
            transcript.display(),
            kb_core::sessions::CAPTURE_MAX_TRANSCRIPT_BYTES,
        );
    }

    let raw = std::fs::read_to_string(transcript)
        .with_context(|| format!("read transcript {}", transcript.display()))?;

    let activity = kb_core::sessions::parse_session_activity(&raw);

    // Canonical id: the transcript's own JSONL `sessionId` is ground truth
    // (invariant #11) — it wins over the caller-supplied hint (the hook's
    // stdin `.session_id`), which is only a fallback for a transcript that
    // predates the field.
    let raw_sid = activity
        .session_id
        .clone()
        .or_else(|| session_id_hint.map(str::to_string))
        .filter(|s| !s.trim().is_empty());
    let Some(raw_sid) = raw_sid else {
        bail!(
            "no session id recoverable: the transcript carries no sessionId \
             and no --session-id was given"
        );
    };
    let sid = sanitize_sid(&raw_sid);

    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("create sessions dir {}", out_dir.display()))?;

    // Reuse behaviour: kb-capture.sh keeps writing to the SAME filename
    // across re-captures of a live session, so the original start timestamp
    // survives in the name (the parser derives `started_at` from it —
    // invariant #11's note on the truncated-meta bug applies here too: the
    // filename is load-bearing). Mirror the hook's glob-then-take-last
    // exactly (see `find_existing_capture`).
    let existing = find_existing_capture(out_dir, &sid, &raw_sid);
    let capture_ts = stamp
        .filter(|s| valid_compact_stamp(s))
        .map(str::to_string)
        .unwrap_or_else(compact_utc_now);
    let out_path = existing
        .clone()
        .unwrap_or_else(|| out_dir.join(format!("session-{capture_ts}-{sid}.html")));

    // Capture-time commit resolution: ONE `git show -s` per detected sha,
    // against the record cwd (an explicit `--cwd` override, else the
    // transcript's own modal cwd — invariant #11-adjacent "detected, not
    // ground truth" ethos: no cwd recorded ⇒ no resolution attempted, never
    // a guess). `resolve_commit` itself never errors — a missing repo, a
    // rebased-away sha, or a non-repo cwd all degrade to `resolved: false`.
    let record_cwd = cwd_hint
        .map(Path::to_path_buf)
        .or_else(|| activity.cwd.as_deref().map(PathBuf::from));
    let mut commits_resolved = 0usize;
    let mut captured: Vec<CapturedCommit> = Vec::with_capacity(activity.commits.len());
    for c in &activity.commits {
        let resolution = match (&record_cwd, &c.sha) {
            (Some(cwd), Some(sha)) => Some(resolve_commit(cwd, sha).await),
            _ => None,
        };
        let resolved = resolution.as_ref().is_some_and(|r| r.resolved);
        if resolved {
            commits_resolved += 1;
        }
        captured.push(CapturedCommit {
            kind: c.kind.clone(),
            sha: c.sha.clone(),
            // The TRUE subject wins when resolution succeeded; the
            // transcript-detected one survives otherwise (never fabricated).
            subject: resolution
                .as_ref()
                .and_then(|r| r.subject.clone())
                .or_else(|| c.subject.clone()),
            resolved,
            sha_full: resolution.as_ref().and_then(|r| r.sha_full.clone()),
            repo_root: resolution.as_ref().and_then(|r| r.repo_root.clone()),
            author: resolution.as_ref().and_then(|r| r.author.clone()),
            parents: resolution.as_ref().and_then(|r| r.parents),
            trailers: resolution.map(|r| r.trailers).unwrap_or_default(),
        });
    }

    // W0.5/W0.6 — walk the session's sidecar directory ONCE for both the
    // subagent digest AND (W0.6) the raw sidecar text each agent's
    // `agent-*.jsonl` carries. Named by the RAW canonical session id (Claude
    // Code's own on-disk convention, `<transcript-dir>/<session-id>/
    // subagents/`) — NOT the kb-sanitized `sid`, since kb never renamed that
    // directory.
    let sidecars_dir = transcript
        .parent()
        .map(|dir| dir.join(&raw_sid).join("subagents"));
    let walk = sidecars_dir
        .as_deref()
        .map(collect_sidecars)
        .unwrap_or_default();
    let (subagents, walked, walked_redactions) = (walk.digests, walk.spools, walk.redactions);
    // W5/R10 — workflow journals + TaskOutput snapshots, folded into the
    // SAME sidecar-text list (same caps, per-source labels — see
    // `collect_extra_sidecar_sources`'s doc comment).
    let extras = sidecars_dir
        .as_deref()
        .map(|dir| collect_extra_sidecar_sources(dir, &raw))
        .unwrap_or_default();

    // W5.2 (trust-review rec (b)) — scrub every stored transcript lane on
    // the RAW bytes, before html-escaping AND before the sidecar-text
    // truncation budget below: `scrub_transcript` is regex/entropy-only and
    // byte-safe for JSONL (its markers contain no `"` and never span a
    // newline — see the module doc), so line structure survives untouched
    // for every downstream JSONL parse. `secrets_only()` is the deliberate
    // safe floor (known token shapes + labeled secrets; `paths`/`entropy`
    // stay off) with no new config knob — this redaction is always on.
    // Metadata already extracted above (session id, cwd, resolved commits)
    // came from the UNSCRUBBED `raw` on purpose; only what's about to be
    // EMBEDDED is redacted from here on.
    let (raw, sidecar_texts, mut secrets_redacted) =
        scrub_capture_lanes(&raw, extras, walked, walked_redactions);
    // v0.44 F7b — the structured subagents digest (agent ids + file paths) is
    // a stored lane too; it was the one lane the floor skipped.
    let (subagents, digest_redacted) = super::sessions_scrub::scrub_subagents(subagents);
    secrets_redacted += digest_redacted;
    // v0.44 X4 — the commits block is the other stored tail lane: the true
    // subject/author/trailers come from `git show`, never through the
    // transcript scrub above.
    let (captured, commits_redacted) = super::sessions_scrub::scrub_commits(captured);
    secrets_redacted += commits_redacted;

    let html = wrap_envelope_spooled(
        &capture_ts,
        &sid,
        &raw,
        &captured,
        &subagents,
        &sidecar_texts,
    );
    // Pure budget math over the sidecar RAW lengths
    // (`kb_core::sessions::sidecar_text_truncates`), NOT a substring scan of
    // the rendered HTML: `html.contains("kb-sidecar-text: truncated")` used
    // to false-positive whenever the MAIN transcript legitimately contained
    // that literal string (e.g. a session working on this very sidecar-text
    // feature) — the marker text lives in the escaped `<pre>` transcript
    // same as any other assistant prose, so a whole-artifact scan can't tell
    // "the session talked about truncation" from "a sidecar was actually
    // truncated".
    let sidecar_text_truncated = kb_core::sessions::sidecar_text_truncates_spooled(&sidecar_texts);
    // Atomic replace: write to a `.tmp` sibling and rename into place, same
    // as kb-capture.sh's `mv -f "$tmp" "$out"` - the watcher never ingests a
    // half-written multi-MB transcript. The whole write + freshness check +
    // rename runs under the per-session publish lock (see `PublishLock`), and
    // the existing capture is RE-RESOLVED under it: a replay that raced a live
    // capture must see the file the live capture just published (and must not
    // mint a second filename for the session).
    let _publish = PublishLock::acquire(&sid);
    let existing_now = find_existing_capture(out_dir, &sid, &raw_sid);
    let out_path = existing_now.clone().unwrap_or(out_path);
    let mut skipped_stale = false;
    if let (Some(snap), Some(cur)) = (stale_before, existing_now.as_ref()) {
        if let Ok(published) = std::fs::metadata(cur).and_then(|m| m.modified()) {
            skipped_stale = published > snap;
        }
    }
    if !skipped_stale {
        let tmp = PathBuf::from(format!("{}.tmp", out_path.display()));
        std::fs::write(&tmp, &html)
            .with_context(|| format!("write capture tmp {}", tmp.display()))?;
        std::fs::rename(&tmp, &out_path)
            .with_context(|| format!("finalize capture {}", out_path.display()))?;
    }

    Ok(CaptureSummary {
        session_id: raw_sid,
        path: out_path.display().to_string(),
        reused_existing: existing.is_some() || existing_now.is_some(),
        commits_detected: activity.commits.len(),
        commits_resolved,
        subagents_captured: subagents.len(),
        sidecar_text_truncated,
        secrets_redacted,
        skipped_stale,
    })
}

/// The bash hook's `for f in "$dir"/session-*-"$sid.html"; do [ -f "$f" ] &&
/// out="$f"; done` — shell glob expansion is lexically sorted, and the loop
/// keeps overwriting `out` with each match, so the net effect is the
/// lexicographically-GREATEST match (= chronologically latest, since the
/// `<ts>` prefix is fixed-width and sorts correctly as a string either way).
///
/// Also matches the STALE hook's `session-<ts>-<sid>-.html` variant (a
/// trailing dash before `.html` — `jq -r .session_id`'s trailing newline
/// survived `tr -c 'a-zA-Z0-9' '-'` as an extra dash on every pre-fix
/// capture): without this, a continuing session whose only capture is a
/// legacy file is never found, minting a fresh clean-named duplicate and
/// stranding the legacy one. Clean matches always win over legacy ones when
/// both exist for the same sid — reuse standardises on the clean filename
/// going forward and never resurrects a stale legacy file.
///
/// v0.45 N4 - a third rung recognises a capture written under the pre-v0.45
/// LOSSY name (`tr -c ... | cut -c1-80`, which maps "a_b"/"a-b" and ids that
/// share an 80-char prefix to one name). Such a file is reused only when its
/// own embedded `sessionId` / `kb-session` meta equals `raw_sid`, so a
/// colliding id's file is never adopted. It is reused in place (name
/// unchanged), never renamed or duplicated.
fn find_existing_capture(out_dir: &Path, sid: &str, raw_sid: &str) -> Option<PathBuf> {
    let legacy_dash = format!("{sid}-");
    // Strict `session-<ts>-<name>.html` parse: the name must EQUAL the key
    // (a bare `ends_with("-{sid}.html")` let sid "b" match "...-a-b.html").
    let names: Vec<(PathBuf, String)> = std::fs::read_dir(out_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter_map(|p| {
            let n = p.file_name()?.to_str()?.to_string();
            let stem = n.strip_suffix(".html")?.strip_prefix("session-")?;
            let (ts, name) = stem.split_once('-')?;
            (ts.len() == 16 && ts.ends_with('Z')).then(|| (p.clone(), name.to_string()))
        })
        .collect();

    // The file's own embedded id (JSONL ground truth, else the meta) when it
    // names a DIFFERENT session: a pre-v0.45 lossy name was shared by distinct
    // ids, so a name match alone must never adopt a colliding id's file.
    let foreign = |p: &Path| {
        std::fs::read_to_string(p).is_ok_and(|html| {
            kb_core::sessions::recover_jsonl_from_capture(&html)
                .and_then(|j| kb_core::session_bundle::first_transcript_field(&j, "sessionId"))
                .is_some_and(|id| id != raw_sid)
        })
    };
    let owns = |p: &Path| {
        std::fs::read_to_string(p).is_ok_and(|html| {
            kb_core::sessions::recover_jsonl_from_capture(&html)
                .and_then(|j| kb_core::session_bundle::first_transcript_field(&j, "sessionId"))
                .is_some_and(|s| s == raw_sid)
                || super::import::meta_session(&html).is_some_and(|m| m == raw_sid)
        })
    };
    let newest = |want: &dyn Fn(&str) -> bool, ok: &dyn Fn(&Path) -> bool| {
        let mut m: Vec<&PathBuf> = names
            .iter()
            .filter(|(_, n)| want(n.as_str()))
            .map(|(p, _)| p)
            .collect();
        m.sort();
        m.into_iter().rev().find(|p| ok(p.as_path())).cloned()
    };

    // Clean name wins over the stale trailing-dash variant; both rejected
    // when they embed another session's id.
    if let Some(p) = newest(&|n| n == sid, &|p| !foreign(p))
        .or_else(|| newest(&|n| n == legacy_dash, &|p| !foreign(p)))
    {
        return Some(p);
    }

    // Migration rung: the pre-v0.45 lossy name, reused ONLY when the file
    // verifiably holds this raw id; reused in place, never renamed.
    let lossy = super::import::legacy_lossy_sid(raw_sid);
    if lossy == sid {
        return None;
    }
    let lossy_dash = format!("{lossy}-");
    newest(&|n| n == lossy || n == lossy_dash, &owns)
}

/// Is `part` exactly `<of>-p<digits>` with a part number of at least 2? The
/// shape the omp adapter mints for a continuation part (and the one
/// `kb_core::sessions` accepts as a segment link); part 1 keeps the bare id.
fn is_continuation_part_id(part: &str, of: &str) -> bool {
    let Some(rest) = part.strip_prefix(of).and_then(|r| r.strip_prefix("-p")) else {
        return false;
    };
    !of.is_empty()
        && !rest.is_empty()
        && rest.bytes().all(|b| b.is_ascii_digit())
        && rest.parse::<u64>().is_ok_and(|n| n >= 2)
}

/// v0.46 SEG-PR2 - `kb sessions drop-part`: remove the capture file(s) of ONE
/// continuation part of a segmented session, so the daemon's own delete
/// cascade (R2: the watcher sees the file leave the corpus) clears its index
/// rows, sidecars and comments. The omp adapter calls it when a rewind or
/// `/clear` behind a frozen boundary makes the stored parts orphans. The
/// adapter itself never removes anything from the corpus: this verb finds the
/// file the same way the writer does ([`find_existing_capture`]) and refuses
/// anything that is not VERIFIED to be that part (its own embedded `sessionId`
/// is `part_id` AND its `segmentOf` is `segment_of`; part 1 / a plain session
/// is never droppable). Idempotent: nothing found is success with an empty
/// list. Filesystem-only.
pub fn run_drop_part(
    out: Option<PathBuf>,
    part_id: &str,
    segment_of: &str,
    json: bool,
) -> Result<()> {
    if !is_continuation_part_id(part_id, segment_of) {
        bail!("--session-id must be <segment-of>-p<NN> with NN >= 2, got {part_id:?} of {segment_of:?}");
    }
    let out_dir = resolve_out_dir(out)?;
    let key = super::import::sanitize_sid(part_id);
    let mut dropped: Vec<PathBuf> = Vec::new();
    let mut refused: Vec<PathBuf> = Vec::new();
    // `find_existing_capture` returns the newest candidate; loop until none is
    // left (a legacy duplicate), refusing - and then stopping on - any file it
    // finds that is not verifiably this part.
    for _ in 0..8 {
        let Some(path) = find_existing_capture(&out_dir, &key, part_id) else {
            break;
        };
        let verified = std::fs::read_to_string(&path).is_ok_and(|html| {
            kb_core::sessions::recover_jsonl_from_capture(&html).is_some_and(|j| {
                kb_core::session_bundle::first_transcript_field(&j, "sessionId").as_deref()
                    == Some(part_id)
                    && kb_core::session_bundle::first_transcript_field(&j, "segmentOf").as_deref()
                        == Some(segment_of)
            })
        });
        if !verified {
            refused.push(path);
            break;
        }
        std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
        dropped.push(path);
    }
    if json {
        println!(
            "{}",
            serde_json::json!({
                "session_id": part_id,
                "dropped": dropped,
                "refused": refused,
            })
        );
    } else {
        for p in &dropped {
            println!("dropped {}", p.display());
        }
        for p in &refused {
            eprintln!("refused {} (not verifiably part {part_id})", p.display());
        }
    }
    if refused.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("refused to drop an unverified file"))
    }
}

/// Unix-now → `YYYYMMDDTHHMMSSZ`, the kb-capture.sh `date -u
/// +%Y%m%dT%H%M%SZ` stamp.
fn compact_utc_now() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

#[cfg(test)]
mod tests {
    use super::super::import::wrap_envelope;
    use super::*;
    use kb_core::sessions::parse_session_html_full;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, body).unwrap();
        p
    }

    /// Identity AND both dates are pinned, so the fixture repo's commit sha
    /// is a CONSTANT rather than a fresh random value per run.
    ///
    /// This is not tidiness — it is a ~3.7 % CI flake, diagnosed 2026-09-06.
    /// `sessions::parse_git_sha` deliberately requires a hex token to carry
    /// at least one `a`–`f` (so a line count like `1234567` in git output is
    /// never mistaken for a sha). With an unpinned commit date the 7-char
    /// abbreviation this fixture feeds back through that grammar was a fresh
    /// sha every run, and `(10/16)^7 ≈ 3.7 %` of the time it came out ALL
    /// DECIMAL — the sha was then dropped, `commits_resolved` fell to 0, and
    /// `capture_writes_the_envelope_with_resolved_commit_block` failed on an
    /// unrelated PR with a bare `left: 0, right: 1`. Pinning the dates makes
    /// the sha `ba57f42dd3af…`, which is stable and carries letters.
    fn run_git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.email=test@kb",
                "-c",
                "user.name=kb-test",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }

    fn only_html(dir: &Path) -> PathBuf {
        walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_map(Result::ok)
            .find(|e| e.path().extension().and_then(|x| x.to_str()) == Some("html"))
            .unwrap()
            .into_path()
    }

    fn count_html(dir: &Path) -> usize {
        walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("html"))
            .count()
    }

    /// A transcript with one `git commit` whose Bash result carries a short
    /// sha, in a scratch repo `capture()` can resolve against via `cwd`.
    fn fixture_with_commit(sid: &str, cwd: &str, sha: &str) -> String {
        format!(
            r#"{{"sessionId":"{sid}","type":"user","cwd":"{cwd}","timestamp":"2026-03-01T09:00:00.000Z","message":{{"role":"user","content":"ship it"}},"promptSource":"typed"}}
{{"sessionId":"{sid}","type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"b1","name":"Bash","input":{{"command":"git commit -F msg.txt"}}}}]}}}}
{{"sessionId":"{sid}","type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"b1","content":"[main {sha}] whatever transcript subject"}}]}}}}
"#
        )
    }

    /// v0.44 X4 — the commits block is scrubbed at WRITE time: a token typed
    /// into a commit subject or trailer (text `git show` returns, which never
    /// passes the transcript scrub) must not be stored in the envelope.
    #[tokio::test]
    async fn capture_scrubs_secrets_out_of_the_commits_block() {
        const GH: &str = "ghp_0123456789abcdefghijklmnopqrstuvwxyzAB";
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        run_git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("a.txt"), "one").unwrap();
        run_git(&repo, &["add", "a.txt"]);
        run_git(
            &repo,
            &[
                "commit",
                "-q",
                "-m",
                &format!("feat: rotate {GH}\n\nKb-Session: sess-cap-2\nReviewed-by: key {GH}"),
            ],
        );
        let head = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let head_sha = String::from_utf8(head.stdout).unwrap().trim().to_string();
        let short = &head_sha[..7];
        assert!(
            short.bytes().any(|b| b.is_ascii_alphabetic()),
            "fixture commit abbreviates to {short:?} (all-decimal); re-pin the fixture"
        );

        let jsonl = fixture_with_commit("sess-cap-2", repo.to_str().unwrap(), short);
        let transcript = write(&tmp.path().join("src"), "t.jsonl", &jsonl);
        let out_dir = tmp.path().join("sessions");
        let summary = capture(&transcript, None, None, &out_dir, false)
            .await
            .unwrap();
        assert_eq!(summary.commits_resolved, 1);
        assert!(summary.secrets_redacted >= 2, "{summary:?}");

        let html = std::fs::read_to_string(&summary.path).unwrap();
        assert!(!html.contains(GH), "token reached the stored envelope");
        let block = kb_core::sessions::extract_commits_block(&html).unwrap();
        assert!(block[0].resolved);
        assert_eq!(block[0].sha_full.as_deref(), Some(head_sha.as_str()));
        assert!(
            block[0]
                .subject
                .as_deref()
                .unwrap()
                .starts_with("feat: rotate "),
            "{:?}",
            block[0].subject
        );
        assert!(block[0]
            .trailers
            .iter()
            .any(|t| t == "Kb-Session: sess-cap-2"));
    }

    #[tokio::test]
    async fn capture_writes_the_envelope_with_resolved_commit_block() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        run_git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("a.txt"), "one").unwrap();
        run_git(&repo, &["add", "a.txt"]);
        run_git(
            &repo,
            &[
                "commit",
                "-q",
                "-m",
                "feat: real subject\n\nKb-Session: sess-cap-1",
            ],
        );
        let head = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let head_sha = String::from_utf8(head.stdout).unwrap().trim().to_string();
        let short = &head_sha[..7];
        // The grammar this fixture feeds (`sessions::parse_git_sha`) skips a
        // hex token with no `a`–`f` letter, so an all-decimal abbreviation
        // would silently drop the sha and fail the `commits_resolved`
        // assertion below with no clue why. `run_git` pins the commit dates
        // to keep this constant; fail HERE, by name, if a future fixture edit
        // moves it onto an all-decimal sha.
        assert!(
            short.bytes().any(|b| b.is_ascii_alphabetic()),
            "fixture commit abbreviates to {short:?}, which `parse_git_sha` \
             skips as all-decimal — re-pin the fixture so the sha carries a letter"
        );

        let sid = "sess-cap-1";
        let jsonl = fixture_with_commit(sid, repo.to_str().unwrap(), short);
        let transcript = write(&tmp.path().join("src"), "t.jsonl", &jsonl);
        let out_dir = tmp.path().join("sessions");

        let summary = capture(&transcript, None, None, &out_dir, false)
            .await
            .unwrap();
        assert_eq!(summary.session_id, sid);
        assert_eq!(summary.commits_detected, 1);
        assert_eq!(summary.commits_resolved, 1);
        assert!(!summary.reused_existing);
        assert_eq!(count_html(&out_dir), 1);

        let html = std::fs::read_to_string(&summary.path).unwrap();
        assert!(html.contains(r#"<meta name="kb-category" content="memory-session">"#));
        assert!(html.contains(&format!(r#"<meta name="kb-session" content="{sid}">"#)));

        let block = kb_core::sessions::extract_commits_block(&html).unwrap();
        assert_eq!(block.len(), 1);
        assert!(block[0].resolved);
        assert_eq!(block[0].sha_full.as_deref(), Some(head_sha.as_str()));
        assert_eq!(block[0].subject.as_deref(), Some("feat: real subject"));
        assert_eq!(
            block[0].trailers,
            vec!["Kb-Session: sess-cap-1".to_string()]
        );

        // The daemon's own recovery path still reconstructs the JSONL byte-
        // identically, and the canonical id round-trips.
        let name = Path::new(&summary.path)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let recovered = kb_core::sessions::recover_jsonl_from_capture(&html).unwrap();
        assert_eq!(recovered, jsonl);
        let facts = parse_session_html_full(&html, &name, 0).facts;
        assert_eq!(facts.session_id, sid);
    }

    /// W0.5/W0.6 — end-to-end: a main transcript + a sidecar directory
    /// holding two `agent-*.jsonl` files (one with file edits + token usage,
    /// one read-only) + a `subagents/workflows/` subdir that must be
    /// ignored. `capture()` must derive the sidecar dir from the transcript
    /// path + the RAW session id, fold both sidecars into the digest block
    /// AND (W0.6) the sidecar-text block, and leave the `<pre>` transcript
    /// untouched.
    #[tokio::test]
    async fn capture_walks_sidecars_into_a_subagents_digest_block() {
        let tmp = tempfile::tempdir().unwrap();
        let sid = "sess-cap-subagents-1";
        let jsonl = format!(
            "{{\"sessionId\":\"{sid}\",\"type\":\"user\",\"timestamp\":\"2026-03-01T09:00:00.000Z\",\"message\":{{\"role\":\"user\",\"content\":\"go\"}},\"promptSource\":\"typed\"}}\n"
        );
        let src_dir = tmp.path().join("src");
        let transcript = write(&src_dir, "t.jsonl", &jsonl);

        // Sidecar dir: <transcript-dir>/<session-id>/subagents/ — the raw,
        // un-sanitized session id (Claude Code's own on-disk convention).
        let sidecars = src_dir.join(sid).join("subagents");
        let a1_raw = "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"model\":\"claude\",\"usage\":{\"input_tokens\":100,\"output_tokens\":50},\"content\":[{\"type\":\"tool_use\",\"name\":\"Edit\",\"input\":{\"file_path\":\"/p/edited.rs\"}}]}}\n";
        write(&sidecars, "agent-a1.jsonl", a1_raw);
        let a2_raw = "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"name\":\"Read\",\"input\":{\"file_path\":\"/p/readonly.rs\"}}]}}\n";
        write(&sidecars, "agent-a2.jsonl", a2_raw);
        // A workflow journal, sibling to the agent-*.jsonl files — W5/R10:
        // folded into the sidecar-TEXT evidence block (workflow:wf_1), but
        // NEVER into the structured subagents DIGEST block (that stays
        // scoped to direct agent-*.jsonl sidecars only).
        let wf_journal = "{\"type\":\"progress\"}\n";
        write(
            &sidecars.join("workflows/wf_1"),
            "journal.jsonl",
            wf_journal,
        );

        let out_dir = tmp.path().join("sessions");
        let summary = capture(&transcript, None, None, &out_dir, false)
            .await
            .unwrap();
        assert_eq!(summary.subagents_captured, 2, "exactly the two sidecars");
        assert!(!summary.sidecar_text_truncated, "well under the caps");

        let html = std::fs::read_to_string(&summary.path).unwrap();
        let block = kb_core::sessions::extract_subagents_block(&html).unwrap();
        assert_eq!(block.agents.len(), 2);
        assert!(!block.truncated);
        let a1 = block.agents.iter().find(|a| a.agent_id == "a1").unwrap();
        assert_eq!(a1.tokens, 150);
        assert_eq!(a1.files[0].path, "/p/edited.rs");
        assert_eq!(a1.files[0].action, "edit");
        let a2 = block.agents.iter().find(|a| a.agent_id == "a2").unwrap();
        assert_eq!(a2.files[0].path, "/p/readonly.rs");
        assert_eq!(a2.files[0].action, "read");

        // W0.6 — the sidecar-text block carries each agent's RAW jsonl,
        // untruncated (both are tiny), keyed by the SAME resolved agent id.
        // W5/R10 — PLUS the workflow journal, keyed `workflow:<wf_id>`.
        let sidecar_texts = kb_core::sessions::extract_sidecar_text_block(&html);
        assert_eq!(sidecar_texts.len(), 3, "{sidecar_texts:?}");
        let a1_text = sidecar_texts
            .iter()
            .find(|(id, _)| id == "a1")
            .map(|(_, t)| t.as_str())
            .unwrap();
        assert_eq!(a1_text, a1_raw);
        let a2_text = sidecar_texts
            .iter()
            .find(|(id, _)| id == "a2")
            .map(|(_, t)| t.as_str())
            .unwrap();
        assert_eq!(a2_text, a2_raw);
        let wf_text = sidecar_texts
            .iter()
            .find(|(id, _)| id == "workflow:wf_1")
            .map(|(_, t)| t.as_str())
            .unwrap();
        assert_eq!(wf_text, wf_journal);

        // The `<pre>` transcript round-trips byte-identically regardless of
        // the appended tail blocks.
        let recovered = kb_core::sessions::recover_jsonl_from_capture(&html).unwrap();
        assert_eq!(recovered, jsonl);
    }

    /// A session with no sidecar directory at all must omit BOTH the
    /// subagents digest block and the W0.6 sidecar-text block entirely —
    /// zero behavior change from pre-W0.5/W0.6 captures.
    #[tokio::test]
    async fn capture_without_a_sidecar_dir_omits_the_subagents_block() {
        let tmp = tempfile::tempdir().unwrap();
        let sid = "sess-cap-nosubagents-1";
        let jsonl = format!(
            "{{\"sessionId\":\"{sid}\",\"type\":\"user\",\"timestamp\":\"2026-03-01T09:00:00.000Z\",\"message\":{{\"role\":\"user\",\"content\":\"solo\"}},\"promptSource\":\"typed\"}}\n"
        );
        let transcript = write(&tmp.path().join("src"), "t.jsonl", &jsonl);
        let out_dir = tmp.path().join("sessions");

        let summary = capture(&transcript, None, None, &out_dir, false)
            .await
            .unwrap();
        assert_eq!(summary.subagents_captured, 0);
        assert!(!summary.sidecar_text_truncated);
        let html = std::fs::read_to_string(&summary.path).unwrap();
        assert!(kb_core::sessions::extract_subagents_block(&html).is_none());
        assert!(kb_core::sessions::extract_sidecar_text_block(&html).is_empty());
        assert!(!html.contains("kb-session-sidecar-text"));
    }

    /// W0.6 — a sidecar whose raw JSONL exceeds
    /// [`kb_core::sessions::SIDECAR_TEXT_AGENT_CAP_BYTES`] is HEAD/TAIL-
    /// truncated in the rendered block, and `CaptureSummary::
    /// sidecar_text_truncated` surfaces that so `--json` consumers don't
    /// have to grep the artifact for the marker themselves.
    #[tokio::test]
    async fn capture_flags_sidecar_text_truncation_in_the_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let sid = "sess-cap-sidecar-trunc-1";
        let jsonl = format!(
            "{{\"sessionId\":\"{sid}\",\"type\":\"user\",\"timestamp\":\"2026-03-01T09:00:00.000Z\",\"message\":{{\"role\":\"user\",\"content\":\"go\"}},\"promptSource\":\"typed\"}}\n"
        );
        let src_dir = tmp.path().join("src");
        let transcript = write(&src_dir, "t.jsonl", &jsonl);

        let sidecars = src_dir.join(sid).join("subagents");
        // One line comfortably over SIDECAR_TEXT_AGENT_CAP_BYTES (2 MiB).
        let oversized = format!(
            "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"{}\"}}]}}}}\n",
            "x".repeat(kb_core::sessions::SIDECAR_TEXT_AGENT_CAP_BYTES + 4096)
        );
        write(&sidecars, "agent-big.jsonl", &oversized);

        let out_dir = tmp.path().join("sessions");
        let summary = capture(&transcript, None, None, &out_dir, false)
            .await
            .unwrap();
        assert_eq!(summary.subagents_captured, 1);
        assert!(
            summary.sidecar_text_truncated,
            "an over-cap sidecar must flip the summary flag"
        );

        let html = std::fs::read_to_string(&summary.path).unwrap();
        assert!(html.contains("kb-sidecar-text: truncated"));
    }

    /// Review-fold: the OLD `sidecar_text_truncated` derivation
    /// (`html.contains("kb-sidecar-text: truncated")`) false-positived
    /// whenever the MAIN transcript legitimately contained that literal
    /// string — e.g. a session working on THIS repo's sidecar-text
    /// truncation feature. `sidecar_text_truncated` now derives from
    /// `kb_core::sessions::sidecar_text_truncates`'s pure budget math over
    /// the sidecar RAW lengths, so a marker string sitting in the main
    /// `<pre>` — with every actual sidecar comfortably under budget — must
    /// NOT flip the flag.
    #[tokio::test]
    async fn capture_does_not_false_positive_on_marker_text_in_the_main_transcript() {
        let tmp = tempfile::tempdir().unwrap();
        let sid = "sess-cap-sidecar-falsepos-1";
        let msg =
            "yesterday the logs said kb-sidecar-text: truncated and we fixed the false positive";
        let jsonl = format!(
            "{{\"sessionId\":\"{sid}\",\"type\":\"user\",\"timestamp\":\"2026-03-01T09:00:00.000Z\",\"message\":{{\"role\":\"user\",\"content\":\"{msg}\"}},\"promptSource\":\"typed\"}}\n"
        );
        let src_dir = tmp.path().join("src");
        let transcript = write(&src_dir, "t.jsonl", &jsonl);

        // A real sidecar, comfortably under budget — nothing should truncate.
        let sidecars = src_dir.join(sid).join("subagents");
        write(&sidecars, "agent-a1.jsonl", "{\"type\":\"assistant\"}\n");

        let out_dir = tmp.path().join("sessions");
        let summary = capture(&transcript, None, None, &out_dir, false)
            .await
            .unwrap();
        assert_eq!(summary.subagents_captured, 1);

        let html = std::fs::read_to_string(&summary.path).unwrap();
        assert!(
            html.contains("kb-sidecar-text: truncated"),
            "the marker string must land verbatim in the main <pre> for this test to be meaningful"
        );
        assert!(
            !summary.sidecar_text_truncated,
            "marker text living in the MAIN transcript must not false-positive the flag; \
             the one sidecar is nowhere near its budget"
        );
    }

    /// W5.2 (trust-review rec (b)) — a transcript carrying a known-secret-
    /// shaped canary must be stored REDACTED, not verbatim. `canary` is
    /// copied byte-for-byte from `kb_core::session_scrub`'s own
    /// `redacts_known_token_shapes` fixture set so this test can never drift
    /// from what the scrubber actually matches. Also pins that the
    /// canonical `sessionId` — untouched by the secrets-only layer, since no
    /// rule matches that key or a UUID/slug-shaped value — still recovers
    /// correctly off the now-redacted artifact (round-trip contract,
    /// invariant #11).
    #[tokio::test]
    async fn capture_redacts_known_secrets_from_the_main_transcript() {
        let tmp = tempfile::tempdir().unwrap();
        let sid = "sess-cap-secret-1";
        let canary = "AKIAIOSFODNN7EXAMPLE";
        let jsonl = format!(
            "{{\"sessionId\":\"{sid}\",\"type\":\"user\",\"timestamp\":\"2026-03-01T09:00:00.000Z\",\"message\":{{\"role\":\"user\",\"content\":\"here is my key {canary} please use it\"}},\"promptSource\":\"typed\"}}\n"
        );
        let transcript = write(&tmp.path().join("src"), "t.jsonl", &jsonl);
        let out_dir = tmp.path().join("sessions");

        let summary = capture(&transcript, None, None, &out_dir, false)
            .await
            .unwrap();
        assert_eq!(summary.session_id, sid);
        assert!(
            summary.secrets_redacted >= 1,
            "the AWS canary must be counted as a redaction"
        );

        let html = std::fs::read_to_string(&summary.path).unwrap();
        assert!(
            !html.contains(canary),
            "the raw secret must never reach the stored artifact: {html}"
        );
        assert!(
            html.contains("[redacted:aws-access-key-id]"),
            "the scrubber's marker must be present: {html}"
        );

        // The canonical session id still round-trips off the SCRUBBED
        // artifact — the secrets-only layer never touches `sessionId`.
        let recovered = kb_core::sessions::recover_jsonl_from_capture(&html).unwrap();
        assert!(!recovered.contains(canary), "recovered jsonl: {recovered}");
        assert!(recovered.contains(&format!("\"sessionId\":\"{sid}\"")));
        let name = Path::new(&summary.path)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let facts = parse_session_html_full(&html, &name, 0).facts;
        assert_eq!(facts.session_id, sid, "sessionId recovery survives scrub");
    }

    /// W5.2 — the same redaction must apply to the W0.6 sidecar-text
    /// evidence lane (a subagent's `agent-*.jsonl`), not just the main
    /// `<pre>`. `canary` is copied byte-for-byte from
    /// `kb_core::session_scrub`'s own `redacts_known_token_shapes` fixture.
    #[tokio::test]
    async fn capture_redacts_known_secrets_from_the_sidecar_text_block() {
        let tmp = tempfile::tempdir().unwrap();
        let sid = "sess-cap-secret-sidecar-1";
        let jsonl = format!(
            "{{\"sessionId\":\"{sid}\",\"type\":\"user\",\"timestamp\":\"2026-03-01T09:00:00.000Z\",\"message\":{{\"role\":\"user\",\"content\":\"go\"}},\"promptSource\":\"typed\"}}\n"
        );
        let src_dir = tmp.path().join("src");
        let transcript = write(&src_dir, "t.jsonl", &jsonl);

        let canary = "ghp_0123456789abcdefghijklmnopqrstuvwxyzAB";
        let sidecars = src_dir.join(sid).join("subagents");
        let agent_raw = format!(
            "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"token {canary} in the clear\"}}]}}}}\n"
        );
        write(&sidecars, "agent-a1.jsonl", &agent_raw);

        let out_dir = tmp.path().join("sessions");
        let summary = capture(&transcript, None, None, &out_dir, false)
            .await
            .unwrap();
        assert_eq!(summary.subagents_captured, 1);
        assert!(
            summary.secrets_redacted >= 1,
            "the github-token canary in the sidecar must be counted"
        );

        let html = std::fs::read_to_string(&summary.path).unwrap();
        assert!(
            !html.contains(canary),
            "the raw sidecar secret must never reach the stored artifact: {html}"
        );

        let sidecar_texts = kb_core::sessions::extract_sidecar_text_block(&html);
        let a1_text = sidecar_texts
            .iter()
            .find(|(id, _)| id == "a1")
            .map(|(_, t)| t.as_str())
            .unwrap();
        assert!(!a1_text.contains(canary), "{a1_text}");
        assert!(a1_text.contains("[redacted:github-token]"), "{a1_text}");

        // The main transcript's sessionId round-trips unaffected by the
        // sidecar-lane redaction.
        let recovered = kb_core::sessions::recover_jsonl_from_capture(&html).unwrap();
        assert!(recovered.contains(&format!("\"sessionId\":\"{sid}\"")));
    }

    /// v0.44 F7b — the structured subagents DIGEST block (agent ids + file
    /// paths) is a stored lane too; before this it was the one lane the
    /// secrets floor skipped.
    #[tokio::test]
    async fn capture_redacts_known_secrets_from_the_subagents_digest_block() {
        let tmp = tempfile::tempdir().unwrap();
        let sid = "sess-cap-secret-digest-1";
        let jsonl = format!(
            "{{\"sessionId\":\"{sid}\",\"type\":\"user\",\"timestamp\":\"2026-03-01T09:00:00.000Z\",\"message\":{{\"role\":\"user\",\"content\":\"go\"}},\"promptSource\":\"typed\"}}\n"
        );
        let src_dir = tmp.path().join("src");
        let transcript = write(&src_dir, "t.jsonl", &jsonl);
        let canary = "ghp_0123456789abcdefghijklmnopqrstuvwxyzAB";
        let agent_raw = format!(
            "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"name\":\"Edit\",\"input\":{{\"file_path\":\"/work/{canary}/x.rs\"}}}}]}}}}\n"
        );
        write(
            &src_dir.join(sid).join("subagents"),
            "agent-a1.jsonl",
            &agent_raw,
        );

        let out_dir = tmp.path().join("sessions");
        let summary = capture(&transcript, None, None, &out_dir, false)
            .await
            .unwrap();
        let html = std::fs::read_to_string(&summary.path).unwrap();
        let block = kb_core::sessions::extract_subagents_block(&html).unwrap();
        let json = serde_json::to_string(&block).unwrap();
        assert!(!json.contains(canary), "digest block leaked: {json}");
        assert!(json.contains("[redacted:github-token]"), "{json}");
        assert!(!html.contains(canary), "{html}");
    }

    #[tokio::test]
    async fn capture_with_no_git_activity_omits_the_commits_block() {
        let tmp = tempfile::tempdir().unwrap();
        let sid = "sess-cap-nogit";
        let jsonl = format!(
            r#"{{"sessionId":"{sid}","type":"user","timestamp":"2026-03-01T09:00:00.000Z","message":{{"role":"user","content":"just chatting"}},"promptSource":"typed"}}
"#
        );
        let transcript = write(&tmp.path().join("src"), "t.jsonl", &jsonl);
        let out_dir = tmp.path().join("sessions");

        let summary = capture(&transcript, None, None, &out_dir, false)
            .await
            .unwrap();
        assert_eq!(summary.commits_detected, 0);
        assert_eq!(summary.commits_resolved, 0);
        let html = std::fs::read_to_string(&summary.path).unwrap();
        assert!(!html.contains("<script"));
        assert!(kb_core::sessions::extract_commits_block(&html).is_none());
    }

    #[tokio::test]
    async fn capture_falls_back_to_the_session_id_flag_when_transcript_has_none() {
        let tmp = tempfile::tempdir().unwrap();
        // No `sessionId` field anywhere in this transcript.
        let jsonl = "{\"type\":\"user\",\"timestamp\":\"2026-03-01T09:00:00.000Z\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n";
        let transcript = write(&tmp.path().join("src"), "t.jsonl", jsonl);
        let out_dir = tmp.path().join("sessions");

        let summary = capture(&transcript, Some("hook-given-id"), None, &out_dir, false)
            .await
            .unwrap();
        assert_eq!(summary.session_id, "hook-given-id");
    }

    #[tokio::test]
    async fn capture_errors_without_any_recoverable_session_id() {
        let tmp = tempfile::tempdir().unwrap();
        let jsonl = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n";
        let transcript = write(&tmp.path().join("src"), "t.jsonl", jsonl);
        let out_dir = tmp.path().join("sessions");

        let err = capture(&transcript, None, None, &out_dir, false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no session id recoverable"));
    }

    /// 2026-08-21 ci-host incident, defect 1 — a transcript over
    /// [`kb_core::sessions::CAPTURE_MAX_TRANSCRIPT_BYTES`] must be REFUSED
    /// before it's ever read into memory, never truncated. `File::set_len`
    /// mints a sparse file at exactly cap+1 bytes — no need to actually
    /// write 48MiB of fixture data for the size check to trip.
    #[tokio::test]
    async fn capture_refuses_a_transcript_over_the_size_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let transcript = tmp.path().join("src").join("huge.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        let f = std::fs::File::create(&transcript).unwrap();
        f.set_len(kb_core::sessions::CAPTURE_MAX_TRANSCRIPT_BYTES + 1)
            .unwrap();
        let out_dir = tmp.path().join("sessions");

        let err = capture(&transcript, None, None, &out_dir, false)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(&(kb_core::sessions::CAPTURE_MAX_TRANSCRIPT_BYTES + 1).to_string()),
            "must name the actual size: {err}"
        );
        assert!(
            err.contains(&kb_core::sessions::CAPTURE_MAX_TRANSCRIPT_BYTES.to_string()),
            "must name the cap: {err}"
        );
        assert!(
            err.contains("--allow-oversized"),
            "must hint the override flag: {err}"
        );
        assert_eq!(
            count_html(&out_dir),
            0,
            "an oversized capture writes nothing"
        );
    }

    /// `--allow-oversized` must bypass the size refusal — the capture then
    /// proceeds to the normal read/parse path (which fails for an
    /// unrelated reason here, no recoverable sessionId in an all-zero
    /// sparse file — proving the size gate, not some other short-circuit,
    /// is what --allow-oversized lifts).
    #[tokio::test]
    async fn capture_allow_oversized_overrides_the_size_refusal() {
        let tmp = tempfile::tempdir().unwrap();
        let transcript = tmp.path().join("src").join("huge.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        let f = std::fs::File::create(&transcript).unwrap();
        f.set_len(kb_core::sessions::CAPTURE_MAX_TRANSCRIPT_BYTES + 1)
            .unwrap();
        let out_dir = tmp.path().join("sessions");

        let err = capture(&transcript, None, None, &out_dir, true)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("no session id recoverable"),
            "--allow-oversized must let it past the size gate to the real parse: {err}"
        );
        assert!(
            !err.contains("--allow-oversized"),
            "the size-refusal message must not resurface once overridden: {err}"
        );
    }

    #[tokio::test]
    async fn recapture_of_a_live_session_reuses_the_existing_filename() {
        let tmp = tempfile::tempdir().unwrap();
        let sid = "sess-reuse-1";
        let out_dir = tmp.path().join("sessions");
        let jsonl_v1 = format!(
            "{{\"sessionId\":\"{sid}\",\"type\":\"user\",\"timestamp\":\"2026-03-01T09:00:00.000Z\",\"message\":{{\"role\":\"user\",\"content\":\"start\"}},\"promptSource\":\"typed\"}}\n"
        );
        let transcript = write(&tmp.path().join("src"), "t.jsonl", &jsonl_v1);

        let first = capture(&transcript, None, None, &out_dir, false)
            .await
            .unwrap();
        assert!(!first.reused_existing);
        assert_eq!(count_html(&out_dir), 1);
        let first_name = only_html(&out_dir)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        // A longer transcript (the session continued) re-captured to the
        // SAME sid must overwrite the same file, not add a second one.
        let jsonl_v2 = format!(
            "{}{{\"sessionId\":\"{sid}\",\"type\":\"assistant\",\"timestamp\":\"2026-03-01T09:05:00.000Z\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"done\"}}]}}}}\n",
            jsonl_v1
        );
        std::fs::write(&transcript, &jsonl_v2).unwrap();

        let second = capture(&transcript, None, None, &out_dir, false)
            .await
            .unwrap();
        assert!(second.reused_existing, "must reuse the original filename");
        assert_eq!(count_html(&out_dir), 1, "no duplicate file");
        let second_name = only_html(&out_dir)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert_eq!(first_name, second_name);

        // The re-capture's content reflects the LATEST transcript.
        let html = std::fs::read_to_string(second.path).unwrap();
        let recovered = kb_core::sessions::recover_jsonl_from_capture(&html).unwrap();
        assert_eq!(recovered, jsonl_v2);
    }

    /// FIX 3 — a session whose only existing capture is the STALE hook's
    /// `session-<ts>-<sid>-.html` (trailing dash before `.html`, from
    /// `jq -r .session_id`'s trailing newline surviving `tr`) must be found
    /// and reused in place, not stranded alongside a fresh clean-named
    /// duplicate.
    #[tokio::test]
    async fn recapture_reuses_a_legacy_trailing_dash_capture() {
        let tmp = tempfile::tempdir().unwrap();
        let sid = "sess-legacy-1";
        let out_dir = tmp.path().join("sessions");
        let jsonl_v1 = format!(
            "{{\"sessionId\":\"{sid}\",\"type\":\"user\",\"timestamp\":\"2026-03-01T09:00:00.000Z\",\"message\":{{\"role\":\"user\",\"content\":\"start\"}},\"promptSource\":\"typed\"}}\n"
        );
        // A pre-existing legacy capture: dirty (trailing-dash) meta + filename,
        // exactly what the stale hook wrote — the `<pre>` JSONL itself is
        // untouched by the bug (ground truth stays clean).
        let legacy_name = format!("session-20260301T090000Z-{sid}-.html");
        let legacy_html = wrap_envelope(
            "20260301T090000Z",
            &format!("{sid}-"),
            &jsonl_v1,
            &[],
            &[],
            &[],
        );
        write(&out_dir, &legacy_name, &legacy_html);

        let jsonl_v2 = format!(
            "{jsonl_v1}{{\"sessionId\":\"{sid}\",\"type\":\"assistant\",\"timestamp\":\"2026-03-01T09:05:00.000Z\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"done\"}}]}}}}\n"
        );
        let transcript = write(&tmp.path().join("src"), "t.jsonl", &jsonl_v2);

        let summary = capture(&transcript, None, None, &out_dir, false)
            .await
            .unwrap();
        assert!(
            summary.reused_existing,
            "the legacy trailing-dash file must be found"
        );
        assert_eq!(count_html(&out_dir), 1, "no duplicate file minted");
        let name = Path::new(&summary.path)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(
            name, legacy_name,
            "the legacy filename is overwritten in place, not renamed"
        );

        let html = std::fs::read_to_string(&summary.path).unwrap();
        let recovered = kb_core::sessions::recover_jsonl_from_capture(&html).unwrap();
        assert_eq!(
            recovered, jsonl_v2,
            "content reflects the latest transcript"
        );
    }

    /// FIX 3 — when BOTH a legacy trailing-dash capture and a clean-named
    /// capture already exist for the same sid (the exact duplicate this bug
    /// produced pre-fix), the clean one is reused going forward; the legacy
    /// file is left untouched, never resurrected.
    #[tokio::test]
    async fn recapture_prefers_the_clean_named_capture_over_a_coexisting_legacy_one() {
        let tmp = tempfile::tempdir().unwrap();
        let sid = "sess-legacy-2";
        let out_dir = tmp.path().join("sessions");
        let jsonl_v1 = format!(
            "{{\"sessionId\":\"{sid}\",\"type\":\"user\",\"timestamp\":\"2026-03-01T09:00:00.000Z\",\"message\":{{\"role\":\"user\",\"content\":\"start\"}},\"promptSource\":\"typed\"}}\n"
        );

        let legacy_name = format!("session-20260301T090000Z-{sid}-.html");
        let legacy_html = wrap_envelope(
            "20260301T090000Z",
            &format!("{sid}-"),
            &jsonl_v1,
            &[],
            &[],
            &[],
        );
        write(&out_dir, &legacy_name, &legacy_html);

        let clean_name = format!("session-20260301T093000Z-{sid}.html");
        let clean_html = wrap_envelope("20260301T093000Z", sid, &jsonl_v1, &[], &[], &[]);
        write(&out_dir, &clean_name, &clean_html);

        let jsonl_v2 = format!(
            "{jsonl_v1}{{\"sessionId\":\"{sid}\",\"type\":\"assistant\",\"timestamp\":\"2026-03-01T09:05:00.000Z\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"done\"}}]}}}}\n"
        );
        let transcript = write(&tmp.path().join("src"), "t.jsonl", &jsonl_v2);

        let summary = capture(&transcript, None, None, &out_dir, false)
            .await
            .unwrap();
        assert!(summary.reused_existing);
        assert_eq!(
            count_html(&out_dir),
            2,
            "the legacy file must survive untouched, no new file minted"
        );
        let name = Path::new(&summary.path)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(name, clean_name, "the clean-named capture is preferred");

        let updated = std::fs::read_to_string(&summary.path).unwrap();
        let recovered = kb_core::sessions::recover_jsonl_from_capture(&updated).unwrap();
        assert_eq!(recovered, jsonl_v2);

        let legacy_untouched = std::fs::read_to_string(out_dir.join(&legacy_name)).unwrap();
        assert_eq!(
            legacy_untouched, legacy_html,
            "the legacy file is never resurrected/overwritten"
        );
    }

    /// v0.44 X6 - a spooled RAW transcript is replayed through the scrubbing
    /// capture path: the corpus gets the redacted artifact, the spool item is
    /// deleted, and the secret appears nowhere in the corpus.
    #[tokio::test]
    async fn replay_spool_lands_scrubbed_and_deletes_the_item() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path().join("spool");
        let out_dir = tmp.path().join("sessions");
        let secret = "ghp_0123456789abcdefghijklmnopqrstuvwxyzAB";
        let jsonl = format!(
            "{{\"sessionId\":\"sess-spool-1\",\"type\":\"user\",\"timestamp\":\"2026-03-01T09:00:00.000Z\",\"message\":{{\"role\":\"user\",\"content\":\"token {secret}\"}},\"promptSource\":\"typed\"}}\n"
        );
        write(&spool, "sess-spool-1.jsonl", &jsonl);
        write(&spool, "sess-spool-1.meta", "session_id=sess-spool-1\n");
        let sum = replay_spool(&spool, &out_dir).await.unwrap();
        assert_eq!((sum.replayed, sum.failed), (1, 0));
        assert!(!spool.join("sess-spool-1.jsonl").exists());
        assert!(!spool.join("sess-spool-1.meta").exists());
        assert_eq!(count_html(&out_dir), 1);
        for e in std::fs::read_dir(&out_dir).unwrap() {
            let body = std::fs::read_to_string(e.unwrap().path()).unwrap();
            assert!(!body.contains(secret), "raw secret reached the corpus");
            assert!(body.contains("[redacted:"));
        }
    }

    /// A failing item stays spooled (never dropped) and is counted.
    #[tokio::test]
    async fn replay_spool_keeps_a_failing_item() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path().join("spool");
        write(&spool, "bad.jsonl", "");
        // out_dir is a FILE, so every capture fails.
        let out_file = write(tmp.path(), "not-a-dir", "x");
        let sum = replay_spool(&spool, &out_file).await.unwrap();
        assert_eq!((sum.replayed, sum.failed), (0, 1));
        assert!(spool.join("bad.jsonl").exists());
    }

    fn one_line_jsonl(sid: &str) -> String {
        format!(
            "{{\"sessionId\":\"{sid}\",\"type\":\"user\",\"timestamp\":\"2026-03-01T09:00:00.000Z\",\"message\":{{\"role\":\"user\",\"content\":\"start\"}},\"promptSource\":\"typed\"}}\n"
        )
    }

    fn html_names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|n| n.ends_with(".html"))
            .collect();
        v.sort();
        v
    }

    /// v0.45 N4 - "a_b" and "a-b" used to share ONE capture file (lossy key).
    #[tokio::test]
    async fn capture_distinct_non_uuid_ids_write_distinct_files() {
        let tmp = tempfile::tempdir().unwrap();
        let out_dir = tmp.path().join("sessions");
        for sid in ["a_b", "a-b"] {
            let t = write(
                &tmp.path().join("src"),
                &format!("{sid}.jsonl"),
                &one_line_jsonl(sid),
            );
            let s = capture(&t, None, None, &out_dir, false).await.unwrap();
            assert!(!s.reused_existing, "{sid} must not adopt the other's file");
        }
        assert_eq!(count_html(&out_dir), 2, "{:?}", html_names(&out_dir));
    }

    /// The capture throttle (`kb-capture-throttle.sh` `file_sid_literal`) greps
    /// the first 256 KiB of a capture for an UNESCAPED `"sessionId":"<id>"`
    /// JSON literal to tell its own file from a stranger's. This pins that the
    /// envelope keeps the transcript quotes unescaped inside the `<pre>`; if the
    /// HTML escaping ever changes, this fails instead of the throttle silently
    /// treating every legacy file as foreign.
    #[tokio::test]
    async fn capture_envelope_keeps_session_id_json_literal_unescaped_in_head() {
        let tmp = tempfile::tempdir().unwrap();
        let out_dir = tmp.path().join("sessions");
        let sid = "ses_literal-1";
        let t = write(&tmp.path().join("src"), "t.jsonl", &one_line_jsonl(sid));
        capture(&t, None, None, &out_dir, false).await.unwrap();
        let name = html_names(&out_dir).remove(0);
        let bytes = std::fs::read(out_dir.join(name)).unwrap();
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(262_144)]).into_owned();
        assert!(
            head.contains(&format!("\"sessionId\":\"{sid}\"")),
            "throttle's file_sid_literal needs the raw literal in the head"
        );
    }

    /// An old capture under the lossy name is reused in place when it
    /// verifiably holds this id: same name, no duplicate, no rename.
    #[tokio::test]
    async fn capture_reuses_verified_legacy_lossy_named_file() {
        let tmp = tempfile::tempdir().unwrap();
        let out_dir = tmp.path().join("sessions");
        let sid = "ses_legacy_7";
        let lossy = crate::commands::import::legacy_lossy_sid(sid);
        let name = format!("session-20260301T090000Z-{lossy}.html");
        write(
            &out_dir,
            &name,
            &wrap_envelope(
                "20260301T090000Z",
                &lossy,
                &one_line_jsonl(sid),
                &[],
                &[],
                &[],
            ),
        );
        let t = write(&tmp.path().join("src"), "t.jsonl", &one_line_jsonl(sid));
        let s = capture(&t, None, None, &out_dir, false).await.unwrap();
        assert!(s.reused_existing);
        assert_eq!(html_names(&out_dir), vec![name]);
    }

    /// The lossy name is shared by distinct ids: a file whose embedded id is
    /// someone else's must NOT be overwritten.
    #[tokio::test]
    async fn capture_does_not_reuse_legacy_file_of_colliding_id() {
        let tmp = tempfile::tempdir().unwrap();
        let out_dir = tmp.path().join("sessions");
        let other = "a-b"; // plain id; its (legacy) file name is also a_b's lossy name
        let name = format!("session-20260301T090000Z-{other}.html");
        let theirs = wrap_envelope(
            "20260301T090000Z",
            other,
            &one_line_jsonl(other),
            &[],
            &[],
            &[],
        );
        write(&out_dir, &name, &theirs);
        let t = write(&tmp.path().join("src"), "t.jsonl", &one_line_jsonl("a_b"));
        let s = capture(&t, None, None, &out_dir, false).await.unwrap();
        assert!(!s.reused_existing);
        assert_eq!(count_html(&out_dir), 2);
        assert_eq!(
            std::fs::read_to_string(out_dir.join(&name)).unwrap(),
            theirs
        );
        // and a hashed-key session next to a legacy file of another id
        let t2 = write(&tmp.path().join("src"), "t2.jsonl", &one_line_jsonl("a.b"));
        let s2 = capture(&t2, None, None, &out_dir, false).await.unwrap();
        assert!(!s2.reused_existing);
        assert_eq!(count_html(&out_dir), 3);
        assert_eq!(
            std::fs::read_to_string(out_dir.join(&name)).unwrap(),
            theirs
        );
    }

    #[tokio::test]
    async fn capture_stamp_names_new_file_but_is_ignored_on_reuse() {
        let tmp = tempfile::tempdir().unwrap();
        let out_dir = tmp.path().join("sessions");
        let sid = "ses_stamp_1";
        let t = write(&tmp.path().join("src"), "t.jsonl", &one_line_jsonl(sid));
        let s = capture_with(&t, None, None, &out_dir, false, Some("20250102T030405Z"))
            .await
            .unwrap();
        let key = sanitize_sid(sid);
        assert_eq!(
            Path::new(&s.path).file_name().unwrap().to_str().unwrap(),
            format!("session-20250102T030405Z-{key}.html")
        );
        let s2 = capture_with(&t, None, None, &out_dir, false, Some("20260909T010101Z"))
            .await
            .unwrap();
        assert!(s2.reused_existing);
        assert_eq!(s2.path, s.path, "stamp must not rename a reused file");
        assert!(!valid_compact_stamp("2025-01-02"));
        assert!(!valid_compact_stamp("20251302T030405Z"));
        assert!(valid_compact_stamp("20250102T030405Z"));
    }

    #[tokio::test]
    async fn replay_spool_honours_stamp_meta() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path().join("spool");
        let out_dir = tmp.path().join("sessions");
        let sid = "ses_replay_stamp";
        let key = sanitize_sid(sid);
        write(&spool, &format!("{key}.jsonl"), &one_line_jsonl(sid));
        write(
            &spool,
            &format!("{key}.meta"),
            &format!("session_id={sid}\nstamp=20240506T070809Z\nharness=opencode\n"),
        );
        let sum = replay_spool(&spool, &out_dir).await.unwrap();
        assert_eq!((sum.replayed, sum.failed), (1, 0));
        assert_eq!(
            html_names(&out_dir),
            vec![format!("session-20240506T070809Z-{key}.html")]
        );
    }

    #[tokio::test]
    async fn replay_spool_folds_and_then_removes_parked_sidecars() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path().join("spool");
        let out_dir = tmp.path().join("sessions");
        let sid = "ses-omp-sidecar";
        let key = sanitize_sid(sid);
        write(&spool, &format!("{key}.jsonl"), &one_line_jsonl(sid));
        write(
            &spool,
            &format!("{key}.meta"),
            &format!("session_id={sid}\n"),
        );
        write(
            &spool.join(sid).join("subagents"),
            "agent-a1.jsonl",
            &one_line_jsonl(sid),
        );
        let sum = replay_spool(&spool, &out_dir).await.unwrap();
        assert_eq!((sum.replayed, sum.failed), (1, 0));
        assert!(
            !spool.join(sid).exists(),
            "parked sidecars must be removed after a successful replay"
        );
    }

    /// Round 5 - a replayed snapshot OLDER than the capture already in the
    /// corpus is dropped (file untouched, item removed); a snapshot NEWER than
    /// it still replays. The shared spool replays every session's items, so
    /// this is what stops Y's replay publishing X's stale M1 over X's fresh M2.
    #[tokio::test]
    async fn replay_spool_never_overwrites_a_fresher_capture() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path().join("spool");
        let out_dir = tmp.path().join("sessions");
        let sid = "ses_stale_replay";
        let key = sanitize_sid(sid);
        let m1 = one_line_jsonl(sid);
        let m2 = format!(
            "{m1}{{\"sessionId\":\"{sid}\",\"type\":\"assistant\",\"timestamp\":\"2026-03-01T09:05:00.000Z\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"fresh-m2-marker\"}}]}}}}\n"
        );
        // X's M1 snapshot is spooled first ...
        let item = write(&spool, &format!("{key}.jsonl"), &m1);
        write(
            &spool,
            &format!("{key}.meta"),
            &format!("session_id={sid}\n"),
        );
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(30);
        std::fs::File::options()
            .write(true)
            .open(&item)
            .unwrap()
            .set_modified(old)
            .unwrap();
        // ... then X lands M2 fresh (published now, after the snapshot).
        let t2 = write(tmp.path(), "m2.jsonl", &m2);
        let fresh = capture(&t2, None, None, &out_dir, false).await.unwrap();
        let before = std::fs::read_to_string(&fresh.path).unwrap();
        assert!(before.contains("fresh-m2-marker"));

        let sum = replay_spool(&spool, &out_dir).await.unwrap();
        assert_eq!((sum.replayed, sum.dropped_stale, sum.failed), (0, 1, 0));
        assert_eq!(
            std::fs::read_to_string(&fresh.path).unwrap(),
            before,
            "a stale replay must leave the fresher capture byte-identical"
        );
        assert_eq!(count_html(&out_dir), 1, "no duplicate filename minted");
        assert!(
            !item.exists(),
            "the stale item is dropped, not retried forever"
        );

        // A snapshot taken AFTER the corpus file was published still replays.
        let item = write(&spool, &format!("{key}.jsonl"), &m1);
        write(
            &spool,
            &format!("{key}.meta"),
            &format!("session_id={sid}\n"),
        );
        let newer = std::time::SystemTime::now() + std::time::Duration::from_secs(30);
        std::fs::File::options()
            .write(true)
            .open(&item)
            .unwrap()
            .set_modified(newer)
            .unwrap();
        let sum = replay_spool(&spool, &out_dir).await.unwrap();
        assert_eq!((sum.replayed, sum.dropped_stale, sum.failed), (1, 0, 0));
        assert!(!std::fs::read_to_string(&fresh.path)
            .unwrap()
            .contains("fresh-m2-marker"));
    }

    /// A newer snapshot re-parked over an item while it was processed must not
    /// be deleted with the old one.
    #[test]
    fn item_unchanged_detects_a_reparked_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let item = write(tmp.path(), "i.jsonl", "a\n");
        let snap = std::fs::metadata(&item).and_then(|m| m.modified()).ok();
        assert!(item_unchanged(&item, snap));
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&item)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert!(!item_unchanged(&item, snap));
    }

    /// v0.46 SEG-PR2: `drop-part` removes a verified continuation part's file,
    /// is idempotent, and never touches part 1, a plain session, or a file
    /// whose embedded ids do not match.
    #[test]
    fn drop_part_removes_only_a_verified_continuation_part() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path();
        let html = |body: &str| format!("<html><body><pre>{body}</pre></body></html>");
        let part = write(
            out,
            "session-20260101T000000Z-S1-p02.html",
            &html(
                r#"{"sessionId":"S1-p02","type":"adapter-meta","segmentOf":"S1","segmentIdx":2,"rawSessionId":"S1"}"#,
            ),
        );
        let first = write(
            out,
            "session-20260101T000000Z-S1.html",
            &html(r#"{"sessionId":"S1","type":"adapter-meta"}"#),
        );
        // A forged file under part 3's name that belongs to nobody.
        let forged = write(
            out,
            "session-20260101T000000Z-S1-p03.html",
            &html(r#"{"sessionId":"S1-p03","type":"adapter-meta"}"#),
        );
        // Not a continuation id: refused up front.
        assert!(run_drop_part(Some(out.to_path_buf()), "S1", "S1", true).is_err());
        assert!(run_drop_part(Some(out.to_path_buf()), "S1-p01", "S1", true).is_err());
        assert!(first.exists());
        // The forged one is refused and kept.
        assert!(run_drop_part(Some(out.to_path_buf()), "S1-p03", "S1", true).is_err());
        assert!(forged.exists());
        // The verified one goes; a second call is a no-op success.
        run_drop_part(Some(out.to_path_buf()), "S1-p02", "S1", true).unwrap();
        assert!(!part.exists());
        run_drop_part(Some(out.to_path_buf()), "S1-p02", "S1", true).unwrap();
        assert!(first.exists());
    }

    #[test]
    fn resolve_out_dir_errors_naming_flag_and_env() {
        let saved = std::env::var_os(SESSIONS_DIR_ENV);
        std::env::remove_var(SESSIONS_DIR_ENV);
        let err = resolve_out_dir(None).unwrap_err().to_string();
        assert!(err.contains("--out"));
        assert!(err.contains(SESSIONS_DIR_ENV));
        if let Some(v) = saved {
            std::env::set_var(SESSIONS_DIR_ENV, v);
        }
    }
}
