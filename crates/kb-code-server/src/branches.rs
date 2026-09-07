//! `branch-facts/1` and its three siblings (V75-M3, design D15) — the
//! HTTP half of `history::facts` / `history::radar`.
//!
//! Four routes, in three different auth postures, and the difference is
//! the point:
//!
//! | route | posture | why |
//! |---|---|---|
//! | `GET /api/branches/facts` | `auth_bearer` | an ordinary browsing read, same gate as `/branches` |
//! | `GET /api/branches/conflicts` | `auth_bearer` | writes only into a per-request scratch ODB (SEC-15); the repo is untouched |
//! | `GET`/`POST /api/branches/favourites` | `auth_bearer` | an operator PREFERENCE — the `bookmarks`/`doc-lens pin` precedent, not the checkout/review-ref one |
//! | `POST /api/branches/review` | **loopback-only** | it CREATES a review, so it may not be a weaker path to review creation than `POST /api/reviews` itself |
//!
//! That last row is a deliberate deviation from this unit's brief, which
//! named `review_gate`. `review_gate` admits a bearer caller when
//! `[review] remote_mutations` is on; `POST /api/reviews` is
//! loopback-only. Routing review CREATION through a looser gate than the
//! route it delegates to would be a bypass of exactly the kind invariant 1
//! exists to prevent, so this rides `transcripts_api` with `create_review`.
//!
//! # `POST /api/branches/review`, not `POST /api/branches/{ref}/review`
//!
//! A branch ref contains `/` (`feature/auth`, `refs/heads/x`), and axum's
//! wildcard capture must be TERMINAL — `/branches/{*ref}/review` is not a
//! legal route. Percent-encoding the slash does not help either: axum
//! matches on the decoded path. The ref therefore rides the JSON body,
//! which is also where `POST /api/reviews` already puts its `head_ref`.
//!
//! # What this module does NOT do
//!
//! It computes nothing. Every rule — the base ladder, the view membership,
//! stale, merged, agent provenance — lives in `history::facts`, which is
//! pure enough to unit-test without a daemon. This module resolves params,
//! runs the blocking git work under the `git_fanout` semaphore, folds the
//! store and (optionally) GitHub lanes in, and renders. That split is the
//! reason `facts.rs` has 30 unit tests and this file has none.

use crate::history::facts::{self, BaseClass, BranchFact, FactFilter, View};
use crate::history::radar;
use crate::routes::{find_repo, parse_revspec, ApiError};
use crate::state::SharedState;
// The 2026-08-31 starvation incident's convention: every store call goes
// through `run_blocking`, which is a TRAIT method — it has to be in scope.
use crate::store::StoreBlocking;
use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

// --- GET /api/branches/facts ---------------------------------------------

#[derive(Debug, Deserialize)]
pub struct FactsParams {
    pub repo: String,
    /// One of `facts::View`'s eight names; anything else is a 400 naming
    /// the closed set (a mistyped view must not silently become `all`).
    #[serde(default)]
    pub view: Option<String>,
    /// A kbcq/1 query — the `/` filter plus the `branch:`/`touches:`/
    /// `by:`/`agent:` atoms. Parsed by the ONE grammar
    /// (`search::grammar::parse`), never by a second parser here.
    #[serde(default)]
    pub q: Option<String>,
    /// A prefix-folding selection (`feature/`).
    #[serde(default)]
    pub prefix: Option<String>,
    /// `1`/`true`/`yes` — starred branches only.
    #[serde(default)]
    pub fav: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
    /// `1` — fold in open GitHub PRs (ONE `list_pulls` call). Off by
    /// default: a listing route must not do network I/O nobody asked for.
    #[serde(default)]
    pub pr: Option<String>,
    /// `1` — additionally probe CI for the page's PR-bearing rows, capped
    /// at [`MAX_CI_PROBES`]. Implies `pr=1`.
    #[serde(default)]
    pub ci: Option<String>,
    /// `1` — run the patch-id merge probe even outside `view=merged`. See
    /// [`FactsResponse::rules`]'s `merged` block for why it is not the
    /// default.
    #[serde(default)]
    pub patch_id: Option<String>,
}

/// Per-request ceiling on `list_checks` calls — CI is one GitHub round
/// trip per PR, so a 50-row page would otherwise be 50 of them.
pub const MAX_CI_PROBES: usize = 10;

fn truthy(v: Option<&String>) -> bool {
    matches!(
        v.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

fn parse_view(v: Option<&String>) -> Result<View, ApiError> {
    let Some(v) = v else {
        return Ok(View::default());
    };
    View::ALL
        .iter()
        .copied()
        .find(|x| x.as_str() == v.trim())
        .ok_or_else(|| {
            ApiError::bad_request(format!(
                "unknown view {:?} — one of {}",
                v,
                View::ALL
                    .iter()
                    .map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join("|")
            ))
        })
}

#[derive(Debug, Serialize)]
pub struct TipOut {
    pub sha: String,
    pub subject: String,
    pub author_name: String,
    pub author_email: String,
    /// Author time, unix seconds — the same epoch every other commit
    /// surface in this crate uses.
    pub time: i64,
}

#[derive(Debug, Serialize)]
pub struct UpstreamOut {
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub gone: bool,
    /// `None` when git had nothing to say (`%(upstream:track)` empty) —
    /// never a guessed zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ahead: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct ReviewRefOut {
    pub id: i64,
    pub head_ref: String,
    pub base_ref: String,
}

#[derive(Debug, Serialize)]
pub struct PrOut {
    pub number: u64,
    pub title: String,
    pub draft: bool,
    pub base_ref: String,
}

#[derive(Debug, Serialize)]
pub struct CiOut {
    /// Worst-of over the check runs: `fail` > `pending` > `warn` > `pass`.
    /// A DERIVED roll-up; `checks` is the count it was derived from.
    pub status: &'static str,
    pub checks: usize,
}

#[derive(Debug, Serialize)]
pub struct BranchFactOut {
    pub name: String,
    pub full_ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    pub tip: TipOut,
    pub is_head: bool,
    /// A LINKED worktree's path (never the main checkout — that is
    /// `is_head`), plus its basename as a chip label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<UpstreamOut>,
    pub base: facts::BranchBase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ahead: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged: Option<facts::MergedWitness>,
    pub agent: facts::AgentProvenance,
    pub stale: bool,
    pub mine: bool,
    pub favourite: bool,
    pub reviews: Vec<ReviewRefOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<PrOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci: Option<CiOut>,
    /// The stack layer this branch is, when `history::stacks` detected one
    /// — carrying THAT layer's own base, which is not necessarily this
    /// row's `base` (a stack's per-level base is the point).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<StackOut>,
    pub reasons: Vec<facts::Reason>,
}

#[derive(Debug, Serialize)]
pub struct WorktreeOut {
    pub path: String,
    /// The path's final component — the chip label, and the closest thing
    /// to a "worktree id" available from `%(worktreepath)` alone.
    pub id: String,
}

#[derive(Debug, Serialize)]
pub struct StackOut {
    /// The layer's own base branch (`A` for `B` atop `A` atop `main`).
    pub base: String,
    /// 1 for a layer directly on the default branch, 2 for one atop that,
    /// and so on.
    pub depth: usize,
    /// `history::stacks`'s own "the base moved since this layer was cut".
    pub stale: bool,
}

#[derive(Debug, Serialize)]
pub struct MergedRule {
    pub rule: &'static str,
    pub patch_id_probed: usize,
    pub patch_id_candidates: usize,
    pub patch_id_cap: usize,
}

#[derive(Debug, Serialize)]
pub struct AgentRule {
    pub exact: &'static str,
    pub likely: &'static str,
    pub never: &'static str,
    pub agent_emails: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct TouchesRule {
    pub path: String,
    pub scanned: usize,
    pub candidates: usize,
    pub cap: usize,
}

#[derive(Debug, Serialize)]
pub struct Rules {
    /// The ladder's rungs in order — the closed vocabulary, on the wire so
    /// no client hardcodes it.
    pub base_ladder: [&'static str; 4],
    pub base_note: &'static str,
    pub stale: facts::StaleRule,
    pub merged: MergedRule,
    pub agent: AgentRule,
    pub views: &'static str,
    /// The caveat on `view_counts`, stated rather than assumed.
    pub view_counts_note: &'static str,
    pub sort: &'static str,
    pub ahead_behind_source: facts::AheadBehindSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub touches: Option<TouchesRule>,
    /// `(hits, misses)` of the per-boot base cache since boot.
    pub base_cache_hits: u64,
    pub base_cache_misses: u64,
}

#[derive(Debug, Serialize)]
pub struct DegradedLane {
    pub lane: &'static str,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct FactsResponse {
    pub schema: &'static str,
    pub repo: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_sha: Option<String>,
    pub view: &'static str,
    pub rows: Vec<BranchFactOut>,
    /// Rows matching the view + filters BEFORE paging.
    pub total: usize,
    /// Refs the one `for-each-ref` pass returned (after the local-wins
    /// remote de-dup), before any filter.
    pub enumerated: usize,
    pub enumeration_truncated: bool,
    pub limit: usize,
    pub offset: usize,
    /// Prefix tree over the VIEW's rows (not the page's) — folding a page
    /// would make the counts change as you scroll.
    pub prefixes: Vec<facts::PrefixCount>,
    /// Per-view row counts over the whole enumeration, so the view
    /// selector can show its own numbers without eight round trips.
    ///
    /// Computed under THIS request's own probe budget — see
    /// [`Rules::view_counts_note`]. `merged` is the one count that can
    /// therefore differ from what `?view=merged` itself returns, and the
    /// note says so rather than leaving a caller to discover it.
    pub view_counts: HashMap<&'static str, usize>,
    pub rules: Rules,
    /// The kbcq/1 parse's own diagnostics, passed through verbatim — a
    /// mistyped atom is a warning and an ordinary word, never a 400.
    pub diagnostics: Vec<crate::search::grammar::Diagnostic>,
    /// The normalized query, so a caller can round-trip what it typed.
    pub normalized: String,
    pub degraded: Vec<DegradedLane>,
}

/// The blocking half — everything that shells out to git, in ONE closure
/// so it takes ONE `git_fanout` permit and one blocking-pool thread.
struct GitPass {
    default_ref: Option<String>,
    default_sha: Option<String>,
    facts: Vec<BranchFact>,
    enumerated: usize,
    enumeration_truncated: bool,
    ab_source: facts::AheadBehindSource,
    stale: facts::StaleRule,
    stacks: HashMap<String, StackOut>,
    /// The patch-id budget as it was actually spent — counted INSIDE the
    /// git pass, where the candidate set still is the pre-filter one. A
    /// count taken after filtering would understate what the daemon did.
    patch_id_probed: usize,
    patch_id_candidates: usize,
    /// review id → `(head_ref, base_ref)`, so a row can name its reviews
    /// without a second store round trip.
    review_refs: HashMap<i64, (String, String)>,
}

/// `GET /api/branches/facts?repo=&view=&q=&prefix=&fav=&limit=&offset=`
/// (V75-M3, D15) — `branch-facts/1`.
///
/// One `for-each-ref` pass, a CLASSED base per row, and eight
/// URL-addressable views whose membership rules ride the response's own
/// `rules` block rather than living only in a doc. See
/// `history::facts`'s module doc for every rule; see this module's for the
/// auth posture.
///
/// Degrades rather than failing: a GitHub lane that cannot answer lands in
/// `degraded[]` with its reason and the rows come back without `pr`/`ci`.
pub async fn facts_route(
    State(state): State<SharedState>,
    Query(params): Query<FactsParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &params.repo)?;
    let view = parse_view(params.view.as_ref())?;
    let limit = params
        .limit
        .unwrap_or(facts::DEFAULT_LIMIT)
        .clamp(1, facts::MAX_LIMIT);
    let offset = params.offset.unwrap_or(0);
    let want_pr = truthy(params.pr.as_ref()) || truthy(params.ci.as_ref());
    let want_ci = truthy(params.ci.as_ref());
    let want_patch_id = truthy(params.patch_id.as_ref()) || view == View::Merged;

    let parsed = crate::search::grammar::parse(params.q.as_deref().unwrap_or(""));
    let mut filter = FactFilter::from_parsed(&parsed);
    filter.prefix = params.prefix.clone().filter(|p| !p.is_empty());
    filter.favourites_only = truthy(params.fav.as_ref());

    // --- store lanes (one wrapped call each; never inside the git pass) ---
    let repo_name = repo.name.clone();
    let open_reviews = state
        .store
        .run_blocking(move |store| store.list_open_reviews_for_repo(&repo_name))
        .await
        .unwrap_or_default();
    let repo_name = repo.name.clone();
    let favourites: HashSet<String> = state
        .store
        .run_blocking(move |store| store.list_branch_favourites(&repo_name))
        .await
        .unwrap_or_default()
        .into_iter()
        .collect();

    // --- the git pass ----------------------------------------------------
    let repo_root = repo.path.clone();
    let repo_name = repo.name.clone();
    let agent_emails = state.branches.resolved_agent_emails();
    let cache = state.branch_base_cache.clone();
    let open_reviews_for_git = open_reviews.clone();
    let favourites_for_git = favourites.clone();
    let permit = state
        .git_fanout
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let (pass, touches_rule) = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        git_pass(
            &repo_root,
            &repo_name,
            &agent_emails,
            &open_reviews_for_git,
            &favourites_for_git,
            &filter,
            view,
            want_patch_id,
            limit,
            offset,
            &cache,
        )
    })
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("branch-facts task panicked: {e}"),
        )
    })??;

    // --- view filter, paging, prefixes -----------------------------------
    let mut view_counts: HashMap<&'static str, usize> = HashMap::new();
    for v in View::ALL {
        view_counts.insert(
            v.as_str(),
            pass.facts.iter().filter(|f| f.in_view(*v)).count(),
        );
    }
    let selected: Vec<&BranchFact> = pass.facts.iter().filter(|f| f.in_view(view)).collect();
    let prefixes = facts::fold_prefixes(selected.iter().map(|f| f.raw.name.clone()));
    let total = selected.len();
    let page: Vec<&BranchFact> = selected.into_iter().skip(offset).take(limit).collect();

    // --- optional GitHub lanes -------------------------------------------
    let mut degraded: Vec<DegradedLane> = Vec::new();
    let mut prs: HashMap<String, PrOut> = HashMap::new();
    let mut ci: HashMap<String, CiOut> = HashMap::new();
    if want_pr {
        match crate::github::github_repo(&repo.path) {
            Ok(slug) => match state.github.list_pulls(&slug.owner, &slug.name).await {
                Ok(list) => {
                    for p in list {
                        prs.insert(
                            p.head_ref.clone(),
                            PrOut {
                                number: p.number,
                                title: p.title,
                                draft: p.draft,
                                base_ref: p.base_ref,
                            },
                        );
                    }
                    if want_ci {
                        let mut probes = 0usize;
                        for f in &page {
                            if probes >= MAX_CI_PROBES {
                                degraded.push(DegradedLane {
                                    lane: "ci",
                                    reason: format!(
                                        "CI is one GitHub round trip per PR; probed {probes} of \
                                         this page's PR-bearing rows (cap {MAX_CI_PROBES})"
                                    ),
                                });
                                break;
                            }
                            if !prs.contains_key(&f.raw.name) {
                                continue;
                            }
                            probes += 1;
                            match state
                                .github
                                .list_checks(&slug.owner, &slug.name, &f.raw.tip_sha)
                                .await
                            {
                                Ok(runs) => {
                                    ci.insert(f.raw.name.clone(), roll_up_ci(&runs));
                                }
                                Err(e) => degraded.push(DegradedLane {
                                    lane: "ci",
                                    reason: e.to_string(),
                                }),
                            }
                        }
                    }
                }
                Err(e) => degraded.push(DegradedLane {
                    lane: "github",
                    reason: e.to_string(),
                }),
            },
            Err(e) => degraded.push(DegradedLane {
                lane: "github",
                reason: e.to_string(),
            }),
        }
    }

    let (hits, misses) = cache_stats(&state);
    let rows: Vec<BranchFactOut> = page
        .into_iter()
        .map(|f| {
            render(
                f,
                pass.default_ref.as_deref(),
                &pass.stacks,
                &prs,
                &ci,
                &pass.review_refs,
            )
        })
        .collect();

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(FactsResponse {
            schema: facts::SCHEMA,
            repo: params.repo,
            default: pass.default_ref.clone(),
            default_sha: pass.default_sha.clone(),
            view: view.as_str(),
            rows,
            total,
            enumerated: pass.enumerated,
            enumeration_truncated: pass.enumeration_truncated,
            limit,
            offset,
            prefixes,
            view_counts,
            rules: Rules {
                base_ladder: ["upstream", "fork-point", "merge-base", "unknown"],
                base_note: "the base is CLASSED at the first rung that answers and is never \
                            silently defaulted; `unknown` means nothing was measurable, so \
                            ahead/behind are absent rather than zero",
                stale: pass.stale.clone(),
                merged: MergedRule {
                    rule: "ancestry (0 commits ahead of the base) OR patch-id equivalence \
                           (`git cherry`, which is what catches a SQUASH merge). Patch-id \
                           costs one subprocess per candidate, so it runs for view=merged \
                           or ?patch_id=1 and is capped",
                    patch_id_probed: pass.patch_id_probed,
                    patch_id_candidates: pass.patch_id_candidates,
                    patch_id_cap: facts::MAX_PATCH_ID_PROBES,
                },
                agent: AgentRule {
                    exact: "a machine trailer naming the run (Kb-Session:, Kb-Agent:)",
                    likely: "the tip author's email is in [branches] agent_emails",
                    never: "a Co-authored-by: trailer ALONE is not evidence — it is the shape \
                            a human-authored commit takes in an agent-assisted workflow, so \
                            reading it as provenance would label a human's commit agent",
                    agent_emails: state.branches.resolved_agent_emails(),
                },
                views: "current=checked out here or in a linked worktree · mine=your git \
                        identity authored the tip · agent=D18 provenance is exact or likely · \
                        review=an open review names this branch · active and stale PARTITION \
                        the set · merged=a witness proved it · all",
                view_counts_note: "counted under THIS request's own probe budget. Every view \
                                   but `merged` is exact; `merged` counts the ANCESTRY witness \
                                   only unless the patch-id probe ran (?view=merged or \
                                   ?patch_id=1), so it can be lower here than `?view=merged`'s \
                                   own total — an under-count that names its reason, never a \
                                   silently different number",
                sort: "most recent tip first, then name ascending — total and deterministic",
                ahead_behind_source: pass.ab_source,
                touches: touches_rule,
                base_cache_hits: hits,
                base_cache_misses: misses,
            },
            diagnostics: parsed.diagnostics,
            normalized: parsed.normalized,
            degraded,
        }),
    ))
}

fn cache_stats(state: &SharedState) -> (u64, u64) {
    // Root CLAUDE.md #15 — taken and released here, never across an await.
    state.branch_base_cache.lock().stats()
}

fn roll_up_ci(runs: &[crate::github::CheckRunOut]) -> CiOut {
    let worst = if runs.iter().any(|r| r.status == "fail") {
        "fail"
    } else if runs.iter().any(|r| r.status == "pending") {
        "pending"
    } else if runs.iter().any(|r| r.status == "warn") {
        "warn"
    } else if runs.is_empty() {
        "none"
    } else {
        "pass"
    };
    CiOut {
        status: worst,
        checks: runs.len(),
    }
}

fn render(
    f: &BranchFact,
    default_ref: Option<&str>,
    stacks: &HashMap<String, StackOut>,
    prs: &HashMap<String, PrOut>,
    ci: &HashMap<String, CiOut>,
    review_refs: &HashMap<i64, (String, String)>,
) -> BranchFactOut {
    BranchFactOut {
        name: f.raw.name.clone(),
        full_ref: f.raw.full_ref.clone(),
        remote: f.raw.remote.clone(),
        tip: TipOut {
            sha: f.raw.tip_sha.clone(),
            subject: f.raw.subject.clone(),
            author_name: f.raw.author_name.clone(),
            author_email: f.raw.author_email.clone(),
            time: f.raw.author_time,
        },
        is_head: f.raw.is_head,
        worktree: f.worktree.as_ref().map(|p| WorktreeOut {
            id: std::path::Path::new(p)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| p.clone()),
            path: p.clone(),
        }),
        upstream: f.raw.upstream.as_ref().map(|u| UpstreamOut {
            ref_name: u.clone(),
            gone: f.raw.track.gone,
            ahead: f.raw.track.measured.then_some(f.raw.track.ahead),
            behind: f.raw.track.measured.then_some(f.raw.track.behind),
        }),
        base: f.base.clone(),
        ahead: f.ahead,
        behind: f.behind,
        merged: f.merged.clone(),
        agent: f.agent.clone(),
        stale: f.stale,
        mine: f.mine,
        favourite: f.favourite,
        reviews: f
            .open_review_ids
            .iter()
            .filter_map(|id| {
                review_refs.get(id).map(|(head, base)| ReviewRefOut {
                    id: *id,
                    head_ref: head.clone(),
                    base_ref: base.clone(),
                })
            })
            .collect(),
        pr: prs.get(&f.raw.name).map(|p| PrOut {
            number: p.number,
            title: p.title.clone(),
            draft: p.draft,
            base_ref: p.base_ref.clone(),
        }),
        ci: ci.get(&f.raw.name).map(|c| CiOut {
            status: c.status,
            checks: c.checks,
        }),
        stack: stacks.get(&f.raw.name).map(|s| StackOut {
            base: s.base.clone(),
            depth: s.depth,
            stale: s.stale,
        }),
        reasons: {
            let mut r = f.reasons(default_ref);
            // The ladder's answer and the STACK's answer are two different
            // questions, and a row that showed only the first would
            // surprise anyone who then started a review from it: the base
            // ladder is default-branch-relative (D15's four rungs), while
            // a stack layer's own base is its PARENT. Saying both, here,
            // is what keeps `POST /branches/review`'s `base_source:
            // "stack"` from looking like a different daemon's answer.
            if let Some(st) = stacks.get(&f.raw.name) {
                r.push(facts::Reason::new(
                    "stack-base",
                    format!(
                        "stack layer on {} — a review from here compares against {}, not {}",
                        st.base,
                        st.base,
                        f.base.ref_name.as_deref().unwrap_or("(no base)")
                    ),
                ));
            }
            r
        },
    }
}

/// Everything that shells out to git, in submission order. See
/// `history::facts`'s module doc for each rule; the ORDER here is the
/// budget: one enumeration pass for every ref, cheap derivations for every
/// ref, and the per-row subprocesses only for what survives.
#[allow(clippy::too_many_arguments)]
fn git_pass(
    repo_root: &std::path::Path,
    repo_name: &str,
    agent_emails: &[String],
    open_reviews: &[crate::store::ReviewRow],
    favourites: &HashSet<String>,
    filter: &FactFilter,
    view: View,
    want_patch_id: bool,
    limit: usize,
    offset: usize,
    cache: &std::sync::Arc<parking_lot::Mutex<facts::BaseCache>>,
) -> Result<(GitPass, Option<TouchesRule>), ApiError> {
    // The default branch, resolved through the SAME gix helper
    // `/api/branches` uses (origin/HEAD, else HEAD's own branch). `GitRepo`
    // is `!Send`; it is opened and dropped entirely inside this blocking
    // closure, so it never crosses an await.
    let default_ref = {
        let git = crate::git::GitRepo::open(repo_root)?;
        crate::git::default_branch(&git)
    };
    let default_sha = default_ref
        .as_deref()
        .and_then(|r| crate::history::resolve_ref_commit(repo_root, r));

    let (raw_refs, ab_source) = facts::enumerate(repo_root, default_sha.as_deref())?;
    let mut raw_refs = facts::drop_shadowed_remotes(raw_refs);
    let enumerated_total = raw_refs.len();
    let enumeration_truncated = enumerated_total > facts::MAX_REFS;
    raw_refs.truncate(facts::MAX_REFS);

    let mine = facts::mine_identity(repo_root);
    let now = chrono::Utc::now().timestamp();
    let repo_root_canon =
        std::fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());

    // Pass 1 — everything free: agent provenance (trailers came with the
    // enumeration), age, mine, favourite, open reviews, ahead/behind.
    let ages: Vec<i64> = raw_refs
        .iter()
        .map(|r| now.saturating_sub(r.author_time).max(0))
        .collect();
    let stale_rule = facts::stale_rule(&ages);

    let mut out: Vec<BranchFact> = Vec::with_capacity(raw_refs.len());
    for raw in raw_refs {
        let age_secs = now.saturating_sub(raw.author_time).max(0);
        let agent = facts::agent_provenance(&raw, agent_emails);
        let review_ids: Vec<i64> = open_reviews
            .iter()
            .filter(|r| crate::routes::head_ref_names_branch(&r.head_ref, &raw.full_ref, &raw.name))
            .map(|r| r.id)
            .collect();
        // A branch with an OPEN REVIEW is never stale, whatever the
        // distribution says (`facts::STALE_RULE_TEXT`).
        let stale = match stale_rule.threshold_age_secs {
            Some(t) => age_secs > t && review_ids.is_empty(),
            None => false,
        };
        let worktree = raw.worktree_path.as_ref().and_then(|p| {
            let canon = std::fs::canonicalize(p).unwrap_or_else(|_| std::path::PathBuf::from(p));
            (canon != repo_root_canon).then(|| p.clone())
        });
        let mine_row = mine.matches(&raw);
        let favourite = favourites.contains(&raw.full_ref);
        out.push(BranchFact {
            base: facts::BranchBase::unknown(),
            ahead: None,
            behind: None,
            merged: None,
            agent,
            stale,
            mine: mine_row,
            open_review_ids: review_ids,
            favourite,
            age_secs,
            worktree,
            raw,
        });
    }

    // Pass 2 — the base ladder + ahead/behind for EVERY row (view
    // membership needs them). The fork-point probe is the only subprocess
    // and is cached by `(repo, ref, tip, base)`.
    for f in out.iter_mut() {
        let key = facts::BaseCacheKey {
            repo: repo_name.to_string(),
            full_ref: f.raw.full_ref.clone(),
            tip_sha: f.raw.tip_sha.clone(),
            default_sha: default_sha.clone().unwrap_or_default(),
        };
        // Two SEPARATE, short lock scopes. A `match cache.lock().get(..)`
        // would hold the scrutinee's temporary guard for the whole match
        // ARM — and `parking_lot::Mutex` is not reentrant, so the `put`
        // inside the miss arm deadlocks the request. (It did, and the test
        // that caught it simply hung.) Root CLAUDE.md #15's "never hold a
        // guard across an await" has a sibling: never hold one across a
        // call that takes it again.
        let cached = cache.lock().get(&key);
        let base = match cached {
            Some(b) => b,
            None => {
                let b = facts::detect_base(
                    repo_root,
                    &f.raw,
                    default_ref.as_deref(),
                    default_sha.as_deref(),
                );
                cache.lock().put(key, b.clone());
                b
            }
        };
        let (ahead, behind) = match base.class {
            // Measured by git itself, in the one pass.
            BaseClass::Upstream => {
                if f.raw.track.measured {
                    (Some(f.raw.track.ahead), Some(f.raw.track.behind))
                } else {
                    (None, None)
                }
            }
            BaseClass::ForkPoint | BaseClass::MergeBase => match f.raw.ahead_behind_default {
                Some((a, b)) => (Some(a), Some(b)),
                // This git has no `%(ahead-behind:)` atom — fall back to
                // the per-row `rev-list` `/api/branches` has always used.
                None => rev_list_ahead_behind(repo_root, default_ref.as_deref(), &f.raw),
            },
            // Nothing to measure against; never a measured-looking zero.
            BaseClass::Unknown => (None, None),
        };
        f.base = base;
        f.ahead = ahead;
        f.behind = behind;
        // Merged by ANCESTRY is free: 0 ahead of the base means every
        // commit is already reachable from it.
        if let (Some(0), Some(base_ref)) = (f.ahead, f.base.ref_name.clone()) {
            let is_default = default_ref.as_deref() == Some(f.raw.name.as_str());
            if !is_default {
                f.merged = Some(facts::MergedWitness {
                    kind: facts::MergedKind::Ancestry,
                    into: base_ref,
                    into_sha: default_sha.clone(),
                    equivalent: None,
                });
            }
        }
    }

    // Pass 3 — the patch-id probe (squash detection), capped, newest tip
    // first so the cap picks the rows an operator is most likely looking
    // at. Runs only when it was asked for.
    let mut patch_id_probed = 0usize;
    let patch_id_candidates = if want_patch_id {
        out.iter()
            .filter(|f| {
                f.merged.is_none()
                    && f.ahead.map(|a| a > 0).unwrap_or(false)
                    && f.base.ref_name.is_some()
            })
            .count()
    } else {
        0
    };
    if want_patch_id {
        let mut order: Vec<usize> = (0..out.len()).collect();
        order.sort_by(|&a, &b| out[b].raw.author_time.cmp(&out[a].raw.author_time));
        let mut probed = 0usize;
        for i in order {
            if probed >= facts::MAX_PATCH_ID_PROBES {
                break;
            }
            if out[i].merged.is_some() || out[i].ahead.map(|a| a == 0).unwrap_or(true) {
                continue;
            }
            let Some(base_ref) = out[i].base.ref_name.clone() else {
                continue;
            };
            let (Ok(b), Ok(h)) = (
                crate::git::Revspec::parse(&base_ref),
                crate::git::Revspec::parse(&out[i].raw.full_ref),
            ) else {
                continue;
            };
            probed += 1;
            if let Some(equivalent) = facts::patch_id_merged(repo_root, &b, &h) {
                out[i].merged = Some(facts::MergedWitness {
                    kind: facts::MergedKind::PatchId,
                    into: base_ref,
                    into_sha: out[i].base.sha.clone(),
                    equivalent: Some(equivalent),
                });
            }
        }
        patch_id_probed = probed;
    }

    // Pass 4 — the cheap kbcq/1 filter, then the sort, then the capped
    // `touches:` scan over what survived.
    out.retain(|f| filter.matches_cheap(f));
    out.sort_by(|a, b| {
        b.raw
            .author_time
            .cmp(&a.raw.author_time)
            .then_with(|| a.raw.name.cmp(&b.raw.name))
    });

    let mut touches_rule = None;
    if let Some(path) = filter.touches.clone() {
        let candidates = out.len();
        let mut scanned = 0usize;
        let mut keep: Vec<bool> = Vec::with_capacity(out.len());
        for f in out.iter() {
            if scanned >= facts::MAX_TOUCHES_SCAN {
                keep.push(false);
                continue;
            }
            let Some(base_sha) = f.base.sha.clone() else {
                keep.push(false);
                continue;
            };
            scanned += 1;
            keep.push(facts::touches_path(
                repo_root,
                &base_sha,
                &f.raw.tip_sha,
                &path,
            ));
        }
        out = out
            .into_iter()
            .zip(keep)
            .filter_map(|(f, k)| k.then_some(f))
            .collect();
        touches_rule = Some(TouchesRule {
            path,
            scanned,
            candidates,
            cap: facts::MAX_TOUCHES_SCAN,
        });
    }

    // Pass 5 — stack detection, for the rows a caller can actually see.
    // Reuses `history::stacks` rather than re-deriving a second, subtly
    // different per-level base.
    let stacks = detect_stacks_for(repo_root, &out, default_ref.as_deref(), view, limit, offset);

    Ok((
        GitPass {
            default_ref,
            default_sha,
            facts: out,
            enumerated: enumerated_total.min(facts::MAX_REFS),
            enumeration_truncated,
            ab_source,
            stale: stale_rule,
            stacks,
            patch_id_probed,
            patch_id_candidates,
            review_refs: open_reviews
                .iter()
                .map(|r| (r.id, (r.head_ref.clone(), r.base_ref.clone())))
                .collect(),
        },
        touches_rule,
    ))
}

/// The `rev-list` fallback for a git with no `%(ahead-behind:)` atom.
fn rev_list_ahead_behind(
    repo_root: &std::path::Path,
    default_ref: Option<&str>,
    raw: &facts::RawRef,
) -> (Option<u32>, Option<u32>) {
    let Some(default_ref) = default_ref else {
        return (None, None);
    };
    let (Ok(d), Ok(b)) = (
        crate::git::Revspec::parse(default_ref),
        crate::git::Revspec::parse(&raw.full_ref),
    ) else {
        return (None, None);
    };
    match crate::history::branches::ahead_behind(repo_root, &crate::git::RefRange::new(d, b, true))
    {
        Ok(ab) => (Some(ab.ahead), Some(ab.behind)),
        Err(_) => (None, None),
    }
}

/// Stack layers for the rows this request will actually render, keyed by
/// branch name. `include_all` is on: a single-layer stack is still a
/// per-level base worth showing on a branch ROW (the `~stacks` cockpit's
/// own default hides it because a one-layer "stack" is not a stack).
fn detect_stacks_for(
    repo_root: &std::path::Path,
    facts_rows: &[BranchFact],
    default_ref: Option<&str>,
    view: View,
    limit: usize,
    offset: usize,
) -> HashMap<String, StackOut> {
    let mut out = HashMap::new();
    let page: Vec<&BranchFact> = facts_rows
        .iter()
        .filter(|f| f.in_view(view))
        .skip(offset)
        .take(limit)
        .collect();
    if page.is_empty() {
        return out;
    }
    let tips: Vec<crate::history::stacks::BranchTip> = facts_rows
        .iter()
        .filter(|f| f.raw.remote.is_none())
        .map(|f| crate::history::stacks::BranchTip {
            name: f.raw.name.clone(),
            tip_sha: f.raw.tip_sha.clone(),
        })
        .collect();
    let Ok(detected) = crate::history::stacks::detect_stacks(repo_root, &tips, default_ref, true)
    else {
        return out;
    };
    let visible: HashSet<&str> = page.iter().map(|f| f.raw.name.as_str()).collect();
    for stack in detected.stacks {
        for (depth, layer) in stack.layers.iter().enumerate() {
            if !visible.contains(layer.branch.as_str()) || layer.unresolved {
                continue;
            }
            out.insert(
                layer.branch.clone(),
                StackOut {
                    base: layer.base.clone(),
                    depth: depth + 1,
                    stale: layer.stale,
                },
            );
        }
    }
    out
}

// --- GET /api/branches/conflicts -----------------------------------------

#[derive(Debug, Deserialize)]
pub struct ConflictsParams {
    pub repo: String,
    /// The ref every candidate is merged against (usually the default
    /// branch). Required: a radar with no target is not a question.
    pub against: String,
    #[serde(default)]
    pub limit: Option<usize>,
    /// A kbcq/1 query, narrowing the candidate set the same way
    /// `/facts?q=` does — so "conflicts among the agent branches" is one
    /// address, not a client-side join.
    ///
    /// Only the atoms computable from the one `for-each-ref` pass apply
    /// here (`branch:`, `by:`, `agent:`, and the residual name text).
    /// `touches:` is REFUSED rather than silently ignored: it needs a
    /// per-branch diff against a base this route never resolves, and a
    /// filter that parses and does nothing is the dead surface kbcq/1's
    /// own walk exists to prevent.
    ///
    /// There is deliberately no `?view=`: `stale`, `merged`, `review` and
    /// `mine` are derived from the store, the activity distribution and
    /// the repo identity, none of which this route computes — offering
    /// them would return an empty page that looks like an answer.
    #[serde(default)]
    pub q: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ConflictsResponse {
    pub schema: &'static str,
    pub repo: String,
    #[serde(flatten)]
    pub radar: radar::Radar,
    /// The honest caption, pre-rendered so the CLI and the SPA print the
    /// same sentence.
    pub caption: String,
}

/// `GET /api/branches/conflicts?repo=&against=&limit=&q=` (V75-M3,
/// D15) — "which of these branches would collide with `against`?".
///
/// `merge-tree --write-tree` per candidate, against a PER-REQUEST scratch
/// object directory (SEC-15) — the browsed repo's ODB is never written,
/// which is also why this needs no write access to the repo at all. A
/// hard pair cap ([`radar::MAX_PAIRS`]) with `budget.computed` of
/// `budget.candidates` on the wire, and a typed `503`
/// (`urn:kb:errors:scratch-unwritable`) when the daemon's own scratch root
/// is not writable.
pub async fn conflicts_route(
    State(state): State<SharedState>,
    Query(params): Query<ConflictsParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &params.repo)?;
    let against = parse_revspec(&params.against)?;
    let limit = params
        .limit
        .unwrap_or(radar::DEFAULT_PAIRS)
        .clamp(1, radar::MAX_PAIRS);
    let parsed = crate::search::grammar::parse(params.q.as_deref().unwrap_or(""));
    let filter = FactFilter::from_parsed(&parsed);
    if filter.needs_touches_scan() {
        return Err(ApiError::bad_request(
            "`touches:` is not supported by the conflict radar — it needs a per-branch diff \
             against a base this route does not resolve. Narrow with \
             `GET /api/branches/facts?q=touches:…` first, then pass `branch:` here",
        ));
    }

    let repo_root = repo.path.clone();
    let scratch_root = state.scratch_root.clone();
    let agent_emails = state.branches.resolved_agent_emails();
    let permit = state
        .git_fanout
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let out = tokio::task::spawn_blocking(move || -> Result<radar::Radar, ApiError> {
        let _permit = permit;
        let against_sha = crate::history::resolve_ref_commit(&repo_root, against.as_str())
            .ok_or_else(|| {
                ApiError::bad_request(format!(
                    "`against` does not resolve to a commit: {:?}",
                    against.as_str()
                ))
            })?;
        let (raw_refs, _src) = facts::enumerate(&repo_root, Some(&against_sha))?;
        let raw_refs = facts::drop_shadowed_remotes(raw_refs);
        let now = chrono::Utc::now().timestamp();
        let mut candidates: Vec<radar::Candidate> = Vec::new();
        let mut total = 0usize;
        let mut rows: Vec<(i64, radar::Candidate)> = Vec::new();
        for raw in raw_refs {
            // The radar never merges a ref with itself, and a ref already
            // contained in `against` (0 ahead) cannot conflict with it.
            if raw.tip_sha == against_sha {
                continue;
            }
            if matches!(raw.ahead_behind_default, Some((0, _))) {
                continue;
            }
            let fact = BranchFact {
                agent: facts::agent_provenance(&raw, &agent_emails),
                base: facts::BranchBase::unknown(),
                ahead: raw.ahead_behind_default.map(|(a, _)| a),
                behind: raw.ahead_behind_default.map(|(_, b)| b),
                merged: None,
                stale: false,
                mine: false,
                open_review_ids: Vec::new(),
                favourite: false,
                age_secs: now.saturating_sub(raw.author_time).max(0),
                worktree: None,
                raw,
            };
            if !filter.matches_cheap(&fact) {
                continue;
            }
            total += 1;
            rows.push((
                fact.raw.author_time,
                radar::Candidate {
                    branch: fact.raw.name.clone(),
                    full_ref: fact.raw.full_ref.clone(),
                    tip_sha: fact.raw.tip_sha.clone(),
                },
            ));
        }
        // Newest tip first, so a cap keeps the rows an operator is most
        // likely asking about.
        rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.branch.cmp(&b.1.branch)));
        for (_, c) in rows.into_iter().take(limit) {
            candidates.push(c);
        }
        Ok(radar::radar(
            &repo_root,
            &scratch_root,
            &against,
            &against_sha,
            &candidates,
            total,
        )?)
    })
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("conflict-radar task panicked: {e}"),
        )
    })??;

    let caption = format!(
        "{} of {} candidate branch(es) computed (cap {}); {} hunk probe(s) used of {}{}",
        out.budget.computed,
        out.budget.candidates,
        out.budget.pair_cap,
        out.budget.hunk_probes_used,
        out.budget.hunk_probe_cap,
        if out.budget.hunk_budget_exhausted {
            " — some hunk counts are absent, not zero"
        } else {
            ""
        }
    );

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(ConflictsResponse {
            schema: radar::SCHEMA,
            repo: params.repo,
            radar: out,
            caption,
        }),
    ))
}

// --- GET/POST /api/branches/favourites -----------------------------------

#[derive(Debug, Deserialize)]
pub struct FavouritesParams {
    pub repo: String,
}

#[derive(Debug, Serialize)]
pub struct FavouritesOut {
    pub schema: &'static str,
    pub repo: String,
    /// FULL refs (`refs/heads/x`), newest star first.
    pub favourites: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct SetFavouriteBody {
    pub repo: String,
    /// The FULL ref, as `branch-facts/1` reports it.
    #[serde(rename = "ref")]
    pub ref_name: String,
    /// `true` = star, `false` = unstar. Idempotent either way.
    pub on: bool,
}

#[derive(Debug, Serialize)]
pub struct SetFavouriteOut {
    pub schema: &'static str,
    pub repo: String,
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub on: bool,
    /// `false` when the star was already in the requested state — an
    /// honest no-op, not an error.
    pub changed: bool,
}

/// `GET /api/branches/favourites?repo=` — the starred FULL refs.
pub async fn list_favourites(
    State(state): State<SharedState>,
    Query(params): Query<FavouritesParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &params.repo)?;
    let repo_name = repo.name.clone();
    let rows = state
        .store
        .run_blocking(move |store| store.list_branch_favourites(&repo_name))
        .await?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(FavouritesOut {
            schema: "branch-favourites/1",
            repo: params.repo,
            favourites: rows,
        }),
    ))
}

/// `POST /api/branches/favourites` — star/unstar one ref.
///
/// Auth posture: the ordinary `auth_bearer` surface, NOT loopback-only. A
/// favourite is an operator PREFERENCE (the `bookmarks` V0011 /
/// `doc_lens_pins` V0020 precedent), not a tree or ref mutation, and it is
/// daemon-GLOBAL rather than per-identity — kb-code has one identity (the
/// v0.34 ruling).
///
/// The `ref` is validated through `Revspec` even though it never reaches
/// git here: a ref this daemon will later hand to `merge-base`/`cherry`
/// must not be storable in a shape those calls would refuse.
pub async fn set_favourite(
    State(state): State<SharedState>,
    Json(body): Json<SetFavouriteBody>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &body.repo)?;
    let spec = parse_revspec(&body.ref_name)?;
    let repo_name = repo.name.clone();
    let ref_name = spec.as_str().to_string();
    let on = body.on;
    let now = chrono::Utc::now().timestamp();
    let ref_for_store = ref_name.clone();
    let repo_for_store = repo_name.clone();
    let changed = state
        .store
        .run_blocking(move |store| {
            store.set_branch_favourite(&repo_for_store, &ref_for_store, on, now)
        })
        .await?;
    state
        .bus
        .emit("branch.changed", serde_json::json!({ "repo": repo_name }));
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(SetFavouriteOut {
            schema: "branch-favourites/1",
            repo: body.repo,
            ref_name,
            on,
            changed,
        }),
    ))
}

// --- POST /api/branches/review -------------------------------------------

#[derive(Debug, Deserialize)]
pub struct BranchReviewBody {
    pub repo: String,
    /// The branch to review — a full ref or a short name.
    #[serde(rename = "ref")]
    pub ref_name: String,
    /// `"auto"` (default) resolves through the SAME classed ladder
    /// `branch-facts/1` reports; anything else is used verbatim.
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BranchReviewOut {
    pub schema: &'static str,
    /// The classed base this review was started against — the whole point
    /// of the verb. `class` is `upstream|fork-point|merge-base|unknown`,
    /// and an `unknown` base REFUSES rather than silently reviewing
    /// against the default branch.
    pub base: facts::BranchBase,
    /// Which decision produced `base`: `explicit` (the caller named it),
    /// `stack` (this branch is a dependent-stack LAYER, so its own parent
    /// is the base — reviewing it against the default branch is exactly
    /// the "drowned in the layer below's diff" problem stacks cause), or
    /// `ladder` (the four-rung classed ladder). Never inferred by the
    /// caller from the other fields.
    pub base_source: &'static str,
    /// `true` when the diff this base implies is the three-dot one
    /// (`base...head`), which is the default and what every review reads.
    pub three_dot: bool,
    /// The `POST /api/reviews` response, verbatim.
    pub review: serde_json::Value,
}

/// `POST /api/branches/review` (V75-M3, D15) — "Compare with common base",
/// as a verb.
///
/// The value is not that it creates a review — `POST /api/reviews` does
/// that. It is that the base is DETECTED through the classed ladder and
/// STATED, so a stacked branch is reviewed against its own parent rather
/// than against `main` (the GitHub weakness D15's evidence names), and a
/// branch whose base cannot be determined is REFUSED rather than reviewed
/// against a guess.
///
/// Loopback-only — see this module's doc for why it is not on
/// `review_gate`.
pub async fn start_branch_review(
    State(state): State<SharedState>,
    Json(body): Json<BranchReviewBody>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &body.repo)?;
    let head = parse_revspec(&body.ref_name)?;
    let explicit_base = body
        .base
        .as_deref()
        .filter(|b| !b.eq_ignore_ascii_case("auto"))
        .map(parse_revspec)
        .transpose()?;

    let repo_root = repo.path.clone();
    let head_for_task = head.clone();
    let permit = state
        .git_fanout
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let explicit_for_task = explicit_base.clone();
    let (base, base_source) = tokio::task::spawn_blocking(
        move || -> Result<(facts::BranchBase, &'static str), ApiError> {
            let _permit = permit;
            if let Some(b) = explicit_for_task {
                return Ok((
                    facts::BranchBase {
                        // UNKNOWN on purpose: the caller chose this ref,
                        // so this daemon has classed nothing and must not
                        // borrow the ladder's credibility for it.
                        class: BaseClass::Unknown,
                        ref_name: Some(b.as_str().to_string()),
                        sha: crate::history::resolve_ref_commit(&repo_root, b.as_str()),
                    },
                    "explicit",
                ));
            }
            let default_ref = {
                let git = crate::git::GitRepo::open(&repo_root)?;
                crate::git::default_branch(&git)
            };
            let default_sha = default_ref
                .as_deref()
                .and_then(|r| crate::history::resolve_ref_commit(&repo_root, r));
            let (raw_refs, _src) = facts::enumerate(&repo_root, default_sha.as_deref())?;
            let raw_refs = facts::drop_shadowed_remotes(raw_refs);
            let raw = raw_refs
                .iter()
                .find(|r| r.full_ref == head_for_task.as_str() || r.name == head_for_task.as_str())
                .cloned()
                .ok_or_else(|| {
                    ApiError::not_found(format!("no such branch: {:?}", head_for_task.as_str()))
                })?;

            // A dependent-stack LAYER's base is its own parent. Reviewing
            // `B` (atop `A` atop `main`) against `main` drowns the reviewer
            // in `A`'s diff — the exact failure `history::stacks` exists to
            // see — so the stack's per-level base wins over the ladder's
            // default-branch answer when there is one. Reused, never
            // re-derived: a second base detector here would be a second
            // answer to the same question.
            let tips: Vec<crate::history::stacks::BranchTip> = raw_refs
                .iter()
                .filter(|r| r.remote.is_none())
                .map(|r| crate::history::stacks::BranchTip {
                    name: r.name.clone(),
                    tip_sha: r.tip_sha.clone(),
                })
                .collect();
            if let Ok(detected) = crate::history::stacks::detect_stacks(
                &repo_root,
                &tips,
                default_ref.as_deref(),
                true,
            ) {
                for stack in detected.stacks {
                    for layer in stack.layers {
                        if layer.branch != raw.name || layer.unresolved || layer.base.is_empty() {
                            continue;
                        }
                        if default_ref.as_deref() == Some(layer.base.as_str()) {
                            continue;
                        }
                        return Ok((
                            facts::BranchBase {
                                class: BaseClass::MergeBase,
                                sha: crate::history::merge_base_of(
                                    &repo_root,
                                    &layer.base,
                                    &raw.tip_sha,
                                ),
                                ref_name: Some(layer.base),
                            },
                            "stack",
                        ));
                    }
                }
            }
            Ok((
                facts::detect_base(
                    &repo_root,
                    &raw,
                    default_ref.as_deref(),
                    default_sha.as_deref(),
                ),
                "ladder",
            ))
        },
    )
    .await
    .map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("branch-review task panicked: {e}"),
        )
    })??;

    // An explicit base is reported with the class the caller's own choice
    // deserves: `unknown`, because this daemon did not detect it. An
    // AUTO base that came back unknown is a refusal — reviewing against a
    // silently-substituted default is exactly what D15 forbids.
    if explicit_base.is_none() && base.class == BaseClass::Unknown {
        return Err(ApiError::bad_request(format!(
            "the base of {:?} could not be classed (no upstream, no fork point, no merge base \
             with the default branch) — pass an explicit --base rather than reviewing against \
             a guess",
            head.as_str()
        )));
    }
    let Some(base_ref) = base.ref_name.clone() else {
        return Err(ApiError::bad_request("the resolved base names no ref"));
    };

    // Composed, never re-implemented: `create_review_value` IS
    // `POST /api/reviews`'s body, so this verb cannot drift from it (and
    // cannot be a weaker gate than it — see this module's doc).
    let review = crate::reviews::create_review_value(
        &state,
        crate::reviews::CreateReviewBody {
            repo: body.repo.clone(),
            head_ref: head.as_str().to_string(),
            base_ref: Some(base_ref),
            title: body.title.clone(),
            session_id: body.session_id.clone(),
        },
    )
    .await?;

    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(BranchReviewOut {
            schema: "branch-review/1",
            base,
            base_source,
            // Every review in this daemon reads its diff as `base...head`
            // (`reviews::capture_patchset` resolves the merge base), so the
            // three-dot form is not an option here — it is a statement of
            // what the caller is about to read.
            three_dot: true,
            review,
        }),
    ))
}

// --- route contracts (invariant 15) --------------------------------------

/// V75-M3's routes, declared beside the handlers so
/// `entities::every_declared_v71_g0_route_is_registered_and_requires_its_params`
/// and kb-code-cli's `cli_requests_send_every_param_their_route_requires`
/// can both walk them.
pub const BRANCH_FACTS_ROUTE: crate::entities::RouteContract = crate::entities::RouteContract {
    path: "/api/branches/facts",
    handler: "branches::facts_route",
    required_params: &["repo"],
    params_accept_without: facts_params_accept_without,
};

pub const BRANCH_CONFLICTS_ROUTE: crate::entities::RouteContract = crate::entities::RouteContract {
    path: "/api/branches/conflicts",
    handler: "branches::conflicts_route",
    required_params: &["repo", "against"],
    params_accept_without: conflicts_params_accept_without,
};

pub const BRANCH_FAVOURITES_ROUTE: crate::entities::RouteContract =
    crate::entities::RouteContract {
        path: "/api/branches/favourites",
        handler: "branches::list_favourites",
        required_params: &["repo"],
        params_accept_without: favourites_params_accept_without,
    };

pub const V75_M3_ROUTES: &[crate::entities::RouteContract] = &[
    BRANCH_FACTS_ROUTE,
    BRANCH_CONFLICTS_ROUTE,
    BRANCH_FAVOURITES_ROUTE,
];

/// Build a complete query map with `omit` removed — the `seq.rs` shape,
/// deserialized into the route's OWN params struct so the check runs
/// against the real type rather than a restatement of it.
fn without(omit: &str, pairs: &[(&str, serde_json::Value)]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (k, v) in pairs {
        if *k != omit {
            map.insert((*k).to_string(), v.clone());
        }
    }
    serde_json::Value::Object(map)
}

fn facts_params_accept_without(omit: &str) -> bool {
    let v = without(
        omit,
        &[
            ("repo", serde_json::json!("kb")),
            ("view", serde_json::json!("all")),
            ("q", serde_json::json!("auth")),
            ("limit", serde_json::json!(10)),
            ("offset", serde_json::json!(0)),
        ],
    );
    serde_json::from_value::<FactsParams>(v).is_ok()
}

fn conflicts_params_accept_without(omit: &str) -> bool {
    let v = without(
        omit,
        &[
            ("repo", serde_json::json!("kb")),
            ("against", serde_json::json!("main")),
            ("limit", serde_json::json!(5)),
            ("q", serde_json::json!("branch:x")),
        ],
    );
    serde_json::from_value::<ConflictsParams>(v).is_ok()
}

fn favourites_params_accept_without(omit: &str) -> bool {
    let v = without(omit, &[("repo", serde_json::json!("kb"))]);
    serde_json::from_value::<FavouritesParams>(v).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mistyped_view_is_a_400_naming_the_closed_set_not_a_silent_all() {
        assert_eq!(parse_view(None).unwrap(), View::All);
        assert_eq!(
            parse_view(Some(&"merged".to_string())).unwrap(),
            View::Merged
        );
        let err = parse_view(Some(&"recent".to_string())).unwrap_err();
        assert_eq!(err.status_code(), StatusCode::BAD_REQUEST);
        assert!(
            err.message().contains("current|mine|agent"),
            "{}",
            err.message()
        );
    }

    #[test]
    fn ci_rollup_is_worst_of_and_names_its_count() {
        let run = |s: &str| crate::github::CheckRunOut {
            name: "c".to_string(),
            status: s.to_string(),
            note: None,
            duration: None,
        };
        assert_eq!(roll_up_ci(&[]).status, "none");
        assert_eq!(roll_up_ci(&[run("pass")]).status, "pass");
        assert_eq!(roll_up_ci(&[run("pass"), run("warn")]).status, "warn");
        assert_eq!(roll_up_ci(&[run("warn"), run("pending")]).status, "pending");
        assert_eq!(roll_up_ci(&[run("pending"), run("fail")]).status, "fail");
        assert_eq!(roll_up_ci(&[run("pass"), run("pass")]).checks, 2);
    }

    #[test]
    fn truthy_accepts_only_the_documented_spellings() {
        for v in ["1", "true", "yes", "YES", " True "] {
            assert!(truthy(Some(&v.to_string())), "{v:?}");
        }
        for v in ["0", "no", "", "maybe"] {
            assert!(!truthy(Some(&v.to_string())), "{v:?}");
        }
        assert!(!truthy(None));
    }

    #[test]
    fn every_declared_route_requires_its_params() {
        for c in V75_M3_ROUTES {
            assert!(
                (c.params_accept_without)(""),
                "{}: the params struct must accept a complete query",
                c.path
            );
            for p in c.required_params {
                assert!(
                    !(c.params_accept_without)(p),
                    "{}: the params struct accepts a query missing {p:?}",
                    c.path
                );
            }
        }
    }
}
