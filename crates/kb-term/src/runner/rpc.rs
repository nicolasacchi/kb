//! Runner RPC dispatch over the wire `Request`/`Response` types.
//!
//! Every entry validates the id ([`ids::valid_id`]) BEFORE it reaches a
//! path, socket name, unit name or argv; a bad id is `invalid-id` and an
//! unknown harness is `invalid-harness` (checked on the raw JSON so the
//! closed-set failure reports its own code instead of a parse error).
//!
//! Ops implemented in T8: list, launch, kill, resize, read, history, ping.
//! T9 adds: viewer_open, viewer_close, keys, paste, enter, take_size,
//! release_size, grid. Media arrives in a later unit and reports
//! `bad-request` here.
//!
//! Success payloads for launch/kill/read/history are built with `json!`:
//! launch is `{id, pane_pid, cols, rows}`, kill is `{exit_code, forced}`,
//! read/history are `{text, truncated}`. Later units should prefer small
//! typed `Serialize` structs (LaunchData/KillData/ReadData) for these
//! shapes instead of extending the `json!` pattern; the wire map is kept
//! stable either way.

use serde_json::{json, Value};

use crate::runner::discovery;
use crate::runner::enter::QuietSource;
use crate::runner::ids::{valid_harness, valid_id};
use crate::runner::kill;
use crate::runner::launch;
use crate::runner::supervisor::Supervisor;
use crate::runner::tmux::Tmux;
use crate::runner::{Runner, RunnerError};
use crate::wire::{Request, Response};

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
    obj.remove("id");
    serde_json::from_value(v).map_err(|e| RunnerError::BadRequest(format!("bad request: {e}")))
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
            let entries = discovery::list_sessions(runner).await;
            Ok(serde_json::to_value(entries).map_err(|e| RunnerError::Internal(e.to_string()))?)
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
            Ok(json!({
                "exit_code": out.exit_code,
                "forced": out.forced,
            }))
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
            Ok(json!({"viewer": viewer}))
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
            Ok(json!({"viewer": viewer}))
        }
        Request::Keys { id, viewer, input } => {
            if !valid_id(id) {
                return Err(RunnerError::InvalidId);
            }
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
            let bytes = decode_keys_input(input)?;
            let pump = runner
                .viewer_pump(id, viewer)
                .await
                .ok_or_else(|| RunnerError::NoViewer(format!("no viewer `{viewer}` for {id}")))?;
            pump.write(bytes).await?;
            runner.set_paste_only(id, false).await;
            Ok(json!({}))
        }
        Request::Paste { id, viewer, text } => {
            if !valid_id(id) {
                return Err(RunnerError::InvalidId);
            }
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
            let outcome = crate::runner::paste::paste_text(runner, &runner.tmux, id, text).await?;
            runner.set_paste_only(id, outcome.paste_only).await;
            Ok(json!({
                "paste_only": outcome.paste_only,
                "bracketed": outcome.bracketed,
            }))
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
            Ok(json!({}))
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
            Ok(json!({
                "cols": state.cols,
                "rows": state.rows,
                "owner": state.owner,
            }))
        }
        Request::ReleaseSize { id, owner } => {
            if !valid_id(id) {
                return Err(RunnerError::InvalidId);
            }
            if !runner.session_live(id).await {
                return Err(RunnerError::NoSession(format!("no session {id}")));
            }
            crate::runner::grid::release_size(runner, &runner.tmux, id, owner).await?;
            Ok(json!({}))
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
            Ok(json!({
                "cols": state.cols,
                "rows": state.rows,
                "owner": state.owner,
            }))
        }
        // Later units own media; an honest bad-request, not a silent no-op.
        Request::Media { .. } => Err(RunnerError::BadRequest(
            "op not implemented by this unit".to_string(),
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

/// Resize primitive (T9 owns the grid-owner policy): `resize-window`,
/// then the anchor pty winsize (cells + pixel fields).
pub async fn resize<S: Supervisor>(
    runner: &Runner<S>,
    id: &str,
    cols: u16,
    rows: u16,
) -> Result<(), RunnerError> {
    if !(20..=500).contains(&cols) {
        return Err(RunnerError::BadRequest("cols must be 20..=500".to_string()));
    }
    if !(5..=200).contains(&rows) {
        return Err(RunnerError::BadRequest("rows must be 5..=200".to_string()));
    }
    let sock = runner.holder_socket(id);
    let out = runner
        .tmux
        .run(
            &sock,
            &[
                "resize-window",
                "-t",
                "kb",
                "-x",
                &cols.to_string(),
                "-y",
                &rows.to_string(),
            ],
            None,
        )
        .await?;
    if !out.ok {
        return Err(RunnerError::NotFound(format!("no holder for {id}")));
    }
    runner.anchor_set_winsize(id, cols, rows).await;
    Ok(())
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
