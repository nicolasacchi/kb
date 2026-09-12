//! kb-comments/1 — per-artifact comment storage. Topic 10 §I.
//!
//! Each artifact's comments live at
//! `<state>/<kb>/.review/<artifact_id>.json` as a single JSON document
//! holding the artifact metadata, all open + resolved comments, threaded
//! replies (by `you` or `claude`), and the schema version.
//!
//! Concurrency is handled at the daemon edge by a per-id `tokio::Mutex`
//! plus optimistic ETag/If-Match in the HTTP layer. This module exposes
//! the primitives:
//!
//!   - `load(path)` — read + parse, returns `Ok(None)` when the file
//!     doesn't exist (the SPA serves an empty skeleton in that case).
//!   - `save_atomic(path, file, if_match)` — tmpfile + rename. If
//!     `if_match` is provided and doesn't match the file's current ETag,
//!     returns `Error::PreconditionFailed`. Returns the new ETag on success.
//!   - `etag_for(path)` — sha256 of `(mtime_ns ‖ len ‖ first 256B)`. Cheap
//!     enough to compute on every GET; stable across mtime jitter (since
//!     the bytes are part of the hash) and across the read window
//!     (mtime_ns is included so a same-content rewrite still bumps).
//!
//! Anchors use a tagged enum (4 scopes per topic 10 §I) so future
//! migrations can add fields without breaking serde compat.

use crate::types::KbName;
use crate::{Error, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Schema discriminator value embedded in every file. Bump when the
/// shape changes incompatibly so old SPAs can refuse-with-a-message.
pub const SCHEMA: &str = "kb-comments/1";

/// Top-level review document. One per `<kb, artifact_id>` pair.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewFile {
    pub schema: String,
    pub artifact: ArtifactRef,
    #[serde(rename = "generatedAt")]
    pub generated_at: DateTime<Utc>,
    #[serde(default)]
    pub comments: Vec<Comment>,
    /// W2.15a — the review-pass verdict (comment/approve/request-changes),
    /// distinct from any individual comment's open/resolved status. `None`
    /// until a reviewer sets one. Additive + `#[serde(default,
    /// skip_serializing_if)]` — mirrors `choices`/`attachments`: pre-
    /// W2.15a review files load unchanged (no key, deserialises to
    /// `None`) and the key is omitted entirely on a file with no verdict
    /// (not `"verdict":null`). The server also mirrors a set/cleared
    /// verdict onto the artifact's own kb-tags as a `status-approved` /
    /// `status-changes-requested` DISPLAY SHORTCUT (invariant #12,
    /// `kb-server/src/routes/comments.rs`) — this field stays the verdict
    /// of record; the tag is derived, never the other way around.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub verdict: Option<Verdict>,
}

/// W2.15a — one review-pass verdict. `at`/`by` are stamped by
/// [`ReviewFile::set_verdict`] (never client-supplied), mirroring
/// `Comment::created_at`/`author`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub state: VerdictState,
    pub at: DateTime<Utc>,
    pub by: Author,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub note: Option<String>,
    /// v0.34 X1 — attribution username (lowercase). Additive + skip if
    /// none so pre-multi-user review files round-trip byte-identically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub user: Option<String>,
}

/// Three-state review verdict. `Comment` is a working/neutral note (no
/// pass/fail signal — the display-shortcut tag mirror drops any prior
/// `status-*` tag rather than writing one for this state); `Approve` /
/// `RequestChanges` map to the `status-approved` / `status-changes-
/// requested` kb-tag shortcut. `snake_case` on the wire (`request_changes`).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdictState {
    Comment,
    Approve,
    RequestChanges,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub id: String,
    pub title: String,
    pub kb: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub pages: Vec<PageRef>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageRef {
    pub src: String,
    pub label: String,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub status: CommentStatus,
    /// Source file for the comment (artifact id or page src). Lets the
    /// SPA group multi-file artifacts in the panel.
    pub file: String,
    #[serde(rename = "fileLabel")]
    pub file_label: String,
    pub anchor: Anchor,
    pub author: Author,
    pub body: String,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
    #[serde(rename = "editedAt")]
    pub edited_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub replies: Vec<Reply>,
    /// R3 — quick-response buttons. Empty for most comments; Claude
    /// attaches them via the CLI to turn a comment into a one-tap prompt.
    #[serde(default)]
    pub choices: Vec<Choice>,
    /// Y1 — file/image attachments this comment owns. The blob bytes live
    /// at `<state>/<kb>/.attachments/<artifact_id>/<aid>`; this is the
    /// denormalized read-path copy. Additive + `#[serde(default)]` so
    /// pre-Y review files load unchanged (mirrors `replies`/`choices`).
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// v0.34 X1 — attribution username (lowercase). Distinct from
    /// [`Author`] (you|claude ROLE). Additive + skip if none so old
    /// review files round-trip byte-identically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub user: Option<String>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommentStatus {
    Open,
    Resolved,
}

impl Comment {
    /// `true` when this comment's status is `Open`. Convenience for
    /// the indexer's anchor-stale check (skip resolved comments).
    pub fn is_open(&self) -> bool {
        matches!(self.status, CommentStatus::Open)
    }
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Author {
    You,
    Claude,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reply {
    pub id: String,
    pub author: Author,
    pub body: String,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
    /// R4 — set when the reply body is edited in place. Additive +
    /// `#[serde(default)]` so pre-R4 review files (no `editedAt` on a
    /// reply) load unchanged; mirrors `Comment.edited_at` and the SPA's
    /// "edited" badge.
    #[serde(rename = "editedAt", default)]
    pub edited_at: Option<DateTime<Utc>>,
    /// R3 — quick-response buttons, same as on `Comment`. Lets Claude ask
    /// a follow-up question with buttons in a reply (the usual live-loop
    /// shape: Claude replies, the user taps a choice).
    #[serde(default)]
    pub choices: Vec<Choice>,
    /// Y1 — file/image attachments this reply owns (same model + storage
    /// as `Comment::attachments`). Additive + `#[serde(default)]`.
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// v0.34 X1 — attribution username (lowercase). Same field name as
    /// [`Comment::user`] / [`Attachment::user`] / [`Verdict::user`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub user: Option<String>,
}

/// R3 — a quick-response button Claude attaches to a comment or reply.
/// Clicking it in the SPA appends `reply` as a `you` reply and, when
/// `resolve` is set, flips the comment to resolved. Additive +
/// `#[serde(default)]` everywhere, so old review files (no `choices`)
/// load unchanged and old daemons ignore the field.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Choice {
    /// Button text, e.g. "Apply".
    pub label: String,
    /// Markdown posted as the user's reply when the button is clicked.
    pub reply: String,
    /// Also flip the comment to resolved on click.
    #[serde(default)]
    pub resolve: bool,
}

/// Y1 — a file/image attached to a comment or reply. The canonical blob
/// bytes live on disk under `<state>/<kb>/.attachments/<artifact_id>/<aid>`
/// (keyed by `id`); this struct is the denormalized metadata copy carried
/// inline in the review JSON so a read path needs no extra file stat. The
/// body markdown points at one with `![alt](attachment:<id>)` (image) or
/// `[label](attachment:<id>)` (file). Additive + `#[serde(default)]` on
/// the owning vecs, so pre-Y review files load unchanged.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Attachment {
    /// `a_` + 12 hex (see [`new_attachment_id`]).
    pub id: String,
    /// Sanitized original filename — display only, never a filesystem path.
    pub filename: String,
    /// The daemon's magic-byte sniff of the bytes (never the client's
    /// claim) — also the `Content-Type` the serve route emits.
    #[serde(rename = "contentType")]
    pub content_type: String,
    /// Size of the blob in bytes.
    pub size: u64,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
    pub author: Author,
    /// v0.34 X1 — attribution username (lowercase).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub user: Option<String>,
}

/// Anchor strategy — 4 scopes ranked by regen-stability per topic 10 §I.
/// File scope survives any regen; selection scope is the most fragile.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Anchor {
    /// Whole-artifact comment. Survives any regen.
    File,
    /// Heading-text path, e.g. `"Tuning > Virtual nodes"`. Stable if the
    /// kb-prompt convention is followed.
    Chapter { path: String },
    /// `<section>` id or `data-kb-id` attribute. Falls back to heading
    /// text + nth-of-type. `tag` records the element name for paint.
    Section {
        id: String,
        #[serde(default)]
        tag: Option<String>,
        #[serde(default)]
        snippet: Option<String>,
    },
    /// CSS path + offset + ~200 char surrounding context. Fragile on
    /// regen — annotator falls back to fuzzy text match (v0.3+).
    Selection {
        css_path: String,
        offset: u32,
        snippet: String,
    },
}

impl Anchor {
    /// Short scope name — `"file"`, `"chapter"`, `"section"`,
    /// `"selection"`. Mirrors the serde `tag` discriminator so the
    /// SPA + sidecar see the same identifier. Used by the indexer's
    /// stale-event payload + the v3 sidecar metadata (#4).
    pub fn scope_name(&self) -> &'static str {
        match self {
            Anchor::File => "file",
            Anchor::Chapter { .. } => "chapter",
            Anchor::Section { .. } => "section",
            Anchor::Selection { .. } => "selection",
        }
    }
}

// --- v0.3 G1 — fuzzy anchor resolver ---------------------------------------

/// Outcome of `fuzzy_resolve_anchor`. The indexer post-upsert hook
/// uses this to decide whether to fire `comment.anchor_stale`: any
/// `Stale` resolution emits the SSE so the SPA can flag the comment.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    /// Anchor still points at the original element (id/path matched
    /// exactly). The string is the matched id or heading path.
    Exact(String),
    /// Anchor matched approximately; the float is the similarity
    /// score (Jaro-Winkler for selection, Jaccard for chapter, ratio
    /// for section). A separate field so callers can filter
    /// `Fuzzy` below a tighter threshold than the resolver default.
    Fuzzy(String, f32),
    /// No element survived re-resolution. The post-upsert hook fires
    /// `comment.anchor_stale` so the SPA can flag the comment id.
    Stale,
}

/// Default Jaro-Winkler threshold for Selection-scope rebinding.
/// Override at runtime via `KB_COMMENT_FUZZY_THRESHOLD=0.85`. Topic 09
/// §C calls 0.85 a reasonable starting point; revisit in v0.4 with
/// telemetry.
pub const DEFAULT_FUZZY_THRESHOLD: f32 = 0.85;

/// Jaccard threshold for Chapter-scope token-set matching. Lower
/// than the selection threshold because Jaccard is a stricter metric
/// — 0.5 means "more than half the original heading's tokens still
/// appear in some current heading", which is a robust regen signal.
pub const CHAPTER_JACCARD_THRESHOLD: f32 = 0.5;

/// Default context window for Selection-scope snippet matching (chars).
/// Override via `KB_COMMENT_CONTEXT_CHARS=200`. Wider windows catch
/// more reorderings at the cost of slower matching.
pub const DEFAULT_CONTEXT_CHARS: usize = 200;

/// Try to re-resolve `anchor` against the current `html`. Selection
/// scope uses Jaro-Winkler against paragraph text; Chapter scope uses
/// token-set Jaccard against heading paths; Section scope checks ids
/// (exact id, then synthetic `<heading-slug>-<tag>-<nth>`); File
/// scope is always `Exact("file")` (whole-artifact comments survive
/// any regen).
pub fn fuzzy_resolve_anchor(html: &str, anchor: &Anchor) -> Resolution {
    fuzzy_resolve_anchor_with(&mut None, html, anchor)
}

/// [`fuzzy_resolve_anchor`] against a caller-held parsed-DOM slot, so a
/// loop re-resolving MANY anchors of one `html` (the indexer's open-comment
/// pass, the list-anchor enrichment hook) pays ONE `Html::parse_document`
/// instead of one per anchor. `File` anchors never touch the slot (they
/// resolve without a DOM — as before); the first DOM-needing anchor fills
/// it. The slot holds `scraper::Html` (`!Send`): callers in async code must
/// drop it before the next `.await`.
pub fn fuzzy_resolve_anchor_with(
    doc: &mut Option<scraper::Html>,
    html: &str,
    anchor: &Anchor,
) -> Resolution {
    if matches!(anchor, Anchor::File) {
        return Resolution::Exact("file".to_string());
    }
    fuzzy_resolve_anchor_in(
        doc.get_or_insert_with(|| scraper::Html::parse_document(html)),
        anchor,
    )
}

/// [`fuzzy_resolve_anchor`] over an already-parsed document. `File` is
/// answered without reading `doc` (kept here so all entry points share
/// one ladder).
pub fn fuzzy_resolve_anchor_in(doc: &scraper::Html, anchor: &Anchor) -> Resolution {
    match anchor {
        Anchor::File => Resolution::Exact("file".to_string()),
        Anchor::Section { id, .. } => resolve_section_in(doc, id),
        Anchor::Chapter { path } => resolve_chapter_in(doc, path),
        Anchor::Selection {
            snippet, css_path, ..
        } => resolve_selection_in(doc, snippet, css_path),
    }
}

/// `pub(crate)` (not `pub`) — the sessions engine's memory-session anchor
/// resolver (`sessions::view::resolve_selection_in_view`, memo R2) reuses
/// this so a Selection-scope fuzzy match agrees with the generic resolver's
/// threshold (and its `KB_COMMENT_FUZZY_THRESHOLD` override) instead of
/// drifting a second copy.
pub(crate) fn fuzzy_threshold() -> f32 {
    std::env::var("KB_COMMENT_FUZZY_THRESHOLD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_FUZZY_THRESHOLD)
}

/// `pub(crate)` for the same reason as [`fuzzy_threshold`] — shared with the
/// sessions engine's Selection-anchor resolver.
pub(crate) fn context_chars() -> usize {
    std::env::var("KB_COMMENT_CONTEXT_CHARS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_CONTEXT_CHARS)
}

/// Section: try exact id first (both `[id]` and `[data-kb-id]`), else
/// `Stale`. The synthetic-id reconstruction (`<heading-slug>-<tag>-<nth>`)
/// would need the annotator's stateful walk to be precise; v0.4 may
/// add it once we have telemetry on how often it'd help.
///
/// M4: walks DOM elements directly rather than parsing a CSS selector
/// built via string interpolation. The selector grammar requires
/// escaping for `:`, `]`, whitespace, `*`, etc. inside attribute
/// values; the pre-fix code only escaped `\\` and `"`, which left an
/// anchor like `Anchor::Section { id: "my-section.5" }` failing to
/// parse and returning Stale even though the element exists. The
/// `Comment.anchor` JSON is user-authored and has no charset
/// validation — every legal HTML id (per HTML5) needs to round-trip.
fn resolve_section_in(doc: &scraper::Html, id: &str) -> Resolution {
    use scraper::Selector;
    let all_sel = Selector::parse("*").expect("static selector");
    for el in doc.select(&all_sel) {
        let v = el.value();
        if v.id() == Some(id) || v.attr("data-kb-id") == Some(id) {
            return Resolution::Exact(id.to_string());
        }
    }
    Resolution::Stale
}

/// Chapter: parse the heading path "Top > Mid > Leaf"; for each
/// trailing-most heading text, look for a heading whose normalised
/// tokens have Jaccard >= threshold.
fn resolve_chapter_in(doc: &scraper::Html, path: &str) -> Resolution {
    // v0.7.1 P2 — split on the `" > "` separator the annotator joins
    // with, not a bare `>`. A heading that itself contains `>` (generics
    // like `Vec<T>`, breadcrumbs, `a > b`) kept the wrong leaf under the
    // bare-`>` rsplit and went stale on every reindex.
    let target_leaf = match path.rsplit(" > ").next() {
        Some(s) => s.trim().to_string(),
        None => return Resolution::Stale,
    };
    if target_leaf.is_empty() {
        return Resolution::Stale;
    }
    let target_tokens = tokenise(&target_leaf);
    if target_tokens.is_empty() {
        return Resolution::Stale;
    }
    let mut best: f32 = 0.0;
    let mut best_text: String = String::new();
    let mut exact = false;
    for (_lvl, text) in crate::parser::headings_in_order_doc(doc) {
        if text == target_leaf {
            exact = true;
            best_text = text.clone();
            best = 1.0;
            break;
        }
        let s = jaccard(&target_tokens, &tokenise(&text));
        if s > best {
            best = s;
            best_text = text;
        }
    }
    if exact {
        Resolution::Exact(best_text)
    } else if best >= CHAPTER_JACCARD_THRESHOLD {
        Resolution::Fuzzy(best_text, best)
    } else {
        Resolution::Stale
    }
}

/// Selection: scan visible block text for the highest Jaro-Winkler
/// against the stored snippet, capped at `context_chars()`.
///
/// Duplicate-disambiguation (borrowed from redline's anchor model, adapted
/// to kb's data): a Selection `snippet` is the *selected text only* (no
/// surrounding context — see `web/src/scripts/annotate.ts`), so two
/// identical passages produce identical snippets and tie on Jaro-Winkler.
/// The stored `offset` can't break that tie — it is the selection's
/// node-local `range.startOffset`, the same value inside every duplicate
/// block. The positional signal that *does* distinguish them is the stored
/// `css_path` (`body > tag:nth-of-type(n) > …`). So rather than returning
/// the first match (the pre-v0.19 behaviour, which silently bound a comment
/// to the wrong occurrence among duplicates), we rank candidates by
/// `(exact, jaro_winkler, structural-path similarity to css_path)` and keep
/// the best. Single-match documents are unaffected — the path term only
/// arbitrates near-ties, so the score still decides whenever it can.
fn resolve_selection_in(doc: &scraper::Html, snippet: &str, css_path: &str) -> Resolution {
    use scraper::Selector;
    let needle: String = snippet.chars().take(context_chars()).collect();
    let needle_trim = needle.trim();
    if needle_trim.is_empty() {
        return Resolution::Stale;
    }
    let block_sel =
        Selector::parse("p, li, blockquote, td, h1, h2, h3, h4, h5, h6").expect("static selector");
    let stored_path = parse_css_path(css_path);

    // Best-so-far ranked by (is_exact, score, path_similarity). The path
    // term is a tiebreak: it only swaps the winner when the scores are
    // within EPS, so a clearly-better fuzzy match elsewhere still wins and
    // an exact text match always beats a fuzzy one regardless of path.
    let mut best_exact = false;
    let mut best_score: f32 = 0.0;
    let mut best_path: f32 = -1.0;
    let mut best_text: String = String::new();
    let mut any = false;

    for el in doc.select(&block_sel) {
        let text: String = el.text().collect();
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let exact = trimmed == needle_trim;
        let score = if exact {
            1.0
        } else {
            jaro_winkler(&needle, trimmed)
        };
        let path_sim = if stored_path.is_empty() {
            0.0
        } else {
            path_similarity(&stored_path, &structural_path(&el))
        };
        if !any
            || candidate_is_better(
                (exact, score, path_sim),
                (best_exact, best_score, best_path),
            )
        {
            any = true;
            best_exact = exact;
            best_score = score;
            best_path = path_sim;
            best_text = trimmed.to_string();
        }
    }

    if best_exact {
        return Resolution::Exact(snippet.to_string());
    }
    if best_score >= fuzzy_threshold() {
        Resolution::Fuzzy(best_text, best_score)
    } else {
        Resolution::Stale
    }
}

/// Order two Selection candidates: an exact text match beats a non-exact
/// one; otherwise a clearly-higher Jaro-Winkler wins; a near-tie (scores
/// within `EPS`) defers to the candidate whose structural path better
/// matches the stored `css_path`. Returns `true` iff `new` should replace
/// `cur`. `(exact, score, path_sim)`.
fn candidate_is_better(new: (bool, f32, f32), cur: (bool, f32, f32)) -> bool {
    const EPS: f32 = 1e-3;
    let (ne, ns, np) = new;
    let (ce, cs, cp) = cur;
    if ne != ce {
        return ne; // exact beats non-exact
    }
    if (ns - cs).abs() > EPS {
        return ns > cs; // clearly-higher score wins
    }
    np > cp // near-tie → better structural-path match
}

/// Parse a stored `css_path` (`body > main:nth-of-type(1) > p:nth-of-type(2)`)
/// into a root-first list of `(tag, nth_of_type)` segments, dropping the
/// leading `body`. A segment with no `:nth-of-type(n)` gets `nth = 0`
/// (matches anything of that tag). Mirrors `cssPath()` in
/// `web/src/scripts/annotate.ts`.
fn parse_css_path(css: &str) -> Vec<(String, usize)> {
    css.split(" > ")
        .filter_map(|seg| {
            let seg = seg.trim();
            if seg.is_empty()
                || seg.eq_ignore_ascii_case("body")
                || seg.eq_ignore_ascii_case("html")
            {
                return None;
            }
            match seg.split_once(":nth-of-type(") {
                Some((tag, rest)) => {
                    let nth = rest.trim_end_matches(')').parse::<usize>().unwrap_or(0);
                    Some((tag.trim().to_ascii_lowercase(), nth))
                }
                None => Some((seg.to_ascii_lowercase(), 0)),
            }
        })
        .collect()
}

/// The structural path of a parsed element as a root-first list of
/// `(tag, nth_of_type)` segments up to (but not including) `<body>` —
/// directly comparable to [`parse_css_path`]'s output.
fn structural_path(el: &scraper::ElementRef) -> Vec<(String, usize)> {
    let mut chain: Vec<(String, usize)> = Vec::new();
    let mut cur = Some(*el);
    while let Some(e) = cur {
        let tag = e.value().name().to_ascii_lowercase();
        if tag == "body" || tag == "html" {
            break;
        }
        chain.push((tag, nth_of_type(&e)));
        cur = e.parent().and_then(scraper::ElementRef::wrap);
    }
    chain.reverse();
    chain
}

/// 1-based index of `el` among its same-tag element siblings (CSS
/// `nth-of-type` semantics) — the same count `cssPath()` computes in the
/// annotator.
fn nth_of_type(el: &scraper::ElementRef) -> usize {
    let tag = el.value().name();
    let mut n = 1;
    for sib in el.prev_siblings() {
        if let Some(e) = sib.value().as_element() {
            if e.name() == tag {
                n += 1;
            }
        }
    }
    n
}

/// Similarity in [0,1] of two `(tag, nth)` paths, scored over their common
/// trailing segments (the part nearest the anchored element, which regen
/// perturbs least). Each trailing segment scores 1.0 when both tag and a
/// non-zero `nth` match, 0.5 for a tag-only match, and the walk stops at
/// the first tag mismatch. Normalised by the longer path so a deeper exact
/// suffix ranks above a shallow one.
fn path_similarity(a: &[(String, usize)], b: &[(String, usize)]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut score = 0.0f32;
    let n = a.len().min(b.len());
    for k in 1..=n {
        let (at, an) = &a[a.len() - k];
        let (bt, bn) = &b[b.len() - k];
        if at != bt {
            break;
        }
        score += if an == bn && *an != 0 { 1.0 } else { 0.5 };
    }
    score / a.len().max(b.len()) as f32
}

fn tokenise(s: &str) -> std::collections::BTreeSet<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

fn jaccard(a: &std::collections::BTreeSet<String>, b: &std::collections::BTreeSet<String>) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let inter = a.intersection(b).count() as f32;
    let union = a.union(b).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Hand-rolled Jaro-Winkler. Pulling in `strsim` for one function would
/// drag a small dep into a hot indexer-side path; the algorithm is
/// ~30 lines and we already have all the primitives.
/// `pub(crate)` for the same reason as [`fuzzy_threshold`] — shared with the
/// sessions engine's Selection-anchor resolver, so both callers score
/// against one algorithm.
pub(crate) fn jaro_winkler(s1: &str, s2: &str) -> f32 {
    let a: Vec<char> = s1.chars().collect();
    let b: Vec<char> = s2.chars().collect();
    let la = a.len();
    let lb = b.len();
    if la == 0 && lb == 0 {
        return 1.0;
    }
    if la == 0 || lb == 0 {
        return 0.0;
    }
    let match_distance = (la.max(lb) / 2).saturating_sub(1);
    let mut matched_a = vec![false; la];
    let mut matched_b = vec![false; lb];
    let mut matches = 0usize;
    for i in 0..la {
        let lo = i.saturating_sub(match_distance);
        let hi = (i + match_distance + 1).min(lb);
        for j in lo..hi {
            if matched_b[j] || a[i] != b[j] {
                continue;
            }
            matched_a[i] = true;
            matched_b[j] = true;
            matches += 1;
            break;
        }
    }
    if matches == 0 {
        return 0.0;
    }
    // Transpositions.
    let mut transpositions = 0usize;
    let mut k = 0usize;
    for i in 0..la {
        if !matched_a[i] {
            continue;
        }
        while !matched_b[k] {
            k += 1;
        }
        if a[i] != b[k] {
            transpositions += 1;
        }
        k += 1;
    }
    let m = matches as f32;
    let jaro = (m / la as f32 + m / lb as f32 + (m - transpositions as f32 / 2.0) / m) / 3.0;
    // Winkler boost: up to 4-char common prefix scaled by 0.1.
    let mut prefix = 0usize;
    for i in 0..la.min(lb).min(4) {
        if a[i] == b[i] {
            prefix += 1;
        } else {
            break;
        }
    }
    jaro + prefix as f32 * 0.1 * (1.0 - jaro)
}

impl ReviewFile {
    /// Empty skeleton for an artifact that has no comments yet. The SPA
    /// uses this when GET returns 404 so the annotator script always has
    /// a `window.__KB_COMMENTS` object to read.
    pub fn empty_skeleton(kb: &KbName, artifact_id: &str, title: &str) -> Self {
        Self {
            schema: SCHEMA.to_string(),
            artifact: ArtifactRef {
                id: artifact_id.to_string(),
                title: title.to_string(),
                kb: kb.to_string(),
                tags: Vec::new(),
                pages: Vec::new(),
            },
            generated_at: Utc::now(),
            comments: Vec::new(),
            verdict: None,
        }
    }

    pub fn open_count(&self) -> usize {
        self.comments
            .iter()
            .filter(|c| c.status == CommentStatus::Open)
            .count()
    }

    // --- R4 — typed mutations -------------------------------------------
    //
    // The single home for "mutate a review document": the server routes
    // and the SPA/CLI all funnel here so mutation logic isn't reimplemented
    // (the pre-R4 CLI did untyped `serde_json::Value` surgery; the SPA
    // rebuilt the whole document). Each is pure (no I/O) — the daemon runs
    // them under its `review_lock`, then `save_atomic`s the result. Missing
    // ids return `Error::NotFound` (404 at the HTTP edge). `add_comment` /
    // `add_reply` assign the id + `created_at` here, so callers never invent
    // ids (per the CLAUDE.md "don't invent comment ids" rule).

    /// Append a new open comment with a freshly-assigned `c_<hex>` id and
    /// `created_at = now`. Returns the inserted comment (so the route can
    /// echo it back as the 201 body).
    pub fn add_comment(&mut self, spec: NewComment) -> &Comment {
        self.comments.push(Comment {
            id: new_comment_id(),
            status: CommentStatus::Open,
            file: spec.file,
            file_label: spec.file_label,
            anchor: spec.anchor,
            author: spec.author,
            body: spec.body,
            created_at: Utc::now(),
            edited_at: None,
            replies: Vec::new(),
            choices: spec.choices,
            attachments: spec.attachments,
            user: spec.user,
        });
        self.comments.last().expect("just pushed")
    }

    /// Append a reply (freshly-assigned `r_<hex>` id) to `comment_id`.
    /// `Err(NotFound)` when the comment is absent. `user` is the
    /// attribution username (v0.34 X1); `None` leaves the field unset.
    pub fn add_reply(
        &mut self,
        comment_id: &str,
        author: Author,
        body: String,
        choices: Vec<Choice>,
        user: Option<String>,
    ) -> Result<&Reply> {
        let c = self.comment_mut(comment_id)?;
        c.replies.push(Reply {
            id: new_reply_id(),
            author,
            body,
            created_at: Utc::now(),
            edited_at: None,
            choices,
            attachments: Vec::new(),
            user,
        });
        Ok(c.replies.last().expect("just pushed"))
    }

    /// Set one comment's status (resolve/unresolve). `Err(NotFound)` when
    /// the comment is absent. Returns `true` iff the status actually changed
    /// (G8 — lets the route skip a no-op save + `comments.updated` emit when
    /// resolving an already-resolved comment).
    pub fn set_comment_status(&mut self, comment_id: &str, status: CommentStatus) -> Result<bool> {
        let c = self.comment_mut(comment_id)?;
        let changed = c.status != status;
        c.status = status;
        Ok(changed)
    }

    /// Flip every comment NOT already at `status` to `status`; returns the
    /// count flipped. Backs `resolve-all` / `unresolve-all` (one mutation,
    /// one save, one SSE).
    pub fn set_all_status(&mut self, status: CommentStatus) -> usize {
        let mut flipped = 0;
        for c in &mut self.comments {
            if c.status != status {
                c.status = status;
                flipped += 1;
            }
        }
        flipped
    }

    /// W2.15a — set (or replace) the review-pass verdict. Stamps
    /// `at = now` and `by = Author::You` (a verdict is a human review
    /// decision — Claude comments/replies but doesn't grant one). Returns
    /// `false` (no-op, G8) when the incoming `(state, note)` is identical
    /// to the current verdict's — deliberately ignoring `at`, so re-
    /// clicking the same state twice doesn't churn the file/ETag/SSE.
    pub fn set_verdict(
        &mut self,
        state: VerdictState,
        note: Option<String>,
        user: Option<String>,
    ) -> bool {
        let unchanged = self
            .verdict
            .as_ref()
            .is_some_and(|v| v.state == state && v.note == note && v.user == user);
        if unchanged {
            return false;
        }
        self.verdict = Some(Verdict {
            state,
            at: Utc::now(),
            by: Author::You,
            note,
            user,
        });
        true
    }

    /// W2.15a — clear the review-pass verdict. Returns `false` (no-op,
    /// G8) when there was none to clear.
    pub fn clear_verdict(&mut self) -> bool {
        self.verdict.take().is_some()
    }

    /// Replace a comment's body and stamp `edited_at = now`.
    pub fn edit_comment_body(&mut self, comment_id: &str, body: String) -> Result<()> {
        let c = self.comment_mut(comment_id)?;
        c.body = body;
        c.edited_at = Some(Utc::now());
        Ok(())
    }

    /// Re-point a comment's anchor (R9). Explicit, ground-truth override
    /// used after Claude moves/renames the anchored element during an
    /// otherwise-unrelated edit. Deliberately distinct from the indexer's
    /// fuzzy resolver, which stays detection-only and keeps the *frozen*
    /// original anchor — only an intentional call here rewrites it, so the
    /// automatic path never silently drifts. Does NOT stamp `edited_at`
    /// (that badge is for body edits); the indexer re-evaluates staleness
    /// against the new anchor on the next reindex.
    pub fn set_comment_anchor(&mut self, comment_id: &str, anchor: Anchor) -> Result<bool> {
        let c = self.comment_mut(comment_id)?;
        let changed = c.anchor != anchor;
        c.anchor = anchor;
        Ok(changed)
    }

    /// Replace a reply's body and stamp `edited_at = now`. `Err(NotFound)`
    /// distinguishes a missing comment from a present comment whose reply
    /// id is absent.
    pub fn edit_reply_body(
        &mut self,
        comment_id: &str,
        reply_id: &str,
        body: String,
    ) -> Result<()> {
        let c = self.comment_mut(comment_id)?;
        let r = c
            .replies
            .iter_mut()
            .find(|r| r.id == reply_id)
            .ok_or_else(|| Error::NotFound(format!("reply {reply_id} in comment {comment_id}")))?;
        r.body = body;
        r.edited_at = Some(Utc::now());
        Ok(())
    }

    /// Remove a top-level comment (hard delete — no tombstone) and return
    /// it. `Err(NotFound)` when absent.
    pub fn delete_comment(&mut self, comment_id: &str) -> Result<Comment> {
        let idx = self
            .comments
            .iter()
            .position(|c| c.id == comment_id)
            .ok_or_else(|| Error::NotFound(format!("comment {comment_id}")))?;
        Ok(self.comments.remove(idx))
    }

    /// Remove a single reply from a comment thread and return it.
    /// `Err(NotFound)` for both a missing comment and a missing reply.
    pub fn delete_reply(&mut self, comment_id: &str, reply_id: &str) -> Result<Reply> {
        let c = self.comment_mut(comment_id)?;
        let idx = c
            .replies
            .iter()
            .position(|r| r.id == reply_id)
            .ok_or_else(|| Error::NotFound(format!("reply {reply_id} in comment {comment_id}")))?;
        Ok(c.replies.remove(idx))
    }

    // --- Y1 — attachment adopt / detach ---------------------------------
    //
    // Pure (no I/O): the route has already written the blob + manifest
    // entry; these record/remove the denormalized `Attachment` on the
    // owning comment or reply under the per-kb `review_lock`. Detach
    // returns the removed metadata so the route can GC the orphaned blob.

    /// Adopt an attachment onto a comment. `Err(NotFound)` when absent.
    pub fn add_comment_attachment(
        &mut self,
        comment_id: &str,
        att: Attachment,
    ) -> Result<&Attachment> {
        let c = self.comment_mut(comment_id)?;
        c.attachments.push(att);
        Ok(c.attachments.last().expect("just pushed"))
    }

    /// Adopt an attachment onto a reply. `Err(NotFound)` distinguishes a
    /// missing comment from a present comment whose reply id is absent.
    pub fn add_reply_attachment(
        &mut self,
        comment_id: &str,
        reply_id: &str,
        att: Attachment,
    ) -> Result<&Attachment> {
        let c = self.comment_mut(comment_id)?;
        let r = c
            .replies
            .iter_mut()
            .find(|r| r.id == reply_id)
            .ok_or_else(|| Error::NotFound(format!("reply {reply_id} in comment {comment_id}")))?;
        r.attachments.push(att);
        Ok(r.attachments.last().expect("just pushed"))
    }

    /// Detach an attachment from a comment by `aid`; returns the removed
    /// metadata. `Err(NotFound)` for a missing comment or aid.
    pub fn remove_comment_attachment(&mut self, comment_id: &str, aid: &str) -> Result<Attachment> {
        let c = self.comment_mut(comment_id)?;
        let idx = c
            .attachments
            .iter()
            .position(|a| a.id == aid)
            .ok_or_else(|| Error::NotFound(format!("attachment {aid} in comment {comment_id}")))?;
        Ok(c.attachments.remove(idx))
    }

    /// Detach an attachment from a reply by `aid`. `Err(NotFound)` for a
    /// missing comment, reply, or aid.
    pub fn remove_reply_attachment(
        &mut self,
        comment_id: &str,
        reply_id: &str,
        aid: &str,
    ) -> Result<Attachment> {
        let c = self.comment_mut(comment_id)?;
        let r = c
            .replies
            .iter_mut()
            .find(|r| r.id == reply_id)
            .ok_or_else(|| Error::NotFound(format!("reply {reply_id} in comment {comment_id}")))?;
        let idx = r
            .attachments
            .iter()
            .position(|a| a.id == aid)
            .ok_or_else(|| Error::NotFound(format!("attachment {aid} in reply {reply_id}")))?;
        Ok(r.attachments.remove(idx))
    }

    /// Every attachment id referenced by any comment OR reply. Feeds the
    /// GC reference check — an `adopted` blob whose id is NOT in this set
    /// is an orphan (its owning comment/reply was deleted) and reapable.
    pub fn referenced_attachment_ids(&self) -> std::collections::HashSet<String> {
        let mut ids = std::collections::HashSet::new();
        for c in &self.comments {
            for a in &c.attachments {
                ids.insert(a.id.clone());
            }
            for r in &c.replies {
                for a in &r.attachments {
                    ids.insert(a.id.clone());
                }
            }
        }
        ids
    }

    /// Apply an ordered batch of mutations atomically (borrowed from
    /// redline's `apply` primitive). All ops land or none do: they run
    /// against a working clone, and `self` is only replaced once every op
    /// succeeds — so a `NotFound` on op N leaves `self` byte-identical to
    /// before the call. `default_file` is the artifact id used for an
    /// `AddComment` op that omits its `file` (mirrors the `add_comment`
    /// route default). Returns an [`ApplyReport`] (ids minted, count
    /// applied, whether anything actually changed for the G8 no-op skip).
    ///
    /// The point versus N fine-grained calls: one `review_lock` acquisition,
    /// one `save_atomic`, one `comments.updated` SSE for the whole batch,
    /// and true all-or-nothing semantics for an agent's "reply → reanchor →
    /// resolve" sequence. Attachments are out of scope (they use the staging
    /// flow) — batch ops carry no `attachment_ids`.
    pub fn apply_ops(&mut self, ops: &[BatchOp], default_file: &str) -> Result<ApplyReport> {
        let mut working = self.clone();
        let mut report = ApplyReport::default();
        for op in ops {
            op.apply_to(&mut working, default_file, &mut report)?;
        }
        *self = working;
        Ok(report)
    }

    fn comment_mut(&mut self, comment_id: &str) -> Result<&mut Comment> {
        self.comments
            .iter_mut()
            .find(|c| c.id == comment_id)
            .ok_or_else(|| Error::NotFound(format!("comment {comment_id}")))
    }
}

/// One operation in an [`ReviewFile::apply_ops`] batch. Tagged by `op` so
/// the wire shape is `{"op":"resolve","comment_id":"c_…"}` /
/// `{"op":"add_comment","anchor":{…},"author":"claude","body":"…"}` /
/// `{"op":"resolve_all"}`. Mirrors the fine-grained R5 endpoints
/// one-for-one (minus attachments). Ops reference existing ids only — a
/// batch can't reply to a comment it also creates in the same batch (the
/// new id is server-minted), which keeps the schema flat and the semantics
/// obvious.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum BatchOp {
    AddComment {
        anchor: Anchor,
        author: Author,
        body: String,
        #[serde(default)]
        choices: Vec<Choice>,
        #[serde(default)]
        file: Option<String>,
        #[serde(rename = "fileLabel", default)]
        file_label: Option<String>,
    },
    AddReply {
        comment_id: String,
        author: Author,
        body: String,
        #[serde(default)]
        choices: Vec<Choice>,
    },
    EditComment {
        comment_id: String,
        body: String,
    },
    EditReply {
        comment_id: String,
        reply_id: String,
        body: String,
    },
    SetAnchor {
        comment_id: String,
        anchor: Anchor,
    },
    Resolve {
        comment_id: String,
    },
    Unresolve {
        comment_id: String,
    },
    ResolveAll,
    UnresolveAll,
    DeleteComment {
        comment_id: String,
    },
    DeleteReply {
        comment_id: String,
        reply_id: String,
    },
    /// W2.15a — set the review-pass verdict (`{"op":"set_verdict",
    /// "state":"approve","note":"…"}`).
    SetVerdict {
        state: VerdictState,
        #[serde(default)]
        note: Option<String>,
    },
    /// W2.15a — clear the review-pass verdict.
    ClearVerdict,
}

impl BatchOp {
    fn apply_to(
        &self,
        f: &mut ReviewFile,
        default_file: &str,
        report: &mut ApplyReport,
    ) -> Result<()> {
        match self {
            BatchOp::AddComment {
                anchor,
                author,
                body,
                choices,
                file,
                file_label,
            } => {
                let spec = NewComment {
                    file: file.clone().unwrap_or_else(|| default_file.to_string()),
                    file_label: file_label.clone().unwrap_or_else(|| "main".to_string()),
                    anchor: anchor.clone(),
                    author: *author,
                    body: body.clone(),
                    choices: choices.clone(),
                    attachments: Vec::new(),
                    user: None,
                };
                let id = f.add_comment(spec).id.clone();
                report.created_comment_ids.push(id);
                report.mutated = true;
            }
            BatchOp::AddReply {
                comment_id,
                author,
                body,
                choices,
            } => {
                let rid = f
                    .add_reply(comment_id, *author, body.clone(), choices.clone(), None)?
                    .id
                    .clone();
                report.created_reply_ids.push(rid);
                report.mutated = true;
            }
            BatchOp::EditComment { comment_id, body } => {
                f.edit_comment_body(comment_id, body.clone())?;
                report.mutated = true;
            }
            BatchOp::EditReply {
                comment_id,
                reply_id,
                body,
            } => {
                f.edit_reply_body(comment_id, reply_id, body.clone())?;
                report.mutated = true;
            }
            BatchOp::SetAnchor { comment_id, anchor } => {
                report.mutated |= f.set_comment_anchor(comment_id, anchor.clone())?;
            }
            BatchOp::Resolve { comment_id } => {
                report.mutated |= f.set_comment_status(comment_id, CommentStatus::Resolved)?;
            }
            BatchOp::Unresolve { comment_id } => {
                report.mutated |= f.set_comment_status(comment_id, CommentStatus::Open)?;
            }
            BatchOp::ResolveAll => {
                report.mutated |= f.set_all_status(CommentStatus::Resolved) > 0;
            }
            BatchOp::UnresolveAll => {
                report.mutated |= f.set_all_status(CommentStatus::Open) > 0;
            }
            BatchOp::DeleteComment { comment_id } => {
                f.delete_comment(comment_id)?;
                report.mutated = true;
            }
            BatchOp::DeleteReply {
                comment_id,
                reply_id,
            } => {
                f.delete_reply(comment_id, reply_id)?;
                report.mutated = true;
            }
            BatchOp::SetVerdict { state, note } => {
                report.mutated |= f.set_verdict(*state, note.clone(), None);
            }
            BatchOp::ClearVerdict => {
                report.mutated |= f.clear_verdict();
            }
        }
        report.applied += 1;
        Ok(())
    }
}

/// Outcome of [`ReviewFile::apply_ops`]: how many ops landed, the ids minted
/// for `AddComment`/`AddReply` ops (so the caller can record history rows +
/// echo them back), and whether anything actually changed (`mutated` — the
/// route skips the rewrite + SSE when a batch of no-ops leaves the file as
/// it was, honouring the G8 convention).
#[derive(Debug, Clone, Default, Serialize)]
pub struct ApplyReport {
    pub applied: usize,
    pub created_comment_ids: Vec<String>,
    pub created_reply_ids: Vec<String>,
    #[serde(skip)]
    pub mutated: bool,
}

/// Everything a caller supplies to create a comment. The id, `status`
/// (always `Open`), `created_at`, `edited_at`, and `replies` are assigned
/// by [`ReviewFile::add_comment`].
#[derive(Debug, Clone)]
pub struct NewComment {
    pub file: String,
    pub file_label: String,
    pub anchor: Anchor,
    pub author: Author,
    pub body: String,
    pub choices: Vec<Choice>,
    /// Y1 — attachments to adopt onto the new comment. The route resolves
    /// staged `aid`s → `Attachment`s (from the manifest) before building
    /// this spec, so kb-core stays free of the blob-store paths.
    pub attachments: Vec<Attachment>,
    /// v0.34 X1 — attribution username (lowercase). `None` leaves the
    /// field unset on the stored comment.
    pub user: Option<String>,
}

/// Fresh comment id: `c_` + 12 hex chars (6 random bytes). Collisions only
/// matter within one review file; 48 bits of entropy is ample.
pub fn new_comment_id() -> String {
    format!("c_{}", short_random_hex())
}

/// Fresh reply id: `r_` + 12 hex chars (6 random bytes).
pub fn new_reply_id() -> String {
    format!("r_{}", short_random_hex())
}

/// Fresh attachment id: `a_` + 12 hex chars (6 random bytes). Same entropy
/// and scope rationale as [`new_comment_id`] — unique within one review's
/// attachment dir, which is all that matters.
pub fn new_attachment_id() -> String {
    format!("a_{}", short_random_hex())
}

/// 12-hex (6 random bytes). Falls back to subsecond-nanos when the OS RNG
/// is unavailable (extremely rare) — good enough for an in-file unique id.
/// `pub(crate)` so `lists::new_list_id` / `new_entry_id` mint ids with the
/// same shape (`l_` / `le_` beside the review file's `c_` / `r_` / `a_`).
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

/// Load + parse a review file. Returns `Ok(None)` when the file doesn't
/// exist (this is normal — most artifacts have no comments). Returns
/// `Err(Serde)` if the JSON is malformed; `Err(BadRequest)` if the
/// schema discriminator doesn't match.
pub fn load(path: &Path) -> Result<Option<ReviewFile>> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let file: ReviewFile = serde_json::from_slice(&bytes)?;
    if file.schema != SCHEMA {
        return Err(Error::BadRequest(format!(
            "review schema {:?} not supported (expected {SCHEMA})",
            file.schema
        )));
    }
    Ok(Some(file))
}

/// Atomic write with optional ETag check. On If-Match mismatch,
/// returns `Error::PreconditionFailed` — the caller (HTTP handler)
/// surfaces this as 412 problem+json. On success, returns the new ETag
/// (computed by `etag_for` — the same function GET and the If-Match
/// check use).
///
/// Atomicity: writes a sibling `<file>.tmp.<pid>.<rand>` and renames.
/// Same-filesystem rename is atomic on POSIX; the parent dir is fsync'd
/// after rename so the new entry survives a crash.
pub fn save_atomic(path: &Path, file: &ReviewFile, if_match: Option<&str>) -> Result<String> {
    if file.schema != SCHEMA {
        return Err(Error::BadRequest(format!(
            "cannot save review with schema {:?}; expected {SCHEMA}",
            file.schema
        )));
    }
    if let Some(expected) = if_match {
        let current = etag_for(path)?;
        if current.as_deref() != Some(expected) {
            return Err(Error::PreconditionFailed(format!(
                "if-match {expected:?} but current etag is {:?}",
                current.as_deref().unwrap_or("<absent>")
            )));
        }
    }

    let bytes = serde_json::to_vec_pretty(file)?;
    crate::fsx::write_atomic(path, &bytes)?;
    // v0.7.1 C3 — return the ETag via the SAME `etag_for` that GET and
    // the If-Match check above use, so the value the client receives is
    // exactly what its next conditional request will compute. The old
    // `compute_etag` was a second, independent derivation (it stat()ed
    // the file separately and hashed the in-memory bytes) — a needless
    // divergence that could hand back an ETag GET wouldn't reproduce and
    // 412 the next write. `write_atomic` just succeeded, so the file is
    // present and `etag_for` is `Some`.
    etag_for(path)?.ok_or_else(|| {
        Error::Storage(format!(
            "review file {} missing immediately after write",
            path.display()
        ))
    })
}

/// Returns the current ETag for `path`, or `Ok(None)` if the file
/// doesn't exist.
///
/// M3: hashes the FULL file contents (was first-256-bytes only).
/// kb-comments review files are small (a few hundred KB at most), so
/// the cost is microseconds. The 256-byte prefix shortcut silently
/// degraded If-Match optimistic concurrency on:
/// (a) coarse-mtime filesystems (FAT, some NFS, tmpfs with 1 ms
///     granularity) where two `save_atomic` calls within the same
///     mtime tick could land identical (mtime, len, prefix) triples
///     for different bodies;
/// (b) any edit whose first 256 bytes are identical to the prior
///     version (e.g. appending a reply at the end of a long comment
///     thread — the leading `{"schema":"kb-comments/1",...}` is
///     unchanged), giving the same ETag for different content.
/// mtime + len stay in the hash as cheap fast-paths for the common
/// "definitely changed" case where the OS-level metadata diverges.
pub fn etag_for(path: &Path) -> Result<Option<String>> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let meta = std::fs::metadata(path)?;
    let mtime_ns = meta
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let len = meta.len();

    let mut h = Sha256::new();
    h.update(mtime_ns.to_le_bytes());
    h.update(len.to_le_bytes());
    h.update(&bytes);
    Ok(Some(format!("\"{}\"", hex::encode(&h.finalize()[..16]))))
}

// --- v0.7.1 H2 — pre-v0.7 review-file id migration ------------------------

/// Migrate a pre-v0.7 review file keyed on the old content-hash artifact
/// id to the v0.7 path-based id.
///
/// Pre-v0.7 the artifact id WAS the content hash, so a review file landed
/// at `.review/<content-hash>.json`. v0.7 made the id a hash of the
/// source-relative path, which left every pre-v0.7 review file
/// unreachable — comments silently stopped loading. The indexer calls
/// this per artifact with BOTH ids in hand: when the artifact's *current*
/// bytes still hash to a review file's stem, that file demonstrably
/// belongs to this artifact, so it is renamed to the path-based id.
///
/// An artifact edited *before* the upgrade already had its review file
/// orphaned under the pre-v0.7 scheme (its content hash changed then
/// too), so there is nothing this can — or should — recover for it.
///
/// Returns `Ok(true)` when a file was renamed. Idempotent and safe: a
/// no-op once the legacy file is gone, when the path-based file already
/// exists (never clobbers it), or when the two ids coincide.
pub fn migrate_legacy_id(review_dir: &Path, artifact_id: &str, content_hash: &str) -> Result<bool> {
    if artifact_id == content_hash {
        return Ok(false);
    }
    let new_path = review_dir.join(format!("{artifact_id}.json"));
    if new_path.exists() {
        return Ok(false);
    }
    let legacy_path = review_dir.join(format!("{content_hash}.json"));
    if !legacy_path.exists() {
        return Ok(false);
    }
    std::fs::rename(&legacy_path, &new_path)?;
    tracing::info!(
        from = %legacy_path.display(),
        to = %new_path.display(),
        "migrated pre-v0.7 review file to its path-based id"
    );
    Ok(true)
}

// --- v0.5 P3 — review export ----------------------------------------------

/// Render format for `kb_core::review::export`. `Claude` is the v0.2/v0.3
/// `kb comments export` shape (Claude prompt with the anchor, author,
/// and body inlined); `Json` returns the raw kb-comments/1 envelope;
/// `Markdown` is a human-readable summary without the Claude scaffolding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Claude,
    Json,
    Markdown,
}

impl ExportFormat {
    /// Parse from the `?format=` query string. Unknown values
    /// return `None` so the route returns 400 instead of guessing.
    pub fn from_query(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "claude" => Some(ExportFormat::Claude),
            "json" => Some(ExportFormat::Json),
            "md" | "markdown" => Some(ExportFormat::Markdown),
            _ => None,
        }
    }

    /// Content-Type the HTTP route should set on the response.
    pub fn content_type(self) -> &'static str {
        match self {
            ExportFormat::Claude => "text/markdown; charset=utf-8",
            ExportFormat::Markdown => "text/markdown; charset=utf-8",
            ExportFormat::Json => "application/json",
        }
    }

    /// File extension the HTTP route uses for Content-Disposition.
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Claude => "md",
            ExportFormat::Markdown => "md",
            ExportFormat::Json => "json",
        }
    }
}

/// Render a review file in the chosen format. Lifts the v0.2-era
/// `kb-cli build_claude_prompt` body so kb-cli AND the v0.5 server-
/// side `POST /api/kb/{kb}/review/{id}/export` both call one impl.
pub fn export(file: &ReviewFile, kb: &str, format: ExportFormat) -> Result<String> {
    Ok(match format {
        ExportFormat::Claude => build_claude_prompt(file, kb),
        ExportFormat::Markdown => build_markdown_summary(file, kb),
        ExportFormat::Json => serde_json::to_string_pretty(file)?,
    })
}

fn build_claude_prompt(file: &ReviewFile, kb: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Review: {}\n\n",
        if file.artifact.title.is_empty() {
            &file.artifact.id
        } else {
            &file.artifact.title
        }
    ));
    out.push_str(&format!(
        "The kb-comments/1 file lives at `<kb-state>/kb/{kb}/.review/{}.json`.\n\n",
        file.artifact.id
    ));
    let opens: Vec<&Comment> = file
        .comments
        .iter()
        .filter(|c| matches!(c.status, CommentStatus::Open))
        .collect();
    if opens.is_empty() {
        out.push_str("(no open comments)\n");
        return out;
    }
    out.push_str("Open comments to address:\n\n");
    for c in opens {
        out.push_str(&format!(
            "- **{}** ({}):\n  {}\n",
            anchor_label(&c.anchor),
            author_str(&c.author),
            c.body.replace('\n', "\n  ")
        ));
        for r in &c.replies {
            out.push_str(&format!(
                "  - ↳ {}: {}\n",
                author_str(&r.author),
                r.body.replace('\n', "\n    ")
            ));
        }
    }
    out.push_str(
        "\nWhen done, append a reply with `\"author\": \"claude\"` and (optionally) flip the comment's `\"status\": \"resolved\"`.\n",
    );
    out
}

/// Plain Markdown rendering (no Claude prompt scaffolding). Includes
/// resolved comments too with a marker, so a human reviewer sees the
/// full conversation history.
fn build_markdown_summary(file: &ReviewFile, kb: &str) -> String {
    let mut out = String::new();
    let title = if file.artifact.title.is_empty() {
        &file.artifact.id
    } else {
        &file.artifact.title
    };
    out.push_str(&format!("# {title}\n\n"));
    out.push_str(&format!(
        "kb: `{kb}` · artifact: `{}`\n\n",
        file.artifact.id
    ));
    if file.comments.is_empty() {
        out.push_str("(no comments)\n");
        return out;
    }
    for c in &file.comments {
        let marker = match c.status {
            CommentStatus::Open => "○",
            CommentStatus::Resolved => "●",
        };
        out.push_str(&format!(
            "## {marker} {} — {}\n\n  {}\n\n",
            anchor_label(&c.anchor),
            author_str(&c.author),
            c.body.replace('\n', "\n  ")
        ));
        for r in &c.replies {
            out.push_str(&format!(
                "  ↳ **{}**: {}\n\n",
                author_str(&r.author),
                r.body.replace('\n', "\n    ")
            ));
        }
    }
    out
}

fn anchor_label(a: &Anchor) -> String {
    match a {
        Anchor::File => "file".into(),
        Anchor::Chapter { path } => format!("chapter:{path}"),
        Anchor::Section { id, .. } => format!("section:{id}"),
        Anchor::Selection { snippet, .. } => format!("selection:{snippet}"),
    }
}

fn author_str(a: &Author) -> &'static str {
    match a {
        Author::You => "you",
        Author::Claude => "claude",
    }
}

// --- v0.19 — portable round-trip (borrowed from redline) -------------------
//
// The sidecar `.review/<id>.json` stays the canonical store (invariant #6);
// these functions produce/parse an OPTIONAL self-contained copy so a single
// artifact can be emailed/committed/handed off with its comments riding
// inside the HTML — closing the "not portable as a standalone file" gap the
// out-of-band design otherwise has. The inert block mirrors redline's
// `#redline-state`, but carries kb's own `kb-comments/1` envelope verbatim,
// so `import` round-trips ids, statuses, replies, and timestamps exactly
// (unlike replaying as fresh `add` ops, which would reassign ids).

/// `id` of the inert `<script type="application/json">` block that
/// [`embed_into_html`] writes and [`extract_from_html`] reads.
pub const EMBED_SCRIPT_ID: &str = "kb-review-state";

/// Inject (or replace) the review state as an inert
/// `<script type="application/json" id="kb-review-state">` block in `html`,
/// returning the standalone copy. Idempotent: a prior embedded block is
/// stripped first, so exporting twice yields one block. The block goes
/// before `</head>` (else `</body>`, else appended). `</` inside the JSON
/// is escaped to `<\/` so a comment body containing `</script>` can't close
/// the block early; [`extract_from_html`] reverses it.
pub fn embed_into_html(html: &str, file: &ReviewFile) -> Result<String> {
    let stripped = strip_embedded_state(html);
    let json = serde_json::to_string(file)?;
    let safe = json.replace("</", "<\\/");
    let block =
        format!("<script type=\"application/json\" id=\"{EMBED_SCRIPT_ID}\">{safe}</script>");
    Ok(if let Some(pos) = find_ci(&stripped, "</head>") {
        format!("{}{block}\n{}", &stripped[..pos], &stripped[pos..])
    } else if let Some(pos) = find_ci(&stripped, "</body>") {
        format!("{}{block}\n{}", &stripped[..pos], &stripped[pos..])
    } else {
        format!("{stripped}\n{block}\n")
    })
}

/// Parse the embedded `#kb-review-state` block back into a [`ReviewFile`].
/// `Ok(None)` when no block is present (a plain artifact); `Err` when the
/// block is present but malformed or carries an unsupported schema.
pub fn extract_from_html(html: &str) -> Result<Option<ReviewFile>> {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let sel = Selector::parse(&format!("script#{EMBED_SCRIPT_ID}")).expect("static selector");
    let Some(el) = doc.select(&sel).next() else {
        return Ok(None);
    };
    let raw: String = el.text().collect();
    let unescaped = raw.replace("<\\/", "</");
    let file: ReviewFile = serde_json::from_str(unescaped.trim())?;
    if file.schema != SCHEMA {
        return Err(Error::BadRequest(format!(
            "embedded review schema {:?} not supported (expected {SCHEMA})",
            file.schema
        )));
    }
    Ok(Some(file))
}

/// Remove a previously-[`embed_into_html`]-injected block (the
/// `<script…id="kb-review-state">…</script>` span) so a re-embed stays
/// idempotent. Targets only our own block; leaves all other markup intact.
fn strip_embedded_state(html: &str) -> String {
    let needle = format!("id=\"{EMBED_SCRIPT_ID}\"");
    let Some(idpos) = html.find(&needle) else {
        return html.to_string();
    };
    let Some(start) = html[..idpos].rfind("<script") else {
        return html.to_string();
    };
    let Some(end_rel) = html[idpos..].find("</script>") else {
        return html.to_string();
    };
    let end = idpos + end_rel + "</script>".len();
    let mut out = String::with_capacity(html.len());
    out.push_str(html[..start].trim_end_matches([' ', '\t']));
    // Drop a now-empty line left behind by the removed block.
    out.push_str(html[end..].strip_prefix('\n').unwrap_or(&html[end..]));
    out
}

/// Case-insensitive byte-offset search (ASCII lowercasing preserves byte
/// positions, so the index is valid in the original `haystack`).
fn find_ci(haystack: &str, needle_lower: &str) -> Option<usize> {
    haystack.to_ascii_lowercase().find(needle_lower)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fixture_kb() -> KbName {
        KbName::new("smoke").unwrap()
    }

    fn fixture_file() -> ReviewFile {
        let kb = fixture_kb();
        let mut f = ReviewFile::empty_skeleton(&kb, "abc123def456", "Borrow Checker");
        f.generated_at = Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap();
        f.comments.push(Comment {
            id: "c_1".into(),
            status: CommentStatus::Open,
            file: "abc123def456".into(),
            file_label: "main".into(),
            anchor: Anchor::Section {
                id: "intro".into(),
                tag: Some("h2".into()),
                snippet: Some("Borrow Checker is a static analysis…".into()),
            },
            author: Author::You,
            body: "what about Pin<&mut Self>?".into(),
            created_at: Utc.with_ymd_and_hms(2026, 5, 12, 10, 5, 0).unwrap(),
            edited_at: None,
            replies: vec![],
            choices: vec![],
            attachments: vec![],
            user: None,
        });
        f
    }

    #[test]
    fn round_trip_save_load() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("review.json");
        let file = fixture_file();
        let etag = save_atomic(&path, &file, None).unwrap();
        assert!(etag.starts_with('"') && etag.ends_with('"'));

        let loaded = load(&path).unwrap().unwrap();
        assert_eq!(loaded.schema, SCHEMA);
        assert_eq!(loaded.comments.len(), 1);
        assert_eq!(loaded.comments[0].id, "c_1");
        assert_eq!(loaded.comments[0].status, CommentStatus::Open);
        assert_eq!(loaded.open_count(), 1);
    }

    #[test]
    fn choices_round_trip_through_save_load() {
        // R3 — choices on both a comment and a reply survive serialize →
        // disk → deserialize, including the `resolve` flag.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("review.json");
        let mut file = fixture_file();
        file.comments[0].choices = vec![
            Choice {
                label: "Apply".into(),
                reply: "yes, apply it".into(),
                resolve: true,
            },
            Choice {
                label: "Skip".into(),
                reply: "skip for now".into(),
                resolve: false,
            },
        ];
        file.comments[0].replies.push(Reply {
            id: "r_1".into(),
            author: Author::Claude,
            body: "want me to fix?".into(),
            created_at: Utc::now(),
            edited_at: None,
            choices: vec![Choice {
                label: "Fix".into(),
                reply: "go ahead".into(),
                resolve: false,
            }],
            attachments: vec![],
            user: None,
        });
        save_atomic(&path, &file, None).unwrap();

        let loaded = load(&path).unwrap().unwrap();
        let c = &loaded.comments[0];
        assert_eq!(c.choices.len(), 2);
        assert_eq!(c.choices[0].label, "Apply");
        assert!(c.choices[0].resolve);
        assert!(!c.choices[1].resolve);
        assert_eq!(c.replies[0].choices.len(), 1);
        assert_eq!(c.replies[0].choices[0].reply, "go ahead");
    }

    #[test]
    fn choiceless_file_loads_with_empty_choices() {
        // A pre-R3 kb-comments/1 file has no `choices` key on the comment
        // or its reply. #[serde(default)] must backfill empty vecs so old
        // review files load unchanged.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("old.json");
        std::fs::write(
            &path,
            r#"{
              "schema":"kb-comments/1",
              "artifact":{"id":"x","title":"y","kb":"z"},
              "generatedAt":"2026-05-12T10:00:00Z",
              "comments":[{
                "id":"c_1","status":"open","file":"x","fileLabel":"main",
                "anchor":{"kind":"file"},"author":"you","body":"hi",
                "createdAt":"2026-05-12T10:05:00Z","editedAt":null,
                "replies":[{"id":"r_1","author":"claude","body":"yo","createdAt":"2026-05-12T10:06:00Z"}]
              }]
            }"#,
        )
        .unwrap();
        let loaded = load(&path).unwrap().unwrap();
        assert_eq!(loaded.comments.len(), 1);
        assert!(loaded.comments[0].choices.is_empty());
        assert_eq!(loaded.comments[0].replies.len(), 1);
        assert!(loaded.comments[0].replies[0].choices.is_empty());
        // v0.34 X1 — pre-multi-user files have no user field → None.
        assert!(loaded.comments[0].user.is_none());
        assert!(loaded.comments[0].replies[0].user.is_none());
    }

    /// v0.34 X1 — old review files without `user` load as None; new files
    /// with user round-trip; the golden choiceless fixture stays valid.
    #[test]
    fn user_attribution_round_trips_and_old_files_default_none() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("review.json");
        let mut file = fixture_file();
        file.comments[0].user = Some("alice".into());
        file.add_reply(
            "c_1",
            Author::Claude,
            "on it".into(),
            vec![],
            Some("bob".into()),
        )
        .unwrap();
        file.set_verdict(
            VerdictState::Approve,
            Some("lgtm".into()),
            Some("alice".into()),
        );
        save_atomic(&path, &file, None).unwrap();

        let loaded = load(&path).unwrap().unwrap();
        assert_eq!(loaded.comments[0].user.as_deref(), Some("alice"));
        assert_eq!(loaded.comments[0].replies[0].user.as_deref(), Some("bob"));
        assert_eq!(
            loaded.verdict.as_ref().and_then(|v| v.user.as_deref()),
            Some("alice")
        );
        // Wire still schema kb-comments/1; absent user is omitted.
        // save_atomic writes pretty JSON (to_vec_pretty) — keys carry ": ".
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"user\": \"alice\""));
        assert!(raw.contains("\"schema\": \"kb-comments/1\""));
        assert!(
            !raw.contains("\"user\": null"),
            "absent user must be omitted, not null"
        );
    }

    #[test]
    fn load_missing_returns_ok_none() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nope.json");
        assert!(load(&path).unwrap().is_none());
    }

    #[test]
    fn load_rejects_unknown_schema() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bad.json");
        std::fs::write(
            &path,
            r#"{"schema":"kb-comments/99","artifact":{"id":"x","title":"y","kb":"z"},"generatedAt":"2026-05-12T10:00:00Z"}"#,
        )
        .unwrap();
        let err = load(&path).expect_err("expected schema rejection");
        assert!(matches!(err, Error::BadRequest(_)));
    }

    #[test]
    fn etag_changes_on_modification() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("review.json");
        let mut file = fixture_file();
        let e1 = save_atomic(&path, &file, None).unwrap();
        // Sleep just enough to bump mtime_ns even on coarse-grained FS.
        std::thread::sleep(std::time::Duration::from_millis(10));
        file.comments[0].body = "edited".into();
        file.comments[0].edited_at = Some(Utc::now());
        let e2 = save_atomic(&path, &file, Some(&e1)).unwrap();
        assert_ne!(e1, e2, "etag must change after modify");
    }

    #[test]
    fn etag_distinguishes_files_with_identical_256b_prefix() {
        // M3 regression: the pre-fix `etag_for` hashed only the first
        // 256 bytes. kb-comments review files start with a long
        // `{"schema":"kb-comments/1","artifact":{"id":..., ...}}`
        // prelude — two edits that change ONLY the tail (e.g.
        // appending a reply at the end of a long thread) would yield
        // identical (mtime?, len, prefix) triples and the same ETag.
        // The post-fix hash covers the full body.
        use sha2::Sha256;
        let tmp = tempfile::tempdir().unwrap();
        let path_a = tmp.path().join("a.json");
        let path_b = tmp.path().join("b.json");

        // Identical first 512 bytes (well past the old 256B prefix),
        // identical lengths (so the cheap fast-path doesn't save us),
        // different bytes after.
        let prefix = "x".repeat(512);
        let a = format!("{prefix}TAIL_A");
        let b = format!("{prefix}TAIL_B");
        assert_eq!(
            a.len(),
            b.len(),
            "lengths must match to bypass len fast-path"
        );
        std::fs::write(&path_a, a.as_bytes()).unwrap();
        std::fs::write(&path_b, b.as_bytes()).unwrap();
        // Equalise mtime so the (mtime, len, prefix) triple is identical
        // pre-fix. utimes is sketchy from Rust stdlib; instead, demonstrate
        // via direct hash that ignoring tail bytes would collide.
        let _ = Sha256::new();

        let etag_a = etag_for(&path_a).unwrap().unwrap();
        let etag_b = etag_for(&path_b).unwrap().unwrap();
        assert_ne!(
            etag_a, etag_b,
            "two files differing ONLY in bytes 512+ must have distinct ETags"
        );
    }

    #[test]
    fn save_with_stale_etag_returns_precondition_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("review.json");
        let file = fixture_file();
        let _e1 = save_atomic(&path, &file, None).unwrap();
        let err = save_atomic(&path, &file, Some("\"deadbeefdeadbeef\""))
            .expect_err("expected stale-etag rejection");
        assert!(matches!(err, Error::PreconditionFailed(_)));
    }

    #[test]
    fn save_with_correct_etag_succeeds() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("review.json");
        let mut file = fixture_file();
        let e1 = save_atomic(&path, &file, None).unwrap();
        file.comments[0].body = "second".into();
        let e2 = save_atomic(&path, &file, Some(&e1)).unwrap();
        assert_ne!(e1, e2);
    }

    #[test]
    fn save_atomic_etag_matches_etag_for() {
        // v0.7.1 C3 — the ETag `save_atomic` hands back must be exactly
        // what `etag_for` computes for the file it just wrote, so the
        // client's next conditional request doesn't spuriously 412.
        // Pre-C3 `save_atomic` derived it via a separate `compute_etag`.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("review.json");
        let returned = save_atomic(&path, &fixture_file(), None).unwrap();
        let from_etag_for = etag_for(&path).unwrap().expect("file exists");
        assert_eq!(
            returned, from_etag_for,
            "save_atomic's etag must equal etag_for's"
        );
        // The returned etag round-trips an immediate If-Match write —
        // no spurious precondition failure, no mtime-jitter sleep needed.
        let mut edited = fixture_file();
        edited.comments[0].body = "edited".into();
        save_atomic(&path, &edited, Some(&returned))
            .expect("If-Match with the returned etag must succeed");
    }

    #[test]
    fn save_creates_missing_parent_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested/under/review.json");
        let file = fixture_file();
        save_atomic(&path, &file, None).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn etag_for_missing_file_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("absent.json");
        assert!(etag_for(&path).unwrap().is_none());
    }

    // --- v0.7.1 H2 — legacy review-file id migration --------------------

    #[test]
    fn migrate_legacy_id_renames_legacy_file_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let content_hash = "aaaaaaaaaaaa";
        let artifact_id = "bbbbbbbbbbbb";
        let legacy = dir.join(format!("{content_hash}.json"));
        save_atomic(&legacy, &fixture_file(), None).unwrap();

        assert!(
            migrate_legacy_id(dir, artifact_id, content_hash).unwrap(),
            "first call should migrate"
        );
        assert!(!legacy.exists(), "legacy file should be gone");
        let new_path = dir.join(format!("{artifact_id}.json"));
        assert!(new_path.exists(), "path-based file should exist");
        // Content survives the rename.
        assert_eq!(load(&new_path).unwrap().unwrap().comments.len(), 1);

        // Idempotent — the legacy file is gone, so the second call no-ops.
        assert!(!migrate_legacy_id(dir, artifact_id, content_hash).unwrap());
    }

    #[test]
    fn migrate_legacy_id_never_clobbers_existing_path_based_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let content_hash = "cccccccccccc";
        let artifact_id = "dddddddddddd";
        let legacy = dir.join(format!("{content_hash}.json"));
        let new_path = dir.join(format!("{artifact_id}.json"));
        save_atomic(&legacy, &fixture_file(), None).unwrap();
        // A native v0.7 review file already exists at the path-based id.
        save_atomic(&new_path, &fixture_file(), None).unwrap();

        assert!(!migrate_legacy_id(dir, artifact_id, content_hash).unwrap());
        assert!(
            legacy.exists(),
            "legacy file left untouched — never clobbers the live file"
        );
        assert!(new_path.exists());
    }

    #[test]
    fn migrate_legacy_id_noop_when_nothing_to_migrate() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // No legacy file present.
        assert!(!migrate_legacy_id(dir, "eeeeeeeeeeee", "ffffffffffff").unwrap());
        // Ids coincide (degenerate — content hash == path hash).
        assert!(!migrate_legacy_id(dir, "111111111111", "111111111111").unwrap());
    }

    #[test]
    fn anchor_serde_round_trip_each_variant() {
        // File-scope
        let a = Anchor::File;
        let s = serde_json::to_string(&a).unwrap();
        let _: Anchor = serde_json::from_str(&s).unwrap();
        assert_eq!(s, r#"{"kind":"file"}"#);

        // Chapter-scope
        let a = Anchor::Chapter {
            path: "Intro > Setup".into(),
        };
        let s = serde_json::to_string(&a).unwrap();
        assert!(s.contains(r#""kind":"chapter""#));
        assert!(s.contains(r#""path":"Intro > Setup""#));

        // Section-scope
        let a = Anchor::Section {
            id: "section-1".into(),
            tag: Some("h2".into()),
            snippet: None,
        };
        let s = serde_json::to_string(&a).unwrap();
        assert!(s.contains(r#""kind":"section""#));

        // Selection-scope
        let a = Anchor::Selection {
            css_path: "main > p:nth-child(3)".into(),
            offset: 42,
            snippet: "the borrow checker".into(),
        };
        let s = serde_json::to_string(&a).unwrap();
        assert!(s.contains(r#""kind":"selection""#));
        assert!(s.contains(r#""offset":42"#));
    }

    #[test]
    fn empty_skeleton_has_no_comments_and_correct_schema() {
        let kb = fixture_kb();
        let f = ReviewFile::empty_skeleton(&kb, "id1", "Title");
        assert_eq!(f.schema, SCHEMA);
        assert_eq!(f.artifact.id, "id1");
        assert_eq!(f.artifact.title, "Title");
        assert_eq!(f.artifact.kb, "smoke");
        assert!(f.comments.is_empty());
        assert_eq!(f.open_count(), 0);
    }

    #[test]
    fn open_count_excludes_resolved() {
        let kb = fixture_kb();
        let mut f = ReviewFile::empty_skeleton(&kb, "id1", "T");
        f.comments.push(Comment {
            id: "a".into(),
            status: CommentStatus::Open,
            file: "id1".into(),
            file_label: "main".into(),
            anchor: Anchor::File,
            author: Author::You,
            body: "x".into(),
            created_at: Utc::now(),
            edited_at: None,
            replies: vec![],
            choices: vec![],
            attachments: vec![],
            user: None,
        });
        f.comments.push(Comment {
            id: "b".into(),
            status: CommentStatus::Resolved,
            file: "id1".into(),
            file_label: "main".into(),
            anchor: Anchor::File,
            author: Author::You,
            body: "y".into(),
            created_at: Utc::now(),
            edited_at: None,
            replies: vec![],
            choices: vec![],
            attachments: vec![],
            user: None,
        });
        assert_eq!(f.open_count(), 1);
    }

    // --- R4 typed mutations ---------------------------------------------

    fn spec(body: &str) -> NewComment {
        NewComment {
            file: "abc123def456".into(),
            file_label: "main".into(),
            anchor: Anchor::File,
            author: Author::Claude,
            body: body.into(),
            choices: vec![],
            attachments: vec![],
            user: None,
        }
    }

    #[test]
    fn new_ids_are_prefixed_and_unique() {
        let a = new_comment_id();
        let b = new_comment_id();
        assert!(a.starts_with("c_") && a.len() == 14);
        assert_ne!(a, b, "two comment ids should differ");
        let r = new_reply_id();
        assert!(r.starts_with("r_") && r.len() == 14);
    }

    #[test]
    fn add_comment_assigns_open_status_and_id() {
        let kb = fixture_kb();
        let mut f = ReviewFile::empty_skeleton(&kb, "abc123def456", "T");
        let added = f.add_comment(spec("hello"));
        assert!(added.id.starts_with("c_"));
        assert_eq!(added.status, CommentStatus::Open);
        assert_eq!(added.body, "hello");
        assert!(added.edited_at.is_none());
        assert!(added.replies.is_empty());
        assert_eq!(f.comments.len(), 1);
        assert_eq!(f.open_count(), 1);
    }

    #[test]
    fn add_reply_appends_and_errors_on_missing_comment() {
        let mut f = fixture_file();
        let reply = f
            .add_reply("c_1", Author::Claude, "on it".into(), vec![], None)
            .unwrap();
        assert!(reply.id.starts_with("r_"));
        assert!(reply.edited_at.is_none());
        assert_eq!(f.comments[0].replies.len(), 1);

        let err = f
            .add_reply("c_nope", Author::Claude, "x".into(), vec![], None)
            .unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    #[test]
    fn set_comment_status_flips_and_errors_on_missing() {
        let mut f = fixture_file();
        f.set_comment_status("c_1", CommentStatus::Resolved)
            .unwrap();
        assert_eq!(f.comments[0].status, CommentStatus::Resolved);
        assert_eq!(f.open_count(), 0);
        f.set_comment_status("c_1", CommentStatus::Open).unwrap();
        assert_eq!(f.open_count(), 1);
        assert!(matches!(
            f.set_comment_status("c_x", CommentStatus::Resolved),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn set_all_status_counts_only_flipped() {
        let kb = fixture_kb();
        let mut f = ReviewFile::empty_skeleton(&kb, "id", "T");
        f.add_comment(spec("a"));
        f.add_comment(spec("b"));
        let c_resolved = f.add_comment(spec("c")).id.clone();
        f.set_comment_status(&c_resolved, CommentStatus::Resolved)
            .unwrap();
        // 2 open + 1 resolved; resolve-all flips the 2 open only.
        assert_eq!(f.set_all_status(CommentStatus::Resolved), 2);
        assert_eq!(f.open_count(), 0);
        // Already all resolved → 0 flipped (idempotent).
        assert_eq!(f.set_all_status(CommentStatus::Resolved), 0);
        // unresolve-all flips all 3 back.
        assert_eq!(f.set_all_status(CommentStatus::Open), 3);
    }

    #[test]
    fn edit_comment_body_sets_edited_at() {
        let mut f = fixture_file();
        assert!(f.comments[0].edited_at.is_none());
        f.edit_comment_body("c_1", "revised".into()).unwrap();
        assert_eq!(f.comments[0].body, "revised");
        assert!(f.comments[0].edited_at.is_some());
        assert!(matches!(
            f.edit_comment_body("c_x", "y".into()),
            Err(Error::NotFound(_))
        ));
    }

    // invariant:6 reanchor
    #[test]
    fn set_comment_anchor_repoints_without_touching_edited_at() {
        let mut f = fixture_file();
        // fixture starts on a Section anchor with no edited_at.
        assert!(matches!(f.comments[0].anchor, Anchor::Section { .. }));
        assert!(f.comments[0].edited_at.is_none());

        f.set_comment_anchor(
            "c_1",
            Anchor::Section {
                id: "overview".into(),
                tag: Some("h2".into()),
                snippet: None,
            },
        )
        .unwrap();
        match &f.comments[0].anchor {
            Anchor::Section { id, .. } => assert_eq!(id, "overview"),
            other => panic!("expected section anchor, got {other:?}"),
        }
        // Re-pointing an anchor is not a body edit — `edited_at` stays None.
        assert!(f.comments[0].edited_at.is_none());

        // Any variant is accepted (File here).
        f.set_comment_anchor("c_1", Anchor::File).unwrap();
        assert!(matches!(f.comments[0].anchor, Anchor::File));

        // Missing comment → NotFound.
        assert!(matches!(
            f.set_comment_anchor("c_x", Anchor::File),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn edit_reply_body_distinguishes_missing_comment_from_missing_reply() {
        let mut f = fixture_file();
        let rid = f
            .add_reply("c_1", Author::Claude, "first".into(), vec![], None)
            .unwrap()
            .id
            .clone();
        f.edit_reply_body("c_1", &rid, "amended".into()).unwrap();
        assert_eq!(f.comments[0].replies[0].body, "amended");
        assert!(f.comments[0].replies[0].edited_at.is_some());
        // present comment, absent reply
        assert!(matches!(
            f.edit_reply_body("c_1", "r_nope", "z".into()),
            Err(Error::NotFound(_))
        ));
        // absent comment
        assert!(matches!(
            f.edit_reply_body("c_nope", &rid, "z".into()),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn delete_comment_returns_removed_and_errors_on_missing() {
        let mut f = fixture_file();
        let removed = f.delete_comment("c_1").unwrap();
        assert_eq!(removed.id, "c_1");
        assert!(f.comments.is_empty());
        assert!(matches!(f.delete_comment("c_1"), Err(Error::NotFound(_))));
    }

    #[test]
    fn delete_reply_returns_removed_and_errors_on_both_absences() {
        let mut f = fixture_file();
        let rid = f
            .add_reply("c_1", Author::You, "hi".into(), vec![], None)
            .unwrap()
            .id
            .clone();
        let removed = f.delete_reply("c_1", &rid).unwrap();
        assert_eq!(removed.id, rid);
        assert!(f.comments[0].replies.is_empty());
        // reply already gone
        assert!(matches!(
            f.delete_reply("c_1", &rid),
            Err(Error::NotFound(_))
        ));
        // comment gone
        assert!(matches!(
            f.delete_reply("c_nope", &rid),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn reply_edited_at_backfills_default_on_legacy_file() {
        // A pre-R4 reply JSON has no `editedAt`; #[serde(default)] must
        // load it as None and round-trip cleanly.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("legacy.json");
        std::fs::write(
            &path,
            r#"{
              "schema":"kb-comments/1",
              "artifact":{"id":"x","title":"y","kb":"z"},
              "generatedAt":"2026-05-12T10:00:00Z",
              "comments":[{
                "id":"c_1","status":"open","file":"x","fileLabel":"main",
                "anchor":{"kind":"file"},"author":"you","body":"hi",
                "createdAt":"2026-05-12T10:05:00Z","editedAt":null,
                "replies":[{"id":"r_1","author":"claude","body":"yo","createdAt":"2026-05-12T10:06:00Z"}]
              }]
            }"#,
        )
        .unwrap();
        let loaded = load(&path).unwrap().unwrap();
        assert!(loaded.comments[0].replies[0].edited_at.is_none());
    }

    // --- Y1 attachments -------------------------------------------------

    fn mk_attachment(id: &str) -> Attachment {
        Attachment {
            id: id.into(),
            filename: "chart.png".into(),
            content_type: "image/png".into(),
            size: 2048,
            created_at: Utc::now(),
            author: Author::You,
            user: None,
        }
    }

    #[test]
    fn attachments_round_trip_through_save_load() {
        // An attachment on a comment AND on a reply survives serialize →
        // disk → deserialize, including the camelCase `contentType`.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("review.json");
        let mut file = fixture_file();
        file.comments[0]
            .attachments
            .push(mk_attachment("a_aaaa0001"));
        file.comments[0].replies.push(Reply {
            id: "r_1".into(),
            author: Author::Claude,
            body: "see attached".into(),
            created_at: Utc::now(),
            edited_at: None,
            choices: vec![],
            attachments: vec![Attachment {
                id: "a_aaaa0002".into(),
                filename: "spec.pdf".into(),
                content_type: "application/pdf".into(),
                size: 9001,
                created_at: Utc::now(),
                author: Author::Claude,
                user: None,
            }],
            user: None,
        });
        save_atomic(&path, &file, None).unwrap();

        let loaded = load(&path).unwrap().unwrap();
        let c = &loaded.comments[0];
        assert_eq!(c.attachments.len(), 1);
        assert_eq!(c.attachments[0].filename, "chart.png");
        assert_eq!(c.attachments[0].content_type, "image/png");
        assert_eq!(c.attachments[0].size, 2048);
        assert_eq!(c.replies[0].attachments.len(), 1);
        assert_eq!(c.replies[0].attachments[0].id, "a_aaaa0002");

        // The camelCase rename is on the wire.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"contentType\""), "got: {raw}");
        assert!(!raw.contains("\"content_type\""));
    }

    #[test]
    fn attachmentless_file_loads_with_empty_attachments() {
        // A pre-Y kb-comments/1 file has no `attachments` key on the comment
        // or its reply. #[serde(default)] must backfill empty vecs so old
        // review files load unchanged (mirrors the choices/edited_at story).
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("old.json");
        std::fs::write(
            &path,
            r#"{
              "schema":"kb-comments/1",
              "artifact":{"id":"x","title":"y","kb":"z"},
              "generatedAt":"2026-05-12T10:00:00Z",
              "comments":[{
                "id":"c_1","status":"open","file":"x","fileLabel":"main",
                "anchor":{"kind":"file"},"author":"you","body":"hi",
                "createdAt":"2026-05-12T10:05:00Z","editedAt":null,
                "replies":[{"id":"r_1","author":"claude","body":"yo","createdAt":"2026-05-12T10:06:00Z"}]
              }]
            }"#,
        )
        .unwrap();
        let loaded = load(&path).unwrap().unwrap();
        assert!(loaded.comments[0].attachments.is_empty());
        assert!(loaded.comments[0].replies[0].attachments.is_empty());
    }

    // --- W2.15a verdict ---------------------------------------------------

    #[test]
    fn pre_verdict_file_loads_unchanged() {
        // A pre-W2.15a kb-comments/1 file has no `verdict` key at all.
        // #[serde(default)] must backfill `None` — same additive story as
        // choices/attachments/edited_at above.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("old.json");
        std::fs::write(
            &path,
            r#"{
              "schema":"kb-comments/1",
              "artifact":{"id":"x","title":"y","kb":"z"},
              "generatedAt":"2026-05-12T10:00:00Z",
              "comments":[]
            }"#,
        )
        .unwrap();
        let loaded = load(&path).unwrap().unwrap();
        assert!(loaded.verdict.is_none());
    }

    #[test]
    fn verdict_round_trips_through_save_load_and_omits_key_when_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("review.json");
        let mut file = fixture_file();
        assert!(file.verdict.is_none());

        // Unset verdict: the key is omitted entirely (not `"verdict":null`)
        // — skip_serializing_if in action.
        save_atomic(&path, &file, None).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("\"verdict\""), "got: {raw}");

        assert!(file.set_verdict(
            VerdictState::RequestChanges,
            Some("fix the typo".into()),
            None
        ));
        save_atomic(&path, &file, None).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"request_changes\""), "got: {raw}");

        let loaded = load(&path).unwrap().unwrap();
        let v = loaded.verdict.expect("verdict set");
        assert_eq!(v.state, VerdictState::RequestChanges);
        assert_eq!(v.by, Author::You);
        assert_eq!(v.note.as_deref(), Some("fix the typo"));
    }

    #[test]
    fn set_verdict_is_a_noop_when_state_and_note_are_unchanged() {
        let mut f = fixture_file();
        assert!(f.set_verdict(VerdictState::Approve, None, None));
        let first_at = f.verdict.as_ref().unwrap().at;
        // Re-setting the SAME state+note is a no-op (G8): no fresh
        // timestamp, no `mutated` flag for a caller checking one.
        assert!(!f.set_verdict(VerdictState::Approve, None, None));
        assert_eq!(f.verdict.as_ref().unwrap().at, first_at);
        // A different note on the SAME state is a real change.
        assert!(f.set_verdict(VerdictState::Approve, Some("nice work".into()), None));
        // A different state is a real change too.
        assert!(f.set_verdict(VerdictState::RequestChanges, Some("nice work".into()), None));
    }

    #[test]
    fn clear_verdict_is_a_noop_when_already_absent() {
        let mut f = fixture_file();
        assert!(!f.clear_verdict());
        assert!(f.set_verdict(VerdictState::Comment, None, None));
        assert!(f.clear_verdict());
        assert!(f.verdict.is_none());
        assert!(!f.clear_verdict());
    }

    #[test]
    fn apply_ops_set_and_clear_verdict() {
        let mut f = fixture_file();
        let ops = vec![
            BatchOp::SetVerdict {
                state: VerdictState::Approve,
                note: None,
            },
            BatchOp::ClearVerdict,
        ];
        let report = f.apply_ops(&ops, "abc123def456").unwrap();
        assert_eq!(report.applied, 2);
        assert!(report.mutated);
        assert!(f.verdict.is_none());
    }

    #[test]
    fn batch_op_set_verdict_deserialises_tagged_wire_shape() {
        let json = r#"[
          {"op":"set_verdict","state":"request_changes","note":"see comment 1"},
          {"op":"clear_verdict"}
        ]"#;
        let ops: Vec<BatchOp> = serde_json::from_str(json).unwrap();
        assert_eq!(ops.len(), 2);
        assert!(matches!(
            &ops[0],
            BatchOp::SetVerdict {
                state: VerdictState::RequestChanges,
                note: Some(n),
            } if n == "see comment 1"
        ));
        assert!(matches!(ops[1], BatchOp::ClearVerdict));
    }

    #[test]
    fn new_attachment_id_is_prefixed_and_unique() {
        let a = new_attachment_id();
        let b = new_attachment_id();
        assert!(a.starts_with("a_") && a.len() == 14);
        assert_ne!(a, b, "two attachment ids should differ");
    }

    #[test]
    fn attachment_adopt_detach_and_referenced_ids() {
        let mut f = fixture_file();
        let rid = f
            .add_reply("c_1", Author::Claude, "first".into(), vec![], None)
            .unwrap()
            .id
            .clone();
        f.add_comment_attachment("c_1", mk_attachment("a_c1"))
            .unwrap();
        f.add_reply_attachment("c_1", &rid, mk_attachment("a_r1"))
            .unwrap();

        let refs = f.referenced_attachment_ids();
        assert_eq!(refs.len(), 2);
        assert!(refs.contains("a_c1") && refs.contains("a_r1"));

        // Detach returns the removed metadata.
        let removed = f.remove_comment_attachment("c_1", "a_c1").unwrap();
        assert_eq!(removed.id, "a_c1");
        assert!(f.comments[0].attachments.is_empty());
        assert_eq!(
            f.remove_reply_attachment("c_1", &rid, "a_r1").unwrap().id,
            "a_r1"
        );
        assert!(f.referenced_attachment_ids().is_empty());

        // NotFound discrimination across all four mutations.
        assert!(matches!(
            f.add_comment_attachment("c_x", mk_attachment("z")),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            f.add_reply_attachment("c_1", "r_x", mk_attachment("z")),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            f.remove_comment_attachment("c_1", "a_gone"),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            f.remove_reply_attachment("c_1", &rid, "a_gone"),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            f.remove_reply_attachment("c_1", "r_x", "a_gone"),
            Err(Error::NotFound(_))
        ));
    }

    // --- v0.3 G1 fuzzy resolver -----------------------------------------

    #[test]
    fn jaro_winkler_identical_returns_one() {
        assert!((jaro_winkler("hello", "hello") - 1.0).abs() < 1e-6);
    }

    #[test]
    fn jaro_winkler_disjoint_returns_low_score() {
        assert!(jaro_winkler("abcdef", "ghijkl") < 0.3);
    }

    #[test]
    fn jaro_winkler_typo_above_threshold() {
        // Single typo in a 30-char string should easily clear 0.85.
        let original = "Borrow Checker is a static analysis";
        let typo = "Borow Checker is a static analysis";
        let s = jaro_winkler(original, typo);
        assert!(s >= 0.85, "got {s}");
    }

    #[test]
    fn resolve_file_anchor_always_exact() {
        assert_eq!(
            fuzzy_resolve_anchor("<p>nothing</p>", &Anchor::File),
            Resolution::Exact("file".to_string())
        );
    }

    #[test]
    fn resolve_section_exact_id_match() {
        let html = r#"<section id="intro"><p>hi</p></section>"#;
        let anchor = Anchor::Section {
            id: "intro".into(),
            tag: Some("section".into()),
            snippet: None,
        };
        assert_eq!(
            fuzzy_resolve_anchor(html, &anchor),
            Resolution::Exact("intro".to_string())
        );
    }

    #[test]
    fn resolve_with_shared_dom_slot_matches_string_form() {
        // The indexer's anchor loops thread ONE parsed-DOM slot through
        // many anchors; every answer must equal the parse-per-call form,
        // and a File anchor must not fill the slot (it needs no DOM).
        let html = r#"<h1>Top</h1><section id="intro"><p>hello world text</p></section>"#;
        let anchors = [
            Anchor::File,
            Anchor::Section {
                id: "intro".into(),
                tag: Some("section".into()),
                snippet: None,
            },
            Anchor::Chapter { path: "Top".into() },
            Anchor::Section {
                id: "missing".into(),
                tag: None,
                snippet: None,
            },
        ];
        let mut dom = None;
        let shared: Vec<Resolution> = anchors
            .iter()
            .map(|a| {
                let r = fuzzy_resolve_anchor_with(&mut dom, html, a);
                if matches!(a, Anchor::File) {
                    assert!(dom.is_none(), "File anchor must not parse");
                }
                r
            })
            .collect();
        let fresh: Vec<Resolution> = anchors
            .iter()
            .map(|a| fuzzy_resolve_anchor(html, a))
            .collect();
        assert_eq!(shared, fresh);
        assert!(dom.is_some(), "DOM-needing anchors fill the slot once");
    }

    #[test]
    fn resolve_section_data_kb_id_match() {
        let html = r#"<div data-kb-id="hero">…</div>"#;
        let anchor = Anchor::Section {
            id: "hero".into(),
            tag: None,
            snippet: None,
        };
        assert!(matches!(
            fuzzy_resolve_anchor(html, &anchor),
            Resolution::Exact(_)
        ));
    }

    #[test]
    fn resolve_section_missing_id_is_stale() {
        let html = "<p>nothing relevant</p>";
        let anchor = Anchor::Section {
            id: "nope".into(),
            tag: None,
            snippet: None,
        };
        assert_eq!(fuzzy_resolve_anchor(html, &anchor), Resolution::Stale);
    }

    #[test]
    fn resolve_section_handles_ids_with_special_chars() {
        // M4 regression: HTML5 ids can contain `.`, `:`, `[`, `]`,
        // whitespace, etc. Pre-fix `resolve_section` built a CSS
        // selector via `format!("[id=\"{escaped}\"]")` and only escaped
        // `\\` and `"`. Any id with `:` (CSS pseudo), `]` (attribute-
        // selector terminator), whitespace (selector separator), or
        // other CSS-meaningful chars failed `Selector::parse` →
        // returned Stale even though the element existed.
        for tricky_id in [
            "my-section.5",  // dot
            "ns:foo",        // colon (e.g. namespaced)
            "id with space", // whitespace
            "weird]id",      // closing bracket
            "star*id",       // wildcard
            "with(parens)",  // parens
        ] {
            let html = format!(r#"<section id="{tricky_id}"><p>x</p></section>"#);
            let anchor = Anchor::Section {
                id: tricky_id.to_string(),
                tag: Some("section".into()),
                snippet: None,
            };
            let resolved = fuzzy_resolve_anchor(&html, &anchor);
            assert_eq!(
                resolved,
                Resolution::Exact(tricky_id.to_string()),
                "id {tricky_id:?} should resolve exactly; got {resolved:?}"
            );
        }
    }

    #[test]
    fn resolve_chapter_exact_heading_text() {
        let html = "<h1>Top</h1><h2>Borrow Checker</h2><p>x</p>";
        let anchor = Anchor::Chapter {
            path: "Top > Borrow Checker".into(),
        };
        assert_eq!(
            fuzzy_resolve_anchor(html, &anchor),
            Resolution::Exact("Borrow Checker".to_string())
        );
    }

    #[test]
    fn resolve_chapter_handles_gt_in_heading_text() {
        // v0.7.1 P2 — a heading containing `>` (generics, math,
        // breadcrumbs). Splitting the path on a bare `>` mis-parsed the
        // leaf as " internals" and the comment went stale every reindex;
        // splitting on the `" > "` separator keeps the full leaf.
        let html = "<h1>Top</h1><h2>Vec&lt;T&gt; internals</h2><p>x</p>";
        let anchor = Anchor::Chapter {
            path: "Top > Vec<T> internals".into(),
        };
        assert_eq!(
            fuzzy_resolve_anchor(html, &anchor),
            Resolution::Exact("Vec<T> internals".to_string())
        );
    }

    #[test]
    fn resolve_chapter_token_set_overlap_clears_threshold() {
        // Words reordered + minor edits — still matches by tokens.
        let html = "<h1>Top</h1><h2>Borrow checker, revised</h2>";
        let anchor = Anchor::Chapter {
            path: "Top > Borrow Checker".into(),
        };
        match fuzzy_resolve_anchor(html, &anchor) {
            Resolution::Fuzzy(text, score) => {
                assert!(score >= 0.5, "got {score}");
                assert!(text.contains("Borrow"));
            }
            other => panic!("expected Fuzzy, got {other:?}"),
        }
    }

    #[test]
    fn resolve_chapter_unrelated_text_is_stale() {
        let html = "<h1>Completely Different</h1>";
        let anchor = Anchor::Chapter {
            path: "Top > Borrow Checker".into(),
        };
        assert_eq!(fuzzy_resolve_anchor(html, &anchor), Resolution::Stale);
    }

    #[test]
    fn resolve_selection_exact_paragraph_match() {
        let snippet = "Borrow Checker is a static analysis pass that runs at compile time.";
        let html = format!("<p>{snippet}</p>");
        let anchor = Anchor::Selection {
            css_path: "main > p:nth-of-type(1)".into(),
            offset: 0,
            snippet: snippet.into(),
        };
        assert_eq!(
            fuzzy_resolve_anchor(&html, &anchor),
            Resolution::Exact(snippet.to_string())
        );
    }

    #[test]
    fn resolve_selection_typo_clears_threshold() {
        let original = "Borrow Checker is a static analysis pass that runs at compile time.";
        let with_typo = "Borow Checker is a static analysis pass that runs at compile time.";
        let html = format!("<p>{with_typo}</p>");
        let anchor = Anchor::Selection {
            css_path: "main > p:nth-of-type(1)".into(),
            offset: 0,
            snippet: original.into(),
        };
        match fuzzy_resolve_anchor(&html, &anchor) {
            Resolution::Fuzzy(_, score) => assert!(score >= 0.85, "got {score}"),
            other => panic!("expected Fuzzy, got {other:?}"),
        }
    }

    #[test]
    fn resolve_selection_complete_rewrite_is_stale() {
        let original = "Borrow Checker is a static analysis pass that runs at compile time.";
        let unrelated = "<p>The Hilbert space is the linear algebra construct.</p>";
        let anchor = Anchor::Selection {
            css_path: "p".into(),
            offset: 0,
            snippet: original.into(),
        };
        assert_eq!(fuzzy_resolve_anchor(unrelated, &anchor), Resolution::Stale);
    }

    #[test]
    fn fuzzy_threshold_env_override_applies() {
        // Crank threshold up so a near-match still returns Stale.
        std::env::set_var("KB_COMMENT_FUZZY_THRESHOLD", "0.99");
        let html = "<p>almost identical text here, but not quite the same.</p>";
        let anchor = Anchor::Selection {
            css_path: "p".into(),
            offset: 0,
            snippet: "almost identical text here, but not quite same.".into(),
        };
        let outcome = fuzzy_resolve_anchor(html, &anchor);
        std::env::remove_var("KB_COMMENT_FUZZY_THRESHOLD");
        assert!(
            matches!(outcome, Resolution::Stale | Resolution::Fuzzy(_, _)),
            "got {outcome:?}"
        );
    }

    // --- v0.19 — duplicate-snippet disambiguation by css_path -----------

    /// Two near-identical blocks that tie on Jaro-Winkler; the stored
    /// css_path points at the SECOND one (inside a `<section>`). Pre-v0.19
    /// the resolver kept the first match and silently mis-anchored; now the
    /// structural-path tiebreak picks the right occurrence.
    #[test]
    fn resolve_selection_disambiguates_duplicate_by_css_path() {
        // Both differ from the needle by exactly one trailing char, so they
        // score identically — only the path breaks the tie.
        let html = "<p>Alpha beta gamma delta epsilonX</p>\
                    <section><p>Alpha beta gamma delta epsilonY</p></section>";
        let anchor = Anchor::Selection {
            css_path: "body > section:nth-of-type(1) > p:nth-of-type(1)".into(),
            offset: 0,
            snippet: "Alpha beta gamma delta epsilon".into(),
        };
        match fuzzy_resolve_anchor(html, &anchor) {
            Resolution::Fuzzy(text, score) => {
                assert!(score >= 0.85, "score {score}");
                assert!(
                    text.contains("epsilonY"),
                    "css_path points at the 2nd block; got {text:?}"
                );
            }
            other => panic!("expected Fuzzy, got {other:?}"),
        }
    }

    /// Same document, but the css_path points at the FIRST block — the
    /// tiebreak must now select it instead. Proves the path term is what's
    /// deciding (not document order).
    #[test]
    fn resolve_selection_css_path_selects_first_duplicate() {
        let html = "<p>Alpha beta gamma delta epsilonX</p>\
                    <section><p>Alpha beta gamma delta epsilonY</p></section>";
        let anchor = Anchor::Selection {
            css_path: "body > p:nth-of-type(1)".into(),
            offset: 0,
            snippet: "Alpha beta gamma delta epsilon".into(),
        };
        match fuzzy_resolve_anchor(html, &anchor) {
            Resolution::Fuzzy(text, _) => assert!(
                text.contains("epsilonX"),
                "css_path points at the 1st block; got {text:?}"
            ),
            other => panic!("expected Fuzzy, got {other:?}"),
        }
    }

    /// An exact-text duplicate still resolves Exact (the path tiebreak must
    /// not demote an exact match to Fuzzy).
    #[test]
    fn resolve_selection_exact_duplicate_stays_exact() {
        let snippet = "The borrow checker runs at compile time.";
        let html = format!("<p>{snippet}</p><section><p>{snippet}</p></section>");
        let anchor = Anchor::Selection {
            css_path: "body > section:nth-of-type(1) > p:nth-of-type(1)".into(),
            offset: 0,
            snippet: snippet.into(),
        };
        assert_eq!(
            fuzzy_resolve_anchor(&html, &anchor),
            Resolution::Exact(snippet.to_string())
        );
    }

    #[test]
    fn parse_css_path_extracts_tag_and_nth() {
        let p = parse_css_path("body > main:nth-of-type(2) > p:nth-of-type(5)");
        assert_eq!(p, vec![("main".into(), 2), ("p".into(), 5)]);
        // Bare tag (no nth) → 0; `body`/`html` dropped.
        let p = parse_css_path("html > body > div");
        assert_eq!(p, vec![("div".into(), 0)]);
    }

    #[test]
    fn path_similarity_rewards_matching_nth_over_tag_only() {
        let stored = vec![("section".to_string(), 1), ("p".to_string(), 2)];
        // Identical → 1.0.
        assert!((path_similarity(&stored, &stored) - 1.0).abs() < 1e-6);
        // Same tags, different nth on the leaf → tag-only 0.5 on that seg.
        let other = vec![("section".to_string(), 1), ("p".to_string(), 9)];
        let s = path_similarity(&stored, &other);
        assert!(s > 0.5 && s < 1.0, "got {s}");
        // Leaf tag mismatch → trailing walk stops immediately → 0.0.
        assert_eq!(
            path_similarity(&[("p".to_string(), 1)], &[("div".to_string(), 1)]),
            0.0
        );
    }

    // --- v0.19 — atomic apply batch ------------------------------------

    #[test]
    fn apply_ops_applies_in_order_and_reports() {
        let mut f = fixture_file(); // has open comment c_1 (Section anchor)
        let ops = vec![
            BatchOp::AddReply {
                comment_id: "c_1".into(),
                author: Author::Claude,
                body: "fixed in §2".into(),
                choices: vec![],
            },
            BatchOp::SetAnchor {
                comment_id: "c_1".into(),
                anchor: Anchor::Section {
                    id: "overview".into(),
                    tag: Some("h2".into()),
                    snippet: None,
                },
            },
            BatchOp::Resolve {
                comment_id: "c_1".into(),
            },
        ];
        let rep = f.apply_ops(&ops, "abc123def456").unwrap();
        assert_eq!(rep.applied, 3);
        assert_eq!(rep.created_reply_ids.len(), 1);
        assert!(rep.mutated);
        assert_eq!(f.comments[0].replies.len(), 1);
        assert_eq!(f.comments[0].status, CommentStatus::Resolved);
        match &f.comments[0].anchor {
            Anchor::Section { id, .. } => assert_eq!(id, "overview"),
            other => panic!("expected re-pointed section anchor, got {other:?}"),
        }
    }

    #[test]
    fn apply_ops_is_all_or_nothing() {
        let mut f = fixture_file();
        let before_len = f.comments.len();
        // A valid AddComment followed by a reply to a missing comment: the
        // whole batch must roll back, so the AddComment never takes effect.
        let ops = vec![
            BatchOp::AddComment {
                anchor: Anchor::File,
                author: Author::Claude,
                body: "should not persist".into(),
                choices: vec![],
                file: None,
                file_label: None,
            },
            BatchOp::AddReply {
                comment_id: "c_does_not_exist".into(),
                author: Author::Claude,
                body: "x".into(),
                choices: vec![],
            },
        ];
        let err = f.apply_ops(&ops, "abc123def456").unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
        assert_eq!(
            f.comments.len(),
            before_len,
            "failed batch must leave the file unchanged (atomic)"
        );
    }

    #[test]
    fn apply_ops_add_comment_defaults_file_to_artifact_id() {
        let kb = fixture_kb();
        let mut f = ReviewFile::empty_skeleton(&kb, "the-artifact", "T");
        let ops = vec![BatchOp::AddComment {
            anchor: Anchor::File,
            author: Author::You,
            body: "hi".into(),
            choices: vec![],
            file: None,
            file_label: None,
        }];
        let rep = f.apply_ops(&ops, "the-artifact").unwrap();
        assert_eq!(rep.created_comment_ids.len(), 1);
        assert_eq!(f.comments[0].file, "the-artifact");
        assert_eq!(f.comments[0].file_label, "main");
    }

    #[test]
    fn apply_ops_noop_batch_reports_unmutated() {
        let mut f = fixture_file();
        f.set_comment_status("c_1", CommentStatus::Resolved)
            .unwrap();
        // Resolving an already-resolved comment changes nothing.
        let ops = vec![BatchOp::Resolve {
            comment_id: "c_1".into(),
        }];
        let rep = f.apply_ops(&ops, "abc123def456").unwrap();
        assert_eq!(rep.applied, 1);
        assert!(!rep.mutated, "no-op batch must report mutated=false (G8)");
    }

    #[test]
    fn batch_op_deserialises_tagged_wire_shape() {
        let json = r#"[
          {"op":"add_comment","anchor":{"kind":"file"},"author":"claude","body":"b"},
          {"op":"resolve","comment_id":"c_1"},
          {"op":"resolve_all"}
        ]"#;
        let ops: Vec<BatchOp> = serde_json::from_str(json).unwrap();
        assert_eq!(ops.len(), 3);
        assert!(matches!(ops[0], BatchOp::AddComment { .. }));
        assert!(matches!(ops[2], BatchOp::ResolveAll));
    }

    // --- v0.19 — portable embed / extract round-trip --------------------

    #[test]
    fn embed_extract_round_trips_and_is_idempotent() {
        let f = fixture_file();
        let html = "<!doctype html><html><head><title>x</title></head>\
                    <body><p>hi</p></body></html>";
        let embedded = embed_into_html(html, &f).unwrap();
        assert!(embedded.contains(EMBED_SCRIPT_ID));
        // Block lands inside <head>.
        let head_end = embedded.find("</head>").unwrap();
        assert!(embedded[..head_end].contains(EMBED_SCRIPT_ID));

        let back = extract_from_html(&embedded).unwrap().unwrap();
        assert_eq!(back.schema, SCHEMA);
        assert_eq!(back.comments.len(), 1);
        assert_eq!(back.comments[0].id, "c_1");
        assert_eq!(back.comments[0].status, CommentStatus::Open);

        // Re-embedding replaces (not appends) the block — exactly one stays.
        let again = embed_into_html(&embedded, &f).unwrap();
        assert_eq!(again.matches(EMBED_SCRIPT_ID).count(), 1);

        // A plain artifact has no block.
        assert!(extract_from_html(html).unwrap().is_none());
    }

    #[test]
    fn embed_escapes_close_script_in_a_comment_body() {
        let mut f = fixture_file();
        f.comments[0].body = "watch out for </script> injection".into();
        let html = "<html><head></head><body></body></html>";
        let embedded = embed_into_html(html, &f).unwrap();
        // Only the real closing tag remains; the body's literal is escaped.
        assert_eq!(embedded.matches("</script>").count(), 1);
        let back = extract_from_html(&embedded).unwrap().unwrap();
        assert_eq!(back.comments[0].body, "watch out for </script> injection");
    }

    #[test]
    fn extract_rejects_bad_schema() {
        let html = format!(
            "<html><head><script type=\"application/json\" id=\"{EMBED_SCRIPT_ID}\">\
             {{\"schema\":\"kb-comments/99\",\"artifact\":{{\"id\":\"x\",\"title\":\"y\",\"kb\":\"z\"}},\
             \"generatedAt\":\"2026-05-12T10:00:00Z\",\"comments\":[]}}</script></head><body></body></html>"
        );
        assert!(matches!(
            extract_from_html(&html),
            Err(Error::BadRequest(_))
        ));
    }

    // --- v0.5 P3 — review::export ---------------------------------------

    #[test]
    fn export_format_from_query_recognises_aliases() {
        assert_eq!(
            ExportFormat::from_query("claude"),
            Some(ExportFormat::Claude)
        );
        assert_eq!(
            ExportFormat::from_query("CLAUDE"),
            Some(ExportFormat::Claude)
        );
        assert_eq!(ExportFormat::from_query("json"), Some(ExportFormat::Json));
        assert_eq!(ExportFormat::from_query("md"), Some(ExportFormat::Markdown));
        assert_eq!(
            ExportFormat::from_query("markdown"),
            Some(ExportFormat::Markdown)
        );
        assert_eq!(ExportFormat::from_query("yaml"), None);
        assert_eq!(ExportFormat::from_query(""), None);
    }

    #[test]
    fn export_claude_format_includes_review_path_and_open_body() {
        let body = export(&fixture_file(), "smoke", ExportFormat::Claude).unwrap();
        assert!(body.contains("# Review: Borrow Checker"), "got: {body}");
        assert!(body.contains(".review/abc123def456.json"));
        assert!(body.contains("what about Pin"));
        // Claude prompt always ends with the resolve hint.
        assert!(body.contains("flip the comment's"));
    }

    #[test]
    fn export_json_format_round_trips_via_serde() {
        let body = export(&fixture_file(), "smoke", ExportFormat::Json).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["schema"].as_str(), Some(SCHEMA));
        assert_eq!(parsed["artifact"]["id"].as_str(), Some("abc123def456"));
    }

    #[test]
    fn export_markdown_format_includes_resolved_comments_too() {
        let mut file = fixture_file();
        // Mark the existing one resolved + add a 2nd open one.
        file.comments[0].status = CommentStatus::Resolved;
        file.comments.push(Comment {
            id: "c_2".into(),
            status: CommentStatus::Open,
            file: "abc123def456".into(),
            file_label: "main".into(),
            anchor: Anchor::File,
            author: Author::Claude,
            body: "second comment".into(),
            created_at: chrono::Utc::now(),
            edited_at: None,
            replies: vec![],
            choices: vec![],
            attachments: vec![],
            user: None,
        });
        let body = export(&file, "smoke", ExportFormat::Markdown).unwrap();
        // Resolved marker + open marker both appear.
        assert!(body.contains("● section:intro"), "got: {body}");
        assert!(body.contains("○ file"));
        assert!(body.contains("second comment"));
    }
}
