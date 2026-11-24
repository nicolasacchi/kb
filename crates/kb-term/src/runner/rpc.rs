//! Runner RPC dispatch over the wire `Request`/`Response` types.
//!
//! Every entry validates the id ([`ids::valid_id`]) BEFORE it reaches a
//! path, socket name, unit name or argv; a bad id is `invalid-id` and an
//! unknown harness is `invalid-harness` (checked on the raw JSON so the
//! closed-set failure reports its own code instead of a parse error).
//!
//! Ops implemented in T8: list, launch, kill, resize, read, history, ping.
//! T9 adds: viewer_open, viewer_close, keys, paste, enter, take_size,
//! release_size, grid. Media (raw bytes after a `{"len":N}` header) is
//! answered by the connection handler in `server`, not by `dispatch`.
//!
//! Success payloads for the T9 ops (viewer, paste, grid, empty) are
//! typed `Serialize` structs from [`crate::wire`] (`ViewerData`,
//! `PasteData`, `GridData`, `EmptyData`) so the wire map stays
//! generator-visible. Launch/kill/read/history keep their pre-existing
//! `json!` payloads (unchanged in this unit).

use serde_json::{json, Value};

use crate::runner::discovery;
use crate::runner::enter::QuietSource;
use crate::runner::ids::{valid_harness, valid_id};
use crate::runner::kill;
use crate::runner::launch;
use crate::runner::supervisor::Supervisor;
use crate::runner::upload;
use crate::runner::{Runner, RunnerError};
use crate::wire::{
    EmptyData, FileData, GridData, PasteData, Request, Response, UploadBeginData, ViewerData,
};

/// Parse an enveloped request line. The envelope is `{v, id, op, ...}`;
/// `v`/`id` are stripped before the wire `Request` (deny_unknown_fields)
/// is parsed. An unknown harness in a launch is `invalid-harness`.
pub fn parse_request(line: &[u8]) -> Result<Request, RunnerError> {
    let mut v: Value = serde_json::from_slice(line)
        .map_err(|e| RunnerError::BadRequest(format!("bad json: {e}")))?;
    let obj = v
        .as_object_mut()
        .ok_or_else(|| RunnerError::BadRequest("request must be an object".to_string()))?;
    if obj.get("op").and_then(Value::as_str) == Some("launch") {
        if let Some(h) = obj.get("harness").and_then(Value::as_str) {
            if !valid_harness(h) {
                return Err(RunnerError::InvalidHarness(h.to_string()));
            }
        }
    }
    obj.remove("v");
    // The envelope's request id is numeric (`{"v":1,"id":7,"op":...}`);
    // a STRING `id` is the op's own session id (kill, resize, stream, ...)
    // and must reach the wire `Request`. Stripping it unconditionally made
    // every id-carrying op fail with "missing field `id`" over the socket.
    if !obj.get("id").is_some_and(Value::is_string) {
        obj.remove("id");
    }
    serde_json::from_value(v).map_err(|e| RunnerError::BadRequest(format!("bad request: {e}")))
}

/// Run blocking file work off the async workers.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, RunnerError> + Send + 'static,
) -> Result<T, RunnerError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| RunnerError::Io("upload task failed".to_string()))?
}

/// Dispatch one parsed request against the runner.
pub async fn dispatch<S: Supervisor>(runner: &Runner<S>, req: &Request) -> Response {
    match dispatch_inner(runner, req).await {
        Ok(data) => Response::Ok { data },
        Err(e) => Response::Err {
            code: e.code().to_string(),
            message: e.message(),
        },
    }
}

async fn dispatch_inner<S: Supervisor>(
    runner: &Runner<S>,
    req: &Request,
) -> Result<Value, RunnerError> {
    match req {
        Request::Ping {} => Ok(json!({"pong": true})),
        Request::List {} => {
            // The contract promises `SessionInfo`s (`Response::Ok` `data`
            // is an array of them); stale sockets are included with
            // null facts and `stale: true`.
            let entries = discovery::list_sessions(runner).await;
            let infos: Vec<crate::wire::SessionInfo> =
                entries.iter().map(discovery::session_entry_info).collect();
            Ok(serde_json::to_value(infos).map_err(|e| RunnerError::Internal(e.to_string()))?)
        }
        Request::Launch { .. } => {
            // Harness closed-set first (mirrors ids::HARNESSES; the wire
            // enum also rejects unknown names at parse time).
            if let Request::Launch { harness, .. } = req {
                if !valid_harness(harness.as_str()) {
                    return Err(RunnerError::InvalidHarness(harness.as_str().to_string()));
                }
            }
            let spec = launch::validate_launch(req, &runner.cfg)?;
            let out = launch::launch_session(runner, spec).await?;
            Ok(json!({
                "id": out.id,
                "pane_pid": out.pane_pid,
                "cols": out.cols,
                "rows": out.rows,
            }))
        }
        Request::Kill { id, grace_secs } => {
            if !valid_id(id) {
                return Err(RunnerError::InvalidId);
            }
            let out = kill::kill_session(runner, id, *grace_secs).await?;
            let mut reply = json!({
                "exit_code": out.exit_code,
                "forced": out.forced,
            });
            // Only present when a stale entry was reaped, so the live
            // reply stays byte-identical.
            if out.stale_removed {
                reply["stale_removed"] = json!(true);
            }
            Ok(reply)
        }
        Request::Resize { id, cols, rows } => {
            if !valid_id(id) {
                return Err(RunnerError::InvalidId);
            }
            resize(runner, id, *cols, *rows).await?;
            Ok(json!({}))
        }
        Request::History { id } => {
            if !valid_id(id) {
                return Err(RunnerError::InvalidId);
            }
            read_cap(runner, id, true).await
        }
        Request::Read { id } => {
            if !valid_id(id) {
                return Err(RunnerError::InvalidId);
            }
            read_cap(runner, id, false).await
        }
        Request::ViewerOpen { id, viewer } => {
            if !valid_id(id) {
                return Err(RunnerError::InvalidId);
            }
            if !crate::viewer::valid_viewer(viewer) {
                return Err(RunnerError::BadViewer(format!("bad viewer id `{viewer}`")));
            }
            if !runner.session_live(id).await {
                return Err(RunnerError::NoSession(format!("no session {id}")));
            }
            let (cols, rows) = runner
                .current_grid(id)
                .await
                .unwrap_or((runner.cfg.default_cols, runner.cfg.default_rows));
            runner.viewer_open(id, viewer, cols, rows).await?;
            tracing::info!(id, viewer, "viewer opened");
            Ok(serde_json::to_value(&ViewerData {
                viewer: viewer.clone(),
            })
            .map_err(|e| RunnerError::Internal(e.to_string()))?)
        }
        Request::ViewerClose { id, viewer } => {
            if !valid_id(id) {
                return Err(RunnerError::InvalidId);
            }
            if !crate::viewer::valid_viewer(viewer) {
                return Err(RunnerError::BadViewer(format!("bad viewer id `{viewer}`")));
            }
            if !runner.session_live(id).await {
                return Err(RunnerError::NoSession(format!("no session {id}")));
            }
            runner.viewer_close(id, viewer).await?;
            tracing::info!(id, viewer, "viewer closed");
            Ok(serde_json::to_value(&ViewerData {
                viewer: viewer.clone(),
            })
            .map_err(|e| RunnerError::Internal(e.to_string()))?)
        }
        Request::Keys { id, viewer, input } => {
            if !valid_id(id) {
                return Err(RunnerError::InvalidId);
            }
            runner.reap_anchor_closed_viewers(id).await;
            if !crate::viewer::valid_viewer(viewer) {
                return Err(RunnerError::BadViewer(format!("bad viewer id `{viewer}`")));
            }
            if !runner.session_live(id).await {
                return Err(RunnerError::NoSession(format!("no session {id}")));
            }
            if !runner.has_viewer(id, viewer).await {
                return Err(RunnerError::NoViewer(format!(
                    "no viewer `{viewer}` for {id}"
                )));
            }
            // A local attach that no longer owns the grid never types.
            crate::runner::grid::check_input(runner, id, viewer).await?;
            let bytes = decode_keys_input(input)?;
            let pump = runner
                .viewer_pump(id, viewer)
                .await
                .ok_or_else(|| RunnerError::NoViewer(format!("no viewer `{viewer}` for {id}")))?;
            pump.write(bytes).await?;
            runner.set_paste_only(id, false).await;
            Ok(serde_json::to_value(&EmptyData {})
                .map_err(|e| RunnerError::Internal(e.to_string()))?)
        }
        Request::Paste { id, viewer, text } => {
            if !valid_id(id) {
                return Err(RunnerError::InvalidId);
            }
            runner.reap_anchor_closed_viewers(id).await;
            if !crate::viewer::valid_viewer(viewer) {
                return Err(RunnerError::BadViewer(format!("bad viewer id `{viewer}`")));
            }
            if !runner.session_live(id).await {
                return Err(RunnerError::NoSession(format!("no session {id}")));
            }
            if !runner.has_viewer(id, viewer).await {
                return Err(RunnerError::NoViewer(format!(
                    "no viewer `{viewer}` for {id}"
                )));
            }
            crate::runner::grid::check_input(runner, id, viewer).await?;
            let outcome = crate::runner::paste::paste_text(runner, &runner.tmux, id, text).await?;
            runner.set_paste_only(id, outcome.paste_only).await;
            Ok(serde_json::to_value(&PasteData {
                paste_only: outcome.paste_only,
                bracketed: outcome.bracketed,
            })
            .map_err(|e| RunnerError::Internal(e.to_string()))?)
        }
        Request::Enter {
            id,
            viewer,
            blocked,
            from_composer,
        } => {
            if !valid_id(id) {
                return Err(RunnerError::InvalidId);
            }
            runner.reap_anchor_closed_viewers(id).await;
            if !crate::viewer::valid_viewer(viewer) {
                return Err(RunnerError::BadViewer(format!("bad viewer id `{viewer}`")));
            }
            if !runner.session_live(id).await {
                return Err(RunnerError::NoSession(format!("no session {id}")));
            }
            if !runner.has_viewer(id, viewer).await {
                return Err(RunnerError::NoViewer(format!(
                    "no viewer `{viewer}` for {id}"
                )));
            }
            crate::runner::grid::check_input(runner, id, viewer).await?;
            let last_paste_only = runner.paste_only(id).await;
            crate::runner::enter::enter_gate(*blocked, *from_composer, last_paste_only)?;
            let quiet = crate::runner::enter::AnchorQuiet { runner };
            let quiet_ms = runner.cfg.enter_quiet_ms;
            let max_wait_ms = runner.cfg.enter_max_wait_ms;
            crate::runner::enter::wait_quiet(|| quiet.quiet_for(id), quiet_ms, max_wait_ms).await?;
            let pump = runner
                .viewer_pump(id, viewer)
                .await
                .ok_or_else(|| RunnerError::NoViewer(format!("no viewer `{viewer}` for {id}")))?;
            pump.write(vec![0x0d]).await?;
            runner.set_paste_only(id, false).await;
            Ok(serde_json::to_value(&EmptyData {})
                .map_err(|e| RunnerError::Internal(e.to_string()))?)
        }
        Request::TakeSize {
            id,
            cols,
            rows,
            owner,
            force,
        } => {
            if !valid_id(id) {
                return Err(RunnerError::InvalidId);
            }
            if !runner.session_live(id).await {
                return Err(RunnerError::NoSession(format!("no session {id}")));
            }
            let state = crate::runner::grid::take_size(
                runner,
                &runner.tmux,
                id,
                *cols,
                *rows,
                owner,
                *force,
            )
            .await?;
            Ok(serde_json::to_value(&GridData {
                cols: state.cols,
                rows: state.rows,
                owner: state.owner.clone(),
            })
            .map_err(|e| RunnerError::Internal(e.to_string()))?)
        }
        Request::ReleaseSize { id, owner } => {
            if !valid_id(id) {
                return Err(RunnerError::InvalidId);
            }
            if !runner.session_live(id).await {
                return Err(RunnerError::NoSession(format!("no session {id}")));
            }
            crate::runner::grid::release_size(runner, &runner.tmux, id, owner).await?;
            Ok(serde_json::to_value(&EmptyData {})
                .map_err(|e| RunnerError::Internal(e.to_string()))?)
        }
        Request::Grid { id } => {
            if !valid_id(id) {
                return Err(RunnerError::InvalidId);
            }
            if !runner.session_live(id).await {
                return Err(RunnerError::NoSession(format!("no session {id}")));
            }
            let state =
                crate::runner::grid::grid_state(&runner.tmux, &runner.holder_socket(id)).await?;
            Ok(serde_json::to_value(&GridData {
                cols: state.cols,
                rows: state.rows,
                owner: state.owner.clone(),
            })
            .map_err(|e| RunnerError::Internal(e.to_string()))?)
        }
        Request::Typed { id, viewer } => {
            if !valid_id(id) {
                return Err(RunnerError::InvalidId);
            }
            if !crate::viewer::valid_viewer(viewer) {
                return Err(RunnerError::BadViewer(format!("bad viewer id `{viewer}`")));
            }
            // Cheap presence check (no tmux probe): this runs after every
            // phone input call, and a missing holder socket is the same
            // `no-session` the other input ops answer.
            if !runner.holder_socket(id).exists() {
                return Err(RunnerError::NoSession(format!("no session {id}")));
            }
            crate::runner::stream::notify_typed(runner, id, viewer).await?;
            Ok(serde_json::to_value(&EmptyData {})
                .map_err(|e| RunnerError::Internal(e.to_string()))?)
        }
        // `upload` carries raw bytes after its request line, which the
        // envelope-only dispatch cannot express: the connection handler
        // answers it (`server::respond_upload`) before dispatch.
        Request::Upload { .. } => Err(RunnerError::BadRequest(
            "upload is streamed by the connection handler".to_string(),
        )),
        // `upload_append` carries raw bytes after its request line; the
        // connection handler answers it (`server::respond_upload_append`).
        Request::UploadAppend { .. } => Err(RunnerError::BadRequest(
            "upload_append is streamed by the connection handler".to_string(),
        )),
        // The other chunked-upload ops are envelope-only. The blocking
        // file work runs off the async workers.
        Request::UploadBegin {
            id,
            name,
            mime,
            size,
        } => {
            let (dir, id, name, mime, size) = (
                runner.sessions_dir(),
                id.clone(),
                name.clone(),
                mime.clone(),
                *size,
            );
            let token = blocking(move || {
                upload::sweep_orphans(&dir);
                upload::begin_upload(&dir, &id, &name, &mime, size)
            })
            .await?;
            Ok(serde_json::to_value(&UploadBeginData {
                upload: token,
                chunk_max: upload::CHUNK_MAX_BYTES,
            })
            .map_err(|e| RunnerError::Internal(e.to_string()))?)
        }
        Request::UploadCommit { id, upload: token } => {
            let (dir, id, token) = (runner.sessions_dir(), id.clone(), token.clone());
            let c = blocking(move || upload::commit_upload(&dir, &id, &token)).await?;
            Ok(serde_json::to_value(&FileData {
                path: c.path.to_string_lossy().into_owned(),
                bytes: c.bytes,
                mime: c.mime,
                kind: c.kind.to_string(),
                sha256: c.sha256,
                name: c.name,
            })
            .map_err(|e| RunnerError::Internal(e.to_string()))?)
        }
        Request::UploadAbort { id, upload: token } => {
            let (dir, id, token) = (runner.sessions_dir(), id.clone(), token.clone());
            blocking(move || upload::abort_upload(&dir, &id, &token)).await?;
            Ok(serde_json::to_value(&EmptyData {})
                .map_err(|e| RunnerError::Internal(e.to_string()))?)
        }
        // `media` carries raw bytes after its header line, which the
        // envelope-only dispatch cannot express: the connection handler
        // answers it (`server::respond_media`) before dispatch. Reaching
        // this arm means a caller bypassed that path.
        Request::Media { .. } => Err(RunnerError::BadRequest(
            "media is streamed by the connection handler".to_string(),
        )),
        // The stream op never returns as an ordinary RPC: dispatch()
        // must not answer it (the server connection turns into a relay
        // — see server.rs). An ok-shape here would desync the splice,
        // so refuse loudly when reached through dispatch().
        Request::Stream { .. } => Err(RunnerError::BadRequest(
            "stream is a connection splice, not an RPC".to_string(),
        )),
    }
}

/// Decode a `keys` input payload to verbatim bytes (4 KiB decoded cap).
pub fn decode_keys_input(input: &crate::wire::KeyInput) -> Result<Vec<u8>, RunnerError> {
    use base64::Engine;
    match input {
        crate::wire::KeyInput::Bytes { bytes_b64 } => {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(bytes_b64)
                .map_err(|e| RunnerError::BadInput(format!("bad base64: {e}")))?;
            if bytes.len() > 4096 {
                return Err(RunnerError::TooLarge(
                    "keys input exceeds 4 KiB".to_string(),
                ));
            }
            Ok(bytes)
        }
        crate::wire::KeyInput::Named { named } => {
            let mut out = Vec::new();
            for name in named {
                let bytes = crate::runner::keys::named_key_bytes(name)
                    .ok_or_else(|| RunnerError::BadKey(format!("bad key `{name}`")))?;
                out.extend_from_slice(bytes);
            }
            if out.len() > 4096 {
                return Err(RunnerError::TooLarge(
                    "keys input exceeds 4 KiB".to_string(),
                ));
            }
            Ok(out)
        }
    }
}

/// Resize primitive — the legacy op obeys the grid-ownership policy:
/// bounds via `grid::valid_size` (`bad-size`), refuses any recorded
/// grid owner (`grid-owned`, no owner claim on the wire), and applies
/// through `grid::apply_size` (anchor + all viewers). See
/// `grid::resize_legacy`.
pub async fn resize<S: Supervisor>(
    runner: &Runner<S>,
    id: &str,
    cols: u16,
    rows: u16,
) -> Result<(), RunnerError> {
    crate::runner::grid::resize_legacy(runner, &runner.tmux, id, cols, rows).await
}

/// read/history are interim and shape-stable: `capture-pane -p -J`
/// (history adds `-S -`), text capped at 16 MiB (truncated from the head,
/// `truncated: true`).
///
/// Interim backing, replaced by the headless twin when T5/T7 land; RPC
/// shape is frozen.
async fn read_cap<S: Supervisor>(
    runner: &Runner<S>,
    id: &str,
    history: bool,
) -> Result<Value, RunnerError> {
    let text = discovery::capture_pane(runner, id, history).await?;
    const CAP: usize = 16 * 1024 * 1024;
    let (text, truncated) = if text.len() > CAP {
        let cut = text.len() - CAP;
        // Truncate on a char boundary so the output stays valid text.
        let cut = text.ceil_char_boundary(cut);
        (text[cut..].to_string(), true)
    } else {
        (text, false)
    };
    Ok(json!({"text": text, "truncated": truncated}))
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn numeric_envelope_id_is_stripped() {
        let req = parse_request(br#"{"v":1,"id":7,"op":"ping"}"#).expect("ping");
        assert_eq!(req, Request::Ping {});
    }

    #[test]
    fn string_id_is_the_ops_own_session_id() {
        // Without the string-id carve-out these failed with
        // "missing field `id`": the envelope strip ate the session id.
        let stream = parse_request(br#"{"op":"stream","id":"abcd1234","from":null}"#)
            .expect("stream keeps its id");
        assert_eq!(
            stream,
            Request::Stream {
                id: "abcd1234".to_string(),
                from: None,
                epoch: None,
                mseq: None,
            }
        );
        let kill = parse_request(br#"{"v":1,"op":"kill","id":"abcd1234","grace_secs":null}"#)
            .expect("kill keeps its id");
        assert_eq!(
            kill,
            Request::Kill {
                id: "abcd1234".to_string(),
                grace_secs: None
            }
        );
    }
}

/// FXMINTERM: the `kill` reply shape through `dispatch` (what the REST relay
/// forwards): `stale_removed` appears ONLY when a stale entry was reaped.
#[cfg(test)]
mod kill_reply_tests {
    use super::*;
    use crate::runner::supervisor::fake::FakeSupervisor;
    use crate::wire::Request;
    use std::os::unix::fs::PermissionsExt;

    fn runner_with_tmux(
        root: &std::path::Path,
        body: &str,
    ) -> Runner<crate::runner::supervisor::fake::FakeSupervisor> {
        let script = root.join("tmux.sh");
        std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cfg = crate::runner::config::RunnerConfig::test_config(
            root.join("run"),
            script.to_string_lossy().into_owned(),
            std::path::PathBuf::from("/dev/null"),
        );
        Runner::with_supervisor(cfg, FakeSupervisor::new())
    }

    fn kill_req() -> Request {
        Request::Kill {
            id: "abcd1234".to_string(),
            grace_secs: Some(2),
        }
    }

    #[tokio::test]
    async fn a_stale_kill_replies_ok_with_stale_removed_and_a_live_kill_does_not() {
        // Stale: the holder answers "no server running"; its leftovers go.
        let root = tempfile::tempdir().unwrap();
        let runner = runner_with_tmux(root.path(), "echo 'no server running' >&2; exit 1");
        let sdir = runner.sessions_dir();
        std::fs::create_dir_all(sdir.join("abcd1234")).unwrap();
        drop(std::os::unix::net::UnixListener::bind(sdir.join("abcd1234.sock")).unwrap());
        match dispatch(&runner, &kill_req()).await {
            Response::Ok { data } => assert_eq!(
                data,
                json!({"exit_code": null, "forced": false, "stale_removed": true}),
                "a reaped stale entry is a 200 carrying stale_removed"
            ),
            Response::Err { code, message } => panic!("stale kill failed ({code}): {message}"),
        }
        assert!(!sdir.join("abcd1234").exists() && !sdir.join("abcd1234.sock").exists());

        // Live: a holder that answers and whose pane is dead (exit 0): the
        // reply is byte-identical to before, with NO stale_removed key.
        let root = tempfile::tempdir().unwrap();
        let runner = runner_with_tmux(
            root.path(),
            "case \"$*\" in\n\
             *@kb_exit_steps*) echo '[]'; exit 0 ;;\n\
             *'#{pane_dead}'*) printf '1 0\\n'; exit 0 ;;\n\
             esac\n\
             exit 0",
        );
        std::fs::create_dir_all(runner.sessions_dir()).unwrap();
        match dispatch(&runner, &kill_req()).await {
            Response::Ok { data } => assert_eq!(
                data,
                json!({"exit_code": 0, "forced": false}),
                "the live reply carries no stale_removed"
            ),
            Response::Err { code, message } => panic!("live kill failed ({code}): {message}"),
        }
    }
}
