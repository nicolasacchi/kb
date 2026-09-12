//! W4.6 — code annotations: durable, path-scoped line comments on a
//! repo's working-tree file, anchored via kb-core's `review::Anchor` — the
//! SAME tagged-enum/fuzzy-resolve machinery kb's own `.review/*.json`
//! comments use (`crates/kb-core/src/review.rs`, imported wholesale, never
//! copied). This module owns the two things that machinery doesn't already
//! give us for free: constructing an `Anchor` from a plain-text SOURCE
//! line (kb-core's own callers only ever build anchors from HTML), and
//! mapping a resolved anchor back to a current LINE NUMBER (kb-core's
//! `Resolution` only ever returns matched TEXT — see below).
//!
//! # Anchor shape
//!
//! Every annotation here is a `kb_core::review::Anchor::Selection`:
//! - `offset` — the 1-based line number *at creation time*. kb-core's own
//!   resolver never reads this field (only `snippet`/`css_path` feed
//!   `fuzzy_resolve_anchor`); repurposed here as our own bookkeeping so the
//!   original position survives inside the anchor JSON without a second
//!   sqlite column.
//! - `css_path` — always empty. There is no CSS structural path for a
//!   plain source file, so kb-core's path-based duplicate-disambiguation
//!   tiebreak (`resolve_selection_in`'s `path_sim` term) is inert here —
//!   [`locate_line`] below does its own nearest-to-original-line tiebreak
//!   instead.
//! - `snippet` — the line's trimmed text, capped at kb-core's own
//!   `DEFAULT_CONTEXT_CHARS` (200) at construction time. Storing it
//!   PRE-truncated (rather than the full line) matters: kb-core's resolver
//!   caps its comparison NEEDLE to that same width, so an untruncated
//!   snippet longer than 200 chars would never re-compare equal to its own
//!   original line even when the line is byte-for-byte unchanged.
//!
//! # Resolution
//!
//! kb-core's `fuzzy_resolve_anchor_with` parses HTML (`scraper::Html`) and
//! matches `Anchor::Selection` against `p, li, blockquote, td, h1..h6`
//! elements. To reuse it (rather than reimplementing Jaro-Winkler fuzzy
//! matching) against a plain source file, [`resolve`] wraps every current
//! line in a synthetic `<p>` (HTML-escaped, so `<`/`>`/`&` in source code —
//! generics, comparisons, `&mut` — don't corrupt the synthetic markup) and
//! hands the result to kb-core's resolver exactly as-is. The returned
//! [`kb_core::review::Resolution`] carries matched TEXT, not a line index
//! (kb-core has no concept of "line" — an HTML document has none), so
//! [`locate_line`] does a second, cheap pass: scan the current lines for
//! one whose OWN trimmed text equals the resolved text, breaking ties (a
//! duplicated line, e.g. a repeated `}` or blank line) by proximity to the
//! anchor's stored `offset` — the nearest occurrence to where the
//! annotation used to be is the overwhelmingly likely intended one.

//! # Anchor kinds (Phase D-server)
//!
//! v1 shipped exactly one anchor shape (a single `Selection`, "line"
//! kind). D-server adds three more, each still built from the SAME
//! `Selection` primitive above -- none of them is a new kb-core `Anchor`
//! variant, they're new ways THIS module composes/interprets one:
//!
//! - **line** -- unchanged: [`anchor_for_line`] + [`resolve`], exactly as
//!   documented above. Legacy (pre-D-server) rows are indistinguishable
//!   from a freshly-created `line` annotation -- see `store`'s migration
//!   doc for the byte-for-byte pin.
//! - **range** -- TWO independent `Selection`s (start/end line), stored one
//!   in the row's `anchor` column and one in `anchor2`. [`resolve`] each
//!   side separately against the CURRENT working tree, then
//!   [`normalize_range`] sorts them back into `(line, line_end)` order (a
//!   `range` can drift so far that its start ends up textually AFTER its
//!   end) and ORs their `stale` flags.
//! - **symbol** -- the row's `anchor` column holds an ordinary `line`
//!   Selection (the BACKUP), while `anchor2` holds a [`SymbolDescriptor`]
//!   (`{name, container, kind}`) captured from the enclosing symbol at
//!   creation time ([`enclosing_symbol`]). [`resolve_symbol`] re-derives
//!   the descriptor's best current line: an exact name+container match
//!   wins, then a unique name-only match, then the backup Selection's
//!   ordinary fuzzy [`resolve`] (stale exactly when THAT is stale) -- a
//!   symbol annotation follows its function through drift; a line
//!   annotation doesn't.
//! - **diff** -- pins a position inside ONE IMMUTABLE commit blob. `anchor`
//!   holds a `Selection` built from THAT COMMIT's file content (not the
//!   working tree -- see `routes::create_annotation`'s diff branch, which
//!   reads the blob at the resolved full sha); `anchor2` holds a
//!   [`DiffAnchor2`] (`{sha_full}`). Never re-resolved -- [`diff_line`]
//!   just returns the anchor's own recorded `offset`, because there is
//!   nothing to re-resolve against: the commit it's anchored to never
//!   changes.
//!
//! Threads (`parent_id`) and intents are pure metadata on the
//! `annotations` row (no anchor involvement at all) -- see `store`'s
//! migration doc and `routes.rs`'s reply-creation branch for that half of
//! D-server.

use crate::extract::Symbol;
use kb_core::review::{fuzzy_resolve_anchor_with, Anchor, Resolution, DEFAULT_CONTEXT_CHARS};
use serde::{Deserialize, Serialize};

/// `ann_` + 12 hex chars (6 random bytes) — mirrors kb-core's own
/// `review::new_comment_id`/`short_random_hex` shape (that helper is
/// `pub(crate)` to kb-core, so this crate mints its own rather than
/// reaching into kb-core's private surface). Collisions only matter within
/// one repo/path's annotation set; 48 bits of entropy is ample.
pub fn new_annotation_id() -> String {
    format!("ann_{}", short_random_hex())
}

/// `pub(crate)` — reused by `reading_sets::new_set_id` (Phase E3) for the
/// SAME "opaque short random id" shape, rather than a second copy of this
/// two-line generator.
pub(crate) fn short_random_hex() -> String {
    let mut buf = [0u8; 6];
    if getrandom::fill(&mut buf).is_err() {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        return format!("{n:08x}");
    }
    hex::encode(buf)
}

/// Build a `Selection` anchor for `line_1based` given that line's own
/// current text — see the module doc for the field-repurposing contract.
/// `line_text` is expected to be exactly one line (no trailing `\n`); the
/// caller (`routes::create_annotation`) splits the working-tree content and
/// hands in the specific line.
pub fn anchor_for_line(line_1based: u32, line_text: &str) -> Anchor {
    let trimmed = line_text.trim();
    let snippet: String = trimmed.chars().take(DEFAULT_CONTEXT_CHARS).collect();
    Anchor::Selection {
        css_path: String::new(),
        offset: line_1based,
        snippet,
    }
}

/// PRR-R3 — kb-core's own `Resolution::Exact`/`Resolution::Fuzzy` distinction
/// (`kb_core::review::Resolution`), threaded through [`Resolved`] so a
/// caller (findings' `resolution.confidence`, design doc §2 row 9) can
/// surface it WITHOUT a second resolution algorithm — the SAME
/// `fuzzy_resolve_anchor_with` call [`resolve`] already makes is the only
/// source of this value, never re-derived. Meaningless (kept at whatever the
/// constructor happened to set) when `stale = true` — a stale result has no
/// match to grade at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchConfidence {
    /// The anchor's exact snippet text was found verbatim (kb-core's
    /// `Resolution::Exact`) — including a pure line-shift where the text
    /// itself never changed, just moved.
    Exact,
    /// Only a Jaro-Winkler-similar (not verbatim) match was found
    /// (kb-core's `Resolution::Fuzzy`), or the match came from a
    /// structurally-exact but non-textual source that still degrades to
    /// this tier (see [`resolve_symbol`]'s unique-name-only branch).
    Fuzzy,
}

/// Outcome of [`resolve`]: the best-known current line for an anchor, and
/// whether kb-core's resolver considers the anchored content GONE
/// (`stale = true`). `line` still carries a best-effort value when stale —
/// the anchor's own original `offset` — so a caller always has something
/// to display rather than a hole in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    pub line: u32,
    pub stale: bool,
    /// PRR-R3 — see [`MatchConfidence`]'s own doc. Undefined-but-harmless
    /// when `stale`.
    pub confidence: MatchConfidence,
}

/// Re-resolve `anchor` against `current_content` (the file's CURRENT
/// working-tree bytes, as text) — see the module doc for the synthetic-HTML
/// reuse trick. Non-`Selection` anchors (this module never constructs one,
/// but the type is open) fall back to kb-core's own resolution directly:
/// `File` is always fresh at line 1; `Chapter`/`Section` have no source-line
/// meaning, so they resolve to the anchor's own recorded line (best-effort,
/// never stale by construction, since a plain source file will never
/// produce those scopes today).
pub fn resolve(current_content: &str, anchor: &Anchor) -> Resolved {
    let original_line = original_line_hint(anchor);
    let Anchor::Selection { .. } = anchor else {
        return Resolved {
            line: original_line,
            stale: false,
            confidence: MatchConfidence::Exact,
        };
    };

    let lines: Vec<&str> = current_content.lines().collect();
    let html = synthetic_html(&lines);
    let mut doc_slot = None;
    match fuzzy_resolve_anchor_with(&mut doc_slot, &html, anchor) {
        Resolution::Stale => Resolved {
            line: original_line,
            stale: true,
            confidence: MatchConfidence::Fuzzy,
        },
        Resolution::Exact(text) => match locate_line(&lines, &text, original_line) {
            Some(line) => Resolved {
                line,
                stale: false,
                confidence: MatchConfidence::Exact,
            },
            // kb-core found a match but the 200-char comparison cap (see
            // the module doc) means our own equality-based re-scan can
            // miss it for a line longer than that cap — still genuinely
            // resolved, just imprecisely placed.
            None => Resolved {
                line: original_line,
                stale: false,
                confidence: MatchConfidence::Exact,
            },
        },
        Resolution::Fuzzy(text, _) => match locate_line(&lines, &text, original_line) {
            Some(line) => Resolved {
                line,
                stale: false,
                confidence: MatchConfidence::Fuzzy,
            },
            None => Resolved {
                line: original_line,
                stale: false,
                confidence: MatchConfidence::Fuzzy,
            },
        },
    }
}

fn original_line_hint(anchor: &Anchor) -> u32 {
    match anchor {
        Anchor::Selection { offset, .. } => *offset,
        _ => 1,
    }
}

// --- Phase D-server: anchor kinds, threads, intents ------------------------

/// v1's implicit behavior, now a named kind — see the module doc.
pub const ANCHOR_KIND_LINE: &str = "line";
pub const ANCHOR_KIND_RANGE: &str = "range";
pub const ANCHOR_KIND_SYMBOL: &str = "symbol";
pub const ANCHOR_KIND_DIFF: &str = "diff";
/// PRR-R1 (kb v0.39, T2 "The PR Room") — a finding with no locatable line
/// (a `review_findings.location_kind = "whole_file"` finding — recon-r4's
/// bare-path / whole-file variant). `anchor` stores the bare PATH (not a
/// JSON `Anchor::Selection` — there is no line to select), `anchor2` is
/// always `None`. Resolution (a later phase's `resolve_for_ps` extension)
/// treats this as resolved iff the path still exists in the target
/// patchset's tree — no line claim is ever made, so there is nothing to go
/// stale line-wise, only present/absent.
pub const ANCHOR_KIND_WHOLE_FILE: &str = "whole_file";
/// PRR-R1 / R3 — a REVIEW-scoped, path-less annotation (`path = ""`,
/// `anchor` empty/unused, `anchor2 = None`): the "Ask the agent" general
/// question a human asks about the review as a whole rather than about one
/// line. Always resolved (there is no anchor to go stale) — a later phase
/// wires the actual resolution/grouping behavior; this crate's vocab
/// validators accept the value starting here so store rows using it are not
/// rejected by a stale allow-list.
pub const ANCHOR_KIND_REVIEW: &str = "review";
/// V70-A10 ("Workspaces v0") — a WORKSPACE-scoped, path-less general note
/// (`path = ""`, `anchor` empty/unused, `anchor2 = None`): the exact same
/// shape as [`ANCHOR_KIND_REVIEW`] above, one level down (a workspace
/// instead of a review) — `routes::assemble_top_level_annotation`'s
/// dedicated branch requires `set_id` instead of `review_id`. A
/// CODE-anchored workspace note does NOT use this kind: it rides the
/// ordinary `line`/`range`/`symbol` kinds above with `set_id` set
/// alongside (see `store::AnnotationRow::set_id`'s doc) — this kind exists
/// ONLY for the path-less "general note about the workspace as a whole"
/// case, keeping the one-anchor-grammar rule (kb #6/#25: one anchor type)
/// intact rather than growing a second, workspace-flavoured anchor shape.
/// Always resolved (there is no anchor to go stale).
pub const ANCHOR_KIND_SET: &str = "set";

/// The full `anchor_kind` vocabulary — see the migration's doc. Order
/// matches the phase spec, no significance beyond documentation.
pub const ANCHOR_KINDS: [&str; 7] = [
    ANCHOR_KIND_LINE,
    ANCHOR_KIND_RANGE,
    ANCHOR_KIND_SYMBOL,
    ANCHOR_KIND_DIFF,
    ANCHOR_KIND_WHOLE_FILE,
    ANCHOR_KIND_REVIEW,
    ANCHOR_KIND_SET,
];

pub const INTENT_NOTE: &str = "note";
pub const INTENT_QUESTION: &str = "question";
pub const INTENT_TODO: &str = "todo";
pub const INTENT_FLAG_FOR_AGENT: &str = "flag-for-agent";
pub const INTENT_TOUR_STOP: &str = "tour-stop";
/// PRR-R1 — a `review_findings`-owned annotation (see
/// `store::insert_review_finding_on`). Behaves exactly like any other
/// top-level annotation for threading/resolve purposes; the
/// severity/category/slug/disposition fields that make a finding a finding
/// live on the sibling `review_findings` row, never here (V0024's own
/// "field pollution" rationale).
pub const INTENT_FINDING: &str = "finding";
/// V72-J2 (D8's "claim → annotation bridge") — an annotation minted FROM a
/// comments/1 `annotation`-kind comment (a TODO-family keyword hit) via
/// `POST /api/annotations`, never server-synthesized. "Add a value, not a
/// table": the bridge needs a durable, queryable way to tell "this
/// annotation tracks a source comment" apart from an ordinary note/question,
/// and this crate's own `is_valid_intent` is route-boundary string
/// validation with no `CHECK` constraint behind it (see that fn's doc), so
/// widening the vocabulary by one value is the whole change — no migration,
/// no new table. The bridge's four read states (open/tracked/resolved/gone)
/// are DERIVED client-side per render from a comment+annotation join keyed
/// on the annotation's own live-resolved `line` (this module's `resolve`,
/// above) against the comments/1 scan's current line — never stored here.
pub const INTENT_CLAIM: &str = "claim";

/// The full `intent` vocabulary — see the migration's doc.
pub const INTENTS: [&str; 7] = [
    INTENT_NOTE,
    INTENT_QUESTION,
    INTENT_TODO,
    INTENT_FLAG_FOR_AGENT,
    INTENT_TOUR_STOP,
    INTENT_FINDING,
    INTENT_CLAIM,
];

/// Route-boundary validation for `anchor_kind` — stringly-typed per this
/// crate's house convention (see e.g. `join::ladder::Confidence`'s own
/// `as_str`/`parse`), never a SQL `CHECK` constraint (the migration's doc
/// explains why).
pub fn is_valid_anchor_kind(s: &str) -> bool {
    ANCHOR_KINDS.contains(&s)
}

/// Route-boundary validation for `intent` — same convention as
/// [`is_valid_anchor_kind`].
pub fn is_valid_intent(s: &str) -> bool {
    INTENTS.contains(&s)
}

/// `anchor2`'s payload for a `symbol` annotation — captured from the
/// enclosing symbol at creation time ([`enclosing_symbol`]), re-matched
/// against the CURRENT blob's symbols by [`resolve_symbol`]. `kind` is the
/// symbol's own kind (`fn`/`struct`/`method`/... — `extract::Symbol::kind`'s
/// vocabulary), unrelated to this annotation's `anchor_kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolDescriptor {
    pub name: String,
    pub container: Option<String>,
    pub kind: String,
}

/// `anchor2`'s payload for a `diff` annotation — the full (never
/// abbreviated) commit sha this annotation is pinned to. A dedicated
/// one-field struct (rather than a bare string) so `anchor2`'s JSON shape
/// stays self-describing across every kind, and so a future field (e.g. a
/// captured commit subject, if a "diff" panel ever wants one without a
/// second git call) has somewhere to go without changing the wire contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffAnchor2 {
    pub sha_full: String,
}

/// The INNERMOST symbol enclosing `line` (1-based) — the smallest
/// `[line_start, line_end]` span that contains it, ties broken by scan
/// order (`extract::extract_symbols`'s own deterministic byte-position
/// order — see that module's doc). `None` when no symbol in `symbols`
/// contains `line` at all (an empty file, a line outside every symbol's
/// span, or a language with no symbols indexed yet) — `routes::
/// create_annotation`'s symbol-kind branch turns that into an honest 404 so
/// the client can fall back to a plain `line` annotation, per the phase
/// spec ("that's the point": an enclosing symbol is a real semantic claim,
/// not a best-effort guess).
pub fn enclosing_symbol(symbols: &[Symbol], line: u32) -> Option<&Symbol> {
    symbols
        .iter()
        .filter(|s| s.line_start <= line && line <= s.line_end)
        .min_by_key(|s| s.line_end.saturating_sub(s.line_start))
}

/// Re-resolve a `symbol` annotation's [`SymbolDescriptor`] against the
/// CURRENT blob's symbol table — the "follows the function through drift"
/// ladder from the module doc:
///
/// 1. An exact `(name, container)` match — the overwhelmingly common case
///    (the enclosing construct is unchanged or only its BODY moved/shifted,
///    which never touches its own name/container).
/// 2. Else, a UNIQUE name-only match (the container changed — e.g. the
///    symbol moved to a different `impl`/`mod`/`class` block — but the name
///    is still unambiguous file-wide).
/// 3. Else, fall back to the backup `line` Selection's ordinary [`resolve`]
///    (the symbol itself is gone/renamed/duplicated-by-name) — `stale`
///    mirrors THAT fallback's own `stale`, never forced.
///
/// `current_symbols` is a pure STORE lookup by the caller (never derived on
/// the spot here — see `routes.rs`'s `GET /api/file` doc for why this crate
/// never derives symbols inside a request handler), so an unindexed blob
/// (empty `current_symbols`) falls straight through to step 3.
pub fn resolve_symbol(
    current_symbols: &[Symbol],
    descriptor: &SymbolDescriptor,
    fallback_anchor: &Anchor,
    current_content: &str,
) -> Resolved {
    if let Some(sym) = current_symbols
        .iter()
        .find(|s| s.name == descriptor.name && s.container == descriptor.container)
    {
        return Resolved {
            line: sym.line_start,
            stale: false,
            // A structural exact (name, container) match — not a textual
            // fuzzy match, so it grades Exact regardless of how far the
            // symbol's BODY drifted.
            confidence: MatchConfidence::Exact,
        };
    }
    let mut name_matches = current_symbols.iter().filter(|s| s.name == descriptor.name);
    if let Some(only) = name_matches.next() {
        if name_matches.next().is_none() {
            return Resolved {
                line: only.line_start,
                stale: false,
                // Still a structural exact match on the NAME (the
                // container changed, the name did not) — Fuzzy is
                // reserved for textual similarity, which this isn't.
                confidence: MatchConfidence::Exact,
            };
        }
    }
    resolve(current_content, fallback_anchor)
}

/// Normalize a `range` annotation's two INDEPENDENTLY-resolved endpoints
/// back into `(line, line_end, stale)` — the smaller resolved line is
/// always `line`, the larger `line_end`, even if drift has pushed the
/// original START past the original END (each side is re-resolved on its
/// own merits — see the module doc — so nothing but a post-hoc sort
/// guarantees the pair stays ordered). `stale` is true when EITHER side is.
pub fn normalize_range(start: Resolved, end: Resolved) -> (u32, u32, bool) {
    let (lo, hi) = if start.line <= end.line {
        (start.line, end.line)
    } else {
        (end.line, start.line)
    };
    (lo, hi, start.stale || end.stale)
}

/// The 1-based line as originally recorded for a `diff` annotation — NEVER
/// re-resolved (see the module doc's "diff" section): a diff annotation
/// pins a position in one immutable commit blob, so there is nothing to
/// drift against and no `current_content` parameter to pass in.
pub fn diff_line(anchor: &Anchor) -> u32 {
    original_line_hint(anchor)
}

/// Wrap each line in an HTML-escaped `<p>` so kb-core's `scraper`-backed
/// resolver (which selects `p, li, blockquote, td, h1..h6`) has one
/// candidate block per source line, in document order. Escaping is
/// required — unescaped `<`/`>`/`&` (endemic to source code: generics,
/// comparisons, `&mut`, string literals) would otherwise be parsed as
/// markup rather than round-tripped back out through `el.text()`.
fn synthetic_html(lines: &[&str]) -> String {
    let mut out = String::with_capacity(lines.iter().map(|l| l.len() + 8).sum());
    out.push_str("<html><body>");
    for line in lines {
        out.push_str("<p>");
        escape_html(line, &mut out);
        out.push_str("</p>");
    }
    out.push_str("</body></html>");
    out
}

fn escape_html(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

/// Find the current line whose OWN trimmed text equals `want` (the text
/// kb-core's resolver matched), preferring the occurrence CLOSEST to
/// `hint_1based` when more than one line ties (a duplicated line — a
/// repeated `}` or blank line is the common case). `None` only when no
/// current line's trimmed text equals `want` at all (the 200-char
/// comparison-cap edge case documented on [`resolve`]).
fn locate_line(lines: &[&str], want: &str, hint_1based: u32) -> Option<u32> {
    let mut best: Option<(u32, u32)> = None; // (line, distance-from-hint)
    for (i, l) in lines.iter().enumerate() {
        if l.trim() != want {
            continue;
        }
        let line_no = (i + 1) as u32;
        let dist = line_no.abs_diff(hint_1based);
        if best.is_none_or(|(_, best_dist)| dist < best_dist) {
            best = Some((line_no, dist));
        }
    }
    best.map(|(line, _)| line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_annotation_id_has_the_expected_shape() {
        let id = new_annotation_id();
        assert!(id.starts_with("ann_"));
        assert_eq!(id.len(), "ann_".len() + 12);
        assert!(id["ann_".len()..].chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(new_annotation_id(), new_annotation_id());
    }

    #[test]
    fn anchor_for_line_trims_and_caps_the_snippet() {
        let anchor = anchor_for_line(5, "   fn foo() {}   ");
        match anchor {
            Anchor::Selection {
                css_path,
                offset,
                snippet,
            } => {
                assert_eq!(css_path, "");
                assert_eq!(offset, 5);
                assert_eq!(snippet, "fn foo() {}");
            }
            other => panic!("expected Selection, got {other:?}"),
        }

        let long_line = "x".repeat(500);
        let anchor = anchor_for_line(1, &long_line);
        match anchor {
            Anchor::Selection { snippet, .. } => {
                assert_eq!(snippet.chars().count(), DEFAULT_CONTEXT_CHARS);
            }
            other => panic!("expected Selection, got {other:?}"),
        }
    }

    fn content(lines: &[&str]) -> String {
        lines.join("\n")
    }

    #[test]
    fn resolve_stays_fresh_when_content_is_unchanged() {
        let src = content(&["fn a() {}", "fn b() {}", "fn c() {}"]);
        let anchor = anchor_for_line(2, "fn b() {}");
        let resolved = resolve(&src, &anchor);
        assert_eq!(resolved.line, 2);
        assert!(!resolved.stale);
        assert_eq!(resolved.confidence, MatchConfidence::Exact);
    }

    #[test]
    fn resolve_shifts_the_line_when_content_is_inserted_above() {
        let anchor = anchor_for_line(2, "fn b() {}");
        // Insert 5 lines above the anchored line — it now lives at line 7.
        let src = content(&[
            "// header 1",
            "// header 2",
            "// header 3",
            "// header 4",
            "// header 5",
            "fn a() {}",
            "fn b() {}",
            "fn c() {}",
        ]);
        let resolved = resolve(&src, &anchor);
        assert_eq!(resolved.line, 7);
        assert!(!resolved.stale);
        // PRR-R3 — a pure shift (the text itself is byte-identical, just
        // relocated) is still an EXACT kb-core match, not Fuzzy: Fuzzy is
        // reserved for genuine textual similarity, never mere movement.
        assert_eq!(resolved.confidence, MatchConfidence::Exact);
    }

    /// PRR-R3 — a genuinely SIMILAR-but-not-identical line (one word
    /// changed) must resolve at Jaro-Winkler `Fuzzy` confidence, not
    /// `Exact` — the counterpart to the shift-is-still-exact case above.
    #[test]
    fn resolve_reports_fuzzy_confidence_for_a_genuinely_similar_but_changed_line() {
        let anchor = anchor_for_line(1, "the quick brown fox jumps over the lazy dog");
        let src = content(&["the quick brown fox leaps over the lazy dog"]);
        let resolved = resolve(&src, &anchor);
        assert!(!resolved.stale, "one changed word should still fuzzy-match");
        assert_eq!(resolved.confidence, MatchConfidence::Fuzzy);
    }

    #[test]
    fn resolve_goes_stale_when_the_anchored_content_is_deleted() {
        // Deliberately NOT a "fn a/b/c() {}"-shaped sibling here: kb-core's
        // Jaro-Winkler resolver is tuned for prose, and two boilerplate
        // lines differing by a single character (`fn a() {}` vs `fn b()
        // {}`) score ABOVE the fuzzy threshold — a genuinely different
        // decoy is what "deleted, nothing plausible left" looks like.
        let anchor = anchor_for_line(2, "the quick brown fox jumps over the lazy dog");
        let src = content(&["1", "2"]);
        let resolved = resolve(&src, &anchor);
        assert!(resolved.stale);
        // Best-effort fallback to the original recorded line.
        assert_eq!(resolved.line, 2);
    }

    #[test]
    fn resolve_prefers_the_duplicate_nearest_the_original_line() {
        let anchor = anchor_for_line(3, "}");
        let src = content(&["fn a() {", "}", "fn b() {", "}", "fn c() {", "}"]);
        // "}" appears on lines 2, 4, and 6 — the original anchor was line 3,
        // so the nearest duplicate (line 4, distance 1) must win over line
        // 2 (distance 1 too, but line 4 is scanned... exercise both ties by
        // checking the nearest of the two closest candidates is picked).
        let resolved = resolve(&src, &anchor);
        assert!(!resolved.stale);
        assert!(
            resolved.line == 2 || resolved.line == 4,
            "expected one of the two nearest duplicates (2 or 4), got {}",
            resolved.line
        );
    }

    #[test]
    fn resolve_handles_html_special_characters_in_source_lines() {
        let anchor = anchor_for_line(1, "fn cmp<T: PartialOrd>(a: &T, b: &T) -> bool {");
        let src = content(&[
            "fn cmp<T: PartialOrd>(a: &T, b: &T) -> bool {",
            "    a < b",
            "}",
        ]);
        let resolved = resolve(&src, &anchor);
        assert_eq!(resolved.line, 1);
        assert!(!resolved.stale);
    }

    // --- Phase D-server: vocab, symbol/range/diff helpers -------------------

    #[test]
    fn anchor_kind_vocab_accepts_exactly_the_seven_kinds() {
        for k in [
            "line",
            "range",
            "symbol",
            "diff",
            "whole_file",
            "review",
            "set",
        ] {
            assert!(is_valid_anchor_kind(k), "{k} should be valid");
        }
        for k in [
            "Line",
            "diffs",
            "",
            "selection",
            "chapter",
            "whole-file",
            "Review",
            "Set",
        ] {
            assert!(!is_valid_anchor_kind(k), "{k} should be invalid");
        }
    }

    /// PRR-R1 / V70-A10 — pins the D-server-era constants alongside the
    /// newer ones so a future accidental rename/typo of any of them shows
    /// up here too, not just in the `ANCHOR_KINDS` array length.
    #[test]
    fn anchor_kind_constants_match_their_string_literals() {
        assert_eq!(ANCHOR_KIND_WHOLE_FILE, "whole_file");
        assert_eq!(ANCHOR_KIND_REVIEW, "review");
        assert_eq!(ANCHOR_KIND_LINE, "line");
        assert_eq!(ANCHOR_KIND_RANGE, "range");
        assert_eq!(ANCHOR_KIND_SET, "set");
    }

    #[test]
    fn intent_vocab_accepts_exactly_the_seven_intents() {
        for i in [
            "note",
            "question",
            "todo",
            "flag-for-agent",
            "tour-stop",
            "finding",
            "claim",
        ] {
            assert!(is_valid_intent(i), "{i} should be valid");
        }
        for i in ["Note", "flag_for_agent", "", "resolved", "Finding", "Claim"] {
            assert!(!is_valid_intent(i), "{i} should be invalid");
        }
    }

    fn sym(name: &str, container: Option<&str>, kind: &str, start: u32, end: u32) -> Symbol {
        Symbol {
            ordinal: 0,
            name: name.to_string(),
            kind: kind.to_string(),
            line_start: start,
            line_end: end,
            col_start: 0,
            col_end: 0,
            container: container.map(str::to_string),
            signature: None,
            doc: None,
            param_min: None,
            param_max: None,
        }
    }

    #[test]
    fn enclosing_symbol_picks_the_innermost_span() {
        let symbols = vec![
            sym("Point", None, "impl", 1, 20),
            sym("dist", Some("Point"), "method", 5, 8),
            sym("origin", Some("Point"), "method", 10, 15),
        ];
        let found = enclosing_symbol(&symbols, 6).unwrap();
        assert_eq!(found.name, "dist", "line 6 is inside dist, not just Point");

        let found = enclosing_symbol(&symbols, 12).unwrap();
        assert_eq!(found.name, "origin");

        // Line 1 is inside the outer impl only.
        let found = enclosing_symbol(&symbols, 1).unwrap();
        assert_eq!(found.name, "Point");
    }

    #[test]
    fn enclosing_symbol_is_none_outside_every_span() {
        let symbols = vec![sym("dist", Some("Point"), "method", 5, 8)];
        assert!(enclosing_symbol(&symbols, 100).is_none());
        assert!(enclosing_symbol(&[], 1).is_none());
    }

    #[test]
    fn resolve_symbol_prefers_an_exact_name_and_container_match() {
        let descriptor = SymbolDescriptor {
            name: "dist".to_string(),
            container: Some("Point".to_string()),
            kind: "method".to_string(),
        };
        // The function shifted down (was created at its old line_start,
        // e.g. 5); the CURRENT symbol table already reflects the shift.
        let current_symbols = vec![sym("dist", Some("Point"), "method", 12, 15)];
        let fallback = anchor_for_line(5, "fn dist(&self) -> f64 {");
        let resolved = resolve_symbol(&current_symbols, &descriptor, &fallback, "irrelevant");
        assert_eq!(resolved.line, 12, "follows the function through drift");
        assert!(!resolved.stale);
    }

    #[test]
    fn resolve_symbol_falls_back_to_a_unique_name_match_when_the_container_changed() {
        let descriptor = SymbolDescriptor {
            name: "dist".to_string(),
            container: Some("Point".to_string()),
            kind: "method".to_string(),
        };
        // Moved to a different impl block — container changed, name is
        // still unique file-wide.
        let current_symbols = vec![sym("dist", Some("Shape"), "method", 30, 33)];
        let fallback = anchor_for_line(5, "fn dist(&self) -> f64 {");
        let resolved = resolve_symbol(&current_symbols, &descriptor, &fallback, "irrelevant");
        assert_eq!(resolved.line, 30);
        assert!(!resolved.stale);
    }

    #[test]
    fn resolve_symbol_falls_back_to_the_backup_anchor_when_the_symbol_is_gone() {
        let descriptor = SymbolDescriptor {
            name: "dist".to_string(),
            container: Some("Point".to_string()),
            kind: "method".to_string(),
        };
        // Two "dist" methods now exist (ambiguous name match) AND neither
        // has the original container — falls all the way back to the
        // backup line Selection's own fuzzy resolve.
        let current_symbols = vec![
            sym("dist", Some("Shape"), "method", 30, 33),
            sym("dist", Some("Other"), "method", 40, 43),
        ];
        let fallback = anchor_for_line(2, "fn dist(&self) -> f64 {");
        let content_str = content(&["// header", "fn dist(&self) -> f64 {", "0.0", "}"]);
        let resolved = resolve_symbol(&current_symbols, &descriptor, &fallback, &content_str);
        assert_eq!(resolved.line, 2);
        assert!(!resolved.stale);
    }

    #[test]
    fn resolve_symbol_is_stale_when_the_fallback_anchor_is_also_stale() {
        let descriptor = SymbolDescriptor {
            name: "vanished".to_string(),
            container: None,
            kind: "fn".to_string(),
        };
        let fallback = anchor_for_line(2, "the quick brown fox jumps over the lazy dog");
        let content_str = content(&["1", "2"]);
        let resolved = resolve_symbol(&[], &descriptor, &fallback, &content_str);
        assert!(resolved.stale);
    }

    #[test]
    fn normalize_range_sorts_when_drift_pushes_start_past_end() {
        let start = Resolved {
            line: 10,
            stale: false,
            confidence: MatchConfidence::Exact,
        };
        let end = Resolved {
            line: 4,
            stale: false,
            confidence: MatchConfidence::Exact,
        };
        let (lo, hi, stale) = normalize_range(start, end);
        assert_eq!((lo, hi), (4, 10));
        assert!(!stale);
    }

    #[test]
    fn normalize_range_keeps_order_when_undisturbed() {
        let start = Resolved {
            line: 3,
            stale: false,
            confidence: MatchConfidence::Exact,
        };
        let end = Resolved {
            line: 9,
            stale: false,
            confidence: MatchConfidence::Exact,
        };
        let (lo, hi, stale) = normalize_range(start, end);
        assert_eq!((lo, hi), (3, 9));
        assert!(!stale);
    }

    #[test]
    fn normalize_range_is_stale_when_either_side_is() {
        let fresh = Resolved {
            line: 1,
            stale: false,
            confidence: MatchConfidence::Exact,
        };
        let stale = Resolved {
            line: 2,
            stale: true,
            confidence: MatchConfidence::Fuzzy,
        };
        assert!(normalize_range(fresh, stale).2);
        assert!(normalize_range(stale, fresh).2);
    }

    #[test]
    fn diff_line_returns_the_recorded_offset_unconditionally() {
        let anchor = anchor_for_line(42, "some line at that commit");
        assert_eq!(diff_line(&anchor), 42);
    }

    #[test]
    fn symbol_descriptor_and_diff_anchor2_round_trip_through_json() {
        let descriptor = SymbolDescriptor {
            name: "dist".to_string(),
            container: Some("Point".to_string()),
            kind: "method".to_string(),
        };
        let json = serde_json::to_string(&descriptor).unwrap();
        let back: SymbolDescriptor = serde_json::from_str(&json).unwrap();
        assert_eq!(back, descriptor);

        let diff2 = DiffAnchor2 {
            sha_full: "a".repeat(40),
        };
        let json = serde_json::to_string(&diff2).unwrap();
        let back: DiffAnchor2 = serde_json::from_str(&json).unwrap();
        assert_eq!(back, diff2);
    }
}
