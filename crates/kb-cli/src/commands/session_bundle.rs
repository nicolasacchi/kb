//! `kb sessions export` / `kb sessions rehydrate` — Session Portability (SP1).
//!
//! Round-trips a captured Claude Code session out of a kb sessions corpus into
//! a portable `<sid>.kbsession.zip` (`kb-session-bundle/1`), and back onto a
//! DIFFERENT machine so `claude -r <id>` resumes it. Both verbs are
//! filesystem-only + offline (no daemon, no sqlite): the raw JSONL lives in the
//! capture `<pre>` on disk, and the manifest is derived deterministically from
//! [`kb_core::sessions::parse_session_activity`].
//!
//! Symmetric with `kb import claude-history`: `import` reads
//! `~/.claude/projects` and writes captures into `KB_SESSIONS_DIR`; `export`
//! reads captures from `KB_SESSIONS_DIR` (the durable kb copy) and writes a
//! bundle; `rehydrate` writes the transcript back under `~/.claude/projects`.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

use kb_core::session_bundle::{
    assemble_bundle, claude_project_slug, earliest_event_ts, first_transcript_field,
    BundleManifest, BundleOrigin, MANIFEST_ENTRY, TRANSCRIPT_ENTRY,
};
use kb_core::session_scrub::{ScrubOptions, ScrubReport};
use kb_core::sessions::recover_jsonl_from_capture;
use kb_core::sessions::HARNESS_DEFAULT;
use kb_core::timeparse::parse_compact_utc;

use super::import::{expand_tilde, meta_session, sanitize_sid};

/// W4/R13/O4 — the harness-aware resume hint: `claude -r`/`codex resume`/
/// `grok -r … --cwd …` (cli-grok.md's exact spelling)/`omp -r …` for a
/// harness kb knows a resume command for, else an honest "not resumable
/// from this machine" (never a fabricated command). `harness` is derived
/// from the transcript's own adapter-meta line — see
/// `first_transcript_field(jsonl, "harness")` at each call site; absent →
/// [`HARNESS_DEFAULT`] ("claude").
pub(crate) fn resume_hint(harness: &str, session_id: &str, cwd: Option<&str>) -> String {
    match harness {
        "claude" => format!("claude -r {session_id}"),
        // `codex resume <uuid>` — the verb exists (`codex --help`); see
        // docs/research/cross-harness-memory-exchange-ideas-2026-07.html.
        "codex" => format!("codex resume {session_id}"),
        "grok" => match cwd {
            Some(c) if !c.is_empty() => format!("grok -r {session_id} --cwd {c}"),
            _ => format!("grok -r {session_id}"),
        },
        // OK1 — verified against `oh-my-pi/docs/cli-reference.md`'s launch
        // flags table: `--resume [id]`, `-r`, `--session [id]` resumes by
        // session id prefix or path. No `--cwd` needed: per
        // `oh-my-pi/docs/session-switching-and-recent-listing.md`,
        // `--resume <value>` searches the current-cwd bucket first, then
        // ALL sessions globally (unless a custom `--session-dir` disables
        // that fallback) — unlike grok's resume, which needs an explicit
        // `--cwd` to find a session recorded under a different directory.
        "omp" => format!("omp -r {session_id}"),
        other => format!(
            "{other} — not resumable from this machine (no known {other} resume command; \
             session id: {session_id})"
        ),
    }
}

/// Detect the harness a transcript came from: the SAME rung-1 rule
/// `parse_session_activity`/`sessions::view::ViewCarry` use (first
/// `adapter-meta` line's `harness` field), falling back honestly to
/// [`HARNESS_DEFAULT`]. `first_transcript_field` already scans every line for
/// a top-level `harness` key — only an adapter-meta line ever carries one.
pub(crate) fn detect_harness(jsonl: &str) -> String {
    first_transcript_field(jsonl, "harness").unwrap_or_else(|| HARNESS_DEFAULT.to_string())
}

// --- export ------------------------------------------------------------------

/// `kb sessions export <id> [--from DIR] [--out FILE] [--scrub …]` — bundle the
/// newest capture of `session_id` into a portable `.kbsession.zip`. `scrub`
/// opts in to the SP2 redaction layers (cross-account); `yes` skips the
/// interactive consent gate (required for non-interactive/`--json` scrubbing).
pub async fn export(
    session_id: &str,
    from: Option<PathBuf>,
    out: Option<PathBuf>,
    scrub: ScrubOptions,
    yes: bool,
    json: bool,
) -> Result<()> {
    let from_dir = resolve_from(from)?;
    let hit = find_newest_capture(&from_dir, session_id)?.ok_or_else(|| {
        anyhow!(
            "no capture for session {session_id} under {} — is it the right \
             sessions corpus (--from / KB_SESSIONS_DIR)?",
            from_dir.display()
        )
    })?;

    let html = std::fs::read_to_string(&hit.path)
        .with_context(|| format!("read capture {}", hit.path.display()))?;
    let jsonl = recover_jsonl_from_capture(&html)
        .ok_or_else(|| anyhow!("capture {} has no <pre> transcript", hit.path.display()))?;

    // Canonical id + start + cc-version via the SHARED kb-core derivation (the
    // daemon route uses the same helpers, so bundles stay byte-compatible). The
    // transcript's own `sessionId` (ground truth) wins over the caller's arg
    // (invariant #11 — the full id must survive for `claude -r` + the link).
    let canonical_id =
        first_transcript_field(&jsonl, "sessionId").unwrap_or_else(|| session_id.to_string());
    let cc_version = first_transcript_field(&jsonl, "version");
    // W4/R13/O4 — the harness this transcript came from (rung-1 adapter-meta
    // read), so the printed resume hint below is honest.
    let harness = detect_harness(&jsonl);
    let cwd_for_hint = first_transcript_field(&jsonl, "cwd");
    // True session start = earliest event ts; the filename stamp (Stop time,
    // ≈ the END) is only the fallback when no event carries a ts.
    let started_at = earliest_event_ts(&jsonl).unwrap_or(hit.started_at);
    // Git provenance from a cwd the session ran in (best-effort — `None` when
    // the repo is gone on this box). SP2 stamps the manifest so rehydrate warns.
    let (git_remote, git_head_sha) = collect_git(cwd_for_hint.as_deref()).await;

    let origin = BundleOrigin {
        kb: None,
        source_relative: Some(hit.filename.clone()),
        exporter_version: Some(env!("CARGO_PKG_VERSION").to_string()),
    };
    // Shared assembly (kb-core) — the SAME path the daemon export route uses, so
    // a CLI bundle and a `kb sessions pull` bundle are byte-compatible. Pure +
    // deterministic: it parses, shapes the manifest, and scrubs.
    let assembled = assemble_bundle(
        &jsonl,
        canonical_id.clone(),
        Some(started_at),
        cc_version,
        origin,
        git_remote,
        git_head_sha,
        &scrub,
    );
    // Consent gate (cross-account): preview + confirm before writing anything.
    if assembled.report.total > 0 {
        if !json {
            print_scrub_preview(&assembled.report, &scrub);
        }
        if !confirm_scrub(yes, json)? {
            bail!("aborted — bundle not written");
        }
    }
    let manifest = assembled.manifest;
    let zip_bytes = build_bundle_zip(&assembled.manifest_json, &assembled.transcript)?;
    let out_path = out
        .map(expand_tilde)
        .unwrap_or_else(|| PathBuf::from(format!("{}.kbsession.zip", sanitize_sid(&canonical_id))));
    std::fs::write(&out_path, &zip_bytes)
        .with_context(|| format!("write bundle {}", out_path.display()))?;

    let resume_command = resume_hint(&harness, &canonical_id, cwd_for_hint.as_deref());
    if json {
        let summary = serde_json::json!({
            "session_id": canonical_id,
            "bundle": out_path.display().to_string(),
            "bytes": zip_bytes.len(),
            "source": hit.path.display().to_string(),
            "cwd": manifest.cwd.clone(),
            "harness": harness,
            "scrubbed": manifest.scrubbed,
            "redactions_applied": manifest.redactions_applied,
            "resume_command": resume_command,
        });
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!("Exported session {canonical_id}");
        println!(
            "  bundle:  {} ({} bytes)",
            out_path.display(),
            zip_bytes.len()
        );
        if let Some(cwd) = &manifest.cwd {
            println!("  cwd:     {cwd}");
        }
        if harness != "claude" {
            println!("  harness: {harness}");
        }
        if manifest.scrubbed {
            println!("  scrubbed: {} redaction(s)", manifest.redactions_applied);
        }
        println!("  source:  {}", hit.path.display());
        println!();
        if harness == "claude" {
            println!("On the target machine (from the project's working dir):");
            println!("  kb sessions rehydrate {}", out_path.display());
        } else {
            println!(
                "This is a {harness} capture — `kb sessions rehydrate` places it under \
                 ~/.claude/projects, which only `claude -r` reads. Resume it directly instead:"
            );
            println!("  {resume_command}");
        }
    }
    Ok(())
}

/// Git provenance for the session's modal cwd, if it's a repo on this box:
/// `(origin remote url, HEAD sha)`. Best-effort — any miss → `None`.
async fn collect_git(cwd: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(cwd) = cwd.filter(|c| !c.is_empty()) else {
        return (None, None);
    };
    let Some(root) = kb_core::vcs::find_git_root(Path::new(cwd)) else {
        return (None, None);
    };
    let remote = kb_core::vcs::git_remote_url(&root, "origin").await;
    let head = kb_core::vcs::git_head(&root).await.map(|(_, sha)| sha);
    (remote, head)
}

/// Print a redaction preview to stderr (kept off stdout so `--json` stays clean
/// and the preview shows even when piping the bundle path).
fn print_scrub_preview(report: &ScrubReport, scrub: &ScrubOptions) {
    let mut layers = Vec::new();
    if scrub.secrets {
        layers.push("secrets");
    }
    if scrub.paths {
        layers.push("paths");
    }
    if scrub.entropy {
        layers.push("entropy");
    }
    eprintln!(
        "Redaction preview — {} redaction(s) across layers [{}]:",
        report.total,
        layers.join(", ")
    );
    for (kind, n) in &report.by_kind {
        eprintln!("  {n:>4}  {kind}");
    }
}

/// The cross-account consent gate. `yes` bypasses it; otherwise an interactive
/// TTY is prompted. Non-interactive or `--json` runs MUST pass `--yes` (they
/// can't prompt, and silently writing secrets would be worse).
fn confirm_scrub(yes: bool, json: bool) -> Result<bool> {
    use std::io::{IsTerminal, Write};
    if yes {
        return Ok(true);
    }
    if json || !std::io::stdin().is_terminal() {
        bail!(
            "a redacted (--scrub) bundle needs confirmation, but this run can't \
             prompt (non-interactive / --json) — review the preview above, then \
             re-run with --yes"
        );
    }
    eprint!("Write this redacted bundle? [y/N] ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "Yes"))
}

/// Resolve the sessions corpus dir to read captures from: `--from` wins, else
/// `KB_SESSIONS_DIR` (the same env `kb-capture.sh` + `kb import --into` use).
fn resolve_from(from: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = from {
        return Ok(expand_tilde(p));
    }
    match std::env::var_os("KB_SESSIONS_DIR") {
        Some(v) if !v.is_empty() => Ok(expand_tilde(PathBuf::from(v))),
        _ => bail!(
            "no sessions corpus: pass --from <DIR> or set KB_SESSIONS_DIR (the \
             dir kb-capture.sh / `kb import claude-history` write captures into)"
        ),
    }
}

/// A capture file that belongs to the requested session.
struct CaptureHit {
    path: PathBuf,
    filename: String,
    /// The 16-char `YYYYMMDDTHHMMSSZ` filename stamp (chronological string key).
    ts: String,
    /// `ts` as unix seconds (the manifest's `started_at`).
    started_at: i64,
}

/// The NEWEST capture of `session_id` in `dir` (invariant #11: a session
/// accrues one capture per Stop; the latest is the transcript superset). Match
/// on the filename sid (the fast path — a UUID sanitises to itself), falling
/// back to the embedded JSONL `sessionId` / `kb-session` meta for legacy
/// captures whose filename was truncated. "Newest" = the greatest filename
/// timestamp.
fn find_newest_capture(dir: &Path, session_id: &str) -> Result<Option<CaptureHit>> {
    if !dir.exists() {
        bail!("sessions dir {} does not exist", dir.display());
    }
    let target_sid = sanitize_sid(session_id);
    let mut best: Option<CaptureHit> = None;
    for entry in walkdir::WalkDir::new(dir)
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
        let Some(fname) = path.file_name().and_then(|x| x.to_str()) else {
            continue;
        };
        let Some((ts, fsid)) = parse_capture_filename(fname) else {
            continue;
        };
        // Fast path: filename sid matches. Only read the file (the expensive
        // part) when it doesn't — to check the embedded id + meta (legacy
        // truncated-filename robustness, mirrors `import::scan_existing_ids`).
        let mut is_match = fsid == target_sid;
        if !is_match {
            if let Ok(html) = std::fs::read_to_string(path) {
                is_match = recover_jsonl_from_capture(&html)
                    .and_then(|j| first_transcript_field(&j, "sessionId"))
                    .map(|s| s == session_id)
                    .unwrap_or(false)
                    || meta_session(&html)
                        .map(|m| m == session_id)
                        .unwrap_or(false);
            }
        }
        if !is_match {
            continue;
        }
        let newer = best.as_ref().map(|b| ts > b.ts).unwrap_or(true);
        if newer {
            best = Some(CaptureHit {
                path: path.to_path_buf(),
                filename: fname.to_string(),
                started_at: parse_compact_utc(&ts).unwrap_or(0),
                ts,
            });
        }
    }
    Ok(best)
}

/// `session-<YYYYMMDDTHHMMSSZ>-<sid>.html` → `(ts, sid)`. Mirrors
/// `import::filename_sid` but keeps the timestamp for newest-selection.
fn parse_capture_filename(fname: &str) -> Option<(String, String)> {
    let stem = fname.strip_suffix(".html")?;
    let rest = stem.strip_prefix("session-")?;
    let (ts, sid) = rest.split_once('-')?;
    if ts.len() == 16 && ts.ends_with('Z') && !sid.is_empty() {
        Some((ts.to_string(), sid.to_string()))
    } else {
        None
    }
}

// --- rehydrate ---------------------------------------------------------------

/// `kb sessions rehydrate <bundle> [--cwd DIR]` — place the bundle's transcript
/// under `~/.claude/projects/<slug>/` so `claude -r <id>` finds it.
pub async fn rehydrate(
    bundle: &Path,
    cwd: Option<PathBuf>,
    dry_run: bool,
    force: bool,
    json: bool,
) -> Result<()> {
    let bundle = expand_tilde(bundle.to_path_buf());
    let bytes =
        std::fs::read(&bundle).with_context(|| format!("read bundle {}", bundle.display()))?;
    let (manifest, jsonl) = read_bundle_zip(&bytes)?;
    let session_id = manifest.session_id.clone();
    // W4/R13/O4 — the harness this bundle's transcript came from (the
    // manifest itself carries no harness field — SP1 predates it — so this
    // reads the SAME rung-1 adapter-meta line the transcript itself carries).
    let jsonl_text = String::from_utf8_lossy(&jsonl);
    let harness = detect_harness(&jsonl_text);

    // Target working dir: absolute + symlink-resolved when it exists, so the
    // derived slug matches what `claude` computes from `process.cwd()`.
    let target = match cwd {
        Some(p) => expand_tilde(p),
        None => std::env::current_dir().context("resolve current working directory")?,
    };
    let target_abs = target.canonicalize().unwrap_or(target);
    let target_str = target_abs
        .to_str()
        .ok_or_else(|| anyhow!("target cwd is not valid UTF-8: {}", target_abs.display()))?;

    let base = claude_projects_base()?;
    let outcome = place_transcript(&base, target_str, &session_id, &jsonl, force, dry_run)?;
    let dest = outcome.dest().to_path_buf();
    let resume_cmd = resume_hint(&harness, &session_id, Some(target_str));

    // Warn-only git-state check (invariant #10 — git state is DETECTED, never
    // ground truth; a different clone legitimately differs, so we never block).
    // Skip on dry-run (nothing was placed).
    let git_warnings = if matches!(outcome, PlaceOutcome::DryRun(_)) {
        Vec::new()
    } else {
        git_state_warnings(&manifest, &target_abs).await
    };

    if json {
        let summary = serde_json::json!({
            "session_id": session_id,
            "action": outcome.action_str(),
            "dest": dest.display().to_string(),
            "target_cwd": target_abs.display().to_string(),
            "slug": claude_project_slug(target_str),
            "harness": harness,
            "scrubbed": manifest.scrubbed,
            "git_warnings": git_warnings,
            "resume_command": resume_cmd,
        });
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }

    match &outcome {
        PlaceOutcome::DryRun(_) => {
            println!("[dry-run] would place transcript at {}", dest.display());
            println!(
                "  then resume with `{resume_cmd}` from {}",
                target_abs.display()
            );
        }
        PlaceOutcome::SkippedExisting(_) => {
            println!(
                "Already present: {} — pass --force to overwrite.",
                dest.display()
            );
            for w in &git_warnings {
                println!("  ⚠ {w}");
            }
            println!("  resume: cd {} && {resume_cmd}", target_abs.display());
        }
        PlaceOutcome::Written(_) => {
            println!("Rehydrated session {session_id}");
            println!("  transcript: {}", dest.display());
            if harness != "claude" {
                println!(
                    "  note: this is a {harness} capture placed under ~/.claude/projects for \
                     reference — `claude -r` won't understand it; use the resume command below."
                );
            }
            if let Some(branch) = &manifest.git_branch {
                println!("  origin branch: {branch}");
            }
            if manifest.scrubbed {
                println!(
                    "  note: this bundle was redacted ({} redactions) — resumed \
                     context may reference masked values.",
                    manifest.redactions_applied
                );
            }
            for w in &git_warnings {
                println!("  ⚠ {w}");
            }
            println!();
            println!("Resume it:");
            println!("  cd {} && {resume_cmd}", target_abs.display());
        }
    }
    Ok(())
}

/// Advisory git-state diffs between the bundle's origin repo and the target cwd
/// (empty when the target isn't a repo, or matches, or git is unavailable).
/// NEVER an error — a mismatch is surfaced, resume proceeds regardless.
async fn git_state_warnings(manifest: &BundleManifest, target_cwd: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Some(root) = kb_core::vcs::find_git_root(target_cwd) else {
        return out;
    };
    if let Some(exp) = &manifest.git_remote {
        if let Some(actual) = kb_core::vcs::git_remote_url(&root, "origin").await {
            if &actual != exp {
                out.push(format!("origin differs — bundle {exp} · here {actual}"));
            }
        }
    }
    if let Some((branch, sha)) = kb_core::vcs::git_head(&root).await {
        if let Some(exp_branch) = &manifest.git_branch {
            if &branch != exp_branch {
                out.push(format!(
                    "branch differs — bundle {exp_branch} · here {branch}"
                ));
            }
        }
        if let Some(exp_sha) = &manifest.git_head_sha {
            // Short-prefix tolerant (either side may be abbreviated).
            if !sha.starts_with(exp_sha) && !exp_sha.starts_with(&sha) {
                out.push(format!(
                    "HEAD differs — bundle {}… · here {}… (resumed context may be stale)",
                    short_sha(exp_sha),
                    short_sha(&sha)
                ));
            }
        }
    }
    out
}

fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(10)]
}

// --- pull (direct daemon transport) ------------------------------------------

/// `kb sessions pull <id> --from <remote>` — download a session bundle from a
/// REMOTE kb daemon over its authenticated API, then (optionally) rehydrate it
/// here. The remote forces the scrub floor on a non-loopback bind (invariant
/// #4), so a public daemon never streams unredacted transcripts.
#[allow(clippy::too_many_arguments)]
pub async fn pull(
    session_id: &str,
    from: &str,
    out: Option<PathBuf>,
    rehydrate_after: bool,
    cwd: Option<PathBuf>,
    force: bool,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let base = from.trim_end_matches('/');
    let client = crate::http::client_with_timeout_and_bearer(300, bearer)?;
    let url = format!(
        "{base}/api/sessions/{}/export",
        crate::http::encode_path_segment(session_id)
    );
    let bytes = fetch_bundle_bytes(&client, &url).await?;

    let out_path = out
        .map(expand_tilde)
        .unwrap_or_else(|| PathBuf::from(format!("{}.kbsession.zip", sanitize_sid(session_id))));
    std::fs::write(&out_path, &bytes)
        .with_context(|| format!("write bundle {}", out_path.display()))?;

    if rehydrate_after {
        // Hand the freshly-downloaded bundle to the same placement path as a
        // local `kb sessions rehydrate`.
        return rehydrate(&out_path, cwd, false, force, json).await;
    }

    if json {
        let summary = serde_json::json!({
            "session_id": session_id,
            "from": base,
            "bundle": out_path.display().to_string(),
            "bytes": bytes.len(),
        });
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!("Pulled session {session_id} from {base}");
        println!("  bundle: {} ({} bytes)", out_path.display(), bytes.len());
        println!();
        println!("Rehydrate it here (from the project's working dir):");
        println!("  kb sessions rehydrate {}", out_path.display());
    }
    Ok(())
}

/// GET a bundle's raw bytes, surfacing the remote's problem+json `detail` on a
/// non-2xx (mirrors `commands::download::fetch_bytes`).
async fn fetch_bundle_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("detail").and_then(|d| d.as_str()).map(str::to_string))
            .unwrap_or_else(|| body.chars().take(300).collect());
        bail!("remote daemon returned {status}: {detail}");
    }
    Ok(resp.bytes().await?.to_vec())
}

/// The base dir Claude Code stores transcripts under: `$CLAUDE_CONFIG_DIR/
/// projects` (when set) else `~/.claude/projects`.
fn claude_projects_base() -> Result<PathBuf> {
    if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(d).join("projects"));
    }
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow!("HOME not set; cannot locate ~/.claude/projects"))?;
    Ok(PathBuf::from(home).join(".claude").join("projects"))
}

/// Outcome of a placement, carrying the resolved destination path.
enum PlaceOutcome {
    Written(PathBuf),
    SkippedExisting(PathBuf),
    DryRun(PathBuf),
}

impl PlaceOutcome {
    fn dest(&self) -> &Path {
        match self {
            PlaceOutcome::Written(p)
            | PlaceOutcome::SkippedExisting(p)
            | PlaceOutcome::DryRun(p) => p,
        }
    }
    fn action_str(&self) -> &'static str {
        match self {
            PlaceOutcome::Written(_) => "written",
            PlaceOutcome::SkippedExisting(_) => "skipped-existing",
            PlaceOutcome::DryRun(_) => "dry-run",
        }
    }
}

/// Pure-ish placement: compute `<base>/<slug(cwd)>/<sid>.jsonl` and write the
/// transcript there. Idempotent (skips an existing file unless `force`);
/// `dry_run` reports the destination without touching the filesystem. `base`
/// is injected so this is unit-testable without env.
fn place_transcript(
    projects_base: &Path,
    target_cwd_abs: &str,
    session_id: &str,
    jsonl: &[u8],
    force: bool,
    dry_run: bool,
) -> Result<PlaceOutcome> {
    let slug = claude_project_slug(target_cwd_abs);
    let dest = projects_base.join(slug).join(format!("{session_id}.jsonl"));
    if dry_run {
        return Ok(PlaceOutcome::DryRun(dest));
    }
    if dest.exists() && !force {
        return Ok(PlaceOutcome::SkippedExisting(dest));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(&dest, jsonl).with_context(|| format!("write {}", dest.display()))?;
    Ok(PlaceOutcome::Written(dest))
}

// --- zip IO ------------------------------------------------------------------

/// Assemble a `.kbsession.zip` in memory: `manifest.json` + `session.jsonl`.
/// `manifest_json` is passed pre-serialised so the caller can redact it (SP2
/// scrub) before it's zipped. Deflate-compressed, mirroring the share route.
fn build_bundle_zip(manifest_json: &str, jsonl: &str) -> Result<Vec<u8>> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    let mut buf = Vec::new();
    {
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        zw.start_file(MANIFEST_ENTRY, opts)?;
        zw.write_all(manifest_json.as_bytes())?;
        zw.start_file(TRANSCRIPT_ENTRY, opts)?;
        zw.write_all(jsonl.as_bytes())?;
        zw.finish()?;
    }
    Ok(buf)
}

/// Read a `.kbsession.zip` back into `(manifest, raw transcript bytes)`.
fn read_bundle_zip(bytes: &[u8]) -> Result<(BundleManifest, Vec<u8>)> {
    use std::io::Read;
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("bundle is not a valid zip")?;
    let manifest: BundleManifest = {
        let mut f = archive
            .by_name(MANIFEST_ENTRY)
            .with_context(|| format!("bundle missing {MANIFEST_ENTRY}"))?;
        let mut s = String::new();
        f.read_to_string(&mut s)?;
        serde_json::from_str(&s).with_context(|| format!("bundle {MANIFEST_ENTRY} is malformed"))?
    };
    let jsonl = {
        let mut f = archive
            .by_name(TRANSCRIPT_ENTRY)
            .with_context(|| format!("bundle missing {TRANSCRIPT_ENTRY}"))?;
        let mut v = Vec::new();
        f.read_to_end(&mut v)?;
        v
    };
    Ok((manifest, jsonl))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kb_core::session_scrub::scrub_transcript;
    use kb_core::sessions::{parse_session_activity, SessionActivity};

    /// OK1 — pins `resume_hint`'s full match table, including the new
    /// `omp` arm (`omp -r <sid>`, verified against `oh-my-pi/docs/
    /// cli-reference.md`'s `--resume [id]`/`-r`/`--session [id]` launch
    /// flag — no `--cwd` needed, unlike grok, since `--resume` searches
    /// globally when the current-cwd bucket doesn't have the session).
    #[test]
    fn resume_hint_covers_every_known_harness_plus_an_honest_fallback() {
        assert_eq!(resume_hint("claude", "sid1", None), "claude -r sid1");
        assert_eq!(resume_hint("codex", "sid1", None), "codex resume sid1");
        assert_eq!(resume_hint("grok", "sid1", None), "grok -r sid1");
        assert_eq!(
            resume_hint("grok", "sid1", Some("/w/x")),
            "grok -r sid1 --cwd /w/x"
        );
        assert_eq!(resume_hint("omp", "sid1", None), "omp -r sid1");
        // `omp` ignores `cwd` (global resume fallback, unlike grok).
        assert_eq!(resume_hint("omp", "sid1", Some("/w/x")), "omp -r sid1");
        let fallback = resume_hint("kimi", "sid1", None);
        assert!(fallback.contains("kimi"), "got: {fallback}");
        assert!(
            fallback.contains("not resumable"),
            "unknown-resume-command harnesses get an honest fallback, got: {fallback}"
        );
    }

    /// Minimal capture envelope with the hook's 3-char escape (`&` first).
    fn capture_html(sid: &str, jsonl: &str) -> String {
        let esc = jsonl
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        format!(
            "<!DOCTYPE html><html><head><meta name=\"kb-session\" content=\"{sid}\"></head>\
             <body><pre>{esc}</pre></body></html>"
        )
    }

    fn sample_manifest(sid: &str) -> BundleManifest {
        let act = SessionActivity {
            session_id: Some(sid.to_string()),
            cwd: Some("/home/u/proj".into()),
            ..Default::default()
        };
        BundleManifest::from_activity(
            sid.to_string(),
            Some(1),
            None,
            BundleOrigin::default(),
            &act,
        )
    }

    #[test]
    fn bundle_zip_round_trips_manifest_and_transcript() {
        let manifest = sample_manifest("abc-123");
        // Transcript with the tricky chars the escape must survive.
        let jsonl = "{\"sessionId\":\"abc-123\"}\n{\"x\":\"a & b < c > d\"}\n";
        let manifest_json = serde_json::to_string_pretty(&manifest).unwrap();
        let bytes = build_bundle_zip(&manifest_json, jsonl).unwrap();
        let (m2, j2) = read_bundle_zip(&bytes).unwrap();
        assert_eq!(m2, manifest);
        assert_eq!(String::from_utf8(j2).unwrap(), jsonl);
    }

    #[test]
    fn scrubbed_manifest_json_still_deserializes() {
        // A secret in the prompt + a username in cwd — after scrubbing the
        // serialized manifest, it must stay valid JSON `read_bundle_zip` parses.
        let act = SessionActivity {
            session_id: Some("s".into()),
            first_user_prompt: Some("use api_key: sk-abcdefghijklmnopqrstuvwx now".into()),
            cwd: Some("/home/user/proj".into()),
            ..Default::default()
        };
        let manifest =
            BundleManifest::from_activity("s".into(), Some(1), None, BundleOrigin::default(), &act);
        let json = serde_json::to_string_pretty(&manifest).unwrap();
        let opts = ScrubOptions {
            secrets: true,
            paths: true,
            entropy: false,
        };
        let scrubbed = scrub_transcript(&json, &opts).0;
        let zip = build_bundle_zip(&scrubbed, "{\"sessionId\":\"s\"}\n").unwrap();
        let (m2, _) = read_bundle_zip(&zip).unwrap();
        assert!(!m2.first_user_prompt.unwrap().contains("sk-abcdef"));
        assert!(m2.cwd.unwrap().contains("[user]"));
    }

    #[test]
    fn read_bundle_zip_rejects_missing_entries() {
        // A zip with only the manifest → error naming the missing transcript.
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        let mut buf = Vec::new();
        {
            let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            zw.start_file(MANIFEST_ENTRY, SimpleFileOptions::default())
                .unwrap();
            let json = serde_json::to_string(&sample_manifest("x")).unwrap();
            zw.write_all(json.as_bytes()).unwrap();
            zw.finish().unwrap();
        }
        let err = read_bundle_zip(&buf).unwrap_err().to_string();
        assert!(err.contains(TRANSCRIPT_ENTRY), "got: {err}");
    }

    #[test]
    fn find_newest_capture_recovers_byte_identical_and_picks_newest() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let sid = "11111111-2222-3333-4444-555555555555";
        let jsonl_old = format!("{{\"sessionId\":\"{sid}\",\"v\":1}}\n");
        // The newer capture is the superset — and carries the tricky chars.
        let jsonl_new = format!(
            "{{\"sessionId\":\"{sid}\",\"cwd\":\"/home/u/p\"}}\n{{\"t\":\"a < b && c > d\"}}\n"
        );
        std::fs::write(
            dir.join(format!("session-20260101T101010Z-{sid}.html")),
            capture_html(sid, &jsonl_old),
        )
        .unwrap();
        std::fs::write(
            dir.join(format!("session-20260202T090000Z-{sid}.html")),
            capture_html(sid, &jsonl_new),
        )
        .unwrap();

        let hit = find_newest_capture(dir, sid).unwrap().expect("a capture");
        assert!(
            hit.filename.contains("20260202T090000Z"),
            "picks the newest by filename ts, got {}",
            hit.filename
        );
        let html = std::fs::read_to_string(&hit.path).unwrap();
        let recovered = recover_jsonl_from_capture(&html).unwrap();
        assert_eq!(recovered, jsonl_new, "byte-identical recovery");
    }

    #[test]
    fn find_newest_capture_matches_truncated_filename_via_embedded_id() {
        // Legacy: filename sid truncated, but the JSONL carries the full id.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let full = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let trunc = "aaaaaaaa-bbbb-cccc-dddd-e"; // 24-char legacy truncation
        let jsonl = format!("{{\"sessionId\":\"{full}\"}}\n");
        std::fs::write(
            dir.join(format!("session-20260101T101010Z-{trunc}.html")),
            capture_html(trunc, &jsonl),
        )
        .unwrap();
        let hit = find_newest_capture(dir, full).unwrap();
        assert!(hit.is_some(), "matched via embedded sessionId");
    }

    #[test]
    fn place_transcript_writes_to_project_slug_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("projects");
        let sid = "abc-123";
        let cwd = "/home/user/project/kb";
        let jsonl = b"{\"sessionId\":\"abc-123\"}\n";

        // dry-run writes nothing.
        let out = place_transcript(&base, cwd, sid, jsonl, false, true).unwrap();
        assert!(matches!(out, PlaceOutcome::DryRun(_)));
        assert!(!base.exists(), "dry-run must not create the base dir");

        // real write lands at <base>/-home-user-project-kb/abc-123.jsonl.
        let out = place_transcript(&base, cwd, sid, jsonl, false, false).unwrap();
        let dest = match out {
            PlaceOutcome::Written(d) => d,
            _ => panic!("expected Written"),
        };
        assert_eq!(
            dest,
            base.join("-home-user-project-kb").join("abc-123.jsonl")
        );
        assert_eq!(std::fs::read(&dest).unwrap(), jsonl);

        // re-run without --force skips (idempotent).
        let out = place_transcript(&base, cwd, sid, jsonl, false, false).unwrap();
        assert!(matches!(out, PlaceOutcome::SkippedExisting(_)));

        // --force overwrites with new bytes.
        let jsonl2 = b"{\"sessionId\":\"abc-123\",\"more\":1}\n";
        let out = place_transcript(&base, cwd, sid, jsonl2, true, false).unwrap();
        assert!(matches!(out, PlaceOutcome::Written(_)));
        assert_eq!(std::fs::read(&dest).unwrap(), jsonl2);
    }

    #[test]
    fn full_round_trip_capture_to_placement() {
        // capture on disk → export bundle in memory → rehydrate placement,
        // asserting the placed transcript is byte-identical to the original.
        let tmp = tempfile::tempdir().unwrap();
        let sid = "77777777-8888-9999-aaaa-bbbbbbbbbbbb";
        let jsonl =
            format!("{{\"sessionId\":\"{sid}\",\"cwd\":\"/w/x\"}}\n{{\"t\":\"x < y & z\"}}\n");
        std::fs::write(
            tmp.path()
                .join(format!("session-20260303T101010Z-{sid}.html")),
            capture_html(sid, &jsonl),
        )
        .unwrap();

        let hit = find_newest_capture(tmp.path(), sid).unwrap().unwrap();
        let html = std::fs::read_to_string(&hit.path).unwrap();
        let recovered = recover_jsonl_from_capture(&html).unwrap();
        let act = parse_session_activity(&recovered);
        let manifest = BundleManifest::from_activity(
            sid.to_string(),
            Some(hit.started_at),
            None,
            BundleOrigin::default(),
            &act,
        );
        let manifest_json = serde_json::to_string_pretty(&manifest).unwrap();
        let zip = build_bundle_zip(&manifest_json, &recovered).unwrap();

        // ... on the "other machine":
        let (m2, j2) = read_bundle_zip(&zip).unwrap();
        assert_eq!(m2.session_id, sid);
        let base = tmp.path().join("dst-projects");
        let out =
            place_transcript(&base, "/some/where/else", &m2.session_id, &j2, false, false).unwrap();
        let dest = match out {
            PlaceOutcome::Written(d) => d,
            _ => panic!("expected Written"),
        };
        assert_eq!(
            dest.file_name().unwrap().to_str().unwrap(),
            format!("{sid}.jsonl")
        );
        assert_eq!(
            String::from_utf8(std::fs::read(&dest).unwrap()).unwrap(),
            jsonl,
            "placed transcript is byte-identical to the original"
        );
    }
}
