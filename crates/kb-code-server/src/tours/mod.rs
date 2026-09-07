//! V74-L3b — `kbc-tour/1`: the CodeTour shape, plus a blob per step, on
//! the Ladder.
//!
//! Design of record: `docs/research/kb-code-v7-continuum-2026-09.html`
//! §Decisions **D12** ("kbc-tour/1 (CodeTour shape plus a blob per step, on
//! the Ladder), record-a-tour from navigation, `tour pack --budget`") and
//! **D10** ("a board's `steps` and a tour share the step model"), Track L.
//!
//! # The one sentence, and the storage decision it forces
//!
//! A tour is an ordered walk through references this daemon already serves,
//! each step carrying prose and a camera, every reference re-resolved
//! through the Ladder on every read — which is, word for word, a BOARD
//! whose nodes are its steps.
//!
//! So this module creates no table. A tour is a `canvas_boards` row with
//! `kind = 'tour'` (migration V0039): its steps are `canvas_nodes`, their
//! order and cameras are `canvas_steps`, and consecutive steps are joined
//! by `then` edges in `canvas_edges`. That is invariant 9's ruling ("a
//! `reading_sets` row with `kind = 'workspace'` IS a workspace — not a new
//! entity") and invariant 14's restatement of it, applied one table over,
//! and it is what D10's "do not build two" asks for at the strongest
//! reading: not two similar step types kept in sync, but ONE — one lint
//! ([`boards::lint::check`], which this module extends rather than
//! duplicates), one resolver ([`boards::resolve::resolve_node`]), one
//! walkthrough contract, one `?step=` grammar.
//!
//! What a tour adds on top of a board is exactly three things, and each is
//! additive:
//!
//! 1. **`camera`** — per-step fold/context hints ([`Camera`]), on
//!    `canvas_steps.camera_json`. NO COORDINATES: `boards::lint`'s
//!    `coordinates` rule runs over a tour document unchanged, so an author
//!    cannot smuggle geometry in through a camera any more than through a
//!    node.
//! 2. **The `ref` sugar** — a step may name its target with a K1
//!    `kbc-review/1` ref string (`code:app/x.rb:12-40@<sha>`) instead of
//!    the structured reference fields. It is SUGAR, parsed by
//!    `review_doc::refs::parse_ref` (the ONE ref parser) and lowered to the
//!    same `boards::RefFields` a board node carries. See
//!    [`node_from_ref_string`] for exactly which schemes lower and why the
//!    rest deliberately do not.
//! 3. **The linear structure** — steps ARE the nodes, and the `then` chain
//!    between consecutive steps is generated, not authored. A tour
//!    therefore always lints as one connected component without needing
//!    `allow_disconnected`, because a sequence genuinely is connected.
//!
//! # Mutation posture
//!
//! `POST /api/tours/apply` and `DELETE /api/tours/{slug}` are
//! **loopback-only** — the same `transcripts_api` sub-router the board
//! mutations ride, so `security::audit_mutations` (invariant 1) records
//! each attempt with its outcome for free. `accepted`/`archived` remain
//! reachable only through the BOARD transition routes; a tour ships
//! `pending`/`draft` exactly as D21 requires of any agent-authored
//! document.

pub mod export;
pub mod lint;
pub mod pack;
pub mod routes;

use crate::boards::{self, BoardDoc, EdgeIn, NodeIn, RefFields, StepIn as BoardStepIn, KIND_CODE};
use serde::{Deserialize, Serialize};

/// The wire + document schema string.
pub const SCHEMA: &str = "kbc-tour/1";

// --- the `canvas_boards.kind` discriminator -------------------------------

pub const BOARD_KIND_BOARD: &str = "board";
pub const BOARD_KIND_TOUR: &str = "tour";

/// The CLOSED `canvas_boards.kind` vocabulary (migration V0039).
/// Rust-validated, never a SQL CHECK — invariant 9's house convention.
pub const BOARD_KINDS: [&str; 2] = [BOARD_KIND_BOARD, BOARD_KIND_TOUR];

pub fn is_valid_board_kind(s: &str) -> bool {
    BOARD_KINDS.contains(&s)
}

// --- caps -----------------------------------------------------------------

/// Steps one tour may carry. Deliberately below `boards::MAX_NODES`: a tour
/// is walked one step at a time by a human, and a 200-stop walk is a
/// reading list, not a tour. A payload over this is REFUSED with the count.
pub const MAX_STEPS: usize = 60;
/// Default byte budget for `tour pack`, when the caller names none.
pub const DEFAULT_PACK_BUDGET: usize = 24 * 1024;
/// Hard ceiling on a pack budget — a "budgeted" pack with no bound is not
/// budgeted.
pub const MAX_PACK_BUDGET: usize = 512 * 1024;
/// Context lines a camera may ask for around a step's primary range.
pub const MAX_CAMERA_CONTEXT: u32 = 40;

// --- the document ---------------------------------------------------------

/// The `kbc-tour/1` document an agent writes and `kb-code tour apply`
/// sends. CodeTour's shape (title, description, ordered steps with prose)
/// plus this daemon's own two additions: a blob per step, and a camera.
///
/// `deny_unknown_fields` for `boards::BoardDoc`'s reason: a typo'd key in
/// an LLM-authored document must fail loudly at apply time rather than
/// being silently dropped and discovered as a missing step weeks later.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TourDoc {
    /// Must be [`SCHEMA`].
    pub schema: String,
    pub repo: String,
    pub slug: String,
    pub title: String,
    #[serde(default)]
    pub description_md: String,
    /// `pending` (default, D21) or `draft`.
    #[serde(default)]
    pub status: Option<String>,
    /// The authoring git ref — CodeTour's own `ref` field. Advisory
    /// context for a reader, NEVER a resolution input: every step
    /// re-resolves against the CURRENT working tree.
    #[serde(default, rename = "ref")]
    pub authored_ref: Option<String>,
    pub steps: Vec<TourStepIn>,
}

/// The per-step CAMERA: what the reader should SHOW, never where anything
/// SITS. `deny_unknown_fields`, and `boards::lint`'s coordinate rule runs
/// over the whole document anyway, so `{"camera": {"x": 10}}` is refused
/// twice over — by name, and by shape.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Camera {
    /// Fold everything outside the step's primary range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fold: Option<bool>,
    /// Lines of context to reveal around the primary range, `0..=`
    /// [`MAX_CAMERA_CONTEXT`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<u32>,
}

/// One step. Structurally a [`boards::NodeIn`] — the SAME id, title,
/// `body_md`, `thread_id` and flattened [`boards::RefFields`] — plus
/// `camera`, plus the `ref` string sugar.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TourStepIn {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The step's prose. Authored Markdown, stored INERT: this crate never
    /// renders it (`boards::NodeIn::body_md`'s posture, D9-a).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_md: Option<String>,
    /// A kbc-review/1 ref string — SUGAR for the structured reference
    /// below, and mutually exclusive with it. See [`node_from_ref_string`].
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub ref_str: Option<String>,
    /// One of `boards::NODE_KINDS`. Required UNLESS `ref` is given (which
    /// determines the kind by itself).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The `annotations.id` of this step's thread parent — node threads
    /// reuse the annotations store (D10), and so do tour-step threads,
    /// because they are the same rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<Camera>,
    /// The structured reference, byte-identical to a board node's.
    #[serde(flatten)]
    pub reference: RefFields,
}

impl TourStepIn {
    /// `true` when the author gave neither form of reference — a prose-only
    /// step, which is a legitimate `note`.
    pub fn has_structured_reference(&self) -> bool {
        self.reference != RefFields::default()
    }
}

/// Lower a K1 ref STRING to a board node kind + reference fields.
///
/// Deliberately narrow, and the narrowness is the honest part. The sugar
/// covers `code:` refs **that cite a line or a range** and nothing else,
/// because that is the only scheme whose K1 address carries everything a
/// board node's reference needs:
///
/// | scheme | why not |
/// |---|---|
/// | `code:` with no line | a step is a PLACE; `code:app/x.rb` says only "this file", and there is nothing for the Ladder to carry forward |
/// | `sym:` / `ent:` | resolving a symbol to a path+range is a DERIVATION, and persisting it at apply time is exactly the cached-class this crate refuses (invariant 24(a), root invariant #2). Cite the place: `code:app/x.rb:12-40@<sha>` |
/// | `hunk:` | K1's `hunk:<path>@<ps>#<n>` carries no review id and no `kbc-hunkid/1` address; the structured form (`kind: "hunk"` + `review` + `patchset` + `hunk`) carries both |
/// | `finding:` | K1's `finding:<slug>` carries no review id; the structured form does |
/// | `gh:` / `kb:` | inert links this daemon never resolves — put them in the step's `body_md`, or use a `link` node with a real URL |
/// | `ci:` | V73-K5's check-run citation is `is_inert` (a snapshot read) and carries no path or range — a step is a PLACE, and a check has none |
/// | `question:` | V73-K5's document-local back-reference addresses another question in a REVIEW, not a place in the repository |
///
/// Every one of those refusals NAMES the structured field that does the
/// job, so a narrower sugar is never a dead end.
pub fn node_from_ref_string(raw: &str) -> Result<(String, RefFields), String> {
    use crate::review_doc::refs::{parse_ref, Ref};
    let parsed = parse_ref(raw).map_err(|e| format!("{raw:?} is not a valid kbc ref: {e}"))?;
    let Some(r) = parsed else {
        return Err(format!(
            "{raw:?} names no kbc ref scheme — a tour step's `ref` is a kbc-review/1 ref \
             (e.g. `code:app/models/order.rb:12-40@<sha>`), not a bare path"
        ));
    };
    match r {
        Ref::Code {
            path,
            line: Some(line),
            line_end,
            sha,
            ..
        } => Ok((
            KIND_CODE.to_string(),
            RefFields {
                path: Some(path),
                range: Some([line, line_end.unwrap_or(line)]),
                blob_sha: sha,
                ..Default::default()
            },
        )),
        Ref::Code { .. } => Err(format!(
            "{raw:?} cites a whole file — a tour step is a PLACE, so cite a line or a \
             range (`code:<path>:<line>[-<end>][@<sha>]`). A whole-file step gives the \
             Ladder nothing to carry forward when the file changes"
        )),
        Ref::Sym { .. } | Ref::Ent { .. } => Err(format!(
            "{raw:?} addresses a symbol, and resolving one to a path and a range is a \
             DERIVATION this daemon must not persist (a stored derivation is a class that \
             can go stale without saying so). Cite the place instead — \
             `code:<path>:<line>-<end>@<sha>` — or use the structured reference fields"
        )),
        Ref::Hunk { .. } => Err(format!(
            "{raw:?} carries no review id, so it cannot address a hunk on its own. Use \
             the structured form: `\"kind\": \"hunk\", \"review\": <id>, \"patchset\": \
             <n>, \"hunk\": \"<kbc-hunkid/1>\"`"
        )),
        Ref::Finding { .. } => Err(format!(
            "{raw:?} carries no review id. Use the structured form: `\"kind\": \
             \"finding\", \"review\": <id>, \"finding\": \"<f-slug>\"`"
        )),
        Ref::Gh { .. } | Ref::Kb { .. } => Err(format!(
            "{raw:?} is an INERT link — this daemon never calls GitHub and does not own \
             the kb corpus, so it makes no claim about it. Put it in the step's `body_md`, \
             or use a `link` node with a real URL"
        )),
        Ref::Ci { .. } => Err(format!(
            "{raw:?} is an INERT check-run citation (V73-K5, a snapshot read, never a \
             live GitHub call) and carries no path or range. A tour step is a PLACE; put \
             this in the step's `body_md` instead"
        )),
        Ref::Question { .. } => Err(format!(
            "{raw:?} addresses another question in a REVIEW DOCUMENT (V73-K5), not a \
             place in the repository — a tour step has no use for it. Put it in the \
             step's `body_md` instead"
        )),
    }
}

/// Render a resolved step's reference BACK as a K1 ref string, for export
/// and for citation. One-way and lossy by design: it is a PROJECTION for a
/// reader, never a second parser, and the schemes it cannot express say
/// nothing rather than something approximate.
pub fn k1_ref_for(kind: &str, r: &RefFields) -> Option<String> {
    if kind != KIND_CODE {
        return None;
    }
    let path = r.path.as_deref()?;
    let range = r.range?;
    let span = if range[0] == range[1] {
        format!("{}", range[0])
    } else {
        format!("{}-{}", range[0], range[1])
    };
    Some(match r.blob_sha.as_deref() {
        Some(sha) => format!("code:{path}:{span}@{sha}"),
        None => format!("code:{path}:{span}"),
    })
}

/// The `then` edge chain a tour's steps imply — GENERATED, never authored.
///
/// A tour is a sequence, and a sequence is connected: emitting the chain is
/// what lets `boards::lint::check`'s connectivity rule run over a tour
/// unchanged and pass HONESTLY, rather than being suppressed with
/// `allow_disconnected` (which would also suppress a real problem on a
/// board). `then` is already in `boards::EDGE_KINDS`; the provenance is
/// `authored`, because the sequence is the author's, and an authored edge
/// carries no trust class (D10).
pub fn then_chain(step_ids: &[String]) -> Vec<EdgeIn> {
    step_ids
        .windows(2)
        .map(|w| EdgeIn {
            from: w[0].clone(),
            to: w[1].clone(),
            kind: "then".to_string(),
            label: None,
            provenance: Some(boards::PROVENANCE_AUTHORED.to_string()),
            trust: None,
        })
        .collect()
}

/// Lower a tour document to the BOARD document the one lint and the one
/// resolver already understand. Errors are per-step and collected, so an
/// agent retrying an apply sees every problem at once (the `RefFields`
/// flat-struct rationale, one layer up).
pub fn to_board_doc(doc: &TourDoc) -> Result<BoardDoc, Vec<lint::Finding>> {
    let mut errors: Vec<lint::Finding> = Vec::new();
    let mut nodes: Vec<NodeIn> = Vec::new();
    for s in &doc.steps {
        let (kind, reference) = match (&s.ref_str, s.kind.as_deref()) {
            (Some(_), Some(_)) => {
                errors.push(lint::Finding::refuse(
                    "step-ref",
                    Some(s.id.clone()),
                    "a step names BOTH `ref` and `kind` — `ref` is sugar that determines \
                     the kind by itself, so give one or the other",
                ));
                continue;
            }
            (Some(raw), None) => {
                if s.has_structured_reference() {
                    errors.push(lint::Finding::refuse(
                        "step-ref",
                        Some(s.id.clone()),
                        "a step names BOTH `ref` and structured reference fields — `ref` \
                         is sugar for those fields, so give one or the other",
                    ));
                    continue;
                }
                match node_from_ref_string(raw) {
                    Ok(v) => v,
                    Err(m) => {
                        errors.push(lint::Finding::refuse("step-ref", Some(s.id.clone()), m));
                        continue;
                    }
                }
            }
            (None, Some(k)) => (k.to_string(), s.reference.clone()),
            (None, None) => {
                if s.has_structured_reference() {
                    errors.push(lint::Finding::refuse(
                        "step-ref",
                        Some(s.id.clone()),
                        "a step carries reference fields but no `kind` — say which kind \
                         they belong to, or use the `ref` string sugar",
                    ));
                    continue;
                }
                // A prose-only step. `note` is exactly that kind, and it is
                // the one boards already calls inert.
                (boards::KIND_NOTE.to_string(), RefFields::default())
            }
        };
        nodes.push(NodeIn {
            id: s.id.clone(),
            kind,
            title: s.title.clone(),
            body_md: s.body_md.clone(),
            group: None,
            thread_id: s.thread_id.clone(),
            reference,
        });
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
    Ok(BoardDoc {
        schema: boards::SCHEMA.to_string(),
        repo: doc.repo.clone(),
        slug: doc.slug.clone(),
        title: doc.title.clone(),
        description_md: doc.description_md.clone(),
        status: doc.status.clone(),
        authored_ref: doc.authored_ref.clone(),
        nodes,
        edges: then_chain(&ids),
        steps: ids
            .iter()
            .map(|id| BoardStepIn {
                node: id.clone(),
                caption: None,
            })
            .collect(),
        pins: Default::default(),
    })
}

/// The idempotency hash. Computed over the LOWERED board document plus the
/// cameras — `boards::content_hash` is the one hash, and the cameras are
/// the only tour-specific content it does not already cover.
pub fn content_hash(doc: &TourDoc, board: &BoardDoc) -> String {
    #[derive(Serialize)]
    struct Canonical<'a> {
        board: String,
        cameras: Vec<(&'a str, Option<Camera>)>,
    }
    let canonical = Canonical {
        board: boards::content_hash(board),
        cameras: doc
            .steps
            .iter()
            .map(|s| (s.id.as_str(), s.camera))
            .collect(),
    };
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    blake3::hash(&bytes).to_hex().to_string()
}

// --- route contracts (invariant 15) ---------------------------------------

use crate::entities::RouteContract;

pub const TOURS_LIST_ROUTE: RouteContract = RouteContract {
    path: "/api/tours",
    handler: "tours::routes::list_tours",
    required_params: &["repo"],
    params_accept_without: routes::list_params_accept_without,
};

pub const TOUR_GET_ROUTE: RouteContract = RouteContract {
    path: "/api/tours/{slug}",
    handler: "tours::routes::get_tour",
    required_params: &["repo"],
    params_accept_without: routes::get_params_accept_without,
};

pub const TOUR_PACK_ROUTE: RouteContract = RouteContract {
    path: "/api/tours/{slug}/pack",
    handler: "tours::routes::pack_tour",
    required_params: &["repo"],
    params_accept_without: routes::pack_params_accept_without,
};

pub const TOUR_EXPORT_ROUTE: RouteContract = RouteContract {
    path: "/api/tours/{slug}/export",
    handler: "tours::routes::export_tour",
    required_params: &["repo", "format"],
    params_accept_without: routes::export_params_accept_without,
};

/// This unit's TOUR read surface. The two mutations (`apply`, `DELETE`) are
/// absent for `boards::V74_L1_ROUTES`' own stated reason.
pub const V74_L3B_TOUR_ROUTES: &[RouteContract] = &[
    TOURS_LIST_ROUTE,
    TOUR_GET_ROUTE,
    TOUR_PACK_ROUTE,
    TOUR_EXPORT_ROUTE,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn step(id: &str) -> TourStepIn {
        TourStepIn {
            id: id.to_string(),
            title: Some("T".into()),
            body_md: Some("why".into()),
            ref_str: Some("code:app/a.rb:10-20@abc1234".into()),
            kind: None,
            thread_id: None,
            camera: None,
            reference: RefFields::default(),
        }
    }

    fn doc(steps: Vec<TourStepIn>) -> TourDoc {
        TourDoc {
            schema: SCHEMA.to_string(),
            repo: "r".into(),
            slug: "checkout".into(),
            title: "Checkout".into(),
            description_md: String::new(),
            status: None,
            authored_ref: None,
            steps,
        }
    }

    #[test]
    fn the_board_kind_vocabulary_is_closed_and_defaults_to_board() {
        for k in BOARD_KINDS {
            assert!(is_valid_board_kind(k));
        }
        assert!(!is_valid_board_kind("tours"));
        assert!(!is_valid_board_kind(""));
        assert_eq!(
            BOARD_KIND_BOARD, "board",
            "V0039's column DEFAULT is this literal — a V74-L1 row must read back \
             unchanged"
        );
    }

    #[test]
    fn the_ref_sugar_lowers_a_code_ref_and_names_the_fix_for_every_scheme_it_will_not() {
        let (kind, r) = node_from_ref_string("code:app/a.rb:10-20@abc1234").expect("lowers");
        assert_eq!(kind, KIND_CODE);
        assert_eq!(r.path.as_deref(), Some("app/a.rb"));
        assert_eq!(r.range, Some([10, 20]));
        assert_eq!(r.blob_sha.as_deref(), Some("abc1234"));
        // A single line is a one-line range, not a half-open shape.
        let (_, r) = node_from_ref_string("code:app/a.rb:7").expect("lowers");
        assert_eq!(r.range, Some([7, 7]));
        assert_eq!(r.blob_sha, None);

        for (raw, must_name) in [
            ("code:app/a.rb", "cite a line or a range"),
            ("sym:Order#total", "DERIVATION"),
            ("ent:Shop::Order", "DERIVATION"),
            ("hunk:app/a.rb@3#1", "\"kind\": \"hunk\""),
            ("finding:f-slow-query", "\"kind\": \"finding\""),
            ("gh:pr/15476", "INERT"),
            ("kb:docs/abc", "INERT"),
        ] {
            let err = node_from_ref_string(raw).expect_err("this scheme is deliberately not sugar");
            assert!(
                err.contains(must_name),
                "the refusal for {raw:?} must name the fix ({must_name:?}): {err}"
            );
        }
        assert!(node_from_ref_string("app/a.rb:10").is_err());
    }

    #[test]
    fn the_k1_projection_round_trips_a_code_step_and_declines_the_rest() {
        let (kind, r) = node_from_ref_string("code:app/a.rb:10-20@abc1234").expect("lowers");
        assert_eq!(
            k1_ref_for(&kind, &r).as_deref(),
            Some("code:app/a.rb:10-20@abc1234"),
            "the projection must reproduce the author's own address"
        );
        let (kind, r) = node_from_ref_string("code:a.rb:7").expect("lowers");
        assert_eq!(k1_ref_for(&kind, &r).as_deref(), Some("code:a.rb:7"));
        assert_eq!(
            k1_ref_for(boards::KIND_NOTE, &RefFields::default()),
            None,
            "a projection that cannot be exact says nothing rather than something \
             approximate"
        );
    }

    #[test]
    fn a_tour_lowers_to_a_connected_board_whose_nodes_are_its_steps() {
        let b = to_board_doc(&doc(vec![step("s1"), step("s2"), step("s3")])).expect("lowers");
        assert_eq!(b.nodes.len(), 3);
        assert_eq!(b.steps.len(), 3, "every node is a step — that IS a tour");
        assert_eq!(
            b.edges.len(),
            2,
            "n steps imply n-1 `then` edges, which is what makes a tour ONE component"
        );
        assert!(b.edges.iter().all(|e| e.kind == "then"));
        assert!(
            b.edges.iter().all(|e| e.trust.is_none()),
            "an authored edge carries no trust class (D10)"
        );
        assert!(b.pins.is_empty(), "a tour authors no geometry at all");
        // The lowered board passes the ONE lint with no relaxation.
        let report = crate::boards::lint::check(&b, Default::default());
        assert!(!report.refused(), "{:#?}", report.findings);
        assert_eq!(report.components.len(), 1);
    }

    #[test]
    fn a_single_step_tour_has_no_chain_and_still_lints() {
        let b = to_board_doc(&doc(vec![step("only")])).expect("lowers");
        assert!(b.edges.is_empty());
        assert!(!crate::boards::lint::check(&b, Default::default()).refused());
    }

    #[test]
    fn a_prose_only_step_is_a_note_and_a_mixed_reference_is_refused() {
        let mut prose = step("s1");
        prose.ref_str = None;
        let b = to_board_doc(&doc(vec![prose])).expect("lowers");
        assert_eq!(b.nodes[0].kind, boards::KIND_NOTE);

        let mut both = step("s1");
        both.kind = Some(KIND_CODE.into());
        let err = to_board_doc(&doc(vec![both])).expect_err("ref + kind is ambiguous");
        assert_eq!(err[0].rule, "step-ref");

        let mut mixed = step("s1");
        mixed.reference = RefFields {
            path: Some("a.rb".into()),
            ..Default::default()
        };
        let err = to_board_doc(&doc(vec![mixed])).expect_err("ref + fields is ambiguous");
        assert!(err[0].message.contains("sugar for those fields"));

        let mut bare = step("s1");
        bare.ref_str = None;
        bare.reference = RefFields {
            path: Some("a.rb".into()),
            ..Default::default()
        };
        let err = to_board_doc(&doc(vec![bare])).expect_err("fields with no kind");
        assert!(err[0].message.contains("no `kind`"));
    }

    #[test]
    fn the_content_hash_covers_the_camera_and_ignores_the_status() {
        let a = doc(vec![step("s1")]);
        let ba = to_board_doc(&a).expect("lowers");
        let mut b = a.clone();
        b.status = Some(boards::STATUS_DRAFT.into());
        let bb = to_board_doc(&b).expect("lowers");
        assert_eq!(
            content_hash(&a, &ba),
            content_hash(&b, &bb),
            "status is a transition, not authored content (boards::content_hash's rule)"
        );
        let mut c = a.clone();
        c.steps[0].camera = Some(Camera {
            fold: Some(true),
            context: Some(3),
        });
        let bc = to_board_doc(&c).expect("lowers");
        assert_ne!(
            content_hash(&a, &ba),
            content_hash(&c, &bc),
            "a camera IS authored content — changing one must change the revision"
        );
    }

    #[test]
    fn an_unknown_document_or_camera_key_fails_the_parse() {
        let err = serde_json::from_str::<TourDoc>(
            r#"{"schema":"kbc-tour/1","repo":"r","slug":"s","title":"T","stepz":[]}"#,
        )
        .expect_err("an unknown key must fail");
        assert!(err.to_string().contains("stepz"), "{err}");
        let err = serde_json::from_str::<Camera>(r#"{"x":10}"#)
            .expect_err("a camera carries no geometry");
        assert!(err.to_string().contains('x'), "{err}");
    }

    #[test]
    fn every_declared_route_path_is_api_nested_and_distinct() {
        assert!(!V74_L3B_TOUR_ROUTES.is_empty());
        let mut seen = std::collections::BTreeSet::new();
        for c in V74_L3B_TOUR_ROUTES {
            assert!(c.path.starts_with("/api/"), "{}", c.path);
            assert!(seen.insert(c.path), "duplicate contract for {}", c.path);
        }
    }
}
