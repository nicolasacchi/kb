//! SSE event firehose. Lifted from `spike-sse` (the broadcast + VecDeque
//! ring + Last-Event-ID dedup pattern, confirmed end-to-end with reqwest-
//! eventsource integration tests). Production differences from the spike:
//!
//! - Carries `Envelope` (the canonical `{v, id, type, ts, payload}` shape
//!   from `kb_core::types`) rather than the spike's ad-hoc `BusEvent`.
//! - Stream construction lives in `events_stream` — pure tokio_stream, no
//!   axum types. The kb-server crate wraps it in `axum::response::sse::Sse`.
//!   This addresses the spike-sse "handler must live in kb-core, not kb-server"
//!   finding without coupling kb-core to a specific HTTP framework version.
//!
//! Defaults from topic 04 §Decisions: ring capacity 1024, broadcast channel
//! capacity 1024 (bumped from 256 in v0.7.x — initial-walk bursts on large
//! corpora overflowed the smaller buffer, silently dropping `watch.*`
//! envelopes for indexer consumers and triggering the user-visible
//! "sometimes I need to reset the index" symptom).

use crate::types::Envelope;
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};
use tokio::sync::broadcast;
use tokio_stream::wrappers::{errors::BroadcastStreamRecvError, BroadcastStream};

pub const DEFAULT_RING_CAPACITY: usize = 1024;
pub const DEFAULT_LIVE_CAPACITY: usize = 1024;

/// Event firehose with a bounded replay buffer. New subscribers can request
/// all events since `Last-Event-ID` via [`Self::snapshot_since`] before
/// chaining the live broadcast stream — the subscribe-first ordering the
/// research recommends to avoid both gaps and dups.
pub struct EventBus {
    ring: Mutex<VecDeque<Envelope>>,
    sender: broadcast::Sender<Envelope>,
    next_id: AtomicU64,
    capacity: usize,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(DEFAULT_RING_CAPACITY, DEFAULT_LIVE_CAPACITY)
    }
}

/// Minimum bus capacity. The startup walk + reconcile can burst this many
/// envelopes; below this an operator override is more likely to *cause* the
/// overflow it was meant to relieve, so we floor it.
pub const MIN_BUS_CAPACITY: usize = 256;

/// Cold-subscriber replay cap. A connection with NO resume cursor
/// (`last_id == 0` — a first connect, or a reconnect after the SSE worker
/// cleared its cursor on a `gap`) gets at most this many recent ring entries,
/// never the full ring. Replaying the whole ring (up to `capacity`) at wire
/// speed is what pegged browser CPUs: a startup walk bursts ~`capacity`
/// `watch.create`/`index.*` envelopes into the ring, and every cursorless
/// (re)connect — including the `gap → clear cursor → reconnect` loop — re-ate
/// all of them. A real resume (`last_id > 0`) still replays the full gap since
/// the cursor; only the cold "show me recent context" path is bounded.
/// Authoritative state is synced out-of-band (the SPA refetches via REST), so
/// this tail is courtesy, not correctness.
pub const COLD_REPLAY_CAP: usize = 64;

/// Event types that are broadcast live but NEVER retained in the replay ring:
/// pure heartbeats whose history is worthless to a reconnecting client.
/// `metrics.tick` is a 1 Hz fleet heartbeat — buffering it evicts real events
/// (index/watch/comments/errors) from the bounded ring and makes every replay
/// mostly stale ticks. Keeping them out keeps the ring meaningful and small.
pub fn is_ephemeral(type_: &str) -> bool {
    type_ == "metrics.tick"
}

/// Resolve the bus capacity from `KB_EVENT_BUS_CAPACITY` (escape hatch for
/// large corpora — see [`EventBus::from_env`]), falling back to the default.
/// An unparseable or too-small value falls back to `MIN_BUS_CAPACITY` rather
/// than failing the daemon boot.
pub fn capacity_from_env() -> usize {
    match std::env::var("KB_EVENT_BUS_CAPACITY") {
        Ok(v) => match v.trim().parse::<usize>() {
            Ok(n) => n.max(MIN_BUS_CAPACITY),
            Err(_) => DEFAULT_LIVE_CAPACITY,
        },
        Err(_) => DEFAULT_LIVE_CAPACITY,
    }
}

impl EventBus {
    /// Build a bus sized from `KB_EVENT_BUS_CAPACITY` (both the live broadcast
    /// channel and the Last-Event-ID replay ring use the resolved value), or
    /// the 1024 default when unset. The escape hatch the deep-review (G-track)
    /// called for: a corpus whose genuinely-changed-file bursts exceed 1024
    /// can raise the ceiling without a code change. Both buffers are sized
    /// together so replay-gap behaviour stays consistent with the live cap.
    pub fn from_env() -> Self {
        let cap = capacity_from_env();
        Self::new(cap, cap)
    }

    pub fn new(ring_capacity: usize, live_capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(live_capacity);
        Self {
            ring: Mutex::new(VecDeque::with_capacity(ring_capacity)),
            sender,
            next_id: AtomicU64::new(1),
            capacity: ring_capacity,
        }
    }

    /// Publish an event. The envelope's `id` field is overwritten with the
    /// monotonic id assigned here. Returns the assigned id.
    pub fn publish(&self, mut env: Envelope) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        env.id = id;
        // Heartbeat/ephemeral events are broadcast LIVE but never retained for
        // replay. `metrics.tick` is a 1 Hz fleet heartbeat carrying only the
        // current snapshot — its history is worthless to a reconnecting
        // client, and at 1/s it otherwise evicts every real event from the
        // ring within ~`capacity` seconds AND dominates every replay. A
        // resuming SharedWorker then re-processed hundreds of stale ticks →
        // per-tick React/query churn → 100% CPU. Keep them out of the ring so
        // a replay is real events only; live delivery is unchanged.
        if !is_ephemeral(&env.type_) {
            let mut ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
            if ring.len() == self.capacity {
                ring.pop_front();
            }
            ring.push_back(env.clone());
        }
        // broadcast::send Err = no subscribers, fine.
        let _ = self.sender.send(env);
        id
    }

    /// Convenience: build + publish in one call.
    pub fn emit(&self, type_: impl Into<String>, payload: serde_json::Value) -> u64 {
        self.publish(Envelope::new(type_, payload))
    }

    /// Snapshot of all ring-buffered events with `id > last_id`. Cheap clone
    /// of envelopes — no allocation beyond the result Vec + the cloned
    /// `serde_json::Value` payloads.
    pub fn snapshot_since(&self, last_id: u64) -> Vec<Envelope> {
        let ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        ring.iter().filter(|e| e.id > last_id).cloned().collect()
    }

    /// The most recent `n` ring entries (oldest-first). Backs the cold
    /// (cursorless) subscriber's bounded replay — see [`COLD_REPLAY_CAP`] and
    /// [`events_stream`]. `n == 0` yields nothing; `n >= ring_len` yields the
    /// whole ring (so a small ring degrades to today's full-replay behaviour).
    pub fn snapshot_tail(&self, n: usize) -> Vec<Envelope> {
        let ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        let skip = ring.len().saturating_sub(n);
        ring.iter().skip(skip).cloned().collect()
    }

    /// Subscribe to the live broadcast stream.
    pub fn subscribe(&self) -> broadcast::Receiver<Envelope> {
        self.sender.subscribe()
    }

    /// Current ring length.
    pub fn ring_len(&self) -> usize {
        self.ring.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Last id assigned. 0 if nothing has been published.
    pub fn last_id(&self) -> u64 {
        self.next_id.load(Ordering::SeqCst).saturating_sub(1)
    }

    /// Oldest id still in the ring. 0 if the ring is empty.
    pub fn oldest_id(&self) -> u64 {
        self.ring
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .front()
            .map(|e| e.id)
            .unwrap_or(0)
    }
}

// --- Stream construction ----------------------------------------------------

/// One frame in the unified replay-then-live stream. The kb-server SSE
/// adapter maps each variant onto an SSE event line; lower-level consumers
/// (TUI subscriber, Playwright probe, future MCP wrapper) can match on it
/// directly.
#[derive(Debug, Clone)]
pub enum EventFrame {
    /// A real published envelope.
    Envelope(Envelope),
    /// Synthetic `event: lag` — the broadcast channel dropped `skipped`
    /// events because this consumer fell behind. Topic 04 §Decisions.
    Lag { skipped: u64 },
    /// Synthetic `event: gap` — the client requested a `last_event_id`
    /// older than the ring's oldest. The client must full-resync (e.g.
    /// reload state from a snapshot endpoint).
    Gap {
        requested_id: u64,
        oldest_available_id: u64,
    },
}

/// Build the canonical replay-then-live stream for a subscriber.
///
/// Ordering: subscribe first → snapshot ring → dedupe live by
/// `id > max(snapshot.id)`. This is the spike-sse confirmed pattern
/// (test `subscribe_then_snapshot_avoids_dups_via_id_filter`).
///
/// Also detects the gap case — if `last_id < oldest_id`, the first yielded
/// frame is `EventFrame::Gap { requested_id: last_id, oldest_available_id }`.
pub fn events_stream(
    bus: &EventBus,
    last_id: u64,
) -> impl futures::Stream<Item = EventFrame> + Send + 'static {
    use futures::stream::{self, StreamExt};

    // Detect gap before subscribing — even if the snapshot is empty, the
    // client deserves to know its requested id is unusable.
    //   Case 1 — the cursor predates the ring's oldest entry: the events
    //     between are already evicted.
    //   Case 2 (v0.7.1 P2) — the cursor is AHEAD of anything this daemon
    //     process has assigned: the daemon restarted (ids start from 1,
    //     the ring is empty), so the client holds a stale cursor from a
    //     previous process and must full-resync. Without this, a
    //     post-restart reconnect got a silent empty stream.
    let oldest = bus.oldest_id();
    let newest = bus.last_id();
    let mut prefix: Vec<EventFrame> = Vec::new();
    let is_gap =
        last_id > 0 && ((oldest > 0 && last_id < oldest.saturating_sub(1)) || last_id > newest);
    if is_gap {
        prefix.push(EventFrame::Gap {
            requested_id: last_id,
            oldest_available_id: oldest,
        });
    }

    let rx = bus.subscribe();
    // Bound what a (re)connecting client replays:
    //  • gap — the cursor is unusable (evicted from the ring, or ahead of a
    //    restarted daemon). The client MUST resync wholesale (the SharedWorker
    //    refetches state via REST on the `resync` signal), so replaying the
    //    now-meaningless ring is pure waste. This was the 100% CPU: a
    //    cross-restart cursor replayed the WHOLE ring (~1k envelopes) on every
    //    connect. Send the Gap frame only, then live.
    //  • cold (no cursor) — a bounded recent tail (`COLD_REPLAY_CAP`).
    //  • real resume (cursor still in the ring) — the full gap since the
    //    cursor; bounded by the ring, and heartbeats are excluded from it
    //    (see `EventBus::publish`), so it's real events only.
    let snapshot = if is_gap {
        Vec::new()
    } else if last_id == 0 {
        bus.snapshot_tail(COLD_REPLAY_CAP)
    } else {
        bus.snapshot_since(last_id)
    };
    let max_snap_id = snapshot.iter().map(|e| e.id).max().unwrap_or(last_id);

    let snapshot_stream = stream::iter(
        prefix
            .into_iter()
            .chain(snapshot.into_iter().map(EventFrame::Envelope)),
    );

    let live_stream = BroadcastStream::new(rx).filter_map(move |result| async move {
        match result {
            Ok(env) if env.id > max_snap_id => Some(EventFrame::Envelope(env)),
            Ok(_) => None, // already delivered via snapshot — dedupe
            Err(BroadcastStreamRecvError::Lagged(n)) => Some(EventFrame::Lag { skipped: n }),
        }
    });

    snapshot_stream.chain(live_stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn capacity_from_env_resolves_override_floor_and_default() {
        // No other test reads KB_EVENT_BUS_CAPACITY, so process-global env
        // mutation here is contained. Restore to unset at the end.
        std::env::remove_var("KB_EVENT_BUS_CAPACITY");
        assert_eq!(
            capacity_from_env(),
            DEFAULT_LIVE_CAPACITY,
            "unset → default"
        );

        std::env::set_var("KB_EVENT_BUS_CAPACITY", "8192");
        assert_eq!(capacity_from_env(), 8192, "valid override honoured");

        std::env::set_var("KB_EVENT_BUS_CAPACITY", "16");
        assert_eq!(
            capacity_from_env(),
            MIN_BUS_CAPACITY,
            "too-small clamps to floor, never below"
        );

        std::env::set_var("KB_EVENT_BUS_CAPACITY", "not-a-number");
        assert_eq!(
            capacity_from_env(),
            DEFAULT_LIVE_CAPACITY,
            "unparseable → default, never a boot failure"
        );

        std::env::remove_var("KB_EVENT_BUS_CAPACITY");
    }

    #[test]
    fn publish_assigns_monotonic_ids() {
        let bus = EventBus::default();
        let id1 = bus.emit("index.start", json!({"run":"r-001"}));
        let id2 = bus.emit("index.file", json!({"run":"r-001","path":"a"}));
        let id3 = bus.emit("index.complete", json!({"run":"r-001"}));
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
        assert_eq!(bus.last_id(), 3);
    }

    #[test]
    fn ring_drops_oldest_when_full() {
        let bus = EventBus::new(3, 8);
        for i in 1..=5 {
            bus.emit("tick", json!({"n": i}));
        }
        assert_eq!(bus.ring_len(), 3);
        let snap = bus.snapshot_since(0);
        assert_eq!(snap.iter().map(|e| e.id).collect::<Vec<_>>(), vec![3, 4, 5]);
    }

    #[test]
    fn snapshot_since_filters_by_id() {
        let bus = EventBus::default();
        for _ in 0..10 {
            bus.emit("tick", json!({}));
        }
        let after_5 = bus.snapshot_since(5);
        assert_eq!(
            after_5.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![6, 7, 8, 9, 10]
        );
        let after_10 = bus.snapshot_since(10);
        assert!(after_10.is_empty());
    }

    #[tokio::test]
    async fn published_clones_share_one_payload_serialization() {
        use std::sync::Arc;

        // `publish` clones the envelope into the ring and into every
        // broadcast receiver — all copies must share the payload_json memo,
        // so M SSE subscribers pay ONE serialization per event, not M.
        let bus = EventBus::default();
        let mut rx = bus.subscribe();
        bus.emit("index.start", json!({"run": "r-1"}));
        let live = rx.recv().await.unwrap();
        let ring = bus.snapshot_since(0).pop().unwrap();
        assert!(
            Arc::ptr_eq(&live.payload_json(), &ring.payload_json()),
            "live and ring copies share the same memoized payload string"
        );
    }

    #[tokio::test]
    async fn subscribe_receives_live_events() {
        let bus = EventBus::default();
        let mut rx = bus.subscribe();
        bus.emit("tick", json!({"k": "v"}));
        let env = rx.recv().await.unwrap();
        assert_eq!(env.id, 1);
        assert_eq!(env.type_, "tick");
    }

    #[tokio::test]
    async fn events_stream_replays_then_lives() {
        use futures::StreamExt;

        let bus = EventBus::default();
        // Pre-populate.
        for _ in 0..3 {
            bus.emit("tick", json!({}));
        }
        // Build stream for client at last_id=0.
        let mut stream = Box::pin(events_stream(&bus, 0));
        // Three replayed envelopes first.
        let mut got_ids = Vec::new();
        for _ in 0..3 {
            match stream.next().await.unwrap() {
                EventFrame::Envelope(env) => got_ids.push(env.id),
                other => panic!("expected envelope, got {other:?}"),
            }
        }
        assert_eq!(got_ids, vec![1, 2, 3]);

        // New event arrives — should appear via live tail.
        let new_id = bus.emit("tick", json!({"n": "live"}));
        assert_eq!(new_id, 4);
        match stream.next().await.unwrap() {
            EventFrame::Envelope(env) => assert_eq!(env.id, 4),
            other => panic!("expected envelope, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn events_stream_dedupes_live_against_snapshot() {
        use futures::StreamExt;

        let bus = EventBus::default();
        bus.emit("tick", json!({}));
        bus.emit("tick", json!({}));
        bus.emit("tick", json!({}));

        // Subscribe + snapshot. Without dedup, the live stream would emit
        // events 1,2,3 again as broadcast catches them.
        let mut stream = Box::pin(events_stream(&bus, 0));

        // Replay frames first.
        for expected in 1..=3 {
            match stream.next().await.unwrap() {
                EventFrame::Envelope(env) => assert_eq!(env.id, expected),
                other => panic!("expected envelope, got {other:?}"),
            }
        }

        // Publish #4 — should be the next frame, not a re-emission of #1-3.
        bus.emit("tick", json!({"new": true}));
        match stream.next().await.unwrap() {
            EventFrame::Envelope(env) => assert_eq!(env.id, 4),
            other => panic!("expected envelope, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cold_subscriber_replay_is_capped_to_recent_tail() {
        use futures::StreamExt;

        // A cursorless (cold) connect must NOT replay the whole ring — only
        // the most recent COLD_REPLAY_CAP entries. This is what stops a
        // startup `watch.create` burst (or any ring-filling event flood) from
        // blasting every fresh browser tab and re-blasting it on every
        // gap-driven reconnect.
        let bus = EventBus::new(1024, 1024);
        let total = (COLD_REPLAY_CAP + 40) as u64;
        for _ in 0..total {
            bus.emit("tick", json!({}));
        }

        let mut stream = Box::pin(events_stream(&bus, 0));
        let mut replayed = Vec::new();
        for _ in 0..COLD_REPLAY_CAP {
            match stream.next().await.unwrap() {
                EventFrame::Envelope(env) => replayed.push(env.id),
                other => panic!("expected envelope, got {other:?}"),
            }
        }
        assert_eq!(replayed.len(), COLD_REPLAY_CAP);
        assert_eq!(
            *replayed.last().unwrap(),
            total,
            "tail ends at the newest id"
        );
        assert_eq!(
            *replayed.first().unwrap(),
            total - COLD_REPLAY_CAP as u64 + 1,
            "tail starts exactly cap-back from newest",
        );
        assert!(
            !replayed.contains(&1),
            "oldest events are dropped from a cold replay"
        );

        // Live events still flow after the capped tail, deduped against it.
        let live_id = bus.emit("tick", json!({"live": true}));
        match stream.next().await.unwrap() {
            EventFrame::Envelope(env) => assert_eq!(env.id, live_id),
            other => panic!("expected live envelope, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resume_with_cursor_replays_full_gap_not_just_tail() {
        use futures::StreamExt;

        // A real resume (last_id > 0) is unaffected by the cold cap: it
        // replays the WHOLE gap since the cursor, so a briefly-disconnected
        // client never silently loses events between the cursor and the tail.
        let bus = EventBus::new(1024, 1024);
        let total = COLD_REPLAY_CAP + 40;
        for _ in 0..total {
            bus.emit("tick", json!({}));
        }
        let mut stream = Box::pin(events_stream(&bus, 1));
        match stream.next().await.unwrap() {
            EventFrame::Envelope(env) => assert_eq!(
                env.id, 2,
                "resume starts right after the cursor, ignoring COLD_REPLAY_CAP",
            ),
            other => panic!("expected envelope, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn heartbeats_are_not_retained_for_replay() {
        // metrics.tick is a live-only heartbeat: broadcast to live subscribers
        // but never buffered in the replay ring, so it can't evict real events
        // or flood a reconnecting client's replay.
        let bus = EventBus::default();
        let r1 = bus.emit("artifact.indexed", json!({ "kb": "k" }));
        for _ in 0..50 {
            bus.emit("metrics.tick", json!({}));
        }
        let r2 = bus.emit("comments.updated", json!({ "artifact_id": "a" }));

        let ring = bus.snapshot_since(0);
        assert_eq!(
            ring.iter().map(|e| e.type_.as_str()).collect::<Vec<_>>(),
            vec!["artifact.indexed", "comments.updated"],
            "ring holds only the real events; 50 heartbeats were dropped",
        );
        assert_eq!(ring.iter().map(|e| e.id).collect::<Vec<_>>(), vec![r1, r2]);
        assert_eq!(bus.ring_len(), 2);
        assert_eq!(
            bus.last_id(),
            r2,
            "ids still advance monotonically across heartbeats"
        );

        // Heartbeats are still delivered LIVE.
        let mut rx = bus.subscribe();
        bus.emit("metrics.tick", json!({ "live": true }));
        assert_eq!(rx.recv().await.unwrap().type_, "metrics.tick");
    }

    #[tokio::test]
    async fn gap_sends_the_frame_but_replays_nothing() {
        use futures::StreamExt;

        // On a gap the client must resync wholesale (out-of-band via REST), so
        // the stream must NOT also replay the stale ring — that full-ring
        // replay on a cross-restart cursor was the SPA 100% CPU.
        let bus = EventBus::new(3, 8);
        for _ in 0..10 {
            bus.emit("watch.modify", json!({}));
        }
        assert_eq!(bus.oldest_id(), 8);

        let mut stream = Box::pin(events_stream(&bus, 2)); // cursor 2 = long evicted → gap
        match stream.next().await.unwrap() {
            EventFrame::Gap { requested_id, .. } => assert_eq!(requested_id, 2),
            other => panic!("expected Gap, got {other:?}"),
        }
        // The next frame is a LIVE event — the ring (ids 8,9,10) is NOT replayed.
        let live = bus.emit("watch.modify", json!({ "live": true }));
        match stream.next().await.unwrap() {
            EventFrame::Envelope(env) => {
                assert_eq!(env.id, live, "gap replays nothing; straight to live")
            }
            other => panic!("expected live envelope, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn events_stream_emits_gap_after_daemon_restart() {
        use futures::StreamExt;

        // v0.7.1 P2 — a client reconnecting after a daemon restart holds
        // a `Last-Event-ID` from the *previous* process; the fresh
        // daemon's ids start from 1, so the client's cursor is ahead of
        // anything this process has assigned. It must get a `Gap` so it
        // full-resyncs, not a silent empty stream.
        let bus = EventBus::default();
        for _ in 0..3 {
            bus.emit("tick", json!({}));
        }
        assert_eq!(bus.last_id(), 3);

        // Client reconnects claiming id 1000 (from before the restart).
        let mut stream = Box::pin(events_stream(&bus, 1000));
        match stream.next().await.unwrap() {
            EventFrame::Gap { requested_id, .. } => assert_eq!(requested_id, 1000),
            other => panic!("expected Gap for an ahead-of-daemon cursor, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn events_stream_emits_gap_when_last_id_too_old() {
        use futures::StreamExt;

        let bus = EventBus::new(3, 8);
        // Push 10 events; ring capacity 3 means oldest stored is id=8.
        for _ in 0..10 {
            bus.emit("tick", json!({}));
        }
        assert_eq!(bus.oldest_id(), 8);

        let mut stream = Box::pin(events_stream(&bus, 2));
        // First frame should be Gap.
        match stream.next().await.unwrap() {
            EventFrame::Gap {
                requested_id,
                oldest_available_id,
            } => {
                assert_eq!(requested_id, 2);
                assert_eq!(oldest_available_id, 8);
            }
            other => panic!("expected Gap, got {other:?}"),
        }
        // No ring replay after a gap — the client must resync wholesale
        // (b90a5be9: replaying the now-meaningless ring was pure waste).
        // The very next frame is live-only: emit one and expect exactly it.
        bus.emit("tick", json!({}));
        match stream.next().await.unwrap() {
            EventFrame::Envelope(env) => assert_eq!(env.id, 11),
            other => panic!("expected the live envelope after Gap, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn lag_event_emitted_when_consumer_falls_behind() {
        use futures::StreamExt;

        let bus = EventBus::new(1024, 4); // tiny live channel
        let mut stream = Box::pin(events_stream(&bus, 0));

        // Publish more than the live channel can buffer without consumer.
        for _ in 0..20 {
            bus.emit("tick", json!({}));
        }

        // Drain frames; somewhere in the stream we should see a Lag.
        let mut saw_lag = false;
        for _ in 0..10 {
            match tokio::time::timeout(std::time::Duration::from_millis(100), stream.next()).await {
                Ok(Some(EventFrame::Lag { skipped })) => {
                    assert!(skipped > 0);
                    saw_lag = true;
                    break;
                }
                Ok(Some(_)) => continue,
                _ => break,
            }
        }
        assert!(saw_lag, "expected at least one Lag frame");
    }
}
