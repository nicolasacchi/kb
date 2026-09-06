//! `kb queries --zero-hit [--min-count N] [--scope all]` — GC-B3.
//!
//! Surfaces zero-hit search queries as a corpus-gap signal: the
//! questions this corpus's search couldn't answer, grouped by
//! normalized text with occurrence counts. Reads the daemon's in-memory
//! per-kb `queries` ring — daemon-only, no offline fallback (the ring
//! doesn't exist outside a running process).
//!
//! `--scope one` (default) hits `GET /api/kb/{kb}/queries?zero_hit=true`
//! for a single kb (`--kb` required unless the daemon serves exactly
//! one). `--scope all` fans out server-side across every kb on the
//! daemon via `GET /api/queries/zero-hit` (invariant #28 — the fan-out
//! lives in kb-server, this is a single request).
//!
//! `list`/`save`/`rm` (W3 C-c) are a SEPARATE surface: the daemon-wide
//! `SavedQuery` store at `<state>/saved-queries.json`
//! (`crates/kb-server/src/routes/saved_queries.rs`, v0.13 Q4). That store
//! was callable-but-uncalled from the CLI — the SPA's saved-query ribbon
//! and (as of W3 C-c) the reflection canvas's "save as scene" chip were
//! the only writers. A saved query is nothing but `{name, path, search,
//! saved_at}`; a "scene" is just one saved with `path: "/"` and the
//! canvas's own brush-derived search string — not a new store, and NOT a
//! reading list (a list is an ordered set of artifact *entries* with
//! anchors/read-state; a scene is a re-derivable VIEW, no entries at
//! all).

use crate::http::{client_with_timeout_and_bearer, encode_path_segment, send_json};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";

#[derive(Debug, Deserialize, Serialize, Clone)]
struct ZeroHitGroup {
    query: String,
    count: u64,
    last_seen: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct KbZeroHit {
    kb: String,
    groups: Vec<ZeroHitGroup>,
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    kb: Option<&str>,
    scope: &str,
    zero_hit: bool,
    min_count: u64,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    // The verb only implements the zero-hit report today (a raw
    // recent-queries list already exists via the SPA / `/queries`
    // route directly); require the flag explicitly rather than
    // silently defaulting so a bare `kb queries` doesn't look broken.
    if !zero_hit {
        return Err(anyhow!(
            "kb queries currently only supports the zero-hit corpus-gap report; pass --zero-hit"
        ));
    }
    if !matches!(scope, "one" | "all") {
        return Err(anyhow!(
            "unsupported --scope {scope:?}; expected one of: one, all"
        ));
    }

    let base = daemon.unwrap_or(DEFAULT_DAEMON).trim_end_matches('/');
    let client = client_with_timeout_and_bearer(10, bearer)?;

    if scope == "all" {
        let url = format!("{base}/api/queries/zero-hit?min_count={min_count}");
        let rows: Vec<KbZeroHit> = get_json(&client, &url).await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&rows)?);
        } else {
            print_fleet(&rows);
        }
        return Ok(());
    }

    let kb_name = kb.ok_or_else(|| anyhow!("--kb is required for --scope one"))?;
    let url = format!(
        "{base}/api/kb/{}/queries?zero_hit=true&min_count={min_count}",
        encode_path_segment(kb_name)
    );
    let groups: Vec<ZeroHitGroup> = get_json(&client, &url).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&groups)?);
    } else {
        print_single(kb_name, &groups);
    }
    Ok(())
}

async fn get_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
) -> Result<T> {
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("daemon returned {status}: {body}"));
    }
    resp.json::<T>()
        .await
        .with_context(|| format!("decode JSON from {url}"))
}

fn print_single(kb: &str, groups: &[ZeroHitGroup]) {
    if groups.is_empty() {
        println!("kb {kb}: no zero-hit queries recorded — nothing to surface");
        return;
    }
    println!("kb {kb} — zero-hit queries (corpus-gap signal):");
    print_table(groups);
}

fn print_fleet(rows: &[KbZeroHit]) {
    let total: usize = rows.iter().map(|r| r.groups.len()).sum();
    if total == 0 {
        println!("fleet: no zero-hit queries recorded on any kb");
        return;
    }
    for r in rows {
        if r.groups.is_empty() {
            continue;
        }
        println!("kb {} — zero-hit queries (corpus-gap signal):", r.kb);
        print_table(&r.groups);
        println!();
    }
}

fn print_table(groups: &[ZeroHitGroup]) {
    println!("  {:>5}  {:<28}  last seen", "count", "query");
    for g in groups {
        println!("  {:>5}  {:<28}  {}", g.count, g.query, g.last_seen);
    }
}

// ── saved queries / scenes ──────────────────────────────────────────────
// `/api/saved-queries` — daemon-wide, NOT per-kb, so unlike the zero-hit
// report above there's no `--kb`/`--scope` to resolve.

#[derive(Debug, Deserialize, Serialize, Clone)]
struct SavedQueryRow {
    name: String,
    path: String,
    search: String,
    saved_at: i64,
}

#[derive(Debug, Deserialize)]
struct SavedQueriesListBody {
    queries: Vec<SavedQueryRow>,
}

/// `kb queries list` — GET /api/saved-queries.
pub async fn list_saved(daemon: Option<&str>, bearer: Option<&str>, json: bool) -> Result<()> {
    let base = daemon.unwrap_or(DEFAULT_DAEMON).trim_end_matches('/');
    let client = client_with_timeout_and_bearer(10, bearer)?;
    let body = send_json(
        client.get(format!("{base}/api/saved-queries")),
        "list saved queries",
    )
    .await?;
    let list: SavedQueriesListBody =
        serde_json::from_value(body).context("decode /api/saved-queries response")?;
    if json {
        println!("{}", serde_json::to_string_pretty(&list.queries)?);
        return Ok(());
    }
    if list.queries.is_empty() {
        println!("no saved queries");
        return Ok(());
    }
    println!("  {:<24}  {:<8}  search", "name", "path");
    for q in &list.queries {
        println!("  {:<24}  {:<8}  {}", q.name, q.path, q.search);
    }
    Ok(())
}

/// `kb queries save <name> [--path P] [--search S]` — POST
/// /api/saved-queries, upsert-by-case-insensitive-name (matches the SPA
/// hook's rename-via-overwrite behaviour). A "scene" saved from the
/// reflection canvas is exactly this call with `path="/"` and `search`
/// set to the canvas's brush-derived query string.
pub async fn save(
    name: &str,
    path: &str,
    search: &str,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let base = daemon.unwrap_or(DEFAULT_DAEMON).trim_end_matches('/');
    let client = client_with_timeout_and_bearer(10, bearer)?;
    let body = send_json(
        client
            .post(format!("{base}/api/saved-queries"))
            .json(&serde_json::json!({
                "name": name,
                "path": path,
                "search": search,
            })),
        "save query",
    )
    .await?;
    let list: SavedQueriesListBody =
        serde_json::from_value(body).context("decode /api/saved-queries response")?;
    if json {
        println!("{}", serde_json::to_string_pretty(&list.queries)?);
        return Ok(());
    }
    println!("saved {name:?} → {path}{search}");
    Ok(())
}

/// `kb queries rm <name>` — DELETE /api/saved-queries/{name}. Idempotent
/// on the wire (204 regardless of whether the name existed); mirrored here.
pub async fn rm(name: &str, daemon: Option<&str>, bearer: Option<&str>, json: bool) -> Result<()> {
    let base = daemon.unwrap_or(DEFAULT_DAEMON).trim_end_matches('/');
    let client = client_with_timeout_and_bearer(10, bearer)?;
    send_json(
        client.delete(format!(
            "{base}/api/saved-queries/{}",
            encode_path_segment(name)
        )),
        "delete saved query",
    )
    .await?;
    if json {
        println!("{}", serde_json::json!({ "removed": name }));
    } else {
        println!("removed {name:?} (idempotent — no error if it wasn't there)");
    }
    Ok(())
}
