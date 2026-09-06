//! `GET /api/kb/{kb}/resurface` — the deterministic, pull-only resurfacing
//! queue (thin slice: open comments + unfinished reads). Design + the
//! anti-feature analysis: `docs/research/kb-resurface-queue-2026-07.html`.
//!
//! A VIEW over existing state, never state of its own: candidates come from
//! the SHARED open-comments collector (`inbox::collect_open` — one
//! spawn_blocking `.review/` walk per kb, aggregated here per artifact) plus
//! one `reading_rollup` pass (in-progress reads), scored by the pure
//! `kb_core::resurface` module with a single injected `now_unix`. No queue
//! rows, no dismiss state, no counters anywhere in chrome. Reasons ride every
//! item so the CLI and the SPA strip share one wire truth.
//!
//! Per-kb only for now — a fleet scope must ride `routes::buffered_join`
//! (invariant #28) when it's wanted. Doc metadata (title / word_count /
//! category) comes from the shared gallery row-set memo (PF-R1; invariant
//! #15's `docs::gallery_snapshot`, the same per-(kb, generation) scan
//! `/docs`/`/facets`/`/edges` keep warm), so the route adds no per-item
//! storage round-trips and no independent full-corpus scan of its own.

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Extension, Path, Query, State},
    http::Response,
    response::IntoResponse,
    Json,
};
use kb_core::lists::ReadState;
use kb_core::resurface::{rank, ResurfaceSignals};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Default page size. The SPA strip renders only the top 2; the CLI default
/// shows a screenful. Hard cap keeps an oversized `?limit=` bounded.
const DEFAULT_LIMIT: u32 = 8;
const MAX_LIMIT: u32 = 50;

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct ResurfaceItem {
    pub id: String,
    pub title: String,
    /// Source-relative path for the SPA `/a/{kb}/{rel}` deep-link.
    pub source_relative: String,
    /// Total score = `comment_term + read_term` (weighted contributions, so
    /// an explain renderer needs no re-derivation).
    pub score: f32,
    pub comment_term: f32,
    pub read_term: f32,
    pub open_comments: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub oldest_open_unix: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub completion_pct: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub last_opened_unix: Option<i64>,
    /// Progress-aware remaining reading time, minutes — `word_count × (1 −
    /// completion) / 220wpm` ([`kb_core::lists::EST_WORDS_PER_MINUTE`]).
    /// Absent when the doc has no word count or the read signal is absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub est_min_left: Option<u32>,
    /// Server-built, deterministic reason strings — always present (the
    /// recollect house style: signals surfaced inline, never buried).
    pub reasons: Vec<String>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct ResurfaceResponse {
    pub items: Vec<ResurfaceItem>,
    /// The clock every score in `items` was computed against.
    pub now_unix: i64,
    /// W2.9 — the actual per-kb scoring weights `items` were computed
    /// with (`[kb.<name>.resurface]`, defaulting to the shipped
    /// constants). Additive + always present, so the CLI `--explain` and
    /// the SPA score-chip renderers can show the REAL arithmetic instead
    /// of a hardcoded mirror that could silently drift from a tuned kb.
    /// `score_floor` isn't echoed — it's a filter cutoff, not part of the
    /// visible per-item arithmetic.
    pub weights: ResurfaceWeights,
}

/// Wire projection of [`kb_core::resurface::ResurfaceWeights`] — see
/// `ResurfaceResponse::weights`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ResurfaceWeights {
    pub comment_weight: f32,
    pub read_weight: f32,
    pub comment_saturation: u32,
    pub read_halflife_days: f32,
}

fn weights_wire(w: &kb_core::resurface::ResurfaceWeights) -> ResurfaceWeights {
    ResurfaceWeights {
        comment_weight: w.comment_weight,
        read_weight: w.read_weight,
        comment_saturation: w.comment_saturation,
        read_halflife_days: w.read_halflife_days,
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct ResurfaceParams {
    pub limit: Option<u32>,
}

pub async fn list(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<crate::middleware::Identity>,
    Path(kb): Path<String>,
    Query(params): Query<ResurfaceParams>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) as usize;
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let items = match compute_items(
        &state,
        &kb_name,
        ctx,
        limit,
        now_unix,
        identity.user.clone(),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };

    Json(ResurfaceResponse {
        items,
        now_unix,
        weights: weights_wire(&ctx.resurface),
    })
    .into_response()
}

/// Shared body of [`list`], factored out so `routes::daycard` (the e-ink
/// daycard's "worth picking back up" section) can reuse the EXACT SAME
/// candidate gathering + scoring rather than a second private aggregation.
/// Takes an injected `now_unix` (mirroring `kb_core::resurface::rank`'s own
/// injected-clock contract), so a caller building a deterministic
/// per-day document can pass a fixed clock instead of wall time.
pub(crate) async fn compute_items(
    state: &KbHandles,
    kb_name: &kb_core::types::KbName,
    ctx: &crate::state::KbContext,
    limit: usize,
    now_unix: i64,
    user: String,
) -> Result<Vec<ResurfaceItem>, kb_core::Error> {
    // Signal 1 — open comments, via the SHARED inbox collector
    // (`inbox::collect_open`: `.review/` walk + JSON parses on ONE
    // `spawn_blocking` off the async worker, zero-open files skipped, one
    // batched lance lookup) — never a second private dir walk (the N+1 class
    // the perf sweep removed; itm-resurface-comments-walk-perf). Best-effort
    // like the inbox: a missing dir or malformed file contributes nothing.
    // The per-comment rows collapse to per-artifact (count, oldest-open),
    // exactly the golden-pinned scoring inputs.
    let review_dir = state.paths.kb_review_dir(kb_name);
    let open_rows = super::inbox::collect_open(kb_name.as_str(), ctx, &review_dir).await;
    let open_comments = aggregate_open(&open_rows);

    // Signal 2 — unfinished reads: the batched rollup's InProgress rows
    // (opened, < FULLY_READ_PCT, no override) are exactly the candidate set.
    // v0.34 Y1 — requester's user.
    let rollup = ctx.storage.reading_rollup(user).await?;

    // Doc metadata (facets pattern): title, source-rel, word_count, and the
    // memory-lifecycle exclusion. Also drops candidates whose review file /
    // history rows outlived the artifact. PF-R1 — reuses the per-(kb,
    // generation) gallery row-set memo (invariant #15) instead of an
    // independent `list_docs(u32::MAX)` actor round-trip; `compute_items`
    // is also called from `routes::daycard`'s day-mode "worth picking back
    // up" section, so both callers now share the SAME warm scan.
    let (rows, _edge_counts) = crate::routes::docs::gallery_snapshot(ctx).await?;
    let mut meta: HashMap<String, (String, String, Option<u32>)> = HashMap::new();
    for row in rows.iter() {
        let doc = &row.doc;
        // Memories + session transcripts have their own attention economies
        // (recall / the sessions worklog); the reading queue excludes them.
        if doc
            .kb_category
            .as_deref()
            .is_some_and(|c| c.starts_with("memory"))
        {
            continue;
        }
        let rel = kb_core::paths::doc_rel_path(&doc.path, &ctx.source_path);
        meta.insert(doc.id.clone(), (doc.title.clone(), rel, doc.word_count));
    }

    let mut candidates: HashMap<String, ResurfaceSignals> = HashMap::new();
    for (id, (count, oldest)) in &open_comments {
        if !meta.contains_key(id) {
            continue;
        }
        candidates
            .entry(id.clone())
            .or_insert_with(|| blank(id))
            .open_comments = *count;
        candidates
            .get_mut(id)
            .expect("just inserted")
            .oldest_open_unix = Some(*oldest);
    }
    for (id, r) in &rollup {
        if r.state != ReadState::InProgress || !meta.contains_key(id) {
            continue;
        }
        let c = candidates.entry(id.clone()).or_insert_with(|| blank(id));
        c.completion_pct = Some(r.completion_pct);
        c.last_opened_unix = r.last_opened_unix;
    }

    let mut ranked = rank(candidates.into_values().collect(), now_unix, &ctx.resurface);
    ranked.truncate(limit);

    let items: Vec<ResurfaceItem> = ranked
        .into_iter()
        .filter_map(|s| {
            let (title, source_relative, word_count) = meta.get(&s.signals.artifact_id)?.clone();
            let est_min_left = match (s.signals.completion_pct, word_count) {
                (Some(pct), Some(words)) if words > 0 => {
                    let remaining = words.saturating_mul(u32::from(100 - pct.min(100))) / 100;
                    Some(kb_core::lists::est_minutes(remaining))
                }
                _ => None,
            };
            let reasons = build_reasons(&s.signals, est_min_left, now_unix);
            Some(ResurfaceItem {
                id: s.signals.artifact_id.clone(),
                title,
                source_relative,
                score: s.score,
                comment_term: s.comment_term,
                read_term: s.read_term,
                open_comments: s.signals.open_comments,
                oldest_open_unix: s.signals.oldest_open_unix,
                completion_pct: s.signals.completion_pct,
                last_opened_unix: s.signals.last_opened_unix,
                est_min_left,
                reasons,
            })
        })
        .collect();

    Ok(items)
}

/// Collapse the inbox's per-open-comment rows into the resurface signal
/// shape: artifact id → (open count, oldest `created_at`). Pure — the seam
/// between the shared collector and the golden-pinned scoring inputs
/// (`0.6·comments + 0.4·progress·decay` never sees individual rows).
fn aggregate_open(rows: &[super::inbox::InboxItem]) -> HashMap<String, (u32, i64)> {
    let mut out: HashMap<String, (u32, i64)> = HashMap::new();
    for r in rows {
        let e = out
            .entry(r.artifact_id.clone())
            .or_insert((0, r.created_at));
        e.0 += 1;
        e.1 = e.1.min(r.created_at);
    }
    out
}

fn blank(id: &str) -> ResurfaceSignals {
    ResurfaceSignals {
        artifact_id: id.to_string(),
        open_comments: 0,
        oldest_open_unix: None,
        completion_pct: None,
        last_opened_unix: None,
    }
}

/// Deterministic English reason strings — one per active signal, comment
/// debt first (it carries the higher weight).
fn build_reasons(s: &ResurfaceSignals, est_min_left: Option<u32>, now_unix: i64) -> Vec<String> {
    let mut out = Vec::new();
    if s.open_comments > 0 {
        let noun = if s.open_comments == 1 {
            "open comment"
        } else {
            "open comments"
        };
        match s.oldest_open_unix {
            Some(t) => out.push(format!(
                "{} {} (oldest {})",
                s.open_comments,
                noun,
                fmt_age(t, now_unix)
            )),
            None => out.push(format!("{} {}", s.open_comments, noun)),
        }
    }
    if let (Some(pct), Some(opened)) = (s.completion_pct, s.last_opened_unix) {
        let mut r = format!("read {}% · {}", pct, fmt_age(opened, now_unix));
        if let Some(m) = est_min_left {
            if m > 0 {
                r.push_str(&format!(" · ~{m} min left"));
            }
        }
        out.push(r);
    }
    out
}

/// `today` under a day, else `{n}d ago`.
fn fmt_age(unix: i64, now_unix: i64) -> String {
    let days = (now_unix - unix).max(0) / 86_400;
    if days == 0 {
        "today".to_string()
    } else {
        format!("{days}d ago")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(artifact: &str, created: i64) -> crate::routes::inbox::InboxItem {
        crate::routes::inbox::InboxItem {
            kb: "k".into(),
            artifact_id: artifact.into(),
            source_relative: None,
            title: "t".into(),
            comment_id: format!("c-{artifact}-{created}"),
            excerpt: String::new(),
            author: "you".into(),
            reply_count: 0,
            anchor: "file".into(),
            stale: false,
            created_at: created,
            updated_at: created,
        }
    }

    /// The inbox→resurface seam: per-open-comment rows collapse to the
    /// per-artifact (count, oldest created) pair the scorer is golden-pinned
    /// against — count counts EVERY open row, oldest is the min created_at
    /// regardless of row order.
    #[test]
    fn aggregate_open_collapses_rows_to_count_and_oldest() {
        let rows = vec![
            row("aaa111", 100),
            row("bbb222", 55),
            row("aaa111", 40),
            row("aaa111", 70),
        ];
        let agg = aggregate_open(&rows);
        assert_eq!(agg.len(), 2);
        assert_eq!(agg["aaa111"], (3, 40));
        assert_eq!(agg["bbb222"], (1, 55));
        assert!(aggregate_open(&[]).is_empty());
    }

    #[test]
    fn reasons_cover_both_signals_and_age_grammar() {
        let now = 1_780_000_000;
        let s = ResurfaceSignals {
            artifact_id: "abc123def456".into(),
            open_comments: 2,
            oldest_open_unix: Some(now - 12 * 86_400),
            completion_pct: Some(62),
            last_opened_unix: Some(now - 5 * 86_400),
        };
        let reasons = build_reasons(&s, Some(4), now);
        assert_eq!(
            reasons,
            vec![
                "2 open comments (oldest 12d ago)".to_string(),
                "read 62% · 5d ago · ~4 min left".to_string(),
            ]
        );
        let fresh = ResurfaceSignals {
            open_comments: 1,
            oldest_open_unix: Some(now - 3600),
            completion_pct: None,
            last_opened_unix: None,
            ..s
        };
        assert_eq!(
            build_reasons(&fresh, None, now),
            vec!["1 open comment (oldest today)"]
        );
    }

    // W2.9 — the wire projection must mirror a NON-default resolved
    // weights struct exactly (score_floor deliberately excluded — see the
    // `ResurfaceResponse::weights` doc comment).
    #[test]
    fn weights_wire_mirrors_resolved_weights() {
        let w = kb_core::resurface::ResurfaceWeights {
            comment_weight: 1.2,
            read_weight: 0.8,
            comment_saturation: 6,
            read_halflife_days: 30.0,
            score_floor: 0.1,
        };
        let wire = weights_wire(&w);
        assert_eq!(wire.comment_weight, 1.2);
        assert_eq!(wire.read_weight, 0.8);
        assert_eq!(wire.comment_saturation, 6);
        assert_eq!(wire.read_halflife_days, 30.0);
    }
}
