//! `kb search <q>` — auto-detect: try the daemon first, fall back to a
//! lance read-only open if the daemon isn't reachable. `--offline` forces
//! lance, `--daemon URL` forces HTTP.

use super::{load_config_or_default, resolve_config_path};
use crate::http;
use anyhow::{anyhow, Result};
use kb_core::paths::KbPaths;
use kb_core::storage::lance::Storage;
use kb_core::types::KbName;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Mirrors `routes/search.rs`'s `FILTERED_POOL` deepen: the daemon
/// fetches `limit.max(FILTERED_POOL)` whenever a filter will thin the
/// pool before truncation, so `--offline --category` must fetch the
/// same depth or it silently under-returns relative to the online path
/// it substitutes for (a narrow category's hits may rank far down the
/// raw BM25 pool).
const OFFLINE_FILTER_POOL: u32 = 400;

#[derive(Debug, Deserialize)]
struct SearchResp {
    hits: Vec<Hit>,
    ms: u64,
}

#[derive(Debug, Deserialize, Serialize)]
struct Hit {
    id: String,
    title: String,
    path: String,
    #[serde(default)]
    kb_category: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    config_path: Option<&PathBuf>,
    q: &str,
    kb: Option<&str>,
    mode: &str,
    limit: u32,
    category: Option<&str>,
    offline: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
    read_from: Option<&str>,
    read_to: Option<&str>,
) -> Result<()> {
    let result = run_inner(
        config_path,
        q,
        kb,
        mode,
        limit,
        category,
        offline,
        daemon,
        bearer,
        json,
        read_from,
        read_to,
    )
    .await;
    // When --json is set, ANY error path must emit a JSON envelope on
    // stdout so `kb search --json | jq` pipelines parse cleanly.
    // Pre-fix the anyhow error bubbled up to main()'s stderr printer,
    // breaking automation. Deep-review M-cli.
    if json {
        if let Err(ref e) = result {
            let envelope = serde_json::json!({
                "error": e.to_string(),
                "source": daemon.unwrap_or("(auto-detect)"),
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&envelope).unwrap_or_default()
            );
            // Still exit non-zero so the shell knows it failed.
            std::process::exit(1);
        }
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn run_inner(
    config_path: Option<&PathBuf>,
    q: &str,
    kb: Option<&str>,
    mode: &str,
    limit: u32,
    category: Option<&str>,
    offline: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
    read_from: Option<&str>,
    read_to: Option<&str>,
) -> Result<()> {
    if offline && mode != "keyword" {
        return Err(anyhow!(
            "--offline implies --mode=keyword (got {mode}); offline read has no embedder. \
             Drop --offline to use the daemon for hybrid/semantic."
        ));
    }
    // Q-track (board B1) — the read-during window needs the daemon's
    // reading-history table; `--offline` opens lance read-only with no
    // sqlite access at all.
    if offline && (read_from.is_some() || read_to.is_some()) {
        return Err(anyhow!(
            "--offline has no access to reading history; drop --offline to use \
             --read-from/--read-to"
        ));
    }
    let read_from_unix = read_from.map(parse_time_bound).transpose()?;
    let read_to_unix = read_to.map(parse_time_bound).transpose()?;

    if !offline {
        if let Some(url) = http::detect_daemon(daemon, bearer).await {
            return search_via_http(
                &url,
                q,
                kb,
                mode,
                limit,
                category,
                read_from_unix,
                read_to_unix,
                bearer,
                json,
            )
            .await;
        }
        if let Some(forced) = daemon {
            return Err(anyhow!("daemon at {forced} not reachable"));
        }
        eprintln!("daemon not reachable — falling back to offline lance read");
    }

    search_offline(config_path, q, kb, limit, category, json).await
}

/// Parse a `--read-from`/`--read-to` bound: either a bare unix-seconds
/// integer or a `YYYY-MM-DD` calendar date (midnight UTC). Pure so both
/// accepted shapes are unit-testable without a daemon.
fn parse_time_bound(s: &str) -> Result<i64> {
    if let Ok(secs) = s.parse::<i64>() {
        return Ok(secs);
    }
    let date = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|e| {
        anyhow!("invalid --read-from/--read-to {s:?}: expected unix seconds or YYYY-MM-DD ({e})")
    })?;
    let dt = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| anyhow!("invalid date {s:?}"))?;
    Ok(dt.and_utc().timestamp())
}

/// Pure URL builder for `GET /api/search` — kept separate from the
/// network call so the query string (esp. `category`, the R0 escape
/// hatch) is unit-testable without a live daemon.
#[allow(clippy::too_many_arguments)]
fn build_search_url(
    daemon: &str,
    q: &str,
    kb: Option<&str>,
    mode: &str,
    limit: u32,
    category: Option<&str>,
    read_from: Option<i64>,
    read_to: Option<i64>,
) -> String {
    // Use the shared `encode_path_segment` for the query string
    // values too — the RFC 3986 unreserved set is the same, and
    // we get one source of truth (LOW: deep-review urlencoding
    // duplication).
    let mut url = format!(
        "{daemon}/api/search?q={}&mode={mode}&limit={limit}",
        http::encode_path_segment(q)
    );
    if let Some(k) = kb {
        url.push_str(&format!("&kb={}", http::encode_path_segment(k)));
    }
    if let Some(c) = category {
        url.push_str(&format!("&category={}", http::encode_path_segment(c)));
    }
    // Q-track (board B1) — read-during window, already resolved to unix
    // seconds by `parse_time_bound` before this URL is built.
    if let Some(f) = read_from {
        url.push_str(&format!("&read_from={f}"));
    }
    if let Some(t) = read_to {
        url.push_str(&format!("&read_to={t}"));
    }
    url
}

/// Raw `bm25_query` fetch size for `--offline` — widened when a
/// client-side `--category` filter is about to run, so the post-filter
/// result isn't silently short of `--limit`. Pure so it's unit-testable
/// without a lance dataset.
fn offline_fetch_limit(limit: u32, filtering: bool) -> u32 {
    if !filtering {
        return limit;
    }
    limit.max(OFFLINE_FILTER_POOL)
}

/// Exact-match `kb_category` gate — the client-side stand-in for the
/// daemon's R0 filter (`routes/search.rs`'s `Filters::keep`) applied to
/// `--offline` rows, which carry no R0 filter of their own.
fn category_matches(wanted: Option<&str>, actual: Option<&str>) -> bool {
    match wanted {
        Some(c) => actual == Some(c),
        None => true,
    }
}

#[allow(clippy::too_many_arguments)]
async fn search_via_http(
    daemon: &str,
    q: &str,
    kb: Option<&str>,
    mode: &str,
    limit: u32,
    category: Option<&str>,
    read_from: Option<i64>,
    read_to: Option<i64>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let url = build_search_url(daemon, q, kb, mode, limit, category, read_from, read_to);
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let resp = client.get(&url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("daemon returned {status}: {body}"));
    }
    let body: SearchResp = resp.json().await?;
    if json {
        print_hits_json(&body.hits, body.ms, daemon)?;
    } else {
        print_hits(&body.hits, body.ms, daemon);
    }
    Ok(())
}

async fn search_offline(
    config_path: Option<&PathBuf>,
    q: &str,
    kb: Option<&str>,
    limit: u32,
    category: Option<&str>,
    json: bool,
) -> Result<()> {
    let cfg_path = resolve_config_path(config_path)?;
    let cfg = load_config_or_default(&cfg_path)?;
    if cfg.kb.is_empty() {
        return Err(anyhow!("no kbs configured in {}", cfg_path.display()));
    }

    let kb_name = match kb {
        Some(k) => KbName::new(k).map_err(|e| anyhow!("invalid kb {k:?}: {e}"))?,
        None if cfg.kb.len() == 1 => cfg.kb.keys().next().unwrap().clone(),
        None => {
            return Err(anyhow!(
                "must specify --kb when multiple kbs are configured"
            ))
        }
    };

    let paths = KbPaths::new(cfg.daemon.name.as_deref().unwrap_or("default"))?;
    let lance_path = paths.kb_lance(&kb_name);
    if !lance_path.exists() {
        return Err(anyhow!(
            "no lance dataset at {}; run the daemon first",
            lance_path.display()
        ));
    }

    let config_dim = cfg
        .kb
        .get(&kb_name)
        .and_then(|s| s.embedding_model.as_deref())
        .and_then(kb_core::embed::model_info)
        .map(|m| m.dim as i32);
    let storage = Storage::open(&lance_path, config_dim).await?;
    // Offline lance reads carry no R0 filter (unlike `/api/search`, which
    // hides `memory-session` docs unless `?category=` is set — see
    // routes/search.rs), so `--category` is applied client-side below.
    // `bm25_query`'s `limit` truncates BEFORE that filter runs, so
    // deepen the raw fetch when filtering — same idea as the daemon's
    // own `deepen`/`FILTERED_POOL` widening — or a narrow category would
    // silently return fewer than `--limit` hits.
    let fetch_limit = offline_fetch_limit(limit, category.is_some());
    let started = std::time::Instant::now();
    let rows = storage.bm25_query(q, fetch_limit, false).await?;
    let ms = started.elapsed().as_millis() as u64;

    let hits: Vec<Hit> = rows
        .into_iter()
        .map(|r| Hit {
            id: r.id,
            title: r.title,
            path: r.path,
            kb_category: r.kb_category,
        })
        .filter(|h| category_matches(category, h.kb_category.as_deref()))
        .take(limit as usize)
        .collect();

    if json {
        print_hits_json(&hits, ms, "(offline)")?;
    } else {
        print_hits(&hits, ms, "(offline)");
    }
    Ok(())
}

fn print_hits(hits: &[Hit], ms: u64, source: &str) {
    println!("{} hits in {} ms ({})", hits.len(), ms, source);
    for hit in hits {
        let cat = hit.kb_category.as_deref().unwrap_or("-");
        println!(
            "  {}  {}  [{}]\n        {}",
            hit.id, hit.title, cat, hit.path
        );
    }
}

fn print_hits_json(hits: &[Hit], ms: u64, source: &str) -> Result<()> {
    let value = serde_json::json!({
        "hits": hits,
        "ms": ms,
        "source": source,
    });
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

// urlencoding helper moved to `crate::http::encode_path_segment` —
// same Crockford-RFC3986 unreserved set, one source of truth across
// the CLI verbs (LOW: deep-review urlencoding duplication).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_search_url_omits_category_when_unset() {
        let url = build_search_url(
            "http://127.0.0.1:4000",
            "borrow checker",
            None,
            "hybrid",
            20,
            None,
            None,
            None,
        );
        assert_eq!(
            url,
            "http://127.0.0.1:4000/api/search?q=borrow%20checker&mode=hybrid&limit=20"
        );
    }

    #[test]
    fn build_search_url_appends_category_after_kb() {
        let url = build_search_url(
            "http://127.0.0.1:4000",
            "DD_TRACE_PARTIAL_FLUSH_ENABLED",
            Some("sessions"),
            "keyword",
            20,
            Some("memory-session"),
            None,
            None,
        );
        assert_eq!(
            url,
            "http://127.0.0.1:4000/api/search?q=DD_TRACE_PARTIAL_FLUSH_ENABLED&mode=keyword&limit=20&kb=sessions&category=memory-session"
        );
    }

    #[test]
    fn build_search_url_encodes_category_value() {
        // Categories are single tokens in practice, but the encoder is
        // shared with every other query param — confirm it actually runs
        // on `category` rather than being interpolated raw.
        let url = build_search_url("http://d", "q", None, "hybrid", 5, Some("a b"), None, None);
        assert!(url.ends_with("&category=a%20b"), "{url}");
    }

    // Q-track (board B1) — read-during window params.
    #[test]
    fn build_search_url_appends_read_from_and_read_to() {
        let url = build_search_url(
            "http://d",
            "q",
            None,
            "hybrid",
            5,
            None,
            Some(1_700_000_000),
            Some(1_800_000_000),
        );
        assert_eq!(
            url,
            "http://d/api/search?q=q&mode=hybrid&limit=5&read_from=1700000000&read_to=1800000000"
        );
    }

    #[test]
    fn build_search_url_omits_read_window_when_unset() {
        let url = build_search_url("http://d", "q", None, "hybrid", 5, None, None, None);
        assert!(!url.contains("read_from"));
        assert!(!url.contains("read_to"));
    }

    #[test]
    fn parse_time_bound_accepts_unix_seconds() {
        assert_eq!(parse_time_bound("1700000000").unwrap(), 1_700_000_000);
        // A leading '-' still parses as a plain i64 (pre-epoch, allowed).
        assert_eq!(parse_time_bound("-5").unwrap(), -5);
    }

    #[test]
    fn parse_time_bound_accepts_calendar_date_at_utc_midnight() {
        // 2024-01-01T00:00:00Z.
        assert_eq!(parse_time_bound("2024-01-01").unwrap(), 1_704_067_200);
    }

    #[test]
    fn parse_time_bound_rejects_garbage() {
        assert!(parse_time_bound("not-a-date").is_err());
        assert!(parse_time_bound("2024-13-99").is_err());
    }

    #[test]
    fn offline_fetch_limit_unchanged_without_category() {
        assert_eq!(offline_fetch_limit(20, false), 20);
        assert_eq!(offline_fetch_limit(1000, false), 1000);
    }

    #[test]
    fn offline_fetch_limit_matches_the_daemon_filtered_pool_depth() {
        // The daemon fetches limit.max(FILTERED_POOL)=400 whenever a
        // filter is set (routes/search.rs) — offline must match that
        // depth, not a shallower heuristic, or it under-returns.
        assert_eq!(offline_fetch_limit(20, true), 400);
        assert_eq!(offline_fetch_limit(5, true), 400);
    }

    #[test]
    fn offline_fetch_limit_never_shrinks_below_requested_limit() {
        // A --limit above the pool size must still fetch at least that
        // many rows — the widening never narrows the caller's ask.
        assert_eq!(offline_fetch_limit(1000, true), 1000);
    }

    #[test]
    fn category_matches_none_wanted_passes_everything() {
        assert!(category_matches(None, Some("memory-session")));
        assert!(category_matches(None, None));
    }

    #[test]
    fn category_matches_exact_only() {
        assert!(category_matches(
            Some("memory-session"),
            Some("memory-session")
        ));
        assert!(!category_matches(Some("memory-session"), Some("note")));
        assert!(!category_matches(Some("memory-session"), None));
    }
}
