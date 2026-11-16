//! The stream splice (kb-server's WebSocket upstream).
//!
//! The `stream` RPC never returns as an ordinary RPC: the server
//! answers the request line with the standard
//! `{"status":"ok","data":{...}}` envelope, then splices tap.sock
//! envelopes verbatim on the SAME connection in both directions until
//! either side closes (see [`splice`]).
//!
//! Both halves use ONE framing — the tap.sock envelope codec in
//! [`crate::wire`] (`[u32 BE length][u8 kind][body]`, kinds text /
//! binary / control) — never a second one. The subscribe side speaks
//! T7's join/resume contract: on connect the runner sends one
//! `viewer-open` control envelope (`resume: null` for a full join,
//! `{offset, mseq, epoch}` when the client asked to resume). The
//! runner cannot know the epoch itself: it carries the one the client
//! echoes from its last `hello` (and the newest marker sequence it
//! applied), and the tap decides resume vs full join. A request with no
//! epoch resumes with the empty epoch, which never matches, so it is a
//! full join. Tap envelopes are forwarded verbatim to the client
//! (including any control envelopes the tap replies with).
//!
//! The client-to-tap half is NOT blind (FXINPUT): the tap is a
//! read-only viewer and has no input path, so a client `input` frame
//! (`{"t":"input","data":...}`, wrapped in the stream's own viewer-id
//! prefix) is taken out of the stream here and written, whole and in
//! order, to THIS stream's hidden tmux client (the same per-viewer
//! client the host CLI types through), followed by a `typed` control
//! notification to the tap so its clipboard window opens exactly as it
//! does for the CLI. Every other client frame is re-encoded
//! byte-identically and forwarded to the tap. The hidden client is
//! opened lazily on the first input byte (a watch-only stream never
//! spawns one) and closed when the splice ends.

use std::path::PathBuf;

use std::future::Future;

use crate::runner::{Runner, RunnerError};
use crate::wire::{self, ClientFrame, ControlMessage, Envelope, Profile, ResumeInfo};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The tap socket for `id`: `<run-dir>/s/<id>/tap.sock` — the exact
/// path the tap's `TapServer` binds (its constructor joins the same
/// `s/<session>/tap.sock` components).
pub fn tap_socket_path<S: crate::runner::supervisor::Supervisor>(
    runner: &Runner<S>,
    id: &str,
) -> PathBuf {
    runner.sessions_dir().join(id).join("tap.sock")
}

/// The `viewer-open` control body the runner sends on a fresh tap
/// connection. Pure (no IO) so the join-vs-resume choice is unit-pinned:
/// `None` is a full join; `Some(offset)` carries the resume coordinates
/// the tap may accept or downgrade to a full join. `epoch` is the tap
/// stream epoch the client echoes (absent = the empty epoch, which the
/// tap never matches) and `mseq` the newest marker sequence it applied
/// (absent = 0). Both are ignored without `from`.
pub fn join_envelope(
    viewer: &str,
    from: Option<u64>,
    epoch: Option<&str>,
    mseq: Option<u64>,
) -> Result<Vec<u8>, wire::WireError> {
    let resume = from.map(|offset| ResumeInfo {
        offset,
        mseq: mseq.unwrap_or(0),
        epoch: epoch.unwrap_or_default().to_string(),
    });
    let msg = ControlMessage::ViewerOpen {
        viewer: viewer.to_string(),
        profile: Profile::Desktop,
        resume,
    };
    let body = serde_json::to_vec(&msg).map_err(|_| wire::WireError::MalformedJson)?;
    wire::encode_envelope(wire::ENV_CONTROL, &body)
}

/// Open the tap connection for a stream request and write the join
/// envelope. The connection is returned live (unread) together with
/// the freshly minted viewer id it joined under; the caller owns the
/// splice from here. Every call mints its own viewer id: the tap keys
/// viewers by id, so a shared id would make a second socket silently
/// displace the first.
pub async fn connect_tap<S: crate::runner::supervisor::Supervisor>(
    runner: &Runner<S>,
    id: &str,
    from: Option<u64>,
    epoch: Option<&str>,
    mseq: Option<u64>,
) -> Result<(tokio::net::UnixStream, String), RunnerError> {
    if let Some(e) = epoch {
        if !wire::is_valid_epoch(e) {
            return Err(RunnerError::BadRequest("invalid stream epoch".to_string()));
        }
    }
    // Session-down vs tap-down: a session whose directory is gone is
    // "no such session" (NotFound -> 404 at kb-server); a session
    // that exists but whose tap socket will not connect is an
    // upstream failure (TapTimeout -> 502 at kb-server), never the
    // same 404.
    if !runner.sessions_dir().join(id).exists() {
        return Err(RunnerError::NotFound(format!("no session {id}")));
    }
    let path = tap_socket_path(runner, id);
    let mut tap = tokio::net::UnixStream::connect(&path)
        .await
        .map_err(|e| RunnerError::TapTimeout(format!("tap unavailable for {id}: {e}")))?;
    let viewer = crate::runner::ids::generate_viewer_id()?;
    let join = join_envelope(&viewer, from, epoch, mseq)
        .map_err(|e| RunnerError::Internal(format!("encode join envelope: {e}")))?;
    tap.write_all(&join)
        .await
        .map_err(|e| RunnerError::Internal(format!("write tap join: {e}")))?;
    Ok((tap, viewer))
}

/// Where a client `input` frame's bytes go (the stream's hidden tmux
/// client in production; a recorder in unit tests). Deliveries are
/// awaited one at a time, so input stays ordered.
pub trait InputSink: Send {
    /// Write `bytes` (whole, byte-exact) to the viewer's input path.
    fn deliver(&mut self, bytes: Vec<u8>) -> impl Future<Output = Result<(), RunnerError>> + Send;
}

/// The production [`InputSink`]: the runner's per-viewer hidden client
/// for the stream's own `(session, viewer)` pair, never another.
pub struct HiddenInput<'a, S: crate::runner::supervisor::Supervisor> {
    runner: &'a Runner<S>,
    id: &'a str,
    viewer: &'a str,
}

impl<'a, S: crate::runner::supervisor::Supervisor> HiddenInput<'a, S> {
    /// Input route for stream viewer `viewer` of session `id`. Nothing is
    /// spawned until the first [`InputSink::deliver`].
    pub fn new(runner: &'a Runner<S>, id: &'a str, viewer: &'a str) -> Self {
        Self { runner, id, viewer }
    }

    /// Close this stream's hidden client. Unconditional and idempotent:
    /// a `deliver` future cancelled mid-`viewer_open` (the splice's
    /// `select!` drops it) may have registered the viewer without ever
    /// returning, so no local "opened" flag can be trusted. Closing a
    /// viewer that was never opened is a cheap no-op (no spawn, no
    /// durable write); a session already gone is fine too.
    pub async fn close(&mut self) {
        let _ = self.runner.viewer_close(self.id, self.viewer).await;
    }
}

impl<S: crate::runner::supervisor::Supervisor> InputSink for HiddenInput<'_, S> {
    async fn deliver(&mut self, bytes: Vec<u8>) -> Result<(), RunnerError> {
        if !self.runner.has_viewer(self.id, self.viewer).await {
            if !self.runner.session_live(self.id).await {
                return Err(RunnerError::NoSession(format!("no session {}", self.id)));
            }
            let (cols, rows) = self
                .runner
                .current_grid(self.id)
                .await
                .unwrap_or((self.runner.cfg.default_cols, self.runner.cfg.default_rows));
            self.runner
                .viewer_open(self.id, self.viewer, cols, rows)
                .await?;
        }
        let pump = self
            .runner
            .viewer_pump(self.id, self.viewer)
            .await
            .ok_or_else(|| {
                RunnerError::NoViewer(format!("no viewer `{}` for {}", self.viewer, self.id))
            })?;
        pump.write(bytes).await?;
        // Typing clears a `paste_only` latch, like the `keys` RPC; the
        // durable write only happens when the latch is actually set.
        if self.runner.paste_only(self.id).await {
            self.runner.set_paste_only(self.id, false).await;
        }
        Ok(())
    }
}

/// What the client-to-tap half does with one decoded client envelope.
#[derive(Debug, PartialEq, Eq)]
pub enum ClientRoute {
    /// An `input` frame for this stream's viewer: these bytes go to the
    /// hidden client, never to the tap.
    Input(Vec<u8>),
    /// An `input` frame naming some OTHER viewer: dropped (no input to
    /// another viewer, and not forwarded either).
    Drop,
    /// Anything else: forwarded to the tap unchanged.
    Forward,
}

/// Pure routing decision for one client envelope. Only a text envelope
/// whose S2C-wrapped frame parses as a client `input` is intercepted;
/// every other shape (resumefrom, ping, takesize, binary, control,
/// unparsable) is the tap's to judge, exactly as before.
pub fn classify_client_envelope(env: &Envelope, viewer: &str) -> ClientRoute {
    if env.kind != wire::ENV_TEXT {
        return ClientRoute::Forward;
    }
    let Ok((id, frame)) = wire::parse_s2c_body(&env.body) else {
        return ClientRoute::Forward;
    };
    match serde_json::from_slice::<ClientFrame>(frame) {
        Ok(ClientFrame::Input { data }) => {
            if id == viewer {
                ClientRoute::Input(data.into_bytes())
            } else {
                ClientRoute::Drop
            }
        }
        _ => ClientRoute::Forward,
    }
}

async fn client_to_tap<I: InputSink>(
    mut client_read: tokio::net::unix::OwnedReadHalf,
    mut tap_write: tokio::net::unix::OwnedWriteHalf,
    viewer: &str,
    sink: &mut I,
) -> std::io::Result<()> {
    let invalid = |e: String| std::io::Error::new(std::io::ErrorKind::InvalidData, e);
    let mut decoder = wire::EnvelopeDecoder::new();
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        let n = client_read.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        let envs = decoder
            .push(&chunk[..n])
            .map_err(|e| invalid(e.to_string()))?;
        for env in envs {
            match classify_client_envelope(&env, viewer) {
                ClientRoute::Forward => {
                    let bytes = wire::encode_envelope(env.kind, &env.body)
                        .map_err(|e| invalid(e.to_string()))?;
                    tap_write.write_all(&bytes).await?;
                }
                ClientRoute::Drop => {}
                ClientRoute::Input(bytes) => {
                    if bytes.is_empty() {
                        continue;
                    }
                    sink.deliver(bytes)
                        .await
                        .map_err(|e| std::io::Error::other(e.to_string()))?;
                    let typed = serde_json::to_vec(&ControlMessage::Typed {
                        viewer: viewer.to_string(),
                    })
                    .map_err(|e| invalid(e.to_string()))?;
                    let bytes = wire::encode_envelope(wire::ENV_CONTROL, &typed)
                        .map_err(|e| invalid(e.to_string()))?;
                    tap_write.write_all(&bytes).await?;
                }
            }
        }
    }
}

/// The `typed` control body (`{"t":"typed","viewer":...}`) as one envelope.
/// Pure so the exact bytes are unit-pinned.
pub fn typed_envelope(viewer: &str) -> Result<Vec<u8>, wire::WireError> {
    let body = serde_json::to_vec(&ControlMessage::Typed {
        viewer: viewer.to_string(),
    })
    .map_err(|_| wire::WireError::MalformedJson)?;
    wire::encode_envelope(wire::ENV_CONTROL, &body)
}

/// Tell the tap that `viewer` just typed (T18). The tap keeps one
/// last-typed clock per viewer id across all of its connections, so one
/// short connection that writes a single `typed` envelope and closes is
/// enough: no key content ever crosses here, and the tap ignores an id it
/// does not know. The phone's input travels over REST, not the WebSocket,
/// so this is the only way the tap hears about it. The id must already be
/// validated by the caller.
pub async fn notify_typed<S: crate::runner::supervisor::Supervisor>(
    runner: &Runner<S>,
    id: &str,
    viewer: &str,
) -> Result<(), RunnerError> {
    if !runner.sessions_dir().join(id).exists() {
        return Err(RunnerError::NotFound(format!("no session {id}")));
    }
    let path = tap_socket_path(runner, id);
    let env = typed_envelope(viewer)
        .map_err(|e| RunnerError::Internal(format!("encode typed envelope: {e}")))?;
    let attempt = async {
        let mut tap = tokio::net::UnixStream::connect(&path).await?;
        tap.write_all(&env).await?;
        tap.flush().await
    };
    match tokio::time::timeout(std::time::Duration::from_secs(2), attempt).await {
        Ok(Ok(())) => Ok(()),
        _ => Err(RunnerError::TapTimeout(format!("tap unavailable for {id}"))),
    }
}

/// Splice tap.sock envelopes between the tap connection and the runner
/// client halves until either side closes or errors. Tap-to-client bytes
/// are copied verbatim; client-to-tap envelopes are routed by
/// [`classify_client_envelope`] (input to `sink`, the rest to the tap).
/// Taking halves (not streams) lets the RPC loop hand the client-side
/// halves it already owns after answering the `stream` request line;
/// callers with whole streams should `into_split()` first. `viewer` is
/// the id this stream joined the tap under.
pub async fn splice<I: InputSink>(
    tap_read: tokio::net::unix::OwnedReadHalf,
    tap_write: tokio::net::unix::OwnedWriteHalf,
    client_read: tokio::net::unix::OwnedReadHalf,
    client_write: tokio::net::unix::OwnedWriteHalf,
    viewer: &str,
    sink: &mut I,
) -> std::io::Result<()> {
    let mut tap_read = tap_read;
    let mut client_write = client_write;
    let to_client = tokio::io::copy(&mut tap_read, &mut client_write);
    let to_tap = client_to_tap(client_read, tap_write, viewer, sink);
    // The first half to finish owns the close: select returns when the
    // first completes, dropping the other future — and with it (at
    // return) the sockets, which is how the far side observes the
    // close (a dropped runner client tears down its tap connection,
    // and a dead tap ends the client's stream). try_join would wait
    // for both halves on a clean EOF and hang.
    match tokio::select! {
        r = to_client => r.map(|_| ()),
        r = to_tap => r,
    } {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    //! Join-vs-resume choice and the verbatim splice, over a fake
    //! tap.sock server in a temp dir (no tmux, no tap process). Every
    //! read has a hard deadline so a wedged splice fails fast.

    use super::*;
    use crate::runner::config::RunnerConfig;
    use crate::runner::Runner;
    use crate::wire::{ControlMessage, EnvelopeDecoder};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

    async fn read_chunk(stream: &mut tokio::net::UnixStream) -> Vec<u8> {
        let mut buf = vec![0u8; 8192];
        let n = tokio::time::timeout(DEADLINE, stream.read(&mut buf))
            .await
            .expect("read deadline")
            .expect("read");
        assert!(n > 0, "peer closed before sending");
        buf[..n].to_vec()
    }

    fn viewer_open(bytes: &[u8]) -> (String, Profile) {
        let mut dec = EnvelopeDecoder::new();
        let envs = dec.push(bytes).expect("decode");
        assert_eq!(envs.len(), 1, "exactly one envelope");
        assert_eq!(envs[0].kind, wire::ENV_CONTROL);
        match serde_json::from_slice::<ControlMessage>(&envs[0].body).unwrap() {
            ControlMessage::ViewerOpen {
                viewer, profile, ..
            } => (viewer, profile),
            other => panic!("expected viewer-open, got {other:?}"),
        }
    }

    #[test]
    fn join_envelope_full_join_carries_no_resume() {
        let bytes = join_envelope("abcd1234", None, None, None).expect("encode");
        let (viewer, profile) = viewer_open(&bytes);
        assert_eq!(viewer, "abcd1234");
        assert_eq!(profile, Profile::Desktop);
        // resume must be absent/null for a full join (checked via the
        // decoded body containing `"resume":null`).
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("\"resume\":null"), "full join body: {text}");
    }

    fn resume_of(bytes: &[u8]) -> ResumeInfo {
        let mut dec = EnvelopeDecoder::new();
        let envs = dec.push(bytes).expect("decode");
        match serde_json::from_slice::<ControlMessage>(&envs[0].body).unwrap() {
            ControlMessage::ViewerOpen {
                resume: Some(r), ..
            } => r,
            other => panic!("expected viewer-open with resume, got {other:?}"),
        }
    }

    #[test]
    fn join_envelope_with_from_carries_the_clients_epoch_and_mseq() {
        let bytes =
            join_envelope("abcd1234", Some(1234), Some("0123abcd"), Some(7)).expect("encode");
        let (viewer, profile) = viewer_open(&bytes);
        assert_eq!(viewer, "abcd1234");
        assert_eq!(profile, Profile::Desktop);
        let r = resume_of(&bytes);
        assert_eq!(r.offset, 1234);
        assert_eq!(
            r.mseq, 7,
            "the client's applied marker sequence rides along"
        );
        assert_eq!(r.epoch, "0123abcd", "the client's epoch rides along");
    }

    #[test]
    fn join_envelope_without_an_epoch_keeps_the_empty_epoch_full_join_fallback() {
        let r = resume_of(&join_envelope("abcd1234", Some(1234), None, None).expect("encode"));
        assert_eq!(r.offset, 1234);
        assert_eq!(r.mseq, 0);
        assert!(
            r.epoch.is_empty(),
            "no epoch: the tap's empty-epoch mismatch decides"
        );
    }

    #[test]
    fn join_envelope_ignores_epoch_and_mseq_without_from() {
        let bytes = join_envelope("abcd1234", None, Some("0123abcd"), Some(7)).expect("encode");
        let text = String::from_utf8(bytes[5..].to_vec()).unwrap();
        assert!(text.contains("\"resume\":null"), "full join body: {text}");
    }

    #[tokio::test]
    async fn connect_tap_refuses_a_malformed_epoch_before_dialling() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run");
        std::fs::create_dir_all(run_dir.join("s").join("abcd1234")).unwrap();
        let cfg =
            RunnerConfig::test_config(run_dir, "/bin/true".to_string(), tmp.path().join("conf"));
        let runner = Runner::direct(cfg);
        for bad in ["", "a b", "x\n", &"a".repeat(65)] {
            let err = connect_tap(&runner, "abcd1234", Some(5), Some(bad), None)
                .await
                .unwrap_err();
            assert_eq!(err.code(), "bad-request", "epoch {bad:?}");
        }
    }

    #[tokio::test]
    async fn stream_splice_relays_both_directions_verbatim() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run");
        std::fs::create_dir_all(run_dir.join("s")).unwrap();
        std::fs::create_dir_all(run_dir.join("runner")).unwrap();
        let id = "abcd1234";
        let session_dir = run_dir.join("s").join(id);
        std::fs::create_dir_all(&session_dir).unwrap();
        let fake_path = session_dir.join("tap.sock");
        let listener = tokio::net::UnixListener::bind(&fake_path).unwrap();
        // Fake tap: record the join (handed to the test at once through a
        // oneshot: the echo loop below only ends when the splice closes,
        // which the test triggers AFTER reading the join), then echo
        // every further chunk back.
        let (join_tx, join_rx) = tokio::sync::oneshot::channel::<Vec<u8>>();
        let tap_task = tokio::spawn(async move {
            let (mut conn, _) = tokio::time::timeout(DEADLINE, listener.accept())
                .await
                .expect("accept deadline")
                .expect("accept");
            let join = read_chunk(&mut conn).await;
            let _ = join_tx.send(join);
            let mut buf = vec![0u8; 8192];
            loop {
                let n = match tokio::time::timeout(DEADLINE, conn.read(&mut buf)).await {
                    Ok(Ok(0)) | Err(_) => break,
                    Ok(Ok(n)) => n,
                    Ok(Err(_)) => break,
                };
                if conn.write_all(&buf[..n]).await.is_err() {
                    break;
                }
            }
        });
        let cfg =
            RunnerConfig::test_config(run_dir, "/bin/true".to_string(), tmp.path().join("conf"));
        let runner = Runner::direct(cfg);
        let (tap, minted) =
            tokio::time::timeout(DEADLINE, connect_tap(&runner, id, None, None, None))
                .await
                .expect("connect deadline")
                .expect("connect to fake tap");
        let (client_end, mut test_end) = tokio::net::UnixStream::pair().unwrap();
        let (tap_read, tap_write) = tap.into_split();
        let (client_read, client_write) = client_end.into_split();
        let splice_viewer = minted.clone();
        let splice = tokio::spawn(async move {
            let mut sink = RecordingSink::default();
            splice(
                tap_read,
                tap_write,
                client_read,
                client_write,
                &splice_viewer,
                &mut sink,
            )
            .await
        });
        let join = tokio::time::timeout(DEADLINE, join_rx)
            .await
            .expect("fake tap deadline")
            .expect("fake tap join");
        let (viewer, profile) = viewer_open(&join);
        assert_eq!(viewer, minted, "the join names the minted viewer");
        assert_ne!(viewer, id, "the viewer is never the session id");
        assert_eq!(profile, Profile::Desktop);
        // Client -> tap -> (echo) -> client, byte-for-byte.
        let payload = wire::encode_envelope(wire::ENV_TEXT, b"hello").unwrap();
        test_end.write_all(&payload).await.unwrap();
        let echoed = read_chunk(&mut test_end).await;
        assert_eq!(echoed, payload, "spliced bytes must round-trip verbatim");
        // The shared codec pins the frame contract: an envelope past the
        // cap refuses to encode.
        let too_big = vec![0u8; wire::MAX_ENVELOPE_LEN];
        assert_eq!(
            wire::encode_envelope(0, &too_big),
            Err(wire::WireError::BadEnvelope)
        );
        drop(test_end);
        let _ = tokio::time::timeout(DEADLINE, splice)
            .await
            .expect("splice end deadline");
        let _ = tokio::time::timeout(DEADLINE, tap_task).await;
    }

    #[tokio::test]
    async fn two_stream_opens_join_the_tap_under_distinct_viewers() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run");
        let id = "abcd1234";
        let session_dir = run_dir.join("s").join(id);
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::create_dir_all(run_dir.join("runner")).unwrap();
        let listener = tokio::net::UnixListener::bind(session_dir.join("tap.sock")).unwrap();
        let tap_task = tokio::spawn(async move {
            let mut viewers = Vec::new();
            let mut conns = Vec::new();
            for _ in 0..2 {
                let (mut conn, _) = tokio::time::timeout(DEADLINE, listener.accept())
                    .await
                    .expect("accept deadline")
                    .expect("accept");
                let join = read_chunk(&mut conn).await;
                viewers.push(viewer_open(&join).0);
                conns.push(conn);
            }
            viewers
        });
        let cfg =
            RunnerConfig::test_config(run_dir, "/bin/true".to_string(), tmp.path().join("conf"));
        let runner = Runner::direct(cfg);
        let (_t1, v1) = connect_tap(&runner, id, None, None, None)
            .await
            .expect("first");
        let (_t2, v2) = connect_tap(&runner, id, None, None, None)
            .await
            .expect("second");
        assert_ne!(v1, v2, "each stream open mints its own viewer id");
        let seen = tokio::time::timeout(DEADLINE, tap_task)
            .await
            .expect("tap deadline")
            .unwrap();
        assert_eq!(seen.len(), 2);
        assert_ne!(seen[0], seen[1], "the tap saw two DISTINCT viewer-open ids");
        assert!(crate::viewer::valid_viewer(&seen[0]));
    }

    #[test]
    fn typed_envelope_is_one_control_envelope_naming_the_viewer() {
        let bytes = typed_envelope("ws-abcd1234").expect("encode");
        let mut dec = EnvelopeDecoder::new();
        let envs = dec.push(&bytes).expect("decode");
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].kind, wire::ENV_CONTROL);
        assert_eq!(
            serde_json::from_slice::<ControlMessage>(&envs[0].body).unwrap(),
            ControlMessage::Typed {
                viewer: "ws-abcd1234".to_string()
            }
        );
    }

    #[tokio::test]
    async fn notify_typed_writes_exactly_one_typed_envelope_to_the_tap() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run");
        let id = "abcd1234";
        let session_dir = run_dir.join("s").join(id);
        std::fs::create_dir_all(&session_dir).unwrap();
        let listener = tokio::net::UnixListener::bind(session_dir.join("tap.sock")).unwrap();
        let tap_task = tokio::spawn(async move {
            let (mut conn, _) = tokio::time::timeout(DEADLINE, listener.accept())
                .await
                .expect("accept deadline")
                .expect("accept");
            let mut all = Vec::new();
            tokio::time::timeout(DEADLINE, conn.read_to_end(&mut all))
                .await
                .expect("read deadline")
                .expect("read");
            all
        });
        let cfg =
            RunnerConfig::test_config(run_dir, "/bin/true".to_string(), tmp.path().join("conf"));
        let runner = Runner::direct(cfg);
        notify_typed(&runner, id, "ws-abcd1234")
            .await
            .expect("notify");
        let got = tokio::time::timeout(DEADLINE, tap_task)
            .await
            .expect("tap deadline")
            .unwrap();
        assert_eq!(got, typed_envelope("ws-abcd1234").unwrap());
    }

    #[tokio::test]
    async fn notify_typed_names_an_unknown_session_and_a_silent_tap_apart() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run");
        std::fs::create_dir_all(run_dir.join("s")).unwrap();
        let cfg = RunnerConfig::test_config(
            run_dir.clone(),
            "/bin/true".to_string(),
            tmp.path().join("conf"),
        );
        let runner = Runner::direct(cfg);
        let err = notify_typed(&runner, "abcd1234", "ws-x").await.unwrap_err();
        assert!(matches!(err, RunnerError::NotFound(_)), "got {err:?}");
        std::fs::create_dir_all(run_dir.join("s").join("abcd1234")).unwrap();
        let err = notify_typed(&runner, "abcd1234", "ws-x").await.unwrap_err();
        assert!(matches!(err, RunnerError::TapTimeout(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn connect_tap_distinguishes_unknown_session_from_tap_down() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run");
        std::fs::create_dir_all(run_dir.join("s")).unwrap();
        let cfg = RunnerConfig::test_config(
            run_dir.clone(),
            "/bin/true".to_string(),
            tmp.path().join("conf"),
        );
        let runner = Runner::direct(cfg);
        // No session directory at all: "no such session" (code
        // `not-found`, a 404 at kb-server).
        let err = connect_tap(&runner, "abcd1234", None, None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, RunnerError::NotFound(_)), "got {err:?}");
        // The session directory exists but nothing listens on its
        // tap.sock: an upstream failure (`tap-timeout`, a 502), never
        // the same 404.
        std::fs::create_dir_all(run_dir.join("s").join("abcd1234")).unwrap();
        let err = connect_tap(&runner, "abcd1234", None, None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, RunnerError::TapTimeout(_)), "got {err:?}");
    }

    /// Records every delivery (the stand-in for the hidden client).
    #[derive(Default, Clone)]
    struct RecordingSink {
        got: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
        fail: bool,
    }

    impl InputSink for RecordingSink {
        async fn deliver(&mut self, bytes: Vec<u8>) -> Result<(), RunnerError> {
            if self.fail {
                return Err(RunnerError::BusyPty("client writer busy".to_string()));
            }
            self.got.lock().unwrap().push(bytes);
            Ok(())
        }
    }

    /// A client-to-server text envelope: the browser frame wrapped in
    /// the stream's viewer-id prefix, exactly what `kb-server` writes.
    fn c2s_text(viewer: &str, json: &str) -> Vec<u8> {
        let body = wire::s2c_body(viewer, json.as_bytes()).unwrap();
        wire::encode_envelope(wire::ENV_TEXT, &body).unwrap()
    }

    fn input_json(data: &str) -> String {
        serde_json::to_string(&ClientFrame::Input {
            data: data.to_string(),
        })
        .unwrap()
    }

    fn env_of(bytes: &[u8]) -> Envelope {
        let mut dec = EnvelopeDecoder::new();
        let mut envs = dec.push(bytes).unwrap();
        assert_eq!(envs.len(), 1);
        envs.remove(0)
    }

    #[test]
    fn classify_intercepts_only_this_viewers_input_frames() {
        let v = "ws-aaaa1111bb";
        let input = env_of(&c2s_text(v, &input_json("a\u{1b}[A\u{e9}")));
        assert_eq!(
            classify_client_envelope(&input, v),
            ClientRoute::Input("a\u{1b}[A\u{e9}".as_bytes().to_vec())
        );
        // Another viewer's id prefix: dropped, never delivered, never forwarded.
        assert_eq!(
            classify_client_envelope(&input, "ws-other0000"),
            ClientRoute::Drop
        );
        // Everything else is the tap's to judge.
        for json in [
            r#"{"t":"ping"}"#,
            r#"{"t":"takesize","cols":80,"rows":24}"#,
            r#"{"t":"resumefrom","offset":1,"mseq":0,"epoch":"e"}"#,
            r#"{"t":"input","data":"x","extra":1}"#,
            "not json",
        ] {
            let env = env_of(&c2s_text(v, json));
            assert_eq!(
                classify_client_envelope(&env, v),
                ClientRoute::Forward,
                "{json} must be forwarded to the tap"
            );
        }
        // A binary or control envelope is never input.
        let bin = Envelope {
            kind: wire::ENV_BINARY,
            body: wire::s2c_body(v, input_json("x").as_bytes()).unwrap(),
        };
        assert_eq!(classify_client_envelope(&bin, v), ClientRoute::Forward);
        // An unwrapped body (no id prefix) cannot parse and is forwarded.
        let bare = Envelope {
            kind: wire::ENV_TEXT,
            body: b"hello".to_vec(),
        };
        assert_eq!(classify_client_envelope(&bare, v), ClientRoute::Forward);
    }

    /// Run a splice over socket pairs with `sink`; returns what the tap
    /// side received after the join (decoded) once `client_bytes` were
    /// written and the client closed.
    async fn run_splice<I: InputSink + 'static>(
        viewer: &str,
        client_bytes: Vec<Vec<u8>>,
        mut sink: I,
    ) -> (Vec<Envelope>, std::io::Result<()>, I) {
        let (tap_end, mut tap_peer) = tokio::net::UnixStream::pair().unwrap();
        let (client_end, mut client_peer) = tokio::net::UnixStream::pair().unwrap();
        let (tap_read, tap_write) = tap_end.into_split();
        let (client_read, client_write) = client_end.into_split();
        let v = viewer.to_string();
        let task = tokio::spawn(async move {
            let r = splice(
                tap_read,
                tap_write,
                client_read,
                client_write,
                &v,
                &mut sink,
            )
            .await;
            (r, sink)
        });
        for chunk in client_bytes {
            client_peer.write_all(&chunk).await.unwrap();
        }
        // Closing the client ends the splice (or it already ended on error).
        drop(client_peer);
        let (res, sink) = tokio::time::timeout(DEADLINE, task)
            .await
            .expect("splice end deadline")
            .unwrap();
        // The splice dropped its tap write half: read the tap side to EOF.
        let mut seen = Vec::new();
        let mut buf = vec![0u8; 8192];
        loop {
            match tokio::time::timeout(DEADLINE, tap_peer.read(&mut buf)).await {
                Ok(Ok(0)) | Ok(Err(_)) => break,
                Ok(Ok(n)) => seen.extend_from_slice(&buf[..n]),
                Err(_) => panic!("tap read deadline"),
            }
        }
        let envs = EnvelopeDecoder::new().push(&seen).unwrap();
        (envs, res, sink)
    }

    #[tokio::test]
    async fn input_frames_reach_the_sink_in_order_and_never_the_tap() {
        let v = "ws-aaaa1111bb";
        let sink = RecordingSink::default();
        let got = sink.got.clone();
        // Coalesce several envelopes in ONE write (and split another
        // across two) so the framing, not the read boundaries, decides.
        let mut first = Vec::new();
        first.extend(c2s_text(v, &input_json("a")));
        first.extend(c2s_text(v, r#"{"t":"ping"}"#));
        first.extend(c2s_text(v, &input_json("\u{1b}[200~x\r\n\u{1b}[201~")));
        let second = c2s_text(v, &input_json("\u{e9}\u{3}\u{7f}"));
        let (a, b) = second.split_at(7);
        let other = c2s_text("ws-other0000", &input_json("SPOOF"));
        let resume = c2s_text(v, r#"{"t":"resumefrom","offset":9,"mseq":0,"epoch":"e"}"#);
        let (envs, res, _) = run_splice(
            v,
            vec![first, a.to_vec(), b.to_vec(), other, resume.clone()],
            sink,
        )
        .await;
        assert!(res.is_ok(), "{res:?}");
        let delivered = got.lock().unwrap().clone();
        assert_eq!(
            delivered,
            vec![
                b"a".to_vec(),
                b"\x1b[200~x\r\n\x1b[201~".to_vec(),
                "\u{e9}\u{3}\u{7f}".as_bytes().to_vec(),
            ],
            "input frames arrive whole, byte-exact, in send order; a foreign viewer's frame is dropped"
        );
        // The tap saw: ping, typed, typed... in order, no input frame at all.
        let kinds: Vec<String> = envs
            .iter()
            .map(|e| {
                if e.kind == wire::ENV_CONTROL {
                    match serde_json::from_slice::<ControlMessage>(&e.body).unwrap() {
                        ControlMessage::Typed { viewer } => {
                            assert_eq!(viewer, v);
                            "typed".to_string()
                        }
                        other => panic!("unexpected control {other:?}"),
                    }
                } else {
                    let (_, frame) = wire::parse_s2c_body(&e.body).unwrap();
                    let text = std::str::from_utf8(frame).unwrap();
                    assert!(
                        !text.contains("\"input\""),
                        "input leaked to the tap: {text}"
                    );
                    text.to_string()
                }
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "typed".to_string(), // after "a"
                r#"{"t":"ping"}"#.to_string(),
                "typed".to_string(), // after the paste
                "typed".to_string(), // after the split frame
                r#"{"t":"resumefrom","offset":9,"mseq":0,"epoch":"e"}"#.to_string(),
            ],
            "non-input frames reach the tap byte-identically, interleaved in order"
        );
    }

    #[tokio::test]
    async fn a_failed_delivery_ends_the_splice_and_is_not_silently_dropped() {
        let v = "ws-aaaa1111bb";
        let sink = RecordingSink {
            fail: true,
            ..RecordingSink::default()
        };
        let (envs, res, _) = run_splice(v, vec![c2s_text(v, &input_json("k"))], sink).await;
        assert!(res.is_err(), "a delivery failure must end the splice");
        assert!(
            envs.is_empty(),
            "no typed notification for undelivered input"
        );
    }

    /// A fake tmux whose `attach-session` is a raw-mode byte recorder:
    /// the hidden client's pty slave is `cat >> OUT` after `stty raw
    /// -echo` (READY is touched once raw), every other tmux call
    /// succeeds, and `display-message` reports a live 80x24 pane.
    fn fake_tmux_runner() -> (
        Runner<crate::runner::supervisor::DirectSupervisor>,
        std::path::PathBuf,
        std::path::PathBuf,
        tempfile::TempDir,
    ) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("typed.bin");
        let ready = dir.path().join("ready");
        let script = dir.path().join("fake_tmux.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 case \"$*\" in\n\
                 *attach-session*) stty raw -echo; : > {ready}; exec cat >> {out} ;;\n\
                 *display-message*) printf '1234 0 0 80 24 0\\n'; exit 0 ;;\n\
                 esac\n\
                 exit 0\n",
                ready = ready.display(),
                out = out.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let run_dir = dir.path().join("run");
        std::fs::create_dir_all(run_dir.join("s")).unwrap();
        std::fs::create_dir_all(run_dir.join("runner")).unwrap();
        // Discovery globs `<run>/s/<id>.sock`: a placeholder file is
        // enough for the fake tmux to answer as a live holder.
        std::fs::write(run_dir.join("s").join("abcd1234.sock"), b"").unwrap();
        let cfg = RunnerConfig::test_config(
            run_dir,
            script.to_string_lossy().into_owned(),
            dir.path().join("conf"),
        );
        (Runner::direct(cfg), out, ready, dir)
    }

    async fn wait_for(path: &std::path::Path, want_len: usize) -> Vec<u8> {
        let start = std::time::Instant::now();
        loop {
            if path.exists() {
                let bytes = std::fs::read(path).unwrap_or_default();
                if bytes.len() >= want_len {
                    return bytes;
                }
            }
            assert!(
                start.elapsed() < DEADLINE,
                "timed out waiting for {want_len} bytes at {}",
                path.display()
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn browser_input_reaches_the_viewers_hidden_client_byte_exact_and_in_order() {
        let (runner, out, ready, _dir) = fake_tmux_runner();
        let id = "abcd1234";
        let viewer = "ws-aaaa1111bb";
        // Bring the hidden client up and wait until its pty is raw: bytes
        // written earlier would still be line-disciplined (the real tmux
        // client is not, so this only synchronises the fake).
        runner.viewer_open(id, viewer, 80, 24).await.unwrap();
        wait_for(&ready, 0).await;
        let payloads: Vec<String> = vec![
            "a".into(),
            "\u{1b}[A".into(),
            "h\u{e9}llo \u{1f600}".into(),
            "\u{1b}[200~line1\rline2\u{1b}[201~".into(),
            "\u{3}\u{7f}\r".into(),
        ];
        let mut client_bytes = Vec::new();
        for p in &payloads {
            client_bytes.push(c2s_text(viewer, &input_json(p)));
        }
        let mut sink = HiddenInput::new(&runner, id, viewer);
        let (tap_end, _tap_peer) = tokio::net::UnixStream::pair().unwrap();
        let (client_end, mut client_peer) = tokio::net::UnixStream::pair().unwrap();
        let (tap_read, tap_write) = tap_end.into_split();
        let (client_read, client_write) = client_end.into_split();
        let task = async {
            splice(
                tap_read,
                tap_write,
                client_read,
                client_write,
                viewer,
                &mut sink,
            )
            .await
        };
        let feed = async {
            for chunk in client_bytes {
                client_peer.write_all(&chunk).await.unwrap();
            }
            let want: Vec<u8> = payloads
                .iter()
                .flat_map(|p| p.as_bytes().to_vec())
                .collect();
            let got = wait_for(&out, want.len()).await;
            assert_eq!(
                got, want,
                "hidden client received exactly the typed bytes, in order"
            );
            drop(client_peer);
        };
        let (res, ()) = tokio::time::timeout(DEADLINE, async { tokio::join!(task, feed) })
            .await
            .expect("splice deadline");
        assert!(res.is_ok(), "{res:?}");
        assert!(
            runner.has_viewer(id, viewer).await,
            "the hidden client lives until the stream owner closes it"
        );
        sink.close().await;
        assert!(
            !runner.has_viewer(id, viewer).await,
            "closing the stream closes its hidden client"
        );
        let pump_gone = runner.viewer_pump(id, viewer).await.is_none();
        assert!(pump_gone);
    }

    #[tokio::test]
    async fn a_watch_only_stream_never_spawns_a_hidden_client() {
        let (runner, _out, ready, _dir) = fake_tmux_runner();
        let id = "abcd1234";
        let viewer = "ws-bbbb2222cc";
        let mut sink = HiddenInput::new(&runner, id, viewer);
        let (tap_end, _tap_peer) = tokio::net::UnixStream::pair().unwrap();
        let (client_end, mut client_peer) = tokio::net::UnixStream::pair().unwrap();
        let (tap_read, tap_write) = tap_end.into_split();
        let (client_read, client_write) = client_end.into_split();
        // Only non-input traffic: the sink must stay untouched.
        client_peer
            .write_all(&c2s_text(viewer, r#"{"t":"ping"}"#))
            .await
            .unwrap();
        drop(client_peer);
        let res = tokio::time::timeout(
            DEADLINE,
            splice(
                tap_read,
                tap_write,
                client_read,
                client_write,
                viewer,
                &mut sink,
            ),
        )
        .await
        .expect("splice deadline");
        assert!(res.is_ok(), "{res:?}");
        assert!(!runner.has_viewer(id, viewer).await);
        assert!(!ready.exists(), "no attach-session was ever spawned");
        sink.close().await;
    }

    #[tokio::test]
    async fn close_after_an_open_cancelled_mid_persist_leaves_no_hidden_client() {
        let (runner, _out, _ready, dir) = fake_tmux_runner();
        let id = "abcd1234";
        let viewer = "ws-cccc3333dd";
        // Make the durable write (set-option) slow while `slow` exists, so
        // the open is cancellable after the viewer is registered.
        let slow = dir.path().join("slow");
        std::fs::write(&slow, b"").unwrap();
        let script = dir.path().join("fake_tmux.sh");
        let body = std::fs::read_to_string(&script).unwrap();
        let body = body.replace(
            "*display-message*)",
            &format!(
                "*set-option*) if [ -e {} ]; then sleep 1; fi; exit 0 ;;\n*display-message*)",
                slow.display()
            ),
        );
        std::fs::write(&script, body).unwrap();
        let mut sink = HiddenInput::new(&runner, id, viewer);
        let cancelled = tokio::time::timeout(
            std::time::Duration::from_millis(400),
            // The future `HiddenInput::deliver` awaits for a first input;
            // dropped mid-await exactly as the splice's `select!` does.
            runner.viewer_open(id, viewer, 80, 24),
        )
        .await;
        assert!(cancelled.is_err(), "the open was cancelled mid-persist");
        assert!(
            runner.has_viewer(id, viewer).await,
            "precondition: the cancelled open left its viewer registered"
        );
        std::fs::remove_file(&slow).unwrap();
        sink.close().await;
        assert!(
            !runner.has_viewer(id, viewer).await,
            "close() must reap a viewer whose open was cancelled"
        );
    }
}
