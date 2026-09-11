//! PRR-R3 ("The PR Room," kb v0.39 T2, Phase 3) — findings import / list /
//! disposition, plus human-authored ("manual") findings (design doc
//! §2 rows 8-11 + §3.1 `kbc-findings/1` + §4.3 reconciliation;
//! design-addendum-2 §E). This module is a thin ROUTE layer over the
//! reconciliation core R1 already shipped in `store.rs` (search that file
//! for its own `PRR-R1` block) — every write here funnels through
//! `Store::reconcile_findings_import` / `Store::insert_review_finding` /
//! `Store::set_finding_disposition` / `Store::clear_finding_disposition`;
//! this file owns wire validation, anchor derivation (reading the real
//! pinned git blob), and JSON composition.
//!
//! # Routes
//!
//! **Loopback-only** (review-mutation family, `router.rs`'s
//! `transcripts_api` sub-router): `POST /api/reviews/{id}/findings/import`
//! ([`import_findings_route`]) — an agent-side batch-reconcile verb, not a
//! mobile-mutation candidate, so S2-B leaves it here. `POST
//! /api/reviews/{id}/compose` ([`compose_review_route`], V70-R, design doc
//! D9 scoped to v0) is the SAME family and posture — findings + report +
//! an optional review-level verdict in one sqlite transaction, never
//! gated, D22's root invariant #4 left untouched.
//!
//! **Gated** (S2-B, `router.rs`'s `review_remote` sub-router — loopback
//! unconditionally, else `[review] remote_mutations`-gated bearer, default
//! OFF ⇒ 404 for a non-loopback caller, byte-identical to the loopback-only
//! posture above): `POST /api/reviews/{id}/findings`
//! ([`create_manual_finding_route`], addendum §E — a single human-authored
//! finding), `PUT`/`DELETE /api/reviews/{id}/findings/{slug}/disposition`
//! ([`set_finding_disposition_route`] / [`clear_finding_disposition_route`]).
//! See [`crate::review_gate::review_mutations_gate`] for the full admission
//! table.
//!
//! **Bearer** (read): `GET /api/reviews/{id}/findings`
//! ([`list_findings_route`]).
//!
//! # The OWED item (R1 -> R3)
//!
//! R1's own report flagged that `Store::reconcile_findings_import`'s
//! refresh step did not origin-gate — a batch slug colliding with an
//! existing `origin="manual"` finding would silently overwrite it. This
//! module closes that at the ROUTE boundary ([`validate_import_batch`]'s
//! `slug_conflict_manual` check, whole-batch 400, nothing written); the
//! DATA-layer defense-in-depth fix lives in `store.rs`'s
//! `reconcile_findings_import` (see that function's own doc + the pinning
//! test `reconcile_findings_import_refresh_never_overwrites_a_manual_
//! finding_even_on_slug_collision`).
//!
//! # Resolution — the SAME ladder, never a second one
//!
//! Every finding's displayed position is computed via
//! `crate::review_comments::resolve_for_ps_with_content` — the identical
//! function `/comments` and `/distill` use — so a finding can never drift
//! from what a human sees in the browser for the SAME annotation. This
//! unit only ADDS: (a) two new anchor-kind dispatch branches inside that
//! function (`whole_file` resolved-iff-path-exists, `review` always
//! resolved — see that module's own doc), and (b) a `confidence`
//! (`exact`|`fuzzy`) field surfaced from the SAME underlying
//! `annotations::resolve` call (`annotations::MatchConfidence`, no second
//! resolution algorithm).

use crate::annotations;
use crate::review_comments::{self, ResolvedAgainst, ResolvedForPs};
use crate::reviews::{emit_review_changed, require_review, resolve_ps};
use crate::routes::ApiError;
use crate::state::SharedState;
use crate::store::{self, ReviewFindingRow, ReviewPatchsetRow, Store, StoreBlocking};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// `kbc-findings/1` (design doc §3.1) — the batch-import payload's schema
/// tag, validated verbatim against `body.schema`.
pub const IMPORT_SCHEMA: &str = "kbc-findings/1";
/// The `GET /api/reviews/{id}/findings` response envelope's own schema tag
/// — a codebase-convention addition (every sibling read route —
/// `review-comments/1`, `review-distill/1`, `reviews/1` — wraps its array
/// in `{"schema": ..., ...}` rather than a bare top-level array; the
/// design doc's row-9 sketch shows a bare `[...]`, but this module follows
/// the established house shape instead — see this unit's own report for
/// that deviation).
pub const SCHEMA: &str = "review-findings/1";

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// --- wire shapes -------------------------------------------------------

// 2026-08-31 incident (store.rs module doc): `Clone` is additive — needed
// so `import_findings_route` can move an owned copy of the batch into a
// `run_blocking` closure (validation is a store read) while the original
// `body.findings` stays available for the anchor-derivation loop after.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingLocationBody {
    pub path: String,
    pub kind: String,
    #[serde(default)]
    pub lines: Option<Vec<i64>>,
    #[serde(default)]
    pub removed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingEvidenceBody {
    #[serde(default)]
    pub lang: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FindingImportItem {
    pub slug: String,
    pub severity: String,
    pub category: String,
    pub location: FindingLocationBody,
    pub title: String,
    pub rationale: String,
    #[serde(default)]
    pub recommendation: Option<String>,
    #[serde(default)]
    pub evidence: Option<FindingEvidenceBody>,
}

#[derive(Debug, Deserialize)]
pub struct FindingsImportBody {
    pub schema: String,
    /// "full" (default) | "additive" — addendum §E.
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub ps_number: Option<i64>,
    /// Free text, stored on the batch only — the SQL migration's own
    /// `import_note` design intent didn't land a durable column (V0024's
    /// `review_findings` has no such field, only `import_batch_id`); this
    /// unit accepts and echoes it back nowhere durable, matching the "one
    /// owner writes it whole, nothing partial" posture elsewhere, rather
    /// than silently dropping the field with a parse error. See this
    /// unit's report: `import_note` is accepted but NOT persisted (no
    /// column exists for it) — a documented deviation, not an oversight.
    #[serde(default)]
    #[allow(dead_code)]
    pub import_note: Option<String>,
    /// The generator agent's identity, threaded onto the linked
    /// annotations' `author` field — defaults to `"claude"` (R1's own doc:
    /// "the generator agent's identity, e.g. `claude`"), distinct from the
    /// human-authored default (`"you"`) every other identity-resolving
    /// body in this crate uses (`CreateAnnotationBody::author`,
    /// [`CreateManualFindingBody::author`] below).
    #[serde(default)]
    pub author: Option<String>,
    pub findings: Vec<FindingImportItem>,
}

#[derive(Debug, Deserialize)]
pub struct CreateManualFindingBody {
    #[serde(default)]
    pub slug: Option<String>,
    pub severity: String,
    pub category: String,
    pub location: FindingLocationBody,
    pub title: String,
    pub rationale: String,
    #[serde(default)]
    pub recommendation: Option<String>,
    /// Resolved identity — same "body field defaulting to `you`"
    /// convention `routes::assemble_top_level_annotation`'s own `author`
    /// uses.
    #[serde(default)]
    pub author: Option<String>,
    /// V70-A3X — the SAME [`FindingEvidenceBody`] shape [`FindingImportItem`]
    /// already carries for the batch `import` path; a manual `add` never
    /// wired it through (`evidence_lang`/`evidence_source` were hardcoded
    /// `None` regardless of what a caller sent) — see `kb-code review
    /// findings add --evidence`.
    #[serde(default)]
    pub evidence: Option<FindingEvidenceBody>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ListFindingsParams {
    /// Patchset number, or `latest` (default) — same grammar
    /// `ReviewCommentsParams::ps` / `reviews::resolve_ps` already use.
    #[serde(default)]
    pub ps: Option<String>,
    #[serde(default)]
    pub disposition: Option<String>,
    #[serde(default)]
    pub include_superseded: bool,
}

#[derive(Debug, Deserialize)]
pub struct SetFindingDispositionBody {
    pub disposition: String,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
}

// --- pure validation -----------------------------------------------------

/// `slug ~ f-[a-z0-9-]+` (design doc §3.1). Surfaced on a 400 so the
/// caller sees the regex, not just "invalid slug".
pub const FINDING_SLUG_PATTERN: &str = "f-[a-z0-9-]+";

/// Hand-rolled character-class check rather than a `regex` dependency
/// (this crate has none outside its `grep-regex` search lane) for one
/// small, fixed pattern.
///
/// `pub(crate)` (V73-K1) — `review_doc::refs`'s `finding:` scheme and
/// `review_doc::lint` validate against this exact predicate rather than a
/// second copy of the pattern.
pub(crate) fn is_valid_finding_slug(s: &str) -> bool {
    match s.strip_prefix("f-") {
        Some(rest) if !rest.is_empty() => rest
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
        _ => false,
    }
}

/// `f-<kebab-of-title>` (addendum §E) — lowercase alnum runs joined by a
/// single `-`, no leading/trailing/doubled dash. Always produces a string
/// [`is_valid_finding_slug`] accepts (falls back to `"finding"` when the
/// title has no alphanumeric characters at all, e.g. a title that is pure
/// punctuation or emoji).
fn kebab_case(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut pending_dash = false;
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
            pending_dash = false;
        } else {
            pending_dash = true;
        }
    }
    if out.is_empty() {
        out.push_str("finding");
    }
    out
}

/// `POST /api/reviews/{id}/findings`'s slug derivation: `f-<kebab>`,
/// uniquified `-2`/`-3`… against every EXISTING finding on this review
/// (superseded included — `idx_review_findings_review_slug` is unique
/// across all rows regardless of supersession, so this must check the
/// same universe that index enforces).
fn derive_unique_slug(store: &Store, review_id: i64, title: &str) -> Result<String, ApiError> {
    let base = format!("f-{}", kebab_case(title));
    if store.get_review_finding(review_id, &base)?.is_none() {
        return Ok(base);
    }
    for n in 2..1000 {
        let candidate = format!("{base}-{n}");
        if store.get_review_finding(review_id, &candidate)?.is_none() {
            return Ok(candidate);
        }
    }
    Err(ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("could not derive a unique finding slug from {title:?} after 998 attempts"),
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocationShapeError {
    EmptyPath,
    InvalidKind,
    MissingLines,
    InvalidLinesShape,
}

/// Pure, IO-free structural validation of one finding's `location` — the
/// SAME rules `kbc-findings/1`'s spec names (§3.1): `path` non-empty;
/// `kind` in the closed 4-set; `lines` required (and shape-checked) iff
/// `kind != whole_file`, every line number `>= 1`.
fn validate_location_shape(loc: &FindingLocationBody) -> Result<(), LocationShapeError> {
    if loc.path.trim().is_empty() {
        return Err(LocationShapeError::EmptyPath);
    }
    if !store::is_valid_location_kind(&loc.kind) {
        return Err(LocationShapeError::InvalidKind);
    }
    if loc.kind == store::LOCATION_KIND_WHOLE_FILE {
        return Ok(());
    }
    let Some(lines) = &loc.lines else {
        return Err(LocationShapeError::MissingLines);
    };
    let count_ok = if loc.kind == store::LOCATION_KIND_SINGLE {
        lines.len() == 1
    } else if loc.kind == store::LOCATION_KIND_RANGE {
        lines.len() == 2
    } else {
        // multi
        !lines.is_empty()
    };
    if !count_ok || lines.iter().any(|&n| n < 1) {
        return Err(LocationShapeError::InvalidLinesShape);
    }
    Ok(())
}

fn describe_location_error(
    e: LocationShapeError,
    loc: &FindingLocationBody,
) -> (&'static str, String) {
    match e {
        LocationShapeError::EmptyPath => {
            ("empty_path", "location.path must not be empty".to_string())
        }
        LocationShapeError::InvalidKind => (
            "invalid_location_kind",
            format!(
                "location.kind must be single|range|multi|whole_file, got {:?}",
                loc.kind
            ),
        ),
        LocationShapeError::MissingLines => (
            "missing_lines",
            format!("location.lines is required for kind {:?}", loc.kind),
        ),
        LocationShapeError::InvalidLinesShape => (
            "invalid_lines_shape",
            format!(
                "location.lines does not match kind {:?} (single needs exactly 1 line, \
                 range needs exactly 2, multi needs at least 1, every line must be >= 1)",
                loc.kind
            ),
        ),
    }
}

#[derive(Debug, Serialize)]
struct BatchFindingError {
    index: usize,
    kind: &'static str,
    message: String,
}

/// Whole-batch validation (design doc §3.1 + the OWED item, this module's
/// own doc) — every pure per-item check first (severity, location shape,
/// slug format/uniqueness-within-payload), THEN (only if every pure check
/// passed) the one check that needs the store: does any payload slug
/// collide with an existing `origin="manual"` finding on this review.
/// Returns every error found (a finding can contribute more than one) —
/// never fail-fast, matching "whole-batch 400 with per-index errors,
/// nothing written."
fn validate_import_batch(
    store: &Store,
    review_id: i64,
    findings: &[FindingImportItem],
) -> Result<Vec<BatchFindingError>, ApiError> {
    let mut errors = Vec::new();
    let mut seen_slugs: HashMap<&str, usize> = HashMap::new();

    for (i, f) in findings.iter().enumerate() {
        if !store::is_valid_severity(&f.severity) {
            errors.push(BatchFindingError {
                index: i,
                kind: "invalid_severity",
                message: format!("severity must be blocker|concern|ok, got {:?}", f.severity),
            });
        }
        if let Err(e) = validate_location_shape(&f.location) {
            let (kind, message) = describe_location_error(e, &f.location);
            errors.push(BatchFindingError {
                index: i,
                kind,
                message,
            });
        }
        if !is_valid_finding_slug(&f.slug) {
            errors.push(BatchFindingError {
                index: i,
                kind: "invalid_slug",
                message: format!("slug must match {FINDING_SLUG_PATTERN}, got {:?}", f.slug),
            });
        } else if let Some(&first) = seen_slugs.get(f.slug.as_str()) {
            errors.push(BatchFindingError {
                index: i,
                kind: "duplicate_slug",
                message: format!(
                    "slug {:?} duplicates the finding already at index {first} in this payload",
                    f.slug
                ),
            });
        } else {
            seen_slugs.insert(f.slug.as_str(), i);
        }
    }

    if errors.is_empty() {
        // THE OWED CHECK (this module's own doc) — a payload slug
        // colliding with an existing MANUAL finding is rejected wholesale,
        // never silently overwritten.
        let existing = store.list_review_findings(review_id, None, true)?;
        let manual_slugs: std::collections::HashSet<&str> = existing
            .iter()
            .filter(|f| f.origin == store::FINDING_ORIGIN_MANUAL)
            .map(|f| f.slug.as_str())
            .collect();
        for (i, f) in findings.iter().enumerate() {
            if manual_slugs.contains(f.slug.as_str()) {
                errors.push(BatchFindingError {
                    index: i,
                    kind: "slug_conflict_manual",
                    message: format!(
                        "slug {:?} collides with an existing human-authored finding on this \
                         review; an import never overwrites a manual finding",
                        f.slug
                    ),
                });
            }
        }
    }

    Ok(errors)
}

// --- anchor derivation (shared by import + manual create) ----------------

/// Build one finding's `annotations` anchor via
/// `store::derive_finding_anchor`, reading the appropriate pinned blob
/// (`target_ps.base_sha` when `location.removed`, else `.tip_sha`) through
/// the SAME cached `read_blob_text` `/comments` uses — one git read per
/// unique `(path, sha)` for the whole call, not per finding. A cited line
/// past the blob's own line count degrades to an EMPTY line-text (never an
/// error): the resulting anchor snippet is simply empty and will not
/// re-match on a later read, the SAME honest "this looks orphaned now"
/// degrade every other stale comment goes through — not a batch-import
/// failure over an imprecise citation.
fn build_finding_anchor(
    repo_root: &Path,
    blob_cache: &mut HashMap<(String, String), Option<String>>,
    target_ps: &ReviewPatchsetRow,
    location: &FindingLocationBody,
) -> Result<store::DerivedFindingAnchor, ApiError> {
    let sha = if location.removed {
        target_ps.base_sha.clone()
    } else {
        target_ps.tip_sha.clone()
    };
    let key = (location.path.clone(), sha.clone());
    if !blob_cache.contains_key(&key) {
        let text = review_comments::read_blob_text(repo_root, &location.path, &sha);
        blob_cache.insert(key.clone(), text);
    }
    let content = blob_cache
        .get(&key)
        .and_then(|c| c.as_deref())
        .unwrap_or("");
    store::derive_finding_anchor(
        &location.kind,
        &location.path,
        location.lines.as_deref(),
        location.removed,
        |n| {
            let idx = n.max(1) as usize - 1;
            content.lines().nth(idx).unwrap_or("").to_string()
        },
    )
    .map_err(ApiError::bad_request)
}

// --- SSE ---------------------------------------------------------------

/// Deliberately self-contained rather than reusing `reviews::
/// emit_review_changed` (that helper is module-private to `reviews.rs`,
/// a multi-phase hot file this unit avoids touching at all). Same
/// `review.changed` wire shape, plus an additive `finding_slug` (design
/// doc §5) when present.
fn emit_findings_review_changed(
    bus: &kb_core::events::EventBus,
    review_id: i64,
    repo: &str,
    reason: &str,
    finding_slug: Option<&str>,
) {
    let mut body = serde_json::json!({
        "review_id": review_id,
        "repo": repo,
        "reason": reason,
    });
    if let Some(slug) = finding_slug {
        body["finding_slug"] = serde_json::json!(slug);
    }
    bus.emit("review.changed", body);
}

// --- JSON composition ----------------------------------------------------

fn orphaned_resolution(target_ps: &ReviewPatchsetRow) -> ResolvedForPs {
    ResolvedForPs {
        line: None,
        line_end: None,
        orphaned: true,
        resolved_against: ResolvedAgainst {
            ps: target_ps.ps_number,
            sha: target_ps.tip_sha.clone(),
        },
        original: None,
        confidence: None,
    }
}

/// The list-item / single-finding wire shape (design doc §2 row 9) — the
/// ONE JSON builder every findings route returns through, so a manual
/// create's response and a disposition set/clear's response are
/// byte-shape-identical to `GET /findings`'s own rows.
///
/// V73-K2b added the five findings-v2 fields (`act`, `blocking`, `cites`,
/// `fingerprint`, `superseded_by`). V73-K1 landed them as COLUMNS and put
/// them on the document read's own `FindingBrief`, but this — the wire every
/// finding CARD in the SPA reads — never carried them, so the two axes D9
/// added were storable and unreadable. Purely additive: no field changed
/// name, type or position, and every pre-V0034 row answers `"issue"` /
/// `false` / `null`, which is what those rows always meant.
fn finding_json(
    f: &ReviewFindingRow,
    resolution: &ResolvedForPs,
    thread_count: usize,
    unresolved_count: usize,
) -> serde_json::Value {
    let confidence = if resolution.orphaned {
        "orphaned"
    } else {
        // A resolved line/range branch always sets `Some(..)`; whole_file/
        // review resolve with `confidence: None` too (no textual match to
        // grade) — "exact" is the honest default for THOSE two kinds
        // (present-only claims, never wrong-line).
        resolution.confidence.unwrap_or("exact")
    };
    let lines_json: Option<serde_json::Value> = f
        .location_lines
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());
    let evidence = if f.evidence_lang.is_some() || f.evidence_source.is_some() {
        serde_json::json!({ "lang": f.evidence_lang, "source": f.evidence_source })
    } else {
        serde_json::Value::Null
    };
    let disposition = if f.disposition.is_some() {
        serde_json::json!({
            "state": f.disposition,
            "note": f.disposition_note,
            "by": f.disposition_by,
            "at": f.disposition_at,
        })
    } else {
        serde_json::Value::Null
    };
    // findings v2 (V73-K1's own columns, V73-K2b's wire). `cites` is the
    // stored JSON re-parsed — a malformed blob degrades to ABSENT rather
    // than to an empty list, because "this finding cites nothing" and "we
    // could not read what it cites" are different facts and a card must be
    // able to say which. Every one of the five is `null`/`false`/absent on a
    // pre-V0034 row, which is exactly what those rows meant.
    let cites: Option<serde_json::Value> = f
        .cites_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());
    serde_json::json!({
        "slug": f.slug,
        "act": f.act,
        "blocking": f.blocking,
        "cites": cites,
        "fingerprint": f.fingerprint,
        "superseded_by": f.superseded_by,
        "severity": f.severity,
        "category": f.category,
        "location": {
            "kind": f.location_kind,
            "path": f.location_path,
            "lines": lines_json,
            "removed": f.location_removed,
        },
        "title": f.title,
        "rationale": f.rationale,
        "recommendation": f.recommendation,
        "evidence": evidence,
        "origin": f.origin,
        "author": f.author,
        "disposition": disposition,
        "published_state": f.published_state,
        "published_at": f.published_at,
        "published_url": f.published_url,
        "superseded": f.superseded,
        "superseded_reason": f.superseded_reason,
        "content_updated_at": f.content_updated_at,
        "annotation_id": f.annotation_id,
        "import_batch_id": f.import_batch_id,
        "created_at": f.created_at,
        "updated_at": f.updated_at,
        "resolution": {
            "line": resolution.line,
            "line_end": resolution.line_end,
            "orphaned": resolution.orphaned,
            "confidence": confidence,
        },
        "thread_count": thread_count,
        "unresolved_count": unresolved_count,
    })
}

/// Compose ONE finding's full view (resolution + thread counts), for the
/// single-finding routes (manual create, disposition set/clear) — resolved
/// against the review's LATEST patchset (those routes take no `?ps=`).
/// Not used by [`list_findings_route`], which batches its own blob-cache +
/// one bulk `list_review_annotations` call across every row instead of
/// N one-off lookups — see that function's own doc.
///
/// `pub(crate)` — PRR-R5's publish-recording routes
/// (`crate::review_github_export`) reuse this exact composition for their
/// own single-finding response (post-publish-record view), rather than a
/// second near-identical builder.
///
/// 2026-08-31 incident (store.rs module doc): takes `&Store` (not
/// `&SharedState`) so every async caller wraps the whole thing in ONE
/// `run_blocking` closure — this fn makes no other use of `state`.
pub(crate) fn compose_finding_view(
    store: &Store,
    repo_root: &Path,
    target_ps: &ReviewPatchsetRow,
    row: &ReviewFindingRow,
) -> Result<serde_json::Value, ApiError> {
    let ann = store.get_annotation(&row.annotation_id)?;
    let all = store.list_review_annotations(row.review_id, true)?;
    let replies: Vec<_> = all
        .into_iter()
        .filter(|r| r.parent_id.as_deref() == Some(row.annotation_id.as_str()))
        .collect();
    let resolution = match &ann {
        Some(a) => {
            let sha =
                review_comments::target_sha_for_side(a.side.as_deref(), target_ps).to_string();
            let content = if a.anchor_kind == annotations::ANCHOR_KIND_REVIEW {
                None
            } else {
                review_comments::read_blob_text(repo_root, &a.path, &sha)
            };
            review_comments::resolve_for_ps_with_content(a, target_ps, &sha, content.as_deref())
        }
        // A finding's annotation is a 1:1 FK — this should be
        // unreachable in practice, but degrading to an honest orphan
        // (rather than panicking or 500ing) mirrors this codebase's
        // "wrong line is worse than an honest orphan" law even for a
        // should-never-happen data-integrity gap.
        None => orphaned_resolution(target_ps),
    };
    let unresolved = replies.iter().filter(|r| !r.resolved).count();
    let mut view = finding_json(row, &resolution, replies.len(), unresolved);
    // V76-B3 (kbc-prose/1) — additive per-field refs, the SAME helper
    // `list_findings_route`'s batch pass uses, so the single-finding routes
    // and the list can never disagree about the key names.
    if let Some(review) = store.get_review(row.review_id)? {
        if let Some(repo_id) = store.repo_id(&review.repo)? {
            let ctx = crate::prose_refs::RefCtx {
                repo_id,
                review_id: Some(row.review_id),
            };
            if let (Some(obj), serde_json::Value::Object(m)) = (
                view.as_object_mut(),
                crate::prose_refs::finding_field_refs(
                    store,
                    &ctx,
                    &row.title,
                    &row.rationale,
                    row.recommendation.as_deref(),
                )?,
            ) {
                obj.extend(m);
            }
        }
    }
    Ok(view)
}

// --- routes --------------------------------------------------------------

/// `POST /api/reviews/{id}/findings/import` — LOOPBACK-ONLY (design doc §2
/// row 8). One transaction via `Store::reconcile_findings_import`
/// (already shipped by R1); one `review.changed{reason:"findings_import"}`
/// on success. `400` (whole-batch, per-index `details`, nothing written)
/// on: unsupported `schema`, invalid `mode`, or any [`validate_import_
/// batch`] failure — including a slug colliding with an existing manual
/// finding (`slug_conflict_manual`, the OWED item). `400` when the review
/// has zero patchsets (same rule `put_verdict`/`put_review_report`
/// enforce).
pub async fn import_findings_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Json(body): Json<FindingsImportBody>,
) -> Result<axum::response::Response, ApiError> {
    let (review, repo, repo_id) = require_review(&state, id).await?;

    if body.schema != IMPORT_SCHEMA {
        return Err(ApiError::bad_request(format!(
            "unsupported schema: {:?} (expected {IMPORT_SCHEMA:?})",
            body.schema
        )));
    }
    let mode = match body.mode.as_deref() {
        None | Some("full") => store::FindingsImportMode::Full,
        Some("additive") => store::FindingsImportMode::Additive,
        Some(other) => {
            return Err(ApiError::bad_request(format!(
                "mode must be full|additive, got {other:?}"
            )));
        }
    };

    let findings_for_validate = body.findings.clone();
    let errors = state
        .store
        .run_blocking(move |store| validate_import_batch(store, id, &findings_for_validate))
        .await?;
    if !errors.is_empty() {
        return Ok((
            StatusCode::BAD_REQUEST,
            [(header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({
                "error": "findings import batch failed validation; nothing written",
                "details": errors,
            })),
        )
            .into_response());
    }

    let ps_param = body.ps_number.map(|n| n.to_string());
    let target_ps = state
        .store
        .run_blocking(move |store| -> Result<ReviewPatchsetRow, ApiError> {
            store
                .latest_patchset(id)?
                .ok_or_else(|| ApiError::bad_request(format!("review {id} has no patchsets")))?;
            resolve_ps(store, id, ps_param.as_deref())
        })
        .await?;

    let (v1_act, v1_blocking, v1_cites, v1_fp, v1_supersedes) =
        store::ImportedFinding::v1_defaults();
    let mut blob_cache: HashMap<(String, String), Option<String>> = HashMap::new();
    let mut imported = Vec::with_capacity(body.findings.len());
    for f in &body.findings {
        let anchor = build_finding_anchor(&repo.path, &mut blob_cache, &target_ps, &f.location)?;
        imported.push(store::ImportedFinding {
            slug: f.slug.clone(),
            severity: f.severity.clone(),
            category: f.category.clone(),
            location_kind: f.location.kind.clone(),
            location_path: f.location.path.clone(),
            location_lines: f.location.lines.as_deref().map(store::location_lines_json),
            location_removed: f.location.removed,
            title: f.title.clone(),
            rationale: f.rationale.clone(),
            recommendation: f.recommendation.clone(),
            evidence_lang: f.evidence.as_ref().and_then(|e| e.lang.clone()),
            evidence_source: f.evidence.as_ref().and_then(|e| e.source.clone()),
            anchor_kind: anchor.anchor_kind,
            anchor: anchor.anchor,
            anchor2: anchor.anchor2,
            side: anchor.side,
            // V73-K1 — a `kbc-findings/1` row carries no v2 axes; these
            // defaults are exactly what such a row has always meant.
            act: v1_act.clone(),
            blocking: v1_blocking,
            cites_json: v1_cites.clone(),
            fingerprint: v1_fp.clone(),
            supersedes: v1_supersedes.clone(),
        });
    }

    let import_batch_id = format!("batch_{}", annotations::short_random_hex());
    let author = body.author.clone().unwrap_or_else(|| "claude".to_string());
    let now = now_unix();
    let ps_number = target_ps.ps_number;
    let import_batch_id_c = import_batch_id.clone();
    let author_c = author.clone();
    let outcome = state
        .store
        .run_blocking(move |store| {
            store.reconcile_findings_import(
                id,
                repo_id,
                ps_number,
                &import_batch_id_c,
                &author_c,
                &imported,
                mode,
                now,
            )
        })
        .await?;

    emit_findings_review_changed(&state.bus, id, &review.repo, "findings_import", None);

    Ok((
        StatusCode::OK,
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "created": outcome.created,
            "updated": outcome.updated,
            "superseded": outcome.superseded,
            "unchanged": outcome.unchanged,
            "review_id": id,
            "ps_number": target_ps.ps_number,
            "import_batch_id": import_batch_id,
        })),
    )
        .into_response())
}

/// `kbc-compose/1` (V70-R, design doc D9 "the one authoring transaction,"
/// scoped to v0) — a `compose` body's `findings` block reuses
/// [`FindingsImportBody`] byte-for-byte (same `schema`/`mode`/`ps_number`/
/// `author`/`findings` fields `POST .../findings/import` accepts), so a
/// caller building "today's findings JSON" for `import` can hand the exact
/// same object to `compose` under this key.
#[derive(Debug, Deserialize)]
pub struct ComposeBody {
    #[serde(default)]
    pub schema: Option<String>,
    /// The report's prose summary — required on the V0 path (Track R's
    /// "summary + findings JSON" scope); becomes `report.summary`. On the
    /// V73-K1 DOCUMENT path it is ignored and `summary_md` is used instead
    /// (a document that says two different things in two places would be
    /// the worst of both).
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub risk_score: Option<i64>,
    #[serde(default)]
    pub verdict_headline: Option<String>,
    /// Report prose about the verdict — `report.verdict_body`. Distinct
    /// from `verdict_note` below (the short note attached to the
    /// review-LEVEL verdict action, `reviews.verdict_note`).
    #[serde(default)]
    pub verdict_body: Option<String>,
    #[serde(default)]
    pub stats: Option<serde_json::Value>,
    /// `approve` | `comment` | `request-changes` — when present, sets
    /// BOTH the report's own `verdict` field (a plain string, same as
    /// `PUT /report`) AND the review-level `reviews.verdict` column (same
    /// state vocabulary `PUT /verdict` enforces). Omit to touch neither.
    #[serde(default)]
    pub verdict: Option<String>,
    #[serde(default)]
    pub verdict_note: Option<String>,
    /// The V0 findings block (`kbc-findings/1`). Required on the V0 path;
    /// absent on the document path, where `findings_v2` (or the document's
    /// own `findings:` front matter) carries them.
    #[serde(default)]
    pub findings: Option<FindingsImportBody>,

    // --- V73-K1: the kbc-review/1 document path -------------------------
    /// The WHOLE `kbc-review/1` document (YAML front matter + Markdown
    /// body). Its presence is what selects the document path; absent, this
    /// route behaves EXACTLY as it did before V73-K1.
    #[serde(default)]
    pub doc_md: Option<String>,
    /// `minimal` | `standard` | `full` — what this document promises.
    /// Defaults to `standard`.
    #[serde(default)]
    pub tier: Option<String>,
    /// The findings SIDECAR: either a bare JSON array of v2 findings, or
    /// `{"findings": [...]}`. Overrides the document's own `findings:`
    /// front matter when present.
    #[serde(default)]
    pub findings_v2: Option<serde_json::Value>,
    /// `full` (default) | `additive` — the reconciliation mode for the
    /// document path (the V0 path reads `findings.mode` instead).
    #[serde(default)]
    pub mode: Option<String>,
    /// Patchset to compose against; defaults to latest.
    #[serde(default)]
    pub ps_number: Option<i64>,
    /// The author identity threaded onto newly created findings'
    /// annotations. Defaults to `"claude"`, as on the import path.
    #[serde(default)]
    pub author: Option<String>,
    /// Lint + resolve only. Nothing is written, no event fires, and the
    /// response carries the lint and the resolved cards so an author can
    /// see exactly what would land.
    #[serde(default)]
    pub dry_run: bool,
    /// V76-R1c — internal: `(slug, original, canonical)` triples from the
    /// V0 category mapping, spliced onto the document lint as INFO. Never
    /// on the wire.
    #[serde(default, skip)]
    pub v0_category_notes: Vec<(String, String, String)>,
}

pub const COMPOSE_SCHEMA: &str = "kbc-compose/1";

/// `POST /api/reviews/{id}/compose` — LOOPBACK-ONLY, same review-mutation
/// family as `/findings/import` and `PUT /report` (V70-R, design doc D9
/// scoped to v0: "summary + today's findings JSON, one transaction").
/// Runs the SAME validation `import_findings_route` does (schema, mode,
/// [`validate_import_batch`], patchset resolution, per-finding anchor
/// derivation), then normalises the report half through the EXACT SAME
/// [`crate::reviews::normalize_report_shape`] `PUT /report` uses, then
/// writes findings + report + an optional review-level verdict in ONE
/// sqlite transaction via [`Store::compose_review`] — a real rollback unit,
/// not three separate commits. `400` (whole-batch, nothing written) on any
/// of: unsupported `findings.schema`, invalid `findings.mode`, a
/// [`validate_import_batch`] failure, an invalid `verdict` state, or a
/// report-shape violation (`urn:kb:errors:report-shape`, same as
/// `PUT /report`). `400` when the review has zero patchsets (same rule
/// every sibling review-mutation route enforces). Emits ONE
/// `review.changed{reason:"compose"}` on success (never the per-route
/// `"findings_import"`/`"report"`/`"verdict"` reasons — a `compose` call is
/// one caller-visible event, matching `emit_review_changed`'s existing
/// `reason` vocabulary rather than growing it three-wide for one call).
pub async fn compose_review_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Json(body): Json<ComposeBody>,
) -> Result<axum::response::Response, ApiError> {
    // V73-K1 — one route, two shapes. `doc_md` selects the `kbc-review/1`
    // DOCUMENT path. V76-R1c: the V0 shape (`summary` + `kbc-findings/1`)
    // is folded into the same path after synthesising a minimal document,
    // so `GET …/doc`, `lint` and `render` work after either form.
    if body.doc_md.is_some() {
        return compose_document(state, id, body).await;
    }
    compose_v0(state, id, body).await
}

/// The V70-R v0 authoring call — summary + a `kbc-findings/1` block.
///
/// V76-R1c folds this into the document path: categories are mapped onto
/// [`review_doc::CATEGORIES`] (never rejected), slugs that fail
/// [`FINDING_SLUG_PATTERN`] 400 with the regex, and a `minimal`-tier
/// `kbc-review/1` document is synthesised and stored through
/// [`compose_document`] so the two forms share one reconcile core
/// (`FindingIdentity::Fingerprint`).
async fn compose_v0(
    state: SharedState,
    id: i64,
    mut body: ComposeBody,
) -> Result<axum::response::Response, ApiError> {
    let summary = body
        .summary
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| body.verdict_note.clone().filter(|s| !s.trim().is_empty()))
        .or_else(|| body.verdict_body.clone().filter(|s| !s.trim().is_empty()));
    let Some(summary) = summary else {
        return Err(ApiError::bad_request(
            "compose requires `summary` (or `verdict_note` / `verdict_body`), or a `doc_md` \
             document, which carries its own `summary_md`",
        ));
    };
    let Some(findings_body) = body.findings.take() else {
        return Err(ApiError::bad_request(
            "compose requires a `findings` block (kbc-findings/1), or a `doc_md` document \
             whose findings ride its front matter or the `findings_v2` sidecar",
        ));
    };

    if let Some(s) = body.schema.as_deref() {
        if s != COMPOSE_SCHEMA {
            return Err(ApiError::bad_request(format!(
                "unsupported schema: {s:?} (expected {COMPOSE_SCHEMA:?})"
            )));
        }
    }
    if findings_body.schema != IMPORT_SCHEMA {
        return Err(ApiError::bad_request(format!(
            "unsupported findings.schema: {:?} (expected {IMPORT_SCHEMA:?})",
            findings_body.schema
        )));
    }

    let bad_slugs: Vec<String> = findings_body
        .findings
        .iter()
        .filter(|f| !is_valid_finding_slug(&f.slug))
        .map(|f| f.slug.clone())
        .collect();
    if !bad_slugs.is_empty() {
        let first = &bad_slugs[0];
        return Ok((
            StatusCode::BAD_REQUEST,
            [(header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({
                "error": format!("slug must match {FINDING_SLUG_PATTERN}, got {first:?}"),
                "regex": FINDING_SLUG_PATTERN,
                "slug": first,
                "slugs": bad_slugs,
            })),
        )
            .into_response());
    }

    let (v1_act, v1_blocking, ..) = store::ImportedFinding::v1_defaults();
    let mut doc_findings: Vec<crate::review_doc::DocFinding> =
        Vec::with_capacity(findings_body.findings.len());
    let mut notes: Vec<(String, String, String)> = Vec::new();
    for f in &findings_body.findings {
        let mapped = crate::review_doc::map_v0_category(&f.category);
        if mapped.rewritten {
            notes.push((
                f.slug.clone(),
                mapped.normalised.clone(),
                mapped.canonical.to_string(),
            ));
        }
        doc_findings.push(crate::review_doc::DocFinding {
            slug: Some(f.slug.clone()),
            act: v1_act.clone(),
            severity: f.severity.clone(),
            category: mapped.canonical.to_string(),
            blocking: v1_blocking,
            title: f.title.clone(),
            rationale: f.rationale.clone(),
            recommendation: f.recommendation.clone(),
            location: f.location.clone(),
            cites: Vec::new(),
            supersedes: Vec::new(),
            evidence: f.evidence.clone(),
        });
    }

    let doc_md = crate::review_doc::synthesize_v0_document(&summary, &doc_findings);
    let findings_v2 = serde_json::to_value(&doc_findings)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    body.doc_md = Some(doc_md);
    body.findings_v2 = Some(findings_v2);
    body.tier = Some(crate::review_doc::Tier::Minimal.as_str().to_string());
    if body.mode.is_none() {
        body.mode = findings_body.mode.clone();
    }
    if body.ps_number.is_none() {
        body.ps_number = findings_body.ps_number;
    }
    if body.author.is_none() {
        body.author = findings_body.author.clone();
    }
    body.v0_category_notes = notes;
    // `summary` stays on the body so `compose_document` can still put it
    // on the report (it prefers `summary_md` from the synthesised doc,
    // which is the same string).
    body.summary = Some(summary);

    compose_document(state, id, body).await
}

/// V73-K1 — `compose`'s `kbc-review/1` DOCUMENT path (design D9's "the one
/// authoring transaction").
///
/// One call: validate the front matter against the tier, lint (including
/// resolving every ref), reconcile the findings BY FINGERPRINT (minting
/// never-reused `f-<n>` slugs, tombstoning what vanished, never touching a
/// `manual` row), append the document revision, set the report, optionally
/// set the review-level verdict — all inside ONE sqlite transaction
/// ([`Store::compose_review_doc`]) — then emit exactly ONE
/// `review.changed{reason:"compose"}` and return the resolved read.
///
/// `dry_run` stops after the lint and returns what WOULD land, writing
/// nothing and emitting nothing. Any lint ERROR is a `400` carrying the
/// whole lint (every problem at once, never just the first), and nothing is
/// written.
///
/// The report is composed from the DOCUMENT — `summary_md` becomes
/// `report.summary` — and normalised through the exact same
/// `reviews::normalize_report_shape` `PUT /report` uses. It deliberately
/// synthesises no `risk_score`: `risk` is a level plus a sentence, and
/// coercing that into a number would be a precision the document never
/// claimed.
async fn compose_document(
    state: SharedState,
    id: i64,
    body: ComposeBody,
) -> Result<axum::response::Response, ApiError> {
    use crate::review_doc::{self, routes as doc_routes, Tier};

    let doc_md = body.doc_md.clone().expect("dispatched on doc_md");
    let (review, repo, repo_id) = require_review(&state, id).await?;

    if let Some(sc) = body.schema.as_deref() {
        if sc != COMPOSE_SCHEMA && sc != review_doc::SCHEMA {
            return Err(ApiError::bad_request(format!(
                "unsupported schema: {sc:?} (expected {COMPOSE_SCHEMA:?} or {:?})",
                review_doc::SCHEMA
            )));
        }
    }
    let tier = match body.tier.as_deref() {
        None => Tier::Standard,
        Some(t) => Tier::parse(t).ok_or_else(|| {
            ApiError::bad_request(format!(
                "tier must be {}, got {t:?}",
                review_doc::TIERS.join("|")
            ))
        })?,
    };
    let mode = match body.mode.as_deref() {
        None | Some("full") => store::FindingsImportMode::Full,
        Some("additive") => store::FindingsImportMode::Additive,
        Some(other) => {
            return Err(ApiError::bad_request(format!(
                "mode must be full|additive, got {other:?}"
            )));
        }
    };
    if let Some(v) = body.verdict.as_deref() {
        crate::reviews::parse_verdict_state(v)?;
    }

    // The findings SIDECAR: a bare array, or `{"findings": [...]}`. Absent
    // ⇒ the document's own front matter carries them (and if it does not
    // either, the tier lint says so by name).
    let sidecar: Option<Vec<review_doc::DocFinding>> = match &body.findings_v2 {
        None => None,
        Some(v) => {
            let arr = match v {
                serde_json::Value::Array(_) => v.clone(),
                serde_json::Value::Object(m) => m.get("findings").cloned().ok_or_else(|| {
                    ApiError::bad_request("`findings_v2` object must carry a `findings` array")
                })?,
                _ => {
                    return Err(ApiError::bad_request(
                        "`findings_v2` must be an array of findings or {\"findings\": [...]}",
                    ));
                }
            };
            Some(serde_json::from_value(arr).map_err(|e| {
                ApiError::bad_request(format!("`findings_v2` is not a v2 finding list: {e}"))
            })?)
        }
    };

    let ps_param = body.ps_number.map(|n| n.to_string());
    let target_ps = state
        .store
        .run_blocking(move |store| -> Result<ReviewPatchsetRow, ApiError> {
            store
                .latest_patchset(id)?
                .ok_or_else(|| ApiError::bad_request(format!("review {id} has no patchsets")))?;
            resolve_ps(store, id, ps_param.as_deref())
        })
        .await?;

    let prepared = doc_routes::prepare_doc(
        &state,
        id,
        repo_id,
        &repo.path,
        &target_ps,
        &doc_md,
        tier,
        sidecar.as_deref(),
    )
    .await?;

    let mapping_rows: Vec<crate::review_doc::lint::LintRow> = body
        .v0_category_notes
        .iter()
        .map(|(slug, from, to)| crate::review_doc::lint::LintRow::category_mapped(slug, from, to))
        .collect();
    let prepared = match prepared {
        Ok(mut p) => {
            p.lint = p.lint.with_extra(mapping_rows);
            p
        }
        Err((lint, cards)) => {
            let lint = lint.with_extra(mapping_rows);
            return Ok((
                StatusCode::BAD_REQUEST,
                [(header::CACHE_CONTROL, "no-store")],
                Json(serde_json::json!({
                    "error": "the review document failed lint; nothing written",
                    "lint": lint,
                    "cards": cards,
                })),
            )
                .into_response());
        }
    };

    // An EXPLICIT slug that names an existing human-authored finding is
    // refused at the route boundary, whole-compose, nothing written — the
    // same guard `validate_import_batch`'s `slug_conflict_manual` applies on
    // the v1 path, for the same reason (V0024's origin rule: a `manual` row
    // is never superseded or overwritten by an agent's compose).
    let explicit: Vec<String> = prepared
        .findings
        .iter()
        .filter_map(|f| f.slug.clone())
        .collect();
    if !explicit.is_empty() {
        let explicit_c = explicit.clone();
        let conflicts = state
            .store
            .run_blocking(move |store| -> Result<Vec<String>, ApiError> {
                let mut out = Vec::new();
                for slug in &explicit_c {
                    if let Some(row) = store.get_review_finding(id, slug)? {
                        if row.origin == store::FINDING_ORIGIN_MANUAL {
                            out.push(slug.clone());
                        }
                    }
                }
                Ok(out)
            })
            .await?;
        if !conflicts.is_empty() {
            return Err(ApiError::bad_request(format!(
                "these slugs name human-authored findings, which a compose may never \
                 overwrite: {} — drop the explicit slug and a fresh one is minted",
                conflicts.join(", ")
            )));
        }
    }

    if body.dry_run {
        return Ok((
            StatusCode::OK,
            [(header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({
                "schema": review_doc::SCHEMA,
                "dry_run": true,
                "review_id": id,
                "ps_number": target_ps.ps_number,
                "tier": tier.as_str(),
                "lint": prepared.lint,
                "cards": prepared.cards,
                "omitted": review_doc::omitted_blocks(&prepared.doc),
                "would_write": {
                    "findings": prepared.findings.len(),
                    "doc_bytes": doc_md.len(),
                },
            })),
        )
            .into_response());
    }

    // Derive each finding's annotation anchor from the TARGET patchset's
    // pinned blob — the same `build_finding_anchor` the v1 import path uses,
    // so a v2 finding's carry-forward ladder is the identical one.
    let mut blob_cache: HashMap<(String, String), Option<String>> = HashMap::new();
    let mut imported = Vec::with_capacity(prepared.findings.len());
    for f in &prepared.findings {
        let anchor = build_finding_anchor(&repo.path, &mut blob_cache, &target_ps, &f.location)?;
        imported.push(store::ImportedFinding {
            slug: f.slug.clone().unwrap_or_default(),
            severity: f.severity.clone(),
            category: f.category.clone(),
            location_kind: f.location.kind.clone(),
            location_path: f.location.path.clone(),
            location_lines: f.location.lines.as_deref().map(store::location_lines_json),
            location_removed: f.location.removed,
            title: f.title.clone(),
            rationale: f.rationale.clone(),
            recommendation: f.recommendation.clone(),
            evidence_lang: f.evidence.as_ref().and_then(|e| e.lang.clone()),
            evidence_source: f.evidence.as_ref().and_then(|e| e.source.clone()),
            anchor_kind: anchor.anchor_kind,
            anchor: anchor.anchor,
            anchor2: anchor.anchor2,
            side: anchor.side,
            act: f.act.clone(),
            blocking: f.blocking,
            cites_json: if f.cites.is_empty() {
                None
            } else {
                serde_json::to_string(&f.cites).ok()
            },
            fingerprint: Some(f.fingerprint()),
            supersedes: f.supersedes.clone(),
        });
    }

    let mut report_obj = serde_json::json!({
        "schema": review_doc::SCHEMA,
        "summary": prepared.doc.summary_md,
    });
    if let Some(h) = &body.verdict_headline {
        report_obj["verdict_headline"] = serde_json::json!(h);
    }
    if let Some(b) = &body.verdict_body {
        report_obj["verdict_body"] = serde_json::json!(b);
    }
    if let Some(stats) = &body.stats {
        report_obj["stats"] = stats.clone();
    }
    if let Some(v) = &body.verdict {
        report_obj["verdict"] = serde_json::json!(v);
    }
    let mut report_obj = match crate::reviews::normalize_report_shape(report_obj) {
        Ok(v) => v,
        Err(problem) => return Ok(*problem),
    };
    let now = now_unix();
    report_obj
        .as_object_mut()
        .expect("normalize_report_shape guarantees an object")
        .insert("generated_at".to_string(), serde_json::json!(now));
    let report_json = serde_json::to_string(&report_obj)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let import_batch_id = format!("batch_{}", annotations::short_random_hex());
    let author = body.author.clone().unwrap_or_else(|| "claude".to_string());
    let new_row = prepared.new_row.clone();
    let import_batch_id_c = import_batch_id.clone();
    let verdict_state = body.verdict.clone();
    let verdict_note = body.verdict_note.clone();
    let outcome = state
        .store
        .run_blocking(move |store| {
            store.compose_review_doc(
                &new_row,
                repo_id,
                &import_batch_id_c,
                &author,
                &imported,
                mode,
                &report_json,
                verdict_state
                    .as_deref()
                    .map(|s| (s, verdict_note.as_deref())),
                now,
            )
        })
        .await?;

    // ONE event for the whole transaction — never three, and never one per
    // finding (the same rule V70-R's own `compose` follows).
    emit_review_changed(&state.bus, id, &review.repo, "compose", false);

    let (doc_out, _, _) =
        doc_routes::load_doc_out(&state, id, Some(&target_ps.ps_number.to_string()), true).await?;

    Ok((
        StatusCode::OK,
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": review_doc::SCHEMA,
            "dry_run": false,
            "review_id": id,
            "ps_number": target_ps.ps_number,
            "revision": outcome.revision,
            "import_batch_id": import_batch_id,
            "findings": {
                "created": outcome.findings.created,
                "updated": outcome.findings.updated,
                "superseded": outcome.findings.superseded,
                "unchanged": outcome.findings.unchanged,
            },
            "report_set": outcome.report_set,
            "verdict_changed": outcome.verdict_changed,
            "lint": prepared.lint,
            "doc": doc_out,
        })),
    )
        .into_response())
}

/// `POST /api/reviews/{id}/findings` (addendum §E) — S2-B GATED
/// (`router.rs`'s `review_remote` sub-router: loopback unconditionally,
/// else `[review] remote_mutations`, default OFF — see
/// [`crate::review_gate::review_mutations_gate`]). Create
/// ONE manual (`origin="manual"`) finding, anchored against the review's
/// LATEST patchset. `slug` optional (derived `f-<kebab-of-title>`,
/// uniquified `-2`/`-3`… — [`derive_unique_slug`]); an EXPLICIT slug that
/// already exists on this review is `409`. Emits
/// `review.changed{reason:"findings_import", finding_slug}` (design doc's
/// literal reason string for this route, per addendum §E — reused rather
/// than a new `"manual_finding"` reason so every findings-affecting
/// mutation stays under one reason for the watch-loop's dedup, matching
/// how `disposition` alone gets its own reason). `body.evidence`
/// (V70-A3X, [`FindingEvidenceBody`]) is optional and stored verbatim as
/// `evidence_lang`/`evidence_source` — the SAME field the `import` path
/// (above) has always accepted; a manual `add` simply never threaded it
/// through before this fix.
pub async fn create_manual_finding_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Json(body): Json<CreateManualFindingBody>,
) -> Result<axum::response::Response, ApiError> {
    let (review, repo, repo_id) = require_review(&state, id).await?;

    if !store::is_valid_severity(&body.severity) {
        return Err(ApiError::bad_request(format!(
            "severity must be blocker|concern|ok, got {:?}",
            body.severity
        )));
    }
    if let Err(e) = validate_location_shape(&body.location) {
        let (_, message) = describe_location_error(e, &body.location);
        return Err(ApiError::bad_request(message));
    }

    // 2026-08-31 incident (store.rs module doc): latest_patchset + the
    // slug lookup/derivation are contiguous store work — one blocking-pool
    // trip. `SlugOutcome` carries the 409-conflict branch back out since a
    // closure can't early-return the OUTER response.
    enum SlugOutcome {
        Conflict(String),
        Slug(String),
    }
    let body_slug = body.slug.clone();
    let body_title = body.title.clone();
    let (target_ps, slug_outcome) = state
        .store
        .run_blocking(
            move |store| -> Result<(ReviewPatchsetRow, SlugOutcome), ApiError> {
                let target_ps = store.latest_patchset(id)?.ok_or_else(|| {
                    ApiError::bad_request(format!("review {id} has no patchsets"))
                })?;
                let outcome = match &body_slug {
                    Some(s) => {
                        if !is_valid_finding_slug(s) {
                            return Err(ApiError::bad_request(format!(
                                "slug must match {FINDING_SLUG_PATTERN}, got {s:?}"
                            )));
                        }
                        if store.get_review_finding(id, s)?.is_some() {
                            SlugOutcome::Conflict(s.clone())
                        } else {
                            SlugOutcome::Slug(s.clone())
                        }
                    }
                    None => SlugOutcome::Slug(derive_unique_slug(store, id, &body_title)?),
                };
                Ok((target_ps, outcome))
            },
        )
        .await?;
    let slug = match slug_outcome {
        SlugOutcome::Conflict(s) => {
            return Ok((
                StatusCode::CONFLICT,
                [(header::CACHE_CONTROL, "no-store")],
                Json(serde_json::json!({
                    "error": format!("finding slug already exists on this review: {s:?}"),
                })),
            )
                .into_response());
        }
        SlugOutcome::Slug(s) => s,
    };

    let author = body.author.clone().unwrap_or_else(|| "you".to_string());
    let mut blob_cache: HashMap<(String, String), Option<String>> = HashMap::new();
    let anchor = build_finding_anchor(&repo.path, &mut blob_cache, &target_ps, &body.location)?;

    let now = now_unix();
    let new = store::NewReviewFinding {
        review_id: id,
        repo_id,
        ps_number: target_ps.ps_number,
        slug: slug.clone(),
        severity: body.severity.clone(),
        category: body.category.clone(),
        location_kind: body.location.kind.clone(),
        location_path: body.location.path.clone(),
        location_lines: body
            .location
            .lines
            .as_deref()
            .map(store::location_lines_json),
        location_removed: body.location.removed,
        title: body.title.clone(),
        rationale: body.rationale.clone(),
        recommendation: body.recommendation.clone(),
        evidence_lang: body.evidence.as_ref().and_then(|e| e.lang.clone()),
        evidence_source: body.evidence.as_ref().and_then(|e| e.source.clone()),
        anchor_kind: anchor.anchor_kind,
        anchor: anchor.anchor,
        anchor2: anchor.anchor2,
        side: anchor.side,
        author: author.clone(),
        import_batch_id: "manual".to_string(),
        origin: store::FINDING_ORIGIN_MANUAL.to_string(),
        finding_author: Some(author),
        // V73-K1 — a human-authored finding created through the v1 route
        // carries no v2 axes and, deliberately, no fingerprint: a manual
        // finding is never matched by content (V0024's origin rule says a
        // compose may not adopt or supersede one), so giving it one would
        // suggest a reconciliation that must never happen.
        act: "issue".to_string(),
        blocking: false,
        cites_json: None,
        fingerprint: None,
    };
    let slug_c = slug.clone();
    let row = state
        .store
        .run_blocking(move |store| -> Result<ReviewFindingRow, ApiError> {
            store.insert_review_finding(&new, now)?;
            store.get_review_finding(id, &slug_c)?.ok_or_else(|| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "finding vanished immediately after insert",
                )
            })
        })
        .await?;

    emit_findings_review_changed(&state.bus, id, &review.repo, "findings_import", Some(&slug));

    let repo_root = repo.path.clone();
    let target_ps_c = target_ps.clone();
    let row_c = row.clone();
    let view = state
        .store
        .run_blocking(move |store| compose_finding_view(store, &repo_root, &target_ps_c, &row_c))
        .await?;
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(view),
    )
        .into_response())
}

/// `GET /api/reviews/{id}/findings?ps=&disposition=&include_superseded=`
/// (design doc §2 row 9). Bearer. Every row carries a `resolution` block
/// (`line`/`line_end`/`orphaned`/`confidence`) computed via the SAME
/// carry-forward ladder `/comments` uses, plus `thread_count`/
/// `unresolved_count` derived from the linked annotation's replies.
/// Batches its own blob cache + ONE `list_review_annotations` call across
/// every finding (not N one-off lookups) — see `compose_finding_view`'s
/// doc for why the single-finding routes don't share this exact loop.
pub async fn list_findings_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Query(params): Query<ListFindingsParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (review, repo, repo_id) = require_review(&state, id).await?;
    if let Some(d) = params.disposition.as_deref() {
        if !store::is_valid_disposition(d) {
            return Err(ApiError::bad_request(format!(
                "disposition must be agree|dispute|waive|fix-later, got {d:?}"
            )));
        }
    }
    // 2026-08-31 incident (store.rs module doc): the three sequential
    // reads below (ps resolve, findings, annotations) are contiguous
    // store work — one blocking-pool trip.
    let ps_param = params.ps.clone();
    let disposition_param = params.disposition.clone();
    let include_superseded = params.include_superseded;
    let (target_ps, findings, ann_rows) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let target_ps = resolve_ps(store, id, ps_param.as_deref())?;
            let findings =
                store.list_review_findings(id, disposition_param.as_deref(), include_superseded)?;
            let ann_rows = store.list_review_annotations(id, true)?;
            Ok((target_ps, findings, ann_rows))
        })
        .await?;

    let mut by_id: HashMap<String, store::AnnotationRow> = HashMap::new();
    let mut replies_by_parent: HashMap<String, Vec<store::AnnotationRow>> = HashMap::new();
    for row in ann_rows {
        match row.parent_id.clone() {
            Some(pid) => replies_by_parent.entry(pid).or_default().push(row),
            None => {
                by_id.insert(row.id.clone(), row);
            }
        }
    }

    let mut blob_cache: HashMap<(String, String), Option<String>> = HashMap::new();
    let mut out = Vec::with_capacity(findings.len());
    for f in &findings {
        let ann = by_id.get(&f.annotation_id);
        let replies = replies_by_parent
            .remove(&f.annotation_id)
            .unwrap_or_default();
        let resolution = match ann {
            Some(a) => {
                let sha =
                    review_comments::target_sha_for_side(a.side.as_deref(), &target_ps).to_string();
                let content = if a.anchor_kind == annotations::ANCHOR_KIND_REVIEW {
                    None
                } else {
                    let key = (a.path.clone(), sha.clone());
                    if !blob_cache.contains_key(&key) {
                        blob_cache.insert(
                            key.clone(),
                            review_comments::read_blob_text(&repo.path, &a.path, &sha),
                        );
                    }
                    blob_cache.get(&key).and_then(|c| c.as_deref())
                };
                review_comments::resolve_for_ps_with_content(a, &target_ps, &sha, content)
            }
            None => orphaned_resolution(&target_ps),
        };
        let unresolved = replies.iter().filter(|r| !r.resolved).count();
        out.push(finding_json(f, &resolution, replies.len(), unresolved));
    }

    // V76-B3 (kbc-prose/1) — every prose field carries its refs, computed
    // per request through `prose_refs`'s ladders (store reads only, never
    // persisted, capped at `prose_refs::MAX_REFS_PER_FIELD` with an honest
    // `truncated`). ONE blocking-pool trip for the whole batch — the same
    // store-mutex discipline as the three reads above.
    let ref_inputs: Vec<(String, String, Option<String>)> = findings
        .iter()
        .map(|f| {
            (
                f.title.clone(),
                f.rationale.clone(),
                f.recommendation.clone(),
            )
        })
        .collect();
    let refs_list = state
        .store
        .run_blocking(move |store| -> Result<Vec<serde_json::Value>, ApiError> {
            let ctx = crate::prose_refs::RefCtx {
                repo_id,
                review_id: Some(id),
            };
            ref_inputs
                .iter()
                .map(|(t, r, rec)| {
                    crate::prose_refs::finding_field_refs(store, &ctx, t, r, rec.as_deref())
                })
                .collect()
        })
        .await?;
    for (v, refs) in out.iter_mut().zip(refs_list) {
        if let (Some(obj), serde_json::Value::Object(m)) = (v.as_object_mut(), refs) {
            obj.extend(m);
        }
    }

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": SCHEMA,
            "review_id": id,
            "repo": review.repo,
            "ps": target_ps.ps_number,
            "findings": out,
        })),
    ))
}

/// `PUT /api/reviews/{id}/findings/{slug}/disposition` (design doc §2 row
/// 10; Risk #3) — S2-B GATED, same admission table as
/// [`crate::review_gate::review_mutations_gate`] / `PUT
/// /reviews/{id}/verdict`. Mirrors `PUT /reviews/{id}/verdict`'s shape.
/// `404` for an unknown `(review, slug)`. Emits
/// `review.changed{reason:"disposition", finding_slug}` only when the
/// disposition actually changed (same no-op convention `put_verdict`
/// uses via `Store::set_finding_disposition`'s own `Ok(Some(false))`).
pub async fn set_finding_disposition_route(
    State(state): State<SharedState>,
    AxumPath((id, slug)): AxumPath<(i64, String)>,
    Json(body): Json<SetFindingDispositionBody>,
) -> Result<impl IntoResponse, ApiError> {
    let (review, repo, _repo_id) = require_review(&state, id).await?;
    if !store::is_valid_disposition(&body.disposition) {
        return Err(ApiError::bad_request(format!(
            "disposition must be agree|dispute|waive|fix-later, got {:?}",
            body.disposition
        )));
    }
    let by = body.author.clone().unwrap_or_else(|| "you".to_string());
    let now = now_unix();
    let slug_c = slug.clone();
    let disposition = body.disposition.clone();
    let note = body.note.clone();
    let changed = state
        .store
        .run_blocking(move |store| {
            store.set_finding_disposition(id, &slug_c, &disposition, note.as_deref(), &by, now)
        })
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no finding {slug:?} on review {id}")))?;
    if changed {
        emit_findings_review_changed(&state.bus, id, &review.repo, "disposition", Some(&slug));
    }
    let slug_c2 = slug.clone();
    let (target_ps, row) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let target_ps = store
                .latest_patchset(id)?
                .ok_or_else(|| ApiError::not_found(format!("review {id} has no patchsets")))?;
            let row = store.get_review_finding(id, &slug_c2)?.ok_or_else(|| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "finding vanished immediately after disposition set",
                )
            })?;
            Ok((target_ps, row))
        })
        .await?;
    let repo_root = repo.path.clone();
    let target_ps_c = target_ps.clone();
    let row_c = row.clone();
    let view = state
        .store
        .run_blocking(move |store| compose_finding_view(store, &repo_root, &target_ps_c, &row_c))
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(view)))
}

/// `DELETE /api/reviews/{id}/findings/{slug}/disposition` — S2-B GATED,
/// same admission table as [`set_finding_disposition_route`] above.
/// Clears a finding's disposition back to undecided. `404` for an unknown
/// `(review, slug)`. Emits `review.changed{reason:"disposition",
/// finding_slug}` only when there was something to clear.
pub async fn clear_finding_disposition_route(
    State(state): State<SharedState>,
    AxumPath((id, slug)): AxumPath<(i64, String)>,
) -> Result<impl IntoResponse, ApiError> {
    let (review, repo, _repo_id) = require_review(&state, id).await?;
    let now = now_unix();
    let slug_c = slug.clone();
    let changed = state
        .store
        .run_blocking(move |store| store.clear_finding_disposition(id, &slug_c, now))
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no finding {slug:?} on review {id}")))?;
    if changed {
        emit_findings_review_changed(&state.bus, id, &review.repo, "disposition", Some(&slug));
    }
    let slug_c2 = slug.clone();
    let (target_ps, row) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let target_ps = store
                .latest_patchset(id)?
                .ok_or_else(|| ApiError::not_found(format!("review {id} has no patchsets")))?;
            let row = store.get_review_finding(id, &slug_c2)?.ok_or_else(|| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "finding vanished immediately after disposition clear",
                )
            })?;
            Ok((target_ps, row))
        })
        .await?;
    let repo_root = repo.path.clone();
    let target_ps_c = target_ps.clone();
    let row_c = row.clone();
    let view = state
        .store
        .run_blocking(move |store| compose_finding_view(store, &repo_root, &target_ps_c, &row_c))
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(view)))
}

// --- PRR-F: recurring-finding memory (design-ui.md §12.4, frontier 4) -----

/// `GET /api/reviews/{id}/findings/recurrence`'s own schema tag.
pub const RECURRENCE_SCHEMA: &str = "review-findings-recurrence/1";

/// One prior review a finding recurred in — design-ui.md §12.4's own wire
/// shape: `{review_id, pr_number?, title, created_at}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecurrencePriorReview {
    pub review_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<i64>,
    pub title: Option<String>,
    pub created_at: i64,
}

/// One recurring finding on THIS review. `seen_in_reviews` is the SAME
/// `review_count` [`store::Store::recurrence_pairs`] (the shared R9
/// recurrence query — design-addendum-2 §C's own note: "this is also the
/// frontier recurring-finding query — one shared store fn, two consumers")
/// reports for this finding's `(category, location_path)` pair.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FindingRecurrenceRow {
    pub slug: String,
    pub seen_in_reviews: i64,
    pub prior: Vec<RecurrencePriorReview>,
}

/// Pure core: which of `findings` (a single review's own, already non-
/// superseded rows) are recurring per `pairs` (already scoped to this
/// review's repo by the caller, already filtered to `>= RECURRENCE_MIN_
/// REVIEWS` distinct reviews by `Store::recurrence_pairs` itself), and
/// which OTHER review ids each one recurred in. `review_id` is the review
/// `findings` belongs to — excluded from every row's own prior-review list
/// (a finding is never "prior" to itself). Returns `(slug, seen_in_reviews,
/// prior_review_ids)` — prior ids in `pairs`' own ascending order (see
/// [`store::RecurrenceRow`]'s doc), so the route's I/O loop (review title/
/// pr_number lookups) runs over an already-deterministic sequence.
///
/// Pure + IO-free (no `Store` reference) so this fn is unit-testable with
/// fixed fixture rows, same "pure core, thin route" shape `review_
/// analytics::compute_analytics`/`review_timeline::compose_review_timeline`
/// already establish in this crate.
pub(crate) fn recurring_findings(
    findings: &[ReviewFindingRow],
    review_id: i64,
    pairs: &[store::RecurrenceRow],
) -> Vec<(String, i64, Vec<i64>)> {
    let mut by_key: HashMap<(&str, &str), &store::RecurrenceRow> = HashMap::new();
    for p in pairs {
        by_key.insert((p.category.as_str(), p.location_path.as_str()), p);
    }
    let mut out = Vec::new();
    for f in findings {
        let Some(pair) = by_key.get(&(f.category.as_str(), f.location_path.as_str())) else {
            continue;
        };
        // A pair the query itself derived FROM this finding's own review
        // must contain this review's id — a defensive skip (never
        // reachable in practice) rather than an assumed invariant.
        if !pair.review_ids.contains(&review_id) {
            continue;
        }
        let prior: Vec<i64> = pair
            .review_ids
            .iter()
            .copied()
            .filter(|&rid| rid != review_id)
            .collect();
        out.push((f.slug.clone(), pair.review_count, prior));
    }
    out
}

/// The store+CPU compute behind `GET /api/reviews/{id}/findings/
/// recurrence` — pure w.r.t. its caller's async machinery (`&Store` +
/// plain args, no `SharedState`/HTTP), so it runs entirely inside ONE
/// `run_blocking` trip AND is directly measurable/testable without a
/// daemon (same "pure composition, thin route" shape `review_inbox::
/// compose_rows` already establishes).
///
/// PF-K1 — replaces the old per-prior-id `get_review` + `get_review_pr_
/// binding` fan-out (each its own round trip PER prior review) with a
/// batched pass: every distinct prior review id across every recurring
/// finding is collected up front, then [`store::Store::get_reviews_by_ids`]
/// / [`store::Store::get_review_pr_bindings`] (shared with the review-list
/// route's batch, `crate::reviews::compose_review_list_rows`) each run
/// ONCE for the whole set. A prior id absent from `reviews_by_id` (vanished
/// between the recurrence query and this lookup) is dropped from that
/// finding's `prior` list — same "drop the stale id, keep `seen_in_
/// reviews` as the query saw it" behavior the old per-id loop had.
pub fn compose_finding_recurrence(
    store: &Store,
    review_id: i64,
    repo_name: &str,
) -> Result<Vec<FindingRecurrenceRow>, ApiError> {
    let findings = store.list_review_findings(review_id, None, false)?;
    let pairs =
        store.recurrence_pairs(Some(repo_name), None, None, store::RECURRENCE_MIN_REVIEWS)?;
    let recurring = recurring_findings(&findings, review_id, &pairs);

    let mut all_prior_ids: Vec<i64> = recurring
        .iter()
        .flat_map(|(_, _, ids)| ids.iter().copied())
        .collect();
    all_prior_ids.sort_unstable();
    all_prior_ids.dedup();
    let reviews_by_id = store.get_reviews_by_ids(&all_prior_ids)?;
    let bindings_by_id = store.get_review_pr_bindings(&all_prior_ids)?;

    let mut out = Vec::with_capacity(recurring.len());
    for (slug, seen_in_reviews, prior_ids) in recurring {
        let mut prior = Vec::with_capacity(prior_ids.len());
        for rid in prior_ids {
            // A prior review vanished between the recurrence query and
            // this lookup (deleted mid-request) — drop it from the
            // listing rather than error the whole response over one stale
            // id; `seen_in_reviews` still reports the count the query saw.
            let Some(r) = reviews_by_id.get(&rid) else {
                continue;
            };
            let pr_number = bindings_by_id.get(&rid).and_then(|b| b.pr_number);
            prior.push(RecurrencePriorReview {
                review_id: rid,
                pr_number,
                title: r.title.clone(),
                created_at: r.created_at,
            });
        }
        out.push(FindingRecurrenceRow {
            slug,
            seen_in_reviews,
            prior,
        });
    }
    Ok(out)
}

/// `GET /api/reviews/{id}/findings/recurrence` (design-ui.md §12.4). Bearer,
/// same ordinary review-read gate as `/findings`. For the review's non-
/// superseded findings, reports which ones share a `(category,
/// location_path)` pair with `>= store::RECURRENCE_MIN_REVIEWS` distinct
/// reviews (the SAME shared query `GET /api/reviews/analytics`'s own
/// `recurrence` field already computes — design-addendum-2 §C) and names
/// the prior reviews. Computed per request, never persisted — same
/// surfaced-never-scored posture as every other derived recall lane (#10's
/// CT amendment in the project CLAUDE.md).
pub async fn findings_recurrence_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let (review, _repo, _repo_id) = require_review(&state, id).await?;
    // 2026-08-31 incident (store.rs module doc): the whole recurrence
    // compose (findings + pairs + the batched prior-review fan-out) is
    // pure store + CPU work — one blocking-pool trip.
    let repo_name = review.repo.clone();
    let out = state
        .store
        .run_blocking(move |store| compose_finding_recurrence(store, id, &repo_name))
        .await?;

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": RECURRENCE_SCHEMA,
            "review_id": id,
            "repo": review.repo,
            "findings": out,
        })),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finding_slug_vocab() {
        assert!(is_valid_finding_slug("f-dedup-race"));
        assert!(is_valid_finding_slug("f-a"));
        assert!(is_valid_finding_slug("f-issue-123"));
        assert!(!is_valid_finding_slug("f-"));
        assert!(!is_valid_finding_slug("dedup-race"));
        assert!(!is_valid_finding_slug("f-Dedup"));
        assert!(!is_valid_finding_slug("f-dedup_race"));
        assert!(!is_valid_finding_slug(""));
        assert!(!is_valid_finding_slug("F-dedup"));
        assert_eq!(FINDING_SLUG_PATTERN, "f-[a-z0-9-]+");
    }

    #[test]
    fn kebab_case_derives_a_valid_slug_body() {
        assert_eq!(kebab_case("Duplicate Order Rows"), "duplicate-order-rows");
        assert_eq!(
            kebab_case("N+1 query in Order#total"),
            "n-1-query-in-order-total"
        );
        assert_eq!(kebab_case("  leading/trailing  "), "leading-trailing");
        // No alphanumerics at all -> the documented fallback.
        assert_eq!(kebab_case("!!!"), "finding");
        assert!(is_valid_finding_slug(&format!(
            "f-{}",
            kebab_case("Anything Goes Here!")
        )));
    }

    fn loc(kind: &str, lines: Option<Vec<i64>>) -> FindingLocationBody {
        FindingLocationBody {
            path: "app/models/order.rb".to_string(),
            kind: kind.to_string(),
            lines,
            removed: false,
        }
    }

    #[test]
    fn location_shape_single_requires_exactly_one_line() {
        assert!(validate_location_shape(&loc("single", Some(vec![5]))).is_ok());
        assert_eq!(
            validate_location_shape(&loc("single", Some(vec![5, 6]))),
            Err(LocationShapeError::InvalidLinesShape)
        );
        assert_eq!(
            validate_location_shape(&loc("single", None)),
            Err(LocationShapeError::MissingLines)
        );
    }

    #[test]
    fn location_shape_range_requires_exactly_two_lines() {
        assert!(validate_location_shape(&loc("range", Some(vec![5, 9]))).is_ok());
        assert_eq!(
            validate_location_shape(&loc("range", Some(vec![5]))),
            Err(LocationShapeError::InvalidLinesShape)
        );
    }

    #[test]
    fn location_shape_multi_requires_at_least_one_line() {
        assert!(validate_location_shape(&loc("multi", Some(vec![5]))).is_ok());
        assert!(validate_location_shape(&loc("multi", Some(vec![5, 9, 12]))).is_ok());
        assert_eq!(
            validate_location_shape(&loc("multi", Some(vec![]))),
            Err(LocationShapeError::InvalidLinesShape)
        );
    }

    #[test]
    fn location_shape_whole_file_needs_no_lines() {
        assert!(validate_location_shape(&loc("whole_file", None)).is_ok());
    }

    #[test]
    fn location_shape_rejects_a_zero_or_negative_line() {
        assert_eq!(
            validate_location_shape(&loc("single", Some(vec![0]))),
            Err(LocationShapeError::InvalidLinesShape)
        );
        assert_eq!(
            validate_location_shape(&loc("range", Some(vec![-1, 5]))),
            Err(LocationShapeError::InvalidLinesShape)
        );
    }

    #[test]
    fn location_shape_rejects_an_unknown_kind() {
        assert_eq!(
            validate_location_shape(&loc("bogus", None)),
            Err(LocationShapeError::InvalidKind)
        );
    }

    #[test]
    fn location_shape_rejects_an_empty_path() {
        let mut l = loc("whole_file", None);
        l.path = "   ".to_string();
        assert_eq!(
            validate_location_shape(&l),
            Err(LocationShapeError::EmptyPath)
        );
    }

    // --- recurring_findings (PRR-F, design-ui.md §12.4) --------------------

    fn finding_row(review_id: i64, slug: &str, category: &str, path: &str) -> ReviewFindingRow {
        ReviewFindingRow {
            id: 0,
            review_id,
            annotation_id: format!("ann-{slug}"),
            slug: slug.to_string(),
            severity: "concern".into(),
            category: category.to_string(),
            location_kind: "whole_file".into(),
            location_path: path.to_string(),
            location_lines: None,
            location_removed: false,
            title: format!("title-{slug}"),
            rationale: "r".into(),
            recommendation: None,
            evidence_lang: None,
            evidence_source: None,
            origin: store::FINDING_ORIGIN_IMPORT.to_string(),
            author: Some("claude".into()),
            disposition: None,
            disposition_note: None,
            disposition_by: None,
            disposition_at: None,
            content_updated_at: None,
            published_state: "unpublished".into(),
            published_at: None,
            published_url: None,
            superseded: false,
            superseded_at: None,
            superseded_reason: None,
            import_batch_id: "batch_1".into(),
            created_at: 100,
            updated_at: 100,
            act: "issue".into(),
            blocking: false,
            cites_json: None,
            fingerprint: None,
            superseded_by: None,
        }
    }

    fn pair(category: &str, path: &str, review_ids: Vec<i64>) -> store::RecurrenceRow {
        store::RecurrenceRow {
            category: category.to_string(),
            location_path: path.to_string(),
            review_count: review_ids.len() as i64,
            finding_count: review_ids.len() as i64,
            review_ids,
        }
    }

    #[test]
    fn recurring_findings_skips_a_finding_with_no_matching_pair() {
        let findings = vec![finding_row(1, "f-a", "Security", "a.rb")];
        let pairs = vec![pair("Style", "b.rb", vec![1, 2])];
        assert!(recurring_findings(&findings, 1, &pairs).is_empty());
    }

    #[test]
    fn recurring_findings_reports_prior_reviews_excluding_self() {
        let findings = vec![finding_row(1, "f-a", "Security", "a.rb")];
        let pairs = vec![pair("Security", "a.rb", vec![1, 2, 3])];
        let out = recurring_findings(&findings, 1, &pairs);
        assert_eq!(out.len(), 1);
        let (slug, seen_in_reviews, prior) = &out[0];
        assert_eq!(slug, "f-a");
        assert_eq!(*seen_in_reviews, 3);
        assert_eq!(*prior, vec![2, 3], "self (review 1) excluded from prior");
    }

    #[test]
    fn recurring_findings_ignores_a_pair_that_does_not_include_this_review() {
        // Defensive branch: a pair whose review_ids never included the
        // review being queried should never surface (can't happen via the
        // real store query, but the pure fn must not assume it).
        let findings = vec![finding_row(1, "f-a", "Security", "a.rb")];
        let pairs = vec![pair("Security", "a.rb", vec![2, 3])];
        assert!(recurring_findings(&findings, 1, &pairs).is_empty());
    }

    // --- the findings-v2 wire (V73-K2b) -----------------------------------

    fn resolution_for_test() -> ResolvedForPs {
        ResolvedForPs {
            line: Some(12),
            line_end: None,
            orphaned: false,
            resolved_against: ResolvedAgainst {
                ps: 1,
                sha: "aaaa".into(),
            },
            original: None,
            confidence: Some("exact"),
        }
    }

    #[test]
    fn the_findings_wire_carries_the_five_v2_fields() {
        // V73-K1 landed `act`/`blocking`/`cites`/`fingerprint`/
        // `superseded_by` as COLUMNS and put them on the document read's own
        // `FindingBrief`, but the wire every finding CARD reads did not carry
        // them — the two axes D9 added were storable and unreadable. This
        // pins that they are on the shared builder, so a manual create, a
        // disposition set and `GET /findings` all report them alike.
        let mut f = finding_row(1, "f-a", "correctness", "a.rb");
        f.act = "question".into();
        f.blocking = true;
        f.cites_json = Some(r#"["code:a.rb:1","sym:Order#total"]"#.to_string());
        f.fingerprint = Some("deadbeefcafe0001".into());
        f.superseded_by = Some("f-b".into());
        let v = finding_json(&f, &resolution_for_test(), 0, 0);
        assert_eq!(v["act"], "question");
        assert_eq!(v["blocking"], true);
        assert_eq!(v["cites"][0], "code:a.rb:1");
        assert_eq!(v["cites"][1], "sym:Order#total");
        assert_eq!(v["fingerprint"], "deadbeefcafe0001");
        assert_eq!(v["superseded_by"], "f-b");
    }

    #[test]
    fn a_pre_v0034_row_reads_back_as_what_it_always_meant() {
        // Every default is the honest reading of a row nothing computed a v2
        // value for: an `issue`, not blocking, citing nothing, with no
        // fingerprint and no successor.
        let f = finding_row(1, "f-a", "correctness", "a.rb");
        let v = finding_json(&f, &resolution_for_test(), 0, 0);
        assert_eq!(v["act"], "issue");
        assert_eq!(v["blocking"], false);
        assert!(v["cites"].is_null());
        assert!(v["fingerprint"].is_null());
        assert!(v["superseded_by"].is_null());
    }

    #[test]
    fn an_unreadable_cites_blob_is_absent_not_empty() {
        // "cites nothing" and "we could not read what it cites" are
        // different facts; degrading the second into the first would let a
        // card state an absence it never verified.
        let mut f = finding_row(1, "f-a", "correctness", "a.rb");
        f.cites_json = Some("{ not json".to_string());
        let v = finding_json(&f, &resolution_for_test(), 0, 0);
        assert!(v["cites"].is_null());
    }

    #[test]
    fn recurring_findings_handles_a_mixed_set() {
        let findings = vec![
            finding_row(1, "f-a", "Security", "a.rb"),
            finding_row(1, "f-b", "Style", "b.rb"),
        ];
        let pairs = vec![pair("Security", "a.rb", vec![1, 5])];
        let out = recurring_findings(&findings, 1, &pairs);
        assert_eq!(out.len(), 1, "only f-a recurs; f-b has no matching pair");
        assert_eq!(out[0].0, "f-a");
    }
}
