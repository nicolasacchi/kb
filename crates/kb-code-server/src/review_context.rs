//! v0.44 F9b — `GET /api/reviews/{id}/context?ps=&budget=` (`kbc-review-context/1`)
//! and `GET /api/reviews/{id}/explain-base` (`kbc-review-base-explain/1`):
//! ONE deterministic context bundle for the agent reviewing a patchset.
//!
//! Before this module the agent chained find, status, show, findings list,
//! timeline, github-threads, diff --stat, diff --patch, log and cat to learn
//! what a human already knew, and every skill rebuilt its own prompt. The
//! bundle is a PURE COMPOSITION of reads that already exist, in a fixed
//! order, cut by a token budget:
//!
//! 1. `header`: title, state, refs, PR binding, the patchset (with why it was minted), the base envelope and HOW the base was resolved, the verdict and the local drift probe. Always present; never cut.
//! 2. `threads`: the open human threads (non-finding), exactly the objects `GET …/comments` returns ([`crate::review_comments::compose_comments`]).
//! 3. `findings`: the findings with dispositions, exactly the objects `GET …/findings` returns ([`crate::review_findings::compose_findings_list`]), blockers first.
//! 4. `other_reviews`: findings a human DISPUTED or WAIVED in other reviews of this repo on the paths this patchset changes ([`crate::store::Store::other_review_judgements`]).
//! 5. `since`: what the author changed since the verdict ([`crate::review_since`]), when a verdict sits on an earlier patchset.
//! 6. `reading_order`: the deterministic reading order of the change set ([`crate::review_map::build_order_sync`]).
//! 7. `files`: the change set (stat rows).
//! 8. `patch`: the patch text in READING ORDER until the budget runs out.
//!
//! # The budget rule
//!
//! Sections are filled in order. A list section keeps the longest PREFIX of
//! its items that still fits; the first section that is cut ends the walk, so
//! every later section is omitted entirely (never "a cut section, then a
//! lower-priority one that happened to fit"). The patch gets what is left,
//! whole files in reading order, the last one cut at a line boundary with a
//! marker. Every cut is named in `omitted[]` (`section`, `kept`, `total`,
//! `reason`). The bundle is always a LEADING PART of the full content, never
//! a summary — the same rule `tour pack` follows. The budget is approximate:
//! bytes = tokens x [`BYTES_PER_TOKEN`], measured on the JSON the section
//! serialises to.
//!
//! # Security posture
//!
//! The patch honours the same secret denylist `GET …/diff?mode=patch` does —
//! both read it from [`crate::review_views::partition_by_secret_policy`]; a
//! denylisted file's hunks are absent and the file is NAMED under
//! `patch.redacted` (pattern, never bytes). Nothing from the transcript-text
//! lanes (loopback-only) is read. A bearer read, computed per request, never
//! stored, never a score and never a verdict; no in-daemon LLM.

use crate::entities::RouteContract;
use crate::git::roots::GitCtx;
use crate::review_base::{BaseSource, ReviewBaseOut};
use crate::review_findings::{compose_findings_list, ListFindingsParams};
use crate::review_views::{
    partition_by_secret_policy, patch_text, truncate_patch, BYTES_PER_TOKEN, MAX_PATCH_BYTES,
};
use crate::reviews::{files_changed, require_review, resolve_ps, review_base_block, verdict_block};
use crate::routes::ApiError;
use crate::state::SharedState;
use crate::store::{ReviewPatchsetRow, ReviewRow, Store, StoreBlocking};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

pub const CONTEXT_SCHEMA: &str = "kbc-review-context/1";
pub const EXPLAIN_BASE_SCHEMA: &str = "kbc-review-base-explain/1";
/// The bundle size when `?budget=` is absent (tokens).
pub const DEFAULT_BUDGET_TOKENS: u64 = 20_000;
/// The most a caller may ask for (tokens): the patch ceiling in tokens.
pub const MAX_BUDGET_TOKENS: u64 = (MAX_PATCH_BYTES as u64) / BYTES_PER_TOKEN;
/// A cut patch file keeps no fewer bytes than this, or is omitted whole —
/// a three-line stub of a diff reads as noise.
const MIN_PATCH_STUB_BYTES: usize = 200;
/// `omitted[].files` lists at most this many paths.
const MAX_OMITTED_PATHS: usize = 50;

fn join_err(e: tokio::task::JoinError) -> ApiError {
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

// --- the pure assembler -----------------------------------------------------

/// Everything the bundle is made of, already composed. The assembler only
/// weighs, cuts and names; it reads nothing.
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    pub header: Value,
    pub threads: Vec<Value>,
    pub findings: Vec<Value>,
    pub other_reviews: Vec<Value>,
    /// `Ok(report)` when a verdict-to-target `since` was computed, else the
    /// reason it does not apply.
    pub since: Option<Result<Value, String>>,
    pub reading_order: Vec<Value>,
    pub files: Vec<Value>,
    /// `(path, that file's unified diff)` in reading order, denylisted files
    /// already excluded.
    pub patches: Vec<(String, String)>,
    pub redacted: Vec<Value>,
}

fn json_len(v: &Value) -> usize {
    // +1 for the separating comma; deterministic, no allocation tricks.
    serde_json::to_string(v).map(|s| s.len()).unwrap_or(0) + 1
}

fn escaped_len(s: &str) -> usize {
    serde_json::to_string(s)
        .map(|j| j.len().saturating_sub(2))
        .unwrap_or(s.len())
}

/// Keep the longest prefix of `items` whose weights fit in `remaining`.
/// Returns `(kept, bytes)`. Pure.
fn fit_prefix(items: &[Value], remaining: usize) -> (usize, usize) {
    let mut used = 0usize;
    let mut kept = 0usize;
    for it in items {
        let w = json_len(it);
        if used + w > remaining {
            break;
        }
        used += w;
        kept += 1;
    }
    (kept, used)
}

/// The leading part of one file's diff that fits `remaining` bytes once
/// JSON-escaped, with [`truncate_patch`]'s marker line, or `None` when not
/// even [`MIN_PATCH_STUB_BYTES`] fit. The raw limit shrinks by an eighth per
/// attempt (newlines and quotes grow when escaped), so the result is a
/// deterministic function of its inputs. Pure.
fn fit_stub(body: &str, remaining: usize, reason: &str) -> Option<String> {
    let mut limit = remaining;
    while limit >= MIN_PATCH_STUB_BYTES {
        let (piece, _) = truncate_patch(body, limit, reason);
        if escaped_len(&piece) <= remaining {
            return Some(piece);
        }
        limit = limit - limit / 8 - 1;
    }
    None
}

/// Order per-file diffs by the reading order. A part matches a stop path
/// when its key is that path or, for a rename (`a/old b/new`), ends with
/// ` b/<path>`; parts no stop names keep their git order after the named
/// ones. Pure.
pub fn order_patches(parts: Vec<(String, String)>, order: &[String]) -> Vec<(String, String)> {
    let mut slots: Vec<Option<(String, String)>> = parts.into_iter().map(Some).collect();
    let mut out: Vec<(String, String)> = Vec::new();
    for p in order {
        let suffix = format!(" b/{p}");
        let hit = slots.iter().position(|s| {
            s.as_ref()
                .is_some_and(|(k, _)| k == p || k.ends_with(&suffix))
        });
        if let Some(i) = hit {
            if let Some((_, body)) = slots[i].take() {
                out.push((p.clone(), body));
            }
        }
    }
    for (k, body) in slots.into_iter().flatten() {
        let label = k
            .rsplit_once(" b/")
            .map(|(_, p)| p.to_string())
            .unwrap_or(k);
        out.push((label, body));
    }
    out
}

fn omitted_entry(section: &str, kept: usize, total: usize) -> Value {
    json!({ "section": section, "reason": "budget", "kept": kept, "total": total })
}

/// Weigh, cut and name. Returns the bundle's body (everything except the
/// `schema`/`review_id`/`repo`/`ps` envelope the route adds). Pure and
/// deterministic: the same [`Inputs`] and budget give the same value.
pub fn assemble(inputs: Inputs, budget_tokens: u64) -> Value {
    let budget_bytes = usize::try_from(budget_tokens.saturating_mul(BYTES_PER_TOKEN))
        .unwrap_or(usize::MAX)
        .min(MAX_PATCH_BYTES);
    let header_bytes = json_len(&inputs.header);
    let mut remaining = budget_bytes.saturating_sub(header_bytes);
    let mut used = header_bytes;
    let mut cut = false;
    let mut omitted: Vec<Value> = Vec::new();

    // The list sections, in their fixed order.
    let mut take_list = |name: &str, items: Vec<Value>| -> Vec<Value> {
        let total = items.len();
        if cut {
            if total > 0 {
                omitted.push(omitted_entry(name, 0, total));
            }
            return Vec::new();
        }
        let (kept, bytes) = fit_prefix(&items, remaining);
        if kept < total {
            cut = true;
            omitted.push(omitted_entry(name, kept, total));
        }
        remaining -= bytes;
        used += bytes;
        items.into_iter().take(kept).collect()
    };
    let threads = take_list("threads", inputs.threads);
    let findings = take_list("findings", inputs.findings);
    let other_reviews = take_list("other_reviews", inputs.other_reviews);

    // `since`: a "does not apply" answer is tiny and always shown (it is the
    // reason there is nothing to read); a real report is atomic, all or
    // nothing.
    let since = match inputs.since {
        None => Value::Null,
        Some(Err(reason)) => {
            let v = json!({ "applicable": false, "reason": reason });
            let w = json_len(&v);
            remaining = remaining.saturating_sub(w);
            used += w;
            v
        }
        Some(Ok(report)) => {
            let v = json!({ "applicable": true, "report": report });
            let w = json_len(&v);
            if cut || w > remaining {
                cut = true;
                omitted.push(omitted_entry("since", 0, 1));
                Value::Null
            } else {
                remaining -= w;
                used += w;
                v
            }
        }
    };

    let reading_order = {
        let total = inputs.reading_order.len();
        if cut {
            if total > 0 {
                omitted.push(omitted_entry("reading_order", 0, total));
            }
            Vec::new()
        } else {
            let (kept, bytes) = fit_prefix(&inputs.reading_order, remaining);
            if kept < total {
                cut = true;
                omitted.push(omitted_entry("reading_order", kept, total));
            }
            remaining -= bytes;
            used += bytes;
            inputs.reading_order.into_iter().take(kept).collect()
        }
    };
    let files = {
        let total = inputs.files.len();
        if cut {
            if total > 0 {
                omitted.push(omitted_entry("files", 0, total));
            }
            Vec::new()
        } else {
            let (kept, bytes) = fit_prefix(&inputs.files, remaining);
            if kept < total {
                cut = true;
                omitted.push(omitted_entry("files", kept, total));
            }
            remaining -= bytes;
            used += bytes;
            inputs.files.into_iter().take(kept).collect()
        }
    };

    // The patch: whole files in reading order, the last one cut.
    let files_total = inputs.patches.len();
    let mut text = String::new();
    let mut included = 0usize;
    let mut truncated = false;
    let mut left_out: Vec<String> = Vec::new();
    if !cut {
        let reason = format!("budget {budget_tokens} tokens");
        for (i, (_, body)) in inputs.patches.iter().enumerate() {
            let w = escaped_len(body);
            if w <= remaining {
                text.push_str(body);
                remaining -= w;
                used += w;
                included += 1;
                continue;
            }
            truncated = true;
            if let Some(piece) = fit_stub(body, remaining, &reason) {
                let pw = escaped_len(&piece);
                text.push_str(&piece);
                used += pw;
                included += 1;
                left_out = inputs.patches[i + 1..]
                    .iter()
                    .map(|(p, _)| p.clone())
                    .collect();
                break;
            }
            left_out = inputs.patches[i..].iter().map(|(p, _)| p.clone()).collect();
            break;
        }
    } else {
        left_out = inputs.patches.iter().map(|(p, _)| p.clone()).collect();
    }
    if included < files_total {
        let mut e = omitted_entry("patch", included, files_total);
        let shown: Vec<&String> = left_out.iter().take(MAX_OMITTED_PATHS).collect();
        e["files"] = json!(shown);
        if left_out.len() > MAX_OMITTED_PATHS {
            e["files_truncated"] = json!(true);
        }
        omitted.push(e);
    }

    json!({
        "budget": {
            "tokens": budget_tokens,
            "bytes": budget_bytes,
            "used_bytes": used,
            "estimate": format!("bytes = tokens x {BYTES_PER_TOKEN}; measured on the serialised sections"),
        },
        "header": inputs.header,
        "threads": threads,
        "findings": findings,
        "other_reviews": other_reviews,
        "since": since,
        "reading_order": reading_order,
        "files": files,
        "patch": {
            "text": text,
            "files_included": included,
            "files_total": files_total,
            "truncated": truncated,
            "redacted": inputs.redacted,
        },
        "omitted": omitted,
        "note": "A leading part of the full content in a fixed order, never a summary: every cut is named in omitted[]. Derived on each read from existing reads; shown, never a verdict; the daemon carries nothing forward.",
    })
}

// --- how the base was resolved (explain-base, pure) -------------------------

/// One rung of the base resolution chain as it applies to THIS review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rung {
    pub rung: &'static str,
    pub label: &'static str,
    /// `applied` | `missed` | `not-reached` | `not-built` | `unknown`.
    pub status: &'static str,
}

const PR_CHAIN: &[(&str, &str)] = &[
    ("explicit", "an explicit --base"),
    ("forge-api", "the PR target the forge reported"),
    ("caller", "a target the caller supplied"),
    ("merge-ref", "merge-ref inference"),
    ("default-assumed", "the default branch, assumed"),
];
const NON_PR_CHAIN: &[(&str, &str)] = &[
    ("explicit", "an explicit --base"),
    ("stack-parent", "the stack parent branch"),
    ("upstream", "the head branch's upstream"),
    ("default-assumed", "the default branch"),
];

/// The resolution chain (README section 6) for a PR-bound or plain review,
/// with each rung marked by what the RECORDED source implies: first hit wins,
/// so every rung before the recorded one missed and every one after was not
/// reached. A legacy or unrecorded source marks every rung `unknown` — the
/// chain is never invented for a row that recorded nothing. `merge-ref`
/// inference is not built; it is `not-built`. Pure.
pub fn resolution_chain(pr_bound: bool, recorded_source: Option<&str>) -> Vec<Rung> {
    let chain = if pr_bound { PR_CHAIN } else { NON_PR_CHAIN };
    let applied = recorded_source
        .filter(|s| *s != "legacy")
        .and_then(|s| chain.iter().position(|(r, _)| *r == s));
    chain
        .iter()
        .enumerate()
        .map(|(i, (rung, label))| {
            let status = if *rung == "merge-ref" {
                "not-built"
            } else {
                match applied {
                    None => "unknown",
                    Some(a) if i < a => "missed",
                    Some(a) if i == a => "applied",
                    Some(_) => "not-reached",
                }
            };
            Rung {
                rung,
                label,
                status,
            }
        })
        .collect()
}

/// One sentence on what the base policy IS and where it came from. Pure.
pub fn base_summary(base: &ReviewBaseOut) -> String {
    let src = base
        .source
        .as_deref()
        .and_then(BaseSource::parse)
        .map(|s| s.label())
        .unwrap_or("unrecorded");
    match (base.mode.as_deref(), base.branch.as_deref()) {
        (Some("track"), Some(b)) => format!(
            "tracks `{b}` on the forge (source: {src}; set by {})",
            base.set_by
        ),
        (Some("local"), Some(b)) => format!(
            "follows the member clone's `{b}` (source: {src}; set by {})",
            base.set_by
        ),
        (Some("pin"), _) => format!(
            "pinned to a frozen commit, following nothing (source: {src}; set by {})",
            base.set_by
        ),
        _ => "a legacy base ref classified on read; nothing was recorded about how it was chosen"
            .to_string(),
    }
}

fn rungs_json(rungs: &[Rung]) -> Vec<Value> {
    rungs
        .iter()
        .map(|r| json!({ "rung": r.rung, "label": r.label, "status": r.status }))
        .collect()
}

// --- store-side composition (blocking) --------------------------------------

fn base_value(
    store: &Store,
    review: &ReviewRow,
    root: &Path,
    target: &ReviewPatchsetRow,
    pr_bound: bool,
) -> Value {
    let (base, warnings) = review_base_block(store, review, root, Some(&target.base_sha));
    json!({
        "base": base,
        "warnings": warnings,
        "resolution": {
            "summary": base_summary(&base),
            "source": base.source,
            "pr_bound": pr_bound,
        },
    })
}

fn patchset_list(store: &Store, id: i64, patchsets: &[ReviewPatchsetRow]) -> Vec<Value> {
    patchsets
        .iter()
        .map(|p| {
            let fields = store.get_patchset_base(id, p.ps_number).ok().flatten();
            json!({
                "ps": p.ps_number,
                "tip_sha": p.tip_sha,
                "base_sha": p.base_sha,
                "base_tip_sha": fields.as_ref().and_then(|f| f.base_tip_sha.clone()),
                "kind": fields.and_then(|f| f.kind),
                "captured_at": p.captured_at,
            })
        })
        .collect()
}

fn header_value(
    store: &Store,
    review: &ReviewRow,
    root: &Path,
    target: &ReviewPatchsetRow,
    patchsets: &[ReviewPatchsetRow],
) -> Result<Value, ApiError> {
    let id = review.id;
    let binding = store.get_review_pr_binding(id)?.unwrap_or_default();
    let latest = patchsets.last();
    let latest_ps = latest.map(|p| p.ps_number);
    let (verdict, verdict_stale) = verdict_block(review, latest_ps);
    let pr_bound = binding.pr_number.is_some();
    // The LOCAL half of `review status` only (no network): the head the
    // review last synced against vs the latest patchset tip.
    let head_moved = match (binding.pr_head_sha.as_deref(), latest) {
        (Some(h), Some(l)) if pr_bound => Some(h != l.tip_sha),
        _ => None,
    };
    let kind = store
        .get_patchset_base(id, target.ps_number)
        .ok()
        .flatten()
        .and_then(|f| f.kind);
    let base = base_value(store, review, root, target, pr_bound);
    Ok(json!({
        "title": review.title,
        "state": review.state,
        "head_ref": review.head_ref,
        "base_ref": review.base_ref,
        "pr_number": binding.pr_number,
        "pr_head_sha": binding.pr_head_sha,
        "patchset": {
            "ps": target.ps_number,
            "tip_sha": target.tip_sha,
            "base_sha": target.base_sha,
            "kind": kind,
            "is_latest": latest_ps == Some(target.ps_number),
            "latest_ps": latest_ps,
        },
        "base": base["base"],
        "base_resolution": base["resolution"],
        "warnings": base["warnings"],
        "verdict": verdict,
        "verdict_stale": verdict_stale,
        "drift": { "head_moved": head_moved, "probe": "local: stored PR head vs latest patchset tip" },
        "explain_base": format!("kb-code review explain-base {id}"),
    }))
}

fn severity_rank(f: &Value) -> u8 {
    match f["severity"].as_str() {
        Some("blocker") => 0,
        Some("concern") => 1,
        _ => 2,
    }
}

/// Findings, blockers first and otherwise in the route's order (a stable
/// sort), so a budget cut drops `ok` notes before blockers. Pure.
pub fn order_findings(mut findings: Vec<Value>) -> Vec<Value> {
    findings.sort_by_key(severity_rank);
    findings
}

/// The open human threads: every comment of the `…/comments` body except a
/// finding's own thread (that is in `findings`). Pure.
pub fn human_threads(comments_body: &Value) -> Vec<Value> {
    comments_body["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|g| g["comments"].as_array().into_iter().flatten())
        .filter(|c| c["intent"].as_str() != Some(crate::annotations::INTENT_FINDING))
        .cloned()
        .collect()
}

// --- the routes ---------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct ContextParams {
    /// Patchset number or `latest` (default).
    #[serde(default)]
    pub ps: Option<String>,
    /// Bundle size in tokens ([`BYTES_PER_TOKEN`] bytes each).
    #[serde(default)]
    pub budget: Option<u64>,
}

/// `GET /api/reviews/{id}/context?ps=&budget=`
pub async fn review_context_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Query(params): Query<ContextParams>,
) -> Result<Response, ApiError> {
    let budget = params.budget.unwrap_or(DEFAULT_BUDGET_TOKENS);
    if budget > MAX_BUDGET_TOKENS {
        return Err(ApiError::bad_request(format!(
            "budget must be at most {MAX_BUDGET_TOKENS} tokens, got {budget}"
        )));
    }
    let (review, repo, repo_id) = require_review(&state, id).await?;
    let ctx = GitCtx::resolve_entry(&state.store, repo).await;
    let root = repo.path.clone();

    // One store pass: the target patchset, the header, the verdict's
    // patchset (for `since`).
    let review_c = review.clone();
    let ps_param = params.ps.clone();
    let (target, bundle_header, verdict_ps_row) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let target = resolve_ps(store, id, ps_param.as_deref())?;
            let patchsets = store.list_patchsets(id)?;
            let bundle_header = header_value(store, &review_c, &root, &target, &patchsets)?;
            let verdict_row = match review_c.verdict_ps {
                Some(vp) => store.get_patchset(id, vp)?,
                None => None,
            };
            Ok((target, bundle_header, verdict_row))
        })
        .await?;
    let ps_str = target.ps_number.to_string();

    // The change set (git), once.
    let files = {
        let (c, b, t) = (ctx.clone(), target.base_sha.clone(), target.tip_sha.clone());
        tokio::task::spawn_blocking(move || files_changed(&c, &b, &t))
            .await
            .map_err(join_err)??
    };

    // Threads: the SAME composition the comments route returns.
    let comments = {
        let (c, name, ps) = (ctx.clone(), review.repo.clone(), ps_str.clone());
        state
            .store
            .run_blocking(move |store| {
                crate::review_comments::compose_comments(
                    store,
                    &c,
                    &name,
                    repo_id,
                    id,
                    Some(ps.as_str()),
                    false,
                )
            })
            .await?
    };
    let threads = human_threads(&comments);

    // Findings: the SAME composition the findings route returns.
    let findings_body = compose_findings_list(
        &state,
        id,
        &ListFindingsParams {
            ps: Some(ps_str.clone()),
            disposition: None,
            include_superseded: false,
        },
    )
    .await?;
    let findings = order_findings(
        findings_body["findings"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
    );

    // Other reviews' disputes/waives on the paths this patchset changes.
    let mut paths: Vec<String> = Vec::new();
    for f in &files {
        paths.push(f.path.clone());
        if let Some(o) = &f.old_path {
            paths.push(o.clone());
        }
    }
    paths.sort();
    paths.dedup();
    let repo_name = review.repo.clone();
    let other_reviews = state
        .store
        .run_blocking(move |store| -> Result<Vec<Value>, ApiError> {
            let rows = store.other_review_judgements(&repo_name, id, &paths)?;
            let ids: Vec<i64> = {
                let mut v: Vec<i64> = rows.iter().map(|r| r.review_id).collect();
                v.sort_unstable();
                v.dedup();
                v
            };
            let prs = store.get_review_pr_bindings(&ids)?;
            Ok(rows
                .into_iter()
                .map(|r| {
                    json!({
                        "review_id": r.review_id,
                        "review_state": r.review_state,
                        "pr_number": prs.get(&r.review_id).and_then(|b| b.pr_number),
                        "path": r.path,
                        "slug": r.slug,
                        "severity": r.severity,
                        "title": r.title,
                        "disposition": r.disposition,
                        "note": r.note,
                        "by": r.by,
                        "at": r.at,
                    })
                })
                .collect())
        })
        .await?;

    // Since the verdict, when a verdict sits on an EARLIER patchset.
    let since: Option<Result<Value, String>> = match (review.verdict_ps, verdict_ps_row) {
        (None, _) => Some(Err("no-verdict".to_string())),
        (Some(vp), _) if vp >= target.ps_number => {
            Some(Err("verdict-on-this-or-a-later-patchset".to_string()))
        }
        (Some(_), None) => Some(Err("verdict-patchset-missing".to_string())),
        (Some(_), Some(from)) => {
            let (c, to) = (ctx.clone(), target.clone());
            let computed = tokio::task::spawn_blocking(move || {
                crate::review_since::compute_since(&c, id, &from, &to, "verdict")
            })
            .await
            .map_err(join_err)?;
            Some(match computed {
                Ok(report) => Ok(serde_json::to_value(report).unwrap_or(Value::Null)),
                Err(_) => Err("unreadable".to_string()),
            })
        }
    };

    // Reading order over the change set (the route's own composition).
    let order = {
        let store = state.store.clone();
        let (files_c, name, ps_n) = (files.clone(), review.repo.clone(), target.ps_number);
        tokio::task::spawn_blocking(move || {
            crate::review_map::build_order_sync(&store, repo_id, id, &name, ps_n, &files_c)
        })
        .await
        .map_err(join_err)??
    };
    let stops = order["stops"].as_array().cloned().unwrap_or_default();
    let order_paths: Vec<String> = stops
        .iter()
        .filter_map(|s| s["path"].as_str().map(str::to_string))
        .collect();

    // The patch: denylist first, then reading order.
    let (redacted, allowed) = partition_by_secret_policy(&state.secret_policy, &files);
    let raw = patch_text(
        &ctx,
        &target.base_sha,
        &target.tip_sha,
        &allowed,
        redacted.is_empty(),
    )
    .await?;
    let patches = order_patches(crate::review_since::split_file_diffs(&raw), &order_paths);

    let file_rows: Vec<Value> = files
        .iter()
        .map(|f| {
            json!({
                "path": f.path,
                "old_path": f.old_path,
                "status": f.status,
                "additions": f.insertions,
                "deletions": f.deletions,
                "binary": f.binary,
            })
        })
        .collect();

    let mut body = assemble(
        Inputs {
            header: bundle_header,
            threads,
            findings,
            other_reviews,
            since,
            reading_order: stops,
            files: file_rows,
            patches,
            redacted,
        },
        budget,
    );
    body["schema"] = json!(CONTEXT_SCHEMA);
    body["review_id"] = json!(id);
    body["repo"] = json!(review.repo);
    body["ps"] = json!(target.ps_number);
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(body)).into_response())
}

/// `GET /api/reviews/{id}/explain-base` — how this review's base was chosen,
/// from what the review RECORDED (the stored policy, its source and status,
/// each patchset's kind). It does not re-run the chain: that can need a
/// network probe, and a read must not fetch.
pub async fn review_explain_base_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
) -> Result<Response, ApiError> {
    let (review, repo, _repo_id) = require_review(&state, id).await?;
    let root = repo.path.clone();
    let review_c = review.clone();
    let body = state
        .store
        .run_blocking(move |store| -> Result<Value, ApiError> {
            let patchsets = store.list_patchsets(id)?;
            let latest = patchsets
                .last()
                .cloned()
                .ok_or_else(|| ApiError::bad_request(format!("review {id} has no patchsets")))?;
            let binding = store.get_review_pr_binding(id)?.unwrap_or_default();
            let pr_bound = binding.pr_number.is_some();
            let b = base_value(store, &review_c, &root, &latest, pr_bound);
            let source = b["base"]["source"].as_str().map(str::to_string);
            let chain = resolution_chain(pr_bound, source.as_deref());
            Ok(json!({
                "schema": EXPLAIN_BASE_SCHEMA,
                "review_id": id,
                "repo": review_c.repo,
                "pr_number": binding.pr_number,
                "summary": b["resolution"]["summary"],
                "base": b["base"],
                "warnings": b["warnings"],
                "chain": rungs_json(&chain),
                "patchsets": patchset_list(store, id, &patchsets),
                "note": "Explains the resolution this review RECORDED; it does not re-run the chain (which can need a network probe). Rungs before the recorded source missed, rungs after were not reached; merge-ref inference is not built; a legacy row records nothing, so its rungs are unknown.",
            }))
        })
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(body)).into_response())
}

// --- route contracts (invariant 15) -------------------------------------------

fn context_accept_without(omit: &str) -> bool {
    let mut map = serde_json::Map::new();
    for (k, v) in [("ps", json!("latest")), ("budget", json!(1000))] {
        if k != omit {
            map.insert(k.to_string(), v);
        }
    }
    serde_json::from_value::<ContextParams>(Value::Object(map)).is_ok()
}

fn explain_base_accept_without(_omit: &str) -> bool {
    true
}

pub const CONTEXT_ROUTE: RouteContract = RouteContract {
    path: "/api/reviews/{id}/context",
    handler: "review_context::review_context_route",
    required_params: &[],
    params_accept_without: context_accept_without,
};

pub const EXPLAIN_BASE_ROUTE: RouteContract = RouteContract {
    path: "/api/reviews/{id}/explain-base",
    handler: "review_context::review_explain_base_route",
    required_params: &[],
    params_accept_without: explain_base_accept_without,
};

/// v0.44 F9b's reads, walked from BOTH sides (router registration in
/// `entities`' test, CLI request builders in kb-code-cli's).
pub const V044_F9B_ROUTES: &[RouteContract] = &[CONTEXT_ROUTE, EXPLAIN_BASE_ROUTE];

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> Value {
        json!({ "id": id })
    }

    fn fixture() -> Inputs {
        Inputs {
            header: json!({ "title": "t" }),
            threads: vec![item("a"), item("b")],
            findings: vec![json!({ "slug": "f-1", "severity": "blocker" })],
            other_reviews: vec![],
            since: Some(Err("no-verdict".into())),
            reading_order: vec![json!({ "path": "x.rs" }), json!({ "path": "y.rs" })],
            files: vec![json!({ "path": "x.rs" }), json!({ "path": "y.rs" })],
            patches: vec![
                ("x.rs".into(), "diff --git a/x.rs b/x.rs\n+x\n".into()),
                ("y.rs".into(), "diff --git a/y.rs b/y.rs\n+y\n".into()),
            ],
            redacted: vec![json!({ "path": ".env", "pattern": ".env" })],
        }
    }

    /// The byte-identical golden: the whole bundle for a fixed fixture at a
    /// budget that holds everything. `used_bytes` is checked against an
    /// independent sum, the rest against the literal.
    #[test]
    fn golden_bundle_when_everything_fits() {
        let inputs = fixture();
        let want_used = json_len(&inputs.header)
            + inputs.threads.iter().map(json_len).sum::<usize>()
            + inputs.findings.iter().map(json_len).sum::<usize>()
            + json_len(&json!({ "applicable": false, "reason": "no-verdict" }))
            + inputs.reading_order.iter().map(json_len).sum::<usize>()
            + inputs.files.iter().map(json_len).sum::<usize>()
            + inputs
                .patches
                .iter()
                .map(|(_, b)| escaped_len(b))
                .sum::<usize>();
        let got = assemble(inputs, 1000);
        let golden: Value =
            serde_json::from_str(include_str!("../grammar/review-context.golden.json"))
                .expect("golden parses");
        let mut got_cmp = got.clone();
        got_cmp["budget"]["used_bytes"] = json!(0);
        assert_eq!(got_cmp, golden);
        assert_eq!(got["budget"]["used_bytes"], json!(want_used));
        // Same input, same bytes.
        assert_eq!(
            serde_json::to_string(&assemble(fixture(), 1000)).unwrap(),
            serde_json::to_string(&got).unwrap()
        );
    }

    /// A budget that holds the header and ONE thread: the cut section keeps
    /// its leading prefix, every later section is omitted whole and named.
    #[test]
    fn a_cut_keeps_a_leading_prefix_and_names_everything_after_it() {
        let inputs = fixture();
        let budget_bytes = json_len(&inputs.header) + json_len(&inputs.threads[0]) + 2;
        // Round UP to whole tokens: room for the header and thread "a" (and
        // the few spare bytes), not for thread "b" as well.
        let tokens = budget_bytes.div_ceil(BYTES_PER_TOKEN as usize) as u64;
        let got = assemble(inputs, tokens);
        assert_eq!(got["threads"], json!([{ "id": "a" }]));
        assert_eq!(got["findings"], json!([]));
        assert_eq!(got["reading_order"], json!([]));
        assert_eq!(got["files"], json!([]));
        assert_eq!(got["patch"]["text"], json!(""));
        let sections: Vec<(&str, u64, u64)> = got["omitted"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| {
                (
                    o["section"].as_str().unwrap(),
                    o["kept"].as_u64().unwrap(),
                    o["total"].as_u64().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            sections,
            vec![
                ("threads", 1, 2),
                ("findings", 0, 1),
                ("reading_order", 0, 2),
                ("files", 0, 2),
                ("patch", 0, 2),
            ]
        );
        assert!(
            got["omitted"]
                .as_array()
                .unwrap()
                .iter()
                .all(|o| o["reason"] == "budget"),
            "every cut says why"
        );
        let used = got["budget"]["used_bytes"].as_u64().unwrap();
        assert!(used <= got["budget"]["bytes"].as_u64().unwrap(), "{used}");
    }

    #[test]
    fn the_header_is_never_cut_even_by_a_zero_budget() {
        let got = assemble(fixture(), 0);
        assert_eq!(got["header"], json!({ "title": "t" }));
        assert_eq!(got["threads"], json!([]));
        assert!(got["omitted"].as_array().unwrap().len() >= 4);
    }

    /// The patch is cut at a line boundary on the LAST file and the files
    /// after it are named.
    #[test]
    fn the_patch_fills_whole_files_then_cuts_the_last_one() {
        let big = format!("diff --git a/b.rs b/b.rs\n{}", "+line\n".repeat(400));
        let mut inputs = Inputs {
            header: json!({}),
            patches: vec![
                ("a.rs".into(), "diff --git a/a.rs b/a.rs\n+a\n".into()),
                ("b.rs".into(), big),
                ("c.rs".into(), "diff --git a/c.rs b/c.rs\n+c\n".into()),
            ],
            ..Inputs::default()
        };
        inputs.since = None;
        // header "{}" = 3 bytes; leave room for a.rs plus ~600 bytes.
        let got = assemble(inputs, 190);
        let text = got["patch"]["text"].as_str().unwrap();
        assert!(text.starts_with("diff --git a/a.rs b/a.rs\n+a\n"));
        assert!(
            text.contains("diff --git a/b.rs"),
            "b.rs is cut, not dropped"
        );
        assert!(text.contains("[kb-code: patch truncated"), "{text}");
        assert!(!text.contains("a/c.rs"));
        assert_eq!(got["patch"]["truncated"], json!(true));
        assert_eq!(got["patch"]["files_included"], json!(2));
        let o = got["omitted"].as_array().unwrap();
        assert_eq!(o.len(), 1);
        assert_eq!(o[0]["section"], "patch");
        assert_eq!(o[0]["files"], json!(["c.rs"]));
    }

    #[test]
    fn patches_follow_the_reading_order_and_renames_match_on_the_new_path() {
        let parts = vec![
            ("z.rs".to_string(), "Z".to_string()),
            ("a/old.rs b/new.rs".to_string(), "R".to_string()),
            ("m.rs".to_string(), "M".to_string()),
            ("stray.rs".to_string(), "S".to_string()),
        ];
        let order = vec!["new.rs".to_string(), "m.rs".to_string(), "z.rs".to_string()];
        let got = order_patches(parts, &order);
        let labels: Vec<&str> = got.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(labels, vec!["new.rs", "m.rs", "z.rs", "stray.rs"]);
        let bodies: Vec<&str> = got.iter().map(|(_, b)| b.as_str()).collect();
        assert_eq!(bodies, vec!["R", "M", "Z", "S"]);
    }

    #[test]
    fn findings_are_blockers_first_and_otherwise_stable() {
        let f = |slug: &str, sev: &str| json!({ "slug": slug, "severity": sev });
        let got = order_findings(vec![
            f("ok1", "ok"),
            f("c1", "concern"),
            f("b1", "blocker"),
            f("c2", "concern"),
            f("b2", "blocker"),
        ]);
        let slugs: Vec<&str> = got.iter().map(|v| v["slug"].as_str().unwrap()).collect();
        assert_eq!(slugs, vec!["b1", "b2", "c1", "c2", "ok1"]);
    }

    #[test]
    fn a_finding_thread_is_not_a_human_thread() {
        let body = json!({ "groups": [
            { "path": "a.rs", "comments": [
                { "id": "1", "intent": "question" },
                { "id": "2", "intent": "finding" },
            ]},
            { "path": "", "comments": [ { "id": "3", "intent": "note" } ] },
        ]});
        let ids: Vec<String> = human_threads(&body)
            .iter()
            .map(|c| c["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids, vec!["1", "3"]);
    }

    #[test]
    fn the_chain_marks_rungs_by_the_recorded_source() {
        let statuses = |pr: bool, src: Option<&str>| -> Vec<&'static str> {
            resolution_chain(pr, src).iter().map(|r| r.status).collect()
        };
        assert_eq!(
            statuses(true, Some("forge-api")),
            vec![
                "missed",
                "applied",
                "not-reached",
                "not-built",
                "not-reached"
            ]
        );
        assert_eq!(
            statuses(true, Some("default-assumed")),
            vec!["missed", "missed", "missed", "not-built", "applied"]
        );
        assert_eq!(
            statuses(false, Some("upstream")),
            vec!["missed", "missed", "applied", "not-reached"]
        );
        // legacy / unrecorded / a source not in this chain: nothing invented
        assert_eq!(
            statuses(true, Some("legacy")),
            vec!["unknown", "unknown", "unknown", "not-built", "unknown"]
        );
        assert_eq!(statuses(false, None), vec!["unknown"; 4]);
        assert_eq!(statuses(false, Some("forge-api")), vec!["unknown"; 4]);
    }

    #[test]
    fn the_summary_says_what_the_policy_is() {
        let base = |mode: Option<&str>, branch: Option<&str>, source: Option<&str>| ReviewBaseOut {
            mode: mode.map(str::to_string),
            branch: branch.map(str::to_string),
            set_by: "auto".into(),
            source: source.map(str::to_string),
            state: None,
            merge_base: None,
            fetched_at: None,
            last_fetch: None,
            fetched_via: None,
        };
        assert_eq!(
            base_summary(&base(Some("track"), Some("main"), Some("forge-api"))),
            "tracks `main` on the forge (source: forge api; set by auto)"
        );
        assert!(base_summary(&base(Some("pin"), None, Some("explicit"))).starts_with("pinned"));
        assert!(base_summary(&base(None, None, None)).starts_with("a legacy base ref"));
    }

    #[test]
    fn the_params_deserialize_without_each_other() {
        assert!(context_accept_without(""));
        assert!(context_accept_without("ps"));
        assert!(context_accept_without("budget"));
    }
}
