//! The lip/1 HTTP surface: `GET /lip/identity` + `POST /lip/{hover,
//! definition,references,symbols,diagnostics,code-actions}`. Closed
//! endpoint set — SUPERSEDE NOTE (operator ratified 2026-08-28,
//! design-s2.md §S2-C): design-lip.md's original refusal "no code
//! actions, no rename, no formatting: kb-code is a reader" is REVERSED
//! for code actions (the sixth endpoint, below); the rest of that refusal
//! STANDS — no rename, no formatting, no `workspace/executeCommand` (an
//! action whose edit can't be materialized as `TextEdit`s is dropped and
//! counted, never executed — kb-lip never asks a server to run a
//! command). The first four POST routes run the SAME guarded pipeline
//! ([`guarded_request`]): resolve path → hash+verify (pre) → ensure the
//! doc is open with THOSE bytes → LSP request → hash+verify (post) →
//! translate. `/lip/diagnostics` runs an analogous but distinct pipeline
//! ([`guarded_diagnostics`]) that branches on the server's advertised
//! diagnostics mode (`crate::lsp::ServerInfo::diagnostics_mode`, PRR-L5):
//! a server that advertised `diagnosticProvider` at `initialize` is
//! queried via the LSP 3.17 PULL method (`textDocument/diagnostic`,
//! `LspClient::pull_diagnostics`); one that didn't is served from the
//! adapter's own [`crate::lsp`] PUSH cache (`publishDiagnostics`),
//! unchanged from before PRR-L5. `/lip/code-actions` runs its OWN
//! pipeline ([`guarded_code_actions`], S2-C): the same pre/post blob
//! guard bracketing an `textDocument/codeAction` request, but the hash
//! POST check runs ONCE after every per-action `codeAction/resolve`
//! round trip an action lacking `.edit` may need — see that function's
//! doc comment. A refusal is always HTTP 200 with a `refused` field —
//! never a 5xx (the closed reason set: `blob_mismatch` | `server_down` |
//! `unsupported` | `indexing` — the last checked right after acquiring a
//! live client, before `ensure_doc_open`, see
//! [`crate::lsp::LspClient::is_indexing`]).

use crate::blob;
use crate::lsp;
use crate::position;
use crate::supervisor::Supervisor;
use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// `POST /lip/references` cap ("capped, default 200", design-lip.md).
pub const REFERENCES_CAP: usize = 200;

/// `GET /lip/identity`'s `capabilities` array: the fixed set of lip/1
/// endpoints THIS kb-lip BUILD supports (design-addendum-2.md §D — lets a
/// consumer feature-detect against an older deployment that predates a
/// given endpoint, e.g. `/lip/diagnostics` or `/lip/code-actions`).
/// Static, not a reflection of the underlying LSP server's own
/// capabilities — every kb-lip v1 binary implements all six
/// unconditionally (whether the CHILD server itself supports a given verb
/// is a separate, per-request `refused: "unsupported"` question, gated by
/// `crate::lsp::capability_key_for`).
const LIP_CAPABILITIES: [&str; 6] = [
    "hover",
    "definition",
    "references",
    "symbols",
    "diagnostics",
    "code-actions",
];

pub fn router(supervisor: Arc<Supervisor>) -> Router {
    Router::new()
        .route("/lip/identity", get(identity))
        .route("/lip/hover", post(hover))
        .route("/lip/definition", post(definition))
        .route("/lip/references", post(references))
        .route("/lip/symbols", post(symbols))
        .route("/lip/diagnostics", post(diagnostics))
        .route("/lip/code-actions", post(code_actions))
        .with_state(supervisor)
}

#[derive(Debug, Deserialize)]
pub struct LipRequest {
    /// Workspace-relative path (never absolute, never `..`-escaping).
    pub path: String,
    pub blob_sha: String,
    /// 1-based line number.
    #[serde(default)]
    pub line: u32,
    /// 0-based BYTE offset into that line. Ignored by `/lip/symbols`
    /// (whole-document request).
    #[serde(default)]
    pub col: u32,
}

/// `POST /lip/diagnostics` request shape — no position (design-addendum-2.md
/// §D: `{path, blob_sha}`, whole-document).
#[derive(Debug, Deserialize)]
pub struct DiagnosticsRequest {
    pub path: String,
    pub blob_sha: String,
}

/// `POST /lip/code-actions` request shape (S2-C, design-s2.md — pinned
/// wire shape, do not improvise fields). `start_line`/`start_col` use the
/// same byte-col codec as every other lip/1 endpoint (1-based line,
/// 0-based byte column); `end_line`/`end_col` are optional and default to
/// `start_line`/`start_col` (a point range); `kinds` is optional and maps
/// to LSP's `CodeActionContext.only`.
#[derive(Debug, Deserialize)]
pub struct CodeActionsRequest {
    pub path: String,
    pub blob_sha: String,
    #[serde(default)]
    pub start_line: u32,
    #[serde(default)]
    pub start_col: u32,
    #[serde(default)]
    pub end_line: Option<u32>,
    #[serde(default)]
    pub end_col: Option<u32>,
    #[serde(default)]
    pub kinds: Option<Vec<String>>,
}

async fn identity(State(sup): State<Arc<Supervisor>>) -> Json<Value> {
    let id = sup.identity().await;
    Json(json!({
        "protocol": "lip/1",
        "lip_major": 1,
        "langs": id.langs,
        "server_name": id.server_name,
        "server_version": id.server_version,
        "workspace_root": id.workspace_root,
        "pid": id.pid,
        "uptime_secs": id.uptime_secs,
        "healthy": id.healthy,
        "indexing": id.indexing,
        "diagnostics_mode": id.diagnostics_mode,
        "capabilities": LIP_CAPABILITIES,
    }))
}

async fn hover(State(sup): State<Arc<Supervisor>>, Json(req): Json<LipRequest>) -> Json<Value> {
    match guarded_request(&sup, &req, "hover", "textDocument/hover", true).await {
        Ok(g) => {
            let results = translate_hover(&g.raw_result, &g.queried_bytes);
            Json(ok_envelope(&g.verified_blob_sha, results))
        }
        Err(r) => Json(r.into_json()),
    }
}

async fn definition(
    State(sup): State<Arc<Supervisor>>,
    Json(req): Json<LipRequest>,
) -> Json<Value> {
    match guarded_request(&sup, &req, "definition", "textDocument/definition", true).await {
        Ok(g) => {
            let results = translate_locations(
                &g.raw_result,
                &g.queried_uri,
                &g.queried_bytes,
                sup.workspace_root(),
                usize::MAX,
            );
            Json(ok_envelope(&g.verified_blob_sha, results))
        }
        Err(r) => Json(r.into_json()),
    }
}

async fn references(
    State(sup): State<Arc<Supervisor>>,
    Json(req): Json<LipRequest>,
) -> Json<Value> {
    match guarded_request(&sup, &req, "references", "textDocument/references", true).await {
        Ok(g) => {
            let results = translate_locations(
                &g.raw_result,
                &g.queried_uri,
                &g.queried_bytes,
                sup.workspace_root(),
                REFERENCES_CAP,
            );
            Json(ok_envelope(&g.verified_blob_sha, results))
        }
        Err(r) => Json(r.into_json()),
    }
}

async fn symbols(State(sup): State<Arc<Supervisor>>, Json(req): Json<LipRequest>) -> Json<Value> {
    match guarded_request(&sup, &req, "symbols", "textDocument/documentSymbol", false).await {
        Ok(g) => {
            let results = translate_symbols(&g.raw_result, &g.queried_bytes);
            Json(ok_envelope(&g.verified_blob_sha, results))
        }
        Err(r) => Json(r.into_json()),
    }
}

async fn diagnostics(
    State(sup): State<Arc<Supervisor>>,
    Json(req): Json<DiagnosticsRequest>,
) -> Json<Value> {
    match guarded_diagnostics(&sup, &req).await {
        Ok(g) => {
            let results = translate_diagnostics(&g.raw_result, &g.queried_bytes);
            Json(ok_envelope(&g.verified_blob_sha, results))
        }
        Err(r) => Json(r.into_json()),
    }
}

async fn code_actions(
    State(sup): State<Arc<Supervisor>>,
    Json(req): Json<CodeActionsRequest>,
) -> Json<Value> {
    match guarded_code_actions(&sup, &req).await {
        Ok(g) => Json(json!({
            "verified_blob_sha": g.verified_blob_sha,
            "results": g.results,
            "dropped_command_only": g.dropped_command_only,
            "dropped_unsupported": g.dropped_unsupported,
        })),
        Err(r) => Json(r.into_json()),
    }
}

fn ok_envelope(verified_blob_sha: &str, results: Value) -> Value {
    json!({"verified_blob_sha": verified_blob_sha, "results": results})
}

#[derive(Debug)]
enum Refusal {
    BlobMismatch {
        have: String,
        want: String,
    },
    ServerDown,
    Unsupported,
    /// The LSP child has outstanding `$/progress` work (e.g. a cold-boot
    /// workspace index) — see `lsp::LspClient::is_indexing`'s doc comment
    /// for the confirmed real-world finding this closes: querying too
    /// early silently returned a null/empty result even for a plain
    /// intra-file position. An honest refusal, not a guessed answer.
    Indexing,
}

impl Refusal {
    fn into_json(self) -> Value {
        match self {
            Refusal::BlobMismatch { have, want } => {
                json!({"refused": "blob_mismatch", "have": have, "want": want})
            }
            Refusal::ServerDown => json!({"refused": "server_down"}),
            Refusal::Unsupported => json!({"refused": "unsupported"}),
            Refusal::Indexing => json!({"refused": "indexing"}),
        }
    }
}

struct Guarded {
    verified_blob_sha: String,
    queried_uri: String,
    queried_bytes: Vec<u8>,
    raw_result: Value,
}

/// The blob guard + doc-sync + request pipeline shared by all four POST
/// routes: resolve → hash (pre) → ensure-open/synced with those SAME
/// bytes → LSP request → hash (post). Either hash mismatch is a REFUSAL,
/// never a downgrade (design-lip.md's "THE BLOB GUARD").
async fn guarded_request(
    sup: &Supervisor,
    req: &LipRequest,
    lip_method: &str,
    lsp_method: &str,
    include_position: bool,
) -> Result<Guarded, Refusal> {
    let abs_path =
        resolve_workspace_path(sup.workspace_root(), &req.path).ok_or(Refusal::Unsupported)?;
    let pre = blob::read_and_hash(&abs_path).map_err(|_| Refusal::Unsupported)?;
    if pre.sha != req.blob_sha {
        return Err(Refusal::BlobMismatch {
            have: pre.sha,
            want: req.blob_sha.clone(),
        });
    }

    let uri = lsp::path_to_file_uri(&abs_path);
    let language_id = sup
        .config()
        .lang_ids
        .first()
        .cloned()
        .unwrap_or_else(|| "plaintext".to_string());

    let mut params = json!({"textDocument": {"uri": uri.clone()}});
    if include_position {
        let line_text = nth_line(&pre.bytes, req.line).ok_or(Refusal::Unsupported)?;
        let character = position::byte_col_to_utf16(line_text, req.col as usize);
        params["position"] = json!({
            "line": position::line_to_lsp(req.line),
            "character": character,
        });
        if lsp_method == "textDocument/references" {
            params["context"] = json!({"includeDeclaration": true});
        }
    }

    let client = sup.client().await.ok_or(Refusal::ServerDown)?;
    if client.is_indexing() {
        return Err(Refusal::Indexing);
    }
    if client
        .ensure_doc_open(&uri, &pre, &language_id)
        .await
        .is_err()
    {
        sup.report_failure(&client).await;
        return Err(Refusal::ServerDown);
    }

    let raw_result = match client
        .request_if_supported(lip_method, lsp_method, params)
        .await
    {
        None => return Err(Refusal::Unsupported),
        Some(Ok(v)) => v,
        Some(Err(_e)) => {
            sup.report_failure(&client).await;
            return Err(Refusal::ServerDown);
        }
    };

    let post = blob::read_and_hash(&abs_path).map_err(|_| Refusal::Unsupported)?;
    if post.sha != req.blob_sha {
        return Err(Refusal::BlobMismatch {
            have: post.sha,
            want: req.blob_sha.clone(),
        });
    }

    Ok(Guarded {
        verified_blob_sha: post.sha,
        queried_uri: uri,
        queried_bytes: post.bytes,
        raw_result,
    })
}

/// `/lip/diagnostics`'s own pipeline (design-addendum-2.md §D, amended
/// PRR-L5): resolve → hash (pre) → indexing gate → ensure-open (which
/// marks the doc dirty in the [`crate::lsp`] PUSH cache if this is a fresh
/// open/change — harmless in PULL mode, which never reads that flag) →
/// fetch diagnostics → hash (post). The fetch step branches on
/// [`crate::lsp::ServerInfo::diagnostics_mode`]:
///
/// - **pull** (`diagnosticProvider` advertised at `initialize`): sends a
///   live `textDocument/diagnostic` request every call
///   (`LspClient::pull_diagnostics`) — the LSP 3.17 pull protocol IS a
///   request/response, so there's no cache to wait on; `previousResultId`
///   lets an unchanged server answer cheaply instead of resending the same
///   diagnostics. A malformed reply is treated exactly like any other
///   failed LSP request (`refused: "server_down"`).
/// - **push** (unchanged from before PRR-L5): WAITS on the adapter's own
///   push cache (bounded by `config.diagnostics_wait_ms`, capped at
///   [`crate::config::Config::MAX_DIAGNOSTICS_WAIT_MS`]) — `ensure_doc_open`
///   (which triggers the server to analyze and publish) plus the cache
///   wait IS the round trip; never a synchronous LSP request. A timeout
///   with nothing published is `Ok` with an empty results array, never a
///   `Refusal` — "empty-after-wait = valid {diagnostics: []}, never a
///   refusal".
async fn guarded_diagnostics(
    sup: &Supervisor,
    req: &DiagnosticsRequest,
) -> Result<Guarded, Refusal> {
    let abs_path =
        resolve_workspace_path(sup.workspace_root(), &req.path).ok_or(Refusal::Unsupported)?;
    let pre = blob::read_and_hash(&abs_path).map_err(|_| Refusal::Unsupported)?;
    if pre.sha != req.blob_sha {
        return Err(Refusal::BlobMismatch {
            have: pre.sha,
            want: req.blob_sha.clone(),
        });
    }

    let uri = lsp::path_to_file_uri(&abs_path);
    let language_id = sup
        .config()
        .lang_ids
        .first()
        .cloned()
        .unwrap_or_else(|| "plaintext".to_string());

    let client = sup.client().await.ok_or(Refusal::ServerDown)?;
    if client.is_indexing() {
        return Err(Refusal::Indexing);
    }
    if client
        .ensure_doc_open(&uri, &pre, &language_id)
        .await
        .is_err()
    {
        sup.report_failure(&client).await;
        return Err(Refusal::ServerDown);
    }

    let diags = if client.server_info.diagnostics_mode() == "pull" {
        match client.pull_diagnostics(&uri).await {
            Ok(items) => items,
            Err(_e) => {
                sup.report_failure(&client).await;
                return Err(Refusal::ServerDown);
            }
        }
    } else {
        let wait = std::time::Duration::from_millis(sup.config().diagnostics_wait_ms);
        client.diagnostics_for(&uri, wait).await
    };

    let post = blob::read_and_hash(&abs_path).map_err(|_| Refusal::Unsupported)?;
    if post.sha != req.blob_sha {
        return Err(Refusal::BlobMismatch {
            have: post.sha,
            want: req.blob_sha.clone(),
        });
    }

    Ok(Guarded {
        verified_blob_sha: post.sha,
        queried_uri: uri,
        queried_bytes: post.bytes,
        raw_result: Value::Array(diags),
    })
}

/// `/lip/code-actions`'s own result shape — distinct from [`Guarded`]
/// because the translation work (per-action `codeAction/resolve` round
/// trips, `WorkspaceEdit` translation, the two drop counters) happens
/// INSIDE the guard function itself rather than in the route handler, so
/// there is no single `raw_result` left to hand back untranslated.
struct CodeActionsGuarded {
    verified_blob_sha: String,
    /// Already-translated `results` array (lip/1 wire shape).
    results: Value,
    /// Actions with neither `.edit` nor a resolvable edit (a bare
    /// `Command`, or a `CodeAction` with no `data`/no resolve support) —
    /// dropped, never executed (kb-lip never calls
    /// `workspace/executeCommand`).
    dropped_command_only: usize,
    /// Actions whose edit named a `Create`/`Rename`/`DeleteFile` resource
    /// operation, or a `TextDocumentEdit` targeting a URI outside
    /// `workspace_root` — dropped, never applied.
    dropped_unsupported: usize,
}

/// `/lip/code-actions`'s own pipeline (S2-C, design-s2.md; "diagnostics
/// precedent" — its own guard fn rather than contorting
/// [`guarded_request`]): resolve → hash (pre) → indexing gate →
/// `ensure_doc_open` → build `context.diagnostics` ITSELF (pull: a live
/// `textDocument/diagnostic` fetch filtered to the requested range; push:
/// a non-waiting [`crate::lsp::LspClient::diagnostics_snapshot`] read,
/// same filter — never translated to lip/1 shape, LSP diagnostics stay
/// LSP-native the whole way since they're only ever OUTGOING context here)
/// → capability-gated `textDocument/codeAction`
/// (`crate::lsp::capability_key_for("code-actions")` ==
/// `"codeActionProvider"`) → for each returned action lacking `.edit`,
/// when [`crate::lsp::ServerInfo::code_action_resolve_supported`] and the
/// action carries `data`: a `codeAction/resolve` round trip → hash (post)
/// ONCE after ALL of the above round trips → a post-hash mismatch REFUSES
/// the WHOLE response (`blob_mismatch`), discarding every action already
/// translated — never a partial answer against content that moved
/// mid-request.
async fn guarded_code_actions(
    sup: &Supervisor,
    req: &CodeActionsRequest,
) -> Result<CodeActionsGuarded, Refusal> {
    let abs_path =
        resolve_workspace_path(sup.workspace_root(), &req.path).ok_or(Refusal::Unsupported)?;
    let pre = blob::read_and_hash(&abs_path).map_err(|_| Refusal::Unsupported)?;
    if pre.sha != req.blob_sha {
        return Err(Refusal::BlobMismatch {
            have: pre.sha,
            want: req.blob_sha.clone(),
        });
    }

    let uri = lsp::path_to_file_uri(&abs_path);
    let language_id = sup
        .config()
        .lang_ids
        .first()
        .cloned()
        .unwrap_or_else(|| "plaintext".to_string());

    let client = sup.client().await.ok_or(Refusal::ServerDown)?;
    if client.is_indexing() {
        return Err(Refusal::Indexing);
    }
    if client
        .ensure_doc_open(&uri, &pre, &language_id)
        .await
        .is_err()
    {
        sup.report_failure(&client).await;
        return Err(Refusal::ServerDown);
    }

    let (start_line, start_col, end_line, end_col) = resolve_range_bounds(req);
    let start_line_text = nth_line(&pre.bytes, start_line).ok_or(Refusal::Unsupported)?;
    let end_line_text = nth_line(&pre.bytes, end_line).ok_or(Refusal::Unsupported)?;
    let lsp_range = json!({
        "start": {
            "line": position::line_to_lsp(start_line),
            "character": position::byte_col_to_utf16(start_line_text, start_col as usize),
        },
        "end": {
            "line": position::line_to_lsp(end_line),
            "character": position::byte_col_to_utf16(end_line_text, end_col as usize),
        },
    });

    let diagnostics = context_diagnostics(&client, &uri, &lsp_range).await;
    let mut context = json!({"diagnostics": diagnostics});
    if let Some(kinds) = &req.kinds {
        context["only"] = json!(kinds);
    }

    let params = json!({
        "textDocument": {"uri": uri.clone()},
        "range": lsp_range,
        "context": context,
    });

    let raw_result = match client
        .request_if_supported("code-actions", "textDocument/codeAction", params)
        .await
    {
        None => return Err(Refusal::Unsupported),
        Some(Ok(v)) => v,
        Some(Err(_e)) => {
            sup.report_failure(&client).await;
            return Err(Refusal::ServerDown);
        }
    };
    let items: Vec<Value> = match raw_result {
        Value::Array(a) => a,
        Value::Null => vec![],
        other => vec![other],
    };

    let resolve_supported = client.server_info.code_action_resolve_supported();
    let mut results = Vec::new();
    let mut dropped_command_only = 0usize;
    let mut dropped_unsupported = 0usize;

    for item in items {
        let edit = match item.get("edit").filter(|e| !e.is_null()) {
            Some(edit) => Some(edit.clone()),
            None if resolve_supported && item.get("data").is_some() => {
                match client.resolve_code_action(item.clone()).await {
                    Ok(resolved) => resolved.get("edit").filter(|e| !e.is_null()).cloned(),
                    Err(_e) => {
                        sup.report_failure(&client).await;
                        return Err(Refusal::ServerDown);
                    }
                }
            }
            None => None,
        };

        let Some(edit) = edit else {
            dropped_command_only += 1;
            continue;
        };

        match translate_workspace_edit(&edit, &uri, &pre.bytes, sup.workspace_root()) {
            EditTranslation::Edits(edits) if !edits.is_empty() => {
                let title = item
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let kind = item.get("kind").and_then(Value::as_str);
                let is_preferred = item
                    .get("isPreferred")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                results.push(json!({
                    "title": title,
                    "kind": kind,
                    "is_preferred": is_preferred,
                    "edits": edits,
                }));
            }
            EditTranslation::Edits(_) => dropped_command_only += 1,
            EditTranslation::Unsupported => dropped_unsupported += 1,
        }
    }

    // The hash-post check runs ONCE here, after every codeAction/resolve
    // round trip above has completed — never per-action — so a file that
    // changed mid-resolve refuses the WHOLE response rather than mixing
    // stale and fresh actions.
    let post = blob::read_and_hash(&abs_path).map_err(|_| Refusal::Unsupported)?;
    if post.sha != req.blob_sha {
        return Err(Refusal::BlobMismatch {
            have: post.sha,
            want: req.blob_sha.clone(),
        });
    }

    Ok(CodeActionsGuarded {
        verified_blob_sha: post.sha,
        results: json!(results),
        dropped_command_only,
        dropped_unsupported,
    })
}

/// `(start_line, start_col, end_line, end_col)` for a code-actions
/// request — `end_line`/`end_col` default to `start_line`/`start_col`
/// (a point range) when the caller omits them, per design-s2.md's pinned
/// wire shape.
fn resolve_range_bounds(req: &CodeActionsRequest) -> (u32, u32, u32, u32) {
    let end_line = req.end_line.unwrap_or(req.start_line);
    let end_col = req.end_col.unwrap_or(req.start_col);
    (req.start_line, req.start_col, end_line, end_col)
}

/// Builds `context.diagnostics` for `/lip/code-actions` ENTIRELY inside
/// kb-lip (design-s2.md §S2-C: "No reverse wire translation, the
/// diagnostics never leave LSP-native shape") — the returned items are
/// raw LSP `Diagnostic` objects, passed straight into the outgoing
/// `textDocument/codeAction` request's `context.diagnostics`, filtered to
/// those intersecting `range` (also an LSP-native `{start, end}` object).
/// Pull mode issues a live `textDocument/diagnostic` fetch; a failure
/// there degrades to an EMPTY diagnostics context rather than refusing
/// the whole code-actions request — a missing diagnostics hint just means
/// a server may offer fewer/less-targeted quick fixes, not a functional
/// failure worth a refusal. Push mode reads the adapter's own cache
/// WITHOUT waiting (`LspClient::diagnostics_snapshot`) — code-actions must
/// never delay on a publish that may never land. Any other
/// `diagnostics_mode()` value (there is none today — see that fn's doc
/// comment) degrades to empty the same way.
async fn context_diagnostics(client: &lsp::LspClient, uri: &str, range: &Value) -> Vec<Value> {
    let all = match client.server_info.diagnostics_mode() {
        "pull" => client.pull_diagnostics(uri).await.unwrap_or_default(),
        "push" => client.diagnostics_snapshot(uri),
        _ => Vec::new(),
    };
    all.into_iter()
        .filter(|d| diagnostic_intersects_range(d, range))
        .collect()
}

/// Parses an LSP `Range`-shaped `{start: {line, character}, end: {line,
/// character}}` object into `((start_line, start_char), (end_line,
/// end_char))`. Returns `None` on any missing/malformed field — a
/// diagnostic (or query range) that doesn't parse is treated as
/// non-intersecting rather than guessed at.
fn parse_lsp_range(v: &Value) -> Option<((u32, u32), (u32, u32))> {
    let start = v.get("start")?;
    let end = v.get("end")?;
    let sl = start.get("line")?.as_u64()? as u32;
    let sc = start.get("character")?.as_u64()? as u32;
    let el = end.get("line")?.as_u64()? as u32;
    let ec = end.get("character")?.as_u64()? as u32;
    Some(((sl, sc), (el, ec)))
}

/// Half-open-interval-free range overlap over `(line, character)` pairs
/// compared lexicographically: `a` and `b` intersect iff `a.start <=
/// b.end` AND `b.start <= a.end`. Touching-but-not-overlapping ranges
/// (e.g. `a.end == b.start`) count as intersecting — matching a
/// zero-width diagnostic landing exactly on a point-range request.
fn ranges_intersect(a: ((u32, u32), (u32, u32)), b: ((u32, u32), (u32, u32))) -> bool {
    fn le(x: (u32, u32), y: (u32, u32)) -> bool {
        x.0 < y.0 || (x.0 == y.0 && x.1 <= y.1)
    }
    le(a.0, b.1) && le(b.0, a.1)
}

fn diagnostic_intersects_range(diagnostic: &Value, range: &Value) -> bool {
    let Some(d_range) = diagnostic.get("range").and_then(parse_lsp_range) else {
        return false;
    };
    let Some(q_range) = parse_lsp_range(range) else {
        return false;
    };
    ranges_intersect(d_range, q_range)
}

/// The result of translating one LSP `WorkspaceEdit` into lip/1's
/// per-file edit shape.
enum EditTranslation {
    /// Every entry in the `WorkspaceEdit` translated cleanly (possibly to
    /// an EMPTY vec, e.g. an `edit: {}` with neither `changes` nor
    /// `documentChanges` — the caller treats that the same as
    /// command-only, since there's nothing to apply).
    Edits(Vec<Value>),
    /// The edit named a `Create`/`Rename`/`DeleteFile` resource operation,
    /// or a `TextDocumentEdit`/`changes` entry targeted a URI outside
    /// `workspace_root` (or a non-`file://` URI) — dropped as a whole,
    /// never partially applied.
    Unsupported,
}

/// Translates one LSP `WorkspaceEdit` (`changes` and/or `documentChanges`,
/// `TextDocumentEdit` entries only — see [`EditTranslation::Unsupported`])
/// into lip/1's `{path, edits: [{start_line, start_col, end_line, end_col,
/// new_text}]}` per-file shape. `queried_uri`/`queried_bytes` are the
/// already-hashed content of the file the caller asked about — reused
/// verbatim for edits touching that SAME uri; any OTHER uri's bytes are a
/// best-effort fresh read (same cross-file posture as
/// `translate_locations`'s own fallback — not blob-guarded, a documented
/// v1 limitation).
fn translate_workspace_edit(
    edit: &Value,
    queried_uri: &str,
    queried_bytes: &[u8],
    workspace_root: &Path,
) -> EditTranslation {
    let mut per_file: Vec<(String, Vec<Value>)> = Vec::new();

    if let Some(changes) = edit.get("changes").and_then(Value::as_object) {
        for (uri, edits_val) in changes {
            let Some(path) = uri_within_workspace(uri, workspace_root) else {
                return EditTranslation::Unsupported;
            };
            let Some(text_edits) = edits_val.as_array() else {
                return EditTranslation::Unsupported;
            };
            let bytes = bytes_for_uri(uri, queried_uri, queried_bytes);
            per_file.push((path, translate_text_edits(text_edits, &bytes)));
        }
    }

    if let Some(doc_changes) = edit.get("documentChanges").and_then(Value::as_array) {
        for change in doc_changes {
            // A `CreateFile`/`RenameFile`/`DeleteFile` resource operation
            // carries a `kind` field (`"create"|"rename"|"delete"`); a
            // plain `TextDocumentEdit` never does — kb-lip only ever
            // materializes the latter.
            if change.get("kind").and_then(Value::as_str).is_some() {
                return EditTranslation::Unsupported;
            }
            let Some(uri) = change
                .get("textDocument")
                .and_then(|t| t.get("uri"))
                .and_then(Value::as_str)
            else {
                return EditTranslation::Unsupported;
            };
            let Some(path) = uri_within_workspace(uri, workspace_root) else {
                return EditTranslation::Unsupported;
            };
            let text_edits = change
                .get("edits")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let bytes = bytes_for_uri(uri, queried_uri, queried_bytes);
            per_file.push((path, translate_text_edits(&text_edits, &bytes)));
        }
    }

    EditTranslation::Edits(
        per_file
            .into_iter()
            .filter(|(_, edits)| !edits.is_empty())
            .map(|(path, edits)| json!({"path": path, "edits": edits}))
            .collect(),
    )
}

/// The bytes to translate a `TextEdit`'s UTF-16 range against for `uri`:
/// the caller's own already-blob-guarded bytes when `uri` is the queried
/// file, else a best-effort fresh read (empty on any read failure — a
/// malformed cross-file edit degrades to byte-offset `0`s rather than
/// panicking or refusing the whole response).
fn bytes_for_uri(uri: &str, queried_uri: &str, queried_bytes: &[u8]) -> Vec<u8> {
    if uri == queried_uri {
        queried_bytes.to_vec()
    } else {
        file_uri_to_path(uri)
            .and_then(|p| std::fs::read(p).ok())
            .unwrap_or_default()
    }
}

/// Resolves `uri` to a workspace-relative display path, or `None` if it
/// isn't a `file://` URI under `workspace_root` at all — the strict
/// counterpart to [`uri_to_display_path`]'s lenient fallback-to-absolute
/// (that one is for read-only result display; this one gates whether an
/// edit is even eligible to be materialized).
fn uri_within_workspace(uri: &str, workspace_root: &Path) -> Option<String> {
    let path = file_uri_to_path(uri)?;
    let rel = path.strip_prefix(workspace_root).ok()?;
    Some(rel.to_string_lossy().into_owned())
}

/// Converts one file's `TextEdit[]` (`{range, newText}`) into lip/1's
/// `{start_line, start_col, end_line, end_col, new_text}` shape. Entries
/// with no `range` are dropped individually (malformed — never surfaced
/// rather than guessed at), matching `translate_diagnostics`'s own
/// filter_map posture.
fn translate_text_edits(text_edits: &[Value], bytes: &[u8]) -> Vec<Value> {
    text_edits
        .iter()
        .filter_map(|e| {
            let range = e.get("range")?;
            let (sl, sc) = lsp_position_to_byte(&range["start"], bytes);
            let (el, ec) = lsp_position_to_byte(&range["end"], bytes);
            let new_text = e.get("newText").and_then(Value::as_str).unwrap_or_default();
            Some(json!({
                "start_line": sl, "start_col": sc,
                "end_line": el, "end_col": ec,
                "new_text": new_text,
            }))
        })
        .collect()
}

/// Resolve a caller-supplied workspace-relative `rel` path against
/// `root`, rejecting anything absolute or that escapes the root via `..`
/// or a root component — never trust a client-controlled path straight
/// into `root.join(rel)`.
fn resolve_workspace_path(root: &Path, rel: &str) -> Option<PathBuf> {
    let relp = Path::new(rel);
    if relp.is_absolute() {
        return None;
    }
    let mut resolved = root.to_path_buf();
    for comp in relp.components() {
        match comp {
            std::path::Component::Normal(c) => resolved.push(c),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => return None,
        }
    }
    Some(resolved)
}

fn nth_line(bytes: &[u8], line_1based: u32) -> Option<&str> {
    let text = std::str::from_utf8(bytes).ok()?;
    let idx = line_1based.checked_sub(1)?;
    text.lines().nth(idx as usize)
}

fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    Some(PathBuf::from(rest.replace("%20", " ")))
}

/// Best-effort workspace-relative path for a result location: strips
/// `workspace_root` if the uri resolves under it, else falls back to the
/// raw uri string (e.g. a definition landing in a vendored/external file
/// outside the workspace).
fn uri_to_display_path(uri: &str, workspace_root: &Path) -> String {
    match file_uri_to_path(uri) {
        Some(p) => match p.strip_prefix(workspace_root) {
            Ok(rel) => rel.to_string_lossy().into_owned(),
            Err(_) => p.to_string_lossy().into_owned(),
        },
        None => uri.to_string(),
    }
}

/// Convert one LSP `Position` (0-based line, UTF-16 column) back into
/// lip/1's (1-based line, byte column) shape. `bytes` must be the content
/// of the document that position is IN — the queried file's own bytes for
/// a same-file position (hover's range, or a definition/reference that
/// landed back in the queried file), or a best-effort fresh read for a
/// cross-file result (documented v1 limitation: not blob-guarded).
fn lsp_position_to_byte(pos: &Value, bytes: &[u8]) -> (u32, u32) {
    let line0 = pos.get("line").and_then(Value::as_u64).unwrap_or(0) as u32;
    let character = pos.get("character").and_then(Value::as_u64).unwrap_or(0) as u32;
    let line1 = position::line_from_lsp(line0);
    let byte_col = nth_line(bytes, line1)
        .map(|t| position::utf16_col_to_byte(t, character) as u32)
        .unwrap_or(character);
    (line1, byte_col)
}

fn extract_hover_contents(contents: &Value) -> String {
    match contents {
        Value::String(s) => s.clone(),
        Value::Object(o) => o
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        Value::Array(items) => items
            .iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(s.clone()),
                Value::Object(o) => o.get("value").and_then(Value::as_str).map(str::to_string),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

fn translate_hover(result: &Value, queried_bytes: &[u8]) -> Value {
    if result.is_null() {
        return json!([]);
    }
    let contents = extract_hover_contents(&result["contents"]);
    let range = result.get("range").filter(|r| !r.is_null()).map(|r| {
        let (sl, sc) = lsp_position_to_byte(&r["start"], queried_bytes);
        let (el, ec) = lsp_position_to_byte(&r["end"], queried_bytes);
        json!({"line": sl, "col": sc, "end_line": el, "end_col": ec})
    });
    json!([{"contents": contents, "range": range}])
}

/// Extracts `(uri, range)` from one `textDocument/definition` or
/// `textDocument/references` result item. LSP's `textDocument/definition`
/// response may legally take THREE shapes (the spec's `Definition |
/// DefinitionLink[] | null` for definition, `Location[] | null` for
/// references — but many servers, ruby-lsp included, use the same
/// `Location`/`LocationLink` union for both regardless of the client's
/// advertised `linkSupport`):
/// - `Location` (`{uri, range}`) — the pre-3.14 shape, and what kb-lip's
///   own `initialize` capabilities request (never sends `linkSupport:
///   true`).
/// - `LocationLink` (`{targetUri, targetRange, targetSelectionRange,
///   originSelectionRange?}`) — legal for a server to reply with
///   regardless of client capabilities (confirmed live against ruby-lsp
///   0.26.11, PRR-L3 smoke); `targetSelectionRange` is preferred over the
///   wider `targetRange` for the reported position when both are present
///   (the narrower "this is the symbol" span vs. the full browsable
///   extent, e.g. a whole method body) — falls back to `targetRange` if
///   `targetSelectionRange` is absent.
///
/// A bare single item (non-array `result`) is handled by the caller
/// collecting `result` itself into a one-element list before this runs.
fn extract_uri_and_range(loc: &Value) -> Option<(&str, &Value)> {
    if let Some(uri) = loc.get("uri").and_then(Value::as_str) {
        if let Some(range) = loc.get("range") {
            return Some((uri, range));
        }
    }
    let uri = loc.get("targetUri").and_then(Value::as_str)?;
    let range = loc
        .get("targetSelectionRange")
        .or_else(|| loc.get("targetRange"))?;
    Some((uri, range))
}

fn translate_locations(
    result: &Value,
    queried_uri: &str,
    queried_bytes: &[u8],
    workspace_root: &Path,
    cap: usize,
) -> Value {
    let items: Vec<&Value> = match result {
        Value::Array(a) => a.iter().collect(),
        Value::Null => vec![],
        other => vec![other], // some servers reply with a single Location(Link)
    };
    let translated: Vec<Value> = items
        .into_iter()
        .take(cap)
        .filter_map(|loc| {
            let (uri, range) = extract_uri_and_range(loc)?;
            let owned_bytes;
            let bytes_ref: &[u8] = if uri == queried_uri {
                queried_bytes
            } else {
                owned_bytes = file_uri_to_path(uri).and_then(|p| std::fs::read(p).ok());
                owned_bytes.as_deref().unwrap_or(&[])
            };
            let (sl, sc) = lsp_position_to_byte(&range["start"], bytes_ref);
            let (el, ec) = lsp_position_to_byte(&range["end"], bytes_ref);
            Some(json!({
                "path": uri_to_display_path(uri, workspace_root),
                "line": sl, "col": sc, "end_line": el, "end_col": ec
            }))
        })
        .collect();
    json!(translated)
}

/// Flattens LSP `DocumentSymbol[]` (nested via `children`) — or the older
/// `SymbolInformation[]` shape (`location` instead of `range`) — into a
/// flat lip/1 symbol list.
fn translate_symbols(result: &Value, queried_bytes: &[u8]) -> Value {
    let mut out = Vec::new();
    if let Value::Array(items) = result {
        flatten_symbols(items, queried_bytes, &mut out);
    }
    json!(out)
}

fn flatten_symbols(items: &[Value], queried_bytes: &[u8], out: &mut Vec<Value>) {
    for item in items {
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let kind = item.get("kind").and_then(Value::as_i64).unwrap_or(0);
        let range = item
            .get("range")
            .or_else(|| item.get("location").and_then(|l| l.get("range")));
        if let Some(range) = range {
            let (sl, sc) = lsp_position_to_byte(&range["start"], queried_bytes);
            let (el, ec) = lsp_position_to_byte(&range["end"], queried_bytes);
            out.push(json!({
                "name": name, "kind": kind,
                "line": sl, "col": sc, "end_line": el, "end_col": ec
            }));
        }
        if let Some(Value::Array(children)) = item.get("children") {
            flatten_symbols(children, queried_bytes, out);
        }
    }
}

/// Converts the [`crate::lsp`] diagnostics cache's raw LSP `Diagnostic[]`
/// (`{range, severity?, code?, source?, message}`) into lip/1's 1-based
/// byte-column shape. Diagnostics with no `range` are dropped (malformed
/// per the LSP spec — never surfaced rather than guessed at); everything
/// else is best-effort passthrough (`severity`/`code`/`source` default to
/// `null` when the server omits them).
fn translate_diagnostics(result: &Value, queried_bytes: &[u8]) -> Value {
    let items: Vec<&Value> = match result {
        Value::Array(a) => a.iter().collect(),
        _ => vec![],
    };
    let translated: Vec<Value> = items
        .into_iter()
        .filter_map(|d| {
            let range = d.get("range")?;
            let (sl, sc) = lsp_position_to_byte(&range["start"], queried_bytes);
            let (el, ec) = lsp_position_to_byte(&range["end"], queried_bytes);
            Some(json!({
                "line": sl, "col": sc, "end_line": el, "end_col": ec,
                "severity": d.get("severity").cloned().unwrap_or(Value::Null),
                "code": d.get("code").cloned().unwrap_or(Value::Null),
                "source": d.get("source").cloned().unwrap_or(Value::Null),
                "message": d.get("message").and_then(Value::as_str).unwrap_or_default(),
            }))
        })
        .collect();
    json!(translated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_workspace_path_accepts_a_normal_relative_path() {
        let root = Path::new("/repo");
        assert_eq!(
            resolve_workspace_path(root, "lib/foo.rb"),
            Some(PathBuf::from("/repo/lib/foo.rb"))
        );
        assert_eq!(
            resolve_workspace_path(root, "./lib/foo.rb"),
            Some(PathBuf::from("/repo/lib/foo.rb"))
        );
    }

    #[test]
    fn resolve_workspace_path_rejects_absolute_paths() {
        let root = Path::new("/repo");
        assert_eq!(resolve_workspace_path(root, "/etc/passwd"), None);
    }

    #[test]
    fn resolve_workspace_path_rejects_parent_dir_escape() {
        let root = Path::new("/repo");
        assert_eq!(resolve_workspace_path(root, "../secret"), None);
        assert_eq!(resolve_workspace_path(root, "lib/../../secret"), None);
    }

    #[test]
    fn nth_line_is_1_based_and_bounds_checked() {
        let bytes = b"one\ntwo\nthree";
        assert_eq!(nth_line(bytes, 1), Some("one"));
        assert_eq!(nth_line(bytes, 3), Some("three"));
        assert_eq!(nth_line(bytes, 0), None);
        assert_eq!(nth_line(bytes, 99), None);
    }

    #[test]
    fn file_uri_to_path_decodes_spaces() {
        assert_eq!(
            file_uri_to_path("file:///home/me/my%20repo/f.rb"),
            Some(PathBuf::from("/home/me/my repo/f.rb"))
        );
        assert_eq!(file_uri_to_path("not-a-uri"), None);
    }

    #[test]
    fn uri_to_display_path_strips_workspace_root() {
        let root = Path::new("/repo");
        assert_eq!(
            uri_to_display_path("file:///repo/lib/foo.rb", root),
            "lib/foo.rb"
        );
        // Outside the workspace: falls back to the absolute path.
        assert_eq!(
            uri_to_display_path("file:///gems/other/foo.rb", root),
            "/gems/other/foo.rb"
        );
    }

    #[test]
    fn translate_hover_extracts_markdown_contents_and_range() {
        let result = json!({
            "contents": {"kind": "markdown", "value": "**Foo**"},
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 3}}
        });
        let bytes = b"foo bar";
        let out = translate_hover(&result, bytes);
        assert_eq!(out[0]["contents"], json!("**Foo**"));
        assert_eq!(out[0]["range"]["line"], json!(1));
        assert_eq!(out[0]["range"]["col"], json!(0));
        assert_eq!(out[0]["range"]["end_col"], json!(3));
    }

    #[test]
    fn translate_hover_null_result_is_empty() {
        assert_eq!(translate_hover(&Value::Null, b""), json!([]));
    }

    #[test]
    fn translate_locations_caps_at_the_given_limit() {
        let uri = "file:///repo/f.rb";
        let items: Vec<Value> = (0..250)
            .map(|i| {
                json!({"uri": uri, "range": {"start": {"line": i, "character": 0}, "end": {"line": i, "character": 1}}})
            })
            .collect();
        let result = Value::Array(items);
        let out = translate_locations(&result, uri, b"", Path::new("/repo"), REFERENCES_CAP);
        assert_eq!(out.as_array().unwrap().len(), REFERENCES_CAP);
    }

    #[test]
    fn translate_locations_reads_the_location_link_shape() {
        // ruby-lsp 0.26.11 replies to textDocument/definition with
        // LocationLink[] unconditionally (confirmed live, PRR-L3 smoke),
        // even though kb-lip never advertises
        // textDocument.definition.linkSupport. Before this fix,
        // translate_locations only read uri/range and silently dropped
        // every LocationLink item via filter_map's `?` — this pins the
        // regression.
        let uri = "file:///repo/f.rb";
        let result = json!([{
            "targetUri": uri,
            "targetRange": {"start": {"line": 0, "character": 0}, "end": {"line": 2, "character": 5}},
            "targetSelectionRange": {"start": {"line": 0, "character": 6}, "end": {"line": 0, "character": 9}}
        }]);
        let out = translate_locations(&result, uri, b"", Path::new("/repo"), usize::MAX);
        let arr = out.as_array().unwrap();
        assert_eq!(
            arr.len(),
            1,
            "LocationLink items must not be dropped: {out}"
        );
        assert_eq!(arr[0]["path"], json!("f.rb"));
        // targetSelectionRange (the narrower span) wins over targetRange.
        assert_eq!(arr[0]["col"], json!(6));
        assert_eq!(arr[0]["end_col"], json!(9));
    }

    #[test]
    fn translate_locations_location_link_falls_back_to_target_range_without_selection_range() {
        let uri = "file:///repo/f.rb";
        let result = json!([{
            "targetUri": uri,
            "targetRange": {"start": {"line": 4, "character": 1}, "end": {"line": 4, "character": 10}}
        }]);
        let out = translate_locations(&result, uri, b"", Path::new("/repo"), usize::MAX);
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["line"], json!(5));
        assert_eq!(arr[0]["col"], json!(1));
    }

    #[test]
    fn translate_locations_accepts_a_single_non_array_location() {
        // LSP's `Definition` result type permits a bare single `Location`
        // (not wrapped in an array) — some servers use this shape.
        let uri = "file:///repo/f.rb";
        let result = json!({
            "uri": uri,
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 3}}
        });
        let out = translate_locations(&result, uri, b"", Path::new("/repo"), usize::MAX);
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["path"], json!("f.rb"));
    }

    #[test]
    fn translate_locations_accepts_a_single_non_array_location_link() {
        let uri = "file:///repo/f.rb";
        let result = json!({
            "targetUri": uri,
            "targetSelectionRange": {"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 6}}
        });
        let out = translate_locations(&result, uri, b"", Path::new("/repo"), usize::MAX);
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["line"], json!(2));
        assert_eq!(arr[0]["col"], json!(2));
    }

    #[test]
    fn translate_symbols_flattens_nested_children() {
        let result = json!([{
            "name": "Outer", "kind": 5,
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 10, "character": 1}},
            "children": [{
                "name": "inner_method", "kind": 6,
                "range": {"start": {"line": 1, "character": 2}, "end": {"line": 3, "character": 3}}
            }]
        }]);
        let out = translate_symbols(&result, b"");
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["name"], json!("Outer"));
        assert_eq!(arr[1]["name"], json!("inner_method"));
    }

    #[test]
    fn translate_diagnostics_converts_range_and_passes_through_fields() {
        let result = json!([{
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 3}},
            "severity": 1,
            "code": "FAKE001",
            "source": "fake-lsp",
            "message": "fake diagnostic"
        }]);
        let out = translate_diagnostics(&result, b"foo bar");
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["line"], json!(1));
        assert_eq!(arr[0]["col"], json!(0));
        assert_eq!(arr[0]["end_col"], json!(3));
        assert_eq!(arr[0]["severity"], json!(1));
        assert_eq!(arr[0]["code"], json!("FAKE001"));
        assert_eq!(arr[0]["source"], json!("fake-lsp"));
        assert_eq!(arr[0]["message"], json!("fake diagnostic"));
    }

    #[test]
    fn translate_diagnostics_empty_result_is_empty_not_missing() {
        assert_eq!(translate_diagnostics(&json!([]), b""), json!([]));
        assert_eq!(translate_diagnostics(&Value::Null, b""), json!([]));
    }

    #[test]
    fn translate_diagnostics_drops_entries_with_no_range() {
        let result = json!([{"message": "no range, malformed"}]);
        assert_eq!(translate_diagnostics(&result, b""), json!([]));
    }

    // -------------------------------------------------------------------
    // /lip/code-actions — pure helpers
    // -------------------------------------------------------------------

    #[test]
    fn resolve_range_bounds_defaults_end_to_start_for_a_point_range() {
        let req = CodeActionsRequest {
            path: "f.rs".to_string(),
            blob_sha: "x".to_string(),
            start_line: 10,
            start_col: 4,
            end_line: None,
            end_col: None,
            kinds: None,
        };
        assert_eq!(resolve_range_bounds(&req), (10, 4, 10, 4));
    }

    #[test]
    fn resolve_range_bounds_keeps_an_explicit_end() {
        let req = CodeActionsRequest {
            path: "f.rs".to_string(),
            blob_sha: "x".to_string(),
            start_line: 10,
            start_col: 0,
            end_line: Some(12),
            end_col: Some(4),
            kinds: None,
        };
        assert_eq!(resolve_range_bounds(&req), (10, 0, 12, 4));
    }

    #[test]
    fn ranges_intersect_touching_endpoints_count_as_intersecting() {
        assert!(ranges_intersect(((0, 0), (0, 3)), ((0, 3), (0, 5))));
    }

    #[test]
    fn ranges_intersect_detects_a_disjoint_pair() {
        assert!(!ranges_intersect(((0, 0), (0, 3)), ((1, 0), (1, 5))));
    }

    #[test]
    fn ranges_intersect_detects_containment() {
        assert!(ranges_intersect(((0, 0), (5, 0)), ((2, 0), (2, 3))));
    }

    #[test]
    fn diagnostic_intersects_range_filters_out_a_diagnostic_on_a_different_line() {
        let diag = json!({
            "range": {"start": {"line": 5, "character": 0}, "end": {"line": 5, "character": 3}},
            "message": "elsewhere"
        });
        let range =
            json!({"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 3}});
        assert!(!diagnostic_intersects_range(&diag, &range));
    }

    #[test]
    fn diagnostic_intersects_range_keeps_an_overlapping_diagnostic() {
        let diag = json!({
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 3}},
            "message": "here"
        });
        let range =
            json!({"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 2}});
        assert!(diagnostic_intersects_range(&diag, &range));
    }

    #[test]
    fn diagnostic_intersects_range_malformed_diagnostic_never_intersects() {
        let diag = json!({"message": "no range at all"});
        let range =
            json!({"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 3}});
        assert!(!diagnostic_intersects_range(&diag, &range));
    }

    #[test]
    fn uri_within_workspace_accepts_a_uri_under_the_root() {
        assert_eq!(
            uri_within_workspace("file:///repo/lib/foo.rb", Path::new("/repo")),
            Some("lib/foo.rb".to_string())
        );
    }

    #[test]
    fn uri_within_workspace_rejects_a_uri_outside_the_root() {
        assert_eq!(
            uri_within_workspace("file:///etc/passwd", Path::new("/repo")),
            None
        );
    }

    #[test]
    fn uri_within_workspace_rejects_a_non_file_uri() {
        assert_eq!(
            uri_within_workspace("untitled:Untitled-1", Path::new("/repo")),
            None
        );
    }

    #[test]
    fn translate_text_edits_converts_range_and_new_text() {
        let edits = json!([{
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 3}},
            "newText": "foo"
        }]);
        let out = translate_text_edits(edits.as_array().unwrap(), b"bar baz");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["start_line"], json!(1));
        assert_eq!(out[0]["start_col"], json!(0));
        assert_eq!(out[0]["end_col"], json!(3));
        assert_eq!(out[0]["new_text"], json!("foo"));
    }

    #[test]
    fn translate_text_edits_drops_entries_with_no_range() {
        let edits = json!([{"newText": "no range, malformed"}]);
        assert_eq!(
            translate_text_edits(edits.as_array().unwrap(), b""),
            Vec::<Value>::new()
        );
    }

    #[test]
    fn translate_workspace_edit_translates_a_plain_changes_map() {
        let uri = "file:///repo/f.rb";
        let edit = json!({
            "changes": {
                uri: [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}, "newText": "x"}]
            }
        });
        match translate_workspace_edit(&edit, uri, b"content", Path::new("/repo")) {
            EditTranslation::Edits(files) => {
                assert_eq!(files.len(), 1);
                assert_eq!(files[0]["path"], json!("f.rb"));
                assert_eq!(files[0]["edits"].as_array().unwrap().len(), 1);
            }
            EditTranslation::Unsupported => panic!("expected a translated edit"),
        }
    }

    #[test]
    fn translate_workspace_edit_translates_document_changes_across_two_files() {
        let uri = "file:///repo/f.rb";
        let other = "file:///repo/other.rb";
        let edit = json!({
            "documentChanges": [
                {
                    "textDocument": {"uri": uri, "version": null},
                    "edits": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}, "newText": "a"}]
                },
                {
                    "textDocument": {"uri": other, "version": null},
                    "edits": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}, "newText": "b"}]
                }
            ]
        });
        match translate_workspace_edit(&edit, uri, b"content", Path::new("/repo")) {
            EditTranslation::Edits(files) => {
                assert_eq!(files.len(), 2);
                let paths: Vec<&str> = files.iter().map(|f| f["path"].as_str().unwrap()).collect();
                assert!(paths.contains(&"f.rb"));
                assert!(paths.contains(&"other.rb"));
            }
            EditTranslation::Unsupported => panic!("expected translated edits"),
        }
    }

    #[test]
    fn translate_workspace_edit_rejects_a_create_file_resource_op() {
        let uri = "file:///repo/f.rb";
        let edit = json!({
            "documentChanges": [
                {"kind": "create", "uri": "file:///repo/new.rb"}
            ]
        });
        assert!(matches!(
            translate_workspace_edit(&edit, uri, b"content", Path::new("/repo")),
            EditTranslation::Unsupported
        ));
    }

    #[test]
    fn translate_workspace_edit_rejects_a_uri_outside_the_workspace_root() {
        let uri = "file:///repo/f.rb";
        let edit = json!({
            "changes": {
                "file:///etc/passwd": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}, "newText": "x"}]
            }
        });
        assert!(matches!(
            translate_workspace_edit(&edit, uri, b"content", Path::new("/repo")),
            EditTranslation::Unsupported
        ));
    }

    #[test]
    fn translate_workspace_edit_empty_edit_is_edits_with_nothing_in_it() {
        let uri = "file:///repo/f.rb";
        let edit = json!({});
        match translate_workspace_edit(&edit, uri, b"content", Path::new("/repo")) {
            EditTranslation::Edits(files) => assert!(files.is_empty()),
            EditTranslation::Unsupported => panic!("an empty edit is not an unsupported op"),
        }
    }
}
