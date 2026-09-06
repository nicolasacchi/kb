//! DCB W1.B — `coderef/1`: the doc-level code-reference route
//! (`GET /api/kb/{kb}/docs/{id}/code-refs`) and the corpus cursor feed
//! (`GET /api/kb/{kb}/code-refs`) kb-code's doc-lens sync pages over.
//!
//! Pure reads over W1.A's `code_refs_docs`/`code_refs` sqlite tables
//! (`kb_core::storage::sqlite::{CodeRefHeaderRow, CodeRefRow, CodeRefDoc}`).
//! kb never resolves a ref against a working tree — it extracts hints only
//! (invariant #2/#4); resolution is kb-code's `codelens/1`, computed live,
//! never persisted here.
//!
//! Wire schema is `11-w1b-kb-server-cli.md` §2 (frozen, R1) — three named
//! ts-rs shapes (R19): [`CodeRefsDocOut`] is the doc-level payload with no
//! `schema`/`kb` envelope; [`CodeRefsResponse`] is the single-doc route's
//! envelope with the doc flattened in; [`CodeRefsFeedResponse`] is the feed
//! route's envelope, carrying `docs: Vec<CodeRefsDocOut>` plus `next_cursor`.

use crate::middleware::error_to_problem_json;
use crate::routes::docs::moves_redirect_or_404;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::Response,
    response::IntoResponse,
    Json,
};
use kb_core::coderefs::{parse_code_rev, CodeRev};
use kb_core::paths::doc_rel_path;
use kb_core::storage::sqlite::{CodeRefDoc, CodeRefRow};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Feed default/max page size (R3 — `?limit=`, clamped `1..=FEED_MAX_LIMIT`).
const FEED_DEFAULT_LIMIT: u32 = 25;
const FEED_MAX_LIMIT: u32 = 100;

// --- Wire types ----------------------------------------------------------

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct CodeRevOut {
    pub label: String,
    pub sha: String,
    pub dirty: bool,
}

impl From<CodeRev> for CodeRevOut {
    fn from(r: CodeRev) -> Self {
        Self {
            label: r.label,
            sha: r.sha,
            dirty: r.dirty,
        }
    }
}

/// A ref-bearing heading. `key` == `anchor` today (kept as separate fields
/// on purpose — the SPA's `kb:scroll-to-id` consumer wants a field named for
/// what it is). See [`build_groups_and_refs`]'s doc comment for how this
/// list is reconstructed from denormalized ref rows.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct CodeRefGroupOut {
    pub ordinal: u32,
    pub key: String,
    pub label: String,
    pub anchor: String,
}

/// One extracted reference. Every nullable field is ALWAYS present on the
/// wire (serializes as JSON `null`, never omitted) — this is a hint record,
/// not an optional-field envelope, so "the doc didn't say" is a fact worth
/// showing, not a key worth dropping (`11-w1b` §2.1's frozen example payload
/// shows every field, several as literal `null`).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct CodeRefOut {
    pub ordinal: u32,
    /// FK into the doc's `groups[]` (by `key`); `null` = before the first
    /// `h2`/`h3` (see the envelope's `ungrouped_count` — there is NO
    /// sentinel `groups[]` entry for this, R9).
    pub group: Option<String>,
    pub kind: String,
    pub raw: String,
    pub path_hint: Option<String>,
    /// For `kind == "issue"`, the issue NUMBER (R11 — there is no
    /// `issue.href` field on this wire; a consumer rebuilds
    /// `https://github.com/{path_hint}/issues/{line_start}` from `path_hint`
    /// + this field. `codelens/1` is where the literal href is minted).
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    pub line_spans: Option<String>,
    pub symbol_container: Option<String>,
    pub symbol_member: Option<String>,
    pub context: String,
    pub context_tokens: Vec<String>,
    pub declared: bool,
}

/// The doc-level payload, with NO `schema`/`kb` envelope fields — this is
/// what the feed's `docs[]` array holds (repeating `schema`/`kb` once per
/// row would be redundant with the envelope those already live on, R19).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct CodeRefsDocOut {
    pub doc_id: String,
    pub doc_path: String,
    pub title: String,
    /// Hash of the source AT THE LAST EXTRACTION (R14) — not necessarily the
    /// doc's current bytes. `null` iff `never_scanned`.
    pub doc_hash: Option<String>,
    /// Wall clock at the write that most recently changed the extraction
    /// (R4). `null` iff `never_scanned`.
    pub extracted_at: Option<i64>,
    /// True when this doc has no `code_refs_docs` row at all. As of
    /// DCB-W1.B.R this means exactly ONE of two things: indexed before DCB
    /// (never enriched at all), or skipped by `CodeRefHook::interested`
    /// (memory-session transcripts — the ONLY exclusion left there). It does
    /// NOT mean "no code signal in the doc" — a prose-only doc (no `<code`,
    /// no GitHub link) still gets a header row (zero refs, `never_scanned ==
    /// false`); the cheap substring pre-gate that used to leave such docs
    /// rowless now lives inside the hook's `enrich()` and records an empty
    /// extraction instead of skipping the row. `kb reindex` genuinely fixes
    /// this flag now — before DCB-W1.B.R it was a permanent, unfixable-by-
    /// reindex `true` for any prose-only doc. The SPA renders "not scanned
    /// yet — reindex to populate", NEVER "no code refs"; `kb refs --lint`
    /// counts it separately. Distinguishing this from a real zero-ref scan
    /// is the whole reason `code_refs_docs` exists (`10-w1a` §14.8).
    pub never_scanned: bool,
    pub code_rev: Option<CodeRevOut>,
    pub ref_count: u32,
    pub ungrouped_count: u32,
    pub truncated: bool,
    pub groups: Vec<CodeRefGroupOut>,
    pub refs: Vec<CodeRefOut>,
}

/// The single-doc route's actual response type: `schema` + `kb` at the top
/// level, the doc fields flattened in beside them (so the wire shape is
/// exactly one flat JSON object, not a nested `doc: {...}`).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct CodeRefsResponse {
    pub schema: &'static str,
    pub kb: String,
    #[serde(flatten)]
    pub doc: CodeRefsDocOut,
}

/// The corpus cursor feed's response.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct CodeRefsFeedResponse {
    pub schema: &'static str,
    pub kb: String,
    pub docs: Vec<CodeRefsDocOut>,
    /// `SessionsListResponse` convention (`routes/sessions.rs:318-331`):
    /// omitted (not `null`) when the page came back shorter than `limit` —
    /// there is no next page.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct FeedParams {
    /// Opaque, verbatim the previous page's `next_cursor` — never
    /// hand-constructed by a client. `"<extracted_at>:<artifact_id>"` (R3).
    pub cursor: Option<String>,
    /// Clamped `1..=FEED_MAX_LIMIT`; default `FEED_DEFAULT_LIMIT`.
    pub limit: Option<u32>,
    /// `refs=0` ⇒ headers only (`refs: []` on every doc, counts intact).
    /// Any other value / absent ⇒ full bodies. NOT a `bool` — the wire is
    /// `?refs=0`, matching the plan's own spelling.
    ///
    /// Non-numeric values 400 (the `Query<FeedParams>` extractor rejects
    /// them before the handler runs — there is no permissive fallback here).
    ///
    /// `refs=0` ALSO empties `groups: []` on every doc, not just `refs: []`
    /// — `groups[]` is reconstructed FROM the ref rows
    /// (`build_groups_and_refs`), and headers-only mode fetches no rows
    /// (`code_refs_feed`'s `with_refs = false` returns `refs: Vec::new()`
    /// per doc), so there is nothing to reconstruct groups from. A consumer
    /// wanting group labels must fetch full bodies.
    pub refs: Option<u8>,
    /// CT-B3 — reverse lookup: "every doc citing path P" (exact `path_hint`
    /// match). When present this BYPASSES `cursor`/`limit` entirely — it is
    /// a complete resolve-to-id-set query (typically a handful of docs),
    /// not the corpus sync walk — and the response always omits
    /// `next_cursor` (there is no next page; every match came back in this
    /// one body). Additive: absent behaves byte-identically to pre-CT-B3.
    pub by_target: Option<String>,
}

// --- Wire builders ---------------------------------------------------------

/// Reconstruct `groups[]` from a doc's ref rows.
///
/// **Heading slugs are NOT guaranteed unique** — `kb_core::headings::heading_id`
/// deliberately doesn't dedupe, so two `## Findings` headings in the same doc
/// share a `group_key`. This builder does NOT dedupe by `key` alone (that
/// would silently merge two distinct headings and could pick the wrong
/// `label` for the second one's refs). Instead it walks refs in row
/// (document) order and emits one `groups[]` entry per FIRST APPEARANCE of
/// the `(key, label, anchor)` TRIPLE — so a genuine repeat heading (same
/// key+label+anchor) collapses into one entry, while a same-key-different-
/// label collision (rarer, but the slugifier permits it) gets its own entry.
/// Either way, `ref.group` equalling >=1 `groups[].key` (the documented FK,
/// not a uniqueness guarantee) always holds.
///
/// **Never assert `groups.len() == header.group_count`** — `group_count` is
/// the RAW heading count recorded at extraction time (`10-w1a`), which can
/// exceed this reconstruction when headings collide, or when `with_refs =
/// false` (feed headers-only mode) leaves no rows to reconstruct groups
/// from at all. `groups.len()` is *derived*, `group_count` is *stored*; they
/// are related, not equal.
fn build_groups_and_refs(rows: &[CodeRefRow]) -> (Vec<CodeRefGroupOut>, Vec<CodeRefOut>) {
    let mut groups: Vec<CodeRefGroupOut> = Vec::new();
    let mut seen: std::collections::HashSet<(String, String, String)> =
        std::collections::HashSet::new();
    for r in rows {
        if let Some(key) = &r.group_key {
            let label = r.group_label.clone().unwrap_or_default();
            let anchor = r.group_anchor.clone().unwrap_or_default();
            let triple = (key.clone(), label.clone(), anchor.clone());
            if seen.insert(triple) {
                groups.push(CodeRefGroupOut {
                    ordinal: groups.len() as u32,
                    key: key.clone(),
                    label,
                    anchor,
                });
            }
        }
    }
    let refs = rows.iter().map(ref_to_wire).collect();
    (groups, refs)
}

fn ref_to_wire(r: &CodeRefRow) -> CodeRefOut {
    CodeRefOut {
        ordinal: r.ordinal,
        group: r.group_key.clone(),
        kind: r.kind.clone(),
        raw: r.raw_text.clone(),
        path_hint: r.path_hint.clone(),
        line_start: r.line_start,
        line_end: r.line_end,
        line_spans: r.line_spans.clone(),
        symbol_container: r.symbol_container.clone(),
        symbol_member: r.symbol_member.clone(),
        context: r.context.clone(),
        // Stored space-joined; split on write, join on read (the route owns
        // the conversion) — `split_whitespace` also makes an empty stored
        // string collapse to an empty Vec rather than `[""]`.
        context_tokens: r
            .context_tokens
            .split_whitespace()
            .map(str::to_string)
            .collect(),
        declared: r.declared,
    }
}

/// Build the doc-level wire payload. `doc = None` ⇒ `never_scanned` (no
/// `code_refs_docs` row at all); `title`/`doc_path` are `""` in the
/// "impossible in practice" case where a `code_refs` row survives without a
/// matching doc row (the cascade prevents this, but the route stays honest
/// rather than panicking or 500ing).
fn doc_out(id: &str, doc_path: String, title: String, doc: Option<CodeRefDoc>) -> CodeRefsDocOut {
    match doc {
        None => CodeRefsDocOut {
            doc_id: id.to_string(),
            doc_path,
            title,
            doc_hash: None,
            extracted_at: None,
            never_scanned: true,
            code_rev: None,
            ref_count: 0,
            ungrouped_count: 0,
            truncated: false,
            groups: Vec::new(),
            refs: Vec::new(),
        },
        Some(doc) => {
            let (groups, refs) = build_groups_and_refs(&doc.refs);
            CodeRefsDocOut {
                doc_id: id.to_string(),
                doc_path,
                title,
                doc_hash: Some(doc.header.doc_hash),
                extracted_at: Some(doc.header.extracted_at),
                never_scanned: false,
                code_rev: doc
                    .header
                    .code_rev
                    .as_deref()
                    .and_then(parse_code_rev)
                    .map(CodeRevOut::from),
                ref_count: doc.header.ref_count,
                ungrouped_count: doc.header.ungrouped_count,
                truncated: doc.header.truncated,
                groups,
                refs,
            }
        }
    }
}

// --- Handlers ----------------------------------------------------------

/// `GET /api/kb/{kb}/docs/{id}/code-refs` — one doc's `coderef/1` payload.
pub async fn for_doc(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(r) => return r,
    };
    if !crate::routes::is_safe_id(&id) {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "invalid doc id {id:?}"
        )));
    }

    let code_refs = match ctx.storage.code_refs_of(id.clone()).await {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };
    let doc_row = match ctx.storage.get_by_id(id.clone()).await {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };

    // Doc unknown to BOTH the docs table and code_refs (the cascade keeps
    // these in sync in practice) — try the moves chain, else 404.
    if doc_row.is_none() && code_refs.is_none() {
        return moves_redirect_or_404(ctx, &kb, &id, "/code-refs").await;
    }

    let (title, doc_path) = match &doc_row {
        Some(row) => (row.title.clone(), doc_rel_path(&row.path, &ctx.source_path)),
        None => (String::new(), String::new()),
    };

    let doc = doc_out(&id, doc_path, title, code_refs);
    #[cfg(debug_assertions)]
    {
        let keys: std::collections::HashSet<&str> =
            doc.groups.iter().map(|g| g.key.as_str()).collect();
        for r in &doc.refs {
            if let Some(g) = &r.group {
                debug_assert!(
                    keys.contains(g.as_str()),
                    "ref.group {g:?} must equal some groups[].key"
                );
            }
        }
    }
    Json(CodeRefsResponse {
        schema: "coderef/1",
        kb: kb.clone(),
        doc,
    })
    .into_response()
}

/// `GET /api/kb/{kb}/code-refs?cursor=&limit=&refs=0` — the corpus cursor
/// feed, ASCENDING keyset over `(extracted_at, artifact_id)`. What kb-code's
/// doc-lens sync pages over.
///
/// **Same-second keyset gap**: `extracted_at` is second-granularity
/// wall-clock; the ordering tiebreak is `artifact_id`. A doc whose
/// extraction changes again in the SAME second a consumer's cursor already
/// paged past it can be skipped on the next pull (its new row sorts before
/// the cursor if its artifact_id is lexically smaller). Consumers that need
/// to be robust to this SHOULD resume one second earlier than their stored
/// watermark (`extracted_at - 1`) rather than trust second-granularity
/// exactness — W3's sync does this.
pub async fn feed(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Query(params): Query<FeedParams>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(r) => return r,
    };

    // CT-B3 — `?by_target=` is a different query shape entirely (a complete
    // reverse resolution, not a cursor page), so it's handled and returned
    // before any of the cursor/limit parsing below even runs.
    if let Some(target) = &params.by_target {
        let with_refs = params.refs != Some(0);
        let rows = match ctx
            .storage
            .code_refs_by_target(target.clone(), with_refs)
            .await
        {
            Ok(v) => v,
            Err(e) => return error_to_problem_json(&e),
        };
        let ids: Vec<String> = rows.iter().map(|d| d.header.artifact_id.clone()).collect();
        let summaries = match ctx.storage.get_by_ids(ids).await {
            Ok(v) => v,
            Err(e) => return error_to_problem_json(&e),
        };
        let by_id: HashMap<String, kb_core::storage::lance::DocSummary> =
            summaries.into_iter().map(|s| (s.id.clone(), s)).collect();
        let docs: Vec<CodeRefsDocOut> = rows
            .into_iter()
            .map(|d| {
                let id = d.header.artifact_id.clone();
                let (title, doc_path) = match by_id.get(&id) {
                    Some(row) => (row.title.clone(), doc_rel_path(&row.path, &ctx.source_path)),
                    None => (String::new(), String::new()),
                };
                doc_out(&id, doc_path, title, Some(d))
            })
            .collect();
        return Json(CodeRefsFeedResponse {
            schema: "coderef-feed/1",
            kb: kb.clone(),
            docs,
            next_cursor: None,
        })
        .into_response();
    }

    // An explicitly-present `?cursor=` (including the EMPTY string) always
    // goes through `parse_feed_cursor`, which 400s it — only a genuinely
    // ABSENT `cursor` param means "no cursor, start from page 1". Silently
    // treating `?cursor=` as absent would be exactly the "reset to page 1
    // without saying so" honesty bug this route's error handling exists to
    // avoid (see `parse_feed_cursor`'s own doc comment).
    let after = match &params.cursor {
        Some(raw) => match parse_feed_cursor(raw) {
            Ok(v) => Some(v),
            Err(r) => return r,
        },
        None => None,
    };
    let limit = params
        .limit
        .unwrap_or(FEED_DEFAULT_LIMIT)
        .clamp(1, FEED_MAX_LIMIT);
    let with_refs = params.refs != Some(0);

    let rows = match ctx.storage.code_refs_feed(after, limit, with_refs).await {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };

    let next_cursor = if rows.len() as u32 == limit {
        rows.last()
            .map(|d| format!("{}:{}", d.header.extracted_at, d.header.artifact_id))
    } else {
        None
    };

    let ids: Vec<String> = rows.iter().map(|d| d.header.artifact_id.clone()).collect();
    let summaries = match ctx.storage.get_by_ids(ids).await {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };
    let by_id: HashMap<String, kb_core::storage::lance::DocSummary> =
        summaries.into_iter().map(|s| (s.id.clone(), s)).collect();

    let docs: Vec<CodeRefsDocOut> = rows
        .into_iter()
        .map(|d| {
            let id = d.header.artifact_id.clone();
            let (title, doc_path) = match by_id.get(&id) {
                Some(row) => (row.title.clone(), doc_rel_path(&row.path, &ctx.source_path)),
                None => (String::new(), String::new()),
            };
            doc_out(&id, doc_path, title, Some(d))
        })
        .collect();

    Json(CodeRefsFeedResponse {
        schema: "coderef-feed/1",
        kb: kb.clone(),
        docs,
        next_cursor,
    })
    .into_response()
}

/// Splits on the first `:`; the left half must parse as `i64`, the right
/// half must pass `is_safe_id`. A structurally invalid cursor (no `:`,
/// non-numeric prefix, unsafe id half) is a 400 — NEVER silently treated as
/// "start from the beginning" (that would silently re-walk the whole corpus
/// into a sync consumer that thought it was resuming).
///
/// The `Err` variant is a fully-built `Response` (the `resolve_kb` precedent,
/// `routes/mod.rs`) so the caller can `return` it directly; accepted over
/// boxing since the error path is the rare malformed-cursor case.
#[allow(clippy::result_large_err)]
fn parse_feed_cursor(raw: &str) -> Result<(i64, String), Response<Body>> {
    let Some((ts_str, id)) = raw.split_once(':') else {
        return Err(error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "invalid_cursor: {raw:?} (expected \"<extracted_at>:<artifact_id>\")"
        ))));
    };
    let Ok(ts) = ts_str.parse::<i64>() else {
        return Err(error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "invalid_cursor: non-numeric extracted_at in {raw:?}"
        ))));
    };
    if !crate::routes::is_safe_id(id) {
        return Err(error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "invalid_cursor: unsafe artifact id in {raw:?}"
        ))));
    }
    Ok((ts, id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(ordinal: u32, group: Option<&str>, tokens: &str) -> CodeRefRow {
        CodeRefRow {
            ordinal,
            kind: "path".into(),
            raw_text: format!("f{ordinal}.rb"),
            path_hint: Some(format!("f{ordinal}.rb")),
            line_start: None,
            line_end: None,
            line_spans: None,
            symbol_container: None,
            symbol_member: None,
            context: String::new(),
            context_tokens: tokens.to_string(),
            group_key: group.map(String::from),
            group_label: group.map(|g| format!("Heading {g}")),
            group_anchor: group.map(String::from),
            declared: false,
        }
    }

    #[test]
    fn groups_built_by_first_appearance_not_dedup_by_key_alone() {
        // Two rows share group_key "g1" with the SAME label/anchor (a
        // genuine repeat heading) — collapse into one groups[] entry.
        let rows = vec![
            row(0, Some("g1"), ""),
            row(1, Some("g1"), ""),
            row(2, None, ""),
        ];
        let (groups, refs) = build_groups_and_refs(&rows);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].key, "g1");
        assert_eq!(groups[0].ordinal, 0);
        assert_eq!(refs.len(), 3);
        assert_eq!(refs[2].group, None);
    }

    /// The documented SPLIT case: two rows share `group_key` "g1" but carry
    /// DIFFERENT labels (a genuine heading-slug collision, e.g. two `##
    /// Findings` headings that `heading_id` didn't dedupe). These must NOT
    /// collapse into one `groups[]` entry — that would silently pick the
    /// first ref's label for the second ref's heading. Both entries share
    /// `key == "g1"`; `ref.group == groups[].key` still holds for every ref
    /// (the FK is by key, not by row identity).
    #[test]
    fn groups_split_on_same_key_different_label() {
        let mut r0 = row(0, Some("g1"), "");
        r0.group_label = Some("Findings".into());
        let mut r1 = row(1, Some("g1"), "");
        r1.group_label = Some("Findings (again)".into());
        let rows = vec![r0, r1];
        let (groups, refs) = build_groups_and_refs(&rows);
        assert_eq!(
            groups.len(),
            2,
            "a same-key/different-label collision must SPLIT, not collapse"
        );
        assert_eq!(groups[0].key, "g1");
        assert_eq!(groups[1].key, "g1");
        assert_eq!(groups[0].label, "Findings");
        assert_eq!(groups[1].label, "Findings (again)");
        let group_keys: std::collections::HashSet<&str> =
            groups.iter().map(|g| g.key.as_str()).collect();
        for r in &refs {
            if let Some(g) = &r.group {
                assert!(
                    group_keys.contains(g.as_str()),
                    "ref.group must equal SOME groups[].key (FK by key, not by row)"
                );
            }
        }
    }

    #[test]
    fn groups_ordinal_is_dense_and_zero_based() {
        let rows = vec![
            row(0, Some("a"), ""),
            row(1, Some("b"), ""),
            row(2, Some("a"), ""),
        ];
        let (groups, _) = build_groups_and_refs(&rows);
        assert_eq!(
            groups.iter().map(|g| g.ordinal).collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[test]
    fn context_tokens_split_on_read() {
        let rows = vec![row(0, None, "foo bar baz")];
        let (_, refs) = build_groups_and_refs(&rows);
        assert_eq!(refs[0].context_tokens, vec!["foo", "bar", "baz"]);
    }

    #[test]
    fn context_tokens_empty_string_yields_empty_vec() {
        let rows = vec![row(0, None, "")];
        let (_, refs) = build_groups_and_refs(&rows);
        assert!(refs[0].context_tokens.is_empty());
    }

    #[test]
    fn parse_feed_cursor_round_trips() {
        let (ts, id) = parse_feed_cursor("1754563200:9f8b7182d433").unwrap();
        assert_eq!(ts, 1_754_563_200);
        assert_eq!(id, "9f8b7182d433");
    }

    #[test]
    fn parse_feed_cursor_rejects_malformed() {
        assert!(parse_feed_cursor("notanumber").is_err());
        assert!(parse_feed_cursor("").is_err());
        assert!(parse_feed_cursor("123").is_err());
        assert!(parse_feed_cursor("123:../etc").is_err());
        assert!(parse_feed_cursor("123:a/b").is_err());
    }
}
