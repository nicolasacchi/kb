//! v0.40 TN1/TN2 — `GET /api/review-notes`, the private-NOTE browser.
//!
//! A "note" is a comment with `private: true`: written for the human, never
//! to be seen by an LLM. Nothing else in the daemon will show one to an
//! agent, which leaves the operator with exactly one problem — no single
//! place to read them. `/api/inbox` hides them, `GET /review/{id}` hides
//! them without `?visibility=all`, and `kb comments list` (the CLI) hides
//! them permanently. This route is that place.
//!
//! **PRIVATE NOTES ONLY, ALWAYS.** There is no `visibility` parameter on
//! this route and there must never be one: a notes browser with a
//! `?visibility=public` mode is a second `/reviews` with a different sort,
//! and every extra default is another place to get the hide-rule wrong. The
//! route has one meaning.
//!
//! The tag facets are computed over the **pre-tag-filter, pre-`q`, and
//! pre-`?status=`** set (the status filter runs inside the per-corpus walk),
//! and before the row cap, so clicking a second tag narrows the rows instead
//! of emptying them — the property the page's chip row depends on. Facet
//! counts describe the kb as it is, not the current selection.
//!
//! `?bodies=false` is a per-row PROJECTION, not a visibility switch: the row
//! set is identical, and only the `body` key is dropped. See
//! [`ReviewNotesQuery::bodies`].
//!
//! Cost shape is `inbox::collect_open`'s, deliberately: one `spawn_blocking`
//! per corpus walks `.review/`, parses each sidecar once via
//! `kb_core::review::load`, and returns that corpus's notes; only then does
//! ONE batched `ctx.storage.get_by_ids` join the live title +
//! `source_relative` for the artifacts that actually contribute a row. No
//! per-comment storage lookup, and no second `.review/` walk anywhere in
//! the feature outside the daycard's private-id join.
//!
//! NOTE the tag NAMESPACE: these are COMMENT tags (`Comment::tags`), a
//! different slug space from an artifact's `kb-tags` frontmatter. The two
//! are never mixed and a comment tag is never mirrored onto the artifact.
//! The facet row type is nevertheless [`super::tags::TagSummary`] verbatim —
//! same `{name, count, color_seed}` shape and the same deterministic chip
//! colour the rail already renders, because the SPA's tag chips must not
//! grow a second colour space.

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Query, RawQuery, State},
    http::Response,
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Utc};
use kb_core::review::{self, Anchor, Author, CommentStatus};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use super::tags::TagSummary;

/// Hard cap on returned rows. A cap with an honest `truncated` flag (the
/// daycard / inbox convention) rather than silent paging: this is a
/// triage page, and a note list that quietly drops its tail is a lie.
const MAX_REVIEW_NOTES: usize = 500;
/// Hard cap on the facet list. Same reasoning, plus a bound on the chip row:
/// 200 chips is already past anything a human scrolls.
const MAX_REVIEW_NOTE_TAGS: usize = 200;

/// One private note, with enough context to triage it and deep-link to the
/// text it points at (the SPA builds that link with `buildCiteUrl`, which
/// already emits `?panel=comments&comment=<id>` plus the anchor's own
/// locator).
///
/// snake_case, not camelCase: this is a LIST ROW, and `InboxItem` /
/// `ReviewRow` / `DaycardCommentItem` are all snake_case for a stated reason
/// (the CLI's `| jq` contract / the list-row convention). `Comment` itself
/// stays camelCase because that is the sidecar DOCUMENT, not a row.
#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "ReviewNoteRow")
)]
#[derive(Debug, Serialize)]
pub struct ReviewNoteRow {
    pub kb: String,
    pub artifact_id: String,
    /// Current title from storage, else the review file's own recorded title
    /// (the artifact may have left lance while its sidecar lives on).
    pub artifact_title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub source_relative: Option<String>,
    pub comment_id: String,
    pub status: CommentStatus,
    pub author: Author,
    /// The note text. `None` only under `?bodies=false` (see
    /// [`ReviewNotesQuery::bodies`]), and then the key is ABSENT from the
    /// JSON rather than present-and-empty: an empty string standing in for
    /// an omitted body is a lie the client cannot detect.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub body: Option<String>,
    pub anchor: Anchor,
    /// COMMENT tags — never the artifact's `kb-tags`. Always a (possibly
    /// empty) array, so the client never has to null-check.
    pub tags: Vec<String>,
    /// Always `true` on this route — it exists to list notes. Carried
    /// explicitly so the row is self-describing and shares a shape with a
    /// `/reviews` row.
    pub private: bool,
    pub reply_count: u32,
    /// The indexer's anchor-stale sidecar flags this anchor.
    pub stale: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub user: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Unix seconds — `InboxItem`'s convention, and the sort key. NOT
    /// `InboxItem`'s `created_at`: the two fields mean different things and
    /// are named independently.
    pub updated_at: i64,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "ReviewNotesResponse")
)]
#[derive(Debug, Serialize)]
pub struct ReviewNotesResponse {
    pub notes: Vec<ReviewNoteRow>,
    /// Facet counts over the PRE-`?tag=`, PRE-`?q=` and PRE-`?status=` set
    /// (the status filter runs inside the per-corpus walk, so it is already
    /// applied by the time this is computed), sorted
    /// `count DESC, name ASC` (the exact `aggregate_tags` order, so the chip
    /// row and the rail's tag list agree). Reused verbatim from
    /// `routes::tags::TagSummary` — see the module doc on why the TYPE is
    /// shared while the NAMESPACE is not.
    pub tags: Vec<TagSummary>,
    /// Total matching rows BEFORE the [`MAX_REVIEW_NOTES`] cap, so the page
    /// can say "showing 500 of 812" instead of implying 812 were sent.
    pub total: usize,
    pub truncated: bool,
    pub tags_truncated: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct ReviewNotesQuery {
    /// Restrict to one corpus. Absent → every configured kb (fleet-wide,
    /// like `/inbox`).
    pub kb: Option<String>,
    /// `?tag=` is repeatable and AND ("carries every listed tag"). NOT a
    /// struct field: a repeated key cannot be expressed by serde's derive
    /// here, so it is parsed from the raw query by [`parse_tag_filters`].
    /// Case-insensitive substring of the comment BODY.
    pub q: Option<String>,
    /// `open` | `resolved` | `all` (DEFAULT `all` — a note browser wants
    /// resolved notes; this is the opposite of `/reviews`, whose default is
    /// `open`, and the difference is deliberate).
    pub status: Option<String>,
    /// `bodies=false` omits the `body` field from every row, for callers
    /// that only need identity fields (which kb / artifact a note belongs
    /// to) and must not pull note text into their process — the CLI's
    /// `comments tag|untag`, which resolves a note's kb/artifact by
    /// scanning this route fleet-wide.
    ///
    /// This is a PROJECTION, not a visibility switch: the route still
    /// returns private notes ONLY, still has no `visibility` parameter, and
    /// `bodies=false` never reveals a note the default call would hide. The
    /// row set is identical to the default call's — only the per-row payload
    /// shrinks. Absent → `true`.
    pub bodies: Option<bool>,
}

/// Collect the repeated `?tag=` values out of the RAW query string.
///
/// Neither serde's derived struct nor axum's `Query` can express a repeated
/// key here, and both fail in ways that look like a server bug rather than a
/// grammar choice: a lone `tag=x` against `Vec<String>` is rejected
/// "expected a sequence", and a repeated key against a `deserialize_with`
/// field is rejected "duplicate field". `?tag=` is documented as repeatable,
/// so it is parsed here instead — which also means a single `?tag=wording`
/// and `?tag=a&tag=b` behave identically, as the grammar says they should.
fn parse_tag_filters(raw: &str) -> Result<Vec<String>, kb_core::Error> {
    let mut out = Vec::new();
    for (key, value) in form_urlencoded::parse(raw.as_bytes()) {
        if key != "tag" {
            continue;
        }
        let slug = kb_core::parser::slugify_tag(&value);
        if slug.is_empty() {
            return Err(kb_core::Error::BadRequest(format!(
                "tag filter {value:?} is empty once normalised; it would filter nothing"
            )));
        }
        out.push(slug);
    }
    Ok(out)
}

/// Per-corpus partial: rows already carrying the review file's own recorded
/// title and `source_relative: None`; the live values are patched in after
/// the batched storage join.
type CorpusNotes = Vec<ReviewNoteRow>;

/// `GET /api/review-notes?kb=&tag=&tag=&q=&status=&bodies=`
pub async fn list(
    State(state): State<Arc<KbHandles>>,
    // `RawQuery` before `Query` because the latter is a body extractor and
    // must stay last. `?tag=` is read from the raw string (repeatable keys
    // are inexpressible in the derived struct — see `parse_tag_filters`);
    // every other filter comes off the struct as usual.
    RawQuery(raw): RawQuery,
    Query(q): Query<ReviewNotesQuery>,
) -> Response<Body> {
    // Validate the filters BEFORE fanning out, so a typo costs no IO.
    let want_tags = match parse_tag_filters(raw.as_deref().unwrap_or("")) {
        Ok(t) => t,
        Err(e) => return error_to_problem_json(&e),
    };
    let want_status = match parse_status(q.status.as_deref()) {
        Ok(s) => s,
        Err(e) => return error_to_problem_json(&e),
    };
    let want_kb = q.kb.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let needle =
        q.q.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_lowercase);
    // Absent → `true`. Applied at the very END of the pipeline
    // (`finish_notes`), never before a filter, so it cannot change which
    // rows come back.
    let include_bodies = q.bodies.unwrap_or(true);

    // Fan out per corpus (invariant #28) — submission order, cap from
    // `[server] fanout_cap`, partial-tolerant: a corpus with a missing or
    // broken `.review/` dir yields an empty Vec rather than 500ing the fleet.
    // Only the STATUS filter runs inside the walk; `?tag=` and `?q=` are
    // applied after the facet counts are taken (see below).
    let mut futs: Vec<super::CorpusFut<'_, CorpusNotes>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        if let Some(w) = want_kb {
            if kb_name.as_str() != w {
                continue;
            }
        }
        let review_dir = state.paths.kb_review_dir(kb_name);
        futs.push(Box::pin(async move {
            collect_notes(kb_name.as_str(), ctx, &review_dir, want_status).await
        }));
    }
    let collected: Vec<ReviewNoteRow> = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect();

    // Facets are computed over the PRE-`?tag=`, PRE-`?q=` set, and BEFORE
    // the row cap. That is the whole point of a facet list: clicking a
    // second tag must NARROW the rows rather than empty them, which it
    // cannot do if the chip row only describes the current selection. The
    // set is "every private note that reached this point" — the operator's
    // own notes, so it is bounded by what a human wrote — and the status
    // filter has ALREADY been applied to it, because it runs inside the
    // per-corpus walk. So `?status=open` describes the open notes, not the
    // fleet. The counts are NOT bounded by the row cap: the facets of rows
    // 501+ are counted even when those rows are not returned.
    let (tags, tags_truncated) = facet_tags(&collected);

    // Row filters, sort, cap, and the `?bodies=false` projection — in that
    // order, and the projection LAST.
    let (notes, total, truncated) =
        finish_notes(collected, &want_tags, needle.as_deref(), include_bodies);

    Json(ReviewNotesResponse {
        notes,
        tags,
        total,
        truncated,
        tags_truncated,
    })
    .into_response()
}

/// `open` | `resolved` | `all`; absent or `all` → no status filter.
fn parse_status(raw: Option<&str>) -> Result<Option<CommentStatus>, kb_core::Error> {
    match raw {
        None | Some("all") => Ok(None),
        Some("open") => Ok(Some(CommentStatus::Open)),
        Some("resolved") => Ok(Some(CommentStatus::Resolved)),
        Some(other) => Err(kb_core::Error::BadRequest(format!(
            "status {other:?} not one of open|resolved|all"
        ))),
    }
}

/// Aggregate the comment-tag facet counts over `rows`, sorted
/// `count DESC, name ASC` and capped at [`MAX_REVIEW_NOTE_TAGS`].
///
/// Takes the row set as it stands — the caller decides WHAT that set is
/// (here: post-status, pre-`?tag=`, pre-`?q=`, pre-cap) — so the facet
/// contract lives with the call site rather than being buried here.
fn facet_tags(rows: &[ReviewNoteRow]) -> (Vec<TagSummary>, bool) {
    let mut counts: HashMap<&str, u32> = HashMap::new();
    for row in rows {
        for t in &row.tags {
            *counts.entry(t.as_str()).or_insert(0) += 1;
        }
    }
    let mut facets: Vec<TagSummary> = counts
        .into_iter()
        .map(|(name, count)| TagSummary {
            name: name.to_string(),
            count,
            color_seed: fnv1a(name),
        })
        .collect();
    // `count DESC, name ASC` — the exact `aggregate_tags` order, so the chip
    // row and the rail's tag list never disagree on ordering.
    facets.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    let truncated = facets.len() > MAX_REVIEW_NOTE_TAGS;
    facets.truncate(MAX_REVIEW_NOTE_TAGS);
    (facets, truncated)
}

/// Everything the route does to the collected rows once the per-corpus
/// join is done: `?tag=`, `?q=`, the sort, the row cap, and finally the
/// `?bodies=false` projection. Returns `(rows, total_before_cap, truncated)`.
///
/// One function because the ORDER of those steps is the contract: the body
/// projection runs LAST so `?q=` — which searches bodies — behaves the same
/// with `bodies=false` as without it, and so the projection provably cannot
/// change which rows are selected.
fn finish_notes(
    mut rows: Vec<ReviewNoteRow>,
    want_tags: &[String],
    needle: Option<&str>,
    include_bodies: bool,
) -> (Vec<ReviewNoteRow>, usize, bool) {
    // `?tag=` is repeatable and AND: a note must carry EVERY listed tag, so
    // adding one can only narrow. `?q=` is a case-insensitive substring of
    // the body, which is still present at this point under
    // `bodies=false`.
    if !want_tags.is_empty() || needle.is_some() {
        rows.retain(|row| {
            want_tags.iter().all(|t| row.tags.iter().any(|ct| ct == t))
                && needle.is_none_or(|n| {
                    row.body
                        .as_deref()
                        .is_some_and(|b| b.to_lowercase().contains(n))
                })
        });
    }

    let total = rows.len();
    // Newest activity first, with inbox's total-order tiebreak so a
    // same-second batch is stable across requests and a test can pin the
    // order.
    rows.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.kb.cmp(&b.kb))
            .then_with(|| a.artifact_id.cmp(&b.artifact_id))
            .then_with(|| a.comment_id.cmp(&b.comment_id))
    });
    let truncated = rows.len() > MAX_REVIEW_NOTES;
    rows.truncate(MAX_REVIEW_NOTES);

    if !include_bodies {
        for r in &mut rows {
            r.body = None;
        }
    }
    (rows, total, truncated)
}

/// Walk one corpus's `.review/` dir and collect its PRIVATE notes.
///
/// Best-effort per invariant #28: a missing/unreadable dir or a malformed
/// review file is skipped, never propagated — this future must NOT
/// `?`-return an error. Cost shape mirrors `inbox::collect_open`: the dir
/// walk, the per-file parse and the stale-anchor sidecar load all happen in
/// ONE `spawn_blocking` off the async worker, and a file that contributes no
/// note never reaches lance.
async fn collect_notes(
    kb: &str,
    ctx: &crate::state::KbContext,
    review_dir: &std::path::Path,
    want_status: Option<CommentStatus>,
) -> CorpusNotes {
    let kb_owned = kb.to_string();
    let review_dir = review_dir.to_path_buf();

    let groups: Vec<(String, Vec<ReviewNoteRow>)> = tokio::task::spawn_blocking(move || {
        let stale_map = kb_core::anchors::load(&kb_core::anchors::sidecar_path(&review_dir));

        // Missing dir (fresh kb) or unreadable dir → no notes, no 500.
        let entries = match std::fs::read_dir(&review_dir) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };
        // Collect + sort the artifact ids for a deterministic within-kb order
        // before the cross-kb sort (mirrors `inbox::collect_open`).
        let mut files: Vec<(String, std::path::PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            files.push((stem.to_string(), path));
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));

        let mut groups: Vec<(String, Vec<ReviewNoteRow>)> = Vec::new();
        for (artifact_id, path) in files {
            // Malformed / non-kb-comments JSON (e.g. the stale-anchor
            // sidecar itself, or a partial write) → skip, never 500.
            let file = match review::load(&path) {
                Ok(Some(f)) => f,
                _ => continue,
            };
            let mut rows: Vec<ReviewNoteRow> = Vec::new();
            // `is_private()` is the ONE predicate, so this list cannot
            // disagree with any other surface about what a note is. It takes
            // no opt-in: the absence of one IS the guarantee.
            for c in file.comments.iter().filter(|c| c.is_private()) {
                if let Some(s) = want_status {
                    if c.status != s {
                        continue;
                    }
                }
                let created = c.created_at.timestamp();
                let mut updated =
                    created.max(c.edited_at.map(|t| t.timestamp()).unwrap_or(created));
                for r in &c.replies {
                    updated = updated.max(r.created_at.timestamp());
                    if let Some(edited) = r.edited_at {
                        updated = updated.max(edited.timestamp());
                    }
                }
                rows.push(ReviewNoteRow {
                    kb: kb_owned.clone(),
                    artifact_id: artifact_id.clone(),
                    artifact_title: file.artifact.title.clone(),
                    source_relative: None,
                    comment_id: c.id.clone(),
                    status: c.status,
                    author: c.author,
                    // Always `Some` here: the row is built from the
                    // sidecar, and the `?bodies=false` projection runs
                    // after every filter, in `finish_notes`.
                    body: Some(c.body.clone()),
                    anchor: c.anchor.clone(),
                    tags: c.tags.clone(),
                    private: c.private,
                    reply_count: c.replies.len() as u32,
                    stale: stale_map.contains_key(&(artifact_id.clone(), c.id.clone())),
                    user: c.user.clone(),
                    created_at: c.created_at,
                    updated_at: updated,
                });
            }
            // No notes → this file contributes nothing; skip the lance
            // lookup entirely.
            if !rows.is_empty() {
                groups.push((artifact_id, rows));
            }
        }
        groups
    })
    .await
    .unwrap_or_default();

    if groups.is_empty() {
        return Vec::new();
    }

    // ONE batched lance query for every surviving artifact's live title +
    // source-rel — the same join `list_reviews` and `collect_open` use. A
    // missing row (the artifact left lance; the sidecar outlived it) keeps
    // the review file's own title and a `None` source_relative, which makes
    // the row non-navigable rather than wrongly navigable.
    let ids: Vec<String> = groups.iter().map(|(id, _)| id.clone()).collect();
    let resolved: HashMap<String, kb_core::storage::lance::DocSummary> =
        match ctx.storage.get_by_ids(ids).await {
            Ok(docs) => docs.into_iter().map(|d| (d.id.clone(), d)).collect(),
            Err(_) => HashMap::new(),
        };

    let mut out: Vec<ReviewNoteRow> = Vec::new();
    for (artifact_id, mut rows) in groups {
        if let Some(doc) = resolved.get(&artifact_id) {
            let rel = kb_core::paths::doc_rel_path(&doc.path, &ctx.source_path);
            for r in &mut rows {
                r.artifact_title = doc.title.clone();
                r.source_relative = Some(rel.clone());
            }
        }
        out.extend(rows);
    }
    out
}

/// Stable FNV-1a of the slug, so the SPA maps a tag name to the same
/// deterministic chip colour it already uses for `kb-tags`. Duplicated from
/// `routes::tags` rather than imported: that module keeps the helper
/// private because it is a presentation detail, and widening a module this
/// unit does not own — for one 4-line hash — is the worse trade.
fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::{
        facet_tags, finish_notes, parse_status, parse_tag_filters, ReviewNoteRow, ReviewNotesQuery,
        MAX_REVIEW_NOTES,
    };
    use chrono::{TimeZone, Utc};
    use kb_core::review::{Anchor, Author, CommentStatus};

    /// One private-note row, `updated_at` fixed per id so the sort order is
    /// predictable (ids sort as "1" < "2" < "3" numerically, which is the
    /// opposite of the reverse-chronological order, so a mis-wired sort shows
    /// up as a mismatch rather than passing by luck).
    fn row(id: &str, body: &str, tags: &[&str]) -> ReviewNoteRow {
        let n: i64 = id.parse().unwrap();
        ReviewNoteRow {
            kb: "kb-a".into(),
            artifact_id: "art-1".into(),
            artifact_title: "Doc".into(),
            source_relative: Some("docs/doc.md".into()),
            comment_id: id.into(),
            status: CommentStatus::Open,
            author: Author::You,
            body: Some(body.into()),
            anchor: Anchor::File,
            tags: tags.iter().map(|t| (*t).to_string()).collect(),
            private: true,
            reply_count: 0,
            stale: false,
            user: None,
            created_at: Utc.timestamp_opt(n, 0).unwrap(),
            updated_at: n,
        }
    }

    /// The three notes every `?bodies` test shares. Note `2` is the only one
    /// whose body contains `secret`, so a body that survived the projection
    /// is unmissable in a payload assert.
    fn fleet() -> Vec<ReviewNoteRow> {
        vec![
            row("1", "public body", &["api"]),
            row("2", "the secret is in here", &["api", "perf"]),
            row("3", "another body", &["docs"]),
        ]
    }

    fn ids(rows: &[ReviewNoteRow]) -> Vec<String> {
        rows.iter().map(|r| r.comment_id.clone()).collect()
    }

    #[test]
    fn bodies_false_omits_the_key_and_the_default_call_keeps_it() {
        let (with, _, _) = finish_notes(fleet(), &[], None, true);
        let (without, _, _) = finish_notes(fleet(), &[], None, false);
        assert_eq!(with.len(), 3);
        assert_eq!(without.len(), 3);
        for (a, b) in with.iter().zip(&without) {
            let ja = serde_json::to_value(a).unwrap();
            let jb = serde_json::to_value(b).unwrap();
            // The default call keeps the text, row by row.
            assert_eq!(ja.get("body").and_then(|v| v.as_str()), a.body.as_deref());
            assert!(a.body.is_some());
            // ABSENT, not "" — an empty string standing in for a withheld
            // body is indistinguishable from a note whose text really is
            // empty.
            assert!(jb.get("body").is_none(), "body key present: {jb}");
        }
    }

    #[test]
    fn bodies_false_selects_exactly_the_same_rows() {
        // No filter, then every filter this route has: a projection that
        // changed selection would show up as a differing id list.
        for (tags, needle) in [
            (vec![], None),
            (vec!["api".to_string(), "perf".to_string()], None),
            (vec![], Some("secret".to_lowercase())),
        ] {
            let (with, total_with, trunc_with) =
                finish_notes(fleet(), &tags, needle.as_deref(), true);
            let (without, total_without, trunc_without) =
                finish_notes(fleet(), &tags, needle.as_deref(), false);
            assert_eq!(ids(&with), ids(&without), "tags={tags:?} q={needle:?}");
            assert_eq!(total_with, total_without);
            assert_eq!(trunc_with, trunc_without);
        }
    }

    #[test]
    fn bodies_false_never_surfaces_a_row_or_body_the_default_call_hides() {
        // The selection is already fixed upstream of the projection (only
        // `c.is_private()` rows are ever built), so the property is: the
        // projection adds nothing. Assert it on the payload, not on the
        // count — an id set that grew, or note text that survived the
        // projection, is the failure this guards.
        let (with, _, _) = finish_notes(fleet(), &[], None, true);
        let (without, _, _) = finish_notes(fleet(), &[], None, false);
        let payload = serde_json::to_string(&without).unwrap();
        assert!(!payload.contains("secret"), "note body leaked: {payload}");
        // Same set, both directions: no id added, none dropped. A widening
        // regression would have to invent a row, and a narrowing one would
        // have to drop one.
        let shown: Vec<&str> = with.iter().map(|r| r.comment_id.as_str()).collect();
        let projected: Vec<&str> = without.iter().map(|r| r.comment_id.as_str()).collect();
        assert_eq!(shown, projected);
        for w in &with {
            assert!(without.iter().any(|r| r.comment_id == w.comment_id));
        }
        // Every row stays self-describing as a private note: dropping the
        // text must not turn a note into something that reads as public.
        for r in &without {
            assert!(r.private);
            assert_eq!(r.body, None);
        }
    }

    #[test]
    fn q_still_filters_the_body_when_bodies_are_omitted() {
        // The projection runs AFTER the filters; if it ever moved before
        // them, `?q=` would silently degrade to "match everything".
        let (rows, _, _) = finish_notes(fleet(), &[], Some("secret"), false);
        assert_eq!(ids(&rows), vec!["2".to_string()]);
        assert!(rows[0].body.is_none());
    }

    #[test]
    fn bodies_defaults_to_true_when_the_query_omits_it() {
        let q: ReviewNotesQuery = serde_json::from_value(serde_json::json!({
            "kb": "kb-a",
        }))
        .unwrap();
        assert_eq!(q.bodies, None);
        assert!(q.bodies.unwrap_or(true));
        let q: ReviewNotesQuery =
            serde_json::from_value(serde_json::json!({ "bodies": false })).unwrap();
        assert!(!q.bodies.unwrap_or(true));
    }

    #[test]
    fn a_tag_filter_that_slugifies_to_empty_is_rejected() {
        for bad in ["", "!!!", "   ", "---", "///"] {
            let err = parse_tag_filters(&format!("tag={bad}"))
                .expect_err("empty slug must be a 400, not a filter that matches everything");
            assert!(
                matches!(err, kb_core::Error::BadRequest(_)),
                "{bad:?} produced {err:?}"
            );
        }
        // Still rejected when it arrives alongside a usable tag — one bad
        // value fails the whole request rather than being dropped.
        assert!(parse_tag_filters("tag=api&tag=%21%21%21").is_err());
    }

    /// WHY `?tag=` is parsed from the RAW query rather than off the derived
    /// struct: a repeated key is inexpressible there, and both obvious
    /// workarounds answer 400 — a lone value against `Vec<String>` is
    /// "expected a sequence", a repeated one against a `deserialize_with`
    /// field is "duplicate field". The grammar promises `?tag=a&tag=b`, so
    /// this pins that the promise holds, that a SINGLE `?tag=a` is not the
    /// special case it was in between, and that values are percent-decoded.
    #[test]
    fn a_repeated_tag_key_is_accepted_and_percent_decoded() {
        assert_eq!(parse_tag_filters("").unwrap(), Vec::<String>::new());
        assert_eq!(parse_tag_filters("kb=smoke").unwrap(), Vec::<String>::new());
        assert_eq!(parse_tag_filters("tag=api").unwrap(), ["api"]);
        assert_eq!(
            parse_tag_filters("tag=api&tag=perf").unwrap(),
            ["api", "perf"]
        );
        // Order is preserved, so the AND is applied in the caller's order.
        assert_eq!(
            parse_tag_filters("tag=one&tag=two&tag=one").unwrap(),
            ["one", "two", "one"]
        );
        assert_eq!(parse_tag_filters("tag=Tag%20One").unwrap(), ["tag-one"]);
        // A different key is not a tag.
        assert_eq!(parse_tag_filters("q=api").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn tag_filters_are_repeatable_and_anded_without_the_write_paths_limits() {
        // Slugified with the bare normaliser, so a filter accepts what a
        // write would refuse: >8 values, an over-long value, and duplicates.
        let long = "x".repeat(60);
        let mut raw: Vec<String> = (0..12).map(|i| format!("tag{i}")).collect();
        raw.push(long.clone());
        raw.push("Tag One".to_string());
        // `api` twice: the write path's dedupe has no business here.
        raw.push("api".to_string());
        raw.push("api".to_string());
        let query = raw
            .iter()
            .map(|t| format!("tag={t}"))
            .collect::<Vec<_>>()
            .join("&");
        let parsed = parse_tag_filters(&query).unwrap();
        assert_eq!(
            parsed.len(),
            raw.len(),
            "a filter is not a write: no cap, no dedupe"
        );
        assert_eq!(parsed[12], long, "over-long filter value is kept whole");
        assert_eq!(parsed[13], "tag-one");

        // AND, not OR: a note must carry EVERY listed tag, so adding one can
        // only narrow.
        let both = parse_tag_filters("tag=api&tag=perf").unwrap();
        let (rows, _, _) = finish_notes(fleet(), &both, None, true);
        assert_eq!(ids(&rows), vec!["2".to_string()]);
        let (none, _, _) = finish_notes(fleet(), &both[..1], None, true);
        // Newest-activity-first, so note 2 leads.
        assert_eq!(ids(&none), vec!["2".to_string(), "1".to_string()]);
    }

    #[test]
    fn status_parses_open_resolved_and_all() {
        assert_eq!(parse_status(None).unwrap(), None);
        assert_eq!(parse_status(Some("all")).unwrap(), None);
        assert_eq!(
            parse_status(Some("open")).unwrap(),
            Some(CommentStatus::Open)
        );
        assert_eq!(
            parse_status(Some("resolved")).unwrap(),
            Some(CommentStatus::Resolved)
        );
        assert!(matches!(
            parse_status(Some("closed")),
            Err(kb_core::Error::BadRequest(_))
        ));
    }

    #[test]
    fn facets_describe_the_pre_tag_pre_q_set_and_ignore_the_row_cap() {
        // The load-bearing half of the facet contract: the counts come from
        // the set as handed over — before `?tag=` / `?q=` narrow it, and
        // before the row cap — so clicking a second tag narrows the list
        // instead of emptying it.
        let (facets, truncated) = facet_tags(&fleet());
        assert!(!truncated);
        let names: Vec<(&str, u32)> = facets.iter().map(|f| (f.name.as_str(), f.count)).collect();
        assert_eq!(names, vec![("api", 2), ("docs", 1), ("perf", 1)]);

        // `?tag=api` selected on its own leaves an EMPTY page, while the
        // facet row — computed by the route BEFORE those filters run — still
        // offers `perf`. That is the property the chip row depends on: a
        // second click narrows instead of emptying the list.
        let want = parse_tag_filters("tag=api").unwrap();
        let (rows, _, _) = finish_notes(fleet(), &want, Some("nothing matches this"), true);
        assert!(rows.is_empty());
        let (pre_filter_facets, _) = facet_tags(&fleet());
        assert!(pre_filter_facets
            .iter()
            .any(|f| f.name == "perf" && f.count == 1));

        // Facet counts are not bounded by the row cap: a note past
        // MAX_REVIEW_NOTES is not returned but its tags are still counted.
        let many: Vec<ReviewNoteRow> = (0..MAX_REVIEW_NOTES + 10)
            .map(|i| row(&i.to_string(), "b", &["bulk"]))
            .collect();
        // Facets are taken from the PRE-cap set, so they are computed before
        // `finish_notes` consumes it.
        let (facets, facets_truncated) = facet_tags(&many);
        let (returned, total, truncated) = finish_notes(many, &[], None, true);
        assert_eq!(returned.len(), MAX_REVIEW_NOTES);
        assert_eq!(total, MAX_REVIEW_NOTES + 10);
        assert!(truncated);
        assert!(!facets_truncated);
        assert_eq!(facets[0].count as usize, MAX_REVIEW_NOTES + 10);
    }
}
