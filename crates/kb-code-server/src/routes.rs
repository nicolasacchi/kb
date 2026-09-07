//! `GET /api/identity` + `GET /healthz` — daemon self-description and
//! liveness probe. Shapes mirror kb-server's `routes::identity` /
//! `routes::health` (`crates/kb-server/src/routes/{identity,health}.rs`),
//! trimmed to what kb-code has (no per-kb corpora, no artifact subdomain).
//! `identity` is ALSO this daemon's kb-sibling/1 version Hello
//! (`build_sha`/`sibling_protocol`/`sibling_major`/`schema_epoch`, mirroring
//! kb-server's); `healthz` stays PURE liveness by design — it must not learn
//! about schema or protocol, so an orchestrator's restart loop can never be
//! driven by a contract mismatch.
//!
//! W1.6 adds the browsing surface: `GET /api/repos` (per-repo counts, HEAD,
//! and watcher state), `GET /api/tree` (W1.3's `list_tree` over HTTP),
//! `GET /api/file` (blob-at-ref OR working-tree bytes, plus derived
//! symbols/highlights looked up by blob hash), `GET /api/symbols` (per-file
//! list, or a repo-wide substring query — superseded by the real fuzzy lane
//! for the `q=` form, see W2.1 below; the per-file `path=` form is
//! unchanged), and `GET /api/events` (the SSE bus the live-mirror sink
//! publishes onto — `sink.rs`). All five are pure reads: none of these
//! handlers ever write to `state.store` — every store row comes from
//! `sink::IndexSink` (live edits) or `bind_and_spawn`'s initial `HEAD`-tree
//! walk (`lib.rs`), never from a request handler. An unindexed blob (e.g. a
//! historical ref the boot walk/live mirror never touched) simply reports
//! empty symbols/highlights rather than deriving them on demand — see
//! `file`/`symbols`' doc below.
//!
//! **W2.1 adds the INSTANT search lanes**: `GET /api/search/files`,
//! `GET /api/search/symbols`, `GET /api/search/text` — thin HTTP wrappers
//! over `search::{files, symbols, text}` (the actual nucleo/grep-searcher
//! logic lives there; see that module's doc). This is the one exception to
//! "every handler is a pure read": `file` below now also BUMPS the files
//! lane's open-history frecency signal (`store::Store::bump_file_open`) on
//! every successful read — a side effect, but a purely additive one (an
//! event log row), never a mutation of `state.store`'s indexed content.

use crate::annotations::{self, DiffAnchor2, SymbolDescriptor};
use crate::config::RepoEntry;
use crate::extract::Symbol;
use crate::git::{GitError, GitRepo, RefKind, RefRange, Revspec, DEFAULT_BLOB_SIZE_CAP};
use crate::ingest;
use crate::lang;
use crate::search;
use crate::semantic;
use crate::state::SharedState;
use crate::store::{self, Store, StoreBlocking, StoreError};
use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::{
        sse::{Event as SseEvent, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json,
};
use base64::Engine;
use futures::{Stream, StreamExt};
use kb_core::events::{events_stream, EventFrame};
use kb_core::review::Anchor;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::convert::Infallible;
use std::path::{Component, Path};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize)]
pub struct RepoSummary {
    pub name: String,
    pub path: String,
    /// W1.5 — cheap `COUNT(*)` against the kb-code store. `0` for a repo
    /// nothing has indexed yet (`bind_and_spawn` only REGISTERS repos at
    /// boot — see its doc comment — it doesn't eagerly walk them; a repo's
    /// counts stay `0` until something calls
    /// `ingest::index_repo_working_tree`, e.g. a future CLI verb).
    pub file_count: u64,
    pub symbol_count: u64,
}

#[derive(Debug, Serialize)]
pub struct IdentityResponse {
    pub name: &'static str,
    pub version: &'static str,
    pub repos: Vec<RepoSummary>,
    pub started_at: String,
    /// The browser-facing base URL for links kb-code's own UI builds into
    /// kb's SPA (`crate::config::KbDaemonSection::public_base` — `[kb_daemon]
    /// public_url` if the operator set one, else the federation `url`). The
    /// SPA reads this once at boot (`GET /api/identity`) and rewrites its
    /// hardcoded local-dev default before rendering any such link — see
    /// `web-code/src/lib/searchLanes.ts::setKbSessionBase`.
    pub kb_public_url: String,
    /// kb-sibling/1 Hello — the commit this binary was built from.
    /// kb-code-server has no `build.rs` of its own (kb-server bakes
    /// `KB_GIT_SHA` in one), so this reads the deploy-time
    /// `KB_BUILD_SHA` env stamp and falls back to `"dev"` for a local
    /// build — an honest "unstamped", never a fabricated sha.
    pub build_sha: &'static str,
    /// kb-sibling/1 Hello — the contract name
    /// (`kb_core::sibling::SIBLING_PROTOCOL`), mirroring kb-server's own
    /// identity route. A different string is a DIFFERENT contract.
    pub sibling_protocol: &'static str,
    /// kb-sibling/1 Hello — the contract's major version.
    pub sibling_major: u32,
    /// kb-sibling/1 Hello — this binary's kb-code schema epoch (highest
    /// EMBEDDED migration version in `crates/kb-code-server/migrations/`,
    /// a set entirely separate from kb's). The same number `Store::open`'s
    /// boot guard compares the volume against.
    pub schema_epoch: u32,
    /// S2-B — additive capability discovery for `[review] remote_mutations`
    /// (`config::ReviewSection`, default `false`): whether the five
    /// review-mutation route families on `router.rs`'s gated `review_remote`
    /// sub-router (see [`crate::review_gate::review_mutations_gate`]) admit
    /// a non-loopback bearer caller. Never REQUIRED reading — a caller that
    /// never checks it still gets the correct 404/401/200 from the routes
    /// themselves; this field only lets a client render the capability
    /// (e.g. a Settings chip) without probing.
    pub remote_mutations: bool,
}

/// `GET /api/identity` — under the `/api` nest, so it's behind
/// `auth_bearer` (loopback bypasses per invariant #4).
pub async fn identity(State(state): State<SharedState>) -> impl IntoResponse {
    // Every configured repo was `upsert_repo`'d in `bind_and_spawn`, so
    // `id` is always `Some` in practice; `unwrap_or_default()` (zero
    // counts) is the defensive fallback rather than a panic if that ever
    // drifts.
    let repo_meta: Vec<(String, String, Option<i64>)> = state
        .repos
        .iter()
        .map(|r| {
            (
                r.name.clone(),
                r.path.display().to_string(),
                state.repo_ids.get(&r.name).copied(),
            )
        })
        .collect();
    // 2026-08-31 incident (store.rs module doc): the per-repo file/symbol
    // count loop — previously N inline store calls on this async worker —
    // now runs as ONE closure on the blocking pool. Confirmed case named
    // in the incident writeup.
    let repos = state
        .store
        .run_blocking(move |store| {
            repo_meta
                .into_iter()
                .map(|(name, path, id)| {
                    let file_count = id
                        .and_then(|id| store.file_count(id).ok())
                        .unwrap_or_default();
                    let symbol_count = id
                        .and_then(|id| store.symbol_count_for_repo(id).ok())
                        .unwrap_or_default();
                    RepoSummary {
                        name,
                        path,
                        file_count,
                        symbol_count,
                    }
                })
                .collect::<Vec<_>>()
        })
        .await;
    let body = Json(IdentityResponse {
        name: "kb-code",
        version: state.version,
        kb_public_url: state.kb_daemon.public_base().to_string(),
        build_sha: option_env!("KB_BUILD_SHA").unwrap_or("dev"),
        sibling_protocol: kb_core::sibling::SIBLING_PROTOCOL,
        sibling_major: kb_core::sibling::SIBLING_MAJOR,
        schema_epoch: crate::store::schema_epoch(),
        remote_mutations: state.review.remote_mutations,
        repos,
        started_at: state.started_at.to_rfc3339(),
    });
    // `no-store`, mirroring kb-server's identity/health routes: a stale
    // cached response would defeat a client's daemon-liveness check.
    ([(header::CACHE_CONTROL, "no-store")], body)
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub uptime_secs: i64,
}

/// `GET /healthz` — mounted OUTSIDE the `/api` nest (see `router.rs`), so
/// it is NOT behind `auth_bearer`. Unauthenticated liveness probe, no I/O.
pub async fn healthz(State(state): State<SharedState>) -> impl IntoResponse {
    let uptime_secs = (chrono::Utc::now() - state.started_at).num_seconds().max(0);
    let body = Json(HealthResponse {
        status: "ok",
        uptime_secs,
    });
    ([(header::CACHE_CONTROL, "no-store")], body)
}

// --- W1.6 — errors -----------------------------------------------------

/// Uniform JSON error body (`{"error": "..."}`) for every W1.6 route below.
/// `identity`/`healthz` predate this (they have no fallible path) and stay
/// on their own ad-hoc shape rather than being retrofitted here.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
    /// DCB W1.C (D11/R12) — the machine-readable degrade code the doc-lens
    /// routes carry beside the human `error` string. `None` on every
    /// pre-DCB construction, and `skip_serializing_if` keeps the key OUT of
    /// the body entirely there, so no existing route's response changes by a
    /// byte. The v1 vocabulary is enumerated in `crate::doclens`'s module doc.
    reason: Option<&'static str>,
    /// V70-A2 — an RFC 7807 `type` URN for the SECURITY refusals
    /// (`crate::security`'s module doc enumerates the vocabulary). `None`
    /// on every pre-V70-A2 construction, and the key is omitted entirely
    /// there, so no existing route's response changes by a byte; when set,
    /// the body gains a `"type"` key and the response's content type
    /// becomes `application/problem+json`.
    problem_type: Option<&'static str>,
}

impl ApiError {
    // `pub(crate)` (not private) — W2.5's `transcripts::search` route
    // handlers live in a sibling module and reuse these same constructors
    // rather than growing a second ad-hoc error-response shape.
    pub(crate) fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            reason: None,
            problem_type: None,
        }
    }

    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    /// V73-K3 — the human message, for a caller that FOLDS an error into a
    /// degraded lane instead of returning it (`review_timeline`'s `turns`
    /// lane: a join that could not run must caption itself, not 500 the
    /// whole stream). Read-only; the wire shape is unchanged.
    pub(crate) fn message_text(&self) -> &str {
        &self.message
    }

    /// DCB W1.C — attach a machine-readable `reason` (D11). Builder-style, so
    /// every existing constructor and call site stays untouched.
    pub(crate) fn with_reason(mut self, reason: &'static str) -> Self {
        self.reason = Some(reason);
        self
    }

    /// `400` + `reason` — the shape `doclens::validate_{kb,doc}_segment`
    /// returns (12-w1c §4.1).
    pub(crate) fn bad_request_with_reason(
        message: impl Into<String>,
        reason: &'static str,
    ) -> Self {
        Self::bad_request(message).with_reason(reason)
    }

    /// The bare message, no status code — `search::unified`'s per-lane
    /// runners fold a repo-resolution `ApiError` into that lane's
    /// `unavailable_reason` string (drop-on-error, never a whole-box 500),
    /// which needs the text without re-deriving an HTTP status nobody reads.
    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    /// The attached degrade code, if any. `#[allow(dead_code)]` because
    /// W1.C's only readers are its own unit tests: the FIRST production
    /// consumer is W3.A's in-process `resolve_lens` caller (R8), which
    /// branches on the reason rather than re-parsing a rendered body — the
    /// same "surface lands with its phase, consumer lands next" convention
    /// `AppState`'s RAII fields already use.
    #[allow(dead_code)]
    pub(crate) fn reason(&self) -> Option<&'static str> {
        self.reason
    }

    /// V70-A2 — attach an RFC 7807 `type` URN. Builder-style, same shape
    /// as `with_reason` above, so every existing construction is untouched.
    /// `pub` (not `pub(crate)`): `crate::security`'s guards are the only
    /// producers today, but the URN vocabulary is a wire contract this
    /// crate's integration tests assert directly.
    pub fn with_problem_type(mut self, urn: &'static str) -> Self {
        self.problem_type = Some(urn);
        self
    }

    /// The attached `type` URN, if any — what the security tests assert on
    /// rather than re-parsing a rendered body.
    pub fn problem_type(&self) -> Option<&'static str> {
        self.problem_type
    }

    /// The HTTP status this error renders as.
    pub fn status_code(&self) -> StatusCode {
        self.status
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = serde_json::Map::new();
        body.insert("error".into(), serde_json::Value::String(self.message));
        // `skip_serializing_if` semantics, hand-rolled over the existing
        // `json!` body: absent (not `null`) when no reason was attached.
        if let Some(reason) = self.reason {
            body.insert("reason".into(), serde_json::Value::String(reason.into()));
        }
        // V70-A2 — a security refusal additionally renders as RFC 7807
        // problem+json (`type`/`title`/`status` beside the existing
        // `error`), so a client can branch on the machine URN. Absent
        // `problem_type` = the pre-V70-A2 body and content type, byte for
        // byte.
        let Some(urn) = self.problem_type else {
            return (
                self.status,
                [(header::CACHE_CONTROL, "no-store")],
                Json(serde_json::Value::Object(body)),
            )
                .into_response();
        };
        body.insert("type".into(), serde_json::Value::String(urn.into()));
        body.insert(
            "title".into(),
            serde_json::Value::String(
                self.status
                    .canonical_reason()
                    .unwrap_or("Error")
                    .to_string(),
            ),
        );
        body.insert(
            "status".into(),
            serde_json::Value::Number(self.status.as_u16().into()),
        );
        let mut resp = (
            self.status,
            [(header::CACHE_CONTROL, "no-store")],
            Json(serde_json::Value::Object(body)),
        )
            .into_response();
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/problem+json"),
        );
        resp
    }
}

impl From<GitError> for ApiError {
    fn from(e: GitError) -> Self {
        use GitError::*;
        let status = match &e {
            PathNotFound { .. } | Resolve { .. } => StatusCode::NOT_FOUND,
            NotADir { .. } | NotABlob { .. } => StatusCode::BAD_REQUEST,
            TooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
            Open { .. } | Head { .. } | Refs { .. } | Odb { .. } => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        ApiError::new(status, e.to_string())
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            // Phase E3 — the one `StoreError` variant with its own status:
            // a `reading_sets(repo_id, name)` collision is a client error
            // (`409`), not a server fault.
            StoreError::NameConflict(_) => ApiError::new(StatusCode::CONFLICT, e.to_string()),
            // V74-L3b — the same class one table over: a tour and a board
            // share `canvas_boards`' slug space (V0039), so a collision
            // across the two families is a client error too.
            StoreError::SlugTakenByOtherKind { .. } => {
                ApiError::new(StatusCode::CONFLICT, e.to_string())
            }
            // V4.C2 — batch unknown-id path. 400 (not 404) so a batch
            // never reports a partial apply via a not-found status.
            StoreError::NotFound(_) => ApiError::bad_request(e.to_string()),
            other => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, other.to_string()),
        }
    }
}

// --- W1.6 — shared helpers -----------------------------------------------

/// Look up a configured repo by name — every W1.6 route takes `?repo=`.
/// `pub(crate)` — W3.4's `provenance::{why,story,report}` handlers live in a
/// sibling module and reuse this rather than a second `repos`/`repo_ids`
/// lookup (same precedent as `resolve_search_repos`/`clamp_limit` below).
pub(crate) fn find_repo<'a>(
    state: &'a SharedState,
    name: &str,
) -> Result<(&'a RepoEntry, i64), ApiError> {
    let repo = state
        .repos
        .iter()
        .find(|r| r.name == name)
        .ok_or_else(|| ApiError::not_found(format!("no such repo: {name:?}")))?;
    let repo_id = *state.repo_ids.get(name).ok_or_else(|| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("repo {name:?} is configured but has no store id"),
        )
    })?;
    Ok((repo, repo_id))
}

/// The reverse of `find_repo` — given a `store` row's `repo_id`, recover
/// the configured `RepoEntry` (name + working-tree root). W4.6's
/// annotation PATCH/DELETE routes only carry an annotation `id` (no
/// `?repo=`), so they look the repo up this way instead. A linear scan
/// over `state.repos` (typically single-digit length — one daemon's
/// configured repos), same cost class as `find_repo`'s own `.iter().find`.
pub(crate) fn find_repo_by_id(state: &SharedState, repo_id: i64) -> Result<&RepoEntry, ApiError> {
    state
        .repos
        .iter()
        .find(|r| state.repo_ids.get(&r.name) == Some(&repo_id))
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("annotation references unknown repo_id {repo_id}"),
            )
        })
}

/// Reject a `path` query param that could escape the repo root once joined
/// (`..`, an absolute path, or — Windows-only, dead code on this fleet's
/// Linux/WSL2 targets — a drive prefix). `GET /api/tree`'s ref-based
/// reads don't strictly need this (a git tree has no `..` entries to escape
/// through), but `GET /api/file`'s NO-REF case joins straight onto the real
/// filesystem (`repo.path.join(path)`), where it matters; applied uniformly
/// so every route shares one rule rather than only the routes that
/// currently need it. `pub(crate)` — reused by W3.4's `provenance` routes
/// (same rationale as `find_repo` above).
/// V70-A2 (SEC-17) — parse a caller-supplied `?from=`/`?to=`/`?branch=`
/// into the validated [`Revspec`] every git helper in this crate now
/// takes. The 400 body is the same shape the per-module dash guards
/// produced before ("invalid revspec: ..."), so no client's error handling
/// changes.
pub(crate) fn parse_revspec(spec: &str) -> Result<Revspec, ApiError> {
    Revspec::parse(spec).map_err(|e| ApiError::bad_request(e.to_string()))
}

/// V70-A2 (SEC-17) — the range counterpart, for the routes whose param IS
/// a range (`GET /api/range-diff`'s `?old=`/`?new=`). `parse_loose`
/// because `git range-diff` legitimately accepts a bare revspec too.
pub(crate) fn parse_ref_range(spec: &str) -> Result<RefRange, ApiError> {
    RefRange::parse_loose(spec).map_err(|e| ApiError::bad_request(e.to_string()))
}

pub(crate) fn safe_rel_path(path: &str) -> Result<&str, ApiError> {
    let p = Path::new(path);
    let escapes = p.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir
        )
    });
    if escapes {
        return Err(ApiError::bad_request(format!("invalid path: {path:?}")));
    }
    Ok(path)
}

/// The bytes + git-compatible content hash for `path` in `repo`, either at
/// `rev` (a git blob, via the ODB) or — when `rev` is `None` — the live
/// working-tree bytes straight off disk. `blob_hash` is computed the same
/// way either way (`ingest::git_blob_hash`), so a clean working tree and its
/// committed `HEAD` blob hash identically and share the same derived-data
/// cache slot (ADR-2). `pub(crate)` — B2's `resolve.rs` reuses this exact
/// read (same rationale as `find_repo`/`safe_rel_path` above).
pub(crate) struct FileRead {
    pub(crate) bytes: Vec<u8>,
    pub(crate) blob_hash: String,
}

pub(crate) fn read_repo_file(
    repo: &RepoEntry,
    path: &str,
    rev: Option<&str>,
) -> Result<FileRead, ApiError> {
    // V70-A2 — three checks, in this order, all BEFORE any bytes move:
    //   1. `safe_rel_path` — the pre-existing LEXICAL gate (`..`,
    //      absolute, prefix), kept as the first check per SEC-13's fix.
    //   2. the secret denylist — a typed 403 naming the matched PATTERN,
    //      applied to the ref-based read too (a `.env` in `HEAD` is the
    //      same secret as a `.env` on disk).
    //   3. path containment — canonicalise and assert we are still under
    //      the repo root. Only meaningful for the working-tree branch (a
    //      git tree has no symlink to follow out of the ODB), but the
    //      lexical + denylist checks are uniform across both.
    let path = safe_rel_path(path)?;
    crate::security::secrets::builtin_policy().check(path)?;
    let bytes = match rev {
        Some(rev) => {
            let git = GitRepo::open(&repo.path)?;
            git.read_blob(rev, path, DEFAULT_BLOB_SIZE_CAP)?
        }
        None => {
            let abs = crate::security::paths::contained_abs_path(&repo.path, path)?;
            std::fs::read(&abs).map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    ApiError::not_found(format!("{path}: not found in the working tree"))
                } else {
                    ApiError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("read {path}: {e}"),
                    )
                }
            })?
        }
    };
    let blob_hash = ingest::git_blob_hash(&bytes);
    Ok(FileRead { bytes, blob_hash })
}

// --- GET /api/repos --------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct RepoListEntry {
    pub name: String,
    pub path: String,
    pub file_count: u64,
    pub symbol_count: u64,
    pub head: Option<crate::git::HeadInfo>,
    /// `"watching"` | `"polling"` | `"gated"` — see `watcher_state`'s doc.
    pub watcher: &'static str,
    /// PRR-N12 (N1) — the SCIP-automation status for this repo. Additive:
    /// existing `/api/repos` consumers that don't read this field are
    /// unaffected.
    pub scip: ScipStatus,
    /// PRR-L2 — this repo's configured lip/1 live-LSP provider, or `None`
    /// when no `[[intel.providers]]` entry names it. Additive, same
    /// posture as `scip` above. Kept verbatim (first config-order match)
    /// for back-compat — see `intel_providers` below for a mixed-language
    /// repo's full set.
    pub intel: Option<RepoIntelStatus>,
    /// S2-D — EVERY `[[intel.providers]]` entry that names this repo, in
    /// config order (`crate::lip::LipRegistry::status_all_for_repo`), not
    /// just the first. `intel` above can only ever surface one provider,
    /// which understates a mixed-language repo (e.g. kb itself: rust +
    /// typescript) that legitimately has more than one provider wired to
    /// it. Empty when no provider names this repo at all. Purely additive
    /// — existing `/api/repos` consumers that only read `intel` are
    /// unaffected.
    pub intel_providers: Vec<RepoIntelStatus>,
    /// V70-A8 (D20 `repos --json`) — best-effort: `std::fs::metadata`'s
    /// `Permissions::readonly()` on the repo root, inverted. Unix-only
    /// nuance inherited from std: this reflects whether ANY write bit is
    /// set, not specifically "writable by the daemon's own uid" — an
    /// honest cheap signal, not an access-control check (the daemon
    /// doesn't chown/probe-write the tree to find out more precisely).
    /// `false` on any read failure (a repo root that vanished out from
    /// under a stale config entry is not writable, full stop).
    pub writable: bool,
    /// V70-A8 (D20 `repos --json`) — `.git` is a FILE (not a directory) in
    /// a linked worktree (`gitdir: <main>/.git/worktrees/<name>`), the same
    /// on-disk tell `git worktree` itself relies on. `false` for a bare
    /// repo, a main checkout, or a repo root that can't be read.
    pub is_worktree: bool,
}

/// PRR-L2 — one repo's lip/1 provider status
/// (`crate::lip::LipRegistry::status_for_repo`, computed from the cached
/// handshake state — NEVER a fresh network probe, see that fn's doc for
/// why `/api/repos` must stay cheap).
#[derive(Debug, Serialize)]
pub struct RepoIntelStatus {
    pub provider: String,
    pub langs: Vec<String>,
    pub alive: bool,
    pub server_version: Option<String>,
}

/// PRR-N12 (N1) — one repo's SCIP-automation status, computed fresh per
/// `GET /api/repos` request (cheap: a config-map lookup + two small `COUNT`
/// queries + the HEAD read `repos()` already does for `head` above — no new
/// caching layer). Never persisted itself; the only durable state behind it
/// is the `scip_runs` log (`crate::scip::scip_ingest_route` stamps a row on
/// every successful ingest — see migration `V0025__scip_runs.sql`).
#[derive(Debug, Serialize)]
pub struct ScipStatus {
    /// This repo has a `[[scip.repos]]` entry configured on THIS daemon.
    pub configured: bool,
    /// The configured indexer argv (`command[0]` is the executable), echoed
    /// back so `kb-code scip run` (kb-code-cli, N2) can spawn it without a
    /// second, separate config source — `kb-code-cli` never reads
    /// `kb-code.toml` itself. Empty when `configured` is `false`.
    pub command: Vec<String>,
    /// The configured `output` path (repo-root-relative). Empty when
    /// `configured` is `false`.
    pub output: String,
    /// The configured `langs` tags (informational — also the source of
    /// `docs_total`, below). Empty when `configured` is `false`.
    pub langs: Vec<String>,
    /// RFC3339 timestamp of the most recent successful `scip ingest` for
    /// this repo, or `None` if it has never been ingested.
    pub last_ingested_at: Option<String>,
    /// This repo's git HEAD at the moment of that most recent ingest.
    pub head_sha_at_ingest: Option<String>,
    /// This repo's CURRENT git HEAD (the same value `RepoListEntry::head`
    /// carries, mirrored here for a self-contained freshness comparison).
    pub current_head_sha: Option<String>,
    /// `head_sha_at_ingest == current_head_sha` (both `Some`) — `false` for
    /// a never-ingested repo, an unconfigured repo, or one whose HEAD has
    /// moved since the last ingest.
    pub fresh: bool,
    /// `COUNT(DISTINCT path)` among this repo's current files carrying at
    /// least one `source = 'scip'` occurrence row right now.
    pub docs_covered: u64,
    /// `COUNT(*)` of this repo's current files whose `lang` is one of the
    /// configured `langs`. `0` when `configured` is `false` (nothing to
    /// count against).
    pub docs_total: u64,
}

/// Compute [`ScipStatus`] for one repo — the ONE place both `routes::repos`
/// (production) and this module's own tests build it, so the "configured"/
/// "fresh"/count logic can't drift between what a real request sees and
/// what a test asserts.
fn scip_status(
    store: &Store,
    state: &SharedState,
    repo_name: &str,
    repo_id: i64,
    current_head_sha: Option<String>,
) -> ScipStatus {
    let entry = state.scip.for_repo(repo_name);
    let configured = entry.is_some();
    let (command, output, langs) = match entry {
        Some(e) => (e.command.clone(), e.output.clone(), e.langs.clone()),
        None => (Vec::new(), String::new(), Vec::new()),
    };

    let last_run = store.latest_scip_run(repo_id).ok().flatten();
    let last_ingested_at = last_run
        .as_ref()
        .and_then(|r| chrono::DateTime::from_timestamp(r.ingested_at, 0).map(|dt| dt.to_rfc3339()));
    let head_sha_at_ingest = last_run.as_ref().map(|r| r.head_sha.clone());
    let fresh = matches!(
        (&head_sha_at_ingest, &current_head_sha),
        (Some(a), Some(b)) if a == b
    );

    // 2026-09-01 perf finding (repos()/scip_status, live on prod under
    // v0.40.1 verification): `count_scip_covered_files` is a correlated
    // EXISTS subquery over `occurrences` — cheap per file, but run
    // UNCONDITIONALLY for every repo it was previously inline-called for
    // every repo regardless of SCIP config, one call to `run_blocking`
    // per query. `last_run.is_none()` means nothing was EVER ingested
    // (`replace_scip_occurrences` is the only writer, always preceded by
    // a `latest_scip_run` row), so `docs_covered` is provably `0` without
    // running the query — matching `count_files_by_langs`'s existing
    // empty-`langs` short-circuit just below. On kb's own repo (109k
    // files, no `[[scip.repos]]` entry, so never ingested) this turned a
    // guaranteed-zero query into a multi-minute correlated scan over a
    // very large `occurrences` table under real disk contention — the one
    // remaining slow path `run_blocking` correctly kept off the async
    // runtime, but still needlessly slow for callers.
    let docs_covered = if last_run.is_some() {
        store.count_scip_covered_files(repo_id).unwrap_or_default()
    } else {
        0
    };
    let docs_total = store
        .count_files_by_langs(repo_id, &langs)
        .unwrap_or_default();

    ScipStatus {
        configured,
        command,
        output,
        langs,
        last_ingested_at,
        head_sha_at_ingest,
        current_head_sha,
        fresh,
        docs_covered,
        docs_total,
    }
}

/// `"gated"` if a git operation (rebase/merge/cherry-pick/bisect) is
/// CURRENTLY suspending this repo's watcher — a fresh filesystem stat of
/// `git_dir`'s marker children per request (`mirror::MARKER_NAMES`, the
/// same set `mirror::gate` itself watches for), not a read of the drain
/// thread's live `RepoGate` state (which isn't shared cross-thread by
/// `mirror`'s design — see that module). Otherwise the daemon's resolved
/// watcher backend (`state.watch_mode`), which is constant for the whole
/// daemon since W1.4's `[watcher] mode` has no per-repo override.
fn watcher_state(git: &GitRepo, base: &'static str) -> &'static str {
    if crate::mirror::MARKER_NAMES
        .iter()
        .any(|m| git.git_dir().join(m).exists())
    {
        "gated"
    } else {
        base
    }
}

/// V70-A7 — the kbc-theme/1 registry, served verbatim.
///
/// The palette catalogue lives in THIS crate (`themes/registry.json`), not in
/// `web-code/`, for the same reason the command registry does: it is data the
/// daemon owns and every consumer must read the same bytes of. The SPA gets a
/// checked-in generated copy (`web-code/src/themes/registry.gen.ts`, pinned
/// byte-for-byte by `registry.gen.test.ts`) because its Docker build stage
/// only copies `web-code/` and so cannot reach across the crate boundary at
/// build time; an agent or a CLI caller reads it from here.
///
/// `include_str!` rather than a runtime file read: the registry is a build
/// input, not deployment state, so there is no path to get wrong, no IO to
/// fail at request time, and no way for a container to ship a binary and a
/// registry that disagree.
///
/// A pure read on the ordinary `auth_bearer` surface — a colour palette is
/// public-ish metadata, carries no repo content, and needs no loopback gate.
const THEME_REGISTRY_JSON: &str = include_str!("../themes/registry.json");

pub async fn themes() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        THEME_REGISTRY_JSON,
    )
}

/// V70-A5 — the kbc-cmd/1 command registry, served verbatim.
///
/// `crates/kb-code-server/commands/registry.json` is the ONE declaration home
/// for kb-code's keyboard and command surface (v7 §P2): the `?` sheet, the
/// palette, the which-key overlay and the printable cheatsheet are all
/// generated from it, and `kb-code commands doctor` is the CI gate that keeps
/// it honest. It lives in THIS crate for the same reason the theme registry
/// does — the daemon owns the bytes, and the CLI embeds the SAME file via its
/// own `include_str!` so the two binaries can never disagree about what a key
/// means. The SPA reads a checked-in generated copy
/// (`web-code/src/commands/registry.gen.ts`, byte-pinned by
/// `registry.gen.test.ts`), because its Docker build stage copies only
/// `web-code/`.
///
/// Unlike `/api/themes` this one carries an ETag: the registry is the biggest
/// build-time blob the SPA and every agent caller fetch, it changes only when
/// a human edits the file, and a strong validator computed once at first use
/// turns every subsequent poll into a 34-byte 304. The tag is the FNV-1a hash
/// of the embedded bytes — a build input, so it is constant for the life of
/// the process and needs no invalidation path.
const COMMAND_REGISTRY_JSON: &str = include_str!("../commands/registry.json");

/// FNV-1a over the embedded registry — a strong ETag with no dependency and
/// no allocation. Not a security hash: this is a cache validator over bytes
/// the daemon itself compiled in.
fn command_registry_etag() -> &'static str {
    static ETAG: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ETAG.get_or_init(|| {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in COMMAND_REGISTRY_JSON.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("\"kbc-cmd-{h:016x}\"")
    })
    .as_str()
}

pub async fn commands(headers: axum::http::HeaderMap) -> Response {
    let etag = command_registry_etag();
    // A conditional request may send a list, and a proxy may weaken the tag
    // (`W/"…"`). Accept `*`, an exact member, or the weak form of one —
    // anything else falls through to a full 200 rather than a wrong 304.
    let fresh = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .map(str::trim)
                .any(|c| c == "*" || c == etag || c.strip_prefix("W/").map(str::trim) == Some(etag))
        });
    if fresh {
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response();
    }
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ETAG, etag),
        ],
        COMMAND_REGISTRY_JSON,
    )
        .into_response()
}

/// `GET /api/repos` — every configured repo's counts, HEAD, and watcher
/// state. Counts come from `state.store`; HEAD/watcher state are live
/// per-request reads (a HEAD read is a handful of stats + one ref resolve —
/// cheap enough to do fresh every time rather than cache). 2026-08-31
/// incident (store.rs module doc): the store calls here (this fn's own
/// counts plus `scip_status`'s) are no longer assumed "fast enough to call
/// inline" — the whole per-repo loop below runs as one `run_blocking`
/// closure on the blocking pool instead.
///
/// V70-A8 (D20) additionally reports a top-level `loopback` bool: whether
/// THIS request arrived over loopback (same `ConnectInfo`-from-extensions +
/// `is_loopback_origin` derivation `search_unified` already uses — see that
/// handler's own doc for why this is the one route in this crate allowed to
/// read `ConnectInfo` directly). It answers "can *I*, this caller, reach
/// this daemon's loopback-only mutating routes (`checkout`, `session-diff`,
/// suggestion apply, …)" — a per-repo `writable` bit alone can't tell an
/// agent that if the daemon itself is only reachable non-loopback.
pub async fn repos(
    State(state): State<SharedState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let is_loopback = kb_server::middleware::is_loopback_origin(
        Some(peer.ip()),
        &headers,
        &state.auth.trusted_proxies,
    );
    let state_bg = state.clone();
    let list: Vec<RepoListEntry> = state
        .store
        .run_blocking(move |store| {
            state_bg
                .repos
                .iter()
                .map(|r| repos_entry_for(store, &state_bg, r))
                .collect()
        })
        .await;
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({ "repos": list, "loopback": is_loopback })),
    )
}

/// V70-A8 — best-effort writability probe for [`RepoListEntry::writable`].
/// See that field's doc for the Unix-permission-bits caveat.
fn repo_writable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| !m.permissions().readonly())
        .unwrap_or(false)
}

/// V70-A8 — linked-worktree detection for [`RepoListEntry::is_worktree`].
/// See that field's doc.
fn repo_is_worktree(path: &Path) -> bool {
    std::fs::metadata(path.join(".git"))
        .map(|m| m.is_file())
        .unwrap_or(false)
}

/// One [`RepoListEntry`] — split out of [`repos`] so the whole per-repo
/// computation (counts, HEAD, watcher state, SCIP status, intel status) is
/// a single unit `repos` can hand to `run_blocking` as ONE closure over
/// every configured repo, rather than N per-repo round trips.
fn repos_entry_for(store: &Store, state: &SharedState, r: &RepoEntry) -> RepoListEntry {
    let repo_id = state.repo_ids.get(&r.name).copied();
    let file_count = repo_id
        .and_then(|id| store.file_count(id).ok())
        .unwrap_or_default();
    let symbol_count = repo_id
        .and_then(|id| store.symbol_count_for_repo(id).ok())
        .unwrap_or_default();
    let git = GitRepo::open(&r.path).ok();
    let head = git.as_ref().and_then(|g| g.head_info().ok());
    let watcher = git
        .as_ref()
        .map(|g| watcher_state(g, state.watch_mode))
        .unwrap_or(state.watch_mode);
    let current_head_sha = head.as_ref().and_then(|h| h.sha.clone());
    let scip = match repo_id {
        Some(id) => scip_status(store, state, &r.name, id, current_head_sha),
        None => ScipStatus {
            configured: false,
            command: Vec::new(),
            output: String::new(),
            langs: Vec::new(),
            last_ingested_at: None,
            head_sha_at_ingest: None,
            current_head_sha,
            fresh: false,
            docs_covered: 0,
            docs_total: 0,
        },
    };
    let intel = state
        .lip
        .status_for_repo(&r.name)
        .map(|(provider, langs, snap)| RepoIntelStatus {
            provider,
            langs,
            alive: snap.alive,
            server_version: snap.server_version,
        });
    let intel_providers = state
        .lip
        .status_all_for_repo(&r.name)
        .into_iter()
        .map(|(provider, langs, snap)| RepoIntelStatus {
            provider,
            langs,
            alive: snap.alive,
            server_version: snap.server_version,
        })
        .collect();
    RepoListEntry {
        name: r.name.clone(),
        path: r.path.display().to_string(),
        file_count,
        symbol_count,
        head,
        watcher,
        scip,
        intel,
        intel_providers,
        writable: repo_writable(&r.path),
        is_worktree: repo_is_worktree(&r.path),
    }
}

// --- GET /api/tree ----------------------------------------------------------

/// `true` for the query-string spellings `"1"`/`"true"` (case-sensitive —
/// matches how every other literal query flag in this crate is documented,
/// e.g. `regex=`/`case=`), `false` for anything else INCLUDING absent —
/// shared by every additive `?flag=1`-shaped opt-in this crate's routes
/// take (V70-A3X: `/api/tree?worktree=1`, `/api/refs?remotes=1`). A plain
/// `bool` query field won't do: axum's `Query` extractor deserializes a
/// bare `bool` from `"true"`/`"false"` only, and this crate's design doc
/// explicitly spells these flags `=1`.
fn query_flag(v: Option<&str>) -> bool {
    matches!(v, Some("1") | Some("true"))
}

#[derive(Debug, Deserialize)]
pub struct TreeParams {
    pub repo: String,
    #[serde(default)]
    pub path: String,
    #[serde(rename = "ref")]
    pub rev: Option<String>,
    /// V70-A3X — `"1"`/`"true"` switches this route to the WORKING-TREE
    /// listing (`git_status::list_worktree_dir`) instead of the ODB read;
    /// absent (or any other value) is the default, byte-identical ODB path.
    /// Mutually exclusive with `ref` in spirit (a working-tree listing has
    /// no ref), but `ref` is simply ignored when both are given rather than
    /// erroring — same "additive param, old param becomes a no-op" shape
    /// `docs_query`'s `folder_exact` uses (invariant #35).
    #[serde(default)]
    pub worktree: Option<String>,
}

/// `GET /api/tree?repo=&path=&ref=[&worktree=1]` — W1.3's `list_tree` over
/// HTTP by default (`ref` defaults to `HEAD`; `path` defaults to the repo
/// root; always an ODB read, never the live working tree — the git
/// module's own scope, see `crate::git`'s doc). `?worktree=1` (V70-A3X)
/// switches to [`crate::git_status::list_worktree_dir`] instead — see that
/// fn's doc for exactly what it reports (one level, dirs derived,
/// deleted-in-worktree files omitted). The response shape is intentionally
/// NOT unified across the two modes (`entries` carries `git::tree::
/// TreeEntry` structs in the default branch, `git_status::WorktreeEntry`
/// structs under `?worktree=1`) — see `WorktreeEntry`'s doc for why it
/// can't carry the same `size`/`oid` fields.
pub async fn tree(
    State(state): State<SharedState>,
    Query(params): Query<TreeParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &params.repo)?;
    let path = safe_rel_path(&params.path)?.to_string();

    if query_flag(params.worktree.as_deref()) {
        let repo_root = repo.path.clone();
        let path_for_task = path.clone();
        let entries = tokio::task::spawn_blocking(move || {
            crate::git_status::list_worktree_dir(&repo_root, &path_for_task)
        })
        .await
        .map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("worktree tree task panicked: {e}"),
            )
        })??;
        return Ok((
            [(header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({
                "repo": params.repo,
                "path": path,
                "worktree": true,
                "entries": entries,
            })),
        ));
    }

    let rev = params.rev.as_deref().unwrap_or("HEAD");
    let git = GitRepo::open(&repo.path)?;
    let entries = git.list_tree(rev, &path)?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "repo": params.repo,
            "path": path,
            "ref": rev,
            "entries": entries,
        })),
    ))
}

// --- GET /api/refs (W4.1 — the reader's ref picker) ------------------------

#[derive(Debug, Deserialize)]
pub struct RefsParams {
    pub repo: String,
    /// V70-A3X — `"1"`/`"true"` opts into a separate `remotes` array
    /// alongside the existing local `refs` — absent (or any other value)
    /// keeps the response byte-identical to before this flag existed. See
    /// [`query_flag`]'s doc for the flag-value grammar.
    #[serde(default)]
    pub remotes: Option<String>,
}

/// `GET /api/refs?repo=[&remotes=1]` (W4.1, `remotes` V70-A3X) — every
/// branch and tag `GitRepo::list_refs` (W1.3) already knows how to list,
/// now reachable over HTTP. Pure ODB read, same cost class as `tree`/`file`
/// above — no `spawn_blocking` needed (a `references()` walk is a handful
/// of ref-file reads, not a subprocess). `?remotes=1` ADDITIONALLY reports
/// `refs/remotes/<remote>/*` branches (deliberately excluded from `refs`
/// itself — see `git::refs::list_refs`'s doc) via the ALREADY-existing
/// `GitRepo::list_remote_branches` (until now only consumed internally by
/// `branches_route`'s attribution logic, never exposed on this route).
pub async fn refs(
    State(state): State<SharedState>,
    Query(params): Query<RefsParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &params.repo)?;
    let git = GitRepo::open(&repo.path)?;
    let refs = git.list_refs()?;
    let mut body = serde_json::json!({
        "repo": params.repo,
        "refs": refs,
    });
    if query_flag(params.remotes.as_deref()) {
        let remotes = git.list_remote_branches()?;
        body["remotes"] = serde_json::to_value(&remotes)
            .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(body)))
}

// --- GET /api/status (V70-A3X) -------------------------------------------

impl From<crate::git_status::StatusError> for ApiError {
    fn from(e: crate::git_status::StatusError) -> Self {
        use crate::git_status::StatusError::*;
        match e {
            Spawn(err) => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("spawn git: {err}"),
            ),
            // `git status`/`git ls-files` take no caller-supplied argument
            // beyond an already-validated repo-relative path — a failure
            // here is a daemon-side fault (a corrupt repo), never a caller
            // mistake, same reasoning as `RepoStateError`'s own `From` impl
            // above.
            GitFailed { stderr, .. } => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, stderr),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct StatusParams {
    pub repo: String,
}

/// `GET /api/status?repo=` (V70-A3X) — `git status --porcelain=v2 -z`,
/// parsed into `{paths, dirty, generation}` (`git_status::RepoStatus`'s own
/// doc has the exact shape). Cached per `Store::generation()` inside
/// `git_status::StatusIndex` — the generation READ and the (possible)
/// subprocess call happen in the SAME `spawn_blocking` closure (2026-08-31
/// incident precedent, `store.rs`'s module doc): both need to observe the
/// SAME generation snapshot, and `Store::generation()` itself is cheap
/// enough that routing it through the blocking pool alongside the git call
/// costs nothing extra.
pub async fn status_route(
    State(state): State<SharedState>,
    Query(params): Query<StatusParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo_root = repo.path.clone();
    let status_index = state.status_index.clone();
    let status = state
        .store
        .run_blocking(move |store| status_index.status(store, repo_id, &repo_root))
        .await?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "repo": params.repo,
            "paths": status.paths,
            "dirty": status.dirty,
            "generation": status.generation,
        })),
    ))
}

// --- GET /api/file -----------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct FileParams {
    pub repo: String,
    pub path: String,
    #[serde(rename = "ref")]
    pub rev: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FileResponse {
    pub repo: String,
    pub path: String,
    #[serde(rename = "ref")]
    pub rev: Option<String>,
    pub size: u64,
    pub blob_hash: String,
    pub lang: Option<&'static str>,
    /// V72-H1 (D7) — the `syntax/1` extraction tier for this file TYPE:
    /// `"full"` | `"highlight_only"` | `"none"`. It says what this
    /// daemon's pipeline runs for files of this kind, NOT that this
    /// particular blob has been derived (that is what `symbols`/
    /// `highlights` themselves report). A `highlight_only` file therefore
    /// has spans and an honestly EMPTY `symbols` list — "no symbols by
    /// tier", not an empty list that looks like a bug. Additive: a client
    /// that ignores both fields sees byte-identical behaviour.
    pub tier: &'static str,
    /// Why `tier` is not `"full"` — `null` when it is. An unregistered
    /// file type says so here rather than being silently indistinguishable
    /// from a registered one with no grammar.
    pub tier_reason: Option<&'static str>,
    /// `"utf8"` | `"base64"` — which of `content`'s two encodings applies.
    pub encoding: &'static str,
    pub content: String,
    /// Empty when `lang` has no grammar, OR when this blob hasn't been
    /// derived yet (boot's `HEAD`-tree walk and the live mirror are the only
    /// writers — see the module doc).
    pub symbols: Vec<Symbol>,
    /// `None` under the same two conditions as `symbols` (no grammar, or
    /// not yet derived) — `Some([])` never happens (a parseable file with
    /// zero highlight spans doesn't occur for any Wave-1 grammar on
    /// non-empty content, and empty content parses to `[]` either way, so
    /// this distinction is purely "did we look."
    pub highlights: Option<Vec<crate::highlight::Span>>,
    /// V72-H2b (D7) — what the HIGHLIGHT cache gate would say about this
    /// blob: `"hit"` (spans exist under the CURRENT `highlight_salt`),
    /// `"miss"` (this blob has not been painted under it yet — a
    /// `highlight_salt` bump puts every blob here until the mirror catches
    /// up) or `"skipped_tier"` (the file TYPE derives no spans, or its
    /// content was capped). Additive and read-only: this route never
    /// derives (see its doc), so the field REPORTS the gate rather than
    /// running it.
    pub highlight_cache: &'static str,
    /// V70-A2 (the critique's MISSING #5) — `true` when a cheap content
    /// sniff (`security::secrets::redaction_hint`) matched a
    /// credential-shaped pattern in this file's TEXT: a private-key
    /// header, an AWS-shaped access key, or a line-leading
    /// `password=`/`secret=`-style assignment with a real-looking value.
    ///
    /// A HINT, not a redaction: the bytes are served in full and the SPA
    /// decides whether to show a banner. Redacting on a heuristic would
    /// be a correctness bug in a code reader (a docs file that TALKS
    /// about `AWS_ACCESS_KEY_ID` is not a leak), and the honest-signal
    /// discipline this codebase applies to trust classes applies here
    /// too. The DENYLIST (`urn:kb:errors:redacted-by-policy`) is the
    /// mechanism that actually refuses.
    ///
    /// Additive: `false` on every non-text/base64 response, so a client
    /// that ignores the field sees byte-identical behaviour.
    pub redaction_hint: bool,
}

/// `GET /api/file?repo=&path=&ref=` — blob content at `ref`, or (no `ref`)
/// the WORKING-TREE bytes via `fs::read`. `content` travels as UTF-8 text
/// when the bytes decode cleanly, else base64 (`encoding` tags which).
/// `symbols`/`highlights` are a STORE LOOKUP by the read bytes' own content
/// hash — never derived on the spot: deriving here would either (a) skip
/// persisting (repeated reads re-parse every time) or (b) persist under an
/// arbitrary ref's content, which would corrupt `files`' "current working-
/// tree state" invariant if this were the no-ref path (see `store.rs`'s
/// module doc) — so this route stays a pure read, and an unindexed blob
/// just reports empty/`None`.
pub async fn file(
    State(state): State<SharedState>,
    Query(params): Query<FileParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    // V70-A2 (SEC-13) — the CONFIGURED denylist (built-in floor +
    // `[security] secret_globs`) at the directly-addressable read.
    // `read_repo_file` re-checks the floor for every caller that has no
    // `AppState`; see `security::secrets`' two-level split.
    state.secret_policy.check(&params.path)?;
    let read = read_repo_file(repo, &params.path, params.rev.as_deref())?;
    let lang_info = lang::detect(&params.path, Some(&read.bytes));
    // V72-H1 — the same registry lookup `ingest` makes, so the wire can
    // never disagree with what the pipeline actually did.
    let (tier, tier_reason) = crate::syntax::tier_for_path(&params.path, Some(&read.bytes));

    let (encoding, content) = match String::from_utf8(read.bytes.clone()) {
        Ok(s) => ("utf8", s),
        Err(_) => (
            "base64",
            base64::engine::general_purpose::STANDARD.encode(&read.bytes),
        ),
    };

    // 2026-08-31 incident (store.rs module doc): the frecency bump and the
    // symbols/highlights lookup are the only store calls this handler
    // makes, with no async work between them — one closure.
    let opened_at_ms = chrono::Utc::now().timestamp_millis();
    let path = params.path.clone();
    let repo_label = params.repo.clone();
    let blob_hash = read.blob_hash.clone();
    // V72-H2b — two salts, two lookups. Reading `highlights` under the
    // SYMBOL salt (the pre-split shape) would silently return `None` for
    // every blob the moment the two diverged.
    let salt = lang_info.map(|l| l.symbol_salt);
    let hl_salt = lang_info.map(|l| l.highlight_salt);
    let paints = crate::syntax::row_for_path(&params.path, Some(&read.bytes))
        .map(|row| row.plan().highlight)
        .unwrap_or(false);
    let (symbols, highlights) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            // W2.1 — frecency signal for the files search lane
            // (`search::files`'s module doc). Best-effort: bumped on EVERY
            // successful read regardless of `rev` (a historical-ref read
            // still means "the operator/agent looked at this path" — the
            // frecency blend doesn't distinguish why), a side effect that
            // never fails the request itself.
            if let Err(e) = store.bump_file_open(repo_id, &path, opened_at_ms) {
                tracing::warn!(
                    repo = %repo_label, path = %path, error = %e,
                    "kb-code: failed to record file-open frecency event",
                );
            }
            match (salt, hl_salt) {
                (Some(salt), Some(hl_salt)) => Ok((
                    store.symbols_for_blob(&blob_hash, salt)?,
                    store.highlights_for_blob(&blob_hash, hl_salt)?,
                )),
                _ => Ok((Vec::new(), None)),
            }
        })
        .await?;

    let body = FileResponse {
        repo: params.repo,
        path: params.path,
        rev: params.rev,
        size: read.bytes.len() as u64,
        blob_hash: read.blob_hash,
        lang: lang_info.map(|l| l.id),
        tier: tier.as_str(),
        tier_reason,
        encoding,
        // V70-A2 — sniff the TEXT only; a base64 body is opaque bytes and
        // a hint over its encoding would be noise.
        redaction_hint: encoding == "utf8" && crate::security::secrets::redaction_hint(&content),
        content,
        symbols,
        highlight_cache: if !paints {
            crate::ingest::HighlightCache::SkippedTier
        } else if highlights.is_some() {
            crate::ingest::HighlightCache::Hit
        } else {
            crate::ingest::HighlightCache::Miss
        }
        .as_str(),
        highlights,
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(body)))
}

// --- GET /api/symbols --------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SymbolsParams {
    pub repo: String,
    /// Per-file form (with `path`): exactly the file's symbol list.
    pub path: Option<String>,
    #[serde(rename = "ref")]
    pub rev: Option<String>,
    /// Repo-wide form (with `q`): a case-insensitive SUBSTRING scan over
    /// every symbol this repo has indexed — NOT the real fuzzy-match lane
    /// (nucleo-backed ranking), which is W2.1's job.
    pub q: Option<String>,
}

#[derive(Debug, Serialize)]
struct SymbolMatch {
    path: String,
    #[serde(flatten)]
    symbol: Symbol,
}

/// Repo-wide substring query results are capped — a plain Wave-1 scan, not
/// a ranked/paginated search surface.
const MAX_SYMBOL_MATCHES: usize = 200;

/// `GET /api/symbols?repo=&path=[&ref=]` (per-file) or
/// `GET /api/symbols?repo=&q=` (repo-wide substring) — exactly one of
/// `path`/`q` must be given. See the module doc for why this is a pure
/// store lookup, never an on-demand derive.
pub async fn symbols(
    State(state): State<SharedState>,
    Query(params): Query<SymbolsParams>,
) -> Result<Response, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    match (&params.path, &params.q) {
        (Some(_), Some(_)) => Err(ApiError::bad_request(
            "pass exactly one of `path` or `q`, not both",
        )),
        (None, None) => Err(ApiError::bad_request("pass one of `path` or `q`")),
        (Some(path), None) => {
            let read = read_repo_file(repo, path, params.rev.as_deref())?;
            let lang_info = lang::detect(path, Some(&read.bytes));
            // 2026-08-31 incident (store.rs module doc): single store call,
            // still wrapped so it can never park this async worker.
            let symbols = match lang_info {
                Some(li) => {
                    let blob_hash = read.blob_hash.clone();
                    state
                        .store
                        .run_blocking(move |store| {
                            store.symbols_for_blob(&blob_hash, li.symbol_salt)
                        })
                        .await?
                }
                None => Vec::new(),
            };
            // V72-H1 — the extraction tier rides the per-file symbol list
            // for the same reason it rides `/api/file`: an empty
            // `symbols` array on a `highlight_only`/`none` file is a
            // DESIGN outcome, and a reader (or an outline renderer) must
            // be able to say "no symbols by tier" instead of showing an
            // empty list that reads as a failure.
            let (tier, tier_reason) = crate::syntax::tier_for_path(path, Some(&read.bytes));
            Ok((
                [(header::CACHE_CONTROL, "no-store")],
                Json(serde_json::json!({
                    "repo": params.repo,
                    "path": path,
                    "ref": params.rev,
                    "lang": lang_info.map(|l| l.id),
                    "tier": tier.as_str(),
                    "tier_reason": tier_reason,
                    "symbols": symbols,
                })),
            )
                .into_response())
        }
        (None, Some(q)) => {
            let needle = q.to_lowercase();
            // 2026-08-31 incident (store.rs module doc): single store call,
            // still wrapped so it can never park this async worker.
            let all = state
                .store
                .run_blocking(move |store| store.symbols_for_repo(repo_id))
                .await?;
            let matches: Vec<SymbolMatch> = all
                .into_iter()
                .filter(|(_, sym)| sym.name.to_lowercase().contains(&needle))
                .take(MAX_SYMBOL_MATCHES)
                .map(|(path, symbol)| SymbolMatch { path, symbol })
                .collect();
            Ok((
                [(header::CACHE_CONTROL, "no-store")],
                Json(serde_json::json!({
                    "repo": params.repo,
                    "q": q,
                    "matches": matches,
                })),
            )
                .into_response())
        }
    }
}

// --- GET /api/search/{files,symbols,text} (W2.1) ------------------------

/// Resolve `?repo=` to the `(repo_name, repo_id)` pairs a search-lane call
/// should run over: `Some(name)` scopes to exactly that repo (404 if
/// unknown, via `find_repo`); `None` means "every configured repo" — the
/// files/symbols lanes' "or all repos" mode. `pub(crate)` — reused by
/// `search::unified`'s files/symbols/text lane runners (W2.4), which resolve
/// the SAME `repo`/`repo:` scope once and share it across all three, rather
/// than three independent `find_repo` calls.
pub(crate) fn resolve_search_repos(
    state: &SharedState,
    repo: Option<&str>,
) -> Result<Vec<(String, i64)>, ApiError> {
    match repo {
        Some(name) => {
            let (_repo, id) = find_repo(state, name)?;
            Ok(vec![(name.to_string(), id)])
        }
        None => Ok(state
            .repos
            .iter()
            .filter_map(|r| state.repo_ids.get(&r.name).map(|&id| (r.name.clone(), id)))
            .collect()),
    }
}

/// `pub(crate)` — `search::unified` reuses the SAME default/ceiling for the
/// box's own `?limit=` (W2.4) rather than re-deriving it.
pub(crate) fn clamp_limit(limit: Option<usize>) -> usize {
    limit
        .unwrap_or(search::DEFAULT_LIMIT)
        .clamp(1, search::MAX_LIMIT)
}

#[derive(Debug, Deserialize)]
pub struct SearchFilesParams {
    pub repo: Option<String>,
    #[serde(default)]
    pub q: String,
    pub limit: Option<usize>,
}

/// `GET /api/search/files?q=&repo=&limit=` — nucleo fuzzy match over file
/// paths, blended with open-history frecency (`search::files`'s module
/// doc). An empty (or missing) `q` is NOT an error here — it returns the
/// `limit` most recently opened files instead (`search::files::FileIndex::
/// recent`), so a client can render a "recent files" list by hitting this
/// same endpoint with no query.
pub async fn search_files(
    State(state): State<SharedState>,
    Query(params): Query<SearchFilesParams>,
) -> Result<impl IntoResponse, ApiError> {
    let repos = resolve_search_repos(&state, params.repo.as_deref())?;
    let limit = clamp_limit(params.limit);
    let q = params.q.trim().to_string();
    let file_index = state.file_index.clone();
    // 2026-08-31 incident (store.rs module doc): whichever branch runs
    // makes its store calls in one closure on the blocking pool.
    // V71-D1 — the same `LaneOpts` (and therefore the same factor flags and
    // the same matcher) the unified box hands its files lane: a hit must not
    // rank differently depending on which route asked.
    let opts = search::LaneOpts {
        factors: state.search_factors,
        ..Default::default()
    };
    let hits = if q.is_empty() {
        state
            .store
            .run_blocking(move |store| file_index.recent(store, &repos, limit, &opts))
            .await?
    } else {
        let now_ms = chrono::Utc::now().timestamp_millis();
        state
            .store
            .run_blocking(move |store| file_index.search(store, &repos, &q, limit, now_ms, &opts))
            .await?
    };
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({ "q": params.q, "hits": hits })),
    ))
}

#[derive(Debug, Deserialize)]
pub struct SearchSymbolsParams {
    pub repo: Option<String>,
    #[serde(default)]
    pub q: String,
    pub limit: Option<usize>,
}

/// `GET /api/search/symbols?q=&repo=&limit=` — nucleo fuzzy match over
/// symbol names (`search::symbols`'s module doc). Unlike the files lane, an
/// empty `q` is a 400 here: there is no equivalent "recent symbols" concept
/// to fall back to.
pub async fn search_symbols(
    State(state): State<SharedState>,
    Query(params): Query<SearchSymbolsParams>,
) -> Result<impl IntoResponse, ApiError> {
    let repos = resolve_search_repos(&state, params.repo.as_deref())?;
    let q = params.q.trim();
    if q.is_empty() {
        return Err(ApiError::bad_request("q must not be empty"));
    }
    let limit = clamp_limit(params.limit);
    let q = q.to_string();
    let symbol_index = state.symbol_index.clone();
    // 2026-08-31 incident (store.rs module doc): single store-backed call,
    // still wrapped so it can never park this async worker.
    let opts = search::LaneOpts {
        factors: state.search_factors,
        ..Default::default()
    };
    let hits = state
        .store
        .run_blocking(move |store| symbol_index.search(store, &repos, &q, limit, &opts))
        .await?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({ "q": params.q, "hits": hits })),
    ))
}

impl From<search::TextSearchError> for ApiError {
    fn from(e: search::TextSearchError) -> Self {
        match e {
            search::TextSearchError::EmptyQuery | search::TextSearchError::Pattern(_) => {
                ApiError::bad_request(e.to_string())
            }
            search::TextSearchError::Store(store_err) => store_err.into(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SearchTextParams {
    /// Required — unlike the files/symbols lanes, text search reads a
    /// specific repo's WORKING-TREE files off disk (`search::text`'s module
    /// doc), so there is no cheap "all repos" fan-out for this lane in
    /// W2.1's scope (the per-request `time_budget` is sized for one repo).
    pub repo: String,
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub regex: bool,
    #[serde(default)]
    pub case: bool,
}

/// `GET /api/search/text?q=&repo=&regex=&case=` — `grep-searcher` streaming
/// literal/regex search over `repo`'s working tree (`search::text`'s module
/// doc). `regex=true` switches from literal to regex syntax; `case=true`
/// makes the match case-sensitive (default: case-insensitive literal).
pub async fn search_text_route(
    State(state): State<SharedState>,
    Query(params): Query<SearchTextParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let q = params.q.trim();
    if q.is_empty() {
        return Err(ApiError::bad_request("q must not be empty"));
    }
    let repo_root = repo.path.clone();
    let q_owned = q.to_string();
    let regex = params.regex;
    let case = params.case;
    // 2026-08-31 incident (store.rs module doc): `search_text` opens with a
    // store call (`list_files`) and then runs a genuinely slow streaming
    // grep bounded by `DEFAULT_TIME_BUDGET` — the whole call runs on the
    // blocking pool, not just the store read.
    let factors = state.search_factors;
    let symbol_index = state.symbol_index.clone();
    let resp = state
        .store
        .run_blocking(move |store| {
            // V71-D1b — same candidate-first signal `search::unified`'s
            // text lane passes; see `search::symbol_candidate_paths`'s doc.
            let atoms = search::matcher::identifier_atoms(&q_owned);
            let candidate_paths =
                search::symbol_candidate_paths(&symbol_index, store, repo_id, &atoms);
            let opts = search::LaneOpts {
                factors,
                candidate_paths: candidate_paths.as_ref(),
                ..Default::default()
            };
            search::search_text(
                store,
                &repo_root,
                repo_id,
                &q_owned,
                regex,
                case,
                search::text::DEFAULT_TIME_BUDGET,
                &opts,
            )
        })
        .await?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "repo": params.repo,
            "q": params.q,
            "regex": params.regex,
            "case": params.case,
            "results": resp.results,
            "truncated": resp.truncated,
            "time_budget_exceeded": resp.time_budget_exceeded,
            // V71-D1b — additive; see `search::text::TextSearchResponse`'s
            // doc for what these count.
            "scanned": resp.scanned,
            "total": resp.total,
        })),
    ))
}

// --- GET /api/search/semantic (W2.3) -------------------------------------

impl From<semantic::SemanticSearchError> for ApiError {
    fn from(e: semantic::SemanticSearchError) -> Self {
        match e {
            semantic::SemanticSearchError::EmptyQuery => ApiError::bad_request(e.to_string()),
            semantic::SemanticSearchError::Store(_) | semantic::SemanticSearchError::Embed(_) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SearchSemanticParams {
    pub repo: Option<String>,
    #[serde(default)]
    pub q: String,
    pub limit: Option<u32>,
}

/// `GET /api/search/semantic?q=&repo=&limit=` (W2.3) — embed `q` (see
/// `semantic::chunk::QUERY_PREFIX`), lance nearest-neighbour over-fetch,
/// max-pool per `(repo, path)` (`semantic::store::ChunkStore::search`'s
/// doc). 400s — HONESTLY, never silently empty — when the semantic lane
/// isn't enabled at all, or isn't enabled for the specific `repo`
/// requested (`config::SemanticSection::repo_enabled`'s per-repo staged-
/// rollout gate). The embed call is genuinely blocking IPC, so it runs
/// inside `spawn_blocking` (`semantic::search::embed_query`'s doc).
pub async fn search_semantic(
    State(state): State<SharedState>,
    Query(params): Query<SearchSemanticParams>,
) -> Result<impl IntoResponse, ApiError> {
    match &params.repo {
        Some(name) => {
            find_repo(&state, name)?; // 404 for an unknown repo name
            if !state.semantic.repo_enabled(name) {
                return Err(ApiError::bad_request(format!(
                    "semantic search is not enabled for repo {name:?} — add it to \
                     [semantic] repos (with [semantic] enabled = true) in kb-code.toml to opt in"
                )));
            }
        }
        None => {
            if !state.semantic.enabled || state.semantic.repos.is_empty() {
                return Err(ApiError::bad_request(
                    "semantic search is disabled — set [semantic] enabled = true and list at \
                     least one repo under [semantic] repos in kb-code.toml",
                ));
            }
        }
    }
    let (Some(chunk_store), Some(embedder)) = (
        state.semantic_chunk_store.clone(),
        state.semantic_embedder.clone(),
    ) else {
        return Err(ApiError::bad_request(
            "semantic search is disabled for this daemon",
        ));
    };

    let (q, limit) = semantic::search::validate_query(&params.q, params.limit)?;
    let query_vec = semantic::search::embed_query(&embedder, &q).await?;
    let hits =
        semantic::search::search(&chunk_store, &query_vec, params.repo.as_deref(), limit).await?;

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({ "q": q, "repo": params.repo, "hits": hits })),
    ))
}

// --- GET /api/search — the unified Search-Everywhere box (W2.4) --------

#[derive(Debug, Deserialize)]
pub struct SearchUnifiedParams {
    #[serde(default)]
    pub q: String,
    pub repo: Option<String>,
    pub limit: Option<usize>,
}

/// `GET /api/search?q=&repo=&limit=` (W2.4) — the unified Search-Everywhere
/// box: one query, grammar-routed to one or more of the six lanes
/// (`search::grammar::parse`), run concurrently, returned as FIXED,
/// never-interleaved sections (`search::unified::run`'s own doc has the
/// full contract). The ONLY route in this crate that reads `ConnectInfo`
/// directly in a plain handler (not middleware) — invariant #3 is about
/// pulling it from request EXTENSIONS rather than failing when it's absent;
/// as a typed axum extractor argument on a handler served via
/// `into_make_service_with_connect_info` (`lib.rs`), this IS that same
/// extensions-backed read, just spelled the ordinary axum way (mirrors
/// `kb_server::routes::docs::artifact_bytes`'s own `ConnectInfo(peer)`
/// parameter for the identical "decide a per-request loopback-gated feature
/// inside the handler, after `auth_bearer` has already admitted the
/// request" shape).
pub async fn search_unified(
    State(state): State<SharedState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Query(params): Query<SearchUnifiedParams>,
) -> impl IntoResponse {
    let is_loopback = kb_server::middleware::is_loopback_origin(
        Some(peer.ip()),
        &headers,
        &state.auth.trusted_proxies,
    );
    let body = crate::search::unified::run(
        &state,
        is_loopback,
        &params.q,
        params.repo.as_deref(),
        params.limit,
    )
    .await;
    ([(header::CACHE_CONTROL, "no-store")], Json(body))
}

// --- GET /api/blame + GET /api/blame/timeline (W3.1) ---------------------

impl From<crate::blame::BlameError> for ApiError {
    fn from(e: crate::blame::BlameError) -> Self {
        use crate::blame::BlameError::*;
        match e {
            Git(git_err) => git_err.into(),
            Incremental(inc_err) => incremental_to_api_error(inc_err),
            Read { path, source } => {
                if source.kind() == std::io::ErrorKind::NotFound {
                    ApiError::not_found(format!("{path}: not found in the working tree"))
                } else {
                    ApiError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("read {path}: {source}"),
                    )
                }
            }
        }
    }
}

/// `git blame`'s own "no such path" fatal maps to 404 (the same signal
/// `From<GitError>` already uses for `PathNotFound`); every OTHER git
/// failure (a malformed `-L` range, an unresolvable rev inside the
/// subprocess, etc.) maps to 400 rather than 500 — it's an ill-formed
/// REQUEST from the caller's point of view, not a daemon-side fault.
fn incremental_to_api_error(e: crate::blame::IncrementalError) -> ApiError {
    use crate::blame::IncrementalError::*;
    match e {
        GitFailed { stderr, .. } if stderr.contains("no such path") => {
            ApiError::not_found(stderr.trim().to_string())
        }
        GitFailed { stderr, .. } => ApiError::bad_request(stderr.trim().to_string()),
        Spawn(err) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("spawn git blame: {err}"),
        ),
        Io(err) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("read git blame output: {err}"),
        ),
        MalformedHeader { line } => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("malformed git blame output: {line:?}"),
        ),
        TruncatedOutput => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "git blame output ended mid-block",
        ),
    }
}

/// `pub(crate)` — reused by W3.4's `provenance::story` (its own bounded
/// per-region `git log -L` calls hit the exact same failure shapes).
pub(crate) fn timeline_to_api_error(e: crate::blame::TimelineError) -> ApiError {
    use crate::blame::TimelineError::*;
    match e {
        // `git log -L` failures are near-always a caller-supplied line
        // number out of range for the file — 400, not 500.
        GitFailed { stderr, .. } => ApiError::bad_request(stderr.trim().to_string()),
        Spawn(err) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("spawn git log: {err}"),
        ),
        InvalidUtf8 => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "git log -L produced non-UTF8 output",
        ),
    }
}

#[derive(Debug, Deserialize)]
pub struct BlameParams {
    pub repo: String,
    pub path: String,
    #[serde(rename = "ref")]
    pub rev: Option<String>,
    /// 1-based inclusive line-range narrowing — must be given together with
    /// `end`, or not at all.
    pub start: Option<u32>,
    pub end: Option<u32>,
}

fn parse_line_range(start: Option<u32>, end: Option<u32>) -> Result<Option<(u32, u32)>, ApiError> {
    match (start, end) {
        (Some(s), Some(e)) if s >= 1 && e >= s => Ok(Some((s, e))),
        (Some(_), Some(_)) => Err(ApiError::bad_request(
            "start must be >= 1 and end must be >= start",
        )),
        (None, None) => Ok(None),
        _ => Err(ApiError::bad_request(
            "start and end must both be given, or neither",
        )),
    }
}

/// `GET /api/blame?repo=&path=[&ref=][&start=&end=]` (W3.1) — per-region
/// `git blame` attribution for a file (`crate::blame::blame_file`'s own doc
/// has the full clean/dirty/caching contract). `ref` omitted blames the
/// repo's CURRENT state (HEAD, or the live working-tree bytes if `path` has
/// an uncommitted edit — `dirty` in the response distinguishes the two);
/// `ref` given blames that historical revision, always cacheably. `start`/
/// `end` (both required together) narrow the response to the regions
/// overlapping that 1-based inclusive line range, without changing what's
/// cached (see the module doc on `blame`'s "Gitiles' shape").
///
/// A real `git blame` subprocess is genuinely blocking I/O — this handler
/// runs it inside `spawn_blocking`, reopening `GitRepo` FRESH there (it is
/// `!Send`, so an already-open handle can never cross the boundary — see
/// `crate::git`'s own module doc on this).
pub async fn blame(
    State(state): State<SharedState>,
    Query(params): Query<BlameParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let path = safe_rel_path(&params.path)?.to_string();
    let line_range = parse_line_range(params.start, params.end)?;

    let repo_root = repo.path.clone();
    let cache = state.blame_cache.clone();
    let rev = params.rev.clone();
    let path_for_task = path.clone();
    let result =
        tokio::task::spawn_blocking(move || -> Result<crate::blame::BlameResult, ApiError> {
            let git = GitRepo::open(&repo_root)?;
            crate::blame::blame_file(
                &cache,
                &git,
                repo_id,
                &repo_root,
                &path_for_task,
                rev.as_deref(),
                line_range,
            )
            .map_err(ApiError::from)
        })
        .await
        .map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("blame task panicked: {e}"),
            )
        })??;

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "repo": params.repo,
            "path": path,
            "ref": result.resolved_ref,
            "dirty": result.dirty,
            "cached": result.cached,
            "regions": result.regions,
            "truncated": result.truncated,
        })),
    ))
}

#[derive(Debug, Deserialize)]
pub struct BlameTimelineParams {
    pub repo: String,
    pub path: String,
    pub line: u32,
    pub max: Option<usize>,
}

/// `GET /api/blame/timeline?repo=&path=&line=&max=` (W3.1) — the bounded,
/// newest-first set of commits that have ever touched `line`
/// (`crate::blame::line_timeline`'s own doc). `max` defaults to
/// `blame::DEFAULT_MAX_ENTRIES` (20), clamped to a `[1, 200]` ceiling so an
/// operator-typo'd `max` can't turn this into an unbounded full-history
/// walk.
pub async fn blame_timeline(
    State(state): State<SharedState>,
    Query(params): Query<BlameTimelineParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &params.repo)?;
    let path = safe_rel_path(&params.path)?.to_string();
    if params.line < 1 {
        return Err(ApiError::bad_request("line must be >= 1"));
    }
    let max = params
        .max
        .unwrap_or(crate::blame::DEFAULT_MAX_ENTRIES)
        .clamp(1, 200);

    let repo_root = repo.path.clone();
    let line = params.line;
    let path_for_task = path.clone();
    let entries = tokio::task::spawn_blocking(move || {
        crate::blame::line_timeline(&repo_root, &path_for_task, line, max)
    })
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("timeline task panicked: {e}"),
        )
    })?
    .map_err(timeline_to_api_error)?;

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "repo": params.repo,
            "path": path,
            "line": params.line,
            "max": max,
            "entries": entries,
        })),
    ))
}

// --- GET /api/diff (W4.2 — the reader's diff view) --------------------------

impl From<crate::diff::DiffError> for ApiError {
    fn from(e: crate::diff::DiffError) -> Self {
        use crate::diff::DiffError::*;
        match e {
            Spawn(err) => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("spawn git diff: {err}"),
            ),
            // A malformed/unresolvable `from`/`to` revspec is the near-
            // exclusive real-world cause here (same reasoning as
            // `timeline_to_api_error`'s `git log -L` case) — a caller
            // mistake, not a daemon-side fault.
            GitFailed { stderr, .. } => ApiError::bad_request(stderr.trim().to_string()),
            // A dash-prefixed `from`/`to` — rejected before git ever spawns
            // (argument-injection guard, see `DiffError::BadRevspec`'s doc).
            err @ BadRevspec(_) => ApiError::bad_request(err.to_string()),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct DiffParams {
    pub repo: String,
    pub path: String,
    pub from: String,
    /// Omitted = diff `from` against the CURRENT WORKING TREE (mirrors
    /// `GET /api/file`'s "no `ref` = working tree" default) — see
    /// `diff::diff_file`'s doc.
    pub to: Option<String>,
}

/// `GET /api/diff?repo=&path=&from=[&to=]` (W4.2) — a single file's unified
/// diff between two refs, or `from` vs. the working tree when `to` is
/// omitted (`crate::diff::diff_file`'s own doc has the full contract). The
/// raw unified-diff TEXT travels as-is; the SPA parses hunk headers
/// client-side (`web-code/src/lib/diff.ts`) rather than this route
/// pre-structuring them — see the `diff` module doc for why.
///
/// A real `git diff` subprocess is genuinely blocking I/O — this handler
/// runs it inside `spawn_blocking`, same discipline as `blame`/
/// `blame_timeline` above.
pub async fn diff_route(
    State(state): State<SharedState>,
    Query(params): Query<DiffParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &params.repo)?;
    let path = safe_rel_path(&params.path)?.to_string();

    let repo_root = repo.path.clone();
    // V70-A2 (SEC-17) — parse the caller's revspecs into the validated
    // type BEFORE the subprocess task exists; a bad one is a 400 here.
    let from = parse_revspec(&params.from)?;
    let to = params.to.as_deref().map(parse_revspec).transpose()?;
    let path_for_task = path.clone();
    let diff_text = tokio::task::spawn_blocking(move || {
        crate::diff::diff_file(&repo_root, &from, to.as_ref(), &path_for_task)
    })
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("diff task panicked: {e}"),
        )
    })??;

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "repo": params.repo,
            "path": path,
            "from": params.from,
            "to": params.to,
            "diff": diff_text,
        })),
    ))
}

// --- GET /api/join/commit (W3.2 — the join ladder) ------------------------

#[derive(Debug, Deserialize)]
pub struct JoinCommitParams {
    pub repo: String,
    pub sha: String,
}

/// `GET /api/join/commit?repo=&sha=` (W3.2) — the join ladder's HTTP
/// surface: given a configured repo NAME and a git sha (full or a short
/// prefix — at least 4 hex chars, `crate::join::ladder::is_plausible_sha`;
/// deliberately more permissive than kb's own `by-commit` 7-char floor, see
/// that fn's doc), resolve which session (if any) produced it
/// (`join::ladder::resolve_commit`'s six-arm ladder). `400` on a malformed
/// `sha` or an unknown `repo`; never 500s past that — an unreachable kb
/// daemon degrades to a `confidence: "none"` body (`via: "kb-unreachable"`),
/// not an error response.
pub async fn join_commit(
    State(state): State<SharedState>,
    Query(params): Query<JoinCommitParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let sha = params.sha.trim();
    if !crate::join::ladder::is_plausible_sha(sha) {
        return Err(ApiError::bad_request(format!(
            "sha must be 4-64 hex characters (got {:?})",
            params.sha
        )));
    }
    let attribution =
        crate::join::ladder::resolve_commit(repo, repo_id, sha, &state.store, &state.kb_client)
            .await;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(attribution)))
}

// --- POST /api/backfill (W3.6 — the join ladder's precompute) ------------

impl From<crate::join::backfill::BackfillError> for ApiError {
    fn from(e: crate::join::backfill::BackfillError) -> Self {
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    }
}

#[derive(Debug, Deserialize)]
pub struct BackfillParams {
    pub repo: String,
}

/// `POST /api/backfill?repo=` (W3.6) — the join ladder's PRECOMPUTE:
/// proactively resolves every commit `[backfill] depth` allows
/// (`state.backfill_depth`, resolved once at boot) through
/// `join::backfill::backfill_repo`, warming the `commit_sessions` cache
/// ahead of a live `why`/`story`/`join` query. Ordinary `auth_bearer`-gated
/// (like `join/commit` — loopback bypasses per invariant #4), NOT the
/// transcripts lane's stricter loopback-only guard: this route's response
/// carries nothing more sensitive than `join/commit`'s own `Attribution`
/// already does, just many of them plus counts. A `git log` failure (e.g. a
/// corrupt repo) is the only HARD failure path (500); an unreachable/
/// disabled federated kb daemon degrades the STATS (`degraded: true`) and
/// never fails the request — the trailer arm still resolves purely locally
/// (see `join::backfill`'s module doc).
pub async fn backfill_route(
    State(state): State<SharedState>,
    Query(params): Query<BackfillParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let stats = crate::join::backfill::backfill_repo(
        repo,
        repo_id,
        state.backfill_depth,
        &state.store,
        &state.kb_client,
    )
    .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(stats)))
}

// --- GET /api/session-diff (W3.5 — the session diff) ----------------------

#[derive(Debug, Deserialize)]
pub struct SessionDiffParams {
    pub session: String,
    pub repo: Option<String>,
}

impl From<crate::sessiondiff::SessionDiffError> for ApiError {
    fn from(e: crate::sessiondiff::SessionDiffError) -> Self {
        use crate::sessiondiff::SessionDiffError::*;
        match e {
            UnknownSession(_) => ApiError::not_found(e.to_string()),
            UnknownRepo(_) => ApiError::bad_request(e.to_string()),
            Store(_) => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        }
    }
}

/// `GET /api/session-diff?session=<sid>[&repo=]` (W3.5) — the session
/// diff's HTTP surface: `sessiondiff::session_diff`'s narrative change-set
/// for one session, optionally narrowed to one configured repo. Mounted on
/// the LOOPBACK-ONLY transcripts sub-router (`router.rs`), NOT the ordinary
/// `auth_bearer` nest — see `sessiondiff`'s module doc for why (its payload
/// carries raw transcript prompt text). `404` for a session this daemon's
/// own transcript index has never seen; `400` for an unrecognized `repo`;
/// an unreachable/disabled kb daemon never errors the request — the
/// commits half just degrades (`commits_status`, see the module doc).
pub async fn session_diff_route(
    State(state): State<SharedState>,
    Query(params): Query<SessionDiffParams>,
) -> Result<impl IntoResponse, ApiError> {
    let diff = crate::sessiondiff::session_diff(
        &params.session,
        params.repo.as_deref(),
        &state.repos,
        &state.store,
        &state.transcripts_root,
        &state.kb_client,
    )
    .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(diff)))
}

// --- GET/POST /api/annotations + PATCH/DELETE /api/annotations/{id}
//     + GET /api/annotations/open (W4.6; Phase D-server adds anchor kinds,
//     threads, intents, and the `open` listing) -----------------------------
//
// See `crate::annotations`'s module doc for the full per-kind anchor
// construction/resolution contract (kb-core's `review::Anchor` reused
// wholesale — every kind is still built from `Anchor::Selection`). Every
// route here re-reads the CURRENT working-tree file (or, for a `diff`
// annotation, simply trusts its own immutable recorded position — see
// below) to re-resolve each annotation's anchor on the way out —
// annotations are never served stale by omission, only by an honest
// `stale: true` when the anchored content is genuinely gone.

/// Wire shape for one annotation — the store row plus its live-resolved
/// position. `anchor` travels as the raw `kb_core::review::Anchor` JSON
/// (the SAME shape kb's own `.review/*.json` comments serialize) — `None`
/// only for a REPLY, which has no anchor of its own. `line_end` is `Some`
/// only for a `range` annotation; `sha` only for a `diff` annotation.
#[derive(Debug, Serialize)]
pub struct AnnotationView {
    pub id: String,
    pub repo: String,
    pub path: String,
    pub anchor: Option<Anchor>,
    pub anchor_kind: String,
    pub intent: String,
    pub parent_id: Option<String>,
    pub body: String,
    pub author: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub resolved: bool,
    pub line: u32,
    pub stale: bool,
    pub line_end: Option<u32>,
    pub sha: Option<String>,
    /// `symbol`-kind only: a display label for the attached symbol
    /// (`container::name` when a container exists, else just the name) from
    /// the stored [`SymbolDescriptor`] — D3's CLI review flagged that
    /// without this no client can SAY which symbol an annotation follows;
    /// the resolved `line` alone reads like a plain line anchor.
    pub symbol: Option<String>,
    /// V4.C1 — present only on a review-scoped row. `skip_serializing_if`
    /// keeps the key off the wire for every pre-V0023 / plain annotation
    /// so existing clients stay byte-identical.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ps_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side: Option<String>,
    /// V70-A10 — present only on a workspace-scoped row (same
    /// `skip_serializing_if` posture as `review_id` above, for the same
    /// reason: every pre-V0028 / non-workspace annotation stays
    /// byte-identical on the wire).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_id: Option<String>,
    /// V74-L3b — `trails.id` when this annotation is a DISSENT note on an
    /// agent-authored trail. `skip_serializing_if` for `set_id`'s reason:
    /// every pre-V0039 / non-trail annotation stays byte-identical on the
    /// wire.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trail_id: Option<String>,
}

fn corrupt_anchor_error(id: &str, field: &str, e: impl std::fmt::Display) -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("corrupt annotation {field} {id:?}: {e}"),
    )
}

fn missing_anchor_field_error(id: &str, field: &str, kind: &str) -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("{kind} annotation {id:?} has no {field}"),
    )
}

/// Build an [`AnnotationView`] from a store row, dispatching on
/// `row.anchor_kind` (or, for a reply, on `row.parent_id`) — see
/// `crate::annotations`'s module doc for each kind's resolution contract.
/// `content` is the CURRENT working-tree text of `row.path` (ignored
/// entirely for a `diff` annotation, which never re-resolves — callers may
/// pass an empty string in that case rather than paying for a read, see
/// `create_annotation`'s `view_content` local). A malformed stored anchor is
/// a daemon-side data-integrity fault (500), never a caller mistake —
/// nothing on the write path can produce one.
/// 2026-08-31 incident (store.rs module doc): takes `store: &Store` (not
/// `&SharedState`) precisely so every async call site can run this inside
/// its own `run_blocking` closure alongside its other store calls.
/// `pub(crate)` (V71-X1) so `agentview::pack` can resolve the SAME view
/// shape for its own open-annotations section rather than growing a
/// second resolver.
pub(crate) fn annotation_view(
    store: &Store,
    row: store::AnnotationRow,
    repo_name: &str,
    content: &str,
) -> Result<AnnotationView, ApiError> {
    // A REPLY carries no anchor of its own — `line`/`stale` mirror its
    // parent's OWN resolved view. Parents can never themselves be replies
    // (enforced in `create_annotation`), so this recursion is exactly one
    // level deep.
    if let Some(parent_id) = row.parent_id.clone() {
        let parent = store.get_annotation(&parent_id)?.ok_or_else(|| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "reply {:?} references a vanished parent {parent_id:?}",
                    row.id
                ),
            )
        })?;
        let parent_view = annotation_view(store, parent, repo_name, content)?;
        return Ok(AnnotationView {
            id: row.id,
            repo: repo_name.to_string(),
            path: row.path,
            anchor: None,
            anchor_kind: row.anchor_kind,
            intent: row.intent,
            parent_id: Some(parent_id),
            body: row.body,
            author: row.author,
            created_at: row.created_at,
            updated_at: row.updated_at,
            resolved: row.resolved,
            line: parent_view.line,
            stale: parent_view.stale,
            line_end: None,
            sha: None,
            symbol: parent_view.symbol,
            review_id: row.review_id,
            ps_number: row.ps_number,
            side: row.side,
            set_id: row.set_id,
            trail_id: row.trail_id,
        });
    }

    // PRR-R3 — a review-scoped, PATH-LESS "general question" has no
    // `Anchor` at all (its stored `anchor` column is an empty string, NOT
    // JSON — see `routes::assemble_top_level_annotation`'s dedicated
    // branch and `annotations::ANCHOR_KIND_REVIEW`'s own doc), so it must
    // never reach the generic `serde_json::from_str::<Anchor>` parse below
    // (that would always fail with a corrupt-anchor 500). Always resolved,
    // `line: 0` is an honest sentinel (never a real line number) — a
    // client keys off `anchor_kind == "review"` rather than trusting this
    // field for this one kind, the SAME convention every other
    // kind-specific field on this view already follows (e.g. `symbol` is
    // meaningless outside `anchor_kind: "symbol"`).
    if row.anchor_kind == annotations::ANCHOR_KIND_REVIEW {
        return Ok(AnnotationView {
            id: row.id,
            repo: repo_name.to_string(),
            path: row.path,
            anchor: None,
            anchor_kind: row.anchor_kind,
            intent: row.intent,
            parent_id: row.parent_id,
            body: row.body,
            author: row.author,
            created_at: row.created_at,
            updated_at: row.updated_at,
            resolved: row.resolved,
            line: 0,
            stale: false,
            line_end: None,
            sha: None,
            symbol: None,
            review_id: row.review_id,
            ps_number: row.ps_number,
            side: row.side,
            set_id: row.set_id,
            trail_id: row.trail_id,
        });
    }

    // V70-A10 — a WORKSPACE-scoped, PATH-LESS general note
    // (`annotations::ANCHOR_KIND_SET`'s own doc) — the exact same
    // path-less shape as `ANCHOR_KIND_REVIEW` above, one level down. Same
    // "line: 0 is an honest sentinel" posture: a client keys off
    // `anchor_kind == "set"`, never this field, for this one kind.
    if row.anchor_kind == annotations::ANCHOR_KIND_SET {
        return Ok(AnnotationView {
            id: row.id,
            repo: repo_name.to_string(),
            path: row.path,
            anchor: None,
            anchor_kind: row.anchor_kind,
            intent: row.intent,
            parent_id: row.parent_id,
            body: row.body,
            author: row.author,
            created_at: row.created_at,
            updated_at: row.updated_at,
            resolved: row.resolved,
            line: 0,
            stale: false,
            line_end: None,
            sha: None,
            symbol: None,
            review_id: row.review_id,
            ps_number: row.ps_number,
            side: row.side,
            set_id: row.set_id,
            trail_id: row.trail_id,
        });
    }

    let anchor: Anchor = serde_json::from_str(
        row.anchor
            .as_deref()
            .ok_or_else(|| missing_anchor_field_error(&row.id, "anchor", "top-level"))?,
    )
    .map_err(|e| corrupt_anchor_error(&row.id, "anchor", e))?;

    let (line, stale, line_end, sha, symbol) = match row.anchor_kind.as_str() {
        annotations::ANCHOR_KIND_RANGE => {
            let end_json = row
                .anchor2
                .as_deref()
                .ok_or_else(|| missing_anchor_field_error(&row.id, "anchor2", "range"))?;
            let end_anchor: Anchor = serde_json::from_str(end_json)
                .map_err(|e| corrupt_anchor_error(&row.id, "anchor2", e))?;
            let start_resolved = annotations::resolve(content, &anchor);
            let end_resolved = annotations::resolve(content, &end_anchor);
            let (lo, hi, stale) = annotations::normalize_range(start_resolved, end_resolved);
            (lo, stale, Some(hi), None, None)
        }
        annotations::ANCHOR_KIND_SYMBOL => {
            let descriptor: SymbolDescriptor = serde_json::from_str(
                row.anchor2
                    .as_deref()
                    .ok_or_else(|| missing_anchor_field_error(&row.id, "anchor2", "symbol"))?,
            )
            .map_err(|e| corrupt_anchor_error(&row.id, "anchor2", e))?;
            // A pure STORE lookup, never derived on the spot here — same
            // rule `GET /api/file`'s doc spells out for `symbols`/
            // `highlights` (deriving inside a request handler would either
            // skip persisting or corrupt the `files` working-tree
            // invariant).
            let blob_hash = ingest::git_blob_hash(content.as_bytes());
            let current_symbols = match lang::detect(&row.path, Some(content.as_bytes())) {
                Some(li) => store.symbols_for_blob(&blob_hash, li.symbol_salt)?,
                None => Vec::new(),
            };
            let resolved =
                annotations::resolve_symbol(&current_symbols, &descriptor, &anchor, content);
            let label = match &descriptor.container {
                Some(c) => format!("{c}::{}", descriptor.name),
                None => descriptor.name.clone(),
            };
            (resolved.line, resolved.stale, None, None, Some(label))
        }
        annotations::ANCHOR_KIND_DIFF => {
            let anchor2: DiffAnchor2 = serde_json::from_str(
                row.anchor2
                    .as_deref()
                    .ok_or_else(|| missing_anchor_field_error(&row.id, "anchor2", "diff"))?,
            )
            .map_err(|e| corrupt_anchor_error(&row.id, "anchor2", e))?;
            // NEVER re-resolved — see `crate::annotations::diff_line`'s doc.
            (
                annotations::diff_line(&anchor),
                false,
                None,
                Some(anchor2.sha_full),
                None,
            )
        }
        // "line" (the v1 default) plus a defensive fallback for any
        // unrecognized stored kind — an honest degrade (ordinary fuzzy
        // resolve) rather than a hard failure, matching this crate's
        // general forward-compatibility posture.
        _ => {
            let resolved = annotations::resolve(content, &anchor);
            (resolved.line, resolved.stale, None, None, None)
        }
    };

    Ok(AnnotationView {
        id: row.id,
        repo: repo_name.to_string(),
        path: row.path,
        anchor: Some(anchor),
        anchor_kind: row.anchor_kind,
        intent: row.intent,
        parent_id: row.parent_id,
        body: row.body,
        author: row.author,
        created_at: row.created_at,
        updated_at: row.updated_at,
        resolved: row.resolved,
        line,
        stale,
        line_end,
        sha,
        symbol,
        review_id: row.review_id,
        ps_number: row.ps_number,
        side: row.side,
        set_id: row.set_id,
        trail_id: row.trail_id,
    })
}

#[derive(Debug, Deserialize)]
pub struct AnnotationsListParams {
    /// Required unless `set_id` is given.
    #[serde(default)]
    pub repo: Option<String>,
    /// Required unless `set_id` is given.
    #[serde(default)]
    pub path: Option<String>,
    /// V70-A10 — list a workspace's notes instead of one file's
    /// annotations. When present, `repo`/`path` are IGNORED rather than
    /// erroring (a caller that always sends `repo` alongside `set_id` for
    /// its own bookkeeping is never punished) — see [`list_annotations`]'s
    /// doc.
    #[serde(default)]
    pub set_id: Option<String>,
}

/// `GET /api/annotations?repo=&path=` (W4.6) — every annotation for one
/// (repo, path) — parents AND their replies alike — each carrying its
/// live-resolved `line`/`stale`. A file that no longer exists (or isn't
/// valid UTF-8) degrades to resolving every anchor against empty content
/// (every `Selection` anchor goes stale, per kb-core's own resolver) rather
/// than failing the whole list — the annotations themselves are still real
/// rows worth showing.
///
/// V70-A10 — `GET /api/annotations?set_id=` (a second, mutually exclusive
/// query shape on the SAME route, same "one route, a param picks the
/// branch" convention `reading_sets::list_sets`' own `kind=`/`group=`
/// params use): every annotation scoped to that workspace — general
/// path-less notes AND code-anchored comments alike — each still
/// live-resolved against ITS OWN path's current content (a general note's
/// `path` is `""`, which resolves against empty content and is never
/// actually read, same as the plain `repo`/`path` branch's own missing-file
/// degrade). `404` unknown `set_id`. `repo`/`path` are IGNORED when
/// `set_id` is present (never a conflict error) — see
/// [`AnnotationsListParams`]'s doc.
pub async fn list_annotations(
    State(state): State<SharedState>,
    Query(params): Query<AnnotationsListParams>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(set_id) = params.set_id.clone() {
        let state_bg = state.clone();
        let set_id_bg = set_id.clone();
        // 2026-08-31 incident (store.rs module doc): the set lookup + the
        // list + per-row resolve all run as one closure; `find_repo_by_id`
        // only touches `state.repos` (in-memory), so a cloned `state`
        // handle rides along for that lookup, same as `reading_sets::
        // get_set`'s own pattern.
        let views = state
            .store
            .run_blocking(move |store| -> Result<_, ApiError> {
                let set = store
                    .get_reading_set(&set_id_bg)?
                    .ok_or_else(|| ApiError::not_found(format!("set {set_id_bg:?}")))?;
                let repo = find_repo_by_id(&state_bg, set.repo_id)?;
                let repo_root = repo.path.clone();
                let repo_label = repo.name.clone();
                let rows = store.list_annotations_by_set(&set_id_bg)?;
                let mut views = Vec::with_capacity(rows.len());
                for row in rows {
                    // A general (path-less) workspace note never reads a
                    // file (its `path` is `""`); a code-anchored one
                    // resolves against ITS OWN path's current content —
                    // notes on DIFFERENT files can coexist in one set_id
                    // listing, unlike the plain repo/path branch below.
                    let content = if row.path.is_empty() {
                        String::new()
                    } else {
                        std::fs::read_to_string(repo_root.join(&row.path)).unwrap_or_default()
                    };
                    views.push(annotation_view(store, row, &repo_label, &content)?);
                }
                Ok(views)
            })
            .await?;
        return Ok((
            [(header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({
                "set_id": set_id,
                "annotations": views,
            })),
        ));
    }

    let repo_name = params
        .repo
        .clone()
        .ok_or_else(|| ApiError::bad_request("repo is required (or pass set_id)"))?;
    let path_param = params
        .path
        .clone()
        .ok_or_else(|| ApiError::bad_request("path is required (or pass set_id)"))?;
    let (repo, repo_id) = find_repo(&state, &repo_name)?;
    let path = safe_rel_path(&path_param)?.to_string();
    let repo_root = repo.path.clone();
    let repo_label = repo_name.clone();
    let path_bg = path.clone();
    // 2026-08-31 incident (store.rs module doc): the list + per-row
    // resolve (a store call apiece for symbol-kind/reply rows) run as one
    // closure; no async work happens anywhere in this handler.
    let views = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let rows = store.list_annotations(repo_id, &path_bg)?;
            let content = std::fs::read_to_string(repo_root.join(&path_bg)).unwrap_or_default();
            let mut views = Vec::with_capacity(rows.len());
            for row in rows {
                views.push(annotation_view(store, row, &repo_label, &content)?);
            }
            Ok(views)
        })
        .await?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "repo": repo_name,
            "path": path,
            "annotations": views,
        })),
    ))
}

/// Read `path`'s CURRENT working-tree text, mapping a missing/unreadable
/// file the same way every annotation-creation kind needs (404 on
/// not-found, 500 otherwise) — shared by the `line`/`range`/`symbol`
/// creation branches (`diff` reads a historical blob instead, via
/// `read_repo_file`, and so has no use for this).
fn read_working_tree_text(repo: &RepoEntry, path: &str) -> Result<String, ApiError> {
    // V70-A2 — same three-check ladder as `read_repo_file` above; this is
    // the OTHER directly-addressable working-tree read in this module.
    let path = safe_rel_path(path)?;
    crate::security::secrets::builtin_policy().check(path)?;
    let abs = crate::security::paths::contained_abs_path(&repo.path, path)?;
    std::fs::read_to_string(abs).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ApiError::not_found(format!("{path}: not found in the working tree"))
        } else {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("read {path}: {e}"),
            )
        }
    })
}

/// The 1-based `line`'s own text within `content`, or `400` when it's out
/// of the file's current range — shared by every annotation-creation kind.
fn line_at<'a>(content: &'a str, line: u32, path: &str) -> Result<&'a str, ApiError> {
    content
        .lines()
        .nth((line - 1) as usize)
        .ok_or_else(|| ApiError::bad_request(format!("{path} has no line {line}")))
}

/// `serde_json::to_string`, mapped to the SAME 500 shape every annotation
/// field-serialize call used inline before this helper existed.
fn encode_json<T: Serialize>(value: &T, what: &str) -> Result<String, ApiError> {
    serde_json::to_string(value).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("serialize annotation {what}: {e}"),
        )
    })
}

// `Clone` added 2026-08-31 incident fix (store.rs module doc): async call
// sites need an owned copy to move into a `'static` `run_blocking` closure.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateAnnotationBody {
    pub repo: String,
    pub path: String,
    /// 1-based line number — required for every kind EXCEPT a reply
    /// (`parent_id` set), where it's ignored (a reply has no anchor). For
    /// `line`/`range`/`symbol` this is a CURRENT working-tree line number;
    /// for `diff` it's a line number in `sha`'s own version of the file.
    #[serde(default)]
    pub line: Option<u32>,
    pub body: String,
    #[serde(default)]
    pub author: Option<String>,
    /// "line" (default) | "range" | "symbol" | "diff" — validated against
    /// `annotations::is_valid_anchor_kind`. Ignored for a reply.
    #[serde(default)]
    pub anchor_kind: Option<String>,
    /// `range` only — the inclusive end line (working-tree, current).
    #[serde(default)]
    pub line_end: Option<u32>,
    /// `diff` only — the commit this annotation pins its position to (any
    /// revspec `join::ladder::is_plausible_sha` accepts as a shape; resolved
    /// to the full sha via `history::commit::commit_meta`, the SAME
    /// sha-resolution gate `GET /api/commit` uses).
    #[serde(default)]
    pub sha: Option<String>,
    /// "note" (default) | "question" | "todo" | "flag-for-agent" |
    /// "tour-stop" | "claim" (V72-J2) — validated against `annotations::is_valid_intent`.
    #[serde(default)]
    pub intent: Option<String>,
    /// `Some` makes this a REPLY to an existing (non-reply) annotation —
    /// one level of nesting only; see this fn's doc.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// V4.C1 — when set, this is a review-scoped comment. The review
    /// must exist, belong to `repo`, and `ps` (default: latest) must
    /// exist. A reply inherits these from its parent and 400s on a
    /// conflicting value.
    #[serde(default)]
    pub review_id: Option<i64>,
    /// Patchset to create against. Default = the review's latest.
    #[serde(default)]
    pub ps: Option<i64>,
    /// `"old"` | `"new"` (default `"new"`). Which side of the patchset
    /// the anchor is built from.
    #[serde(default)]
    pub side: Option<String>,
    /// V70-A10 — when set, this annotation is scoped to a workspace
    /// (`reading_sets.id`, `kind = "workspace"`). Independent of
    /// `review_id` above (a note may be workspace-scoped, review-scoped,
    /// both, or neither) — validated to exist and belong to `repo`
    /// (`resolve_set_scope`). `anchor_kind: "set"` REQUIRES this field (a
    /// general path-less workspace note); every other anchor kind treats
    /// it as an optional extra tag (a code-anchored note filed under a
    /// workspace). A reply inherits this from its parent and 400s on a
    /// conflicting value — same `inherit_scope_field` ladder `review_id`
    /// uses.
    #[serde(default)]
    pub set_id: Option<String>,
    /// V74-L3b — when set, this annotation is a DISSENT note on an
    /// agent-AUTHORED `kbc-trail/1` trail (`trails.id`). Independent of
    /// `review_id`/`set_id`; validated to exist and belong to `repo`
    /// (`resolve_trail_scope`). A reply inherits it from its parent and
    /// 400s on a conflicting value — the SAME `inherit_scope_field`
    /// ladder, third instance. Notes REUSE this store rather than growing
    /// a second comments table (D10's node-thread ruling, one layer over).
    #[serde(default)]
    pub trail_id: Option<String>,
}

/// V4.C1 — inherit review scope from `parent`, 400 if the body tries to
/// set a conflicting `review_id`/`ps`/`side`. A body that omits a field
/// inherits; a body that repeats the parent's value is accepted.
fn inherit_scope_field<T: PartialEq>(
    parent: Option<T>,
    requested: Option<T>,
    unset_msg: &'static str,
    conflict_msg: &'static str,
) -> Result<Option<T>, ApiError> {
    match requested {
        None => Ok(parent),
        Some(r) => match parent {
            Some(p) if p == r => Ok(Some(p)),
            Some(_) => Err(ApiError::bad_request(conflict_msg)),
            None => Err(ApiError::bad_request(unset_msg)),
        },
    }
}

/// A reply's inherited (or parent-matching) review scope — all `None`
/// for a reply under a plain, non-review-scoped parent.
struct InheritedReviewScope {
    review_id: Option<i64>,
    ps_number: Option<i64>,
    side: Option<String>,
}

fn inherit_reply_review_scope(
    parent: &store::AnnotationRow,
    payload: &CreateAnnotationBody,
) -> Result<InheritedReviewScope, ApiError> {
    let review_id = inherit_scope_field(
        parent.review_id,
        payload.review_id,
        "a reply cannot set review_id on a parent that is not review-scoped",
        "a reply's review_id must match its parent's",
    )?;
    let ps_number = inherit_scope_field(
        parent.ps_number,
        payload.ps,
        "a reply cannot set ps on a parent that is not review-scoped",
        "a reply's ps must match its parent's",
    )?;
    let side = inherit_scope_field(
        parent.side.clone(),
        payload.side.clone(),
        "a reply cannot set side on a parent that is not review-scoped",
        "a reply's side must match its parent's",
    )?;
    Ok(InheritedReviewScope {
        review_id,
        ps_number,
        side,
    })
}

/// V4.C1 — validate an optional `review_id` on a top-level create.
/// `None` when the body has no `review_id` (plain create, unchanged).
struct ReviewCreateScope {
    review_id: i64,
    ps: store::ReviewPatchsetRow,
    side: String,
}

/// 2026-08-31 incident (store.rs module doc): takes `store: &Store` plus
/// the specific primitives it needs (not `&CreateAnnotationBody`, which
/// isn't `'static`-cloneable-for-free) so every async call site can run
/// this inside its own `run_blocking` closure.
fn resolve_review_create_scope(
    store: &Store,
    review_id: Option<i64>,
    repo_name: &str,
    ps: Option<i64>,
    side: Option<&str>,
) -> Result<Option<ReviewCreateScope>, ApiError> {
    let Some(review_id) = review_id else {
        return Ok(None);
    };
    let review = store
        .get_review(review_id)?
        .ok_or_else(|| ApiError::bad_request(format!("no such review: {review_id}")))?;
    if review.repo != repo_name {
        return Err(ApiError::bad_request(format!(
            "review {review_id} belongs to repo {:?}, not {:?}",
            review.repo, repo_name
        )));
    }
    let ps = match ps {
        None => store
            .latest_patchset(review_id)?
            .ok_or_else(|| ApiError::bad_request(format!("review {review_id} has no patchsets")))?,
        Some(n) => store.get_patchset(review_id, n)?.ok_or_else(|| {
            ApiError::bad_request(format!("no patchset {n} on review {review_id}"))
        })?,
    };
    let side = side.unwrap_or("new");
    if side != "old" && side != "new" {
        return Err(ApiError::bad_request(format!(
            "invalid side: {side:?} (expected \"old\" or \"new\")"
        )));
    }
    Ok(Some(ReviewCreateScope {
        review_id,
        ps,
        side: side.to_string(),
    }))
}

/// Read the pinned blob for `side` of `ps` — same ODB path the `diff`
/// create branch uses (`read_repo_file` at a full sha).
fn read_pinned_review_file(
    repo: &RepoEntry,
    path: &str,
    ps: &store::ReviewPatchsetRow,
    side: &str,
) -> Result<String, ApiError> {
    let sha = if side == "old" {
        ps.base_sha.as_str()
    } else {
        ps.tip_sha.as_str()
    };
    let read = read_repo_file(repo, path, Some(sha))?;
    String::from_utf8(read.bytes)
        .map_err(|_| ApiError::bad_request(format!("{path} at {sha}: not valid UTF-8")))
}

pub(crate) fn emit_annotation_changed(
    bus: &kb_core::events::EventBus,
    repo: &str,
    path: &str,
    review_id: Option<i64>,
) {
    let mut body = serde_json::json!({ "repo": repo, "path": path });
    if let Some(id) = review_id {
        body["review_id"] = serde_json::json!(id);
    }
    bus.emit("annotation.changed", body);
}

/// Outcome of assembling a new annotation without writing it — V4.C2
/// batch reuses this so create and `add_comment` stay byte-identical.
struct BuiltAnnotation {
    row: store::AnnotationRow,
    view_content: String,
}

fn assemble_reply_annotation(
    store: &Store,
    repo_id: i64,
    path: &str,
    payload: &CreateAnnotationBody,
    now: i64,
) -> Result<store::AnnotationRow, ApiError> {
    let parent_id = payload
        .parent_id
        .clone()
        .ok_or_else(|| ApiError::bad_request("parent_id is required for a reply"))?;
    let parent = store
        .get_annotation(&parent_id)?
        .ok_or_else(|| ApiError::not_found(format!("annotation {parent_id:?}")))?;
    if parent.parent_id.is_some() {
        return Err(ApiError::bad_request(
            "cannot reply to a reply (one level of nesting only)",
        ));
    }
    if parent.repo_id != repo_id || parent.path != path {
        return Err(ApiError::bad_request(
            "a reply's repo/path must match its parent's",
        ));
    }
    let InheritedReviewScope {
        review_id,
        ps_number,
        side,
    } = inherit_reply_review_scope(&parent, payload)?;
    // V70-A10 — same inherit-or-match ladder as the review scope above,
    // applied to `set_id`: a reply under a workspace-scoped parent stays
    // scoped to that SAME workspace (never a different one, never
    // unscoped-when-the-parent-wasn't).
    let set_id = inherit_scope_field(
        parent.set_id.clone(),
        payload.set_id.clone(),
        "a reply cannot set set_id on a parent that is not workspace-scoped",
        "a reply's set_id must match its parent's",
    )?;
    // V74-L3b — the SAME ladder again, for a trail dissent note: a reply
    // under a trail-scoped parent stays on that SAME trail, which is what
    // keeps `Store::trail_notes` a single `WHERE trail_id = ?` rather than
    // a parent/reply two-step.
    let trail_id = inherit_scope_field(
        parent.trail_id.clone(),
        payload.trail_id.clone(),
        "a reply cannot set trail_id on a parent that is not trail-scoped",
        "a reply's trail_id must match its parent's",
    )?;
    let intent = payload
        .intent
        .as_deref()
        .unwrap_or(annotations::INTENT_NOTE)
        .to_string();
    if !annotations::is_valid_intent(&intent) {
        return Err(ApiError::bad_request(format!("invalid intent: {intent:?}")));
    }
    let author = payload.author.clone().unwrap_or_else(|| "you".to_string());
    Ok(store::AnnotationRow {
        id: annotations::new_annotation_id(),
        repo_id,
        path: path.to_string(),
        anchor: None,
        anchor_kind: annotations::ANCHOR_KIND_LINE.to_string(),
        anchor2: None,
        parent_id: Some(parent_id),
        intent,
        body: payload.body.clone(),
        author,
        created_at: now,
        updated_at: now,
        resolved: false,
        review_id,
        ps_number,
        side,
        set_id,
        trail_id,
    })
}

/// Async wrapper around [`resolve_review_create_scope`] — the ONE place
/// that hops to the blocking pool for it (2026-08-31 incident, store.rs
/// module doc); both call sites in [`assemble_top_level_annotation`] just
/// `.await` this instead of duplicating the `run_blocking` wrap.
async fn resolve_review_create_scope_async(
    state: &SharedState,
    payload: &CreateAnnotationBody,
) -> Result<Option<ReviewCreateScope>, ApiError> {
    let review_id = payload.review_id;
    let repo_name = payload.repo.clone();
    let ps = payload.ps;
    let side = payload.side.clone();
    state
        .store
        .run_blocking(move |store| {
            resolve_review_create_scope(store, review_id, &repo_name, ps, side.as_deref())
        })
        .await
}

/// V70-A10 — validate an optional `set_id` on a top-level create: the
/// referenced reading set must exist and belong to `repo_id` (`400`
/// otherwise — the SAME "no such X" / "belongs to a different Y" shape
/// [`resolve_review_create_scope`] uses for `review_id`). `None` when
/// `set_id` is `None` (unscoped create, unchanged) — returns the
/// (unmodified) id back out so the async wrapper below can hand it
/// straight to [`assemble_top_level_annotation`] without a second lookup.
fn resolve_set_scope(
    store: &Store,
    set_id: Option<&str>,
    repo_id: i64,
) -> Result<Option<String>, ApiError> {
    let Some(set_id) = set_id else {
        return Ok(None);
    };
    let set = store
        .get_reading_set(set_id)?
        .ok_or_else(|| ApiError::bad_request(format!("no such set: {set_id:?}")))?;
    if set.repo_id != repo_id {
        return Err(ApiError::bad_request(format!(
            "set {set_id:?} belongs to a different repo"
        )));
    }
    Ok(Some(set_id.to_string()))
}

/// Async wrapper around [`resolve_set_scope`] — same "hop to the blocking
/// pool once, every call site just `.await`s it" shape as
/// [`resolve_review_create_scope_async`] above.
async fn resolve_set_scope_async(
    state: &SharedState,
    set_id: Option<String>,
    repo_id: i64,
) -> Result<Option<String>, ApiError> {
    state
        .store
        .run_blocking(move |store| resolve_set_scope(store, set_id.as_deref(), repo_id))
        .await
}

/// V74-L3b — the SAME validation, third instance: an optional `trail_id`
/// on a top-level create must name a trail that exists and belongs to
/// `repo_id`. `None` when absent (every ordinary annotation, unchanged).
fn resolve_trail_scope(
    store: &Store,
    trail_id: Option<&str>,
    repo_id: i64,
) -> Result<Option<String>, ApiError> {
    let Some(trail_id) = trail_id else {
        return Ok(None);
    };
    store
        .get_trail(repo_id, trail_id)?
        .ok_or_else(|| ApiError::bad_request(format!("no such trail: {trail_id:?}")))?;
    Ok(Some(trail_id.to_string()))
}

async fn resolve_trail_scope_async(
    state: &SharedState,
    trail_id: Option<String>,
    repo_id: i64,
) -> Result<Option<String>, ApiError> {
    state
        .store
        .run_blocking(move |store| resolve_trail_scope(store, trail_id.as_deref(), repo_id))
        .await
}

async fn assemble_top_level_annotation(
    state: &SharedState,
    repo: &RepoEntry,
    repo_id: i64,
    path: &str,
    payload: &CreateAnnotationBody,
    now: i64,
) -> Result<BuiltAnnotation, ApiError> {
    let intent = payload
        .intent
        .as_deref()
        .unwrap_or(annotations::INTENT_NOTE)
        .to_string();
    if !annotations::is_valid_intent(&intent) {
        return Err(ApiError::bad_request(format!("invalid intent: {intent:?}")));
    }
    let author = payload.author.clone().unwrap_or_else(|| "you".to_string());
    let anchor_kind = payload
        .anchor_kind
        .as_deref()
        .unwrap_or(annotations::ANCHOR_KIND_LINE)
        .to_string();
    if !annotations::is_valid_anchor_kind(&anchor_kind) {
        return Err(ApiError::bad_request(format!(
            "invalid anchor_kind: {anchor_kind:?}"
        )));
    }
    // V70-A10 — resolved ONCE, reused by every branch below (independent
    // of `anchor_kind`/`review_id`: a note may be workspace-scoped,
    // review-scoped, both, or neither). Each early-return branch MOVES
    // this rather than cloning — legal because Rust's borrow checker sees
    // the later uses are only reachable on the path where an earlier move
    // didn't happen (every branch that consumes it also `return`s).
    let set_id = resolve_set_scope_async(state, payload.set_id.clone(), repo_id).await?;
    // V74-L3b — resolved the same way, and independently: a note may be
    // workspace-scoped, review-scoped, trail-scoped, several of those, or
    // none.
    let trail_id = resolve_trail_scope_async(state, payload.trail_id.clone(), repo_id).await?;

    // PRR-R3 (design arbitration #6) — a review-scoped, PATH-LESS "general
    // question": no working-tree file to anchor against at all, so this
    // kind takes its own branch entirely separate from the line/range/
    // symbol/diff ladder below (which all assume a real path + a real
    // `line`). `path` validation is loosened ONLY for this one kind — every
    // other anchor_kind still requires a non-empty, resolvable path via the
    // ladder below; `safe_rel_path` already accepts `""`, so no change was
    // needed there, only here (this kind is the only caller that WANTS an
    // empty path).
    if anchor_kind == annotations::ANCHOR_KIND_REVIEW {
        if !path.is_empty() {
            return Err(ApiError::bad_request(
                "anchor_kind \"review\" requires an empty path",
            ));
        }
        let scope = resolve_review_create_scope_async(state, payload)
            .await?
            .ok_or_else(|| ApiError::bad_request("anchor_kind \"review\" requires review_id"))?;
        return Ok(BuiltAnnotation {
            row: store::AnnotationRow {
                id: annotations::new_annotation_id(),
                repo_id,
                path: path.to_string(),
                anchor: Some(String::new()),
                anchor_kind,
                anchor2: None,
                parent_id: None,
                intent,
                body: payload.body.clone(),
                author,
                created_at: now,
                updated_at: now,
                resolved: false,
                review_id: Some(scope.review_id),
                ps_number: Some(scope.ps.ps_number),
                side: Some(scope.side),
                set_id,
                trail_id,
            },
            view_content: String::new(),
        });
    }

    // V70-A10 — a WORKSPACE-scoped, PATH-LESS general note
    // (`annotations::ANCHOR_KIND_SET`'s own doc): the exact same shape as
    // `ANCHOR_KIND_REVIEW` above, one level down (a workspace instead of a
    // review). `set_id` is REQUIRED here (unlike every other anchor kind,
    // where it's an optional extra tag) — there is nothing else this kind
    // could possibly be scoped to.
    if anchor_kind == annotations::ANCHOR_KIND_SET {
        if !path.is_empty() {
            return Err(ApiError::bad_request(
                "anchor_kind \"set\" requires an empty path",
            ));
        }
        let set_id =
            set_id.ok_or_else(|| ApiError::bad_request("anchor_kind \"set\" requires set_id"))?;
        return Ok(BuiltAnnotation {
            row: store::AnnotationRow {
                id: annotations::new_annotation_id(),
                repo_id,
                path: path.to_string(),
                anchor: Some(String::new()),
                anchor_kind,
                anchor2: None,
                parent_id: None,
                intent,
                body: payload.body.clone(),
                author,
                created_at: now,
                updated_at: now,
                resolved: false,
                review_id: None,
                ps_number: None,
                side: None,
                set_id: Some(set_id),
                trail_id,
            },
            view_content: String::new(),
        });
    }

    let line = payload
        .line
        .ok_or_else(|| ApiError::bad_request("line is required"))?;
    if line < 1 {
        return Err(ApiError::bad_request("line must be >= 1 (1-based)"));
    }

    let review_scope = resolve_review_create_scope_async(state, payload).await?;
    if review_scope.is_some()
        && anchor_kind != annotations::ANCHOR_KIND_LINE
        && anchor_kind != annotations::ANCHOR_KIND_RANGE
    {
        return Err(ApiError::bad_request(format!(
            "review-scoped annotations only support anchor_kind line|range (got {anchor_kind:?})"
        )));
    }
    // Pre-read the pinned blob when review-scoped so the line/range
    // arms below can share it. Plain creates leave this None and take
    // the existing working-tree read in each arm.
    let pinned_content = match &review_scope {
        Some(scope) => Some(read_pinned_review_file(repo, path, &scope.ps, &scope.side)?),
        None => None,
    };

    let (anchor_json, anchor2_json, view_content) = match anchor_kind.as_str() {
        annotations::ANCHOR_KIND_DIFF => {
            let sha = payload.sha.as_deref().ok_or_else(|| {
                ApiError::bad_request("sha is required for anchor_kind: \"diff\"")
            })?;
            if !crate::join::ladder::is_plausible_sha(sha) {
                return Err(ApiError::bad_request(format!(
                    "sha must be 4-64 hex characters (got {sha:?})"
                )));
            }
            let repo_root = repo.path.clone();
            let sha_owned = sha.to_string();
            let full_sha = tokio::task::spawn_blocking(move || {
                crate::history::commit::commit_meta(&repo_root, &sha_owned).map(|m| m.sha)
            })
            .await
            .map_err(|e| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("sha resolve task panicked: {e}"),
                )
            })??;
            let read = read_repo_file(repo, path, Some(&full_sha))?;
            let content = String::from_utf8(read.bytes).map_err(|_| {
                ApiError::bad_request(format!("{path} at {full_sha}: not valid UTF-8"))
            })?;
            let line_text = line_at(&content, line, path)?;
            let anchor = annotations::anchor_for_line(line, line_text);
            let anchor_json = encode_json(&anchor, "anchor")?;
            let anchor2_json = encode_json(&DiffAnchor2 { sha_full: full_sha }, "anchor2")?;
            (anchor_json, Some(anchor2_json), String::new())
        }
        annotations::ANCHOR_KIND_RANGE => {
            let line_end = payload.line_end.ok_or_else(|| {
                ApiError::bad_request("line_end is required for anchor_kind: \"range\"")
            })?;
            if line_end < 1 {
                return Err(ApiError::bad_request("line_end must be >= 1 (1-based)"));
            }
            let content = match pinned_content {
                Some(c) => c,
                None => read_working_tree_text(repo, path)?,
            };
            let start_text = line_at(&content, line, path)?;
            let start_anchor = annotations::anchor_for_line(line, start_text);
            let end_text = line_at(&content, line_end, path)?;
            let end_anchor = annotations::anchor_for_line(line_end, end_text);
            let anchor_json = encode_json(&start_anchor, "anchor")?;
            let anchor2_json = encode_json(&end_anchor, "anchor2")?;
            (anchor_json, Some(anchor2_json), content)
        }
        annotations::ANCHOR_KIND_SYMBOL => {
            let content = read_working_tree_text(repo, path)?;
            let line_text = line_at(&content, line, path)?;
            let blob_hash = ingest::git_blob_hash(content.as_bytes());
            // 2026-08-31 incident (store.rs module doc): single store call,
            // still wrapped so it can never park this async worker.
            let current_symbols = match lang::detect(path, Some(content.as_bytes())) {
                Some(li) => {
                    let blob_hash = blob_hash.clone();
                    state
                        .store
                        .run_blocking(move |store| {
                            store.symbols_for_blob(&blob_hash, li.symbol_salt)
                        })
                        .await?
                }
                None => Vec::new(),
            };
            let sym = annotations::enclosing_symbol(&current_symbols, line).ok_or_else(|| {
                ApiError::not_found(format!("no enclosing symbol at {path}:{line}"))
            })?;
            let descriptor = SymbolDescriptor {
                name: sym.name.clone(),
                container: sym.container.clone(),
                kind: sym.kind.clone(),
            };
            let backup_anchor = annotations::anchor_for_line(line, line_text);
            let anchor_json = encode_json(&backup_anchor, "anchor")?;
            let anchor2_json = encode_json(&descriptor, "anchor2")?;
            (anchor_json, Some(anchor2_json), content)
        }
        _ => {
            let content = match pinned_content {
                Some(c) => c,
                None => read_working_tree_text(repo, path)?,
            };
            let line_text = line_at(&content, line, path)?;
            let anchor = annotations::anchor_for_line(line, line_text);
            let anchor_json = encode_json(&anchor, "anchor")?;
            (anchor_json, None, content)
        }
    };

    let (review_id, ps_number, side) = match review_scope {
        Some(scope) => (
            Some(scope.review_id),
            Some(scope.ps.ps_number),
            Some(scope.side),
        ),
        None => (None, None, None),
    };
    Ok(BuiltAnnotation {
        row: store::AnnotationRow {
            id: annotations::new_annotation_id(),
            repo_id,
            path: path.to_string(),
            anchor: Some(anchor_json),
            anchor_kind,
            anchor2: anchor2_json,
            parent_id: None,
            intent,
            body: payload.body.clone(),
            author,
            created_at: now,
            updated_at: now,
            resolved: false,
            review_id,
            ps_number,
            side,
            set_id,
            trail_id,
        },
        view_content,
    })
}

/// `POST /api/annotations` (W4.6; Phase D-server adds `anchor_kind`,
/// `line_end`/`sha` per kind, `intent`, and `parent_id` replies;
/// V4.C1 adds optional `review_id`/`ps`/`side`; V70-A10 adds optional
/// `set_id`, and `anchor_kind: "set"`).
///
/// - A REPLY (`parent_id` set): the referenced annotation must exist
///   (`404`) and must NOT itself be a reply (`400` — one level of nesting
///   only, kb-comments style); the reply's `repo`/`path` must match the
///   parent's (`400` otherwise — "inherits repo/path"). Carries no anchor.
///   Review scope (`review_id`/`ps_number`/`side`) is inherited from the
///   parent; a body that sets a conflicting value 400s.
/// - Otherwise, a TOP-LEVEL annotation of `anchor_kind` (default `"line"`):
///   `400` for an invalid `anchor_kind`/`intent`, a missing/out-of-range
///   `line` (`range`/`symbol`/`diff` all still require `line`), a missing
///   `line_end` (`range`) or `sha` (`diff`); `404` if the working-tree file
///   doesn't exist (`line`/`range`/`symbol`) or the file doesn't exist AT
///   `sha` (`diff`), OR (symbol) no enclosing symbol is found at `line` —
///   honest, the client falls back to a `line` annotation.
/// - When `review_id` is set: the review must exist, belong to `repo`,
///   and `ps` (default latest) must exist (`400` otherwise);
///   `anchor_kind` is restricted to `line|range|review`; the anchor is
///   built from the pinned blob (`side=new` → `ps.tip_sha`, `side=old` →
///   `ps.base_sha`). Plain (no-`review_id`) creates are unchanged.
/// - PRR-R3 (design arbitration #6) — `anchor_kind: "review"` is a
///   review-scoped, PATH-LESS "general question": `path` MUST be `""`,
///   `review_id` is REQUIRED (`400` otherwise), `line`/`line_end`/`sha` are
///   ignored, and no working-tree file is ever read. Always resolved (no
///   anchor to go stale) — see `crate::review_comments::resolve_for_ps_
///   with_content`'s own dispatch for this kind.
///
/// Emits `annotation.changed {repo, path[, review_id]}` on `state.bus`
/// after the write, for every kind including a reply. `review_id` is
/// present only when the row is review-scoped.
pub async fn create_annotation(
    State(state): State<SharedState>,
    Json(payload): Json<CreateAnnotationBody>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &payload.repo)?;
    let path = safe_rel_path(&payload.path)?.to_string();
    let now = chrono::Utc::now().timestamp();

    if payload.parent_id.is_some() {
        let repo_root = repo.path.clone();
        let repo_label = payload.repo.clone();
        let path_bg = path.clone();
        let payload_bg = payload.clone();
        // 2026-08-31 incident (store.rs module doc): assemble + insert +
        // the working-tree read + the view resolve are all synchronous
        // (no `.await` on the reply path at all) — one closure.
        let (view, review_id) = state
            .store
            .run_blocking(move |store| -> Result<_, ApiError> {
                let row = assemble_reply_annotation(store, repo_id, &path_bg, &payload_bg, now)?;
                store.insert_annotation(&row)?;
                let content =
                    std::fs::read_to_string(repo_root.join(&row.path)).unwrap_or_default();
                let review_id = row.review_id;
                let view = annotation_view(store, row, &repo_label, &content)?;
                Ok((view, review_id))
            })
            .await?;
        emit_annotation_changed(&state.bus, &payload.repo, &path, review_id);
        return Ok((
            StatusCode::CREATED,
            [(header::CACHE_CONTROL, "no-store")],
            Json(view),
        ));
    }

    let built = assemble_top_level_annotation(&state, repo, repo_id, &path, &payload, now).await?;
    let repo_label = payload.repo.clone();
    // 2026-08-31 incident (store.rs module doc): insert + the view resolve
    // are the only store calls left after the (possibly async, diff-kind)
    // assemble step above — one closure for them.
    let (view, review_id) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            store.insert_annotation(&built.row)?;
            let review_id = built.row.review_id;
            let view = annotation_view(store, built.row, &repo_label, &built.view_content)?;
            Ok((view, review_id))
        })
        .await?;
    emit_annotation_changed(&state.bus, &payload.repo, &path, review_id);
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(view),
    ))
}

#[derive(Debug, Deserialize)]
pub struct PatchAnnotationBody {
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub resolved: Option<bool>,
    /// Validated against `annotations::is_valid_intent` when present.
    #[serde(default)]
    pub intent: Option<String>,
}

/// `PATCH /api/annotations/{id}` (W4.6; Phase D-server adds `intent`) —
/// update `body`/`resolved`/`intent` (whichever is present; an omitted
/// field is left untouched). NEVER touches any anchor field — the v1 rule,
/// preserved (`store::update_annotation`'s SQL has no `anchor*` column in
/// its SET list at all). `400` for an invalid `intent`; `404` for an
/// unknown `id`. Emits `annotation.changed {repo, path}` after a successful
/// update.
pub async fn patch_annotation(
    State(state): State<SharedState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(payload): Json<PatchAnnotationBody>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(intent) = payload.intent.as_deref() {
        if !annotations::is_valid_intent(intent) {
            return Err(ApiError::bad_request(format!("invalid intent: {intent:?}")));
        }
    }
    let now = chrono::Utc::now().timestamp();
    let state_bg = state.clone();
    let id_bg = id.clone();
    // 2026-08-31 incident (store.rs module doc): update + read-back +
    // repo lookup + view resolve are all synchronous (no `.await` in this
    // handler at all) — one closure.
    let (view, repo_name, path, review_id) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let existed = store.update_annotation(
                &id_bg,
                payload.body.as_deref(),
                payload.resolved,
                payload.intent.as_deref(),
                now,
            )?;
            if !existed {
                return Err(ApiError::not_found(format!("annotation {id_bg:?}")));
            }
            let row = store.get_annotation(&id_bg)?.ok_or_else(|| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("annotation {id_bg:?} vanished immediately after update"),
                )
            })?;
            let repo = find_repo_by_id(&state_bg, row.repo_id)?;
            let repo_name = repo.name.clone();
            // V70-A2 (SEC-13) — `row.path` is an annotation's stored path,
            // not a live query param, but it was a query param once; the
            // containment check costs two `canonicalize` calls and closes
            // the "a symlink appeared after the row was written" case.
            let content = crate::security::paths::contained_abs_path(&repo.path, &row.path)
                .ok()
                .and_then(|abs| std::fs::read_to_string(abs).ok())
                .unwrap_or_default();
            let path = row.path.clone();
            let review_id = row.review_id;
            let view = annotation_view(store, row, &repo_name, &content)?;
            Ok((view, repo_name, path, review_id))
        })
        .await?;
    emit_annotation_changed(&state.bus, &repo_name, &path, review_id);
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(view)))
}

/// `DELETE /api/annotations/{id}` (W4.6; Phase D-server: cascades to any
/// replies) — hard delete, no tombstone. Deleting a PARENT also deletes
/// every reply nested under it (`store::delete_annotation`'s own
/// transaction); deleting a reply removes just that one row. `404` for an
/// unknown `id`. Emits `annotation.changed {repo, path}` after the delete.
pub async fn delete_annotation(
    State(state): State<SharedState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let state_bg = state.clone();
    let id_bg = id.clone();
    // 2026-08-31 incident (store.rs module doc): fetch + delete + repo
    // lookup run as one closure; no async work anywhere in this handler.
    let (repo_name, path, review_id) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let row = store
                .get_annotation(&id_bg)?
                .ok_or_else(|| ApiError::not_found(format!("annotation {id_bg:?}")))?;
            store.delete_annotation(&id_bg)?;
            let repo = find_repo_by_id(&state_bg, row.repo_id)?;
            Ok((repo.name.clone(), row.path, row.review_id))
        })
        .await?;
    emit_annotation_changed(&state.bus, &repo_name, &path, review_id);
    Ok(StatusCode::NO_CONTENT)
}

// --- V4.C2 suggestion storage + batch ------------------------------------

#[derive(Debug, Deserialize)]
pub struct PutSuggestionBody {
    pub replacement: String,
}

/// Line span a stored `line`/`range` anchor describes (1-based, inclusive).
struct StoredAnchorLines {
    start: u32,
    end: u32,
}

fn stored_anchor_lines(row: &store::AnnotationRow) -> Result<StoredAnchorLines, ApiError> {
    let raw = row
        .anchor
        .as_deref()
        .ok_or_else(|| ApiError::bad_request("annotation has no anchor"))?;
    let start_anchor: Anchor =
        serde_json::from_str(raw).map_err(|e| corrupt_anchor_error(&row.id, "anchor", e))?;
    let start = match start_anchor {
        Anchor::Selection { offset, .. } => offset,
        _ => {
            return Err(ApiError::bad_request(
                "suggestion target anchor is not a line selection",
            ))
        }
    };
    let end = if row.anchor_kind == annotations::ANCHOR_KIND_RANGE {
        let end_raw = row
            .anchor2
            .as_deref()
            .ok_or_else(|| ApiError::bad_request("range annotation is missing its end anchor"))?;
        let end_anchor: Anchor = serde_json::from_str(end_raw)
            .map_err(|e| corrupt_anchor_error(&row.id, "anchor2", e))?;
        match end_anchor {
            Anchor::Selection { offset, .. } => offset,
            _ => {
                return Err(ApiError::bad_request(
                    "range suggestion target end-anchor is not a line selection",
                ))
            }
        }
    } else {
        start
    };
    let (lo, hi) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    Ok(StoredAnchorLines { start: lo, end: hi })
}

fn original_from_content(
    content: &str,
    lines: StoredAnchorLines,
    path: &str,
) -> Result<String, ApiError> {
    let mut out = Vec::new();
    for n in lines.start..=lines.end {
        out.push(line_at(content, n, path)?.to_string());
    }
    Ok(out.join("\n"))
}

/// 400 unless `row` is a top-level `line|range` annotation whose side
/// is not `"old"` (plain annotations have `side = NULL` and are valid).
pub(crate) fn validate_suggestion_target(row: &store::AnnotationRow) -> Result<(), ApiError> {
    if row.parent_id.is_some() {
        return Err(ApiError::bad_request(
            "cannot attach a suggestion to a reply (top-level only)",
        ));
    }
    if row.anchor_kind != annotations::ANCHOR_KIND_LINE
        && row.anchor_kind != annotations::ANCHOR_KIND_RANGE
    {
        return Err(ApiError::bad_request(format!(
            "suggestions require anchor_kind line|range (got {:?})",
            row.anchor_kind
        )));
    }
    if row.side.as_deref() == Some("old") {
        return Err(ApiError::bad_request(
            "cannot propose an edit on side=old (deleted lines)",
        ));
    }
    Ok(())
}

/// Captured suggestion source: `original` text + `base_blob_sha`
/// (`""` for a plain working-tree annotation).
struct SuggestionCapture {
    original: String,
    base_blob_sha: String,
}

fn capture_suggestion_from_row(
    store: &Store,
    repo: &RepoEntry,
    row: &store::AnnotationRow,
) -> Result<SuggestionCapture, ApiError> {
    validate_suggestion_target(row)?;
    let lines = stored_anchor_lines(row)?;
    if let Some(review_id) = row.review_id {
        let ps_number = row.ps_number.ok_or_else(|| {
            ApiError::bad_request(format!(
                "review-scoped annotation {} is missing ps_number",
                row.id
            ))
        })?;
        let ps = store.get_patchset(review_id, ps_number)?.ok_or_else(|| {
            ApiError::bad_request(format!(
                "patchset {ps_number} for review {review_id} is gone"
            ))
        })?;
        // side != old is already rejected; C1's new-side path is tip_sha.
        let content = read_pinned_review_file(repo, &row.path, &ps, "new")?;
        let original = original_from_content(&content, lines, &row.path)?;
        Ok(SuggestionCapture {
            original,
            base_blob_sha: ingest::git_blob_hash(content.as_bytes()),
        })
    } else {
        let content = read_working_tree_text(repo, &row.path)?;
        let original = original_from_content(&content, lines, &row.path)?;
        Ok(SuggestionCapture {
            original,
            base_blob_sha: String::new(),
        })
    }
}

/// `PUT /api/annotations/{id}/suggestion` — BEARER. Captures `original`
/// from the pinned ps tip blob (review-scoped) or the working tree
/// (plain). Re-PUT replaces the row and resets `applied`.
pub async fn put_annotation_suggestion(
    State(state): State<SharedState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(payload): Json<PutSuggestionBody>,
) -> Result<impl IntoResponse, ApiError> {
    let state_bg = state.clone();
    let id_bg = id.clone();
    // 2026-08-31 incident (store.rs module doc): fetch + repo lookup +
    // capture + upsert + read-back are all synchronous (no `.await`
    // anywhere in this handler) — one closure.
    let (body, repo_name, path, review_id) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let row = store
                .get_annotation(&id_bg)?
                .ok_or_else(|| ApiError::not_found(format!("annotation {id_bg:?}")))?;
            let repo = find_repo_by_id(&state_bg, row.repo_id)?;
            let capture = capture_suggestion_from_row(store, repo, &row)?;
            let now = chrono::Utc::now().timestamp();
            store.upsert_annotation_suggestion(
                &id_bg,
                &payload.replacement,
                &capture.original,
                &capture.base_blob_sha,
                now,
            )?;
            let stored = store.get_annotation_suggestion(&id_bg)?.ok_or_else(|| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("suggestion for {id_bg:?} vanished immediately after upsert"),
                )
            })?;
            let body = serde_json::json!({
                "annotation_id": stored.annotation_id,
                "replacement": stored.replacement,
                "original": stored.original,
                "base_blob_sha": stored.base_blob_sha,
                "applied": stored.applied,
                "applied_at": stored.applied_at,
                "applied_head_sha": stored.applied_head_sha,
            });
            Ok((body, repo.name.clone(), row.path.clone(), row.review_id))
        })
        .await?;
    emit_annotation_changed(&state.bus, &repo_name, &path, review_id);
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(body)))
}

/// `DELETE /api/annotations/{id}/suggestion` — BEARER. 404 when no row.
pub async fn delete_annotation_suggestion(
    State(state): State<SharedState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let state_bg = state.clone();
    let id_bg = id.clone();
    // 2026-08-31 incident (store.rs module doc): fetch + delete + repo
    // lookup run as one closure; no async work in this handler.
    let (repo_name, path, review_id) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let row = store
                .get_annotation(&id_bg)?
                .ok_or_else(|| ApiError::not_found(format!("annotation {id_bg:?}")))?;
            let existed = store.delete_annotation_suggestion(&id_bg)?;
            if !existed {
                return Err(ApiError::not_found(format!(
                    "no suggestion on annotation {id_bg:?}"
                )));
            }
            let repo = find_repo_by_id(&state_bg, row.repo_id)?;
            Ok((repo.name.clone(), row.path, row.review_id))
        })
        .await?;
    emit_annotation_changed(&state.bus, &repo_name, &path, review_id);
    Ok(StatusCode::NO_CONTENT)
}

const MAX_ANNOTATION_BATCH_OPS: usize = 100;

#[derive(Debug, Deserialize)]
pub struct AnnotationBatchBody {
    pub repo: String,
    pub ops: Vec<AnnotationBatchOp>,
}

#[derive(Debug, Deserialize)]
pub struct SuggestionReplacement {
    pub replacement: String,
}

/// One `POST /api/annotations/batch` op. Tagged `op` snake_case. Ops
/// reference EXISTING ids only — an `add_comment`'s minted id cannot be
/// used by a later op in the same batch.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AnnotationBatchOp {
    AddComment {
        path: String,
        #[serde(default)]
        line: Option<u32>,
        body: String,
        #[serde(default)]
        author: Option<String>,
        #[serde(default)]
        anchor_kind: Option<String>,
        #[serde(default)]
        line_end: Option<u32>,
        #[serde(default)]
        sha: Option<String>,
        #[serde(default)]
        intent: Option<String>,
        #[serde(default)]
        review_id: Option<i64>,
        #[serde(default)]
        ps: Option<i64>,
        #[serde(default)]
        side: Option<String>,
        #[serde(default)]
        suggestion: Option<SuggestionReplacement>,
    },
    AddReply {
        parent_id: String,
        body: String,
        #[serde(default)]
        intent: Option<String>,
    },
    EditBody {
        id: String,
        body: String,
    },
    SetIntent {
        id: String,
        intent: String,
    },
    Resolve {
        id: String,
    },
    Unresolve {
        id: String,
    },
    Delete {
        id: String,
    },
    SetSuggestion {
        id: String,
        replacement: String,
    },
    ClearSuggestion {
        id: String,
    },
}

fn emit_annotation_batch_changed(
    bus: &kb_core::events::EventBus,
    repo: &str,
    paths: &[String],
    review_id: Option<i64>,
) {
    let mut body = serde_json::json!({
        "repo": repo,
        "paths": paths,
        "batch": true,
    });
    if let Some(id) = review_id {
        body["review_id"] = serde_json::json!(id);
    }
    bus.emit("annotation.changed", body);
}

/// Collect unique sorted paths + the shared review_id (present only
/// when every review-scoped op names the same review).
struct BatchEmitScope {
    paths: Vec<String>,
    review_id: Option<i64>,
}

fn batch_emit_scope(
    paths: impl IntoIterator<Item = String>,
    review_ids: impl IntoIterator<Item = Option<i64>>,
) -> BatchEmitScope {
    let mut uniq: Vec<String> = paths.into_iter().collect();
    uniq.sort();
    uniq.dedup();
    let mut seen: Vec<i64> = Vec::new();
    for rid in review_ids.into_iter().flatten() {
        if !seen.contains(&rid) {
            seen.push(rid);
        }
    }
    // Present iff every review-scoped op names the same review
    // (plain ops do not veto; zero review-scoped ops → omit).
    let review_id = if seen.len() == 1 { Some(seen[0]) } else { None };
    BatchEmitScope {
        paths: uniq,
        review_id,
    }
}

fn require_existing_annotation(store: &Store, id: &str) -> Result<store::AnnotationRow, ApiError> {
    store
        .get_annotation(id)?
        .ok_or_else(|| ApiError::bad_request(format!("unknown annotation id {id:?}")))
}

/// `POST /api/annotations/batch` — BEARER. Validate + git-read first,
/// then ONE store transaction, then at most ONE SSE. All-no-op →
/// `{changed:false}` and no SSE. Cap 100 ops.
pub async fn batch_annotations(
    State(state): State<SharedState>,
    Json(payload): Json<AnnotationBatchBody>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &payload.repo)?;
    if payload.ops.is_empty() {
        return Err(ApiError::bad_request("batch requires at least one op"));
    }
    if payload.ops.len() > MAX_ANNOTATION_BATCH_OPS {
        return Err(ApiError::bad_request(format!(
            "batch of {} ops exceeds the {MAX_ANNOTATION_BATCH_OPS} limit",
            payload.ops.len()
        )));
    }

    let now = chrono::Utc::now().timestamp();
    let repo_label = payload.repo.clone();

    // (a1) — the only per-op ASYNC work: assemble every `AddComment`'s
    // top-level annotation up front, in op order (2026-08-31 incident,
    // store.rs module doc: `assemble_top_level_annotation` already wraps
    // its own store calls internally, so nothing extra to do for it here).
    // Every other op kind's store work is purely synchronous and is
    // folded into the ONE closure in (a2) below, alongside the write tx —
    // "then ONE store transaction" (this fn's own doc) now also covers
    // the validate/prepare reads, not just the final write.
    let mut built_comments: Vec<Option<BuiltAnnotation>> = Vec::with_capacity(payload.ops.len());
    for op in &payload.ops {
        let built = if let AnnotationBatchOp::AddComment {
            path,
            line,
            body,
            author,
            anchor_kind,
            line_end,
            sha,
            intent,
            review_id,
            ps,
            side,
            ..
        } = op
        {
            let rel = safe_rel_path(path)?.to_string();
            let create = CreateAnnotationBody {
                repo: repo_label.clone(),
                path: rel.clone(),
                line: *line,
                body: body.clone(),
                author: author.clone(),
                anchor_kind: anchor_kind.clone(),
                line_end: *line_end,
                sha: sha.clone(),
                intent: intent.clone(),
                parent_id: None,
                review_id: *review_id,
                ps: *ps,
                side: side.clone(),
                // V70-A10 — `AnnotationBatchOp::AddComment` has no `set_id`
                // field of its own (batch creation isn't wired to
                // workspaces in v0); every batch-created annotation is
                // therefore unscoped, same as an omitted `set_id` on a
                // plain `POST /api/annotations`.
                set_id: None,
                trail_id: None,
            };
            Some(assemble_top_level_annotation(&state, repo, repo_id, &rel, &create, now).await?)
        } else {
            None
        };
        built_comments.push(built);
    }

    // (a2) — validate the remaining ops + fold everything (including the
    // AddComment suggestion capture, pure CPU over the pre-built rows from
    // (a1)) into `prepared`, then (b) the one store transaction — all
    // synchronous now: one closure, one hop to the blocking pool.
    let ops = payload.ops;
    let repo_bg = repo.clone();
    let repo_label_bg = repo_label.clone();
    let (report, paths, review_ids) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let mut prepared: Vec<store::PreparedAnnotationOp> = Vec::with_capacity(ops.len());
            let mut paths: Vec<String> = Vec::new();
            let mut review_ids: Vec<Option<i64>> = Vec::new();

            for (op, built) in ops.into_iter().zip(built_comments) {
                match op {
                    AnnotationBatchOp::AddComment { suggestion, .. } => {
                        let built = built.expect("AddComment ops were pre-built in lockstep");
                        let rel = built.row.path.clone();
                        let sug = if let Some(s) = suggestion {
                            validate_suggestion_target(&built.row)?;
                            let lines = stored_anchor_lines(&built.row)?;
                            let original = original_from_content(&built.view_content, lines, &rel)?;
                            let base_blob_sha = if built.row.review_id.is_some() {
                                ingest::git_blob_hash(built.view_content.as_bytes())
                            } else {
                                String::new()
                            };
                            Some(store::PreparedSuggestionWrite {
                                annotation_id: built.row.id.clone(),
                                replacement: s.replacement,
                                original,
                                base_blob_sha,
                            })
                        } else {
                            None
                        };
                        paths.push(built.row.path.clone());
                        review_ids.push(built.row.review_id);
                        prepared.push(store::PreparedAnnotationOp::Insert {
                            row: Box::new(built.row),
                            suggestion: sug,
                        });
                    }
                    AnnotationBatchOp::AddReply {
                        parent_id,
                        body,
                        intent,
                    } => {
                        let parent = require_existing_annotation(store, &parent_id)?;
                        if parent.repo_id != repo_id {
                            return Err(ApiError::bad_request(format!(
                                "reply parent {parent_id:?} belongs to a different repo"
                            )));
                        }
                        let create = CreateAnnotationBody {
                            repo: repo_label_bg.clone(),
                            path: parent.path.clone(),
                            line: None,
                            body,
                            author: None,
                            anchor_kind: None,
                            line_end: None,
                            sha: None,
                            intent,
                            parent_id: Some(parent_id),
                            review_id: None,
                            ps: None,
                            side: None,
                            // V70-A10 — `None` here means "inherit from the
                            // parent" (`assemble_reply_annotation`'s
                            // `inherit_scope_field` ladder), same as
                            // `review_id: None` above already does — a
                            // batch reply under a workspace-scoped parent
                            // still inherits that `set_id` correctly.
                            set_id: None,
                            trail_id: None,
                        };
                        let row =
                            assemble_reply_annotation(store, repo_id, &parent.path, &create, now)?;
                        paths.push(row.path.clone());
                        review_ids.push(row.review_id);
                        prepared.push(store::PreparedAnnotationOp::Insert {
                            row: Box::new(row),
                            suggestion: None,
                        });
                    }
                    AnnotationBatchOp::EditBody { id, body } => {
                        let row = require_existing_annotation(store, &id)?;
                        paths.push(row.path);
                        review_ids.push(row.review_id);
                        prepared.push(store::PreparedAnnotationOp::EditBody { id, body });
                    }
                    AnnotationBatchOp::SetIntent { id, intent } => {
                        if !annotations::is_valid_intent(&intent) {
                            return Err(ApiError::bad_request(format!(
                                "invalid intent: {intent:?}"
                            )));
                        }
                        let row = require_existing_annotation(store, &id)?;
                        paths.push(row.path);
                        review_ids.push(row.review_id);
                        prepared.push(store::PreparedAnnotationOp::SetIntent { id, intent });
                    }
                    AnnotationBatchOp::Resolve { id } => {
                        let row = require_existing_annotation(store, &id)?;
                        paths.push(row.path);
                        review_ids.push(row.review_id);
                        prepared
                            .push(store::PreparedAnnotationOp::SetResolved { id, resolved: true });
                    }
                    AnnotationBatchOp::Unresolve { id } => {
                        let row = require_existing_annotation(store, &id)?;
                        paths.push(row.path);
                        review_ids.push(row.review_id);
                        prepared.push(store::PreparedAnnotationOp::SetResolved {
                            id,
                            resolved: false,
                        });
                    }
                    AnnotationBatchOp::Delete { id } => {
                        let row = require_existing_annotation(store, &id)?;
                        paths.push(row.path);
                        review_ids.push(row.review_id);
                        prepared.push(store::PreparedAnnotationOp::Delete { id });
                    }
                    AnnotationBatchOp::SetSuggestion { id, replacement } => {
                        let row = require_existing_annotation(store, &id)?;
                        let capture = capture_suggestion_from_row(store, &repo_bg, &row)?;
                        paths.push(row.path);
                        review_ids.push(row.review_id);
                        prepared.push(store::PreparedAnnotationOp::UpsertSuggestion(
                            store::PreparedSuggestionWrite {
                                annotation_id: id,
                                replacement,
                                original: capture.original,
                                base_blob_sha: capture.base_blob_sha,
                            },
                        ));
                    }
                    AnnotationBatchOp::ClearSuggestion { id } => {
                        let row = require_existing_annotation(store, &id)?;
                        paths.push(row.path);
                        review_ids.push(row.review_id);
                        prepared.push(store::PreparedAnnotationOp::ClearSuggestion {
                            annotation_id: id,
                        });
                    }
                }
            }

            // (b) ONE store transaction.
            let report = store.apply_annotation_ops(&prepared, now)?;
            Ok((report, paths, review_ids))
        })
        .await?;

    // (c)+(d) one SSE iff anything changed.
    if report.changed {
        let scope = batch_emit_scope(paths, review_ids);
        emit_annotation_batch_changed(&state.bus, &repo_label, &scope.paths, scope.review_id);
    }

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "applied": report.applied,
            "created_ids": report.created_ids,
            "changed": report.changed,
        })),
    ))
}

/// `list_open_annotations`'s hard cap — see that fn's doc.
const MAX_OPEN_ANNOTATIONS: usize = 500;

#[derive(Debug, Deserialize)]
pub struct OpenAnnotationsParams {
    pub repo: String,
    #[serde(default)]
    pub intent: Option<String>,
    #[serde(default)]
    pub path_prefix: Option<String>,
}

/// One `GET /api/annotations/open` row — an [`AnnotationView`] plus its
/// direct reply count (see `store::Store::list_open_annotations`'s doc for
/// why replies themselves are excluded from the listing).
#[derive(Debug, Serialize)]
pub struct OpenAnnotationEntry {
    #[serde(flatten)]
    pub view: AnnotationView,
    pub reply_count: i64,
}

/// `GET /api/annotations/open?repo=[&intent=][&path_prefix=]` (Phase
/// D-server) — every UNRESOLVED, TOP-LEVEL annotation across the WHOLE
/// repo (not scoped to one `path`, unlike `GET /api/annotations`),
/// optionally filtered to an exact `intent` match and/or a `path_prefix`,
/// newest-first, capped at [`MAX_OPEN_ANNOTATIONS`] with a `truncated`
/// flag. The agent hook's + a future dashboard's query surface. `400` for
/// an invalid `intent`.
pub async fn list_open_annotations(
    State(state): State<SharedState>,
    Query(params): Query<OpenAnnotationsParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    if let Some(intent) = params.intent.as_deref() {
        if !annotations::is_valid_intent(intent) {
            return Err(ApiError::bad_request(format!("invalid intent: {intent:?}")));
        }
    }
    let repo_root = repo.path.clone();
    let repo_label = params.repo.clone();
    let intent_owned = params.intent.clone();
    let path_prefix_owned = params.path_prefix.clone();
    // 2026-08-31 incident (store.rs module doc): the list + per-row view
    // resolve (a store call apiece for symbol-kind/reply rows) run as one
    // closure; no async work happens anywhere in this handler.
    let (entries, truncated) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let mut rows = store.list_open_annotations(
                repo_id,
                intent_owned.as_deref(),
                path_prefix_owned.as_deref(),
                MAX_OPEN_ANNOTATIONS + 1,
            )?;
            let truncated = rows.len() > MAX_OPEN_ANNOTATIONS;
            rows.truncate(MAX_OPEN_ANNOTATIONS);

            // Cache each path's working-tree read — several open
            // annotations commonly share a path, and re-reading the same
            // file per row would be wasteful for a repo-wide listing.
            let mut content_cache: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            let mut entries = Vec::with_capacity(rows.len());
            for (row, reply_count) in rows {
                let content = if let Some(c) = content_cache.get(&row.path) {
                    c.clone()
                } else {
                    let c = std::fs::read_to_string(repo_root.join(&row.path)).unwrap_or_default();
                    content_cache.insert(row.path.clone(), c.clone());
                    c
                };
                let view = annotation_view(store, row, &repo_label, &content)?;
                entries.push(OpenAnnotationEntry { view, reply_count });
            }
            Ok((entries, truncated))
        })
        .await?;

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "repo": params.repo,
            "intent": params.intent,
            "path_prefix": params.path_prefix,
            "annotations": entries,
            "truncated": truncated,
        })),
    ))
}

// --- POST /api/checkout (W4.7 — confirmed checkout) ------------------------

#[derive(Debug, Deserialize)]
pub struct CheckoutBody {
    pub repo: String,
    #[serde(rename = "ref")]
    pub target: String,
}

/// `POST /api/checkout` (W4.7) — see `crate::checkout`'s module doc for the
/// full dirty-refuse/switch-vs-checkout contract; this handler is a thin
/// wrapper (the subprocess work is genuinely blocking, so it runs inside
/// `spawn_blocking`, same discipline as `blame`/`diff`/`timeline` above).
/// A dirty working tree is NOT an `ApiError` — it gets its own structured
/// 409 body (`dirty_paths`) rather than the uniform `{"error": ...}` shape,
/// since a caller needs the actual path list to act on the refusal.
/// Mounted on the LOOPBACK-ONLY sub-router (`router.rs`) — the daemon's
/// first sanctioned working-tree mutation (V4.S1 apply is the second)
/// gets a strictly stronger gate than every other route in this crate.
pub async fn checkout_route(
    State(state): State<SharedState>,
    Json(params): Json<CheckoutBody>,
) -> Result<Response, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &params.repo)?;
    let repo_root = repo.path.clone();
    // V70-A2 (SEC-17) — validate the caller's target into the type
    // `switch_repo` takes. A `RevspecError` folds into the same
    // `CheckoutError::BadTarget` refusal the inline guard produced, so
    // this route's wire shape is unchanged.
    let target = crate::git::Revspec::parse(&params.target)
        .map_err(|e| ApiError::bad_request(crate::checkout::CheckoutError::from(e).to_string()))?;
    let result =
        tokio::task::spawn_blocking(move || crate::checkout::switch_repo(&repo_root, &target))
            .await
            .map_err(|e| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("checkout task panicked: {e}"),
                )
            })?;

    match result {
        Ok(outcome) => Ok((
            [(header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({
                "repo": params.repo,
                "ref": outcome.target,
                "detached": outcome.detached,
            })),
        )
            .into_response()),
        Err(crate::checkout::CheckoutError::Dirty(paths)) => Ok((
            StatusCode::CONFLICT,
            [(header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({
                "error": "working tree is dirty",
                "repo": params.repo,
                "dirty_paths": paths,
            })),
        )
            .into_response()),
        Err(e) => Err(ApiError::bad_request(e.to_string())),
    }
}

// --- GET /api/commit, /api/compare, /api/branches, /api/file-history -----
//
// Phase C-server ("The Operable Reader," time-first-class) — the four
// endpoints in `crate::history`. Same ordinary `auth_bearer`-gated `api`
// router as `/join/commit`/`/diff` (`router.rs`): none of these exposes
// anything more sensitive than those two already do — commit metadata,
// file stats, and branch listings a `git log`/`git branch` at a terminal
// would already show, plus the SAME `join::ladder::Attribution` shape
// `/join/commit` already serves over this gate.

impl From<crate::history::HistoryError> for ApiError {
    fn from(e: crate::history::HistoryError) -> Self {
        use crate::history::HistoryError::*;
        match e {
            Spawn(err) => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("spawn git: {err}"),
            ),
            // A malformed/unresolvable revspec is the near-exclusive
            // real-world cause here (same reasoning as `diff::DiffError`'s
            // own `GitFailed` mapping) — a caller mistake, not a
            // daemon-side fault.
            GitFailed { stderr, .. } => ApiError::bad_request(stderr.trim().to_string()),
            err @ BadRevspec(_) => ApiError::bad_request(err.to_string()),
            NotFound(sha) => ApiError::not_found(format!("no such commit: {sha:?}")),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CommitParams {
    pub repo: String,
    pub sha: String,
}

#[derive(Debug, Serialize)]
pub struct CommitPageResponse {
    pub schema: &'static str,
    pub repo: String,
    pub sha: String,
    pub subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub author: crate::history::Person,
    pub committer: crate::history::Person,
    pub parents: Vec<String>,
    pub trailers: Vec<crate::history::Trailer>,
    pub attribution: crate::join::ladder::Attribution,
    pub files: Vec<crate::numstat::FileChange>,
    pub totals: crate::history::FileTotals,
}

/// `GET /api/commit?repo=&sha=` (Phase C1) — the commit page hub:
/// `crate::history::commit`'s metadata + file list, plus attribution
/// through the join ladder (`join::ladder::resolve_commit` — cached,
/// degrades to `confidence: "none"` when kb is unreachable, NEVER 500s for
/// that, exactly like `GET /api/join/commit`). `400` on a malformed `sha`
/// (reusing `join::ladder::is_plausible_sha`'s 4-64-hex gate); `404` when
/// it's well-formed but unresolvable (`history::HistoryError::NotFound`).
///
/// The two blocking git calls (`commit_meta`, `commit_files`) run inside
/// ONE `spawn_blocking` — sequential, not concurrent, since `commit_files`
/// needs `commit_meta`'s resolved FULL sha (a short caller-supplied prefix
/// must not silently diverge between the two reads). The join ladder call
/// happens afterward, in the async task itself (it's already async, see
/// `join::ladder::resolve_commit`'s own doc — no `spawn_blocking` needed
/// there).
pub async fn commit_route(
    State(state): State<SharedState>,
    Query(params): Query<CommitParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let sha = params.sha.trim();
    if !crate::join::ladder::is_plausible_sha(sha) {
        return Err(ApiError::bad_request(format!(
            "sha must be 4-64 hex characters (got {:?})",
            params.sha
        )));
    }

    let repo_root = repo.path.clone();
    let sha_owned = sha.to_string();
    let (meta, files) = tokio::task::spawn_blocking(move || {
        let meta = crate::history::commit::commit_meta(&repo_root, &sha_owned)?;
        let files = crate::history::commit::commit_files(&repo_root, &meta.sha)?;
        Ok::<_, crate::history::HistoryError>((meta, files))
    })
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("commit task panicked: {e}"),
        )
    })??;

    let attribution = crate::join::ladder::resolve_commit(
        repo,
        repo_id,
        &meta.sha,
        &state.store,
        &state.kb_client,
    )
    .await;
    let totals = crate::history::totals_for(&files);

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(CommitPageResponse {
            schema: "commit/1",
            repo: params.repo,
            sha: meta.sha,
            subject: meta.subject,
            body: meta.body,
            author: meta.author,
            committer: meta.committer,
            parents: meta.parents,
            trailers: meta.trailers,
            attribution,
            files,
            totals,
        }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct CompareParams {
    pub repo: String,
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub three_dot: bool,
    /// Phase G-server — opt-in per-commit join-ladder attribution (see
    /// [`CompareCommitOut`]'s doc). Absent/`false` is the DEFAULT and
    /// leaves `commits[]` byte-for-byte what it always was — the flag
    /// exists so the plain compare (the common case) stays cheap, never
    /// paying for a ladder resolution per commit it didn't ask for.
    #[serde(default)]
    pub attribution: bool,
}

/// One `commits[]` entry — Phase G-server wraps the plain
/// `CommitSummary` (flattened, so its own four fields land at the TOP
/// level exactly as before) with an OPTIONAL `attribution`, present only
/// when `?attribution=true` was given. `skip_serializing_if` means a
/// flag-off response has NO `attribution` key anywhere in `commits[]` —
/// structurally identical to the pre-Phase-G `Vec<CommitSummary>` shape
/// (pinned by `compare_attribution_flag_absent_is_byte_identical_to_plain_
/// commit_summary` in `tests/time_routes.rs`).
#[derive(Debug, Serialize)]
pub struct CompareCommitOut {
    #[serde(flatten)]
    pub summary: crate::history::CommitSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution: Option<crate::join::ladder::Attribution>,
}

#[derive(Debug, Serialize)]
pub struct ComparePageResponse {
    pub schema: &'static str,
    pub repo: String,
    pub from: String,
    pub to: String,
    pub three_dot: bool,
    pub resolved: crate::history::Resolved,
    pub commits: Vec<CompareCommitOut>,
    pub commits_truncated: bool,
    pub files: Vec<crate::numstat::FileChange>,
    pub totals: crate::history::FileTotals,
}

/// `GET /api/compare?repo=&from=&to=&three_dot=[&attribution=true]` (Phase
/// C2 + Phase G-server's `attribution` flag) — repo-level compare:
/// `crate::history::compare`'s own doc has the two-dot/three-dot contract.
/// `400` on a dash-prefixed `from`/`to` (argument-injection guard,
/// `history::HistoryError::BadRevspec`) or an unresolvable revspec
/// (`GitFailed`); identical `from`/`to` is NOT an error (empty
/// `commits`/`files`). `attribution=true` resolves each commit through the
/// SAME cached join ladder `GET /api/join/commit`/`GET /api/branches`
/// already use — sequential, one `resolve_commit` per commit (mirrors
/// `branches_route`'s own per-branch loop below; `commits` is already
/// capped at `history::compare::MAX_COMMITS`, so this is bounded the same
/// way), never 500ing on a kb-unreachable degrade (each commit's
/// attribution just comes back `confidence: "none"`).
pub async fn compare_route(
    State(state): State<SharedState>,
    Query(params): Query<CompareParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo_root = repo.path.clone();
    let from = parse_revspec(&params.from)?;
    let to = parse_revspec(&params.to)?;
    let three_dot = params.three_dot;
    let cmp = tokio::task::spawn_blocking(move || {
        crate::history::compare::compare(&repo_root, &from, &to, three_dot)
    })
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("compare task panicked: {e}"),
        )
    })??;

    let mut commits = Vec::with_capacity(cmp.commits.len());
    for summary in cmp.commits {
        let attribution = if params.attribution {
            Some(
                crate::join::ladder::resolve_commit(
                    repo,
                    repo_id,
                    &summary.sha,
                    &state.store,
                    &state.kb_client,
                )
                .await,
            )
        } else {
            None
        };
        commits.push(CompareCommitOut {
            summary,
            attribution,
        });
    }

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(ComparePageResponse {
            schema: "compare/1",
            repo: params.repo,
            from: params.from,
            to: params.to,
            three_dot,
            resolved: cmp.resolved,
            commits,
            commits_truncated: cmp.commits_truncated,
            files: cmp.files,
            totals: cmp.totals,
        }),
    ))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BranchSort {
    /// Lexicographic by short name — today's default, including the
    /// name-then-truncate semantics.
    #[default]
    Name,
    /// Cheap gix/store terms first, cap, then rev-list survivors.
    Suggested,
}

#[derive(Debug, Deserialize)]
pub struct BranchesParams {
    pub repo: String,
    /// `name` (default) or `suggested`.
    #[serde(default)]
    pub sort: BranchSort,
}

#[derive(Debug, Serialize)]
pub struct BranchLast {
    pub subject: String,
    pub author_time: i64,
}

#[derive(Debug, Serialize)]
pub struct BranchOut {
    pub name: String,
    pub target_sha: String,
    pub is_head: bool,
    /// V70-A3X: `Some(0)` for the DEFAULT branch (definitionally zero ahead/
    /// behind of itself — measured, not guessed) and for every branch
    /// `ahead_behind` actually ran `git rev-list` for; `None` when there was
    /// NOTHING to compare against (a detached/unborn HEAD with no
    /// `origin/HEAD` either) — previously both cases silently reported
    /// `0`, indistinguishable from a genuine zero-diff measurement.
    pub ahead: Option<u32>,
    pub behind: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last: Option<BranchLast>,
    pub attribution: crate::join::ladder::Attribution,
    /// `None` for a local branch (skipped on the wire).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    pub has_open_review: bool,
    /// Present only for `sort=suggested`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggest: Option<crate::history::branches::Suggest>,
}

#[derive(Debug, Serialize)]
pub struct BranchesResponse {
    pub schema: &'static str,
    pub repo: String,
    pub default: Option<String>,
    pub branches: Vec<BranchOut>,
    pub truncated: bool,
}

/// One branch ref's pre-async-await data, gathered from the `!Send` gix
/// handle BEFORE any `.await` in `branches_route` below — see that fn's
/// doc for why this split exists.
struct BranchPre {
    name: String,
    /// Full ref (`refs/heads/…` or `refs/remotes/…`) — the revspec handed
    /// to `ahead_behind` so a remote-only short name still resolves.
    revspec: String,
    remote: Option<String>,
    target_sha: String,
    is_head: bool,
    last: Option<BranchLast>,
    has_open_review: bool,
}

/// Join-ladder confidence → suggested-rank weight. `"none"` is unmeasured
/// (stays out of `terms`), never a silent 0.
fn attribution_suggest_weight(c: crate::join::ladder::Confidence) -> Option<f32> {
    use crate::join::ladder::Confidence;
    match c {
        Confidence::Trailer => Some(0.8),
        Confidence::Exact => Some(0.5),
        Confidence::Fuzzy => Some(0.2),
        Confidence::None => None,
    }
}

fn branch_name_key<'a>(name: &'a str, remote: Option<&'a str>) -> (&'a str, &'a str) {
    (name, remote.unwrap_or(""))
}

/// `true` if `head_ref` (a review's raw, user-typed head-ref string — NEVER
/// normalized at write time, see `reviews::create_review`'s own body: it
/// stores `body.head_ref.clone()` verbatim, and both the CLI and the SPA
/// send the bare short name) names this branch. V70-A3X: matches EXACTLY
/// as stored (covers a caller that already passed a full ref — the OLD
/// short-name-only comparison, `open_heads.contains(&r.name)`, could NEVER
/// match that shape at all) OR — the dominant case, a bare/short name —
/// qualified as a LOCAL branch (`refs/heads/<head_ref>`, the same shape
/// `git rev-parse` resolves a bare local-looking name to FIRST), compared
/// against the branch's OWN full ref rather than its bare short name.
/// Falls back to the bare short-name match when neither of the above hit,
/// so a `head_ref` naming a REMOTE branch directly still matches (as it
/// did before this fix) — `RefInfo::name` strips the `<remote>/` prefix,
/// so a bare `head_ref` can be genuinely ambiguous between two remotes
/// sharing a short name with NO local branch of that name to dedup them
/// (the local-wins filter in `branches_route` already removes a remote
/// entry whenever a SAME-named local branch exists, which is the far more
/// common shape); this fn does not attempt to resolve that residual,
/// structurally-ambiguous case — the caller-supplied string alone can't.
fn head_ref_matches(head_ref: &str, full_ref: &str, short_name: &str) -> bool {
    head_ref == full_ref || full_ref == format!("refs/heads/{head_ref}") || head_ref == short_name
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `GET /api/branches?repo=&sort=` (Phase C3 + V4.L1) — local + remote
/// branches (local-wins dedup on the short name), ahead/behind vs. the
/// default (`crate::git::default_branch`: origin/HEAD else HEAD's branch;
/// ONE `git rev-list` per non-default survivor) plus tip attribution
/// through the join ladder (cached in `commit_sessions`; degrades to
/// `confidence: "none"` on an unreachable kb, never 500s). The default
/// branch itself gets `ahead: Some(0), behind: Some(0)` (measured,
/// definitionally zero) without a `rev-list` call; a branch with NOTHING to
/// compare against (detached/unborn HEAD, no `origin/HEAD` either) gets
/// `None`/`None` (V70-A3X — `null` on the wire, distinct from a genuine
/// zero-diff measurement).
///
/// `sort=name` (default) sorts by short name then truncates to
/// `history::branches::MAX_BRANCHES` DIRECTLY — today's truncation set on a
/// locals-only repo. `sort=suggested` ranks on the cheap gix/store terms
/// (recency, has_open_review) FIRST, but truncates to `3×MAX_BRANCHES`
/// (V70-A3X two-phase widening, not `MAX_BRANCHES` itself) BEFORE running
/// rev-list + the join ladder for that WIDER survivor set — a branch that's
/// merely average on the cheap terms but genuinely strong on the EXPENSIVE
/// ones (attribution, ahead/behind) still gets measured and a chance to
/// rank, rather than being cut before those signals exist at all. Each
/// survivor's `suggest: {score, terms}` is attached (terms name every
/// computed signal; score is their sum), the FULL list is re-sorted by that
/// full score (name tie-break), and ONLY THEN truncated to the final
/// `MAX_BRANCHES` for the response.
///
/// `crate::git::GitRepo` (gix) is `!Send` — it is opened and read
/// (`default_branch`, `list_refs`, `list_remote_branches`, `commit_info`)
/// ENTIRELY inside a scoped block that ends before the first `.await`, so
/// it never has to cross an await point (which would make this handler's
/// future `!Send` and fail to compile as an axum route). The per-branch
/// `rev-list` call (genuinely blocking subprocess I/O) then runs inside
/// `spawn_blocking`, and the join-ladder call runs directly in this async
/// fn (already async, no `spawn_blocking` needed) — same discipline
/// `commit_route`/`compare_route` above use, just interleaved per-branch
/// instead of once up front.
pub async fn branches_route(
    State(state): State<SharedState>,
    Query(params): Query<BranchesParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let sort = params.sort;

    // 2026-08-31 incident (store.rs module doc): single store call, still
    // wrapped so it can never park this async worker.
    let repo_name = repo.name.clone();
    let open_heads: HashSet<String> = match state
        .store
        .run_blocking(move |store| store.list_open_reviews_for_repo(&repo_name))
        .await
    {
        Ok(rows) => rows.into_iter().map(|r| r.head_ref).collect(),
        Err(_) => HashSet::new(),
    };

    let (default, truncated, pre): (Option<String>, bool, Vec<BranchPre>) = {
        let git = GitRepo::open(&repo.path)?;
        let default = git.default_branch();
        let locals: Vec<_> = git
            .list_refs()?
            .into_iter()
            .filter(|r| r.kind == RefKind::Branch)
            .collect();
        let local_names: HashSet<String> = locals.iter().map(|r| r.name.clone()).collect();
        let remotes = git.list_remote_branches()?.into_iter().filter(|r| {
            // Local wins: the operator's handle, not the tracking copy.
            !local_names.contains(&r.name)
        });
        // One gix commit_info per ref (cheap) BEFORE rank/cap so the
        // suggested comparator never re-opens the same commit.
        let mut pre: Vec<BranchPre> = locals
            .into_iter()
            .chain(remotes)
            .map(|r| {
                let last = git.commit_info(&r.target_sha).ok().map(|c| BranchLast {
                    // `CommitInfo::title` is gix's raw, lossless
                    // `MessageRef::title` — for a single-line message (no
                    // blank-line body separator) that's the WHOLE message
                    // verbatim, trailing newline included (see
                    // `git::commit`'s own module doc/tests). Every other
                    // `subject` field in this crate's wire responses
                    // (`CommitMeta::subject`, `CommitSummary::subject`)
                    // comes from git's `%s` format placeholder instead,
                    // which never carries one — trimmed here so
                    // `branches/1`'s `last.subject` matches that same
                    // convention rather than leaking gix's own internal
                    // representation.
                    subject: c.title.trim_end().to_string(),
                    author_time: c.author_time_unix,
                });
                let has_open_review = open_heads
                    .iter()
                    .any(|h| head_ref_matches(h, &r.full_name, &r.name));
                BranchPre {
                    name: r.name,
                    revspec: r.full_name,
                    remote: r.remote,
                    target_sha: r.target_sha,
                    is_head: r.is_head,
                    last,
                    has_open_review,
                }
            })
            .collect();

        let now = now_unix();
        // The FULL pre-cap count — what `truncated` reports below — must
        // be captured before EITHER branch's truncation, regardless of how
        // wide `Suggested`'s two-phase widening (below) goes.
        let total_before_cap = pre.len();
        match sort {
            BranchSort::Name => {
                pre.sort_by(|a, b| {
                    branch_name_key(&a.name, a.remote.as_deref())
                        .cmp(&branch_name_key(&b.name, b.remote.as_deref()))
                });
                pre.truncate(crate::history::branches::MAX_BRANCHES);
            }
            BranchSort::Suggested => {
                // Cheap rank (gix last-commit + store open-review) BEFORE
                // the cap. rev-list / ladder run only for survivors.
                pre.sort_by(|a, b| {
                    let sa = crate::history::branches::suggest(
                        &crate::history::branches::SuggestInput {
                            now_unix: now,
                            author_time: a.last.as_ref().map(|l| l.author_time),
                            has_open_review: a.has_open_review,
                            attribution: None,
                            ahead: None,
                            behind: None,
                        },
                    );
                    let sb = crate::history::branches::suggest(
                        &crate::history::branches::SuggestInput {
                            now_unix: now,
                            author_time: b.last.as_ref().map(|l| l.author_time),
                            has_open_review: b.has_open_review,
                            attribution: None,
                            ahead: None,
                            behind: None,
                        },
                    );
                    sb.score.total_cmp(&sa.score).then_with(|| {
                        branch_name_key(&a.name, a.remote.as_deref())
                            .cmp(&branch_name_key(&b.name, b.remote.as_deref()))
                    })
                });
                // V70-A3X two-phase widening: keep the top 3×MAX_BRANCHES
                // (not MAX_BRANCHES itself) surviving the CHEAP rank, so a
                // branch that's merely average on recency/open-review but
                // genuinely strong on the EXPENSIVE terms (join-ladder
                // attribution, ahead/behind) still has room to surface once
                // the full score is computed below — rather than being
                // permanently cut before those terms were ever measured.
                // The expensive per-branch loop (rev-list + the ladder)
                // now costs up to 3× what it used to for a large repo; the
                // FINAL response still truncates back to MAX_BRANCHES
                // after the full re-rank (see below).
                pre.truncate(3 * crate::history::branches::MAX_BRANCHES);
            }
        }
        let truncated = total_before_cap > crate::history::branches::MAX_BRANCHES;
        (default, truncated, pre)
    };

    // Prefer the default branch's full ref as the rev-list left side so a
    // remote-only origin/HEAD target still resolves.
    let default_revspec = pre
        .iter()
        .find(|p| default.as_deref() == Some(p.name.as_str()))
        .map(|p| p.revspec.clone())
        .or_else(|| default.clone());

    let mut branches = Vec::with_capacity(pre.len());
    for p in pre {
        let is_default = default.as_deref() == Some(p.name.as_str());
        // V70-A3X: `Some` for a genuinely MEASURED value (definitionally
        // zero for the default branch itself, or an actual `rev-list`
        // count), `None` when there was nothing to compare against at all
        // — see `BranchOut::ahead`'s doc.
        let (ahead, behind) = if is_default {
            (Some(0), Some(0))
        } else if let Some(default_spec) = default_revspec.as_deref() {
            let repo_root = repo.path.clone();
            // V70-A2 (SEC-17) — both names come from ref ENUMERATION, but
            // git permits a ref whose name starts with `-`; they go
            // through the same `Revspec` constructor a caller-supplied one
            // does.
            match (Revspec::parse(default_spec), Revspec::parse(&p.revspec)) {
                (Ok(default_owned), Ok(branch_owned)) => {
                    // SEC-15 — one permit per git child in this per-branch
                    // loop (the second fan-out lane after merge-check).
                    let permit = state
                        .git_fanout
                        .clone()
                        .acquire_owned()
                        .await
                        .map_err(|e| {
                            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
                        })?;
                    let ab = tokio::task::spawn_blocking(move || {
                        let _permit = permit;
                        crate::history::branches::ahead_behind(
                            &repo_root,
                            &RefRange::new(default_owned, branch_owned, true),
                        )
                    })
                    .await
                    .map_err(|e| {
                        ApiError::new(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("ahead/behind task panicked: {e}"),
                        )
                    })??;
                    (Some(ab.ahead), Some(ab.behind))
                }
                // A ref this daemon cannot validate has nothing MEASURED to
                // report: `None`, never a measured zero (V70-A3X's
                // `BranchOut::ahead` contract). The whole listing still
                // succeeds — the same drop-on-error posture as the rest of
                // this loop.
                _ => (None, None),
            }
        } else {
            // Detached/unborn HEAD and no origin/HEAD — nothing to compare against.
            (None, None)
        };

        let attribution = crate::join::ladder::resolve_commit(
            repo,
            repo_id,
            &p.target_sha,
            &state.store,
            &state.kb_client,
        )
        .await;

        branches.push(BranchOut {
            name: p.name,
            target_sha: p.target_sha,
            is_head: p.is_head,
            ahead,
            behind,
            last: p.last,
            attribution,
            remote: p.remote,
            has_open_review: p.has_open_review,
            suggest: None,
        });
    }

    if sort == BranchSort::Suggested {
        let now = now_unix();
        for b in &mut branches {
            b.suggest = Some(crate::history::branches::suggest(
                &crate::history::branches::SuggestInput {
                    now_unix: now,
                    author_time: b.last.as_ref().map(|l| l.author_time),
                    has_open_review: b.has_open_review,
                    attribution: attribution_suggest_weight(b.attribution.confidence),
                    ahead: b.ahead,
                    behind: b.behind,
                },
            ));
        }
        branches.sort_by(|a, b| {
            let sa = a.suggest.as_ref().map(|s| s.score).unwrap_or(0.0);
            let sb = b.suggest.as_ref().map(|s| s.score).unwrap_or(0.0);
            sb.total_cmp(&sa).then_with(|| {
                branch_name_key(&a.name, a.remote.as_deref())
                    .cmp(&branch_name_key(&b.name, b.remote.as_deref()))
            })
        });
        // V70-A3X: the two-phase widening above (`pre` truncated to
        // 3×MAX_BRANCHES, not MAX_BRANCHES, before the expensive terms
        // ran) means `branches` can be wider than MAX_BRANCHES here — cut
        // to the final MAX_BRANCHES AFTER the full-score re-rank, so the
        // response still respects its documented cap.
        branches.truncate(crate::history::branches::MAX_BRANCHES);
    }

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(BranchesResponse {
            schema: "branches/1",
            repo: params.repo,
            default,
            branches,
            truncated,
        }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct FileHistoryParams {
    pub repo: String,
    pub path: String,
    pub limit: Option<usize>,
    /// Unix seconds — commits authored after this are excluded.
    pub before: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct FileHistoryResponse {
    pub schema: &'static str,
    pub repo: String,
    pub path: String,
    pub entries: Vec<crate::history::CommitSummary>,
    pub truncated: bool,
}

/// `GET /api/file-history?repo=&path=&limit=&before=` (Phase C4) — one
/// file's history, newest-first (`crate::history::file_history`'s own doc:
/// `git log --follow`). `path` reuses `safe_rel_path`'s traversal gate
/// (same as `tree`/`file`/`diff` above); `limit` defaults to
/// `history::file_history::DEFAULT_LIMIT` (100), clamped to
/// `[1, MAX_LIMIT]` (500) so an operator-typo'd `limit` can't turn this
/// into an unbounded full-history walk.
pub async fn file_history_route(
    State(state): State<SharedState>,
    Query(params): Query<FileHistoryParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &params.repo)?;
    let path = safe_rel_path(&params.path)?.to_string();
    let limit = params
        .limit
        .unwrap_or(crate::history::file_history::DEFAULT_LIMIT)
        .clamp(1, crate::history::file_history::MAX_LIMIT);

    let repo_root = repo.path.clone();
    let path_for_task = path.clone();
    let before = params.before;
    let (entries, truncated) = tokio::task::spawn_blocking(move || {
        crate::history::file_history::file_history(&repo_root, &path_for_task, limit, before)
    })
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("file-history task panicked: {e}"),
        )
    })??;

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(FileHistoryResponse {
            schema: "file-history/1",
            repo: params.repo,
            path,
            entries,
            truncated,
        }),
    ))
}

// --- GET /api/stacks, GET /api/stacks/layer-diff (V3.3-S2) --------------

#[derive(Debug, Deserialize)]
pub struct StacksParams {
    pub repo: String,
    /// When true, include single-layer stacks (branches based on the
    /// default with no dependents). Default listing hides them so the
    /// surface highlights real multi-layer stacks.
    #[serde(default)]
    pub all: bool,
}

#[derive(Debug, Serialize)]
pub struct StacksResponse {
    pub schema: &'static str,
    pub repo: String,
    pub default_branch: Option<String>,
    pub stacks: Vec<crate::history::stacks::Stack>,
    /// True when the repo has more local branches than
    /// [`crate::history::branches::MAX_BRANCHES`] — detection ran over the
    /// lex-first cap only (never silent truncation).
    pub truncated: bool,
}

/// Branch-tip enumeration shared by the two stacks routes, CAPPED at
/// `branches::MAX_BRANCHES` (same ceiling as `/api/branches`). Detection
/// is O(branches²) `git merge-base` spawns — an uncapped 500-branch repo
/// would turn one GET into tens of thousands of subprocesses. Sorted by
/// name BEFORE the cap so the surviving set is deterministic.
fn stack_branch_tips(
    repo_path: &std::path::Path,
) -> Result<(Option<String>, Vec<crate::history::stacks::BranchTip>, bool), ApiError> {
    let git = GitRepo::open(repo_path)?;
    let default = git.head_info()?.branch;
    let mut tips: Vec<crate::history::stacks::BranchTip> = git
        .list_refs()?
        .into_iter()
        .filter(|r| r.kind == RefKind::Branch)
        .map(|r| crate::history::stacks::BranchTip {
            name: r.name,
            tip_sha: r.target_sha,
        })
        .collect();
    tips.sort_by(|a, b| a.name.cmp(&b.name));
    let truncated = tips.len() > crate::history::branches::MAX_BRANCHES;
    tips.truncate(crate::history::branches::MAX_BRANCHES);
    Ok((default, tips, truncated))
}

/// `GET /api/stacks?repo=&all=` (V3.3-S2) — dependent-branch stack
/// detection. Pure derivation over local refs (`history::stacks`); never
/// writes. `all=1` includes single-layer stacks; default excludes them.
/// Same `HistoryError` → `ApiError` mapping as `/branches`/`/compare`.
pub async fn stacks_route(
    State(state): State<SharedState>,
    Query(params): Query<StacksParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &params.repo)?;
    let include_all = params.all;
    // `GitRepo` is `!Send` — enumeration happens inside the helper before
    // any `.await` (same discipline as `branches_route`), capped.
    let (default, branch_tips, truncated) = stack_branch_tips(&repo.path)?;
    let repo_root = repo.path.clone();
    let default_for_task = default.clone();
    let detected = tokio::task::spawn_blocking(move || {
        crate::history::stacks::detect_stacks(
            &repo_root,
            &branch_tips,
            default_for_task.as_deref(),
            include_all,
        )
    })
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("stacks task panicked: {e}"),
        )
    })??;

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(StacksResponse {
            schema: "stacks/1",
            repo: params.repo,
            default_branch: detected.default_branch,
            stacks: detected.stacks,
            truncated,
        }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct StacksLayerDiffParams {
    pub repo: String,
    pub branch: String,
}

#[derive(Debug, Serialize)]
pub struct StacksLayerDiffResponse {
    pub schema: &'static str,
    pub repo: String,
    pub branch: String,
    pub base: String,
    pub base_tip: String,
    pub tip: String,
    pub stale: bool,
    /// Flattened compare payload (same fields as `compare/1`'s body) so
    /// the SPA can reuse the compare file/numstat renderer.
    pub resolved: crate::history::Resolved,
    pub commits: Vec<crate::history::CommitSummary>,
    pub commits_truncated: bool,
    pub files: Vec<crate::numstat::FileChange>,
    pub totals: crate::history::FileTotals,
    /// True when the branch enumeration hit `branches::MAX_BRANCHES` —
    /// base detection saw only the lex-first cap (a missing branch may
    /// be a truncation casualty, not a detection failure).
    pub truncated: bool,
}

/// `GET /api/stacks/layer-diff?repo=&branch=` (V3.3-S2) — the INCREMENTAL
/// diff of one stack layer (`base_tip..branch_tip`). Reuses
/// `history::compare` (two-dot) for the file/commit lists. When the layer
/// is stale, still diffs against the recorded base tip (honest incremental
/// view) and echoes `stale: true`.
pub async fn stacks_layer_diff_route(
    State(state): State<SharedState>,
    Query(params): Query<StacksLayerDiffParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &params.repo)?;
    let (default, branch_tips, truncated) = stack_branch_tips(&repo.path)?;
    let repo_root = repo.path.clone();
    let default_for_task = default.clone();
    let branch = parse_revspec(&params.branch)?;
    let ld = tokio::task::spawn_blocking(move || {
        crate::history::stacks::layer_diff(
            &repo_root,
            &branch_tips,
            default_for_task.as_deref(),
            &branch,
        )
    })
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("stacks layer-diff task panicked: {e}"),
        )
    })??;

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(StacksLayerDiffResponse {
            schema: "stacks-layer-diff/1",
            repo: params.repo,
            branch: ld.branch,
            base: ld.base,
            base_tip: ld.base_tip,
            tip: ld.tip,
            stale: ld.stale,
            resolved: ld.compare.resolved,
            commits: ld.compare.commits,
            commits_truncated: ld.compare.commits_truncated,
            files: ld.compare.files,
            totals: ld.compare.totals,
            truncated,
        }),
    ))
}

// --- GET /api/merge-check (Phase G-server) -------------------------------

#[derive(Debug, Deserialize)]
pub struct MergeCheckParams {
    pub repo: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Serialize)]
pub struct MergeCheckResponse {
    pub schema: &'static str,
    pub repo: String,
    pub from: String,
    pub to: String,
    pub resolved: crate::history::Resolved,
    pub clean: bool,
    pub conflicts: Vec<crate::history::merge_check::ConflictEntry>,
    pub ahead: u32,
    pub behind: u32,
}

/// `GET /api/merge-check?repo=&from=&to=` (Phase G-server) — dry-run merge
/// readiness: `crate::history::merge_check`'s own module doc has the full
/// `git merge-tree --write-tree`/exit-code-ambiguity contract. Same
/// `HistoryError`-to-`ApiError` mapping as `compare`/`commit` above (a
/// dash-prefixed or unresolvable `from`/`to` is a `400`, never a `500`);
/// the merge-tree subprocess is genuinely blocking, run inside
/// `spawn_blocking` like every other subprocess-backed route.
pub async fn merge_check_route(
    State(state): State<SharedState>,
    Query(params): Query<MergeCheckParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &params.repo)?;
    let repo_root = repo.path.clone();
    let from = parse_revspec(&params.from)?;
    let to = parse_revspec(&params.to)?;
    let scratch_root = state.scratch_root.clone();
    // V70-A2 (SEC-15) — merge-check is a git FAN-OUT lane on a documented
    // IO-bound host, so it takes a permit from the daemon-wide semaphore
    // before spawning its child. Acquired here (async) rather than inside
    // the blocking closure so a queued request parks on the semaphore, not
    // on a blocking-pool thread.
    let _permit = state
        .git_fanout
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mc = tokio::task::spawn_blocking(move || {
        crate::history::merge_check::merge_check(&repo_root, &scratch_root, &from, &to)
    })
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("merge-check task panicked: {e}"),
        )
    })??;

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(MergeCheckResponse {
            schema: "merge-check/1",
            repo: params.repo,
            from: params.from,
            to: params.to,
            resolved: mc.resolved,
            clean: mc.clean,
            conflicts: mc.conflicts,
            ahead: mc.ahead,
            behind: mc.behind,
        }),
    ))
}

// --- GET /api/range-diff (Phase G-server) --------------------------------

#[derive(Debug, Deserialize)]
pub struct RangeDiffParams {
    pub repo: String,
    pub old: String,
    pub new: String,
}

#[derive(Debug, Serialize)]
pub struct RangeDiffResponse {
    pub schema: &'static str,
    pub repo: String,
    pub old: String,
    pub new: String,
    pub pairs: Vec<crate::history::range_diff::RangeDiffPair>,
    pub truncated: bool,
}

/// `GET /api/range-diff?repo=&old=&new=` (Phase G-server) — wraps `git
/// range-diff --no-color <old> <new>` (`old`/`new` are RANGE strings, e.g.
/// `main..topic@{1}` vs `main..topic`); `crate::history::range_diff`'s own
/// module doc has the summary-line grammar + why a `modified` pair only
/// ever carries `old_subject`. `400` on a dash-prefixed range or an
/// unresolvable one (`GitFailed`) — same mapping as every other history
/// route above.
pub async fn range_diff_route(
    State(state): State<SharedState>,
    Query(params): Query<RangeDiffParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &params.repo)?;
    let repo_root = repo.path.clone();
    let old = parse_ref_range(&params.old)?;
    let new = parse_ref_range(&params.new)?;
    let rd = tokio::task::spawn_blocking(move || {
        crate::history::range_diff::range_diff(&repo_root, &old, &new)
    })
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("range-diff task panicked: {e}"),
        )
    })??;

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(RangeDiffResponse {
            schema: "range-diff/1",
            repo: params.repo,
            old: params.old,
            new: params.new,
            pairs: rd.pairs,
            truncated: rd.truncated,
        }),
    ))
}

// --- GET /api/repo-state (Phase G-server) --------------------------------

impl From<crate::repo_state::RepoStateError> for ApiError {
    fn from(e: crate::repo_state::RepoStateError) -> Self {
        use crate::repo_state::RepoStateError::*;
        match e {
            Spawn(err) => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("spawn git: {err}"),
            ),
            // Neither of `repo_state`'s two subprocess calls (`git status
            // --porcelain`, `git diff --name-only --diff-filter=U`) takes
            // any caller-supplied argument — a failure here is a
            // daemon-side fault (a corrupt repo), never a caller mistake.
            GitFailed { stderr, .. } => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, stderr),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct RepoStateParams {
    pub repo: String,
}

#[derive(Debug, Serialize)]
pub struct RepoStateResponse {
    pub schema: &'static str,
    pub repo: String,
    pub op: crate::mirror::RepoOp,
    pub detail: crate::mirror::OpDetail,
    pub conflicted: Vec<String>,
    pub dirty: bool,
}

/// `GET /api/repo-state?repo=` (Phase G-server) — this repo's CURRENT git
/// operation state: `crate::repo_state`'s own module doc has the full
/// contract (it reuses `mirror::gate::detect_op`, never a second copy of
/// the marker list). `GitRepo::open`/`git_dir()` are cheap, non-blocking
/// reads (same "open fresh per request" precedent as `routes::repos`'s own
/// `watcher_state` helper) done OUTSIDE `spawn_blocking` — only the two
/// actual subprocess calls (`repo_state::repo_state`'s own git status/diff)
/// need it.
pub async fn repo_state_route(
    State(state): State<SharedState>,
    Query(params): Query<RepoStateParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &params.repo)?;
    // `GitRepo` (gix) is `!Send` — opened and read entirely inside a scoped
    // block that ends before the first `.await`, same discipline
    // `branches_route` uses (see that fn's own doc).
    let git_dir = {
        let git = GitRepo::open(&repo.path)?;
        git.git_dir().to_path_buf()
    };
    let repo_root = repo.path.clone();

    let rs =
        tokio::task::spawn_blocking(move || crate::repo_state::repo_state(&repo_root, &git_dir))
            .await
            .map_err(|e| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("repo-state task panicked: {e}"),
                )
            })??;

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(RepoStateResponse {
            schema: "repo-state/1",
            repo: params.repo,
            op: rs.op,
            detail: rs.detail,
            conflicted: rs.conflicted,
            dirty: rs.dirty,
        }),
    ))
}

// --- GET /api/prs, GET /api/prs/{number}/comments, POST /api/prs/fetch
//     (Phase G-server — the GitHub read overlay) -----------------------------

impl From<crate::github::GithubError> for ApiError {
    fn from(e: crate::github::GithubError) -> Self {
        use crate::github::GithubError::*;
        match e {
            Spawn(err) => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("spawn git: {err}"),
            ),
            // A repo with no `origin` remote, or an `origin` that isn't a
            // GitHub host — a clean caller-facing 400 ("not a github
            // origin"), never a panic, per the phase brief.
            NoOrigin(_) | NotGithubOrigin(_) => ApiError::bad_request("not a github origin"),
            FetchFailed(stderr) => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, stderr),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct PrsParams {
    pub repo: String,
}

#[derive(Debug, Serialize)]
pub struct PrsResponse {
    pub schema: &'static str,
    pub prs: Vec<crate::github::PrOut>,
    pub truncated: bool,
    /// Present ONLY when the GitHub call itself failed (rate-limited,
    /// unreachable, a bad response, ...) — `prs` is then always empty and
    /// the HTTP status is still 200, never a 5xx for "GitHub had a bad
    /// day" (mirrors `search::sessions`'s own degrade-not-fail posture).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

/// `GET /api/prs?repo=` (Phase G-server) — every open PR on `repo`'s
/// GitHub origin (`crate::github::GithubClient::list_pulls`). `400`
/// ("not a github origin") when `repo`'s `origin` isn't a GitHub remote —
/// resolved BEFORE any network call. Every OTHER failure (rate-limited,
/// unreachable, a bad response) degrades to `unavailable_reason`, HTTP 200,
/// `prs: []` — never an error response.
pub async fn prs_route(
    State(state): State<SharedState>,
    Query(params): Query<PrsParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &params.repo)?;
    let repo_root = repo.path.clone();
    let gh_repo = tokio::task::spawn_blocking(move || crate::github::github_repo(&repo_root))
        .await
        .map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("origin lookup task panicked: {e}"),
            )
        })??;

    let (prs, truncated, unavailable_reason) =
        match state.github.list_pulls(&gh_repo.owner, &gh_repo.name).await {
            Ok(mut list) => {
                let truncated = list.len() > crate::github::MAX_PRS;
                list.truncate(crate::github::MAX_PRS);
                (list, truncated, None)
            }
            Err(e) => (Vec::new(), false, Some(e.to_string())),
        };

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(PrsResponse {
            schema: "prs/1",
            prs,
            truncated,
            unavailable_reason,
        }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct PrCommentsParams {
    pub repo: String,
}

#[derive(Debug, Serialize)]
pub struct PrCommentsResponse {
    pub schema: &'static str,
    pub comments: Vec<crate::github::PrCommentOut>,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

/// `GET /api/prs/{number}/comments?repo=` (Phase G-server) — `number`'s
/// review comments (inline code comments) merged with its issue comments
/// (general PR discussion) — `crate::github::GithubClient::
/// list_pull_comments`'s own doc has the merge order + the `path`-presence
/// distinction. Same 400-vs-degrade split as `prs_route` above.
pub async fn pr_comments_route(
    State(state): State<SharedState>,
    axum::extract::Path(number): axum::extract::Path<u64>,
    Query(params): Query<PrCommentsParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &params.repo)?;
    let repo_root = repo.path.clone();
    let gh_repo = tokio::task::spawn_blocking(move || crate::github::github_repo(&repo_root))
        .await
        .map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("origin lookup task panicked: {e}"),
            )
        })??;

    // V70-A3X: `list_pull_comments` now itself enforces `MAX_COMMENTS`
    // (paginating via `Link: rel="next"` up to that shared budget) and
    // reports honestly whether more existed — no further truncation needed
    // here.
    let (comments, truncated, unavailable_reason) = match state
        .github
        .list_pull_comments(&gh_repo.owner, &gh_repo.name, number)
        .await
    {
        Ok((list, truncated)) => (list, truncated, None),
        Err(e) => (Vec::new(), false, Some(e.to_string())),
    };

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(PrCommentsResponse {
            schema: "pr-comments/1",
            comments,
            truncated,
            unavailable_reason,
        }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct PrDetailParams {
    pub repo: String,
}

#[derive(Debug, Serialize)]
pub struct PrDetailResponse {
    pub schema: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<crate::github::PrDetailOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

/// `GET /api/prs/{number}?repo=` (PRR-R2, design doc §2 row 2) — one PR's
/// full detail via `GithubClient::get_pull` (works for open/closed/merged,
/// unlike [`prs_route`]'s `state=open`-only list). Same 400-vs-degrade
/// split as `prs_route`/`pr_comments_route`: a non-GitHub origin 400s
/// before any network call; every OTHER failure degrades to
/// `unavailable_reason`, HTTP 200, `pr: null`.
pub async fn pr_route(
    State(state): State<SharedState>,
    axum::extract::Path(number): axum::extract::Path<u64>,
    Query(params): Query<PrDetailParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &params.repo)?;
    let repo_root = repo.path.clone();
    let gh_repo = tokio::task::spawn_blocking(move || crate::github::github_repo(&repo_root))
        .await
        .map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("origin lookup task panicked: {e}"),
            )
        })??;

    let (pr, unavailable_reason) = match state
        .github
        .get_pull(&gh_repo.owner, &gh_repo.name, number)
        .await
    {
        Ok(pr) => (Some(pr), None),
        Err(e) => (None, Some(e.to_string())),
    };

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(PrDetailResponse {
            schema: "pr-detail/1",
            pr,
            unavailable_reason,
        }),
    ))
}

#[derive(Debug, Serialize)]
pub struct PrChecksResponse {
    pub schema: &'static str,
    pub checks: Vec<crate::github::CheckRunOut>,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

/// `GET /api/prs/{number}/checks?repo=` (PRR-R2, design doc §2 row 3) —
/// every check-run against `number`'s CURRENT head sha (resolved via a
/// `get_pull` call first — GitHub's checks API is keyed by commit sha, not
/// PR number). `MAX_CHECKS` cap. Same 400-vs-degrade split as the other PR
/// routes: `get_pull` failing degrades exactly like a `list_checks`
/// failure would (never a distinct error shape for "couldn't even find the
/// head sha").
pub async fn pr_checks_route(
    State(state): State<SharedState>,
    axum::extract::Path(number): axum::extract::Path<u64>,
    Query(params): Query<PrDetailParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &params.repo)?;
    let repo_root = repo.path.clone();
    let gh_repo = tokio::task::spawn_blocking(move || crate::github::github_repo(&repo_root))
        .await
        .map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("origin lookup task panicked: {e}"),
            )
        })??;

    let (checks, truncated, unavailable_reason) = match state
        .github
        .get_pull(&gh_repo.owner, &gh_repo.name, number)
        .await
    {
        Ok(pr) => {
            match state
                .github
                .list_checks(&gh_repo.owner, &gh_repo.name, &pr.head_sha)
                .await
            {
                Ok(mut list) => {
                    let truncated = list.len() > crate::github::MAX_CHECKS;
                    list.truncate(crate::github::MAX_CHECKS);
                    (list, truncated, None)
                }
                Err(e) => (Vec::new(), false, Some(e.to_string())),
            }
        }
        Err(e) => (Vec::new(), false, Some(e.to_string())),
    };

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(PrChecksResponse {
            schema: "pr-checks/1",
            checks,
            truncated,
            unavailable_reason,
        }),
    ))
}

#[derive(Debug, Serialize)]
pub struct PrReviewsResponse {
    pub schema: &'static str,
    pub reviewers: Vec<crate::github::ReviewerStateOut>,
    pub requested_reviewers: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

/// `GET /api/prs/{number}/reviews?repo=` (PRR addendum-2 §A) — per-reviewer
/// latest state + requested reviewers + a locally-computed
/// `review_decision` approximation, via `GithubClient::list_reviews`. Raw
/// read only — the `/github-threads` composition route that anchors these
/// into the diff is a LATER unit. Same 400-vs-degrade split as the other
/// PR routes.
pub async fn pr_reviews_route(
    State(state): State<SharedState>,
    axum::extract::Path(number): axum::extract::Path<u64>,
    Query(params): Query<PrDetailParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &params.repo)?;
    let repo_root = repo.path.clone();
    let gh_repo = tokio::task::spawn_blocking(move || crate::github::github_repo(&repo_root))
        .await
        .map_err(|e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("origin lookup task panicked: {e}"),
            )
        })??;

    let (reviewers, requested_reviewers, review_decision, unavailable_reason) = match state
        .github
        .list_reviews(&gh_repo.owner, &gh_repo.name, number)
        .await
    {
        Ok(out) => (
            out.reviewers,
            out.requested_reviewers,
            out.review_decision,
            None,
        ),
        Err(e) => (Vec::new(), Vec::new(), None, Some(e.to_string())),
    };

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(PrReviewsResponse {
            schema: "pr-reviews/1",
            reviewers,
            requested_reviewers,
            review_decision,
            unavailable_reason,
        }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct PrFetchBody {
    pub repo: String,
    /// `u32` end-to-end — see `github::fetch_pr_ref`'s doc for why this
    /// type choice IS the injection guard (a `u32` can never deserialize
    /// from anything but a non-negative integer, so no revspec/refspec
    /// metacharacter can ever reach the refspec this builds).
    pub number: u32,
}

#[derive(Debug, Serialize)]
pub struct PrFetchResponse {
    pub repo: String,
    pub number: u32,
    #[serde(rename = "ref")]
    pub ref_: String,
    pub sha: String,
}

/// `POST /api/prs/fetch` (Phase G-server) — body `{repo, number}`, runs
/// `git fetch origin +refs/pull/<n>/head:refs/kbc/pr/<n>`
/// (`crate::github::fetch_pr_ref`'s own doc names `refs/kbc/*` as the
/// operator-approved namespace). Mounted on the LOOPBACK-ONLY sub-router
/// (`router.rs`, the `checkout`/`session-diff` family) — the ONE new git
/// ref-write this daemon performs outside `checkout::switch_repo`, so it
/// gets that same stronger gate. Deliberately does NOT gate on
/// `github::github_repo` (unlike `prs_route`/`pr_comments_route`, which
/// need a GitHub owner/name to call the REST API): this route's actual
/// work is a plain `git fetch` against whatever `origin` the repo already
/// has configured — `refs/pull/<n>/head` happens to be GitHub's own PR-ref
/// convention, but the fetch mechanics themselves are host-agnostic, and
/// a caller who names a PR number implicitly already knows `origin` is a
/// GitHub remote. A missing/unreachable `origin` still fails cleanly via
/// `git fetch`'s own ordinary error path (`GithubError::FetchFailed`, a
/// 500 — a daemon-/network-side fault, not a caller mistake).
pub async fn prs_fetch_route(
    State(state): State<SharedState>,
    Json(body): Json<PrFetchBody>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &body.repo)?;
    let repo_root = repo.path.clone();
    let number = body.number;
    let (ref_, sha) =
        tokio::task::spawn_blocking(move || crate::github::fetch_pr_ref(&repo_root, number))
            .await
            .map_err(|e| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("pr fetch task panicked: {e}"),
                )
            })??;

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(PrFetchResponse {
            repo: body.repo,
            number,
            ref_,
            sha,
        }),
    ))
}

// --- GET /api/events (SSE) ----------------------------------------------

const SSE_KEEP_ALIVE: Duration = Duration::from_secs(15);

#[derive(Debug, Deserialize, Default)]
pub struct EventsParams {
    /// Resume cursor for clients that can't set the `Last-Event-ID` header
    /// (mirrors kb-server's own `routes/events.rs` param of the same name
    /// and rationale).
    pub last_event_id: Option<u64>,
}

/// `GET /api/events` — SSE firehose over `state.bus` (`kb_core::events`,
/// reused wholesale per the design brief). No `?types=`/`?filter=` glob
/// filtering yet (kb-server's own `routes/events.rs` has that; kb-code's
/// event vocabulary — see `crate::schema::V1_TYPES`, introspectable via
/// `GET /api/events.schema.json` — is still small enough that a filter DSL
/// isn't worth it yet).
pub async fn events(
    State(state): State<SharedState>,
    Query(params): Query<EventsParams>,
    headers: axum::http::HeaderMap,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let last_id: u64 = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .or(params.last_event_id)
        .unwrap_or(0);

    let sse_stream =
        events_stream(&state.bus, last_id).map(|frame| Ok::<_, Infallible>(frame_to_sse(frame)));

    Sse::new(sse_stream).keep_alive(
        KeepAlive::new()
            .interval(SSE_KEEP_ALIVE)
            .text(":keep-alive"),
    )
}

// --- Phase N — todos + scopes ------------------------------------------

/// Default page size for `GET /api/todos` when `?limit=` is omitted.
pub const TODOS_DEFAULT_LIMIT: usize = 500;
/// Hard cap on `?limit=` for todos (same spirit as other list clamps).
pub const TODOS_MAX_LIMIT: usize = 5_000;

#[derive(Debug, Deserialize)]
pub struct ListTodosParams {
    pub repo: String,
    #[serde(default)]
    pub marker: Option<String>,
    #[serde(default)]
    pub path_prefix: Option<String>,
    /// Scope name to include, or `!<name>` to exclude. Unknown name → 404.
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct TodoItemOut {
    pub path: String,
    pub line: i64,
    pub marker: String,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct TodosListOut {
    pub items: Vec<TodoItemOut>,
    pub total: usize,
    pub truncated: bool,
}

/// `GET /api/todos?repo=[&marker=][&path_prefix=][&scope=][&limit=]` —
/// ordered by path, line. Default limit 500. `scope=<name>` keeps paths
/// matching the scope's globs; `scope=!<name>` drops them. Unknown scope
/// name → 404.
///
/// **V72-J1: this is now a FILTERED VIEW over `comments/1`**
/// (`Store::list_todo_items` reads the `comments` table; `todo_items` and
/// `extract::extract_todos` are gone — there is exactly one scanner over
/// these lines). The response STRUCT, its field names, the ordering, the
/// limit/truncation semantics and the scope filter are byte-identical.
/// The ROW SET changes in exactly two enumerated ways, both pinned by
/// tests in `tests/http_pack/todos_route.rs`:
///
/// 1. **Outline-tier languages are now scanned.** `extract_todos` was
///    gated to the eight token-level languages, so a `# TODO` in a YAML
///    or TOML comment was structurally invisible. `comments/1` walks
///    every grammar with comment nodes, so those markers now appear.
/// 2. **A two-marker line reports the LEFTMOST keyword.** The deleted
///    `find_todo_marker` iterated its hardcoded marker array in the outer
///    loop, so `// TODO: drop this FIXME shim` reported `FIXME`.
///
/// The keyword vocabulary this view reports is fixed at
/// `comments::keywords::TODO_FAMILY` (the five markers the old index
/// scanned) — a repo whose `comments/1` set also finds `OPTIMIZE`/
/// `REVIEW`/`NOTE` does not leak them into this route. An operator who
/// REPLACES the set via `[comments] keywords` narrows this view along
/// with it, which is the honest consequence of one scanner.
pub async fn list_todos(
    State(state): State<SharedState>,
    Query(params): Query<ListTodosParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (_repo, repo_id) = find_repo(&state, &params.repo)?;
    let limit = params
        .limit
        .unwrap_or(TODOS_DEFAULT_LIMIT)
        .clamp(1, TODOS_MAX_LIMIT);

    // Resolve scope filter before the store query so an unknown name 404s
    // without a wasted scan.
    let scope_filter: Option<(bool, &[String])> = match params.scope.as_deref() {
        None => None,
        Some(raw) => {
            let (exclude, name) = if let Some(n) = raw.strip_prefix('!') {
                (true, n)
            } else {
                (false, raw)
            };
            if name.is_empty() {
                return Err(ApiError::bad_request(
                    "scope must be a name or !<name>, not empty",
                ));
            }
            let patterns = state
                .scopes
                .get(name)
                .ok_or_else(|| ApiError::not_found(format!("unknown scope {name:?}")))?;
            Some((exclude, patterns))
        }
    };

    // 2026-08-31 incident (store.rs module doc): single store call, still
    // wrapped so it can never park this async worker.
    let marker = params.marker.clone();
    let path_prefix = params.path_prefix.clone();
    let rows = state
        .store
        .run_blocking(move |store| {
            store.list_todo_items(repo_id, marker.as_deref(), path_prefix.as_deref())
        })
        .await?;

    let filtered: Vec<_> = rows
        .into_iter()
        .filter(|r| match scope_filter {
            None => true,
            Some((exclude, patterns)) => {
                let matches = crate::scopes::path_matches_any(&r.path, patterns);
                if exclude {
                    !matches
                } else {
                    matches
                }
            }
        })
        .collect();

    let total = filtered.len();
    let truncated = total > limit;
    let items = filtered
        .into_iter()
        .take(limit)
        .map(|r| TodoItemOut {
            path: r.path,
            line: r.line,
            marker: r.marker,
            text: r.text,
        })
        .collect();

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(TodosListOut {
            items,
            total,
            truncated,
        }),
    ))
}

#[derive(Debug, Serialize)]
pub struct ScopesOut {
    pub scopes: std::collections::BTreeMap<String, Vec<String>>,
}

/// `GET /api/scopes` — the configured `[scopes]` map (name → patterns).
pub async fn list_scopes(State(state): State<SharedState>) -> Result<impl IntoResponse, ApiError> {
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(ScopesOut {
            scopes: state.scopes.map.clone(),
        }),
    ))
}

/// Mirrors kb-server's own `routes/events.rs::frame_to_sse` — same wire
/// shape (`{payload, ts, v}` body, `id:`/`event:` lines from the envelope),
/// duplicated rather than imported since kb-server doesn't expose it as a
/// reusable helper (it's a private fn in that route module).
fn frame_to_sse(frame: EventFrame) -> SseEvent {
    match frame {
        EventFrame::Envelope(env) => {
            let body = serde_json::json!({
                "payload": env.payload,
                "ts": env.ts.to_rfc3339(),
                "v": env.v,
            })
            .to_string();
            SseEvent::default()
                .id(env.id.to_string())
                .event(env.type_)
                .data(body)
        }
        EventFrame::Lag { skipped } => SseEvent::default()
            .event("lag")
            .data(serde_json::json!({ "skipped": skipped }).to_string()),
        EventFrame::Gap {
            requested_id,
            oldest_available_id,
        } => SseEvent::default().event("gap").data(
            serde_json::json!({
                "requested_id": requested_id,
                "oldest_available_id": oldest_available_id,
            })
            .to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_rel_path_accepts_ordinary_relative_paths() {
        assert!(safe_rel_path("src/lib.rs").is_ok());
        assert!(safe_rel_path("").is_ok());
        assert!(safe_rel_path("a/b/c.py").is_ok());
    }

    #[test]
    fn safe_rel_path_rejects_traversal_and_absolute_paths() {
        assert!(safe_rel_path("../etc/passwd").is_err());
        assert!(safe_rel_path("a/../../b").is_err());
        assert!(safe_rel_path("/etc/passwd").is_err());
        assert!(safe_rel_path("a/../b").is_err());
    }

    // ── V70-A5: the kbc-cmd/1 registry (`GET /api/commands`) ──────────────
    //
    // The DEEP checks (clap cross-join, reserved chords, shadowing, the
    // conflicts gate) live in `kb-code commands doctor`, which is wired to a
    // `#[test]` in kb-code-cli. What belongs HERE is the narrower promise
    // this crate makes: the bytes it compiled in are parseable JSON of the
    // declared schema, and the route's cache contract holds.

    fn registry() -> serde_json::Value {
        serde_json::from_str(COMMAND_REGISTRY_JSON).expect("registry.json is valid JSON")
    }

    #[test]
    fn command_registry_parses_and_declares_its_schema() {
        let r = registry();
        assert_eq!(r["schema"], "kbc-cmd/1");
        assert_eq!(r["leader"], "Space");
        let cmds = r["commands"].as_array().expect("commands is an array");
        assert!(
            cmds.len() > 100,
            "expected the full keymap, got {}",
            cmds.len()
        );
        for c in cmds {
            for field in [
                "id",
                "title",
                "group",
                "scope",
                "cli",
                "mutation",
                "lifecycle",
            ] {
                assert!(c[field].is_string(), "{field} missing on {:?}", c["id"]);
            }
            for preset in ["vim", "plain", "helix"] {
                assert!(
                    c["keys"][preset].is_array(),
                    "{preset} column missing on {:?} — presets are three explicit columns, never inheritance",
                    c["id"]
                );
            }
        }
    }

    #[test]
    fn command_registry_ids_are_unique_and_scopes_are_declared() {
        let r = registry();
        let declared: std::collections::HashSet<&str> = r["scopes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        let mut seen = std::collections::HashSet::new();
        for c in r["commands"].as_array().unwrap() {
            let id = c["id"].as_str().unwrap();
            assert!(seen.insert(id), "duplicate command id {id}");
            let scope = c["scope"].as_str().unwrap();
            assert!(
                declared.contains(scope),
                "{id} names undeclared scope {scope}"
            );
        }
    }

    #[test]
    fn command_registry_coactivity_is_symmetric() {
        // The conflicts report is only as honest as this matrix: if A lists B
        // as coactive but B omits A, one direction of every collision between
        // them silently disappears from the gate.
        let r = registry();
        let scopes = r["scopes"].as_array().unwrap();
        for a in scopes {
            let a_id = a["id"].as_str().unwrap();
            for b_id in a["coactive_with"].as_array().unwrap() {
                let b_id = b_id.as_str().unwrap();
                let b = scopes
                    .iter()
                    .find(|s| s["id"] == b_id)
                    .unwrap_or_else(|| panic!("{a_id} names unknown scope {b_id}"));
                let back: Vec<&str> = b["coactive_with"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap())
                    .collect();
                assert!(back.contains(&a_id), "{b_id} does not list {a_id} back");
            }
        }
    }

    #[test]
    fn command_registry_etag_is_a_stable_strong_validator() {
        let a = command_registry_etag();
        let b = command_registry_etag();
        assert_eq!(a, b);
        assert!(
            a.starts_with('"') && a.ends_with('"'),
            "not a quoted tag: {a}"
        );
        assert!(!a.starts_with("W/"), "must be strong, not weak: {a}");
    }

    #[tokio::test]
    async fn commands_route_serves_the_registry_and_honours_if_none_match() {
        use axum::http::HeaderMap;

        let res = commands(HeaderMap::new()).await;
        assert_eq!(res.status(), StatusCode::OK);
        let etag = res.headers()[header::ETAG].to_str().unwrap().to_string();
        assert_eq!(etag, command_registry_etag());

        for candidate in [
            etag.clone(),
            format!("W/{etag}"),
            "*".to_string(),
            format!("\"nope\", {etag}"),
        ] {
            let mut h = HeaderMap::new();
            h.insert(header::IF_NONE_MATCH, candidate.parse().unwrap());
            let res = commands(h).await;
            assert_eq!(res.status(), StatusCode::NOT_MODIFIED, "for {candidate}");
        }

        let mut stale = HeaderMap::new();
        stale.insert(
            header::IF_NONE_MATCH,
            "\"kbc-cmd-0000000000000000\"".parse().unwrap(),
        );
        assert_eq!(commands(stale).await.status(), StatusCode::OK);
    }
}
