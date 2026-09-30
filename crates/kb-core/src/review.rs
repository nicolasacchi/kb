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
//!
//! Comments carry two orthogonal v0.40 axes: `tags` (comment-scoped
//! labels, normalised by [`normalize_comment_tags`]) and `private` (a
//! note no agent may see). The read rule is fail-closed and enforced
//! structurally: all three renderers behind [`export`] take NO visibility
//! parameter at all, so there is no flag a caller could pass;
//! [`ReviewFile::open_count`] counts public comments only; and
//! [`ReviewFile::visible`] / [`Visibility::includes`] are the only ways to
//! opt in, which only the two operator `?visibility=` reads do. The two
//! transports ([`embed_into_html`] / [`extract_from_html`]) and
//! `kb backup` carry everything verbatim on purpose.

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

/// serde's `skip_serializing_if` takes `&T -> bool`; a hand-rolled
/// `is_false` is clippy's `nonminimal_bool` alternative to
/// `std::ops::Not::not`. Used only by [`Comment::private`] (absent on
/// disk == public).
fn is_false(b: &bool) -> bool {
    !*b
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
    /// v0.40 TN1 — comment-scoped labels, slug-normalised + sorted + deduped
    /// at the write edge by [`normalize_comment_tags`] (the ONE normaliser
    /// both the `PATCH …/meta` route and `BatchOp::SetMeta` go through, so
    /// the two write paths cannot diverge). NEVER mirrored onto the
    /// artifact's own `kb-tags`: that namespace belongs to frontmatter tags
    /// and to the `status-approved` / `status-changes-requested` verdict
    /// display shortcut, and sharing it would make a comment tag re-key the
    /// artifact. Additive + skipped when empty, so a pre-TN sidecar
    /// round-trips BYTE-IDENTICALLY.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    // No `ts(optional)`: ts-rs permits it only on `Option<T>`, and the
    // `choices`/`attachments` precedent above already emits a skipped
    // `Vec` as a required TS field. Callers still read `c.tags ?? []` and
    // `c.private === true`, which stays correct against an older daemon
    // whose sidecars predate both keys.
    pub tags: Vec<String>,
    /// v0.40 TN2 — a private note: a comment no agent may ever see. The
    /// read side is fail-closed: every kb-core renderer filters on
    /// [`ReviewFile::visible`] / [`Comment::is_private`] with no opt-in
    /// parameter, and [`ReviewFile::open_count`] counts public comments
    /// only, so a missed filter undercounts (cosmetic) rather than leaking.
    /// The two LOSSLESS transports ([`embed_into_html`] and `kb backup`,
    /// which copies `.review/` verbatim) deliberately carry it — a move or a
    /// restore must not destroy operator data. Additive + skipped when
    /// false, so absence is indistinguishable from `false` on disk.
    #[serde(default, skip_serializing_if = "is_false")]
    // No `ts(optional)` — see `tags` above.
    pub private: bool,
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

    /// `true` when this comment is a private note — i.e. one no agent may
    /// see. Visibility is a SEPARATE axis from [`Comment::is_open`]: a
    /// note can be open or resolved, and a private resolved comment is still
    /// private. Every renderer pairs the two predicates rather than folding
    /// them together, so "public and open" stays expressible.
    pub fn is_private(&self) -> bool {
        self.private
    }
}

/// How much of a review file a read may see. `Public` is the fail-closed
/// default every agent-facing surface uses; `All` is the operator opt-in
/// carried by the `?visibility=` query param on the two review read routes.
///
/// Deliberately NOT ts-exported and never serialised: it is a request-side
/// selector, not a wire value. The only wire form is the query string
/// [`Visibility::from_query`] parses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    /// Only non-private comments. The default — an absent `?visibility=`
    /// means this, never "everything".
    #[default]
    Public,
    /// Every comment, private notes included. Reached only through an
    /// explicit operator read.
    All,
}

impl Visibility {
    /// Parse the `?visibility=` query value. Unknown values return `None`
    /// so the route answers 400 rather than guessing (mirrors
    /// [`ExportFormat::from_query`]).
    pub fn from_query(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "public" => Some(Visibility::Public),
            "all" => Some(Visibility::All),
            _ => None,
        }
    }

    /// Does this reader get to see `c`?
    pub fn includes(self, c: &Comment) -> bool {
        matches!(self, Visibility::All) || !c.is_private()
    }
}

/// v0.40 TN1 — max comment tags on one comment. A chip row plus a facet
/// list, not a metadata field.
pub const MAX_COMMENT_TAGS: usize = 8;

/// v0.40 TN1 — max length of ONE comment tag, measured on the SLUG (so it
/// bounds what actually lands on disk, not the user's pre-slug phrasing).
pub const MAX_COMMENT_TAG_LEN: usize = 48;

/// v0.40 TN1 — WORK bound on the RAW array, NOT a product limit. Deliberately
/// a different number from [`MAX_COMMENT_TAGS`] so the two can never be read
/// as one.
///
/// The tag cap alone bounds the OUTPUT, not the WORK: it is checked only
/// after the normalise loop, so it says nothing about how long that loop
/// runs. Both write edges (`PATCH …/meta`, `POST …/comments`) take a `Json`
/// body under axum's default 2 MB limit, and a 2 MB tags array is ~250k
/// entries — minutes of CPU on a tokio worker, from a request any local
/// caller can send unauthenticated over loopback. Rejecting over this bound
/// BEFORE the loop is what makes that 400 cheap; the `HashSet` dedupe is the
/// other half of the same fix, so the surviving per-entry work is a hash
/// insert instead of a scan of everything seen so far.
///
/// The head-room over the real limit (4x) is for what a human actually types
/// into a chip row — pre-slug phrasing, case variants, the same tag twice —
/// all of which normalises down and stays well inside [`MAX_COMMENT_TAGS`].
/// Anything that survives normalisation is still held to the real limit;
/// this constant only says how much input may be OFFERED for normalisation.
pub const MAX_RAW_COMMENT_TAGS: usize = 4 * MAX_COMMENT_TAGS;

/// The ONE normaliser for comment tags, shared by the `PATCH …/meta` route
/// and `BatchOp::SetMeta` so the two write paths cannot diverge.
///
/// Slugifies through [`crate::parser::slugify_tag`] (the only slugifier in
/// the repo), drops slugs that come out empty — the same rule `PATCH
/// …/meta` applies to artifact tags — dedupes AFTER slugifying (so
/// `"Fleet Doc"` and `"fleet-doc"` are one tag, not two facet hits) and
/// sorts, so the same tag SET always serialises to the same bytes and a
/// no-op re-tag cannot churn the file's ETag.
///
/// Over-limit is an error, never a silent truncation: dropping a tag the
/// operator asked for would make the stored document disagree with the
/// response that claims to be its "effective values".
pub fn normalize_comment_tags(raw: &[String]) -> Result<Vec<String>> {
    if raw.len() > MAX_RAW_COMMENT_TAGS {
        return Err(Error::BadRequest(format!(
            "{} raw comment tag entries; at most {MAX_RAW_COMMENT_TAGS} reach the \
             normaliser (a work bound — the {MAX_COMMENT_TAGS}-tag limit applies \
             to the normalised set)",
            raw.len()
        )));
    }
    // `seen` is a `HashSet`, not a `Vec::contains` scan: this dedupe runs over
    // caller-supplied input, so the linear scan made the whole function
    // quadratic in the body size. `MAX_RAW_COMMENT_TAGS` is what keeps the
    // loop short; the hash insert is what keeps each pass O(1).
    let mut out: Vec<String> = Vec::with_capacity(raw.len());
    let mut seen: std::collections::HashSet<String> =
        std::collections::HashSet::with_capacity(raw.len());
    for t in raw {
        let s = crate::parser::slugify_tag(t);
        if s.is_empty() {
            continue;
        }
        if s.len() > MAX_COMMENT_TAG_LEN {
            return Err(Error::BadRequest(format!(
                "comment tag {s:?} is {} chars; max {MAX_COMMENT_TAG_LEN}",
                s.len()
            )));
        }
        if seen.insert(s.clone()) {
            out.push(s);
        }
    }
    if out.len() > MAX_COMMENT_TAGS {
        return Err(Error::BadRequest(format!(
            "{} comment tags; max {MAX_COMMENT_TAGS}",
            out.len()
        )));
    }
    out.sort();
    Ok(out)
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

    /// The comments a reader of `v` may see, in document order. THE single
    /// selection point: every list/render surface in the workspace selects
    /// through this or through [`Comment::is_private`] — never by hand-
    /// rolling `!c.private` beside a status check, which is how the two
    /// axes silently drift apart.
    pub fn visible(&self, v: Visibility) -> Vec<&Comment> {
        self.comments.iter().filter(|c| v.includes(c)).collect()
    }

    /// Open comments an agent may see. The ONE iterator behind
    /// [`ReviewFile::open_count`], [`ReviewFile::visible_open_count`] and
    /// the Claude-prompt builder, so "public and open" cannot be spelled
    /// three slightly different ways.
    fn open_visible_comments(&self) -> impl Iterator<Item = &Comment> {
        self.comments
            .iter()
            .filter(|c| c.is_open() && !c.is_private())
    }

    /// How many OPEN comments a reader may see. `Visibility::Public` is
    /// what every count an agent can observe uses — including the
    /// `comments.updated` SSE payload — so a private note does not even
    /// register as a change in the fleet's open work.
    pub fn visible_open_count(&self) -> usize {
        self.open_visible_comments().count()
    }

    /// How many open comments this file has, for any reader. Signature
    /// unchanged since v0.2; the MEANING is public-only as of v0.40 TN2
    /// (changed in place, deliberately with no `open_count_public()`
    /// sibling — fail-closed: a call site nobody updates undercounts,
    /// which is a cosmetic lie, instead of leaking the existence of a
    /// note). The operator's exact figure comes from the review file
    /// itself, not from a count.
    pub fn open_count(&self) -> usize {
        self.visible_open_count()
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
            tags: spec.tags,
            private: spec.private,
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
        self.reject_private_note(comment_id)?;
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

    /// v0.40 TN2 — the ONE fail-closed guard every single-comment mutation
    /// runs before it touches a row: `set_comment_status`,
    /// `set_comment_anchor` and `add_reply` all refuse a PRIVATE note.
    ///
    /// Why it has to live HERE and not in the HTTP routes. `set_all_status`
    /// skips notes (and documents why), but a single-comment route reaches
    /// the same rows without passing through it — and a note id is not a
    /// secret: `/api/anchors/stale` answers `{kb, artifact_id, comment_id}`
    /// fleet-wide and the indexer walks every open comment with no private
    /// filter, so any caller can name a note id without ever having seen the
    /// note. Resolving it returned 200 and the operator's private reminder
    /// silently left their open-note list.
    ///
    /// Same rationale as `set_all_status`, restated for the single path:
    /// the side effect is the leak. A local agent could quietly resolve,
    /// re-anchor or reply onto a row it must never see, and — because
    /// `set_all_status` deliberately leaves `flipped`/`open_count` in
    /// agreement with the PUBLIC set — the response would still look like a
    /// perfectly ordinary no-op. Note ids are enumerable, so "the agent
    /// can't read the note" was never a barrier here.
    ///
    /// Deliberately NOT owner-gated (unlike `set_comment_meta`, which is):
    /// on loopback with no credentials every request resolves to the
    /// operator identity, so an owner check would be waved through by the
    /// exact local agent this is meant to stop. The escape hatch is the
    /// same one `keep` offers — un-private the note first (`PATCH …/meta`
    /// with `private: false`), mutate it, re-privatise — which is a
    /// deliberate act on a row the operator can already see.
    ///
    /// `Error::Conflict` → 409, the status the other private-note refusal
    /// in this feature (`keep`) already uses; no new status is invented.
    /// A missing id still falls through to `comment_mut`'s canonical
    /// `NotFound` (404) rather than being reported as "it's a note".
    fn reject_private_note(&self, comment_id: &str) -> Result<()> {
        if self
            .comments
            .iter()
            .any(|c| c.id == comment_id && c.is_private())
        {
            return Err(Error::Conflict(format!(
                "comment {comment_id} is a private note; un-private it before changing it"
            )));
        }
        Ok(())
    }

    /// Set one comment's status (resolve/unresolve). `Err(NotFound)` when
    /// the comment is absent. Returns `true` iff the status actually changed
    /// (G8 — lets the route skip a no-op save + `comments.updated` emit when
    /// resolving an already-resolved comment).
    pub fn set_comment_status(&mut self, comment_id: &str, status: CommentStatus) -> Result<bool> {
        self.reject_private_note(comment_id)?;
        let c = self.comment_mut(comment_id)?;
        let changed = c.status != status;
        c.status = status;
        Ok(changed)
    }

    /// Flip every comment NOT already at `status` to `status`; returns the
    /// count flipped. Backs `resolve-all` / `unresolve-all` (one mutation,
    /// one save, one SSE).
    ///
    /// v0.40 TN2 — PRIVATE notes are skipped in the flip AND in the count,
    /// and there is deliberately no parameter to widen that: the rule is
    /// fail-closed, so no caller can opt in from the outside. Two reasons,
    /// and the second is the one that bites:
    ///   1. The count. `flipped` is answered to the agent in the same JSON
    ///      object as the public-only `open_count`, so counting a note
    ///      yields `flipped: 1, open_count: 0` — an existence disclosure
    ///      needing no arithmetic at all. Skipping keeps the two numbers in
    ///      agreement by construction rather than by a second filter.
    ///   2. The side effect. Flipping a note the agent cannot see was
    ///      harmless only while `flipped` and the visible set were the same
    ///      set; with notes they differ, and a local agent could quietly
    ///      resolve the operator's private reminders.
    ///
    /// A file whose only open comments are notes therefore reports
    /// `flipped: 0`, which both callers turn into a no-op — no rewrite, no
    /// `comments.updated` emit, and `BatchOp::ResolveAll` counts the op as
    /// unmutated. Flipping notes deliberately stays a DIFFERENT operation:
    /// if it is ever wanted, it gets its own explicit method (the same
    /// public/all pair `visible` uses), never a flag on this one.
    pub fn set_all_status(&mut self, status: CommentStatus) -> usize {
        let mut flipped = 0;
        for c in &mut self.comments {
            if c.is_private() {
                continue;
            }
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
        self.reject_private_note(comment_id)?;
        let c = self.comment_mut(comment_id)?;
        let changed = c.anchor != anchor;
        c.anchor = anchor;
        Ok(changed)
    }

    /// v0.40 TN1/TN2 — set a comment's tags and/or private flag.
    ///
    /// `None` on a field means "don't touch it", which is why the request
    /// shape carries `Option<bool>` while the stored field is a plain
    /// `bool`: absent must be distinguishable from an explicit
    /// un-privating, and the three states (don't touch / public / private)
    /// are not a two-valued enum.
    ///
    /// `tags` is normalised through [`normalize_comment_tags`] HERE
    /// rather than at the edges, so the `PATCH …/meta` route and
    /// `BatchOp::SetMeta` cannot store two different shapes for the same
    /// input. Over-limit / neither-field-set are `Err(BadRequest)` (400);
    /// a missing id is `Err(NotFound)` (404).
    ///
    /// Returns `Ok(false)` when neither field actually moved (G8 — the
    /// route then skips both the `save_atomic` and the `comments.updated`
    /// emit, so re-clicking a tag doesn't rewrite the file or re-notify
    /// every open tab). Does NOT stamp `edited_at`, for the same reason
    /// [`ReviewFile::set_comment_anchor`] doesn't: that badge means "the
    /// human changed what I wrote", and tagging a comment is not that.
    pub fn set_comment_meta(
        &mut self,
        comment_id: &str,
        tags: Option<&[String]>,
        private: Option<bool>,
    ) -> Result<bool> {
        if tags.is_none() && private.is_none() {
            return Err(Error::BadRequest(
                "comment meta patch must set at least one of: tags, private".into(),
            ));
        }
        let normalized = tags.map(normalize_comment_tags).transpose()?;
        let c = self.comment_mut(comment_id)?;
        let mut changed = false;
        if let Some(next) = normalized {
            if c.tags != next {
                c.tags = next;
                changed = true;
            }
        }
        if let Some(next) = private {
            if c.private != next {
                c.private = next;
                changed = true;
            }
        }
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
    ///
    /// v0.40 TN2 — deliberately UNFILTERED. A private note's attachment
    /// is still referenced; dropping it here would let the GC reap the
    /// operator's blob. GC is a storage decision, not a read surface.
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
        #[serde(default)]
        tags: Vec<String>,
        #[serde(default)]
        private: bool,
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
    /// v0.40 TN1/TN2 — set a comment's tags and/or private flag
    /// (`{"op":"set_meta","comment_id":"c_…","tags":["a"],"private":true}`).
    /// Present so the field is reachable from the batch path too: a
    /// capability that only the HTTP route can reach is dead everywhere
    /// else. Tags are normalised by
    /// [`ReviewFile::set_comment_meta`], the same call the route makes.
    SetMeta {
        comment_id: String,
        #[serde(default)]
        tags: Option<Vec<String>>,
        #[serde(default)]
        private: Option<bool>,
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
                tags,
                private,
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
                    // Same normaliser the `add_comment` route runs, so a
                    // tag set written through the batch path is
                    // byte-identical to one written through HTTP.
                    tags: normalize_comment_tags(tags)?,
                    private: *private,
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
            BatchOp::SetMeta {
                comment_id,
                tags,
                private,
            } => {
                report.mutated |= f.set_comment_meta(comment_id, tags.as_deref(), *private)?;
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
    /// v0.40 TN1 — comment tags, already normalised by
    /// [`normalize_comment_tags`] at the write edge. `Vec::new()` means
    /// "no tags" (the key is then skipped on disk). No `Default`: like
    /// every other field here, an omitted spec field is a bug, and this
    /// one is deliberately a compile-time tripwire so a caller can't
    /// forget to think about tags on a new comment.
    pub tags: Vec<String>,
    /// v0.40 TN2 — create this comment as a private note. `false` is the
    /// ordinary public comment.
    pub private: bool,
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
///
/// Every arm is public-only, with NO opt-in parameter — the absence of
/// the parameter IS the guarantee, so there is no flag a future caller
/// can pass to an agent-reachable renderer. (`export --embed` is the
/// documented lossless path, and `kb backup` copies `.review/` verbatim,
/// so no operator data is lost by this filter.)
pub fn export(file: &ReviewFile, kb: &str, format: ExportFormat) -> Result<String> {
    Ok(match format {
        ExportFormat::Claude => build_claude_prompt(file, kb),
        ExportFormat::Markdown => build_markdown_summary(file, kb),
        // v0.40 TN2 — `export --format json` is agent-reachable
        // (`kb comments export --format json`), so it is public-only per
        // the visibility rule. One clone, this arm only.
        ExportFormat::Json => {
            let mut public = file.clone();
            public.comments.retain(|c| !c.is_private());
            serde_json::to_string_pretty(&public)?
        }
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
    // v0.40 TN2 — `open_visible_comments` is the same iterator
    // `open_count` uses, so the prompt can never show a private note and
    // the count can never disagree with it.
    let opens: Vec<&Comment> = file.open_visible_comments().collect();
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
/// full conversation history. v0.40 TN2 — public-only, like the Claude
/// arm: the markdown summary is what `kb comments export` hands an agent
/// and what a human reads in the same place, so a note is simply not
/// rendered rather than rendered with a marker.
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
    // The early-out tests the VISIBLE set: a file whose only comment is a
    // private note must read "(no comments)", not print a heading and
    // then nothing under it.
    let visible = file.visible(Visibility::Public);
    if visible.is_empty() {
        out.push_str("(no comments)\n");
        return out;
    }
    for c in visible {
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
//
// v0.40 TN2 — DELIBERATELY UNFILTERED. This pair is a LOSSLESS
// MOVE/RESTORE TRANSPORT, not a render: the embedded block is the whole
// sidecar envelope, and `POST …/import` restores from it. A
// `private`-filtering "consistency sweep" here would silently destroy
// the operator's notes on every export/import cycle — a far worse failure
// than the leak it prevents, and one that needs the operator to hand an
// HTML bundle to an agent to trigger. `kb backup` is the other verbatim
// path (it copies `.review/` as-is) and the anchor-stale sidecar is a
// third (ids and a score only, never a body). Renderers filter; transports
// do not. Do not add a filter here.

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
/// v0.40 TN2 — carries private notes verbatim; see the module note above.
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
/// v0.40 TN2 — restores private notes verbatim; see the module note above.
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
            tags: vec![],
            private: false,
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

    // --- v0.40 TN1/TN2 — tags + private notes ---------------------------

    /// The one test that guards the additive-on-disk contract. A legacy
    /// sidecar — written before `tags`/`private` existed, hand-checked
    /// against the shape `save_atomic` produces — must come back out
    /// BYTE-IDENTICAL after a load + save with no mutation. A missing
    /// `skip_serializing_if` injects `"tags": []` / `"private": false`
    /// into every comment of every review file in the fleet, which is an
    /// ETag/byte-diff storm rather than a subtle bug.
    #[test]
    fn legacy_sidecar_round_trips_byte_identically() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("legacy.json");
        let legacy = r#"{
  "schema": "kb-comments/1",
  "artifact": {
    "id": "abc123def456",
    "title": "Borrow Checker",
    "kb": "smoke",
    "tags": [],
    "pages": []
  },
  "generatedAt": "2026-05-12T10:00:00Z",
  "comments": [
    {
      "id": "c_1",
      "status": "open",
      "file": "abc123def456",
      "fileLabel": "main",
      "anchor": {
        "kind": "section",
        "id": "intro",
        "tag": "h2",
        "snippet": "Borrow Checker is a static analysis…"
      },
      "author": "you",
      "body": "what about Pin<&mut Self>?",
      "createdAt": "2026-05-12T10:05:00Z",
      "editedAt": null,
      "replies": [],
      "choices": [],
      "attachments": []
    },
    {
      "id": "c_2",
      "status": "resolved",
      "file": "abc123def456",
      "fileLabel": "main",
      "anchor": {
        "kind": "file"
      },
      "author": "claude",
      "body": "answered already",
      "createdAt": "2026-05-12T11:00:00Z",
      "editedAt": null,
      "replies": [],
      "choices": [],
      "attachments": [],
      "user": "nik"
    }
  ]
}"#;
        // No trailing newline: `save_atomic` writes `to_vec_pretty`, which
        // emits none, so a legacy literal that ends in one would fail this
        // assertion on a byte that says nothing about key injection.
        std::fs::write(&path, legacy).unwrap();
        let before = std::fs::read(&path).unwrap();

        let file = load(&path).unwrap().expect("legacy sidecar must load");
        assert_eq!(file.comments.len(), 2);
        assert!(file.comments[0].tags.is_empty());
        assert!(!file.comments[0].private);
        save_atomic(&path, &file, None).unwrap();

        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "an unmutated legacy sidecar must re-save byte-identically"
        );
    }

    #[test]
    fn tags_and_private_round_trip_and_stay_absent_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("review.json");
        let mut file = fixture_file();
        file.comments[0].tags = vec!["fleet-doc".into(), "wording".into()];
        file.comments[0].private = true;
        save_atomic(&path, &file, None).unwrap();

        let loaded = load(&path).unwrap().unwrap();
        assert_eq!(loaded.comments[0].tags, ["fleet-doc", "wording"]);
        assert!(loaded.comments[0].is_private());
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"tags\""));
        assert!(raw.contains("\"private\": true"));

        // The other direction: a comment with no tags and no private flag
        // must serialise with NEITHER key present — absence is the
        // representation, and the SPA reads `c.tags ?? []`.
        //
        // Scoped to the COMMENT object on purpose. A whole-document
        // `contains("\"tags\"")` would trip on `artifact.tags`, which is a
        // different field with a different owner and legitimately present.
        let plain = export(&fixture_file(), "smoke", ExportFormat::Json).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&plain).unwrap();
        let comment = &doc["comments"][0];
        assert!(comment.get("tags").is_none(), "got: {comment}");
        assert!(comment.get("private").is_none(), "got: {comment}");
    }

    #[test]
    fn normalize_comment_tags_slugifies_dedupes_and_sorts() {
        let raw: Vec<String> = ["Fleet Doc", "fleet-doc", "  ", "TODO: rewrite"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            normalize_comment_tags(&raw).unwrap(),
            vec!["fleet-doc".to_string(), "todo-rewrite".to_string()]
        );
        // Slugs that come out empty are dropped silently (the artifact-tag
        // rule), and an all-empty input CLEARS the tags rather than erroring.
        assert_eq!(
            normalize_comment_tags(&["!!".to_string(), "real".to_string()]).unwrap(),
            vec!["real".to_string()]
        );
        assert!(normalize_comment_tags(&["!!".to_string()])
            .unwrap()
            .is_empty());
        // Order-independent: the same set always produces the same bytes.
        let a = normalize_comment_tags(&["b".to_string(), "a".to_string()]).unwrap();
        let b = normalize_comment_tags(&["a".to_string(), "b".to_string()]).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn normalize_comment_tags_rejects_over_limit() {
        let nine: Vec<String> = (0..9).map(|i| format!("t{i}")).collect();
        let err = normalize_comment_tags(&nine).unwrap_err();
        assert!(
            matches!(err, Error::BadRequest(_)),
            "9 tags must be rejected, never silently truncated, got {err:?}"
        );
        // Exactly at the limit is fine.
        assert!(normalize_comment_tags(&nine[..8]).is_ok());
        let long = "x".repeat(MAX_COMMENT_TAG_LEN + 1);
        assert!(matches!(
            normalize_comment_tags(&[long]).unwrap_err(),
            Error::BadRequest(_)
        ));
        assert!(normalize_comment_tags(&["x".repeat(MAX_COMMENT_TAG_LEN)]).is_ok());

        // v0.40 TN1 — the RAW work bound, which is NOT the tag limit. The tag
        // cap is checked after the loop, so on its own it bounds the OUTPUT
        // and not the WORK: both write edges take a `Json` body under axum's
        // 2 MB default limit, and a dedupe scan over that is minutes of CPU
        // on a worker, from an unauthenticated loopback POST. This bound is
        // what makes the 400 cheap, and it counts RAW entries — so the input
        // that must still be ACCEPTED is one full past the raw bound that
        // collapses to a single tag.
        let collapsed: Vec<String> = (0..MAX_RAW_COMMENT_TAGS)
            .map(|_| "fleet doc".to_string())
            .collect();
        assert_eq!(
            normalize_comment_tags(&collapsed).unwrap(),
            vec!["fleet-doc".to_string()],
            "{MAX_RAW_COMMENT_TAGS} raw entries that normalise to one tag is still one tag"
        );

        // One entry over the raw bound is rejected — and rejected by the
        // BOUND rather than by normalisation: the first entry is an
        // over-long slug, so a bound checked after the loop would report the
        // per-tag length error instead. The message is the only observable
        // difference between the two, which is why it is asserted.
        let mut over: Vec<String> = vec!["x".repeat(MAX_COMMENT_TAG_LEN + 1)];
        over.extend((0..MAX_RAW_COMMENT_TAGS).map(|i| format!("t{i}")));
        let err = normalize_comment_tags(&over).unwrap_err();
        assert!(
            matches!(&err, Error::BadRequest(m) if m.contains("raw comment tag entries")),
            "an over-bound body must be refused by the work bound, got {err:?}"
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
    fn open_count_excludes_resolved_and_private() {
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
            tags: vec![],
            private: false,
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
            tags: vec![],
            private: false,
        });
        assert_eq!(f.open_count(), 1);

        // v0.40 TN2 — an OPEN private note must not move the count the
        // agent reads (and the `comments.updated` payload carries): the
        // count would otherwise prove the note exists.
        f.comments.push(Comment {
            id: "c_note".into(),
            status: CommentStatus::Open,
            file: "id1".into(),
            file_label: "main".into(),
            anchor: Anchor::File,
            author: Author::You,
            body: "secret".into(),
            created_at: Utc::now(),
            edited_at: None,
            replies: vec![],
            choices: vec![],
            attachments: vec![],
            user: None,
            tags: vec!["wording".into()],
            private: true,
        });
        assert_eq!(
            f.open_count(),
            1,
            "a private note must not change the agent-visible open count"
        );
        assert_eq!(f.visible_open_count(), 1, "Public is the counted view");
        assert_eq!(
            f.visible(Visibility::All).len(),
            3,
            "the operator's opt-in read still sees everything"
        );
        assert_eq!(f.visible(Visibility::Public).len(), 2);
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
            tags: vec![],
            private: false,
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
        // v0.40 TN2 — a fourth comment, open and PRIVATE.
        let mut note = spec("a private note");
        note.private = true;
        let c_note = f.add_comment(note).id.clone();
        // 2 open + 1 resolved + 1 open note; resolve-all flips the 2 open
        // PUBLIC ones only. Both halves of that matter, and the count is the
        // sharper one: the route answers `flipped` in the same JSON object
        // as the public-only `open_count`, so a counted note is a bare
        // existence disclosure (`flipped: 1, open_count: 0`) on a route any
        // agent can call. The flip itself would resolve a note the agent is
        // not allowed to know exists.
        assert_eq!(f.set_all_status(CommentStatus::Resolved), 2);
        assert_eq!(f.open_count(), 0);
        assert_eq!(
            f.comments.iter().find(|c| c.id == c_note).unwrap().status,
            CommentStatus::Open,
            "resolve-all must leave a private note's status alone"
        );
        // Already all resolved (the note is never "all") → 0 flipped.
        assert_eq!(f.set_all_status(CommentStatus::Resolved), 0);
        // unresolve-all flips the 3 public ones back — 3, not 4: the note is
        // already Open, and it is not in the public open set either.
        assert_eq!(f.set_all_status(CommentStatus::Open), 3);
        assert_eq!(f.open_count(), 3);
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

    // --- v0.40 TN2: single-comment mutations refuse a private note -------

    /// The batch path (`set_all_status`) already skipped notes; these are
    /// the single-comment twins, and they are reachable with nothing but an
    /// id from `/api/anchors/stale`. A resolve that silently succeeded
    /// removed the operator's reminder from their open-note list — the same
    /// side effect `set_all_status` documents refusing to cause.
    #[test]
    fn single_comment_mutations_refuse_a_private_note() {
        let mut f = fixture_file();
        let pub_cid = f.add_comment(spec("public row")).id.clone();
        let note_cid = {
            let mut s = spec("operator's private reminder");
            s.private = true;
            let id = f.add_comment(s).id.clone();
            // Sanity: the note really is private and OPEN, so a refusal
            // below cannot be explained by it being already resolved.
            assert!(f.comments.iter().any(|c| c.id == id && c.is_private()));
            id
        };

        assert!(matches!(
            f.set_comment_status(&note_cid, CommentStatus::Resolved),
            Err(Error::Conflict(_))
        ));
        // Re-pointing at a DIFFERENT anchor is what a refusal has to stop;
        // `spec()` rows are created on `Anchor::File`.
        let reanchor = Anchor::Section {
            id: "overview".into(),
            tag: Some("h2".into()),
            snippet: None,
        };
        assert!(matches!(
            f.set_comment_anchor(&note_cid, reanchor),
            Err(Error::Conflict(_))
        ));
        assert!(matches!(
            f.add_reply(&note_cid, Author::Claude, "on it".into(), vec![], None),
            Err(Error::Conflict(_))
        ));

        // Nothing moved, on ANY of the three paths.
        let note = f.comments.iter().find(|c| c.id == note_cid).unwrap();
        assert_eq!(note.status, CommentStatus::Open);
        assert!(note.replies.is_empty());
        assert!(matches!(note.anchor, Anchor::File));

        // The public sibling is untouched by the guard — a note must not
        // cost the ordinary comment its resolve.
        assert!(f
            .set_comment_status(&pub_cid, CommentStatus::Resolved)
            .unwrap());
    }

    /// The refusal must be a 409 on a note that EXISTS, and must not
    /// become a disclosure for one that doesn't: a missing id keeps the
    /// canonical 404 from `comment_mut`.
    #[test]
    fn private_note_refusal_does_not_disclose_a_missing_comment() {
        let mut f = fixture_file();
        assert!(matches!(
            f.set_comment_status("c_x", CommentStatus::Resolved),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            f.set_comment_anchor("c_x", Anchor::File),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            f.add_reply("c_x", Author::Claude, "x".into(), vec![], None),
            Err(Error::NotFound(_))
        ));
    }

    /// The escape hatch `keep` already documents: `set_comment_meta` is
    /// deliberately NOT guarded, so un-privating a note (and re-privatising
    /// it afterwards) still works. Without this the note would be frozen.
    #[test]
    fn un_privating_re_opens_the_guarded_mutations() {
        let mut f = fixture_file();
        f.comments[0].private = true;
        assert!(matches!(
            f.set_comment_status("c_1", CommentStatus::Resolved),
            Err(Error::Conflict(_))
        ));
        assert!(f.set_comment_meta("c_1", None, Some(false)).unwrap());
        assert!(f
            .set_comment_status("c_1", CommentStatus::Resolved)
            .unwrap());
    }

    #[test]
    fn set_comment_meta_is_a_noop_when_nothing_changes() {
        let mut f = fixture_file();
        assert!(f.comments[0].edited_at.is_none());

        // A patch that sets NEITHER field is a caller bug, not a no-op —
        // answering 200 here would silently swallow a malformed request.
        assert!(matches!(
            f.set_comment_meta("c_1", None, None),
            Err(Error::BadRequest(_))
        ));

        let raw = vec!["Fleet Doc".to_string(), "a".to_string()];
        assert!(f.set_comment_meta("c_1", Some(&raw), Some(true)).unwrap());
        assert_eq!(f.comments[0].tags, ["a", "fleet-doc"]);
        assert!(f.comments[0].private);
        // Tagging is not a content edit — the badge stays off (same rule
        // as set_comment_anchor).
        assert!(f.comments[0].edited_at.is_none());

        // G8: the same effective values (here in different spelling) move
        // nothing, so the route can skip both the save and the SSE.
        let same = vec!["FLEET DOC".to_string(), "A".to_string()];
        assert!(!f.set_comment_meta("c_1", Some(&same), Some(true)).unwrap());
        // …and neither does a patch that only repeats the private flag.
        assert!(!f.set_comment_meta("c_1", None, Some(true)).unwrap());
        // …but clearing the tags is a real change.
        assert!(f.set_comment_meta("c_1", Some(&[]), None).unwrap());
        assert!(f.comments[0].tags.is_empty());
        assert!(f.comments[0].private, "an absent field must not be touched");

        assert!(matches!(
            f.set_comment_meta("c_nope", None, Some(false)),
            Err(Error::NotFound(_))
        ));
        // Over-limit input is rejected BEFORE the comment is touched.
        let nine: Vec<String> = (0..9).map(|i| format!("t{i}")).collect();
        assert!(matches!(
            f.set_comment_meta("c_1", Some(&nine), None),
            Err(Error::BadRequest(_))
        ));
        assert!(f.comments[0].tags.is_empty());
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
                tags: vec![],
                private: false,
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
            tags: vec![],
            private: false,
        }];
        let rep = f.apply_ops(&ops, "the-artifact").unwrap();
        assert_eq!(rep.created_comment_ids.len(), 1);
        assert_eq!(f.comments[0].file, "the-artifact");
        assert_eq!(f.comments[0].file_label, "main");
    }

    // --- v0.40 TN1/TN2 — meta through the batch path --------------------

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

        // Same rule for a meta patch that lands on the values already
        // stored — otherwise every tag click rewrites the file and
        // re-notifies every open tab.
        f.set_comment_meta("c_1", Some(&["wording".to_string()]), Some(true))
            .unwrap();
        let rep = f
            .apply_ops(
                &[BatchOp::SetMeta {
                    comment_id: "c_1".into(),
                    tags: Some(vec!["WORDING".to_string()]),
                    private: Some(true),
                }],
                "abc123def456",
            )
            .unwrap();
        assert!(
            !rep.mutated,
            "no-op set_meta must report mutated=false (G8)"
        );
        assert_eq!(f.comments[0].tags, ["wording"]);
    }

    #[test]
    fn batch_op_add_comment_carries_tags_and_private() {
        let mut f = fixture_file();
        let ops = vec![BatchOp::AddComment {
            anchor: Anchor::File,
            author: Author::You,
            body: "a note".into(),
            choices: vec![],
            file: None,
            file_label: None,
            tags: vec!["Wording".into(), "wording".into()],
            private: true,
        }];
        f.apply_ops(&ops, "abc123def456").unwrap();
        let added = f.comments.last().unwrap();
        assert!(added.is_private());
        // Normalised by the SAME helper the route calls, so the batch and
        // HTTP write paths cannot store two shapes for one input.
        assert_eq!(added.tags, ["wording"]);
        // …and the note stays out of the agent-facing read paths.
        assert_eq!(f.open_count(), 1);
        assert_eq!(f.visible(Visibility::All).len(), 2);
    }

    #[test]
    fn batch_op_set_meta_deserialises_tagged_wire_shape() {
        let json = r#"[{"op":"set_meta","comment_id":"c_1","tags":["A","a"]}]"#;
        let ops: Vec<BatchOp> = serde_json::from_str(json).unwrap();
        let BatchOp::SetMeta {
            comment_id,
            tags,
            private,
        } = &ops[0]
        else {
            panic!("expected a set_meta op, got {:?}", ops[0]);
        };
        assert_eq!(comment_id, "c_1");
        assert!(private.is_none(), "an absent private key means don't touch");
        assert_eq!(tags.as_deref().unwrap().len(), 2, "raw input, pre-slugify");

        let mut f = fixture_file();
        assert!(f.apply_ops(&ops, "abc123def456").unwrap().mutated);
        assert_eq!(f.comments[0].tags, ["a"]);
    }

    #[test]
    fn batch_op_set_meta_rejects_over_limit_and_missing_comments() {
        let mut f = fixture_file();
        let nine: Vec<String> = (0..9).map(|i| format!("t{i}")).collect();
        let err = f
            .apply_ops(
                &[BatchOp::SetMeta {
                    comment_id: "c_1".into(),
                    tags: Some(nine),
                    private: None,
                }],
                "abc123def456",
            )
            .unwrap_err();
        assert!(matches!(err, Error::BadRequest(_)), "got {err:?}");
        assert!(matches!(
            f.apply_ops(
                &[BatchOp::SetMeta {
                    comment_id: "c_nope".into(),
                    tags: None,
                    private: Some(true),
                }],
                "abc123def456",
            )
            .unwrap_err(),
            Error::NotFound(_)
        ));
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

    /// v0.40 TN2 — the transport is LOSSLESS on purpose. Filtering a
    /// private note out of `#kb-review-state` would delete operator data
    /// on every export → import move, which is worse than any leak this
    /// filter guards; `kb backup` copies `.review/` verbatim for the same
    /// reason.
    #[test]
    fn embed_extract_round_trips_private_comments() {
        let mut f = fixture_file();
        let mut note = f.comments[0].clone();
        note.id = "c_note".into();
        note.body = "do not ship this wording".into();
        note.tags = vec!["wording".into()];
        note.private = true;
        f.comments.push(note);

        let html = "<html><head></head><body></body></html>";
        let embedded = embed_into_html(html, &f).unwrap();
        let back = extract_from_html(&embedded).unwrap().unwrap();
        assert_eq!(back.comments.len(), 2);
        assert!(back.comments[1].is_private());
        assert_eq!(back.comments[1].body, "do not ship this wording");
        assert_eq!(back.comments[1].tags, ["wording"]);
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
            tags: vec![],
            private: false,
        });
        let body = export(&file, "smoke", ExportFormat::Markdown).unwrap();
        // Resolved marker + open marker both appear.
        assert!(body.contains("● section:intro"), "got: {body}");
        assert!(body.contains("○ file"));
        assert!(body.contains("second comment"));
    }

    /// A review file with one public and one private OPEN comment — the
    /// exact shape every leak test below needs. The note is tagged too, so
    /// a renderer that echoed the whole comment would leak its tags as
    /// well as its body.
    fn file_with_one_public_and_one_note() -> ReviewFile {
        let mut f = fixture_file();
        let mut note = f.comments[0].clone();
        note.id = "c_note".into();
        note.body = "do not ship this wording".into();
        note.tags = vec!["wording".into()];
        note.private = true;
        f.comments.push(note);
        f
    }

    #[test]
    fn export_claude_format_omits_private_comments() {
        // The LLM's actual read path (`kb comments export --format claude`
        // and `POST …/export?format=claude`). No visibility parameter
        // exists on this function, so the filter is the only thing
        // standing between a note and the model.
        let body = export(
            &file_with_one_public_and_one_note(),
            "smoke",
            ExportFormat::Claude,
        )
        .unwrap();
        assert!(body.contains("what about Pin"), "got: {body}");
        assert!(
            !body.contains("do not ship this wording"),
            "private note leaked into the claude prompt: {body}"
        );
    }

    #[test]
    fn export_markdown_format_omits_private_comments() {
        let body = export(
            &file_with_one_public_and_one_note(),
            "smoke",
            ExportFormat::Markdown,
        )
        .unwrap();
        assert!(body.contains("what about Pin"), "got: {body}");
        assert!(!body.contains("do not ship this wording"), "got: {body}");
    }

    #[test]
    fn export_markdown_of_a_note_only_file_reads_as_empty() {
        // The early-out must test the VISIBLE set, not `comments.is_empty()`.
        let mut f = fixture_file();
        f.comments[0].private = true;
        let body = export(&f, "smoke", ExportFormat::Markdown).unwrap();
        assert!(body.contains("(no comments)"), "got: {body}");
        assert!(!body.contains("what about Pin"), "got: {body}");
    }

    #[test]
    fn export_json_format_omits_private_comments() {
        let body = export(
            &file_with_one_public_and_one_note(),
            "smoke",
            ExportFormat::Json,
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        let comments = parsed["comments"].as_array().unwrap();
        assert_eq!(comments.len(), 1, "got: {body}");
        assert_eq!(comments[0]["id"].as_str(), Some("c_1"));
    }
}
