//! `tour export --format md|codetour` (V74-L3b, D12).
//!
//! # Both formats are SNAPSHOTS, and both say so
//!
//! A live tour re-resolves every step through the Ladder on each read. An
//! exported file cannot: it is bytes on a disk from the moment it is
//! written. `boards::export`'s rule applies unchanged — every format states
//! in its own payload that it is a snapshot, with the revision and the
//! resolution counts it was taken at.
//!
//! # `codetour`, and exactly what it loses
//!
//! [CodeTour](https://marketplace.visualstudio.com/items?itemName=vsls-contrib.codetour)
//! is a VS Code extension with a `.tour` JSON format. Exporting to it is
//! genuinely useful (a colleague can walk the tour in an editor they
//! already have) and it is genuinely LOSSY, so the export names every loss
//! in a `kbc_lossy` array INSIDE the file rather than in a doc nobody will
//! re-read:
//!
//! | dropped | why |
//! |---|---|
//! | the blob per step | CodeTour anchors on a line number or a text `pattern`; it has no notion of the bytes an author read, so the Ladder's whole witness is gone |
//! | the resolution state | there is no `pinned`/`carried`/`orphan` in that format; every step reads as if it were fresh |
//! | the camera | no fold/context hint exists in the schema |
//! | every non-`code` step | a finding, a hunk, an annotation or a session turn has no file+line to point at |
//!
//! Rather than invent fields, the export puts what it cannot express in the
//! step's own `description` text (where a human will read it) and lists it
//! in `kbc_lossy` (where a tool can). Namespaced `kbc_*` keys are
//! `boards::export`'s JSON-Canvas convention, second instance.

use super::pack::render_step;
use super::routes::TourOut;

/// The formats `?format=` accepts.
pub const FORMATS: [&str; 2] = ["md", "codetour"];

/// The vocabulary check the ROUTE runs — `pub` so `kb-code tour export`
/// reuses it rather than keeping a second copy that could drift
/// (`boards::export::is_valid_format`'s precedent).
pub fn is_valid_format(s: &str) -> bool {
    FORMATS.contains(&s)
}

/// Render a resolved tour. Returns `(content type, body)`.
pub fn render(tour: &TourOut, format: &str) -> (&'static str, String) {
    match format {
        "codetour" => ("application/json; charset=utf-8", codetour(tour)),
        // `md` is the default for an unknown value only because the route
        // validates `format` against `FORMATS` first — this arm is
        // unreachable from HTTP.
        _ => ("text/markdown; charset=utf-8", markdown(tour)),
    }
}

/// The Markdown export — the SAME per-step rendering `tour pack` uses
/// ([`render_step`]), so a budgeted pack is literally a prefix of this
/// document rather than a second serialization of the same tour.
pub fn markdown(tour: &TourOut) -> String {
    let mut out = format!("# {}\n\n", tour.title);
    if !tour.description_md.trim().is_empty() {
        out.push_str(tour.description_md.trim());
        out.push_str("\n\n");
    }
    out.push_str(&format!(
        "> **Snapshot.** kbc-tour/1 `{}`, revision {}, exported from a live read. The \
         tour itself re-resolves every step through the Ladder on each read; this file \
         does not. At export time: {} pinned, {} carried, {} ORPHAN of {} steps.\n\n",
        tour.slug,
        tour.revision,
        tour.honesty.pinned,
        tour.honesty.carried,
        tour.honesty.orphans,
        tour.honesty.steps
    ));
    if let Some(r) = &tour.authored_ref {
        out.push_str(&format!(
            "_Authored at `{r}` (advisory context, never a resolution input)._\n\n"
        ));
    }
    for s in &tour.steps {
        out.push_str(&render_step(s));
    }
    out
}

/// The CodeTour export. Every loss is listed in `kbc_lossy`.
pub fn codetour(tour: &TourOut) -> String {
    let mut lossy: Vec<String> = vec![
        "the blob each step pinned — CodeTour anchors on a line or a text pattern, so the \
         Ladder's witness cannot be carried into this format"
            .to_string(),
        "each step's resolution state (pinned/carried/orphan) — this format has no way to \
         say a step's target moved"
            .to_string(),
    ];
    let mut steps = Vec::new();
    let mut skipped = 0usize;
    let mut had_camera = false;
    for s in &tour.steps {
        if s.camera.is_some() {
            had_camera = true;
        }
        let Some(code) = &s.node.code else {
            skipped += 1;
            continue;
        };
        let mut description = String::new();
        if let Some(t) = &s.node.title {
            description.push_str(&format!("**{t}**\n\n"));
        }
        if let Some(b) = &s.node.body_md {
            description.push_str(b.trim());
            description.push_str("\n\n");
        }
        description.push_str(&format!(
            "_(kb-code: {} — {})_",
            s.node.state, s.node.address
        ));
        steps.push(serde_json::json!({
            "file": code.path,
            "line": code.range[1],
            "description": description,
            "kbc_step_id": s.node.id,
            "kbc_state": s.node.state,
            "kbc_ref": s.ref_str,
        }));
    }
    if skipped > 0 {
        lossy.push(format!(
            "{skipped} step(s) that do not address a file and a line (a finding, a hunk, \
             an annotation, a session turn or a prose note) — CodeTour has nowhere to put \
             them, so they are omitted rather than approximated"
        ));
    }
    if had_camera {
        lossy.push(
            "every per-step camera (fold / context) — the CodeTour schema has no such hint"
                .to_string(),
        );
    }
    let doc = serde_json::json!({
        "$schema": "https://aka.ms/codetour-schema",
        "title": tour.title,
        "description": tour.description_md,
        "ref": tour.authored_ref,
        "steps": steps,
        "kbc_schema": super::SCHEMA,
        "kbc_slug": tour.slug,
        "kbc_revision": tour.revision,
        "kbc_snapshot": true,
        "kbc_snapshot_note":
            "a SNAPSHOT: the live tour re-resolves every step through kb-code's Ladder on \
             each read, this file does not",
        "kbc_lossy": lossy,
    });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boards::resolve::{honesty, CodeCard, NodeOut, STATE_PINNED};
    use crate::boards::RefFields;
    use crate::tours::{routes::TourStepOut, Camera};

    fn code_card() -> CodeCard {
        CodeCard {
            path: "app/a.rb".into(),
            symbol: None,
            range: [10, 12],
            context: None,
            authored_range: [10, 12],
            shifted_by: 0,
            authored_blob_sha: Some("abc1234".into()),
            current_blob_sha: Some("abc1234".into()),
            snippet: Some("def total\n  1\nend".into()),
            snippet_truncated: false,
            highlights: None,
            anchor_snippet: None,
            context_snippet: None,
        }
    }

    fn step(i: usize, with_code: bool, camera: Option<Camera>) -> TourStepOut {
        TourStepOut {
            ordinal: i,
            node: NodeOut {
                id: format!("s{i}"),
                kind: if with_code {
                    "code".into()
                } else {
                    "note".into()
                },
                title: Some("Where it starts".into()),
                body_md: Some("because the order is built here".into()),
                group: None,
                state: STATE_PINNED,
                reason: crate::boards::resolve::REASON_BLOB_CURRENT,
                address: "app/a.rb:10-12".into(),
                note: None,
                code: with_code.then(code_card),
                query: None,
                thread: None,
                pin: None,
                reference: RefFields::default(),
            },
            camera,
            ref_str: with_code.then(|| "code:app/a.rb:10-12@abc1234".to_string()),
        }
    }

    fn tour(steps: Vec<TourStepOut>) -> TourOut {
        let nodes: Vec<NodeOut> = steps.iter().map(|s| s.node.clone()).collect();
        let n = steps.len();
        TourOut {
            schema: crate::tours::SCHEMA,
            repo: "r".into(),
            slug: "checkout".into(),
            title: "Checkout".into(),
            description_md: "how an order is built".into(),
            status: "pending".into(),
            authored_ref: Some("main".into()),
            revision: 3,
            content_hash: "h".into(),
            created_unix: 0,
            updated_unix: 0,
            honesty: honesty(&nodes, 0, n, false, Vec::new()),
            steps,
        }
    }

    #[test]
    fn the_markdown_export_says_it_is_a_snapshot_and_reuses_the_pack_rendering() {
        let t = tour(vec![step(0, true, None)]);
        let md = markdown(&t);
        assert!(md.starts_with("# Checkout"));
        assert!(md.contains("**Snapshot.**"));
        assert!(md.contains("revision 3"));
        assert!(md.contains("advisory context"));
        // The per-step block is `pack::render_step`'s, verbatim.
        assert!(md.contains(&render_step(&t.steps[0])));
    }

    #[test]
    fn the_codetour_export_names_every_loss_inside_the_file() {
        let t = tour(vec![
            step(
                0,
                true,
                Some(Camera {
                    fold: Some(true),
                    context: None,
                }),
            ),
            step(1, false, None),
        ]);
        let v: serde_json::Value = serde_json::from_str(&codetour(&t)).expect("valid JSON");
        assert_eq!(v["$schema"], "https://aka.ms/codetour-schema");
        assert_eq!(v["title"], "Checkout");
        assert_eq!(v["ref"], "main");
        assert_eq!(v["kbc_snapshot"], true);
        let steps = v["steps"].as_array().expect("steps");
        assert_eq!(
            steps.len(),
            1,
            "the prose-only step has no file to point at"
        );
        assert_eq!(steps[0]["file"], "app/a.rb");
        assert_eq!(steps[0]["line"], 12);
        assert!(steps[0]["description"]
            .as_str()
            .expect("description")
            .contains("kb-code: pinned"));
        assert_eq!(steps[0]["kbc_ref"], "code:app/a.rb:10-12@abc1234");

        let lossy: Vec<&str> = v["kbc_lossy"]
            .as_array()
            .expect("lossy")
            .iter()
            .map(|s| s.as_str().unwrap_or_default())
            .collect();
        assert!(lossy.iter().any(|l| l.contains("blob")));
        assert!(lossy.iter().any(|l| l.contains("resolution state")));
        assert!(
            lossy.iter().any(|l| l.contains("1 step(s)")),
            "the omitted step must be COUNTED, not silently gone: {lossy:?}"
        );
        assert!(lossy.iter().any(|l| l.contains("camera")));
    }

    #[test]
    fn a_tour_with_no_code_steps_exports_an_empty_but_honest_codetour() {
        let v: serde_json::Value =
            serde_json::from_str(&codetour(&tour(vec![step(0, false, None)]))).expect("JSON");
        assert!(v["steps"].as_array().expect("steps").is_empty());
        assert!(v["kbc_lossy"]
            .as_array()
            .expect("lossy")
            .iter()
            .any(|l| l.as_str().unwrap_or_default().contains("1 step(s)")));
    }

    #[test]
    fn the_format_vocabulary_is_closed() {
        assert_eq!(FORMATS.len(), 2);
        assert!(FORMATS.contains(&"md"));
        assert!(FORMATS.contains(&"codetour"));
    }
}
