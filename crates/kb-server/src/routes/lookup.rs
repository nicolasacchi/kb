//! `GET /api/kb/{kb}/lookup?q=<input>` — resolve a user-supplied
//! identifier (12-hex artifact id, source-relative path, unique
//! filename/suffix, or a ticket/slug token) to a single artifact's
//! metadata.
//!
//! Drives the `kb find` CLI subcommand and underlies the `--path` flag
//! on `kb comments {list,export,resolve,add}`. The point is to let
//! Claude Code (and humans) talk about artifacts by filename, ticket
//! number, or slug instead of memorising 12-hex hashes.
//!
//! Resolution ladder, first hit wins. Arms 1–4 stay first and unchanged;
//! arm 5 only fills a miss they did not already answer.
//!   1. `q` looks like a 12-hex id AND `get_by_id` returns Some → Exact
//!   2. `q` treated as source-relative path AND `get_by_source_path`
//!      (against `source_root.join(q)`) returns Some → Exact
//!   3. F3b — moves-table fallback (old_id OR old_rel → new id chain;
//!      newest-wins). Resolved doc under the new id → Exact
//!   4. Filename / suffix match against `list_docs(200)`:
//!        - 1 hit  → UniqueSuffix
//!        - 2..=10 → Ambiguous { candidates }
//!        - 11+    → Ambiguous { candidates: first 10, truncated: true }
//!        - 0      → fall through to arm 5
//!   5. Identifier token, closed patterns (not config):
//!        ticket `^#?\d{3,}$` (`15715`, `#15715`) or a slug of ASCII
//!        letters/digits/hyphens, length ≥ 6. Matched against title and
//!        filename stem.
//!        - 1 hit → UniqueSuffix
//!        - 0 or >1 → NotFound. Never Ambiguous and never 404, so a
//!          non-unique token does not hard-miss a query ranked search
//!          would still answer.
//!
//! No write surface; pure read. Inherits the api tree's bearer auth
//! and origin allowlist.

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{Response, StatusCode},
    response::IntoResponse,
    Json,
};
use kb_core::paths::{doc_folder, doc_rel_path};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const LIST_CAP: u32 = 200;
const CANDIDATES_CAP: usize = 10;
const Q_MAX_LEN: usize = 256;

#[derive(Debug, Deserialize)]
pub struct LookupParams {
    pub q: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct LookupHit {
    pub id: String,
    /// Absolute on-disk path (what the indexer stored).
    pub path: String,
    /// Path relative to the kb's source root, forward-slash separated.
    /// Empty string is impossible for an indexed doc; defensive None
    /// path returns an empty string when the canonicalisation in
    /// `doc_rel_path` fails.
    pub source_relative: String,
    pub folder: String,
    pub title: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum LookupResult {
    Exact(LookupHit),
    UniqueSuffix(LookupHit),
    Ambiguous {
        candidates: Vec<LookupHit>,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        truncated: bool,
    },
    NotFound,
}

pub async fn lookup(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Query(params): Query<LookupParams>,
) -> Response<Body> {
    let q = params.q.trim();
    if q.is_empty() {
        return (StatusCode::BAD_REQUEST, "q must be non-empty").into_response();
    }
    if q.len() > Q_MAX_LEN {
        return (StatusCode::BAD_REQUEST, "q too long").into_response();
    }

    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let source_root = ctx.source_path.clone();

    // 1. 12-hex id passthrough.
    if is_id_shape(q) {
        match ctx.storage.get_by_id(q.to_string()).await {
            Ok(Some(row)) => {
                return Json(LookupResult::Exact(hit_from_row(&row, &source_root))).into_response();
            }
            Ok(None) => { /* fall through */ }
            Err(e) => return error_to_problem_json(&e),
        }
    }

    // 2. Treat q as source-relative path; compose absolute and look up.
    //    The indexer stored absolute paths; the storage helper does
    //    `path = '<absolute>'` filtering. We try both raw and
    //    canonicalised forms so a kb mounted under a symlink still
    //    resolves.
    let q_norm = q.trim_start_matches('/').replace('\\', "/");
    let absolute = source_root.join(&q_norm);
    let abs_str = absolute.to_string_lossy().to_string();
    match ctx.storage.get_by_source_path(abs_str.clone()).await {
        Ok(Some(row)) => {
            return Json(LookupResult::Exact(hit_from_row(&row, &source_root))).into_response();
        }
        Ok(None) => { /* fall through */ }
        Err(e) => return error_to_problem_json(&e),
    }
    // Second try: canonicalised absolute (resolves symlinks).
    if let Ok(canon) = absolute.canonicalize() {
        let canon_str = canon.to_string_lossy().to_string();
        if canon_str != abs_str {
            match ctx.storage.get_by_source_path(canon_str).await {
                Ok(Some(row)) => {
                    return Json(LookupResult::Exact(hit_from_row(&row, &source_root)))
                        .into_response();
                }
                Ok(None) => { /* fall through */ }
                Err(e) => return error_to_problem_json(&e),
            }
        }
    }

    // 3. F3b — moves-table fallback (old id and/or old source-rel). Chain
    // resolution is newest-wins; return the live doc under the new id as
    // Exact so existing CLI consumers (`kb find`, --path resolvers) keep
    // working without learning a new kind.
    match kb_core::relocate::moves_lookup(&ctx.storage, q).await {
        Ok(Some((new_id, _new_rel))) => {
            match ctx.storage.get_by_id(new_id).await {
                Ok(Some(row)) => {
                    return Json(LookupResult::Exact(hit_from_row(&row, &source_root)))
                        .into_response();
                }
                Ok(None) => { /* fall through */ }
                Err(e) => return error_to_problem_json(&e),
            }
        }
        Ok(None) => { /* fall through */ }
        Err(e) => return error_to_problem_json(&e),
    }
    // Also try the normalised source-rel form when `q` was path-shaped
    // but the id-shape branch above already skipped moves.
    if !is_id_shape(q) && q_norm != q {
        match kb_core::relocate::moves_lookup(&ctx.storage, &q_norm).await {
            Ok(Some((new_id, _new_rel))) => {
                match ctx.storage.get_by_id(new_id).await {
                    Ok(Some(row)) => {
                        return Json(LookupResult::Exact(hit_from_row(&row, &source_root)))
                            .into_response();
                    }
                    Ok(None) => { /* fall through */ }
                    Err(e) => return error_to_problem_json(&e),
                }
            }
            Ok(None) => { /* fall through */ }
            Err(e) => return error_to_problem_json(&e),
        }
    }

    // 4. Suffix match against the full doc list. Cheap — 200-row cap.
    let rows = match ctx.storage.list_docs(LIST_CAP).await {
        Ok(r) => r,
        Err(e) => return error_to_problem_json(&e),
    };

    let mut candidates: Vec<LookupHit> = rows
        .iter()
        .filter(|row| path_matches_suffix(&row.path, &q_norm))
        .map(|row| hit_from_row(row, &source_root))
        .collect();

    // 5. Identifier token. Arm 4 stays first: a suffix hit is not
    // overridden. On a suffix miss, exactly one title/stem hit is pushed
    // so the match below resolves it. Zero or many fall through to
    // NotFound — not Ambiguous, not 404.
    if candidates.is_empty() {
        if let Some(row) = resolve_identifier_token(q, &rows) {
            candidates.push(hit_from_row(row, &source_root));
        }
    }

    let result = match candidates.len() {
        0 => LookupResult::NotFound,
        1 => LookupResult::UniqueSuffix(candidates.pop().expect("len==1")),
        n => {
            let truncated = n > CANDIDATES_CAP;
            candidates.truncate(CANDIDATES_CAP);
            LookupResult::Ambiguous {
                candidates,
                truncated,
            }
        }
    };
    Json(result).into_response()
}

fn is_id_shape(q: &str) -> bool {
    q.len() == 12
        && q.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// Does `row_path` end with `q` such that `q` is either:
/// - the entire path, or
/// - everything from after a `/` to the end.
///
/// Avoids matching `subatlas.html` for `q=atlas.html`. If `q` itself
/// contains a `/`, must match a path-segment-aligned suffix.
fn path_matches_suffix(row_path: &str, q: &str) -> bool {
    if q.is_empty() {
        return false;
    }
    if row_path == q {
        return true;
    }
    // Allow either / or \ on the segment-boundary side so this works on
    // Windows paths if they ever appear (kb is Linux-only today; cheap
    // defense). The indexer normalises to `/` for stored paths via
    // `doc_rel_path`, but `row.path` is the absolute on-disk form.
    let needle = format!("/{q}");
    let alt = format!("\\{q}");
    row_path.ends_with(&needle) || row_path.ends_with(&alt)
}

fn hit_from_row(
    row: &kb_core::storage::lance::DocSummary,
    source_root: &std::path::Path,
) -> LookupHit {
    LookupHit {
        id: row.id.clone(),
        path: row.path.clone(),
        source_relative: doc_rel_path(&row.path, source_root),
        folder: doc_folder(&row.path, source_root),
        title: row.title.clone(),
    }
}

/// Fifth-arm token. Ticket needle is the digit run with at most one
/// leading `#` stripped. Slug needle is `q` itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdentifierToken<'a> {
    Ticket(&'a str),
    Slug(&'a str),
}

/// `^#?\d{3,}$` — optional single `#`, then three or more ASCII digits.
fn ticket_digits(q: &str) -> Option<&str> {
    let digits = q.strip_prefix('#').unwrap_or(q);
    if digits.len() >= 3 && digits.bytes().all(|b| b.is_ascii_digit()) {
        Some(digits)
    } else {
        None
    }
}

/// Slug of ASCII letters, digits, and hyphens, length ≥ 6. A pure digit
/// run is a ticket, not a slug. All-hyphen is not an identifier.
fn is_slug_token(q: &str) -> bool {
    let bytes = q.as_bytes();
    bytes.len() >= 6
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
        && bytes.iter().any(|b| b.is_ascii_alphanumeric())
        && bytes.iter().any(|b| !b.is_ascii_digit())
}

fn classify_identifier_token(q: &str) -> Option<IdentifierToken<'_>> {
    if let Some(digits) = ticket_digits(q) {
        return Some(IdentifierToken::Ticket(digits));
    }
    if is_slug_token(q) {
        return Some(IdentifierToken::Slug(q));
    }
    None
}

/// Last path segment with the final extension stripped, matching
/// `Path::file_stem`. Leading-dot names (`.gitignore`) keep the dot.
fn filename_stem(path: &str) -> &str {
    let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match base.rfind('.') {
        Some(i) if i > 0 => &base[..i],
        _ => base,
    }
}

fn is_token_boundary(b: u8) -> bool {
    !b.is_ascii_alphanumeric()
}

/// `needle` occurs in `haystack` bounded by start/end or a non-alphanumeric
/// byte (hyphen, `#`, space, …). Case-insensitive. `15715` matches
/// `#15715` and `15715-login`, not `157150`.
fn token_bounded(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    let last = h.len() - n.len();
    let mut i = 0;
    while i <= last {
        let end = i + n.len();
        if h[i..end].eq_ignore_ascii_case(n)
            && (i == 0 || is_token_boundary(h[i - 1]))
            && (end == h.len() || is_token_boundary(h[end]))
        {
            return true;
        }
        i += 1;
    }
    false
}

fn row_matches_identifier(title: &str, path: &str, token: IdentifierToken<'_>) -> bool {
    let needle = match token {
        IdentifierToken::Ticket(digits) | IdentifierToken::Slug(digits) => digits,
    };
    token_bounded(title, needle) || token_bounded(filename_stem(path), needle)
}

/// Unique title/stem hit for an identifier token. `None` when `q` is not
/// a token, or the token matches 0 or >1 rows — those fall through.
fn resolve_identifier_token<'a>(
    q: &str,
    rows: &'a [kb_core::storage::lance::DocSummary],
) -> Option<&'a kb_core::storage::lance::DocSummary> {
    let token = classify_identifier_token(q)?;
    let mut found = None;
    for row in rows {
        if row_matches_identifier(&row.title, &row.path, token) {
            if found.is_some() {
                return None;
            }
            found = Some(row);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_id_shape_accepts_12_lowercase_hex() {
        assert!(is_id_shape("abc123def456"));
        assert!(is_id_shape("000000000000"));
        assert!(is_id_shape("0a1b2c3d4e5f"));
    }

    #[test]
    fn is_id_shape_rejects_other_shapes() {
        assert!(!is_id_shape("abc123")); // too short
        assert!(!is_id_shape("abc123def4567")); // too long
        assert!(!is_id_shape("ABC123def456")); // uppercase
        assert!(!is_id_shape("atlas.html")); // has dot
        assert!(!is_id_shape("notes/foo")); // has slash
    }

    #[test]
    fn path_matches_suffix_basename() {
        assert!(path_matches_suffix("/x/y/atlas.html", "atlas.html"));
        assert!(path_matches_suffix("/x/atlas.html", "atlas.html"));
        assert!(!path_matches_suffix("/x/subatlas.html", "atlas.html"));
        assert!(!path_matches_suffix("/x/atlas.html.bak", "atlas.html"));
    }

    #[test]
    fn path_matches_suffix_multi_segment() {
        assert!(path_matches_suffix("/x/pm/01.html", "pm/01.html"));
        assert!(!path_matches_suffix("/x/notpm/01.html", "pm/01.html"));
        // Whole-path match
        assert!(path_matches_suffix("/x/pm/01.html", "/x/pm/01.html"));
    }

    #[test]
    fn path_matches_suffix_rejects_empty() {
        assert!(!path_matches_suffix("/x/y", ""));
    }

    fn summary(id: &str, title: &str, path: &str) -> kb_core::storage::lance::DocSummary {
        kb_core::storage::lance::DocSummary {
            id: id.to_string(),
            title: title.to_string(),
            path: path.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn classify_identifier_token_accepts_tickets() {
        assert_eq!(
            classify_identifier_token("15715"),
            Some(IdentifierToken::Ticket("15715"))
        );
        assert_eq!(
            classify_identifier_token("#15715"),
            Some(IdentifierToken::Ticket("15715"))
        );
        assert_eq!(
            classify_identifier_token("157"),
            Some(IdentifierToken::Ticket("157"))
        );
        // Pure digit runs stay tickets, even when long enough to be a slug.
        assert_eq!(
            classify_identifier_token("157150"),
            Some(IdentifierToken::Ticket("157150"))
        );
    }

    #[test]
    fn classify_identifier_token_rejects_non_tickets() {
        assert_eq!(classify_identifier_token("12"), None);
        assert_eq!(classify_identifier_token("#12"), None);
        assert_eq!(classify_identifier_token("##15715"), None);
        assert_eq!(classify_identifier_token("15715#"), None);
        assert_eq!(classify_identifier_token("#15715 "), None);
        assert_eq!(classify_identifier_token(""), None);
    }

    #[test]
    fn classify_identifier_token_accepts_slugs() {
        assert_eq!(
            classify_identifier_token("2026-09-19-the-margin"),
            Some(IdentifierToken::Slug("2026-09-19-the-margin"))
        );
        assert_eq!(
            classify_identifier_token("2026-09-19"),
            Some(IdentifierToken::Slug("2026-09-19"))
        );
        assert_eq!(
            classify_identifier_token("kb-code"),
            Some(IdentifierToken::Slug("kb-code"))
        );
        assert_eq!(
            classify_identifier_token("readme"),
            Some(IdentifierToken::Slug("readme"))
        );
    }

    #[test]
    fn classify_identifier_token_rejects_non_slugs() {
        assert_eq!(classify_identifier_token("ux-09"), None); // 5 chars
        assert_eq!(classify_identifier_token("atlas.html"), None);
        assert_eq!(classify_identifier_token("notes/foo-bar"), None);
        assert_eq!(classify_identifier_token("foo_bar_baz"), None);
        assert_eq!(classify_identifier_token("------"), None);
        assert_eq!(classify_identifier_token("foo bar"), None);
    }

    #[test]
    fn filename_stem_strips_final_extension_only() {
        assert_eq!(filename_stem("/x/y/15715-login.html"), "15715-login");
        assert_eq!(filename_stem("/x/foo.bar.html"), "foo.bar");
        assert_eq!(filename_stem("/x/.gitignore"), ".gitignore");
        assert_eq!(filename_stem(r"C:\x\notes.html"), "notes");
    }

    #[test]
    fn resolve_identifier_token_unique_ticket_and_slug() {
        let rows = vec![
            summary("aaa", "Fix #15715 login", "/corpus/login.html"),
            summary("bbb", "The Margin", "/corpus/2026-09-19-the-margin.html"),
            summary("ccc", "Notes", "/corpus/15715/notes.html"),
        ];
        assert_eq!(resolve_identifier_token("15715", &rows).unwrap().id, "aaa");
        assert_eq!(resolve_identifier_token("#15715", &rows).unwrap().id, "aaa");
        assert_eq!(
            resolve_identifier_token("2026-09-19-the-margin", &rows)
                .unwrap()
                .id,
            "bbb"
        );
        // Dated prefix, hyphen-bounded inside the stem.
        assert_eq!(
            resolve_identifier_token("2026-09-19", &rows).unwrap().id,
            "bbb"
        );
    }

    #[test]
    fn resolve_identifier_token_matches_stem_when_title_misses() {
        let rows = vec![summary("aaa", "Login fix", "/corpus/15715-login.html")];
        assert_eq!(resolve_identifier_token("#15715", &rows).unwrap().id, "aaa");
        let rows = vec![summary("bbb", "Other", "/corpus/The-Margin.html")];
        assert_eq!(
            resolve_identifier_token("the-margin", &rows).unwrap().id,
            "bbb"
        );
    }

    #[test]
    fn resolve_identifier_token_ambiguous_and_unknown_fall_through() {
        let rows = vec![
            summary("aaa", "Fix #15715", "/corpus/a.html"),
            summary("bbb", "Also #15715", "/corpus/b.html"),
            summary("ccc", "atlas.html mention", "/corpus/atlas.html"),
        ];
        // >1 ticket hits: fall through, not a hard miss.
        assert!(resolve_identifier_token("15715", &rows).is_none());
        assert!(resolve_identifier_token("#15715", &rows).is_none());
        // Unknown shapes are not tokens, even when a title mentions them.
        assert!(resolve_identifier_token("atlas.html", &rows).is_none());
        assert!(resolve_identifier_token("12", &rows).is_none());
        assert!(resolve_identifier_token("ux-09", &rows).is_none());
        // Classified, but zero hits.
        assert!(resolve_identifier_token("99999", &rows).is_none());
        assert!(resolve_identifier_token("not-a-real-slug", &rows).is_none());
        // Shared dated prefix is ambiguous.
        let dated = vec![
            summary("a", "A", "/c/2026-09-19-alpha.html"),
            summary("b", "B", "/c/2026-09-19-beta.html"),
        ];
        assert!(resolve_identifier_token("2026-09-19", &dated).is_none());
    }

    #[test]
    fn resolve_identifier_token_rejects_digit_prefix_and_directory() {
        let rows = vec![
            summary("aaa", "Ticket 157150", "/corpus/157150-notes.html"),
            summary("bbb", "Notes", "/corpus/15715/notes.html"),
            summary("ccc", "Readme more", "/corpus/readmemore.html"),
        ];
        assert!(resolve_identifier_token("15715", &rows).is_none());
        assert!(resolve_identifier_token("readme", &rows).is_none());
    }
}
