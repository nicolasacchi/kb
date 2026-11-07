//! Runner RPC dispatch over the wire `Request`/`Response` types.
//!
//! Every entry validates the id ([`ids::valid_id`]) BEFORE it reaches a
//! path, socket name, unit name or argv; a bad id is `invalid-id` and an
//! unknown harness is `invalid-harness` (checked on the raw JSON so the
//! closed-set failure reports its own code instead of a parse error).
//!
//! Ops implemented in T8: list, launch, kill, resize, read, history, ping.
//! The tap/viewer ops (paste, enter, keys, take_size, media) arrive in
//! later units and report `bad-request` here.

use serde_json::{json, Value};

use crate::runner::discovery;
use crate::runner::ids::{valid_harness, valid_id};
use crate::runner::kill;
use crate::runner::launch;
use crate::runner::supervisor::Supervisor;
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
        // Later units own these ops; an honest bad-request, not a silent no-op.
        Request::Paste { .. }
        | Request::Enter { .. }
        | Request::Keys { .. }
        | Request::TakeSize { .. }
        | Request::Media { .. } => Err(RunnerError::BadRequest(
            "op not implemented by this unit".to_string(),
        )),
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
