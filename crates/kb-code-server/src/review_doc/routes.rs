//! `kbc-review/1` ROUTES — the document read, the lint, the render, and the
//! shared half of `compose` (V73-K1).
//!
//! | route | posture | what |
//! |---|---|---|
//! | `GET /api/reviews/{id}/doc` | bearer | the stored document, with `?resolve=true` for live cards |
//! | `GET /api/reviews/{id}/doc/lint` | bearer | lint the STORED document |
//! | `GET /api/reviews/{id}/doc/render` | bearer | render through a REGISTERED template |
//! | `POST /api/reviews/{id}/doc/render` | loopback-only | render through the operator's own template bytes |
//! | `POST /api/reviews/{id}/compose` | loopback-only | the ONE authoring transaction (`review_findings`) |
//!
//! The two mutating-shaped routes here are `POST …/doc/render` (which writes
//! nothing — it is a POST only because a template is a whole file, not a
//! query parameter) and `compose`. Both sit on the loopback-only
//! `transcripts_api` family, which is D22's local-canonical ruling applied
//! unchanged: no review-authoring surface graduates off loopback, and root
//! invariant #4 is not amended.
//!
//! There is ONE renderer and ONE lint: `POST …/doc/render` and
//! `GET …/doc/render` call the same [`crate::review_doc::render::render`],
//! and `compose --dry-run` is [`lint_document`] plus the same card
//! resolution the read route performs — so a document that lints clean and
//! then fails to compose would be one bug, not two surfaces disagreeing.

use crate::review_doc::cards::{self, Card, CardCtx};
use crate::review_doc::lint::{self, LintOut};
use crate::review_doc::render::{self, RenderCtx};
use crate::review_doc::{self, DocFinding, ReviewDoc, Tier};
use crate::reviews::{files_changed, require_review, resolve_ps};
use crate::routes::ApiError;
use crate::state::SharedState;
use crate::store::{ReviewDocRow, ReviewFindingRow, ReviewPatchsetRow, Store, StoreBlocking};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;

pub const RENDER_SCHEMA: &str = "review-render/1";

/// The built-in template's registered name — always available, never
/// configurable away.
pub const DEFAULT_TEMPLATE_NAME: &str = "default";

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// --- params ----------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
pub struct DocParams {
    /// Patchset number, or `latest` (default) — the same grammar
    /// `ReviewCommentsParams::ps` / `reviews::resolve_ps` use.
    #[serde(default)]
    pub ps: Option<String>,
    /// Resolve every ref into a live card (`?resolve=true`). Off by
    /// default: card resolution reads git blobs and the symbol index, and a
    /// caller that only wants the prose should not pay for it. Spelled
    /// `true`/`false` — the same bool spelling `?all=` and
    /// `?include_superseded=` use on this route family.
    #[serde(default)]
    pub resolve: bool,
}

#[derive(Debug, Deserialize, Default)]
pub struct RenderParams {
    #[serde(default)]
    pub ps: Option<String>,
    /// A REGISTERED template name — `default` (built-in) or a key of
    /// `[review] doc_templates`. Never a path: this route cannot be talked
    /// into reading an arbitrary file.
    #[serde(default)]
    pub template: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RenderBody {
    /// The operator's own template, verbatim. Loopback-only.
    pub template: String,
    #[serde(default)]
    pub ps: Option<String>,
}

// --- the assembled read ----------------------------------------------------

/// A reading order that is either the author's or this daemon's, and always
/// says which.
#[derive(Debug, Clone, Serialize)]
pub struct ReadingOrderOut {
    /// `authored` | `derived`.
    pub source: &'static str,
    pub caption: String,
    pub chapters: Vec<review_doc::Chapter>,
}

/// V73-K5 — a `ci:` block that is either the author's or DERIVED from the
/// review's own `pr_meta_json.checks` snapshot, and always says which
/// (`ReadingOrderOut`'s exact pattern, applied to a second block).
#[derive(Debug, Clone, Serialize)]
pub struct CiOut {
    /// `authored` | `derived`.
    pub source: &'static str,
    pub checks: Vec<review_doc::CiCheck>,
}

/// `doc_ci` (this document's OWN authored `ci:` block) wins when non-empty;
/// otherwise every entry is DERIVED from `pr_meta_json.checks` (PRR-R2's
/// `CheckRunOut` snapshot, embedded verbatim at the review's last PR-meta
/// fetch — never a fresh GitHub call). An empty result either way is an
/// honest "no known checks", not an error.
pub fn derive_ci(
    doc_ci: &[review_doc::CiCheck],
    pr_meta_json: Option<&str>,
    pr_meta_fetched_at: Option<i64>,
) -> CiOut {
    if !doc_ci.is_empty() {
        return CiOut {
            source: "authored",
            checks: doc_ci.to_vec(),
        };
    }
    let raw_checks: Vec<crate::github::CheckRunOut> = pr_meta_json
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.get("checks").cloned())
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let checks = raw_checks
        .into_iter()
        .map(|c| review_doc::CiCheck {
            name: c.name,
            status: ci_status_from_check(&c.status, c.note.as_deref()).to_string(),
            // `CheckRunOut` carries no URL (the daemon has never fetched
            // one) — absent is the honest value, not a guess.
            url: None,
            observed_at: pr_meta_fetched_at,
        })
        .collect();
    CiOut {
        source: "derived",
        checks,
    }
}

/// `CheckRunOut`'s own `pass|fail|warn|pending` (the generic GitHub-Checks
/// normalization) is a COARSER vocabulary than `review_doc::CI_STATUSES`;
/// this refines it using the raw `note` field when GitHub's own
/// `conclusion`/`status` string is still available, falling back to the
/// coarse mapping only when `note` is absent. `neutral`/`stale` fold to
/// `skipped` — the closest honest bucket in a 4-value set that has no
/// "neutral" of its own.
fn ci_status_from_check(status: &str, note: Option<&str>) -> &'static str {
    match note {
        Some("success") => "success",
        Some("failure") | Some("timed_out") | Some("action_required") | Some("cancelled") => {
            "failure"
        }
        Some("skipped") | Some("neutral") | Some("stale") => "skipped",
        Some("queued") | Some("in_progress") => "pending",
        _ => match status {
            "pass" => "success",
            "fail" => "failure",
            "pending" => "pending",
            _ => "skipped",
        },
    }
}

/// A finding as the DOCUMENT read surfaces it: the identity and the two v2
/// axes, no resolution and no threads. `GET /api/reviews/{id}/findings` is
/// still the full view (carry-forward resolution, thread counts, publish
/// state) — this is deliberately the compact half, so the document read is
/// one round trip and never a second, slightly-different findings API.
#[derive(Debug, Clone, Serialize)]
pub struct FindingBrief {
    pub slug: String,
    pub act: String,
    pub severity: String,
    pub blocking: bool,
    pub category: String,
    pub title: String,
    pub location_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    pub origin: String,
    pub superseded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disposition: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cites: Option<Value>,
}

impl From<&ReviewFindingRow> for FindingBrief {
    fn from(r: &ReviewFindingRow) -> Self {
        FindingBrief {
            slug: r.slug.clone(),
            act: r.act.clone(),
            severity: r.severity.clone(),
            blocking: r.blocking,
            category: r.category.clone(),
            title: r.title.clone(),
            location_path: r.location_path.clone(),
            fingerprint: r.fingerprint.clone(),
            origin: r.origin.clone(),
            superseded: r.superseded,
            superseded_by: r.superseded_by.clone(),
            disposition: r.disposition.clone(),
            cites: r
                .cites_json
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok()),
        }
    }
}

/// Everything `GET …/doc` returns, and the same object `compose` echoes.
#[derive(Debug, Clone, Serialize)]
pub struct DocOut {
    pub schema: &'static str,
    pub review_id: i64,
    pub repo: String,
    pub ps_number: i64,
    pub revision: i64,
    /// How many revisions exist for this review across every patchset —
    /// the append-only chain's true length.
    pub revisions: usize,
    pub tier: String,
    pub created_at: i64,
    /// The lossless record: front matter + body, byte-for-byte as composed.
    pub doc_md: String,
    pub summary_md: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk: Option<review_doc::Risk>,
    pub reading_order: ReadingOrderOut,
    /// V73-K5 — the `ci:` block, authored-or-derived (same dual-source
    /// pattern as `reading_order`).
    pub ci: CiOut,
    pub blocks: std::collections::BTreeMap<String, String>,
    pub flows: Vec<review_doc::Flow>,
    pub questions: Vec<review_doc::Question>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<review_doc::Author>,
    pub findings: Vec<FindingBrief>,
    /// Every optional block this document does NOT carry. Always present,
    /// even when empty — an absence is stated, never discovered.
    pub omitted: Vec<String>,
    /// `null` unless `?resolve=true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cards: Option<Vec<Card>>,
    pub cards_resolved: bool,
}

/// The repository-side inputs a card resolution and a derived reading order
/// both need: one git diff, shared.
pub struct RepoInputs {
    changed: Vec<crate::numstat::FileChange>,
    changed_paths: HashSet<String>,
}

pub async fn repo_inputs(repo_root: &Path, ps: &ReviewPatchsetRow) -> Result<RepoInputs, ApiError> {
    let root = repo_root.to_path_buf();
    let base = ps.base_sha.clone();
    let tip = ps.tip_sha.clone();
    let changed = tokio::task::spawn_blocking(move || files_changed(&root, &base, &tip))
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    let changed_paths = changed.iter().map(|f| f.path.clone()).collect();
    Ok(RepoInputs {
        changed,
        changed_paths,
    })
}

/// The stored document + everything derived from it. Shared by the three
/// read routes and by `compose`'s echo, so all four agree by construction.
pub async fn load_doc_out(
    state: &SharedState,
    id: i64,
    ps_param: Option<&str>,
    resolve: bool,
) -> Result<(DocOut, ReviewDoc, Vec<Card>), ApiError> {
    let (review, repo, repo_id) = require_review(state, id).await?;
    let ps_owned = ps_param.map(str::to_string);
    let (ps, row, revisions) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let ps = resolve_ps(store, id, ps_owned.as_deref())?;
            let row = store.latest_review_doc(id, ps.ps_number)?;
            let revisions = store.list_review_docs(id)?.len();
            Ok((ps, row, revisions))
        })
        .await?;
    let Some(row) = row else {
        return Err(ApiError::not_found(format!(
            "review {id} has no kbc-review/1 document at patchset {} — compose one with \
             `kb-code review compose {id} --doc <file.md>`",
            ps.ps_number
        )));
    };
    let doc = review_doc::parse(&row.doc_md).map_err(|e| {
        // A stored document that no longer parses is a data-integrity gap,
        // not a client error: it was validated at compose time. Report it
        // honestly with the parse reason rather than 500ing blind.
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "stored review document (revision {}) no longer parses: {e}",
                row.revision
            ),
        )
    })?;

    let inputs = repo_inputs(&repo.path, &ps).await?;
    let out = build_doc_out(
        state,
        &review.repo,
        id,
        repo_id,
        &repo.path,
        &ps,
        &row,
        revisions,
        &doc,
        &inputs,
        resolve,
    )
    .await?;
    let cards = out.cards.clone().unwrap_or_default();
    Ok((out, doc, cards))
}

#[allow(clippy::too_many_arguments)]
async fn build_doc_out(
    state: &SharedState,
    repo_name: &str,
    id: i64,
    repo_id: i64,
    repo_root: &Path,
    ps: &ReviewPatchsetRow,
    row: &ReviewDocRow,
    revisions: usize,
    doc: &ReviewDoc,
    inputs: &RepoInputs,
    resolve: bool,
) -> Result<DocOut, ApiError> {
    let refs = review_doc::all_refs(doc);
    let doc_c = doc.clone();
    let ps_c = ps.clone();
    let root_c = repo_root.to_path_buf();
    let changed_paths = inputs.changed_paths.clone();
    let changed_rows: Vec<(String, String)> = {
        let mut by_path: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for f in &inputs.changed {
            by_path.insert(
                f.path.clone(),
                crate::review_map::status_wire(&f.status).to_string(),
            );
        }
        by_path.into_iter().collect()
    };
    let authored_order = doc.reading_order.clone();

    let doc_ci = doc.ci.clone();
    let question_count = doc.questions.len();

    let (findings, reading_order, ci, cards) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let findings: Vec<FindingBrief> = store
                .list_review_findings(id, None, true)?
                .iter()
                .map(FindingBrief::from)
                .collect();
            let reading_order = if authored_order.is_empty() {
                derive_reading_order(store, repo_id, &changed_rows)
            } else {
                ReadingOrderOut {
                    source: "authored",
                    caption: "the reading order this document declares".to_string(),
                    chapters: authored_order,
                }
            };
            // Fetched unconditionally (one cheap row, not gated on
            // `resolve`): `ci` is a document-read field like
            // `reading_order`, not a card-resolution artifact.
            let pr_binding = store.get_review_pr_binding(id)?.unwrap_or_default();
            let ci = derive_ci(
                &doc_ci,
                pr_binding.pr_meta_json.as_deref(),
                pr_binding.pr_meta_fetched_at,
            );
            let cards = if resolve {
                let known_ps: HashSet<i64> = store
                    .list_patchsets(id)?
                    .into_iter()
                    .map(|p| p.ps_number)
                    .collect();
                // V73-K3 — the review's pseudo-files, so a `code:
                // ~review/<name>` ref resolves through the same ladder as a
                // tracked path. Built here (not lazily inside the resolver)
                // because they are rendered from rows this closure already
                // has plus ONE `git log`, and because every ref in one pass
                // must see the SAME bytes.
                let pseudo_doc = store.latest_review_doc(id, ps_c.ps_number)?;
                let pseudo_findings = store.list_review_findings(id, None, true)?;
                let (pseudo_commits, pseudo_truncated) =
                    crate::review_pseudo::commit_list(&root_c, &ps_c.base_sha, &ps_c.tip_sha);
                let pseudo = crate::review_pseudo::build_set(
                    id,
                    ps_c.ps_number,
                    &pr_binding,
                    pseudo_doc.as_ref(),
                    &pseudo_findings,
                    &pseudo_commits,
                    pseudo_truncated,
                );
                let ctx = CardCtx {
                    repo_root: &root_c,
                    repo_id,
                    review_id: id,
                    target_ps: &ps_c,
                    known_ps: &known_ps,
                    changed_paths: &changed_paths,
                    pseudo: Some(&pseudo),
                    ci_checks: &ci.checks,
                    question_count,
                };
                Some(cards::resolve_cards(store, &ctx, &refs))
            } else {
                None
            };
            Ok((findings, reading_order, ci, cards))
        })
        .await?;

    Ok(DocOut {
        schema: review_doc::SCHEMA,
        review_id: id,
        repo: repo_name.to_string(),
        ps_number: ps.ps_number,
        revision: row.revision,
        revisions,
        tier: row.tier.clone(),
        created_at: row.created_at,
        doc_md: row.doc_md.clone(),
        summary_md: doc_c.summary_md,
        risk: doc_c.risk,
        reading_order,
        ci,
        blocks: doc_c.blocks,
        flows: doc_c.flows,
        questions: doc_c.questions,
        author: doc_c.author,
        findings,
        omitted: review_doc::omitted_blocks(doc),
        cards_resolved: cards.is_some(),
        cards,
    })
}

/// The DERIVED reading order — one chapter, the existing review map's own
/// deterministic tour, captioned as derived.
///
/// D9's degrade rule, literally: "no reading_order ⇒ derived, captioned
/// derived". It is not a second ordering algorithm — it is
/// `review_map::compute_reading_order`, the same function `GET
/// /api/reviews/{id}/reading-order` serves, so the document and the cockpit
/// can never show two different tours of one change set.
fn derive_reading_order(
    store: &Store,
    repo_id: i64,
    changed: &[(String, String)],
) -> ReadingOrderOut {
    let paths: Vec<String> = changed.iter().map(|(p, _)| p.clone()).collect();
    let edges = store
        .import_edges_among_paths(repo_id, &paths)
        .unwrap_or_default();
    let stops = crate::review_map::compute_reading_order(changed, &edges);
    ReadingOrderOut {
        source: "derived",
        caption: "this document declares no reading order — the tour below is DERIVED from the \
                  review map (dependencies first, tests last), not authored"
            .to_string(),
        chapters: vec![
            // V73-K3 — CHAPTER ZERO (design D9: "PR description and all
            // GitHub comments absorbed into one timeline and chapter
            // zero"). The review's own pseudo-files come before the diff,
            // because a reviewer who reads the code before the description
            // is reading it without the question it was meant to answer.
            // Listed unconditionally, in `review_pseudo::NAMES` order — a
            // pseudo-file always EXISTS, and one with no content says so
            // when its card resolves, which is a better degrade than a
            // chapter whose membership silently varies per review.
            review_pseudo_chapter(),
            review_doc::Chapter {
                chapter: "Derived from the review map".to_string(),
                stops: stops
                    .into_iter()
                    .map(|s| review_doc::Stop {
                        r#ref: format!("code:{}", s.path),
                        why: Some(if s.cycle {
                            format!("{} (in an import cycle)", s.reason)
                        } else {
                            s.reason
                        }),
                    })
                    .collect(),
            },
        ],
    }
}

/// Chapter zero: the four `kbc-pseudo/1` files, as ordinary `code:` stops.
/// They address like paths because they ARE addressable like paths — see
/// `crate::review_pseudo`'s module doc.
fn review_pseudo_chapter() -> review_doc::Chapter {
    review_doc::Chapter {
        chapter: "The review itself".to_string(),
        stops: crate::review_pseudo::NAMES
            .iter()
            .map(|name| review_doc::Stop {
                r#ref: format!("code:{}", crate::review_pseudo::path_for(name)),
                why: Some(
                    match *name {
                        crate::review_pseudo::PR_BODY => "what the author says this change is for",
                        crate::review_pseudo::REVIEW_MD => "the review document itself",
                        crate::review_pseudo::FINDINGS_JSON => "every finding, as the sidecar",
                        _ => "the commits, with their trailers",
                    }
                    .to_string(),
                ),
            })
            .collect(),
    }
}

// --- GET /api/reviews/{id}/doc ---------------------------------------------

/// `GET /api/reviews/{id}/doc?ps=latest|N&resolve=true` — the stored
/// `kbc-review/1` document. Bearer.
pub async fn get_review_doc(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Query(params): Query<DocParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (out, _, _) = load_doc_out(&state, id, params.ps.as_deref(), params.resolve).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

// --- GET /api/reviews/{id}/doc/lint ----------------------------------------

/// `GET /api/reviews/{id}/doc/lint?ps=` — lint the STORED document. Pure:
/// this route writes nothing and resolves refs the same way `?resolve=true`
/// does. Bearer.
///
/// Linting a document that is NOT yet stored is `POST …/compose` with
/// `dry_run: true` — the same lint, the same resolution, no write. That is
/// deliberately not a second endpoint: a candidate document is a whole file
/// and does not fit a query string, and `--dry-run` is where an author
/// already looks.
pub async fn lint_review_doc(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Query(params): Query<DocParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (_review, repo, repo_id) = require_review(&state, id).await?;
    let ps_owned = params.ps.clone();
    let (ps, row) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let ps = resolve_ps(store, id, ps_owned.as_deref())?;
            let row = store.latest_review_doc(id, ps.ps_number)?;
            Ok((ps, row))
        })
        .await?;
    let Some(row) = row else {
        return Err(ApiError::not_found(format!(
            "review {id} has no kbc-review/1 document at patchset {}",
            ps.ps_number
        )));
    };
    let inputs = repo_inputs(&repo.path, &ps).await?;
    let tier = Tier::parse(&row.tier).unwrap_or(Tier::Minimal);
    let out = lint_document(
        &state,
        id,
        repo_id,
        &repo.path,
        &ps,
        &inputs,
        &row.doc_md,
        tier,
        None,
        Some(row.created_at),
    )
    .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// Lint one candidate (or stored) document end to end — the cards it
/// resolved on the way are dropped. [`lint_and_resolve`] is the same pass
/// keeping them, for a caller (`compose --dry-run`, `render`) that wants to
/// SHOW what the refs turned into.
#[allow(clippy::too_many_arguments)]
pub async fn lint_document(
    state: &SharedState,
    id: i64,
    repo_id: i64,
    repo_root: &Path,
    ps: &ReviewPatchsetRow,
    inputs: &RepoInputs,
    doc_md: &str,
    tier: Tier,
    findings_override: Option<&[DocFinding]>,
    composed_at: Option<i64>,
) -> Result<LintOut, ApiError> {
    lint_and_resolve(
        state,
        id,
        repo_id,
        repo_root,
        ps,
        inputs,
        doc_md,
        tier,
        findings_override,
        composed_at,
    )
    .await
    .map(|(lint, _)| lint)
}

/// Lint one candidate (or stored) document end to end: parse → size caps →
/// structural rows → resolved-card rows, returning both the lint and the
/// cards that produced its ref rows.
///
/// `findings_override` is the SIDECAR when a `compose` supplies one;
/// `None` means "use the document's own `findings:` front matter".
#[allow(clippy::too_many_arguments)]
pub async fn lint_and_resolve(
    state: &SharedState,
    id: i64,
    repo_id: i64,
    repo_root: &Path,
    ps: &ReviewPatchsetRow,
    inputs: &RepoInputs,
    doc_md: &str,
    tier: Tier,
    findings_override: Option<&[DocFinding]>,
    composed_at: Option<i64>,
) -> Result<(LintOut, Vec<Card>), ApiError> {
    let doc = match review_doc::parse(doc_md) {
        Ok(d) => d,
        Err(e) => return Ok((lint::parse_failure(&e), Vec::new())),
    };
    let findings: Option<Vec<DocFinding>> = match findings_override {
        Some(f) => Some(f.to_vec()),
        None => doc.findings.clone(),
    };
    let refs = review_doc::all_refs(&doc);

    let mut rows = lint::size_rows(
        doc_md,
        refs.len(),
        findings.as_ref().map(Vec::len).unwrap_or(0),
    );
    rows.extend(lint::structural_rows(
        &doc,
        doc_md,
        tier,
        findings.as_deref(),
        composed_at,
    ));

    let ps_c = ps.clone();
    let root_c = repo_root.to_path_buf();
    let changed_paths = inputs.changed_paths.clone();
    let doc_ci = doc.ci.clone();
    let question_count = doc.questions.len();
    let (card_rows, resolved) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let known_ps: HashSet<i64> = store
                .list_patchsets(id)?
                .into_iter()
                .map(|p| p.ps_number)
                .collect();
            // V73-K3 — the review's pseudo-files, so a `code:
            // ~review/<name>` ref resolves through the same ladder as a
            // tracked path. Built here (not lazily inside the resolver)
            // because they are rendered from rows this closure already
            // has plus ONE `git log`, and because every ref in one pass
            // must see the SAME bytes.
            let pseudo_binding = store.get_review_pr_binding(id)?.unwrap_or_default();
            let pseudo_doc = store.latest_review_doc(id, ps_c.ps_number)?;
            let pseudo_findings = store.list_review_findings(id, None, true)?;
            let (pseudo_commits, pseudo_truncated) =
                crate::review_pseudo::commit_list(&root_c, &ps_c.base_sha, &ps_c.tip_sha);
            let pseudo = crate::review_pseudo::build_set(
                id,
                ps_c.ps_number,
                &pseudo_binding,
                pseudo_doc.as_ref(),
                &pseudo_findings,
                &pseudo_commits,
                pseudo_truncated,
            );
            let ci = derive_ci(
                &doc_ci,
                pseudo_binding.pr_meta_json.as_deref(),
                pseudo_binding.pr_meta_fetched_at,
            );
            let ctx = CardCtx {
                repo_root: &root_c,
                repo_id,
                review_id: id,
                target_ps: &ps_c,
                known_ps: &known_ps,
                changed_paths: &changed_paths,
                pseudo: Some(&pseudo),
                ci_checks: &ci.checks,
                question_count,
            };
            let cards = cards::resolve_cards(store, &ctx, &refs);
            let rows = lint::card_rows(store, repo_id, &cards);
            Ok((rows, cards))
        })
        .await?;
    rows.extend(card_rows);
    Ok((LintOut::from_rows(rows), resolved))
}

// --- render ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct RenderOut {
    pub schema: &'static str,
    pub review_id: i64,
    pub ps_number: i64,
    pub revision: i64,
    pub template: String,
    pub html: String,
    /// `{{…}}` tokens the template used that this renderer does not fill.
    /// Left verbatim in `html`; reported so a typo is never silent.
    pub unknown_placeholders: Vec<String>,
    pub cards: usize,
    pub orphans: usize,
}

/// `GET /api/reviews/{id}/doc/render?ps=&template=<registered name>` —
/// render through a REGISTERED template. Bearer.
///
/// `template` is a NAME, never a path: `default` (the built-in) plus every
/// key of `[review] doc_templates`. An unknown name is a 400 that lists the
/// registered names — this route is structurally unable to be talked into
/// reading an arbitrary file.
pub async fn render_review_doc(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Query(params): Query<RenderParams>,
) -> Result<impl IntoResponse, ApiError> {
    let name = params
        .template
        .clone()
        .unwrap_or_else(|| DEFAULT_TEMPLATE_NAME.to_string());
    let template = resolve_registered_template(&state, &name)?;
    let out = render_with(&state, id, params.ps.as_deref(), &name, &template).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// `POST /api/reviews/{id}/doc/render` — render through the template bytes
/// the caller supplies. LOOPBACK-ONLY, and writes nothing: it is a POST
/// only because a template is a whole HTML file. This is what `kb-code
/// review render --template <file.html>` calls, which is what keeps
/// rendering in ONE place instead of a second implementation inside the CLI.
pub async fn render_review_doc_with_template(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Json(body): Json<RenderBody>,
) -> Result<impl IntoResponse, ApiError> {
    if body.template.len() > MAX_TEMPLATE_BYTES {
        return Err(ApiError::bad_request(format!(
            "template is {} bytes; the cap is {MAX_TEMPLATE_BYTES}",
            body.template.len()
        )));
    }
    let out = render_with(
        &state,
        id,
        body.ps.as_deref(),
        "(request body)",
        &body.template,
    )
    .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// Cap on an operator template. Generous — this is a whole HTML page — and
/// a refusal names the numbers rather than truncating.
pub const MAX_TEMPLATE_BYTES: usize = 512 * 1024;

fn resolve_registered_template(state: &SharedState, name: &str) -> Result<String, ApiError> {
    if name == DEFAULT_TEMPLATE_NAME {
        return Ok(render::DEFAULT_TEMPLATE.to_string());
    }
    let Some(path) = state.review.doc_templates.get(name) else {
        let mut names: Vec<&str> = state
            .review
            .doc_templates
            .keys()
            .map(String::as_str)
            .collect();
        names.push(DEFAULT_TEMPLATE_NAME);
        names.sort_unstable();
        return Err(ApiError::bad_request(format!(
            "unknown template {name:?} — registered names are {} (add one under \
             `[review] doc_templates` in kb-code.toml)",
            names.join(", ")
        )));
    };
    std::fs::read_to_string(path).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "registered template {name:?} at {} is not readable: {e}",
                path.display()
            ),
        )
    })
}

async fn render_with(
    state: &SharedState,
    id: i64,
    ps: Option<&str>,
    template_name: &str,
    template: &str,
) -> Result<RenderOut, ApiError> {
    let (out, doc, cards) = load_doc_out(state, id, ps, true).await?;
    // The RENDER shows the findings the store holds after reconciliation —
    // minted slugs, tombstones and human dispositions included — not the
    // ones the document's front matter happened to declare (which may carry
    // no slug at all, and which a sidecar compose leaves empty entirely).
    let findings: Vec<render::FindingLine> = out
        .findings
        .iter()
        .map(|f| render::FindingLine {
            slug: f.slug.clone(),
            act: f.act.clone(),
            severity: f.severity.clone(),
            blocking: f.blocking,
            category: f.category.clone(),
            title: f.title.clone(),
            location: f.location_path.clone(),
            superseded: f.superseded,
            disposition: f.disposition.clone(),
        })
        .collect();
    let title = doc_title(&out);
    // V73-K5 (gap 6) — `pr_number` (the review's own PR binding) and
    // `risk_score` (the pre-K1 `report_json.risk_score` lane) both live
    // outside the document; one extra cheap read, alongside the export
    // machine block's OWN finding rows (gap 4) so this is the only place
    // that pays for either.
    let summary_md = doc.summary_md.clone();
    let risk = doc.risk.clone();
    let (pr_number, report_risk_score, export_findings, pr_labels) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let binding = store.get_review_pr_binding(id)?;
            let pr_number = binding.as_ref().and_then(|b| b.pr_number);
            let labels: Vec<String> = binding
                .as_ref()
                .and_then(|b| b.pr_meta_json.as_deref())
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                .and_then(|v| {
                    v.get("labels").and_then(|l| l.as_array()).map(|arr| {
                        arr.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                })
                .unwrap_or_default();
            let risk_score = store
                .get_review_report(id)?
                .and_then(|r| crate::reviews::report_risk_score_numeric(r.report_json.as_deref()));
            let findings = store.list_review_findings(id, None, false)?;
            Ok((pr_number, risk_score, findings, labels))
        })
        .await?;
    let tags = render::kb_tags(&out.repo, pr_number, &pr_labels);
    let summary_text = render::summary_text(&summary_md);
    let ctx = RenderCtx {
        repo: &out.repo,
        review_id: id,
        title: &title,
        ps_number: out.ps_number,
        revision: out.revision,
        tier: &out.tier,
        rendered_at: now_unix(),
        omitted: &out.omitted,
        pr_number,
        risk_score: report_risk_score,
        tags: &tags,
        summary_text: &summary_text,
    };
    let rendered = render::render(template, &doc, &findings, &cards, &ctx);
    // V73-K5 (gap 4) — every render, including the built-in default
    // template, embeds a re-importable machine block: `kb-code review
    // import-legacy` on this SAME export reproduces the document (modulo
    // freshly-minted finding slugs). Appended OUTSIDE the operator's own
    // template substitution — a custom template that names no placeholder
    // for it must still round-trip.
    let machine_block = crate::review_legacy::export_block_html(
        &crate::review_legacy::export_block_json(&summary_md, risk.as_ref(), &export_findings),
    );
    let html = format!("{}\n{machine_block}\n", rendered.html);
    Ok(RenderOut {
        schema: RENDER_SCHEMA,
        review_id: id,
        ps_number: out.ps_number,
        revision: out.revision,
        template: template_name.to_string(),
        html,
        unknown_placeholders: rendered.unknown_placeholders,
        cards: cards.len(),
        orphans: cards
            .iter()
            .filter(|c| c.state == cards::STATE_ORPHAN)
            .count(),
    })
}

/// The export's `<title>` — the document's own first Markdown heading when
/// it has one, else the repo and review id. Never invented prose.
fn doc_title(out: &DocOut) -> String {
    format!("{} · review {}", out.repo, out.review_id)
}

// --- route contracts -------------------------------------------------------

use crate::entities::RouteContract;

pub const DOC_ROUTE: RouteContract = RouteContract {
    path: "/api/reviews/{id}/doc",
    handler: "review_doc::routes::get_review_doc",
    required_params: &[],
    params_accept_without: doc_params_accept_without,
};

pub const DOC_LINT_ROUTE: RouteContract = RouteContract {
    path: "/api/reviews/{id}/doc/lint",
    handler: "review_doc::routes::lint_review_doc",
    required_params: &[],
    params_accept_without: doc_params_accept_without,
};

pub const DOC_RENDER_ROUTE: RouteContract = RouteContract {
    path: "/api/reviews/{id}/doc/render",
    handler: "review_doc::routes::render_review_doc",
    required_params: &[],
    params_accept_without: render_params_accept_without,
};

fn doc_params_accept_without(omit: &str) -> bool {
    let mut map = serde_json::Map::new();
    if omit != "ps" {
        map.insert("ps".to_string(), Value::String("latest".to_string()));
    }
    if omit != "resolve" {
        // `resolve` is a BOOL on the wire — `?resolve=true`, the same
        // spelling `?all=true` / `?include_superseded=true` already use on
        // this route family. `serde_urlencoded` parses a bool with
        // `str::parse::<bool>()`, which accepts `true`/`false` and NOTHING
        // else, so `?resolve=1` is a 400 with a plain-text body rather than
        // this crate's JSON error shape — pinned here rather than
        // rediscovered.
        map.insert("resolve".to_string(), Value::Bool(true));
    }
    serde_json::from_value::<DocParams>(Value::Object(map)).is_ok()
}

fn render_params_accept_without(omit: &str) -> bool {
    let mut map = serde_json::Map::new();
    for (k, v) in [("ps", "latest"), ("template", "default")] {
        if k != omit {
            map.insert(k.to_string(), Value::String(v.to_string()));
        }
    }
    serde_json::from_value::<RenderParams>(Value::Object(map)).is_ok()
}

/// Every route V73-K1 adds, declared as DATA beside the routes so
/// kb-code-server's own dead-surface walk (invariant 15) and kb-code-cli's
/// mirror both see it. `POST …/doc/render` shares `DOC_RENDER_ROUTE`'s path
/// (one path, two methods) and `POST …/compose` predates this unit, so
/// neither has a second entry.
pub const V73_K1_ROUTES: &[RouteContract] = &[DOC_ROUTE, DOC_LINT_ROUTE, DOC_RENDER_ROUTE];

// --- compose's document half -----------------------------------------------

/// What `compose` needs from this module: the parse, the lint, the store
/// row to append, and the resolved echo. Kept here (rather than in
/// `review_findings`) so every rule about a DOCUMENT lives in one module,
/// and `review_findings` keeps owning the findings transaction.
pub struct PreparedDoc {
    pub doc: ReviewDoc,
    pub findings: Vec<DocFinding>,
    pub lint: LintOut,
    pub cards: Vec<Card>,
    pub new_row: crate::store::NewReviewDoc,
}

/// Parse + lint a candidate document and build the row `compose` would
/// append. Returns the lint even when it fails, so the caller can hand the
/// author every problem at once rather than the first one.
#[allow(clippy::too_many_arguments)]
pub async fn prepare_doc(
    state: &SharedState,
    id: i64,
    repo_id: i64,
    repo_root: &Path,
    ps: &ReviewPatchsetRow,
    doc_md: &str,
    tier: Tier,
    findings_override: Option<&[DocFinding]>,
) -> Result<Result<PreparedDoc, (LintOut, Vec<Card>)>, ApiError> {
    let inputs = repo_inputs(repo_root, ps).await?;
    // `None` — this is a CANDIDATE document, not yet composed, so it has
    // no `created_at` to be stale relative to (the same reasoning
    // `lint::structural_rows`'s `composed_at` doc states).
    let (lint, cards) = lint_and_resolve(
        state,
        id,
        repo_id,
        repo_root,
        ps,
        &inputs,
        doc_md,
        tier,
        findings_override,
        None,
    )
    .await?;
    let Ok(doc) = review_doc::parse(doc_md) else {
        return Ok(Err((lint, cards)));
    };
    if !lint.ok() {
        return Ok(Err((lint, cards)));
    }
    let findings = match findings_override {
        Some(f) => f.to_vec(),
        None => doc.findings.clone().unwrap_or_default(),
    };
    let new_row = crate::store::NewReviewDoc {
        review_id: id,
        ps_number: ps.ps_number,
        schema: review_doc::SCHEMA.to_string(),
        tier: tier.as_str().to_string(),
        doc_md: doc_md.to_string(),
        summary_md: doc.summary_md.clone(),
        risk_level: doc.risk.as_ref().map(|r| r.level.clone()),
        risk_why: doc.risk.as_ref().map(|r| r.why.clone()),
        omitted_json: serde_json::to_string(&review_doc::omitted_blocks(&doc))
            .unwrap_or_else(|_| "[]".to_string()),
        author_json: doc
            .author
            .as_ref()
            .and_then(|a| serde_json::to_string(a).ok()),
    };
    Ok(Ok(PreparedDoc {
        doc,
        findings,
        lint,
        cards,
        new_row,
    }))
}

#[cfg(test)]
mod ci_derive_tests {
    use super::*;

    #[test]
    fn an_authored_ci_block_wins_over_any_pr_meta_snapshot() {
        let authored = vec![review_doc::CiCheck {
            name: "custom".to_string(),
            status: "skipped".to_string(),
            url: None,
            observed_at: None,
        }];
        let pr_meta = serde_json::json!({ "checks": [
            { "name": "build", "status": "pass" }
        ] })
        .to_string();
        let out = derive_ci(&authored, Some(&pr_meta), Some(1));
        assert_eq!(out.source, "authored");
        assert_eq!(out.checks, authored);
    }

    #[test]
    fn an_empty_authored_block_derives_from_the_pr_meta_snapshot() {
        let pr_meta = serde_json::json!({ "checks": [
            { "name": "build", "status": "pass" },
            { "name": "lint", "status": "fail", "note": "failure" },
            { "name": "docs", "status": "warn", "note": "skipped" },
            { "name": "deploy", "status": "pending" }
        ] })
        .to_string();
        let out = derive_ci(&[], Some(&pr_meta), Some(1_700_000_000));
        assert_eq!(out.source, "derived");
        assert_eq!(out.checks.len(), 4);
        let by_name = |n: &str| out.checks.iter().find(|c| c.name == n).unwrap();
        assert_eq!(by_name("build").status, "success");
        assert_eq!(by_name("lint").status, "failure");
        assert_eq!(by_name("docs").status, "skipped");
        assert_eq!(by_name("deploy").status, "pending");
        assert_eq!(by_name("build").observed_at, Some(1_700_000_000));
        assert_eq!(by_name("build").url, None);
    }

    #[test]
    fn no_pr_meta_and_no_authored_block_is_an_honest_empty_derived_list() {
        let out = derive_ci(&[], None, None);
        assert_eq!(out.source, "derived");
        assert!(out.checks.is_empty());
    }

    #[test]
    fn ci_status_from_check_prefers_the_raw_note_over_the_coarse_status() {
        assert_eq!(ci_status_from_check("warn", Some("neutral")), "skipped");
        assert_eq!(ci_status_from_check("warn", Some("stale")), "skipped");
        assert_eq!(ci_status_from_check("pending", Some("queued")), "pending");
        assert_eq!(
            ci_status_from_check("pending", Some("in_progress")),
            "pending"
        );
        assert_eq!(ci_status_from_check("fail", Some("cancelled")), "failure");
        // No note at all: falls back to the coarse status.
        assert_eq!(ci_status_from_check("pass", None), "success");
        assert_eq!(ci_status_from_check("warn", None), "skipped");
    }
}
