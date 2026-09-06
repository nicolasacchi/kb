//! DCB W1.C — `PUT`/`DELETE /api/doc-lens/pin`.
//!
//! **The pin IS the remembered read-time choice** (Decision 1): which
//! CHECKOUT a human picked for a document. It is deliberately NOT a
//! resolution cache — nothing about what that pick resolved to is ever
//! stored, because no cached verdict may outlive the tree it was computed
//! against.
//!
//! D-A moved this route OFF the loopback-only sub-router onto `auth_bearer`
//! so kb's own reader (a different origin in prod) can write it. It is an
//! operator PREFERENCE, not a tree/ref mutation — the bookmarks (V0011)
//! precedent, not the checkout/review-ref one. `POST /api/doc-lens/sync`
//! (W3.A) does NOT follow it: that route drives a bulk pull of doc prose
//! into this daemon's store and STAYS loopback-only.
//!
//! **W2.A adds the LIFECYCLE** (amendment 11 / E7): a boot prune, a read-time
//! re-check (in `resolve::select_repo`), and `GET /api/doc-lens/pins`. The
//! write itself is W1.D's (R5); nothing here mints a pin.
//!
//! **W3.A adds an invariant this module must now uphold too: a claim exists
//! only under a live pin.** `doclens::sync`'s `doc_refs` reverse index is
//! keyed on `(kb, doc_id)` with no foreign key back to `doc_lens_pins` —
//! nothing enforces this at the schema level, so every pin-removal path here
//! (`delete_pin_route`, `prune_stale_pins`) must drop the doc's `doc_refs`
//! rows in the SAME call that drops its pin. Leaving them behind orphans them
//! (unpinned, so no future sync pass ever revisits them to correct them) and,
//! worse, lets a `store::upsert_repo` re-point (`ON CONFLICT(name) DO UPDATE
//! SET root` — the exact reused-repo-id shape `prune_stale_pins` already
//! guards the PIN itself against) serve a persisted claim as a live link
//! against a tree it was never resolved on: a verdict outliving the tree it
//! was computed against, which is the one thing this whole feature refuses
//! to produce.

use std::path::Path;

use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};

use super::{gate, validate_kb_segment, PINS_SCHEMA, PIN_SCHEMA};
use crate::config::RepoEntry;
use crate::routes::{find_repo, ApiError};
use crate::state::SharedState;
use crate::store::{DocLensPin, Store, StoreBlocking, StoreError};

#[derive(Debug, Deserialize)]
pub struct PutPinBody {
    pub kb: String,
    /// kb's ARTIFACT ID (D14).
    pub doc: String,
    pub repo: String,
    /// `coderef/1`'s `doc_hash` at pin time; informational in v1 (W3.C's
    /// "doc changed since" is the consumer).
    #[serde(default)]
    pub doc_hash: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PinOut {
    pub schema: &'static str,
    pub kb: String,
    pub doc_id: String,
    pub repo: String,
    /// The filesystem root the pin was CHOSEN against — what makes "same
    /// name, re-pointed at a different checkout" detectable at read time.
    pub repo_root: String,
    pub doc_hash: Option<String>,
    pub pinned_at: i64,
}

impl From<DocLensPin> for PinOut {
    fn from(p: DocLensPin) -> Self {
        Self {
            schema: PIN_SCHEMA,
            kb: p.kb,
            doc_id: p.doc_id,
            repo: p.repo,
            repo_root: p.repo_root,
            doc_hash: p.doc_hash,
            pinned_at: p.pinned_at,
        }
    }
}

/// `PUT /api/doc-lens/pin` — remember a checkout for this document.
pub async fn put_pin_route(
    State(state): State<SharedState>,
    Json(body): Json<PutPinBody>,
) -> Result<impl IntoResponse, ApiError> {
    // The SAME `[doclens] kbs` allowlist + segment validators the read
    // routes use — a pin for a corpus this daemon will not read is never
    // stored.
    gate(&state.doclens, &body.kb, &body.doc)?;
    // 404 for an unknown repo — a pin naming a repo this daemon doesn't have
    // is never stored.
    let (entry, _repo_id) = find_repo(&state, &body.repo)?;
    let pin = DocLensPin {
        kb: body.kb.clone(),
        doc_id: body.doc.clone(),
        repo: entry.name.clone(),
        repo_root: entry.path.to_string_lossy().to_string(),
        doc_hash: body.doc_hash.clone(),
        pinned_at: chrono::Utc::now().timestamp(),
    };
    let pin_for_write = pin.clone();
    state
        .store
        .run_blocking(move |store| store.put_doc_lens_pin(&pin_for_write))
        .await?;
    state.bus.emit(
        "doclens.pin.changed",
        serde_json::json!({ "kb": pin.kb, "doc_id": pin.doc_id, "repo": pin.repo }),
    );
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(PinOut::from(pin)),
    ))
}

#[derive(Debug, Deserialize)]
pub struct DeletePinParams {
    pub kb: String,
    pub doc: String,
}

/// `DELETE /api/doc-lens/pin?kb=&doc=` — idempotent: `204` whether or not a
/// row existed.
pub async fn delete_pin_route(
    State(state): State<SharedState>,
    Query(p): Query<DeletePinParams>,
) -> Result<impl IntoResponse, ApiError> {
    gate(&state.doclens, &p.kb, &p.doc)?;
    // A claim exists only under a live pin (module doc): the unpinned doc's
    // `doc_refs` rows must die WITH the pin, or they become immortal
    // orphans — no future sync pass ever revisits an unpinned doc to correct
    // them. Idempotent like the pin delete (a doc with no claims costs one
    // harmless no-op DELETE). Both writes share one blocking-pool round trip
    // (store.rs's 2026-08-31 incident note).
    let kb = p.kb.clone();
    let doc = p.doc.clone();
    let (removed, claims_dropped) = state
        .store
        .run_blocking(move |store| {
            let removed = store.delete_doc_lens_pin(&kb, &doc)?;
            let claims_dropped = store.delete_doc_refs_for_doc(&kb, &doc)?;
            Ok::<_, StoreError>((removed, claims_dropped))
        })
        .await?;
    if removed {
        tracing::info!(
            kb = %p.kb, doc = %p.doc, claims_dropped,
            "doc-lens: pin removed — its doc_refs claims were dropped with it"
        );
        state.bus.emit(
            "doclens.pin.changed",
            serde_json::json!({ "kb": p.kb, "doc_id": p.doc, "repo": serde_json::Value::Null }),
        );
    }
    Ok((
        StatusCode::NO_CONTENT,
        [(header::CACHE_CONTROL, "no-store")],
    ))
}

// --- W2.A: the lifecycle ---------------------------------------------------

/// Is `recorded` (a pin's `repo_root`, captured at pin time) still the SAME
/// filesystem location as the repo's configured root?
///
/// Canonicalises BOTH sides with a lossy-string fallback, matching how
/// `tests/bookmarks_route.rs` canonicalises a configured repo path. Two
/// spellings of one checkout (a symlinked root, a WSL remount)
/// must not read as "the operator re-pointed this repo" — that is
/// kb invariant #27's ethos applied to the pin. When neither side canonicalises
/// (the root has since vanished), the raw strings are compared: an unresolvable
/// root is a mismatch to be reported, not a panic.
pub(crate) fn root_matches(configured: &Path, recorded: &str) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(configured) == canon(Path::new(recorded))
}

/// Drop every pin that can no longer honour Decision 1's "the pin IS the
/// remembered read-time choice":
///   (a) `repo` names a repo that is no longer configured, or
///   (b) `repo_root` differs from the configured root for that name.
///
/// (b) is the load-bearing one. `store::upsert_repo` is
/// `INSERT … ON CONFLICT(name) DO UPDATE SET root`, so re-pointing `path`
/// under a stable name silently RE-USES the repo id — a surviving pin would
/// then pre-select a tree it was never chosen against, which is the
/// silently-wrong-checkout failure the scorecard exists to prevent. A stale
/// pin is strictly worse than no pin: no pin merely shows the scorecard.
/// Deleted, never "marked", with one `tracing::warn!` per row — and, per this
/// module's own invariant (a claim exists only under a live pin), the doc's
/// `doc_refs` rows die in the SAME pass: a stale pin's claims are exactly the
/// "reused repo_id serves stale claims against a tree they were never
/// resolved on" failure this prune exists to prevent, so leaving them behind
/// would defeat the point of pruning the pin at all.
///
/// Called from `lib.rs::bind_and_spawn` immediately after the `upsert_repo`
/// loop and BEFORE the initial index spawn — the only moment a configured root
/// can have changed, since `KbCodeConfig` has no live reload (see `AppState`'s
/// own doc). Returns the number pruned.
pub fn prune_stale_pins(store: &Store, repos: &[RepoEntry]) -> Result<usize, StoreError> {
    let pins = store.list_doc_lens_pins(None)?;
    let mut pruned = 0usize;
    for pin in pins {
        let live = repos.iter().find(|r| r.name == pin.repo);
        let (why, live_root) = match live {
            None => ("repo is no longer configured", String::new()),
            Some(entry) if !root_matches(&entry.path, &pin.repo_root) => (
                "configured repo root was re-pointed",
                entry.path.to_string_lossy().to_string(),
            ),
            Some(_) => continue,
        };
        // Computed BEFORE the pin delete (order is immaterial to either
        // statement, but logging both counts together — "beside pruned" —
        // keeps the two halves of one drop visible on one line).
        let claims_dropped = store.delete_doc_refs_for_doc(&pin.kb, &pin.doc_id)?;
        tracing::warn!(
            kb = %pin.kb,
            doc = %pin.doc_id,
            repo = %pin.repo,
            pinned_root = %pin.repo_root,
            live_root = %live_root,
            claims_dropped,
            "doc-lens: pruning a stale pin at boot ({why}) — its doc_refs claims are dropped with it"
        );
        if store.delete_doc_lens_pin(&pin.kb, &pin.doc_id)? {
            pruned += 1;
        }
    }
    Ok(pruned)
}

#[derive(Debug, Deserialize)]
pub struct ListPinsParams {
    pub kb: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PinListEntry {
    pub kb: String,
    pub doc_id: String,
    pub repo: String,
    pub repo_root: String,
    /// Computed LIVE against `state.repos`. After a boot prune both are always
    /// `true`; the fields exist so the invariant is visible rather than
    /// implicit (and so a mid-life config edit, which cannot happen today,
    /// would be legible rather than silent).
    pub repo_configured: bool,
    pub root_matches: bool,
    pub doc_hash: Option<String>,
    pub pinned_at: i64,
}

#[derive(Debug, Serialize)]
pub struct PinsOut {
    pub schema: &'static str,
    pub pins: Vec<PinListEntry>,
}

/// `GET /api/doc-lens/pins?kb=` — the operator's remembered checkouts.
///
/// Mounted on the doclens READ sub-router (E8), so it inherits `auth_bearer`
/// and the GET `CorsLayer`: it returns the same information class the
/// scorecard already returns cross-origin (kb name, doc id, repo name, root —
/// and `GET /api/repos` already exposes roots), and W1.D/W2.B want it there.
///
/// `kb` is OPTIONAL (unlike every other doc-lens surface): the whole point of
/// the verb is "what have I pinned", which has no single doc to gate on. The
/// allowlist still applies — rows for a kb that is no longer in `[doclens] kbs`
/// are filtered out rather than listed, so removing a corpus from the
/// allowlist closes this surface for it too.
pub async fn list_pins_route(
    State(state): State<SharedState>,
    Query(p): Query<ListPinsParams>,
) -> Result<impl IntoResponse, ApiError> {
    if !state.doclens.enabled() {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "the doc-lens feature is off on this daemon ([doclens] kbs is empty)",
        )
        .with_reason("doclens_disabled"));
    }
    if let Some(kb) = p.kb.as_deref() {
        validate_kb_segment(kb)?;
        if !state.doclens.kb_allowed(kb) {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                format!("kb {kb:?} is not in [doclens] kbs"),
            )
            .with_reason("kb_not_allowlisted"));
        }
    }
    let kb_filter = p.kb.clone();
    let rows = state
        .store
        .run_blocking(move |store| store.list_doc_lens_pins(kb_filter.as_deref()))
        .await?;
    let pins = rows
        .into_iter()
        .filter(|row| state.doclens.kb_allowed(&row.kb))
        .map(|row| {
            let live = state.repos.iter().find(|r| r.name == row.repo);
            PinListEntry {
                repo_configured: live.is_some(),
                root_matches: live.is_some_and(|e| root_matches(&e.path, &row.repo_root)),
                kb: row.kb,
                doc_id: row.doc_id,
                repo: row.repo,
                repo_root: row.repo_root,
                doc_hash: row.doc_hash,
                pinned_at: row.pinned_at,
            }
        })
        .collect();
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(PinsOut {
            schema: PINS_SCHEMA,
            pins,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn store() -> (tempfile::TempDir, Store) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        (tmp, store)
    }

    fn pin(kb: &str, doc: &str, repo: &str, root: &str) -> DocLensPin {
        DocLensPin {
            kb: kb.to_string(),
            doc_id: doc.to_string(),
            repo: repo.to_string(),
            repo_root: root.to_string(),
            doc_hash: None,
            pinned_at: 1_754_500_000,
        }
    }

    fn entry(name: &str, path: &str) -> RepoEntry {
        RepoEntry {
            name: name.to_string(),
            path: PathBuf::from(path),
        }
    }

    #[test]
    fn prune_stale_pins_drops_a_pin_whose_repo_is_no_longer_configured() {
        let (tmp, store) = store();
        let root = tmp.path().to_string_lossy().to_string();
        store
            .put_doc_lens_pin(&pin("platform", "d1", "gone", &root))
            .unwrap();
        assert_eq!(
            prune_stale_pins(&store, &[entry("alpha", &root)]).unwrap(),
            1
        );
        assert!(store.get_doc_lens_pin("platform", "d1").unwrap().is_none());
    }

    #[test]
    fn prune_stale_pins_drops_a_pin_whose_repo_root_was_re_pointed() {
        let (tmp, store) = store();
        let old = tmp.path().join("old");
        let new = tmp.path().join("new");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        store
            .put_doc_lens_pin(&pin("platform", "d1", "alpha", &old.to_string_lossy()))
            .unwrap();
        // `upsert_repo` is ON CONFLICT(name) DO UPDATE SET root, so the repo
        // id survives a re-point — which is exactly why the ROOT, not the
        // name, is what a pin must be validated against.
        let repos = [RepoEntry {
            name: "alpha".into(),
            path: new,
        }];
        assert_eq!(prune_stale_pins(&store, &repos).unwrap(), 1);
        assert!(store.get_doc_lens_pin("platform", "d1").unwrap().is_none());
    }

    #[test]
    fn prune_stale_pins_keeps_a_healthy_pin() {
        let (tmp, store) = store();
        let root = tmp.path().join("alpha");
        std::fs::create_dir_all(&root).unwrap();
        store
            .put_doc_lens_pin(&pin("platform", "d1", "alpha", &root.to_string_lossy()))
            .unwrap();
        let repos = [RepoEntry {
            name: "alpha".into(),
            path: root.clone(),
        }];
        assert_eq!(prune_stale_pins(&store, &repos).unwrap(), 0);
        assert!(store.get_doc_lens_pin("platform", "d1").unwrap().is_some());
        // Idempotent: a second boot prunes nothing.
        assert_eq!(prune_stale_pins(&store, &repos).unwrap(), 0);
    }

    /// Two spellings of ONE checkout are not a re-point (kb invariant #27's
    /// ethos): a symlinked root must never cost the operator their pin.
    #[test]
    fn root_matches_sees_through_a_symlinked_root() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = tmp.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();
        #[cfg(not(unix))]
        std::fs::create_dir_all(&link).unwrap();
        #[cfg(unix)]
        assert!(root_matches(&link, &real.to_string_lossy()));
        assert!(root_matches(&real, &real.to_string_lossy()));
        assert!(!root_matches(
            &real,
            &tmp.path().join("other").to_string_lossy()
        ));
        // A root that no longer exists on either side falls back to a raw
        // string compare rather than panicking.
        assert!(root_matches(Path::new("/nope/gone"), "/nope/gone"));
        assert!(!root_matches(Path::new("/nope/gone"), "/nope/other"));
    }
}
