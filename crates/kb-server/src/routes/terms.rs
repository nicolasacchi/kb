//! SPA-only terminal REST relays (`/api/terms/*`, T11).
//!
//! Every mutation calls [`gate`](crate::terminal::gate::gate) first
//! (enabled → Origin → intent → caller class) and every GET calls
//! [`gate_read`](crate::terminal::gate::gate_read) (the one GET gate);
//! the runner is reached
//! per request over a unix socket with the per-request client in
//! [`runner_client`](crate::terminal::runner_client). kb-server holds
//! NO terminal state except the ticket table.
//!
//! Validation order in each handler: gate → `id` shape → body shape.
//! The runner failure mapping is: unreachable/timeout → 503
//! (`terminal-runner-unavailable`, the socket path NEVER leaks);
//! the runner's dash codes map as follows (pinned by
//! `runner_err_maps_every_dash_code`): `not-found`/`no-session`/
//! `no-viewer` → 404, `blocked`/`grid-owned`/`paste-only`/`busy`/
//! `busy-pty`/`too-many-viewers` → 409 (with the reply's message),
//! `multi-line-refused`/`not-a-file` → 422, `outside-root` → 403,
//! `invalid-id`/`invalid-harness`/
//! `bad-request`/`bad-viewer`/`bad-input`/`bad-key`/`bad-text`/
//! `bad-size` → 400, `too-large` → 413, `bad-image` → 415, `tap-timeout`/`tmux-version`
//! → 503, `tmux-failed`/`internal`/anything else → 502.

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
    gate, gate_read, Problem, ERR_TERMINAL_BAD_REPLY, ERR_TERMINAL_BAD_REQUEST,
    ERR_TERMINAL_CONFLICT, ERR_TERMINAL_NOT_FOUND, ERR_TERMINAL_PAYLOAD_TOO_LARGE,
    ERR_TERMINAL_REFUSED, ERR_TERMINAL_RUNNER_UNAVAILABLE, ERR_TERMINAL_TICKET_FULL,
    ERR_TERMINAL_TICKET_UNAVAILABLE,
};
use crate::terminal::keys_allow::{
    is_named_key, is_valid_viewer_id, ERR_TERMINAL_BAD_KEY, MAX_KEYS_PER_REQUEST,
};
use crate::terminal::launch_guard::{audit_fields, cap_refusal, count_live, emit_audit, Outcome};
use crate::terminal::runner_client::{RunnerError, UnixRunner};
use crate::terminal::wire::{Harness, RunnerReply, RunnerRequest};
use axum::Extension;

use kb_core::sessions::live::LiveState;

// R6: ONE kb-side term-id grammar (`^[a-z0-9]{1,32}$`), the validator in
// `binding.rs`. Only the runner mints/validates 8..=12; an id inside this
// grammar that the runner does not know comes back as a mapped 4xx
// (`invalid-id` 400 / `not-found` 404), never a 502.
use crate::terminal::binding::{is_valid_term_id, pick_session_hint, HintCandidate};

/// CONVBIND — rows read per kb by the hint's capture scan (the same bound
/// live-status Tier-0 uses).
// Mirrors sessions.rs's private TIER0_SCAN_LIMIT (not this unit's file); a
// per-daemon memo of the scan is a tracked follow-up.
const HINT_SCAN_LIMIT: u32 = 500;

/// The maximum request body the relay parses before a 413.
const MAX_BODY_BYTES: usize = 256 * 1024;

/// The maximum decoded `paste.text` (a terminal injection beyond this is
/// refused, never truncated).
const MAX_PASTE_TEXT_BYTES: usize = 32 * 1024;

pub(crate) fn no_store(mut resp: Response<Body>) -> Response<Body> {
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    resp
}

pub(crate) fn bad_request(detail: impl Into<String>) -> Response<Body> {
    no_store(Problem::response(
        ERR_TERMINAL_BAD_REQUEST,
        StatusCode::BAD_REQUEST,
        "Bad Request",
        detail.into(),
    ))
}

pub(crate) fn payload_too_large(detail: impl Into<String>) -> Response<Body> {
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
/// One runner `list` item as a compile-checked struct: `id` and `pane_pid`
/// are typed; every other runner field is preserved verbatim through the
/// `extra` flatten so a newer runner's fields stay additive-readable. The
/// additive enrichment fields (`session_id`, `state`, `blocked`) are real
/// fields — never `obj.insert` on an opaque value — and always serialize
/// (`None` as `null`, `blocked` as `bool`), never skipped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct TermItem {
    id: String,
    pane_pid: Option<i64>,
    #[serde(flatten)]
    pub(crate) extra: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    state: Option<LiveState>,
    #[serde(default)]
    blocked: bool,
    // CONVBIND — a derived folder-match guess on an UNBOUND row; always
    // serialized (`null` when absent), computed per request, never stored,
    // never read by any gate.
    #[serde(default)]
    session_hint: Option<SessionHint>,
}

/// CONVBIND — the label on a guessed session link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum HintVia {
    Cwd,
}

/// CONVBIND — `{session_id, via:"cwd"}`: unverified, derived, not persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SessionHint {
    session_id: String,
    via: HintVia,
}

/// Parse a runner `list` reply `data` into typed items, tolerating the bare
/// array and the `{"terms": array}` envelope. `None` means the shape is
/// neither (malformed): the caller must not replace the directory nor
/// enrich. A valid array — including an empty one — is `Some`; a record
/// without an `id` is dropped but its siblings are kept.
fn parse_term_items(data: &serde_json::Value) -> Option<Vec<TermItem>> {
    fn from_values(items: &[serde_json::Value]) -> Vec<TermItem> {
        items
            .iter()
            .filter_map(|item| serde_json::from_value(item.clone()).ok())
            .collect()
    }
    if let Some(items) = data.as_array() {
        return Some(from_values(items));
    }
    if let Some(items) = data.get("terms").and_then(|v| v.as_array()) {
        return Some(from_values(items));
    }
    None
}

/// Serialize typed items back into the envelope shape `data` had (bare
/// array or `{"terms": array}`), so a forward-compatible runner keeps its
/// own envelope while the items are the typed structs.
fn serialize_items(items: &[TermItem], envelope: &serde_json::Value) -> serde_json::Value {
    let array = serde_json::to_value(items).unwrap_or(serde_json::Value::Array(Vec::new()));
    if envelope.is_array() {
        array
    } else {
        serde_json::json!({"terms": array})
    }
}
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

/// The runner error-code → HTTP status mapping. The runner emits DASH
/// codes (`RunnerError::code()`); matching underscore forms here would
/// make every failure a 502. Every code the runner can emit is pinned
/// by `runner_err_maps_every_dash_code`.
/// The status, problem `type` and detail a runner error code maps to.
fn runner_err_parts(code: &str, message: &str) -> (StatusCode, &'static str, String) {
    let (status, urn) = match code {
        "invalid-id" | "invalid-harness" | "bad-request" | "bad-viewer" | "bad-input"
        | "bad-key" | "bad-text" | "bad-size" => {
            (StatusCode::BAD_REQUEST, ERR_TERMINAL_BAD_REQUEST)
        }
        "not-found" | "no-session" | "no-viewer" => (StatusCode::NOT_FOUND, ERR_TERMINAL_NOT_FOUND),
        "too-large" => (
            StatusCode::PAYLOAD_TOO_LARGE,
            ERR_TERMINAL_PAYLOAD_TOO_LARGE,
        ),
        // T18: an upload whose bytes are not an allowed picture type.
        "bad-image" => (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            crate::terminal::media::ERR_UNSUPPORTED_MEDIA,
        ),
        "blocked" | "grid-owned" | "paste-only" | "busy" | "busy-pty" | "too-many-viewers" => {
            (StatusCode::CONFLICT, ERR_TERMINAL_CONFLICT)
        }
        "multi-line-refused" => (StatusCode::UNPROCESSABLE_ENTITY, ERR_TERMINAL_REFUSED),
        // FXLS: a file read or listing that names something that is not a
        // regular file / directory, or a place outside the working area.
        "not-a-file" => (
            StatusCode::UNPROCESSABLE_ENTITY,
            crate::terminal::files_read::ERR_TERMINAL_NOT_A_FILE,
        ),
        "outside-root" => (
            StatusCode::FORBIDDEN,
            crate::terminal::files_read::ERR_TERMINAL_OUTSIDE_ROOT,
        ),
        "tap-timeout" | "tmux-version" => (
            StatusCode::SERVICE_UNAVAILABLE,
            ERR_TERMINAL_RUNNER_UNAVAILABLE,
        ),
        _ => (StatusCode::BAD_GATEWAY, ERR_TERMINAL_BAD_REPLY),
    };
    let detail = if message.trim().is_empty() {
        format!("terminal runner reports `{code}`")
    } else {
        message.to_string()
    };
    (status, urn, detail)
}

pub(crate) fn runner_err(code: &str, message: &str) -> Response<Body> {
    let (status, urn, detail) = runner_err_parts(code, message);
    no_store(Problem::response(
        urn,
        status,
        status.canonical_reason().unwrap_or("Error"),
        detail,
    ))
}

pub(crate) fn runner_down() -> Response<Body> {
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
pub(crate) fn runner_bad_reply() -> Response<Body> {
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
pub(crate) async fn runner_for(state: &Arc<KbHandles>) -> UnixRunner {
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

/// The relay's single hidden-client viewer identity. The runner
/// requires a registered viewer for paste/enter/keys, and nothing else
/// opens one over HTTP, so every input handler opens `spa` first.
/// The runner's `viewer_open` is idempotent, so re-opening on every
/// call is safe; the `viewer-close` route closes it explicitly and a
/// successful `kill` closes it best-effort (the kill path closes
/// viewers server-side anyway).
const SPA_VIEWER: &str = "spa";

/// Open (idempotently) the relay's hidden client for `id`. Best-effort
/// by design: it must never fail an input call with a viewer-open
/// error of its own — if the open fails (e.g. no session), the input
/// call's own error is the honest one to surface.
async fn ensure_spa_viewer(state: &Arc<KbHandles>, id: &str) {
    let _ = call(
        state,
        &RunnerRequest::ViewerOpen {
            id: id.to_string(),
            viewer: SPA_VIEWER.to_string(),
        },
    )
    .await;
}

/// T18 — the optional drive `viewer` of a request body, validated against
/// the tap's viewer-id grammar. `Ok(None)` = absent. A bad id is a 400
/// before any runner call: a client string never reaches the runner
/// unchecked, and it is never persisted (LF-7).
fn drive_viewer(viewer: Option<&str>) -> Result<Option<String>, BadViewer> {
    match viewer {
        None => Ok(None),
        Some(v) if is_valid_viewer_id(v) => Ok(Some(v.to_string())),
        Some(_) => Err(BadViewer),
    }
}

/// A drive `viewer` outside the tap's id grammar (the caller answers 400).
struct BadViewer;

/// T18 — after a drive call SUCCEEDED, tell the tap which device typed.
/// The phone's composer, key bar and choice bar travel over REST, not the
/// WebSocket, so this is the only way the tap learns that the viewer typed
/// (an OSC 52 clipboard write is released only to a viewer that typed in
/// the last 10 s). Best-effort by design: the input already happened, so a
/// failed notification must never turn it into an error. A refused call
/// never reaches here, so a failed paste is not "typing".
async fn notify_typed(state: &Arc<KbHandles>, id: &str, viewer: Option<&str>, reply: &RunnerReply) {
    let Some(viewer) = viewer else { return };
    if !matches!(reply, RunnerReply::Ok { .. }) {
        return;
    }
    if let Err(resp) = call(
        state,
        &RunnerRequest::Typed {
            id: id.to_string(),
            viewer: viewer.to_string(),
        },
    )
    .await
    {
        tracing::debug!(status = %resp.status(), "typed notification not delivered");
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
/// `None` = malformed shape: the old directory is kept untouched by the
/// caller's success-only `replace`. A valid empty array is `Some(vec![])`
/// and a replace with it is correct (the runner really has no terms).
fn items_of(data: &serde_json::Value) -> Option<Vec<(String, i64)>> {
    parse_term_items(data).map(|items| {
        items
            .into_iter()
            .filter_map(|item| Some((item.id, item.pane_pid?)))
            .collect()
    })
}

/// T13 — a successful `list` refreshes the term directory deterministically,
/// but only when the reply's shape parses; a malformed `Ok` must never wipe
/// the directory (keep the old map).
fn refresh_directory(state: &KbHandles, data: &serde_json::Value, now_unix: i64) {
    if let Some(items) = items_of(data) {
        state.terminals.term_dir.replace(items, now_unix);
    }
}

/// T13 — additively enrich each typed list item in place with `session_id`,
/// `state` and `blocked` from the live registry, on the typed fields (never
/// an opaque `obj.insert`). `bound: None` means "state unknown".
fn enrich_terms(state: &KbHandles, items: &mut [TermItem], now_unix: i64) {
    for item in items.iter_mut() {
        let gate = state.live_registry.term_gate(&item.id, now_unix);
        item.session_id = gate.bound;
        item.state = gate.state;
        item.blocked = gate.blocked;
        // The hint is never carried over from the runner's reply.
        item.session_hint = None;
    }
}

/// CONVBIND — the unbound rows that may get a hint: `(index, id, harness,
/// cwd, started_unix)`. A verified binding, a dead pane, a stale row or a row
/// without a harness/cwd never qualifies.
fn hint_wanted(items: &[TermItem]) -> Vec<(usize, String, String, String, Option<i64>)> {
    items
        .iter()
        .enumerate()
        .filter(|(_, i)| i.session_id.is_none())
        .filter(|(_, i)| i.extra.get("pane_dead").and_then(|v| v.as_bool()) != Some(true))
        .filter(|(_, i)| i.extra.get("stale").and_then(|v| v.as_bool()) != Some(true))
        .filter_map(|(n, i)| {
            let harness = i.extra.get("harness")?.as_str()?.to_string();
            let cwd = i.extra.get("cwd")?.as_str()?.to_string();
            let started = i.extra.get("started_unix").and_then(|v| v.as_i64());
            Some((n, i.id.clone(), harness, cwd, started))
        })
        .collect()
}

/// CONVBIND — the live registry's rows as hint candidates (a registry row
/// wins over a capture of the same session; an ended row stays in the list
/// as excluded so its capture cannot resurrect it).
fn registry_hint_candidates(state: &KbHandles, now_unix: i64) -> Vec<(HintCandidate, bool)> {
    state
        .live_registry
        .snapshot(now_unix)
        .into_iter()
        .map(|(r, _, _)| {
            let ended = r.holder == kb_core::sessions::live::Holder::Ended;
            let verified = r.term_bound_unix.is_some();
            (
                HintCandidate {
                    session_id: r.session_id,
                    harness: r.harness,
                    cwd: r.cwd,
                    last_activity_unix: r.last_activity_unix,
                    is_subagent_child: false,
                    bound_term: r.term.filter(|_| verified),
                },
                ended,
            )
        })
        .collect()
}

/// CONVBIND — fill `session_hint` on unbound rows from the live registry
/// plus recent captures (the live-status Tier-0 source, so a restarted
/// daemon with an empty registry still guesses). No registry guard is held
/// across an await (#15); the capture scan fans out through `buffered_join`
/// (#28) and only runs when at least one row qualifies.
async fn attach_session_hints(state: &KbHandles, items: &mut [TermItem], now_unix: i64) {
    let wanted = hint_wanted(items);
    if wanted.is_empty() {
        return;
    }
    let registry = registry_hint_candidates(state, now_unix);
    let known: std::collections::HashSet<String> =
        registry.iter().map(|(c, _)| c.session_id.clone()).collect();
    let mut candidates: Vec<HintCandidate> = registry
        .into_iter()
        .filter(|(_, ended)| !ended)
        .map(|(c, _)| c)
        .collect();

    let known = &known;
    let mut futs: Vec<super::CorpusFut<'_, Vec<HintCandidate>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            let list = match ctx
                .storage
                .sessions_list(
                    HINT_SCAN_LIMIT,
                    None,
                    None,
                    None,
                    None,
                    kb_core::sessions::ProjectFilter::default(),
                    Vec::new(),
                    Vec::new(),
                )
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(kb = %kb_name, error = %e, "sessions_list failed in terms session-hint scan");
                    return Vec::new();
                }
            };
            super::sessions::group_tier0_rows(list)
                .into_iter()
                .filter(|row| !known.contains(&row.session_id))
                .filter_map(|row| {
                    let last = row.started_at.max(row.ended_at);
                    if now_unix - last > kb_core::sessions::ACTIVE_WINDOW_SECS {
                        return None;
                    }
                    let is_subagent_child = row
                        .root_session_id
                        .as_deref()
                        .is_some_and(|root| root != row.session_id);
                    Some(HintCandidate {
                        session_id: row.session_id,
                        harness: row.harness,
                        cwd: row.cwd,
                        last_activity_unix: last,
                        is_subagent_child,
                        bound_term: None,
                    })
                })
                .collect()
        }));
    }
    for part in super::buffered_join(futs, state.fanout_cap).await {
        candidates.extend(part);
    }

    for (n, id, harness, cwd, started) in wanted {
        items[n].session_hint =
            pick_session_hint(&id, &harness, &cwd, started, &candidates).map(|session_id| {
                SessionHint {
                    session_id,
                    via: HintVia::Cwd,
                }
            });
    }
}

/// A `resume_sid`/`cwd` value: non-empty, ≤ 512 bytes, no NUL/control.
fn free_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && !value.chars().any(|c| c.is_control() || c == '\0')
}

/// A `handoff_from` value: non-empty, ≤ 128 bytes, first char ASCII
/// alphanumeric, the rest alphanumeric or `.` `_` `:` `-` (a plain
/// token; the runner and the plan re-validate it).
pub(crate) fn plain_token(value: &str) -> bool {
    let mut chars = value.chars();
    value.len() <= 128
        && chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}

// ---------------------------------------------------------------- list

pub async fn list(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: axum::http::HeaderMap,
) -> Response<Body> {
    if let Err(e) = gate_read(&state, &headers, &identity).await {
        return e;
    }
    match call(&state, &RunnerRequest::List {}).await {
        Err(e) => e,
        Ok(RunnerReply::Ok { data }) => {
            let now_unix = chrono::Utc::now().timestamp();
            // Typed boundary: a parseable list refreshes the directory and
            // is enriched on its typed fields; a malformed one relays
            // verbatim — never a directory wipe, never an enrichment pass.
            let terms = match parse_term_items(&data) {
                Some(mut items) => {
                    refresh_directory(&state, &data, now_unix);
                    enrich_terms(&state, &mut items, now_unix);
                    attach_session_hints(&state, &mut items, now_unix).await;
                    serialize_items(&items, &data)
                }
                None => data,
            };
            no_store(axum::Json(TermsList { terms }).into_response())
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
    handoff_from: Option<String>,
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
    // The gate first and unchanged: a gate refusal logs no launch audit
    // line and never reaches the lock or the runner.
    if let Err(e) = gate(&state, &headers, &identity).await {
        return e;
    }
    // From here every exit path writes exactly one audit line.
    let mut facts = LaunchFacts::default();
    let (resp, outcome) = launch_admitted(&state, &body, &mut facts).await;
    emit_audit(&audit_fields(
        facts.harness,
        facts.cwd.as_deref(),
        facts.resume_sid.as_deref(),
        &identity.user,
        outcome,
    ));
    resp
}

/// What the audit line may know about a launch request (reduced further by
/// `audit_fields`). Prompt/handoff text is deliberately not recorded here.
#[derive(Default)]
struct LaunchFacts {
    harness: Option<&'static str>,
    cwd: Option<String>,
    resume_sid: Option<String>,
}

async fn launch_admitted(
    state: &Arc<KbHandles>,
    body: &axum::body::Bytes,
    facts: &mut LaunchFacts,
) -> (Response<Body>, Outcome) {
    let bad = |resp: Response<Body>| (resp, Outcome::BadRequest);
    if body.len() > MAX_BODY_BYTES {
        return bad(payload_too_large("request body over 256 KiB"));
    }
    let parsed: LaunchBody = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return bad(bad_request("malformed launch body")),
    };
    facts.cwd = parsed.cwd.clone();
    facts.resume_sid = parsed.resume_sid.clone();
    let harness = match parse_harness(&parsed.harness) {
        Some(h) => h,
        None => return bad(bad_request("unknown harness")),
    };
    facts.harness = Some(harness.as_str());
    if let Some(sid) = &parsed.resume_sid {
        if !free_text(sid) {
            return bad(bad_request("invalid resume_sid"));
        }
    }
    if let Some(cwd) = &parsed.cwd {
        if !free_text(cwd) || !std::path::Path::new(cwd).is_absolute() {
            return bad(bad_request("cwd must be an absolute path"));
        }
    }
    if let Some(sid) = &parsed.handoff_from {
        if !plain_token(sid) {
            return bad(bad_request("invalid handoff_from"));
        }
        if !matches!(harness, Harness::Claude | Harness::Omp) {
            return bad(bad_request("handoff targets claude or omp"));
        }
        if parsed.resume_sid.is_some() {
            return bad(bad_request("handoff_from excludes resume_sid"));
        }
        if parsed.cwd.is_none() {
            return bad(bad_request("handoff_from requires an absolute cwd"));
        }
    }
    // HUBGUARD: `max_terms = 0` refuses every launch before the lock or the
    // runner is touched.
    let cap = state.config.read().await.terminal.max_terms();
    if cap == 0 {
        return (cap_refusal(0, 0), Outcome::RefusedCap);
    }
    // `cols`/`rows` pass through as `None` when the client omits them,
    // so the runner's own defaults (120x40) apply — the relay must not
    // fabricate its own size. `cwd` defaults to `/` (the runner
    // requires a value); T10 owns the launch profiles (argv/env) and
    // may ignore them.
    let req = RunnerRequest::Launch {
        harness,
        argv: None,
        resume_sid: parsed.resume_sid.clone(),
        handoff_from: parsed.handoff_from.clone(),
        cwd: parsed.cwd.clone().unwrap_or_else(|| "/".to_string()),
        cols: parsed.cols,
        rows: parsed.rows,
        env: None,
        exit_keys: None,
    };
    // HUBGUARD: list -> count -> launch under ONE async lock, dropped before
    // the event and the response. A runner problem on the pre-check fails
    // closed: the launch is never attempted.
    let launched = {
        let _guard = state.terminals.launch_lock.lock().await;
        let live = match call(state, &RunnerRequest::List {}).await {
            Err(e) => return (e, Outcome::RunnerError),
            Ok(RunnerReply::Err { code, message }) => {
                return (runner_err(&code, &message), Outcome::RunnerError)
            }
            Ok(RunnerReply::Ok { data }) => match parse_term_items(&data) {
                Some(items) => count_live(&items),
                None => return (runner_bad_reply(), Outcome::RunnerError),
            },
        };
        if live >= cap {
            return (cap_refusal(live, cap), Outcome::RefusedCap);
        }
        call(state, &req).await
    };
    match launched {
        Err(e) => (e, Outcome::RunnerError),
        Ok(RunnerReply::Ok { data }) => {
            // T12: the runner confirmed the launch — the SPA's
            // `term.created`. The id is the runner's; cols/rows prefer
            // the runner's confirm values, then the launch body, then
            // the runner-shaped defaults (the event must not state
            // dimensions the runner never used). A confirm without
            // an id cannot be emitted as `created` (there is no id
            // to key it on): the reply is malformed, so the launch
            // fails 502 instead of silently skipping the event.
            let new_id = data
                .get("id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_default();
            if new_id.is_empty() {
                return (
                    runner_err("bad-reply", "runner launch confirm has no id"),
                    Outcome::RunnerError,
                );
            }
            // Last resort: the runner's own built-in default (120x40),
            // reached only when a confirm omits its dims and the body
            // sent none (the relay never fabricates its own size).
            let dim = |key: &str, fallback: Option<u16>, default: u16| -> u16 {
                data.get(key)
                    .and_then(|v| v.as_u64())
                    .and_then(|v| u16::try_from(v).ok())
                    .or(fallback)
                    .unwrap_or(default)
            };
            crate::terminal::events::emit_created(
                state,
                &new_id,
                &parsed.harness,
                dim("cols", parsed.cols, 120),
                dim("rows", parsed.rows, 40),
            );
            (
                no_store((StatusCode::CREATED, axum::Json(data)).into_response()),
                Outcome::Accepted,
            )
        }
        Ok(RunnerReply::Err { code, message }) => {
            let outcome = if code == "bad-request" || code == "invalid-harness" {
                Outcome::BadRequest
            } else {
                Outcome::RunnerError
            };
            (runner_err(&code, &message), outcome)
        }
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
    if let Err(e) = gate(&state, &headers, &identity).await {
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
    /// FUPHONESEND: press Enter after the paste, in this one request (the
    /// composer's Send). The runner's Enter guards all apply; a refused
    /// Enter does not undo the paste (see [`PasteEnterReply`]).
    #[serde(default)]
    enter: bool,
    /// T18: the panel's viewer id (the OSC 52 typing clock).
    #[serde(default)]
    viewer: Option<String>,
}

/// The answer to a paste that asked for Enter: the runner's paste data
/// plus whether the Enter went. A refused Enter is `entered: false` with
/// the refusal inside a 200, because the text IS in the pane: the caller
/// must tell "pasted, not sent" from a failed paste (which is an error
/// status and wrote nothing).
#[derive(Serialize)]
struct PasteEnterReply {
    #[serde(flatten)]
    paste: serde_json::Value,
    entered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    enter_error: Option<EnterRefusal>,
}

#[derive(Serialize)]
struct EnterRefusal {
    status: u16,
    r#type: &'static str,
    detail: String,
}

pub async fn paste(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response<Body> {
    if let Err(e) = gate(&state, &headers, &identity).await {
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
    let typed_by = match drive_viewer(parsed.viewer.as_deref()) {
        Ok(v) => v,
        Err(BadViewer) => return bad_request("invalid viewer id"),
    };
    ensure_spa_viewer(&state, &id).await;
    let req = RunnerRequest::Paste {
        id: id.clone(),
        viewer: SPA_VIEWER.to_string(),
        text: parsed.text,
    };
    match call(&state, &req).await {
        Err(e) => e,
        Ok(reply) => {
            notify_typed(&state, &id, typed_by.as_deref(), &reply).await;
            match reply {
                RunnerReply::Ok { data } if parsed.enter => {
                    let reply = paste_then_enter(&state, &id, data).await;
                    no_store(axum::Json(reply).into_response())
                }
                reply => relay(reply),
            }
        }
    }
}

/// The Enter half of a paste-with-enter: the same gates as `POST …/enter`
/// (a blocked bound session, then the runner's paste-only / quiet / cap
/// guards), but a refusal is reported inside the answer instead of failing
/// the request, since the paste already landed.
async fn paste_then_enter(
    state: &Arc<KbHandles>,
    id: &str,
    paste: serde_json::Value,
) -> PasteEnterReply {
    let refused = |status: StatusCode, r#type: &'static str, detail: String| PasteEnterReply {
        paste: paste.clone(),
        entered: false,
        enter_error: Some(EnterRefusal {
            status: status.as_u16(),
            r#type,
            detail,
        }),
    };
    let term_gate = state
        .live_registry
        .term_gate(id, chrono::Utc::now().timestamp());
    if term_gate.blocked {
        return refused(
            StatusCode::CONFLICT,
            crate::terminal::gate::ERR_TERM_BLOCKED,
            "the bound session is blocked".to_string(),
        );
    }
    let req = RunnerRequest::Enter {
        id: id.to_string(),
        viewer: SPA_VIEWER.to_string(),
        blocked: false,
        from_composer: true,
    };
    match call(state, &req).await {
        Ok(RunnerReply::Ok { .. }) => PasteEnterReply {
            paste: paste.clone(),
            entered: true,
            enter_error: None,
        },
        Ok(RunnerReply::Err { code, message }) => {
            let (status, urn, detail) = runner_err_parts(&code, &message);
            refused(status, urn, detail)
        }
        Err(resp) => refused(
            resp.status(),
            ERR_TERMINAL_RUNNER_UNAVAILABLE,
            "terminal runner unavailable".to_string(),
        ),
    }
}

// -------------------------------------------------------------- enter

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnterBody {
    /// T18: the panel's viewer id (the OSC 52 typing clock).
    #[serde(default)]
    viewer: Option<String>,
}

pub async fn enter(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response<Body> {
    if let Err(e) = gate(&state, &headers, &identity).await {
        return e;
    }
    if !check_id(&id) {
        return bad_request("invalid terminal id");
    }
    if body.len() > MAX_BODY_BYTES {
        return payload_too_large("request body over 256 KiB");
    }
    let parsed: EnterBody = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return bad_request("malformed enter body"),
    };
    let typed_by = match drive_viewer(parsed.viewer.as_deref()) {
        Ok(v) => v,
        Err(BadViewer) => return bad_request("invalid viewer id"),
    };
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
    ensure_spa_viewer(&state, &id).await;
    // The SPA's Enter is always the composer Enter: after a
    // paste-only paste (`/` or `@`) the runner refuses with
    // `paste-only` (409) and nothing is written.
    let req = RunnerRequest::Enter {
        id: id.clone(),
        viewer: SPA_VIEWER.to_string(),
        blocked: false,
        from_composer: true,
    };
    match call(&state, &req).await {
        Err(e) => e,
        Ok(reply) => {
            notify_typed(&state, &id, typed_by.as_deref(), &reply).await;
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
    /// T18: the panel's viewer id (the OSC 52 typing clock).
    #[serde(default)]
    viewer: Option<String>,
}

pub async fn keys(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response<Body> {
    if let Err(e) = gate(&state, &headers, &identity).await {
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
    if parsed.keys.is_empty() || parsed.keys.len() > MAX_KEYS_PER_REQUEST {
        return bad_request("keys must be 1..=32 entries");
    }
    // T18: a closed vocabulary. An unknown name is its own error type, and
    // no client string is forwarded that the runner's table does not map.
    if let Some(unknown) = parsed.keys.iter().find(|k| !is_named_key(k)) {
        return no_store(Problem::response(
            ERR_TERMINAL_BAD_KEY,
            StatusCode::BAD_REQUEST,
            "Bad Request",
            format!(
                "unknown key name `{}`",
                unknown.chars().take(16).collect::<String>()
            ),
        ));
    }
    let typed_by = match drive_viewer(parsed.viewer.as_deref()) {
        Ok(v) => v,
        Err(BadViewer) => return bad_request("invalid viewer id"),
    };
    ensure_spa_viewer(&state, &id).await;
    let req = RunnerRequest::Keys {
        id: id.clone(),
        viewer: SPA_VIEWER.to_string(),
        input: crate::terminal::wire::KeyInput::Named { named: parsed.keys },
    };
    match call(&state, &req).await {
        Err(e) => e,
        Ok(reply) => {
            notify_typed(&state, &id, typed_by.as_deref(), &reply).await;
            relay(reply)
        }
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
    if let Err(e) = gate(&state, &headers, &identity).await {
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
    let reply = match call(&state, &req).await {
        Err(e) => return e,
        Ok(reply) => reply,
    };
    if let RunnerReply::Ok { ref data } = reply {
        // T12: the runner confirmed the kill: `term.ended` (once per
        // id; the relay's `exit`-frame path claims the same slot and
        // loses gracefully). Emitted before the viewer close round
        // trip so a slow runner never delays the event.
        let exit_code = data
            .get("exit_code")
            .and_then(|v| v.as_i64())
            // Out of range: skip the code, never wrap it.
            .and_then(|c| i32::try_from(c).ok());
        crate::terminal::events::emit_ended(&state, &id, exit_code);
        // Best-effort: the kill path closes viewers server-side,
        // but an explicit close keeps a stale hidden client from
        // outliving the session if the kill was partial.
        let _ = call(
            &state,
            &RunnerRequest::ViewerClose {
                id: id.clone(),
                viewer: SPA_VIEWER.to_string(),
            },
        )
        .await;
    }
    relay(reply)
}

// ---------------------------------------------------------------- take-size

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TakeSizeBody {
    cols: u16,
    rows: u16,
    /// T18: the panel's viewer id (the OSC 52 typing clock).
    #[serde(default)]
    viewer: Option<String>,
}

pub async fn take_size(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response<Body> {
    if let Err(e) = gate(&state, &headers, &identity).await {
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
    let typed_by = match drive_viewer(parsed.viewer.as_deref()) {
        Ok(v) => v,
        Err(BadViewer) => return bad_request("invalid viewer id"),
    };
    let req = RunnerRequest::TakeSize {
        id: id.clone(),
        cols: parsed.cols,
        rows: parsed.rows,
        owner: "kb-term".to_string(),
        // Not forced: the runner's precedence (`grid::decide_take`) lets the
        // web owner take over a local attach WITHOUT force, while a foreign
        // non-local owner still surfaces grid-owned (409).
        force: false,
    };
    match call(&state, &req).await {
        Err(e) => e,
        Ok(reply) => {
            notify_typed(&state, &id, typed_by.as_deref(), &reply).await;
            relay(reply)
        }
    }
}

// -------------------------------------------------------------- read/history

pub async fn read(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Response<Body> {
    if let Err(e) = gate_read(&state, &headers, &identity).await {
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
    if let Err(e) = gate_read(&state, &headers, &identity).await {
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

// ------------------------------------------------- grid/release/resize/viewer

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyBody {}

pub async fn grid(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Response<Body> {
    if let Err(e) = gate_read(&state, &headers, &identity).await {
        return e;
    }
    if !check_id(&id) {
        return bad_request("invalid terminal id");
    }
    match call(&state, &RunnerRequest::Grid { id: id.clone() }).await {
        Err(e) => e,
        Ok(reply) => relay(reply),
    }
}

pub async fn release_size(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response<Body> {
    if let Err(e) = gate(&state, &headers, &identity).await {
        return e;
    }
    if !check_id(&id) {
        return bad_request("invalid terminal id");
    }
    if body.len() > MAX_BODY_BYTES {
        return payload_too_large("request body over 256 KiB");
    }
    if serde_json::from_slice::<EmptyBody>(&body).is_err() {
        return bad_request("malformed release-size body");
    }
    // The relay's single grid owner: the SPA can always release the
    // grid it took through `take-size` (same `"kb-term"` owner).
    let req = RunnerRequest::ReleaseSize {
        id: id.clone(),
        owner: "kb-term".to_string(),
    };
    match call(&state, &req).await {
        Err(e) => e,
        Ok(reply) => relay(reply),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResizeBody {
    cols: u16,
    rows: u16,
}

pub async fn resize(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response<Body> {
    if let Err(e) = gate(&state, &headers, &identity).await {
        return e;
    }
    if !check_id(&id) {
        return bad_request("invalid terminal id");
    }
    if body.len() > MAX_BODY_BYTES {
        return payload_too_large("request body over 256 KiB");
    }
    let parsed: ResizeBody = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return bad_request("malformed resize body"),
    };
    let req = RunnerRequest::Resize {
        id: id.clone(),
        cols: parsed.cols,
        rows: parsed.rows,
    };
    match call(&state, &req).await {
        Err(e) => e,
        Ok(reply) => relay(reply),
    }
}

pub async fn viewer_close(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response<Body> {
    if let Err(e) = gate(&state, &headers, &identity).await {
        return e;
    }
    if !check_id(&id) {
        return bad_request("invalid terminal id");
    }
    if body.len() > MAX_BODY_BYTES {
        return payload_too_large("request body over 256 KiB");
    }
    if serde_json::from_slice::<EmptyBody>(&body).is_err() {
        return bad_request("malformed viewer-close body");
    }
    let req = RunnerRequest::ViewerClose {
        id: id.clone(),
        viewer: SPA_VIEWER.to_string(),
    };
    match call(&state, &req).await {
        Err(e) => e,
        Ok(reply) => relay(reply),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_item() -> serde_json::Value {
        serde_json::json!({
            "id": "abc123",
            "harness": "omp",
            "cwd": "/work/proj",
            "started_unix": 1234567890,
            "pane_pid": 4242,
        })
    }

    #[test]
    fn malformed_list_shape_is_none_valid_empty_is_some() {
        assert!(items_of(&serde_json::json!({"oops": 1})).is_none());
        assert_eq!(
            items_of(&serde_json::json!([])),
            Some(Vec::new()),
            "a valid empty array is a real (empty) directory, not malformed"
        );
        assert_eq!(
            items_of(&serde_json::json!({"terms": []})),
            Some(Vec::new())
        );
        assert_eq!(
            items_of(&serde_json::json!([one_item()])),
            Some(vec![("abc123".to_string(), 4242)])
        );
    }

    #[test]
    fn term_item_round_trips_runner_fields_and_adds_typed_enrichment() {
        let items = parse_term_items(&serde_json::json!([one_item()])).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "abc123");
        assert_eq!(items[0].pane_pid, Some(4242));
        // Enrichment defaults: unbound, unknown state, not blocked — always
        // serialized (null/null/false), never skipped.
        let out = serialize_items(&items, &serde_json::json!([]));
        let item = &out[0];
        assert_eq!(item["id"], "abc123");
        assert_eq!(
            item["harness"], "omp",
            "runner fields survive the typed round trip"
        );
        assert_eq!(item["session_id"], serde_json::Value::Null);
        assert_eq!(item["state"], serde_json::Value::Null);
        assert_eq!(item["blocked"], false);
        assert_eq!(item["session_hint"], serde_json::Value::Null);
        // Envelope shape is preserved.
        let wrapped = serialize_items(&items, &serde_json::json!({"terms": []}));
        assert!(wrapped.get("terms").is_some(), "{wrapped}");
        assert_eq!(wrapped["terms"][0]["id"], "abc123");
    }

    #[test]
    fn serialized_item_always_carries_session_hint_null_by_default() {
        let items = parse_term_items(&serde_json::json!([one_item()])).unwrap();
        let out = serialize_items(&items, &serde_json::json!([]));
        assert!(out[0].as_object().unwrap().contains_key("session_hint"));
        assert_eq!(out[0]["session_hint"], serde_json::Value::Null);
    }

    #[test]
    fn hint_serializes_as_session_id_and_via_cwd() {
        let mut items = parse_term_items(&serde_json::json!([one_item()])).unwrap();
        items[0].session_hint = Some(SessionHint {
            session_id: "sid-a".into(),
            via: HintVia::Cwd,
        });
        let out = serialize_items(&items, &serde_json::json!([]));
        assert_eq!(
            out[0]["session_hint"],
            serde_json::json!({"session_id": "sid-a", "via": "cwd"})
        );
    }

    #[test]
    fn bound_row_has_no_hint() {
        let mut items = parse_term_items(&serde_json::json!([one_item()])).unwrap();
        assert_eq!(hint_wanted(&items).len(), 1);
        items[0].session_id = Some("sid-v".into());
        assert!(hint_wanted(&items).is_empty(), "a verified binding wins");
    }

    #[test]
    fn dead_stale_or_unlocated_rows_are_not_hinted() {
        for patch in [
            serde_json::json!({"pane_dead": true}),
            serde_json::json!({"stale": true}),
            serde_json::json!({"cwd": null}),
            serde_json::json!({"harness": null}),
        ] {
            let mut v = one_item();
            for (k, val) in patch.as_object().unwrap() {
                v[k] = val.clone();
            }
            let items = parse_term_items(&serde_json::json!([v])).unwrap();
            assert!(hint_wanted(&items).is_empty(), "{patch}");
        }
    }

    #[test]
    fn refresh_directory_keeps_old_map_on_a_malformed_ok() {
        let dir = crate::terminal::binding::TermDirectory::new();
        dir.replace([("abc123".to_string(), 4242)], 1_000_000);
        // The caller gate (items_of) returns None for malformed shape, so
        // replace is never called and the primed pane_pid still resolves.
        assert!(items_of(&serde_json::json!({"oops": 1})).is_none());
        assert_eq!(dir.pane_pid("abc123"), Some(4242));
    }

    #[test]
    fn relay_accepts_the_unified_32_char_grammar() {
        assert!(is_valid_term_id(&"a".repeat(32)));
        assert!(!is_valid_term_id(&"a".repeat(33)));
        assert!(!is_valid_term_id("Bad"));
    }

    fn status(code: &str) -> u16 {
        runner_err(code, "synthetic message").status().as_u16()
    }

    /// Every dash code `RunnerError::code()` can emit keeps its
    /// natural HTTP status; the underscore forms the old mapping
    /// matched (and the fake runner used to send) now fall to 502,
    /// which is the observable proof the dead path is gone.
    #[test]
    fn runner_err_maps_every_dash_code() {
        for code in [
            "invalid-id",
            "invalid-harness",
            "bad-request",
            "bad-viewer",
            "bad-input",
            "bad-key",
            "bad-text",
            "bad-size",
        ] {
            assert_eq!(status(code), 400, "{code}");
        }
        for code in ["not-found", "no-session", "no-viewer"] {
            assert_eq!(status(code), 404, "{code}");
        }
        assert_eq!(status("too-large"), 413);
        // T18: an upload that is not an allowed picture is 415; a runner
        // that could not write it is the generic 502.
        assert_eq!(status("bad-image"), 415);
        assert_eq!(status("io"), 502);
        for code in [
            "blocked",
            "grid-owned",
            "paste-only",
            "busy",
            "busy-pty",
            "too-many-viewers",
        ] {
            assert_eq!(status(code), 409, "{code}");
        }
        assert_eq!(status("multi-line-refused"), 422);
        // FXLS: a non-file path is 422, a path outside the working area 403.
        assert_eq!(status("not-a-file"), 422);
        assert_eq!(status("outside-root"), 403);
        assert_eq!(status("tap-timeout"), 503);
        assert_eq!(status("tmux-version"), 503);
        assert_eq!(status("tmux-failed"), 502);
        assert_eq!(status("internal"), 502);
        // The underscore forms the dead mapping matched are NO LONGER
        // special: they must not accidentally keep their old status.
        assert_eq!(status("not_found"), 502);
        assert_eq!(status("grid_owned"), 502);
        assert_eq!(status("multi_line_refused"), 502);
        assert_eq!(status("bad_request"), 502);
        assert_eq!(status("weird_code"), 502);
    }

    /// R6: the relay shares the single kb-side validator, so an id inside
    /// 1..=32 that the runner will reject (`invalid-id`) or not know
    /// (`not-found`) is a mapped 4xx, never a 502.
    #[test]
    fn ids_in_the_kb_grammar_unknown_to_the_runner_map_to_4xx() {
        for ok in ["a", "abc", "abcd1234", &"a".repeat(32)] {
            assert!(is_valid_term_id(ok), "{ok:?} is inside the kb grammar");
        }
        for bad in ["", &"a".repeat(33), "ABCD1234", "abcd-123", "a b"] {
            assert!(!is_valid_term_id(bad), "{bad:?} must be rejected");
        }
        assert_eq!(status("invalid-id"), 400);
        assert_eq!(status("not-found"), 404);
    }
}
