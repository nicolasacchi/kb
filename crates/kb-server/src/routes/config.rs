//! `GET /api/config` + `PUT /api/config` — read the daemon's live kb.toml
//! config and write edits back to the file it was loaded from (CE-track).
//!
//! GET returns the running config as JSON plus the resolved source path, a
//! presence map for the share token env vars (names only — never values),
//! and the embedding-model registry for the editor's selects. PUT
//! validates (`kb_core::config::validate`), rejects a changed
//! `[daemon] name` (the state dir + pid file are pinned for the daemon's
//! lifetime), persists comment-preservingly via `save_preserving`, then
//! trips an in-process restart so every field takes effect. A bad edit
//! that won't boot rolls back to last-good in `serve_loop` (CE4).
//!
//! Auth: inherits the `/api/*` `auth_bearer` + `origin_allowlist` layers
//! (loopback bypass), so it's as privileged as `/api/shutdown` — local
//! SPA/CLI need no token; off-loopback needs the bearer + a valid Origin.

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderValue, Response, StatusCode},
    response::IntoResponse,
    Json,
};
use kb_core::config::{KbConfig, ValidationIssue};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

#[derive(Debug, Serialize)]
pub struct ConfigGetResponse {
    /// The running config (what the daemon loaded), as JSON. Secrets are
    /// never here — the share section holds env-var NAMES only.
    pub config: KbConfig,
    /// Resolved path the config was loaded from + that writes go back to.
    pub config_path: String,
    /// Presence (not value) of each configured share token env var, so
    /// the editor can show a "set ✓ / not set" pill without leaking it.
    pub env_present: BTreeMap<String, bool>,
    /// Embedding-model registry names for the per-kb model `<select>`.
    pub embedding_models: Vec<String>,
}

/// Presence map for the share token env vars named in the config. Reports
/// only whether each var is set + non-empty — never the value (mirrors
/// `config::read_secret_env`'s predicate minus the value).
fn env_presence(config: &KbConfig) -> BTreeMap<String, bool> {
    let mut out = BTreeMap::new();
    let mut probe = |name: &str| {
        let present = std::env::var(name)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        out.insert(name.to_string(), present);
    };
    if let Some(cf) = &config.share.cloudflare {
        probe(&cf.api_token_env);
    }
    if let Some(gh) = &config.share.github {
        probe(&gh.token_env);
    }
    out
}

/// GET /api/config — the running config + metadata for the web editor.
pub async fn get(State(state): State<Arc<KbHandles>>) -> Json<ConfigGetResponse> {
    let config = state.config.read().await.clone();
    let env_present = env_presence(&config);
    let embedding_models = kb_core::embed::SUPPORTED_MODELS
        .iter()
        .map(|m| m.name.to_string())
        .collect();
    Json(ConfigGetResponse {
        config,
        config_path: state.config_path.display().to_string(),
        env_present,
        embedding_models,
    })
}

#[derive(Debug, Serialize)]
struct IssueJson {
    pointer: String,
    detail: String,
}

impl IssueJson {
    fn from(issue: &ValidationIssue) -> Self {
        Self {
            pointer: issue.pointer.clone(),
            detail: issue.message.clone(),
        }
    }
}

/// Build an RFC7807 problem+json 400 carrying the per-field hard issues in
/// an `errors` array (mirrors `error_to_problem_json`'s shape so the SPA's
/// parser surfaces `detail`, plus field mapping the editor maps back).
fn validation_problem(issues: &[ValidationIssue]) -> Response<Body> {
    let detail = issues
        .iter()
        .filter(|i| i.is_hard())
        .map(|i| format!("{}: {}", i.pointer, i.message))
        .collect::<Vec<_>>()
        .join("; ");
    let errors: Vec<IssueJson> = issues
        .iter()
        .filter(|i| i.is_hard())
        .map(IssueJson::from)
        .collect();
    let err = kb_core::Error::BadRequest(detail.clone());
    let body = serde_json::json!({
        "type": err.problem_type(),
        "title": StatusCode::BAD_REQUEST.canonical_reason().unwrap_or("Bad Request"),
        "status": err.http_status(),
        "detail": detail,
        "errors": errors,
    });
    let mut resp = (StatusCode::BAD_REQUEST, axum::Json(body)).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    resp
}

#[derive(Debug, Serialize)]
struct PutResponse {
    restarting: bool,
    config_path: String,
    warnings: Vec<IssueJson>,
}

/// X2 — the embedding dim a kb's boot will ENFORCE under `cfg`: `Some(d)` when
/// a model resolves (`Storage::open` then requires the on-disk dataset to be
/// exactly `d`), `None` when no model resolves (`config_dim = None` → boot
/// trusts whatever dim is on disk, so no clash is possible). Mirrors the boot
/// resolution EXACTLY (`config_dim = resolved_embedding_model(kb)
/// .and_then(model_info).map(dim)` in `lib.rs`); routing through
/// `resolved_embedding_model` gets the per-kb → `[defaults]` → registry-default
/// fallback for free AND honours `disable_embedder_fallback`. The key
/// consequence the old `match` missed: an UNKNOWN model name does NOT resolve to
/// `None` here — it falls through to the registry default (or `[defaults]`), so
/// a typo'd new model whose real dim differs is caught up front instead of
/// failing boot and silently rolling back (deep-review HIGH).
///
/// NB the two call sites are ASYMMETRIC. For the EDITED (new) config this is the
/// enforced dim verbatim. For the RUNNING config, the *disk* dim is what matters,
/// and a fresh dataset was created at `default_model().dim` when no model was
/// enforced — so the running side unwraps `None` to the default at the call
/// site, never here. Collapsing both into one default-bearing helper would skip
/// a genuine clash (new explicit model vs a disk built at the default).
fn enforced_dim(cfg: &KbConfig, kb: &kb_core::config::KbSection) -> Option<i32> {
    cfg.resolved_embedding_model(kb)
        .and_then(kb_core::embed::model_info)
        .map(|i| i.dim as i32)
}

/// PUT /api/config — validate, persist (preserving comments), then trip an
/// in-process restart so every field takes effect. `serve_loop` rolls back
/// to last-good if the new config won't boot.
pub async fn put(
    State(state): State<Arc<KbHandles>>,
    Json(body): Json<KbConfig>,
) -> Response<Body> {
    let running = state.config.read().await.clone();
    let (running_name, running_addr) = (running.daemon.name.clone(), running.server.addr.clone());

    // The daemon name pins the state dir + pid file for the process's
    // lifetime; an in-process restart can't move them. Reject a change.
    if body.daemon.name != running_name {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "changing [daemon] name needs a full `kb daemon stop` + restart \
             (the state dir + pid file are pinned to it)"
                .into(),
        ));
    }

    let issues = body.validate();
    if issues.iter().any(|i| i.is_hard()) {
        return validation_problem(&issues);
    }

    // CE — for a CHANGED bind addr, test-bind it NOW so an unbindable port
    // is rejected before we persist + restart: immediate feedback, and the
    // on-disk config never goes bad (a bad addr that reached disk would
    // strand the daemon on the next OS-level restart). Skip when unchanged
    // — the running daemon already holds the port. The serve_loop rollback
    // (CE4) is the backstop for the residual race between this probe and
    // the restart's real bind.
    if body.server.addr != running_addr {
        if let Err(e) = tokio::net::TcpListener::bind(&body.server.addr).await {
            return error_to_problem_json(&kb_core::Error::BadRequest(format!(
                "cannot bind [server] addr `{}`: {e}",
                body.server.addr
            )));
        }
        // The probe listener drops here, freeing the port for the restart.
    }

    // X2 — reject a dim-incompatible embedding_model change up front. The
    // running daemon booted, so each existing kb's on-disk lance dim equals
    // its current effective-model dim (invariant #4). A changed model (per
    // kb OR via [defaults]) whose dim differs would fail Storage::open at
    // re-boot and silently roll back to the old config — the edit appears to
    // succeed but doesn't take. Catch it here with a clear field error.
    // Skips newly-added kbs (no dataset yet → fresh build at the new dim). An
    // unknown new model is NOT skipped: it resolves to the registry default,
    // whose dim is checked like any other (deep-review HIGH — a typo previously
    // slipped through to a silent boot rollback).
    for (name, new_kb) in &body.kb {
        let Some(old_kb) = running.kb.get(name) else {
            continue; // brand-new kb — fresh, no existing dim to clash with
        };
        // The edited config enforces a dim only when a model resolves; `None`
        // means "no embedder" → boot trusts the disk, so nothing to check.
        if let Some(new_dim) = enforced_dim(&body, new_kb) {
            // The disk dim the running daemon's dataset was built at: its
            // enforced dim, else the default the fresh dataset took when no
            // model was enforced. (Asymmetric on purpose — see `enforced_dim`.)
            let old_dim = enforced_dim(&running, old_kb)
                .unwrap_or(kb_core::embed::default_model().dim as i32);
            if old_dim != new_dim {
                return error_to_problem_json(&kb_core::Error::BadRequest(format!(
                    "kb `{name}`: embedding model resolves to dim {new_dim}, incompatible \
                     with its existing dim {old_dim}. Changing the embedding dimension needs \
                     a re-index (drop + re-add the kb), not a config edit — the daemon would \
                     otherwise reject this at boot and roll back."
                )));
            }
        }
    }

    // Persist to the file the daemon loaded from, preserving comments.
    if let Err(e) = body.save_preserving(&state.config_path) {
        return error_to_problem_json(&e);
    }
    // Reflect the pending change for any GET that races the restart.
    *state.config.write().await = body.clone();

    // Announce BEFORE tripping the watch — the SSE stream closes on
    // shutdown, so an emit after the flip might never flush.
    state.bus.emit(
        "daemon.restarting",
        serde_json::json!({
            "addr": body.server.addr,
            "config_path": state.config_path.display().to_string(),
        }),
    );
    state.restart_requested.store(true, Ordering::SeqCst);
    let _ = state.shutdown.send(true);

    let warnings = issues
        .iter()
        .filter(|i| !i.is_hard())
        .map(IssueJson::from)
        .collect();
    Json(PutResponse {
        restarting: true,
        config_path: state.config_path.display().to_string(),
        warnings,
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kb_with_model(model: Option<&str>) -> kb_core::config::KbSection {
        // KbSection has no Default; build via serde so every #[serde(default)]
        // field is filled and we only set `path` + `embedding_model`.
        serde_json::from_value(serde_json::json!({
            "path": "/tmp/x",
            "embedding_model": model,
        }))
        .unwrap()
    }

    // deep-review HIGH — the old `match` returned `None` for an unknown model
    // name (check skipped), but boot resolves an unknown name THROUGH to the
    // registry default, so a typo whose default dim differs from the disk would
    // slip past the guard and fail boot, silently rolling back. `enforced_dim`
    // must match boot: unknown → Some(registry-default dim), never None.
    #[test]
    fn enforced_dim_resolves_unknown_model_to_registry_default_not_none() {
        let cfg = KbConfig::default(); // defaults: no model, fallback enabled
        let default_dim = kb_core::embed::default_model().dim as i32;

        // Unknown name → registry default dim (the bug fix), NOT None.
        assert_eq!(
            enforced_dim(&cfg, &kb_with_model(Some("bge-totally-bogus"))),
            Some(default_dim),
        );
        // A known 768 model still resolves to 768.
        assert_eq!(
            enforced_dim(&cfg, &kb_with_model(Some("bge-base-en-v1.5"))),
            Some(768),
        );
        // No model + fallback enabled → registry default (the disk dim a fresh
        // dataset was built at).
        assert_eq!(enforced_dim(&cfg, &kb_with_model(None)), Some(default_dim));
    }

    // With `disable_embedder_fallback`, no resolvable model → None ("no embedder
    // enforced"), so the new (edited) side of the check is skipped — boot trusts
    // the disk dim, so there is nothing to clash with.
    #[test]
    fn enforced_dim_none_when_no_model_and_fallback_disabled() {
        let mut cfg = KbConfig::default();
        cfg.defaults.disable_embedder_fallback = true;
        assert_eq!(enforced_dim(&cfg, &kb_with_model(None)), None);
        // An unknown name with fallback disabled also resolves to nothing.
        assert_eq!(enforced_dim(&cfg, &kb_with_model(Some("bge-bogus"))), None,);
    }
}
