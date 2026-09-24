//! `GET /api/metrics` — queryable JSON snapshot of the daemon's request +
//! pipeline timing (TM-track). `GET /metrics` renders the same snapshot as
//! Prometheus text exposition 0.0.4. No extra counters.
//!
//! The COARSE block (per-route latency histograms, request total, storage
//! queue depth) is ALWAYS present — it mirrors what the 1 Hz `metrics.tick`
//! SSE streams, but here as a one-shot `curl`-able snapshot with p99 + the
//! raw bucket counts the SSE omits. The DETAILED block (`detailed`) is
//! populated ONLY when `[server] metrics = true`, else `null`: per-search-
//! stage latency (embed/bm25/vector/hybrid), per-kb request latency, and the
//! ingest-pipeline snapshot (indexer / index-side embed / storage actor).
//!
//! A Read route — it sits in the un-rate-limited api tree behind
//! `auth_bearer` (loopback bypass), like `/identity` + `/stats`. Percentiles
//! are bucket-approximated (the same histogram the coarse layer always used);
//! treat them as indicative, not exact. `GET /metrics` is top-level (not
//! under `/api`) and carries the same auth via its own `route_layer`.

use crate::middleware::auth_bearer;
use crate::state::{
    AuthConfig, KbHandles, RouteKind, RouteMetrics, SearchStage, LATENCY_BUCKETS_MS,
    LATENCY_BUCKET_COUNT,
};
use axum::{
    extract::State, http::header, middleware::from_fn_with_state, response::IntoResponse,
    routing::get as route_get, Json, Router,
};
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Serialize)]
pub struct MetricsResponse {
    /// Total requests since daemon boot.
    pub requests_total: u64,
    /// Busiest kb's storage-actor queue depth right now (live, not the 1 Hz
    /// ticker's snapshot).
    pub storage_channel_depth: u32,
    /// Storage-actor channel capacity (depth / capacity = load fraction).
    pub storage_channel_capacity: u32,
    /// v0.24 T1 — daemon-wide embedder health from the 1 Hz ticker probe
    /// (same signal `metrics.tick` streams; up to 1 s stale). `true` iff
    /// any embedder subprocess is currently unrecoverable — semantic
    /// search degraded to keyword-only.
    pub embedder_degraded: bool,
    /// v0.24 T1 — cumulative embedder-subprocess respawns since boot,
    /// summed across distinct backends. Climbing = embedder keeps dying.
    pub embedder_respawn_count: u64,
    /// The histogram bucket upper boundaries in ms, so a client can label the
    /// `routes[].buckets` array without hardcoding them.
    pub buckets_ms: &'static [u32],
    /// Per-route-family latency (always present).
    pub routes: Vec<RouteSnapshot>,
    /// Whether the detailed layer is enabled (`[server] metrics`).
    pub detailed_enabled: bool,
    /// Detailed layer — `None` (serialised `null`) unless `detailed_enabled`.
    pub detailed: Option<DetailedSnapshot>,
}

#[derive(Debug, Serialize)]
pub struct RouteSnapshot {
    pub kind: &'static str,
    pub count: u64,
    pub p50_ms: u32,
    pub p95_ms: u32,
    pub p99_ms: u32,
    /// Raw cumulative bucket counts aligned to `buckets_ms` (+1 overflow).
    pub buckets: [u64; LATENCY_BUCKET_COUNT],
}

#[derive(Debug, Serialize)]
pub struct LatencySnapshot {
    /// `stage` for search stages, `kb` for per-kb — see the wrappers.
    pub label: String,
    pub count: u64,
    pub p50_ms: u32,
    pub p95_ms: u32,
    pub p99_ms: u32,
}

#[derive(Debug, Serialize)]
pub struct DetailedSnapshot {
    /// Per-search-stage latency (embed / bm25 / vector / hybrid).
    pub search_stages: Vec<LatencySnapshot>,
    /// Per-kb request latency, sorted by kb name for a stable order.
    pub per_kb: Vec<LatencySnapshot>,
    /// Ingest-pipeline timing (indexer / index-side embed / storage actor).
    pub pipeline: kb_core::metrics::PipelineSnapshot,
}

fn route_snapshot(kind: &'static str, m: &RouteMetrics) -> RouteSnapshot {
    let (p50, p95, p99) = m.hist.percentiles();
    RouteSnapshot {
        kind,
        count: m.count(),
        p50_ms: p50,
        p95_ms: p95,
        p99_ms: p99,
        buckets: m.buckets_snapshot(),
    }
}

fn latency_snapshot(label: String, m: &RouteMetrics) -> LatencySnapshot {
    let (p50, p95, p99) = m.hist.percentiles();
    LatencySnapshot {
        label,
        count: m.count(),
        p50_ms: p50,
        p95_ms: p95,
        p99_ms: p99,
    }
}

/// Prometheus text exposition 0.0.4. Scrapers branch on `version=0.0.4`;
/// `charset=utf-8` is the form the exposition spec names.
pub const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

fn snapshot(state: &KbHandles) -> MetricsResponse {
    let m = &state.metrics;

    let routes = RouteKind::ALL
        .iter()
        .map(|k| route_snapshot(k.label(), &m.by_route[*k as usize]))
        .collect();

    let detailed_enabled = m.detailed_enabled();
    let detailed = detailed_enabled.then(|| {
        let search_stages = SearchStage::ALL
            .iter()
            .map(|s| latency_snapshot(s.label().to_string(), &m.search_stages[*s as usize]))
            .collect();
        // The per-kb map iterates in HashMap (random) order; sort for a stable
        // response the SPA/CLI table can render without reshuffling.
        let mut per_kb: Vec<LatencySnapshot> = m
            .per_kb
            .iter()
            .map(|(kb, rm)| latency_snapshot(kb.clone(), rm))
            .collect();
        per_kb.sort_by(|a, b| a.label.cmp(&b.label));
        DetailedSnapshot {
            search_stages,
            per_kb,
            pipeline: state.pipeline.snapshot(),
        }
    });

    // Live depth: busiest kb's queue right now (matches the ticker's max-over-
    // kbs aggregation, but without the up-to-1s staleness).
    let storage_channel_depth = state
        .kbs
        .values()
        .map(|ctx| ctx.storage.queue_depth() as u32)
        .max()
        .unwrap_or(0);

    MetricsResponse {
        requests_total: m.total.load(std::sync::atomic::Ordering::Relaxed),
        embedder_degraded: m
            .embedder_degraded
            .load(std::sync::atomic::Ordering::Relaxed),
        embedder_respawn_count: m
            .embedder_respawn_count
            .load(std::sync::atomic::Ordering::Relaxed),
        storage_channel_depth,
        storage_channel_capacity: kb_core::storage::actor::StorageHandle::queue_capacity() as u32,
        buckets_ms: LATENCY_BUCKETS_MS,
        routes,
        detailed_enabled,
        detailed,
    }
}

/// `GET /api/metrics` — JSON snapshot. Unchanged shape.
pub async fn get(State(state): State<Arc<KbHandles>>) -> impl IntoResponse {
    Json(snapshot(&state))
}

/// `GET /metrics` — Prometheus text of [`snapshot`]. Same counters, no new state.
pub async fn prometheus(State(state): State<Arc<KbHandles>>) -> impl IntoResponse {
    let body = render_prometheus(&snapshot(&state));
    (
        [
            (header::CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
}

/// Top-level mount. Auth matches `GET /api/metrics` (loopback bypass) without
/// re-wrapping the `/api` tree. The route string lives here, not in
/// `router.rs`, so the api-docs extractor does not prefix it onto `/api/metrics`.
pub fn prometheus_router(auth: Arc<AuthConfig>) -> Router<Arc<KbHandles>> {
    Router::new()
        .route("/metrics", route_get(prometheus))
        .route_layer(from_fn_with_state(auth, auth_bearer))
}

fn render_prometheus(snap: &MetricsResponse) -> String {
    let mut out = String::with_capacity(4096);
    emit_scalars(&mut out, snap);
    emit_route_series(&mut out, snap);
    if let Some(detailed) = &snap.detailed {
        emit_detailed(&mut out, detailed);
    }
    out
}

fn emit_scalars(out: &mut String, snap: &MetricsResponse) {
    family(
        out,
        "kb_http_requests_total",
        "counter",
        "HTTP requests since daemon boot (GET /api/metrics requests_total).",
    );
    sample(out, "kb_http_requests_total", &[], snap.requests_total);

    family(
        out,
        "kb_storage_channel_depth",
        "gauge",
        "Busiest kb storage-actor queue depth.",
    );
    sample(
        out,
        "kb_storage_channel_depth",
        &[],
        u64::from(snap.storage_channel_depth),
    );

    family(
        out,
        "kb_storage_channel_capacity",
        "gauge",
        "Storage-actor channel capacity.",
    );
    sample(
        out,
        "kb_storage_channel_capacity",
        &[],
        u64::from(snap.storage_channel_capacity),
    );

    family(
        out,
        "kb_embedder_degraded",
        "gauge",
        "1 if any embedder subprocess is currently unrecoverable.",
    );
    sample(
        out,
        "kb_embedder_degraded",
        &[],
        u64::from(snap.embedder_degraded),
    );

    family(
        out,
        "kb_embedder_respawns_total",
        "counter",
        "Embedder subprocess respawns since boot.",
    );
    sample(
        out,
        "kb_embedder_respawns_total",
        &[],
        snap.embedder_respawn_count,
    );

    family(
        out,
        "kb_metrics_detailed",
        "gauge",
        "1 if the detailed metrics layer is enabled.",
    );
    sample(
        out,
        "kb_metrics_detailed",
        &[],
        u64::from(snap.detailed_enabled),
    );
}

fn emit_route_series(out: &mut String, snap: &MetricsResponse) {
    family(
        out,
        "kb_route_requests_total",
        "counter",
        "Requests since boot by route family.",
    );
    for route in &snap.routes {
        sample(
            out,
            "kb_route_requests_total",
            &[("route", route.kind)],
            route.count,
        );
    }

    family(
        out,
        "kb_route_latency_ms",
        "gauge",
        "Bucket-approximated route latency percentile, milliseconds.",
    );
    for route in &snap.routes {
        sample(
            out,
            "kb_route_latency_ms",
            &[("route", route.kind), ("quantile", "0.5")],
            u64::from(route.p50_ms),
        );
        sample(
            out,
            "kb_route_latency_ms",
            &[("route", route.kind), ("quantile", "0.95")],
            u64::from(route.p95_ms),
        );
        sample(
            out,
            "kb_route_latency_ms",
            &[("route", route.kind), ("quantile", "0.99")],
            u64::from(route.p99_ms),
        );
    }

    // Non-cumulative: each observation lands in one bucket (the JSON
    // `buckets` array). Not a Prometheus histogram — no `_sum`, and `le_ms`
    // is a label rather than the histogram `le` convention. Overflow is `+Inf`.
    family(
        out,
        "kb_route_latency_bucket",
        "counter",
        "Non-cumulative latency-bucket observations by route (le_ms is the upper bound; +Inf is overflow).",
    );
    for route in &snap.routes {
        for (i, count) in route.buckets.iter().enumerate() {
            let le = snap
                .buckets_ms
                .get(i)
                .map(u32::to_string)
                .unwrap_or_else(|| "+Inf".to_string());
            sample(
                out,
                "kb_route_latency_bucket",
                &[("route", route.kind), ("le_ms", &le)],
                *count,
            );
        }
    }
}

fn emit_detailed(out: &mut String, detailed: &DetailedSnapshot) {
    emit_search_and_kb(out, detailed);
    emit_pipeline_index(out, &detailed.pipeline);
    emit_pipeline_storage(out, &detailed.pipeline);
}

fn emit_search_and_kb(out: &mut String, detailed: &DetailedSnapshot) {
    family(
        out,
        "kb_search_stage_requests_total",
        "counter",
        "Search-stage observations since boot.",
    );
    for stage in &detailed.search_stages {
        sample(
            out,
            "kb_search_stage_requests_total",
            &[("stage", &stage.label)],
            stage.count,
        );
    }

    family(
        out,
        "kb_search_stage_latency_ms",
        "gauge",
        "Bucket-approximated search-stage latency percentile, milliseconds.",
    );
    for stage in &detailed.search_stages {
        emit_quantiles(
            out,
            "kb_search_stage_latency_ms",
            "stage",
            &stage.label,
            stage,
        );
    }

    family(
        out,
        "kb_per_kb_requests_total",
        "counter",
        "Requests since boot attributed to a kb.",
    );
    for kb in &detailed.per_kb {
        sample(
            out,
            "kb_per_kb_requests_total",
            &[("kb", &kb.label)],
            kb.count,
        );
    }

    family(
        out,
        "kb_per_kb_latency_ms",
        "gauge",
        "Bucket-approximated per-kb request latency percentile, milliseconds.",
    );
    for kb in &detailed.per_kb {
        emit_quantiles(out, "kb_per_kb_latency_ms", "kb", &kb.label, kb);
    }
}

fn emit_pipeline_index(out: &mut String, pipe: &kb_core::metrics::PipelineSnapshot) {
    family(
        out,
        "kb_pipeline_enabled",
        "gauge",
        "1 if ingest-pipeline timing is being recorded.",
    );
    sample(out, "kb_pipeline_enabled", &[], u64::from(pipe.enabled));

    family(
        out,
        "kb_indexer_files_total",
        "counter",
        "Files observed by the indexer.",
    );
    sample(
        out,
        "kb_indexer_files_total",
        &[],
        pipe.indexer.files_indexed,
    );

    family(
        out,
        "kb_indexer_latency_ms",
        "gauge",
        "Bucket-approximated indexer file latency percentile, milliseconds.",
    );
    emit_ms_quantiles(
        out,
        "kb_indexer_latency_ms",
        &[],
        pipe.indexer.p50_ms,
        pipe.indexer.p95_ms,
        pipe.indexer.p99_ms,
    );

    family(
        out,
        "kb_index_embed_calls_total",
        "counter",
        "Index-side embed calls.",
    );
    sample(
        out,
        "kb_index_embed_calls_total",
        &[],
        pipe.embed_index.calls,
    );

    family(
        out,
        "kb_index_embed_docs_total",
        "counter",
        "Documents passed to the index-side embedder.",
    );
    sample(out, "kb_index_embed_docs_total", &[], pipe.embed_index.docs);

    family(
        out,
        "kb_index_embed_latency_ms",
        "gauge",
        "Bucket-approximated index-side embed latency percentile, milliseconds.",
    );
    emit_ms_quantiles(
        out,
        "kb_index_embed_latency_ms",
        &[],
        pipe.embed_index.p50_ms,
        pipe.embed_index.p95_ms,
        pipe.embed_index.p99_ms,
    );
}

fn emit_pipeline_storage(out: &mut String, pipe: &kb_core::metrics::PipelineSnapshot) {
    family(
        out,
        "kb_storage_ops_total",
        "counter",
        "Storage-actor operations by kind.",
    );
    for op in &pipe.storage {
        sample(out, "kb_storage_ops_total", &[("kind", op.kind)], op.count);
    }

    family(
        out,
        "kb_storage_handler_latency_ms",
        "gauge",
        "Bucket-approximated storage-handler latency percentile, milliseconds.",
    );
    for op in &pipe.storage {
        emit_ms_quantiles(
            out,
            "kb_storage_handler_latency_ms",
            &[("kind", op.kind)],
            op.handler_p50_ms,
            op.handler_p95_ms,
            op.handler_p99_ms,
        );
    }

    family(
        out,
        "kb_storage_queue_wait_ms",
        "gauge",
        "Bucket-approximated storage-actor queue-wait percentile, milliseconds.",
    );
    for op in &pipe.storage {
        emit_ms_quantiles(
            out,
            "kb_storage_queue_wait_ms",
            &[("kind", op.kind)],
            op.queue_wait_p50_ms,
            op.queue_wait_p95_ms,
            op.queue_wait_p99_ms,
        );
    }
}

fn emit_quantiles(out: &mut String, name: &str, label: &str, value: &str, snap: &LatencySnapshot) {
    emit_ms_quantiles(
        out,
        name,
        &[(label, value)],
        snap.p50_ms,
        snap.p95_ms,
        snap.p99_ms,
    );
}

fn emit_ms_quantiles(
    out: &mut String,
    name: &str,
    labels: &[(&str, &str)],
    p50: u32,
    p95: u32,
    p99: u32,
) {
    for (q, ms) in [("0.5", p50), ("0.95", p95), ("0.99", p99)] {
        let mut with_q = Vec::with_capacity(labels.len() + 1);
        with_q.extend_from_slice(labels);
        with_q.push(("quantile", q));
        sample(out, name, &with_q, u64::from(ms));
    }
}

fn family(out: &mut String, name: &str, kind: &str, help: &str) {
    out.push_str("# HELP ");
    out.push_str(name);
    out.push(' ');
    out.push_str(help);
    out.push('\n');
    out.push_str("# TYPE ");
    out.push_str(name);
    out.push(' ');
    out.push_str(kind);
    out.push('\n');
}

fn sample(out: &mut String, name: &str, labels: &[(&str, &str)], value: u64) {
    out.push_str(name);
    if !labels.is_empty() {
        out.push('{');
        for (i, (k, v)) in labels.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(k);
            out.push_str("=\"");
            push_escaped(out, v);
            out.push('"');
        }
        out.push('}');
    }
    out.push(' ');
    use std::fmt::Write as _;
    let _ = write!(out, "{value}");
    out.push('\n');
}

fn push_escaped(out: &mut String, value: &str) {
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(kind: &'static str, count: u64, bucket_at: usize) -> RouteSnapshot {
        let mut buckets = [0; LATENCY_BUCKET_COUNT];
        buckets[bucket_at] = count;
        RouteSnapshot {
            kind,
            count,
            p50_ms: 5,
            p95_ms: 10,
            p99_ms: 25,
            buckets,
        }
    }

    #[test]
    fn prometheus_text_is_exposition_not_html() {
        let snap = MetricsResponse {
            requests_total: 7,
            storage_channel_depth: 2,
            storage_channel_capacity: 64,
            embedder_degraded: true,
            embedder_respawn_count: 3,
            buckets_ms: LATENCY_BUCKETS_MS,
            routes: vec![route("search", 4, 1)],
            detailed_enabled: false,
            detailed: None,
        };
        let text = render_prometheus(&snap);
        assert!(PROMETHEUS_CONTENT_TYPE.starts_with("text/plain; version=0.0.4"));
        assert!(text.contains("kb_http_requests_total 7\n"), "{text}");
        assert!(text.contains("kb_embedder_degraded 1\n"), "{text}");
        assert!(text.contains("kb_embedder_respawns_total 3\n"), "{text}");
        assert!(text.contains("kb_metrics_detailed 0\n"), "{text}");
        assert!(
            text.contains("kb_route_requests_total{route=\"search\"} 4\n"),
            "{text}"
        );
        assert!(
            text.contains("kb_route_latency_ms{route=\"search\",quantile=\"0.5\"} 5\n"),
            "{text}"
        );
        // Bucket 1 is the 5ms boundary; the observation count lives there.
        assert!(
            text.contains("kb_route_latency_bucket{route=\"search\",le_ms=\"5\"} 4\n"),
            "{text}"
        );
        assert!(text.contains("le_ms=\"+Inf\"} 0\n"), "{text}");
        assert!(!text.contains("kb_search_stage_requests_total"), "{text}");
        assert!(!text.contains("<html"), "{text}");
        assert!(!text.contains("text/html"), "{text}");
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn prometheus_escapes_labels_and_emits_detailed_counters() {
        let snap = MetricsResponse {
            requests_total: 0,
            storage_channel_depth: 0,
            storage_channel_capacity: 0,
            embedder_degraded: false,
            embedder_respawn_count: 0,
            buckets_ms: LATENCY_BUCKETS_MS,
            routes: vec![],
            detailed_enabled: true,
            detailed: Some(DetailedSnapshot {
                search_stages: vec![LatencySnapshot {
                    label: "embed".into(),
                    count: 2,
                    p50_ms: 1,
                    p95_ms: 1,
                    p99_ms: 1,
                }],
                per_kb: vec![LatencySnapshot {
                    label: "a\"b\\c".into(),
                    count: 9,
                    p50_ms: 0,
                    p95_ms: 0,
                    p99_ms: 0,
                }],
                pipeline: kb_core::metrics::PipelineSnapshot {
                    enabled: true,
                    indexer: kb_core::metrics::IndexerStats {
                        files_indexed: 11,
                        p50_ms: 0,
                        p95_ms: 0,
                        p99_ms: 0,
                    },
                    embed_index: kb_core::metrics::EmbedStats {
                        calls: 1,
                        docs: 2,
                        p50_ms: 0,
                        p95_ms: 0,
                        p99_ms: 0,
                    },
                    storage: vec![kb_core::metrics::StorageVariantStats {
                        kind: "query",
                        count: 8,
                        handler_p50_ms: 0,
                        handler_p95_ms: 0,
                        handler_p99_ms: 0,
                        queue_wait_p50_ms: 0,
                        queue_wait_p95_ms: 0,
                        queue_wait_p99_ms: 0,
                    }],
                },
            }),
        };
        let text = render_prometheus(&snap);
        assert!(text.contains("kb_metrics_detailed 1\n"), "{text}");
        assert!(
            text.contains("kb_search_stage_requests_total{stage=\"embed\"} 2\n"),
            "{text}"
        );
        assert!(
            text.contains("kb_per_kb_requests_total{kb=\"a\\\"b\\\\c\"} 9\n"),
            "{text}"
        );
        assert!(text.contains("kb_indexer_files_total 11\n"), "{text}");
        assert!(text.contains("kb_index_embed_docs_total 2\n"), "{text}");
        assert!(
            text.contains("kb_storage_ops_total{kind=\"query\"} 8\n"),
            "{text}"
        );
        assert!(!text.contains("<html"), "{text}");
    }
}
