//! DCB W3.A — `doc_refs`, the reverse "cited by" index, plus the background
//! sync that fills it and the read route that re-validates it.
//!
//! ## What this module is
//!
//! [`super::resolve`] answers "what code does THIS document cite?", live and
//! uncached. This module answers the mirror question — "which documents cite
//! THIS file?" — which cannot be computed live: nothing in a repo points back
//! at a corpus. So it is the ONE place in the doc-lens design that persists
//! anything derived from a resolution, and it does so under three rules that
//! keep it from becoming the cached-verdict store the whole feature refuses
//! to be:
//!
//! * **A row is a CLAIM, not a verdict.** `doc_refs.resolved_path` records
//!   "a file at this exact path existed in this repo when we looked", the
//!   least verdict-like fact this system can produce. Every read
//!   ([`doc_refs_route`]) re-validates it against the LIVE `files` table, so
//!   a rotted claim renders "cited, path no longer present" and never a live
//!   link. Nothing here is ever trusted as current truth on its own.
//! * **Only `path_state == "present"` (a UNIQUE match) gets a row.** The PK
//!   `(repo_id, kb, doc_id, ordinal)` admits exactly one resolved path per
//!   ordinal, and an `ambiguous` ref has N candidates with no single honest
//!   answer to store — widening the PK to hold them would mean persisting a
//!   resolution CLASS, which is exactly what this design forbids.
//!   Ambiguous/absent/external refs stay fully visible in the LIVE lens with
//!   its search escape hatch; the reverse index simply does not duplicate
//!   that disambiguation-needed UX in a background-synced table.
//! * **The index only ever covers PINNED docs** — i.e. documents a human has
//!   opened and picked a checkout for. This is a "grows as you read" index,
//!   not a corpus scan, matching kb's own no-full-corpus-background-scan
//!   posture. It is a deliberate design property, not an oversight: a doc
//!   nobody has pinned has no checkout to be honest about, and guessing one
//!   is the exact failure the pin exists to prevent (Decision 1).
//!
//! ## The pass, and why it fetches each body exactly once
//!
//! A pass walks each pinned kb's `coderef/1` cursor feed in HEADER mode
//! (`?refs=0`) — the cheap what-changed walk W1.B built that flag for. The
//! persisted cursor means a page only ever contains docs whose `extracted_at`
//! advanced past the last pass, so **the walk IS the change detection**:
//! "appeared in this page" and "changed since we last looked" are the same
//! predicate, and a header is used for exactly two things — its `doc_id` (is
//! it pinned?) and the `next_cursor` bookkeeping.
//!
//! For a PINNED doc the pass then calls [`super::resolve::resolve_lens`] and
//! **nothing else**. There is deliberately NO `KbClient::code_refs` call
//! here: `resolve_lens` fetches the body itself, so an explicit pre-fetch
//! would pay for every ref body TWICE and run two layers of the same side
//! effects. `resolve_lens` also already OWNS both side effects this pass
//! would otherwise duplicate — the 404 pin-drop and the
//! body-authoritative moves re-key (D13/R13; never a parsed redirect URL) —
//! so sync's job shrinks to mapping those outcomes onto its own table:
//! `reason == "doc_not_found"` ⇒ drop this doc's claims; any other error ⇒
//! record it against the kb and leave its claims alone (an unreachable kb is
//! not evidence that a doc stopped citing code); `Ok` ⇒ replace this doc's
//! claims, keyed on `lens.doc_id` (never the id we asked for — a re-keyed pin
//! would otherwise orphan a row no future pass revisits).
//! `sync_fetches_each_pinned_doc_body_exactly_once` is the regression guard.
//!
//! ## The same-second gap, and why each PASS rewinds one second
//!
//! kb's feed cursor is `"<extracted_at>:<artifact_id>"` with second-
//! granularity wall-clock and an artifact_id tiebreak, so a doc re-extracted
//! within the same second a consumer already paged past can sort BEFORE the
//! stored cursor and be missed. kb's own prescription is to resume one second
//! earlier than the stored watermark. [`rewind_cursor`] does that ONCE per
//! PASS (never per page — reparsing inside a pass would rewind repeatedly and
//! never converge), and the rewound value is never persisted: only real
//! `next_cursor` values from the feed are. The overlap is harmless because
//! `replace_doc_refs` is keyed on `(kb, doc_id)` — a repeat visit is an
//! idempotent overwrite, not a duplicate row.
//!
//! ## Scope + cost
//!
//! The `[doclens] kbs` allowlist gates this pass WHOLESALE, before the feed
//! is ever called for a kb (R8): removing a corpus from the allowlist stops
//! its prose flowing into this daemon, not merely its routes. `batch_cap`
//! bounds one pass; a capped pass reports the skip LOUDLY and the persisted
//! cursor makes the next pass resume rather than starve. A per-kb failure is
//! recorded and the pass continues — one corpus never sinks the fleet.

use crate::routes::{find_repo, safe_rel_path, ApiError};
use crate::state::SharedState;
use crate::store::{DocRefWrite, NewDocRef, StoreBlocking};
use axum::{
    extract::{Query, State},
    http::header,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use super::resolve::{resolve_lens, PathState};
use super::wire::CodeLensOut;

/// `GET /api/doc-refs`'s wire schema.
pub const DOC_REFS_SCHEMA: &str = "doc-refs/1";
/// `POST /api/doc-lens/sync`'s wire schema.
pub const SYNC_SCHEMA: &str = "doclens-sync/1";

/// Docs per feed page. Smaller than `kb_client`'s own `SNAPSHOT_PAGE_LIMIT`
/// (500) on purpose: even a header-only `coderef/1` row carries
/// `doc_hash`/`title`/`extracted_at`, and the walk is a latency-insensitive
/// background job — there is nothing to buy by pulling bigger pages.
pub const PAGE_LIMIT: u32 = 100;

/// Degrade reasons that are a property of the KB (transport/config), not of
/// the one doc that surfaced them — hitting one aborts that kb's walk rather
/// than re-failing identically on every remaining doc. Everything else
/// (`repo_required`, `repo_indexing`, `repo_unavailable`, `invalid_segment`,
/// `kb_upstream_error` — a parse failure on ONE doc's body) is per-doc and
/// only costs that doc.
const KB_LEVEL_REASONS: &[&str] = &[
    "kb_unreachable",
    "kb_forbidden",
    "kb_daemon_disabled",
    "doclens_disabled",
    "kb_not_allowlisted",
];

// --- the pass --------------------------------------------------------------

/// One sync pass's outcome. Every counter is a SKIP the operator can act on,
/// which is why they are separate fields rather than one "skipped" total: a
/// pass that resolved nothing because the cap was hit and one that resolved
/// nothing because every doc is unpinned are different situations.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SyncStats {
    /// kbs actually WALKED (an allowlist- or cap-skipped kb is not counted).
    pub kbs_synced: usize,
    /// Every PINNED doc for which `resolve_lens` was actually called this
    /// PASS — `batch_cap`'s own accounting unit (DCB-W3.A.R fix 3), and
    /// STRICTLY the superset of `docs_resolved`: it also counts the
    /// `docs_dropped_404` arm and every per-doc error arm, both of which pay
    /// a full network round trip just like a resolved doc does. Counting
    /// only `docs_resolved` toward the cap (the pre-fix behaviour) let a kb
    /// that 5xxs or 404s every body burn one request per pinned doc per
    /// pass, unbounded by `batch_cap` entirely.
    pub docs_attempted: usize,
    pub docs_resolved: usize,
    pub docs_skipped_unpinned: usize,
    /// A SKIP, deliberately mixing two different units (DCB-W3.A.R fix 7):
    /// a kb skipped WHOLESALE because the cap was already spent before its
    /// walk started counts every PIN under that kb, while a cap trip
    /// mid-walk counts the remaining FEED DOCS in the page that trip
    /// happened on. Both answer "how much did this pass decline to
    /// attempt," never a single homogeneous unit.
    pub docs_skipped_cap: usize,
    pub docs_dropped_404: usize,
    /// R8/M11 — a kb that HAS pins but is no longer in `[doclens] kbs` (or
    /// the section is off entirely) is skipped WHOLESALE, never fed-walked at
    /// all: this counts every pin under that kb, not a per-doc observation.
    pub docs_skipped_not_allowlisted: usize,
    /// One entry per kb that errored, `"{kb}: {reason}: {message}"`. A kb's
    /// FIRST error only — a corpus that is down produces one line, not one
    /// per doc.
    pub errors: Vec<String>,
}

/// The one function both `POST /api/doc-lens/sync` and the periodic worker
/// call. `force` resets every synced kb's cursor before walking (a full
/// corpus-side rescan — the explicit operator escape hatch for "a repo-side
/// rename happened and I want claims to catch up sooner than the doc's own
/// next kb-side edit").
///
/// Never returns an error: a pass is a fold over independent corpora and one
/// broken kb must not sink the others (kb-server invariant #28's ethos,
/// applied to a sequential loop — the sync set is "kbs with at least one
/// pin", never more than a handful).
pub async fn run_doclens_sync(state: &SharedState, force: bool) -> SyncStats {
    let started = std::time::Instant::now();
    let mut stats = SyncStats::default();
    let kbs = match state
        .store
        .run_blocking(|store| store.doc_lens_pin_kbs())
        .await
    {
        Ok(kbs) => kbs,
        Err(e) => {
            tracing::warn!(error = %e, "doc-lens sync: cannot read the pin ledger — pass aborted");
            stats.errors.push(format!("(pin ledger): {e}"));
            return stats;
        }
    };
    let mut capped = false;
    for kb in kbs {
        // R8 — the allowlist gate runs BEFORE the feed is ever called for
        // this kb, so a corpus dropped from `[doclens] kbs` stops flowing
        // into this daemon entirely, not just out of its routes.
        if !state.doclens.kb_allowed(&kb) {
            let pins = pin_count(state, &kb).await;
            stats.docs_skipped_not_allowlisted += pins;
            tracing::info!(
                kb = %kb, pins,
                "doc-lens sync: kb has pins but is not in [doclens] kbs — skipped wholesale \
                 (its prose never enters this daemon)"
            );
            continue;
        }
        if capped {
            // `batch_cap` is a PASS budget, so a kb reached after it is spent
            // cannot process anything — walking its feed anyway would be a
            // round trip whose every doc immediately re-hits the cap. Counted
            // and named loudly instead; the persisted cursor means the next
            // pass picks it up.
            let pins = pin_count(state, &kb).await;
            stats.docs_skipped_cap += pins;
            tracing::warn!(
                kb = %kb, pins, cap = state.doclens.batch_cap(),
                "doc-lens sync: batch_cap already spent — this kb was not walked at all this pass"
            );
            continue;
        }
        capped = sync_one_kb(state, &kb, force, &mut stats).await;
        stats.kbs_synced += 1;
    }
    tracing::info!(
        kbs_synced = stats.kbs_synced,
        docs_attempted = stats.docs_attempted,
        docs_resolved = stats.docs_resolved,
        docs_skipped_unpinned = stats.docs_skipped_unpinned,
        docs_skipped_cap = stats.docs_skipped_cap,
        docs_dropped_404 = stats.docs_dropped_404,
        docs_skipped_not_allowlisted = stats.docs_skipped_not_allowlisted,
        errors = stats.errors.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "doc-lens sync pass complete"
    );
    stats
}

async fn pin_count(state: &SharedState, kb: &str) -> usize {
    let kb = kb.to_string();
    state
        .store
        .run_blocking(move |store| {
            store
                .list_doc_lens_pins(Some(&kb))
                .map(|p| p.len())
                .unwrap_or(0)
        })
        .await
}

/// Walk ONE kb's feed. Returns `true` when `batch_cap` stopped the walk (the
/// caller stops starting new kbs).
async fn sync_one_kb(state: &SharedState, kb: &str, force: bool, stats: &mut SyncStats) -> bool {
    let now = chrono::Utc::now().timestamp();
    // One round trip: the optional --force cursor reset and the cursor read
    // are sequential store calls with no async work between them.
    let kb_owned = kb.to_string();
    let stored: Option<String> = state
        .store
        .run_blocking(move |store| {
            if force {
                if let Err(e) = store.clear_doclens_sync_cursor(&kb_owned) {
                    tracing::warn!(
                        kb = %kb_owned, error = %e,
                        "doc-lens sync: could not reset the cursor for --force"
                    );
                }
            }
            store
                .get_doclens_sync_cursor(&kb_owned)
                .ok()
                .flatten()
                .and_then(|c| c.cursor)
        })
        .await;

    // §1.6 — the same-second keyset-gap rewind: ONCE per pass, on the FIRST
    // page only. `committed` tracks what we would persist; it is never the
    // rewound value (persisting that would rewind cumulatively, one second
    // further back every pass, and never converge).
    let mut request_cursor = stored.as_deref().map(rewind_cursor);
    let mut committed = stored;
    let mut resolved_here = 0usize;
    let mut kb_error: Option<String> = None;
    let mut capped = false;
    let cap = state.doclens.batch_cap();

    'pages: loop {
        // A page must never hold more docs than the pass can afford to
        // ATTEMPT. The cursor only advances at a PAGE boundary, so a cap
        // that trips mid-page leaves that page's tail unreachable — and
        // with §1.6's rewind re-presenting the page's head on every
        // subsequent pass, "unreachable" means STARVED, not merely
        // deferred. Clamping the request keeps the cap and the page
        // boundary aligned, which is what makes `batch_cap` a throttle
        // rather than a ceiling on what this daemon can ever see. (The
        // production numbers never meet: `batch_cap` defaults to 200,
        // `PAGE_LIMIT` is 100.)
        //
        // DCB-W3.A.R fix 4 — recomputed EVERY page, never once per kb:
        // `stats` (and so `docs_attempted`/`docs_skipped_unpinned`) is
        // shared across this whole PASS's `for kb in kbs` loop in
        // `run_doclens_sync`, so a kb reached after an earlier kb already
        // spent part of the budget must request only what's actually left —
        // a fresh `PAGE_LIMIT.min(cap)` page it cannot afford to finish
        // just trips the cap mid-page and wastes the round trip on docs
        // immediately discarded. `u32::try_from` (not `as u32`) so an
        // absurd config value degrades to `u32::MAX` rather than silently
        // truncating.
        let remaining_budget =
            cap.saturating_sub(stats.docs_attempted + stats.docs_skipped_unpinned);
        let page_limit = PAGE_LIMIT.min(u32::try_from(remaining_budget.max(1)).unwrap_or(u32::MAX));
        let page = match state
            .kb_client
            .code_refs_feed(kb, request_cursor.as_deref(), page_limit, false)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(kb, error = %e, "doc-lens sync: feed walk failed — claims left untouched");
                kb_error.get_or_insert_with(|| format!("feed: {e}"));
                break 'pages;
            }
        };
        if page.docs.is_empty() {
            break 'pages;
        }
        for (i, doc) in page.docs.iter().enumerate() {
            // Checked BEFORE processing each doc, never after, so the doc
            // that trips the cap is re-examined next pass rather than
            // silently half-processed. `docs_attempted` (DCB-W3.A.R fix 3),
            // not `docs_resolved` — a doc that reached `resolve_lens` and
            // then 404'd or errored still cost a full round trip and must
            // count toward the budget just as much as a resolved one.
            if stats.docs_attempted + stats.docs_skipped_unpinned >= cap {
                let remaining = page.docs.len() - i;
                stats.docs_skipped_cap += remaining;
                tracing::warn!(
                    kb,
                    cap,
                    remaining_in_page = remaining,
                    "doc-lens sync: batch_cap reached — stopping this kb's walk here; the \
                     persisted cursor resumes at this page next pass (more docs may remain \
                     beyond it)"
                );
                capped = true;
                // Deliberately NOT persisting this page's `next_cursor`:
                // `committed` still names the cursor that FETCHED this page,
                // so the next pass re-walks it and reaches the docs we just
                // skipped.
                break 'pages;
            }
            if doc.doc_id.is_empty() {
                continue;
            }
            let kb_c = kb.to_string();
            let doc_id_c = doc.doc_id.clone();
            let pin_res = state
                .store
                .run_blocking(move |store| store.get_doc_lens_pin(&kb_c, &doc_id_c))
                .await;
            let pin = match pin_res {
                Ok(Some(p)) => p,
                Ok(None) => {
                    // The NORMAL case, not an anomaly — most of a corpus is
                    // unpinned. Counted in the pass summary, `debug!` not
                    // `warn!`, and never a per-doc info line.
                    stats.docs_skipped_unpinned += 1;
                    tracing::debug!(kb, doc = %doc.doc_id, "doc-lens sync: unpinned — skipped");
                    continue;
                }
                Err(e) => {
                    tracing::warn!(kb, doc = %doc.doc_id, error = %e, "doc-lens sync: pin lookup failed");
                    kb_error.get_or_insert_with(|| format!("pin lookup: {e}"));
                    continue;
                }
            };

            // DCB-W3.A.R fix 3 — counted for EVERY pinned doc reaching
            // `resolve_lens`, success or failure: the 404 arm and the
            // per-doc error arm below both pay a full round trip, so the
            // budget must include them or a kb that 5xxs/404s every body
            // could burn far more than `batch_cap` requests in one pass.
            stats.docs_attempted += 1;

            // R8 — the ONE call. `resolve_lens` fetches the body itself and
            // owns both the 404 pin-drop and the moves re-key (see the module
            // doc); anything else here would double-fetch.
            // CT-F2's `at=declared` is an opt-in READ-time analysis only —
            // this background sync persists `codelens/1`'s regular claims
            // (`write_claims`), never `when_written`, so it never asks for it.
            match resolve_lens(state, kb, &doc.doc_id, Some(&pin.repo), false).await {
                Ok(lens) => {
                    if write_claims(state, kb, &lens, now, &mut kb_error).await {
                        stats.docs_resolved += 1;
                        resolved_here += 1;
                    }
                }
                Err(e) if e.reason() == Some("doc_not_found") => {
                    // The pin is already gone (dropped inside `resolve_lens`);
                    // its claims go with it — amendment 11's "claims dropped
                    // when the doc 404s on the feed".
                    let kb_c = kb.to_string();
                    let doc_id_c = doc.doc_id.clone();
                    match state
                        .store
                        .run_blocking(move |store| store.delete_doc_refs_for_doc(&kb_c, &doc_id_c))
                        .await
                    {
                        Ok(dropped) => tracing::warn!(
                            kb, doc = %doc.doc_id, claims_dropped = dropped,
                            "doc-lens sync: kb 404s this doc — dropped its claims"
                        ),
                        Err(e) => tracing::warn!(
                            kb, doc = %doc.doc_id, error = %e,
                            "doc-lens sync: claim drop failed for a 404'd doc"
                        ),
                    }
                    stats.docs_dropped_404 += 1;
                }
                Err(e) => {
                    let reason = e.reason().unwrap_or("error");
                    tracing::warn!(
                        kb, doc = %doc.doc_id, reason, error = %e.message(),
                        "doc-lens sync: could not resolve this doc — its claims are left untouched"
                    );
                    kb_error.get_or_insert_with(|| format!("{reason}: {}", e.message()));
                    if KB_LEVEL_REASONS.contains(&reason) {
                        break 'pages;
                    }
                }
            }
        }
        match page.next_cursor {
            // Persisted after EVERY page (not just at a kb boundary) so a
            // mid-kb crash resumes from the last fully-processed page rather
            // than re-walking from scratch.
            Some(next) => {
                let kb_c = kb.to_string();
                let next_c = next.clone();
                if let Err(e) = state
                    .store
                    .run_blocking(move |store| {
                        store.set_doclens_sync_cursor(&kb_c, Some(&next_c), now, None)
                    })
                    .await
                {
                    tracing::warn!(kb, error = %e, "doc-lens sync: cursor persist failed");
                }
                committed = Some(next.clone());
                request_cursor = Some(next);
            }
            // kb OMITS `next_cursor` on the last page.
            None => break 'pages,
        }
    }

    {
        let kb_c = kb.to_string();
        let committed_c = committed.clone();
        let kb_error_c = kb_error.clone();
        if let Err(e) = state
            .store
            .run_blocking(move |store| {
                store.set_doclens_sync_cursor(
                    &kb_c,
                    committed_c.as_deref(),
                    now,
                    kb_error_c.as_deref(),
                )
            })
            .await
        {
            tracing::warn!(kb, error = %e, "doc-lens sync: cursor persist failed");
        }
    }
    if let Some(msg) = kb_error {
        stats.errors.push(format!("{kb}: {msg}"));
    }
    // Per-kb (not once per pass) so a future consumer can invalidate one
    // corpus's query without over-invalidating — `set.changed`'s own per-repo
    // scoping precedent rather than a bare global signal.
    state.bus.emit(
        "doc_refs.synced",
        serde_json::json!({ "kb": kb, "resolved": resolved_here }),
    );
    capped
}

/// Replace ONE doc's claims in one transaction. Returns `false` when nothing
/// was written (a repo that vanished from config between the pin read and
/// here, or a store failure) so the caller does not count it as resolved.
async fn write_claims(
    state: &SharedState,
    kb: &str,
    lens: &CodeLensOut,
    now: i64,
    kb_error: &mut Option<String>,
) -> bool {
    let Ok((_, repo_id)) = find_repo(state, &lens.repo.name) else {
        tracing::warn!(
            kb, doc = %lens.doc_id, repo = %lens.repo.name,
            "doc-lens sync: resolved against a repo that is no longer configured — no claims written"
        );
        return false;
    };
    // The moves chain fired and `resolve_lens` already re-keyed the pin under
    // us. Everything below keys on `lens.doc_id`; the rows filed under the
    // OLD id would otherwise be orphans no future pass ever revisits, so they
    // are dropped in the same pass that supersedes them. Both store writes
    // (the optional old-id claim drop + the replace) share ONE blocking-pool
    // round trip — no async work between them (store.rs's 2026-08-31
    // incident note).
    let kb_owned = kb.to_string();
    let doc_id = lens.doc_id.clone();
    let moved_from = lens.moved_from.clone();
    // DCB-W3.A.R fix 6 — never an empty string: a title-less kb doc would
    // otherwise become an invisible link downstream (nothing to show in the
    // `CitedBy` strip). Falls back to the doc path's basename, then the doc
    // id itself; see `doc_title_or_fallback`. W3.B was separately asked to
    // guard the RENDER side too — this is the WRITE-time half, so a
    // title-less claim is never persisted empty in the first place.
    let doc_title =
        doc_title_or_fallback(lens.doc_title.as_deref(), lens.doc_path.as_deref(), &doc_id)
            .to_string();
    let doc_path = lens.doc_path.clone().unwrap_or_default();
    let doc_hash = lens.doc_hash.clone();
    let head_sha = lens.repo.head_sha.clone();
    let dirty = lens.repo.dirty.unwrap_or(false);
    let claims = claims_of(lens);

    let result = state
        .store
        .run_blocking(move |store| {
            if let Some(old) = moved_from.as_deref() {
                match store.delete_doc_refs_for_doc(&kb_owned, old) {
                    Ok(n) if n > 0 => tracing::info!(
                        kb = %kb_owned, from = old, to = %doc_id, claims_moved = n,
                        "doc-lens sync: kb's moves chain re-keyed this doc — dropped the claims \
                         filed under its old id"
                    ),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(
                        kb = %kb_owned, doc = old, error = %e,
                        "doc-lens sync: old-id claim drop failed"
                    ),
                }
            }
            let write = DocRefWrite {
                kb: &kb_owned,
                doc_id: &doc_id,
                repo_id,
                doc_title: &doc_title,
                doc_path: &doc_path,
                doc_hash: doc_hash.as_deref(),
                head_sha: head_sha.as_deref(),
                dirty,
                seen_at: now,
            };
            store.replace_doc_refs(&write, &claims)
        })
        .await;

    match result {
        Ok(n) => {
            tracing::debug!(kb, doc = %lens.doc_id, claims = n, "doc-lens sync: claims replaced");
            true
        }
        Err(e) => {
            tracing::warn!(kb, doc = %lens.doc_id, error = %e, "doc-lens sync: claim write failed");
            kb_error.get_or_insert_with(|| format!("claim write: {e}"));
            false
        }
    }
}

/// DCB-W3.A.R fix 6 — `doc_refs.doc_title` must never be persisted empty.
/// Prefers `title` (blank/whitespace-only treated as absent); falls back to
/// `doc_path`'s basename (the last `/`-segment; blank/absent treated the
/// same way); falls back to `doc_id` itself, which is always non-empty
/// (`validate_doc_segment`'s own invariant). All three inputs borrow from the
/// SAME `CodeLensOut`, so the result's lifetime ties to it — never an
/// allocation.
fn doc_title_or_fallback<'a>(
    title: Option<&'a str>,
    doc_path: Option<&'a str>,
    doc_id: &'a str,
) -> &'a str {
    if let Some(t) = title {
        if !t.trim().is_empty() {
            return t;
        }
    }
    if let Some(p) = doc_path {
        if !p.trim().is_empty() {
            let basename = p.rsplit('/').next().unwrap_or(p);
            if !basename.is_empty() {
                return basename;
            }
        }
    }
    doc_id
}

/// The `path_state == "present"` filter — the ONE rule that decides which
/// refs become rows (see the module doc). `resolved_path` is re-checked
/// rather than unwrapped: `present` implies it, but a `None` here would be a
/// contract break to skip, never to panic on.
fn claims_of(lens: &CodeLensOut) -> Vec<NewDocRef> {
    lens.refs
        .iter()
        .filter(|r| r.path_state == Some(PathState::Present))
        .filter_map(|r| {
            let resolved_path = r.resolved_path.clone()?;
            Some(NewDocRef {
                ordinal: i64::from(r.ordinal),
                kind: r.kind.clone(),
                raw_hint: r.raw.clone(),
                resolved_path,
                // The RESOLVED line when the lens produced one (a confirmed,
                // drifted or git-remapped landing), else the document's own
                // hint. `line_state` is what says which of the two a reader
                // is looking at — `raw_hint` always carries the prose's own
                // literal text either way.
                line_start: r.resolved_line.or(r.line_hint).map(i64::from),
                line_end: r.resolved_line_end.or(r.line_hint_end).map(i64::from),
                line_state: Some(r.line_state.as_str().to_string()),
                group_key: r.group.clone(),
                group_label: r.group.as_deref().and_then(|key| {
                    lens.groups
                        .iter()
                        .find(|g| g.key == key)
                        .map(|g| g.label.clone())
                }),
            })
        })
        .collect()
}

/// kb's feed cursor is ONE opaque string, `"<extracted_at>:<artifact_id>"`.
/// This is the ONLY place doclens ever decomposes it, and only to resume a
/// NEW pass one second earlier than the stored watermark (the same-second
/// keyset gap — see the module doc). Feed-internal paging round-trips the
/// cursor verbatim and never comes here.
///
/// TOTAL by construction: a cursor whose head is not an integer (a grammar
/// kb may change without telling this client) is returned VERBATIM — the
/// worst case is that we lose the rewind for that pass, never that a pass
/// fails or resumes somewhere arbitrary.
pub(crate) fn rewind_cursor(cursor: &str) -> String {
    let Some((head, rest)) = cursor.split_once(':') else {
        return cursor.to_string();
    };
    let Ok(secs) = head.parse::<i64>() else {
        return cursor.to_string();
    };
    format!("{}:{rest}", secs.saturating_sub(1))
}

// --- re-entrancy guard -------------------------------------------------

/// DCB-W3.A.R fix 5 — the daemon-wide guard both entry points below share.
/// `run_doclens_sync` itself is individually idempotent (a repeat pass
/// DELETE+INSERTs the same claims, per-kb cursors persist per page), so two
/// overlapping passes cannot corrupt `doc_refs`. What they CAN do is race a
/// `force` cursor reset against a concurrent non-force pass: the non-force
/// pass may already have read the OLD cursor before the force pass clears
/// it, so it resumes from a watermark the force pass is simultaneously
/// invalidating — a "partially undone" reset, not a crash, but not what
/// either caller asked for either. CAS'd `false → true` before a pass starts
/// and reset after; `None` means a pass is already running elsewhere.
///
/// Both callers below (the route, the worker) turn a busy guard into a
/// declined attempt rather than a queued one — no retry loop, no backlog:
/// the route answers `409` and the operator can just ask again; the worker
/// skips this tick and tries again on its own schedule.
async fn try_run_doclens_sync(state: &SharedState, force: bool) -> Option<SyncStats> {
    use std::sync::atomic::Ordering;
    if state
        .doclens_sync_running
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return None;
    }
    let stats = run_doclens_sync(state, force).await;
    state.doclens_sync_running.store(false, Ordering::Release);
    Some(stats)
}

// --- the worker ------------------------------------------------------------

/// The periodic pass. Mirrors `reviews::spawn_auto_capture_worker`: spawned
/// UNCONDITIONALLY, deciding internally whether to idle out, so the call site
/// in `lib.rs` stays uniform with its sibling worker rather than growing a
/// config branch of its own.
pub fn spawn_doclens_sync_worker(state: SharedState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let every = state.doclens.sync_interval_secs;
        if every == 0 {
            tracing::info!(
                "kb-code: [doclens] sync_interval_secs = 0 — periodic doc-lens sync idle \
                 (POST /api/doc-lens/sync and [doclens] sync_on_boot still work)"
            );
            return;
        }
        let mut ticker = tokio::time::interval(Duration::from_secs(every));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // `interval`'s first tick completes IMMEDIATELY. Burn it: whether a
        // pass runs at boot is `[doclens] sync_on_boot`'s decision, and an
        // operator who left that false did not ask for one.
        ticker.tick().await;
        let shutdown = crate::shutdown_signal();
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    // DCB-W3.A.R fix 5 — skip this tick ENTIRELY (never
                    // queue) when a pass is already running, whether that's
                    // `POST /api/doc-lens/sync` or a previous tick that
                    // overran its own interval. `run_doclens_sync` logs the
                    // pass summary itself when it actually runs.
                    if try_run_doclens_sync(&state, false).await.is_none() {
                        tracing::info!(
                            "kb-code: periodic doc-lens sync tick skipped — a pass is already \
                             running"
                        );
                    }
                }
                _ = &mut shutdown => {
                    tracing::info!("kb-code: periodic doc-lens sync stopping (shutdown)");
                    break;
                }
            }
        }
    })
}

// --- routes ----------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct SyncBody {
    /// Reset every synced kb's cursor before walking — a full corpus-side
    /// rescan.
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Serialize)]
pub struct SyncOut {
    pub schema: &'static str,
    pub forced: bool,
    #[serde(flatten)]
    pub stats: SyncStats,
}

/// `POST /api/doc-lens/sync` — LOOPBACK-ONLY (mounted on `transcripts_api`,
/// `router.rs`). D-A moved the PIN onto `auth_bearer` so kb's own reader can
/// write it; this route deliberately did NOT follow, because it drives a BULK
/// pull of document prose across the process boundary into this daemon's
/// store — a different blast radius from remembering one checkout.
///
/// A pass is bounded by `batch_cap`, so it always completes within the
/// request's lifetime: `200` synchronously, no job handle to poll.
///
/// DCB-W3.A.R fix 5 — `409` (`reason = "sync_already_running"`) when a pass
/// — this route's own previous call, or the periodic worker's tick — is
/// still in flight, rather than queueing a second one behind it (see
/// `try_run_doclens_sync`'s doc for why overlap is declined outright).
pub async fn sync_route(
    State(state): State<SharedState>,
    body: Option<Json<SyncBody>>,
) -> Result<impl IntoResponse, ApiError> {
    let force = body.map(|Json(b)| b.force).unwrap_or(false);
    let Some(stats) = try_run_doclens_sync(&state, force).await else {
        return Err(ApiError::new(
            axum::http::StatusCode::CONFLICT,
            "a doc-lens sync pass is already running on this daemon",
        )
        .with_reason("sync_already_running"));
    };
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(SyncOut {
            schema: SYNC_SCHEMA,
            forced: force,
            stats,
        }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct DocRefsParams {
    pub repo: String,
    pub path: String,
}

/// One claim. Deliberately NOT the full stored row: `head_sha`/`dirty`/
/// `doc_hash`/`kind` are persisted (amendment 11's "resolving head_sha +
/// worktree label") but unread by any v1 consumer, and a wire field with no
/// reader is a field that drifts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DocRefClaim {
    pub kb: String,
    pub doc_id: String,
    pub doc_title: String,
    pub doc_path: String,
    /// Built by [`super::doc_href`] — the ONE kb link-out builder in this
    /// crate, never a second inline `format!` + encoder (R1/m29). `None` when
    /// `[kb_daemon]` has no usable public base; a claim row is still perfectly
    /// useful without a link back to kb.
    pub doc_public_href: Option<String>,
    pub group_label: Option<String>,
    pub raw_hint: String,
    pub line_start: Option<i64>,
    pub line_end: Option<i64>,
    pub line_state: Option<String>,
    pub seen_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DocRefsOut {
    pub schema: &'static str,
    pub repo: String,
    pub path: String,
    /// `Store::get_file(repo_id, path).is_some()` — checked NOW, not at sync
    /// time (amendment 5's exact-read rule: never the fuzzy search lane).
    /// **This is the sole "rotted claim" signal**: every claim in one
    /// response is a claim about the SAME path, so liveness is a property of
    /// the response, not of a row. `false` ⇒ render "cited, path no longer
    /// present", never a live link.
    pub live: bool,
    pub claims: Vec<DocRefClaim>,
}

/// The engine behind `GET /api/doc-refs` — split from the handler so the
/// revalidation rule is unit-testable without an HTTP round trip.
pub(crate) fn doc_refs_for(
    state: &SharedState,
    repo: &str,
    path: &str,
) -> Result<DocRefsOut, ApiError> {
    let (_entry, repo_id) = find_repo(state, repo)?;
    if path.trim().is_empty() {
        return Err(ApiError::bad_request_with_reason(
            "path must not be empty",
            "invalid_segment",
        ));
    }
    let path = safe_rel_path(path)?;
    // DCB-W2.B.R fix 1's lesson, applied to every path param on this lane:
    // `safe_rel_path` rejects `..`/absolute components, this additionally
    // rejects a bare `.` segment, and neither is redundant with the other.
    if super::path_has_dot_segment(path) {
        return Err(ApiError::bad_request_with_reason(
            format!("invalid path segment in {path:?}: \".\"/\"..\" are not allowed"),
            "invalid_segment",
        ));
    }
    let live = state.store.get_file(repo_id, path)?.is_some();
    let rows = state.store.doc_refs_for_path(repo_id, path)?;
    let public_base = state.kb_daemon.public_base();
    let claims = rows
        .into_iter()
        .map(|r| DocRefClaim {
            doc_public_href: super::doc_href(public_base, &r.kb, &r.doc_path),
            kb: r.kb,
            doc_id: r.doc_id,
            doc_title: r.doc_title,
            doc_path: r.doc_path,
            group_label: r.group_label,
            raw_hint: r.raw_hint,
            line_start: r.line_start,
            line_end: r.line_end,
            line_state: r.line_state,
            seen_at: r.seen_at,
        })
        .collect();
    Ok(DocRefsOut {
        schema: DOC_REFS_SCHEMA,
        repo: repo.to_string(),
        path: path.to_string(),
        live,
        claims,
    })
}

/// `GET /api/doc-refs?repo=&path=` — the reverse lookup, on the ORDINARY
/// `auth_bearer` router (same-origin only, deliberately NOT the CORS'd
/// `doclens_read` set: web-code's own `CitedBy` strip is this daemon's own
/// frontend, and a cross-origin "which documents mention this file" oracle is
/// a surface no consumer asked for). `cors_layer_route_set_is_pinned` asserts
/// it carries no ACAO.
pub async fn doc_refs_route(
    State(state): State<SharedState>,
    Query(p): Query<DocRefsParams>,
) -> Result<impl IntoResponse, ApiError> {
    // `doc_refs_for` keeps its own `&SharedState` signature (rule: sync
    // helpers keep their signature — it is ALSO called directly, sync, from
    // `sync_tests.rs`) — the async boundary is here, so the whole call
    // (two sequential store reads) runs on the blocking pool via a cloned
    // `SharedState` (cheap `Arc` clone); the `&Store` `run_blocking` hands
    // in is unused since `doc_refs_for` reaches the store through `state`
    // itself.
    let state_for_read = state.clone();
    let repo = p.repo.clone();
    let path = p.path.clone();
    let out = state
        .store
        .run_blocking(move |_store| doc_refs_for(&state_for_read, &repo, &path))
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

#[cfg(test)]
#[path = "sync_tests.rs"]
mod tests;
