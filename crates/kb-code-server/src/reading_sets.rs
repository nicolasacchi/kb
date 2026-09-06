//! Phase E3 ("kb-code v2 — The Operable Reader") — reading sets: named,
//! server-persisted, ordered collections of file/span references. kb's own
//! reading-list design (kb's CLAUDE.md invariant #25 — ordered spans,
//! notes, import-as-one-transaction) is the spiritual ancestor, ported to
//! code: a shareable understanding-path ("the ingest path," "everything the
//! auth rework touched"), agent-feedable via `GET /api/pack?set=`
//! (`agentview::pack`), and auto-materializable from a session's touched
//! files (`POST /api/sets/from-session`, LOOPBACK-ONLY — the session-diff
//! family's own gate, since it reuses `sessiondiff::session_diff`'s
//! narrative assembly, which carries raw transcript prompt text) or from a
//! kb document's resolved code references (`POST /api/sets/from-doc`,
//! DCB-W3.C — see "From-doc materialization" below).
//!
//! # Wire routes
//!
//! `GET /api/sets?repo=` ([`list_sets`]) · `POST /api/sets` ([`create_set`],
//! `409` on a `(repo, name)` collision) · `GET /api/sets/{id}`
//! ([`get_set`]) · `PATCH /api/sets/{id}` ([`patch_set`] — a `spans` field
//! is a FULL replacement, one transaction) · `POST /api/sets/{id}/spans`
//! ([`append_span`], one span appended after the current last ordinal) ·
//! `DELETE /api/sets/{id}` ([`delete_set`], cascades to spans) · `POST
//! /api/sets/from-session` ([`from_session_route`], LOOPBACK-ONLY) · `POST
//! /api/sets/from-doc` ([`from_doc_route`], LOOPBACK-ONLY). Every mutation
//! emits `set.changed {repo}` on `state.bus` — same pattern as
//! `annotation.changed` (`routes::create_annotation`'s doc), a bare
//! `{repo}` payload since a set has no single `path` of its own to report.
//!
//! # Workspaces (V70-A10, kb-code v7 "The Continuum")
//!
//! A "workspace" is a `reading_sets` row with `kind = "workspace"` plus a
//! captured desk snapshot (`desk_json`), an optional branch/ref label
//! (`ref`), and a longer free-text description (`description_md`) —
//! `V0028__workspaces.sql`. No new table: this unit deliberately widens the
//! existing `reading_sets`/`reading_set_spans` substrate rather than
//! forking a parallel one, per the design of record (docs/research/
//! kb-code-v7-continuum-2026-09.html §The spine P6, kbc-seq/1) — the full
//! physical unification (one table for reading sets / tours / trails /
//! boards) is a LATER (v7.1) unit; this is deliberately still `reading_sets`
//! underneath. `GET /api/sets?kind=workspace` lists workspaces instead of
//! plain sets; `&group=ref` further groups them
//! (`{groups: [{ref, workspaces: [...]}]}`). `POST`/`PATCH /api/sets`
//! accept the same four fields (`kind`/`desk_json`/`ref`/`description_md`,
//! see [`CreateSetBody`]/[`PatchSetBody`]) — the server never interprets
//! `desk_json`, storing it byte-for-byte verbatim.
//!
//! A workspace's NOTES (general path-less notes AND code-anchored comments
//! filed under it) are ordinary `annotations` rows carrying `set_id`
//! (`V0028__workspaces.sql`'s ALTER on `annotations`, no SQL FK — same
//! `review_id`/`parent_id` precedent) — see `annotations::ANCHOR_KIND_SET`
//! for the general-note shape and `routes::create_annotation`'s doc for the
//! full `set_id` contract. `GET /api/annotations?set_id=` lists them;
//! `DELETE /api/sets/{id}` cascades to every annotation scoped to it
//! (`store::Store::delete_reading_set`'s doc).
//!
//! # From-session materialization
//!
//! [`materialize_spans`] reuses `sessiondiff::session_diff` (scoped to ONE
//! repo via its own `repo_filter` — the same repo `from_session_route`
//! resolved from `?repo=`/the request body) rather than re-deriving the
//! touched-file list. It walks the diff's `segments` in NARRATIVE order,
//! collecting (repo-relative path, optional note) pairs, first-touch wins:
//!
//! - a `Commits` segment's `files` are ALREADY repo-relative (`git
//!   --numstat`'s own output) — `note` is that commit's own `subject`
//!   WHEN exactly one commit in the WHOLE session touched the path (an
//!   "unambiguous" attribution, see [`unambiguous_commit_notes`]); `None`
//!   when two or more different commits touched the same path (nothing
//!   honest to attribute a single subject to).
//! - an `Uncommitted` segment's `files` are ABSOLUTE paths (`sessiondiff`'s
//!   own convention) — converted to repo-relative here via
//!   [`repo_relative`], and DROPPED when the path isn't under the target
//!   repo at all. That drop matters: `sessiondiff::session_diff`'s own
//!   `Uncommitted.files` can carry a path belonging to a DIFFERENT repo
//!   when that edit was never committed anywhere (see that module's
//!   `owning_repo_rel` — a path outside the `repo_filter` still isn't
//!   "committed" by the filter's own definition, so it still shows up as
//!   uncommitted evidence); a from-session set is scoped to exactly one
//!   repo, so such a path is simply not this daemon's to reference here.
//!
//! Every materialized span is WHOLE-FILE (`line_start`/`line_end` both
//! `None`): a session's touched-file list has no natural line range of its
//! own (unlike a hand-placed annotation), so `pack`'s existing whole-file
//! content is the right granularity — same "spans, not excerpts" posture
//! kb's own reading-list precedent takes for a coarse-grained source.
//!
//! # From-doc materialization (DCB-W3.C)
//!
//! [`materialize_lens_spans`] is the doc-lens mirror of `materialize_spans`
//! above: [`from_doc_route`] calls `doclens::resolve::resolve_lens`
//! IN-PROCESS (never a second HTTP round trip to self) against a caller-named
//! `(kb, doc, repo)`, then keeps only `path_state == "present"` refs — the
//! same filter `doclens::sync`'s `doc_refs` reverse index applies, for the
//! same reason: an ambiguous/absent/external ref has no single honest path
//! to turn into a span. Unlike a from-session set, every from-doc span
//! carries a LINE range when the doc-lens resolved one, falling back to the
//! doc's own unverified hint — a from-doc set is a snapshot of ONE
//! resolution against ONE checkout at ONE instant, not an evidence trail
//! accumulated across many commits the way a session is.
//!
//! (DCB-W3.C.R Blocker 1) [`normalized_span_lines`] builds that range
//! WITHOUT ever mixing sources: a resolved `line_start` only ever pairs with
//! a resolved `line_end` (never a stale doc `line_hint_end`) and vice versa
//! — the old `r.resolved_line.or(r.line_hint)` /
//! `r.resolved_line_end.or(r.line_hint_end)` construction could pick a
//! start from one source and an end from the other, producing a half-range
//! (`Some(2), None`) or an inverted range (a drifted resolved start past a
//! stale hint end). The chosen pair is then clamped (an end `< start` drops
//! to `end = start`) and routed through [`build_span`] — the SAME
//! both-or-neither/`>= 1`/non-inverted validation `validate_span` applies on
//! every `POST`/`PATCH` — so this writer is STRUCTURALLY unable to persist a
//! span shape its own `PATCH` sibling would reject.
//!
//! (DCB-W3.C.R Blocker 2) The span-level `ref` is a REAL git revspec, never
//! a fabricated label: [`span_git_ref`] pins the full 40-hex `head_sha` when
//! the resolving checkout was clean, and `None` — never a
//! `"{repo}@{sha}+dirty"` string `GET /api/file?ref=`'s blob lookup can't
//! resolve (a 404 on every from-doc span link/Tour step) — when the tree was
//! dirty or the repo has no commits yet. The lost provenance is folded into
//! a [`dirty_honesty_note`] suffix on the span's own `note` instead, so a
//! dirty/uncommitted resolve stays legible without ever being fabricated
//! into an unresolvable ref.
//!
//! The created set's four `source_*` columns
//! (`V0022__reading_sets_doc_provenance.sql`) let `SetDetail.tsx` detect
//! "this doc changed since materialization" and offer a re-materialize
//! action — the columns are provenance for THAT UX, never consulted by any
//! read path in this file itself. (DCB-W3.C.R Major 1) The DEFAULT set name
//! also embeds the new set's own id (first 6 hex chars after `set_`)
//! alongside the minute-precision timestamp — collision-proof BY
//! CONSTRUCTION, not merely by the timestamp's granularity.

use crate::config::RepoEntry;
use crate::doclens;
use crate::routes::{find_repo, find_repo_by_id, safe_rel_path, ApiError};
use crate::sessiondiff::{self, Segment};
use crate::state::SharedState;
use crate::store::{self, NewReadingSetSpan, Store, StoreBlocking};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub const SCHEMA: &str = "sets/1";

/// `set_` + 12 hex chars — same shape as `annotations::new_annotation_id`
/// (that helper's `short_random_hex` is `pub(crate)`, reused here rather
/// than a second copy of a two-line generator).
pub fn new_set_id() -> String {
    format!("set_{}", crate::annotations::short_random_hex())
}

// --- wire shapes ------------------------------------------------------------

/// V70-A10 ("Workspaces v0") — a "set" (Phase E3's original, and still the
/// pre-existing DEFAULT — `V0028__workspaces.sql`'s `DEFAULT 'set'`) or a
/// "workspace" (a set plus a captured desk snapshot, an optional ref
/// label, and a longer description). Route-boundary validated
/// (`is_valid_set_kind`), never a SQL CHECK — same house convention
/// `annotations::is_valid_anchor_kind` documents.
pub const SET_KIND_SET: &str = "set";
pub const SET_KIND_WORKSPACE: &str = "workspace";
pub const SET_KINDS: [&str; 2] = [SET_KIND_SET, SET_KIND_WORKSPACE];

pub fn is_valid_set_kind(s: &str) -> bool {
    SET_KINDS.contains(&s)
}

/// V70-A10 — size caps for the two free-form workspace fields (both
/// route-enforced, never a SQL constraint): a generous but bounded
/// ceiling, same spirit as kb's own artifact-scrub caps.
pub const MAX_DESK_JSON_BYTES: usize = 64 * 1024;
pub const MAX_DESCRIPTION_MD_BYTES: usize = 64 * 1024;

/// `desk_json` must be valid JSON (any shape — the server never
/// interprets it further beyond this parseability check, see the module
/// doc's "the server never interprets desk geometry") and no larger than
/// [`MAX_DESK_JSON_BYTES`]. Shared by [`create_set`] and [`patch_set`].
fn validate_desk_json(s: &str) -> Result<(), ApiError> {
    if s.len() > MAX_DESK_JSON_BYTES {
        return Err(ApiError::bad_request(format!(
            "desk_json exceeds {MAX_DESK_JSON_BYTES} bytes"
        )));
    }
    serde_json::from_str::<serde_json::Value>(s)
        .map_err(|e| ApiError::bad_request(format!("desk_json is not valid JSON: {e}")))?;
    Ok(())
}

/// `description_md` — size only (free Markdown text, never parsed
/// server-side). Shared by [`create_set`] and [`patch_set`].
fn validate_description_md(s: &str) -> Result<(), ApiError> {
    if s.len() > MAX_DESCRIPTION_MD_BYTES {
        return Err(ApiError::bad_request(format!(
            "description_md exceeds {MAX_DESCRIPTION_MD_BYTES} bytes"
        )));
    }
    Ok(())
}

/// Normalizes + shape-validates an optional `ref` label: an empty/
/// whitespace-only string is treated as "no ref" (a save dialog's ref
/// field left blank, e.g. a detached-HEAD checkout), never an error;
/// anything else must pass `reviews::reject_user_ref` (SHAPE only —
/// injection-safety, never checked for existence — same posture
/// `reading_set_spans.ref` already takes). Shared by [`create_set`] and
/// [`patch_set`].
fn validate_ref_label(r: Option<&str>) -> Result<Option<String>, ApiError> {
    match r.map(str::trim) {
        None | Some("") => Ok(None),
        Some(s) => {
            crate::reviews::reject_user_ref(s).map_err(ApiError::from)?;
            Ok(Some(s.to_string()))
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ListSetsParams {
    pub repo: String,
    /// V70-A10 — filter by [`is_valid_set_kind`]. Omitted defaults to
    /// `"set"` (the pre-existing default, and the ONLY kind that existed
    /// before this unit) — so a caller that has never heard of `kind=`
    /// sees EXACTLY the rows it always did, byte-identical.
    #[serde(default)]
    pub kind: Option<String>,
    /// V70-A10 — `"ref"` groups the response by `ref` label instead of a
    /// flat list: `{groups: [{ref, workspaces: [...]}]}`. Any other value
    /// (or absent) returns the flat `{sets: [...]}` shape.
    #[serde(default)]
    pub group: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetSummary {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub span_count: i64,
    /// V70-A10 — `COUNT(*)` of `annotations.set_id = this set's id`
    /// (general path-less notes AND code-anchored comments alike).
    pub note_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
    /// V70-A10 — `"set"` | `"workspace"`.
    pub kind: String,
    /// V70-A10 — the wire key is literally `ref` (same `#[serde(rename)]`
    /// convention `SpanOut::git_ref` already uses); omitted when absent,
    /// unlike `SetView`'s `source_*` provenance quartet — there's no
    /// "unknown vs. never set" distinction to protect here.
    #[serde(skip_serializing_if = "Option::is_none", rename = "ref")]
    pub ref_label: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetsListOut {
    pub schema: &'static str,
    pub sets: Vec<SetSummary>,
}

/// V70-A10 — one `ref`-grouped bucket of workspaces (`GET /api/sets?kind=
/// workspace&group=ref`'s data source).
#[derive(Debug, Clone, Serialize)]
pub struct SetGroupOut {
    #[serde(rename = "ref")]
    pub ref_label: Option<String>,
    pub workspaces: Vec<SetSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetGroupsListOut {
    pub schema: &'static str,
    pub groups: Vec<SetGroupOut>,
}

/// Group `summaries` by their own `ref_label`, alphabetical by ref — a
/// `None`/no-ref group sorts LAST (the same "ungrouped trails every real
/// group" convention [`materialize_lens_spans`]'s doc / `docLensUrl.ts`'s
/// client-side `stepGroup` already use), each group's own workspaces kept
/// in the caller's incoming order (already alphabetical by name —
/// `list_reading_sets`'s own `ORDER BY s.name ASC`).
fn group_by_ref(summaries: Vec<SetSummary>) -> Vec<SetGroupOut> {
    let mut groups: Vec<(Option<String>, Vec<SetSummary>)> = Vec::new();
    for s in summaries {
        let key = s.ref_label.clone();
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, bucket)) => bucket.push(s),
            None => groups.push((key, vec![s])),
        }
    }
    groups.sort_by(|(a, _), (b, _)| match (a, b) {
        (Some(a), Some(b)) => a.cmp(b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    groups
        .into_iter()
        .map(|(ref_label, workspaces)| SetGroupOut {
            ref_label,
            workspaces,
        })
        .collect()
}

/// `GET /api/sets?repo=[&kind=][&group=ref]` — every reading set (or, with
/// `kind=workspace`, every workspace) in `repo`, alphabetical by name
/// (`store::Store::list_reading_sets`'s own order) — see
/// [`ListSetsParams`]'s doc for the two params. `400` on an invalid `kind`.
pub async fn list_sets(
    State(state): State<SharedState>,
    Query(params): Query<ListSetsParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (_repo, repo_id) = find_repo(&state, &params.repo)?;
    if let Some(k) = &params.kind {
        if !is_valid_set_kind(k) {
            return Err(ApiError::bad_request(format!("invalid kind: {k:?}")));
        }
    }
    let kind = params.kind.clone();
    // 2026-08-31 incident (store.rs module doc): single store call, still
    // wrapped so it can never park this async worker on the mutex wait.
    let rows = state
        .store
        .run_blocking(move |store| store.list_reading_sets(repo_id, kind.as_deref()))
        .await?;
    let sets: Vec<SetSummary> = rows
        .into_iter()
        .map(|(row, span_count, note_count)| SetSummary {
            id: row.id,
            name: row.name,
            description: row.description,
            span_count,
            note_count,
            created_at: row.created_at,
            updated_at: row.updated_at,
            kind: row.kind,
            ref_label: row.ref_label,
        })
        .collect();

    let body: serde_json::Value = if params.group.as_deref() == Some("ref") {
        serde_json::to_value(SetGroupsListOut {
            schema: SCHEMA,
            groups: group_by_ref(sets),
        })
        .expect("SetGroupsListOut always serializes")
    } else {
        serde_json::to_value(SetsListOut {
            schema: SCHEMA,
            sets,
        })
        .expect("SetsListOut always serializes")
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(body)))
}

/// One span on the wire — the caller-supplied shape, no `ordinal` (the
/// server assigns it). `ref` is renamed on the wire (`ref` is a Rust
/// keyword) but travels unmodified — same rename convention as
/// `routes::CheckoutBody::target`'s own `#[serde(rename = "ref")]`.
#[derive(Debug, Clone, Deserialize)]
pub struct SpanInput {
    pub path: String,
    #[serde(default)]
    pub line_start: Option<u32>,
    #[serde(default)]
    pub line_end: Option<u32>,
    #[serde(default, rename = "ref")]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpanOut {
    pub ordinal: i64,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_start: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_end: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "ref")]
    pub git_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl From<store::ReadingSetSpanRow> for SpanOut {
    fn from(r: store::ReadingSetSpanRow) -> Self {
        SpanOut {
            ordinal: r.ordinal,
            path: r.path,
            line_start: r.line_start,
            line_end: r.line_end,
            git_ref: r.git_ref,
            note: r.note,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SetView {
    pub schema: &'static str,
    pub id: String,
    pub repo: String,
    pub name: String,
    pub description: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub spans: Vec<SpanOut>,
    /// DCB-W3.C — doc-materialization provenance
    /// (`V0022__reading_sets_doc_provenance.sql`). R25 — deliberately NOT
    /// `skip_serializing_if`: every OTHER optional `SetView` field omits
    /// itself when absent, but these four wire as an EXPLICIT `null` on
    /// purpose. `SetDetail.tsx`'s "doc changed since" banner (§3.3) reads
    /// `null` as "unknown, never changed" (R14) off an ALWAYS-present key —
    /// an omitted-vs-null ambiguity is exactly the guess that check must
    /// never make. A recorded, deliberate divergence from the sibling
    /// fields above; do not "fix" it into `skip_serializing_if`.
    pub source_kb: Option<String>,
    pub source_doc_id: Option<String>,
    pub source_doc_path: Option<String>,
    pub source_doc_hash: Option<String>,
    /// V70-A10 — `"set"` | `"workspace"`.
    pub kind: String,
    /// V70-A10 — the workspace's opaque `DeskState` snapshot, verbatim
    /// (see the module doc). `None` on a plain `'set'` row and on a
    /// `'workspace'` row saved before a client sent one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desk_json: Option<String>,
    /// V70-A10 — the wire key is literally `ref` (same `#[serde(rename)]`
    /// convention `SpanOut::git_ref` already uses).
    #[serde(skip_serializing_if = "Option::is_none", rename = "ref")]
    pub ref_label: Option<String>,
    /// V70-A10 — free-text Markdown, separate from the short `description`
    /// above.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description_md: Option<String>,
}

/// The shared span-shape check: `path` must not escape the repo root
/// ([`safe_rel_path`]), and a line RANGE (`line_start`/`line_end`) must be
/// BOTH present or BOTH absent (no "start only" half-range), `>= 1`
/// (1-based, same convention as `annotations::create_annotation`'s own
/// `line` check), and `line_start <= line_end`. BOTH [`validate_span`] (the
/// `POST`/`PATCH` wire path) and [`materialize_lens_spans`] (the from-doc
/// writer, DCB-W3.C.R Blocker 1) route through this ONE function, so the
/// from-doc writer cannot structurally emit a span shape its own `PATCH`
/// sibling would reject.
fn build_span(
    path: &str,
    line_start: Option<u32>,
    line_end: Option<u32>,
    git_ref: Option<String>,
    note: Option<String>,
) -> Result<NewReadingSetSpan, ApiError> {
    safe_rel_path(path)?;
    match (line_start, line_end) {
        (Some(s), Some(e)) => {
            if s < 1 || e < 1 {
                return Err(ApiError::bad_request(
                    "line_start/line_end must be >= 1 (1-based)",
                ));
            }
            if s > e {
                return Err(ApiError::bad_request(format!(
                    "line_start ({s}) must be <= line_end ({e})"
                )));
            }
        }
        (None, None) => {}
        _ => {
            return Err(ApiError::bad_request(
                "line_start and line_end must both be set (a range) or both omitted \
                 (a whole-file span)",
            ))
        }
    }
    Ok(NewReadingSetSpan {
        path: path.to_string(),
        line_start: line_start.map(i64::from),
        line_end: line_end.map(i64::from),
        git_ref,
        note,
    })
}

/// Route-boundary validation for one wire [`SpanInput`] — thin wrapper over
/// [`build_span`].
fn validate_span(span: &SpanInput) -> Result<NewReadingSetSpan, ApiError> {
    build_span(
        &span.path,
        span.line_start,
        span.line_end,
        span.git_ref.clone(),
        span.note.clone(),
    )
}

fn validate_spans(spans: &[SpanInput]) -> Result<Vec<NewReadingSetSpan>, ApiError> {
    spans.iter().map(validate_span).collect()
}

/// Build the full [`SetView`] for an already-fetched `row` — every mutation
/// route re-reads and returns this after its write (same "return the whole
/// resource" convention `routes::create_annotation`/`patch_annotation` use).
/// 2026-08-31 incident (store.rs module doc): takes `&Store` (not
/// `&SharedState`) precisely so every call site can run this inside its
/// own `run_blocking` closure alongside its other store calls, rather than
/// this being a second async-context store touch of its own.
fn set_view(
    store: &Store,
    row: store::ReadingSetRow,
    repo_name: &str,
) -> Result<SetView, ApiError> {
    let spans = store.reading_set_spans(&row.id)?;
    Ok(SetView {
        schema: SCHEMA,
        id: row.id,
        repo: repo_name.to_string(),
        name: row.name,
        description: row.description,
        created_at: row.created_at,
        updated_at: row.updated_at,
        spans: spans.into_iter().map(SpanOut::from).collect(),
        source_kb: row.source_kb,
        source_doc_id: row.source_doc_id,
        source_doc_path: row.source_doc_path,
        source_doc_hash: row.source_doc_hash,
        kind: row.kind,
        desk_json: row.desk_json,
        ref_label: row.ref_label,
        description_md: row.description_md,
    })
}

fn vanished_after(what: &str, id: &str) -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("set {id:?} vanished immediately after {what}"),
    )
}

// --- POST /api/sets ----------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateSetBody {
    pub repo: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub spans: Vec<SpanInput>,
    /// V70-A10 — `"set"` (default) | `"workspace"`.
    #[serde(default)]
    pub kind: Option<String>,
    /// V70-A10 — opaque `DeskState` JSON, ≤ [`MAX_DESK_JSON_BYTES`],
    /// stored verbatim.
    #[serde(default)]
    pub desk_json: Option<String>,
    /// V70-A10 — an optional branch/ref label; the wire key is literally
    /// `ref` (same `#[serde(rename)]` convention `SpanInput::git_ref`
    /// already uses). Empty/whitespace-only is treated as "no ref"
    /// ([`validate_ref_label`]'s doc).
    #[serde(default, rename = "ref")]
    pub ref_label: Option<String>,
    /// V70-A10 — free-text Markdown, ≤ [`MAX_DESCRIPTION_MD_BYTES`].
    #[serde(default)]
    pub description_md: Option<String>,
}

/// `POST /api/sets` — `400` on an invalid span (escaping path, malformed
/// line range), an invalid `kind`, an oversized/malformed `desk_json`, an
/// oversized `description_md`, or a shape-invalid `ref`; `409` on a
/// `(repo, name)` collision (`store::StoreError::NameConflict`, mapped by
/// `routes.rs`'s `impl From<StoreError> for ApiError`). Emits `set.changed
/// {repo}` after a successful create.
pub async fn create_set(
    State(state): State<SharedState>,
    Json(body): Json<CreateSetBody>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &body.repo)?;
    let repo_name = repo.name.clone();
    let spans = validate_spans(&body.spans)?;
    let kind = body.kind.as_deref().unwrap_or(SET_KIND_SET).to_string();
    if !is_valid_set_kind(&kind) {
        return Err(ApiError::bad_request(format!("invalid kind: {kind:?}")));
    }
    if let Some(dj) = &body.desk_json {
        validate_desk_json(dj)?;
    }
    if let Some(md) = &body.description_md {
        validate_description_md(md)?;
    }
    let ref_label = validate_ref_label(body.ref_label.as_deref())?;
    let id = new_set_id();
    let now = chrono::Utc::now().timestamp();
    let name = body.name.clone();
    let description = body.description.clone();
    let desk_json = body.desk_json.clone();
    let description_md = body.description_md.clone();
    // 2026-08-31 incident (store.rs module doc): create + read-back + view
    // compose run as one closure; `bus.emit` doesn't touch the store, so
    // it stays outside, after.
    let repo_name_bg = repo_name.clone();
    let view = state
        .store
        .run_blocking(move |store| {
            store.create_reading_set_with_provenance(
                &id,
                repo_id,
                &name,
                description.as_deref(),
                &spans,
                now,
                None,
                None,
                None,
                None,
                &kind,
                desk_json.as_deref(),
                ref_label.as_deref(),
                description_md.as_deref(),
            )?;
            let row = store
                .get_reading_set(&id)?
                .ok_or_else(|| vanished_after("create", &id))?;
            set_view(store, row, &repo_name_bg)
        })
        .await?;
    state
        .bus
        .emit("set.changed", serde_json::json!({ "repo": repo_name }));
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(view),
    ))
}

// --- GET /api/sets/{id} --------------------------------------------------

/// `GET /api/sets/{id}` — full spans, in order. `404` unknown id.
pub async fn get_set(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<String>,
) -> Result<impl IntoResponse, ApiError> {
    // 2026-08-31 incident (store.rs module doc): fetch + repo lookup + view
    // compose run as one closure. `find_repo_by_id` only touches
    // `state.repos` (in-memory), so a cloned `state` handle rides along for
    // that lookup.
    let state_bg = state.clone();
    let view = state
        .store
        .run_blocking(move |store| {
            let row = store
                .get_reading_set(&id)?
                .ok_or_else(|| ApiError::not_found(format!("set {id:?}")))?;
            let repo = find_repo_by_id(&state_bg, row.repo_id)?;
            let repo_name = repo.name.clone();
            set_view(store, row, &repo_name)
        })
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(view)))
}

// --- PATCH /api/sets/{id} ------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct PatchSetBody {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// A FULL replacement of the span list, when present — never a partial
    /// edit (same "PATCH never partially edits an anchor" convention `PATCH
    /// /api/annotations/{id}` already follows).
    #[serde(default)]
    pub spans: Option<Vec<SpanInput>>,
    /// V70-A10 — `is_valid_set_kind`-validated when present. Lets a plain
    /// `'set'` be promoted to a `'workspace'` (or, in principle, the
    /// reverse) without a dedicated route.
    #[serde(default)]
    pub kind: Option<String>,
    /// V70-A10 — replaces the stored `desk_json` wholesale when present
    /// (`validate_desk_json`'d, same as `create_set`).
    #[serde(default)]
    pub desk_json: Option<String>,
    /// V70-A10 — wire key `ref` (see `CreateSetBody::ref_label`'s doc).
    /// `Some("")`/whitespace-only CLEARS it to no-ref
    /// (`validate_ref_label`), same as an omitted field leaving it
    /// untouched only when the KEY itself is absent from the JSON body —
    /// note this is `Option<String>`, not `Option<Option<String>>>`, so a
    /// present-but-empty string is the only way to explicitly clear it
    /// (mirrors every other COALESCE-updated field on this route: there is
    /// no explicit "set to NULL" for `name`/`description` either).
    #[serde(default, rename = "ref")]
    pub ref_label: Option<String>,
    /// V70-A10 — replaces the stored `description_md` wholesale when
    /// present (`validate_description_md`'d, same as `create_set`).
    #[serde(default)]
    pub description_md: Option<String>,
}

/// `PATCH /api/sets/{id}` — every span is re-validated BEFORE any write, so
/// a bad span in `spans` never leaves a half-applied name/description
/// update behind (V70-A10: same for an invalid `kind`/oversized or
/// malformed `desk_json`/oversized `description_md`/shape-invalid `ref`).
/// `404` unknown id; `409` a `name` collision with a DIFFERENT set in the
/// same repo. Emits `set.changed {repo}` after a successful update.
pub async fn patch_set(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<PatchSetBody>,
) -> Result<impl IntoResponse, ApiError> {
    let validated_spans = match &body.spans {
        Some(spans) => Some(validate_spans(spans)?),
        None => None,
    };
    if let Some(k) = &body.kind {
        if !is_valid_set_kind(k) {
            return Err(ApiError::bad_request(format!("invalid kind: {k:?}")));
        }
    }
    if let Some(dj) = &body.desk_json {
        validate_desk_json(dj)?;
    }
    if let Some(md) = &body.description_md {
        validate_description_md(md)?;
    }
    // `ref_label` uses a plain-string sentinel for "clear" (see the field's
    // own doc) — `validate_ref_label` already normalizes empty/whitespace
    // to `None`, but note that means a body with NO `ref` key AT ALL and a
    // body with `"ref": ""` are indistinguishable here (both normalize to
    // `None`, i.e. "leave untouched" via `update_reading_set_meta`'s
    // COALESCE) — same limitation the pre-existing `name`/`description`
    // fields already have.
    let ref_label = validate_ref_label(body.ref_label.as_deref())?;
    let now = chrono::Utc::now().timestamp();
    let state_bg = state.clone();
    // 2026-08-31 incident (store.rs module doc): existence check, the
    // meta/span writes, and the read-back+view compose are up to five
    // sequential store calls with no async work between them — one
    // closure, one hop to the blocking pool. `bus.emit` doesn't touch the
    // store, so it stays outside, after.
    let (view, repo_name) = state
        .store
        .run_blocking(move |store| {
            let existing = store
                .get_reading_set(&id)?
                .ok_or_else(|| ApiError::not_found(format!("set {id:?}")))?;
            let repo = find_repo_by_id(&state_bg, existing.repo_id)?;
            let repo_name = repo.name.clone();

            if body.name.is_some()
                || body.description.is_some()
                || body.kind.is_some()
                || body.desk_json.is_some()
                || ref_label.is_some()
                || body.description_md.is_some()
            {
                store.update_reading_set_meta(
                    &id,
                    body.name.as_deref(),
                    body.description.as_deref(),
                    body.kind.as_deref(),
                    body.desk_json.as_deref(),
                    ref_label.as_deref(),
                    body.description_md.as_deref(),
                    now,
                )?;
            }
            if let Some(spans) = validated_spans {
                store.replace_reading_set_spans(&id, &spans, now)?;
            }

            let row = store
                .get_reading_set(&id)?
                .ok_or_else(|| vanished_after("update", &id))?;
            let view = set_view(store, row, &repo_name)?;
            Ok::<_, ApiError>((view, repo_name))
        })
        .await?;
    state
        .bus
        .emit("set.changed", serde_json::json!({ "repo": repo_name }));
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(view)))
}

// --- POST /api/sets/{id}/spans -------------------------------------------

/// `POST /api/sets/{id}/spans` — append ONE span after the set's current
/// last ordinal. `404` unknown id; `400` an invalid span. Returns the FULL
/// updated [`SetView`] (`201 Created` — a new span row now exists). Emits
/// `set.changed {repo}` after the append.
pub async fn append_span(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<SpanInput>,
) -> Result<impl IntoResponse, ApiError> {
    let span = validate_span(&body)?;
    let now = chrono::Utc::now().timestamp();
    let state_bg = state.clone();
    // 2026-08-31 incident (store.rs module doc): existence check, append,
    // and read-back+view compose run as one closure; `bus.emit` doesn't
    // touch the store, so it stays outside, after.
    let (view, repo_name) = state
        .store
        .run_blocking(move |store| {
            let existing = store
                .get_reading_set(&id)?
                .ok_or_else(|| ApiError::not_found(format!("set {id:?}")))?;
            let repo = find_repo_by_id(&state_bg, existing.repo_id)?;
            let repo_name = repo.name.clone();

            if store.append_reading_set_span(&id, &span, now)?.is_none() {
                return Err(ApiError::not_found(format!("set {id:?}")));
            }

            let row = store
                .get_reading_set(&id)?
                .ok_or_else(|| vanished_after("append", &id))?;
            let view = set_view(store, row, &repo_name)?;
            Ok::<_, ApiError>((view, repo_name))
        })
        .await?;
    state
        .bus
        .emit("set.changed", serde_json::json!({ "repo": repo_name }));
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(view),
    ))
}

// --- DELETE /api/sets/{id} ------------------------------------------------

/// `DELETE /api/sets/{id}` — cascades to spans in one transaction. `404`
/// unknown id. Emits `set.changed {repo}` after the delete.
pub async fn delete_set(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<String>,
) -> Result<impl IntoResponse, ApiError> {
    let state_bg = state.clone();
    // 2026-08-31 incident (store.rs module doc): existence check + delete
    // run as one closure; `bus.emit` doesn't touch the store, so it stays
    // outside, after.
    let repo_name = state
        .store
        .run_blocking(move |store| {
            let row = store
                .get_reading_set(&id)?
                .ok_or_else(|| ApiError::not_found(format!("set {id:?}")))?;
            let repo = find_repo_by_id(&state_bg, row.repo_id)?;
            let repo_name = repo.name.clone();
            store.delete_reading_set(&id)?;
            Ok::<_, ApiError>(repo_name)
        })
        .await?;
    state
        .bus
        .emit("set.changed", serde_json::json!({ "repo": repo_name }));
    Ok(StatusCode::NO_CONTENT)
}

// --- POST /api/sets/from-session -----------------------------------------

#[derive(Debug, Deserialize)]
pub struct FromSessionBody {
    pub repo: String,
    pub session_id: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// Truncation length for the server-computed default name (`"session:
/// <display_name>"`) — shorter than `sessiondiff`'s own
/// `PROMPT_TRUNCATE_CHARS` (200): a SET NAME is a short list label, not a
/// narrative excerpt.
const DEFAULT_NAME_PROMPT_CHARS: usize = 60;

/// `POST /api/sets/from-session` (LOOPBACK-ONLY — mounted on the same
/// sub-router as `session-diff`/`search/transcripts`/`checkout`, since it
/// reuses `sessiondiff::session_diff`'s narrative assembly, which itself
/// carries raw transcript prompt text via `display_name`/segment prompts;
/// none of that text is stored here — only used to derive the DEFAULT
/// name, see [`default_session_set_name`]). `404` unknown session
/// (`sessiondiff::SessionDiffError::UnknownSession`, converted by
/// `routes.rs`'s existing `impl From<SessionDiffError> for ApiError`);
/// `409` the resulting name already exists in the repo. Emits
/// `set.changed {repo}` after a successful materialize.
pub async fn from_session_route(
    State(state): State<SharedState>,
    Json(body): Json<FromSessionBody>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &body.repo)?;
    let diff = sessiondiff::session_diff(
        &body.session_id,
        Some(repo.name.as_str()),
        &state.repos,
        &state.store,
        &state.transcripts_root,
        &state.kb_client,
    )
    .await?;

    let name = body
        .name
        .clone()
        .unwrap_or_else(|| default_session_set_name(&diff));
    let spans = materialize_spans(&diff, repo);
    let repo_name = repo.name.clone();

    let id = new_set_id();
    let now = chrono::Utc::now().timestamp();
    // 2026-08-31 incident (store.rs module doc): the create + read-back +
    // view compose (everything after the async session-diff pull above)
    // runs as one closure; `bus.emit` doesn't touch the store, so it stays
    // outside, after.
    let repo_name_bg = repo_name.clone();
    let view = state
        .store
        .run_blocking(move |store| {
            store.create_reading_set(&id, repo_id, &name, None, &spans, now)?;
            let row = store
                .get_reading_set(&id)?
                .ok_or_else(|| vanished_after("create", &id))?;
            set_view(store, row, &repo_name_bg)
        })
        .await?;
    state
        .bus
        .emit("set.changed", serde_json::json!({ "repo": repo_name }));
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(view),
    ))
}

fn default_session_set_name(diff: &sessiondiff::SessionDiff) -> String {
    let label = diff
        .display_name
        .as_deref()
        .unwrap_or(diff.session_id.as_str());
    format!(
        "session: {}",
        truncate_chars(label, DEFAULT_NAME_PROMPT_CHARS)
    )
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        s.to_string()
    }
}

// --- POST /api/sets/from-doc (DCB-W3.C) -----------------------------------

#[derive(Debug, Deserialize)]
pub struct FromDocBody {
    pub repo: String,
    pub kb: String,
    pub doc: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// `POST /api/sets/from-doc` (LOOPBACK-ONLY — `transcripts_api`, the same
/// sub-router as `/sets/from-session`; unlike that route this one never
/// carries raw transcript/doc-prose text of its own, but consistency with
/// its one sibling route — and materializing into a new named resource
/// being the same blast-radius class as every other mutation on that
/// sub-router — argues for staying put, `16-w3-reverse-index.md` §3.1).
///
/// Materializes the CURRENTLY RESOLVED doc-lens (`doclens::resolve::
/// resolve_lens`, called IN-PROCESS — never a second HTTP round trip to
/// self) into a new reading set. Only `path_state == "present"` refs become
/// spans — the same "no span without a single honest path" rule
/// `doclens::sync`'s module doc states for `doc_refs` (§1.1 there):
/// ambiguous/absent/external refs have nothing honest to materialize.
/// Spans are GROUP-ORDERED (`(group.ordinal, ref.ordinal)`, an ungrouped
/// ref sorting AFTER every real group — the same trailing-ungrouped
/// convention `docLensUrl.ts`'s `stepGroup` already uses client-side for
/// the sibling group-nav feature), not raw wire order: a doc's headings are
/// the natural reading order Tour mode should walk. See
/// [`materialize_lens_spans`] for each span's `note`/`ref` construction.
///
/// `404` unknown repo (`find_repo`'s own error). `404` also on ANY
/// unresolved-lens outcome (kb daemon disabled/unreachable, the doc 404s on
/// kb's side, the repo is still indexing, …) — collapsed to ONE honest
/// `ApiError::not_found`, the same shape `from_session_route` uses for its
/// own "unknown session" case: this route does not need to distinguish
/// `resolve_lens`'s finer-grained `reason` vocabulary the way the live lens
/// route does, since there is nothing a caller of THIS route can do with a
/// `repo_indexing` vs. a `kb_unreachable` beyond "try again later" — but
/// (DCB-W3.C.R Minor 3) the collapsed error still CARRIES `resolve_lens`'s
/// own `reason` (via `with_reason`, falling back to the generic
/// `"lens_unresolved"` on the rare error that attached none), so a caller
/// that DOES want the finer-grained code (an agent retry loop, say) can
/// still read it off the same `{error, reason}` shape every other DCB route
/// uses, without this route growing a second branch to expose it.
/// `409` on a `(repo, name)` collision (`create_reading_set_with_provenance`'s
/// existing behavior, via `StoreError::NameConflict`) — the DEFAULT name
/// embeds the new set's own id alongside a minute-precision timestamp
/// (DCB-W3.C.R Major 1) specifically so re-materializing (`SetDetail.tsx`'s
/// banner, §3.3) never collides BY CONSTRUCTION; an EXPLICIT `name` can
/// still collide, same as `from-session`.
///
/// (deviation, recorded) — `source_doc_id` is stamped from `lens.doc_id`,
/// the RESOLVED id, never the requested `body.doc`: `resolve_lens` may have
/// followed kb's moves chain and re-keyed the pin under us
/// (`lens.moved_from.is_some()`), and every other consumer in this
/// milestone (`doclens::sync::write_claims`) keys its own persisted row on
/// `lens.doc_id` for exactly that reason — storing the caller's original id
/// here would be the one place in DCB that persists a pre-move id as if it
/// were canonical.
pub async fn from_doc_route(
    State(state): State<SharedState>,
    Json(body): Json<FromDocBody>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &body.repo)?;

    // CT-F2's `at=declared` is a read-surface opt-in; materializing a
    // reading set from the current-tree lens never needs it.
    let lens =
        crate::doclens::resolve::resolve_lens(&state, &body.kb, &body.doc, Some(&repo.name), false)
            .await
            .map_err(|e| {
                let reason = e.reason().unwrap_or("lens_unresolved");
                ApiError::not_found(format!(
                    "could not resolve {}/{}: {}",
                    body.kb,
                    body.doc,
                    e.message()
                ))
                .with_reason(reason)
            })?;

    let now_dt = chrono::Utc::now();
    let now = now_dt.timestamp();
    // DCB-W3.C.R Major 1 — the id is generated FIRST so the default name can
    // embed it (collision-proof by construction, not just by the minute-
    // precision timestamp).
    let id = new_set_id();
    let name = body
        .name
        .clone()
        .unwrap_or_else(|| default_doc_set_name(&lens, now_dt, &id));
    let spans = materialize_lens_spans(&lens)?;
    let repo_name = repo.name.clone();
    let kb = body.kb.clone();
    let doc_id = lens.doc_id.clone();
    let doc_path = lens.doc_path.clone();
    let doc_hash = lens.doc_hash.clone();

    // 2026-08-31 incident (store.rs module doc): the create + read-back +
    // view compose (everything after the async lens resolve above) runs
    // as one closure; `bus.emit` doesn't touch the store, so it stays
    // outside, after.
    let repo_name_bg = repo_name.clone();
    let view = state
        .store
        .run_blocking(move |store| {
            store.create_reading_set_with_provenance(
                &id,
                repo_id,
                &name,
                None,
                &spans,
                now,
                Some(&kb),
                Some(&doc_id),
                doc_path.as_deref(),
                doc_hash.as_deref(),
                SET_KIND_SET,
                None,
                None,
                None,
            )?;
            let row = store
                .get_reading_set(&id)?
                .ok_or_else(|| vanished_after("create", &id))?;
            set_view(store, row, &repo_name_bg)
        })
        .await?;
    state
        .bus
        .emit("set.changed", serde_json::json!({ "repo": repo_name }));
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(view),
    ))
}

/// `"{doc_title or doc_id} · {minute-precision timestamp} · {id6}"` — the
/// DEFAULT set name (`from_doc_route`'s own doc). `doc_title` is
/// empty/whitespace-only exactly when kb's own `write_claims`-equivalent
/// write-time guard didn't apply here (this is a LIVE resolve, not a
/// `doc_refs` row) — falls back to `doc_id`, which is always non-empty.
/// `id6` (DCB-W3.C.R Major 1) is the new set's own id, so a default name is
/// collision-proof BY CONSTRUCTION rather than merely by the timestamp's
/// minute granularity — two materializations of the same doc in the same
/// minute get two different ids, hence two different default names, so
/// `create_reading_set_with_provenance`'s `(repo, name)` uniqueness check
/// can never 409 on a default name.
fn default_doc_set_name(
    lens: &doclens::wire::CodeLensOut,
    now: chrono::DateTime<chrono::Utc>,
    id: &str,
) -> String {
    let title = lens
        .doc_title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(lens.doc_id.as_str());
    format!("{title} · {} · {}", now.format("%Y-%m-%d %H:%M"), id6(id))
}

/// The first 6 hex chars of a `set_`-prefixed id (`new_set_id`'s own shape)
/// — the literal `"set_"` prefix is stripped first so the suffix is always
/// hex, never diluted by the constant prefix.
fn id6(id: &str) -> &str {
    let hex = id.strip_prefix("set_").unwrap_or(id);
    &hex[..hex.len().min(6)]
}

/// The span-level pinned git ref (DCB-W3.C.R Blocker 2): the full 40-hex
/// `head_sha`, UNMODIFIED, when the resolving checkout was clean — an
/// unambiguous revspec forever, which `GET /api/file?ref=` can resolve
/// directly. `None` when the tree was dirty or the repo has no commits yet
/// (`head_sha` is `None` in that case, `doc_refs`'s own schema comment) —
/// there is no honest pin in either case, and the old
/// `"{repo}@{short_sha}[+dirty]"` LABEL was never a valid revspec to begin
/// with (a 404 on every from-doc span link/Tour step). The lost provenance
/// is folded into [`dirty_honesty_note`] instead of fabricated here.
/// `SetDetail.tsx`'s span-ref chip shortens this client-side (`shortSha`,
/// `web-code/src/lib/format.ts`) — display-only, never round-tripped.
fn span_git_ref(repo: &doclens::wire::RepoOut) -> Option<String> {
    if repo.dirty.unwrap_or(false) {
        return None;
    }
    repo.head_sha.clone()
}

/// The honesty-note suffix appended to every span's `note` whenever
/// [`span_git_ref`] returned `None` — so a dirty/uncommitted resolve's
/// provenance isn't silently dropped, just never fabricated into an
/// unresolvable `ref`. `None` on a clean, committed resolve (the pinned
/// `ref` already says everything a note suffix would).
fn dirty_honesty_note(repo: &doclens::wire::RepoOut) -> Option<String> {
    match (repo.dirty.unwrap_or(false), repo.head_sha.as_deref()) {
        (true, Some(sha)) => Some(format!(" · from dirty tree @ {}", short_sha(sha))),
        (true, None) => Some(" · from dirty tree (no commits yet)".to_string()),
        (false, None) => Some(" · no commits yet".to_string()),
        (false, Some(_)) => None,
    }
}

/// First 7 chars — same length as `lenses.rs`'s own `short_id(&sha, 7)` /
/// `shortSha`'s client-side default (`web-code/src/lib/format.ts`).
/// Char-based (not byte slicing) so a pathological non-hex sha can never
/// panic on a multi-byte boundary.
fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// Normalizes one ref's line range to the shape [`build_span`] requires —
/// BOTH set or BOTH absent, never a half-range, never inverted (DCB-W3.C.R
/// Blocker 1). The resolved and hint sources are never mixed: a resolved
/// `line_end` only ever pairs with a resolved `line_start`, and a hint
/// `line_end` only ever pairs with a hint `line_start` — the OLD
/// `r.resolved_line.or(r.line_hint)` / `r.resolved_line_end.or(r.line_hint_end)`
/// construction could instead pair a resolved start with a STALE hint end
/// (or the reverse), producing a half-range whenever only one side had
/// resolved, or an inversion whenever the hint end had drifted behind a
/// resolved start. An end that IS present but invalid (`< start`) clamps
/// down to `end = start` rather than being rejected — the range is still
/// honestly anchored at the one line this daemon verified/was told, just
/// without a wider tail we can't stand behind.
fn normalized_span_lines(r: &doclens::wire::RefOut) -> (Option<u32>, Option<u32>) {
    let (start, end) = match r.resolved_line {
        Some(s) => (s, r.resolved_line_end),
        None => match r.line_hint {
            Some(s) => (s, r.line_hint_end),
            None => return (None, None),
        },
    };
    let end = end.filter(|&e| e >= start).unwrap_or(start);
    (Some(start), Some(end))
}

/// Materializes `lens.refs` into ordered reading-set spans — mirrors
/// `doclens::sync::claims_of`'s `path_state == "present"` filter (the doc
/// comment on [`from_doc_route`] states the "no span without a single
/// honest path" rule) but produces [`NewReadingSetSpan`]s, not `doc_refs`
/// claims: every span carries the SAME pinned `ref` (this materialization's
/// one resolved checkout, [`span_git_ref`]) and a per-ref `note`. Every span
/// is built via [`build_span`] (DCB-W3.C.R Blocker 1) — the SAME validation
/// `validate_span` applies on the `POST`/`PATCH` wire path — so this writer
/// cannot structurally persist a shape its own sibling route would reject;
/// propagates that `Err` (an escaping `resolved_path`, which should never
/// happen — `resolve_lens` only ever resolves against this repo's own file
/// index — but is surfaced rather than silently dropped if it somehow did).
fn materialize_lens_spans(
    lens: &doclens::wire::CodeLensOut,
) -> Result<Vec<NewReadingSetSpan>, ApiError> {
    let group_ordinal: HashMap<&str, u32> = lens
        .groups
        .iter()
        .map(|g| (g.key.as_str(), g.ordinal))
        .collect();

    let mut present: Vec<&doclens::wire::RefOut> = lens
        .refs
        .iter()
        .filter(|r| {
            r.path_state == Some(doclens::resolve::PathState::Present) && r.resolved_path.is_some()
        })
        .collect();
    // GROUP-ORDERED: `(group.ordinal, ref.ordinal)`. An ungrouped ref
    // (`group: None`, or a `group` key with no matching entry in
    // `lens.groups` — should never happen, but sorts the same way a real
    // orphan would) gets `u32::MAX` — sorts AFTER every real group, the
    // same trailing-ungrouped convention `docLensUrl.ts`'s `stepGroup`
    // already uses client-side (`[null, ...groups, UNGROUPED_SEL]`).
    present.sort_by_key(|r| {
        let go = r
            .group
            .as_deref()
            .and_then(|k| group_ordinal.get(k))
            .copied()
            .unwrap_or(u32::MAX);
        (go, r.ordinal)
    });

    let fallback_title = lens
        .doc_title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(lens.doc_id.as_str());

    let git_ref = span_git_ref(&lens.repo);
    let honesty_suffix = dirty_honesty_note(&lens.repo);

    present
        .into_iter()
        .map(|r| {
            let path = r.resolved_path.clone().unwrap_or_default();
            let basename = path.rsplit('/').next().unwrap_or(&path).to_string();
            let group_label = r
                .group
                .as_deref()
                .and_then(|key| lens.groups.iter().find(|g| g.key == key))
                .map(|g| g.label.as_str())
                .unwrap_or(fallback_title);
            let mut note = format!("{group_label} · {basename}");
            if let Some(suffix) = &honesty_suffix {
                note.push_str(suffix);
            }
            let (line_start, line_end) = normalized_span_lines(r);
            build_span(&path, line_start, line_end, git_ref.clone(), Some(note))
        })
        .collect()
}

/// Every path touched by exactly ONE commit across the whole session, paired
/// with that commit's own `subject` (`None` when the commit had none) — the
/// module doc's "unambiguous" rule. A path touched by TWO OR MORE distinct
/// commits maps to `None` here (and is simply absent from
/// [`materialize_spans`]'s note lookup, same effect).
fn unambiguous_commit_notes(diff: &sessiondiff::SessionDiff) -> HashMap<String, Option<String>> {
    let mut touches: HashMap<String, Vec<Option<String>>> = HashMap::new();
    for seg in &diff.segments {
        if let Segment::Commits { commits } = seg {
            for c in commits {
                for f in &c.files {
                    touches
                        .entry(f.path.clone())
                        .or_default()
                        .push(c.subject.clone());
                }
            }
        }
    }
    touches
        .into_iter()
        .filter_map(|(path, subjects)| {
            if subjects.len() == 1 {
                Some((path, subjects.into_iter().next().flatten()))
            } else {
                None
            }
        })
        .collect()
}

/// Absolute path -> repo-relative, forward-slash — `None` when `abs` isn't
/// under `repo.path` at all (see the module doc's "Uncommitted segment"
/// note on why that can legitimately happen). A small, deliberate
/// duplication of `sessiondiff`'s own private `owning_repo_rel` (that fn
/// returns a repo REFERENCE this caller doesn't need, and is private to its
/// module) — same "duplicated here rather than imported" precedent
/// `sessiondiff::repo_root_matches`'s own doc comment already documents for
/// an equally small helper.
fn repo_relative(abs: &str, repo: &RepoEntry) -> Option<String> {
    Path::new(abs)
        .strip_prefix(&repo.path)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
}

/// Walk `diff.segments` in NARRATIVE order, producing one whole-file span
/// per FIRST-TOUCHED path (commits' files, then uncommitted-evidence files
/// — see the module doc). Dedup is global across the whole diff, not
/// per-segment.
fn materialize_spans(diff: &sessiondiff::SessionDiff, repo: &RepoEntry) -> Vec<NewReadingSetSpan> {
    let notes = unambiguous_commit_notes(diff);
    let mut seen: HashSet<String> = HashSet::new();
    let mut spans = Vec::new();
    for seg in &diff.segments {
        match seg {
            Segment::Commits { commits } => {
                for c in commits {
                    for f in &c.files {
                        if !seen.insert(f.path.clone()) {
                            continue;
                        }
                        spans.push(NewReadingSetSpan {
                            path: f.path.clone(),
                            note: notes.get(&f.path).cloned().flatten(),
                            ..Default::default()
                        });
                    }
                }
            }
            Segment::Uncommitted { files, .. } => {
                for abs in files {
                    let Some(rel) = repo_relative(abs, repo) else {
                        continue;
                    };
                    if !seen.insert(rel.clone()) {
                        continue;
                    }
                    spans.push(NewReadingSetSpan {
                        path: rel,
                        ..Default::default()
                    });
                }
            }
            Segment::Prompt { .. } => {}
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessiondiff::{CommitEntryOut, CommitFileOut, CommitsStatus, Totals};

    fn repo(path: &str) -> RepoEntry {
        RepoEntry {
            name: "r".to_string(),
            path: std::path::PathBuf::from(path),
        }
    }

    fn commit_file(path: &str) -> CommitFileOut {
        CommitFileOut {
            path: path.to_string(),
            insertions: 1,
            deletions: 0,
            binary: false,
        }
    }

    fn diffed_commit(sha: &str, subject: &str, files: Vec<CommitFileOut>) -> CommitEntryOut {
        CommitEntryOut {
            sha: sha.to_string(),
            repo: Some("r".to_string()),
            subject: Some(subject.to_string()),
            author: None,
            trailers: Vec::new(),
            diffed: true,
            author_time: Some(1),
            files,
            insertions: 1,
            deletions: 0,
        }
    }

    fn fixture_diff(segments: Vec<Segment>) -> sessiondiff::SessionDiff {
        sessiondiff::SessionDiff {
            version: "session-diff/1",
            session_id: "sess-1".to_string(),
            display_name: Some("add the widget".to_string()),
            segments,
            repos_touched: vec!["r".to_string()],
            totals: Totals::default(),
            commits_status: CommitsStatus::Ok,
        }
    }

    #[test]
    fn validate_span_accepts_whole_file_and_ranges_rejects_half_ranges_and_bad_order() {
        let whole = SpanInput {
            path: "a.rs".to_string(),
            line_start: None,
            line_end: None,
            git_ref: None,
            note: None,
        };
        assert!(validate_span(&whole).is_ok());

        let range = SpanInput {
            line_start: Some(2),
            line_end: Some(5),
            ..whole.clone()
        };
        let v = validate_span(&range).unwrap();
        assert_eq!(v.line_start, Some(2));
        assert_eq!(v.line_end, Some(5));

        let half = SpanInput {
            line_start: Some(2),
            line_end: None,
            ..whole.clone()
        };
        assert!(validate_span(&half).is_err());

        let backwards = SpanInput {
            line_start: Some(9),
            line_end: Some(2),
            ..whole.clone()
        };
        assert!(validate_span(&backwards).is_err());

        let zero = SpanInput {
            line_start: Some(0),
            line_end: Some(1),
            ..whole.clone()
        };
        assert!(validate_span(&zero).is_err());

        let escaping = SpanInput {
            path: "../etc/passwd".to_string(),
            ..whole
        };
        assert!(validate_span(&escaping).is_err());
    }

    #[test]
    fn default_session_set_name_truncates_a_long_prompt() {
        let mut diff = fixture_diff(Vec::new());
        diff.display_name = Some("x".repeat(200));
        let name = default_session_set_name(&diff);
        assert!(name.starts_with("session: "));
        assert!(name.ends_with('…'));
        assert!(name.chars().count() <= "session: ".len() + DEFAULT_NAME_PROMPT_CHARS);
    }

    #[test]
    fn default_session_set_name_falls_back_to_the_session_id_with_no_prompt() {
        let mut diff = fixture_diff(Vec::new());
        diff.display_name = None;
        assert_eq!(default_session_set_name(&diff), "session: sess-1");
    }

    #[test]
    fn repo_relative_strips_the_repo_root_and_drops_foreign_paths() {
        let r = repo("/home/user/proj");
        assert_eq!(
            repo_relative("/home/user/proj/src/lib.rs", &r).as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(repo_relative("/somewhere/else/x.rs", &r), None);
    }

    #[test]
    fn materialize_spans_dedups_first_touch_and_notes_unambiguous_commits_only() {
        let r = repo("/repo");
        let diff = fixture_diff(vec![
            Segment::Prompt {
                ts: 0,
                uuid: "u1".to_string(),
                text: "hi".to_string(),
            },
            Segment::Commits {
                commits: vec![diffed_commit(
                    "sha1",
                    "fix the widget",
                    vec![commit_file("a.rs"), commit_file("b.rs")],
                )],
            },
            // b.rs touched again by a SECOND commit -> ambiguous, no note.
            Segment::Commits {
                commits: vec![diffed_commit("sha2", "tweak b", vec![commit_file("b.rs")])],
            },
            Segment::Uncommitted {
                files: vec![
                    "/repo/c.rs".to_string(),
                    // Already covered by a commit — session_diff itself
                    // would normally have filtered this out already, but
                    // materialize_spans's own dedup must handle a
                    // re-appearance defensively too.
                    "/repo/a.rs".to_string(),
                    // Outside the repo entirely — dropped.
                    "/elsewhere/d.rs".to_string(),
                ],
                turns: Vec::new(),
            },
        ]);

        let spans = materialize_spans(&diff, &r);
        let paths: Vec<&str> = spans.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["a.rs", "b.rs", "c.rs"],
            "first-touch order, deduped, foreign path dropped"
        );
        assert!(spans
            .iter()
            .all(|s| s.line_start.is_none() && s.line_end.is_none()));

        let by_path: HashMap<&str, &NewReadingSetSpan> =
            spans.iter().map(|s| (s.path.as_str(), s)).collect();
        assert_eq!(
            by_path["a.rs"].note.as_deref(),
            Some("fix the widget"),
            "a.rs touched by exactly one commit"
        );
        assert_eq!(
            by_path["b.rs"].note, None,
            "b.rs touched by two different commits — ambiguous"
        );
        assert_eq!(
            by_path["c.rs"].note, None,
            "uncommitted evidence has no note"
        );
    }

    // --- from-doc materialization (DCB-W3.C) --------------------------------

    fn present_ref(
        ordinal: u32,
        group: Option<&str>,
        resolved_path: &str,
    ) -> doclens::wire::RefOut {
        doclens::wire::RefOut {
            ordinal,
            group: group.map(str::to_string),
            kind: "path".into(),
            raw: resolved_path.to_string(),
            declared: false,
            path_hint: Some(resolved_path.to_string()),
            line_hint: None,
            line_hint_end: None,
            symbol_container: None,
            symbol_member: None,
            context: None,
            path_state: Some(doclens::resolve::PathState::Present),
            resolved_path: Some(resolved_path.to_string()),
            candidate_count: 1,
            candidates: vec![resolved_path.to_string()],
            issue: None,
            line_state: doclens::resolve::LineState::Confirmed,
            line_evidence: doclens::resolve::LineEvidence::None,
            confirm_token: None,
            token_line: None,
            resolved_line: None,
            resolved_line_end: None,
            line_hint_delta: None,
            file_lines: 10,
            line_reason: None,
            remap: None,
            symbol_state: doclens::resolve::SymbolState::NoSymbol,
            symbol_hit_count: 0,
            symbol_hits: Vec::new(),
            spans: Vec::new(),
            reader: None,
            search: None,
            note: None,
            when_written: None,
        }
    }

    fn ambiguous_ref(ordinal: u32) -> doclens::wire::RefOut {
        doclens::wire::RefOut {
            path_state: Some(doclens::resolve::PathState::Ambiguous),
            resolved_path: None,
            candidate_count: 2,
            ..present_ref(ordinal, None, "dup.rs")
        }
    }

    fn group_out(key: &str, label: &str, ordinal: u32) -> doclens::wire::GroupOut {
        doclens::wire::GroupOut {
            key: key.to_string(),
            label: label.to_string(),
            anchor: key.to_string(),
            ordinal,
            ref_count: 0,
        }
    }

    fn repo_out(name: &str, head_sha: Option<&str>, dirty: Option<bool>) -> doclens::wire::RepoOut {
        doclens::wire::RepoOut {
            name: name.to_string(),
            root: "/repos/x".to_string(),
            state: "ready",
            head_sha: head_sha.map(str::to_string),
            head_branch: Some("main".to_string()),
            dirty,
            source: "param",
        }
    }

    fn lens_fixture(
        doc_title: Option<&str>,
        refs: Vec<doclens::wire::RefOut>,
        groups: Vec<doclens::wire::GroupOut>,
        repo: doclens::wire::RepoOut,
    ) -> doclens::wire::CodeLensOut {
        doclens::wire::CodeLensOut {
            schema: "codelens/1",
            kb: "platform".into(),
            doc_id: "9f8b7182d433".into(),
            moved_from: None,
            doc_path: Some("docs/checkout.html".into()),
            doc_href: None,
            doc_hash: Some("deadbeef".into()),
            doc_title: doc_title.map(str::to_string),
            doc_extracted_at: None,
            doc_code_rev: None,
            rev_remap: None,
            never_scanned: false,
            repo,
            resolved_unix: 1_000,
            truncated: false,
            partial: false,
            partial_reason: None,
            counts: doclens::wire::LensCounts::default(),
            ungrouped_count: 0,
            groups,
            refs,
            era: "none",
            note: "",
        }
    }

    /// A 40-hex fixture sha — realistic enough to exercise [`span_git_ref`]'s
    /// "pinned verbatim" contract (the old label-based test used a
    /// deliberately-short fake sha; this one proves the FULL hex round-trips
    /// unmodified rather than being silently truncated somewhere).
    const FIXTURE_SHA: &str = "abcdef0123456789abcdef0123456789abcdef01";

    #[test]
    fn materialize_lens_spans_keeps_only_present_refs_group_ordered() {
        let refs = vec![
            present_ref(5, Some("g0"), "a.rs"),
            present_ref(1, Some("g1"), "b.rs"),
            present_ref(0, None, "z.rs"), // ungrouped — lowest ref ordinal, must still sort LAST
            ambiguous_ref(2),             // filtered out entirely — no honest path to store
        ];
        let groups = vec![group_out("g0", "G0", 0), group_out("g1", "G1", 1)];
        let lens = lens_fixture(
            Some("Checkout flow"),
            refs,
            groups,
            repo_out("alpha", Some(FIXTURE_SHA), Some(false)),
        );

        let spans = materialize_lens_spans(&lens).unwrap();
        let paths: Vec<&str> = spans.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["a.rs", "b.rs", "z.rs"],
            "(group.ordinal, ref.ordinal) order — ungrouped trails every real group \
             regardless of its own ref ordinal"
        );
        assert_eq!(spans[0].note.as_deref(), Some("G0 · a.rs"));
        assert_eq!(spans[1].note.as_deref(), Some("G1 · b.rs"));
        // Ungrouped falls back to the doc title, not an empty group label.
        assert_eq!(spans[2].note.as_deref(), Some("Checkout flow · z.rs"));
        // A clean, committed resolve: every span pins the SAME full 40-hex
        // sha, verbatim — never a "{repo}@{sha}" label (DCB-W3.C.R Blocker 2).
        assert!(spans
            .iter()
            .all(|s| s.git_ref.as_deref() == Some(FIXTURE_SHA)));
    }

    #[test]
    fn materialize_lens_spans_prefers_the_resolved_line_over_the_doc_hint() {
        let mut r = present_ref(0, None, "a.rs");
        r.line_hint = Some(40);
        r.line_hint_end = Some(41);
        r.resolved_line = Some(12);
        r.resolved_line_end = Some(13);
        let lens = lens_fixture(
            Some("Doc"),
            vec![r],
            vec![],
            repo_out("alpha", Some(FIXTURE_SHA), Some(false)),
        );
        let spans = materialize_lens_spans(&lens).unwrap();
        assert_eq!(spans[0].line_start, Some(12));
        assert_eq!(spans[0].line_end, Some(13));

        // No resolution at all ⇒ fall back to the doc's own unverified hint.
        let mut r2 = present_ref(0, None, "a.rs");
        r2.line_hint = Some(40);
        r2.line_hint_end = Some(41);
        let lens2 = lens_fixture(
            Some("Doc"),
            vec![r2],
            vec![],
            repo_out("alpha", Some(FIXTURE_SHA), Some(false)),
        );
        let spans2 = materialize_lens_spans(&lens2).unwrap();
        assert_eq!(spans2[0].line_start, Some(40));
        assert_eq!(spans2[0].line_end, Some(41));
    }

    /// (DCB-W3.C.R Blocker 1) — every combination [`normalized_span_lines`]
    /// must produce a BOTH-or-NEITHER, non-inverted pair: a resolved half-
    /// range, a drifted-vs-hint inversion, a fully resolved pair (kept
    /// as-is), and no line info at all.
    #[test]
    fn normalized_span_lines_never_mixes_sources_and_never_inverts() {
        // (Some(2), None) — a resolved start with NO resolved end (the
        // token-pass/single-line-citation case) — must NOT fall through to
        // the doc's own (possibly stale) hint end; both sides collapse to
        // the single resolved line.
        let mut r = present_ref(0, None, "a.rs");
        r.resolved_line = Some(2);
        r.resolved_line_end = None;
        r.line_hint = Some(2);
        r.line_hint_end = Some(2);
        assert_eq!(normalized_span_lines(&r), (Some(2), Some(2)));

        // Drifted resolved start (62) past a stale hint end (45) — the OLD
        // `resolved_line_end.or(line_hint_end)` construction would have
        // paired them into an inverted (62, 45); the fix must never even
        // look at the hint end once a resolved start exists.
        let mut r2 = present_ref(0, None, "a.rs");
        r2.resolved_line = Some(62);
        r2.resolved_line_end = None;
        r2.line_hint = Some(40);
        r2.line_hint_end = Some(45);
        assert_eq!(normalized_span_lines(&r2), (Some(62), Some(62)));

        // Both resolved, a real range — kept as-is.
        let mut r3 = present_ref(0, None, "a.rs");
        r3.resolved_line = Some(10);
        r3.resolved_line_end = Some(20);
        assert_eq!(normalized_span_lines(&r3), (Some(10), Some(20)));

        // No resolution AND no hint at all — a whole-file span.
        let r4 = present_ref(0, None, "a.rs");
        assert_eq!(normalized_span_lines(&r4), (None, None));

        // A hint-only ref (never resolved) whose OWN hint end is behind its
        // hint start — same clamp applies within the hint source too.
        let mut r5 = present_ref(0, None, "a.rs");
        r5.line_hint = Some(30);
        r5.line_hint_end = Some(10);
        assert_eq!(normalized_span_lines(&r5), (Some(30), Some(30)));
    }

    #[test]
    fn default_doc_set_name_falls_back_to_doc_id_when_title_is_blank_and_embeds_id6() {
        use chrono::TimeZone;
        let now = chrono::Utc
            .with_ymd_and_hms(2026, 8, 11, 14, 32, 0)
            .unwrap();

        let with_title = lens_fixture(
            Some("Checkout flow"),
            vec![],
            vec![],
            repo_out("alpha", None, None),
        );
        assert_eq!(
            default_doc_set_name(&with_title, now, "set_abc123def456"),
            "Checkout flow · 2026-08-11 14:32 · abc123"
        );

        let blank_title = lens_fixture(Some("   "), vec![], vec![], repo_out("alpha", None, None));
        assert_eq!(
            default_doc_set_name(&blank_title, now, "set_abc123def456"),
            "9f8b7182d433 · 2026-08-11 14:32 · abc123"
        );

        let no_title = lens_fixture(None, vec![], vec![], repo_out("alpha", None, None));
        assert_eq!(
            default_doc_set_name(&no_title, now, "set_abc123def456"),
            "9f8b7182d433 · 2026-08-11 14:32 · abc123"
        );
    }

    #[test]
    fn default_doc_set_name_is_collision_proof_across_two_ids_in_the_same_minute() {
        use chrono::TimeZone;
        let now = chrono::Utc
            .with_ymd_and_hms(2026, 8, 11, 14, 32, 0)
            .unwrap();
        let lens = lens_fixture(Some("Doc"), vec![], vec![], repo_out("alpha", None, None));
        let a = default_doc_set_name(&lens, now, &new_set_id());
        let b = default_doc_set_name(&lens, now, &new_set_id());
        assert_ne!(
            a, b,
            "two ids in the same minute must never collide by name"
        );
    }

    #[test]
    fn id6_strips_the_set_prefix_and_takes_six_hex_chars() {
        assert_eq!(id6("set_abc123def456"), "abc123");
        // Defensive: an id without the expected prefix still degrades to
        // "first 6 chars," never panics.
        assert_eq!(id6("short"), "short");
    }

    #[test]
    fn span_git_ref_pins_the_full_sha_only_when_clean_and_committed() {
        // Clean + committed ⇒ the full 40-hex sha, unmodified.
        assert_eq!(
            span_git_ref(&repo_out("alpha", Some(FIXTURE_SHA), Some(false))),
            Some(FIXTURE_SHA.to_string())
        );
        // Dirty ⇒ no honest pin, never a fabricated "+dirty" label.
        assert_eq!(
            span_git_ref(&repo_out("alpha", Some(FIXTURE_SHA), Some(true))),
            None
        );
        // No commits yet ⇒ no sha to pin regardless of dirtiness.
        assert_eq!(span_git_ref(&repo_out("alpha", None, Some(false))), None);
        assert_eq!(span_git_ref(&repo_out("alpha", None, Some(true))), None);
    }

    #[test]
    fn dirty_honesty_note_only_fires_when_span_git_ref_is_none() {
        // Clean + committed: `span_git_ref` already says everything — no
        // suffix.
        assert_eq!(
            dirty_honesty_note(&repo_out("alpha", Some(FIXTURE_SHA), Some(false))),
            None
        );
        // Dirty, with a sha to reference: the short form, same length as
        // `short_sha`'s 7-char convention.
        assert_eq!(
            dirty_honesty_note(&repo_out("alpha", Some(FIXTURE_SHA), Some(true))),
            Some(" · from dirty tree @ abcdef0".to_string())
        );
        // Dirty, no commits at all: no sha to reference.
        assert_eq!(
            dirty_honesty_note(&repo_out("alpha", None, Some(true))),
            Some(" · from dirty tree (no commits yet)".to_string())
        );
        // Clean-per-git-status but no commits: still no honest pin.
        assert_eq!(
            dirty_honesty_note(&repo_out("alpha", None, Some(false))),
            Some(" · no commits yet".to_string())
        );
    }

    // --- V70-A10 workspaces v0 ------------------------------------------

    #[test]
    fn set_kind_vocab_accepts_exactly_set_and_workspace() {
        assert!(is_valid_set_kind("set"));
        assert!(is_valid_set_kind("workspace"));
        for k in ["Set", "Workspace", "", "sets", "review"] {
            assert!(!is_valid_set_kind(k), "{k} should be invalid");
        }
    }

    #[test]
    fn validate_desk_json_accepts_any_valid_json_and_rejects_malformed_or_oversized() {
        assert!(validate_desk_json("{}").is_ok());
        assert!(validate_desk_json("[1,2,3]").is_ok());
        assert!(validate_desk_json("\"just a string\"").is_ok());
        assert!(validate_desk_json("not json").is_err());
        let oversized = "1".repeat(MAX_DESK_JSON_BYTES + 1);
        assert!(validate_desk_json(&oversized).is_err());
        // Right at the cap is fine — a valid JSON STRING of exactly that
        // many bytes (quotes included in the budget). NOT a bare numeric
        // literal of that many digits: serde_json legitimately rejects a
        // JSON number that overflows f64's representable range as
        // malformed (`ErrorCode::NumberOutOfRange`, `de.rs`'s
        // `f64_from_parts`/`parse_long_integer`) regardless of how many
        // digits it has — a 64 KiB run of `1`s parses as `+inf`, not a
        // valid number, so it would exercise the wrong failure mode here.
        let at_cap = format!("\"{}\"", "a".repeat(MAX_DESK_JSON_BYTES - 2));
        assert_eq!(at_cap.len(), MAX_DESK_JSON_BYTES);
        assert!(validate_desk_json(&at_cap).is_ok());
    }

    #[test]
    fn validate_description_md_rejects_only_oversized() {
        assert!(validate_description_md("# hello").is_ok());
        assert!(validate_description_md("").is_ok());
        let oversized = "x".repeat(MAX_DESCRIPTION_MD_BYTES + 1);
        assert!(validate_description_md(&oversized).is_err());
    }

    #[test]
    fn validate_ref_label_normalizes_blank_to_none_and_rejects_injection_shapes() {
        assert_eq!(validate_ref_label(None).unwrap(), None);
        assert_eq!(validate_ref_label(Some("")).unwrap(), None);
        assert_eq!(validate_ref_label(Some("   ")).unwrap(), None);
        assert_eq!(
            validate_ref_label(Some("feature/x")).unwrap(),
            Some("feature/x".to_string())
        );
        // Trimmed.
        assert_eq!(
            validate_ref_label(Some("  main  ")).unwrap(),
            Some("main".to_string())
        );
        // Shape-rejected the same way `reviews::reject_user_ref` rejects
        // any other caller-supplied ref.
        assert!(validate_ref_label(Some("--output=/tmp/x")).is_err());
        assert!(validate_ref_label(Some("-u")).is_err());
    }

    fn summary(id: &str, name: &str, ref_label: Option<&str>) -> SetSummary {
        SetSummary {
            id: id.to_string(),
            name: name.to_string(),
            description: None,
            span_count: 0,
            note_count: 0,
            created_at: 0,
            updated_at: 0,
            kind: SET_KIND_WORKSPACE.to_string(),
            ref_label: ref_label.map(str::to_string),
        }
    }

    #[test]
    fn group_by_ref_sorts_alphabetically_with_ungrouped_last() {
        let summaries = vec![
            summary("set_c", "c", Some("zeta")),
            summary("set_a", "a", Some("alpha")),
            summary("set_b", "b", None),
            summary("set_d", "d", Some("alpha")),
        ];
        let groups = group_by_ref(summaries);
        let refs: Vec<Option<&str>> = groups.iter().map(|g| g.ref_label.as_deref()).collect();
        assert_eq!(refs, vec![Some("alpha"), Some("zeta"), None]);
        // The "alpha" bucket keeps both members, in incoming order.
        let alpha = &groups[0].workspaces;
        assert_eq!(alpha.len(), 2);
        assert_eq!(alpha[0].id, "set_a");
        assert_eq!(alpha[1].id, "set_d");
    }
}
