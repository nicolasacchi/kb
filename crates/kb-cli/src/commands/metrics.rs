//! `kb metrics` — print the daemon's `GET /api/metrics` snapshot.
//!
//! The coarse per-route latency table is always shown. The detailed tables
//! (search stages / per-kb / ingest pipeline) appear only when the daemon was
//! started with `[server] metrics = true`; otherwise a hint is printed.
//! Percentiles are bucket-approximated — indicative, not exact.

use crate::http::client_with_timeout_and_bearer;
use anyhow::{anyhow, Result};
use serde_json::Value;

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";

fn base_url(daemon: Option<&str>) -> String {
    daemon
        .unwrap_or(DEFAULT_DAEMON)
        .trim_end_matches('/')
        .to_string()
}

pub async fn run(daemon: Option<&str>, bearer: Option<&str>, json: bool) -> Result<()> {
    let base = base_url(daemon);
    let client = client_with_timeout_and_bearer(5, bearer)?;
    let url = format!("{base}/api/metrics");
    let resp =
        client.get(&url).send().await.map_err(|e| {
            anyhow!("daemon not reachable at {base} ({e}); start it with `kb daemon`")
        })?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("daemon returned {status}: {body}"));
    }
    // A daemon running an OLDER build has no `/api/metrics` route, so the SPA
    // fallback answers 200 text/html. Detect that and give a clear hint
    // rather than a raw JSON-decode error (binary/bundle drift is a known
    // footgun — `kb daemon` with the current binary fixes it).
    let is_json = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("application/json"));
    if !is_json {
        return Err(anyhow!(
            "daemon at {base} responded with non-JSON — it's likely running an \
             older build without /api/metrics. Restart it with the current binary \
             (`kb daemon`)."
        ));
    }
    let body: Value = resp.json().await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    print_human(&base, &body);
    Ok(())
}

fn u(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn print_human(base: &str, body: &Value) {
    println!("daemon: {base}");
    println!(
        "requests total: {}    storage queue: {}/{}",
        u(body, "requests_total"),
        u(body, "storage_channel_depth"),
        u(body, "storage_channel_capacity"),
    );
    // v0.24 T1 — embedder health (1 Hz ticker probe; absent on an older
    // daemon → skip rather than print a misleading default).
    if let Some(degraded) = body.get("embedder_degraded").and_then(Value::as_bool) {
        let state = if degraded {
            "DEGRADED (semantic search down — keyword-only)"
        } else {
            "ok"
        };
        println!(
            "embedder: {state}    respawns: {}",
            u(body, "embedder_respawn_count")
        );
    }

    // Coarse per-route latency (always present).
    println!("\nroutes (end-to-end request latency):");
    println!(
        "  {:<10} {:>8} {:>7} {:>7} {:>7}",
        "kind", "count", "p50", "p95", "p99"
    );
    if let Some(routes) = body.get("routes").and_then(Value::as_array) {
        for r in routes {
            println!(
                "  {:<10} {:>8} {:>7} {:>7} {:>7}",
                r.get("kind").and_then(Value::as_str).unwrap_or("?"),
                u(r, "count"),
                ms(u(r, "p50_ms")),
                ms(u(r, "p95_ms")),
                ms(u(r, "p99_ms")),
            );
        }
    }

    let detailed = body.get("detailed");
    if !body
        .get("detailed_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || detailed.map(Value::is_null).unwrap_or(true)
    {
        println!(
            "\n(detailed metrics off — set `[server] metrics = true` in kb.toml to enable\n \
             per-search-stage, per-kb, and ingest-pipeline timing.)"
        );
        return;
    }
    let detailed = detailed.unwrap();

    // Search stages.
    println!("\nsearch stages:");
    println!(
        "  {:<10} {:>8} {:>7} {:>7} {:>7}",
        "stage", "count", "p50", "p95", "p99"
    );
    print_latency_rows(detailed.get("search_stages"));

    // Per-kb request latency.
    println!("\nper-kb request latency:");
    println!(
        "  {:<10} {:>8} {:>7} {:>7} {:>7}",
        "kb", "count", "p50", "p95", "p99"
    );
    print_latency_rows(detailed.get("per_kb"));

    // Ingest pipeline.
    if let Some(p) = detailed.get("pipeline") {
        println!("\npipeline:");
        if let Some(i) = p.get("indexer") {
            println!(
                "  indexer:  files={:<6} p50={} p95={} p99={}",
                u(i, "files_indexed"),
                ms(u(i, "p50_ms")),
                ms(u(i, "p95_ms")),
                ms(u(i, "p99_ms")),
            );
        }
        if let Some(e) = p.get("embed_index") {
            println!(
                "  embed:    calls={:<6} docs={:<6} p50={} p95={} p99={}",
                u(e, "calls"),
                u(e, "docs"),
                ms(u(e, "p50_ms")),
                ms(u(e, "p95_ms")),
                ms(u(e, "p99_ms")),
            );
        }
        if let Some(storage) = p.get("storage").and_then(Value::as_array) {
            println!("  storage actor (handler / queue-wait):");
            println!(
                "    {:<9} {:>7} {:>16} {:>16}",
                "kind", "count", "handler p50/95/99", "queue p50/95/99"
            );
            for s in storage {
                println!(
                    "    {:<9} {:>7} {:>16} {:>16}",
                    s.get("kind").and_then(Value::as_str).unwrap_or("?"),
                    u(s, "count"),
                    trio(
                        u(s, "handler_p50_ms"),
                        u(s, "handler_p95_ms"),
                        u(s, "handler_p99_ms")
                    ),
                    trio(
                        u(s, "queue_wait_p50_ms"),
                        u(s, "queue_wait_p95_ms"),
                        u(s, "queue_wait_p99_ms")
                    ),
                );
            }
        }
    }
}

fn print_latency_rows(rows: Option<&Value>) {
    if let Some(arr) = rows.and_then(Value::as_array) {
        for r in arr {
            println!(
                "  {:<10} {:>8} {:>7} {:>7} {:>7}",
                r.get("label").and_then(Value::as_str).unwrap_or("?"),
                u(r, "count"),
                ms(u(r, "p50_ms")),
                ms(u(r, "p95_ms")),
                ms(u(r, "p99_ms")),
            );
        }
    }
}

/// Format a millisecond value; the >10s overflow bucket reports `10001`.
fn ms(v: u64) -> String {
    if v > 10_000 {
        ">10s".to_string()
    } else {
        format!("{v}ms")
    }
}

fn trio(p50: u64, p95: u64, p99: u64) -> String {
    format!("{}/{}/{}", ms(p50), ms(p95), ms(p99))
}
