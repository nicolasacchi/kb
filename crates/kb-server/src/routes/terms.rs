//! SPA-only terminal REST relays (`/api/terms/*`, T11).
//!
//! Every handler calls [`gate`](crate::terminal::gate::gate) first
//! (enabled → Origin → intent → caller class); the runner is reached
//! per request over a unix socket with the per-request client in
//! [`runner_client`](crate::terminal::runner_client). kb-server holds
//! NO terminal state except the ticket table.
//!
//! Validation order in each handler: gate → `id` shape → body shape.
//! The runner failure mapping is: unreachable/timeout → 503
//! (`terminal-runner-unavailable`, the socket path NEVER leaks),
//! `not_found` → 404, `blocked` → 409, `multi_line_refused` → 422,
//! `grid_owned` → 409 (with the reply's message), `bad_request` → 400,
//! anything else → 502.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::middleware::Identity;
use crate::state::KbHandles;
use crate::terminal::gate::{
    gate, Problem, ERR_TERMINAL_BAD_REPLY, ERR_TERMINAL_BAD_REQUEST, ERR_TERMINAL_CONFLICT,
    ERR_TERMINAL_NOT_FOUND, ERR_TERMINAL_PAYLOAD_TOO_LARGE, ERR_TERMINAL_REFUSED,
    ERR_TERMINAL_RUNNER_UNAVAILABLE, ERR_TERMINAL_TICKET_FULL, ERR_TERMINAL_TICKET_UNAVAILABLE,
};
use crate::terminal::runner_client::{RunnerError, UnixRunner};
use crate::terminal::wire::{Harness, RunnerReply, RunnerRequest};
use axum::Extension;

/// `ids` are 1..=16 chars drawn from `[a-z0-9]` only. (The runner's own
/// `is_valid_term_id` is 8..=12 — this relay accepts the wider shape
/// so the ticket handler can 404 on an id the runner never minted
/// rather than 400 it.)
fn is_valid_term_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 16
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// The maximum request body the relay parses before a 413.
const MAX_BODY_BYTES: usize = 256 * 1024;

/// The maximum decoded `paste.text` (a terminal injection beyond this is
/// refused, never truncated).
const MAX_PASTE_TEXT_BYTES: usize = 32 * 1024;

fn no_store(mut resp: Response<Body>) -> Response<Body> {
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    resp
}

fn bad_request(detail: impl Into<String>) -> Response<Body> {
    no_store(Problem::response(
        ERR_TERMINAL_BAD_REQUEST,
        StatusCode::BAD_REQUEST,
        "Bad Request",
        detail.into(),
    ))
}

fn payload_too_large(detail: impl Into<String>) -> Response<Body> {
    no_store(Problem::response(
        ERR_TERMINAL_PAYLOAD_TOO_LARGE,
        StatusCode::PAYLOAD_TOO_LARGE,
        "Payload Too Large",
        detail.into(),
    ))
}

fn not_found(detail: impl Into<String>) -> Response<Body> {
    no_store(Problem::response(
        ERR_TERMINAL_NOT_FOUND,
        StatusCode::NOT_FOUND,
        "Not Found",
        detail.into(),
    ))
}

/// The `{"terms": ...}` list envelope (typed, no `json!`).
#[derive(Debug, Serialize)]
struct TermsList {
    terms: serde_json::Value,
}

/// Map one runner reply into an HTTP response. `List` wraps `data` as
/// `{"terms": data}`; everything else relays `data` verbatim as the
/// payload.
fn relay(reply: RunnerReply) -> Response<Body> {
    match reply {
        RunnerReply::Ok { data } => no_store(axum::Json(data).into_response()),
        RunnerReply::Err { code, message } => runner_err(&code, &message),
    }
}

/// The runner error-code → HTTP status mapping (typed strings, no wire
/// assertion stronger than the code).
fn runner_err(code: &str, message: &str) -> Response<Body> {
    let (status, urn) = match code {
        "not_found" => (StatusCode::NOT_FOUND, ERR_TERMINAL_NOT_FOUND),
        "blocked" => (StatusCode::CONFLICT, ERR_TERMINAL_CONFLICT),
        "grid_owned" => (StatusCode::CONFLICT, ERR_TERMINAL_CONFLICT),
        "multi_line_refused" => (StatusCode::UNPROCESSABLE_ENTITY, ERR_TERMINAL_REFUSED),
        "bad_request" => (StatusCode::BAD_REQUEST, ERR_TERMINAL_BAD_REQUEST),
        _ => (StatusCode::BAD_GATEWAY, ERR_TERMINAL_BAD_REPLY),
    };
    let detail = if message.trim().is_empty() {
        format!("terminal runner reports `{code}`")
    } else {
        message.to_string()
    };
    no_store(Problem::response(
        urn,
        status,
        status.canonical_reason().unwrap_or("Error"),
        detail,
    ))
}

fn runner_down() -> Response<Body> {
    no_store(Problem::response(
        ERR_TERMINAL_RUNNER_UNAVAILABLE,
        StatusCode::SERVICE_UNAVAILABLE,
        "Service Unavailable",
        "terminal runner unavailable".to_string(),
    ))
}

/// The runner answered with something unreadable (oversize or
/// unparseable): a bad-gateway shape (502), not "unavailable" — the
/// runner was reachable and spoke nonsense.
fn runner_bad_reply() -> Response<Body> {
    no_store(Problem::response(
        ERR_TERMINAL_BAD_REPLY,
        StatusCode::BAD_GATEWAY,
        "Bad Gateway",
        "terminal runner returned an unreadable reply".to_string(),
    ))
}

/// Build the per-request runner client from the CURRENT config
/// (invariant #13: a config edit restarts in-process; building it here
/// means the very next request uses the new socket/timeout).
async fn runner_for(state: &Arc<KbHandles>) -> UnixRunner {
    let cfg = state.config.read().await;
    let terminal = &cfg.terminal;
    UnixRunner::new(
        terminal.resolved_runner_socket(),
        Duration::from_secs(terminal.runner_timeout_secs()),
    )
}

async fn call(state: &Arc<KbHandles>, req: &RunnerRequest) -> Result<RunnerReply, Response<Body>> {
    let runner = runner_for(state).await;
    match runner.call(req).await {
        Ok(reply) => Ok(reply),
        Err(RunnerError::Unavailable | RunnerError::Timeout) => Err(runner_down()),
        Err(RunnerError::TooLarge | RunnerError::BadReply) => Err(runner_bad_reply()),
    }
}

/// Does `data` (a `RunnerReply::Ok.data` for `list`) contain `id`?
fn list_contains(data: &serde_json::Value, id: &str) -> bool {
    if let Some(items) = data.as_array() {
        return items
            .iter()
            .any(|item| item.get("id").and_then(|v| v.as_str()) == Some(id));
    }
    if let Some(map) = data.as_object() {
        return map
            .get("terms")
            .and_then(|v| v.as_array())
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item.get("id").and_then(|v| v.as_str()) == Some(id))
            });
    }
    false
}

fn check_id(id: &str) -> bool {
    is_valid_term_id(id)
}

/// T13 — the `(id, pane_pid)` pairs of a runner `list` reply `data`.
fn items_of(data: &serde_json::Value) -> Vec<(String, i64)> {
    fn from_items(items: &[serde_json::Value]) -> Vec<(String, i64)> {
        items
            .iter()
            .filter_map(|item| {
                let id = item.get("id").and_then(|v| v.as_str())?.to_string();
                let pid = item.get("pane_pid").and_then(|v| v.as_i64())?;
                Some((id, pid))
            })
            .collect()
    }
    if let Some(items) = data.as_array() {
        return from_items(items);
    }
    if let Some(items) = data.get("terms").and_then(|v| v.as_array()) {
        return from_items(items);
    }
    Vec::new()
}

/// T13 — a successful `list` refreshes the term directory deterministically.
fn refresh_directory(state: &KbHandles, data: &serde_json::Value, now_unix: i64) {
    state.terminals.term_dir.replace(items_of(data), now_unix);
}

/// T13 — additively enrich each list item with `session_id`, `state` and
/// `blocked` from the live registry. `bound: None` means "state unknown".
fn enrich_terms(state: &KbHandles, data: &mut serde_json::Value, now_unix: i64) {
    fn enrich_items(state: &KbHandles, items: &mut [serde_json::Value], now_unix: i64) {
        for item in items.iter_mut() {
            let Some(id) = item.get("id").and_then(|v| v.as_str()).map(str::to_string) else {
                continue;
            };
            let gate = state.live_registry.term_gate(&id, now_unix);
            if let Some(obj) = item.as_object_mut() {
                obj.insert(
                    "session_id".to_string(),
                    gate.bound
                        .clone()
                        .map(serde_json::Value::String)
                        .unwrap_or(serde_json::Value::Null),
                );
                obj.insert(
                    "state".to_string(),
                    gate.state
                        .map(|s| serde_json::to_value(s).unwrap_or(serde_json::Value::Null))
                        .unwrap_or(serde_json::Value::Null),
                );
                obj.insert("blocked".to_string(), serde_json::Value::Bool(gate.blocked));
            }
        }
    }
    if let Some(items) = data.as_array_mut() {
        enrich_items(state, items, now_unix);
    } else if let Some(items) = data.get_mut("terms").and_then(|v| v.as_array_mut()) {
        enrich_items(state, items, now_unix);
    }
}

/// A `resume_sid`/`cwd` value: non-empty, ≤ 512 bytes, no NUL/control.
fn free_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && !value.chars().any(|c| c.is_control() || c == '\0')
}

// ---------------------------------------------------------------- list

pub async fn list(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: axum::http::HeaderMap,
) -> Response<Body> {
    if let Err(e) = gate(&state, &headers, &identity, false).await {
        return e;
    }
    match call(&state, &RunnerRequest::List {}).await {
        Err(e) => e,
        Ok(RunnerReply::Ok { data }) => {
            let mut data = data;
            let now_unix = chrono::Utc::now().timestamp();
            refresh_directory(&state, &data, now_unix);
            enrich_terms(&state, &mut data, now_unix);
            no_store(axum::Json(TermsList { terms: data }).into_response())
        }
        Ok(RunnerReply::Err { code, message }) => runner_err(&code, &message),
    }
}

// ---------------------------------------------------------------- launch

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchBody {
    harness: String,
    resume_sid: Option<String>,
    cwd: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
}

fn parse_harness(name: &str) -> Option<Harness> {
    Harness::ALL.iter().copied().find(|h| h.as_str() == name)
}

pub async fn launch(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Response<Body> {
    if let Err(e) = gate(&state, &headers, &identity, true).await {
        return e;
    }
    if body.len() > MAX_BODY_BYTES {
        return payload_too_large("request body over 256 KiB");
    }
    let parsed: LaunchBody = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return bad_request("malformed launch body"),
    };
    let harness = match parse_harness(&parsed.harness) {
        Some(h) => h,
        None => return bad_request("unknown harness"),
    };
    if let Some(sid) = &parsed.resume_sid {
        if !free_text(sid) {
            return bad_request("invalid resume_sid");
        }
    }
    if let Some(cwd) = &parsed.cwd {
        if !free_text(cwd) || !std::path::Path::new(cwd).is_absolute() {
            return bad_request("cwd must be an absolute path");
        }
    }
    // kb-server supplies the runner-shaped defaults; T10 owns the
    // launch profiles (argv/env) and may ignore them.
    let req = RunnerRequest::Launch {
        harness,
        argv: None,
        resume_sid: parsed.resume_sid.clone(),
        cwd: parsed.cwd.clone().unwrap_or_else(|| "/".to_string()),
        cols: parsed.cols.unwrap_or(80),
        rows: parsed.rows.unwrap_or(24),
        env: None,
        exit_keys: None,
    };
    match call(&state, &req).await {
        Err(e) => e,
        Ok(RunnerReply::Ok { data }) => {
            no_store((StatusCode::CREATED, axum::Json(data)).into_response())
        }
        Ok(RunnerReply::Err { code, message }) => runner_err(&code, &message),
    }
}

// -------------------------------------------------------------- ticket

#[derive(Debug, Serialize)]
struct TicketResponse {
    ticket: String,
    expires_in: u64,
    term: String,
}

pub async fn ticket(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response<Body> {
    if let Err(e) = gate(&state, &headers, &identity, true).await {
        return e;
    }
    if !check_id(&id) {
        return bad_request("invalid terminal id");
    }
    if body.len() > MAX_BODY_BYTES {
        return payload_too_large("request body over 256 KiB");
    }
    // The ticket client sends `{}`; anything else is a malformed body,
    // but `{}` is the only body this route accepts.
    if !body.is_empty()
        && serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.as_object().map(|m| m.is_empty()))
            != Some(true)
    {
        return bad_request("ticket route accepts an empty object body");
    }
    // Presence is proven relay-first: the runner's `list` is the
    // authority on whether `id` exists.
    match call(&state, &RunnerRequest::List {}).await {
        Err(e) => e,
        Ok(RunnerReply::Err { code, message }) => runner_err(&code, &message),
        Ok(RunnerReply::Ok { data }) => {
            if !list_contains(&data, &id) {
                return not_found("no such terminal");
            }
            let ttl_secs = state.config.read().await.terminal.ticket_ttl_secs();
            match state.terminals.tickets.mint(
                &id,
                &identity.user,
                Duration::from_secs(ttl_secs),
                Instant::now(),
            ) {
                Ok(token) => no_store(
                    axum::Json(TicketResponse {
                        ticket: token,
                        expires_in: ttl_secs,
                        term: id.clone(),
                    })
                    .into_response(),
                ),
                Err(crate::terminal::ticket::TicketError::Full) => no_store(Problem::response(
                    ERR_TERMINAL_TICKET_FULL,
                    StatusCode::TOO_MANY_REQUESTS,
                    "Too Many Requests",
                    "ticket table is full".to_string(),
                )),
                Err(crate::terminal::ticket::TicketError::Rng) => no_store(Problem::response(
                    ERR_TERMINAL_TICKET_UNAVAILABLE,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal Server Error",
                    "ticket mint failed".to_string(),
                )),
                Err(_) => no_store(Problem::response(
                    ERR_TERMINAL_TICKET_UNAVAILABLE,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal Server Error",
                    "ticket mint failed".to_string(),
                )),
            }
        }
    }
}

// -------------------------------------------------------------- paste

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PasteBody {
    text: String,
}

pub async fn paste(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response<Body> {
    if let Err(e) = gate(&state, &headers, &identity, true).await {
        return e;
    }
    if !check_id(&id) {
        return bad_request("invalid terminal id");
    }
    if body.len() > MAX_BODY_BYTES {
        return payload_too_large("request body over 256 KiB");
    }
    let parsed: PasteBody = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return bad_request("malformed paste body"),
    };
    if parsed.text.len() > MAX_PASTE_TEXT_BYTES {
        return payload_too_large("paste text over 32 KiB");
    }
    let req = RunnerRequest::Paste {
        id: id.clone(),
        viewer: "spa".to_string(),
        text: parsed.text,
    };
    match call(&state, &req).await {
        Err(e) => e,
        Ok(reply) => relay(reply),
    }
}

// -------------------------------------------------------------- enter

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnterBody {}

pub async fn enter(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response<Body> {
    if let Err(e) = gate(&state, &headers, &identity, true).await {
        return e;
    }
    if !check_id(&id) {
        return bad_request("invalid terminal id");
    }
    if body.len() > MAX_BODY_BYTES {
        return payload_too_large("request body over 256 KiB");
    }
    if serde_json::from_slice::<EnterBody>(&body).is_err() {
        return bad_request("malformed enter body");
    }
    // T13 — a bound-but-blocked terminal refuses with 409 BEFORE the
    // runner relay; an unbound terminal is NOT blocked (state unknown)
    // and gets an `x-kb-term-state: unbound` header on the relayed reply.
    let term_gate = state
        .live_registry
        .term_gate(&id, chrono::Utc::now().timestamp());
    if term_gate.blocked {
        return no_store(Problem::response(
            crate::terminal::gate::ERR_TERM_BLOCKED,
            StatusCode::CONFLICT,
            "Conflict",
            "the bound session is blocked".to_string(),
        ));
    }
    let unbound = term_gate.bound.is_none();
    let req = RunnerRequest::Enter {
        id: id.clone(),
        viewer: "spa".to_string(),
        blocked: false,
        from_composer: false,
    };
    match call(&state, &req).await {
        Err(e) => e,
        Ok(reply) => {
            let mut resp = relay(reply);
            if unbound {
                resp.headers_mut().insert(
                    "x-kb-term-state",
                    axum::http::HeaderValue::from_static("unbound"),
                );
            }
            resp
        }
    }
}

// -------------------------------------------------------------- keys

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeysBody {
    keys: Vec<String>,
}

fn is_key_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 16
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub async fn keys(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response<Body> {
    if let Err(e) = gate(&state, &headers, &identity, true).await {
        return e;
    }
    if !check_id(&id) {
        return bad_request("invalid terminal id");
    }
    if body.len() > MAX_BODY_BYTES {
        return payload_too_large("request body over 256 KiB");
    }
    let parsed: KeysBody = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return bad_request("malformed keys body"),
    };
    if parsed.keys.is_empty()
        || parsed.keys.len() > 32
        || !parsed.keys.iter().all(|k| is_key_name(k))
    {
        return bad_request("keys must be 1..=32 entries of [A-Za-z0-9_-] up to 16 bytes");
    }
    let req = RunnerRequest::Keys {
        id: id.clone(),
        viewer: "spa".to_string(),
        input: crate::terminal::wire::KeyInput::Named { named: parsed.keys },
    };
    match call(&state, &req).await {
        Err(e) => e,
        Ok(reply) => relay(reply),
    }
}

// -------------------------------------------------------------- kill

pub async fn kill(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response<Body> {
    if let Err(e) = gate(&state, &headers, &identity, true).await {
        return e;
    }
    if !check_id(&id) {
        return bad_request("invalid terminal id");
    }
    // Kill takes no client body: anything sent is ignored, but the
    // 256 KiB ceiling still applies so the shape stays uniform with
    // the other POSTs (spec §5 common rules).
    if body.len() > MAX_BODY_BYTES {
        return payload_too_large("request body over 256 KiB");
    }
    let req = RunnerRequest::Kill {
        id: id.clone(),
        grace_secs: None,
    };
    match call(&state, &req).await {
        Err(e) => e,
        Ok(reply) => relay(reply),
    }
}

// ---------------------------------------------------------------- take-size

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TakeSizeBody {
    cols: u16,
    rows: u16,
}

pub async fn take_size(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response<Body> {
    if let Err(e) = gate(&state, &headers, &identity, true).await {
        return e;
    }
    if !check_id(&id) {
        return bad_request("invalid terminal id");
    }
    if body.len() > MAX_BODY_BYTES {
        return payload_too_large("request body over 256 KiB");
    }
    let parsed: TakeSizeBody = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return bad_request("malformed take-size body"),
    };
    let req = RunnerRequest::TakeSize {
        id: id.clone(),
        cols: parsed.cols,
        rows: parsed.rows,
        owner: "kb-term".to_string(),
        force: true,
    };
    match call(&state, &req).await {
        Err(e) => e,
        Ok(reply) => relay(reply),
    }
}

// -------------------------------------------------------------- read/history

pub async fn read(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Response<Body> {
    if let Err(e) = gate(&state, &headers, &identity, false).await {
        return e;
    }
    if !check_id(&id) {
        return bad_request("invalid terminal id");
    }
    match call(&state, &RunnerRequest::Read { id: id.clone() }).await {
        Err(e) => e,
        Ok(reply) => relay(reply),
    }
}

pub async fn history(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Response<Body> {
    if let Err(e) = gate(&state, &headers, &identity, false).await {
        return e;
    }
    if !check_id(&id) {
        return bad_request("invalid terminal id");
    }
    match call(&state, &RunnerRequest::History { id: id.clone() }).await {
        Err(e) => e,
        Ok(reply) => relay(reply),
    }
}
