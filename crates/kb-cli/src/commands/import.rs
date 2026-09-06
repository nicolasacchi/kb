//! Z5 — `kb import claude-history`: a retroactive backfill verb.
//!
//! Every Claude Code user has months of historical session transcripts in
//! `~/.claude/projects/*/<uuid>.jsonl` that predate kb's live Stop-hook
//! capture (`plugins/kb-memory/hooks/kb-capture.sh`). This verb walks that
//! tree and, for each transcript, writes the **exact same envelope** the live
//! hook produces into a kb sessions corpus — so the daemon's watcher indexes
//! each one as episodic memory, indistinguishable from a live capture.
//!
//! Envelope parity with kb-capture.sh (the source of truth):
//!   - filename `session-<ts>-<sid>.html`
//!   - `<meta name="kb-category" content="memory-session">` (drives the
//!     SessionCaptureHook + the R0 search/recall exclusion + the R1 digest)
//!   - `<meta name="kb-decay" content="fast">`
//!   - `<meta name="kb-session" content="<sid>">` (a canonical-id fallback)
//!   - the raw JSONL transcript, HTML-escaped, wrapped in a single `<pre>`
//!   - `<sid>` sanitised exactly like the hook: `tr -c 'a-zA-Z0-9' '-'` then
//!     the first 80 chars.
//!
//! W0.4 split contract: [`wrap_envelope`] is shared with `kb sessions
//! capture` (the Rust envelope writer that replaces the live kb-capture.sh
//! heredoc). Everything through the closing `</pre>` stays BYTE-IDENTICAL to
//! the historical hook output — `recover_jsonl_from_capture` reads only that
//! block, so the round-trip invariant (#11) never depends on what follows.
//! OPTIONAL, ADDITIVE tail blocks after `</pre>` carry capture-time git
//! resolution (`kb_core::sessions::render_commits_block`), a subagent
//! file-grain digest (W0.5, `render_subagents_block`), and — W0.6 — the
//! subagents' raw sidecar TEXT (`render_sidecar_text_block`, searchable
//! evidence for BM25/recollect, distinct from the structured digest), each
//! present only when it has something to add. Backfills through the NORMAL
//! import path never resolve commits (no live cwd to `git show -s`
//! against), but DO walk each imported session's sidecar directory (W0.6 —
//! purely filesystem, no daemon/live-cwd dependency, so it costs nothing
//! extra here) for the subagents digest + sidecar-text tail blocks, keeping
//! a backfilled capture's searchable surface consistent with a live one.
//! `--refresh-subagents` never calls this function at all — it rewrites an
//! EXISTING envelope's tail blocks in place (see
//! [`refresh_subagents_backfill`]).
//!
//! Canonical id (invariant #11): the transcript's own JSONL `sessionId` is
//! ground truth — recovered here, NEVER taken from the `.jsonl` filename — so
//! the produced file's `kb-session` meta + filename carry the authoritative
//! id the indexer's `parse_session_html_full` recovery path expects. A
//! `.jsonl` with no recoverable `sessionId` is skipped, not imported: there
//! is no filename fallback (a fallback once minted a phantom "journal"
//! session from a subagent workflow journal).
//!
//! Walk shape: transcripts live DIRECTLY under each project dir
//! (`~/.claude/projects/<project>/<uuid>.jsonl`) — the walk is depth-capped
//! there and never descends into session subdirectories
//! (`<project>/<session>/subagents/workflows/wf_*/journal.jsonl` are
//! Workflow-tool agent journals, not transcripts).
//!
//! Deduped + idempotent: a session whose canonical id already has a capture in
//! the target (live captures included) is skipped, so re-running imports 0.
//! This is a filesystem verb only — it never talks to the daemon (the watcher
//! does the indexing).
//!
//! `--refresh-subagents` is a SEPARATE, opt-in mode ([`refresh_subagents`]):
//! instead of importing new transcripts, it re-walks EXISTING captures
//! already in `--into`, finds each one's sidecar directory under
//! `--transcripts-root` (default the same `~/.claude/projects`), and
//! rewrites BOTH tail blocks in place — the subagents digest (W0.5,
//! `kb_core::sessions::replace_subagents_block`) and — W0.6 — the raw
//! sidecar-text evidence (`replace_sidecar_text_block`) — from the SAME
//! sidecar walk ([`collect_sidecars`]). The `<pre>` transcript and every
//! other block (e.g. commits) are left byte-for-byte untouched. Idempotent:
//! refreshing twice against unchanged sidecars produces byte-identical
//! output across BOTH blocks together (a session lands in `NoChange` only
//! when neither block's rewrite differs from what's already on disk).

use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use kb_core::sessions::{SubagentDigest, MEMORY_SESSION_CATEGORY};
use kb_core::timeparse::parse_iso_utc;

/// The default project tree the live capture reads transcripts from.
const DEFAULT_DIR: &str = "~/.claude/projects";
/// The env var the capture hook resolves its target from. Mirror it exactly so
/// an existing capture setup needs no new configuration to backfill.
const SESSIONS_DIR_ENV: &str = "KB_SESSIONS_DIR";
/// The hook's `cut -c1-80` bound on the sanitised session id.
const SID_MAX_CHARS: usize = 80;

/// What happened to one transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Outcome {
    Imported,
    SkippedDuplicate,
    SkippedOther,
    /// 2026-08-21 ci-host incident, defect 1 — the RAW file is over
    /// [`kb_core::sessions::CAPTURE_MAX_TRANSCRIPT_BYTES`] and
    /// `--allow-oversized` wasn't passed: skipped rather than read into
    /// memory whole, so one monster transcript can't abort an entire
    /// backfill sweep.
    SkippedOversized,
}

/// One transcript's disposition, for the `--json` item list + the per-file
/// human lines.
#[derive(Debug, Clone, Serialize)]
struct Item {
    action: Outcome,
    /// Canonical session id, recovered from the transcript's own JSONL
    /// `sessionId` (never the filename). Empty when the file was skipped
    /// before an id could be recovered (unreadable / empty / no sessionId).
    session_id: String,
    /// The source `.jsonl` path (absolute).
    source: String,
    /// The written capture filename (present only when imported).
    #[serde(skip_serializing_if = "Option::is_none")]
    written: Option<String>,
    /// Why it was skipped (present only for skips).
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

/// The machine summary emitted by `--json`.
#[derive(Debug, Clone, Serialize)]
struct Summary {
    imported: usize,
    skipped_duplicate: usize,
    skipped_other: usize,
    /// 2026-08-21 ci-host incident hardening — count of transcripts skipped for
    /// being over [`kb_core::sessions::CAPTURE_MAX_TRANSCRIPT_BYTES`]
    /// (broken out from `skipped_other` so a `--json` consumer can tell a
    /// monster transcript apart from an unreadable/empty/id-less one).
    skipped_oversized: usize,
    total: usize,
    dry_run: bool,
    dir: String,
    into: String,
    items: Vec<Item>,
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    dir: Option<PathBuf>,
    into: Option<PathBuf>,
    dry_run: bool,
    limit: Option<u32>,
    json: bool,
    quiet: bool,
    refresh_subagents: bool,
    transcripts_root: Option<PathBuf>,
    allow_oversized: bool,
) -> Result<()> {
    if refresh_subagents {
        let into = resolve_into(into)?;
        let root = expand_tilde(transcripts_root.unwrap_or_else(|| PathBuf::from(DEFAULT_DIR)));
        let summary = refresh_subagents_backfill(&into, &root, dry_run, limit.map(|n| n as usize))?;

        if json {
            println!("{}", serde_json::to_string_pretty(&summary)?);
            return Ok(());
        }
        if !quiet {
            for it in &summary.items {
                match it.outcome {
                    RefreshOutcome::Updated => println!(
                        "{} refresh   {}  {}",
                        if dry_run { "would" } else { "  ok " },
                        it.session_id,
                        it.capture
                    ),
                    RefreshOutcome::NoSidecars => {
                        println!("  skip      no-sidecars  {}", it.capture)
                    }
                    RefreshOutcome::NoChange => println!("  skip      no-change  {}", it.capture),
                    RefreshOutcome::NoSessionId => {
                        println!("  skip      no-session-id  {}", it.capture)
                    }
                }
            }
        }
        let verb = if dry_run {
            "would refresh"
        } else {
            "refreshed"
        };
        println!(
            "{verb} {} · {} unchanged · {} without sidecars · {} capture(s) scanned → {}",
            summary.updated, summary.no_change, summary.no_sidecars, summary.total, summary.into,
        );
        return Ok(());
    }

    let dir = expand_tilde(dir.unwrap_or_else(|| PathBuf::from(DEFAULT_DIR)));
    let into = resolve_into(into)?;
    let summary = import(
        &dir,
        &into,
        dry_run,
        limit.map(|n| n as usize),
        allow_oversized,
    )?;

    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }

    if !quiet {
        for it in &summary.items {
            match it.action {
                Outcome::Imported => println!(
                    "{} import    {}  {}",
                    if dry_run { "would" } else { "  ok " },
                    it.session_id,
                    it.source
                ),
                Outcome::SkippedDuplicate => {
                    println!("  skip-dup  {}  {}", it.session_id, it.source)
                }
                Outcome::SkippedOther => println!(
                    "  skip      {}  {}",
                    it.reason.unwrap_or("other"),
                    it.source
                ),
                Outcome::SkippedOversized => println!(
                    "  skip-big  {}  {}",
                    it.reason.unwrap_or("oversized"),
                    it.source
                ),
            }
        }
    }

    let verb = if dry_run { "would import" } else { "imported" };
    println!(
        "{verb} {} · skipped {} duplicate · skipped {} other · skipped {} oversized · \
         {} transcript(s) scanned → {}",
        summary.imported,
        summary.skipped_duplicate,
        summary.skipped_other,
        summary.skipped_oversized,
        summary.total,
        summary.into,
    );
    Ok(())
}

/// The whole import, printing-free — returns the [`Summary`] `run` renders
/// (and tests assert on).
fn import(
    dir: &Path,
    into: &Path,
    dry_run: bool,
    limit: Option<usize>,
    allow_oversized: bool,
) -> Result<Summary> {
    if !dir.exists() {
        bail!(
            "transcript dir not found: {}\n(pass --dir, or point it at your \
             Claude Code projects tree)",
            dir.display()
        );
    }

    // Seed the dedupe set from every capture already in the target (live
    // captures + prior imports). Idempotence + "skip existing" ride this.
    let mut seen = scan_existing_ids(into);

    // Transcripts live DIRECTLY under each project dir:
    // `<dir>/<project>/<uuid>.jsonl` (depth 2; depth 1 also accepted so
    // `--dir` can point at a single project dir). NEVER deeper — session
    // subdirectories hold non-transcript `.jsonl` (subagent workflow
    // journals, `…/<session>/subagents/workflows/wf_*/journal.jsonl`) that
    // carry no `sessionId` and must not be walked at all.
    // Deterministic order: sort the paths so per-file output + the
    // `--limit` cut are stable across runs.
    let mut transcripts: Vec<PathBuf> = walkdir::WalkDir::new(dir)
        .min_depth(1)
        .max_depth(2)
        .sort_by_file_name()
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("jsonl"))
        .collect();
    transcripts.sort();

    if !dry_run {
        std::fs::create_dir_all(into)
            .with_context(|| format!("create sessions dir {}", into.display()))?;
    }

    let mut items: Vec<Item> = Vec::new();
    let mut imported = 0usize;
    let mut skipped_duplicate = 0usize;
    let mut skipped_other = 0usize;
    let mut skipped_oversized = 0usize;

    for path in &transcripts {
        // Stop examining once we've imported enough. Duplicates/skips don't
        // count against `--limit` — it caps NEW captures written, so
        // `--limit 5` gives you five fresh sessions to try.
        if let Some(n) = limit {
            if imported >= n {
                break;
            }
        }

        // 2026-08-21 ci-host incident, defect 1 — stat BEFORE reading: a
        // monster transcript (292MB raw Codex rollout, 77% base64
        // screenshots, in the original incident) must not abort the whole
        // sweep. Skip it with a clear stderr warning and keep walking —
        // an import backfill covers hundreds of transcripts, one outsized
        // file is not grounds to fail the rest.
        let meta_len = std::fs::metadata(path).map(|m| m.len());
        if let Ok(len) = meta_len {
            if kb_core::sessions::transcript_size_verdict(
                len,
                kb_core::sessions::CAPTURE_MAX_TRANSCRIPT_BYTES,
                allow_oversized,
            ) == kb_core::sessions::TranscriptSizeVerdict::Refuse
            {
                eprintln!(
                    "warning: skipping oversized transcript {} ({len} bytes > \
                     {}-byte cap, kb_core::sessions::CAPTURE_MAX_TRANSCRIPT_BYTES); \
                     pass --allow-oversized to import it verbatim anyway",
                    path.display(),
                    kb_core::sessions::CAPTURE_MAX_TRANSCRIPT_BYTES,
                );
                skipped_oversized += 1;
                items.push(Item {
                    action: Outcome::SkippedOversized,
                    session_id: String::new(),
                    source: path.display().to_string(),
                    written: None,
                    reason: Some("oversized"),
                });
                continue;
            }
        }

        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => {
                skipped_other += 1;
                items.push(Item {
                    action: Outcome::SkippedOther,
                    session_id: String::new(),
                    source: path.display().to_string(),
                    written: None,
                    reason: Some("unreadable"),
                });
                continue;
            }
        };

        let scan = scan_transcript(&raw);
        // A `.jsonl` with no transcript records is not a session — the live
        // hook only ever sees a real transcript, so guard the tree-walk
        // against stub/empty files rather than minting a phantom session.
        if scan.nonempty == 0 {
            skipped_other += 1;
            items.push(Item {
                action: Outcome::SkippedOther,
                session_id: String::new(),
                source: path.display().to_string(),
                written: None,
                reason: Some("empty"),
            });
            continue;
        }

        // Canonical id = the JSONL `sessionId` — ground truth (invariant
        // #11), the raw un-sanitised id the indexer recovers + the dedupe
        // key. NO filename fallback: a `.jsonl` without a recoverable id is
        // not a session transcript we can attribute, so it's skipped rather
        // than minted under a phantom id.
        let Some(canonical) = scan.session_id.clone() else {
            skipped_other += 1;
            items.push(Item {
                action: Outcome::SkippedOther,
                session_id: String::new(),
                source: path.display().to_string(),
                written: None,
                reason: Some("no sessionId"),
            });
            continue;
        };

        if seen.contains(&canonical) {
            skipped_duplicate += 1;
            items.push(Item {
                action: Outcome::SkippedDuplicate,
                session_id: canonical,
                source: path.display().to_string(),
                written: None,
                reason: Some("already-captured"),
            });
            continue;
        }

        // Timestamp: the transcript's earliest event time → a truthful
        // `started_at` (the parser derives started_at from the filename ts).
        // Fall back to the file mtime when no event carried a timestamp.
        let ts_unix = scan.earliest.unwrap_or_else(|| file_mtime_unix(path));
        let ts = compact_utc(ts_unix);
        let sid = sanitize_sid(&canonical);
        let filename = format!("session-{ts}-{sid}.html");
        let out_path = into.join(&filename);

        if !dry_run {
            // W0.6 — walk the transcript's OWN sidecar dir (RAW canonical id,
            // Claude Code's own on-disk convention: `<project-dir>/<session-
            // id>/subagents/` — `path`'s parent IS the project dir, since
            // transcripts live directly under it). Filesystem-only, no live
            // cwd needed (unlike commit resolution above), so a backfill's
            // digest + sidecar-text tail blocks match what a live capture of
            // the same session would have written.
            let sidecar_dir = path.parent().map(|d| d.join(&canonical).join("subagents"));
            let (subagents, mut sidecar_texts) = sidecar_dir
                .as_deref()
                .map(collect_sidecars)
                .unwrap_or_default();
            // W5/R10 — workflow journals + TaskOutput snapshots (same caps,
            // same list — see `collect_extra_sidecar_sources`).
            if let Some(d) = sidecar_dir.as_deref() {
                sidecar_texts.extend(collect_extra_sidecar_sources(d, &raw));
            }
            let html = wrap_envelope(&ts, &sid, &raw, &[], &subagents, &sidecar_texts);
            std::fs::write(&out_path, html)
                .with_context(|| format!("write capture {}", out_path.display()))?;
        }

        seen.insert(canonical.clone());
        imported += 1;
        items.push(Item {
            action: Outcome::Imported,
            session_id: canonical,
            source: path.display().to_string(),
            written: Some(filename),
            reason: None,
        });
    }

    Ok(Summary {
        imported,
        skipped_duplicate,
        skipped_other,
        skipped_oversized,
        total: items.len(),
        dry_run,
        dir: dir.display().to_string(),
        into: into.display().to_string(),
        items,
    })
}

/// Resolve `--into`: the flag wins; else `$KB_SESSIONS_DIR` (exactly how the
/// capture hook resolves its target); else a hard error naming both.
fn resolve_into(into: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = into {
        return Ok(expand_tilde(p));
    }
    match std::env::var_os(SESSIONS_DIR_ENV) {
        Some(v) if !v.is_empty() => Ok(expand_tilde(PathBuf::from(v))),
        _ => bail!(
            "no target sessions corpus: pass --into <DIR> or set {SESSIONS_DIR_ENV} \
             (the same env var the capture hook uses). This should be a configured \
             sessions corpus's source dir so the daemon indexes the imports."
        ),
    }
}

/// Wrap a raw JSONL transcript in the kb-capture.sh envelope — byte-for-byte
/// identical to the hook's heredoc THROUGH the closing `</pre>` (see the
/// module doc's W0.4 split-contract note), with `commits`, `subagents`
/// (W0.5), and — W0.6 — `sidecar_texts` each rendered as an OPTIONAL tail
/// block right after it — present only when non-empty, in digest-then-
/// evidence order. This verb (`kb import claude-history`'s NORMAL import
/// path) always passes `&[]` for `commits` (a backfill has no live cwd to
/// resolve commits against), but DOES walk sidecars for `subagents`/
/// `sidecar_texts` (see [`import`]'s call site) so a backfilled capture's
/// searchable surface matches a live one. `--refresh-subagents` never calls
/// this function — it rewrites an EXISTING envelope's tail blocks in place
/// instead (see [`replace_subagents_block`](kb_core::sessions::replace_subagents_block)
/// / [`replace_sidecar_text_block`](kb_core::sessions::replace_sidecar_text_block)).
pub(crate) fn wrap_envelope(
    ts: &str,
    sid: &str,
    raw_jsonl: &str,
    commits: &[kb_core::sessions::CapturedCommit],
    subagents: &[kb_core::sessions::SubagentDigest],
    sidecar_texts: &[(String, String)],
) -> String {
    let esc = html_escape(raw_jsonl);
    let mut tail = String::new();
    let commits_block = kb_core::sessions::render_commits_block(commits);
    if !commits_block.is_empty() {
        tail.push_str(&commits_block);
        tail.push('\n');
    }
    let subagents_block = kb_core::sessions::render_subagents_block(subagents);
    if !subagents_block.is_empty() {
        tail.push_str(&subagents_block);
        tail.push('\n');
    }
    if let Some(sidecar_text_block) = kb_core::sessions::render_sidecar_text_block(sidecar_texts) {
        tail.push_str(&sidecar_text_block);
        tail.push('\n');
    }
    format!(
        "<!DOCTYPE html>\n\
         <html lang=\"en\"><head><meta charset=\"utf-8\">\n\
         <title>Session transcript {ts}</title>\n\
         <meta name=\"kb-category\" content=\"{MEMORY_SESSION_CATEGORY}\">\n\
         <meta name=\"kb-decay\" content=\"fast\">\n\
         <meta name=\"kb-session\" content=\"{sid}\">\n\
         </head><body>\n\
         <h1>Session transcript {ts}</h1>\n\
         <pre>{esc}</pre>\n\
         {tail}\
         </body></html>\n"
    )
}

/// The hook's `sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g'`.
/// `&` first so the `&` it emits isn't re-encoded (mirror of the parser's
/// `html_unescape`, which reverses in the opposite order).
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// W0.6 — walk a session's sidecar directory (`<session-id>/subagents/`) for
/// `agent-*.jsonl` files, returning BOTH the parsed digests (for
/// [`render_subagents_block`](kb_core::sessions::render_subagents_block))
/// AND the raw JSONL text keyed by the SAME resolved `agent_id` (for
/// [`render_sidecar_text_block`](kb_core::sessions::render_sidecar_text_block))
/// — one directory walk + one read per file, not two. Mirrors
/// `kb_core::sessions::collect_subagent_digests`'s file-listing + sort
/// exactly (deterministic order; `subagents/workflows/**` is excluded by
/// construction since only direct-child `agent-*.jsonl` files match) but
/// additionally keeps the raw string `parse_subagent_jsonl` would otherwise
/// discard. Both callers ([`import`] and [`refresh_subagents_backfill`])
/// need the pair, and `kb sessions capture` (`sessions_capture.rs`) reuses
/// this too — one shared walk instead of three divergent ones. Best-effort,
/// same contract as `collect_subagent_digests`: an unreadable sidecar is
/// skipped, never a hard error; an absent/empty dir returns `(vec![],
/// vec![])`.
pub(crate) fn collect_sidecars(dir: &Path) -> (Vec<SubagentDigest>, Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (Vec::new(), Vec::new());
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("agent-") && n.ends_with(".jsonl"))
        })
        .collect();
    // Deterministic order — same reasoning as `collect_subagent_digests`:
    // the tail blocks' agent order shouldn't depend on the OS's
    // directory-listing order.
    files.sort();

    let mut digests = Vec::with_capacity(files.len());
    let mut texts = Vec::with_capacity(files.len());
    let mut skipped = 0u32;
    for path in &files {
        match std::fs::read_to_string(path) {
            Ok(raw) => {
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let filename_id = stem.strip_prefix("agent-").unwrap_or(stem);
                let digest = kb_core::sessions::parse_subagent_jsonl(&raw, filename_id);
                texts.push((digest.agent_id.clone(), raw));
                digests.push(digest);
            }
            Err(e) => {
                skipped += 1;
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "skipping unreadable subagent sidecar"
                );
            }
        }
    }
    if skipped > 0 {
        tracing::warn!(
            skipped,
            dir = %dir.display(),
            "sidecar collection skipped unreadable files"
        );
    }
    (digests, texts)
}

/// Per-source read ceiling, well above [`kb_core::sessions::
/// SIDECAR_TEXT_AGENT_CAP_BYTES`] (2 MiB): the render-time truncation
/// bounds what's KEPT, this bounds what's READ into memory at capture
/// time, so a pathological multi-GB stray file can't blow up capture.
const EXTRA_SIDECAR_SOURCE_READ_CEILING_BYTES: u64 = 32 * 1024 * 1024;

/// W5/R10 — additional sidecar-TEXT sources beyond the direct
/// `agent-*.jsonl` sidecars [`collect_sidecars`] already walks, folded into
/// the SAME `(id, raw_text)` list that feeds
/// [`kb_core::sessions::render_sidecar_text_block`] — which is already
/// generic over `(id, text)` pairs (the `id` becomes the block's per-source
/// `<summary>` label), so this needs NO kb-core rendering changes, only
/// more sources under the SAME caps (2 MiB/source, 8 MiB total, head/tail
/// truncate, absent-when-empty — unchanged contract).
///
/// Two sources, both best-effort (a missing/unreadable/oversized file is
/// silently skipped, never a hard error — same posture as every other
/// sidecar read in this module):
///
/// 1. **Workflow journals** — `<subagents_dir>/workflows/<wf_id>/
///    journal.jsonl` (verified live: a workflow-launched subagent's full
///    per-agent JSONL does NOT exist as a sibling `agent-*.jsonl` under
///    `subagents/` the way a direct Task-tool delegation's does — the
///    journal's `started`/`result` events, INCLUDING each agent's
///    synthesized `result` payload, are the only durable record of what a
///    workflow's subagents actually found). One source per workflow, id
///    `workflow:<wf_id>`.
/// 2. **TaskOutput snapshots** — paths named by
///    [`kb_core::sessions::task_output_paths`] scanned over the MAIN
///    transcript text (a backgrounded Bash tool's truncated-output marker),
///    snapshotted IF the file still exists at capture time (`/tmp` is
///    ephemeral — this window is the only time this data exists at all).
///    One source per resolved path, id `task-output:<file-stem>`.
pub(crate) fn collect_extra_sidecar_sources(
    subagents_dir: &Path,
    main_transcript: &str,
) -> Vec<(String, String)> {
    let mut out = Vec::new();

    let workflows_dir = subagents_dir.join("workflows");
    if let Ok(entries) = std::fs::read_dir(&workflows_dir) {
        let mut wf_dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        // Deterministic order — same reasoning as `collect_sidecars`.
        wf_dirs.sort();
        for wf_dir in wf_dirs {
            let journal = wf_dir.join("journal.jsonl");
            if !bounded_size_ok(&journal) {
                continue;
            }
            if let Ok(raw) = std::fs::read_to_string(&journal) {
                if !raw.trim().is_empty() {
                    let wf_id = wf_dir
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("workflow");
                    out.push((format!("workflow:{wf_id}"), raw));
                }
            }
        }
    }

    for path_str in kb_core::sessions::task_output_paths(main_transcript) {
        let path = PathBuf::from(&path_str);
        if !bounded_size_ok(&path) {
            continue;
        }
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if !raw.trim().is_empty() {
                let id = path
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .unwrap_or("task-output");
                out.push((format!("task-output:{id}"), raw));
            }
        }
    }

    out
}

fn bounded_size_ok(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.len() <= EXTRA_SIDECAR_SOURCE_READ_CEILING_BYTES)
        .unwrap_or(false)
}

// --- `--refresh-subagents` backfill (W0.5 digest, W0.6 sidecar text) -------

/// One capture's disposition under `--refresh-subagents`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum RefreshOutcome {
    /// The subagent digest + sidecar text blocks were (re)written with
    /// fresh sidecar data (whichever of the pair actually differed — see
    /// `refresh_subagents_backfill`).
    Updated,
    /// No sidecar directory was found for this session under
    /// `--transcripts-root` (or it exists but has no `agent-*.jsonl` files).
    NoSidecars,
    /// Sidecars were found, but the rewrite is byte-identical to what's
    /// already there — a second run against unchanged sidecars lands every
    /// capture here (idempotence).
    NoChange,
    /// The capture's own session id couldn't be recovered (corrupt or
    /// foreign `.html`); skipped rather than guessed.
    NoSessionId,
}

/// One capture's refresh result, for `--json` + the per-capture human lines.
#[derive(Debug, Clone, Serialize)]
struct RefreshItem {
    outcome: RefreshOutcome,
    session_id: String,
    /// The capture `.html` path (absolute).
    capture: String,
}

/// The machine summary emitted by `--refresh-subagents --json`.
#[derive(Debug, Clone, Serialize)]
struct RefreshSummary {
    updated: usize,
    no_sidecars: usize,
    no_change: usize,
    total: usize,
    dry_run: bool,
    into: String,
    transcripts_root: String,
    items: Vec<RefreshItem>,
}

/// `kb import claude-history --refresh-subagents`: re-walk EVERY existing
/// capture under `into` and rewrite its subagent digest + sidecar text
/// blocks (W0.5's `kb-session-subagents` digest AND W0.6's
/// `kb-session-sidecar-text` evidence, from the SAME [`collect_sidecars`]
/// walk) from `transcripts_root`'s sidecar directories
/// (`<transcripts_root>/<project>/<session-id>/subagents/agent-*.jsonl`,
/// searched across every project dir since a capture doesn't record which
/// project produced it — only its canonical session id), leaving the `<pre>`
/// transcript and every other tail block (e.g. commits) byte-for-byte
/// untouched. Idempotent: a second run against unchanged sidecars is a true
/// no-op across BOTH blocks (byte-identical output, landing every capture in
/// `NoChange`).
fn refresh_subagents_backfill(
    into: &Path,
    transcripts_root: &Path,
    dry_run: bool,
    limit: Option<usize>,
) -> Result<RefreshSummary> {
    if !into.exists() {
        bail!(
            "sessions corpus not found: {}\n(pass --into, or point it at an \
             existing captures dir)",
            into.display()
        );
    }

    // Every project dir directly under transcripts_root — sidecar session
    // dirs live beside each project's own transcripts, keyed by the RAW
    // session id (independent of what the main .jsonl happens to be named).
    let project_dirs: Vec<PathBuf> = if transcripts_root.exists() {
        std::fs::read_dir(transcripts_root)
            .with_context(|| format!("read transcripts root {}", transcripts_root.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect()
    } else {
        Vec::new()
    };

    let mut captures: Vec<PathBuf> = walkdir::WalkDir::new(into)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("html"))
        .collect();
    captures.sort();

    let mut items: Vec<RefreshItem> = Vec::new();
    let mut updated = 0usize;
    let mut no_sidecars = 0usize;
    let mut no_change = 0usize;

    for path in &captures {
        // Same "--limit caps NEW writes" contract as the normal import path.
        if let Some(n) = limit {
            if updated >= n {
                break;
            }
        }
        let Ok(html) = std::fs::read_to_string(path) else {
            continue; // unreadable capture; skip silently
        };
        // Canonical id (invariant #11): the embedded JSONL sessionId first,
        // the `kb-session` meta as a fallback (a truncated legacy meta is
        // never WORSE than nothing here — refresh only needs a session id to
        // find the sidecar dir, not to dedupe).
        let session_id = extract_pre(&html)
            .and_then(|pre| first_session_id(&html_unescape(&pre)))
            .or_else(|| meta_session(&html));
        let Some(session_id) = session_id else {
            items.push(RefreshItem {
                outcome: RefreshOutcome::NoSessionId,
                session_id: String::new(),
                capture: path.display().to_string(),
            });
            continue;
        };

        let sidecar_dir = project_dirs
            .iter()
            .map(|p| p.join(&session_id).join("subagents"))
            .find(|d| d.is_dir());
        let (agents, mut sidecar_texts) = match &sidecar_dir {
            Some(d) => collect_sidecars(d),
            None => (Vec::new(), Vec::new()),
        };
        // W5/R10 — workflow journals + TaskOutput snapshots ride the SAME
        // refresh (same caps, same list). TaskOutput scanning needs the
        // main transcript text, recovered from the capture's own `<pre>`
        // (byte-identical round-trip, invariant #11) — no live transcript
        // file needed, so a refresh keeps working for a project dir that's
        // since been pruned from `~/.claude/projects`.
        let extra = match &sidecar_dir {
            Some(d) => {
                let main_text = extract_pre(&html)
                    .map(|pre| html_unescape(&pre))
                    .unwrap_or_default();
                collect_extra_sidecar_sources(d, &main_text)
            }
            None => Vec::new(),
        };
        sidecar_texts.extend(extra);
        if agents.is_empty() && sidecar_texts.is_empty() {
            no_sidecars += 1;
            items.push(RefreshItem {
                outcome: RefreshOutcome::NoSidecars,
                session_id,
                capture: path.display().to_string(),
            });
            continue;
        }

        // W0.6 — refresh BOTH tail blocks from the SAME sidecar walk: the
        // subagents digest, then the raw sidecar-text evidence spliced in
        // right after it (`replace_sidecar_text_block`'s own placement
        // logic). Comparing the FINAL rewrite against the original `html`
        // (rather than checking each block separately) is what makes
        // `NoChange` mean "the pair is byte-identical to what's on disk" —
        // a session where only one of the two blocks actually changed still
        // counts as `Updated`.
        let with_subagents = kb_core::sessions::replace_subagents_block(&html, &agents);
        let sidecar_text_block = kb_core::sessions::render_sidecar_text_block(&sidecar_texts);
        let rewritten =
            kb_core::sessions::replace_sidecar_text_block(&with_subagents, sidecar_text_block);
        if rewritten == html {
            no_change += 1;
            items.push(RefreshItem {
                outcome: RefreshOutcome::NoChange,
                session_id,
                capture: path.display().to_string(),
            });
            continue;
        }

        if !dry_run {
            // Atomic replace, same discipline as `kb sessions capture`.
            let tmp = PathBuf::from(format!("{}.tmp", path.display()));
            std::fs::write(&tmp, &rewritten)
                .with_context(|| format!("write refresh tmp {}", tmp.display()))?;
            std::fs::rename(&tmp, path)
                .with_context(|| format!("finalize refresh {}", path.display()))?;
        }
        updated += 1;
        items.push(RefreshItem {
            outcome: RefreshOutcome::Updated,
            session_id,
            capture: path.display().to_string(),
        });
    }

    Ok(RefreshSummary {
        updated,
        no_sidecars,
        no_change,
        total: items.len(),
        dry_run,
        into: into.display().to_string(),
        transcripts_root: transcripts_root.display().to_string(),
        items,
    })
}

/// The hook's `jq -r '.session_id' | tr -c 'a-zA-Z0-9' '-' | cut -c1-80`:
/// every non-alphanumeric char → `-`, then the first 80 chars. For a UUID
/// this is a no-op (hyphens stay, length ≤ 36).
pub(crate) fn sanitize_sid(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(SID_MAX_CHARS)
        .collect()
}

/// One pass over a raw JSONL transcript for the three facts the import needs.
struct Scan {
    /// First non-empty `sessionId` (ground truth for the canonical id).
    session_id: Option<String>,
    /// Earliest parseable event `timestamp`, unix seconds.
    earliest: Option<i64>,
    /// Count of non-empty lines (0 ⇒ not a real transcript).
    nonempty: usize,
}

fn scan_transcript(jsonl: &str) -> Scan {
    let mut session_id = None;
    let mut earliest: Option<i64> = None;
    let mut nonempty = 0usize;
    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        nonempty += 1;
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if session_id.is_none() {
            if let Some(sid) = v.get("sessionId").and_then(|x| x.as_str()) {
                let t = sid.trim();
                if !t.is_empty() {
                    session_id = Some(t.to_string());
                }
            }
        }
        if let Some(ts) = v.get("timestamp").and_then(|x| x.as_str()) {
            if let Some(unix) = parse_iso_utc(ts) {
                earliest = Some(earliest.map_or(unix, |cur| cur.min(unix)));
            }
        }
    }
    Scan {
        session_id,
        earliest,
        nonempty,
    }
}

/// The first non-empty `sessionId` in a raw JSONL transcript, returning as
/// soon as one is found. A dedupe-seed-only fast path for `scan_existing_ids`:
/// unlike [`scan_transcript`] it does not JSON-parse every line (only lines
/// carrying the `sessionId` key, and only until the first hit) and skips the
/// unused earliest/nonempty tallies, so seeding stays cheap as the corpus grows.
fn first_session_id(jsonl: &str) -> Option<String> {
    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() || !line.contains("\"sessionId\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(sid) = v.get("sessionId").and_then(|x| x.as_str()) {
            let t = sid.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

/// Every canonical id already present in the target dir, so the import skips
/// sessions that already have a capture (live or previously imported). For
/// each existing `.html` we collect ALL identifying candidates — the embedded
/// JSONL `sessionId` (ground truth), the `kb-session` meta, and the filename
/// `sid` — so a match by ANY of them dedupes (robust to the legacy hook bug
/// that truncated the meta + filename to 24 chars while the JSONL kept the
/// full id). Mirrors how the indexer identifies a capture.
fn scan_existing_ids(into: &Path) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    if !into.exists() {
        return ids;
    }
    for entry in walkdir::WalkDir::new(into)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("html") {
            continue;
        }
        let Ok(html) = std::fs::read_to_string(path) else {
            continue;
        };
        // Embedded JSONL sessionId (ground truth). Cheap first-hit scan —
        // seeding the dedupe set only needs the first sessionId, not the
        // full per-line JSON parse `scan_transcript` does for earliest/
        // nonempty (both unused here).
        let jsonl_sid = extract_pre(&html).and_then(|pre| first_session_id(&html_unescape(&pre)));
        let have_jsonl_id = jsonl_sid.is_some();
        if let Some(sid) = jsonl_sid {
            ids.insert(sid);
        }
        // `kb-session` meta (may be the truncated legacy value) — only a
        // fallback when the JSONL scan found nothing. When the JSONL id is
        // present it is ground truth (invariant #11) and equals what an
        // incoming capture's canonical id would be, so a truncated meta can
        // never add a matching dedupe key; skipping it drops a full
        // `parser::extract` HTML DOM parse per existing capture.
        if !have_jsonl_id {
            if let Some(m) = meta_session(&html) {
                ids.insert(m);
            }
        }
        // Filename `session-<ts>-<sid>.html`.
        if let Some(fname) = path.file_name().and_then(|x| x.to_str()) {
            if let Some(sid) = filename_sid(fname) {
                ids.insert(sid);
            }
        }
    }
    ids
}

/// First `<pre>…</pre>` inner text (the escaped JSONL). Mirrors the private
/// `sessions::extract_pre` — one `<pre>` per capture, no HTML parse needed.
fn extract_pre(html: &str) -> Option<String> {
    let open = html.find("<pre>")?;
    let after = &html[open + "<pre>".len()..];
    let close = after.find("</pre>")?;
    Some(after[..close].to_string())
}

/// Reverse of `html_escape` (the parser's `html_unescape`): `&amp;` last so
/// the `&` it produces isn't re-decoded.
fn html_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// `<meta name="kb-session" content="…">` content, via the canonical parser.
pub(crate) fn meta_session(html: &str) -> Option<String> {
    kb_core::parser::extract(html).kb_session
}

/// The `<sid>` out of a `session-<YYYYMMDDTHHMMSSZ>-<sid>.html` filename.
fn filename_sid(fname: &str) -> Option<String> {
    let stem = fname.strip_suffix(".html").unwrap_or(fname);
    let rest = stem.strip_prefix("session-")?;
    // Drop the `<ts>-` prefix: the 16-char compact stamp + its trailing `-`.
    let (ts, sid) = rest.split_once('-')?;
    if ts.len() == 16 && ts.ends_with('Z') && !sid.is_empty() {
        Some(sid.to_string())
    } else {
        None
    }
}

/// A path's mtime as unix seconds (0 on any stat failure).
fn file_mtime_unix(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Unix seconds → `YYYYMMDDTHHMMSSZ`, the kb-capture.sh filename stamp
/// (`date -u +%Y%m%dT%H%M%SZ`). Parseable by `timeparse::parse_compact_utc`.
fn compact_utc(secs: i64) -> String {
    use chrono::{TimeZone, Utc};
    Utc.timestamp_opt(secs, 0)
        .single()
        .map(|dt| dt.format("%Y%m%dT%H%M%SZ").to_string())
        .unwrap_or_else(|| "19700101T000000Z".to_string())
}

/// Expand a leading `~` / `~/…` against `$HOME` (the config loader doesn't).
pub(crate) fn expand_tilde(p: PathBuf) -> PathBuf {
    let Some(s) = p.to_str() else { return p };
    if s == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
        return p;
    }
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return Path::new(&home).join(rest);
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use kb_core::sessions::parse_session_html_full;

    /// A minimal but realistic Claude Code transcript: a `sessionId`, a couple
    /// of records with timestamps, and a typed user prompt.
    fn fixture_jsonl(session_id: &str) -> String {
        format!(
            r#"{{"sessionId":"{session_id}","type":"user","cwd":"/home/u/proj","gitBranch":"main","timestamp":"2026-03-01T09:00:00.000Z","message":{{"role":"user","content":"fix the parser"}},"promptSource":"typed"}}
{{"sessionId":"{session_id}","type":"assistant","timestamp":"2026-03-01T09:05:00.000Z","message":{{"role":"assistant","model":"claude","content":[{{"type":"text","text":"done"}}]}}}}
"#
        )
    }

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, body).unwrap();
        p
    }

    fn count_html(dir: &Path) -> usize {
        walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("html"))
            .count()
    }

    fn only_html(dir: &Path) -> PathBuf {
        walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_map(Result::ok)
            .find(|e| e.path().extension().and_then(|x| x.to_str()) == Some("html"))
            .unwrap()
            .into_path()
    }

    #[test]
    fn envelope_carries_the_load_bearing_pieces() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("projects/proj-a");
        let into = tmp.path().join("sessions");
        let sid = "550e8400-e29b-41d4-a716-446655440000";
        write(&src, &format!("{sid}.jsonl"), &fixture_jsonl(sid));

        run(
            Some(tmp.path().join("projects")),
            Some(into.clone()),
            false,
            None,
            false,
            true,
            false,
            None,
            false,
        )
        .unwrap();

        assert_eq!(count_html(&into), 1);
        let out = only_html(&into);
        let name = out.file_name().unwrap().to_str().unwrap();
        // Filename shape: session-<compact-ts>-<sid>.html. The ts is the
        // earliest event (2026-03-01T09:00:00Z), NOT import-now.
        assert!(
            name.starts_with("session-20260301T090000Z-") && name.ends_with(".html"),
            "unexpected capture filename: {name}"
        );
        assert!(
            name.contains(sid),
            "filename must carry the full sid: {name}"
        );

        let html = std::fs::read_to_string(&out).unwrap();
        assert!(html.contains(r#"<meta name="kb-category" content="memory-session">"#));
        assert!(html.contains(r#"<meta name="kb-decay" content="fast">"#));
        assert!(html.contains(&format!(r#"<meta name="kb-session" content="{sid}">"#)));
        assert!(html.contains("<pre>"));

        // The clincher: the indexer's OWN recovery path recovers the full
        // canonical id + a real started_at from the produced file.
        let facts = parse_session_html_full(&html, name, 0).facts;
        assert_eq!(facts.session_id, sid, "canonical id must round-trip full");
        assert_eq!(facts.first_user_prompt.as_deref(), Some("fix the parser"));
        assert_eq!(
            facts.started_at,
            parse_iso_utc("2026-03-01T09:00:00Z").unwrap()
        );
    }

    #[test]
    fn canonical_id_prefers_jsonl_over_filename() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("projects/p");
        let into = tmp.path().join("sessions");
        let real = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        // Filename deliberately DIFFERS from the JSONL sessionId.
        write(&src, "not-the-session-id.jsonl", &fixture_jsonl(real));

        run(
            Some(tmp.path().join("projects")),
            Some(into.clone()),
            false,
            None,
            false,
            true,
            false,
            None,
            false,
        )
        .unwrap();

        let out = only_html(&into);
        let name = out.file_name().unwrap().to_str().unwrap();
        assert!(
            name.contains(real) && !name.contains("not-the-session-id"),
            "file/meta must use the JSONL id, not the filename: {name}"
        );
        let html = std::fs::read_to_string(&out).unwrap();
        assert!(html.contains(&format!(r#"content="{real}""#)));
    }

    #[test]
    fn second_run_imports_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("projects/p");
        let into = tmp.path().join("sessions");
        let sid = "11111111-2222-3333-4444-555555555555";
        write(&src, &format!("{sid}.jsonl"), &fixture_jsonl(sid));
        let projects = tmp.path().join("projects");

        run(
            Some(projects.clone()),
            Some(into.clone()),
            false,
            None,
            false,
            true,
            false,
            None,
            false,
        )
        .unwrap();
        assert_eq!(count_html(&into), 1);

        // Re-run: the seen-set (seeded from the target) skips it.
        run(
            Some(projects),
            Some(into.clone()),
            false,
            None,
            false,
            true,
            false,
            None,
            false,
        )
        .unwrap();
        assert_eq!(count_html(&into), 1, "re-import must not add a duplicate");
    }

    #[test]
    fn dedupes_against_a_preexisting_live_capture() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("projects/p");
        let into = tmp.path().join("sessions");
        let sid = "99999999-8888-7777-6666-555555555555";
        write(&src, &format!("{sid}.jsonl"), &fixture_jsonl(sid));

        // Simulate a live capture already in the corpus (hook-shaped file).
        let existing = wrap_envelope(
            "20260301T090000Z",
            &sanitize_sid(sid),
            &fixture_jsonl(sid),
            &[],
            &[],
            &[],
        );
        write(
            &into,
            &format!("session-20260301T090000Z-{sid}.html"),
            &existing,
        );
        assert_eq!(count_html(&into), 1);

        run(
            Some(tmp.path().join("projects")),
            Some(into.clone()),
            false,
            None,
            false,
            true,
            false,
            None,
            false,
        )
        .unwrap();
        assert_eq!(
            count_html(&into),
            1,
            "must not re-capture an existing session"
        );
    }

    /// FIX 3 — a legacy capture written by the STALE hook (`session-<ts>-
    /// <sid>-.html`, dirty trailing-dash filename + `kb-session` meta) must
    /// still dedupe a backfill: `scan_existing_ids` recovers the CLEAN id
    /// from the `<pre>` JSONL `sessionId` first (ground truth, invariant
    /// #11), which the trailing-dash corruption never touched, so this
    /// already works without any matching change — this test locks the
    /// behavior in as a regression guard.
    #[test]
    fn dedupes_against_a_legacy_trailing_dash_capture() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("projects/p");
        let into = tmp.path().join("sessions");
        let sid = "88888888-7777-6666-5555-444444444444";
        write(&src, &format!("{sid}.jsonl"), &fixture_jsonl(sid));

        // A legacy-shaped capture: dirty (trailing-dash) filename + meta,
        // exactly what the stale hook wrote — the embedded `<pre>` JSONL
        // carries the clean id untouched, as on every real legacy artifact.
        let legacy = wrap_envelope(
            "20260301T090000Z",
            &format!("{sid}-"),
            &fixture_jsonl(sid),
            &[],
            &[],
            &[],
        );
        write(
            &into,
            &format!("session-20260301T090000Z-{sid}-.html"),
            &legacy,
        );
        assert_eq!(count_html(&into), 1);

        let summary = import(&tmp.path().join("projects"), &into, false, None, false).unwrap();
        assert_eq!(
            summary.skipped_duplicate, 1,
            "the legacy capture must count as already-present"
        );
        assert_eq!(summary.imported, 0);
        assert_eq!(
            count_html(&into),
            1,
            "no duplicate written alongside the legacy capture"
        );
    }

    #[test]
    fn dry_run_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("projects/p");
        let into = tmp.path().join("sessions");
        let sid = "abcdef00-1111-2222-3333-444444444444";
        write(&src, &format!("{sid}.jsonl"), &fixture_jsonl(sid));

        run(
            Some(tmp.path().join("projects")),
            Some(into.clone()),
            true, // dry-run
            None,
            false,
            true,
            false,
            None,
            false,
        )
        .unwrap();
        // The target dir was never even created.
        assert_eq!(count_html(&into), 0, "dry-run must not write any capture");
    }

    #[test]
    fn missing_into_errors_naming_flag_and_env() {
        // With no --into and KB_SESSIONS_DIR unset, resolve_into must fail
        // with a message naming both. (env manipulation is process-global;
        // keep it local + restore.)
        let saved = std::env::var_os(SESSIONS_DIR_ENV);
        std::env::remove_var(SESSIONS_DIR_ENV);
        let err = resolve_into(None).unwrap_err().to_string();
        assert!(err.contains("--into"), "err should name the flag: {err}");
        assert!(
            err.contains(SESSIONS_DIR_ENV),
            "err should name the env var: {err}"
        );
        if let Some(v) = saved {
            std::env::set_var(SESSIONS_DIR_ENV, v);
        }
    }

    #[test]
    fn empty_transcript_is_skipped_other() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("projects/p");
        let into = tmp.path().join("sessions");
        write(&src, "stub.jsonl", "   \n\n");

        run(
            Some(tmp.path().join("projects")),
            Some(into.clone()),
            false,
            None,
            false,
            true,
            false,
            None,
            false,
        )
        .unwrap();
        assert_eq!(
            count_html(&into),
            0,
            "an empty stub must not become a session"
        );
    }

    /// 2026-08-21 ci-host incident, defect 1 — a transcript over
    /// [`kb_core::sessions::CAPTURE_MAX_TRANSCRIPT_BYTES`] must be SKIPPED
    /// (not read into memory, not aborting the sweep), counted in
    /// `skipped_oversized`, and NOT imported. `File::set_len` mints a
    /// sparse file at cap+1 bytes so the fixture doesn't need to actually
    /// contain 48MiB.
    #[test]
    fn oversized_transcript_is_skipped_and_counted_separately() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("projects/p");
        let into = tmp.path().join("sessions");
        std::fs::create_dir_all(&src).unwrap();
        let huge = src.join("huge.jsonl");
        let f = std::fs::File::create(&huge).unwrap();
        f.set_len(kb_core::sessions::CAPTURE_MAX_TRANSCRIPT_BYTES + 1)
            .unwrap();
        // A normal-sized transcript alongside it must still import — one
        // monster file must not abort the whole sweep.
        let sid = "aaaa0000-1111-2222-3333-444444444444";
        write(&src, &format!("{sid}.jsonl"), &fixture_jsonl(sid));

        let summary = import(&tmp.path().join("projects"), &into, false, None, false).unwrap();
        assert_eq!(summary.skipped_oversized, 1);
        assert_eq!(
            summary.imported, 1,
            "the normal-sized sibling still imports"
        );
        assert_eq!(summary.skipped_other, 0);
        assert_eq!(count_html(&into), 1);
    }

    /// `--allow-oversized` must let the oversized file through to the
    /// normal read/wrap path instead of skipping it.
    #[test]
    fn allow_oversized_imports_the_huge_transcript_anyway() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("projects/p");
        let into = tmp.path().join("sessions");
        std::fs::create_dir_all(&src).unwrap();
        let sid = "bbbb0000-1111-2222-3333-444444444444";
        let huge = src.join(format!("{sid}.jsonl"));
        // A real (if wasteful) oversized-but-parseable transcript: the
        // fixture JSONL padded with a trailing comment-shaped filler line
        // so it both carries a recoverable sessionId AND exceeds the cap.
        let mut body = fixture_jsonl(sid);
        body.push_str(&"x".repeat((kb_core::sessions::CAPTURE_MAX_TRANSCRIPT_BYTES + 1) as usize));
        std::fs::write(&huge, &body).unwrap();

        let summary = import(&tmp.path().join("projects"), &into, false, None, true).unwrap();
        assert_eq!(
            summary.skipped_oversized, 0,
            "the override must prevent the size-skip"
        );
        assert_eq!(summary.imported, 1);
        assert_eq!(count_html(&into), 1);
    }

    #[test]
    fn limit_caps_new_captures() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("projects/p");
        let into = tmp.path().join("sessions");
        for i in 0..5 {
            let sid = format!("session-{i:02}-0000-0000-0000-000000000000");
            write(&src, &format!("{sid}.jsonl"), &fixture_jsonl(&sid));
        }

        run(
            Some(tmp.path().join("projects")),
            Some(into.clone()),
            false,
            Some(2),
            false,
            true,
            false,
            None,
            false,
        )
        .unwrap();
        assert_eq!(count_html(&into), 2, "--limit 2 imports exactly two");
    }

    #[test]
    fn subagent_workflow_journals_are_never_walked() {
        // `<project>/<session>/subagents/workflows/wf_*/journal.jsonl` are
        // Workflow-tool agent journals, NOT transcripts. The depth-capped
        // walk must not even scan them — they are neither imported nor
        // counted as duplicates (a filename fallback once collapsed them
        // all onto one phantom "journal" session).
        let tmp = tempfile::tempdir().unwrap();
        let projects = tmp.path().join("projects");
        let into = tmp.path().join("sessions");
        let sid = "12121212-3434-5656-7878-909090909090";
        write(
            &projects.join("proj-a"),
            &format!("{sid}.jsonl"),
            &fixture_jsonl(sid),
        );
        // Journals under two different fake session dirs (no sessionId,
        // like the real ones) — plus one WITH a sessionId to prove depth,
        // not content, is what excludes them.
        write(
            &projects.join(format!("proj-a/{sid}/subagents/workflows/wf_1")),
            "journal.jsonl",
            r#"{"type":"progress","timestamp":"2026-03-01T09:00:00Z"}"#,
        );
        write(
            &projects.join("proj-b/other-session/subagents/workflows/wf_2"),
            "journal.jsonl",
            &fixture_jsonl("deep-should-never-be-seen"),
        );

        let summary = import(&projects, &into, false, None, false).unwrap();
        assert_eq!(summary.imported, 1, "only the depth-2 transcript imports");
        assert_eq!(
            summary.skipped_duplicate, 0,
            "journals must not collide as dups"
        );
        assert_eq!(
            summary.skipped_other, 0,
            "journals must not be scanned at all"
        );
        assert_eq!(summary.total, 1);
        assert_eq!(count_html(&into), 1);
        let name = only_html(&into);
        let name = name.file_name().unwrap().to_str().unwrap();
        assert!(
            name.contains(sid) && !name.contains("journal"),
            "no journal capture may exist: {name}"
        );
    }

    /// W0.6 — the PLAIN `kb import claude-history` path (no
    /// `--refresh-subagents`) walks each imported transcript's OWN sidecar
    /// dir (`<project>/<session-id>/subagents/agent-*.jsonl`, directly
    /// beside the `.jsonl` being imported) and folds the result into BOTH
    /// tail blocks — a backfilled capture's searchable surface matches a
    /// live one when sidecars happen to be sitting right there already.
    #[test]
    fn import_walks_sidecars_and_emits_digest_and_text_blocks() {
        let tmp = tempfile::tempdir().unwrap();
        let projects = tmp.path().join("projects");
        let into = tmp.path().join("sessions");
        let sid = "22222222-3333-4444-5555-666666666666";
        write(
            &projects.join("proj-a"),
            &format!("{sid}.jsonl"),
            &fixture_jsonl(sid),
        );
        let raw = "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"name\":\"Edit\",\"input\":{\"file_path\":\"/p/x.rs\"}}]}}\n";
        write(
            &projects.join(format!("proj-a/{sid}/subagents")),
            "agent-a1.jsonl",
            raw,
        );

        let summary = import(&projects, &into, false, None, false).unwrap();
        assert_eq!(summary.imported, 1);

        let html = std::fs::read_to_string(only_html(&into)).unwrap();
        let digest = kb_core::sessions::extract_subagents_block(&html).unwrap();
        assert_eq!(digest.agents.len(), 1);
        assert_eq!(digest.agents[0].agent_id, "a1");
        assert_eq!(digest.agents[0].files[0].path, "/p/x.rs");

        let texts = kb_core::sessions::extract_sidecar_text_block(&html);
        assert_eq!(texts, vec![("a1".to_string(), raw.to_string())]);
    }

    // --- W5/R10 — collect_extra_sidecar_sources -------------------------

    #[test]
    fn collect_extra_sidecar_sources_folds_in_a_workflow_journal() {
        let tmp = tempfile::tempdir().unwrap();
        let subagents_dir = tmp.path().join("subagents");
        let journal = "{\"type\":\"started\",\"agentId\":\"a1\"}\n{\"type\":\"result\",\"agentId\":\"a1\",\"result\":{\"headline\":\"done\"}}\n";
        write(
            &subagents_dir.join("workflows/wf_abc"),
            "journal.jsonl",
            journal,
        );
        // A workflow dir with per-agent .meta.json stubs only (no
        // agent-*.jsonl) must not itself be picked up — only journal.jsonl.
        write(
            &subagents_dir.join("workflows/wf_abc"),
            "agent-a1.meta.json",
            "{\"agentType\":\"workflow-subagent\"}",
        );

        let extra = collect_extra_sidecar_sources(&subagents_dir, "");
        assert_eq!(
            extra,
            vec![("workflow:wf_abc".to_string(), journal.to_string())]
        );
    }

    #[test]
    fn collect_extra_sidecar_sources_orders_multiple_workflows_deterministically() {
        let tmp = tempfile::tempdir().unwrap();
        let subagents_dir = tmp.path().join("subagents");
        write(
            &subagents_dir.join("workflows/wf_b"),
            "journal.jsonl",
            "b\n",
        );
        write(
            &subagents_dir.join("workflows/wf_a"),
            "journal.jsonl",
            "a\n",
        );
        let extra = collect_extra_sidecar_sources(&subagents_dir, "");
        assert_eq!(
            extra,
            vec![
                ("workflow:wf_a".to_string(), "a\n".to_string()),
                ("workflow:wf_b".to_string(), "b\n".to_string()),
            ]
        );
    }

    #[test]
    fn collect_extra_sidecar_sources_snapshots_an_existing_task_output_file() {
        let tmp = tempfile::tempdir().unwrap();
        let subagents_dir = tmp.path().join("subagents"); // absent — fine
        let tasks_dir = tmp.path().join("scratch/tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();
        std::fs::write(
            tasks_dir.join("wl75we7oz.output"),
            "the full command output\n",
        )
        .unwrap();
        let main = format!(
            "[Truncated. Full output: {}]",
            tasks_dir.join("wl75we7oz.output").display()
        );

        let extra = collect_extra_sidecar_sources(&subagents_dir, &main);
        assert_eq!(
            extra,
            vec![(
                "task-output:wl75we7oz".to_string(),
                "the full command output\n".to_string()
            )]
        );
    }

    #[test]
    fn collect_extra_sidecar_sources_skips_a_task_output_marker_whose_file_is_gone() {
        // /tmp is ephemeral — the referenced path never existing (or having
        // been cleaned up since capture) is the COMMON case, not an error.
        let tmp = tempfile::tempdir().unwrap();
        let subagents_dir = tmp.path().join("subagents");
        let main = "Full output: /tmp/does-not-exist-kb-fixture/tasks/xyz.output";
        let extra = collect_extra_sidecar_sources(&subagents_dir, main);
        assert!(extra.is_empty());
    }

    #[test]
    fn collect_extra_sidecar_sources_empty_when_neither_source_present() {
        let tmp = tempfile::tempdir().unwrap();
        let subagents_dir = tmp.path().join("subagents");
        std::fs::create_dir_all(&subagents_dir).unwrap();
        assert!(collect_extra_sidecar_sources(&subagents_dir, "no markers here").is_empty());
    }

    /// End-to-end: a session whose ONLY sidecar evidence is a
    /// workflow-launched subagent (no DIRECT `agent-*.jsonl` — the real
    /// on-disk shape verified live) still lands sidecar-text evidence in
    /// the capture, riding `import()`'s normal backfill path.
    #[test]
    fn import_folds_a_workflow_only_session_into_the_sidecar_text_block() {
        let tmp = tempfile::tempdir().unwrap();
        let projects = tmp.path().join("projects");
        let into = tmp.path().join("sessions");
        let sid = "44444444-5555-6666-7777-888888888888";
        write(
            &projects.join("proj-a"),
            &format!("{sid}.jsonl"),
            &fixture_jsonl(sid),
        );
        let journal = "{\"type\":\"result\",\"agentId\":\"a9\",\"result\":{\"headline\":\"ok\"}}\n";
        write(
            &projects.join(format!("proj-a/{sid}/subagents/workflows/wf_x")),
            "journal.jsonl",
            journal,
        );

        let summary = import(&projects, &into, false, None, false).unwrap();
        assert_eq!(summary.imported, 1);

        let html = std::fs::read_to_string(only_html(&into)).unwrap();
        // No DIRECT agent-*.jsonl sidecars, so the structured digest block
        // is absent (unchanged behavior) — but the sidecar-TEXT evidence
        // block carries the workflow journal.
        assert!(kb_core::sessions::extract_subagents_block(&html).is_none());
        let texts = kb_core::sessions::extract_sidecar_text_block(&html);
        assert_eq!(
            texts,
            vec![("workflow:wf_x".to_string(), journal.to_string())]
        );
    }

    /// A backfilled session with NO sidecar dir at all keeps the pre-W0.6
    /// behavior exactly: neither tail block is written.
    #[test]
    fn import_without_sidecars_omits_both_subagent_blocks() {
        let tmp = tempfile::tempdir().unwrap();
        let projects = tmp.path().join("projects");
        let into = tmp.path().join("sessions");
        let sid = "33333333-4444-5555-6666-777777777777";
        write(
            &projects.join("proj-a"),
            &format!("{sid}.jsonl"),
            &fixture_jsonl(sid),
        );

        let summary = import(&projects, &into, false, None, false).unwrap();
        assert_eq!(summary.imported, 1);

        let html = std::fs::read_to_string(only_html(&into)).unwrap();
        assert!(kb_core::sessions::extract_subagents_block(&html).is_none());
        assert!(kb_core::sessions::extract_sidecar_text_block(&html).is_empty());
    }

    #[test]
    fn no_session_id_transcript_is_skipped_not_imported() {
        // A top-level `.jsonl` whose content carries no recoverable
        // `sessionId` is skipped-other ("no sessionId") — never imported
        // via a filename fallback.
        let tmp = tempfile::tempdir().unwrap();
        let projects = tmp.path().join("projects");
        let into = tmp.path().join("sessions");
        write(
            &projects.join("p"),
            "orphan.jsonl",
            r#"{"type":"user","timestamp":"2026-03-01T09:00:00Z","message":{"role":"user","content":"hi"}}"#,
        );

        let summary = import(&projects, &into, false, None, false).unwrap();
        assert_eq!(summary.imported, 0);
        assert_eq!(summary.skipped_other, 1);
        assert_eq!(summary.items[0].reason, Some("no sessionId"));
        assert_eq!(
            count_html(&into),
            0,
            "no capture may be minted without a sessionId"
        );
    }

    #[test]
    fn first_session_id_returns_first_hit_and_matches_scan_transcript() {
        // The cheap dedupe-seed path must recover the same id `scan_transcript`
        // does — the FIRST non-empty sessionId — while tolerating a leading
        // line that carries none.
        let jsonl = fixture_jsonl("77777777-1111-2222-3333-444444444444");
        assert_eq!(
            first_session_id(&jsonl),
            scan_transcript(&jsonl).session_id,
            "fast path must agree with the full scan"
        );

        let with_prelude = format!(
            "{}\n{}",
            r#"{"type":"summary","timestamp":"2026-03-01T08:59:00Z"}"#, jsonl
        );
        assert_eq!(
            first_session_id(&with_prelude).as_deref(),
            Some("77777777-1111-2222-3333-444444444444"),
            "a leading sessionId-less line must not stop the scan"
        );

        // No sessionId anywhere, blank lines, and unparseable junk → None.
        assert_eq!(first_session_id("   \n\nnot json\n"), None);
        assert_eq!(
            first_session_id(r#"{"type":"user","timestamp":"2026-03-01T09:00:00Z"}"#),
            None
        );
    }

    #[test]
    fn scan_existing_ids_dedupes_via_the_jsonl_ground_truth() {
        // A live-shaped capture in the target seeds the dedupe set by its
        // embedded JSONL sessionId even when meta/filename are absent/differ.
        let tmp = tempfile::tempdir().unwrap();
        let into = tmp.path().join("sessions");
        let sid = "abababab-cdcd-efef-0101-232323232323";
        let existing = wrap_envelope(
            "20260301T090000Z",
            &sanitize_sid(sid),
            &fixture_jsonl(sid),
            &[],
            &[],
            &[],
        );
        write(
            &into,
            &format!("session-20260301T090000Z-{sid}.html"),
            &existing,
        );

        let ids = scan_existing_ids(&into);
        assert!(
            ids.contains(sid),
            "the full JSONL sessionId must seed dedupe"
        );
    }

    /// FIX 3 — the legacy trailing-dash shape specifically: dirty filename
    /// AND dirty `kb-session` meta (both carry `<sid>-`), but the `<pre>`
    /// JSONL is unaffected. `scan_existing_ids` must still seed the CLEAN
    /// id, since it consults the JSONL ground truth before the meta/
    /// filename fallbacks (invariant #11 — JSONL id > meta > filename).
    #[test]
    fn scan_existing_ids_dedupes_a_legacy_trailing_dash_capture_via_jsonl_ground_truth() {
        let tmp = tempfile::tempdir().unwrap();
        let into = tmp.path().join("sessions");
        let sid = "cdcdcdcd-efef-0101-2323-454545454545";
        let legacy = wrap_envelope(
            "20260301T090000Z",
            &format!("{sid}-"),
            &fixture_jsonl(sid),
            &[],
            &[],
            &[],
        );
        write(
            &into,
            &format!("session-20260301T090000Z-{sid}-.html"),
            &legacy,
        );

        let ids = scan_existing_ids(&into);
        assert!(
            ids.contains(sid),
            "the clean id must be recovered from the <pre> JSONL despite \
             the dirty filename/meta"
        );
    }

    #[test]
    fn envelope_is_byte_identical_to_the_hook_heredoc() {
        // Guard against envelope drift from kb-capture.sh's cat<<EOF block.
        // No commits/subagents ⇒ no tail block ⇒ byte-identical to the
        // ORIGINAL (pre-W0.4) heredoc in full, not just through `</pre>`.
        let got = wrap_envelope("20260301T090000Z", "sid-123", "a<b>&c", &[], &[], &[]);
        let want = "<!DOCTYPE html>\n\
             <html lang=\"en\"><head><meta charset=\"utf-8\">\n\
             <title>Session transcript 20260301T090000Z</title>\n\
             <meta name=\"kb-category\" content=\"memory-session\">\n\
             <meta name=\"kb-decay\" content=\"fast\">\n\
             <meta name=\"kb-session\" content=\"sid-123\">\n\
             </head><body>\n\
             <h1>Session transcript 20260301T090000Z</h1>\n\
             <pre>a&lt;b&gt;&amp;c</pre>\n\
             </body></html>\n";
        assert_eq!(got, want);
    }

    /// W0.4 split contract — the SAME `wrap_envelope` call with commits
    /// present must keep the pre-block (through `</pre>`) BYTE-IDENTICAL to
    /// the no-commits case, then append an additive tail. Verifies both
    /// halves of the contract in one test: pre-block identity + tail
    /// presence/shape.
    #[test]
    fn envelope_pre_block_is_byte_identical_with_commits_appended_as_an_additive_tail() {
        let empty = wrap_envelope("20260301T090000Z", "sid-123", "a<b>&c", &[], &[], &[]);
        let commits = vec![kb_core::sessions::CapturedCommit {
            kind: "commit".to_string(),
            sha: Some("abc1234".to_string()),
            subject: Some("feat: x".to_string()),
            resolved: true,
            sha_full: Some("abc1234def5678".to_string()),
            repo_root: Some("/home/u/proj".to_string()),
            author: Some("kb-test <test@kb>".to_string()),
            parents: Some(1),
            trailers: vec!["Kb-Session: sid-123".to_string()],
        }];
        let with_commits =
            wrap_envelope("20260301T090000Z", "sid-123", "a<b>&c", &commits, &[], &[]);

        // Pre-block byte-identity: everything through `</pre>\n` matches the
        // no-commits envelope exactly.
        let pre_end = empty.find("</pre>\n").unwrap() + "</pre>\n".len();
        assert_eq!(&with_commits[..pre_end], &empty[..pre_end]);
        assert_eq!(&empty[..pre_end], &with_commits[..pre_end]);

        // The tail: present, well-formed, and immediately followed by the
        // closing body/html tags.
        let tail = &with_commits[pre_end..];
        assert!(tail.starts_with(r#"<script type="application/json" id="kb-session-commits">"#));
        assert!(tail.contains("</body></html>\n"));
        assert!(tail.contains("abc1234def5678"));
        assert!(tail.contains("Kb-Session: sid-123"));

        // Absent case has NO script tag at all.
        assert!(!empty.contains("<script"));

        // The tail round-trips through the canonical extractor.
        let got = kb_core::sessions::extract_commits_block(&with_commits).unwrap();
        assert_eq!(got, commits);

        // And the `<pre>` recovery is unaffected either way — the raw JSONL
        // recovers identically whether or not the tail block is present.
        assert_eq!(
            kb_core::sessions::recover_jsonl_from_capture(&empty),
            kb_core::sessions::recover_jsonl_from_capture(&with_commits)
        );
    }

    /// W0.5 — both tail blocks can coexist: commits block first, subagents
    /// block second, both independently extractable.
    #[test]
    fn wrap_envelope_appends_subagents_block_after_commits_block() {
        let commits = vec![kb_core::sessions::CapturedCommit {
            kind: "commit".to_string(),
            sha: Some("abc1234".to_string()),
            resolved: true,
            ..Default::default()
        }];
        let agents = vec![kb_core::sessions::SubagentDigest {
            agent_id: "a1".to_string(),
            files: vec![],
            tokens: 5,
            tool_calls: 1,
            errors: 0,
        }];
        let html = wrap_envelope(
            "20260301T090000Z",
            "sid-123",
            "a<b>&c",
            &commits,
            &agents,
            &[],
        );
        let commits_at = html.find(r#"id="kb-session-commits""#).unwrap();
        let subagents_at = html.find(r#"id="kb-session-subagents""#).unwrap();
        assert!(
            commits_at < subagents_at,
            "commits block must precede the subagents block"
        );
        assert_eq!(
            kb_core::sessions::extract_commits_block(&html).unwrap(),
            commits
        );
        assert_eq!(
            kb_core::sessions::extract_subagents_block(&html)
                .unwrap()
                .agents,
            agents
        );
    }

    #[test]
    fn wrap_envelope_subagents_only_omits_the_commits_block() {
        let agents = vec![kb_core::sessions::SubagentDigest {
            agent_id: "a1".to_string(),
            files: vec![],
            tokens: 5,
            tool_calls: 1,
            errors: 0,
        }];
        let html = wrap_envelope("20260301T090000Z", "sid-123", "a<b>&c", &[], &agents, &[]);
        assert!(kb_core::sessions::extract_commits_block(&html).is_none());
        assert_eq!(
            kb_core::sessions::extract_subagents_block(&html)
                .unwrap()
                .agents,
            agents
        );
    }

    /// W0.6 — all THREE tail blocks can coexist in digest-then-evidence
    /// order: commits, then the subagents digest, then the raw sidecar-text
    /// evidence — each independently extractable.
    #[test]
    fn wrap_envelope_appends_sidecar_text_block_after_subagents_block() {
        let commits = vec![kb_core::sessions::CapturedCommit {
            kind: "commit".to_string(),
            sha: Some("abc1234".to_string()),
            resolved: true,
            ..Default::default()
        }];
        let agents = vec![kb_core::sessions::SubagentDigest {
            agent_id: "a1".to_string(),
            files: vec![],
            tokens: 5,
            tool_calls: 1,
            errors: 0,
        }];
        let sidecar_texts = vec![("a1".to_string(), "raw a1 sidecar jsonl".to_string())];
        let html = wrap_envelope(
            "20260301T090000Z",
            "sid-123",
            "a<b>&c",
            &commits,
            &agents,
            &sidecar_texts,
        );
        let commits_at = html.find(r#"id="kb-session-commits""#).unwrap();
        let subagents_at = html.find(r#"id="kb-session-subagents""#).unwrap();
        let sidecar_text_at = html.find(r#"id="kb-session-sidecar-text""#).unwrap();
        assert!(
            commits_at < subagents_at && subagents_at < sidecar_text_at,
            "tail blocks must land in digest-then-evidence order: {html}"
        );
        assert_eq!(
            kb_core::sessions::extract_commits_block(&html).unwrap(),
            commits
        );
        assert_eq!(
            kb_core::sessions::extract_subagents_block(&html)
                .unwrap()
                .agents,
            agents
        );
        assert_eq!(
            kb_core::sessions::extract_sidecar_text_block(&html),
            sidecar_texts
        );
        // The `<pre>` recovery is unaffected by the extra tail block.
        assert_eq!(
            kb_core::sessions::recover_jsonl_from_capture(&html).unwrap(),
            "a<b>&c"
        );
    }

    /// A session with sidecars but no `agent-*.jsonl` output for the
    /// sidecar-text block (e.g. every sidecar file is unreadable) never
    /// happens in practice since both blocks come from the same walk — but
    /// the no-sidecars-at-all case must omit the sidecar-text block exactly
    /// like the subagents block.
    #[test]
    fn wrap_envelope_omits_sidecar_text_block_when_no_sidecars() {
        let html = wrap_envelope("20260301T090000Z", "sid-123", "a<b>&c", &[], &[], &[]);
        assert!(kb_core::sessions::extract_sidecar_text_block(&html).is_empty());
        assert!(!html.contains("kb-session-sidecar-text"));
    }

    // --- `--refresh-subagents` backfill (W0.5) ------------------------------

    fn write_sidecar(root: &Path, project: &str, session_id: &str, agent: &str, body: &str) {
        write(
            &root.join(project).join(session_id).join("subagents"),
            &format!("agent-{agent}.jsonl"),
            body,
        );
    }

    #[test]
    fn refresh_subagents_backfill_rewrites_the_digest_block() {
        let tmp = tempfile::tempdir().unwrap();
        let into = tmp.path().join("sessions");
        let roots = tmp.path().join("projects");
        let sid = "sess-refresh-1";

        let base = wrap_envelope(
            "20260301T090000Z",
            &sanitize_sid(sid),
            &fixture_jsonl(sid),
            &[],
            &[],
            &[],
        );
        write(
            &into,
            &format!("session-20260301T090000Z-{sid}.html"),
            &base,
        );

        let raw = "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"name\":\"Edit\",\"input\":{\"file_path\":\"/p/z.rs\"}}]}}\n";
        write_sidecar(&roots, "proj-a", sid, "1", raw);

        let summary = refresh_subagents_backfill(&into, &roots, false, None).unwrap();
        assert_eq!(summary.updated, 1);
        assert_eq!(summary.no_sidecars, 0);
        assert_eq!(summary.no_change, 0);

        let html = std::fs::read_to_string(only_html(&into)).unwrap();
        let block = kb_core::sessions::extract_subagents_block(&html).unwrap();
        assert_eq!(block.agents.len(), 1);
        assert_eq!(block.agents[0].agent_id, "1");
        assert_eq!(block.agents[0].files[0].path, "/p/z.rs");
        // W0.6 — the sidecar-text block is refreshed FROM THE SAME walk.
        assert_eq!(
            kb_core::sessions::extract_sidecar_text_block(&html),
            vec![("1".to_string(), raw.to_string())]
        );
        // The `<pre>` transcript is untouched.
        assert_eq!(
            kb_core::sessions::recover_jsonl_from_capture(&base),
            kb_core::sessions::recover_jsonl_from_capture(&html)
        );
    }

    /// W0.6 retrofit — a capture that ALREADY carries the W0.5 subagents
    /// digest block (from a prior refresh, before W0.6 shipped) but has NO
    /// sidecar-text block yet must gain one on the next refresh, without
    /// disturbing the pre-existing digest content.
    #[test]
    fn refresh_subagents_backfill_retrofits_the_sidecar_text_block_onto_an_existing_digest() {
        let tmp = tempfile::tempdir().unwrap();
        let into = tmp.path().join("sessions");
        let roots = tmp.path().join("projects");
        let sid = "sess-refresh-retrofit";

        let raw = "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"name\":\"Read\",\"input\":{\"file_path\":\"/p/y.rs\"}}]}}\n";
        // Pre-W0.6 shape: the envelope already carries a subagents digest
        // block (built straight from `render_subagents_block`, the way a
        // pre-W0.6 `--refresh-subagents` run would have left it) but no
        // sidecar-text block at all.
        let pre_agents = vec![kb_core::sessions::parse_subagent_jsonl(raw, "1")];
        let pre_w06 = wrap_envelope(
            "20260301T090000Z",
            &sanitize_sid(sid),
            &fixture_jsonl(sid),
            &[],
            &pre_agents,
            &[],
        );
        assert!(
            kb_core::sessions::extract_sidecar_text_block(&pre_w06).is_empty(),
            "fixture must start with no sidecar-text block"
        );
        write(
            &into,
            &format!("session-20260301T090000Z-{sid}.html"),
            &pre_w06,
        );
        write_sidecar(&roots, "proj-a", sid, "1", raw);

        let summary = refresh_subagents_backfill(&into, &roots, false, None).unwrap();
        assert_eq!(summary.updated, 1, "the missing block must be retrofitted");

        let html = std::fs::read_to_string(only_html(&into)).unwrap();
        assert_eq!(
            kb_core::sessions::extract_sidecar_text_block(&html),
            vec![("1".to_string(), raw.to_string())],
            "the sidecar-text block now exists"
        );
        // The digest block that was ALREADY there survives, unchanged.
        assert_eq!(
            kb_core::sessions::extract_subagents_block(&html)
                .unwrap()
                .agents,
            pre_agents
        );
        assert_eq!(
            kb_core::sessions::recover_jsonl_from_capture(&pre_w06),
            kb_core::sessions::recover_jsonl_from_capture(&html)
        );
    }

    /// FIX 3 parity — `--refresh-subagents` must find + rewrite a LEGACY
    /// trailing-dash-named capture (`session-<ts>-<sid>-.html`, the stale
    /// hook's dirty filename/meta shape) exactly as it does a clean-named
    /// one: both tail blocks land, the filename is never renamed, and the
    /// `<pre>` transcript stays untouched.
    #[test]
    fn refresh_subagents_backfill_refreshes_a_legacy_trailing_dash_capture_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let into = tmp.path().join("sessions");
        let roots = tmp.path().join("projects");
        let sid = "sess-refresh-legacy";

        // The legacy shape: dirty (trailing-dash) filename AND `kb-session`
        // meta — the embedded `<pre>` JSONL carries the clean id, untouched
        // (same fixture shape `sessions_capture.rs`'s legacy-capture tests
        // use).
        let base = wrap_envelope(
            "20260301T090000Z",
            &format!("{sid}-"),
            &fixture_jsonl(sid),
            &[],
            &[],
            &[],
        );
        let legacy_name = format!("session-20260301T090000Z-{sid}-.html");
        write(&into, &legacy_name, &base);

        let raw =
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"legacy sidecar\"}}\n";
        write_sidecar(&roots, "proj-a", sid, "1", raw);

        let summary = refresh_subagents_backfill(&into, &roots, false, None).unwrap();
        assert_eq!(summary.updated, 1);
        assert_eq!(
            summary.items[0].capture,
            into.join(&legacy_name).display().to_string(),
            "the legacy filename is rewritten in place, never renamed"
        );
        assert_eq!(count_html(&into), 1, "no duplicate file minted");

        let html = std::fs::read_to_string(into.join(&legacy_name)).unwrap();
        assert!(kb_core::sessions::extract_subagents_block(&html).is_some());
        assert_eq!(
            kb_core::sessions::extract_sidecar_text_block(&html),
            vec![("1".to_string(), raw.to_string())]
        );
        assert_eq!(
            kb_core::sessions::recover_jsonl_from_capture(&base),
            kb_core::sessions::recover_jsonl_from_capture(&html)
        );

        // A second run against the SAME unchanged sidecar is a true no-op.
        let second = refresh_subagents_backfill(&into, &roots, false, None).unwrap();
        assert_eq!(second.updated, 0);
        assert_eq!(second.no_change, 1);
    }

    #[test]
    fn refresh_subagents_backfill_is_idempotent_on_a_second_run() {
        let tmp = tempfile::tempdir().unwrap();
        let into = tmp.path().join("sessions");
        let roots = tmp.path().join("projects");
        let sid = "sess-refresh-2";
        let base = wrap_envelope(
            "20260301T090000Z",
            &sanitize_sid(sid),
            &fixture_jsonl(sid),
            &[],
            &[],
            &[],
        );
        write(
            &into,
            &format!("session-20260301T090000Z-{sid}.html"),
            &base,
        );
        write_sidecar(
            &roots,
            "proj-a",
            sid,
            "1",
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
        );

        let first = refresh_subagents_backfill(&into, &roots, false, None).unwrap();
        assert_eq!(first.updated, 1);
        let after_first = std::fs::read_to_string(only_html(&into)).unwrap();

        let second = refresh_subagents_backfill(&into, &roots, false, None).unwrap();
        assert_eq!(second.updated, 0);
        assert_eq!(
            second.no_change, 1,
            "unchanged sidecars must be a true no-op on re-run"
        );
        let after_second = std::fs::read_to_string(only_html(&into)).unwrap();
        assert_eq!(after_first, after_second, "byte-identical on the re-run");
    }

    #[test]
    fn refresh_subagents_backfill_reports_no_sidecars_and_leaves_the_file_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let into = tmp.path().join("sessions");
        let roots = tmp.path().join("projects"); // never created — no sidecars anywhere
        let sid = "sess-refresh-3";
        let base = wrap_envelope(
            "20260301T090000Z",
            &sanitize_sid(sid),
            &fixture_jsonl(sid),
            &[],
            &[],
            &[],
        );
        write(
            &into,
            &format!("session-20260301T090000Z-{sid}.html"),
            &base,
        );

        let summary = refresh_subagents_backfill(&into, &roots, false, None).unwrap();
        assert_eq!(summary.updated, 0);
        assert_eq!(summary.no_sidecars, 1);
        let html = std::fs::read_to_string(only_html(&into)).unwrap();
        assert_eq!(html, base, "no sidecars ⇒ the capture is left untouched");
    }

    /// W5/R10 — a session with a workflow journal but NO direct
    /// `agent-*.jsonl` sidecar must NOT be reported `NoSidecars` (the
    /// pre-fix behavior, which would silently drop the only sidecar
    /// evidence this session has) — it lands `Updated` with the journal in
    /// the sidecar-text block.
    #[test]
    fn refresh_subagents_backfill_picks_up_a_workflow_only_session() {
        let tmp = tempfile::tempdir().unwrap();
        let into = tmp.path().join("sessions");
        let roots = tmp.path().join("projects");
        let sid = "sess-refresh-wf";
        let base = wrap_envelope(
            "20260301T090000Z",
            &sanitize_sid(sid),
            &fixture_jsonl(sid),
            &[],
            &[],
            &[],
        );
        write(
            &into,
            &format!("session-20260301T090000Z-{sid}.html"),
            &base,
        );
        let journal = "{\"type\":\"result\",\"agentId\":\"a1\",\"result\":{\"headline\":\"ok\"}}\n";
        write(
            &roots.join(format!("proj-a/{sid}/subagents/workflows/wf_only")),
            "journal.jsonl",
            journal,
        );

        let summary = refresh_subagents_backfill(&into, &roots, false, None).unwrap();
        assert_eq!(summary.no_sidecars, 0, "{:?}", summary.items);
        assert_eq!(summary.updated, 1);
        let html = std::fs::read_to_string(only_html(&into)).unwrap();
        assert!(kb_core::sessions::extract_subagents_block(&html).is_none());
        let texts = kb_core::sessions::extract_sidecar_text_block(&html);
        assert_eq!(
            texts,
            vec![("workflow:wf_only".to_string(), journal.to_string())]
        );
    }

    #[test]
    fn refresh_subagents_backfill_dry_run_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let into = tmp.path().join("sessions");
        let roots = tmp.path().join("projects");
        let sid = "sess-refresh-4";
        let base = wrap_envelope(
            "20260301T090000Z",
            &sanitize_sid(sid),
            &fixture_jsonl(sid),
            &[],
            &[],
            &[],
        );
        write(
            &into,
            &format!("session-20260301T090000Z-{sid}.html"),
            &base,
        );
        write_sidecar(
            &roots,
            "proj-a",
            sid,
            "1",
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
        );

        let summary = refresh_subagents_backfill(&into, &roots, true, None).unwrap();
        assert_eq!(summary.updated, 1, "dry-run still COUNTS what would update");
        let html = std::fs::read_to_string(only_html(&into)).unwrap();
        assert_eq!(html, base, "dry-run must not write");
    }
}
