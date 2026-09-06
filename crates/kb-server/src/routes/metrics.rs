//! `GET /api/metrics` — queryable snapshot of the daemon's request +
//! pipeline timing (TM-track).
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
//! treat them as indicative, not exact.

use crate::state::{
    KbHandles, RouteKind, RouteMetrics, SearchStage, LATENCY_BUCKETS_MS, LATENCY_BUCKET_COUNT,
};
use axum::{extract::State, response::IntoResponse, Json};
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

pub async fn get(State(state): State<Arc<KbHandles>>) -> impl IntoResponse {
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

    Json(MetricsResponse {
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
    })
}
