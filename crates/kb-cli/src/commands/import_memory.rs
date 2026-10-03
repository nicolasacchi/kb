//! `kb import claude-memory` — bring a Claude Code user's native auto-memory
//! (`<dir>/<project>/memory/*.md`, YAML-ish frontmatter + markdown body) into
//! kb's HUMAN-GATED proposal inbox (`kb propose` / `kb proposals approve`).
//!
//! Division of labour (README non-goals: no in-daemon LLM, memory quality is
//! its curation gate): this module is pure, deterministic CLI-side parsing.
//! It NEVER writes a memory — `--apply` only queues candidates through the
//! same `POST /api/kb/{kb}/proposals` route `kb propose` uses, and a human
//! approves them. Dry-run is the default and touches neither the daemon nor
//! the network.
//!
//! Mapping per file: `name` → title · `description` + `metadata.type` +
//! file mtime + project-relative source path → a provenance footer appended
//! to the verbatim body · tags `imported-claude-memory`,
//! `claude-type-<type>`, `cm-<hash12>` (the dedupe key, a hash of the
//! parsed content AND the target corpus, so an edited file re-proposes, an
//! unchanged one never does, and the same text in two projects stays two
//! candidates). The file mtime also rides the proposal as the memory's
//! creation date, so recall decay ages an imported fact by when it was
//! written, not by when it was imported. The footer names the decoded
//! project slug, never the encoded home-path directory. `MEMORY.md` (the
//! index) is skipped.
//!
//! The project directory name (`-home-user-project-my-app`) is a lossy
//! encoding of a path (dashes vs slashes), so the derived slug is a
//! best-effort heuristic: strip the encoded `$HOME` prefix and one common
//! container directory. The dry-run table shows the mapping, and `--link`
//! overrides it for every candidate.

use crate::commands::import::expand_tilde;
use crate::http;
use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const DEFAULT_DIR: &str = "~/.claude/projects";
/// Tag every imported candidate carries.
pub const IMPORT_TAG: &str = "imported-claude-memory";
/// Directory names commonly sitting between `$HOME` and a repo.
const CONTAINERS: &[&str] = &[
    "project",
    "projects",
    "src",
    "code",
    "work",
    "dev",
    "repos",
    "git",
    "workspace",
];

/// One parsed `memory/*.md` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub name: Option<String>,
    pub description: Option<String>,
    pub mem_type: Option<String>,
    pub body: String,
}

/// Why a file was not turned into a candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkipReason {
    IndexFile,
    NoFrontmatter,
    EmptyBody,
    Unreadable,
}

/// A fully mapped proposal candidate.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Candidate {
    /// `<project-dir>/memory/<file>` — relative, never absolute.
    pub source: String,
    pub title: String,
    pub description: Option<String>,
    pub mem_type: Option<String>,
    pub tags: Vec<String>,
    /// `memory-<slug>` (derived or `--link`), or `null` = global.
    pub target: Option<String>,
    /// The `cm-<hash12>` tag.
    pub dedupe_key: String,
    /// The pre-X8 `cm-<hash12>` key (no target folded in). Read-only
    /// transition key: a queue/corpus entry tagged with it counts as a
    /// duplicate, so a re-run after upgrade does not re-queue facts that were
    /// already imported or approved. Never written to a tag, never serialised.
    #[serde(skip)]
    pub legacy_key: String,
    pub mtime_unix: Option<i64>,
    /// The full proposal body (verbatim body + provenance footer).
    pub body: String,
    /// `new` | `duplicate` (already in the queue / corpus) | `unknown`
    /// (dry-run with no reachable daemon) | `over-limit` (past `--limit`).
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Skipped {
    pub source: String,
    pub reason: SkipReason,
}

#[derive(Debug, Serialize)]
pub struct Plan {
    pub candidates: Vec<Candidate>,
    pub skipped: Vec<Skipped>,
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    if v.len() >= 2 {
        let b = v.as_bytes();
        if (b[0] == b'"' && b[v.len() - 1] == b'"') || (b[0] == b'\'' && b[v.len() - 1] == b'\'') {
            return v[1..v.len() - 1].to_string();
        }
    }
    v.to_string()
}

/// Parse `---\n<frontmatter>\n---\n<body>`. `None` when there is no
/// frontmatter block. Understands top-level scalar `key: value` lines and
/// one nested `metadata:` block (`type` is read from there, falling back to
/// a top-level `type:`). Anything fancier is ignored, never guessed.
pub fn parse_frontmatter(src: &str) -> Option<Parsed> {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let rest = src
        .strip_prefix("---\r\n")
        .or_else(|| src.strip_prefix("---\n"))?;
    let mut fm = String::new();
    let mut body_start = None;
    let mut off = 0usize;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            body_start = Some(off + line.len());
            break;
        }
        fm.push_str(line);
        off += line.len();
    }
    let body = rest[body_start?..].trim().to_string();

    let (mut name, mut description, mut top_type, mut meta_type) = (None, None, None, None);
    let mut in_meta = false;
    for line in fm.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        let Some((k, v)) = line.trim().split_once(':') else {
            continue;
        };
        let (k, v) = (k.trim(), unquote(v));
        if indented {
            if in_meta && k == "type" && !v.is_empty() {
                meta_type = Some(v);
            }
            continue;
        }
        in_meta = k == "metadata";
        if v.is_empty() {
            continue;
        }
        match k {
            "name" => name = Some(v),
            "description" => description = Some(v),
            "type" => top_type = Some(v),
            _ => {}
        }
    }
    Some(Parsed {
        name,
        description,
        mem_type: meta_type.or(top_type),
        body,
    })
}

/// Tag-safe slug: lowercase ascii alphanumerics joined by single dashes.
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Claude Code's project-dir encoding of a path: every non-alphanumeric
/// char becomes `-`.
pub fn encode_path(p: &str) -> String {
    p.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Best-effort project slug from an encoded project dir name. Lossy by
/// nature (see module docs); the dry-run table exists so a human can see it.
pub fn project_slug(dirname: &str, home: Option<&str>) -> Option<String> {
    let mut rest = dirname.trim_start_matches('-').to_string();
    if let Some(h) = home {
        let enc = encode_path(h);
        let enc = enc.trim_start_matches('-');
        if rest == enc {
            return None;
        }
        if !enc.is_empty() {
            if let Some(r) = rest.strip_prefix(&format!("{enc}-")) {
                rest = r.to_string();
                // one common container directory between $HOME and the repo
                if let Some((first, tail)) = rest.split_once('-') {
                    if CONTAINERS.contains(&first.to_ascii_lowercase().as_str()) {
                        rest = tail.to_string();
                    }
                }
            } else {
                rest = rest.rsplit('-').next().unwrap_or("").to_string();
            }
        }
    } else {
        rest = rest.rsplit('-').next().unwrap_or("").to_string();
    }
    let s = slugify(&rest);
    (!s.is_empty()).then_some(s)
}

fn content_hash(p: &Parsed, title: &str, target: Option<&str>) -> String {
    hash_parts(&[
        title,
        target.unwrap_or(""),
        p.description.as_deref().unwrap_or(""),
        p.mem_type.as_deref().unwrap_or(""),
        p.body.as_str(),
    ])
}

/// The pre-X8 hash shape: the same parts WITHOUT the target corpus.
fn legacy_content_hash(p: &Parsed, title: &str) -> String {
    hash_parts(&[
        title,
        p.description.as_deref().unwrap_or(""),
        p.mem_type.as_deref().unwrap_or(""),
        p.body.as_str(),
    ])
}

fn hash_parts(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for part in parts {
        h.update((part.len() as u64).to_le_bytes());
        h.update(part.as_bytes());
    }
    hex::encode(h.finalize())[..12].to_string()
}

fn fmt_date(mtime: Option<i64>) -> String {
    use chrono::{TimeZone, Utc};
    mtime
        .and_then(|s| Utc.timestamp_opt(s, 0).single())
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Map one parsed file to a candidate. Pure.
pub fn map_candidate(
    source: &str,
    project: &str,
    file_stem: &str,
    p: &Parsed,
    mtime_unix: Option<i64>,
    target: Option<String>,
) -> Candidate {
    let title = p
        .name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| file_stem.to_string());
    let key = format!("cm-{}", content_hash(p, &title, target.as_deref()));
    let mut tags = vec![IMPORT_TAG.to_string()];
    if let Some(t) = p.mem_type.as_deref().map(slugify).filter(|t| !t.is_empty()) {
        tags.push(format!("claude-type-{t}"));
    }
    tags.push(key.clone());

    let mut body = p.body.clone();
    body.push_str("\n\n---\n");
    if let Some(d) = &p.description {
        body.push_str(&format!("_{d}_\n\n"));
    }
    // The decoded project slug + file name only: `source` starts with the
    // ENCODED project dir (`-home-user-project-x`), a mangled absolute path
    // that must not be stored in a memory.
    let fname = source.rsplit('/').next().unwrap_or(source);
    body.push_str(&format!(
        "Imported from Claude Code auto-memory `{project}/memory/{fname}` (file modified {}); \
         an older note may no longer be true — review before approving.\n",
        fmt_date(mtime_unix)
    ));
    Candidate {
        source: source.to_string(),
        title,
        description: p.description.clone(),
        mem_type: p.mem_type.clone(),
        tags,
        target,
        dedupe_key: key,
        legacy_key: format!("cm-{}", legacy_content_hash(p, &title)),
        mtime_unix,
        body,
        status: "new".to_string(),
    }
}

/// Walk `<root>/<project>/memory/*.md` (sorted, deterministic) into a plan.
pub fn build_plan(root: &Path, home: Option<&str>, link: Option<&str>) -> Result<Plan> {
    let mut projects: Vec<PathBuf> = std::fs::read_dir(root)
        .with_context(|| format!("read {}", root.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    projects.sort();
    let (mut candidates, mut skipped) = (Vec::new(), Vec::new());
    for proj in projects {
        let dirname = proj.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let mem = proj.join("memory");
        let Ok(rd) = std::fs::read_dir(&mem) else {
            continue;
        };
        let mut files: Vec<PathBuf> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("md"))
            .collect();
        files.sort();
        let target = match link {
            Some(l) => Some(l.to_string()),
            None => project_slug(dirname, home).map(|s| format!("memory-{s}")),
        };
        for f in files {
            let fname = f.file_name().and_then(|s| s.to_str()).unwrap_or("");
            let source = format!("{dirname}/memory/{fname}");
            if fname.eq_ignore_ascii_case("MEMORY.md") {
                skipped.push(Skipped {
                    source,
                    reason: SkipReason::IndexFile,
                });
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&f) else {
                skipped.push(Skipped {
                    source,
                    reason: SkipReason::Unreadable,
                });
                continue;
            };
            let Some(parsed) = parse_frontmatter(&text) else {
                skipped.push(Skipped {
                    source,
                    reason: SkipReason::NoFrontmatter,
                });
                continue;
            };
            if parsed.body.trim().is_empty() {
                skipped.push(Skipped {
                    source,
                    reason: SkipReason::EmptyBody,
                });
                continue;
            }
            let mtime = std::fs::metadata(&f)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64);
            let stem = f.file_stem().and_then(|s| s.to_str()).unwrap_or("memory");
            let project = project_slug(dirname, home).unwrap_or_else(|| "home".to_string());
            candidates.push(map_candidate(
                &source,
                &project,
                stem,
                &parsed,
                mtime,
                target.clone(),
            ));
        }
    }
    Ok(Plan {
        candidates,
        skipped,
    })
}

/// Mark candidates whose dedupe key is already known (queue or corpus) as
/// `duplicate`; also collapses two identical files in one run. Pure.
pub fn mark_duplicates(plan: &mut Plan, known: &BTreeSet<String>) {
    let mut seen = known.clone();
    for c in &mut plan.candidates {
        // Transition read: an entry imported before X8 carries the old key
        // shape (no target folded in); it still marks the fact as known.
        let legacy_known = known.contains(&c.legacy_key);
        if !seen.insert(c.dedupe_key.clone()) || legacy_known {
            c.status = "duplicate".to_string();
        }
    }
}

/// Relabel every non-duplicate candidate past the first `limit` as
/// `over-limit`, so the table (dry-run AND apply) shows exactly what `--apply`
/// would queue. Pure.
pub fn apply_limit(plan: &mut Plan, limit: Option<usize>) {
    let Some(limit) = limit else {
        return;
    };
    let mut kept = 0usize;
    for c in &mut plan.candidates {
        if c.status == "new" || c.status == "unknown" {
            if kept < limit {
                kept += 1;
            } else {
                c.status = "over-limit".to_string();
            }
        }
    }
}

/// Candidates `--apply` will submit: `new` only, capped by `limit`.
pub fn select_to_apply(plan: &Plan, limit: Option<usize>) -> Vec<&Candidate> {
    plan.candidates
        .iter()
        .filter(|c| c.status == "new")
        .take(limit.unwrap_or(usize::MAX))
        .collect()
}

/// Known `cm-*` keys from `GET /api/proposals` (the queue; hard-fails) and
/// `GET /api/kb/{kb}/docs?tags=…` (already-approved memories; best-effort).
async fn known_keys(
    url: &str,
    kb: &str,
    keys: &[String],
    bearer: Option<&str>,
) -> Result<BTreeSet<String>> {
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let mut known = BTreeSet::new();
    let q: serde_json::Value = client
        .get(format!("{url}/api/proposals"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    queue_keys(&q, &mut known)?;
    if !keys.is_empty() {
        let resp = client
            .get(format!(
                "{url}/api/kb/{}/docs",
                http::encode_path_segment(kb)
            ))
            .query(&[
                ("tags", keys.join(",")),
                ("envelope", "1".to_string()),
                ("limit", "500".to_string()),
            ])
            .send()
            .await;
        match resp {
            Ok(r) if r.status().is_success() => {
                if let Ok(v) = r.json::<serde_json::Value>().await {
                    collect_keys(v["items"].as_array(), &mut known);
                }
            }
            _ => eprintln!("warn: could not check {kb} for already-approved imports"),
        }
    }
    Ok(known)
}

/// Dry-run duplicate detection: READ-ONLY (two GETs, never a write). `None`
/// when the daemon is unreachable, no corpus can be resolved, or the queue is
/// only partially visible — the caller then reports `unknown`, not `new`.
async fn dry_run_known(
    daemon: Option<&str>,
    kb: Option<&str>,
    keys: &[String],
    bearer: Option<&str>,
) -> Option<BTreeSet<String>> {
    let url = http::detect_daemon(daemon, bearer).await?;
    let kb = match http::resolve_default_kb(kb, Some(&url), bearer).await {
        Ok(k) => k,
        Err(e) => {
            eprintln!("note: duplicate check skipped ({e}); pass --kb to enable it");
            return None;
        }
    };
    match known_keys(&url, &kb, keys, bearer).await {
        Ok(k) => Some(k),
        Err(e) => {
            eprintln!("note: duplicate check skipped ({e})");
            None
        }
    }
}

/// Fold the `cm-*` keys of a `GET /api/proposals` response into `into`.
/// The route truncates to a fleet-wide cap and reports the pre-truncation
/// `total`; when `total` exceeds the items returned the queue is only
/// partially visible, so dedupe cannot be trusted and `--apply` refuses
/// rather than risk re-queueing already-proposed memories.
fn queue_keys(resp: &serde_json::Value, into: &mut BTreeSet<String>) -> Result<()> {
    let items = resp["items"].as_array();
    let shown = items.map_or(0, |a| a.len() as u64);
    if let Some(total) = resp["total"].as_u64().filter(|t| *t > shown) {
        return Err(anyhow!(
            "the proposal queue holds {total} items but the daemon listed only {shown}; \
             duplicate detection would be blind to the rest — triage the queue \
             (`kb proposals`) below the cap and re-run --apply"
        ));
    }
    collect_keys(items, into);
    Ok(())
}

/// Resolve the target corpus list for one candidate. An EXPLICIT `--link`
/// (comma-separated) must name only configured kbs — anything else is an
/// error, never a silent downgrade to global scope (global memories are
/// visible to every project). A DERIVED guess falls back to global when its
/// corpus is not configured; the `bool` flags that fallback so the caller can
/// warn.
pub fn resolve_link(
    target: Option<&str>,
    explicit: bool,
    kbs: &BTreeSet<String>,
) -> Result<(Option<String>, bool)> {
    let Some(t) = target else {
        return Ok((None, false));
    };
    let parts: Vec<&str> = t
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if explicit {
        if parts.is_empty() {
            return Err(anyhow!("--link is empty"));
        }
        if let Some(bad) = parts.iter().find(|p| !kbs.contains(**p)) {
            return Err(anyhow!(
                "--link names unconfigured kb `{bad}` (configured: {})",
                kbs.iter().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
        return Ok((Some(parts.join(",")), false));
    }
    if !parts.is_empty() && parts.iter().all(|p| kbs.contains(*p)) {
        Ok((Some(parts.join(",")), false))
    } else {
        Ok((None, true))
    }
}

fn collect_keys(items: Option<&Vec<serde_json::Value>>, into: &mut BTreeSet<String>) {
    for it in items.into_iter().flatten() {
        for t in it["tags"].as_array().into_iter().flatten() {
            if let Some(t) = t.as_str().filter(|t| t.starts_with("cm-")) {
                into.insert(t.to_string());
            }
        }
    }
}

async fn configured_kbs(url: &str, bearer: Option<&str>) -> Result<BTreeSet<String>> {
    let client = http::client_with_timeout_and_bearer(5, bearer)?;
    let v: serde_json::Value = client
        .get(format!("{url}/api/kbs"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|k| k["name"].as_str().map(str::to_string))
        .collect())
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    dir: Option<PathBuf>,
    limit: Option<u32>,
    apply: bool,
    link: Option<String>,
    kb: Option<String>,
    daemon: Option<String>,
    bearer: Option<String>,
    json: bool,
) -> Result<()> {
    let root = expand_tilde(dir.unwrap_or_else(|| PathBuf::from(DEFAULT_DIR)));
    let home = std::env::var("HOME").ok();
    let mut plan = build_plan(&root, home.as_deref(), link.as_deref())?;
    let mut submitted: Vec<serde_json::Value> = Vec::new();
    let mut global_fallbacks = 0usize;

    if !apply {
        let keys: Vec<String> = plan
            .candidates
            .iter()
            .map(|c| c.dedupe_key.clone())
            .collect();
        match dry_run_known(daemon.as_deref(), kb.as_deref(), &keys, bearer.as_deref()).await {
            Some(known) => mark_duplicates(&mut plan, &known),
            None => {
                for c in &mut plan.candidates {
                    c.status = "unknown".to_string();
                }
            }
        }
        apply_limit(&mut plan, limit.map(|n| n as usize));
    }

    if apply {
        let url = http::detect_daemon(daemon.as_deref(), bearer.as_deref())
            .await
            .ok_or_else(|| anyhow!("daemon not reachable — start it with `kb daemon`"))?;
        let kb = http::resolve_default_kb(kb.as_deref(), Some(&url), bearer.as_deref()).await?;
        let kbs = configured_kbs(&url, bearer.as_deref()).await?;
        let keys: Vec<String> = plan
            .candidates
            .iter()
            .map(|c| c.dedupe_key.clone())
            .collect();
        let known = known_keys(&url, &kb, &keys, bearer.as_deref()).await?;
        mark_duplicates(&mut plan, &known);
        apply_limit(&mut plan, limit.map(|n| n as usize));
        // Resolve every target BEFORE queueing anything so a bad explicit
        // --link fails the whole run instead of half-applying.
        let explicit = link.is_some();
        let mut resolved = Vec::new();
        for c in select_to_apply(&plan, limit.map(|n| n as usize)) {
            let (l, fell_back) = resolve_link(c.target.as_deref(), explicit, &kbs)?;
            if fell_back {
                eprintln!(
                    "warn: {} -> corpus {} is not configured; queued as GLOBAL \
                     (visible to every project)",
                    c.source,
                    c.target.as_deref().unwrap_or("?")
                );
                global_fallbacks += 1;
            }
            resolved.push((c, l));
        }
        for (c, link) in resolved {
            let tags = c.tags.join(",");
            let out = crate::commands::proposals::propose_inner(
                &c.title,
                &c.body,
                Some(&kb),
                Some(&tags),
                link.is_none(),
                link.as_deref(),
                None,
                None,
                c.mtime_unix,
                Some(&url),
                bearer.as_deref(),
            )
            .await
            .with_context(|| format!("propose {}", c.source))?;
            submitted.push(serde_json::json!({
                "source": c.source,
                "proposal_id": out["id"],
                "linked": link,
            }));
        }
    }

    if json {
        let mut v = serde_json::to_value(&plan)?;
        v["dry_run"] = serde_json::json!(!apply);
        v["dir"] = serde_json::json!(root.display().to_string());
        v["submitted"] = serde_json::json!(submitted);
        v["global_fallbacks"] = serde_json::json!(global_fallbacks);
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }

    println!(
        "{:<8} {:<34} {:<10} {:<22} KEY",
        "STATUS", "TITLE", "TYPE", "TARGET"
    );
    for c in &plan.candidates {
        println!(
            "{:<8} {:<34} {:<10} {:<22} {}",
            c.status,
            c.title.chars().take(34).collect::<String>(),
            c.mem_type.as_deref().unwrap_or("-"),
            c.target.as_deref().unwrap_or("(global)"),
            c.dedupe_key
        );
    }
    for s in &plan.skipped {
        println!("skip     {}  ({:?})", s.source, s.reason);
    }
    if apply {
        println!(
            "queued {} proposal(s) ({} as GLOBAL scope via fallback); review with `kb proposals`",
            submitted.len(),
            global_fallbacks
        );
    } else {
        println!(
            "dry-run: {} candidate(s), nothing written. Re-run with --apply to queue them \
             in the proposal inbox (a human approves with `kb proposals approve`). \
             Targets are a best-effort guess from the lossy project dir name; \
             --link overrides (comma-separated, must be configured), and an unconfigured derived corpus falls back to global at --apply. \
             Status: `duplicate` = already queued/approved (checked read-only against a reachable daemon), \
             `unknown` = no daemon to ask, `over-limit` = past --limit.",
            plan.candidates.len()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---\nname: Prefer fast profile\ndescription: \"use --profile fast\"\nmetadata:\n  type: feedback\n---\n\nBody line one.\n\nLine two.\n";

    #[test]
    fn frontmatter_parses_name_description_nested_type() {
        let p = parse_frontmatter(SAMPLE).unwrap();
        assert_eq!(p.name.as_deref(), Some("Prefer fast profile"));
        assert_eq!(p.description.as_deref(), Some("use --profile fast"));
        assert_eq!(p.mem_type.as_deref(), Some("feedback"));
        assert_eq!(p.body, "Body line one.\n\nLine two.");
    }

    #[test]
    fn no_frontmatter_is_none_and_top_level_type_is_fallback() {
        assert!(parse_frontmatter("just text").is_none());
        assert!(parse_frontmatter("---\nname: x\nno close").is_none());
        let p = parse_frontmatter("---\nname: x\ntype: project\n---\nb").unwrap();
        assert_eq!(p.mem_type.as_deref(), Some("project"));
    }

    #[test]
    fn slug_strips_home_and_container() {
        let h = Some("/home/alice");
        assert_eq!(
            project_slug("-home-alice-project-kb", h).as_deref(),
            Some("kb")
        );
        assert_eq!(
            project_slug("-home-alice-project-my-app", h).as_deref(),
            Some("my-app")
        );
        assert_eq!(
            project_slug("-home-alice-notes", h).as_deref(),
            Some("notes")
        );
        // foreign prefix / no home: last segment only
        assert_eq!(project_slug("-srv-x-thing", h).as_deref(), Some("thing"));
        assert_eq!(project_slug("-home-alice", h), None);
    }

    #[test]
    fn candidate_mapping_is_golden() {
        let p = parse_frontmatter(SAMPLE).unwrap();
        let c = map_candidate(
            "-home-alice-project-kb/memory/fast.md",
            "kb",
            "fast",
            &p,
            Some(1_700_000_000),
            Some("memory-kb".into()),
        );
        assert_eq!(c.title, "Prefer fast profile");
        assert_eq!(c.tags[0], "imported-claude-memory");
        assert_eq!(c.tags[1], "claude-type-feedback");
        assert!(c.tags[2].starts_with("cm-") && c.tags[2].len() == 15);
        assert_eq!(c.tags[2], c.dedupe_key);
        assert!(c
            .body
            .starts_with("Body line one.\n\nLine two.\n\n---\n_use --profile fast_"));
        assert!(c.body.contains("file modified 2023-11-14"));
        // The footer names the decoded project slug, never the encoded
        // home-path directory.
        assert!(c.body.contains("`kb/memory/fast.md`"), "{}", c.body);
        assert!(!c.body.contains("-home-alice"), "{}", c.body);
        assert_eq!(c.mtime_unix, Some(1_700_000_000));
        // same content → same key; edited body → different key
        let again = map_candidate(
            "x/memory/y.md",
            "kb",
            "fast",
            &p,
            None,
            Some("memory-kb".into()),
        );
        assert_eq!(again.dedupe_key, c.dedupe_key);
        let mut edited = p.clone();
        edited.body.push_str(" more");
        assert_ne!(
            map_candidate("x", "kb", "fast", &edited, None, Some("memory-kb".into())).dedupe_key,
            c.dedupe_key
        );
    }

    #[test]
    fn missing_name_falls_back_to_file_stem() {
        let p = parse_frontmatter("---\ndescription: d\n---\nbody").unwrap();
        assert_eq!(
            map_candidate("s", "p", "my_note", &p, None, None).title,
            "my_note"
        );
    }

    #[test]
    fn plan_walks_skips_index_and_maps_projects() {
        let tmp = tempfile::tempdir().unwrap();
        let mem = tmp.path().join("-home-alice-project-kb/memory");
        std::fs::create_dir_all(&mem).unwrap();
        std::fs::write(mem.join("MEMORY.md"), "- [a](a.md)").unwrap();
        std::fs::write(mem.join("a.md"), SAMPLE).unwrap();
        std::fs::write(mem.join("plain.md"), "no frontmatter").unwrap();
        std::fs::write(mem.join("empty.md"), "---\nname: e\n---\n").unwrap();
        std::fs::write(mem.join("notes.txt"), "ignored").unwrap();
        // a project without a memory dir is ignored
        std::fs::create_dir_all(tmp.path().join("-home-alice-other")).unwrap();

        let plan = build_plan(tmp.path(), Some("/home/alice"), None).unwrap();
        assert_eq!(plan.candidates.len(), 1);
        assert_eq!(plan.candidates[0].target.as_deref(), Some("memory-kb"));
        assert_eq!(
            plan.candidates[0].source,
            "-home-alice-project-kb/memory/a.md"
        );
        let reasons: Vec<_> = plan.skipped.iter().map(|s| s.reason.clone()).collect();
        assert!(reasons.contains(&SkipReason::IndexFile));
        assert!(reasons.contains(&SkipReason::NoFrontmatter));
        assert!(reasons.contains(&SkipReason::EmptyBody));

        let linked = build_plan(tmp.path(), Some("/home/alice"), Some("memory-x")).unwrap();
        assert_eq!(linked.candidates[0].target.as_deref(), Some("memory-x"));
    }

    #[test]
    fn duplicates_are_marked_and_not_applied_and_limit_caps() {
        let mk = |name: &str, body: &str| {
            let p = Parsed {
                name: Some(name.into()),
                description: None,
                mem_type: None,
                body: body.into(),
            };
            map_candidate(name, "p", name, &p, None, None)
        };
        let known_one = mk("a", "1");
        let mut plan = Plan {
            candidates: vec![mk("a", "1"), mk("b", "2"), mk("b", "2"), mk("c", "3")],
            skipped: vec![],
        };
        let known: BTreeSet<String> = [known_one.dedupe_key].into();
        mark_duplicates(&mut plan, &known);
        let st: Vec<_> = plan.candidates.iter().map(|c| c.status.as_str()).collect();
        assert_eq!(st, ["duplicate", "new", "duplicate", "new"]);
        assert_eq!(select_to_apply(&plan, None).len(), 2);
        let capped = select_to_apply(&plan, Some(1));
        assert_eq!(capped.len(), 1);
        assert_eq!(capped[0].title, "b");
    }

    #[test]
    fn collect_keys_reads_only_cm_tags() {
        let v = serde_json::json!([{"tags":["x","cm-abc"]},{"tags":["cm-def"]}]);
        let mut s = BTreeSet::new();
        collect_keys(v.as_array(), &mut s);
        assert_eq!(s.into_iter().collect::<Vec<_>>(), ["cm-abc", "cm-def"]);
    }

    fn kbset(v: &[&str]) -> BTreeSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn explicit_multi_link_survives_and_unknown_errors() {
        let kbs = kbset(&["memory-a", "memory-b", "memory-kb"]);
        let (l, fb) = resolve_link(Some("memory-a, memory-b"), true, &kbs).unwrap();
        assert_eq!(l.as_deref(), Some("memory-a,memory-b"));
        assert!(!fb);
        assert!(resolve_link(Some("memory-a,nope"), true, &kbs).is_err());
        assert!(resolve_link(Some("nope"), true, &kbs).is_err());
    }

    #[test]
    fn derived_unconfigured_target_falls_back_to_global_flagged() {
        let kbs = kbset(&["memory-kb"]);
        assert_eq!(
            resolve_link(Some("memory-kb"), false, &kbs).unwrap(),
            (Some("memory-kb".to_string()), false)
        );
        assert_eq!(
            resolve_link(Some("memory-zzz"), false, &kbs).unwrap(),
            (None, true)
        );
        assert_eq!(resolve_link(None, false, &kbs).unwrap(), (None, false));
    }

    #[test]
    fn queue_keys_refuses_a_truncated_queue() {
        let mut s = BTreeSet::new();
        let ok = serde_json::json!({"items":[{"tags":["cm-a"]}],"total":1});
        queue_keys(&ok, &mut s).unwrap();
        assert!(s.contains("cm-a"));
        let capped = serde_json::json!({"items":[{"tags":["cm-a"]}],"total":201});
        let e = queue_keys(&capped, &mut BTreeSet::new()).unwrap_err();
        assert!(e.to_string().contains("201"));
    }

    #[test]
    fn the_target_corpus_is_part_of_the_dedupe_key() {
        let p = parse_frontmatter(SAMPLE).unwrap();
        let key = |t: Option<&str>| {
            map_candidate("a/memory/f.md", "kb", "f", &p, None, t.map(str::to_string)).dedupe_key
        };
        assert_ne!(key(Some("memory-a")), key(Some("memory-b")));
        assert_ne!(key(Some("memory-a")), key(None));
        assert_eq!(key(Some("memory-a")), key(Some("memory-a")));
        // Same text in two projects is two candidates, not a duplicate.
        let mut plan = Plan {
            candidates: vec![
                map_candidate("a/memory/f.md", "a", "f", &p, None, Some("memory-a".into())),
                map_candidate("b/memory/f.md", "b", "f", &p, None, Some("memory-b".into())),
            ],
            skipped: vec![],
        };
        mark_duplicates(&mut plan, &BTreeSet::new());
        assert!(plan.candidates.iter().all(|c| c.status == "new"));
    }

    #[test]
    fn a_pre_x8_key_still_marks_the_fact_as_known() {
        let p = parse_frontmatter(SAMPLE).unwrap();
        let c = map_candidate("a/memory/f.md", "a", "f", &p, None, Some("memory-a".into()));
        assert_ne!(c.legacy_key, c.dedupe_key);
        // Re-run after upgrade: the queue/corpus only holds the OLD key.
        let mut plan = Plan {
            candidates: vec![c.clone()],
            skipped: vec![],
        };
        mark_duplicates(&mut plan, &kbset(&[c.legacy_key.as_str()]));
        assert_eq!(plan.candidates[0].status, "duplicate");
        // An unrelated key does not.
        let mut plan = Plan {
            candidates: vec![c],
            skipped: vec![],
        };
        mark_duplicates(&mut plan, &kbset(&["cm-000000000000"]));
        assert_eq!(plan.candidates[0].status, "new");
    }

    #[test]
    fn limit_is_reflected_in_the_plan_statuses() {
        let p = parse_frontmatter(SAMPLE).unwrap();
        let mk = |n: &str| map_candidate(n, "p", n, &p, None, Some(format!("memory-{n}")));
        let mut plan = Plan {
            candidates: vec![mk("a"), mk("b"), mk("c")],
            skipped: vec![],
        };
        plan.candidates[0].status = "duplicate".into();
        apply_limit(&mut plan, Some(1));
        let st: Vec<_> = plan.candidates.iter().map(|c| c.status.as_str()).collect();
        // the duplicate does not consume the budget; the 2nd new is over it
        assert_eq!(st, ["duplicate", "new", "over-limit"]);
        assert_eq!(select_to_apply(&plan, Some(1)).len(), 1);
        let mut unlimited = Plan {
            candidates: vec![mk("a")],
            skipped: vec![],
        };
        apply_limit(&mut unlimited, None);
        assert_eq!(unlimited.candidates[0].status, "new");
    }
}
