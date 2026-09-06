//! V70-A8 (D20 CLI hygiene) — `GET /api/schemas` + `GET /api/schemas/{name}`:
//! a self-description surface an agent can use to learn kb-code's wire
//! shapes without reading Rust (`kb-code schema list|show` is the CLI
//! twin).
//!
//! # Scope: a curated STARTER set, not a corpus-wide dump
//!
//! D20 asks for schemas "generated from the Rust types (schemars)". Most
//! routes in this crate build their response as an ad-hoc `serde_json::
//! json!({...})` literal rather than a typed struct — there is no Rust
//! type to derive `JsonSchema` from without first inventing one, and doing
//! that for the ~130 routes this daemon serves is its own multi-day unit,
//! not a hygiene pass. So this module ships FOUR names, each a small
//! struct DEFINED HERE that mirrors (by hand, not generated from) the
//! shape of an existing typed response: [`IdentitySchema`] (`GET
//! /api/identity`), [`HealthzSchema`] (`GET /api/healthz`), [`ScopesSchema`]
//! (`GET /api/scopes`), and [`RepoEntrySchema`] (one entry of `GET
//! /api/repos`, including this unit's own `writable`/`is_worktree`
//! additions — see `routes::RepoListEntry`). Each is intentionally a
//! SIMPLIFIED mirror (`PathBuf` fields become `String`, `&'static str`
//! becomes `String`) so every field type is one schemars supports natively
//! with no extra feature flags — deriving `JsonSchema` directly on the
//! REAL response structs (`RepoListEntry`, `IdentityResponse`, …) would
//! cascade the derive across every field type they carry transitively
//! (`ScipStatus`, `RepoIntelStatus`, `crate::git::HeadInfo`, …), which is a
//! correctness bet this unit's one-shot-compile-check budget can't afford
//! to get wrong. Expanding the registry (more names, or switching a mirror
//! to derive straight off the real struct) is future work — the registry
//! is additive by construction (`NAMES` + one match arm per name), so
//! growing it never breaks an existing name.
//!
//! No drift test ties these mirrors to the real response structs today
//! (also future work, named here rather than silently assumed) — a field
//! renamed on `RepoListEntry` without a matching edit to
//! [`RepoEntrySchema`] would go unnoticed until someone reads the wrong
//! schema for the real body's shape.
//!
//! The five mirror structs below exist SOLELY to give `schemars::
//! schema_for!` a type to walk — none is ever constructed (their `Debug`/
//! `JsonSchema` derives are the only consumers), which would otherwise trip
//! rustc's "struct is never constructed" `dead_code` lint on every one;
//! `#![allow(dead_code)]` below is scoped to this module for exactly that
//! reason.
#![allow(dead_code)]

use axum::{extract::Path, response::IntoResponse, Json};
use schemars::JsonSchema;
use serde_json::json;
use std::collections::BTreeMap;

use crate::routes::ApiError;

/// Mirrors `routes::IdentityResponse`'s agent-relevant fields (drops the
/// kb-sibling/1 handshake fields this schema's own consumer doesn't need
/// to introspect — `kb_public_url`/`remote_mutations`).
#[derive(Debug, JsonSchema)]
pub struct IdentitySchema {
    pub name: String,
    pub version: String,
    pub build_sha: String,
    pub sibling_protocol: String,
    pub sibling_major: u32,
    pub schema_epoch: u32,
    pub repos: Vec<RepoSummarySchema>,
}

/// Mirrors `routes::RepoSummary`.
#[derive(Debug, JsonSchema)]
pub struct RepoSummarySchema {
    pub name: String,
    pub path: String,
    pub file_count: u64,
    pub symbol_count: u64,
}

/// Mirrors `routes::HealthResponse`.
#[derive(Debug, JsonSchema)]
pub struct HealthzSchema {
    pub status: String,
    pub uptime_secs: i64,
}

/// Mirrors `routes::ScopesOut`.
#[derive(Debug, JsonSchema)]
pub struct ScopesSchema {
    pub scopes: BTreeMap<String, Vec<String>>,
}

/// Mirrors one entry of `routes::RepoListEntry`, INCLUDING this unit's own
/// `writable`/`is_worktree` additions (drops `head`/`scip`/`intel`/
/// `intel_providers` — each is its own nested type this starter set
/// deliberately doesn't cascade into, see the module doc).
#[derive(Debug, JsonSchema)]
pub struct RepoEntrySchema {
    pub name: String,
    pub path: String,
    pub file_count: u64,
    pub symbol_count: u64,
    pub watcher: String,
    pub writable: bool,
    pub is_worktree: bool,
}

/// The registry's name set — `GET /api/schemas`' `names` field and
/// [`schema_json`]/[`example_json`]'s match arms share this ONE list (the
/// literal strings appear nowhere else), so a typo in either match can
/// only ever produce a `None` (a 404), never silently list a name the
/// getter can't resolve.
const NAMES: &[&str] = &["identity", "healthz", "scopes", "repos-entry"];

fn schema_json(name: &str) -> Option<serde_json::Value> {
    let schema = match name {
        "identity" => schemars::schema_for!(IdentitySchema),
        "healthz" => schemars::schema_for!(HealthzSchema),
        "scopes" => schemars::schema_for!(ScopesSchema),
        "repos-entry" => schemars::schema_for!(RepoEntrySchema),
        _ => return None,
    };
    // `schemars::Schema` is `Serialize` (it wraps a `serde_json::Value`
    // internally) — this can only fail if `Schema`'s own `Serialize` impl
    // panics or errors, which it does not for the closed struct set above.
    serde_json::to_value(&schema).ok()
}

/// One hand-written example per name — not derived from anything (there is
/// no live daemon state this route can read from with no `State` param and
/// no repo context), just a plausible instance for a `kb-code schema show
/// --example` reader to see the shape populated.
fn example_json(name: &str) -> Option<serde_json::Value> {
    match name {
        "identity" => Some(json!({
            "name": "kb-code",
            "version": "0.40.0",
            "build_sha": "b42b3167",
            "sibling_protocol": "kbc-sibling/1",
            "sibling_major": 1,
            "schema_epoch": 27,
            "repos": [{
                "name": "kb",
                "path": "/home/user/project/kb",
                "file_count": 4200,
                "symbol_count": 38000,
            }],
        })),
        "healthz" => Some(json!({"status": "ok", "uptime_secs": 3600})),
        "scopes" => Some(json!({"scopes": {"app": ["app/**", "!app/assets/**"]}})),
        "repos-entry" => Some(json!({
            "name": "kb",
            "path": "/home/user/project/kb",
            "file_count": 4200,
            "symbol_count": 38000,
            "watcher": "watching",
            "writable": true,
            "is_worktree": false,
        })),
        _ => None,
    }
}

/// `GET /api/schemas` — the list of names this daemon can describe. Ordinary
/// `auth_bearer` read (no repo/content — same sensitivity class as
/// `/api/identity`); needs no `State` since the registry is a compile-time
/// constant, not derived from configured repos.
pub async fn list_schemas_route() -> impl IntoResponse {
    Json(json!({
        "schema": "kbc-api-schemas/1",
        "names": NAMES,
    }))
}

/// `GET /api/schemas/{name}` — the named JSON Schema document plus a
/// hand-written example. 404s (via [`ApiError::not_found`], naming the
/// valid set) on an unknown name — never a silent empty body.
pub async fn get_schema_route(Path(name): Path<String>) -> Result<impl IntoResponse, ApiError> {
    let json_schema = schema_json(&name).ok_or_else(|| {
        ApiError::not_found(format!(
            "unknown schema name: {name:?} (see GET /api/schemas for the valid set: {NAMES:?})"
        ))
    })?;
    Ok(Json(json!({
        "name": name,
        "json_schema": json_schema,
        "example": example_json(&name),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_name_resolves_a_schema_and_an_example() {
        for name in NAMES {
            assert!(
                schema_json(name).is_some(),
                "{name} is in NAMES but schema_json returned None"
            );
            assert!(
                example_json(name).is_some(),
                "{name} is in NAMES but example_json returned None"
            );
        }
    }

    #[test]
    fn unknown_name_resolves_neither() {
        assert!(schema_json("nope-not-a-real-schema").is_none());
        assert!(example_json("nope-not-a-real-schema").is_none());
    }
}
