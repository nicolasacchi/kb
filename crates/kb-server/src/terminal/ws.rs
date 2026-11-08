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
//! BEFORE `ws.on_upgrade(...)` and fails with a plain HTTP response:
//! enabled (bodiless 404) -> exact Origin via
//! [`check_origin`](crate::terminal::gate::check_origin) (absent fails
//! closed; the loopback bypass does NOT apply — agents run on
//! loopback) -> caller class -> ticket (single-use, bound to
//! `{id}` + the caller's [`Identity`]) -> socket cap (reserve/release,
//! so a cap rejection never burns the ticket) -> `from` parse -> open
//! the upstream stream -> upgrade.
//!
//! The ticket rides in the query string (browsers cannot set headers on
//! a WS handshake). It is consumed at the upgrade (replayed or logged
//! copies are dead), and this module never logs it or the raw URI.

use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::extract::ws::{close_code, CloseFrame, Message, WebSocket};
use axum::{
    body::Bytes,
    extract::{Path, Query, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    response::Response,
    Extension,
};
use futures::{Sink, SinkExt, Stream, StreamExt};
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
    !s.is_empty()
        && s.len() <= 16
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
    {
        let counter = &state.terminals.sockets;
        match reserve_slot(counter) {
            Some(_) => {}
            None => return socket_cap(),
        }
    }
    if !consume_ticket(&state, &q.ticket, &id, &identity.user) {
        state.terminals.sockets.fetch_sub(1, Ordering::SeqCst);
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
            state.terminals.sockets.fetch_sub(1, Ordering::SeqCst);
            return Problem::response(
                "urn:kb:errors:terminal-not-found",
                StatusCode::NOT_FOUND,
                "Not Found",
                "no such terminal".to_string(),
            );
        }
        Err(StreamError::Unavailable) => {
            state.terminals.sockets.fetch_sub(1, Ordering::SeqCst);
            return Problem::response(
                "urn:kb:errors:terminal-runner-unavailable",
                StatusCode::SERVICE_UNAVAILABLE,
                "Service Unavailable",
                "runner unavailable".to_string(),
            );
        }
        Err(StreamError::BadReply) => {
            state.terminals.sockets.fetch_sub(1, Ordering::SeqCst);
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
    let upgraded = ws.max_message_size(256 * 1024).on_upgrade(move |socket| {
        tokio::spawn(async move {
            run_relay(
                socket,
                upstream,
                shutdown,
                RelayTimers::default(),
                |event| {
                    on_relay_event(&state2, &id2, event);
                },
            )
            .await;
            state2.terminals.sockets.fetch_sub(1, Ordering::SeqCst);
            tracing::info!(term = %id2, "terminal ws closed");
        })
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
fn reserve_slot(counter: &AtomicUsize) -> Option<SlotGuard> {
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
            Ok(_) => return Some(SlotGuard { counter }),
            Err(next) => current = next,
        }
    }
}

/// RAII socket-slot guard: the increment is undone on drop. Not
/// `Send`-unsafe: the counter lives until process exit.
struct SlotGuard {
    counter: &'static AtomicUsize,
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
    U: Upstream,
    F: Fn(RelayEvent) + Send + 'static,
    <S as SinkExt<Message>>::Error: std::fmt::Debug,
{
    let (mut sink, mut stream) = sink_stream.split();
    let (slow_tx, mut slow_rx) = mpsc::channel::<()>(1);
    let (upstream_tx, mut upstream_rx) = mpsc::channel::<StreamItem>(64);
    let reader = tokio::spawn(async move {
        let mut up = upstream;
        loop {
            match up.recv().await {
                None => return,
                Some(item) => {
                    match tokio::time::timeout(timers.slow_consumer_after, upstream_tx.send(item))
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
            _ = shutdown.wait_for(|&down| down) => {
                let _ = tokio::time::timeout(Duration::from_millis(500),
                    sink.send(Message::Close(Some(CloseFrame {
                        code: close_code::RESTART.into(),
                        reason: "service restart".into(),
                    })))).await;
                break;
            }
            // Slow consumer signalled by the reader: close 1013.
            _ = slow_rx.recv() => {
                let _ = sink.send(Message::Close(Some(CloseFrame {
                    code: close_code::TRY_AGAIN_LATER.into(),
                    reason: "slow consumer".into(),
                }))).await;
                break;
            }
            // (b) browser -> upstream, verbatim (Pong/Ping refresh
            // last-seen).
            msg = stream.next() => {
                match msg {
                    None => break,
                    Some(Err(_)) => break,
                    Some(Ok(Message::Binary(bytes))) => {
                        last_seen = Instant::now();
                        let _ = upstream_rx.send(StreamItem::Binary(bytes.to_vec())).await;
                    }
                    Some(Ok(Message::Text(text))) => {
                        last_seen = Instant::now();
                        if text.len() <= 64 * 1024 {
                            let _ = upstream_rx.send(StreamItem::Text(text.to_string())).await;
                        }
                    }
                    Some(Ok(Message::Pong(_))) | Some(Ok(Message::Ping(_))) => {
                        last_seen = Instant::now();
                    }
                    Some(Ok(Message::Close(_))) => break,
                }
            }
            // (c) mpsc -> sink (upstream items verbatim, in order).
            item = upstream_rx.recv() => {
                let Some(item) = item else {
                    // Upstream EOF/error: 1011, unless an exit frame was
                    // already flushed (then close 1000).
                    let code = exit_seen.take().map(|_| CLOSE_NORMAL).unwrap_or(CLOSE_UPSTREAM);
                    let reason = if code == CLOSE_NORMAL { "normal" } else { "upstream closed" };
                    let _ = sink.send(Message::Close(Some(CloseFrame {
                        code: (code as u16).into(),
                        reason: reason.into(),
                    }))).await;
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
                        let _ = sink.send(Message::Text(text.clone().into())).await;
                    }
                    StreamItem::Binary(bytes) => {
                        let _ = sink.send(Message::Binary(bytes.clone().into())).await;
                    }
                }
            }
            // (d) ping + idle close.
            _ = ping.tick() => {
                if last_seen.elapsed() >= timers.idle_close_after {
                    let _ = sink.send(Message::Close(Some(CloseFrame {
                        code: close_code::NORMAL.into(),
                        reason: "idle".into(),
                    }))).await;
                    break;
                }
                let _ = sink.send(Message::Ping(Bytes::new())).await;
            }
        }
    }
    // Browser close/shutdown: drop the upstream (the runner tears down
    // this viewer's tap connection) and let the reader task end.
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
    Some(v.get("code").and_then(|c| c.as_i64()).map(|c| c as i32))
}

fn parse_sizechanged(text: &str) -> Option<(u16, u16)> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    if v.get("t").and_then(|t| t.as_str()) != Some("sizechanged") {
        return None;
    }
    let cols = v.get("cols").and_then(|c| c.as_u64())? as u16;
    let rows = v.get("rows").and_then(|c| c.as_u64())? as u16;
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
    /// browser->upstream bytes.
    struct MemUpstream {
        items: std::collections::VecDeque<StreamItem>,
        sent: std::sync::Arc<std::sync::Mutex<Vec<StreamItem>>>,
        block: bool,
    }

    impl Upstream for MemUpstream {
        fn recv<'a>(&'a mut self) -> Pin<Box<dyn Future<Output = Option<StreamItem>> + Send + 'a>> {
            // Take one item if queued; with `block`, park forever (the
            // relay's shutdown/idle arms end the test).
            if let Some(item) = self.items.pop_front() {
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

    /// The in-memory sink end: start_send never blocks and never fails
    /// (a try_send onto the test's drain channel records everything;
    /// the test stops reading when it has seen what it needs).
    struct MemSinkStream {
        to_test: mpsc::Sender<Message>,
        from_test: mpsc::Receiver<Result<Message, axum::Error>>,
    }

    impl Stream for MemSinkStream {
        type Item = Result<Message, axum::Error>;
        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            self.get_mut().from_test.poll_recv(cx)
        }
    }

    impl Sink<Message> for MemSinkStream {
        type Error = std::convert::Infallible;
        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
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
        let (to_test, out_rx) = mpsc::channel::<Message>(1024);
        let (to_relay, from_relay) = mpsc::channel::<Result<Message, axum::Error>>(64);
        let mut out = out_rx;
        let _into = to_relay;
        let sink = MemSinkStream {
            to_test,
            from_test: from_relay,
        };
        let upstream = MemUpstream {
            items: (0..500)
                .map(|_| StreamItem::Binary(b"x".to_vec()))
                .collect(),
            sent: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            block: false,
        };
        let (_tx, shutdown) = watch::channel(false);
        let timers = RelayTimers {
            ping_every: Duration::from_secs(25),
            idle_close_after: Duration::from_secs(75),
            slow_consumer_after: Duration::from_millis(50),
        };
        // The test never reads `out` for the first burst: the relay's
        // mpsc (cap 64) fills, the reader blocks on send_timeout, then
        // the writer closes 1013. Read AFTER the flood so the close
        // must already be queued.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let relay = tokio::spawn(async move {
            run_relay(sink, upstream, shutdown, timers, |_| {}).await;
        });
        let mut saw_1013 = false;
        while !saw_1013 {
            match tokio::time::timeout(Duration::from_secs(5), out.recv()).await {
                Err(_) => break,
                Ok(None) => break,
                Ok(Some(Message::Close(Some(frame)))) => {
                    if frame.code.as_u16() == CLOSE_SLOW_CONSUMER {
                        saw_1013 = true;
                    }
                }
                Ok(Some(_)) => {}
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), relay).await;
        assert!(saw_1013, "a flooded, undrained sink must close 1013");
    }

    #[tokio::test]
    async fn pings_arrive_and_silent_peer_closes_1001() {
        let (to_test, out_rx) = mpsc::channel::<Message>(1024);
        let (to_relay, from_relay) = mpsc::channel::<Result<Message, axum::Error>>(64);
        let mut out = out_rx;
        let _into = to_relay;
        let sink = MemSinkStream {
            to_test,
            from_test: from_relay,
        };
        let upstream = MemUpstream {
            items: std::collections::VecDeque::new(),
            sent: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            block: true,
        };
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
        let sink = MemSinkStream {
            to_test,
            from_test: from_relay,
        };
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
        let upstream = MemUpstream {
            items: [
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
            sent: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            block: false,
        };
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
}
