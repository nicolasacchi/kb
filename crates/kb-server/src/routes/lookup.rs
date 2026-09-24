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
//!        - 0      → config ticket arm, then arm 5
//!          4b. Config ticket arm, only when `[kb.<name>] id_patterns` is
//!          non-empty. Token is bare digits, `#` plus digits, or `q`
//!          matching one of those regexes (compiled once; an invalid
//!          pattern is skipped and does not 500). Matched against title,
//!          filename stem, and `kb-ticket` meta (HTML or frontmatter).
//!        - 1 hit → Exact, plus `"match": "ticket"`
//!        - 0 or >1 → fall through. Empty `id_patterns` skips this arm,
//!          so today's miss is unchanged.
//!   5. Identifier token, closed patterns (not config):
//!      ticket `^#?\d{3,}$` (`15715`, `#15715`) or a slug of ASCII
//!      letters/digits/hyphens, length ≥ 6. Matched against title and
//!      filename stem.
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
    /// Ticket-arm only. Omitted on every other hit so id / suffix /
    /// ambiguous JSON stays unchanged.
    #[serde(rename = "match", skip_serializing_if = "Option::is_none")]
    pub r#match: Option<&'static str>,
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

    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
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

    // Config ticket arm, then the closed-pattern identifier arm. Both
    // only fill a suffix miss. A unique config-ticket hit returns now as
    // Exact + `"match":"ticket"`. Zero or many, and an empty
    // `id_patterns`, fall through so today's miss is unchanged.
    if candidates.is_empty() {
        let id_patterns = {
            let cfg = state.config.read().await;
            cfg.kb
                .get(&kb_name)
                .map(|sec| sec.id_patterns.clone())
                .unwrap_or_default()
        };
        if let Some(row) = resolve_config_ticket(q, &rows, &id_patterns) {
            let mut hit = hit_from_row(row, &source_root);
            hit.r#match = Some("ticket");
            return Json(LookupResult::Exact(hit)).into_response();
        }
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
        r#match: None,
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

/// Config ticket arm. `None` when `id_patterns` is empty, `q` is not a
/// ticket token, or the token matches 0 or >1 rows — those fall through.
fn resolve_config_ticket<'a>(
    q: &str,
    rows: &'a [kb_core::storage::lance::DocSummary],
    id_patterns: &[String],
) -> Option<&'a kb_core::storage::lance::DocSummary> {
    resolve_config_ticket_with(q, rows, id_patterns, |row| kb_ticket_meta(&row.path))
}

fn resolve_config_ticket_with<'a>(
    q: &str,
    rows: &'a [kb_core::storage::lance::DocSummary],
    id_patterns: &[String],
    mut meta_of: impl FnMut(&kb_core::storage::lance::DocSummary) -> Option<String>,
) -> Option<&'a kb_core::storage::lance::DocSummary> {
    if id_patterns.is_empty() {
        return None;
    }
    let patterns = compiled_id_patterns(id_patterns);
    let needle = config_ticket_needle(q, &patterns)?;
    let mut found = None;
    for row in rows {
        let meta = meta_of(row);
        if row_matches_ticket(&row.title, &row.path, meta.as_deref(), needle) {
            if found.is_some() {
                return None;
            }
            found = Some(row);
        }
    }
    found
}

/// Bare digits, or a single `#` plus digits. The needle is the digit run.
fn bare_or_hash_digits(q: &str) -> Option<&str> {
    let digits = q.strip_prefix('#').unwrap_or(q);
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        Some(digits)
    } else {
        None
    }
}

/// Ticket needle is the digit run. A dated slug's needle is `q` itself,
/// and only when `q` matches one of the compiled id patterns.
fn config_ticket_needle<'a>(q: &'a str, patterns: &[regex::Regex]) -> Option<&'a str> {
    if let Some(digits) = bare_or_hash_digits(q) {
        return Some(digits);
    }
    if patterns.iter().any(|re| re.is_match(q)) {
        Some(q)
    } else {
        None
    }
}

fn row_matches_ticket(title: &str, path: &str, meta: Option<&str>, needle: &str) -> bool {
    token_bounded(title, needle)
        || token_bounded(filename_stem(path), needle)
        || meta.is_some_and(|m| token_bounded(m, needle))
}

/// Each pattern string is compiled at most once. Invalid patterns are
/// remembered as absent so a bad regex never 500s the route.
fn compiled_id_patterns(patterns: &[String]) -> Vec<regex::Regex> {
    patterns
        .iter()
        .filter_map(|p| cached_id_pattern(p))
        .collect()
}

fn cached_id_pattern(pat: &str) -> Option<regex::Regex> {
    use std::sync::{LazyLock, Mutex};
    static CACHE: LazyLock<Mutex<std::collections::HashMap<String, Option<regex::Regex>>>> =
        LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));
    let mut guard = match CACHE.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(hit) = guard.get(pat) {
        return hit.clone();
    }
    let compiled = regex::Regex::new(pat).ok();
    guard.insert(pat.to_string(), compiled.clone());
    compiled
}

fn kb_ticket_meta(path: &str) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = [0u8; 65_536];
    let n = std::io::Read::read(&mut file, &mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf[..n]);
    extract_kb_ticket(&text)
}

/// `kb-ticket` from a leading Markdown frontmatter block, else an HTML
/// `<meta name="kb-ticket">`. Frontmatter wins, matching the indexer.
fn extract_kb_ticket(src: &str) -> Option<String> {
    frontmatter_kb_ticket(src).or_else(|| html_meta_kb_ticket(src))
}

fn frontmatter_kb_ticket(src: &str) -> Option<String> {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let rest = src
        .strip_prefix("---\r\n")
        .or_else(|| src.strip_prefix("---\n"))?;
    for line in rest.lines() {
        if line.trim() == "---" {
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim() != "kb-ticket" {
            continue;
        }
        let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
        let value = value.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn html_meta_kb_ticket(src: &str) -> Option<String> {
    let lower = src.to_ascii_lowercase();
    let mut from = 0;
    while let Some(rel) = lower[from..].find("<meta") {
        let start = from + rel;
        let tag = match src[start..].find('>') {
            Some(end) => &src[start..=start + end],
            None => &src[start..],
        };
        if attr_eq_ignore_ascii_case(tag, "name", "kb-ticket") {
            if let Some(value) = attr_value(tag, "content") {
                let value = value.trim();
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
        from = start + tag.len();
        if from >= src.len() {
            break;
        }
    }
    None
}

fn attr_eq_ignore_ascii_case(tag: &str, name: &str, expected: &str) -> bool {
    attr_value(tag, name).is_some_and(|v| v.eq_ignore_ascii_case(expected))
}

fn attr_value<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let lower = tag.to_ascii_lowercase();
    let key = format!("{name}=");
    let idx = lower.find(&key)?;
    let after = tag[idx + key.len()..].trim_start();
    if let Some(rest) = after.strip_prefix('"') {
        return rest.split('"').next();
    }
    if let Some(rest) = after.strip_prefix('\'') {
        return rest.split('\'').next();
    }
    Some(
        after
            .split(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .next()
            .unwrap_or(""),
    )
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

    #[test]
    fn config_ticket_missing_file_does_not_panic() {
        let patterns = vec![r"^\d{4}-\d{2}-\d{2}-[a-z0-9-]+$".to_string()];
        let rows = vec![summary("aaa", "Fix #15715", "/no/such/15715.html")];
        assert_eq!(
            resolve_config_ticket("#15715", &rows, &patterns)
                .unwrap()
                .id,
            "aaa"
        );
        assert!(resolve_config_ticket("#15715", &rows, &[]).is_none());
    }

    fn no_meta(_: &kb_core::storage::lance::DocSummary) -> Option<String> {
        None
    }

    #[test]
    fn config_ticket_unique_hash_and_dated_slug() {
        let patterns = vec![r"^\d{4}-\d{2}-\d{2}-[a-z0-9-]+$".to_string()];
        let rows = vec![
            summary("aaa", "Fix #15715 login", "/corpus/login.html"),
            summary("bbb", "The Margin", "/corpus/2026-09-19-the-margin.html"),
        ];
        assert_eq!(
            resolve_config_ticket_with("#15715", &rows, &patterns, no_meta)
                .unwrap()
                .id,
            "aaa"
        );
        assert_eq!(
            resolve_config_ticket_with("15715", &rows, &patterns, no_meta)
                .unwrap()
                .id,
            "aaa"
        );
        assert_eq!(
            resolve_config_ticket_with("2026-09-19-the-margin", &rows, &patterns, no_meta)
                .unwrap()
                .id,
            "bbb"
        );
    }

    #[test]
    fn config_ticket_matches_stem_and_kb_ticket_meta() {
        let patterns = vec![r"^\d{4}-\d{2}-\d{2}-[a-z0-9-]+$".to_string()];
        let stem = vec![summary("aaa", "Login fix", "/corpus/15715-login.html")];
        assert_eq!(
            resolve_config_ticket_with("#15715", &stem, &patterns, no_meta)
                .unwrap()
                .id,
            "aaa"
        );
        let meta_row = vec![summary("ccc", "Other", "/corpus/notes.html")];
        let meta = |row: &kb_core::storage::lance::DocSummary| {
            (row.id == "ccc").then(|| "15715".to_string())
        };
        assert_eq!(
            resolve_config_ticket_with("#15715", &meta_row, &patterns, meta)
                .unwrap()
                .id,
            "ccc"
        );
        let slug_row = vec![summary("ddd", "Plain", "/corpus/plain.html")];
        let slug_meta =
            |_: &kb_core::storage::lance::DocSummary| Some("2026-09-19-the-margin".to_string());
        assert_eq!(
            resolve_config_ticket_with("2026-09-19-the-margin", &slug_row, &patterns, slug_meta)
                .unwrap()
                .id,
            "ddd"
        );
    }

    #[test]
    fn config_ticket_two_hits_and_empty_patterns_fall_through() {
        let patterns = vec![r"^\d{4}-\d{2}-\d{2}".to_string()];
        let rows = vec![
            summary("aaa", "Fix #15715", "/corpus/a.html"),
            summary("bbb", "Also #15715", "/corpus/b.html"),
        ];
        assert!(resolve_config_ticket_with("#15715", &rows, &patterns, no_meta).is_none());
        assert!(resolve_config_ticket_with("15715", &rows, &[], no_meta).is_none());
        let dated = vec![summary(
            "bbb",
            "The Margin",
            "/corpus/2026-09-19-the-margin.html",
        )];
        assert!(
            resolve_config_ticket_with("2026-09-19-the-margin", &dated, &[], no_meta).is_none()
        );
        assert!(
            resolve_config_ticket_with("not-a-dated-slug", &dated, &patterns, no_meta).is_none()
        );
    }

    #[test]
    fn config_ticket_invalid_pattern_is_ignored() {
        let rows = vec![summary("aaa", "Fix #15715", "/corpus/a.html")];
        let bad = vec!["(unclosed".to_string()];
        assert_eq!(
            resolve_config_ticket_with("#15715", &rows, &bad, no_meta)
                .unwrap()
                .id,
            "aaa"
        );
        let dated = vec![summary("bbb", "X", "/corpus/2026-09-19-the-margin.html")];
        assert!(
            resolve_config_ticket_with("2026-09-19-the-margin", &dated, &bad, no_meta).is_none()
        );
        let mixed = vec![
            "(unclosed".to_string(),
            r"^\d{4}-\d{2}-\d{2}-[a-z0-9-]+$".to_string(),
        ];
        assert_eq!(
            resolve_config_ticket_with("2026-09-19-the-margin", &dated, &mixed, no_meta)
                .unwrap()
                .id,
            "bbb"
        );
    }

    #[test]
    fn config_ticket_rejects_digit_prefix() {
        let patterns = vec![r"\d+".to_string()];
        let rows = vec![summary("aaa", "Ticket 157150", "/corpus/157150-notes.html")];
        assert!(resolve_config_ticket_with("15715", &rows, &patterns, no_meta).is_none());
    }

    #[test]
    fn extract_kb_ticket_from_html_and_frontmatter() {
        assert_eq!(
            extract_kb_ticket(r#"<meta name="kb-ticket" content="15715">"#).as_deref(),
            Some("15715")
        );
        assert_eq!(
            extract_kb_ticket(r##"<meta content="#15715" name="KB-ticket"/>"##).as_deref(),
            Some("#15715")
        );
        let md = "---\ntitle: x\nkb-ticket: \"2026-09-19-the-margin\"\n---\n\nbody\n";
        assert_eq!(
            extract_kb_ticket(md).as_deref(),
            Some("2026-09-19-the-margin")
        );
        let both =
            "---\nkb-ticket: from-fm\n---\n<meta name=\"kb-ticket\" content=\"from-html\">\n";
        assert_eq!(extract_kb_ticket(both).as_deref(), Some("from-fm"));
        assert!(extract_kb_ticket("<title>no ticket</title>").is_none());
    }

    #[test]
    fn ticket_hit_json_is_exact_plus_match_and_id_hit_omits_it() {
        let ticket = LookupHit {
            id: "aaa".into(),
            path: "/corpus/a.html".into(),
            source_relative: "a.html".into(),
            folder: String::new(),
            title: "Fix #15715".into(),
            r#match: Some("ticket"),
        };
        let v = serde_json::to_value(LookupResult::Exact(ticket)).unwrap();
        assert_eq!(v["kind"], "exact");
        assert_eq!(v["id"], "aaa");
        assert_eq!(v["source_relative"], "a.html");
        assert_eq!(v["match"], "ticket");

        let id_hit = LookupHit {
            id: "bbb".into(),
            path: "/corpus/b.html".into(),
            source_relative: "b.html".into(),
            folder: String::new(),
            title: "Other".into(),
            r#match: None,
        };
        let v = serde_json::to_value(LookupResult::Exact(id_hit)).unwrap();
        assert_eq!(v["kind"], "exact");
        assert!(v.get("match").is_none());
    }
}
