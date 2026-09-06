//! W2.15b — the tribal-knowledge proposal inbox.
//!
//! A post-session memory CANDIDATE (an agent skill's "I think this is worth
//! remembering") lands in a reviewable queue instead of being written
//! straight into a memory corpus. Approving one fires the EXISTING
//! memory-write path (`routes::artifacts::ingest`, the core `kb remember`
//! uses) with the candidate's provenance carried along; the human gate IS
//! the invariant — no in-daemon LLM ever writes a memory on its own
//! (root CLAUDE.md non-goals), so a candidate stays inert JSON until a
//! human (or an explicit `kb proposals approve`) says yes. Mirrors the
//! `/kb-distill` + `/kb-reflect` "propose in a table, stop for approval"
//! precedent, mechanized as a daemon-side queue instead of a chat turn.
//!
//! Storage — recon option 2: a per-kb sidecar dir
//! `<state>/<kb>/.proposals/<id>.json`, one `kb-proposal/1` JSON file per
//! candidate, guarded by its own per-kb `AsyncMutex`
//! (`KbHandles::proposal_lock_for`, the `review_locks` map pattern —
//! invariant #6's SHAPE, but a distinct lock: proposals never touch
//! `.review/`). The fleet-wide GET clones `routes::inbox::collect_open`'s
//! spawn_blocking walk shape: one blocking directory scan per kb, fanned
//! out via `routes::buffered_join` (invariant #28), tolerant of any
//! non-`kb-proposal/1` JSON in the dir (skipped, never 500s the fleet).

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{Response, StatusCode},
    response::IntoResponse,
    Json,
};
use kb_core::types::KbName;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

/// Schema discriminator, mirroring `kb_core::review::SCHEMA`'s convention.
pub const SCHEMA: &str = "kb-proposal/1";

/// Fleet-wide GET hard cap (recon spec: "cap 200") — not a `?limit=` knob,
/// just a backstop so an unreviewed backlog can't blow out one response.
const FLEET_CAP: usize = 200;

/// Who authored the candidate. The daemon always stamps `Agent` on
/// [`submit`] today (the route is the agent-layer submit verb); `Human` is
/// carried in the schema for a future SPA-compose path, per the recon's
/// forward-compatible field list.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProposalSource {
    Agent,
    Human,
}

/// One queued memory candidate — the `kb-proposal/1` on-disk shape. Field
/// set is the recon spec verbatim: everything `IngestBody` (the memory-write
/// wire body) needs, plus the queue's own `id`/`schema`/`created_at`/
/// `source`/`note`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    /// `p_` + 12 hex chars, minted at submit time.
    pub id: String,
    pub schema: String,
    /// Unix seconds.
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub session_id: Option<String>,
    pub title: String,
    /// Markdown — rendered to HTML at approve time via
    /// `kb_core::markdown::render_comment_fragment` (the same untrusted-body
    /// renderer `.review` comment bodies use: raw HTML is escaped, not
    /// passed through, since a candidate is agent-authored, not the
    /// operator's own text).
    pub body: String,
    #[serde(default = "default_category")]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Resolved at submit time the same way `kb remember`/`IngestBody`
    /// resolve it (`--global` unless `--link` scopes it) — stored already
    /// resolved so approve never has to re-derive it.
    #[serde(default = "default_true")]
    pub global: bool,
    #[serde(default)]
    pub linked_kbs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub salience: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub supersedes: Option<String>,
    pub source: ProposalSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub note: Option<String>,
}

fn default_category() -> String {
    "memory-user".to_string()
}

fn default_true() -> bool {
    true
}

/// `POST /api/kb/{kb}/proposals` request body — everything [`Proposal`]
/// carries except the queue-assigned `id`/`schema`/`created_at`/`source`.
#[derive(Debug, Deserialize)]
pub struct NewProposalBody {
    pub title: String,
    pub body: String,
    #[serde(default = "default_category")]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Absent ⇒ resolved the same way `IngestBody.global` is: `true`
    /// unless `linked_kbs` is non-empty.
    pub global: Option<bool>,
    #[serde(default)]
    pub linked_kbs: Vec<String>,
    pub salience: Option<f32>,
    pub supersedes: Option<String>,
    pub session_id: Option<String>,
    pub note: Option<String>,
}

/// One row of the fleet-wide `GET /api/proposals` list — a [`Proposal`]
/// plus the `kb` it lives in (the on-disk file itself doesn't carry `kb`,
/// same as `kb-comments/1`'s directory-implies-kb shape; the fleet response
/// re-attaches it for context, mirroring `routes::inbox::InboxItem`).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct ProposalItem {
    pub kb: String,
    #[serde(flatten)]
    pub proposal: Proposal,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct ProposalsResponse {
    pub items: Vec<ProposalItem>,
    /// Fleet-wide count BEFORE the [`FLEET_CAP`] truncation.
    pub total: u64,
}

#[derive(Debug, Deserialize, Default)]
pub struct ProposalsParams {
    /// Restrict to a single corpus. Absent → every configured kb.
    pub kb: Option<String>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct ApproveResponse {
    pub outcome: String,
    pub artifact_id: String,
    pub path: String,
}

/// `POST /api/kb/{kb}/proposals` — the agent-layer submit verb (`kb
/// propose`). Same auth/origin posture as every other state-changing route
/// (loopback bypass + `auth_bearer` + `origin_allowlist`, applied by the
/// router's outer layers — nothing route-local to add).
pub async fn submit(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Json(body): Json<NewProposalBody>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let title = body.title.trim();
    if title.is_empty() {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "proposal title must not be empty".into(),
        ));
    }
    if body.body.trim().is_empty() {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "proposal body must not be empty".into(),
        ));
    }
    for k in &body.linked_kbs {
        let target = match KbName::new(k) {
            Ok(k) => k,
            Err(e) => return error_to_problem_json(&e),
        };
        if !state.kbs.contains_key(&target) {
            return error_to_problem_json(&kb_core::Error::BadRequest(format!(
                "linked_kbs: unknown kb '{k}'"
            )));
        }
    }

    let global = body.global.unwrap_or(body.linked_kbs.is_empty());
    let salience = body.salience.map(|s| s.clamp(0.0, 1.0));
    let proposal = Proposal {
        id: new_proposal_id(),
        schema: SCHEMA.to_string(),
        created_at: chrono::Utc::now().timestamp(),
        session_id: body.session_id.clone(),
        title: title.to_string(),
        body: body.body.clone(),
        category: body.category.clone(),
        tags: body.tags.clone(),
        global,
        linked_kbs: body.linked_kbs.clone(),
        salience,
        supersedes: body.supersedes.clone(),
        source: ProposalSource::Agent,
        note: body.note.clone(),
    };

    let path = state.paths.kb_proposal_file(&kb_name, &proposal.id);
    let lock = state.proposal_lock_for(&kb_name);
    let guard = lock.lock().await;
    let saved = save_proposal_atomic(&path, &proposal);
    drop(guard);
    if let Err(e) = saved {
        return error_to_problem_json(&e);
    }

    ctx.bus.emit(
        "proposal.created",
        json!({ "kb": kb_name.as_str(), "id": proposal.id.clone(), "title": proposal.title.clone() }),
    );
    (StatusCode::CREATED, Json(proposal)).into_response()
}

/// `GET /api/proposals?kb=` — every queued proposal across the fleet (or
/// one kb), newest first. Federated per corpus (invariant #28).
pub async fn list(
    State(state): State<Arc<KbHandles>>,
    Query(params): Query<ProposalsParams>,
) -> impl IntoResponse {
    let want_kb = params
        .kb
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let mut futs: Vec<super::CorpusFut<'_, Vec<ProposalItem>>> = Vec::new();
    for kb_name in state.kbs.keys() {
        if let Some(w) = want_kb {
            if kb_name.as_str() != w {
                continue;
            }
        }
        let dir = state.paths.kb_proposals_dir(kb_name);
        let kb_owned = kb_name.as_str().to_string();
        futs.push(Box::pin(
            async move { collect_proposals(kb_owned, dir).await },
        ));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let mut items: Vec<ProposalItem> = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect();

    let total = items.len() as u64;
    // Newest-submitted first; deterministic tiebreak (mirrors inbox::list).
    items.sort_by(|a, b| {
        b.proposal
            .created_at
            .cmp(&a.proposal.created_at)
            .then_with(|| a.kb.cmp(&b.kb))
            .then_with(|| a.proposal.id.cmp(&b.proposal.id))
    });
    items.truncate(FLEET_CAP);

    Json(ProposalsResponse { items, total })
}

/// Walk one kb's `.proposals/` dir. Best-effort per invariant #28: a
/// missing dir, an unreadable file, or a non-`kb-proposal/1` JSON blob is
/// silently skipped — this future must NEVER `?`-propagate (one bad corpus
/// can't 500 the fleet). Blocking IO runs in ONE `spawn_blocking`, off the
/// tokio worker (same shape as `routes::inbox::collect_open`).
async fn collect_proposals(kb: String, dir: std::path::PathBuf) -> Vec<ProposalItem> {
    tokio::task::spawn_blocking(move || {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };
        let mut paths: Vec<std::path::PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
            .collect();
        // Deterministic within-kb order before the cross-kb sort above.
        paths.sort();
        paths
            .into_iter()
            .filter_map(|path| match load_proposal(&path) {
                Ok(Some(proposal)) => Some(ProposalItem {
                    kb: kb.clone(),
                    proposal,
                }),
                // Malformed / non-kb-proposal/1 JSON (or a vanished file) —
                // skip, never fail the walk.
                _ => None,
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// Resolve `{kb}` + validate the `{id}` path segment for approve/reject —
/// mirrors `routes::comments::validate`.
#[allow(clippy::result_large_err)]
fn validate(
    state: &KbHandles,
    kb: &str,
    id: &str,
) -> Result<(KbName, std::path::PathBuf), Response<Body>> {
    let (kb_name, _ctx) = crate::routes::resolve_kb(state, kb)?;
    if !super::is_safe_id(id) {
        return Err(error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "proposal id {id:?} contains illegal characters"
        ))));
    }
    let path = state.paths.kb_proposal_file(&kb_name, id);
    Ok((kb_name, path))
}

/// `POST /api/kb/{kb}/proposals/{id}/approve` — re-validates (by calling
/// the SAME [`crate::routes::artifacts::ingest`] the `POST …/artifacts`
/// handler uses, which re-runs its title/linked_kbs checks against the
/// CURRENT fleet), writes the memory artifact, deletes the proposal file,
/// and emits `proposal.resolved`.
pub async fn approve(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
) -> Response<Body> {
    let (kb_name, path) = match validate(&state, &kb, &id) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let Some(ctx) = state.kbs.get(&kb_name) else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!("kb {kb_name}")));
    };

    let lock = state.proposal_lock_for(&kb_name);
    let guard = lock.lock().await;
    let proposal = match load_proposal(&path) {
        Ok(Some(p)) => p,
        Ok(None) => {
            drop(guard);
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "no proposal {kb_name}/{id}"
            )));
        }
        Err(e) => {
            drop(guard);
            return error_to_problem_json(&e);
        }
    };

    let ingest_body = crate::routes::artifacts::IngestBody {
        title: proposal.title.clone(),
        body: None,
        body_html: Some(kb_core::markdown::render_comment_fragment(&proposal.body)),
        category: proposal.category.clone(),
        tags: proposal.tags.clone(),
        salience: proposal.salience,
        decay: None,
        supersedes: proposal.supersedes.clone(),
        summary: None,
        session_id: proposal.session_id.clone(),
        global: Some(proposal.global),
        linked_kbs: proposal.linked_kbs.clone(),
        // U3 — no highlight provenance on this path, deliberately. An
        // approved proposal has no source artifact and no selection
        // anchor: it came from a session, and `session_id` above already
        // records that origin honestly. Stamping `author` here would also
        // be a judgement call this phase's ruling didn't make (the body is
        // agent-written, the approval is human), and leaving all four
        // `None` keeps the rendered memory byte-identical to pre-U3.
        author: None,
        source_kb: None,
        source_artifact: None,
        source_anchor: None,
        // MI-W3.3a / MI-W3.4 — neither is part of the proposal schema
        // (W2.15b); a candidate has no typed classification or write-time
        // trust tag to carry through approval. Absent, not inferred.
        source: None,
        memory_type: None,
        // CT-C3 — same rule: `outcome` isn't part of the proposal schema; a
        // failed-approach candidate is written directly via `kb remember
        // --failed`, never inferred at approval time.
        outcome: None,
    };
    let ingested = match crate::routes::artifacts::ingest(&state, &kb_name, ctx, ingest_body) {
        Ok(r) => r,
        Err(resp) => {
            drop(guard);
            return resp;
        }
    };

    // The memory write already happened — a failure to remove the queue
    // file is logged, not surfaced as an error (the alternative, telling
    // the caller "approve failed" after the memory landed, is worse).
    if let Err(e) = std::fs::remove_file(&path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(kb = %kb_name, id = %id, error = %e, "failed to remove approved proposal file");
        }
    }
    drop(guard);

    ctx.bus.emit(
        "proposal.resolved",
        json!({
            "kb": kb_name.as_str(),
            "id": id,
            "outcome": "approved",
            "artifact_id": ingested.id.clone(),
        }),
    );
    (
        StatusCode::OK,
        Json(ApproveResponse {
            outcome: "approved".to_string(),
            artifact_id: ingested.id,
            path: ingested.path,
        }),
    )
        .into_response()
}

/// `POST /api/kb/{kb}/proposals/{id}/reject` — discard a proposal. 204 on
/// success, 404 when the id is unknown.
pub async fn reject(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
) -> Response<Body> {
    let (kb_name, path) = match validate(&state, &kb, &id) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let Some(ctx) = state.kbs.get(&kb_name) else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!("kb {kb_name}")));
    };

    let lock = state.proposal_lock_for(&kb_name);
    let guard = lock.lock().await;
    match load_proposal(&path) {
        Ok(Some(_)) => {}
        Ok(None) => {
            drop(guard);
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "no proposal {kb_name}/{id}"
            )));
        }
        Err(e) => {
            drop(guard);
            return error_to_problem_json(&e);
        }
    }
    if let Err(e) = std::fs::remove_file(&path) {
        drop(guard);
        return error_to_problem_json(&kb_core::Error::Storage(format!(
            "remove {}: {e}",
            path.display()
        )));
    }
    drop(guard);

    ctx.bus.emit(
        "proposal.resolved",
        json!({ "kb": kb_name.as_str(), "id": id, "outcome": "rejected" }),
    );
    StatusCode::NO_CONTENT.into_response()
}

// --- pure helpers (id minting, load/save, schema validation) --------------

/// Fresh proposal id: `p_` + 12 hex chars. No `getrandom` dependency in
/// kb-server (kb-core's `short_random_hex` is `pub(crate)` there, scoped to
/// review/lists id minting) — hashes a handful of cheap entropy sources
/// (wall-clock nanos, pid, a per-process call counter, and a stack address
/// as a per-call salt) through the `sha2` this crate already links. Only
/// needs to avoid collisions within one kb's `.proposals/` dir, not resist
/// an adversary.
fn new_proposal_id() -> String {
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h = Sha256::new();
    h.update(nanos.to_le_bytes());
    h.update(std::process::id().to_le_bytes());
    h.update(n.to_le_bytes());
    let salt = &h as *const Sha256 as usize;
    h.update(salt.to_le_bytes());
    format!("p_{}", hex::encode(&h.finalize()[..6]))
}

/// Load + parse a proposal file. `Ok(None)` on a missing file (normal —
/// most callers first check existence via this same fn); `Err` on
/// unreadable/malformed JSON or a schema mismatch — callers that walk a
/// whole directory (`collect_proposals`) treat every `Err` as "skip", never
/// propagate.
fn load_proposal(path: &std::path::Path) -> kb_core::Result<Option<Proposal>> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let proposal: Proposal = serde_json::from_slice(&bytes)?;
    if proposal.schema != SCHEMA {
        return Err(kb_core::Error::BadRequest(format!(
            "proposal schema {:?} not supported (expected {SCHEMA})",
            proposal.schema
        )));
    }
    Ok(Some(proposal))
}

/// Atomic write: tmp sibling + rename (same-filesystem rename is atomic on
/// POSIX), mirroring `routes::attachments::write_blob_atomic`. Caller holds
/// the per-kb proposal lock.
fn save_proposal_atomic(path: &std::path::Path, proposal: &Proposal) -> kb_core::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(proposal)?;
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let tmp = parent.join(format!(
        "{}.tmp.{}",
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("proposal"),
        std::process::id()
    ));
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: &str) -> Proposal {
        Proposal {
            id: id.to_string(),
            schema: SCHEMA.to_string(),
            created_at: 1_700_000_000,
            session_id: Some("sess-1".to_string()),
            title: "worth remembering".to_string(),
            body: "the **body**".to_string(),
            category: "memory-user".to_string(),
            tags: vec!["kb".to_string()],
            global: true,
            linked_kbs: Vec::new(),
            salience: Some(0.7),
            supersedes: None,
            source: ProposalSource::Agent,
            note: None,
        }
    }

    #[test]
    fn new_proposal_id_is_prefixed_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..64 {
            let id = new_proposal_id();
            assert!(id.starts_with("p_"), "{id:?} missing p_ prefix");
            assert_eq!(id.len(), 14, "{id:?} not p_ + 12 hex chars");
            assert!(seen.insert(id), "duplicate proposal id minted");
        }
    }

    #[test]
    fn proposal_round_trips_through_save_and_load() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("p_deadbeef0001.json");
        let proposal = sample("p_deadbeef0001");
        save_proposal_atomic(&path, &proposal).unwrap();
        let loaded = load_proposal(&path).unwrap().unwrap();
        assert_eq!(loaded.id, proposal.id);
        assert_eq!(loaded.title, proposal.title);
        assert_eq!(loaded.body, proposal.body);
        assert_eq!(loaded.session_id, proposal.session_id);
        assert_eq!(loaded.salience, proposal.salience);
        assert!(loaded.global);
        assert_eq!(loaded.source, ProposalSource::Agent);
        // No leftover tmp sibling after the atomic rename.
        let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(
            leftovers,
            vec![std::ffi::OsString::from("p_deadbeef0001.json")]
        );
    }

    #[test]
    fn load_proposal_missing_file_is_ok_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load_proposal(&tmp.path().join("nope.json"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn load_proposal_rejects_wrong_schema() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bad.json");
        let mut proposal = sample("p_bad");
        proposal.schema = "kb-proposal/2".to_string();
        std::fs::write(&path, serde_json::to_vec(&proposal).unwrap()).unwrap();
        let err = load_proposal(&path).unwrap_err();
        assert!(matches!(err, kb_core::Error::BadRequest(_)));
    }

    #[test]
    fn load_proposal_rejects_malformed_json() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("junk.json");
        std::fs::write(&path, b"{not json").unwrap();
        assert!(load_proposal(&path).is_err());
    }

    #[tokio::test]
    async fn collect_proposals_skips_junk_and_sorts_by_filename() {
        let tmp = tempfile::tempdir().unwrap();
        // A valid proposal…
        save_proposal_atomic(&tmp.path().join("p_aaa.json"), &sample("p_aaa")).unwrap();
        // …a malformed JSON file…
        std::fs::write(tmp.path().join("p_bbb.json"), b"{not json").unwrap();
        // …a wrong-schema file…
        let mut wrong = sample("p_ccc");
        wrong.schema = "kb-proposal/2".to_string();
        std::fs::write(
            tmp.path().join("p_ccc.json"),
            serde_json::to_vec(&wrong).unwrap(),
        )
        .unwrap();
        // …and a non-JSON sidecar (e.g. a stray tmp file that never got
        // cleaned up).
        std::fs::write(tmp.path().join("p_ddd.txt"), b"not json at all").unwrap();

        let items = collect_proposals("smoke".to_string(), tmp.path().to_path_buf()).await;
        assert_eq!(items.len(), 1, "only the one valid proposal survives");
        assert_eq!(items[0].proposal.id, "p_aaa");
        assert_eq!(items[0].kb, "smoke");
    }

    #[tokio::test]
    async fn collect_proposals_missing_dir_is_empty_not_error() {
        let tmp = tempfile::tempdir().unwrap();
        let items = collect_proposals("smoke".to_string(), tmp.path().join("nope")).await;
        assert!(items.is_empty());
    }

    #[test]
    fn proposal_source_serializes_lowercase() {
        assert_eq!(
            serde_json::to_value(ProposalSource::Agent).unwrap(),
            serde_json::json!("agent")
        );
        assert_eq!(
            serde_json::to_value(ProposalSource::Human).unwrap(),
            serde_json::json!("human")
        );
    }
}
