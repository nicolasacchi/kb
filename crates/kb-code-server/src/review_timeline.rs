//! PRR-R4 ("The PR Room," kb v0.39 T2, Phase 4) — `GET
//! /api/reviews/{id}/timeline` (milestone plan arbitration #7: "pure
//! composition of existing rows, no storage"). Bearer — same ordinary
//! review-read gate as `/comments`/`/distill`/`/findings`/`/pr-status`.
//!
//! [`compose_review_timeline`] is the pure core: every event is derived
//! from a row this crate already persists for OTHER reasons (the review
//! itself, its PR binding, its patchsets, its findings, its review-scoped
//! annotations, its verdict-publish record) — nothing new is written, and
//! re-composing the SAME review at the SAME state yields the SAME sequence
//! (the same determinism guarantee `review_distill` already gives its own
//! consumers).
//!
//! # Event kinds (closed vocab, this module's own — NOT `review.changed`'s
//! `reason` strings, though several names intentionally line up)
//!
//! `review_created` · `pr_bound` · `patchset` · `findings_import` (a real
//! `findings/import` batch, grouped by `import_batch_id`) ·
//! `finding_added` (a single `origin="manual"` finding — see below for why
//! these are NOT grouped like `findings_import`) · `disposition` ·
//! `verdict` · `finding_published` · `verdict_published` · `comment`
//! (review-scoped annotation/reply activity that ISN'T a finding's own
//! top-level row).
//!
//! # Why manual findings get their OWN event, never grouped
//!
//! The milestone brief's own wording is "group `review_findings` by
//! `import_batch_id`: 'N findings imported.'" Taken completely literally,
//! this would also group every `origin="manual"` finding — because
//! `review_findings::create_manual_finding_route` stamps EVERY manual
//! finding with the exact same literal `import_batch_id = "manual"`
//! (`store::NewReviewFinding` docs this; it is not a per-call unique id the
//! way a real import batch's `format!("batch_{}", short_random_hex())` is).
//! Grouping on that literal would collapse every manually-added finding
//! across the review's ENTIRE lifetime into one fake "N findings imported"
//! event, timestamped at whichever row's `created_at` a `BTreeMap` happens
//! to keep — silently wrong for a timeline, whose whole point is WHEN
//! things happened. This module instead groups ONLY `origin="import"` rows
//! by `import_batch_id` (accurate: every row created by one real import
//! call shares the exact same `created_at`, stamped once by
//! `Store::reconcile_findings_import`'s single `now` argument) and emits
//! one INDIVIDUAL `finding_added` event per `origin="manual"` row instead.
//! A deliberate, documented deviation from the brief's literal wording —
//! flagged here and in this unit's own report.
//!
//! # Ordering
//!
//! Events are built in a canonical, already-deterministic order (one
//! source at a time, each source's own query already `ORDER BY`'d or
//! iterated in a fixed sequence) and then STABLE-sorted by `at` ascending
//! (`Vec::sort_by_key` is stable) — so two events sharing the same `at`
//! keep their construction-order relative position, itself deterministic,
//! rather than needing a second explicit tiebreak key.

use crate::reviews::require_review;
use crate::routes::ApiError;
use crate::state::SharedState;
use crate::store;
use crate::store::StoreBlocking;
use axum::extract::{Path as AxumPath, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use std::collections::{BTreeMap, HashSet};

pub const SCHEMA: &str = "review-timeline/1";

/// Pure composition — see the module doc. Takes every already-fetched row
/// this event stream is derived from, so it is unit-testable with fixed
/// fixture rows and no repo/daemon at all.
pub(crate) fn compose_review_timeline(
    review: &store::ReviewRow,
    binding: &store::ReviewPrBinding,
    patchsets: &[store::ReviewPatchsetRow],
    findings: &[store::ReviewFindingRow],
    annotations: &[store::AnnotationRow],
    verdict_published: Option<(Option<i64>, Option<String>)>,
) -> Vec<serde_json::Value> {
    let mut events: Vec<serde_json::Value> = Vec::new();

    events.push(serde_json::json!({
        "at": review.created_at,
        "kind": "review_created",
        "review_id": review.id,
        "repo": review.repo,
    }));

    if let Some(fetched_at) = binding.pr_meta_fetched_at {
        events.push(serde_json::json!({
            "at": fetched_at,
            "kind": "pr_bound",
            "pr_number": binding.pr_number,
            "pr_repo_slug": binding.pr_repo_slug,
        }));
    }

    for ps in patchsets {
        events.push(serde_json::json!({
            "at": ps.captured_at,
            "kind": "patchset",
            "ps_number": ps.ps_number,
            "tip_sha": ps.tip_sha,
        }));
    }

    // Findings — see the module doc's "manual findings get their OWN
    // event" section for why `origin="import"`/`"manual"` are split.
    let mut import_batches: BTreeMap<&str, Vec<&store::ReviewFindingRow>> = BTreeMap::new();
    for f in findings {
        if f.origin == store::FINDING_ORIGIN_MANUAL {
            events.push(serde_json::json!({
                "at": f.created_at,
                "kind": "finding_added",
                "slug": f.slug,
                "title": f.title,
            }));
        } else {
            import_batches
                .entry(f.import_batch_id.as_str())
                .or_default()
                .push(f);
        }
    }
    for (batch_id, batch_findings) in &import_batches {
        // Every row in one real import call shares the exact same
        // `created_at` (see the module doc) — `min` is defensive, not
        // load-bearing, in case that invariant is ever loosened.
        let at = batch_findings
            .iter()
            .map(|f| f.created_at)
            .min()
            .unwrap_or(0);
        let mut slugs: Vec<&str> = batch_findings.iter().map(|f| f.slug.as_str()).collect();
        slugs.sort_unstable();
        events.push(serde_json::json!({
            "at": at,
            "kind": "findings_import",
            "import_batch_id": batch_id,
            "count": batch_findings.len(),
            "slugs": slugs,
        }));
    }

    for f in findings {
        if let Some(at) = f.disposition_at {
            events.push(serde_json::json!({
                "at": at,
                "kind": "disposition",
                "slug": f.slug,
                "state": f.disposition,
                "by": f.disposition_by,
            }));
        }
        if let Some(at) = f.published_at {
            events.push(serde_json::json!({
                "at": at,
                "kind": "finding_published",
                "slug": f.slug,
                "url": f.published_url,
            }));
        }
    }

    if let Some(at) = review.verdict_at {
        events.push(serde_json::json!({
            "at": at,
            "kind": "verdict",
            "state": review.verdict,
        }));
    }

    if let Some((Some(at), url)) = verdict_published {
        events.push(serde_json::json!({
            "at": at,
            "kind": "verdict_published",
            "url": url,
        }));
    }

    // Comment/reply activity — every review-scoped annotation that is NOT
    // itself a finding's top-level row (findings already got their own
    // `finding_added`/`findings_import` event above); a REPLY on a
    // finding's thread DOES count here — that's the Q&A conversation,
    // distinct from the finding's own lifecycle.
    let finding_annotation_ids: HashSet<&str> =
        findings.iter().map(|f| f.annotation_id.as_str()).collect();
    for a in annotations {
        let is_finding_top_level =
            a.parent_id.is_none() && finding_annotation_ids.contains(a.id.as_str());
        if is_finding_top_level {
            continue;
        }
        events.push(serde_json::json!({
            "at": a.created_at,
            "kind": "comment",
            "annotation_id": a.id,
            "path": a.path,
            "intent": a.intent,
            "author": a.author,
            "is_reply": a.parent_id.is_some(),
        }));
    }

    events.sort_by_key(|e| e["at"].as_i64().unwrap_or(i64::MAX));
    events
}

/// `GET /api/reviews/{id}/timeline` (arbitration #7). Bearer. `include_
/// superseded=true`/`include_resolved=true` equivalents are NOT params
/// here — a timeline is a HISTORY, so every row this review has ever
/// touched is always included (a superseded finding's OWN `findings_
/// import`/`finding_added`/`disposition` events still happened; hiding
/// them would make the timeline lie about the past).
pub async fn review_timeline_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let (review, _repo, _) = require_review(&state, id).await?;
    // 2026-08-31 incident (store.rs module doc): five independent reads,
    // all pure composition inputs — one blocking-pool trip.
    let (binding, patchsets, findings, annotations, verdict_published) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let binding = store.get_review_pr_binding(id)?.unwrap_or_default();
            let patchsets = store.list_patchsets(id)?;
            let findings = store.list_review_findings(id, None, true)?;
            let annotations = store.list_review_annotations(id, true)?;
            let verdict_published = store.get_review_verdict_published(id)?;
            Ok((binding, patchsets, findings, annotations, verdict_published))
        })
        .await?;

    let events = compose_review_timeline(
        &review,
        &binding,
        &patchsets,
        &findings,
        &annotations,
        verdict_published,
    );

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": SCHEMA,
            "review_id": id,
            "repo": review.repo,
            "events": events,
        })),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_review() -> store::ReviewRow {
        store::ReviewRow {
            id: 1,
            repo: "r".into(),
            title: Some("t".into()),
            base_ref: "main".into(),
            head_ref: "feature".into(),
            session_id: None,
            state: "open".into(),
            created_at: 100,
            updated_at: 100,
            verdict: None,
            verdict_note: None,
            verdict_at: None,
            verdict_ps: None,
        }
    }

    fn ps(ps_number: i64, captured_at: i64) -> store::ReviewPatchsetRow {
        store::ReviewPatchsetRow {
            id: ps_number,
            review_id: 1,
            ps_number,
            tip_sha: format!("sha{ps_number}"),
            base_sha: "base".into(),
            captured_at,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn finding(
        slug: &str,
        origin: &str,
        import_batch_id: &str,
        created_at: i64,
        disposition: Option<(&str, i64)>,
        published_at: Option<i64>,
    ) -> store::ReviewFindingRow {
        store::ReviewFindingRow {
            id: 0,
            review_id: 1,
            annotation_id: format!("ann-{slug}"),
            slug: slug.to_string(),
            severity: "concern".into(),
            category: "cat".into(),
            location_kind: "whole_file".into(),
            location_path: "a.rb".into(),
            location_lines: None,
            location_removed: false,
            title: format!("title-{slug}"),
            rationale: "r".into(),
            recommendation: None,
            evidence_lang: None,
            evidence_source: None,
            origin: origin.to_string(),
            author: Some("claude".into()),
            disposition: disposition.map(|(s, _)| s.to_string()),
            disposition_note: None,
            disposition_by: disposition.map(|_| "you".to_string()),
            disposition_at: disposition.map(|(_, at)| at),
            content_updated_at: None,
            published_state: if published_at.is_some() {
                "published".into()
            } else {
                "unpublished".into()
            },
            published_at,
            published_url: published_at.map(|_| "https://github.com/x/y/pull/1".to_string()),
            superseded: false,
            superseded_at: None,
            superseded_reason: None,
            import_batch_id: import_batch_id.to_string(),
            created_at,
            updated_at: created_at,
            act: "issue".into(),
            blocking: false,
            cites_json: None,
            fingerprint: None,
            superseded_by: None,
        }
    }

    fn note(
        id: &str,
        parent_id: Option<&str>,
        intent: &str,
        created_at: i64,
    ) -> store::AnnotationRow {
        store::AnnotationRow {
            id: id.to_string(),
            repo_id: 1,
            path: "a.rb".into(),
            anchor: parent_id.is_none().then(|| "{}".to_string()),
            anchor_kind: "whole_file".into(),
            anchor2: None,
            parent_id: parent_id.map(|s| s.to_string()),
            intent: intent.to_string(),
            body: "hi".into(),
            author: "you".into(),
            created_at,
            updated_at: created_at,
            resolved: false,
            review_id: Some(1),
            ps_number: Some(1),
            side: None,
            set_id: None,
        }
    }

    #[test]
    fn empty_review_has_only_the_review_created_event() {
        let review = base_review();
        let binding = store::ReviewPrBinding::default();
        let events = compose_review_timeline(&review, &binding, &[], &[], &[], None);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "review_created");
        assert_eq!(events[0]["at"], 100);
    }

    /// Fixed-clock fixture rows → an EXACT event sequence (VERIFY plan's
    /// own ask), covering every kind in one composition.
    #[test]
    fn composes_every_kind_in_ascending_at_order() {
        let mut review = base_review();
        review.verdict = Some("approve".into());
        review.verdict_at = Some(500);

        let binding = store::ReviewPrBinding {
            pr_number: Some(42),
            pr_repo_slug: Some("acme/widget".into()),
            pr_meta_fetched_at: Some(150),
            ..store::ReviewPrBinding::default()
        };

        let patchsets = vec![ps(1, 110), ps(2, 300)];

        let findings = vec![
            // A real import batch: two findings, same batch id + same
            // created_at (as `reconcile_findings_import` guarantees).
            finding("f-a", "import", "batch_1", 200, None, None),
            finding("f-b", "import", "batch_1", 200, Some(("agree", 250)), None),
            // A manual finding — its OWN event, not folded into a "manual"
            // batch group.
            finding("f-c", "manual", "manual", 220, None, Some(400)),
        ];

        let annotations = vec![
            // A finding's own top-level annotation row — must NOT surface
            // as a separate "comment" event (it's already `finding_added`/
            // `findings_import`).
            note("ann-f-a", None, "finding", 200),
            // A reply on that finding's thread — DOES count as "comment".
            note("reply-1", Some("ann-f-a"), "question", 260),
            // An ordinary review-level note.
            note("note-1", None, "note", 130),
        ];

        let events = compose_review_timeline(
            &review,
            &binding,
            &patchsets,
            &findings,
            &annotations,
            Some((
                Some(450),
                Some("https://github.com/x/y/pull/1#review-1".to_string()),
            )),
        );

        let kinds_and_at: Vec<(String, i64)> = events
            .iter()
            .map(|e| {
                (
                    e["kind"].as_str().unwrap().to_string(),
                    e["at"].as_i64().unwrap(),
                )
            })
            .collect();

        assert_eq!(
            kinds_and_at,
            vec![
                ("review_created".to_string(), 100),
                ("patchset".to_string(), 110),
                ("comment".to_string(), 130),
                ("pr_bound".to_string(), 150),
                ("findings_import".to_string(), 200),
                ("finding_added".to_string(), 220),
                ("disposition".to_string(), 250),
                ("comment".to_string(), 260),
                ("patchset".to_string(), 300),
                ("finding_published".to_string(), 400),
                ("verdict_published".to_string(), 450),
                ("verdict".to_string(), 500),
            ]
        );

        // Spot-check a few payloads.
        let import_evt = events
            .iter()
            .find(|e| e["kind"] == "findings_import")
            .unwrap();
        assert_eq!(import_evt["import_batch_id"], "batch_1");
        assert_eq!(import_evt["count"], 2);
        assert_eq!(import_evt["slugs"], serde_json::json!(["f-a", "f-b"]));

        let manual_evt = events
            .iter()
            .find(|e| e["kind"] == "finding_added")
            .unwrap();
        assert_eq!(manual_evt["slug"], "f-c");

        let comment_evts: Vec<&serde_json::Value> =
            events.iter().filter(|e| e["kind"] == "comment").collect();
        assert_eq!(
            comment_evts.len(),
            2,
            "the finding's own top-level row must be excluded"
        );
        assert!(comment_evts.iter().any(|e| e["annotation_id"] == "note-1"));
        assert!(comment_evts
            .iter()
            .any(|e| e["annotation_id"] == "reply-1" && e["is_reply"] == true));
    }

    #[test]
    fn manual_findings_never_collapse_into_one_shared_batch_event() {
        let review = base_review();
        let binding = store::ReviewPrBinding::default();
        let findings = vec![
            finding("f-a", "manual", "manual", 100, None, None),
            finding("f-b", "manual", "manual", 999, None, None),
        ];
        let events = compose_review_timeline(&review, &binding, &[], &findings, &[], None);
        let finding_added: Vec<&serde_json::Value> = events
            .iter()
            .filter(|e| e["kind"] == "finding_added")
            .collect();
        assert_eq!(
            finding_added.len(),
            2,
            "each manual finding gets its OWN event"
        );
        assert!(finding_added
            .iter()
            .any(|e| e["at"] == 100 && e["slug"] == "f-a"));
        assert!(finding_added
            .iter()
            .any(|e| e["at"] == 999 && e["slug"] == "f-b"));
        assert!(
            events.iter().all(|e| e["kind"] != "findings_import"),
            "no manual finding should ever produce a findings_import event"
        );
    }

    #[test]
    fn recomposing_the_same_state_is_byte_identical() {
        let review = base_review();
        let binding = store::ReviewPrBinding::default();
        let patchsets = vec![ps(1, 110), ps(2, 110)]; // tie on `at`
        let a = compose_review_timeline(&review, &binding, &patchsets, &[], &[], None);
        let b = compose_review_timeline(&review, &binding, &patchsets, &[], &[], None);
        assert_eq!(a, b);
    }
}
