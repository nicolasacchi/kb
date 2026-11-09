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
//! `{offset, mseq: 0, epoch: ""}` when the client asked to resume —
//! the runner cannot know the epoch, so the tap's full-join fallback
//! decides). Client bytes are forwarded to the tap untouched, and tap
//! envelopes are forwarded verbatim (including any control envelopes
//! the tap replies with).

use std::path::PathBuf;

use crate::runner::{Runner, RunnerError};
use crate::wire::{self, ControlMessage, Profile, ResumeInfo};
use tokio::io::AsyncWriteExt;

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
/// the tap may accept or downgrade to a full join.
pub fn join_envelope(viewer: &str, from: Option<u64>) -> Result<Vec<u8>, wire::WireError> {
    let resume = from.map(|offset| ResumeInfo {
        offset,
        mseq: 0,
        epoch: String::new(),
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
/// envelope. The connection is returned live (unread); the caller owns
/// the splice from here.
pub async fn connect_tap<S: crate::runner::supervisor::Supervisor>(
    runner: &Runner<S>,
    id: &str,
    from: Option<u64>,
) -> Result<tokio::net::UnixStream, RunnerError> {
    let path = tap_socket_path(runner, id);
    let mut tap = tokio::net::UnixStream::connect(&path)
        .await
        .map_err(|e| RunnerError::NotFound(format!("no tap socket for {id}: {e}")))?;
    let join = join_envelope(id, from)
        .map_err(|e| RunnerError::Internal(format!("encode join envelope: {e}")))?;
    tap.write_all(&join)
        .await
        .map_err(|e| RunnerError::Internal(format!("write tap join: {e}")))?;
    Ok(tap)
}

/// Splice tap.sock envelopes between the tap connection and the runner
/// client halves until either side closes or errors. Bytes are copied
/// verbatim in both directions — the runner never interprets a frame.
/// Taking halves (not streams) lets the RPC loop hand the client-side
/// halves it already owns after answering the `stream` request line;
/// callers with whole streams should `into_split()` first.
pub async fn splice(
    tap_read: tokio::net::unix::OwnedReadHalf,
    tap_write: tokio::net::unix::OwnedWriteHalf,
    client_read: tokio::net::unix::OwnedReadHalf,
    client_write: tokio::net::unix::OwnedWriteHalf,
) -> std::io::Result<()> {
    let mut tap_read = tap_read;
    let mut tap_write = tap_write;
    let mut client_read = client_read;
    let mut client_write = client_write;
    let to_client = tokio::io::copy(&mut tap_read, &mut client_write);
    let to_tap = tokio::io::copy(&mut client_read, &mut tap_write);
    // The first half to finish owns the close: dropping the other
    // future drops its sockets, which is how the far side observes the
    // close (a dropped runner client tears down its tap connection,
    // and a dead tap ends the client's stream).
    match tokio::try_join!(to_client, to_tap) {
        Ok(_) => Ok(()),
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
        let bytes = join_envelope("abcd1234", None).expect("encode");
        let (viewer, profile) = viewer_open(&bytes);
        assert_eq!(viewer, "abcd1234");
        assert_eq!(profile, Profile::Desktop);
        // resume must be absent/null for a full join (checked via the
        // decoded body containing `"resume":null`).
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("\"resume\":null"), "full join body: {text}");
    }

    #[test]
    fn join_envelope_with_from_carries_resume_coordinates() {
        let bytes = join_envelope("abcd1234", Some(1234)).expect("encode");
        let mut dec = EnvelopeDecoder::new();
        let envs = dec.push(&bytes).expect("decode");
        match serde_json::from_slice::<ControlMessage>(&envs[0].body).unwrap() {
            ControlMessage::ViewerOpen {
                viewer,
                profile,
                resume: Some(r),
            } => {
                assert_eq!(viewer, "abcd1234");
                assert_eq!(profile, Profile::Desktop);
                assert_eq!(r.offset, 1234);
                assert_eq!(r.mseq, 0, "the runner cannot know the marker seq");
                assert!(
                    r.epoch.is_empty(),
                    "the runner cannot know the epoch; the tap's full-join fallback decides"
                );
            }
            other => panic!("expected viewer-open with resume, got {other:?}"),
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
        // Fake tap: record the join, then echo every further chunk back.
        let tap_task = tokio::spawn(async move {
            let (mut conn, _) = tokio::time::timeout(DEADLINE, listener.accept())
                .await
                .expect("accept deadline")
                .expect("accept");
            let join = read_chunk(&mut conn).await;
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
            join
        });
        let cfg =
            RunnerConfig::test_config(run_dir, "/bin/true".to_string(), tmp.path().join("conf"));
        let runner = Runner::direct(cfg);
        let tap = tokio::time::timeout(DEADLINE, connect_tap(&runner, id, None))
            .await
            .expect("connect deadline")
            .expect("connect to fake tap");
        let (client_end, mut test_end) = tokio::net::UnixStream::pair().unwrap();
        let (tap_read, tap_write) = tap.into_split();
        let (client_read, client_write) = client_end.into_split();
        let splice = tokio::spawn(splice(tap_read, tap_write, client_read, client_write));
        let join = tokio::time::timeout(DEADLINE, tap_task)
            .await
            .expect("fake tap deadline")
            .expect("fake tap task");
        let (viewer, profile) = viewer_open(&join);
        assert_eq!(viewer, id, "the join names this session as the viewer");
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
    }
}
