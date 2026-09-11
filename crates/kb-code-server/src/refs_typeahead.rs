//! V76-R3c — `GET /api/refs/typeahead?repo=&q=`.
//!
//! Ranked ref picker over branches, tags, `refs/kbc/pr/*`, review
//! patchsets, `HEAD~n`, SHA prefixes (≥ 7), and linked worktrees.
//! Ranking is deterministic: exact > prefix > recent > substring. The
//! page is capped; `total` is the untruncated match count (true totals,
//! never a silent trim).

use crate::entities::RouteContract;
use crate::git::{GitRepo, RefKind};
use crate::routes::{find_repo, ApiError};
use crate::state::SharedState;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = "kbc-refs-typeahead/1";
pub const DEFAULT_CAP: usize = 25;
pub const MAX_CAP: usize = 100;

#[derive(Debug, Deserialize)]
pub struct TypeaheadParams {
    pub repo: String,
    pub q: Option<String>,
    pub limit: Option<usize>,
}

fn typeahead_params_accept_without(omit: &str) -> bool {
    let mut q = serde_json::json!({ "repo": "r", "q": "main" });
    q.as_object_mut().expect("object").remove(omit);
    serde_json::from_value::<TypeaheadParams>(q).is_ok()
}

pub const TYPEAHEAD_ROUTE: RouteContract = RouteContract {
    path: "/api/refs/typeahead",
    handler: "refs_typeahead::typeahead_route",
    required_params: &["repo"],
    params_accept_without: typeahead_params_accept_without,
};

pub const V76_R3C_ROUTES: &[RouteContract] =
    &[TYPEAHEAD_ROUTE, crate::compare_file::COMPARE_FILE_ROUTE];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TypeaheadHit {
    /// What the picker shows.
    pub name: String,
    /// `branch` | `tag` | `pr` | `patchset` | `sha` | `rev` | `worktree`
    pub kind: &'static str,
    /// What lands in `?ref=` — a validated revspec spelling.
    pub insert: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    pub recent: bool,
}

#[derive(Debug, Serialize)]
pub struct TypeaheadResponse {
    pub schema: &'static str,
    pub repo: String,
    pub q: String,
    pub hits: Vec<TypeaheadHit>,
    pub returned: usize,
    pub total: usize,
    pub truncated: bool,
    pub cap: usize,
}

/// Rank bucket: exact (0) > prefix (1) > recent-on-empty / substring (2).
/// Hits that don't match at all are `None` (dropped) when `q` is non-empty.
fn rank_bucket(q: &str, hit: &TypeaheadHit) -> Option<u8> {
    if q.is_empty() {
        return Some(if hit.recent { 0 } else { 1 });
    }
    let qn = q.to_ascii_lowercase();
    let name = hit.name.to_ascii_lowercase();
    let insert = hit.insert.to_ascii_lowercase();
    if name == qn || insert == qn {
        return Some(0);
    }
    if name.starts_with(&qn) || insert.starts_with(&qn) {
        return Some(1);
    }
    if hit
        .sha
        .as_deref()
        .is_some_and(|s| s.to_ascii_lowercase().starts_with(&qn))
    {
        return Some(1);
    }
    if name.contains(&qn) || insert.contains(&qn) {
        return Some(2);
    }
    None
}

/// Pure rank+cap. `total` is the match count before the cap.
pub fn rank_hits(q: &str, hits: Vec<TypeaheadHit>, cap: usize) -> (Vec<TypeaheadHit>, usize) {
    let q = q.trim();
    let mut scored: Vec<(u8, String, TypeaheadHit)> = hits
        .into_iter()
        .filter_map(|h| {
            let bucket = rank_bucket(q, &h)?;
            Some((bucket, h.name.to_ascii_lowercase(), h))
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let total = scored.len();
    let page = scored.into_iter().take(cap).map(|(_, _, h)| h).collect();
    (page, total)
}

fn is_hex_prefix(q: &str) -> bool {
    let n = q.len();
    (7..=40).contains(&n) && q.chars().all(|c| c.is_ascii_hexdigit())
}

fn collect_hits(git: &GitRepo, q: &str) -> Result<Vec<TypeaheadHit>, ApiError> {
    let mut hits: Vec<TypeaheadHit> = Vec::new();
    let refs = git.list_refs()?;
    for r in refs {
        let kind = match r.kind {
            RefKind::Branch => "branch",
            RefKind::Tag => "tag",
        };
        hits.push(TypeaheadHit {
            name: r.name.clone(),
            kind,
            insert: r.name,
            sha: Some(r.target_sha),
            recent: r.is_head,
        });
    }

    for k in git.list_kbc_refs()? {
        if let Some(n) = k.full_name.strip_prefix("refs/kbc/pr/") {
            hits.push(TypeaheadHit {
                name: format!("pr/{n}"),
                kind: "pr",
                insert: k.full_name,
                sha: Some(k.target_sha),
                recent: false,
            });
        } else if k.full_name.contains("/ps") {
            let short = k
                .full_name
                .strip_prefix("refs/kbc/")
                .unwrap_or(&k.full_name)
                .to_string();
            hits.push(TypeaheadHit {
                name: short,
                kind: "patchset",
                insert: k.full_name,
                sha: Some(k.target_sha),
                recent: false,
            });
        }
    }

    for id in git.linked_worktree_ids() {
        hits.push(TypeaheadHit {
            name: id.clone(),
            kind: "worktree",
            insert: id,
            sha: None,
            recent: false,
        });
    }

    // Synthetic HEAD~n / HEAD^ spellings. Always offer HEAD; offer
    // HEAD~1..HEAD~9 when q is empty or a HEAD prefix so a typeahead
    // that has not been typed into still has a recency ladder.
    let head_recent = ["HEAD", "HEAD~1", "HEAD~2"];
    for spec in head_recent {
        hits.push(TypeaheadHit {
            name: spec.to_string(),
            kind: "rev",
            insert: spec.to_string(),
            sha: git.resolve(spec).ok().map(|id| id.to_string()),
            recent: spec == "HEAD" || spec == "HEAD~1",
        });
    }
    if q.starts_with("HEAD~") {
        for n in 3..=9 {
            let spec = format!("HEAD~{n}");
            if hits.iter().any(|h| h.insert == spec) {
                continue;
            }
            hits.push(TypeaheadHit {
                name: spec.clone(),
                kind: "rev",
                insert: spec.clone(),
                sha: git.resolve(&spec).ok().map(|id| id.to_string()),
                recent: false,
            });
        }
    }

    if is_hex_prefix(q) {
        if let Ok(id) = git.resolve(q) {
            let sha = id.to_string();
            if !hits.iter().any(|h| h.sha.as_deref() == Some(sha.as_str())) {
                hits.push(TypeaheadHit {
                    name: sha[..sha.len().min(12)].to_string(),
                    kind: "sha",
                    insert: sha.clone(),
                    sha: Some(sha),
                    recent: false,
                });
            }
        }
    }

    Ok(hits)
}

pub async fn typeahead_route(
    State(state): State<SharedState>,
    Query(params): Query<TypeaheadParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &params.repo)?;
    let q = params.q.as_deref().unwrap_or("").trim().to_string();
    let cap = params.limit.unwrap_or(DEFAULT_CAP).clamp(1, MAX_CAP);
    let git = GitRepo::open(&repo.path)?;
    let raw = collect_hits(&git, &q)?;
    let (hits, total) = rank_hits(&q, raw, cap);
    let returned = hits.len();
    let body = TypeaheadResponse {
        schema: SCHEMA,
        repo: params.repo,
        q,
        hits,
        returned,
        total,
        truncated: total > returned,
        cap,
    };
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(body)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(name: &str, kind: &'static str, recent: bool) -> TypeaheadHit {
        TypeaheadHit {
            name: name.to_string(),
            kind,
            insert: name.to_string(),
            sha: None,
            recent,
        }
    }

    #[test]
    fn exact_beats_prefix_beats_substring() {
        let hits = vec![
            hit("feature/mainish", "branch", false),
            hit("main", "branch", false),
            hit("maintain", "branch", false),
            hit("topic", "branch", false),
        ];
        let (page, total) = rank_hits("main", hits, 10);
        assert_eq!(total, 3, "topic is not a match");
        assert_eq!(
            page.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
            ["main", "maintain", "feature/mainish"]
        );
    }

    #[test]
    fn empty_q_puts_recent_first_then_name() {
        let hits = vec![
            hit("zeta", "branch", false),
            hit("alpha", "branch", true),
            hit("beta", "branch", false),
        ];
        let (page, total) = rank_hits("", hits, 10);
        assert_eq!(total, 3);
        assert_eq!(
            page.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
            ["alpha", "beta", "zeta"]
        );
    }

    #[test]
    fn cap_truncates_but_total_is_true() {
        let hits: Vec<_> = (0..10)
            .map(|i| hit(&format!("b{i:02}"), "branch", false))
            .collect();
        let (page, total) = rank_hits("b", hits, 3);
        assert_eq!(total, 10);
        assert_eq!(page.len(), 3);
    }

    #[test]
    fn ranking_is_case_insensitive_and_deterministic() {
        let hits = vec![
            hit("Main", "branch", false),
            hit("mainline", "branch", false),
        ];
        let (a, _) = rank_hits("MAIN", hits.clone(), 10);
        let (b, _) = rank_hits("main", hits, 10);
        assert_eq!(a, b);
        assert_eq!(a[0].name, "Main");
    }
}
