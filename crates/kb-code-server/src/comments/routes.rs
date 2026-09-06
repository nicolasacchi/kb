//! `comments/1` — the read surface.
//!
//! Four routes, all ordinary `auth_bearer` READS of already-indexed
//! metadata: nothing here touches a working tree, mutates a ref, or serves
//! transcript text.
//!
//! ## Honesty rules these routes hold
//!
//! * **True totals.** `total` is the COUNT of rows matching the SQL-side
//!   filters, taken from the database, not the length of the page.
//! * **Every bound is named.** The state lane is computed per request over
//!   a BOUNDED set, and every response says how much it scanned, what the
//!   bound was, and whether it hit it ([`ScanBasis`], [`BlameBasis`]).
//! * **A refusal is a state, not an omission.** A row the blame budget
//!   did not reach is `unknown` with `reason: "blame-budget"` — it is
//!   never quietly reported `fresh`, and never dropped from the page.
//! * **The summary counts what it can count exactly, and says what it
//!   cannot.** `by_kind`/`by_keyword` are whole-repo `GROUP BY`s.
//!   `by_state` covers only the two lanes derivable from stored columns
//!   (`aged`, `unreasoned`); the three blame-derived states are ABSENT
//!   from it with the reason spelled out, rather than being a number
//!   computed over a silently-tiny sample.

use crate::comments::drift::{self, CommentState, FileBlame};
use crate::comments::keywords::{SmartTodoFields, TODO_FAMILY};
use crate::comments::{classify::CommentKind, KeywordSet};
use crate::entities::RouteContract;
use crate::routes::{find_repo, safe_rel_path, ApiError};
use crate::state::SharedState;
use crate::store::{CommentRow, StoreBlocking};
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Default page size for `GET /api/comments`.
pub const DEFAULT_LIMIT: usize = 100;
/// Hard ceiling on `?limit=`.
pub const MAX_LIMIT: usize = 500;

/// How many rows the state lane may examine when `?state=` narrows the
/// result — the filter cannot run in SQL (no column holds a state), so the
/// scan is bounded and the bound is reported.
pub const STATE_SCAN_CAP: usize = 2_000;

/// How many DISTINCT FILES one request may blame. The drift oracle needs
/// one `git blame` per file; a request that wanted more says so
/// (`BlameBasis::exhausted`) and reports the rows it could not reach as
/// `unknown`, rather than running an unbounded fan-out of git children on
/// an IO-bound box (kb-code-server invariant 5's posture).
pub const MAX_BLAMED_FILES: usize = 20;

/// How many rows `GET /api/comments/summary` may examine for its two
/// stored-column state lanes.
pub const SUMMARY_STATE_CAP: usize = 5_000;

// --- wire shapes ------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct DocSymbolOut {
    pub name: String,
    pub kind: String,
    pub line_start: i64,
    pub line_end: i64,
}

#[derive(Debug, Serialize)]
pub struct DirectiveOut {
    pub tool: String,
    /// `None` for a magic comment / build tag — nothing to justify.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_reason: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct CommentOut {
    pub path: String,
    pub kind: String,
    pub line_start: i64,
    pub line_end: i64,
    pub text: String,
    pub text_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyword: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyword_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<SmartTodoFields>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<DocSymbolOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directive: Option<DirectiveOut>,
    /// The blob these rows were derived from — the caller's own drift
    /// check against a file it just read.
    pub blob_sha: String,
    /// Computed per request, persisted nowhere.
    pub state: CommentState,
}

/// What the state lane actually looked at.
#[derive(Debug, Serialize)]
pub struct ScanBasis {
    pub rows_scanned: usize,
    /// The true count of rows matching the SQL-side filters, from the
    /// database — not the length of anything in this response.
    pub rows_matching_filters: i64,
    pub bound: usize,
    /// The scan hit its bound, so rows past it were never examined.
    pub truncated: bool,
}

/// What the drift oracle actually blamed.
#[derive(Debug, Serialize)]
pub struct BlameBasis {
    pub files_blamed: usize,
    pub files_wanted: usize,
    pub budget: usize,
    pub exhausted: bool,
}

#[derive(Debug, Serialize)]
pub struct CommentsListOut {
    pub repo: String,
    pub comments: Vec<CommentOut>,
    /// The number of rows a caller could page through with these filters.
    /// With `?state=` this is the number of SURVIVORS within the scan —
    /// `scan.truncated` says whether that is the whole story.
    pub total: usize,
    /// More rows exist past this page.
    pub truncated: bool,
    pub offset: usize,
    pub limit: usize,
    pub scan: ScanBasis,
    pub blame: BlameBasis,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct CommentsFileOut {
    pub repo: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob_sha: Option<String>,
    pub comments: Vec<CommentOut>,
    pub total: usize,
    pub truncated: bool,
    pub blame: BlameBasis,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct KeywordsOut {
    pub keywords: Vec<String>,
    /// `"default"` or `"config"`.
    pub source: &'static str,
    pub rubocop_defaults: Vec<&'static str>,
    /// The two markers carried beyond RuboCop's six so `GET /api/todos`
    /// keeps its row set.
    pub legacy_extra: Vec<&'static str>,
    /// The keywords `GET /api/todos` reports.
    pub todo_family: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct StateBasisOut {
    /// The state lanes these counts cover.
    pub lanes: Vec<&'static str>,
    /// The state lanes deliberately NOT counted here, and why.
    pub excluded: Vec<&'static str>,
    pub excluded_reason: &'static str,
    pub candidates_scanned: usize,
    pub bound: usize,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct CommentsSummaryOut {
    pub repo: String,
    /// Exact — a `COUNT(*)` over the repo's rows.
    pub total: i64,
    /// Exact — a whole-repo `GROUP BY kind`.
    pub by_kind: BTreeMap<String, i64>,
    /// Exact — a whole-repo `GROUP BY keyword` over annotation rows.
    pub by_keyword: BTreeMap<String, i64>,
    /// The two lanes derivable without `git blame`.
    pub by_state: BTreeMap<String, i64>,
    pub state_basis: StateBasisOut,
    pub keywords: KeywordsOut,
}

// --- params -----------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ListParams {
    pub repo: String,
    /// A path PREFIX (`app/models/`), matching `GET /api/todos`'s own
    /// `path_prefix` semantics.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub keyword: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct FileParams {
    pub repo: String,
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct SummaryParams {
    pub repo: String,
}

#[derive(Debug, Deserialize)]
pub struct KeywordsParams {}

// --- handlers ---------------------------------------------------------------

/// `GET /api/comments?repo=[&path=][&kind=][&keyword=][&state=][&limit=]
/// [&offset=]` — the paged index.
pub async fn list_comments(
    State(state): State<SharedState>,
    Query(params): Query<ListParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let kind = validate_kind(params.kind.as_deref())?;
    let want_state = validate_state(params.state.as_deref())?;
    let path_prefix = match params.path.as_deref() {
        Some(p) => Some(safe_rel_path(p)?.to_string()),
        None => None,
    };
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let offset = params.offset.unwrap_or(0);

    // With no `?state=`, the page IS the scan: nothing is filtered after
    // SQL, so reading `offset + limit` rows is exact. With `?state=`, the
    // filter cannot run in SQL, so the scan is bounded and reported.
    let scan_cap = if want_state.is_some() {
        STATE_SCAN_CAP
    } else {
        offset.saturating_add(limit).min(STATE_SCAN_CAP)
    };

    let kind_q = kind.map(str::to_string);
    let keyword_q = params.keyword.clone();
    let prefix_q = path_prefix.clone();
    let (rows, sql_total) = state
        .store
        .run_blocking(
            move |store| -> Result<(Vec<CommentRow>, i64), crate::store::StoreError> {
                let rows = store.list_comments(
                    repo_id,
                    kind_q.as_deref(),
                    keyword_q.as_deref(),
                    prefix_q.as_deref(),
                    scan_cap,
                )?;
                let total = store.comment_count(
                    repo_id,
                    kind_q.as_deref(),
                    keyword_q.as_deref(),
                    prefix_q.as_deref(),
                )?;
                Ok((rows, total))
            },
        )
        .await?;

    let scan = ScanBasis {
        rows_scanned: rows.len(),
        rows_matching_filters: sql_total,
        bound: scan_cap,
        truncated: rows.len() >= scan_cap && (sql_total as usize) > rows.len(),
    };

    let (blames, blame_basis) = blame_for_rows(&state, repo.path.clone(), repo_id, &rows).await;
    let today = chrono::Utc::now().date_naive();
    let mut out: Vec<CommentOut> = rows
        .into_iter()
        .map(|r| {
            let st = state_for(&r, &blames, today);
            to_out(r, st)
        })
        .collect();
    if let Some(want) = want_state {
        out.retain(|c| c.state.state == want);
    }

    let total = if want_state.is_some() {
        out.len()
    } else {
        sql_total.max(0) as usize
    };
    let page: Vec<CommentOut> = out.into_iter().skip(offset).take(limit).collect();
    let truncated = total > offset.saturating_add(page.len());

    let mut notes = Vec::new();
    if scan.truncated {
        notes.push(format!(
            "state filter examined the first {} of {} matching rows",
            scan.rows_scanned, scan.rows_matching_filters
        ));
    }
    if blame_basis.exhausted {
        notes.push(format!(
            "blame budget reached: {} of {} files blamed; the rest report state \"unknown\"",
            blame_basis.files_blamed, blame_basis.files_wanted
        ));
    }

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(CommentsListOut {
            repo: params.repo,
            comments: page,
            total,
            truncated,
            offset,
            limit,
            scan,
            blame: blame_basis,
            notes,
        }),
    ))
}

/// `GET /api/comments/file?repo=&path=` — every block in one file, in line
/// order. The per-file gutter's feed.
pub async fn comments_file(
    State(state): State<SharedState>,
    Query(params): Query<FileParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let path = safe_rel_path(&params.path)?.to_string();
    let path_q = path.clone();
    let rows = state
        .store
        .run_blocking(move |store| store.comments_for_file(repo_id, &path_q))
        .await?;

    let (blames, blame_basis) = blame_for_rows(&state, repo.path.clone(), repo_id, &rows).await;
    let today = chrono::Utc::now().date_naive();
    let blob_sha = rows.first().map(|r| r.blob_sha.clone());
    let truncated = rows.iter().any(|r| r.text_truncated);
    let comments: Vec<CommentOut> = rows
        .into_iter()
        .map(|r| {
            let st = state_for(&r, &blames, today);
            to_out(r, st)
        })
        .collect();

    let mut notes = Vec::new();
    if comments.is_empty() {
        // The empty-with-reason read state: a file with no comment rows is
        // not the same thing as a file this daemon has never indexed.
        notes.push(
            "no comment rows for this path — the file has no comments, or it is not indexed"
                .to_string(),
        );
    }

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(CommentsFileOut {
            repo: params.repo,
            path,
            blob_sha,
            total: comments.len(),
            truncated,
            comments,
            blame: blame_basis,
            notes,
        }),
    ))
}

/// The four reads `comments_summary` takes in ONE hop to the blocking
/// pool: `(total, per-kind counts, per-keyword counts, state candidates)`.
type SummaryReads = (i64, Vec<(String, i64)>, Vec<(String, i64)>, Vec<CommentRow>);

/// `GET /api/comments/summary?repo=` — exact per-kind and per-keyword
/// counts, plus the two state lanes that need no `git blame`.
pub async fn comments_summary(
    State(state): State<SharedState>,
    Query(params): Query<SummaryParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (_repo, repo_id) = find_repo(&state, &params.repo)?;
    let (total, kinds, keywords, candidates) = state
        .store
        .run_blocking(
            move |store| -> Result<SummaryReads, crate::store::StoreError> {
                Ok((
                    store.comment_total(repo_id)?,
                    store.comment_kind_counts(repo_id)?,
                    store.comment_keyword_counts(repo_id)?,
                    store.comment_state_candidates(repo_id, SUMMARY_STATE_CAP)?,
                ))
            },
        )
        .await?;

    let today = chrono::Utc::now().date_naive();
    let mut by_state: BTreeMap<String, i64> = BTreeMap::new();
    by_state.insert("aged".to_string(), 0);
    by_state.insert("unreasoned".to_string(), 0);
    for r in &candidates {
        let st = cheap_state(r, today);
        if st.state == "aged" || st.state == "unreasoned" {
            *by_state.entry(st.state.to_string()).or_insert(0) += 1;
        }
    }

    let kw = state.comment_keywords.as_ref();
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(CommentsSummaryOut {
            repo: params.repo,
            total,
            by_kind: kinds.into_iter().collect(),
            by_keyword: keywords.into_iter().collect(),
            by_state,
            state_basis: StateBasisOut {
                lanes: vec!["aged", "unreasoned"],
                excluded: vec!["fresh", "drifted", "unknown"],
                excluded_reason:
                    "computed from git blame per request; ask GET /api/comments?state=drifted",
                candidates_scanned: candidates.len(),
                bound: SUMMARY_STATE_CAP,
                truncated: candidates.len() >= SUMMARY_STATE_CAP,
            },
            keywords: keywords_out(kw),
        }),
    ))
}

/// `GET /api/comments/keywords` — the effective annotation vocabulary.
/// Daemon-wide (`[comments] keywords` is not per-repo), so it takes no
/// `repo`.
pub async fn comments_keywords(
    State(state): State<SharedState>,
    Query(_): Query<KeywordsParams>,
) -> Result<impl IntoResponse, ApiError> {
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(keywords_out(state.comment_keywords.as_ref())),
    ))
}

fn keywords_out(kw: &KeywordSet) -> KeywordsOut {
    KeywordsOut {
        keywords: kw.keywords.clone(),
        source: kw.source,
        rubocop_defaults: crate::comments::keywords::RUBOCOP_KEYWORDS.to_vec(),
        legacy_extra: crate::comments::keywords::LEGACY_EXTRA_KEYWORDS.to_vec(),
        todo_family: TODO_FAMILY.to_vec(),
    }
}

// --- state computation ------------------------------------------------------

/// The two lanes that need no git subprocess.
fn cheap_state(r: &CommentRow, today: chrono::NaiveDate) -> CommentState {
    match r.kind.as_str() {
        "annotation" => {
            let fields: Option<SmartTodoFields> = r
                .fields_json
                .as_deref()
                .and_then(|j| serde_json::from_str(j).ok());
            drift::annotation_state(fields.as_ref(), today)
        }
        "directive" => drift::directive_state(r.directive_tool.as_deref(), r.directive_has_reason),
        _ => CommentState::none(),
    }
}

/// The full state for one row: the cheap lanes, plus the blame-derived
/// `doc` lane when this request managed to blame the row's file.
fn state_for(
    r: &CommentRow,
    blames: &BTreeMap<String, Option<FileBlame>>,
    today: chrono::NaiveDate,
) -> CommentState {
    if r.kind != "doc" {
        return cheap_state(r, today);
    }
    let (Some(body_start), Some(body_end)) = (r.symbol_line_start, r.symbol_line_end) else {
        return CommentState::unknown("no-documented-symbol");
    };
    match blames.get(&r.path) {
        None => CommentState::unknown("blame-budget"),
        Some(None) => CommentState::unknown("blame-unavailable"),
        Some(Some(b)) => drift::doc_state(
            b,
            r.line_start.max(0) as u32,
            r.line_end.max(0) as u32,
            body_start.max(0) as u32,
            body_end.max(0) as u32,
        ),
    }
}

/// Blame every file that carries a `doc` row in `rows`, up to
/// [`MAX_BLAMED_FILES`]. Returns the per-path blame (a `None` value means
/// "this file was in budget but git could not blame it" — a distinct,
/// honest state from "not in budget", which is an ABSENT key).
///
/// One `git_fanout` permit for the whole batch, one `spawn_blocking` hop,
/// blames run sequentially inside it: this is the same bounded-git-child
/// posture kb-code-server invariant 5 records, and it keeps a 20-file
/// request from becoming 20 concurrent git children on an IO-bound host.
async fn blame_for_rows(
    state: &SharedState,
    repo_root: std::path::PathBuf,
    repo_id: i64,
    rows: &[CommentRow],
) -> (BTreeMap<String, Option<FileBlame>>, BlameBasis) {
    let wanted: BTreeSet<String> = rows
        .iter()
        .filter(|r| r.kind == "doc" && r.symbol_line_start.is_some())
        .map(|r| r.path.clone())
        .collect();
    let files_wanted = wanted.len();
    let planned: Vec<String> = wanted.into_iter().take(MAX_BLAMED_FILES).collect();
    let basis = BlameBasis {
        files_blamed: planned.len(),
        files_wanted,
        budget: MAX_BLAMED_FILES,
        exhausted: files_wanted > MAX_BLAMED_FILES,
    };
    if planned.is_empty() {
        return (BTreeMap::new(), basis);
    }

    let Ok(permit) = state.git_fanout.clone().acquire_owned().await else {
        // The semaphore is only ever closed at shutdown; degrade to
        // "nothing blamed" rather than failing the whole read.
        return (BTreeMap::new(), basis);
    };
    let cache = state.blame_cache.clone();
    let out = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut map: BTreeMap<String, Option<FileBlame>> = BTreeMap::new();
        let git = match crate::git::GitRepo::open(&repo_root) {
            Ok(g) => g,
            Err(_) => {
                for p in planned {
                    map.insert(p, None);
                }
                return map;
            }
        };
        for path in planned {
            let result =
                crate::blame::blame_file(&cache, &git, repo_id, &repo_root, &path, None, None);
            map.insert(path, result.ok().map(|r| FileBlame::new(r.regions)));
        }
        map
    })
    .await
    .unwrap_or_default();
    (out, basis)
}

fn to_out(r: CommentRow, state: CommentState) -> CommentOut {
    let has_reason = r.directive_has_reason;
    let symbol = match (
        r.symbol_name,
        r.symbol_kind,
        r.symbol_line_start,
        r.symbol_line_end,
    ) {
        (Some(name), Some(kind), Some(a), Some(b)) => Some(DocSymbolOut {
            name,
            kind,
            line_start: a,
            line_end: b,
        }),
        _ => None,
    };
    CommentOut {
        path: r.path,
        kind: r.kind,
        line_start: r.line_start,
        line_end: r.line_end,
        text: r.text,
        text_truncated: r.text_truncated,
        keyword: r.keyword,
        keyword_text: r.keyword_text,
        fields: r
            .fields_json
            .as_deref()
            .and_then(|j| serde_json::from_str(j).ok()),
        symbol,
        directive: r
            .directive_tool
            .map(|tool| DirectiveOut { tool, has_reason }),
        blob_sha: r.blob_sha,
        state,
    }
}

// --- param validation -------------------------------------------------------

fn validate_kind(kind: Option<&str>) -> Result<Option<&str>, ApiError> {
    let Some(k) = kind else { return Ok(None) };
    if CommentKind::parse(k).is_none() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!(
                "unknown kind {k:?} — one of: {}",
                CommentKind::ALL
                    .iter()
                    .map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    Ok(Some(k))
}

fn validate_state(state: Option<&str>) -> Result<Option<&str>, ApiError> {
    let Some(s) = state else { return Ok(None) };
    if !drift::STATE_NAMES.contains(&s) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!(
                "unknown state {s:?} — one of: {}",
                drift::STATE_NAMES.join(", ")
            ),
        ));
    }
    Ok(Some(s))
}

// --- route contracts (kb-code-server invariant 15) --------------------------

pub const COMMENTS_ROUTE: RouteContract = RouteContract {
    path: "/api/comments",
    handler: "comments::routes::list_comments",
    required_params: &["repo"],
    params_accept_without: list_params_accept_without,
};

pub const COMMENTS_FILE_ROUTE: RouteContract = RouteContract {
    path: "/api/comments/file",
    handler: "comments::routes::comments_file",
    required_params: &["repo", "path"],
    params_accept_without: file_params_accept_without,
};

pub const COMMENTS_SUMMARY_ROUTE: RouteContract = RouteContract {
    path: "/api/comments/summary",
    handler: "comments::routes::comments_summary",
    required_params: &["repo"],
    params_accept_without: summary_params_accept_without,
};

pub const COMMENTS_KEYWORDS_ROUTE: RouteContract = RouteContract {
    path: "/api/comments/keywords",
    handler: "comments::routes::comments_keywords",
    required_params: &[],
    params_accept_without: keywords_params_accept_without,
};

fn params_map(pairs: &[(&str, &str)], omit: &str) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (k, v) in pairs {
        if *k != omit {
            map.insert(
                (*k).to_string(),
                serde_json::Value::String((*v).to_string()),
            );
        }
    }
    serde_json::Value::Object(map)
}

fn list_params_accept_without(omit: &str) -> bool {
    serde_json::from_value::<ListParams>(params_map(&[("repo", "r")], omit)).is_ok()
}

fn file_params_accept_without(omit: &str) -> bool {
    serde_json::from_value::<FileParams>(params_map(&[("repo", "r"), ("path", "a.rb")], omit))
        .is_ok()
}

fn summary_params_accept_without(omit: &str) -> bool {
    serde_json::from_value::<SummaryParams>(params_map(&[("repo", "r")], omit)).is_ok()
}

fn keywords_params_accept_without(omit: &str) -> bool {
    serde_json::from_value::<KeywordsParams>(params_map(&[], omit)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: &str) -> CommentRow {
        CommentRow {
            path: "a.rb".into(),
            blob_sha: "deadbeef".into(),
            comments_version: "comments@1+kw0+ruby@0".into(),
            ordinal: 0,
            kind: kind.into(),
            keyword: None,
            keyword_text: None,
            fields_json: None,
            line_start: 1,
            line_end: 2,
            text: "t".into(),
            text_truncated: false,
            symbol_name: None,
            symbol_kind: None,
            symbol_line_start: None,
            symbol_line_end: None,
            directive_tool: None,
            directive_has_reason: None,
        }
    }

    fn today() -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(2026, 9, 6).unwrap()
    }

    #[test]
    fn an_unknown_kind_or_state_is_a_400_naming_the_vocabulary() {
        let e = validate_kind(Some("nope")).unwrap_err();
        assert!(format!("{e:?}").contains("commented_code"), "{e:?}");
        let e = validate_state(Some("stale")).unwrap_err();
        assert!(format!("{e:?}").contains("unreasoned"), "{e:?}");
        assert!(validate_kind(Some("doc")).is_ok());
        assert!(validate_state(Some("drifted")).is_ok());
        assert!(validate_kind(None).unwrap().is_none());
    }

    #[test]
    fn a_doc_row_whose_file_was_not_blamed_is_unknown_with_the_budget_reason() {
        let mut r = row("doc");
        r.symbol_line_start = Some(3);
        r.symbol_line_end = Some(9);
        let st = state_for(&r, &BTreeMap::new(), today());
        assert_eq!(st.state, "unknown");
        assert_eq!(st.reason, Some("blame-budget"));
    }

    #[test]
    fn a_doc_row_whose_blame_failed_is_a_different_unknown() {
        let mut r = row("doc");
        r.symbol_line_start = Some(3);
        r.symbol_line_end = Some(9);
        let mut blames = BTreeMap::new();
        blames.insert("a.rb".to_string(), None);
        assert_eq!(
            state_for(&r, &blames, today()).reason,
            Some("blame-unavailable")
        );
    }

    #[test]
    fn a_doc_row_with_no_symbol_is_unknown_not_fresh() {
        let st = state_for(&row("doc"), &BTreeMap::new(), today());
        assert_eq!(st.reason, Some("no-documented-symbol"));
    }

    #[test]
    fn an_unreasoned_suppression_needs_no_blame() {
        let mut r = row("directive");
        r.directive_tool = Some("rubocop".into());
        r.directive_has_reason = Some(false);
        assert_eq!(cheap_state(&r, today()).state, "unreasoned");
        assert_eq!(state_for(&r, &BTreeMap::new(), today()).state, "unreasoned");
    }

    #[test]
    fn an_aged_annotation_is_read_back_out_of_the_stored_json() {
        let mut r = row("annotation");
        r.keyword = Some("TODO".into());
        r.fields_json = Some(
            serde_json::to_string(&SmartTodoFields {
                raw: Default::default(),
                on_kind: Some("date".into()),
                on_date: Some("2026-01-01".into()),
                to: Some("owner@example.com".into()),
            })
            .unwrap(),
        );
        assert_eq!(cheap_state(&r, today()).state, "aged");
    }

    #[test]
    fn a_malformed_fields_json_degrades_to_none_never_a_panic() {
        let mut r = row("annotation");
        r.fields_json = Some("{not json".into());
        assert_eq!(cheap_state(&r, today()).state, "none");
    }

    #[test]
    fn every_route_contract_names_a_distinct_path_and_handler() {
        let paths: BTreeSet<&str> = crate::comments::V72_J1_ROUTES
            .iter()
            .map(|c| c.path)
            .collect();
        assert_eq!(paths.len(), crate::comments::V72_J1_ROUTES.len());
        for c in crate::comments::V72_J1_ROUTES {
            assert!(c.handler.starts_with("comments::routes::"), "{}", c.path);
        }
    }
}
