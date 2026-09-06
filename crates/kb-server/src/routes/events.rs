//! `GET /api/events` — SSE firehose adapter wrapping
//! `kb_core::events::events_stream`. Supports `Last-Event-ID` replay
//! (header, or `?last_event_id=` for EventSource clients that can't set
//! headers), `?types=glob,glob` filtering (topic 04 §Decisions), and the
//! synthetic `lag` / `gap` events.
//!
//! v0.7.1 H5 — subscribes to the single daemon-wide event bus. Every
//! kb's watcher + indexer share that one bus, so a single stream with a
//! global id space covers all kbs and `Last-Event-ID` resume is
//! unambiguous. (Pre-H5 this merged one stream per kb, each with ids
//! from 1, so two kbs both emitted `id: 42` and resume drifted.)
//!
//! v0.24 T1 — `?filter=` honours `run:<id>`, `kb:<name>`, and
//! `artifact:<id>`; SL2 adds a fourth token, `slug:<slug>`, matched
//! against `payload.slug` (the slate family). Comma-separated tokens AND
//! together. These are for
//! CLI consumers (`kb events --follow --kb/--artifact`); the SPA's
//! SharedWorker keeps consuming the UNFILTERED stream per invariant #24.

use crate::state::KbHandles;
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::sse::{Event as SseEvent, KeepAlive, Sse},
};
use futures::{Stream, StreamExt};
use kb_core::events::{events_stream, EventFrame};
use kb_core::types::Envelope;
use serde::Deserialize;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

const KEEP_ALIVE: Duration = Duration::from_secs(15);

#[derive(Debug, Deserialize, Default)]
pub struct EventsParams {
    /// Comma-separated globs (e.g. `index.*,error`). Empty = all.
    pub types: Option<String>,
    /// Comma-separated `key:value` tokens, ANDed: `run:<id>`,
    /// `kb:<name>`, `artifact:<12-hex id>` (v0.24 T1 — previously only
    /// `run:` was honoured; `kb:`/`artifact:` were parsed + dropped).
    pub filter: Option<String>,
    /// Resume cursor for clients that can't set the `Last-Event-ID`
    /// header — the browser `EventSource` API exposes no way to send
    /// headers, and the SPA's manual-backoff layer reconnects with a
    /// FRESH EventSource, so the browser's own header replay never
    /// fires either. Without a cursor every reconnect replays the whole
    /// ring (`last_id = 0`). The header, when present, wins: it comes
    /// from a native auto-reconnect and is always at least as fresh as
    /// a URL baked at EventSource construction time.
    pub last_event_id: Option<u64>,
}

pub async fn get(
    State(state): State<Arc<KbHandles>>,
    Query(params): Query<EventsParams>,
    headers: HeaderMap,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let last_id: u64 = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .or(params.last_event_id)
        .unwrap_or(0);

    // Wrap the parsed filters in Arc so the per-frame `filter_map` closure
    // clones a refcount bump instead of deep-copying the prefix/exact Vecs
    // (and the PayloadFilter) on every event for the life of the stream.
    let type_filters = Arc::new(parse_type_filters(params.types.as_deref()));
    let payload_filter = Arc::new(parse_payload_filter(params.filter.as_deref()));

    // SW1 — live-consumer gauge. The guard is owned by the filter_map
    // closure below, so it drops exactly when the response stream drops:
    // client disconnect, shutdown take_until, or an error path. The
    // ticker surfaces the count as `sse_subscribers` in metrics.tick.
    state
        .metrics
        .sse_clients
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let client_guard = SseClientGuard(state.clone());

    // v0.7.1 H5 — one daemon-wide bus, one stream. The id space is
    // global, so `Last-Event-ID` resume is unambiguous (pre-H5 this
    // merged per-kb streams whose ids all started at 1 and collided).
    let sse_stream = events_stream(&state.bus, last_id).filter_map(move |frame| {
        let _ = &client_guard; // keep the gauge guard alive with the stream
        let type_filters = type_filters.clone();
        let payload_filter = payload_filter.clone();
        async move {
            // Synthetic frames (Lag/Gap) bypass the type filter — clients
            // need to know about them.
            match &frame {
                EventFrame::Envelope(env) => {
                    if !type_matches(&env.type_, &type_filters) {
                        return None;
                    }
                    if !payload_matches(env, &payload_filter) {
                        return None;
                    }
                }
                EventFrame::Lag { .. } | EventFrame::Gap { .. } => {}
            }
            Some(Ok::<_, Infallible>(frame_to_sse(frame)))
        }
    });

    // End the stream when the daemon starts shutting down. This subscription
    // never completes on its own (keep-alive every 15s, forever), so without
    // this axum's graceful-shutdown drain would block until every open SSE
    // client disconnects — i.e. forever — and `kb daemon stop` would time
    // out. `take_until` resolves the moment `shutdown` flips to `true` (or
    // the sender is dropped), closing the connection cleanly.
    let mut shutdown = state.shutdown.subscribe();
    let sse_stream = sse_stream.take_until(async move {
        let _ = shutdown.wait_for(|&down| down).await;
    });

    Sse::new(sse_stream).keep_alive(KeepAlive::new().interval(KEEP_ALIVE).text(":keep-alive"))
}

/// Decrements `metrics.sse_clients` when the response stream is dropped.
/// Owned (via capture) by the stream's `filter_map` closure — the only
/// thing whose lifetime exactly matches the connection's.
struct SseClientGuard(Arc<KbHandles>);

impl Drop for SseClientGuard {
    fn drop(&mut self) {
        self.0
            .metrics
            .sse_clients
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

fn frame_to_sse(frame: EventFrame) -> SseEvent {
    match frame {
        EventFrame::Envelope(env) => {
            let body = envelope_body(&env);
            SseEvent::default()
                .id(env.id.to_string())
                .event(env.type_)
                .data(body)
        }
        EventFrame::Lag { skipped } => SseEvent::default()
            .event("lag")
            .data(serde_json::json!({ "skipped": skipped }).to_string()),
        EventFrame::Gap {
            requested_id,
            oldest_available_id,
        } => SseEvent::default().event("gap").data(
            serde_json::json!({
                "requested_id": requested_id,
                "oldest_available_id": oldest_available_id,
            })
            .to_string(),
        ),
    }
}

/// The SSE `data:` body for an envelope frame — the `{payload, ts, v}` wire
/// shape (serde_json map keys serialize alphabetically). The payload's JSON
/// comes from the envelope's shared memo ([`Envelope::payload_json`]), so
/// with M open SSE streams each event's payload is serialized once, not M
/// times — only this small wrapper is built per subscriber. MUST stay
/// byte-identical to the pre-memo
/// `serde_json::to_string(&json!({"v": …, "ts": …, "payload": …}))`
/// (pinned by `envelope_body_matches_full_serialization`).
fn envelope_body(env: &Envelope) -> String {
    let payload = env.payload_json();
    // RFC 3339 needs no JSON escaping, but serialize through serde_json
    // anyway so the quoting/escaping rules can never drift.
    let ts = serde_json::to_string(&env.ts.to_rfc3339()).unwrap_or_else(|_| "\"\"".to_string());
    format!("{{\"payload\":{payload},\"ts\":{ts},\"v\":{}}}", env.v)
}

#[derive(Debug, Clone, Default)]
struct TypeFilters {
    /// Exact-match prefixes (e.g. `index.` from `index.*`).
    prefixes: Vec<String>,
    /// Exact-match types (e.g. `error` matches `error`).
    exacts: Vec<String>,
    /// True if no filter was supplied — match everything.
    match_all: bool,
}

fn parse_type_filters(raw: Option<&str>) -> TypeFilters {
    match raw {
        None | Some("") => TypeFilters {
            match_all: true,
            ..Default::default()
        },
        Some(s) => {
            let mut filters = TypeFilters::default();
            for token in s.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                if let Some(prefix) = token.strip_suffix("*") {
                    filters.prefixes.push(prefix.to_string());
                } else {
                    filters.exacts.push(token.to_string());
                }
            }
            filters
        }
    }
}

fn type_matches(type_: &str, filters: &TypeFilters) -> bool {
    if filters.match_all {
        return true;
    }
    if filters.exacts.iter().any(|t| t == type_) {
        return true;
    }
    if filters.prefixes.iter().any(|p| type_.starts_with(p)) {
        return true;
    }
    false
}

#[derive(Debug, Clone, Default)]
struct PayloadFilter {
    run_id: Option<String>,
    /// v0.24 T1 — keep only events whose `payload.kb` equals this name.
    /// Events without a `kb` field (e.g. `metrics.tick`) are dropped.
    kb: Option<String>,
    /// v0.24 T1 — keep only events that reference this artifact id:
    /// `payload.artifact_id`, with `payload.hash` / `payload.id` as
    /// fallbacks (older emits carry the id under those keys — e.g.
    /// `memory.ingested` uses `id`). Exact string match only.
    artifact_id: Option<String>,
    /// SL2 — keep only events whose `payload.slug` equals this slate slug
    /// (`kb slate watch`, and the SPA board's own filtered subscription).
    /// Events without a `slug` field are dropped, exactly as `kb:` drops
    /// `metrics.tick`. `run:`/`kb:`/`artifact:` are untouched.
    slug: Option<String>,
}

fn parse_payload_filter(raw: Option<&str>) -> PayloadFilter {
    let mut out = PayloadFilter::default();
    if let Some(s) = raw {
        for token in s.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if let Some(run) = token.strip_prefix("run:") {
                out.run_id = Some(run.to_string());
            } else if let Some(kb) = token.strip_prefix("kb:") {
                out.kb = Some(kb.to_string());
            } else if let Some(artifact) = token.strip_prefix("artifact:") {
                out.artifact_id = Some(artifact.to_string());
            } else if let Some(slug) = token.strip_prefix("slug:") {
                out.slug = Some(slug.to_string());
            }
        }
    }
    out
}

fn payload_matches(env: &Envelope, filter: &PayloadFilter) -> bool {
    if let Some(want) = &filter.run_id {
        match env.payload.get("run").and_then(|v| v.as_str()) {
            Some(have) if have == want => {}
            _ => return false,
        }
    }
    if let Some(want) = &filter.kb {
        match env.payload.get("kb").and_then(|v| v.as_str()) {
            Some(have) if have == want => {}
            _ => return false,
        }
    }
    if let Some(want) = &filter.slug {
        match env.payload.get("slug").and_then(|v| v.as_str()) {
            Some(have) if have == want => {}
            _ => return false,
        }
    }
    if let Some(want) = &filter.artifact_id {
        let referenced = ["artifact_id", "hash", "id"].iter().any(|key| {
            env.payload
                .get(key)
                .and_then(|v| v.as_str())
                .is_some_and(|have| have == want)
        });
        if !referenced {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_filter_match_all_when_unset() {
        let f = parse_type_filters(None);
        assert!(type_matches("anything", &f));
    }

    #[test]
    fn type_filter_exact() {
        let f = parse_type_filters(Some("error,query"));
        assert!(type_matches("error", &f));
        assert!(type_matches("query", &f));
        assert!(!type_matches("index.start", &f));
    }

    #[test]
    fn type_filter_prefix_glob() {
        let f = parse_type_filters(Some("index.*,error"));
        assert!(type_matches("index.start", &f));
        assert!(type_matches("index.file", &f));
        assert!(type_matches("index.complete", &f));
        assert!(type_matches("error", &f));
        assert!(!type_matches("query", &f));
    }

    #[test]
    fn envelope_body_matches_full_serialization() {
        // The memoized fast path must stay byte-identical to serializing the
        // whole {v, ts, payload} wrapper in one serde_json call (the pre-memo
        // wire format the SPA's SharedWorker parses — invariant #24).
        let payloads = vec![
            serde_json::json!({}),
            serde_json::json!(null),
            serde_json::json!({"run": "r-abc123", "kb": "smoke", "n": 3}),
            serde_json::json!({"nested": {"a": [1, 2, {"b": "c"}], "f": 1.5}, "t": true}),
            serde_json::json!({"unicode": "héllo — ✓ 日本語", "quote": "a\"b\\c\nd"}),
            serde_json::json!([1, "two", null]),
        ];
        for payload in payloads {
            let mut env = Envelope::new("index.start", payload);
            env.id = 42; // id is on the SSE `id:` line, never in the body
            let old = serde_json::to_string(&serde_json::json!({
                "v": env.v,
                "ts": env.ts.to_rfc3339(),
                "payload": env.payload,
            }))
            .unwrap();
            assert_eq!(envelope_body(&env), old);
        }
    }

    #[test]
    fn payload_filter_run_id() {
        let f = parse_payload_filter(Some("run:r-abc123"));
        let env = Envelope::new(
            "index.start",
            serde_json::json!({"run": "r-abc123", "kb": "smoke"}),
        );
        assert!(payload_matches(&env, &f));

        let env_other = Envelope::new(
            "index.start",
            serde_json::json!({"run": "r-other", "kb": "smoke"}),
        );
        assert!(!payload_matches(&env_other, &f));
    }

    #[test]
    fn payload_filter_kb() {
        // v0.24 T1 — `kb:<name>` keeps only that kb's events; events
        // without a `kb` field (metrics.tick) are dropped too.
        let f = parse_payload_filter(Some("kb:alpha"));
        let alpha = Envelope::new(
            "artifact.indexed",
            serde_json::json!({"kb": "alpha", "path": "a.html"}),
        );
        assert!(payload_matches(&alpha, &f));

        let beta = Envelope::new(
            "artifact.indexed",
            serde_json::json!({"kb": "beta", "path": "b.html"}),
        );
        assert!(!payload_matches(&beta, &f));

        let no_kb = Envelope::new("metrics.tick", serde_json::json!({"requests_total": 3}));
        assert!(!payload_matches(&no_kb, &f));
    }

    #[test]
    fn payload_filter_artifact_matches_id_hash_and_id_keys() {
        let f = parse_payload_filter(Some("artifact:abc123def456"));
        // Canonical key.
        let by_artifact_id = Envelope::new(
            "comments.updated",
            serde_json::json!({"kb": "smoke", "artifact_id": "abc123def456", "open_count": 1}),
        );
        assert!(payload_matches(&by_artifact_id, &f));
        // `hash` fallback (artifact.indexed carries the id there too).
        let by_hash = Envelope::new(
            "artifact.indexed",
            serde_json::json!({"kb": "smoke", "path": "a.html", "hash": "abc123def456"}),
        );
        assert!(payload_matches(&by_hash, &f));
        // `id` fallback (memory.* events).
        let by_id = Envelope::new(
            "memory.ingested",
            serde_json::json!({"kb": "memory", "id": "abc123def456", "path": "m.md"}),
        );
        assert!(payload_matches(&by_id, &f));
        // A different artifact never matches.
        let other = Envelope::new(
            "comments.updated",
            serde_json::json!({"kb": "smoke", "artifact_id": "000000000000"}),
        );
        assert!(!payload_matches(&other, &f));
    }

    #[test]
    fn payload_filter_tokens_and_together() {
        // `kb:` + `artifact:` combine conjunctively — both must hold.
        let f = parse_payload_filter(Some("kb:alpha,artifact:abc123def456"));
        let both = Envelope::new(
            "comments.updated",
            serde_json::json!({"kb": "alpha", "artifact_id": "abc123def456"}),
        );
        assert!(payload_matches(&both, &f));
        let wrong_kb = Envelope::new(
            "comments.updated",
            serde_json::json!({"kb": "beta", "artifact_id": "abc123def456"}),
        );
        assert!(!payload_matches(&wrong_kb, &f));
        let wrong_artifact = Envelope::new(
            "comments.updated",
            serde_json::json!({"kb": "alpha", "artifact_id": "000000000000"}),
        );
        assert!(!payload_matches(&wrong_artifact, &f));
    }
}
