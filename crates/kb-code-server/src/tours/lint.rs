//! `kbc-tour/1`'s lint — the BOARD lint, plus four tour-only rules.
//!
//! There is no second lint here, and that is the point of D10's "do not
//! build two". [`check`] lowers the tour to a board
//! ([`super::to_board_doc`]) and hands it to
//! [`crate::boards::lint::check`], which owns every rule about slugs,
//! titles, caps, node kinds, reference fields, step order, connectivity and
//! COORDINATES. What this module adds are the four things a board document
//! cannot express:
//!
//! 1. the tour's own `schema` string,
//! 2. the tour step cap ([`super::MAX_STEPS`], tighter than a board's node
//!    cap because a tour is walked one stop at a time by a human),
//! 3. the `ref`-sugar lowering failures (already collected by
//!    `to_board_doc`, surfaced here as ordinary findings), and
//! 4. the camera's own bounds.
//!
//! Severity means what it means on a board (invariant 24(d)): `refuse` is
//! "I cannot read this", `warn` is "I read this and it points at something
//! that is gone". The report is a LIST, never a first error, because an
//! agent retries the whole document.

pub use crate::boards::lint::{Finding, Report, Severity};

use super::{Camera, TourDoc, MAX_CAMERA_CONTEXT, MAX_STEPS, SCHEMA};
use crate::boards::lint as board_lint;

/// The whole lint. PURE: no store, no filesystem, no clock — the semantic
/// half (does this reference resolve TODAY) is `boards::resolve`'s, run
/// afterwards and reported as warnings, exactly as it is for a board.
pub fn check(doc: &TourDoc) -> Report {
    let mut findings: Vec<Finding> = Vec::new();

    if doc.schema != SCHEMA {
        findings.push(Finding::refuse(
            "schema",
            None,
            format!("schema must be {SCHEMA:?}, got {:?}", doc.schema),
        ));
    }
    if doc.steps.is_empty() {
        findings.push(Finding::refuse(
            "steps-required",
            None,
            "a tour with no steps is not a tour — every tour declares at least one stop",
        ));
    }
    if doc.steps.len() > MAX_STEPS {
        findings.push(Finding::refuse(
            "step-cap",
            None,
            format!(
                "{} steps; the cap is {MAX_STEPS}. A tour is walked one stop at a time by \
                 a human — split it, or make it a board (which caps at {} nodes)",
                doc.steps.len(),
                crate::boards::MAX_NODES
            ),
        ));
    }
    for s in &doc.steps {
        if let Some(c) = &s.camera {
            findings.extend(check_camera(&s.id, c));
        }
    }

    // The lowering collects every `ref`-sugar failure at once. When it
    // fails there is no board to lint, so those findings ARE the report —
    // plus whatever the tour-only rules above already found.
    let board = match super::to_board_doc(doc) {
        Ok(b) => b,
        Err(mut errs) => {
            findings.append(&mut errs);
            return Report {
                findings,
                components: Vec::new(),
            };
        }
    };

    // `allow_disconnected` is deliberately NOT set: a tour's generated
    // `then` chain makes it genuinely one component, so the board rule
    // passes on its merits rather than by suppression.
    let mut board_report = board_lint::check(&board, board_lint::Opts::default());
    findings.append(&mut board_report.findings);
    Report {
        findings,
        components: board_report.components,
    }
}

fn check_camera(step_id: &str, c: &Camera) -> Vec<Finding> {
    let mut out = Vec::new();
    if let Some(ctx) = c.context {
        if ctx > MAX_CAMERA_CONTEXT {
            out.push(Finding::refuse(
                "camera",
                Some(step_id.to_string()),
                format!(
                    "camera.context is {ctx}; the cap is {MAX_CAMERA_CONTEXT}. A camera \
                     is a hint about what to REVEAL around a step, not a way to open the \
                     whole file"
                ),
            ));
        }
    }
    if c.fold.is_none() && c.context.is_none() {
        out.push(Finding::warn(
            "camera",
            Some(step_id.to_string()),
            "an empty camera says nothing — drop it, or set `fold` and/or `context`",
        ));
    }
    out
}

/// The one-line refusal an HTTP 400 / a CLI exit carries — the tour's own
/// wording, because `Report::refusal_summary` says "canvas apply".
pub fn refusal_summary(report: &Report) -> String {
    let msgs: Vec<String> = report
        .refusals()
        .map(|f| match &f.at {
            Some(at) => format!("[{}] {at}: {}", f.rule, f.message),
            None => format!("[{}] {}", f.rule, f.message),
        })
        .collect();
    format!(
        "tour apply refused by {} lint rule(s): {}",
        msgs.len(),
        msgs.join(" · ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boards::RefFields;
    use crate::tours::TourStepIn;

    fn step(id: &str, r: &str) -> TourStepIn {
        TourStepIn {
            id: id.into(),
            title: Some("T".into()),
            body_md: None,
            ref_str: Some(r.into()),
            kind: None,
            thread_id: None,
            camera: None,
            reference: RefFields::default(),
        }
    }

    fn doc(steps: Vec<TourStepIn>) -> TourDoc {
        TourDoc {
            schema: SCHEMA.into(),
            repo: "r".into(),
            slug: "t".into(),
            title: "T".into(),
            description_md: String::new(),
            status: None,
            authored_ref: None,
            steps,
        }
    }

    fn rules(r: &Report) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = r.refusals().map(|f| f.rule).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    #[test]
    fn a_well_formed_tour_passes_with_no_relaxation() {
        let r = check(&doc(vec![
            step("s1", "code:a.rb:1-5@abc1234"),
            step("s2", "code:b.rb:9"),
        ]));
        assert!(!r.refused(), "{:#?}", r.findings);
        assert_eq!(r.components.len(), 1);
    }

    #[test]
    fn the_tour_only_rules_fire_and_keep_their_own_ids() {
        let mut d = doc(vec![step("s1", "code:a.rb:1")]);
        d.schema = "kbc-canvas/1".into();
        assert!(rules(&check(&d)).contains(&"schema"));

        assert!(rules(&check(&doc(Vec::new()))).contains(&"steps-required"));

        let many: Vec<TourStepIn> = (0..MAX_STEPS + 1)
            .map(|i| step(&format!("s{i}"), "code:a.rb:1"))
            .collect();
        assert!(rules(&check(&doc(many))).contains(&"step-cap"));

        let mut cam = doc(vec![step("s1", "code:a.rb:1")]);
        cam.steps[0].camera = Some(Camera {
            fold: None,
            context: Some(MAX_CAMERA_CONTEXT + 1),
        });
        assert!(rules(&check(&cam)).contains(&"camera"));
    }

    #[test]
    fn an_empty_camera_warns_rather_than_refusing() {
        let mut d = doc(vec![step("s1", "code:a.rb:1")]);
        d.steps[0].camera = Some(Camera::default());
        let r = check(&d);
        assert!(
            !r.refused(),
            "an empty camera is noise, not a malformed document"
        );
        assert!(r.warnings().any(|f| f.rule == "camera"));
    }

    #[test]
    fn the_board_lint_still_owns_every_rule_it_already_owned() {
        // A duplicate step id is a BOARD rule (`node-id`), reached through
        // the lowering — proof that this module adds rules rather than
        // re-implementing them.
        let r = check(&doc(vec![
            step("same", "code:a.rb:1"),
            step("same", "code:b.rb:2"),
        ]));
        assert!(rules(&r).contains(&"node-id"), "{:#?}", r.findings);

        // …and so is the slug rule.
        let mut d = doc(vec![step("s1", "code:a.rb:1")]);
        d.slug = "Not A Slug".into();
        assert!(rules(&check(&d)).contains(&"slug"));

        // …and the coordinate refusal is reached by the RAW pre-pass, which
        // a tour apply runs unchanged — see `routes::apply_tour`.
        let raw = serde_json::json!({"steps": [{"id": "s1", "x": 10}]});
        assert!(board_lint::precheck_raw(&raw)
            .iter()
            .any(|f| f.rule == "coordinates"));
    }

    #[test]
    fn a_lowering_failure_reports_every_bad_step_at_once() {
        let r = check(&doc(vec![
            step("s1", "sym:Order#total"),
            step("s2", "gh:pr/1"),
            step("s3", "code:a.rb"),
        ]));
        assert!(r.refused());
        assert_eq!(
            r.refusals().filter(|f| f.rule == "step-ref").count(),
            3,
            "an agent retrying an apply needs every problem, not the first"
        );
        assert!(refusal_summary(&r).starts_with("tour apply refused by 3"));
    }
}
