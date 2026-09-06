//! GitHub READ overlay (Phase G-server) — origin parsing, the PR list/
//! comments lanes (`GET /api/prs`, `GET /api/prs/{number}/comments`), and
//! the ONE ref-write this module performs (`POST /api/prs/fetch`,
//! loopback-only — see [`fetch_pr_ref`]'s doc).
//!
//! # Origin parsing
//!
//! [`github_repo`] reads the configured repo's `origin` remote URL (`git
//! config --get remote.origin.url` — PRR-R2: deliberately NOT `git remote
//! get-url origin`, see [`origin_url`]'s own doc for why) and
//! [`parse_github_origin`] extracts an `(owner, name)` pair from either the https
//! (`https://github.com/owner/repo[.git]`) or ssh (`git@github.com:owner/
//! repo[.git]`, `ssh://git@github.com/owner/repo[.git]`) form. A
//! non-GitHub origin (a different host, or a repo with no `origin`
//! configured at all) is [`GithubError::NotGithubOrigin`]/
//! [`GithubError::NoOrigin`] — every route in `routes.rs` maps both to a
//! clean 400 ("not a github origin"), never a panic.
//!
//! # Auth + degradation
//!
//! [`GithubClient`] mirrors `join::kb_client::KbClient`'s own federation
//! precedent: one pooled `reqwest::Client` built once at construction, a
//! fixed [`TIMEOUT`] (5s, no retries), `Authorization: Bearer <token>`
//! applied ONLY when `[github] token_file` is configured (an
//! unauthenticated request still works for a PUBLIC repo — just GitHub's
//! lower unauthenticated rate limit). A network failure or a non-2xx
//! response (403 rate-limit, 404 unknown repo, ...) is reported as a
//! [`GithubApiError`] the ROUTE then folds into an honest
//! `unavailable_reason` string with an EMPTY result set and an ordinary
//! HTTP 200 — never a 5xx for "GitHub had a bad day" — mirroring how
//! `search::sessions`'s lane degrades kb-unreachable rather than failing
//! the whole Search-Everywhere box.
//!
//! `[github] api_base` (default [`GithubSection::DEFAULT_API_BASE`]
//! (`crate::config`)) is the ONLY seam this module's own tests (and
//! `tests/review_routes.rs`) use to redirect every call at a local mock
//! server instead of the real network — no test in this crate ever
//! reaches api.github.com.

use crate::config::GithubSection;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Fixed, non-configurable — see the module doc. This lane must never be
/// the reason a `GET /api/prs`/`GET /api/prs/{n}/comments` request hangs.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// `GET /api/prs`'s cap — a plain fixed ceiling, not a paginated surface,
/// mirroring `history::compare::MAX_COMMITS`'s own convention.
pub const MAX_PRS: usize = 100;

/// `GET /api/prs/{n}/comments`'s cap (review + issue comments combined).
pub const MAX_COMMENTS: usize = 200;

/// PRR-R2 (design doc §2 row 3) — `GET /api/prs/{n}/checks`'s cap. Same
/// fixed-ceiling convention as [`MAX_PRS`]/[`MAX_COMMENTS`].
pub const MAX_CHECKS: usize = 50;

/// PRR addendum-2 §A — `GET /api/prs/{n}/reviews`'s cap on the number of
/// individual review submissions folded into the per-reviewer summary
/// (NOT a cap on distinct reviewers — many submissions can collapse to few
/// reviewers). Same fixed-ceiling convention as the others above.
pub const MAX_REVIEW_SUBMISSIONS: usize = 100;

// --- origin parsing --------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum GithubError {
    #[error("failed to spawn git: {0}")]
    Spawn(std::io::Error),
    #[error("git remote get-url origin failed: {0}")]
    NoOrigin(String),
    #[error("origin {0:?} is not a github repository")]
    NotGithubOrigin(String),
    #[error("git fetch failed: {0}")]
    FetchFailed(String),
}

pub type Result<T> = std::result::Result<T, GithubError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GithubRepo {
    pub owner: String,
    pub name: String,
}

/// `git -C repo_root config --get remote.origin.url`, trimmed.
///
/// PRR-R2 — deliberately NOT `git remote get-url origin`: per `git-remote(1)`,
/// `get-url` "expands" `url.<base>.insteadOf`/`pushInsteadOf` rewrites, so on
/// a repo using those (a corporate GitHub Enterprise mirror, or — the case
/// that actually surfaced this — this crate's OWN test fixtures, which use
/// `insteadOf` to redirect a `https://github.com/owner/repo.git` origin's
/// ACTUAL fetch traffic to a local bare repo while keeping the CONFIGURED
/// URL github-shaped) `get-url` would report the REWRITTEN target, which
/// [`parse_github_origin`] can never recognise as a GitHub host. `config
/// --get` reads the raw configured value — exactly "what GitHub repo does
/// this origin represent," which is what every caller of [`github_repo`]
/// actually wants, and is also MORE correct in the mirror case (an
/// `insteadOf`-redirected origin is still, semantically, the same GitHub
/// repo). `fetch_pr_ref`'s own `git fetch origin …` call is a SEPARATE git
/// invocation that keeps applying `insteadOf` internally regardless of how
/// this function reads the configured URL — the two are not coupled.
fn origin_url(repo_root: &Path) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .map_err(GithubError::Spawn)?;
    if !out.status.success() {
        return Err(GithubError::NoOrigin(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Parse a raw git remote URL into `(owner, name)` — see the module doc
/// for the accepted https/ssh forms. Pure (no I/O), directly unit-testable
/// against fixture strings. `None` for anything that isn't recognisably a
/// `github.com` origin.
pub fn parse_github_origin(url: &str) -> Option<GithubRepo> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, name) = rest.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some(GithubRepo {
        owner: owner.to_string(),
        name: name.to_string(),
    })
}

/// `repo_root`'s `origin` remote, resolved to a GitHub `(owner, name)` —
/// [`GithubError::NoOrigin`] when there's no `origin` remote configured at
/// all, [`GithubError::NotGithubOrigin`] when there IS one but it doesn't
/// point at `github.com`.
pub fn github_repo(repo_root: &Path) -> Result<GithubRepo> {
    let url = origin_url(repo_root)?;
    parse_github_origin(&url).ok_or_else(|| GithubError::NotGithubOrigin(url.clone()))
}

// --- the ONE ref-write: `POST /api/prs/fetch` ------------------------------

/// `git fetch origin +refs/pull/<n>/head:refs/kbc/pr/<n>` — the operator-
/// approved ref-write namespace for a fetched PR head (plan §Architecture):
/// `refs/kbc/*` is deliberately NOT `refs/heads/*` (never surfaces as a
/// local branch a plain `git branch`/checkout would show, and can never
/// collide with an operator's own branch names) and is the ONE new git
/// ref-write this daemon performs outside `checkout::switch_repo` (W4.7's
/// "the ONLY working-tree mutation" ruling is unaffected — a ref-db write
/// under `refs/kbc/*` touches neither the working tree nor HEAD, so it
/// doesn't compete with that rule; it is a companion ruling for this ONE
/// additional ref-namespace, not a relaxation of it).
///
/// `number` is typed `u32` end-to-end (the route's JSON body deserializes
/// straight into a `u32`) — this is what makes the refspec injection-safe
/// BY CONSTRUCTION: a `u32`'s `Display` can only ever produce ASCII
/// digits, so `format!("+refs/pull/{number}/head:refs/kbc/pr/{number}")`
/// can never smuggle a `-`-prefixed flag, a `..`, or any other
/// revspec/refspec metacharacter into the argv this hands to `git fetch`.
pub fn fetch_pr_ref(repo_root: &Path, number: u32) -> Result<(String, String)> {
    let refspec = format!("+refs/pull/{number}/head:refs/kbc/pr/{number}");
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["fetch", "origin", &refspec])
        .output()
        .map_err(GithubError::Spawn)?;
    if !out.status.success() {
        return Err(GithubError::FetchFailed(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }

    let target_ref = format!("refs/kbc/pr/{number}");
    let sha_out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "--verify", &target_ref])
        .output()
        .map_err(GithubError::Spawn)?;
    if !sha_out.status.success() {
        return Err(GithubError::FetchFailed(format!(
            "fetch succeeded but {target_ref} could not be resolved: {}",
            String::from_utf8_lossy(&sha_out.stderr).trim()
        )));
    }
    let sha = String::from_utf8_lossy(&sha_out.stdout).trim().to_string();
    Ok((target_ref, sha))
}

// --- the GitHub REST API client --------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PrOut {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub head_ref: String,
    pub base_ref: String,
    pub updated_at: String,
    pub draft: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PrCommentOut {
    /// PRR addendum-2 §A — the comment's own GitHub id. Every prior
    /// consumer of this struct (`GET /api/prs/{n}/comments`) simply never
    /// read it; `GET /api/reviews/{id}/github-threads` needs it to nest
    /// replies (`in_reply_to`) and key its position cache.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub author: String,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    /// PRR addendum-2 §A — GitHub's own `original_line`, kept SEPARATE
    /// from `line` (which already falls back to this below when `line`
    /// itself is null — an outdated inline comment) so a caller that needs
    /// to tell the two apart (`github-threads`' position mapper, which
    /// prefers `line` but falls back to this same value) still can.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_line: Option<u64>,
    /// PRR addendum-2 §A — `"LEFT"` (old file) | `"RIGHT"` (new file,
    /// GitHub's own default when the field is absent), verbatim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side: Option<String>,
    /// PRR addendum-2 §A — the unified-diff hunk GitHub anchors this
    /// comment to. `github-threads`' position mapper derives the anchored
    /// line's text from this hunk's LAST line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_hunk: Option<String>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<u64>,
    /// PRR addendum-2 §A — the comment's GitHub web URL. A GitHub-origin
    /// thread's "Reply" action deep-links here — the SPA never writes to
    /// GitHub.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html_url: Option<String>,
}

/// PRR-R2 (design doc §2 row 2) — `GET /api/prs/{n}`'s shape. A SEPARATE
/// struct from [`PrOut`] (not an extension of it): the single-PR REST
/// endpoint (`GET /repos/{o}/{r}/pulls/{n}`) returns fields the LIST
/// endpoint (`list_pulls`'s `GET .../pulls?state=open`) never does —
/// `mergeable_state`/`merged`/`labels` are computed/populated lazily by
/// GitHub and are absent from the list response — so widening [`PrOut`]
/// would either lie about what `list_pulls` actually returns or force every
/// existing `PrOut` construction site to grow new `None`s for fields that
/// endpoint can never fill. Works for open, closed, AND merged PRs (unlike
/// `list_pulls`, which is `state=open`-only by design) — GitHub's single-PR
/// endpoint answers regardless of state.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PrDetailOut {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub head_ref: String,
    /// The PR head commit's sha, per GitHub — distinct from (but normally
    /// identical to) the sha a local `refs/kbc/pr/<n>` fetch resolves to.
    pub head_sha: String,
    pub base_ref: String,
    pub updated_at: String,
    pub draft: bool,
    /// `"open"` | `"closed"` — GitHub's own `state` field.
    pub state: String,
    pub merged: bool,
    pub labels: Vec<String>,
    /// PRR addendum-2 §Expansion-2 (operator "GitHub conversation in the
    /// Room" addition) — GitHub REST's `mergeable_state` (`clean` | `dirty`
    /// | `unstable` | `blocked` | `behind` | `draft` | `unknown` |
    /// `has_hooks`), surfaced verbatim under the name the design docs use
    /// for the concept (`mergeStateStatus`, GitHub's GraphQL name for the
    /// same idea). `None` only when GitHub's own response omits the field
    /// entirely (has not happened in practice, but the REST field is
    /// documented as computed asynchronously and can be genuinely absent on
    /// a very fresh PR) — never fabricated as `"unknown"` when the wire
    /// simply didn't say so.
    pub merge_state_status: Option<String>,
    /// V70-A3X — the PR's description body (GitHub's own `body`, raw
    /// markdown, verbatim). `None` when GitHub's response has `body: null`
    /// (an empty-description PR) — additive field, every existing
    /// `PrDetailOut` construction site (only [`GithubClient::get_pull`]
    /// itself) gained it in lock-step.
    pub body: Option<String>,
}

/// PRR-R2 (design doc §2 row 3) — one normalized GitHub Checks API run, the
/// shape both `GET /api/prs/{n}/checks` and `create_review_pr`'s embedded
/// `pr_meta_json.checks` snapshot (design doc §1.2) use. `status` is
/// NORMALIZED into the design doc's closed 4-set (`pass|fail|warn|pending`)
/// — see [`normalize_check_status`] — because GitHub's own two-axis
/// `status`/`conclusion` pair is not itself the vocabulary any consumer of
/// this crate's wire format wants to branch on.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CheckRunOut {
    pub name: String,
    /// `"pass"` | `"fail"` | `"warn"` | `"pending"` — see
    /// [`normalize_check_status`].
    pub status: String,
    /// GitHub's own raw `conclusion` (when `status="completed"`) or
    /// `status` (queued/in_progress) string — preserved verbatim beside the
    /// normalized field above so a caller that wants GitHub's own precision
    /// (e.g. distinguishing `skipped` from `neutral`, both normalized to
    /// `warn`) still can.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Seconds between `started_at` and `completed_at`, when GitHub
    /// reported both as parseable RFC3339 timestamps; `None` on a
    /// still-running check or an unparseable timestamp — never a guessed 0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<i64>,
}

/// PRR addendum-2 §A — one reviewer's LATEST review state, `GET
/// /api/prs/{n}/reviews`'s per-reviewer summary row (raw read; the
/// `/github-threads` composition route that anchors these into the diff is
/// a LATER unit).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReviewerStateOut {
    pub reviewer: String,
    /// GitHub's own review-state vocabulary, verbatim:
    /// `APPROVED | CHANGES_REQUESTED | COMMENTED | DISMISSED | PENDING`.
    pub state: String,
    pub submitted_at: Option<String>,
}

/// PRR addendum-2 §A — `GET /api/prs/{n}/reviews`'s response body.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PrReviewsOut {
    /// One row per reviewer who has SUBMITTED at least one review, folded
    /// down to their most-recently-submitted state (GitHub's own reviews
    /// list is append-only across re-review rounds — a reviewer can
    /// APPROVE, then a later push makes them re-review and DISMISS/re-
    /// APPROVE; only the latest state per login is meaningful).
    pub reviewers: Vec<ReviewerStateOut>,
    /// Logins with a PENDING review request who have not yet submitted
    /// anything (`GET .../requested_reviewers`) — distinct from a
    /// `"PENDING"` state in `reviewers` above (which GitHub only reports
    /// for an in-progress, not-yet-submitted review DRAFT by someone who
    /// HAS started one; requested-but-untouched reviewers never appear
    /// there at all).
    pub requested_reviewers: Vec<String>,
    /// A LOCAL, best-effort approximation of GitHub's GraphQL
    /// `reviewDecision` (`APPROVED | CHANGES_REQUESTED | REVIEW_REQUIRED |
    /// null`) — computed purely from `reviewers`' latest states (any
    /// CHANGES_REQUESTED wins; else any APPROVED; else `None`). This is
    /// NOT the authoritative branch-protection-aware value GitHub's GraphQL
    /// API computes (which additionally knows required-review-count rules
    /// this REST-only client has no way to see) — deliberately never
    /// reports `"REVIEW_REQUIRED"` (that would claim knowledge of branch
    /// protection rules this client doesn't have); an unresolved PR with no
    /// CHANGES_REQUESTED and no APPROVED is honestly `None`, not a guess.
    pub review_decision: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhUser {
    login: String,
}

#[derive(Debug, Deserialize)]
struct GhRef {
    #[serde(rename = "ref")]
    r: String,
}

#[derive(Debug, Deserialize)]
struct GhPull {
    number: u64,
    title: String,
    user: Option<GhUser>,
    head: GhRef,
    base: GhRef,
    updated_at: String,
    #[serde(default)]
    draft: bool,
}

#[derive(Debug, Deserialize)]
struct GhReviewComment {
    #[serde(default)]
    id: Option<u64>,
    user: Option<GhUser>,
    body: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    line: Option<u64>,
    #[serde(default)]
    original_line: Option<u64>,
    /// PRR addendum-2 §A — `"LEFT"` | `"RIGHT"`.
    #[serde(default)]
    side: Option<String>,
    #[serde(default)]
    diff_hunk: Option<String>,
    created_at: String,
    #[serde(default)]
    in_reply_to_id: Option<u64>,
    #[serde(default)]
    html_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhIssueComment {
    #[serde(default)]
    id: Option<u64>,
    user: Option<GhUser>,
    body: String,
    created_at: String,
    #[serde(default)]
    html_url: Option<String>,
}

/// `GET /repos/{o}/{r}/pulls/{n}`'s response — only the fields
/// [`PrDetailOut`] needs; every other field GitHub returns is unread.
#[derive(Debug, Deserialize)]
struct GhPullDetail {
    number: u64,
    title: String,
    user: Option<GhUser>,
    head: GhPullDetailHead,
    base: GhRef,
    updated_at: String,
    #[serde(default)]
    draft: bool,
    state: String,
    #[serde(default)]
    merged: bool,
    #[serde(default)]
    labels: Vec<GhLabel>,
    #[serde(default)]
    mergeable_state: Option<String>,
    /// V70-A3X.
    #[serde(default)]
    body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhPullDetailHead {
    #[serde(rename = "ref")]
    r: String,
    sha: String,
}

#[derive(Debug, Deserialize)]
struct GhLabel {
    name: String,
}

/// `GET /repos/{o}/{r}/commits/{sha}/check-runs`'s response envelope.
#[derive(Debug, Deserialize)]
struct GhCheckRunsResponse {
    #[serde(default)]
    check_runs: Vec<GhCheckRun>,
}

#[derive(Debug, Deserialize)]
struct GhCheckRun {
    name: String,
    /// `"queued"` | `"in_progress"` | `"completed"`.
    status: String,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    completed_at: Option<String>,
}

/// `GET /repos/{o}/{r}/pulls/{n}/reviews`'s response — one row per
/// SUBMITTED review (may include multiple rows per reviewer across
/// re-review rounds; [`GithubClient::list_reviews`] folds to latest-only).
#[derive(Debug, Deserialize)]
struct GhReview {
    user: Option<GhUser>,
    /// `APPROVED | CHANGES_REQUESTED | COMMENTED | DISMISSED | PENDING`.
    state: String,
    #[serde(default)]
    submitted_at: Option<String>,
}

/// `GET /repos/{o}/{r}/pulls/{n}/requested_reviewers`'s response — this
/// client only reads the individual-user half (`users`); team review
/// requests (`teams`) are a distinct GitHub concept this v1 doesn't surface.
#[derive(Debug, Deserialize)]
struct GhRequestedReviewers {
    #[serde(default)]
    users: Vec<GhUser>,
}

/// Normalize GitHub's two-axis `status`/`conclusion` pair into the design
/// doc's closed 4-set (`pass|fail|warn|pending`). `status != "completed"`
/// is always `"pending"` regardless of `conclusion` (GitHub only ever
/// populates `conclusion` once a run completes). A `"completed"` run with
/// no `conclusion` at all (a documented-but-rare GitHub inconsistency) maps
/// to `"warn"` rather than crashing or guessing pass/fail.
fn normalize_check_status(status: &str, conclusion: Option<&str>) -> &'static str {
    if status != "completed" {
        return "pending";
    }
    match conclusion {
        Some("success") => "pass",
        Some("failure") | Some("timed_out") | Some("action_required") | Some("cancelled") => "fail",
        Some("neutral") | Some("skipped") | Some("stale") | None => "warn",
        Some(_other) => "warn",
    }
}

/// Parse a GitHub `Link` response header (RFC 8288) for the `rel="next"`
/// target URL, if present. GitHub's own format: comma-separated
/// `<url>; rel="name"[; ...]` entries, e.g. `<https://api.github.com/
/// repositories/1/pulls/1/comments?page=2>; rel="next", <...>; rel="last"`.
/// `None` on the last page (no `next` entry) or a malformed header.
fn parse_link_next(header_value: &str) -> Option<String> {
    for entry in header_value.split(',') {
        let mut parts = entry.split(';').map(str::trim);
        let url_part = parts.next()?;
        let is_next = parts.any(|p| p == r#"rel="next""#);
        if is_next {
            let url = url_part.strip_prefix('<')?.strip_suffix('>')?;
            return Some(url.to_string());
        }
    }
    None
}

/// `started_at`/`completed_at` (RFC3339) → whole seconds, when both parse.
fn check_duration_secs(started_at: Option<&str>, completed_at: Option<&str>) -> Option<i64> {
    let started = chrono::DateTime::parse_from_rfc3339(started_at?).ok()?;
    let completed = chrono::DateTime::parse_from_rfc3339(completed_at?).ok()?;
    Some((completed - started).num_seconds())
}

#[derive(Debug, thiserror::Error)]
pub enum GithubApiError {
    #[error("build http client: {0}")]
    ClientBuild(String),
    #[error("github unreachable at {0}: {1}")]
    Unreachable(String, String),
    #[error("github rate-limited (403)")]
    RateLimited,
    #[error("github returned {0}")]
    BadStatus(reqwest::StatusCode),
    #[error("parse github response: {0}")]
    Parse(String),
}

pub type ApiResult<T> = std::result::Result<T, GithubApiError>;

/// Per-boot GitHub REST API handle (`AppState::github`) — see the module
/// doc. Stateless beyond the pooled client/token/`api_base`, unlike
/// `join::kb_client::KbClient` (no cached snapshot to own here).
pub struct GithubClient {
    api_base: String,
    token: Option<String>,
    client: std::result::Result<reqwest::Client, String>,
}

impl GithubClient {
    pub fn new(cfg: &GithubSection) -> Self {
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .user_agent("kb-code-server")
            .build()
            .map_err(|e| e.to_string());
        Self {
            api_base: cfg.api_base.clone(),
            token: cfg.bearer_token(),
            client,
        }
    }

    fn client(&self) -> ApiResult<&reqwest::Client> {
        self.client
            .as_ref()
            .map_err(|e| GithubApiError::ClientBuild(e.clone()))
    }

    fn get(&self, client: &reqwest::Client, path: &str) -> reqwest::RequestBuilder {
        self.get_url(
            client,
            &format!("{}{}", self.api_base.trim_end_matches('/'), path),
        )
    }

    /// Same header/auth application as [`Self::get`], but over an ALREADY
    /// absolute URL — V70-A3X's [`Self::fetch_paginated`] needs this: a
    /// GitHub `Link: rel="next"` target is a full URL (including whatever
    /// `page`/`per_page` params GitHub itself chose), never a path to
    /// re-prefix with `api_base`.
    fn get_url(&self, client: &reqwest::Client, url: &str) -> reqwest::RequestBuilder {
        let rb = client
            .get(url)
            .header("Accept", "application/vnd.github+json");
        match &self.token {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        }
    }

    /// GET `first_path` (relative, `api_base`-prefixed) and follow its
    /// `Link: rel="next"` response header (RFC 8288 — GitHub's own
    /// pagination convention) across subsequent ABSOLUTE-URL pages,
    /// accumulating JSON-array items until EITHER a page has no `next` link
    /// OR `budget` items have been collected — whichever comes first.
    /// Returns `(items, truncated)`: `items` is capped at `budget`;
    /// `truncated` is `true` iff more existed beyond it (a `next` link went
    /// unfollowed, or the very last page alone pushed past `budget`) — the
    /// honest signal a single-page, no-follow fetch (this fn's predecessor)
    /// could structurally never set once the caller's cap and GitHub's own
    /// `per_page` happened to be equal.
    ///
    /// `budget == 0` short-circuits with NO network call — a caller
    /// splitting one combined budget across multiple sources (`list_pull_
    /// comments`'s review+issue split) hits this once the first source
    /// alone already exhausted it.
    async fn fetch_paginated<T: serde::de::DeserializeOwned>(
        &self,
        client: &reqwest::Client,
        first_path: &str,
        budget: usize,
    ) -> ApiResult<(Vec<T>, bool)> {
        if budget == 0 {
            return Ok((Vec::new(), false));
        }
        let mut items: Vec<T> = Vec::new();
        let mut next_url = Some(format!(
            "{}{}",
            self.api_base.trim_end_matches('/'),
            first_path
        ));
        let mut truncated = false;
        while let Some(url) = next_url.take() {
            let resp = self
                .get_url(client, &url)
                .send()
                .await
                .map_err(|e| GithubApiError::Unreachable(url.clone(), e.to_string()))?;
            if resp.status() == reqwest::StatusCode::FORBIDDEN {
                return Err(GithubApiError::RateLimited);
            }
            if !resp.status().is_success() {
                return Err(GithubApiError::BadStatus(resp.status()));
            }
            let link_next = resp
                .headers()
                .get(reqwest::header::LINK)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_link_next);
            let page: Vec<T> = resp
                .json()
                .await
                .map_err(|e| GithubApiError::Parse(e.to_string()))?;
            items.extend(page);
            if items.len() >= budget {
                truncated = items.len() > budget || link_next.is_some();
                items.truncate(budget);
                break;
            }
            next_url = link_next;
        }
        Ok((items, truncated))
    }

    /// `GET {api_base}/repos/{owner}/{repo}/pulls?state=open` — every open
    /// PR, mapped into [`PrOut`]. 403 is reported distinctly
    /// ([`GithubApiError::RateLimited`]) since that's the near-exclusive
    /// real-world cause for an otherwise-valid, existing repo (GitHub's
    /// documented behaviour for both real rate-limiting AND, confusingly,
    /// some permission failures — either way "back off, try later" is the
    /// honest framing for a caller).
    pub async fn list_pulls(&self, owner: &str, repo: &str) -> ApiResult<Vec<PrOut>> {
        let client = self.client()?;
        let path = format!("/repos/{owner}/{repo}/pulls?state=open&per_page=100");
        let url = format!("{}{}", self.api_base.trim_end_matches('/'), path);
        let resp = self
            .get(client, &path)
            .send()
            .await
            .map_err(|e| GithubApiError::Unreachable(url, e.to_string()))?;
        if resp.status() == reqwest::StatusCode::FORBIDDEN {
            return Err(GithubApiError::RateLimited);
        }
        if !resp.status().is_success() {
            return Err(GithubApiError::BadStatus(resp.status()));
        }
        let body: Vec<GhPull> = resp
            .json()
            .await
            .map_err(|e| GithubApiError::Parse(e.to_string()))?;
        Ok(body
            .into_iter()
            .map(|p| PrOut {
                number: p.number,
                title: p.title,
                author: p.user.map(|u| u.login).unwrap_or_default(),
                head_ref: p.head.r,
                base_ref: p.base.r,
                updated_at: p.updated_at,
                draft: p.draft,
            })
            .collect())
    }

    /// `GET {api_base}/repos/{owner}/{repo}/pulls/{n}/comments` (inline
    /// review comments) + `GET .../issues/{n}/comments` (general PR
    /// discussion) merged into one list — review comments first, then
    /// issue comments; `path`'s presence is what distinguishes them on the
    /// wire (`PrCommentOut::path`), per the phase brief.
    ///
    /// V70-A3X: both sources now follow `Link: rel="next"` (via
    /// [`Self::fetch_paginated`]) rather than trusting a single
    /// `per_page=100` page, sharing ONE combined [`MAX_COMMENTS`] budget —
    /// review comments draw from it first, and whatever remains (possibly
    /// zero) is issue comments' budget. Returns `(comments, truncated)`:
    /// `truncated` is `true` iff EITHER source had more beyond its share of
    /// the budget — an honest signal the old single-page fetch could
    /// structurally never set (its two `per_page=100` pages summed to
    /// exactly `MAX_COMMENTS`, so "more than the cap" could never be
    /// observed even on a PR with hundreds of comments).
    pub async fn list_pull_comments(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> ApiResult<(Vec<PrCommentOut>, bool)> {
        let client = self.client()?;

        let review_path = format!("/repos/{owner}/{repo}/pulls/{number}/comments?per_page=100");
        let (review, review_truncated): (Vec<GhReviewComment>, bool) = self
            .fetch_paginated(client, &review_path, MAX_COMMENTS)
            .await?;

        let remaining_budget = MAX_COMMENTS.saturating_sub(review.len());
        let issue_path = format!("/repos/{owner}/{repo}/issues/{number}/comments?per_page=100");
        let (issue, issue_truncated): (Vec<GhIssueComment>, bool) = self
            .fetch_paginated(client, &issue_path, remaining_budget)
            .await?;

        let truncated = review_truncated || issue_truncated;
        let mut out = Vec::with_capacity(review.len() + issue.len());
        for c in review {
            out.push(PrCommentOut {
                id: c.id,
                author: c.user.map(|u| u.login).unwrap_or_default(),
                body: c.body,
                path: c.path,
                line: c.line.or(c.original_line),
                original_line: c.original_line,
                side: c.side,
                diff_hunk: c.diff_hunk,
                created_at: c.created_at,
                in_reply_to: c.in_reply_to_id,
                html_url: c.html_url,
            });
        }
        for c in issue {
            out.push(PrCommentOut {
                id: c.id,
                author: c.user.map(|u| u.login).unwrap_or_default(),
                body: c.body,
                path: None,
                line: None,
                original_line: None,
                side: None,
                diff_hunk: None,
                created_at: c.created_at,
                in_reply_to: None,
                html_url: c.html_url,
            });
        }
        Ok((out, truncated))
    }

    /// `GET {api_base}/repos/{owner}/{repo}/pulls/{n}` (PRR-R2, design doc
    /// §2 row 2) — one PR's full detail, mapped into [`PrDetailOut`]. Unlike
    /// [`Self::list_pulls`] (`state=open`-only), this works for a PR in any
    /// state (open/closed/merged) — GitHub's single-PR endpoint always
    /// answers regardless. Same rate-limit/bad-status/parse degrade shape
    /// as every other method here.
    pub async fn get_pull(&self, owner: &str, repo: &str, number: u64) -> ApiResult<PrDetailOut> {
        let client = self.client()?;
        let path = format!("/repos/{owner}/{repo}/pulls/{number}");
        let url = format!("{}{}", self.api_base.trim_end_matches('/'), path);
        let resp = self
            .get(client, &path)
            .send()
            .await
            .map_err(|e| GithubApiError::Unreachable(url, e.to_string()))?;
        if resp.status() == reqwest::StatusCode::FORBIDDEN {
            return Err(GithubApiError::RateLimited);
        }
        if !resp.status().is_success() {
            return Err(GithubApiError::BadStatus(resp.status()));
        }
        let p: GhPullDetail = resp
            .json()
            .await
            .map_err(|e| GithubApiError::Parse(e.to_string()))?;
        Ok(PrDetailOut {
            number: p.number,
            title: p.title,
            author: p.user.map(|u| u.login).unwrap_or_default(),
            head_ref: p.head.r,
            head_sha: p.head.sha,
            base_ref: p.base.r,
            updated_at: p.updated_at,
            draft: p.draft,
            state: p.state,
            merged: p.merged,
            labels: p.labels.into_iter().map(|l| l.name).collect(),
            merge_state_status: p.mergeable_state,
            body: p.body,
        })
    }

    /// `GET {api_base}/repos/{owner}/{repo}/commits/{sha}/check-runs` (PRR-R2,
    /// design doc §2 row 3) — every check-run reported against `sha`, mapped
    /// into [`CheckRunOut`] via [`normalize_check_status`]. Caps at
    /// [`MAX_CHECKS`] (the caller truncates; this method returns whatever
    /// GitHub's first page reports — same "one bounded page, not a paginated
    /// surface" convention as [`Self::list_pulls`]/[`Self::
    /// list_pull_comments`]).
    pub async fn list_checks(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
    ) -> ApiResult<Vec<CheckRunOut>> {
        let client = self.client()?;
        let path = format!("/repos/{owner}/{repo}/commits/{sha}/check-runs?per_page=100");
        let url = format!("{}{}", self.api_base.trim_end_matches('/'), path);
        let resp = self
            .get(client, &path)
            .send()
            .await
            .map_err(|e| GithubApiError::Unreachable(url, e.to_string()))?;
        if resp.status() == reqwest::StatusCode::FORBIDDEN {
            return Err(GithubApiError::RateLimited);
        }
        if !resp.status().is_success() {
            return Err(GithubApiError::BadStatus(resp.status()));
        }
        let body: GhCheckRunsResponse = resp
            .json()
            .await
            .map_err(|e| GithubApiError::Parse(e.to_string()))?;
        Ok(body
            .check_runs
            .into_iter()
            .map(|c| {
                let status = normalize_check_status(&c.status, c.conclusion.as_deref());
                let note = if c.status == "completed" {
                    c.conclusion.clone()
                } else {
                    Some(c.status.clone())
                };
                let duration =
                    check_duration_secs(c.started_at.as_deref(), c.completed_at.as_deref());
                CheckRunOut {
                    name: c.name,
                    status: status.to_string(),
                    note,
                    duration,
                }
            })
            .collect())
    }

    /// `GET {api_base}/repos/{owner}/{repo}/pulls/{n}/reviews` +
    /// `GET .../pulls/{n}/requested_reviewers` (PRR addendum-2 §A) —
    /// per-reviewer LATEST state + pending review requests + a locally
    /// computed [`PrReviewsOut::review_decision`] approximation (see that
    /// field's own doc for why it is honestly NOT GitHub's authoritative
    /// `reviewDecision`). Raw reads only — the `/github-threads`
    /// composition route that anchors these into the diff is a LATER unit.
    /// Caps submissions at [`MAX_REVIEW_SUBMISSIONS`] BEFORE folding to
    /// latest-per-reviewer (so a very actively re-reviewed PR degrades by
    /// dropping its OLDEST submissions, never silently dropping a reviewer
    /// entirely — folding first then capping could drop a reviewer whose
    /// only submission was early).
    pub async fn list_reviews(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> ApiResult<PrReviewsOut> {
        let client = self.client()?;

        let reviews_path = format!("/repos/{owner}/{repo}/pulls/{number}/reviews?per_page=100");
        let reviews_url = format!("{}{}", self.api_base.trim_end_matches('/'), reviews_path);
        let reviews_resp = self
            .get(client, &reviews_path)
            .send()
            .await
            .map_err(|e| GithubApiError::Unreachable(reviews_url, e.to_string()))?;
        if reviews_resp.status() == reqwest::StatusCode::FORBIDDEN {
            return Err(GithubApiError::RateLimited);
        }
        if !reviews_resp.status().is_success() {
            return Err(GithubApiError::BadStatus(reviews_resp.status()));
        }
        let mut raw: Vec<GhReview> = reviews_resp
            .json()
            .await
            .map_err(|e| GithubApiError::Parse(e.to_string()))?;
        raw.truncate(MAX_REVIEW_SUBMISSIONS);

        // Fold to latest-per-reviewer. GitHub returns reviews in
        // submission order (oldest first); the LAST occurrence per login
        // in that order is the latest state — a plain overwrite-on-iterate
        // into an order-preserving map gets this right without a separate
        // sort-by-timestamp (timestamps can tie or be absent for a
        // synthetic/dismissed entry).
        let mut latest: Vec<ReviewerStateOut> = Vec::new();
        let mut index_of: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for r in raw {
            let login = r.user.map(|u| u.login).unwrap_or_default();
            let row = ReviewerStateOut {
                reviewer: login.clone(),
                state: r.state,
                submitted_at: r.submitted_at,
            };
            if let Some(&i) = index_of.get(&login) {
                latest[i] = row;
            } else {
                index_of.insert(login, latest.len());
                latest.push(row);
            }
        }

        let requested_path = format!("/repos/{owner}/{repo}/pulls/{number}/requested_reviewers");
        let requested_url = format!("{}{}", self.api_base.trim_end_matches('/'), requested_path);
        let requested_resp = self
            .get(client, &requested_path)
            .send()
            .await
            .map_err(|e| GithubApiError::Unreachable(requested_url, e.to_string()))?;
        if requested_resp.status() == reqwest::StatusCode::FORBIDDEN {
            return Err(GithubApiError::RateLimited);
        }
        if !requested_resp.status().is_success() {
            return Err(GithubApiError::BadStatus(requested_resp.status()));
        }
        let requested: GhRequestedReviewers = requested_resp
            .json()
            .await
            .map_err(|e| GithubApiError::Parse(e.to_string()))?;

        let review_decision = if latest.iter().any(|r| r.state == "CHANGES_REQUESTED") {
            Some("CHANGES_REQUESTED".to_string())
        } else if latest.iter().any(|r| r.state == "APPROVED") {
            Some("APPROVED".to_string())
        } else {
            None
        };

        Ok(PrReviewsOut {
            reviewers: latest,
            requested_reviewers: requested.users.into_iter().map(|u| u.login).collect(),
            review_decision,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_github_origin: https / ssh / non-github --------------------

    #[test]
    fn parses_https_with_dot_git_suffix() {
        let r = parse_github_origin("https://github.com/octocat/hello-world.git").unwrap();
        assert_eq!(r.owner, "octocat");
        assert_eq!(r.name, "hello-world");
    }

    #[test]
    fn parses_https_without_dot_git_suffix() {
        let r = parse_github_origin("https://github.com/octocat/hello-world").unwrap();
        assert_eq!(r.owner, "octocat");
        assert_eq!(r.name, "hello-world");
    }

    #[test]
    fn parses_https_with_a_trailing_slash() {
        let r = parse_github_origin("https://github.com/octocat/hello-world/").unwrap();
        assert_eq!(r.owner, "octocat");
        assert_eq!(r.name, "hello-world");
    }

    #[test]
    fn parses_scp_like_ssh_form() {
        let r = parse_github_origin("git@github.com:octocat/hello-world.git").unwrap();
        assert_eq!(r.owner, "octocat");
        assert_eq!(r.name, "hello-world");
    }

    #[test]
    fn parses_ssh_url_form() {
        let r = parse_github_origin("ssh://git@github.com/octocat/hello-world.git").unwrap();
        assert_eq!(r.owner, "octocat");
        assert_eq!(r.name, "hello-world");
    }

    #[test]
    fn rejects_a_non_github_https_origin() {
        assert!(parse_github_origin("https://gitlab.com/octocat/hello-world.git").is_none());
    }

    #[test]
    fn rejects_a_non_github_ssh_origin() {
        assert!(parse_github_origin("git@gitlab.com:octocat/hello-world.git").is_none());
    }

    #[test]
    fn rejects_a_bare_local_path() {
        assert!(parse_github_origin("/home/user/some/repo").is_none());
        assert!(parse_github_origin("../relative/repo.git").is_none());
    }

    #[test]
    fn rejects_a_url_missing_the_repo_name() {
        assert!(parse_github_origin("https://github.com/octocat").is_none());
        assert!(parse_github_origin("https://github.com/").is_none());
    }

    // --- github_repo: real git subprocess -----------------------------------

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git -C {} {:?} failed: {}",
            dir.display(),
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        tmp
    }

    #[test]
    fn github_repo_resolves_a_configured_github_origin() {
        let tmp = init_repo();
        git(
            tmp.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/octocat/hello-world.git",
            ],
        );
        let r = github_repo(tmp.path()).unwrap();
        assert_eq!(r.owner, "octocat");
        assert_eq!(r.name, "hello-world");
    }

    #[test]
    fn github_repo_errors_cleanly_with_no_origin_configured() {
        let tmp = init_repo();
        let err = github_repo(tmp.path()).unwrap_err();
        assert!(matches!(err, GithubError::NoOrigin(_)), "got: {err:?}");
    }

    #[test]
    fn github_repo_errors_cleanly_for_a_non_github_origin() {
        let tmp = init_repo();
        git(
            tmp.path(),
            &["remote", "add", "origin", "https://gitlab.com/a/b.git"],
        );
        let err = github_repo(tmp.path()).unwrap_err();
        assert!(
            matches!(err, GithubError::NotGithubOrigin(_)),
            "got: {err:?}"
        );
    }

    // --- fetch_pr_ref: a local bare repo acting as its own "origin" --------

    /// Simulates a GitHub PR by creating `refs/pull/<n>/head` directly in a
    /// bare repo acting as `origin` (real GitHub only ever lets you FETCH
    /// that ref, never push to it directly — this fixture reproduces the
    /// same shape locally without any network dependency).
    #[test]
    fn fetch_pr_ref_creates_the_refs_kbc_namespace_and_resolves_a_sha() {
        let bare_tmp = tempfile::tempdir().unwrap();
        let bare_dir = bare_tmp.path().join("origin.git");
        std::fs::create_dir_all(&bare_dir).unwrap();
        git(&bare_dir, &["init", "-q", "--bare", "-b", "main"]);

        // A working checkout to populate the bare "origin" with a commit
        // and a simulated PR head ref.
        let work_tmp = tempfile::tempdir().unwrap();
        let work_dir = work_tmp.path();
        git(work_dir, &["init", "-q", "-b", "main"]);
        git(work_dir, &["config", "user.email", "test@example.com"]);
        git(work_dir, &["config", "user.name", "Test"]);
        std::fs::write(work_dir.join("a.txt"), "hello\n").unwrap();
        git(work_dir, &["add", "-A"]);
        git(work_dir, &["commit", "-q", "-m", "pr commit"]);
        let pr_sha = String::from_utf8(
            std::process::Command::new("git")
                .arg("-C")
                .arg(work_dir)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        git(
            work_dir,
            &[
                "push",
                "-q",
                bare_dir.to_str().unwrap(),
                "HEAD:refs/pull/42/head",
            ],
        );

        // The repo under test: `origin` points at the bare fixture.
        let repo_tmp = tempfile::tempdir().unwrap();
        let repo_dir = repo_tmp.path();
        git(repo_dir, &["init", "-q", "-b", "main"]);
        git(repo_dir, &["config", "user.email", "test@example.com"]);
        git(repo_dir, &["config", "user.name", "Test"]);
        std::fs::write(repo_dir.join("base.txt"), "base\n").unwrap();
        git(repo_dir, &["add", "-A"]);
        git(repo_dir, &["commit", "-q", "-m", "base"]);
        git(
            repo_dir,
            &["remote", "add", "origin", bare_dir.to_str().unwrap()],
        );

        let (target_ref, sha) = fetch_pr_ref(repo_dir, 42).unwrap();
        assert_eq!(target_ref, "refs/kbc/pr/42");
        assert_eq!(sha, pr_sha);

        // The ref must actually resolve in the repo's OWN ref-db now.
        let verify = std::process::Command::new("git")
            .arg("-C")
            .arg(repo_dir)
            .args(["rev-parse", "--verify", "refs/kbc/pr/42"])
            .output()
            .unwrap();
        assert!(verify.status.success());
    }

    #[test]
    fn fetch_pr_ref_reports_a_clean_error_for_an_unknown_pr_number() {
        let tmp = init_repo();
        // No origin at all configured — `git fetch origin` fails cleanly.
        let err = fetch_pr_ref(tmp.path(), 999).unwrap_err();
        assert!(matches!(err, GithubError::FetchFailed(_)), "got: {err:?}");
    }

    // --- GithubClient: mocked GitHub API (never touches the real network) -

    use crate::join::kb_client::test_support::mock_kb_server;
    use axum::routing::get;
    use axum::{Json, Router};

    fn test_cfg(api_base: String) -> GithubSection {
        GithubSection {
            token_file: None,
            api_base,
        }
    }

    #[tokio::test]
    async fn list_pulls_parses_a_mocked_open_pr_list() {
        let router = Router::new().route(
            "/repos/acme/widget/pulls",
            get(|| async {
                Json(serde_json::json!([
                    {
                        "number": 7,
                        "title": "Add feature",
                        "user": {"login": "octocat"},
                        "head": {"ref": "feature-branch"},
                        "base": {"ref": "main"},
                        "updated_at": "2024-01-01T00:00:00Z",
                        "draft": false
                    }
                ]))
            }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = GithubClient::new(&test_cfg(format!("http://{addr}")));
        let prs = client.list_pulls("acme", "widget").await.unwrap();
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].number, 7);
        assert_eq!(prs[0].title, "Add feature");
        assert_eq!(prs[0].author, "octocat");
        assert_eq!(prs[0].head_ref, "feature-branch");
        assert_eq!(prs[0].base_ref, "main");
        assert!(!prs[0].draft);
    }

    #[tokio::test]
    async fn list_pulls_reports_rate_limited_on_403() {
        let router = Router::new().route(
            "/repos/acme/widget/pulls",
            get(|| async { axum::http::StatusCode::FORBIDDEN }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = GithubClient::new(&test_cfg(format!("http://{addr}")));
        let err = client.list_pulls("acme", "widget").await.unwrap_err();
        assert!(matches!(err, GithubApiError::RateLimited));
    }

    #[tokio::test]
    async fn list_pulls_unreachable_is_reported_not_panicked() {
        let client = GithubClient::new(&test_cfg("http://127.0.0.1:0".to_string()));
        let err = client.list_pulls("acme", "widget").await.unwrap_err();
        assert!(matches!(err, GithubApiError::Unreachable(_, _)));
    }

    #[tokio::test]
    async fn list_pull_comments_merges_review_and_issue_comments() {
        let router = Router::new()
            .route(
                "/repos/acme/widget/pulls/7/comments",
                get(|| async {
                    Json(serde_json::json!([
                        {
                            "user": {"login": "reviewer"},
                            "body": "nit: rename this",
                            "path": "src/lib.rs",
                            "line": 42,
                            "created_at": "2024-01-01T00:00:00Z",
                            "in_reply_to_id": null
                        }
                    ]))
                }),
            )
            .route(
                "/repos/acme/widget/issues/7/comments",
                get(|| async {
                    Json(serde_json::json!([
                        {
                            "user": {"login": "author"},
                            "body": "thanks for the review!",
                            "created_at": "2024-01-02T00:00:00Z"
                        }
                    ]))
                }),
            );
        let (addr, _server) = mock_kb_server(router).await;
        let client = GithubClient::new(&test_cfg(format!("http://{addr}")));
        let (comments, truncated) = client
            .list_pull_comments("acme", "widget", 7)
            .await
            .unwrap();
        assert_eq!(comments.len(), 2);
        assert!(!truncated);
        assert_eq!(comments[0].path.as_deref(), Some("src/lib.rs"));
        assert_eq!(comments[0].line, Some(42));
        assert_eq!(comments[1].path, None, "an issue comment has no path");
        assert_eq!(comments[1].author, "author");
    }

    #[tokio::test]
    async fn list_pull_comments_follows_link_next_across_pages() {
        // V70-A3X — a single `per_page=100` page used to be the whole
        // story; this pins that a `Link: rel="next"` on page 1 is actually
        // followed, and that the item from page 2 shows up in the merged
        // result. Binds the listener FIRST (not via `mock_kb_server`) so
        // the Link header can bake in the mock's own real address — a
        // GitHub `Link` target is always absolute.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = Router::new()
            .route(
                "/repos/acme/widget/pulls/7/comments",
                get({
                    let next = format!(
                        r#"<http://{addr}/repos/acme/widget/pulls/7/comments2>; rel="next""#
                    );
                    move || {
                        let next = next.clone();
                        async move {
                            (
                                [(axum::http::header::LINK, next)],
                                Json(serde_json::json!([
                                    {
                                        "user": {"login": "reviewer"},
                                        "body": "page 1 comment",
                                        "path": "src/lib.rs",
                                        "line": 1,
                                        "created_at": "2024-01-01T00:00:00Z"
                                    }
                                ])),
                            )
                        }
                    }
                }),
            )
            .route(
                "/repos/acme/widget/pulls/7/comments2",
                get(|| async {
                    Json(serde_json::json!([
                        {
                            "user": {"login": "reviewer"},
                            "body": "page 2 comment",
                            "path": "src/lib.rs",
                            "line": 2,
                            "created_at": "2024-01-01T00:01:00Z"
                        }
                    ]))
                }),
            )
            .route(
                "/repos/acme/widget/issues/7/comments",
                get(|| async { Json(serde_json::json!([])) }),
            );
        let _server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let client = GithubClient::new(&test_cfg(format!("http://{addr}")));
        let (comments, truncated) = client
            .list_pull_comments("acme", "widget", 7)
            .await
            .unwrap();
        assert_eq!(comments.len(), 2, "got {comments:?}");
        assert!(!truncated, "under budget — never actually truncated");
        assert_eq!(comments[0].body, "page 1 comment");
        assert_eq!(comments[1].body, "page 2 comment");
    }

    #[tokio::test]
    async fn fetch_paginated_reports_truncated_when_the_budget_cuts_a_followed_page_short() {
        // V70-A3X — direct unit test of the pagination primitive with a
        // deliberately tiny budget (avoids needing 200+ live items to
        // exercise `MAX_COMMENTS` end-to-end): 2 pages of 2 items each, a
        // budget of 3, must return exactly 3 items AND report `truncated`.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = Router::new()
            .route(
                "/repos/acme/widget/issues/7/comments",
                get({
                    let next =
                        format!(r#"<http://{addr}/repos/acme/widget/issues/7/comments2>; rel="next""#);
                    move || {
                        let next = next.clone();
                        async move {
                            (
                                [(axum::http::header::LINK, next)],
                                Json(serde_json::json!([
                                    {"user": {"login": "a"}, "body": "1", "created_at": "2024-01-01T00:00:00Z"},
                                    {"user": {"login": "a"}, "body": "2", "created_at": "2024-01-01T00:00:01Z"}
                                ])),
                            )
                        }
                    }
                }),
            )
            .route(
                "/repos/acme/widget/issues/7/comments2",
                get(|| async {
                    Json(serde_json::json!([
                        {"user": {"login": "a"}, "body": "3", "created_at": "2024-01-01T00:00:02Z"},
                        {"user": {"login": "a"}, "body": "4", "created_at": "2024-01-01T00:00:03Z"}
                    ]))
                }),
            );
        let _server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let client = GithubClient::new(&test_cfg(format!("http://{addr}")));
        let http = client.client().unwrap();
        let (items, truncated): (Vec<GhIssueComment>, bool) = client
            .fetch_paginated(http, "/repos/acme/widget/issues/7/comments", 3)
            .await
            .unwrap();
        assert_eq!(items.len(), 3, "got {items:?}");
        assert!(truncated, "4 items existed across 2 pages, budget was 3");
        assert_eq!(items[0].body, "1");
        assert_eq!(items[2].body, "3");
    }

    #[test]
    fn parse_link_next_extracts_the_next_url_and_ignores_other_rels() {
        let header = r#"<https://api.github.com/x?page=2>; rel="next", <https://api.github.com/x?page=5>; rel="last""#;
        assert_eq!(
            parse_link_next(header).as_deref(),
            Some("https://api.github.com/x?page=2")
        );
    }

    #[test]
    fn parse_link_next_is_none_on_the_last_page() {
        let header = r#"<https://api.github.com/x?page=1>; rel="prev", <https://api.github.com/x?page=1>; rel="first""#;
        assert_eq!(parse_link_next(header), None);
    }

    // --- origin_url via `git config --get` survives an insteadOf rewrite --

    /// PRR-R2 — the regression this crate's own `origin_url` switch guards:
    /// with an `insteadOf` rule redirecting `origin`'s ACTUAL fetch traffic
    /// to a local bare repo, `git remote get-url origin` would report the
    /// rewritten (local) target, but `github_repo` must still resolve the
    /// CONFIGURED github.com URL — this is the exact fixture shape
    /// `tests/review/review_routes.rs`'s `create_review_pr` happy-path test
    /// depends on.
    #[test]
    fn github_repo_resolves_the_configured_url_even_under_an_insteadof_rewrite() {
        let bare_tmp = tempfile::tempdir().unwrap();
        let bare_dir = bare_tmp.path().join("origin.git");
        std::fs::create_dir_all(&bare_dir).unwrap();
        git(&bare_dir, &["init", "-q", "--bare", "-b", "main"]);

        let repo_tmp = tempfile::tempdir().unwrap();
        let repo_dir = repo_tmp.path();
        git(repo_dir, &["init", "-q", "-b", "main"]);
        git(
            repo_dir,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/acme/widget.git",
            ],
        );
        git(
            repo_dir,
            &[
                "config",
                &format!("url.{}.insteadOf", bare_dir.to_str().unwrap()),
                "https://github.com/acme/widget.git",
            ],
        );

        // Sanity: `remote get-url` really does show the rewritten target
        // (the behaviour this function must NOT rely on).
        let get_url = std::process::Command::new("git")
            .arg("-C")
            .arg(repo_dir)
            .args(["remote", "get-url", "origin"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&get_url.stdout).trim(),
            bare_dir.to_str().unwrap(),
            "insteadOf must actually be in effect for this test to prove anything"
        );

        let r = github_repo(repo_dir).unwrap();
        assert_eq!(r.owner, "acme");
        assert_eq!(r.name, "widget");
    }

    // --- GithubClient::get_pull ---------------------------------------------

    #[tokio::test]
    async fn get_pull_parses_a_mocked_merged_pr_with_merge_state_status() {
        let router = Router::new().route(
            "/repos/acme/widget/pulls/7",
            get(|| async {
                Json(serde_json::json!({
                    "number": 7,
                    "title": "Add feature",
                    "user": {"login": "octocat"},
                    "head": {"ref": "feature-branch", "sha": "deadbeef"},
                    "base": {"ref": "main"},
                    "updated_at": "2024-01-01T00:00:00Z",
                    "draft": false,
                    "state": "closed",
                    "merged": true,
                    "labels": [{"name": "bug"}, {"name": "priority-1"}],
                    "mergeable_state": "clean"
                }))
            }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = GithubClient::new(&test_cfg(format!("http://{addr}")));
        let pr = client.get_pull("acme", "widget", 7).await.unwrap();
        assert_eq!(pr.number, 7);
        assert_eq!(pr.head_sha, "deadbeef");
        assert_eq!(pr.state, "closed");
        assert!(pr.merged);
        assert_eq!(pr.labels, vec!["bug", "priority-1"]);
        assert_eq!(pr.merge_state_status.as_deref(), Some("clean"));
    }

    #[tokio::test]
    async fn get_pull_works_for_an_open_pr_missing_mergeable_state() {
        let router = Router::new().route(
            "/repos/acme/widget/pulls/8",
            get(|| async {
                Json(serde_json::json!({
                    "number": 8,
                    "title": "WIP",
                    "user": null,
                    "head": {"ref": "wip-branch", "sha": "abc123"},
                    "base": {"ref": "main"},
                    "updated_at": "2024-01-01T00:00:00Z",
                    "draft": true,
                    "state": "open",
                    "merged": false
                }))
            }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = GithubClient::new(&test_cfg(format!("http://{addr}")));
        let pr = client.get_pull("acme", "widget", 8).await.unwrap();
        assert_eq!(pr.author, "", "a null user degrades to an empty author");
        assert!(pr.draft);
        assert!(!pr.merged);
        assert!(pr.labels.is_empty());
        assert_eq!(pr.merge_state_status, None);
    }

    #[tokio::test]
    async fn get_pull_reports_rate_limited_on_403() {
        let router = Router::new().route(
            "/repos/acme/widget/pulls/7",
            get(|| async { axum::http::StatusCode::FORBIDDEN }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = GithubClient::new(&test_cfg(format!("http://{addr}")));
        let err = client.get_pull("acme", "widget", 7).await.unwrap_err();
        assert!(matches!(err, GithubApiError::RateLimited));
    }

    // --- GithubClient::list_checks -------------------------------------------

    #[tokio::test]
    async fn list_checks_normalizes_status_and_computes_duration() {
        let router = Router::new().route(
            "/repos/acme/widget/commits/deadbeef/check-runs",
            get(|| async {
                Json(serde_json::json!({
                    "check_runs": [
                        {
                            "name": "build",
                            "status": "completed",
                            "conclusion": "success",
                            "started_at": "2024-01-01T00:00:00Z",
                            "completed_at": "2024-01-01T00:01:30Z"
                        },
                        {
                            "name": "lint",
                            "status": "completed",
                            "conclusion": "failure",
                            "started_at": "2024-01-01T00:00:00Z",
                            "completed_at": "2024-01-01T00:00:10Z"
                        },
                        {
                            "name": "flaky",
                            "status": "completed",
                            "conclusion": "skipped"
                        },
                        {
                            "name": "e2e",
                            "status": "in_progress",
                            "conclusion": null
                        }
                    ]
                }))
            }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = GithubClient::new(&test_cfg(format!("http://{addr}")));
        let checks = client
            .list_checks("acme", "widget", "deadbeef")
            .await
            .unwrap();
        assert_eq!(checks.len(), 4);
        assert_eq!(checks[0].status, "pass");
        assert_eq!(checks[0].duration, Some(90));
        assert_eq!(checks[1].status, "fail");
        assert_eq!(checks[1].duration, Some(10));
        assert_eq!(checks[2].status, "warn");
        assert_eq!(checks[2].duration, None, "no timestamps -> no duration");
        assert_eq!(checks[3].status, "pending");
        assert_eq!(checks[3].note.as_deref(), Some("in_progress"));
    }

    #[tokio::test]
    async fn list_checks_reports_rate_limited_on_403() {
        let router = Router::new().route(
            "/repos/acme/widget/commits/deadbeef/check-runs",
            get(|| async { axum::http::StatusCode::FORBIDDEN }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = GithubClient::new(&test_cfg(format!("http://{addr}")));
        let err = client
            .list_checks("acme", "widget", "deadbeef")
            .await
            .unwrap_err();
        assert!(matches!(err, GithubApiError::RateLimited));
    }

    // --- GithubClient::list_reviews ------------------------------------------

    #[tokio::test]
    async fn list_reviews_folds_to_latest_per_reviewer_and_computes_decision() {
        let router = Router::new()
            .route(
                "/repos/acme/widget/pulls/7/reviews",
                get(|| async {
                    Json(serde_json::json!([
                        {"user": {"login": "alice"}, "state": "APPROVED", "submitted_at": "2024-01-01T00:00:00Z"},
                        {"user": {"login": "bob"}, "state": "CHANGES_REQUESTED", "submitted_at": "2024-01-01T01:00:00Z"},
                        // alice re-reviews after a push — this later entry
                        // must win over her earlier APPROVED.
                        {"user": {"login": "alice"}, "state": "COMMENTED", "submitted_at": "2024-01-02T00:00:00Z"}
                    ]))
                }),
            )
            .route(
                "/repos/acme/widget/pulls/7/requested_reviewers",
                get(|| async {
                    Json(serde_json::json!({"users": [{"login": "carol"}], "teams": []}))
                }),
            );
        let (addr, _server) = mock_kb_server(router).await;
        let client = GithubClient::new(&test_cfg(format!("http://{addr}")));
        let out = client.list_reviews("acme", "widget", 7).await.unwrap();
        assert_eq!(out.reviewers.len(), 2, "alice folds to one row");
        let alice = out
            .reviewers
            .iter()
            .find(|r| r.reviewer == "alice")
            .unwrap();
        assert_eq!(alice.state, "COMMENTED", "latest submission wins");
        let bob = out.reviewers.iter().find(|r| r.reviewer == "bob").unwrap();
        assert_eq!(bob.state, "CHANGES_REQUESTED");
        assert_eq!(out.requested_reviewers, vec!["carol"]);
        assert_eq!(
            out.review_decision.as_deref(),
            Some("CHANGES_REQUESTED"),
            "a CHANGES_REQUESTED latest state wins over an unrelated APPROVED"
        );
    }

    #[tokio::test]
    async fn list_reviews_reports_approved_decision_with_no_outstanding_requests() {
        let router = Router::new()
            .route(
                "/repos/acme/widget/pulls/9/reviews",
                get(|| async {
                    Json(serde_json::json!([
                        {"user": {"login": "alice"}, "state": "APPROVED", "submitted_at": "2024-01-01T00:00:00Z"}
                    ]))
                }),
            )
            .route(
                "/repos/acme/widget/pulls/9/requested_reviewers",
                get(|| async { Json(serde_json::json!({"users": [], "teams": []})) }),
            );
        let (addr, _server) = mock_kb_server(router).await;
        let client = GithubClient::new(&test_cfg(format!("http://{addr}")));
        let out = client.list_reviews("acme", "widget", 9).await.unwrap();
        assert_eq!(out.review_decision.as_deref(), Some("APPROVED"));
        assert!(out.requested_reviewers.is_empty());
    }

    #[tokio::test]
    async fn list_reviews_with_no_submissions_reports_no_decision_not_a_guess() {
        let router = Router::new()
            .route(
                "/repos/acme/widget/pulls/10/reviews",
                get(|| async { Json(serde_json::json!([])) }),
            )
            .route(
                "/repos/acme/widget/pulls/10/requested_reviewers",
                get(|| async {
                    Json(serde_json::json!({"users": [{"login": "dave"}], "teams": []}))
                }),
            );
        let (addr, _server) = mock_kb_server(router).await;
        let client = GithubClient::new(&test_cfg(format!("http://{addr}")));
        let out = client.list_reviews("acme", "widget", 10).await.unwrap();
        assert!(out.reviewers.is_empty());
        assert_eq!(out.requested_reviewers, vec!["dave"]);
        assert_eq!(out.review_decision, None);
    }

    #[tokio::test]
    async fn list_reviews_reports_rate_limited_on_403() {
        let router = Router::new().route(
            "/repos/acme/widget/pulls/7/reviews",
            get(|| async { axum::http::StatusCode::FORBIDDEN }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let client = GithubClient::new(&test_cfg(format!("http://{addr}")));
        let err = client.list_reviews("acme", "widget", 7).await.unwrap_err();
        assert!(matches!(err, GithubApiError::RateLimited));
    }
}
