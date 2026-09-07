//! V73-K3 — `kbc-pseudo/1`: review-scoped PSEUDO-FILES (design D9,
//! "commit message and PR body as reviewable pseudo-files").
//!
//! A review is not only a diff. Four of the things a reviewer must actually
//! read — the PR description, the review document itself, the findings
//! sidecar, the commit list — are not files in the tree, so today they can
//! be *displayed* but never *addressed*: you cannot cite line 12 of a PR
//! body, and you cannot leave a comment on it that survives the next
//! patchset. This module makes them addressable by giving each one the two
//! properties a path in this daemon needs and nothing more:
//!
//!   * a stable NAME under the reserved [`PREFIX`] (`~review/pr-body.md`),
//!     which is not a legal repo path (a repo path never starts with `~`),
//!     so a pseudo name can never collide with, or be mistaken for, a file
//!     the tree actually has; and
//!   * a real `blob_sha` — literally [`crate::ingest::git_blob_hash`] over
//!     the rendered bytes, the SAME function the mirror index uses. That is
//!     what lets `[[code:~review/pr-body.md:12@<sha>]]` be `pinned` by byte
//!     equality on K1's own ladder, with no second notion of identity.
//!
//! # What this module is NOT
//!
//! It is not storage. Nothing here is written: every pseudo-file is
//! RENDERED per request from rows the daemon already keeps for other
//! reasons (`reviews.pr_meta_json`, `review_docs`, `review_findings`, and
//! `git log` over the patchset range) — `rails/1`'s "a per-request VIEW
//! over tables it does not own" posture, applied to a review's prose. A
//! consequence worth stating rather than discovering: because the PR body
//! is kept as ONE snapshot (`pr_meta_json`, wholesale-replaced by
//! `review sweep`), this daemon has no revision CHAIN for it. A change is
//! DETECTABLE — the `blob_sha` moves, and every ref pinned to the old one
//! reads `carried`/`orphan` instead of `pinned` — but the previous text is
//! not recoverable, and no surface here pretends otherwise.
//!
//! # Absence is content
//!
//! A review with no PR binding still HAS a `~review/pr-body.md`; it is
//! empty, `present: false`, and carries a `reason`. That is deliberate:
//! four names that always exist, two of which may be empty with a stated
//! reason, is a smaller surface than a set whose membership varies, and it
//! means a reading order can list chapter zero unconditionally.

use crate::ingest::git_blob_hash;
use crate::review_doc::DocFinding;
use crate::review_findings::FindingLocationBody;
use crate::store;
use serde::Serialize;

pub const SCHEMA: &str = "kbc-pseudo/1";

/// The reserved path prefix. A repo-relative path never begins with `~`
/// (`routes::safe_rel_path` accepts one lexically, but no git tree entry
/// this crate indexes carries it), so the two namespaces cannot collide.
pub const PREFIX: &str = "~review/";

pub const PR_BODY: &str = "pr-body.md";
pub const REVIEW_MD: &str = "review.md";
pub const FINDINGS_JSON: &str = "findings.json";
pub const COMMITS_MD: &str = "commits.md";

/// The closed set, in reading order. Chapter zero renders them in exactly
/// this sequence.
pub const NAMES: &[&str] = &[PR_BODY, REVIEW_MD, FINDINGS_JSON, COMMITS_MD];

/// How many commits `commits.md` will render before it says it stopped.
/// A refusal to render more is honest; a silently truncated list is not.
pub const MAX_COMMITS: usize = 500;

/// `~review/<name>` for a name in [`NAMES`].
pub fn path_for(name: &str) -> String {
    format!("{PREFIX}{name}")
}

/// The pseudo NAME a `~review/…` path addresses, or `None` for an ordinary
/// repo path. Total — never panics, never allocates on the common path.
pub fn name_for_path(path: &str) -> Option<&str> {
    let rest = path.strip_prefix(PREFIX)?;
    NAMES.iter().copied().find(|n| *n == rest)
}

pub fn is_pseudo_path(path: &str) -> bool {
    path.starts_with(PREFIX)
}

/// One rendered pseudo-file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PseudoFile {
    pub name: &'static str,
    pub path: String,
    /// The git blob hash of `content` — the same hash `files.blob_hash`
    /// carries for a real file, so a `@sha` ref compares directly.
    pub blob_sha: String,
    pub byte_len: usize,
    pub lines: usize,
    /// Where the bytes came from — named so a reader can tell a rendered
    /// snapshot from a stored document.
    pub source: &'static str,
    /// `false` when the review has nothing to render here. The file still
    /// exists, empty, with a `reason`.
    pub present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Omitted from the LIST read, carried by the single-file read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

impl PseudoFile {
    fn build(
        name: &'static str,
        source: &'static str,
        content: String,
        reason: Option<String>,
    ) -> Self {
        let present = !content.is_empty();
        Self {
            name,
            path: path_for(name),
            blob_sha: git_blob_hash(content.as_bytes()),
            byte_len: content.len(),
            lines: if content.is_empty() {
                0
            } else {
                content.lines().count()
            },
            source,
            present,
            reason: if present { None } else { reason },
            content: Some(content),
        }
    }

    /// The list-read projection — everything except the bytes.
    pub fn without_content(&self) -> Self {
        Self {
            content: None,
            ..self.clone()
        }
    }

    pub fn content(&self) -> &str {
        self.content.as_deref().unwrap_or("")
    }
}

/// All four, for one review at one patchset. Built once and shared by the
/// route, the timeline, the ref-card resolver and the reading order — so
/// the `blob_sha` a card pins against is the same one the route served.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PseudoSet {
    pub schema: &'static str,
    pub review_id: i64,
    pub ps_number: i64,
    pub files: Vec<PseudoFile>,
}

impl PseudoSet {
    pub fn get(&self, name_or_path: &str) -> Option<&PseudoFile> {
        let name = name_for_path(name_or_path).unwrap_or(name_or_path);
        self.files.iter().find(|f| f.name == name)
    }
}

/// One commit as `commits.md` renders it. The `Kb-Session:` trailer is
/// carried VERBATIM (never resolved here) because that trailer is the
/// exact evidence the session↔commit join reads, and a reviewer looking at
/// this file should see the same bytes the join does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PseudoCommit {
    pub sha: String,
    pub subject: String,
    pub author: String,
    pub author_time: i64,
    pub trailers: Vec<String>,
}

// --- the four renderers (pure) --------------------------------------------

/// `~review/pr-body.md` — the PR description from the `pr_meta_json`
/// snapshot, VERBATIM. No header is prepended: the file IS the body, so a
/// line number in a ref means the line the author wrote, not a line this
/// daemon shifted by decorating it.
pub fn render_pr_body(binding: &store::ReviewPrBinding) -> PseudoFile {
    let Some(meta) = binding.pr_meta_json.as_deref() else {
        return PseudoFile::build(
            PR_BODY,
            "pr_meta snapshot",
            String::new(),
            Some(
                "this review is not bound to a pull request (or its PR metadata was never \
                 fetched) — bind one with `kb-code review start-pr`, or refresh with \
                 `kb-code review sweep`"
                    .into(),
            ),
        );
    };
    let body = serde_json::from_str::<serde_json::Value>(meta)
        .ok()
        .and_then(|v| v.get("body").and_then(|b| b.as_str()).map(str::to_string));
    match body {
        Some(b) if !b.is_empty() => PseudoFile::build(PR_BODY, "pr_meta snapshot", b, None),
        _ => PseudoFile::build(
            PR_BODY,
            "pr_meta snapshot",
            String::new(),
            Some("this pull request has an empty description".into()),
        ),
    }
}

/// `~review/review.md` — the CURRENT kbc-review/1 document revision,
/// front matter and body, byte-for-byte as composed (K1's lossless record).
pub fn render_review_md(doc: Option<&store::ReviewDocRow>) -> PseudoFile {
    match doc {
        Some(row) => PseudoFile::build(
            REVIEW_MD,
            "review_docs (highest revision)",
            row.doc_md.clone(),
            None,
        ),
        None => PseudoFile::build(
            REVIEW_MD,
            "review_docs (highest revision)",
            String::new(),
            Some(
                "this review has no kbc-review/1 document at this patchset — compose one with \
                 `kb-code review compose <id> --doc <file.md>`"
                    .into(),
            ),
        ),
    }
}

/// `~review/findings.json` — the findings v2 SIDECAR, in exactly the shape
/// `kb-code review compose --findings <file>` accepts, so the file a
/// reviewer reads is the file an agent can re-compose from. Superseded
/// findings are excluded (they are not part of the current review's claim)
/// and the count of them is stated in `superseded` rather than dropped
/// silently — `review_analytics`' own rule.
pub fn render_findings_json(
    review_id: i64,
    ps_number: i64,
    findings: &[store::ReviewFindingRow],
) -> PseudoFile {
    let live: Vec<&store::ReviewFindingRow> = findings.iter().filter(|f| !f.superseded).collect();
    let superseded = findings.len() - live.len();
    let docs: Vec<DocFinding> = live.iter().map(|f| doc_finding_from_row(f)).collect();
    let value = serde_json::json!({
        "schema": "kbc-findings/2",
        "review_id": review_id,
        "ps_number": ps_number,
        "superseded": superseded,
        "findings": docs,
    });
    let content = serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into());
    PseudoFile::build(FINDINGS_JSON, "review_findings (v2)", content, None)
}

/// A stored finding row, projected back into the DOCUMENT shape. `slug` is
/// always carried (it is identity, D9) so re-composing this sidecar
/// reconciles onto the same rows rather than minting new ones.
fn doc_finding_from_row(f: &store::ReviewFindingRow) -> DocFinding {
    DocFinding {
        slug: Some(f.slug.clone()),
        act: f.act.clone(),
        severity: f.severity.clone(),
        category: f.category.clone(),
        blocking: f.blocking,
        title: f.title.clone(),
        rationale: f.rationale.clone(),
        recommendation: f.recommendation.clone(),
        location: FindingLocationBody {
            path: f.location_path.clone(),
            kind: f.location_kind.clone(),
            lines: f
                .location_lines
                .as_deref()
                .and_then(|s| serde_json::from_str::<Vec<i64>>(s).ok()),
            removed: f.location_removed,
        },
        cites: f
            .cites_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
            .unwrap_or_default(),
        // `supersedes` is an AUTHORING instruction, never a stored fact
        // read back — emitting one here would make a re-compose re-assert
        // a supersession that already happened.
        supersedes: Vec::new(),
        evidence: None,
    }
}

/// `~review/commits.md` — the patchset's commit list with its trailers.
pub fn render_commits_md(commits: &[PseudoCommit], truncated_from: Option<usize>) -> PseudoFile {
    if commits.is_empty() {
        return PseudoFile::build(
            COMMITS_MD,
            "git log <base>..<tip>",
            String::new(),
            Some("this patchset's range contains no commits".into()),
        );
    }
    let mut out = String::from("# Commits\n\n");
    for c in commits {
        out.push_str(&format!("## {} {}\n\n", short_sha(&c.sha), c.subject));
        out.push_str(&format!("- sha: `{}`\n", c.sha));
        out.push_str(&format!("- author: {}\n", c.author));
        out.push_str(&format!("- author_time: {}\n", c.author_time));
        if c.trailers.is_empty() {
            out.push_str("- trailers: (none)\n");
        } else {
            out.push_str("- trailers:\n");
            for t in &c.trailers {
                out.push_str(&format!("  - `{t}`\n"));
            }
        }
        out.push('\n');
    }
    if let Some(total) = truncated_from {
        out.push_str(&format!(
            "> This range has {total} commits; the first {} are rendered (the \
             `review_pseudo::MAX_COMMITS` budget). The number above is the TRUE total.\n",
            commits.len()
        ));
    }
    PseudoFile::build(COMMITS_MD, "git log <base>..<tip>", out, None)
}

fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// Build the whole set from already-fetched inputs. Pure — every caller
/// (route, timeline, cards, reading order) goes through this one function,
/// so no two surfaces can disagree about a pseudo-file's bytes or hash.
pub fn build_set(
    review_id: i64,
    ps_number: i64,
    binding: &store::ReviewPrBinding,
    doc: Option<&store::ReviewDocRow>,
    findings: &[store::ReviewFindingRow],
    commits: &[PseudoCommit],
    truncated_from: Option<usize>,
) -> PseudoSet {
    PseudoSet {
        schema: SCHEMA,
        review_id,
        ps_number,
        files: vec![
            render_pr_body(binding),
            render_review_md(doc),
            render_findings_json(review_id, ps_number, findings),
            render_commits_md(commits, truncated_from),
        ],
    }
}

/// Resolve a comment ANCHORED TO A PSEUDO-FILE against that file's current
/// bytes, through the SAME ladder `GET /api/reviews/{id}/comments` uses —
/// `review_comments::resolve_for_ps_with_content`, given the pseudo bytes
/// instead of a blob read. Not a second matcher: the exact→fuzzy→
/// snippet-guard rungs, the orphan rule and the confidence label are all
/// the shared resolver's, and a line that cannot be proved is an honest
/// orphan here exactly as it is on a tracked file.
pub fn resolve_on_pseudo(
    row: &store::AnnotationRow,
    target_ps: &store::ReviewPatchsetRow,
    file: &PseudoFile,
) -> crate::review_comments::ResolvedForPs {
    crate::review_comments::resolve_for_ps_with_content(
        row,
        target_ps,
        &file.blob_sha,
        Some(file.content()),
    )
}

// --- routes ----------------------------------------------------------------

use crate::routes::ApiError;
use crate::state::SharedState;
use crate::store::StoreBlocking;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct PseudoParams {
    #[serde(default)]
    pub ps: Option<String>,
}

/// Build the set for one review at one patchset, doing every read this
/// needs. Shared by both routes and by the reading order, so a pseudo
/// file's `blob_sha` is the same number wherever it is quoted.
pub async fn load_set(
    state: &SharedState,
    id: i64,
    ps_param: Option<&str>,
) -> Result<PseudoSet, ApiError> {
    let (_review, repo, _repo_id) = crate::reviews::require_review(state, id).await?;
    let repo_root = repo.path.clone();
    let owned = ps_param.map(str::to_string);
    let (ps, binding, doc, findings) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let ps = crate::reviews::resolve_ps(store, id, owned.as_deref())?;
            let binding = store.get_review_pr_binding(id)?.unwrap_or_default();
            let doc = store.latest_review_doc(id, ps.ps_number)?;
            let findings = store.list_review_findings(id, None, true)?;
            Ok((ps, binding, doc, findings))
        })
        .await?;

    let base = ps.base_sha.clone();
    let tip = ps.tip_sha.clone();
    let (commits, truncated_from) =
        tokio::task::spawn_blocking(move || commit_list(&repo_root, &base, &tip))
            .await
            .map_err(|e| {
                ApiError::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            })?;

    Ok(build_set(
        id,
        ps.ps_number,
        &binding,
        doc.as_ref(),
        &findings,
        &commits,
        truncated_from,
    ))
}

/// `git log --format=<sha>\x1f<subject>\x1f<author>\x1f<at>\x1f<trailers>`
/// over `base..tip`. Trailers come from `%(trailers:only,unfold)`, git's own
/// parser — never a hand-rolled scan, so `commits.md` shows exactly what the
/// session↔commit join reads.
pub fn commit_list(
    repo_root: &std::path::Path,
    base_sha: &str,
    tip_sha: &str,
) -> (Vec<PseudoCommit>, Option<usize>) {
    let range = format!("{base_sha}..{tip_sha}");
    // `%x1e` (record separator) between commits, `%x1f` (unit) between
    // fields — a trailer block contains newlines, so a line-oriented split
    // would not be unambiguous the way `history::LOG_SUMMARY_FMT`'s is.
    let fmt = "--format=%x1e%H%x1f%s%x1f%an <%ae>%x1f%at%x1f%(trailers:only,unfold)";
    let Ok(out) = crate::history::run_git_raw(repo_root, &["log", fmt, &range]) else {
        return (Vec::new(), None);
    };
    let text = String::from_utf8_lossy(&out);
    let all: Vec<PseudoCommit> = text
        .split('\u{1e}')
        .filter(|r| !r.trim().is_empty())
        .filter_map(parse_commit_record)
        .collect();
    let total = all.len();
    if total > MAX_COMMITS {
        (all.into_iter().take(MAX_COMMITS).collect(), Some(total))
    } else {
        (all, None)
    }
}

fn parse_commit_record(record: &str) -> Option<PseudoCommit> {
    let mut f = record.splitn(5, '\u{1f}');
    let sha = f.next()?.trim().to_string();
    if sha.is_empty() {
        return None;
    }
    let subject = f.next()?.to_string();
    let author = f.next()?.to_string();
    let author_time: i64 = f.next()?.trim().parse().ok()?;
    let trailers = f
        .next()
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    Some(PseudoCommit {
        sha,
        subject,
        author,
        author_time,
        trailers,
    })
}

/// `GET /api/reviews/{id}/pseudo?ps=` — bearer. The LIST: every name, its
/// hash, whether it has content and why not. No bytes.
pub async fn list_pseudo(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Query(params): Query<PseudoParams>,
) -> Result<impl IntoResponse, ApiError> {
    let set = load_set(&state, id, params.ps.as_deref()).await?;
    let files: Vec<PseudoFile> = set.files.iter().map(PseudoFile::without_content).collect();
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(PseudoSet { files, ..set }),
    ))
}

/// `GET /api/reviews/{id}/pseudo/{name}?ps=` — bearer. ONE file, with its
/// bytes. An unknown name 404s NAMING the closed set, so a typo is
/// distinguishable from an empty file.
pub async fn get_pseudo(
    State(state): State<SharedState>,
    AxumPath((id, name)): AxumPath<(i64, String)>,
    Query(params): Query<PseudoParams>,
) -> Result<impl IntoResponse, ApiError> {
    if !NAMES.contains(&name.as_str()) {
        return Err(ApiError::not_found(format!(
            "no such review pseudo-file: {name:?} — the set is closed at {NAMES:?}"
        )));
    }
    let set = load_set(&state, id, params.ps.as_deref()).await?;
    let file = set
        .get(&name)
        .cloned()
        .ok_or_else(|| ApiError::not_found(format!("no such review pseudo-file: {name:?}")))?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": SCHEMA,
            "review_id": set.review_id,
            "ps_number": set.ps_number,
            "file": file,
        })),
    ))
}

use crate::entities::RouteContract;

fn pseudo_accept_without(omit: &str) -> bool {
    let mut map = serde_json::Map::new();
    for (k, v) in [("ps", "latest")] {
        if k != omit {
            map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
        }
    }
    serde_json::from_value::<PseudoParams>(serde_json::Value::Object(map)).is_ok()
}

pub const PSEUDO_LIST_ROUTE: RouteContract = RouteContract {
    path: "/api/reviews/{id}/pseudo",
    handler: "review_pseudo::list_pseudo",
    required_params: &[],
    params_accept_without: pseudo_accept_without,
};

pub const PSEUDO_FILE_ROUTE: RouteContract = RouteContract {
    path: "/api/reviews/{id}/pseudo/{name}",
    handler: "review_pseudo::get_pseudo",
    required_params: &[],
    params_accept_without: pseudo_accept_without,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn binding_with_body(body: Option<&str>) -> store::ReviewPrBinding {
        store::ReviewPrBinding {
            pr_number: Some(7),
            pr_repo_slug: Some("acme/acme-app".into()),
            pr_head_sha: Some("aaa".into()),
            pr_meta_json: Some(serde_json::json!({ "title": "t", "body": body }).to_string()),
            pr_meta_fetched_at: Some(1000),
            artifact_hint_kb: None,
            artifact_hint_id: None,
        }
    }

    #[test]
    fn names_are_closed_and_round_trip_through_the_prefix() {
        assert_eq!(NAMES.len(), 4);
        for n in NAMES {
            assert_eq!(name_for_path(&path_for(n)), Some(*n));
        }
        assert_eq!(name_for_path("app/models/order.rb"), None);
        assert_eq!(name_for_path("~review/nope.md"), None);
        assert!(is_pseudo_path("~review/pr-body.md"));
        assert!(!is_pseudo_path("app/models/order.rb"));
    }

    #[test]
    fn the_pr_body_is_verbatim_and_its_hash_is_a_real_git_blob_hash() {
        let f = render_pr_body(&binding_with_body(Some("line one\nline two\n")));
        assert!(f.present);
        assert_eq!(f.content(), "line one\nline two\n");
        assert_eq!(f.blob_sha, git_blob_hash(b"line one\nline two\n"));
        assert_eq!(f.lines, 2);
    }

    #[test]
    fn an_absent_pr_binding_still_has_the_file_empty_with_a_reason() {
        let f = render_pr_body(&store::ReviewPrBinding::default());
        assert!(!f.present);
        assert_eq!(f.content(), "");
        assert!(f.reason.as_deref().unwrap().contains("not bound"));
        // The empty file still has an honest, stable hash — the git hash of
        // zero bytes, which is a real object id.
        assert_eq!(f.blob_sha, git_blob_hash(b""));
    }

    #[test]
    fn an_empty_pr_description_is_distinguished_from_an_absent_binding() {
        let f = render_pr_body(&binding_with_body(None));
        assert!(!f.present);
        assert!(f.reason.as_deref().unwrap().contains("empty description"));
    }

    #[test]
    fn commits_md_states_the_true_total_when_it_stops() {
        let commits: Vec<PseudoCommit> = (0..3)
            .map(|i| PseudoCommit {
                sha: format!("{i}bcdef0123456789"),
                subject: format!("commit {i}"),
                author: "A <a@example.test>".into(),
                author_time: 100 + i,
                trailers: if i == 0 {
                    vec!["Kb-Session: abc-123".into()]
                } else {
                    vec![]
                },
            })
            .collect();
        let f = render_commits_md(&commits, Some(900));
        assert!(f.present);
        assert!(f.content().contains("Kb-Session: abc-123"));
        assert!(f.content().contains("900 commits"));
        assert!(f.content().contains("(none)"));
    }

    #[test]
    fn the_set_is_always_four_files_in_reading_order() {
        let set = build_set(
            1,
            2,
            &store::ReviewPrBinding::default(),
            None,
            &[],
            &[],
            None,
        );
        let names: Vec<&str> = set.files.iter().map(|f| f.name).collect();
        assert_eq!(names, NAMES.to_vec());
        assert_eq!(
            set.get("~review/review.md").map(|f| f.name),
            Some(REVIEW_MD)
        );
        assert_eq!(
            set.get("findings.json").map(|f| f.name),
            Some(FINDINGS_JSON)
        );
    }

    fn pseudo_ps() -> store::ReviewPatchsetRow {
        store::ReviewPatchsetRow {
            id: 1,
            review_id: 1,
            ps_number: 1,
            tip_sha: "tip".into(),
            base_sha: "base".into(),
            captured_at: 0,
        }
    }

    /// A line-anchored comment, built through the SAME helper
    /// `routes::create_annotation` uses — never a hand-rolled `Anchor`, or
    /// this test would prove something about a shape nothing writes.
    fn anchored(line: u32, snippet: &str) -> store::AnnotationRow {
        let anchor = crate::annotations::anchor_for_line(line, snippet);
        store::AnnotationRow {
            id: "ann-1".into(),
            repo_id: 1,
            path: path_for(PR_BODY),
            anchor: Some(serde_json::to_string(&anchor).unwrap()),
            anchor_kind: "line".into(),
            anchor2: None,
            parent_id: None,
            intent: "note".into(),
            body: "why is this here?".into(),
            author: "you".into(),
            created_at: 1,
            updated_at: 1,
            resolved: false,
            review_id: Some(1),
            ps_number: Some(1),
            side: Some("new".into()),
            set_id: None,
        }
    }

    /// The pseudo-file half of the SAME ladder: a comment whose snippet is
    /// still on its line resolves; one whose snippet moved is an honest
    /// ORPHAN, never a guessed line.
    #[test]
    fn a_comment_on_a_pseudo_file_resolves_or_orphans_through_the_shared_ladder() {
        let file = render_pr_body(&binding_with_body(Some(
            "why this change
second line
third line\n",
        )));
        let ps = pseudo_ps();

        let hit = resolve_on_pseudo(&anchored(2, "second line"), &ps, &file);
        assert!(!hit.orphaned, "{hit:?}");
        assert_eq!(hit.line, Some(2));

        let moved = resolve_on_pseudo(&anchored(2, "a line this file does not have"), &ps, &file);
        assert!(
            moved.orphaned,
            "an unprovable match must be an orphan, never a guessed line: {moved:?}"
        );
        assert!(moved.line.is_none());
    }

    #[test]
    fn the_findings_sidecar_is_the_shape_compose_accepts() {
        let f = render_findings_json(1, 1, &[]);
        let v: serde_json::Value = serde_json::from_str(f.content()).unwrap();
        assert_eq!(v["schema"], "kbc-findings/2");
        assert!(v["findings"].is_array());
        // The array is exactly `Vec<DocFinding>` — the sidecar `compose
        // --findings` reads.
        let parsed: Vec<DocFinding> =
            serde_json::from_value(v["findings"].clone()).expect("round-trips as DocFinding");
        assert!(parsed.is_empty());
    }
}
