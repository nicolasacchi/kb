//! V74-L1 — the READ half: every node re-resolved through the Ladder, now.
//!
//! Nothing this module computes is ever written back. That is not a
//! performance choice; it is the same rule root invariant #2 states for the
//! whole doc↔code bridge and that `lanes::classing` (invariant 21) and
//! `entities::class_for` (invariant 13) already hold in this crate: a stale
//! fact must never read as a fresh one, so the freshness verdict is
//! recomputed from the live tree on every read and the store holds only the
//! CLAIM.
//!
//! # The Ladder is the crate's ONE ladder
//!
//! A `code` node's rungs are `annotations::anchor_for_line` +
//! `annotations::resolve`, guarded by `review_comments::line_matches_snippet`
//! — the same three calls review comments, findings and `aug-lane/1` make.
//! There is deliberately no fourth re-anchoring implementation here.
//!
//! # Five states, and what each of them promises
//!
//! | state | reachable from | means |
//! |---|---|---|
//! | `pinned` | `code` | the claimed bytes are still at the claimed place |
//! | `carried` | `code` | the claimed bytes moved; here is where they are now |
//! | `orphan` | `code`, and the five id-shaped kinds | the target is gone — the card is SHOWN with its last-known text |
//! | `present` | `query`, `hunk`, `finding`, `annotation`, `turn`, `bookmark` | the addressed thing exists |
//! | `inert` | `note`, `group`, `link` | there is nothing to resolve |
//!
//! An orphan is never dropped, never hidden and never silently re-pointed
//! at something nearby. That is the whole product.
//!
//! # What this module deliberately does NOT probe
//!
//! * **A hunk id.** `kbc-hunkid/1` is a CONTENT address the SPA mints from
//!   the diff text (`reviews::is_hunk_id`'s doc). Verifying one would mean
//!   recomputing the patchset diff on every board read, and guessing would
//!   be worse — so `present` for a `hunk` node means the REVIEW and the
//!   PATCHSET exist, and [`NodeOut::note`] says exactly that.
//! * **A session turn's content.** The existence probe reads
//!   `transcript_turns` for the session and matches the `t-<uuid12>` form —
//!   existence ONLY, never a byte of transcript text. The transcripts lane
//!   stays loopback-only; a board is a bearer-readable object and must not
//!   become a side channel into it.
//! * **A query's result set, by default.** Executing a kbcq/1 query per
//!   card on every board read would put a full unified search on every page
//!   load (V71-D1's own bench recorded a 29.6 s cold first search). `?live=1`
//!   opts in, `canvas sweep` always opts in, and the count that comes back
//!   is a PAGE count with `basis: "page"` — the same honesty
//!   `search::results`' facet census already ships, for the same reason.

use super::*;
use crate::annotations::{self, MatchConfidence};
use crate::config::RepoEntry;
use crate::review_comments;
use crate::store::Store;

pub const STATE_PINNED: &str = "pinned";
pub const STATE_CARRIED: &str = "carried";
pub const STATE_ORPHAN: &str = "orphan";
pub const STATE_PRESENT: &str = "present";
pub const STATE_INERT: &str = "inert";

/// The CLOSED state vocabulary.
pub const NODE_STATES: [&str; 5] = [
    STATE_PINNED,
    STATE_CARRIED,
    STATE_ORPHAN,
    STATE_PRESENT,
    STATE_INERT,
];

pub const REASON_BLOB_CURRENT: &str = "blob-current";
pub const REASON_GUARD_MATCH: &str = "guard-match";
pub const REASON_REANCHORED_EXACT: &str = "reanchored-exact";
pub const REASON_REANCHORED_FUZZY: &str = "reanchored-fuzzy";
pub const REASON_PATH_GONE: &str = "path-gone";
pub const REASON_CONTENT_UNREADABLE: &str = "content-unreadable";
pub const REASON_NO_ANCHOR: &str = "no-anchor";
pub const REASON_RANGE_OUTSIDE_FILE: &str = "range-outside-file";
pub const REASON_TARGET_PRESENT: &str = "target-present";
pub const REASON_TARGET_GONE: &str = "target-gone";
pub const REASON_NO_REFERENCE: &str = "no-reference";

/// The CLOSED reason vocabulary. Walked against the resolver by its own
/// test, so a rung cannot be added without appearing here — the
/// `lanes::classing::REASONS` precedent.
pub const NODE_REASONS: [&str; 11] = [
    REASON_BLOB_CURRENT,
    REASON_GUARD_MATCH,
    REASON_REANCHORED_EXACT,
    REASON_REANCHORED_FUZZY,
    REASON_PATH_GONE,
    REASON_CONTENT_UNREADABLE,
    REASON_NO_ANCHOR,
    REASON_RANGE_OUTSIDE_FILE,
    REASON_TARGET_PRESENT,
    REASON_TARGET_GONE,
    REASON_NO_REFERENCE,
];

// --- wire ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CodeCard {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// The range as it is NOW — shifted when the node carried.
    pub range: [u32; 2],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<[u32; 2]>,
    /// The range the author wrote, kept beside the current one so a reader
    /// can see the move rather than infer it.
    pub authored_range: [u32; 2],
    /// Lines the range moved by; `0` for a pinned node.
    pub shifted_by: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authored_blob_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_blob_sha: Option<String>,
    /// The current text of the range (`null` for an orphan — there is no
    /// current text). Capped at [`MAX_SNIPPET_LINES`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// `true` when the range was longer than the cap and the snippet is the
    /// first [`MAX_SNIPPET_LINES`] lines of it.
    pub snippet_truncated: bool,
    /// Cached whole-file highlight spans, CLIPPED to the snippet and
    /// rebased to its byte 0. `None` means "we did not look" — this blob
    /// has no grammar or has not been derived yet — never "no highlights"
    /// (`routes::FileResponse::highlights`' own distinction).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub highlights: Option<Vec<crate::highlight::Span>>,
    /// The one line of text this node was anchored to when it was written.
    /// Present on an ORPHAN too — it is what lets the card keep its text
    /// and say "this code is gone" instead of going blank.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor_snippet: Option<String>,
    /// Present only when `?ctx=1` and the context range resolves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_snippet: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QueryCard {
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authored_count: Option<u32>,
    /// The count the query returns NOW. `None` unless the read was `live`
    /// — never a stale number presented as fresh.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_count: Option<u32>,
    /// `current_count - authored_count`, when both are known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<i64>,
    /// Always `"page"` when `current_count` is present: each lane caps its
    /// own hits, so this is a count over what the search RETURNED, never a
    /// corpus estimate (`search::results`' facet `basis`, same word for the
    /// same reason).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub basis: Option<&'static str>,
    /// `true` when any lane reported it had more.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ThreadOut {
    pub id: String,
    pub resolved: bool,
    pub replies: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NodeOut {
    pub id: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Authored Markdown, VERBATIM and INERT. This daemon never renders it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_md: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// One of [`NODE_STATES`].
    pub state: &'static str,
    /// One of [`NODE_REASONS`].
    pub reason: &'static str,
    /// The human-readable address — what a reader would type to get there.
    pub address: String,
    /// What this resolution could NOT establish, when there is such a
    /// thing. Never decorative: a `hunk` node says its content address was
    /// not verified, and a non-live `query` node says why its count is
    /// absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<CodeCard>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<QueryCard>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread: Option<ThreadOut>,
    /// The authored pin, when there is one. The board itself stays
    /// coordinate-free; this is the override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin: Option<Pin>,
    /// The reference fields as authored — echoed so a reader (and
    /// `canvas export`) never has to re-fetch the document to know what
    /// the node points at.
    #[serde(flatten)]
    pub reference: RefFields,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EdgeOut {
    pub from: String,
    pub to: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub provenance: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StepOut {
    pub node: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

/// Every count on a board, stated once, computed from the resolved nodes.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Honesty {
    pub nodes: usize,
    pub edges: usize,
    pub steps: usize,
    pub pinned: usize,
    pub carried: usize,
    pub orphans: usize,
    pub present: usize,
    pub inert: usize,
    /// Nodes whose snippet was cut at [`MAX_SNIPPET_LINES`].
    pub truncated_snippets: usize,
    /// Pins whose node is now an ORPHAN — the "stale pin" `sweep` reports:
    /// a fixed position held for a card that no longer points anywhere.
    pub stale_pins: usize,
    /// Whether `?live=1` ran the query cards.
    pub live_queries: bool,
    /// The caps this response was computed under, so a reader never has to
    /// guess which number is a limit.
    pub budget: Budget,
    /// Anything this read could not do, in words.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Budget {
    pub max_nodes: usize,
    pub max_edges: usize,
    pub max_snippet_lines: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BoardOut {
    pub schema: &'static str,
    pub repo: String,
    pub slug: String,
    pub title: String,
    pub description_md: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authored_ref: Option<String>,
    pub revision: i64,
    pub content_hash: String,
    pub created_unix: i64,
    pub updated_unix: i64,
    pub nodes: Vec<NodeOut>,
    pub edges: Vec<EdgeOut>,
    pub steps: Vec<StepOut>,
    /// The ONLY geometry on the wire, and it is authored, not derived.
    pub pins: std::collections::BTreeMap<String, Pin>,
    pub honesty: Honesty,
}

// --- resolution ------------------------------------------------------------

/// Everything a resolution needs that is not the node itself. Passed in so
/// [`resolve_node`] stays a pure function of its inputs — the
/// `lanes::classing::class_for` posture, and what makes the state table
/// testable without a repo, a store or a clock.
pub struct Ctx<'a> {
    pub repo: &'a RepoEntry,
    pub store: &'a Store,
    pub repo_id: i64,
    pub want_context: bool,
}

/// The Ladder verdict for ONE node. Reads the working tree for a `code`
/// node and the store for an id-shaped one; touches neither for an inert
/// kind.
pub fn resolve_node(ctx: &Ctx<'_>, row: &crate::store::CanvasNodeRow) -> NodeOut {
    let reference: RefFields = serde_json::from_str(&row.ref_json).unwrap_or_default();
    let mut out = NodeOut {
        id: row.node_id.clone(),
        kind: row.kind.clone(),
        title: row.title.clone(),
        body_md: row.body_md.clone(),
        group: row.group_id.clone(),
        state: STATE_INERT,
        reason: REASON_NO_REFERENCE,
        address: String::new(),
        note: None,
        code: None,
        query: None,
        thread: None,
        pin: match (row.pin_x, row.pin_y) {
            (Some(x), Some(y)) => Some(Pin { x, y }),
            _ => None,
        },
        reference: reference.clone(),
    };
    out.thread = row
        .thread_id
        .as_deref()
        .map(|id| resolve_thread(ctx.store, id));

    match row.kind.as_str() {
        KIND_CODE => resolve_code(ctx, row, &reference, &mut out),
        KIND_QUERY => {
            out.address = reference.query.clone().unwrap_or_default();
            out.state = STATE_PRESENT;
            out.reason = REASON_TARGET_PRESENT;
            out.query = Some(QueryCard {
                query: reference.query.clone().unwrap_or_default(),
                authored_count: reference.authored_count,
                current_count: None,
                delta: None,
                basis: None,
                truncated: None,
            });
            out.note = Some(
                "the count is not re-run on an ordinary read — pass live=1 (or run \
                 `kb-code canvas sweep`) to execute the query"
                    .to_string(),
            );
        }
        KIND_HUNK => {
            let review = reference.review.unwrap_or_default();
            let ps = reference.patchset.unwrap_or_default();
            let hunk = reference.hunk.clone().unwrap_or_default();
            out.address = format!("review {review} ps{ps} hunk {hunk}");
            let exists = ctx
                .store
                .get_review(review)
                .ok()
                .flatten()
                .is_some_and(|_| {
                    ctx.store
                        .get_patchset(review, ps as i64)
                        .ok()
                        .flatten()
                        .is_some()
                });
            set_presence(&mut out, exists);
            out.note = Some(
                "a kbc-hunkid/1 id is a CONTENT address the SPA mints from the diff \
                 text; this daemon verifies the review and the patchset exist and does \
                 not recompute the diff to check the hunk itself"
                    .to_string(),
            );
        }
        KIND_FINDING => {
            let review = reference.review.unwrap_or_default();
            let slug = reference.finding.clone().unwrap_or_default();
            out.address = format!("review {review} finding {slug}");
            let exists = ctx
                .store
                .get_review_finding(review, &slug)
                .ok()
                .flatten()
                .is_some();
            set_presence(&mut out, exists);
        }
        KIND_ANNOTATION => {
            let id = reference.annotation.clone().unwrap_or_default();
            out.address = format!("annotation {id}");
            let exists = ctx.store.get_annotation(&id).ok().flatten().is_some();
            set_presence(&mut out, exists);
        }
        KIND_TURN => {
            let session = reference.session.clone().unwrap_or_default();
            let turn = reference.turn.clone().unwrap_or_default();
            out.address = format!("session {session} turn {turn}");
            let exists = ctx
                .store
                .transcript_turns_for_session(&session)
                .map(|turns| turns.iter().any(|t| turn_id_from_uuid(&t.uuid) == turn))
                .unwrap_or(false);
            set_presence(&mut out, exists);
            out.note = Some(
                "existence only — a board is a bearer-readable object and this probe \
                 never reads a byte of transcript text (the transcripts lane stays \
                 loopback-only)"
                    .to_string(),
            );
        }
        KIND_BOOKMARK => {
            let id = reference.bookmark.unwrap_or_default();
            out.address = format!("bookmark {id}");
            let exists = ctx.store.get_bookmark(id).ok().flatten().is_some();
            set_presence(&mut out, exists);
        }
        KIND_LINK => {
            out.address = reference.url.clone().unwrap_or_default();
        }
        KIND_GROUP => {
            out.address = row
                .title
                .clone()
                .unwrap_or_else(|| format!("group {}", row.node_id));
        }
        // `note`, and anything a future migration relaxes into the column
        // before this match learns about it: inert, addressed by its own id,
        // never silently claimed to resolve.
        _ => {
            out.address = row.node_id.clone();
        }
    }
    out
}

fn set_presence(out: &mut NodeOut, exists: bool) {
    if exists {
        out.state = STATE_PRESENT;
        out.reason = REASON_TARGET_PRESENT;
    } else {
        out.state = STATE_ORPHAN;
        out.reason = REASON_TARGET_GONE;
    }
}

fn resolve_thread(store: &Store, id: &str) -> ThreadOut {
    match store.get_annotation(id) {
        Ok(Some(a)) => ThreadOut {
            id: id.to_string(),
            resolved: a.resolved,
            replies: store.count_annotation_replies(id).unwrap_or(0),
        },
        // A thread id nothing answers to is reported as an empty, unresolved
        // thread rather than dropped — the same "shown, never hidden" rule
        // the node states follow.
        _ => ThreadOut {
            id: id.to_string(),
            resolved: false,
            replies: 0,
        },
    }
}

/// kb-core's own `sessions::view::turn_id_from_uuid` shape — `t-` plus the
/// first 12 hex digits of the turn's uuid. That helper is `pub(crate)` to
/// kb-core, so this crate mints the same string rather than reaching into
/// kb-core's private surface (`annotations::short_random_hex`'s precedent).
/// kb-core's SHA-fallback branch (a merge-group turn with no uuid) is
/// unreachable here: `transcript_turns` stores a uuid per row, and a turn
/// with none is simply not addressable from a board — an honest orphan.
pub fn turn_id_from_uuid(uuid: &str) -> String {
    let hex: String = uuid.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    format!("t-{}", &hex[..hex.len().min(12)])
}

fn resolve_code(
    ctx: &Ctx<'_>,
    row: &crate::store::CanvasNodeRow,
    r: &RefFields,
    out: &mut NodeOut,
) {
    let path = r.path.clone().unwrap_or_default();
    let authored = r.range.unwrap_or([1, 1]);
    out.address = match &r.symbol {
        Some(s) => format!("{path}:{}-{} ({s})", authored[0], authored[1]),
        None => format!("{path}:{}-{}", authored[0], authored[1]),
    };
    let mut card = CodeCard {
        path: path.clone(),
        symbol: r.symbol.clone(),
        range: authored,
        context: r.context,
        authored_range: authored,
        shifted_by: 0,
        authored_blob_sha: r.blob_sha.clone(),
        current_blob_sha: None,
        snippet: None,
        snippet_truncated: false,
        highlights: None,
        anchor_snippet: row.anchor_snippet.clone(),
        context_snippet: None,
    };

    let Ok(read) = crate::routes::read_repo_file(ctx.repo, &path, None) else {
        out.state = STATE_ORPHAN;
        out.reason = REASON_PATH_GONE;
        out.code = Some(card);
        return;
    };
    card.current_blob_sha = Some(read.blob_hash.clone());
    let Ok(text) = String::from_utf8(read.bytes) else {
        out.state = STATE_ORPHAN;
        out.reason = REASON_CONTENT_UNREADABLE;
        out.code = Some(card);
        return;
    };

    let blob_current = r
        .blob_sha
        .as_deref()
        .is_some_and(|claimed| claimed == read.blob_hash);
    let guard_matches = || {
        r.guard_hash
            .as_deref()
            .is_some_and(|g| g == guard_hash(line_slice(&text, authored).as_bytes()))
    };

    let (state, reason, line) = if blob_current {
        (STATE_PINNED, REASON_BLOB_CURRENT, authored[0])
    } else if guard_matches() {
        (STATE_PINNED, REASON_GUARD_MATCH, authored[0])
    } else if let Some(snippet) = row.anchor_snippet.as_deref() {
        if review_comments::line_matches_snippet(&text, authored[0], snippet) {
            // The anchored line is still exactly where it was — the rest of
            // the file moved, this node did not.
            (STATE_PINNED, REASON_REANCHORED_EXACT, authored[0])
        } else {
            let a = annotations::anchor_for_line(authored[0], snippet);
            let resolved = annotations::resolve(&text, &a);
            if resolved.stale {
                (STATE_ORPHAN, REASON_NO_ANCHOR, authored[0])
            } else {
                let verbatim = matches!(resolved.confidence, MatchConfidence::Exact)
                    && review_comments::line_matches_snippet(&text, resolved.line, snippet);
                let reason = if verbatim {
                    REASON_REANCHORED_EXACT
                } else {
                    REASON_REANCHORED_FUZZY
                };
                let state = if resolved.line == authored[0] {
                    STATE_PINNED
                } else {
                    STATE_CARRIED
                };
                (state, reason, resolved.line)
            }
        }
    } else {
        // The blob moved and there is nothing to re-anchor against (no
        // guard, no snippet — an apply that could not read the file at
        // authoring time). Orphan, honestly, rather than a range shown at
        // its old numbers over new bytes.
        (STATE_ORPHAN, REASON_NO_ANCHOR, authored[0])
    };

    out.state = state;
    out.reason = reason;
    if state != STATE_ORPHAN {
        let delta = line as i64 - authored[0] as i64;
        card.shifted_by = delta;
        let end = (authored[1] as i64 + delta).max(line as i64) as u32;
        card.range = [line, end];
        card.context = r.context.map(|[a, b]| {
            [
                ((a as i64 + delta).max(1)) as u32,
                ((b as i64 + delta).max(line as i64)) as u32,
            ]
        });
        let total_lines = text.lines().count() as u32;
        if line > total_lines {
            out.state = STATE_ORPHAN;
            out.reason = REASON_RANGE_OUTSIDE_FILE;
        } else {
            let shown_end = end.min(line.saturating_add(MAX_SNIPPET_LINES as u32 - 1));
            card.snippet_truncated = shown_end < end;
            let snippet = line_slice(&text, [line, shown_end]);
            card.highlights =
                highlights_for_slice(ctx, &path, &read.blob_hash, &text, [line, shown_end]);
            card.snippet = Some(snippet);
            if ctx.want_context {
                if let Some(c) = card.context {
                    card.context_snippet = Some(line_slice(&text, c));
                }
            }
        }
    }
    out.code = Some(card);
}

/// The cached whole-file spans (`Store::highlights_for_blob`), CLIPPED to a
/// line range's byte window and rebased to its byte 0.
///
/// This crate has no per-slice highlighter: `highlight::extract_highlights`
/// runs once at ingest and every route reads the cache (`routes::file`'s own
/// path). Deriving here instead would mean parsing a file per card. A span
/// straddling the window is TRUNCATED to it rather than dropped, so the
/// first and last lines of a card are painted like every other line.
fn highlights_for_slice(
    ctx: &Ctx<'_>,
    path: &str,
    blob_hash: &str,
    text: &str,
    range: [u32; 2],
) -> Option<Vec<crate::highlight::Span>> {
    let lang = crate::lang::detect(path, Some(text.as_bytes()))?;
    let all = ctx
        .store
        .highlights_for_blob(blob_hash, lang.salt)
        .ok()
        .flatten()?;
    // Byte window of the range within the file. `line_slice` joins with
    // `\n`, so the window is the same arithmetic: sum of preceding lines
    // plus their separators.
    let mut offset: usize = 0;
    let mut start_byte: Option<usize> = None;
    let mut end_byte: usize = text.len();
    for (i, l) in text.lines().enumerate() {
        let n = (i + 1) as u32;
        if n == range[0] {
            start_byte = Some(offset);
        }
        offset += l.len() + 1;
        if n == range[1] {
            end_byte = offset.saturating_sub(1).min(text.len());
        }
    }
    let start_byte = start_byte?;
    let clipped: Vec<crate::highlight::Span> = all
        .into_iter()
        .filter_map(|s| {
            let a = s.byte_start as usize;
            let b = a + s.byte_len as usize;
            let a2 = a.max(start_byte);
            let b2 = b.min(end_byte);
            if a2 >= b2 {
                return None;
            }
            Some(crate::highlight::Span {
                byte_start: (a2 - start_byte) as u32,
                byte_len: (b2 - a2) as u32,
                class: s.class,
            })
        })
        .collect();
    Some(clipped)
}

/// Fold the resolved nodes into the board's `honesty` block. Every number
/// is counted from the rows actually returned — none is stored, and none is
/// an estimate.
pub fn honesty(
    nodes: &[NodeOut],
    edges: usize,
    steps: usize,
    live_queries: bool,
    notes: Vec<String>,
) -> Honesty {
    let count = |s: &str| nodes.iter().filter(|n| n.state == s).count();
    let orphan_ids: std::collections::BTreeSet<&str> = nodes
        .iter()
        .filter(|n| n.state == STATE_ORPHAN)
        .map(|n| n.id.as_str())
        .collect();
    Honesty {
        nodes: nodes.len(),
        edges,
        steps,
        pinned: count(STATE_PINNED),
        carried: count(STATE_CARRIED),
        orphans: count(STATE_ORPHAN),
        present: count(STATE_PRESENT),
        inert: count(STATE_INERT),
        truncated_snippets: nodes
            .iter()
            .filter(|n| n.code.as_ref().is_some_and(|c| c.snippet_truncated))
            .count(),
        stale_pins: nodes
            .iter()
            .filter(|n| n.pin.is_some() && orphan_ids.contains(n.id.as_str()))
            .count(),
        live_queries,
        budget: Budget {
            max_nodes: MAX_NODES,
            max_edges: MAX_EDGES,
            max_snippet_lines: MAX_SNIPPET_LINES,
        },
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_and_reason_vocabularies_are_closed_and_used() {
        // Every reason constant appears in the declared list — a rung added
        // without registering it fails here.
        for r in [
            REASON_BLOB_CURRENT,
            REASON_GUARD_MATCH,
            REASON_REANCHORED_EXACT,
            REASON_REANCHORED_FUZZY,
            REASON_PATH_GONE,
            REASON_CONTENT_UNREADABLE,
            REASON_NO_ANCHOR,
            REASON_RANGE_OUTSIDE_FILE,
            REASON_TARGET_PRESENT,
            REASON_TARGET_GONE,
            REASON_NO_REFERENCE,
        ] {
            assert!(NODE_REASONS.contains(&r), "{r} is not in NODE_REASONS");
        }
        assert_eq!(NODE_REASONS.len(), 11);
        // …and the source of THIS module names every one of them, so a
        // constant that stopped being reachable is visible in the diff.
        let src = include_str!("resolve.rs");
        for r in NODE_REASONS {
            assert!(
                src.matches(&format!("\"{r}\"")).count() >= 1,
                "{r} is declared but never produced"
            );
        }
        assert_eq!(NODE_STATES.len(), 5);
    }

    #[test]
    fn turn_ids_take_the_first_twelve_hex_digits() {
        assert_eq!(
            turn_id_from_uuid("9a1f22c0-1111-2222-3333-444455556666"),
            "t-9a1f22c01111"
        );
        // Shorter than 12 hex digits: clamped, never panicking on a slice.
        assert_eq!(turn_id_from_uuid("abc"), "t-abc");
        assert_eq!(turn_id_from_uuid(""), "t-");
        assert_eq!(turn_id_from_uuid("zz-zz"), "t-");
    }

    #[test]
    fn honesty_counts_are_folded_from_the_rows_not_stored() {
        let n = |id: &str, state: &'static str, pin: bool| NodeOut {
            id: id.into(),
            kind: KIND_CODE.into(),
            title: None,
            body_md: None,
            group: None,
            state,
            reason: REASON_BLOB_CURRENT,
            address: String::new(),
            note: None,
            code: None,
            query: None,
            thread: None,
            pin: pin.then_some(Pin { x: 0.0, y: 0.0 }),
            reference: RefFields::default(),
        };
        let nodes = vec![
            n("a", STATE_PINNED, false),
            n("b", STATE_CARRIED, false),
            n("c", STATE_ORPHAN, true),
            n("d", STATE_PRESENT, false),
            n("e", STATE_INERT, true),
        ];
        let h = honesty(&nodes, 3, 2, false, vec![]);
        assert_eq!(h.nodes, 5);
        assert_eq!(
            (h.pinned, h.carried, h.orphans, h.present, h.inert),
            (1, 1, 1, 1, 1)
        );
        assert_eq!(h.edges, 3);
        assert_eq!(h.steps, 2);
        assert_eq!(
            h.stale_pins, 1,
            "a pin held for an orphan is a STALE pin; a pin on a live node is not"
        );
        assert!(!h.live_queries);
        assert_eq!(h.budget.max_nodes, MAX_NODES);
    }
}
