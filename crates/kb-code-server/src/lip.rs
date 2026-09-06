//! `lip.rs` — PRR-L2: kb-code-server's CLIENT for the **lip/1** protocol
//! (design-lip.md's "kb-code-server integration" + design-addendum-2.md
//! §D's `/lip/diagnostics` amendment). `crates/kb-lip` (already on main,
//! PRR-L1) is the SERVER half — a standalone adapter binary that speaks
//! lip/1 over loopback HTTP in front of one real LSP process. THIS module
//! never links `kb-lip`; it only speaks its wire contract over `reqwest`
//! (`crates/kb-lip/src/http.rs` is the wire's spec of record — every
//! request/response shape here is deliberately a duplicate parse of that
//! contract, not a shared type, mirroring `crates/kb-lip/src/blob.rs`'s own
//! "kb-lip must not depend on kb-code-server" isolation in reverse).
//!
//! NOT to be confused with `crate::intel` (the resolve-ladder's own
//! cross-file/arity/access scoring module, unrelated) — this crate's
//! `[intel]` TOML section (`crate::config::IntelSection`) is this module's
//! config, a naming collision at the English-word level only.
//!
//! # Handshake — kb-sibling/1 discipline, scoped to lip/1
//!
//! [`LipClient`] performs ONE `GET {url}/lip/identity` handshake per
//! process per provider, lazily on first real use (never at boot — a
//! provider that happens to be down at daemon start must not delay or fail
//! kb-code's own boot, same posture `join::kb_client::KbClient`'s own
//! sibling handshake documents). The outcome is cached in a `OnceCell` via
//! `get_or_try_init`, which — same trick `KbClient::ensure_sibling` relies
//! on — caches ONLY an `Ok` closure result: a REACHED conclusion
//! (`lip_major == 1` ⇒ usable, `lip_major != 1` ⇒ **fail closed, sticky for
//! the process' lifetime**) latches, while anything that reaches no
//! conclusion at all (`Err(())`: connect failure, non-2xx, unparseable
//! body, or a response that simply omits `lip_major`) is NEVER cached —
//! the next real request retries the probe from scratch. Unlike
//! `kb_core::sibling`'s handshake, lip/1 has no legacy predecessor to
//! grandfather: there is no "absent Hello" special case here, only
//! "reached" (`Ok`/`Mismatch`) vs. "not yet reached" (`Err(())`, retried).
//!
//! # The blob-freshness re-check (the CRITICAL trust rule)
//!
//! kb-lip's OWN blob guard (`crates/kb-lip/src/blob.rs`) already refuses a
//! request whose `blob_sha` doesn't match the adapter's own on-disk read,
//! before AND after the LSP round trip. That closes the window on the
//! ADAPTER's side. It does NOT close the window on kb-code's side: the file
//! could be edited again in the interval between kb-code sending the
//! request and kb-code processing the response. [`verify_blob_freshness`]
//! is that second, independent check — it re-reads the file fresh (never
//! reusing the hash used to build the request) and compares it against the
//! response's own `verified_blob_sha`. A lip result is surfaced as
//! `precision: "lsp-live"`, `trust: "exact"` (`resolve::PRECISION_LSP_LIVE`
//! / `resolve::CLASS_EXACT`) ONLY when this re-check passes; a mismatch
//! DISCARDS the answer entirely — never a downgrade to a lower trust class
//! (the oracle-bar rule: "a wrong exact is a release blocker").
//!
//! # Never persisted; async-overlay architecture
//!
//! Every lip answer is computed fresh, per request, and written to NO
//! table — the doclens/codelens precedent ("computed per request and NEVER
//! persisted", invariant #2). `resolve_position`/`hover_at`/`usages_at`
//! (`crate::resolve`/`crate::hover`/`crate::usages`) stay exactly what they
//! were before this Wave: pure, SYNCHRONOUS, directly unit-testable
//! functions with no knowledge of lip at all. The lsp-live overlay is
//! spliced in one layer up, inside each module's own `*_route` async HTTP
//! handler, via the three entry points this module exports:
//! [`lsp_live_definitions`] + [`prepend_candidates`] (`/api/resolve`),
//! [`overlay_hover`] (`/api/hover`), and [`overlay_usages`]
//! (`/api/usages`). This keeps the existing, heavily-tested sync ladders
//! byte-for-byte unchanged when no provider is configured or reachable,
//! and keeps the (inherently async, inherently I/O-bound) lip round trip
//! entirely out of code that other tests call directly and synchronously.
//!
//! # S2-C — code actions (append; design-s2.md § S2-C)
//!
//! Adds [`LipClient::code_actions`], the 6th lip/1 endpoint (`POST
//! /lip/code-actions`) — kb-lip's own "no code actions" refusal is
//! REVERSED by operator ratification (2026-08-28); the refusal that
//! REMAINS is rename/formatting/`workspace/executeCommand` (an action
//! whose edit can't be materialized as `TextEdit`s is dropped, counted,
//! never executed — kb-lip's own concern, not this client's). Its route,
//! `POST /api/code-actions`, lives in the SIBLING module `crate::
//! code_actions` (not here) — a new module, not folded into this file,
//! since it owns its own request/response wire types distinct from
//! `DiagnosticsOut` above; see that module's doc for the honest-degrade
//! `unavailable_reason` vocabulary (incl. the new `"capability_absent"`).
//! `code_actions`'s conversion into a durable suggestion rides the
//! EXISTING `POST /api/annotations/batch` op — this module adds no new
//! mutation surface.

use crate::config::{IntelProviderEntry, IntelSection};
use crate::routes::{find_repo, read_repo_file, ApiError};
use crate::state::SharedState;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::sync::OnceCell;

/// design-lip.md: "a reqwest client (2s timeout, no retries, degrade to
/// absent — github.rs's 'another system's bad day' law)".
pub const TIMEOUT: Duration = Duration::from_secs(2);

/// lip/1's fixed major version — a provider reporting anything else fails
/// CLOSED (see [`LipClient::probe`]'s doc).
pub const LIP_MAJOR: u32 = 1;

// --- identity/handshake ----------------------------------------------------

/// Deserialize mirror of `GET /lip/identity`'s body
/// (`crates/kb-lip/src/http.rs::identity`). Every field but `capabilities`
/// is `Option`/defaulted on purpose — a malformed or non-lip-speaking
/// server at the configured URL degrades to "no conclusion reached" (see
/// `probe`'s doc) rather than panicking on a missing field.
#[derive(Debug, Clone, Deserialize, Default)]
struct IdentityHello {
    #[serde(default)]
    protocol: Option<String>,
    #[serde(default)]
    lip_major: Option<u32>,
    #[serde(default)]
    server_name: Option<String>,
    #[serde(default)]
    server_version: Option<String>,
    #[serde(default)]
    healthy: Option<bool>,
    #[serde(default)]
    capabilities: Vec<String>,
}

#[derive(Debug, Clone)]
struct IdentitySnapshot {
    server_name: Option<String>,
    server_version: Option<String>,
    #[allow(dead_code)] // surfaced via `ProviderSnapshot` in a later wave if needed
    healthy: bool,
    capabilities: Vec<String>,
}

/// Only ever cached via `get_or_try_init`'s `Ok` arm — see the module doc's
/// "Handshake" section for why an unreached probe never lands here.
#[derive(Debug, Clone)]
enum Handshake {
    /// `lip_major == 1` — usable for the rest of the process' lifetime.
    Ok(IdentitySnapshot),
    /// `lip_major` present but != [`LIP_MAJOR`] — FAIL CLOSED, sticky.
    Mismatch(String),
}

/// `GET /api/repos`'s per-repo `intel` field — see [`LipRegistry::
/// status_for_repo`]'s doc for why this is a cheap, non-blocking peek
/// rather than a fresh probe.
#[derive(Debug, Clone, Default)]
pub struct ProviderSnapshot {
    pub alive: bool,
    pub server_version: Option<String>,
}

/// One provider endpoint's client — see the module doc.
pub struct LipClient {
    name: String,
    url: String,
    client: std::result::Result<reqwest::Client, String>,
    handshake: OnceCell<Handshake>,
}

impl LipClient {
    fn new(name: String, url: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .map_err(|e| e.to_string());
        Self {
            name,
            url,
            client,
            handshake: OnceCell::new(),
        }
    }

    fn client(&self) -> Option<&reqwest::Client> {
        self.client.as_ref().ok()
    }

    /// `GET {url}/lip/identity`, classified into a [`Handshake`].
    /// `Err(())` means the probe reached NO conclusion — see the module
    /// doc's "Handshake" section; the caller must not cache that.
    async fn probe(&self) -> std::result::Result<Handshake, ()> {
        let client = self.client().ok_or(())?;
        let url = format!("{}/lip/identity", self.url.trim_end_matches('/'));
        let resp = client.get(&url).send().await.map_err(|_| ())?;
        if !resp.status().is_success() {
            return Err(());
        }
        let hello: IdentityHello = resp.json().await.map_err(|_| ())?;
        let Some(major) = hello.lip_major else {
            return Err(());
        };
        if major == LIP_MAJOR {
            return Ok(Handshake::Ok(IdentitySnapshot {
                server_name: hello.server_name,
                server_version: hello.server_version,
                healthy: hello.healthy.unwrap_or(true),
                capabilities: hello.capabilities,
            }));
        }
        Ok(Handshake::Mismatch(format!(
            "lip provider {:?} at {} reports lip_major={major} (protocol {:?}); \
             kb-code speaks lip/1 major {LIP_MAJOR}",
            self.name, self.url, hello.protocol,
        )))
    }

    /// The gate every request method runs first — cheap after the first
    /// REACHED conclusion (one `OnceCell` read); an unreached probe is
    /// retried on every call until it reaches one (see the module doc).
    async fn ensure_handshake(&self) -> Option<&IdentitySnapshot> {
        let state = self
            .handshake
            .get_or_try_init(|| async {
                let outcome = self.probe().await?;
                match &outcome {
                    Handshake::Ok(id) => tracing::info!(
                        provider = %self.name, url = %self.url,
                        server = ?id.server_name, version = ?id.server_version,
                        "kb-code: lip/1 handshake ok",
                    ),
                    Handshake::Mismatch(reason) => tracing::error!(
                        provider = %self.name, url = %self.url, %reason,
                        "kb-code: lip/1 handshake MISMATCH — provider marked unusable \
                         until this daemon restarts against a matching major",
                    ),
                }
                Ok(outcome)
            })
            .await;
        match state {
            Ok(Handshake::Ok(id)) => Some(id),
            Ok(Handshake::Mismatch(_)) | Err(()) => None,
        }
    }

    /// Cheap, NON-BLOCKING status peek for `GET /api/repos` — reads
    /// whatever the `OnceCell` already has cached, WITHOUT triggering a
    /// probe (a forced network round trip on every `/api/repos` poll would
    /// defeat the route's "computed from the handshake state, cheap"
    /// contract — design-addendum-2.md §D). A provider that has never
    /// been used yet (no resolve/hover/usages/diagnostics call has run
    /// against it since boot) reports `alive: false` until its first real
    /// request performs the lazy handshake — a documented trade-off, not a
    /// bug (pinned by `lip_registry_status_reports_dead_until_first_real_use`).
    fn snapshot(&self) -> ProviderSnapshot {
        match self.handshake.get() {
            Some(Handshake::Ok(id)) => ProviderSnapshot {
                alive: true,
                server_version: id.server_version.clone(),
            },
            Some(Handshake::Mismatch(_)) | None => ProviderSnapshot::default(),
        }
    }

    fn supports(&self, capability: &str) -> bool {
        matches!(
            self.handshake.get(),
            Some(Handshake::Ok(id)) if id.capabilities.iter().any(|c| c == capability)
        )
    }

    // --- per-request pipeline ----------------------------------------------

    async fn post_positional(
        &self,
        endpoint: &str,
        path: &str,
        blob_sha: &str,
        line: u32,
        col: u32,
    ) -> Option<LipAnswer> {
        self.ensure_handshake().await?;
        let client = self.client()?;
        let url = format!("{}/lip/{endpoint}", self.url.trim_end_matches('/'));
        let body = json!({"path": path, "blob_sha": blob_sha, "line": line, "col": col});
        let resp = client.post(&url).json(&body).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let env: LipEnvelope = resp.json().await.ok()?;
        env.into_answer()
    }

    /// `POST /lip/hover`.
    pub async fn hover(
        &self,
        path: &str,
        blob_sha: &str,
        line: u32,
        col: u32,
    ) -> Option<LipAnswer> {
        self.post_positional("hover", path, blob_sha, line, col)
            .await
    }

    /// `POST /lip/definition`.
    pub async fn definition(
        &self,
        path: &str,
        blob_sha: &str,
        line: u32,
        col: u32,
    ) -> Option<LipAnswer> {
        self.post_positional("definition", path, blob_sha, line, col)
            .await
    }

    /// `POST /lip/references`.
    pub async fn references(
        &self,
        path: &str,
        blob_sha: &str,
        line: u32,
        col: u32,
    ) -> Option<LipAnswer> {
        self.post_positional("references", path, blob_sha, line, col)
            .await
    }

    /// `POST /lip/diagnostics` — whole-document, no position
    /// (design-addendum-2.md §D: `{path, blob_sha}`). Gated on the
    /// handshake's own `capabilities` array (feature-detection against an
    /// older L1-only adapter that predates this endpoint).
    pub async fn diagnostics(&self, path: &str, blob_sha: &str) -> Option<LipAnswer> {
        self.ensure_handshake().await?;
        if !self.supports("diagnostics") {
            return None;
        }
        let client = self.client()?;
        let url = format!("{}/lip/diagnostics", self.url.trim_end_matches('/'));
        let body = json!({"path": path, "blob_sha": blob_sha});
        let resp = client.post(&url).json(&body).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let env: LipEnvelope = resp.json().await.ok()?;
        env.into_answer()
    }

    /// `POST /lip/code-actions` — S2-C (design-s2.md § S2-C), lip/1's 6th
    /// endpoint. Same byte-col/1-based-line codec every lip endpoint uses;
    /// `end_line`/`end_col` are sent through verbatim (a point range when
    /// `None` — kb-lip itself defaults them to `start_line`/`start_col`,
    /// never this client). Gated on the handshake's `capabilities` array
    /// carrying `"code-actions"` (an older adapter build predates this
    /// endpoint, same feature-detection [`Self::diagnostics`] already
    /// performs) — but UNLIKE every other method here, the caller needs to
    /// know WHY no answer came back (a provider that's simply too old to
    /// speak this capability, vs. every other failure class), so this
    /// returns a [`Result`] rather than flattening straight to `Option`.
    #[allow(clippy::too_many_arguments)]
    pub async fn code_actions(
        &self,
        path: &str,
        blob_sha: &str,
        start_line: u32,
        start_col: u32,
        end_line: Option<u32>,
        end_col: Option<u32>,
        kinds: Option<&[String]>,
    ) -> std::result::Result<LipCodeActionsAnswer, CodeActionsFailure> {
        self.ensure_handshake()
            .await
            .ok_or(CodeActionsFailure::Unavailable)?;
        if !self.supports("code-actions") {
            return Err(CodeActionsFailure::CapabilityAbsent);
        }
        let client = self.client().ok_or(CodeActionsFailure::Unavailable)?;
        let url = format!("{}/lip/code-actions", self.url.trim_end_matches('/'));
        let body = CodeActionsRequest {
            path,
            blob_sha,
            start_line,
            start_col,
            end_line,
            end_col,
            kinds,
        };
        let resp = client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|_| CodeActionsFailure::Unavailable)?;
        if !resp.status().is_success() {
            return Err(CodeActionsFailure::Unavailable);
        }
        let env: LipCodeActionsEnvelope = resp
            .json()
            .await
            .map_err(|_| CodeActionsFailure::Unavailable)?;
        if env.refused.is_some() {
            return Err(CodeActionsFailure::Unavailable);
        }
        let verified_blob_sha = env
            .verified_blob_sha
            .ok_or(CodeActionsFailure::Unavailable)?;
        Ok(LipCodeActionsAnswer {
            verified_blob_sha,
            results: env.results.unwrap_or(Value::Null),
            dropped_command_only: env.dropped_command_only,
            dropped_unsupported: env.dropped_unsupported,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

// --- S2-C: POST /lip/code-actions wire (duplicate-parse posture, see the
// module doc) — request struct, response envelope, and the answer/failure
// types `code_actions.rs`'s route handler consumes. Delimited from the
// generic hover/definition/references/diagnostics wire above: a code
// action carries `dropped_*` counters no other lip endpoint has, so it
// gets its OWN envelope/answer pair rather than growing `LipEnvelope`/
// `LipAnswer` with fields meaningless to every other call. ------------------

/// `POST /lip/code-actions` request body (design-s2.md § S2-C's Request
/// block). `end_line`/`end_col`/`kinds` are omitted from the wire
/// entirely when `None` (`skip_serializing_if`) rather than sent as
/// `null` — kb-lip's own point-range default only kicks in on a truly
/// ABSENT key.
#[derive(Debug, Serialize)]
struct CodeActionsRequest<'a> {
    path: &'a str,
    blob_sha: &'a str,
    start_line: u32,
    start_col: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_col: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kinds: Option<&'a [String]>,
}

/// Deserialize mirror of `POST /lip/code-actions`'s response envelope
/// (design-s2.md § S2-C's Response block): either `{"refused": "...", ...}`
/// or `{verified_blob_sha, results, dropped_command_only,
/// dropped_unsupported}` — never both. Every field but the two counters is
/// `Option`/defaulted on purpose (same "malformed degrades to no
/// conclusion" posture [`IdentityHello`]'s doc explains); the counters
/// themselves default to `0` when absent rather than erroring the whole
/// answer — an adapter build that hasn't grown them yet is not the same
/// failure class as a malformed body.
#[derive(Debug, Deserialize)]
struct LipCodeActionsEnvelope {
    #[serde(default)]
    refused: Option<String>,
    #[serde(default)]
    verified_blob_sha: Option<String>,
    #[serde(default)]
    results: Option<Value>,
    #[serde(default)]
    dropped_command_only: u32,
    #[serde(default)]
    dropped_unsupported: u32,
}

/// One successful (non-refused) `POST /lip/code-actions` response —
/// [`LipCodeActionsEnvelope`]'s unwrapped, honest shape. `results` is the
/// RAW translated-actions array; `code_actions.rs`'s `parse_code_actions`
/// is the defensive parser over it (mirrors [`LipAnswer`]'s own
/// results-stays-raw-until-parsed convention). `dropped_command_only`/
/// `dropped_unsupported` are kb-lip's OWN counters, relayed verbatim —
/// `code_actions.rs` must never recompute them from `results` itself.
#[derive(Debug, Clone)]
pub struct LipCodeActionsAnswer {
    pub verified_blob_sha: String,
    pub results: Value,
    pub dropped_command_only: u32,
    pub dropped_unsupported: u32,
}

/// Why [`LipClient::code_actions`] produced no answer — see that method's
/// doc. `CapabilityAbsent` is S2-C's new closed-vocab `/api/code-actions`
/// reason (the provider IS configured and reachable, it simply predates
/// this endpoint); every other failure class (refused / timed out /
/// unreachable / handshake unusable / non-2xx / unparseable body) buckets
/// under `Unavailable` — the SAME bucket `diagnostics_for`'s
/// `"provider_unavailable"` already covers, so `code_actions.rs` maps this
/// arm to that exact string for consistency across both routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeActionsFailure {
    CapabilityAbsent,
    Unavailable,
}

/// Deserialize mirror of every `POST /lip/*` response envelope
/// (`crates/kb-lip/src/http.rs::ok_envelope`/`Refusal::into_json`): either
/// `{"refused": "...", ...}` or `{"verified_blob_sha": "...", "results": ...}`
/// — never both.
#[derive(Debug, Deserialize)]
struct LipEnvelope {
    #[serde(default)]
    refused: Option<String>,
    #[serde(default)]
    verified_blob_sha: Option<String>,
    #[serde(default)]
    results: Option<Value>,
}

impl LipEnvelope {
    fn into_answer(self) -> Option<LipAnswer> {
        if self.refused.is_some() {
            return None;
        }
        Some(LipAnswer {
            verified_blob_sha: self.verified_blob_sha?,
            results: self.results.unwrap_or(Value::Null),
        })
    }
}

/// One successful (non-refused) lip/1 response — [`LipEnvelope`]'s
/// unwrapped, honest shape. `verified_blob_sha` is the adapter's OWN
/// freshly-hashed view of the file at request time; callers MUST run it
/// through [`verify_blob_freshness`] before surfacing anything from
/// `results` as `exact`/`lsp-live` (see the module doc's CRITICAL trust
/// rule).
#[derive(Debug, Clone)]
pub struct LipAnswer {
    pub verified_blob_sha: String,
    pub results: Value,
}

/// The CRITICAL trust rule (design-lip.md, module doc's "blob-freshness
/// re-check" section): re-reads `path` FRESH off `repo`'s working tree
/// (never reusing whatever hash was used to BUILD the original request —
/// that would make this check a tautology) and compares it against
/// `answer.verified_blob_sha`. `false` on any read failure too (an
/// unreadable file can never be honestly verified fresh).
pub(crate) fn verify_blob_freshness(
    repo: &crate::config::RepoEntry,
    path: &str,
    answer: &LipAnswer,
) -> bool {
    verify_blob_freshness_sha(repo, path, &answer.verified_blob_sha)
}

/// The same CRITICAL trust rule as [`verify_blob_freshness`], keyed
/// directly on a `verified_blob_sha` string rather than a [`LipAnswer`] —
/// S2-C's [`LipCodeActionsAnswer`] carries its own extra `dropped_*`
/// fields and is deliberately NOT a [`LipAnswer`] (duplicate-parse
/// posture, this module's doc), so it re-uses this shared core instead of
/// a third copy of the read-and-compare logic.
pub(crate) fn verify_blob_freshness_sha(
    repo: &crate::config::RepoEntry,
    path: &str,
    verified_blob_sha: &str,
) -> bool {
    match read_repo_file(repo, path, None) {
        Ok(read) => read.blob_hash == verified_blob_sha,
        Err(_) => false,
    }
}

// --- response parsing (mirrors kb-lip's own translate_* output shapes) -----

struct LipLocation {
    path: String,
    line: u32,
    col: u32,
}

/// Parses `translate_locations`'s output (`crates/kb-lip/src/http.rs`):
/// `[{path, line, col, end_line, end_col}, ...]`. A malformed entry (missing
/// `path`/`line`) is dropped, never panicked on — an upstream LSP quirk
/// must degrade this ONE entry, not the whole response.
fn parse_locations(results: &Value) -> Vec<LipLocation> {
    results
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| {
            Some(LipLocation {
                path: v.get("path")?.as_str()?.to_string(),
                line: v.get("line")?.as_u64()? as u32,
                col: v.get("col").and_then(Value::as_u64).unwrap_or(0) as u32,
            })
        })
        .collect()
}

/// Parses `translate_hover`'s output: `[{contents, range}]` (0 or 1 items —
/// see that fn's own doc). `None` on a null/empty/malformed result — never
/// a fabricated empty string standing in for "no hover data."
fn parse_hover_contents(results: &Value) -> Option<String> {
    let arr = results.as_array()?;
    let first = arr.first()?;
    let contents = first.get("contents")?.as_str()?;
    if contents.is_empty() {
        return None;
    }
    Some(contents.to_string())
}

/// One `translate_diagnostics`'d row, kb-code's own wire shape for
/// `GET /api/diagnostics` (see [`DiagnosticOut`]).
fn parse_diagnostics(results: &Value) -> Vec<DiagnosticOut> {
    results
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| {
            Some(DiagnosticOut {
                line: d.get("line")?.as_u64()? as u32,
                col: d.get("col")?.as_u64()? as u32,
                end_line: d.get("end_line").and_then(Value::as_u64).unwrap_or(0) as u32,
                end_col: d.get("end_col").and_then(Value::as_u64).unwrap_or(0) as u32,
                severity: d.get("severity").and_then(Value::as_i64),
                code: d.get("code").cloned().filter(|v| !v.is_null()),
                source: d.get("source").and_then(Value::as_str).map(str::to_string),
                message: d
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect()
}

// --- the provider registry --------------------------------------------------

/// Per-boot registry of every configured `[[intel.providers]]` entry —
/// `AppState::lip`. One [`LipClient`] per provider name, constructed once
/// at boot (infallible — a `LipClient` builds its own `reqwest::Client`
/// lazily-degrading, same as `join::kb_client::KbClient::new`) and never
/// mutated afterward (no live-reload, matching `AppState`'s own posture for
/// every other config-derived field).
pub struct LipRegistry {
    section: IntelSection,
    clients: HashMap<String, LipClient>,
}

impl LipRegistry {
    pub fn new(section: IntelSection) -> Self {
        let clients = section
            .providers
            .iter()
            .map(|p| {
                (
                    p.name.clone(),
                    LipClient::new(p.name.clone(), p.url.clone()),
                )
            })
            .collect();
        Self { section, clients }
    }

    /// The configured provider entry usable for `(repo_name, lang_id)` —
    /// see `IntelSection::provider_for`'s doc for the allowlist semantics.
    pub fn provider_for(&self, repo_name: &str, lang_id: &str) -> Option<&IntelProviderEntry> {
        self.section.provider_for(repo_name, lang_id)
    }

    /// The [`LipClient`] for `(repo_name, lang_id)`, or `None` when no
    /// provider is configured for that pair — every ladder-overlay entry
    /// point in this module starts here.
    pub fn client_for(&self, repo_name: &str, lang_id: &str) -> Option<&LipClient> {
        let entry = self.provider_for(repo_name, lang_id)?;
        self.clients.get(&entry.name)
    }

    /// `GET /api/repos`'s per-repo `intel` field — the FIRST configured
    /// provider whose `repos` allowlist includes `repo_name` (config order
    /// wins, same convention `IntelSection::provider_for` uses for the
    /// `(repo, lang)` lookup). `None` when no provider names this repo at
    /// all. NEVER triggers network I/O — see [`LipClient::snapshot`]'s doc.
    pub fn status_for_repo(
        &self,
        repo_name: &str,
    ) -> Option<(String, Vec<String>, ProviderSnapshot)> {
        let entry = self
            .section
            .providers
            .iter()
            .find(|p| p.repos.iter().any(|r| r == repo_name))?;
        let snapshot = self
            .clients
            .get(&entry.name)
            .map(LipClient::snapshot)
            .unwrap_or_default();
        Some((entry.name.clone(), entry.langs.clone(), snapshot))
    }

    /// `GET /api/repos`'s per-repo `intel_providers` field (S2-D) — EVERY
    /// configured provider whose `repos` allowlist includes `repo_name`, in
    /// config order (not just the first, unlike [`Self::status_for_repo`]
    /// above). A mixed-language repo (e.g. kb itself: rust + typescript)
    /// can legitimately have more than one `[[intel.providers]]` entry
    /// naming it — one per language — and `status_for_repo` alone can only
    /// ever surface the first. `status_for_repo` is kept byte-for-byte
    /// unchanged for back-compat; this is a purely additive sibling. Same
    /// cheap, never-network-I/O posture as `status_for_repo` (cached
    /// snapshot only). Empty when no provider names this repo at all.
    pub fn status_all_for_repo(
        &self,
        repo_name: &str,
    ) -> Vec<(String, Vec<String>, ProviderSnapshot)> {
        self.section
            .providers
            .iter()
            .filter(|p| p.repos.iter().any(|r| r == repo_name))
            .map(|entry| {
                let snapshot = self
                    .clients
                    .get(&entry.name)
                    .map(LipClient::snapshot)
                    .unwrap_or_default();
                (entry.name.clone(), entry.langs.clone(), snapshot)
            })
            .collect()
    }
}

// --- /api/resolve overlay: lsp-live TOP tier --------------------------------

/// `resolve_route`'s async lsp-live overlay (design-lip.md: "a new TOP
/// tier `lsp-live`... consulted first when (repo, file-lang) has a
/// configured provider"). `None`/empty in every degrade case: no provider
/// configured for `(repo, lang)`; `rev` is `Some` (an LSP server only ever
/// answers against the LIVE working tree — never a historical git blob, so
/// this tier is a deliberate no-op for a `?ref=` request); the provider
/// refuses/times out/is unreachable; or the post-hoc blob-freshness
/// re-check fails (discarded, never downgraded — see [`verify_blob_freshness`]).
pub(crate) async fn lsp_live_definitions(
    state: &SharedState,
    repo: &crate::config::RepoEntry,
    path: &str,
    rev: Option<&str>,
    line: u32,
    col: u32,
) -> Vec<crate::resolve::Candidate> {
    if rev.is_some() {
        return Vec::new();
    }
    let Some(lang) = crate::lang::detect(path, None) else {
        return Vec::new();
    };
    let Some(client) = state.lip.client_for(&repo.name, lang.id) else {
        return Vec::new();
    };
    let Ok(read) = read_repo_file(repo, path, None) else {
        return Vec::new();
    };
    let Some(answer) = client.definition(path, &read.blob_hash, line, col).await else {
        return Vec::new();
    };
    if !verify_blob_freshness(repo, path, &answer) {
        tracing::debug!(
            repo = %repo.name, path,
            "kb-code: lip definition answer discarded — blob went stale between \
             request and freshness re-check",
        );
        return Vec::new();
    }
    parse_locations(&answer.results)
        .into_iter()
        .map(|loc| crate::resolve::Candidate {
            repo: repo.name.clone(),
            path: loc.path,
            line: loc.line,
            kind: None,
            container: None,
            signature: None,
            doc: None,
            precision: crate::resolve::PRECISION_LSP_LIVE,
            class: crate::resolve::CLASS_EXACT,
        })
        .collect()
}

/// Splices `extra` (the lsp-live tier) onto the FRONT of `out.candidates`,
/// deduping against `(repo, path, line)` locations the sync ladder already
/// emitted (same rule `resolve::push_candidate` enforces within a single
/// ladder pass — the SAME location never appears twice), then re-applies
/// `resolve::MAX_CANDIDATES` and recomputes `out.total`. A no-op when
/// `extra` is empty (the overwhelmingly common case — no provider
/// configured, or the position had nothing lsp-live could answer).
pub(crate) fn prepend_candidates(
    out: &mut crate::resolve::ResolveOut,
    extra: Vec<crate::resolve::Candidate>,
) {
    if extra.is_empty() {
        return;
    }
    let mut seen: HashSet<(String, String, u32)> = out
        .candidates
        .iter()
        .map(|c| (c.repo.clone(), c.path.clone(), c.line))
        .collect();
    let mut merged = Vec::with_capacity(extra.len() + out.candidates.len());
    for c in extra {
        let key = (c.repo.clone(), c.path.clone(), c.line);
        if seen.insert(key) {
            merged.push(c);
        }
    }
    merged.append(&mut out.candidates);
    merged.truncate(crate::resolve::MAX_CANDIDATES);
    out.total = merged.len();
    out.candidates = merged;
}

// --- /api/hover overlay: provider hover fills signature/doc ----------------

/// `hover_route`'s async lsp-live overlay (design-lip.md: "its symbol
/// half — provider hover fills signature/doc; keep the framework half
/// untouched"). Applied ONLY when the local ladder already produced a
/// `symbol` (a `kind` from a real symbols-table row to anchor on — this
/// module never fabricates one): fills `symbol.doc` with the provider's
/// rendered hover contents and reclassifies `precision`/`trust` to
/// lsp-live/exact, but ONLY once blob-freshness is confirmed. `out.
/// framework` is never touched. A no-op in every degrade case (see
/// [`lsp_live_definitions`]'s doc for the shared degrade list).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn overlay_hover(
    state: &SharedState,
    repo: &crate::config::RepoEntry,
    path: &str,
    rev: Option<&str>,
    line: u32,
    col: u32,
    out: &mut crate::hover::HoverOut,
) {
    if rev.is_some() {
        return;
    }
    let Some(symbol) = out.symbol.as_mut() else {
        return;
    };
    let Some(lang) = crate::lang::detect(path, None) else {
        return;
    };
    let Some(client) = state.lip.client_for(&repo.name, lang.id) else {
        return;
    };
    let Ok(read) = read_repo_file(repo, path, None) else {
        return;
    };
    let Some(answer) = client.hover(path, &read.blob_hash, line, col).await else {
        return;
    };
    if !verify_blob_freshness(repo, path, &answer) {
        tracing::debug!(
            repo = %repo.name, path,
            "kb-code: lip hover answer discarded — blob went stale between \
             request and freshness re-check",
        );
        return;
    }
    let Some(contents) = parse_hover_contents(&answer.results) else {
        return;
    };
    symbol.doc = Some(contents);
    out.precision = Some(crate::resolve::PRECISION_LSP_LIVE);
    out.trust = Some(crate::resolve::CLASS_EXACT);
}

// --- /api/usages overlay: references merged into the exact class -----------

/// `usages_route`'s async lsp-live overlay (design-lip.md: "references
/// merged into the exact class, capped, deduped"). Appends every
/// blob-verified `POST /lip/references` location not already present in
/// `out.exact` (by `(path, line, col)`), stopping at `limit` — `out.
/// total_exact`/`out.truncated` are kept honest (incremented by every NEW,
/// deduped location, whether or not the cap let it into `out.exact`
/// itself). A no-op in every degrade case (see [`lsp_live_definitions`]'s
/// doc for the shared degrade list).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn overlay_usages(
    state: &SharedState,
    repo: &crate::config::RepoEntry,
    path: &str,
    rev: Option<&str>,
    line: u32,
    col: u32,
    limit: usize,
    out: &mut crate::usages::UsagesOut,
) {
    let locs = lsp_live_reference_locations(state, repo, path, rev, line, col).await;
    let mut seen: HashSet<(String, u32, u32)> = out
        .exact
        .iter()
        .map(|r| (r.path.clone(), r.line, r.col))
        .collect();
    let mut new_total = 0usize;
    for (loc_path, loc_line, loc_col) in locs {
        let key = (loc_path.clone(), loc_line, loc_col);
        if !seen.insert(key) {
            continue;
        }
        new_total += 1;
        if out.exact.len() < limit {
            let context = line_context(repo, &loc_path, loc_line);
            out.exact.push(crate::usages::UsageRow {
                path: loc_path,
                line: loc_line,
                col: loc_col,
                kind: "ref".to_string(),
                access: None,
                context,
            });
        }
    }
    out.total_exact += new_total;
    out.truncated = out.truncated || out.total_exact > limit;
}

/// The lsp-live reference locations for a position — the ONE place this
/// crate asks a provider for `references`, shared by `usages/1`'s
/// [`overlay_usages`] and `usages/2`'s own overlay (V71-E1). Returns an
/// EMPTY vec in every degrade case (a `?ref=` read, no provider for the
/// pair, a refusal/timeout, or a blob that went stale between the request
/// and the freshness re-check) — a caller can therefore treat "no
/// locations" and "no provider" identically, which is what keeps both
/// overlays byte-identical to their non-provider answer.
pub(crate) async fn lsp_live_reference_locations(
    state: &SharedState,
    repo: &crate::config::RepoEntry,
    path: &str,
    rev: Option<&str>,
    line: u32,
    col: u32,
) -> Vec<(String, u32, u32)> {
    if rev.is_some() {
        return Vec::new();
    }
    let Some(lang) = crate::lang::detect(path, None) else {
        return Vec::new();
    };
    let Some(client) = state.lip.client_for(&repo.name, lang.id) else {
        return Vec::new();
    };
    let Ok(read) = read_repo_file(repo, path, None) else {
        return Vec::new();
    };
    let Some(answer) = client.references(path, &read.blob_hash, line, col).await else {
        return Vec::new();
    };
    if !verify_blob_freshness(repo, path, &answer) {
        tracing::debug!(
            repo = %repo.name, path,
            "kb-code: lip references answer discarded — blob went stale between \
             request and freshness re-check",
        );
        return Vec::new();
    }
    parse_locations(&answer.results)
        .into_iter()
        .map(|l| (l.path, l.line, l.col))
        .collect()
}

/// Best-effort line-text context for a lsp-live reference row — same
/// trim+200-char-cap convention `usages::make_row` already uses for the
/// sync ladder's own rows. A read failure (e.g. the reference landed in a
/// vendored/external file outside this repo) degrades to an empty string,
/// never an error.
pub(crate) fn line_context(repo: &crate::config::RepoEntry, path: &str, line: u32) -> String {
    let Ok(read) = read_repo_file(repo, path, None) else {
        return String::new();
    };
    let Ok(text) = std::str::from_utf8(&read.bytes) else {
        return String::new();
    };
    text.lines()
        .nth((line.saturating_sub(1)) as usize)
        .unwrap_or("")
        .trim()
        .chars()
        .take(200)
        .collect()
}

// --- GET /api/diagnostics ---------------------------------------------------

pub const DIAGNOSTICS_SCHEMA: &str = "diagnostics/1";

#[derive(Debug, Deserialize)]
pub struct DiagnosticsParams {
    pub repo: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DiagnosticOut {
    pub line: u32,
    pub col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub severity: Option<i64>,
    pub code: Option<Value>,
    pub source: Option<String>,
    pub message: String,
}

/// design-addendum-2.md §D: "`{diagnostics: null, unavailable_reason}` —
/// null ≠ [] (null = no provider/refused; [] = provider says clean)".
/// `unavailable_reason`'s closed vocabulary: `"unknown_language"` (path has
/// no detected language) · `"no_provider_configured"` (no `[[intel.
/// providers]]` entry names this (repo, lang)) · `"file_unreadable"` ·
/// `"provider_unavailable"` (refused/timeout/unreachable/handshake
/// unusable/no `diagnostics` capability) · `"blob_stale"` (the CRITICAL
/// trust rule's re-check failed).
#[derive(Debug, Serialize)]
pub struct DiagnosticsOut {
    pub schema: &'static str,
    pub path: String,
    pub diagnostics: Option<Vec<DiagnosticOut>>,
    pub provider: Option<String>,
    pub fetched: bool,
    pub unavailable_reason: Option<&'static str>,
}

/// `GET /api/diagnostics?repo=&path=` (bearer — ordinary `auth_bearer`
/// browsing-class read, same gate as `/api/resolve`/`/api/hover`/
/// `/api/usages`; loopback bypasses per invariant #4). Dispatches to the
/// configured provider for `path`'s detected language; computed
/// fresh-per-request, never persisted (same posture as every other lip
/// overlay in this module).
pub async fn diagnostics_route(
    State(state): State<SharedState>,
    Query(params): Query<DiagnosticsParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &params.repo)?;
    let out = diagnostics_for(&state, repo, &params.path).await;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

async fn diagnostics_for(
    state: &SharedState,
    repo: &crate::config::RepoEntry,
    path: &str,
) -> DiagnosticsOut {
    let empty = |reason: &'static str, provider: Option<String>| DiagnosticsOut {
        schema: DIAGNOSTICS_SCHEMA,
        path: path.to_string(),
        diagnostics: None,
        provider,
        fetched: false,
        unavailable_reason: Some(reason),
    };

    let Some(lang) = crate::lang::detect(path, None) else {
        return empty("unknown_language", None);
    };
    let Some(client) = state.lip.client_for(&repo.name, lang.id) else {
        return empty("no_provider_configured", None);
    };
    let provider_name = client.name().to_string();
    let Ok(read) = read_repo_file(repo, path, None) else {
        return empty("file_unreadable", Some(provider_name));
    };
    let Some(answer) = client.diagnostics(path, &read.blob_hash).await else {
        return empty("provider_unavailable", Some(provider_name));
    };
    if !verify_blob_freshness(repo, path, &answer) {
        return empty("blob_stale", Some(provider_name));
    }
    let diagnostics = parse_diagnostics(&answer.results);
    DiagnosticsOut {
        schema: DIAGNOSTICS_SCHEMA,
        path: path.to_string(),
        diagnostics: Some(diagnostics),
        provider: Some(provider_name),
        fetched: true,
        unavailable_reason: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RepoEntry;
    use crate::join::kb_client::test_support::mock_kb_server;
    use axum::routing::{get, post};
    use axum::{Json as AxumJson, Router};

    fn write_file(root: &std::path::Path, path: &str, content: &str) {
        let abs = root.join(path);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(abs, content).unwrap();
    }

    fn fixture_repo(name: &str) -> (tempfile::TempDir, RepoEntry) {
        let root = tempfile::tempdir().unwrap();
        let entry = RepoEntry {
            name: name.to_string(),
            path: root.path().to_path_buf(),
        };
        (root, entry)
    }

    fn provider_entry(name: &str, url: String) -> IntelProviderEntry {
        IntelProviderEntry {
            name: name.to_string(),
            url,
            langs: vec!["ruby".to_string()],
            repos: vec!["fixture".to_string()],
        }
    }

    // --- handshake: ok / mismatch / unreachable ---------------------------

    #[tokio::test]
    async fn handshake_ok_lets_requests_through() {
        let router = Router::new()
            .route(
                "/lip/identity",
                get(|| async {
                    AxumJson(json!({
                        "protocol": "lip/1", "lip_major": 1,
                        "server_name": "ruby-lsp", "server_version": "0.1.0",
                        "healthy": true, "capabilities": ["hover", "definition"],
                    }))
                }),
            )
            .route(
                "/lip/hover",
                post(|| async {
                    AxumJson(json!({"verified_blob_sha": "deadbeef", "results": []}))
                }),
            );
        let (addr, _server) = mock_kb_server(router).await;
        let client = LipClient::new("ruby".to_string(), format!("http://{addr}"));
        let answer = client.hover("a.rb", "deadbeef", 1, 0).await;
        assert!(
            answer.is_some(),
            "handshake ok must let the real call through"
        );
    }

    #[tokio::test]
    async fn handshake_mismatch_fails_closed_sticky() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let router = Router::new().route(
            "/lip/identity",
            get(move || {
                let calls = calls_clone.clone();
                async move {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    AxumJson(json!({"protocol": "lip/2", "lip_major": 2}))
                }
            }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = LipClient::new("ruby".to_string(), format!("http://{addr}"));

        assert!(client.hover("a.rb", "x", 1, 0).await.is_none());
        assert!(client.definition("a.rb", "x", 1, 0).await.is_none());
        assert!(client.references("a.rb", "x", 1, 0).await.is_none());
        // Sticky — the mismatch is cached, so every call above shares ONE probe.
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn handshake_unreachable_is_absent_and_never_cached() {
        let client = LipClient::new("ruby".to_string(), "http://127.0.0.1:0".to_string());
        assert!(client.hover("a.rb", "x", 1, 0).await.is_none());
        assert!(
            client.handshake.get().is_none(),
            "an unreached probe must never be cached"
        );
    }

    /// An identity response that OMITS `lip_major` entirely is treated the
    /// same as unreachable (no legacy lip predecessor to grandfather — see
    /// the module doc's "Handshake" section) — not cached, retried.
    #[tokio::test]
    async fn handshake_missing_lip_major_is_unreached_not_a_mismatch() {
        let router = Router::new().route(
            "/lip/identity",
            get(|| async { AxumJson(json!({"protocol": "lip/1"})) }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = LipClient::new("ruby".to_string(), format!("http://{addr}"));
        assert!(client.hover("a.rb", "x", 1, 0).await.is_none());
        assert!(client.handshake.get().is_none());
    }

    /// A REACHABLE peer that reports a non-2xx (e.g. mid-restart) is
    /// likewise unreached: retried, never cached.
    #[tokio::test]
    async fn handshake_non_2xx_identity_is_unreached() {
        let router = Router::new().route(
            "/lip/identity",
            get(|| async { axum::http::StatusCode::SERVICE_UNAVAILABLE }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = LipClient::new("ruby".to_string(), format!("http://{addr}"));
        assert!(client.hover("a.rb", "x", 1, 0).await.is_none());
        assert!(client.handshake.get().is_none());
    }

    // --- refusal / diagnostics capability ----------------------------------

    #[tokio::test]
    async fn a_refused_response_is_none_not_an_error() {
        let router = Router::new()
            .route(
                "/lip/identity",
                get(|| async { AxumJson(json!({"protocol": "lip/1", "lip_major": 1})) }),
            )
            .route(
                "/lip/definition",
                post(|| async {
                    AxumJson(json!({"refused": "blob_mismatch", "have": "a", "want": "b"}))
                }),
            );
        let (addr, _server) = mock_kb_server(router).await;
        let client = LipClient::new("ruby".to_string(), format!("http://{addr}"));
        assert!(client.definition("a.rb", "x", 1, 0).await.is_none());
    }

    #[tokio::test]
    async fn diagnostics_capability_gates_the_call() {
        let hit = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hit_clone = hit.clone();
        let router = Router::new()
            .route(
                "/lip/identity",
                // No "diagnostics" in capabilities — an older L1-only adapter.
                get(|| async {
                    AxumJson(
                        json!({"protocol": "lip/1", "lip_major": 1, "capabilities": ["hover"]}),
                    )
                }),
            )
            .route(
                "/lip/diagnostics",
                post(move || {
                    let hit = hit_clone.clone();
                    async move {
                        hit.store(true, std::sync::atomic::Ordering::SeqCst);
                        AxumJson(json!({"verified_blob_sha": "x", "results": []}))
                    }
                }),
            );
        let (addr, _server) = mock_kb_server(router).await;
        let client = LipClient::new("ruby".to_string(), format!("http://{addr}"));
        assert!(client.diagnostics("a.rb", "x").await.is_none());
        assert!(
            !hit.load(std::sync::atomic::Ordering::SeqCst),
            "an unsupported capability must never even reach the endpoint"
        );
    }

    // --- verify_blob_freshness ----------------------------------------------

    #[test]
    fn verify_blob_freshness_matches_a_fresh_read() {
        let (root, repo) = fixture_repo("fixture");
        write_file(root.path(), "a.rb", "puts 1\n");
        let hash = crate::ingest::git_blob_hash(b"puts 1\n");
        let answer = LipAnswer {
            verified_blob_sha: hash,
            results: Value::Null,
        };
        assert!(verify_blob_freshness(&repo, "a.rb", &answer));
    }

    #[test]
    fn verify_blob_freshness_discards_a_stale_answer() {
        let (root, repo) = fixture_repo("fixture");
        write_file(root.path(), "a.rb", "puts 1\n");
        let answer = LipAnswer {
            verified_blob_sha: "not-the-real-hash".to_string(),
            results: Value::Null,
        };
        assert!(!verify_blob_freshness(&repo, "a.rb", &answer));
    }

    #[test]
    fn verify_blob_freshness_catches_a_concurrent_edit() {
        let (root, repo) = fixture_repo("fixture");
        write_file(root.path(), "a.rb", "puts 1\n");
        let hash_at_request_time = crate::ingest::git_blob_hash(b"puts 1\n");
        // The file changes AFTER the (simulated) lip round trip started.
        write_file(root.path(), "a.rb", "puts 2\n");
        let answer = LipAnswer {
            verified_blob_sha: hash_at_request_time,
            results: Value::Null,
        };
        assert!(
            !verify_blob_freshness(&repo, "a.rb", &answer),
            "a file changed between request and verify must be caught, not trusted"
        );
    }

    // --- parse_locations / parse_hover_contents / parse_diagnostics --------

    #[test]
    fn parse_locations_reads_translate_locations_shape() {
        let results = json!([
            {"path": "a.rb", "line": 3, "col": 5, "end_line": 3, "end_col": 9},
            {"path": "b.rb", "line": 1, "col": 0, "end_line": 1, "end_col": 3},
        ]);
        let locs = parse_locations(&results);
        assert_eq!(locs.len(), 2);
        assert_eq!(locs[0].path, "a.rb");
        assert_eq!(locs[0].line, 3);
        assert_eq!(locs[0].col, 5);
    }

    #[test]
    fn parse_locations_drops_malformed_entries() {
        let results = json!([{"path": "a.rb"}, {"line": 1}, "not-an-object"]);
        assert!(parse_locations(&results).is_empty());
    }

    #[test]
    fn parse_hover_contents_extracts_the_first_item() {
        let results = json!([{"contents": "**Foo**", "range": null}]);
        assert_eq!(parse_hover_contents(&results).as_deref(), Some("**Foo**"));
    }

    #[test]
    fn parse_hover_contents_empty_result_is_none() {
        assert!(parse_hover_contents(&json!([])).is_none());
        assert!(parse_hover_contents(&json!({})).is_none());
    }

    #[test]
    fn parse_diagnostics_reads_translate_diagnostics_shape() {
        let results = json!([{
            "line": 2, "col": 0, "end_line": 2, "end_col": 5,
            "severity": 1, "code": "E001", "source": "ruby-lsp", "message": "bad thing"
        }]);
        let diags = parse_diagnostics(&results);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 2);
        assert_eq!(diags[0].severity, Some(1));
        assert_eq!(diags[0].source.as_deref(), Some("ruby-lsp"));
        assert_eq!(diags[0].message, "bad thing");
    }

    #[test]
    fn parse_diagnostics_empty_is_empty_not_missing() {
        assert!(parse_diagnostics(&json!([])).is_empty());
    }

    // --- IntelSection / LipRegistry allowlist + snapshot semantics ---------

    #[test]
    fn registry_provider_for_respects_the_semantic_style_allowlist() {
        let section = IntelSection {
            providers: vec![provider_entry("ruby", "http://127.0.0.1:1".to_string())],
        };
        let registry = LipRegistry::new(section);
        assert!(registry.client_for("fixture", "ruby").is_some());
        assert!(registry.client_for("other-repo", "ruby").is_none());
        assert!(registry.client_for("fixture", "python").is_none());
    }

    #[test]
    fn registry_status_for_repo_is_dead_until_first_real_use() {
        let section = IntelSection {
            providers: vec![provider_entry("ruby", "http://127.0.0.1:0".to_string())],
        };
        let registry = LipRegistry::new(section);
        let (provider, langs, snap) = registry.status_for_repo("fixture").unwrap();
        assert_eq!(provider, "ruby");
        assert_eq!(langs, vec!["ruby".to_string()]);
        assert!(
            !snap.alive,
            "never-probed provider must report alive=false, cheaply"
        );
        assert!(registry.status_for_repo("no-such-repo").is_none());
    }

    #[tokio::test]
    async fn registry_status_for_repo_reports_alive_after_a_successful_handshake() {
        let router = Router::new().route(
            "/lip/identity",
            get(|| async {
                AxumJson(json!({
                    "protocol": "lip/1", "lip_major": 1, "server_version": "1.2.3",
                    "capabilities": ["hover"],
                }))
            }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let section = IntelSection {
            providers: vec![provider_entry("ruby", format!("http://{addr}"))],
        };
        let registry = LipRegistry::new(section);
        // Trigger the lazy handshake via a real request.
        let client = registry.client_for("fixture", "ruby").unwrap();
        let _ = client.diagnostics("a.rb", "x").await; // capability-gated no-op, still handshakes
        let (_, _, snap) = registry.status_for_repo("fixture").unwrap();
        assert!(snap.alive);
        assert_eq!(snap.server_version.as_deref(), Some("1.2.3"));
    }

    #[test]
    fn registry_status_all_for_repo_returns_every_matching_provider_in_config_order() {
        let mut rust = provider_entry("rust", "http://127.0.0.1:1".to_string());
        rust.langs = vec!["rust".to_string()];
        let mut ts = provider_entry("typescript", "http://127.0.0.1:2".to_string());
        ts.langs = vec!["typescript".to_string()];
        let section = IntelSection {
            providers: vec![rust, ts],
        };
        let registry = LipRegistry::new(section);

        // status_for_repo (single-valued, back-compat) sees only the FIRST
        // config-order match.
        let (first, ..) = registry.status_for_repo("fixture").unwrap();
        assert_eq!(first, "rust");

        // status_all_for_repo (additive) sees BOTH, in config order.
        let all = registry.status_all_for_repo("fixture");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].0, "rust");
        assert_eq!(all[0].1, vec!["rust".to_string()]);
        assert_eq!(all[1].0, "typescript");
        assert_eq!(all[1].1, vec!["typescript".to_string()]);

        // A repo no provider names at all gets an empty vec, not a panic.
        assert!(registry.status_all_for_repo("no-such-repo").is_empty());
    }

    // --- a real SharedState, via the crate's own test-boot fixture ---------

    /// Boots a real `SharedState` through `crate::build_state_for_test`
    /// (the SAME fixture every other in-crate route-level unit test in
    /// this crate uses — `resolve.rs`/`hover.rs` stay signature-pure, so
    /// this module is the first to need one) with `repo_root` registered
    /// as the ONLY configured repo and `providers` as `[[intel.
    /// providers]]`. `repo_root` need not be a git repository — every path
    /// this module reads goes through `read_repo_file(..., rev: None)`,
    /// which reads straight off disk.
    async fn test_state(
        providers: Vec<IntelProviderEntry>,
        repo_root: &std::path::Path,
        repo_name: &str,
    ) -> SharedState {
        let cfg = crate::config::KbCodeConfig {
            repos: vec![RepoEntry {
                name: repo_name.to_string(),
                path: repo_root.to_path_buf(),
            }],
            kb_daemon: crate::config::KbDaemonSection {
                enabled: false,
                url: "http://127.0.0.1:0".to_string(),
                token_file: None,
                public_url: None,
            },
            intel: IntelSection { providers },
            ..crate::config::KbCodeConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let paths = kb_core::paths::KbPaths::rooted_at(tmp.path(), "kb-code");
        crate::build_state_for_test(cfg, paths).await.unwrap()
    }

    // --- overlay_usages: merge + dedup + cap --------------------------------

    #[tokio::test]
    async fn overlay_usages_merges_new_locations_and_dedups_existing_ones() {
        let (root, repo) = fixture_repo("fixture");
        write_file(root.path(), "a.rb", "def widget\nend\n");
        let hash = crate::ingest::git_blob_hash(b"def widget\nend\n");
        let router = Router::new()
            .route(
                "/lip/identity",
                get(|| async { AxumJson(json!({"protocol": "lip/1", "lip_major": 1})) }),
            )
            .route(
                "/lip/references",
                post(move || {
                    let hash = hash.clone();
                    async move {
                        AxumJson(json!({
                            "verified_blob_sha": hash,
                            "results": [
                                {"path": "a.rb", "line": 1, "col": 0, "end_line": 1, "end_col": 6},
                                {"path": "a.rb", "line": 5, "col": 0, "end_line": 5, "end_col": 6},
                            ]
                        }))
                    }
                }),
            );
        let (addr, _server) = mock_kb_server(router).await;
        let state = test_state(
            vec![provider_entry("ruby", format!("http://{addr}"))],
            root.path(),
            "fixture",
        )
        .await;

        let mut out = crate::usages::UsagesOut {
            schema: crate::usages::USAGES_SCHEMA,
            symbol: crate::usages::UsageSymbol {
                name: "widget".to_string(),
                kind: None,
                container: None,
            },
            class_of_definition: crate::resolve::CLASS_CANDIDATE,
            exact: vec![crate::usages::UsageRow {
                path: "a.rb".to_string(),
                line: 1,
                col: 0,
                kind: "def".to_string(),
                access: None,
                context: "def widget".to_string(),
            }],
            likely: vec![],
            candidate: vec![],
            truncated: false,
            total_exact: 1,
            total_likely: 0,
            total_candidate: 0,
        };

        overlay_usages(&state, &repo, "a.rb", None, 1, 0, 500, &mut out).await;

        // The pre-existing (path=a.rb,line=1,col=0) row must NOT duplicate;
        // the (line=5) location must be merged in as new.
        assert_eq!(out.exact.len(), 2, "got: {:#?}", out.exact);
        assert_eq!(out.total_exact, 2);
        assert!(out.exact.iter().any(|r| r.line == 5 && r.kind == "ref"));
    }

    #[tokio::test]
    async fn overlay_usages_respects_the_per_class_cap() {
        let (root, repo) = fixture_repo("fixture");
        write_file(root.path(), "a.rb", "def widget\nend\n");
        let hash = crate::ingest::git_blob_hash(b"def widget\nend\n");
        let router = Router::new()
            .route(
                "/lip/identity",
                get(|| async { AxumJson(json!({"protocol": "lip/1", "lip_major": 1})) }),
            )
            .route(
                "/lip/references",
                post(move || {
                    let hash = hash.clone();
                    async move {
                        AxumJson(json!({
                            "verified_blob_sha": hash,
                            "results": [
                                {"path": "a.rb", "line": 1, "col": 0, "end_line": 1, "end_col": 6},
                                {"path": "a.rb", "line": 2, "col": 0, "end_line": 2, "end_col": 6},
                                {"path": "a.rb", "line": 3, "col": 0, "end_line": 3, "end_col": 6},
                            ]
                        }))
                    }
                }),
            );
        let (addr, _server) = mock_kb_server(router).await;
        let state = test_state(
            vec![provider_entry("ruby", format!("http://{addr}"))],
            root.path(),
            "fixture",
        )
        .await;

        let mut out = crate::usages::UsagesOut {
            schema: crate::usages::USAGES_SCHEMA,
            symbol: crate::usages::UsageSymbol {
                name: "widget".to_string(),
                kind: None,
                container: None,
            },
            class_of_definition: crate::resolve::CLASS_CANDIDATE,
            exact: vec![],
            likely: vec![],
            candidate: vec![],
            truncated: false,
            total_exact: 0,
            total_likely: 0,
            total_candidate: 0,
        };

        overlay_usages(&state, &repo, "a.rb", None, 1, 0, 2, &mut out).await;
        assert_eq!(out.exact.len(), 2, "capped at limit=2");
        assert_eq!(out.total_exact, 3, "total must stay honest past the cap");
        assert!(out.truncated);
    }

    // --- overlay_hover: fills doc, reclassifies to lsp-live/exact ----------

    #[tokio::test]
    async fn overlay_hover_fills_doc_and_reclassifies_when_blob_verified() {
        let (root, repo) = fixture_repo("fixture");
        write_file(root.path(), "a.rb", "def widget\nend\n");
        let hash = crate::ingest::git_blob_hash(b"def widget\nend\n");
        let router = Router::new()
            .route(
                "/lip/identity",
                get(|| async { AxumJson(json!({"protocol": "lip/1", "lip_major": 1})) }),
            )
            .route(
                "/lip/hover",
                post(move || {
                    let hash = hash.clone();
                    async move {
                        AxumJson(json!({
                            "verified_blob_sha": hash,
                            "results": [{"contents": "live doc from the LSP", "range": null}]
                        }))
                    }
                }),
            );
        let (addr, _server) = mock_kb_server(router).await;
        let state = test_state(
            vec![provider_entry("ruby", format!("http://{addr}"))],
            root.path(),
            "fixture",
        )
        .await;

        let mut out = crate::hover::HoverOut {
            schema: crate::hover::HOVER_SCHEMA,
            path: "a.rb".to_string(),
            line: 1,
            col: 4,
            precision: Some(crate::resolve::PRECISION_FILE_LOCAL),
            trust: Some(crate::resolve::CLASS_LIKELY),
            symbol: Some(crate::hover::HoverSymbol {
                kind: "fn".to_string(),
                name: "widget".to_string(),
                container: None,
                signature: None,
                doc: None,
            }),
            defsite: Some(crate::hover::HoverDefsite {
                path: "a.rb".to_string(),
                line: 1,
            }),
            framework: None,
        };

        overlay_hover(&state, &repo, "a.rb", None, 1, 4, &mut out).await;
        assert_eq!(
            out.symbol.as_ref().unwrap().doc.as_deref(),
            Some("live doc from the LSP")
        );
        assert_eq!(out.precision, Some(crate::resolve::PRECISION_LSP_LIVE));
        assert_eq!(out.trust, Some(crate::resolve::CLASS_EXACT));
    }

    /// The blob-mismatch discard case, at the overlay layer: a provider
    /// that answers with the WRONG `verified_blob_sha` must be discarded
    /// entirely — the pre-existing local `symbol`/`precision`/`trust`
    /// stay byte-for-byte untouched (never downgraded, never blanked).
    #[tokio::test]
    async fn overlay_hover_discards_on_blob_mismatch_leaving_local_data_untouched() {
        let (root, repo) = fixture_repo("fixture");
        write_file(root.path(), "a.rb", "def widget\nend\n");
        let router = Router::new()
            .route(
                "/lip/identity",
                get(|| async { AxumJson(json!({"protocol": "lip/1", "lip_major": 1})) }),
            )
            .route(
                "/lip/hover",
                post(|| async {
                    AxumJson(json!({
                        "verified_blob_sha": "not-the-real-hash",
                        "results": [{"contents": "stale doc", "range": null}]
                    }))
                }),
            );
        let (addr, _server) = mock_kb_server(router).await;
        let state = test_state(
            vec![provider_entry("ruby", format!("http://{addr}"))],
            root.path(),
            "fixture",
        )
        .await;

        let mut out = crate::hover::HoverOut {
            schema: crate::hover::HOVER_SCHEMA,
            path: "a.rb".to_string(),
            line: 1,
            col: 4,
            precision: Some(crate::resolve::PRECISION_FILE_LOCAL),
            trust: Some(crate::resolve::CLASS_LIKELY),
            symbol: Some(crate::hover::HoverSymbol {
                kind: "fn".to_string(),
                name: "widget".to_string(),
                container: None,
                signature: None,
                doc: None,
            }),
            defsite: None,
            framework: None,
        };

        overlay_hover(&state, &repo, "a.rb", None, 1, 4, &mut out).await;
        assert!(
            out.symbol.as_ref().unwrap().doc.is_none(),
            "a blob-mismatched answer must never fill doc"
        );
        assert_eq!(
            out.precision,
            Some(crate::resolve::PRECISION_FILE_LOCAL),
            "must stay at the local ladder's own tier"
        );
        assert_eq!(out.trust, Some(crate::resolve::CLASS_LIKELY));
    }

    // --- lsp_live_definitions + prepend_candidates: blob-mismatch discard --

    #[tokio::test]
    async fn lsp_live_definitions_is_empty_when_the_blob_guard_fails() {
        let (root, repo) = fixture_repo("fixture");
        write_file(root.path(), "a.rb", "def widget\nend\n");
        let router = Router::new()
            .route(
                "/lip/identity",
                get(|| async { AxumJson(json!({"protocol": "lip/1", "lip_major": 1})) }),
            )
            .route(
                "/lip/definition",
                post(|| async {
                    AxumJson(json!({
                        "verified_blob_sha": "not-the-real-hash",
                        "results": [{"path": "a.rb", "line": 1, "col": 0, "end_line": 1, "end_col": 6}]
                    }))
                }),
            );
        let (addr, _server) = mock_kb_server(router).await;
        let state = test_state(
            vec![provider_entry("ruby", format!("http://{addr}"))],
            root.path(),
            "fixture",
        )
        .await;

        let extra = lsp_live_definitions(&state, &repo, "a.rb", None, 1, 4).await;
        assert!(
            extra.is_empty(),
            "a blob-mismatched definition answer must be discarded, not surfaced"
        );
    }

    #[tokio::test]
    async fn lsp_live_definitions_is_a_no_op_against_a_ref_pinned_request() {
        let (root, repo) = fixture_repo("fixture");
        write_file(root.path(), "a.rb", "def widget\nend\n");
        let hash = crate::ingest::git_blob_hash(b"def widget\nend\n");
        let router = Router::new()
            .route(
                "/lip/identity",
                get(|| async { AxumJson(json!({"protocol": "lip/1", "lip_major": 1})) }),
            )
            .route(
                "/lip/definition",
                post(move || {
                    let hash = hash.clone();
                    async move {
                        AxumJson(json!({
                            "verified_blob_sha": hash,
                            "results": [{"path": "a.rb", "line": 1, "col": 0, "end_line": 1, "end_col": 6}]
                        }))
                    }
                }),
            );
        let (addr, _server) = mock_kb_server(router).await;
        let state = test_state(
            vec![provider_entry("ruby", format!("http://{addr}"))],
            root.path(),
            "fixture",
        )
        .await;

        // rev = Some(...) — the LSP server has no notion of a historical
        // blob, so this tier must be a deliberate no-op here.
        let extra = lsp_live_definitions(&state, &repo, "a.rb", Some("HEAD"), 1, 4).await;
        assert!(extra.is_empty());
    }

    #[test]
    fn prepend_candidates_dedups_against_an_existing_location() {
        let mut out = crate::resolve::ResolveOut {
            schema: crate::resolve::RESOLVE_SCHEMA,
            ident: "widget".to_string(),
            position: crate::resolve::Position { line: 1, col: 0 },
            role: Some("ref".to_string()),
            candidates: vec![crate::resolve::Candidate {
                repo: "fixture".to_string(),
                path: "a.rb".to_string(),
                line: 1,
                kind: Some("fn".to_string()),
                container: None,
                signature: None,
                doc: None,
                precision: crate::resolve::PRECISION_FILE_LOCAL,
                class: crate::resolve::CLASS_LIKELY,
            }],
            total: 1,
            note: crate::resolve::RESOLVE_NOTE,
        };
        let extra = vec![
            // Same location as the existing candidate — must be dropped.
            crate::resolve::Candidate {
                repo: "fixture".to_string(),
                path: "a.rb".to_string(),
                line: 1,
                kind: None,
                container: None,
                signature: None,
                doc: None,
                precision: crate::resolve::PRECISION_LSP_LIVE,
                class: crate::resolve::CLASS_EXACT,
            },
            // A genuinely new location — must be prepended, ranked FIRST.
            crate::resolve::Candidate {
                repo: "fixture".to_string(),
                path: "b.rb".to_string(),
                line: 5,
                kind: None,
                container: None,
                signature: None,
                doc: None,
                precision: crate::resolve::PRECISION_LSP_LIVE,
                class: crate::resolve::CLASS_EXACT,
            },
        ];
        prepend_candidates(&mut out, extra);
        assert_eq!(
            out.candidates.len(),
            2,
            "the duplicate location must be dropped"
        );
        assert_eq!(out.total, 2);
        assert_eq!(
            out.candidates[0].path, "b.rb",
            "the new lsp-live hit ranks FIRST"
        );
        assert_eq!(
            out.candidates[0].precision,
            crate::resolve::PRECISION_LSP_LIVE
        );
        assert_eq!(
            out.candidates[1].path, "a.rb",
            "the pre-existing candidate stays, unduplicated"
        );
    }

    // --- diagnostics_for: null-vs-empty ---------------------------------

    #[tokio::test]
    async fn diagnostics_for_reports_null_when_no_provider_is_configured() {
        let (root, repo) = fixture_repo("fixture");
        write_file(root.path(), "a.rb", "puts 1\n");
        let state = test_state(vec![], root.path(), "fixture").await;
        let out = diagnostics_for(&state, &repo, "a.rb").await;
        assert!(out.diagnostics.is_none());
        assert_eq!(out.unavailable_reason, Some("no_provider_configured"));
        assert!(!out.fetched);
    }

    #[tokio::test]
    async fn diagnostics_for_distinguishes_null_from_an_empty_clean_result() {
        let (root, repo) = fixture_repo("fixture");
        write_file(root.path(), "a.rb", "puts 1\n");
        let hash = crate::ingest::git_blob_hash(b"puts 1\n");
        let router = Router::new()
            .route(
                "/lip/identity",
                get(|| async {
                    AxumJson(json!({"protocol": "lip/1", "lip_major": 1, "capabilities": ["diagnostics"]}))
                }),
            )
            .route(
                "/lip/diagnostics",
                post(move || {
                    let hash = hash.clone();
                    async move { AxumJson(json!({"verified_blob_sha": hash, "results": []})) }
                }),
            );
        let (addr, _server) = mock_kb_server(router).await;
        let state = test_state(
            vec![provider_entry("ruby", format!("http://{addr}"))],
            root.path(),
            "fixture",
        )
        .await;
        let out = diagnostics_for(&state, &repo, "a.rb").await;
        assert!(out.fetched);
        assert_eq!(
            out.diagnostics,
            Some(vec![]),
            "provider says clean must be Some([]), not null"
        );
        assert!(out.unavailable_reason.is_none());
        assert_eq!(out.provider.as_deref(), Some("ruby"));
    }

    #[tokio::test]
    async fn diagnostics_for_reports_provider_unavailable_on_refusal() {
        let (root, repo) = fixture_repo("fixture");
        write_file(root.path(), "a.rb", "puts 1\n");
        let router = Router::new()
            .route(
                "/lip/identity",
                get(|| async {
                    AxumJson(json!({"protocol": "lip/1", "lip_major": 1, "capabilities": ["diagnostics"]}))
                }),
            )
            .route(
                "/lip/diagnostics",
                post(|| async { AxumJson(json!({"refused": "server_down"})) }),
            );
        let (addr, _server) = mock_kb_server(router).await;
        let state = test_state(
            vec![provider_entry("ruby", format!("http://{addr}"))],
            root.path(),
            "fixture",
        )
        .await;
        let out = diagnostics_for(&state, &repo, "a.rb").await;
        assert!(out.diagnostics.is_none());
        assert_eq!(out.unavailable_reason, Some("provider_unavailable"));
        assert_eq!(out.provider.as_deref(), Some("ruby"));
    }

    // --- S2-C: LipClient::code_actions --------------------------------

    #[tokio::test]
    async fn code_actions_capability_gate_reports_capability_absent_and_never_calls_out() {
        let hit = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hit_clone = hit.clone();
        let router = Router::new()
            .route(
                "/lip/identity",
                // No "code-actions" in capabilities — an adapter build
                // that predates S2-C.
                get(|| async {
                    AxumJson(
                        json!({"protocol": "lip/1", "lip_major": 1, "capabilities": ["hover"]}),
                    )
                }),
            )
            .route(
                "/lip/code-actions",
                post(move || {
                    let hit = hit_clone.clone();
                    async move {
                        hit.store(true, std::sync::atomic::Ordering::SeqCst);
                        AxumJson(json!({"verified_blob_sha": "x", "results": []}))
                    }
                }),
            );
        let (addr, _server) = mock_kb_server(router).await;
        let client = LipClient::new("ruby".to_string(), format!("http://{addr}"));
        let err = client
            .code_actions("a.rb", "x", 1, 0, None, None, None)
            .await
            .unwrap_err();
        assert_eq!(err, CodeActionsFailure::CapabilityAbsent);
        assert!(
            !hit.load(std::sync::atomic::Ordering::SeqCst),
            "an unsupported capability must never even reach the endpoint"
        );
    }

    #[tokio::test]
    async fn code_actions_reports_unavailable_on_refusal() {
        let router = Router::new()
            .route(
                "/lip/identity",
                get(|| async {
                    AxumJson(
                        json!({"protocol": "lip/1", "lip_major": 1, "capabilities": ["code-actions"]}),
                    )
                }),
            )
            .route(
                "/lip/code-actions",
                post(|| async { AxumJson(json!({"refused": "blob_mismatch"})) }),
            );
        let (addr, _server) = mock_kb_server(router).await;
        let client = LipClient::new("ruby".to_string(), format!("http://{addr}"));
        let err = client
            .code_actions("a.rb", "x", 1, 0, None, None, None)
            .await
            .unwrap_err();
        assert_eq!(err, CodeActionsFailure::Unavailable);
    }

    #[tokio::test]
    async fn code_actions_parses_a_successful_response_incl_dropped_counters() {
        let router = Router::new()
            .route(
                "/lip/identity",
                get(|| async {
                    AxumJson(
                        json!({"protocol": "lip/1", "lip_major": 1, "capabilities": ["code-actions"]}),
                    )
                }),
            )
            .route(
                "/lip/code-actions",
                post(|| async {
                    AxumJson(json!({
                        "verified_blob_sha": "deadbeef",
                        "results": [{"title": "Add missing import", "kind": "quickfix",
                            "is_preferred": true, "edits": [{"path": "src/lib.rs", "edits": [
                                {"start_line": 1, "start_col": 0, "end_line": 1, "end_col": 0,
                                 "new_text": "use foo;\n"}]}]}],
                        "dropped_command_only": 2,
                        "dropped_unsupported": 1,
                    }))
                }),
            );
        let (addr, _server) = mock_kb_server(router).await;
        let client = LipClient::new("ruby".to_string(), format!("http://{addr}"));
        let answer = client
            .code_actions("src/lib.rs", "deadbeef", 1, 0, None, None, None)
            .await
            .unwrap();
        assert_eq!(answer.verified_blob_sha, "deadbeef");
        assert_eq!(answer.dropped_command_only, 2);
        assert_eq!(answer.dropped_unsupported, 1);
        assert_eq!(answer.results.as_array().unwrap().len(), 1);
    }

    #[test]
    fn verify_blob_freshness_sha_matches_a_fresh_read() {
        let (root, repo) = fixture_repo("fixture");
        write_file(root.path(), "a.rb", "puts 1\n");
        let hash = crate::ingest::git_blob_hash(b"puts 1\n");
        assert!(verify_blob_freshness_sha(&repo, "a.rb", &hash));
        assert!(!verify_blob_freshness_sha(&repo, "a.rb", "not-the-hash"));
    }
}
