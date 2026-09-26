//! RS-U10a — the review's own GIT VIEWS for agents (README §13, "required
//! by relocation"): `GET /api/reviews/{id}/diff`, `/log`, `/cat`, plus the
//! PR lookup `GET /api/reviews/find?pr=N[&repo=R]`.
//!
//! Before this unit an agent computed a review's diff with a raw
//! `git -C <mirror> diff <baseSha> refs/kbc/pr/N`. Once the internal review
//! store owns the PR and patchset refs, that stops working — so the daemon
//! computes every view itself, from the PATCHSET ROW's own `base_sha`
//! (the capture-time merge-base) and `tip_sha`, never from a ref name:
//!
//! * every read runs through [`GitCtx::read_with_fallback`] — the review
//!   store once it is `ready`, the member work tree otherwise (and today,
//!   before any store is ready, exactly the work tree);
//! * nothing reads `refs/kbc/*`: a patchset is addressed by its two full
//!   shas, so the views keep working when the user clone carries NO kb
//!   refs at all (the relocation gate; `tests/review/rs_u10a_views.rs`
//!   deletes every `refs/kbc/*` after capture and reads again).
//!
//! Security posture (crate CLAUDE.md): this module spawns nothing itself —
//! every git call goes through `history::run_git_raw` / `history::
//! diff_files` / `routes::read_repo_file`. The only argv entries are
//! daemon-minted 40-hex shas and paths git ITSELF printed (the change set),
//! each passed after `--` with the `:(literal)` magic so no glob or
//! pathspec magic is ever interpreted; a caller's `?path=` is a FILTER over
//! that list, never an argv entry. Content-returning reads (`/cat`, and the
//! patch text of `/diff`) honour the secret denylist: `/cat` refuses with
//! the typed 403 (`state.secret_policy` — floor + `[security]
//! secret_globs`), and `/diff?mode=patch` omits a denylisted file's hunks
//! and names it (with the pattern, never the bytes) under `redacted`.
//! All four are ordinary `auth_bearer` reads — they return strictly less
//! than `GET /api/file` + `GET /api/reviews/{id}/files` already do.

use crate::entities::RouteContract;
use crate::git::roots::GitCtx;
use crate::history;
use crate::reviews::{files_changed, is_full_sha, require_review, resolve_ps};
use crate::routes::{find_repo, read_repo_file, ApiError, RevResolver};
use crate::state::SharedState;
use crate::store::StoreBlocking;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use base64::Engine as _;
use serde::Deserialize;

pub const DIFF_SCHEMA: &str = "kbc-review-diff/1";
pub const LOG_SCHEMA: &str = "kbc-review-log/1";
pub const CAT_SCHEMA: &str = "kbc-review-cat/1";
pub const FIND_SCHEMA: &str = "kbc-review-find/1";

/// The token→byte heuristic `?budget=` uses (≈4 bytes of diff text per
/// model token). Documented, deterministic, deliberately not a tokenizer:
/// the point is a stable cut an agent can reason about.
pub const BYTES_PER_TOKEN: u64 = 4;
/// Hard ceiling on patch text in one response, budget or not.
pub const MAX_PATCH_BYTES: usize = 8 * 1024 * 1024;
/// `/log` returns at most this many commits (`truncated` says so).
pub const MAX_LOG_COMMITS: usize = 1000;

fn join_err(e: tokio::task::JoinError) -> ApiError {
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

// --- pure helpers ------------------------------------------------------------

/// `?mode=` for `/diff`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffMode {
    Stat,
    NameOnly,
    Patch,
}

impl DiffMode {
    pub fn parse(s: Option<&str>) -> Result<Self, String> {
        match s {
            None | Some("") | Some("stat") => Ok(Self::Stat),
            Some("name-only") => Ok(Self::NameOnly),
            Some("patch") => Ok(Self::Patch),
            Some(other) => Err(format!("mode must be stat|name-only|patch, got {other:?}")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stat => "stat",
            Self::NameOnly => "name-only",
            Self::Patch => "patch",
        }
    }
}

/// Does a change-set entry match the caller's `?path=` filter? Exact file,
/// or a directory prefix (`app/models` matches `app/models/order.rb`).
/// Either side of a rename counts. A filter is a plain string compare —
/// never a glob, never handed to git.
pub fn path_matches(filter: &str, path: &str, old_path: Option<&str>) -> bool {
    let f = filter.trim_matches('/');
    if f.is_empty() {
        return true;
    }
    let hit = |p: &str| p == f || p.strip_prefix(f).is_some_and(|rest| rest.starts_with('/'));
    hit(path) || old_path.is_some_and(hit)
}

/// Cut `patch` to at most `max_bytes`, at the last line boundary, and
/// append ONE deterministic marker line naming what was cut. Returns the
/// (possibly unchanged) text and whether it was cut. Same input, same
/// output — an agent re-running with the same budget sees the same bytes.
pub fn truncate_patch(patch: &str, max_bytes: usize, reason: &str) -> (String, bool) {
    if patch.len() <= max_bytes {
        return (patch.to_string(), false);
    }
    let head = &patch[..floor_char_boundary(patch, max_bytes)];
    let mut cut = head.rfind('\n').map(|i| i + 1).unwrap_or(0);
    if cut == 0 {
        // No newline inside the budget: fall back to a char boundary so a
        // single enormous line still yields SOME bytes, never a panic.
        cut = floor_char_boundary(patch, max_bytes);
    }
    let mut out = patch[..cut].to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!(
        "[kb-code: patch truncated ({reason}): {cut} of {} bytes shown; narrow with --path or use --stat]\n",
        patch.len()
    ));
    (out, true)
}

/// The largest char boundary `<= i` (clamped to the string) — a stable
/// stand-in for the unstable `str::floor_char_boundary`.
fn floor_char_boundary(s: &str, i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    let mut i = i;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// One `git log` record: `%H %P %an %aI %s`, unit-separated.
pub const LOG_FORMAT: &str = "--format=%H%x1f%P%x1f%an%x1f%aI%x1f%s%x1e";

/// Parse [`LOG_FORMAT`] output. Malformed records are skipped, never
/// guessed at.
pub fn parse_log(raw: &str) -> Vec<serde_json::Value> {
    raw.split('\x1e')
        .map(|r| r.trim_matches(|c| c == '\n' || c == '\r'))
        .filter(|r| !r.is_empty())
        .filter_map(|r| {
            let f: Vec<&str> = r.splitn(5, '\x1f').collect();
            if f.len() != 5 || !is_full_sha(f[0]) {
                return None;
            }
            let parents: Vec<&str> = f[1].split_whitespace().collect();
            Some(serde_json::json!({
                "sha": f[0],
                "parents": parents,
                "author": f[2],
                "author_date": f[3],
                "subject": f[4],
            }))
        })
        .collect()
}

// --- GET /api/reviews/{id}/diff -----------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ReviewDiffParams {
    #[serde(default)]
    pub ps: Option<String>,
    /// `stat` (default) | `name-only` | `patch`.
    #[serde(default)]
    pub mode: Option<String>,
    /// Exact file or directory-prefix filter over the change set.
    #[serde(default)]
    pub path: Option<String>,
    /// Patch budget in tokens ([`BYTES_PER_TOKEN`] bytes each).
    #[serde(default)]
    pub budget: Option<u64>,
}

/// `GET /api/reviews/{id}/diff?ps=&mode=&path=&budget=` — the patchset's
/// change set against its OWN base (`base_sha..tip_sha`, the merge-base
/// captured with the patchset). `stat` = per-file counts, `name-only` =
/// paths, `patch` = unified diff text (`-M`, no external diff drivers or
/// textconv), budget-truncated with a marker. Bearer.
pub async fn review_diff_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Query(params): Query<ReviewDiffParams>,
) -> Result<impl IntoResponse, ApiError> {
    let mode = DiffMode::parse(params.mode.as_deref()).map_err(ApiError::bad_request)?;
    let (_review, repo, _) = require_review(&state, id).await?;
    let ps_param = params.ps.clone();
    let ps = state
        .store
        .run_blocking(move |store| resolve_ps(store, id, ps_param.as_deref()))
        .await?;
    let ctx = GitCtx::resolve_entry(&state.store, repo).await;

    let files = {
        let (c, b, t) = (ctx.clone(), ps.base_sha.clone(), ps.tip_sha.clone());
        tokio::task::spawn_blocking(move || files_changed(&c, &b, &t))
            .await
            .map_err(join_err)??
    };
    let total_files = files.len();
    let filter = params.path.clone().unwrap_or_default();
    let selected: Vec<_> = files
        .into_iter()
        .filter(|f| path_matches(&filter, &f.path, f.old_path.as_deref()))
        .collect();

    let (mut additions, mut deletions) = (0u64, 0u64);
    for f in &selected {
        additions += u64::from(f.insertions);
        deletions += u64::from(f.deletions);
    }
    let files_json: Vec<serde_json::Value> = selected
        .iter()
        .map(|f| match mode {
            DiffMode::NameOnly => serde_json::json!({ "path": f.path, "old_path": f.old_path }),
            _ => serde_json::json!({
                "path": f.path,
                "old_path": f.old_path,
                "status": f.status,
                "additions": f.insertions,
                "deletions": f.deletions,
                "binary": f.binary,
            }),
        })
        .collect();

    let mut body = serde_json::json!({
        "schema": DIFF_SCHEMA,
        "review_id": id,
        "ps_number": ps.ps_number,
        "base_sha": ps.base_sha,
        "tip_sha": ps.tip_sha,
        "mode": mode.as_str(),
        "path": params.path,
        "files_total": total_files,
        "files_count": selected.len(),
        "additions": additions,
        "deletions": deletions,
        "files": files_json,
        "source": if ctx.is_fallback() { "work-tree" } else { "store" },
    });

    if mode == DiffMode::Patch {
        // Denylisted files never contribute hunks (same floor `/cat` and
        // `GET /api/file` enforce); they are NAMED, with the pattern.
        let mut redacted = Vec::new();
        let mut allowed: Vec<String> = Vec::new();
        for f in &selected {
            let hit = state
                .secret_policy
                .matched(&f.path)
                .map(str::to_string)
                .or_else(|| {
                    f.old_path
                        .as_deref()
                        .and_then(|o| state.secret_policy.matched(o).map(str::to_string))
                });
            match hit {
                Some(pattern) => redacted.push(serde_json::json!({
                    "path": f.path,
                    "pattern": pattern,
                    "type": crate::security::secrets::ERR_REDACTED_BY_POLICY,
                })),
                None => {
                    allowed.push(f.path.clone());
                    if let Some(o) = &f.old_path {
                        allowed.push(o.clone());
                    }
                }
            }
        }
        let whole = filter.trim_matches('/').is_empty() && redacted.is_empty();
        let patch = if allowed.is_empty() {
            String::new()
        } else {
            let c = ctx.clone();
            let range = format!("{}..{}", ps.base_sha, ps.tip_sha);
            tokio::task::spawn_blocking(move || -> Result<String, ApiError> {
                // Both endpoints are daemon-minted full shas (checked
                // again here — never a name, never caller text).
                let (a, b) = range.split_once("..").unwrap_or(("", ""));
                if !is_full_sha(a) || !is_full_sha(b) {
                    return Err(ApiError::bad_request(format!("bad patchset range {range}")));
                }
                let mut args: Vec<String> = vec![
                    "diff".into(),
                    "--no-color".into(),
                    "--no-ext-diff".into(),
                    "--no-textconv".into(),
                    "-M".into(),
                    range.clone(),
                ];
                if !whole {
                    args.push("--".into());
                    args.extend(allowed.iter().map(|p| format!(":(literal){p}")));
                }
                let argv: Vec<&str> = args.iter().map(String::as_str).collect();
                let out = c
                    .read_with_fallback(|root| history::run_git_raw(root, &argv))
                    .map_err(ApiError::from)?;
                Ok(String::from_utf8_lossy(&out).into_owned())
            })
            .await
            .map_err(join_err)??
        };
        let patch_bytes = patch.len();
        let (limit, reason) = match params.budget {
            Some(tokens) => {
                let bytes = tokens.saturating_mul(BYTES_PER_TOKEN);
                let bytes = usize::try_from(bytes)
                    .unwrap_or(usize::MAX)
                    .min(MAX_PATCH_BYTES);
                (bytes, format!("budget {tokens} tokens"))
            }
            None => (MAX_PATCH_BYTES, format!("{MAX_PATCH_BYTES}-byte cap")),
        };
        let (text, truncated) = truncate_patch(&patch, limit, &reason);
        body["patch"] = serde_json::Value::String(text);
        body["patch_bytes"] = serde_json::json!(patch_bytes);
        body["truncated"] = serde_json::json!(truncated);
        body["budget_tokens"] = serde_json::json!(params.budget);
        body["redacted"] = serde_json::Value::Array(redacted);
    }
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(body)))
}

// --- GET /api/reviews/{id}/log ------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ReviewLogParams {
    #[serde(default)]
    pub ps: Option<String>,
}

/// `GET /api/reviews/{id}/log?ps=` — the patchset's commits
/// (`base_sha..tip_sha`, newest first, capped at [`MAX_LOG_COMMITS`]).
pub async fn review_log_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Query(params): Query<ReviewLogParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (_review, repo, _) = require_review(&state, id).await?;
    let ps_param = params.ps.clone();
    let ps = state
        .store
        .run_blocking(move |store| resolve_ps(store, id, ps_param.as_deref()))
        .await?;
    if !is_full_sha(&ps.base_sha) || !is_full_sha(&ps.tip_sha) {
        return Err(ApiError::bad_request(
            "patchset row carries a malformed sha",
        ));
    }
    let ctx = GitCtx::resolve_entry(&state.store, repo).await;
    let range = format!("{}..{}", ps.base_sha, ps.tip_sha);
    let max = format!("--max-count={}", MAX_LOG_COMMITS + 1);
    let c = ctx.clone();
    let raw = tokio::task::spawn_blocking(move || {
        c.read_with_fallback(|root| {
            history::run_git_raw(root, &["log", "--no-color", &max, LOG_FORMAT, &range])
        })
    })
    .await
    .map_err(join_err)?
    .map_err(ApiError::from)?;
    let mut commits = parse_log(&String::from_utf8_lossy(&raw));
    let truncated = commits.len() > MAX_LOG_COMMITS;
    commits.truncate(MAX_LOG_COMMITS);
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": LOG_SCHEMA,
            "review_id": id,
            "ps_number": ps.ps_number,
            "base_sha": ps.base_sha,
            "tip_sha": ps.tip_sha,
            "count": commits.len(),
            "truncated": truncated,
            "commits": commits,
            "source": if ctx.is_fallback() { "work-tree" } else { "store" },
        })),
    ))
}

// --- GET /api/reviews/{id}/cat ------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ReviewCatParams {
    pub path: String,
    #[serde(default)]
    pub ps: Option<String>,
    /// `new` (default, the patchset tip) | `old` (its base).
    #[serde(default)]
    pub side: Option<String>,
}

/// `GET /api/reviews/{id}/cat?path=&ps=&side=old|new` — one file's bytes at
/// the patchset's base (`old`) or tip (`new`). Same checks as `GET
/// /api/file` (lexical path gate, the configured secret denylist, a typed
/// 404 when the file is absent on that side), same `utf8`/`base64` body.
pub async fn review_cat_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Query(params): Query<ReviewCatParams>,
) -> Result<impl IntoResponse, ApiError> {
    let side = match params.side.as_deref() {
        None | Some("") | Some("new") => "new",
        Some("old") => "old",
        Some(other) => {
            return Err(ApiError::bad_request(format!(
                "side must be old|new, got {other:?}"
            )))
        }
    };
    state.secret_policy.check(&params.path)?;
    let (_review, repo, _) = require_review(&state, id).await?;
    let ps_param = params.ps.clone();
    let ps = state
        .store
        .run_blocking(move |store| resolve_ps(store, id, ps_param.as_deref()))
        .await?;
    let sha = if side == "old" {
        ps.base_sha.clone()
    } else {
        ps.tip_sha.clone()
    };
    if !is_full_sha(&sha) {
        return Err(ApiError::bad_request(
            "patchset row carries a malformed sha",
        ));
    }
    let ctx = GitCtx::resolve_entry(&state.store, repo).await;
    let repo_c = repo.clone();
    let path_c = params.path.clone();
    let sha_c = sha.clone();
    let read = tokio::task::spawn_blocking(move || {
        read_repo_file(
            &repo_c,
            &path_c,
            RevResolver::bridged(&ctx, Some(sha_c.as_str())),
        )
    })
    .await
    .map_err(join_err)??;
    let (encoding, content) = match String::from_utf8(read.bytes.clone()) {
        Ok(s) => ("utf8", s),
        Err(_) => (
            "base64",
            base64::engine::general_purpose::STANDARD.encode(&read.bytes),
        ),
    };
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": CAT_SCHEMA,
            "review_id": id,
            "ps_number": ps.ps_number,
            "side": side,
            "sha": sha,
            "path": params.path,
            "size": read.bytes.len(),
            "blob_hash": read.blob_hash,
            "encoding": encoding,
            "content": content,
        })),
    ))
}

// --- GET /api/reviews/find ------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ReviewFindParams {
    pub pr: u32,
    #[serde(default)]
    pub repo: Option<String>,
}

/// `GET /api/reviews/find?pr=N[&repo=R]` — every review bound to PR `N`,
/// across every configured repo (or just `R`), one store query. `repos`
/// lists the distinct repos that matched and `preferred` maps each to the
/// review `pr:<N>` addressing picks there (open before closed, newest
/// first — `Store::get_review_by_pr_binding`'s own order). Bearer, and the
/// "configured" of the no-`?repo=` case is ENFORCED against `state.repos`
/// (de-configured repos keep their rows, so the store alone would list
/// them) by the same exact-name rule `find_repo` applies for `?repo=R`.
pub async fn review_find_route(
    State(state): State<SharedState>,
    Query(params): Query<ReviewFindParams>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(r) = params.repo.as_deref() {
        find_repo(&state, r)?;
    }
    let pr = i64::from(params.pr);
    let repo_f = params.repo.clone();
    // A de-configured repo's review rows are DELIBERATELY kept (see
    // `store_for_repo_name`'s own doc: "reviews outlive a store
    // re-register"), so `find_reviews_by_pr`'s `(?2 IS NULL OR repo = ?2)`
    // with no `?repo=` would hand back every review this daemon has ever
    // stored — including repos the operator has since dropped from
    // `[[repos]]`. The `find_repo` 404 above only fires when `?repo=` IS
    // supplied. Intersect with the CONFIGURED set instead, by the same
    // exact-name rule `find_repo` applies: `GET /api/reviews` refuses the
    // very same repo with 404, and this route must not be the wider one.
    let configured: std::collections::HashSet<String> =
        state.repos.iter().map(|r| r.name.clone()).collect();
    let (rows, latest) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let rows: Vec<_> = store
                .find_reviews_by_pr(pr, repo_f.as_deref())?
                .into_iter()
                .filter(|(r, _, _)| configured.contains(&r.repo))
                .collect();
            let ids: Vec<i64> = rows.iter().map(|(r, _, _)| r.id).collect();
            let latest = store.latest_patchsets(&ids)?;
            Ok((rows, latest))
        })
        .await?;
    let mut repos: Vec<String> = Vec::new();
    let mut preferred = serde_json::Map::new();
    let mut reviews = Vec::with_capacity(rows.len());
    for (r, slug, head) in rows {
        if !repos.contains(&r.repo) {
            repos.push(r.repo.clone());
            preferred.insert(r.repo.clone(), serde_json::json!(r.id));
        }
        let ps = latest.get(&r.id);
        reviews.push(serde_json::json!({
            "id": r.id,
            "repo": r.repo,
            "state": r.state,
            "title": r.title,
            "base_ref": r.base_ref,
            "head_ref": r.head_ref,
            "pr_repo_slug": slug,
            "pr_head_sha": head,
            "latest_ps": ps.map(|p| p.ps_number),
            "latest_tip_sha": ps.map(|p| p.tip_sha.clone()),
            "verdict": r.verdict,
            "verdict_ps": r.verdict_ps,
            "updated_at": r.updated_at,
        }));
    }
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": FIND_SCHEMA,
            "pr_number": params.pr,
            "repo": params.repo,
            "repos": repos,
            "preferred": preferred,
            "reviews": reviews,
        })),
    ))
}

// --- the surface declaration (crate invariant 15) ------------------------------

fn accept<T: serde::de::DeserializeOwned>(map: serde_json::Map<String, serde_json::Value>) -> bool {
    serde_json::from_value::<T>(serde_json::Value::Object(map)).is_ok()
}

fn diff_params_accept_without(_omit: &str) -> bool {
    accept::<ReviewDiffParams>(serde_json::Map::new())
}

fn log_params_accept_without(_omit: &str) -> bool {
    accept::<ReviewLogParams>(serde_json::Map::new())
}

fn cat_params_accept_without(omit: &str) -> bool {
    let mut m = serde_json::Map::new();
    if omit != "path" {
        m.insert("path".into(), serde_json::json!("src/lib.rs"));
    }
    accept::<ReviewCatParams>(m)
}

fn find_params_accept_without(omit: &str) -> bool {
    let mut m = serde_json::Map::new();
    if omit != "pr" {
        m.insert("pr".into(), serde_json::json!(7));
    }
    accept::<ReviewFindParams>(m)
}

pub const REVIEW_DIFF_ROUTE: RouteContract = RouteContract {
    path: "/api/reviews/{id}/diff",
    handler: "review_views::review_diff_route",
    required_params: &[],
    params_accept_without: diff_params_accept_without,
};

pub const REVIEW_LOG_ROUTE: RouteContract = RouteContract {
    path: "/api/reviews/{id}/log",
    handler: "review_views::review_log_route",
    required_params: &[],
    params_accept_without: log_params_accept_without,
};

pub const REVIEW_CAT_ROUTE: RouteContract = RouteContract {
    path: "/api/reviews/{id}/cat",
    handler: "review_views::review_cat_route",
    required_params: &["path"],
    params_accept_without: cat_params_accept_without,
};

pub const REVIEW_FIND_ROUTE: RouteContract = RouteContract {
    path: "/api/reviews/find",
    handler: "review_views::review_find_route",
    required_params: &["pr"],
    params_accept_without: find_params_accept_without,
};

/// Every read RS-U10a adds — walked from BOTH sides (router registration
/// in `entities`' test, a CLI request builder in kb-code-cli's).
pub const RS_U10A_ROUTES: &[RouteContract] = &[
    REVIEW_DIFF_ROUTE,
    REVIEW_LOG_ROUTE,
    REVIEW_CAT_ROUTE,
    REVIEW_FIND_ROUTE,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_mode_parses_the_closed_set() {
        assert_eq!(DiffMode::parse(None), Ok(DiffMode::Stat));
        assert_eq!(DiffMode::parse(Some("stat")), Ok(DiffMode::Stat));
        assert_eq!(DiffMode::parse(Some("name-only")), Ok(DiffMode::NameOnly));
        assert_eq!(DiffMode::parse(Some("patch")), Ok(DiffMode::Patch));
        assert!(DiffMode::parse(Some("--output=/tmp/x")).is_err());
    }

    #[test]
    fn path_filter_is_exact_or_directory_prefix() {
        assert!(path_matches("", "a/b.rs", None));
        assert!(path_matches("a/b.rs", "a/b.rs", None));
        assert!(path_matches("a", "a/b.rs", None));
        assert!(path_matches("a/", "a/b.rs", None));
        assert!(!path_matches("a/b", "a/bc.rs", None));
        assert!(path_matches("old/x.rs", "new/x.rs", Some("old/x.rs")));
    }

    #[test]
    fn truncation_is_deterministic_line_aligned_and_marked() {
        let patch = "line one\nline two\nline three\n";
        let (same, cut) = truncate_patch(patch, 1000, "budget 250 tokens");
        assert!(!cut);
        assert_eq!(same, patch);

        let (a, cut_a) = truncate_patch(patch, 12, "budget 3 tokens");
        let (b, _) = truncate_patch(patch, 12, "budget 3 tokens");
        assert!(cut_a);
        assert_eq!(a, b, "same input, same bytes");
        assert!(a.starts_with("line one\n"));
        assert!(!a.contains("line two"));
        assert!(a.contains("[kb-code: patch truncated (budget 3 tokens): 9 of 29 bytes shown"));
    }

    #[test]
    fn truncation_never_splits_a_char_or_panics() {
        let patch = "ààààààààààààààààà";
        for n in 0..patch.len() + 2 {
            let (out, _) = truncate_patch(patch, n, "t");
            assert!(out.is_char_boundary(out.len()));
        }
    }

    #[test]
    fn log_parser_skips_malformed_records() {
        let sha = "a".repeat(40);
        let parent = "b".repeat(40);
        let raw = format!(
            "{sha}\x1f{parent}\x1fAda\x1f2026-01-01T00:00:00+00:00\x1fAdd à thing\x1e\nnot-a-record\x1e"
        );
        let got = parse_log(&raw);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0]["sha"], sha);
        assert_eq!(got[0]["parents"][0], parent);
        assert_eq!(got[0]["subject"], "Add à thing");
    }

    #[test]
    fn declared_routes_accept_complete_and_reject_missing_required() {
        for c in RS_U10A_ROUTES {
            assert!((c.params_accept_without)(""), "{}", c.path);
            for p in c.required_params {
                assert!(!(c.params_accept_without)(p), "{} without {p}", c.path);
            }
        }
    }
}
