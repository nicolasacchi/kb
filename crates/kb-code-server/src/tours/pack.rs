//! `tour pack --budget` — the tour's steps as a BUDGETED context pack
//! (V74-L3b, D12; the D20 `--json` envelope).
//!
//! # The one rule
//!
//! A budget that is not enforced is not a budget, and a budget enforced
//! silently is a lie. So: steps are emitted in walk order until the next
//! one would cross the budget, and every step that did not fit is COUNTED
//! and NAMED in the response. A caller always knows whether it read the
//! whole tour.
//!
//! Two smaller consequences of that rule:
//!
//! * A step is emitted whole or not at all. Half a snippet is worse than a
//!   missing one — it reads as complete.
//! * An ORPHAN step is emitted with its prose and its last-known address
//!   and NO snippet, because there is nothing honest to show. It still
//!   costs its own bytes, and `honesty.orphans` on the parent read already
//!   counted it (invariant 24(a): an orphan is SHOWN, never dropped).
//!
//! The pack is plain Markdown. It is the same text `tour export --format
//! md` produces for the steps that fit, which is deliberate: an agent
//! reading a budgeted pack and a human reading the export are looking at
//! the same document, truncated at a stated point.

use super::routes::{TourOut, TourStepOut};
use serde::Serialize;

/// One step's rendered block, plus the two numbers that make the budget
/// auditable.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PackedStep {
    pub ordinal: usize,
    pub id: String,
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    pub bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DroppedStep {
    pub ordinal: usize,
    pub id: String,
    /// Why it is not here — always the budget, always with both numbers.
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PackOut {
    pub schema: &'static str,
    pub repo: String,
    pub slug: String,
    pub title: String,
    /// The Markdown itself.
    pub text: String,
    pub bytes: usize,
    pub budget: usize,
    pub steps_total: usize,
    pub steps_packed: usize,
    /// Every step that did not fit, in walk order. NEVER a bare count:
    /// which stops are missing is what tells a reader whether the pack
    /// still makes sense.
    pub dropped: Vec<DroppedStep>,
    pub packed: Vec<PackedStep>,
    pub notes: Vec<String>,
}

/// Render one step as Markdown. Shared with the `md` export so the two can
/// never drift.
pub fn render_step(s: &TourStepOut) -> String {
    let mut out = String::new();
    let title = s.node.title.as_deref().unwrap_or(s.node.kind.as_str());
    out.push_str(&format!("## {}. {title}\n\n", s.ordinal + 1));
    out.push_str(&format!(
        "`{}` — **{}** ({})\n\n",
        s.node.address, s.node.state, s.node.reason
    ));
    if let Some(r) = &s.ref_str {
        out.push_str(&format!("[[{r}]]\n\n"));
    }
    if let Some(body) = &s.node.body_md {
        if !body.trim().is_empty() {
            out.push_str(body.trim());
            out.push_str("\n\n");
        }
    }
    if let Some(note) = &s.node.note {
        out.push_str(&format!("> {note}\n\n"));
    }
    if let Some(code) = &s.node.code {
        if let Some(snippet) = &code.snippet {
            out.push_str("```\n");
            out.push_str(snippet);
            if !snippet.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("```\n\n");
            if code.snippet_truncated {
                out.push_str(&format!(
                    "_(snippet cut at {} lines)_\n\n",
                    crate::boards::MAX_SNIPPET_LINES
                ));
            }
        }
    }
    if let Some(cam) = &s.camera {
        let mut bits = Vec::new();
        if cam.fold == Some(true) {
            bits.push("fold".to_string());
        }
        if let Some(c) = cam.context {
            bits.push(format!("±{c} lines of context"));
        }
        if !bits.is_empty() {
            out.push_str(&format!("_camera: {}_\n\n", bits.join(", ")));
        }
    }
    out
}

fn header(tour: &TourOut) -> String {
    let mut out = format!("# {}\n\n", tour.title);
    if !tour.description_md.trim().is_empty() {
        out.push_str(tour.description_md.trim());
        out.push_str("\n\n");
    }
    out.push_str(&format!(
        "_kbc-tour/1 · {} · {} steps · rev {} · {} pinned / {} carried / {} orphan_\n\n",
        tour.slug,
        tour.honesty.steps,
        tour.revision,
        tour.honesty.pinned,
        tour.honesty.carried,
        tour.honesty.orphans
    ));
    if let Some(r) = &tour.authored_ref {
        out.push_str(&format!(
            "_authored at `{r}` — advisory only; every step above was re-resolved against \
             the working tree just now._\n\n"
        ));
    }
    out
}

/// Pack a resolved tour under a byte budget.
pub fn pack(tour: &TourOut, budget: usize) -> PackOut {
    let head = header(tour);
    let mut text = head.clone();
    let mut packed: Vec<PackedStep> = Vec::new();
    let mut dropped: Vec<DroppedStep> = Vec::new();

    for s in &tour.steps {
        let block = render_step(s);
        if !dropped.is_empty() || text.len() + block.len() > budget {
            // Once one step has been dropped every later step is dropped
            // too: a pack with a hole in the middle would read as a
            // shorter tour rather than a truncated one.
            dropped.push(DroppedStep {
                ordinal: s.ordinal,
                id: s.node.id.clone(),
                reason: format!(
                    "would take the pack past the {budget}-byte budget (this step is {} \
                     bytes)",
                    block.len()
                ),
            });
            continue;
        }
        packed.push(PackedStep {
            ordinal: s.ordinal,
            id: s.node.id.clone(),
            state: s.node.state,
            address: Some(s.node.address.clone()),
            bytes: block.len(),
        });
        text.push_str(&block);
    }

    let mut notes = vec![format!(
        "every step was re-resolved through the Ladder when this pack was built; a \
         `carried` step's line numbers are where the code is NOW, and an `orphan` step \
         carries no snippet because there is nothing honest to show"
    )];
    if !dropped.is_empty() {
        notes.push(format!(
            "{} of {} steps did not fit the {budget}-byte budget and are listed in \
             `dropped` — this pack is a PREFIX of the tour, not a summary of it",
            dropped.len(),
            tour.steps.len()
        ));
        text.push_str(&format!(
            "---\n\n_{} of {} steps omitted: the {budget}-byte budget stopped at step {}._\n",
            dropped.len(),
            tour.steps.len(),
            packed.len() + 1
        ));
    }
    PackOut {
        schema: super::SCHEMA,
        repo: tour.repo.clone(),
        slug: tour.slug.clone(),
        title: tour.title.clone(),
        bytes: text.len(),
        text,
        budget,
        steps_total: tour.steps.len(),
        steps_packed: packed.len(),
        dropped,
        packed,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boards::resolve::{honesty, NodeOut, STATE_ORPHAN, STATE_PINNED};
    use crate::boards::RefFields;

    fn node(id: &str, state: &'static str, body: &str) -> NodeOut {
        NodeOut {
            id: id.into(),
            kind: "code".into(),
            title: Some(format!("step {id}")),
            body_md: Some(body.into()),
            group: None,
            state,
            reason: crate::boards::resolve::REASON_BLOB_CURRENT,
            address: format!("app/{id}.rb:1-3"),
            note: None,
            code: None,
            query: None,
            thread: None,
            pin: None,
            reference: RefFields::default(),
        }
    }

    fn tour(n: usize, body: &str) -> TourOut {
        let steps: Vec<TourStepOut> = (0..n)
            .map(|i| TourStepOut {
                ordinal: i,
                node: node(&format!("s{i}"), STATE_PINNED, body),
                camera: None,
                ref_str: None,
            })
            .collect();
        let nodes: Vec<NodeOut> = steps.iter().map(|s| s.node.clone()).collect();
        TourOut {
            schema: super::super::SCHEMA,
            repo: "r".into(),
            slug: "t".into(),
            title: "T".into(),
            description_md: String::new(),
            status: "pending".into(),
            authored_ref: None,
            revision: 1,
            content_hash: "h".into(),
            created_unix: 0,
            updated_unix: 0,
            honesty: honesty(&nodes, 0, n, false, Vec::new()),
            steps,
        }
    }

    #[test]
    fn a_generous_budget_packs_every_step_and_drops_none() {
        let p = pack(&tour(3, "why"), MAX_BUDGET_FOR_TEST);
        assert_eq!(p.steps_packed, 3);
        assert_eq!(p.steps_total, 3);
        assert!(p.dropped.is_empty());
        assert_eq!(p.bytes, p.text.len());
        assert!(p.bytes <= MAX_BUDGET_FOR_TEST);
    }
    const MAX_BUDGET_FOR_TEST: usize = 64 * 1024;

    #[test]
    fn a_tight_budget_truncates_as_a_prefix_and_says_exactly_what_is_missing() {
        let t = tour(5, &"x".repeat(200));
        let full = pack(&t, MAX_BUDGET_FOR_TEST);
        // Room for the header plus roughly two steps.
        let budget = full.text.len() / 2;
        let p = pack(&t, budget);
        assert!(p.steps_packed < 5 && p.steps_packed > 0, "{p:#?}");
        assert_eq!(p.steps_packed + p.dropped.len(), 5, "nothing vanishes");
        assert!(p.bytes <= budget + 200, "the trailer is the only overshoot");
        // The dropped list is a SUFFIX — a pack with a hole in the middle
        // would read as a shorter tour rather than a truncated one.
        let first_dropped = p.dropped[0].ordinal;
        assert!(p.packed.iter().all(|s| s.ordinal < first_dropped));
        assert!(p.dropped.iter().all(|s| s.ordinal >= first_dropped));
        assert!(p.dropped[0].reason.contains(&budget.to_string()));
        assert!(p.notes.iter().any(|n| n.contains("PREFIX")));
        assert!(p.text.contains("omitted"));
    }

    #[test]
    fn a_budget_smaller_than_the_header_drops_everything_and_still_reports_honestly() {
        let p = pack(&tour(3, "why"), 1);
        assert_eq!(p.steps_packed, 0);
        assert_eq!(p.dropped.len(), 3);
        assert!(p.text.contains("3 of 3 steps omitted"));
    }

    #[test]
    fn an_orphan_step_is_packed_with_its_prose_and_no_snippet() {
        let mut t = tour(1, "this mattered");
        t.steps[0].node.state = STATE_ORPHAN;
        t.steps[0].node.code = None;
        let p = pack(&t, MAX_BUDGET_FOR_TEST);
        assert_eq!(p.steps_packed, 1, "an orphan is SHOWN, never dropped");
        assert!(p.text.contains("this mattered"));
        assert!(p.text.contains("orphan"));
        assert!(!p.text.contains("```"), "there is nothing honest to show");
    }
}
