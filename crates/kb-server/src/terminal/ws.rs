//! The terminal WebSocket relay (`GET /api/terms/{id}/ws`, T12).
//!
//! kb-server is a thin, dumb, authenticated pipe between the browser
//! and the host-side runner/tap: it opens the upstream `stream` RPC
//! connection (see [`runner_client`](crate::terminal::runner_client))
//! BEFORE upgrading, then relays frames byte-for-byte. Server->browser
//! binary frames are the tap's raw frame bytes (8-byte BE offset
//! header + payload) — kb-server never re-chunks, re-orders, coalesces
//! or inspects the payload. Text frames are relayed verbatim, in
//! order, interleaved with binary frames (ordering is load-bearing).
//!
//! # Handler-side gate
//!
//! The WS upgrade is a `GET`, so `origin_allowlist` (which skips
//! non-state-changing methods) checks nothing. Every check below runs
//! BEFORE `ws.on_upgrade(...)` and fails with a plain HTTP response,
//! in this order:
//! id shape (400) -> enabled (bodiless 404) -> exact Origin via
//! [`check_origin`](crate::terminal::gate::check_origin) (absent fails
//! closed; the loopback bypass does NOT apply — agents run on
//! loopback) -> caller class (403) -> `from` parse (400) -> socket
//! cap (reserve/release, 503) -> ticket consume (single-use, bound to
//! `{id}` + the caller's [`Identity`], 403) -> open the upstream
//! stream (404 no such terminal, 502 runner error) -> upgrade.
//! Neither a cap rejection nor a malformed `from` burns the ticket
//! (both run before the consume). A stream-open failure does consume
//! it: auth runs before any upstream dial, so an unauthenticated
//! probe cannot open runner streams.
//!
//! The ticket rides in the query string (browsers cannot set headers on
//! a WS handshake). It is consumed at the upgrade (replayed or logged
//! copies are dead), and this module never logs it or the raw URI.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::extract::ws::{close_code, CloseFrame, Message};
use axum::{
    body::Bytes,
    extract::{Path, Query, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    response::Response,
    Extension,
};
use futures::{Sink, SinkExt, StreamExt};
use tokio::sync::{mpsc, watch};

use super::events;
use super::gate::{
    caller_class_admitted, check_origin, disabled, Problem, ERR_TERMINAL_ORIGIN_REFUSED,
};
use super::runner_client::{StreamError, StreamItem, TermStream, UnixRunner};
use super::ticket::consume_ticket;
use crate::middleware::Identity;
use crate::state::KbHandles;

/// Daemon-wide live terminal sockets. A const (not a config key) so
/// there is no docs-test burden; over the cap the upgrade refuses 503
/// before consuming the ticket (reserve/release).
pub(crate) const MAX_TERM_SOCKETS: usize = 16;

/// Default relay timers (builder decisions: named consts, production
/// values live here — never `#[cfg(test)]`, because integration tests
/// under `tests/` link the lib without cfg(test)).
pub(crate) const PING_EVERY: Duration = Duration::from_secs(25);
pub(crate) const IDLE_CLOSE_AFTER: Duration = Duration::from_secs(75);
pub(crate) const SLOW_CONSUMER_AFTER: Duration = Duration::from_secs(2);

/// Close codes used by the relay.
pub(crate) const CLOSE_RESTART: u16 = 1012;
pub(crate) const CLOSE_SLOW_CONSUMER: u16 = 1013;
pub(crate) const CLOSE_UPSTREAM: u16 = 1011;
pub(crate) const CLOSE_IDLE: u16 = 1001;
pub(crate) const CLOSE_NORMAL: u16 = 1000;

/// Query parameters on the upgrade. `from` stays a `String` so a parse
/// failure is this handler's 400, never an extractor rejection.
#[derive(Debug, serde::Deserialize)]
pub(crate) struct WsQuery {
    ticket: String,
    from: Option<String>,
}

/// Relay timers (see `PING_EVERY` / `IDLE_CLOSE_AFTER` /
/// `SLOW_CONSUMER_AFTER`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RelayTimers {
    pub ping_every: Duration,
    pub idle_close_after: Duration,
    pub slow_consumer_after: Duration,
}

impl Default for RelayTimers {
    fn default() -> Self {
        Self {
            ping_every: PING_EVERY,
            idle_close_after: IDLE_CLOSE_AFTER,
            slow_consumer_after: SLOW_CONSUMER_AFTER,
        }
    }
}

/// One relay event (the testable core's callback). The production
/// handler passes a closure that emits `term.ended` / `term.state`;
/// unit tests count them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RelayEvent {
    Exit { code: Option<i32> },
    SizeChanged { cols: u16, rows: u16 },
}

/// The upstream stream trait the relay core speaks (so unit tests drive
/// it with an in-memory pair). Implemented for `TermStream` below.
pub(crate) trait Upstream: Send {
    fn recv<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = Option<StreamItem>> + Send + 'a>>;
    fn send<'a>(
        &'a mut self,
        item: StreamItem,
    ) -> Pin<Box<dyn Future<Output = Result<(), StreamError>> + Send + 'a>>;
}

impl Upstream for TermStream {
    fn recv<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = Option<StreamItem>> + Send + 'a>> {
        Box::pin(TermStream::recv(self))
    }
    fn send<'a>(
        &'a mut self,
        item: StreamItem,
    ) -> Pin<Box<dyn Future<Output = Result<(), StreamError>> + Send + 'a>> {
        Box::pin(TermStream::send(self, item))
    }
}

fn is_valid_term_id(s: &str) -> bool {
    // The canonical wire bounds (`kb-term`'s
    // `wire::is_valid_term_id`, also enforced by the runner at its
    // RPC entry): 8..=12 chars from `[a-z0-9]`. A wider local
    // validator would let ids through the WS gate only to fail at
    // the runner as a 502 BadReply instead of this consistent 400.
    (8..=12).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// The one 403 body for every ticket failure (no oracle: unknown,
/// wrong session, wrong identity, expired, replayed are identical).
fn ticket_forbidden() -> Response<axum::body::Body> {
    Problem::response(
        "urn:kb:errors:terminal-ticket-invalid",
        StatusCode::FORBIDDEN,
        "Forbidden",
        "invalid terminal ticket".to_string(),
    )
}

/// The one 503 body for the socket cap.
fn socket_cap() -> Response<axum::body::Body> {
    Problem::response(
        "urn:kb:errors:terminal-too-many-sockets",
        StatusCode::SERVICE_UNAVAILABLE,
        "Service Unavailable",
        "too many terminal sockets".to_string(),
    )
}

/// The upgrade handler. Checks run in the documented order; the
/// shutdown watch is subscribed BEFORE `on_upgrade` so a socket
/// upgraded after shutdown closes immediately with 1012.
pub(crate) async fn upgrade(
    State(state): State<std::sync::Arc<KbHandles>>,
    Path(id): Path<String>,
    Query(q): Query<WsQuery>,
    Extension(identity): Extension<Identity>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response<axum::body::Body> {
    // 0. Id shape (before any state lookup).
    if !is_valid_term_id(&id) {
        return Problem::response(
            "urn:kb:errors:bad-request",
            StatusCode::BAD_REQUEST,
            "Bad Request",
            "invalid terminal id".to_string(),
        );
    }
    // 1. Enabled — bodiless 404 (same fn the REST gate uses).
    {
        let terminal = state.config.read().await.terminal.clone();
        if !terminal.enabled() {
            return disabled();
        }
        // 2. Origin — exact parent origin or same-host (T11's check;
        // absent fails closed). Do NOT reuse `origin_allowed`.
        if check_origin(&headers, &state.origin).is_err() {
            return Problem::response(
                ERR_TERMINAL_ORIGIN_REFUSED,
                StatusCode::FORBIDDEN,
                "Forbidden",
                "terminal routes require an exact parent or same-host Origin".to_string(),
            );
        }
        // 2b. Caller class — token/legacy callers are refused like on
        // REST (the ticket alone is not the gate).
        if !caller_class_admitted(identity.source, terminal.refuse_token_callers()) {
            return Problem::response(
                "urn:kb:errors:terminal-token-refused",
                StatusCode::FORBIDDEN,
                "Forbidden",
                "token-authenticated callers are refused on terminal routes".to_string(),
            );
        }
    }
    // 5. `from` parse (before the ticket + cap steps so a malformed
    // value never burns a single-use ticket nor holds a socket slot).
    let from: Option<u64> = match q.from.as_deref() {
        None => None,
        Some(raw) => match raw.parse::<u64>() {
            Ok(v) => Some(v),
            Err(_) => {
                return Problem::response(
                    "urn:kb:errors:bad-request",
                    StatusCode::BAD_REQUEST,
                    "Bad Request",
                    "invalid `from` offset".to_string(),
                );
            }
        },
    };
    // Socket cap reserve FIRST, then the ticket consume: a cap
    // rejection must not consume the ticket, and any later failure
    // releases the reservation (the guard is dropped when the socket
    // closes).
    let guard = match reserve_slot(&state.terminals.sockets) {
        Some(g) => g,
        None => return socket_cap(),
    };
    if !consume_ticket(&state, &q.ticket, &id, &identity.user) {
        return ticket_forbidden();
    }
    // Open the upstream stream BEFORE upgrading.
    let terminal = state.config.read().await.terminal.clone();
    let runner = UnixRunner::new(
        terminal.resolved_runner_socket(),
        Duration::from_secs(terminal.runner_timeout_secs()),
    );
    let upstream = match runner.open_stream(&id, from).await {
        Ok(stream) => stream,
        Err(StreamError::NotFound(_)) => {
            return Problem::response(
                "urn:kb:errors:terminal-not-found",
                StatusCode::NOT_FOUND,
                "Not Found",
                "no such terminal".to_string(),
            );
        }
        Err(StreamError::Unavailable) => {
            // Spec 1.6: runner unreachable -> 502 (problem+json).
            // (The REST ops keep their urn to 503; this is the
            // stream-open arm only.)
            return Problem::response(
                "urn:kb:errors:terminal-runner-unavailable",
                StatusCode::BAD_GATEWAY,
                "Bad Gateway",
                "runner unavailable".to_string(),
            );
        }
        Err(StreamError::BadReply) => {
            return Problem::response(
                "urn:kb:errors:terminal-bad-reply",
                StatusCode::BAD_GATEWAY,
                "Bad Gateway",
                "bad runner reply".to_string(),
            );
        }
    };
    // Subscribe to shutdown BEFORE the upgrade so a socket upgraded
    // after shutdown is closed at once with 1012 (watch::wait_for
    // resolves immediately when the flag is already true).
    let shutdown = state.shutdown.subscribe();
    let state2 = state.clone();
    let id2 = id.clone();
    let id3 = id2.clone();
    let upgraded = ws
        .max_message_size(256 * 1024)
        .on_upgrade(move |socket| async move {
            run_relay(
                socket,
                upstream,
                shutdown,
                RelayTimers::default(),
                move |event: RelayEvent| {
                    on_relay_event(&state2, &id3, event);
                },
            )
            .await;
            tracing::info!(term = %id2, "terminal ws closed");
            drop(guard);
        });
    tracing::info!(term = %id, "terminal ws upgraded");
    upgraded
}

/// The relay's event callback: `exit` emits `term.ended` (once) and
/// `sizechanged` emits `term.state` (live). All other frames are
/// silent — `term.*` never fires per data frame.
pub(crate) fn on_relay_event(state: &KbHandles, id: &str, event: RelayEvent) {
    match event {
        RelayEvent::Exit { code } => {
            events::emit_ended(state, id, code);
        }
        RelayEvent::SizeChanged { cols, rows } => {
            events::emit_state(state, id, true, Some(cols), Some(rows), None);
        }
    }
}

/// Reserve one socket slot, or `None` when at the cap. The returned
/// guard releases on drop.
fn reserve_slot(counter: &std::sync::Arc<AtomicUsize>) -> Option<SlotGuard> {
    let mut current = counter.load(Ordering::SeqCst);
    loop {
        if current >= MAX_TERM_SOCKETS {
            return None;
        }
        match counter.compare_exchange_weak(
            current,
            current + 1,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => {
                return Some(SlotGuard {
                    counter: std::sync::Arc::clone(counter),
                })
            }
            Err(next) => current = next,
        }
    }
}

/// RAII socket-slot guard: the increment is undone on drop.
struct SlotGuard {
    counter: std::sync::Arc<AtomicUsize>,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The testable relay core: two tasks per socket (reader + biased
/// writer loop) over a generic sink/stream of axum `Message`s, so unit
/// tests drive it with an in-memory pair.
///
/// Close codes: shutdown -> 1012 `service restart`; slow consumer ->
/// 1013 `slow consumer`; upstream EOF/error -> 1011 `upstream
/// closed`, unless an `exit` frame was the last item (flushed, then
/// 1000); idle (no browser frame including Pong for
/// `idle_close_after`) -> 1001.
pub(crate) async fn run_relay<S, U, F>(
    sink_stream: S,
    upstream: U,
    shutdown: watch::Receiver<bool>,
    timers: RelayTimers,
    on_event: F,
) where
    S: SinkExt<Message> + StreamExt<Item = Result<Message, axum::Error>> + Unpin + Send + 'static,
    U: Upstream + 'static,
    F: Fn(RelayEvent) + Send + 'static,
    <S as Sink<Message>>::Error: std::fmt::Debug,
{
    let (mut sink, mut stream) = sink_stream.split();
    let (slow_tx, mut slow_rx) = mpsc::channel::<()>(1);
    let (upstream_tx, mut upstream_rx) = mpsc::channel::<StreamItem>(64);
    let (browser_tx, mut browser_rx) = mpsc::channel::<StreamItem>(64);
    let reader = tokio::spawn(async move {
        let mut up = upstream;
        loop {
            tokio::select! {
                item = up.recv() => {
                    match item {
                        None => return,
                        Some(item) => {
                            match tokio::time::timeout(
                                timers.slow_consumer_after,
                                upstream_tx.send(item),
                            )
                            .await
                            {
                                Err(_) => {
                                    let _ = slow_tx.send(()).await;
                                    return;
                                }
                                Ok(Err(_)) => return,
                                Ok(Ok(())) => {}
                            }
                        }
                    }
                }
                item = browser_rx.recv() => {
                    match item {
                        None => return,
                        Some(item) => {
                            if up.send(item).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            }
        }
    });

    let on_event = std::sync::Arc::new(on_event);
    let mut shutdown = shutdown;
    let mut last_seen = Instant::now();
    let mut ping = tokio::time::interval(timers.ping_every);
    let mut exit_seen: Option<Option<i32>> = None;
    // Discard the first immediate tick (it would fire before any
    // browser frame could have arrived).
    ping.tick().await;

    loop {
        tokio::select! {
            biased;
            // (a) shutdown watch FIRST: close 1012 and return.
            r = shutdown.wait_for(|&down| down) => {
                // `wait_for` resolves to a borrow guard (watch::Ref,
                // !Send). Drop it before the first await so the relay
                // future stays Send (required by spawn and the
                // on_upgrade callback).
                drop(r);
                let _ = tokio::time::timeout(Duration::from_millis(500),
                    sink.send(Message::Close(Some(CloseFrame {
                        code: close_code::RESTART.into(),
                        reason: "service restart".into(),
                    })))).await;
                break;
            }
            // Slow consumer signalled by the reader: close 1013.
            _ = slow_rx.recv() => {
                let _ = tokio::time::timeout(Duration::from_millis(500),
                    sink.send(Message::Close(Some(CloseFrame {
                        code: close_code::AGAIN.into(),
                        reason: "slow consumer".into(),
                    })))).await;
                break;
            }
            // (b) browser -> upstream, verbatim (Pong/Ping refresh
            // last-seen). The channel send is bounded: if the reader
            // is stuck on a dead tap, this arm must not park the
            // whole select (shutdown/slow arms would never run).
            msg = stream.next() => {
                match msg {
                    None => break,
                    Some(Err(_)) => break,
                    Some(Ok(Message::Binary(bytes))) => {
                        last_seen = Instant::now();
                        if tokio::time::timeout(
                            timers.slow_consumer_after,
                            browser_tx.send(StreamItem::Binary(bytes.to_vec())),
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        last_seen = Instant::now();
                        if text.len() <= 64 * 1024 {
                            if tokio::time::timeout(
                                timers.slow_consumer_after,
                                browser_tx.send(StreamItem::Text(text.to_string())),
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Pong(_))) | Some(Ok(Message::Ping(_))) => {
                        last_seen = Instant::now();
                    }
                    Some(Ok(Message::Close(_))) => break,
                }
            }
            // (c) mpsc -> sink (upstream items verbatim, in order).
            // Every sink send is bounded by the same 500 ms write
            // deadline as the shutdown arm: a stalled browser must
            // never park the select where the shutdown arm lives.
            item = upstream_rx.recv() => {
                let Some(item) = item else {
                    // Upstream EOF/error: 1011, unless an exit frame was
                    // already flushed (then close 1000).
                    let code = exit_seen.take().map(|_| CLOSE_NORMAL).unwrap_or(CLOSE_UPSTREAM);
                    let reason = if code == CLOSE_NORMAL { "normal" } else { "upstream closed" };
                    let _ = tokio::time::timeout(Duration::from_millis(500),
                        sink.send(Message::Close(Some(CloseFrame {
                            code: (code as u16).into(),
                            reason: reason.into(),
                        })))).await;
                    break;
                };
                match &item {
                    StreamItem::Text(text) => {
                        if let Some(e) = parse_exit(text) {
                            exit_seen = Some(e);
                            (on_event)(RelayEvent::Exit { code: e });
                        }
                        if let Some((cols, rows)) = parse_sizechanged(text) {
                            (on_event)(RelayEvent::SizeChanged { cols, rows });
                        }
                        let _ = tokio::time::timeout(Duration::from_millis(500),
                            sink.send(Message::Text(text.clone().into()))).await;
                    }
                    StreamItem::Binary(bytes) => {
                        let _ = tokio::time::timeout(Duration::from_millis(500),
                            sink.send(Message::Binary(bytes.clone().into()))).await;
                    }
                }
            }
            // (d) ping + idle close. The idle close is 1001 (going
            // away): the browser is silent, not finished.
            _ = ping.tick() => {
                if last_seen.elapsed() >= timers.idle_close_after {
                    let _ = tokio::time::timeout(Duration::from_millis(500),
                        sink.send(Message::Close(Some(CloseFrame {
                            code: close_code::GOING_AWAY.into(),
                            reason: "idle".into(),
                        })))).await;
                    break;
                }
                let _ = tokio::time::timeout(Duration::from_millis(500),
                    sink.send(Message::Ping(Bytes::new()))).await;
            }
        }
    }
    // Browser close/shutdown: drop the upstream (the runner tears down
    // this viewer's tap connection) and let the reader task end.
    drop(browser_tx);
    drop(upstream_rx);
    let _ = tokio::time::timeout(Duration::from_millis(200), reader).await;
}

// -- exit / sizechanged sniffing (a COPY is parsed; the original is
// -- always forwarded untouched) ------------------------------------

fn parse_exit(text: &str) -> Option<Option<i32>> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    if v.get("t").and_then(|t| t.as_str()) != Some("exit") {
        return None;
    }
    // An out-of-range `code` skips the code (it does not wrap the
    // exit status the SPA displays).
    Some(
        v.get("code")
            .and_then(|c| c.as_i64())
            .and_then(|c| i32::try_from(c).ok()),
    )
}

fn parse_sizechanged(text: &str) -> Option<(u16, u16)> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    if v.get("t").and_then(|t| t.as_str()) != Some("sizechanged") {
        return None;
    }
    // An out-of-range dimension skips the event instead of wrapping
    // the grid the SPA draws (the frame is still forwarded).
    let cols = v
        .get("cols")
        .and_then(|c| c.as_u64())
        .and_then(|c| u16::try_from(c).ok())?;
    let rows = v
        .get("rows")
        .and_then(|c| c.as_u64())
        .and_then(|c| u16::try_from(c).ok())?;
    Some((cols, rows))
}

#[cfg(test)]
mod relay_tests {
    //! The relay core over an in-memory sink/stream pair, with shrunk
    //! timers (never `tokio::time::pause` — the deadlines are
    //! wall-clock so the test pins the real ones).

    use super::*;
    use std::task::{Context, Poll};

    /// An in-memory upstream: a canned queue of items; `send` records
    /// browser->upstream bytes. `recv_count` bounds the reader's pull
    /// rate (the backpressure pin: a relay that drains the queue
    /// instantly is not backpressuring anything).
    struct MemUpstream {
        items: std::collections::VecDeque<StreamItem>,
        sent: std::sync::Arc<std::sync::Mutex<Vec<StreamItem>>>,
        recv_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        block: bool,
    }

    impl MemUpstream {
        fn new(items: std::collections::VecDeque<StreamItem>, block: bool) -> Self {
            Self {
                items,
                sent: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
                recv_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                block,
            }
        }
    }

    impl Upstream for MemUpstream {
        fn recv<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = Option<StreamItem>> + Send + 'a>> {
            // Take one item if queued; with `block`, park forever (the
            // relay's shutdown/idle arms end the test).
            if let Some(item) = self.items.pop_front() {
                self.recv_count
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move { Some(item) })
            } else if self.block {
                Box::pin(async {
                    std::future::pending::<()>().await;
                    None
                })
            } else {
                Box::pin(async move { None })
            }
        }
        fn send<'a>(
            &'a mut self,
            item: StreamItem,
        ) -> Pin<Box<dyn Future<Output = Result<(), StreamError>> + Send + 'a>> {
            let sent = self.sent.clone();
            Box::pin(async move {
                sent.lock().unwrap().push(item);
                Ok(())
            })
        }
    }

    /// The in-memory sink end. Ungated, `start_send` never blocks and
    /// never fails (a try_send onto the test's drain channel records
    /// everything). Gated (`gated: true`), `poll_ready` pends while
    /// the drain channel is full and registers a waker the test taps
    /// after every drain, so a sink nobody drains genuinely stalls
    /// the writer — the slow-consumer pin.
    struct MemSinkStream {
        to_test: mpsc::Sender<Message>,
        from_test: mpsc::Receiver<Result<Message, axum::Error>>,
        gated: bool,
        ready: std::sync::Arc<std::sync::Mutex<Option<std::task::Waker>>>,
    }

    impl MemSinkStream {
        fn new(
            to_test: mpsc::Sender<Message>,
            from_test: mpsc::Receiver<Result<Message, axum::Error>>,
            gated: bool,
        ) -> Self {
            Self {
                to_test,
                from_test,
                gated,
                ready: std::sync::Arc::new(std::sync::Mutex::new(None)),
            }
        }
    }

    impl Stream for MemSinkStream {
        type Item = Result<Message, axum::Error>;
        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            self.get_mut().from_test.poll_recv(cx)
        }
    }

    impl Sink<Message> for MemSinkStream {
        type Error = std::convert::Infallible;
        fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            if !self.gated || self.to_test.capacity() > 0 {
                Poll::Ready(Ok(()))
            } else {
                *self.ready.lock().unwrap() = Some(cx.waker().clone());
                Poll::Pending
            }
        }
        fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
            let _ = self.to_test.try_send(item);
            Ok(())
        }
        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn slow_consumer_closes_1013_and_reader_is_backpressured() {
        // The gated sink has a 4-deep drain: the writer stalls on
        // `poll_ready`, the relay's mpsc (cap 64) fills, the reader
        // blocks on `send_timeout`, and the writer closes 1013. The
        // reader must be backpressured: it pulls only a bounded
        // prefix of the 500-item flood, never the whole queue.
        let (to_test, out_rx) = mpsc::channel::<Message>(4);
        let (to_relay, from_relay) = mpsc::channel::<Result<Message, axum::Error>>(64);
        let mut out = out_rx;
        let _into = to_relay;
        let sink = MemSinkStream::new(to_test, from_relay, true);
        let ready = sink.ready.clone();
        let upstream = MemUpstream::new(
            (0..500)
                .map(|_| StreamItem::Binary(b"x".to_vec()))
                .collect(),
            false,
        );
        let pulled = upstream.recv_count.clone();
        let (_tx, shutdown) = watch::channel(false);
        let timers = RelayTimers {
            ping_every: Duration::from_secs(25),
            idle_close_after: Duration::from_secs(75),
            slow_consumer_after: Duration::from_millis(100),
        };
        // Spawn first, flood second: the pre-spawn sleep used to
        // guarantee the degenerate case (the sink drained instantly
        // and the relay ended via EOF on 1011, never 1013).
        let relay = tokio::spawn(async move {
            run_relay(sink, upstream, shutdown, timers, |_| {}).await;
        });
        let mut saw_1013 = false;
        while !saw_1013 {
            // Drain without awaiting: `try_recv` + thewaker tap
            // keeps the gated sink half-drained (it must progress
            // far enough to deliver the close) while the 500-item
            // flood keeps the reader blocked.
            let mut got = false;
            while let Ok(msg) = out.try_recv() {
                got = true;
                if let Message::Close(Some(frame)) = msg {
                    if frame.code.as_u16() == CLOSE_SLOW_CONSUMER {
                        saw_1013 = true;
                        break;
                    }
                }
            }
            if saw_1013 {
                break;
            }
            if let Some(w) = ready.lock().unwrap().take() {
                w.wake();
            }
            if !got {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            // Bounded: a relay that never signals 1013 fails here.
            assert!(
                pulled.load(std::sync::atomic::Ordering::SeqCst) <= 4 + 64 + 2,
                "the reader must be backpressured at a bounded prefix"
            );
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), relay).await;
        assert!(saw_1013, "a flooded, undrained sink must close 1013");
        assert!(
            pulled.load(std::sync::atomic::Ordering::SeqCst) < 500,
            "the reader must never drain the whole flood"
        );
    }

    #[tokio::test]
    async fn pings_arrive_and_silent_peer_closes_1001() {
        let (to_test, out_rx) = mpsc::channel::<Message>(1024);
        let (to_relay, from_relay) = mpsc::channel::<Result<Message, axum::Error>>(64);
        let mut out = out_rx;
        let _into = to_relay;
        let sink = MemSinkStream::new(to_test, from_relay, false);
        let upstream = MemUpstream::new(std::collections::VecDeque::new(), true);
        let (_tx, shutdown) = watch::channel(false);
        let timers = RelayTimers {
            ping_every: Duration::from_millis(50),
            idle_close_after: Duration::from_millis(250),
            slow_consumer_after: Duration::from_secs(2),
        };
        let relay = tokio::spawn(async move {
            run_relay(sink, upstream, shutdown, timers, |_| {}).await;
        });
        // Never send anything from the browser: expect pings, then 1001.
        let mut pings = 0;
        let mut saw_1001 = false;
        while !saw_1001 {
            match tokio::time::timeout(Duration::from_secs(3), out.recv()).await {
                Err(_) => break,
                Ok(None) => break,
                Ok(Some(Message::Ping(_))) => pings += 1,
                Ok(Some(Message::Close(Some(frame)))) => {
                    if frame.code.as_u16() == CLOSE_IDLE {
                        saw_1001 = true;
                    }
                }
                Ok(Some(_)) => {}
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), relay).await;
        assert!(pings >= 1, "pings must arrive at ping_every");
        assert!(
            saw_1001,
            "a silent peer must close 1001 after idle_close_after"
        );
    }

    #[tokio::test]
    async fn upstream_frames_are_relayed_verbatim_in_order() {
        let (to_test, out_rx) = mpsc::channel::<Message>(1024);
        let (to_relay, from_relay) = mpsc::channel::<Result<Message, axum::Error>>(64);
        let mut out = out_rx;
        let _into = to_relay;
        let sink = MemSinkStream::new(to_test, from_relay, false);
        let binary08 = {
            let mut v = Vec::new();
            v.extend_from_slice(&0u64.to_be_bytes());
            v.extend_from_slice(b"abc");
            v
        };
        let binary3 = {
            let mut v = Vec::new();
            v.extend_from_slice(&3u64.to_be_bytes());
            v.extend_from_slice(b"def");
            v
        };
        let upstream = MemUpstream::new(
            [
                StreamItem::Text(r#"{"t":"hello","proto":"kb-term/1"}"#.to_string()),
                StreamItem::Binary(binary08.clone()),
                StreamItem::Text(
                    r#"{"t":"resize","seq":1,"offset":3,"cols":120,"rows":40}"#.to_string(),
                ),
                StreamItem::Binary(binary3.clone()),
                StreamItem::Text(r#"{"t":"exit","seq":2,"code":0,"offset":6}"#.to_string()),
            ]
            .into_iter()
            .collect(),
            false,
        );
        let (_tx, shutdown) = watch::channel(false);
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let events2 = events.clone();
        let relay = tokio::spawn(async move {
            run_relay(sink, upstream, shutdown, RelayTimers::default(), move |e| {
                events2.lock().unwrap().push(e);
            })
            .await;
        });
        // Read the five frames, then the close (1000 after exit).
        let mut frames: Vec<Message> = Vec::new();
        let mut closed = false;
        while !closed && frames.len() < 10 {
            match tokio::time::timeout(Duration::from_secs(5), out.recv()).await {
                Err(_) => break,
                Ok(None) => break,
                Ok(Some(Message::Close(Some(frame)))) => {
                    assert_eq!(
                        frame.code.as_u16(),
                        CLOSE_NORMAL,
                        "after exit the close is 1000"
                    );
                    closed = true;
                }
                Ok(Some(msg)) => frames.push(msg),
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), relay).await;
        assert_eq!(frames.len(), 5, "five upstream frames, verbatim");
        match &frames[0] {
            Message::Text(t) => assert_eq!(t.as_str(), r#"{"t":"hello","proto":"kb-term/1"}"#),
            other => panic!("frame 0 must be the hello text, got {other:?}"),
        }
        match &frames[1] {
            Message::Binary(b) => assert_eq!(b.as_ref(), &binary08[..]),
            other => panic!("frame 1 must be the binary, got {other:?}"),
        }
        match &frames[2] {
            Message::Text(t) => assert!(t.contains("resize"), "frame 2 is the resize text"),
            other => panic!("frame 2 must be the resize text, got {other:?}"),
        }
        match &frames[3] {
            Message::Binary(b) => assert_eq!(b.as_ref(), &binary3[..]),
            other => panic!("frame 3 must be the second binary, got {other:?}"),
        }
        match &frames[4] {
            Message::Text(t) => {
                assert_eq!(t.as_str(), r#"{"t":"exit","seq":2,"code":0,"offset":6}"#)
            }
            other => panic!("frame 4 must be the exit text, got {other:?}"),
        }
        let ev = events.lock().unwrap();
        assert_eq!(ev.len(), 1, "exactly one exit event (term.ended)");
        assert_eq!(ev[0], RelayEvent::Exit { code: Some(0) });
    }

    #[test]
    fn close_codes_are_the_spec_numbers() {
        assert_eq!(CLOSE_RESTART, 1012);
        assert_eq!(CLOSE_SLOW_CONSUMER, 1013);
        assert_eq!(CLOSE_UPSTREAM, 1011);
        assert_eq!(CLOSE_IDLE, 1001);
        assert_eq!(CLOSE_NORMAL, 1000);
    }

    #[test]
    fn term_id_validator_mirrors_the_canonical_bounds() {
        // The WS gate must reject exactly what the runner would:
        // lengths 7 and 13 (and 1..=16 in the old local validator)
        // are 400s here, not 502s at the runner.
        assert!(!super::is_valid_term_id("abcd123"), "len 7");
        assert!(super::is_valid_term_id("abcd1234"), "len 8");
        assert!(super::is_valid_term_id("abcd1234wxyz"), "len 12");
        assert!(!super::is_valid_term_id("abcd1234wxyz1"), "len 13");
        assert!(!super::is_valid_term_id("ABCD1234"), "uppercase");
        assert!(!super::is_valid_term_id(""), "empty");
        assert!(!super::is_valid_term_id("abcd 1234"), "space");
    }

    #[test]
    fn oversize_dimensions_and_codes_skip_the_event_not_the_frame() {
        assert_eq!(
            super::parse_sizechanged(r#"{"t":"sizechanged","cols":70000,"rows":40}"#),
            None,
            "cols > u16::MAX must skip, not wrap to 4464"
        );
        assert_eq!(
            super::parse_sizechanged(r#"{"t":"sizechanged","cols":65535,"rows":40}"#),
            Some((65535, 40))
        );
        assert_eq!(
            super::parse_exit(r#"{"t":"exit","code":30000000000}"#),
            Some(None),
            "an out-of-range exit code skips the code, not the exit"
        );
        assert_eq!(
            super::parse_exit(r#"{"t":"exit","code":-1}"#),
            Some(Some(-1))
        );
    }

    #[tokio::test]
    async fn upstream_eof_without_exit_closes_1011() {
        let (to_test, out_rx) = mpsc::channel::<Message>(1024);
        let (to_relay, from_relay) = mpsc::channel::<Result<Message, axum::Error>>(64);
        let mut out = out_rx;
        let _into = to_relay;
        let sink = MemSinkStream::new(to_test, from_relay, false);
        let upstream = MemUpstream::new(
            {
                let mut v = std::collections::VecDeque::new();
                v.push_back(StreamItem::Binary(vec![0u8; 8]));
                v
            },
            false,
        );
        let (_tx, shutdown) = watch::channel(false);
        let relay = tokio::spawn(async move {
            run_relay(sink, upstream, shutdown, RelayTimers::default(), |_| {}).await;
        });
        // One binary frame, then EOF with no exit: 1011.
        match tokio::time::timeout(Duration::from_secs(5), out.recv()).await {
            Ok(Some(Message::Binary(_))) => {}
            other => panic!("the binary must relay first, got {other:?}"),
        }
        match tokio::time::timeout(Duration::from_secs(5), out.recv()).await {
            Ok(Some(Message::Close(Some(frame)))) => {
                assert_eq!(
                    frame.code.as_u16(),
                    CLOSE_UPSTREAM,
                    "EOF without exit is 1011"
                );
                assert_eq!(frame.reason.as_str(), "upstream closed");
            }
            other => panic!("EOF must close 1011, got {other:?}"),
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), relay).await;
    }

    #[tokio::test]
    async fn sizechanged_emits_one_live_state_and_data_emits_none() {
        let (to_test, out_rx) = mpsc::channel::<Message>(1024);
        let (to_relay, from_relay) = mpsc::channel::<Result<Message, axum::Error>>(64);
        let mut out = out_rx;
        let _into = to_relay;
        let sink = MemSinkStream::new(to_test, from_relay, false);
        let upstream = MemUpstream::new(
            [
                StreamItem::Binary(vec![0u8; 8]),
                StreamItem::Text(r#"{"t":"sizechanged","cols":120,"rows":40}"#.to_string()),
                StreamItem::Binary(vec![8u8; 8]),
            ]
            .into_iter()
            .collect(),
            false,
        );
        let (_tx, shutdown) = watch::channel(false);
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let events2 = events.clone();
        let relay = tokio::spawn(async move {
            run_relay(sink, upstream, shutdown, RelayTimers::default(), move |e| {
                events2.lock().unwrap().push(e);
            })
            .await;
        });
        // Three frames relayed, then EOF (no exit) -> 1011 close.
        let mut frames = 0;
        let mut close_1011 = false;
        while !close_1011 {
            match tokio::time::timeout(Duration::from_secs(5), out.recv()).await {
                Err(_) | Ok(None) => break,
                Ok(Some(Message::Close(Some(frame)))) => {
                    if frame.code.as_u16() == CLOSE_UPSTREAM {
                        close_1011 = true;
                    }
                }
                Ok(Some(_)) => frames += 1,
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), relay).await;
        assert_eq!(frames, 3, "all three frames relay verbatim");
        assert!(close_1011, "EOF without exit closes 1011");
        let ev = events.lock().unwrap();
        assert_eq!(
            *ev,
            vec![RelayEvent::SizeChanged {
                cols: 120,
                rows: 40
            }],
            "exactly one live state event; the two binary data frames emit none"
        );
    }

    #[tokio::test]
    async fn oversize_browser_text_is_dropped_not_forwarded() {
        let (to_test, out_rx) = mpsc::channel::<Message>(1024);
        let (to_relay, from_relay) = mpsc::channel::<Result<Message, axum::Error>>(64);
        let mut out = out_rx;
        let sink = MemSinkStream::new(to_test, from_relay, false);
        let upstream = MemUpstream::new(std::collections::VecDeque::new(), true);
        let sent = upstream.sent.clone();
        let (mut tx, shutdown) = watch::channel(false);
        let relay = tokio::spawn(async move {
            run_relay(sink, upstream, shutdown, RelayTimers::default(), |_| {}).await;
        });
        let big = "x".repeat(64 * 1024 + 1);
        to_relay.send(Ok(Message::Text(big.into()))).await.unwrap();
        to_relay
            .send(Ok(Message::Text(r#"{"t":"ping"}"#.into())))
            .await
            .unwrap();
        // Wait until the small frame reaches the upstream (or the
        // deadline says it never will).
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            {
                let got = sent.lock().unwrap().clone();
                if !got.is_empty() {
                    assert_eq!(
                        got,
                        vec![StreamItem::Text(r#"{"t":"ping"}"#.to_string())],
                        "the >64 KiB text must be dropped; the small one forwarded"
                    );
                    break;
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the small text must reach the upstream"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // The relay is still alive: shutdown closes it 1012.
        let _ = tx.send(true);
        match tokio::time::timeout(Duration::from_secs(5), out.recv()).await {
            Ok(Some(Message::Close(Some(frame)))) => {
                assert_eq!(frame.code.as_u16(), CLOSE_RESTART, "shutdown arm preempts");
            }
            other => panic!("shutdown must close 1012, got {other:?}"),
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), relay).await;
    }

    #[tokio::test]
    async fn pong_refreshes_the_idle_deadline() {
        let (to_test, out_rx) = mpsc::channel::<Message>(1024);
        let (to_relay, from_relay) = mpsc::channel::<Result<Message, axum::Error>>(64);
        let mut out = out_rx;
        let sink = MemSinkStream::new(to_test, from_relay, false);
        let upstream = MemUpstream::new(std::collections::VecDeque::new(), true);
        let (mut tx, shutdown) = watch::channel(false);
        let timers = RelayTimers {
            ping_every: Duration::from_millis(50),
            idle_close_after: Duration::from_millis(250),
            slow_consumer_after: Duration::from_secs(2),
        };
        let relay = tokio::spawn(async move {
            run_relay(sink, upstream, shutdown, timers, |_| {}).await;
        });
        // Pong every 40 ms for longer than the idle deadline: the
        // peer must stay alive (no 1001) because Pong refreshes
        // last-seen.
        let mut saw_close = false;
        let mut saw_ping = false;
        for _ in 0..16 {
            to_relay
                .send(Ok(Message::Pong(Bytes::new())))
                .await
                .unwrap();
            while let Ok(msg) = out.try_recv() {
                match msg {
                    Message::Ping(_) => saw_ping = true,
                    Message::Close(_) => saw_close = true,
                    _ => {}
                }
            }
            assert!(
                !saw_close,
                "Pong must keep the peer alive past idle_close_after"
            );
            tokio::time::sleep(Duration::from_millis(40)).await;
            while let Ok(msg) = out.try_recv() {
                match msg {
                    Message::Ping(_) => saw_ping = true,
                    Message::Close(_) => saw_close = true,
                    _ => {}
                }
            }
            assert!(
                !saw_close,
                "Pong must keep the peer alive past idle_close_after"
            );
        }
        assert!(saw_ping, "relay pings must arrive during the keep-alive");
        let _ = tx.send(true);
        match tokio::time::timeout(Duration::from_secs(5), out.recv()).await {
            Ok(Some(Message::Close(Some(frame)))) => {
                assert_eq!(
                    frame.code.as_u16(),
                    CLOSE_RESTART,
                    "relay alive until shutdown"
                );
            }
            other => panic!("shutdown must close 1012, got {other:?}"),
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), relay).await;
    }

    #[tokio::test]
    async fn shutdown_already_true_closes_1012_immediately() {
        // Spec 8 #12 twin, pinned deterministically at the relay core:
        // a socket whose shutdown watch is ALREADY true closes at
        // once with 1012 (watch::wait_for resolves immediately). The
        // integration form (POST /api/shutdown, then a fresh TCP
        // connect) is a race by construction — axum's graceful
        // shutdown stops accepting new connections the moment the
        // signal fires — so the mechanism is pinned here and the
        // live form in `ws_shutdown_sends_close_1012_and_drain_finishes_fast`.
        let (to_test, out_rx) = mpsc::channel::<Message>(1024);
        let (to_relay, from_relay) = mpsc::channel::<Result<Message, axum::Error>>(64);
        let mut out = out_rx;
        let _into = to_relay;
        let sink = MemSinkStream::new(to_test, from_relay, false);
        let upstream = MemUpstream::new(std::collections::VecDeque::new(), true);
        let (tx, shutdown) = watch::channel(true);
        let relay = tokio::spawn(async move {
            run_relay(sink, upstream, shutdown, RelayTimers::default(), |_| {}).await;
        });
        match tokio::time::timeout(Duration::from_secs(5), out.recv()).await {
            Ok(Some(Message::Close(Some(frame)))) => {
                assert_eq!(
                    frame.code.as_u16(),
                    CLOSE_RESTART,
                    "already-shutdown closes 1012"
                );
                assert_eq!(frame.reason.as_str(), "service restart");
            }
            other => panic!("already-shutdown must close 1012 at once, got {other:?}"),
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), relay).await;
        drop(tx);
    }
}
