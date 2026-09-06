//! V74-L1 — `kbc-canvas/1`: boards of REFERENCE nodes, on the Ladder.
//!
//! Design of record: `docs/research/kb-code-v7-continuum-2026-09.html`
//! §Decisions **D10** (boards, the canvas rebuilt) and **D21** (an
//! agent-proposed board is PENDING until a human accepts it), Track L.
//! D10 retires v3 D7's "canvas is SPA interaction chrome, no CLI verbs"
//! ruling by citation: a board is now something an agent AUTHORS and a
//! human WALKS, so it needs a document, a CLI and a drift gate.
//!
//! # The one sentence
//!
//! A board is an ordered set of **references** into surfaces this daemon
//! already serves — never a copy of them — plus authored edges between
//! those references, and every reference is re-resolved through the
//! carry-forward **Ladder** on every read.
//!
//! Three consequences, and each is a way to break this quietly:
//!
//! 1. **Nothing about resolution is stored.** There is no
//!    `canvas_nodes.state` column. `pinned` / `carried` / `orphan` is
//!    computed per request by [`resolve`], exactly as
//!    `lanes::classing::class_for` (invariant 21) and `entities::class_for`
//!    (invariant 13) compute theirs, and for the reason root invariant #2
//!    states for the whole doc↔code bridge: kb-code mints classes, nothing
//!    is cached. A node whose target moved must never read back as fresh.
//! 2. **An orphan is SHOWN, never dropped.** It keeps its title, its note,
//!    its last-known address and its authored snippet, and says the code is
//!    gone. A board that silently loses a card is worse than a board that
//!    admits one died — this is the same posture `review_comments`' own
//!    carry-forward ladder takes, and `boards` calls THAT ladder
//!    (`annotations::anchor_for_line` + `annotations::resolve`) rather than
//!    growing a second one.
//! 3. **Boards are coordinate-free.** The document has no `x`/`y` anywhere.
//!    Layout is the SPA's, in TypeScript, with one engine (D10). The ONLY
//!    authored geometry is a `pins` map — an explicit override for a node
//!    the human placed by hand — and a payload that carries coordinates
//!    under any other name is REFUSED by the lint, by name, so an LLM
//!    cannot learn to emit them.
//!
//! # Storage
//!
//! Four new tables (`migrations/V0036__canvas.sql`), NOT a `canvas_sets`
//! row and NOT a `reading_sets` row — that migration's own header argues
//! the case in full. kbc-seq/1 (invariant 14) remains a projection LAYER
//! over whatever already owns the rows: after this unit its `board`
//! projection resolves out of `canvas_boards` as well as `canvas_sets`,
//! and a kbc-canvas/1 board reports a TRUE node count where a
//! `canvas_sets` row still honestly reports `null`.
//!
//! # Mutation posture
//!
//! Every write (`apply`, `accept`, `archive`, `DELETE`) is **loopback-only**
//! — the same `transcripts_api` sub-router the review mutations ride, so
//! `security::audit_mutations` (invariant 1) records each attempt with its
//! outcome for free. D10 sketches a later graduation onto a named-family
//! `[review] remote_mutations = ["review", "canvas"]` allowlist; that is
//! NOT this unit, and nothing here weakens root invariant #4.

pub mod export;
pub mod layout;
pub mod lint;
pub mod resolve;
pub mod routes;

use serde::{Deserialize, Serialize};

/// The wire + document schema string.
pub const SCHEMA: &str = "kbc-canvas/1";

// --- vocabularies ---------------------------------------------------------

pub const KIND_CODE: &str = "code";
pub const KIND_NOTE: &str = "note";
pub const KIND_QUERY: &str = "query";
pub const KIND_HUNK: &str = "hunk";
pub const KIND_FINDING: &str = "finding";
pub const KIND_ANNOTATION: &str = "annotation";
pub const KIND_TURN: &str = "turn";
pub const KIND_BOOKMARK: &str = "bookmark";
pub const KIND_GROUP: &str = "group";
pub const KIND_LINK: &str = "link";

/// The CLOSED node vocabulary, in the order the wire and the docs list it.
/// Every one of these is a REFERENCE into a surface kb-code already has,
/// except `note`/`group`/`link`, which are the three authored-content
/// kinds (and are exactly the three the XSS posture applies to).
pub const NODE_KINDS: [&str; 10] = [
    KIND_CODE,
    KIND_NOTE,
    KIND_QUERY,
    KIND_HUNK,
    KIND_FINDING,
    KIND_ANNOTATION,
    KIND_TURN,
    KIND_BOOKMARK,
    KIND_GROUP,
    KIND_LINK,
];

/// The CLOSED edge vocabulary (D10, verbatim). Each gets a distinct stroke
/// in the SPA; this crate only guarantees the SET.
pub const EDGE_KINDS: [&str; 8] = [
    "calls",
    "renders",
    "reads",
    "writes",
    "then",
    "implements",
    "contradicts",
    "question",
];

pub const PROVENANCE_AUTHORED: &str = "authored";
pub const PROVENANCE_DERIVED: &str = "derived";

/// Trust classes a DERIVED edge may carry — this crate's existing
/// `resolve.rs` vocabulary, borrowed rather than re-minted. An AUTHORED
/// edge carries none, and the lint refuses one that does.
pub const EDGE_TRUST: [&str; 3] = ["exact", "likely", "candidate"];

pub const STATUS_PENDING: &str = "pending";
pub const STATUS_DRAFT: &str = "draft";
pub const STATUS_ACCEPTED: &str = "accepted";
pub const STATUS_ARCHIVED: &str = "archived";

/// The CLOSED status vocabulary. `pending` is D21's: an agent-proposed
/// board is pending until a human accepts it over loopback.
pub const STATUSES: [&str; 4] = [
    STATUS_PENDING,
    STATUS_DRAFT,
    STATUS_ACCEPTED,
    STATUS_ARCHIVED,
];

/// The two statuses `apply` may WRITE. `accepted` and `archived` are
/// reachable only through their own loopback routes — see
/// [`lint::Lint::check`]'s status rule.
pub const APPLIABLE_STATUSES: [&str; 2] = [STATUS_PENDING, STATUS_DRAFT];

pub fn is_valid_node_kind(s: &str) -> bool {
    NODE_KINDS.contains(&s)
}

pub fn is_valid_edge_kind(s: &str) -> bool {
    EDGE_KINDS.contains(&s)
}

pub fn is_valid_status(s: &str) -> bool {
    STATUSES.contains(&s)
}

// --- caps -----------------------------------------------------------------

/// Hard node cap (D10's "node cap"; the canvas research's own ~200 figure).
/// A payload over this is REFUSED with the count, never truncated.
pub const MAX_NODES: usize = 200;
/// Hard edge cap. Generous relative to `MAX_NODES` (a board is allowed to
/// be dense) but finite, for the same reason.
pub const MAX_EDGES: usize = 600;
/// Above this many nodes a board must declare `steps` — a reading order.
/// D10: "steps required above N nodes."
pub const STEPS_REQUIRED_ABOVE: usize = 40;
/// Above this many weakly-connected components an apply refuses unless the
/// caller passes `allow_disconnected`. Below it, disconnection is a WARNING
/// with the component list.
pub const MAX_COMPONENTS: usize = 6;
/// Per-node Markdown body cap, in UTF-8 bytes.
pub const MAX_BODY_BYTES: usize = 8 * 1024;
/// Board `description_md` cap, in UTF-8 bytes.
pub const MAX_DESCRIPTION_BYTES: usize = 16 * 1024;
/// Raw request-body cap for `POST /api/boards/apply`, enforced BEFORE the
/// JSON parse (the `canvas::reject_oversize_raw` precedent: the parse cost
/// must be bounded by a cap, not by axum's default body limit).
pub const MAX_APPLY_BYTES: usize = 512 * 1024;
/// Longest slug / node id accepted.
pub const MAX_ID_LEN: usize = 64;
/// Longest title accepted.
pub const MAX_TITLE_LEN: usize = 200;
/// How many source lines of a `code` node's primary range the read route
/// will render as a snippet. A range longer than this is snipped and the
/// response SAYS SO (`snippet_truncated`), never silently clipped.
pub const MAX_SNIPPET_LINES: usize = 60;

// --- the document ---------------------------------------------------------

/// The `kbc-canvas/1` document an agent writes and `canvas apply` sends.
///
/// Deliberately `deny_unknown_fields`: a typo'd key in an LLM-authored
/// document must fail loudly at apply time rather than being silently
/// dropped and discovered as a missing card weeks later. That is the same
/// bet kbcq/1 declines to make for a SEARCH query (where a total parse and
/// a diagnostic is right, because a keystroke must always do something) and
/// the opposite answer is right here, because an apply is a deliberate,
/// retryable, whole-document write.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BoardDoc {
    /// Must be [`SCHEMA`]. Present so a document is self-identifying on
    /// disk and in a git diff.
    pub schema: String,
    pub repo: String,
    pub slug: String,
    pub title: String,
    #[serde(default)]
    pub description_md: String,
    /// `pending` (default, D21) or `draft`. Never `accepted`/`archived` —
    /// those are transitions, not authored state.
    #[serde(default)]
    pub status: Option<String>,
    /// The authoring git ref (CodeTour's `ref`): advisory context, never a
    /// resolution input.
    #[serde(default)]
    pub authored_ref: Option<String>,
    #[serde(default)]
    pub nodes: Vec<NodeIn>,
    #[serde(default)]
    pub edges: Vec<EdgeIn>,
    /// Walkthrough order — node ids, in reading order.
    #[serde(default)]
    pub steps: Vec<StepIn>,
    /// The ONLY authored geometry: node id → `{x, y}`. Absent = layout
    /// decides. Anything else coordinate-shaped is a lint refusal.
    #[serde(default)]
    pub pins: std::collections::BTreeMap<String, Pin>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NodeIn {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub title: Option<String>,
    /// Authored Markdown. Stored INERT: this crate never renders it, never
    /// sanitises it and never interpolates it into HTML except through
    /// [`export::escape_html`], which escapes it wholesale. The SPA renders
    /// it under its own Markdown contract (D9-a's posture: authored
    /// Markdown, never authored HTML).
    #[serde(default)]
    pub body_md: Option<String>,
    /// The membership pointer for a `group` node's members.
    #[serde(default)]
    pub group: Option<String>,
    /// The `annotations.id` of this node's thread parent (D10: node
    /// threads reuse the annotations store).
    #[serde(default)]
    pub thread_id: Option<String>,
    /// The kind-specific reference. Untagged on the wire so a document
    /// reads naturally; validated against `kind` by the lint.
    #[serde(flatten)]
    pub reference: RefFields,
}

/// The union of every kind's reference fields, flattened onto the node.
/// Kept as one flat optional-field struct rather than a `#[serde(tag =
/// "kind")]` enum on purpose: the lint can then report EVERY problem with a
/// node in one pass (wrong kind AND a missing field AND a stray one),
/// which is what an agent retrying an apply needs, instead of serde's
/// first-error-wins message.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RefFields {
    // code
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// `[start, end]`, 1-based inclusive — the PRIMARY range (what the card
    /// shows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<[u32; 2]>,
    /// `[start, end]`, 1-based inclusive — the CONTEXT range (what `±N`
    /// expands to). Zed's multibuffer distinction, kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<[u32; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_sha: Option<String>,
    /// blake3 of the primary range's bytes at authoring time. The GUARD:
    /// a byte-identical range under a changed blob is still `pinned`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_hash: Option<String>,
    // query
    /// A kbcq/1 query string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// The result count the author saw. `delta` on read is
    /// `current - authored_count`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authored_count: Option<u32>,
    // hunk / finding
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patchset: Option<u32>,
    /// `kbc-hunkid/1` — the SPA's own content address, opaque here
    /// (`reviews::is_hunk_id`'s shape).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hunk: Option<String>,
    /// A finding's `f-*` slug.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finding: Option<String>,
    // annotation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotation: Option<String>,
    // turn
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<String>,
    // bookmark
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bookmark: Option<i64>,
    // group
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub members: Option<Vec<String>>,
    // link
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EdgeIn {
    pub from: String,
    pub to: String,
    pub kind: String,
    #[serde(default)]
    pub label: Option<String>,
    /// `authored` (default) or `derived`.
    #[serde(default)]
    pub provenance: Option<String>,
    /// Only meaningful on a `derived` edge; refused on an authored one.
    #[serde(default)]
    pub trust: Option<String>,
}

impl EdgeIn {
    pub fn provenance_or_default(&self) -> &str {
        self.provenance.as_deref().unwrap_or(PROVENANCE_AUTHORED)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StepIn {
    pub node: String,
    #[serde(default)]
    pub caption: Option<String>,
}

// --- identifiers ----------------------------------------------------------

/// A slug / node id: 1..=[`MAX_ID_LEN`] chars of `[a-z0-9]`, `-` or `_`,
/// starting with an alphanumeric. Deliberately narrow — a board slug rides
/// a URL path segment and a node id rides a JSON Canvas node id, an HTML
/// fragment id and a Markdown anchor, so anything needing escaping in any
/// of those is refused at the door rather than escaped in four places.
pub fn is_valid_id(s: &str) -> bool {
    if s.is_empty() || s.len() > MAX_ID_LEN {
        return false;
    }
    let mut chars = s.chars();
    let first = chars.next().expect("non-empty");
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// The canonical content hash an apply compares to decide `unchanged`.
///
/// Computed over a CANONICAL rendering of the document — serde_json's
/// object key order is the struct's declaration order (stable), `pins` is a
/// `BTreeMap` (sorted), and nodes/edges/steps keep the author's own order
/// because their order is CONTENT (it is the fallback reading order). The
/// fields deliberately EXCLUDED are the ones an apply does not own:
/// `status` (a transition, not authored state) and `repo` (addressing).
pub fn content_hash(doc: &BoardDoc) -> String {
    #[derive(Serialize)]
    struct Canonical<'a> {
        slug: &'a str,
        title: &'a str,
        description_md: &'a str,
        authored_ref: Option<&'a str>,
        nodes: &'a [NodeIn],
        edges: &'a [EdgeIn],
        steps: &'a [StepIn],
        pins: &'a std::collections::BTreeMap<String, Pin>,
    }
    let canonical = Canonical {
        slug: &doc.slug,
        title: &doc.title,
        description_md: &doc.description_md,
        authored_ref: doc.authored_ref.as_deref(),
        nodes: &doc.nodes,
        edges: &doc.edges,
        steps: &doc.steps,
        pins: &doc.pins,
    };
    // `to_string` on a struct with no map-typed field but `pins` (a
    // BTreeMap) is deterministic; a failure here is impossible for these
    // types, and falling back to an empty hash would silently make every
    // apply look "changed" rather than crashing, which is the safer of the
    // two wrong answers.
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    blake3::hash(&bytes).to_hex().to_string()
}

/// The guard hash over a `code` node's primary-range bytes.
pub fn guard_hash(range_bytes: &[u8]) -> String {
    blake3::hash(range_bytes).to_hex().to_string()
}

/// The 1-based inclusive line slice `[a, b]` of `content`, joined with
/// `\n`. Clamps to the file's real length; an entirely out-of-range span
/// yields `""` (the caller reports that as an orphan, never as an empty
/// card that looks resolved).
pub fn line_slice(content: &str, range: [u32; 2]) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let start = range[0].max(1) as usize - 1;
    let end = (range[1] as usize).min(lines.len());
    if start >= lines.len() || start >= end {
        return String::new();
    }
    lines[start..end].join("\n")
}

// --- route contracts (invariant 15) ---------------------------------------

use crate::entities::RouteContract;

pub const BOARDS_LIST_ROUTE: RouteContract = RouteContract {
    path: "/api/boards",
    handler: "boards::routes::list_boards",
    required_params: &["repo"],
    params_accept_without: routes::list_params_accept_without,
};

pub const BOARD_GET_ROUTE: RouteContract = RouteContract {
    path: "/api/boards/{slug}",
    handler: "boards::routes::get_board",
    required_params: &["repo"],
    params_accept_without: routes::get_params_accept_without,
};

pub const BOARD_EXPORT_ROUTE: RouteContract = RouteContract {
    path: "/api/boards/{slug}/export",
    handler: "boards::routes::export_board",
    required_params: &["repo", "format"],
    params_accept_without: routes::export_params_accept_without,
};

pub const BOARD_SWEEP_ROUTE: RouteContract = RouteContract {
    path: "/api/boards/sweep",
    handler: "boards::routes::sweep_boards",
    required_params: &["repo"],
    params_accept_without: routes::sweep_params_accept_without,
};

/// This unit's declared route surface — invariant 15's list, living beside
/// the routes rather than in a test file so it is added in the SAME edit
/// that adds a route. The four READS are here; the four loopback-only
/// MUTATIONS (`apply`/`accept`/`archive`/`DELETE`) are deliberately absent
/// for the reason `lanes::V72_H4A_ROUTES` records for its own ingest route:
/// a `RouteContract` describes a query-param surface a GET verb can be
/// walked against, and a POST whose payload IS the contract has nothing for
/// `params_accept_without` to say.
pub const V74_L1_ROUTES: &[RouteContract] = &[
    BOARDS_LIST_ROUTE,
    BOARD_GET_ROUTE,
    BOARD_EXPORT_ROUTE,
    BOARD_SWEEP_ROUTE,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_vocabularies_are_closed_and_disjointly_validated() {
        for k in NODE_KINDS {
            assert!(is_valid_node_kind(k));
        }
        assert!(!is_valid_node_kind("session"));
        assert!(!is_valid_node_kind(""));
        for k in EDGE_KINDS {
            assert!(is_valid_edge_kind(k));
        }
        assert!(!is_valid_edge_kind("calls "));
        assert!(!is_valid_edge_kind("uses"));
        // No name is in both vocabularies — a `kind` field is unambiguous
        // about which table it is talking about.
        for k in NODE_KINDS {
            assert!(!EDGE_KINDS.contains(&k), "{k:?} is both a node and an edge");
        }
    }

    #[test]
    fn every_appliable_status_is_a_status_and_the_transitions_are_not() {
        for s in APPLIABLE_STATUSES {
            assert!(is_valid_status(s));
        }
        assert!(!APPLIABLE_STATUSES.contains(&STATUS_ACCEPTED));
        assert!(!APPLIABLE_STATUSES.contains(&STATUS_ARCHIVED));
        assert!(is_valid_status(STATUS_ACCEPTED));
        assert!(!is_valid_status("Accepted"));
    }

    #[test]
    fn ids_reject_everything_that_would_need_escaping_downstream() {
        for ok in ["a", "n-place", "g_domain", "0", "checkout-flow-2"] {
            assert!(is_valid_id(ok), "{ok:?} must be a valid id");
        }
        for bad in [
            "",
            "-leading",
            "_leading",
            "Upper",
            "has space",
            "sl/ash",
            "dot.dot",
            "../escape",
            "quote\"",
            "<script>",
            "n%41",
        ] {
            assert!(!is_valid_id(bad), "{bad:?} must be refused");
        }
        assert!(is_valid_id(&"a".repeat(MAX_ID_LEN)));
        assert!(!is_valid_id(&"a".repeat(MAX_ID_LEN + 1)));
    }

    fn doc() -> BoardDoc {
        BoardDoc {
            schema: SCHEMA.to_string(),
            repo: "r".into(),
            slug: "b".into(),
            title: "T".into(),
            description_md: String::new(),
            status: None,
            authored_ref: None,
            nodes: vec![NodeIn {
                id: "n1".into(),
                kind: KIND_NOTE.into(),
                title: None,
                body_md: Some("hi".into()),
                group: None,
                thread_id: None,
                reference: RefFields::default(),
            }],
            edges: Vec::new(),
            steps: Vec::new(),
            pins: Default::default(),
        }
    }

    #[test]
    fn content_hash_ignores_status_and_repo_but_not_content() {
        let a = doc();
        let mut b = a.clone();
        b.status = Some(STATUS_DRAFT.into());
        b.repo = "other".into();
        assert_eq!(
            content_hash(&a),
            content_hash(&b),
            "status and repo are not authored CONTENT — a re-apply that only \
             differs in them must be `unchanged`"
        );
        let mut c = a.clone();
        c.nodes[0].body_md = Some("hi!".into());
        assert_ne!(content_hash(&a), content_hash(&c));
        let mut d = a.clone();
        d.pins.insert("n1".into(), Pin { x: 1.0, y: 2.0 });
        assert_ne!(content_hash(&a), content_hash(&d));
    }

    #[test]
    fn content_hash_is_stable_across_runs_and_sensitive_to_node_order() {
        let a = doc();
        assert_eq!(content_hash(&a), content_hash(&a.clone()));
        let mut two = a.clone();
        two.nodes.push(NodeIn {
            id: "n2".into(),
            kind: KIND_NOTE.into(),
            title: None,
            body_md: Some("second".into()),
            group: None,
            thread_id: None,
            reference: RefFields::default(),
        });
        let mut swapped = two.clone();
        swapped.nodes.swap(0, 1);
        assert_ne!(
            content_hash(&two),
            content_hash(&swapped),
            "node ORDER is content — it is the fallback reading order"
        );
    }

    #[test]
    fn line_slice_clamps_rather_than_panicking() {
        let src = "a\nb\nc\nd";
        assert_eq!(line_slice(src, [2, 3]), "b\nc");
        assert_eq!(line_slice(src, [1, 99]), "a\nb\nc\nd");
        assert_eq!(line_slice(src, [99, 100]), "");
        assert_eq!(line_slice(src, [0, 1]), "a");
        assert_eq!(line_slice(src, [3, 2]), "");
        assert_eq!(line_slice("", [1, 1]), "");
    }

    #[test]
    fn an_unknown_document_key_fails_the_parse_rather_than_being_dropped() {
        let err = serde_json::from_str::<BoardDoc>(
            r#"{"schema":"kbc-canvas/1","repo":"r","slug":"b","title":"T","nodez":[]}"#,
        )
        .expect_err("an unknown key must fail");
        assert!(err.to_string().contains("nodez"), "{err}");
        // …and a node's own unknown key does too (the field that would
        // silently swallow a mistyped `range`).
        let err = serde_json::from_str::<BoardDoc>(
            r#"{"schema":"kbc-canvas/1","repo":"r","slug":"b","title":"T",
                "nodes":[{"id":"n1","kind":"code","ranges":[1,2]}]}"#,
        )
        .expect_err("an unknown node key must fail");
        assert!(err.to_string().contains("ranges"), "{err}");
    }

    #[test]
    fn every_declared_route_path_is_api_nested_and_distinct() {
        assert!(!V74_L1_ROUTES.is_empty());
        let mut seen = std::collections::BTreeSet::new();
        for c in V74_L1_ROUTES {
            assert!(c.path.starts_with("/api/"), "{}", c.path);
            assert!(seen.insert(c.path), "duplicate contract for {}", c.path);
        }
    }
}
