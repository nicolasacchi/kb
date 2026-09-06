//! Reading Lists (RL-track, v0.18) — multiple named, ordered lists per kb
//! whose entries target a whole artifact OR a section of it.
//!
//! Replaces the v0.13 bookmarks feature. The shape:
//!
//! - A **list** is `(id, title, description?, pinned, archived)` — per-kb,
//!   sqlite-backed (`lists` table, migration V0015). Cross-kb listing
//!   happens at the HTTP layer via fan-out (the bookmarks/sessions
//!   precedent).
//! - An **entry** is an ordered pointer at `(artifact_id, anchor?)` plus a
//!   free-form note. `anchor` reuses [`crate::review::Anchor`] verbatim —
//!   the kb-comments anchoring machinery (fuzzy re-resolution, staleness)
//!   applies to list entries for free. `NULL` anchor = the whole artifact.
//! - **Read state is DERIVED**, not tracked: [`derive_read_state`] maps the
//!   artifact's [`crate::reading::ReadingSummary`] (RP-track, V0014) onto
//!   the entry's target — per-section dwell for section anchors, scroll
//!   completion for whole-artifact entries. A manual `read_override`
//!   ('read' | 'unread') wins absolutely when set.
//! - **Reading-time estimates** come from word counts at
//!   [`EST_WORDS_PER_MINUTE`]: the lance `word_count` column for
//!   whole-artifact entries, the stored per-section [`section_words`]
//!   estimate for anchored ones.
//!
//! Storage CRUD lives in [`crate::storage::sqlite::Db`]; this module owns
//! the shared types so HTTP handlers + CLI verbs depend on
//! `kb_core::lists::{ListSummary, ListEntry}` rather than the raw rows.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;

use crate::reading::{ReadingSummary, SectionState};
use crate::review::Anchor;
use crate::storage::sqlite::ListEntryRow;
use crate::storage::StorageHandle;
use crate::{Error, Result};

/// Words-per-minute baseline for the *time-estimate* on entries and lists.
/// Deliberately distinct from [`crate::reading::WORDS_PER_MINUTE`] (200),
/// which is the *classification threshold* tuned to separate "read" from
/// "skimmed"; this one is a friendlier "how long will this take me"
/// estimate.
pub const EST_WORDS_PER_MINUTE: u32 = 220;

/// Estimated reading time in whole minutes for `words` words at
/// [`EST_WORDS_PER_MINUTE`]. Zero words → 0; anything non-zero floors at
/// 1 minute (a 30-second read still displays as "~1m", never "~0m").
pub fn est_minutes(words: u32) -> u32 {
    if words == 0 {
        return 0;
    }
    ((f64::from(words) / f64::from(EST_WORDS_PER_MINUTE)).round() as u32).max(1)
}

/// Derived engagement state of one entry — what the daemon can tell the
/// reader about their own progress without being told.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadState {
    Unread,
    InProgress,
    Read,
}

impl ReadState {
    pub fn as_str(self) -> &'static str {
        match self {
            ReadState::Unread => "unread",
            ReadState::InProgress => "in_progress",
            ReadState::Read => "read",
        }
    }
}

/// Manual per-entry override. When set it beats the derived state
/// absolutely — "I read this elsewhere" / "make me re-read this".
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReadOverride {
    Read,
    Unread,
}

impl ReadOverride {
    /// Stable wire form, used in PATCH bodies, sqlite TEXT, and MD export.
    pub fn as_str(self) -> &'static str {
        match self {
            ReadOverride::Read => "read",
            ReadOverride::Unread => "unread",
        }
    }
}

impl fmt::Display for ReadOverride {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ReadOverride {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "read" => Ok(ReadOverride::Read),
            "unread" => Ok(ReadOverride::Unread),
            _ => Err(Error::BadRequest(format!(
                "invalid read override: {s:?} (expected one of: read | unread)"
            ))),
        }
    }
}

/// Where a structural mutation puts an entry. `Before`/`After` reference
/// a sibling entry id; `At` clamps to the list bounds. Default placement
/// for adds is `Last`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PositionSpec {
    First,
    #[default]
    Last,
    Before(String),
    After(String),
    At(u32),
}

/// Tri-state field patch for PATCH-style updates — explicit `Clear`
/// instead of the `Option<Option<T>>` serde pitfall. The wire layer maps
/// JSON `null` → `Clear`, absent → `Keep`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Patch<T> {
    #[default]
    Keep,
    Clear,
    Set(T),
}

impl<T> Patch<T> {
    /// Apply onto a current value: `Keep` preserves, `Clear` empties,
    /// `Set` replaces.
    pub fn apply(&self, current: Option<T>) -> Option<T>
    where
        T: Clone,
    {
        match self {
            Patch::Keep => current,
            Patch::Clear => None,
            Patch::Set(v) => Some(v.clone()),
        }
    }

    pub fn is_keep(&self) -> bool {
        matches!(self, Patch::Keep)
    }
}

/// Import target semantics for [`crate::storage::sqlite::Db::list_import_entries`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    /// Wipe the list's entries and load the incoming set in order — the
    /// true round-trip mode (default).
    Replace,
    /// Keep existing entries; append incoming ones, skipping duplicates
    /// of targets already present.
    Append,
}

impl FromStr for ImportMode {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "replace" => Ok(ImportMode::Replace),
            "append" => Ok(ImportMode::Append),
            _ => Err(Error::BadRequest(format!(
                "invalid import mode: {s:?} (expected one of: replace | append)"
            ))),
        }
    }
}

/// Insert payload for one entry — what the route/import layer hands the
/// storage actor. `id` is pre-minted by the caller ([`new_entry_id`], or
/// preserved from a `kb-entry` comment on round-trip import) so SSE
/// payloads can carry it without a read-back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewListEntry {
    pub id: String,
    pub list_id: String,
    pub kb: String,
    pub artifact_id: String,
    /// Canonical JSON of [`Anchor`] (via [`anchor_to_json`]); `None` =
    /// whole artifact.
    pub anchor_json: Option<String>,
    pub note: Option<String>,
    /// Per-section word estimate computed at add time; `None` for
    /// whole-artifact entries (lance `word_count` covers those).
    pub words: Option<i64>,
    /// Only import sets this (`[x]` ⇒ "read"); interactive adds pass `None`.
    pub read_override: Option<String>,
}

/// One machine write from the indexer's `ListAnchorHook` — refreshed
/// resolution state + word estimate after a reindex. Applied via
/// `list_entries_sync_resolution`, which deliberately does NOT touch
/// `updated_at` (a machine sync must not churn user-facing ordering).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionUpdate {
    pub entry_id: String,
    pub anchor_stale: bool,
    /// `Some` = refresh the stored estimate; `None` = keep the old one
    /// (an unresolvable anchor keeps its last-known words rather than
    /// losing the time estimate).
    pub words: Option<i64>,
}

/// Fresh list id: `l_` + 12 hex chars (6 random bytes) — the comments
/// `c_<12hex>` precedent. Collisions only matter within one kb's lists.
pub fn new_list_id() -> String {
    format!("l_{}", crate::review::short_random_hex())
}

/// Fresh entry id: `le_` + 12 hex chars (6 random bytes).
pub fn new_entry_id() -> String {
    format!("le_{}", crate::review::short_random_hex())
}

/// Canonical JSON for an entry anchor. ALWAYS serialize through this
/// helper — the dedupe index compares anchor TEXT byte-wise, so the JSON
/// must be canonical (serde struct-field order is deterministic; ad-hoc
/// `json!` literals are not guaranteed to match it).
pub fn anchor_to_json(anchor: &Anchor) -> String {
    serde_json::to_string(anchor).expect("Anchor serialization is infallible")
}

/// One list, projected for HTTP responses — header fields plus derived
/// roll-ups (entry counts by effective read state, time estimates). The
/// route layer computes the roll-ups by deriving each entry's state; `kb`
/// is set by the route's fan-out.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListSummary {
    pub kb: String,
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub description: Option<String>,
    pub pinned: bool,
    pub archived: bool,
    /// Unix epoch seconds when the list was created.
    pub created_at: i64,
    /// Unix epoch seconds of the last list edit or structural entry
    /// mutation. Drives the index ordering (after pinned-first).
    pub updated_at: i64,
    pub entry_count: u32,
    /// Effective (post-override) state counts.
    pub read_count: u32,
    pub in_progress_count: u32,
    pub unread_count: u32,
    /// Σ est_minutes over ALL entries (entries without a word source
    /// count 0).
    pub total_minutes: u32,
    /// Σ est_minutes over entries whose effective state ≠ Read.
    pub remaining_minutes: u32,
}

/// One entry, projected for HTTP responses: the stored row + the lance
/// enrichment (`title`/`source_relative`/`folder`, `None` + `tombstone`
/// when the artifact left lance) + the derived `read_state`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListEntry {
    pub kb: String,
    pub id: String,
    pub list_id: String,
    pub artifact_id: String,
    /// 0-based dense position == display index.
    pub position: u32,
    /// Target within the artifact; absent = the whole artifact.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub anchor: Option<Anchor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub read_override: Option<ReadOverride>,
    /// True when the ListAnchorHook last failed to resolve the anchor
    /// against the artifact's current HTML.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    #[cfg_attr(feature = "ts-export", ts(as = "Option<bool>", optional))]
    pub anchor_stale: bool,
    /// Word estimate backing `est_minutes` (per-section for anchored
    /// entries, lance `word_count` otherwise).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub words: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub est_minutes: Option<u32>,
    /// Effective state: `read_override` when set, else derived.
    pub read_state: ReadState,
    /// P8 / invariant #25 — true when this entry targets a captured session
    /// transcript (`kb-category=memory-session`); read-state is suppressed for
    /// these (a transcript has no "read" progress), so the UI hides the badge.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    #[cfg_attr(feature = "ts-export", ts(as = "Option<bool>", optional))]
    pub is_session: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub source_relative: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub folder: Option<String>,
    /// True iff the artifact has left lance — the SPA greys the row and
    /// the trail's next/prev skip it.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    #[cfg_attr(feature = "ts-export", ts(as = "Option<bool>", optional))]
    pub tombstone: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Derive an entry's effective read state from the artifact's cross-visit
/// reading summary. Pure — the route layer builds one `ReadingSummary` per
/// distinct artifact and derives every entry against it.
///
/// The override (when set) is the **requester's** per-user value from
/// `list_entry_user_state` (v0.34 X1). The legacy `list_entries.read_override`
/// column is frozen after `identity_backfill` — never read again here.
/// Phase Y threads the requester username through the assemble path.
///
/// The rule, in precedence order:
///
/// 1. **Override wins absolutely** when set.
/// 2. **Whole artifact** (no anchor, or `Anchor::File`): no visits →
///    `Unread`; ≥ [`crate::reading::FULLY_READ_PCT`] by scroll completion
///    OR by words-weighted read fraction → `Read`; else `InProgress`.
/// 3. **`Anchor::Section { id }`**: match `summary.sections` by
///    `section_id == id` — the capture side stores the LIVE DOM heading id
///    (invariant #19), which the kb authoring contract's stable section
///    ids make the same id space the anchor targets. Section
///    `Read`/`Skim`/`Unseen` map to `Read`/`InProgress`/`Unread`.
/// 4. **`Anchor::Chapter { path }`**: match the leaf heading text (the
///    `" > "`-joined path the annotator builds — same separator
///    `review::resolve_chapter` splits on), exact first then
///    ASCII-case-insensitive. No fuzzy matching — derivation must be
///    deterministic.
/// 5. **`Anchor::Selection`** has no section mapping → falls back to the
///    whole-artifact rule.
/// 6. An **unmatched** section/chapter (id drift, or visits predating the
///    RP-track) is conservative: no visits → `Unread`, else `InProgress`
///    (we know they opened it; we can't prove they read the target).
pub fn derive_read_state(
    anchor: Option<&Anchor>,
    summary: &ReadingSummary,
    override_: Option<ReadOverride>,
) -> ReadState {
    if let Some(ov) = override_ {
        return match ov {
            ReadOverride::Read => ReadState::Read,
            ReadOverride::Unread => ReadState::Unread,
        };
    }

    let whole_artifact = |summary: &ReadingSummary| -> ReadState {
        if summary.visit_count == 0 {
            ReadState::Unread
        } else if summary.is_fully_read || summary.read_pct >= crate::reading::FULLY_READ_PCT {
            ReadState::Read
        } else {
            ReadState::InProgress
        }
    };

    let section_state = |state: SectionState| -> ReadState {
        match state {
            SectionState::Read => ReadState::Read,
            SectionState::Skim => ReadState::InProgress,
            SectionState::Unseen => ReadState::Unread,
        }
    };

    let unmatched = |summary: &ReadingSummary| -> ReadState {
        if summary.visit_count == 0 {
            ReadState::Unread
        } else {
            ReadState::InProgress
        }
    };

    match anchor {
        None | Some(Anchor::File) | Some(Anchor::Selection { .. }) => whole_artifact(summary),
        Some(Anchor::Section { id, .. }) => summary
            .sections
            .iter()
            .find(|s| s.section_id == *id)
            .map(|s| section_state(s.state))
            .unwrap_or_else(|| unmatched(summary)),
        Some(Anchor::Chapter { path }) => {
            let leaf = match path.rsplit(" > ").next().map(str::trim) {
                Some(l) if !l.is_empty() => l,
                _ => return unmatched(summary),
            };
            summary
                .sections
                .iter()
                .find(|s| s.text.trim() == leaf)
                .or_else(|| {
                    summary
                        .sections
                        .iter()
                        .find(|s| s.text.trim().eq_ignore_ascii_case(leaf))
                })
                .map(|s| section_state(s.state))
                .unwrap_or_else(|| unmatched(summary))
        }
    }
}

// --- section word estimate --------------------------------------------------

/// Estimate the word count of an anchor's target range in `html`.
///
/// - `File` → `None` (callers use the lance `word_count` column).
/// - `Section { id }` → locate the element by `id` / `data-kb-id`
///   (the `review::resolve_section` walk). A heading target counts text
///   from the heading forward until the next heading of the same or a
///   higher level (so an h2's estimate includes its h3 subsections); a
///   container target (e.g. `<section>`) counts its own subtree.
/// - `Chapter { path }` → resolve the leaf heading by exact text (then
///   ASCII-case-insensitive) and count the same forward range.
/// - `Selection { snippet }` → the snippet's own word count.
/// - Unresolvable target → `None` (the caller keeps any prior estimate).
///
/// Text inside `<script>`/`<style>`/`<template>`/`<noscript>` never
/// counts, matching the parser's body-text extraction semantics.
pub fn section_words(html: &str, anchor: &Anchor) -> Option<u32> {
    section_words_with(&mut None, html, anchor)
}

/// [`section_words`] against a caller-held parsed-DOM slot (the same
/// contract as `review::fuzzy_resolve_anchor_with`): a loop estimating MANY
/// anchors of one `html` — the list-anchor enrichment hook — pays ONE
/// `Html::parse_document` instead of one per Section/Chapter anchor.
/// `File`/`Selection` anchors never touch the slot. The slot holds
/// `scraper::Html` (`!Send`): callers in async code must drop it before the
/// next `.await`.
pub fn section_words_with(
    doc: &mut Option<scraper::Html>,
    html: &str,
    anchor: &Anchor,
) -> Option<u32> {
    match anchor {
        Anchor::File => None,
        Anchor::Selection { snippet, .. } => Some(snippet.split_whitespace().count() as u32),
        Anchor::Section { id, .. } => {
            let doc = doc.get_or_insert_with(|| scraper::Html::parse_document(html));
            let target = find_node_by_id(doc, id)?;
            Some(words_from_target(doc, target))
        }
        Anchor::Chapter { path } => {
            let leaf = path.rsplit(" > ").next().map(str::trim)?;
            if leaf.is_empty() {
                return None;
            }
            let doc = doc.get_or_insert_with(|| scraper::Html::parse_document(html));
            let target = find_heading_by_text(doc, leaf)?;
            Some(words_from_target(doc, target))
        }
    }
}

/// Bounded character budget for [`section_passage`]'s returned text — a
/// generous window (CT-B4, `kb memory expand`) so a resolved anchor's
/// surrounding passage prints without dumping an entire long section to a
/// terminal.
pub const SECTION_PASSAGE_CHAR_CAP: usize = 4000;

/// CT-B4 (`kb memory expand`) — the printed passage for a `Section`/
/// `Chapter` anchor that [`crate::review::fuzzy_resolve_anchor`] has ALREADY
/// resolved (not stale). Uses the EXACT SAME target-finding + range rules as
/// [`section_words`] (this file's word-count sibling — a heading target's
/// range runs forward to the next heading of the same-or-higher level; a
/// container target is its own subtree), so the printed passage always
/// matches what the estimate was sized over. `resolution` supplies the
/// CURRENT matched text for `Chapter` (a `Fuzzy` match's heading text can
/// differ from the anchor's stored `path`; re-deriving the target from the
/// stale stored text would desync from what `fuzzy_resolve_anchor` actually
/// found) — `Section`'s `id` is looked up directly since section resolution
/// is exact-or-stale, never fuzzy.
///
/// Returns `None` for `File`/`Selection` (no DOM walk needed: `File` has no
/// single passage — whole-artifact — and a `Selection` match's own text
/// already comes back on `resolution` verbatim, so callers read it from
/// there instead) and for a `Stale` resolution (never guess a passage for
/// an anchor that didn't resolve) or an internal lookup miss (defensive;
/// shouldn't happen when `resolution` truly came from resolving this same
/// `html`/`anchor` pair).
pub fn section_passage(
    html: &str,
    anchor: &Anchor,
    resolution: &crate::review::Resolution,
) -> Option<String> {
    use crate::review::Resolution;
    match (anchor, resolution) {
        (Anchor::File, _) | (Anchor::Selection { .. }, _) => None,
        // A Stale Section resolution means `id` matched no element (the
        // identical check `find_node_by_id` runs), so this arm falls
        // through to `None` on its own — no separate Stale case needed.
        (Anchor::Section { id, .. }, _) => {
            let doc = scraper::Html::parse_document(html);
            let target = find_node_by_id(&doc, id)?;
            Some(cap_passage_chars(&text_from_target(&doc, target)))
        }
        (Anchor::Chapter { .. }, Resolution::Exact(heading) | Resolution::Fuzzy(heading, _)) => {
            let doc = scraper::Html::parse_document(html);
            let target = find_heading_by_text(&doc, heading)?;
            Some(cap_passage_chars(&text_from_target(&doc, target)))
        }
        (Anchor::Chapter { .. }, Resolution::Stale) => None,
    }
}

/// Collapse whitespace runs (the walk below joins text nodes with plain
/// spaces, matching `element_text`) and cap at `SECTION_PASSAGE_CHAR_CAP`
/// characters, trimming to the nearest word boundary with a trailing "…".
fn cap_passage_chars(s: &str) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= SECTION_PASSAGE_CHAR_CAP {
        return collapsed;
    }
    let capped: String = collapsed.chars().take(SECTION_PASSAGE_CHAR_CAP).collect();
    let truncated = match capped.rfind(char::is_whitespace) {
        Some(idx) => capped[..idx].trim_end(),
        None => capped.as_str(),
    };
    format!("{truncated}…")
}

/// `h1`..`h6` → 1..6; anything else → `None`.
fn heading_level(name: &str) -> Option<u8> {
    match name {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

fn find_node_by_id(doc: &scraper::Html, id: &str) -> Option<ego_tree::NodeId> {
    for node in doc.tree.root().descendants() {
        if let Some(el) = node.value().as_element() {
            if el.id() == Some(id) || el.attr("data-kb-id") == Some(id) {
                return Some(node.id());
            }
        }
    }
    None
}

fn find_heading_by_text(doc: &scraper::Html, leaf: &str) -> Option<ego_tree::NodeId> {
    let mut ci_match: Option<ego_tree::NodeId> = None;
    for node in doc.tree.root().descendants() {
        let Some(el) = node.value().as_element() else {
            continue;
        };
        if heading_level(el.name()).is_none() {
            continue;
        }
        let text = element_text(doc, node.id());
        let text = text.trim();
        if text == leaf {
            return Some(node.id());
        }
        if ci_match.is_none() && text.eq_ignore_ascii_case(leaf) {
            ci_match = Some(node.id());
        }
    }
    ci_match
}

/// Concatenated text of one node's subtree, skipping non-content elements.
fn element_text(doc: &scraper::Html, id: ego_tree::NodeId) -> String {
    let node = doc.tree.get(id).expect("node id from this tree");
    let mut out = String::new();
    for d in node.descendants() {
        if let Some(t) = d.value().as_text() {
            if !in_skipped_element(&d) {
                out.push_str(t);
                out.push(' ');
            }
        }
    }
    out
}

/// True when any ancestor is a non-content container whose text must not
/// count (script/style/template/noscript).
fn in_skipped_element(node: &ego_tree::NodeRef<'_, scraper::Node>) -> bool {
    node.ancestors().any(|a| {
        a.value()
            .as_element()
            .is_some_and(|el| matches!(el.name(), "script" | "style" | "template" | "noscript"))
    })
}

/// Words in the target's range. Heading target → from the heading (its own
/// text included — it's inside the range) forward in document order until
/// the next heading of same-or-higher level. Non-heading target → its own
/// subtree only.
fn words_from_target(doc: &scraper::Html, target: ego_tree::NodeId) -> u32 {
    let target_node = doc.tree.get(target).expect("node id from this tree");
    let target_level = target_node
        .value()
        .as_element()
        .and_then(|el| heading_level(el.name()));

    let Some(level) = target_level else {
        // Container element — count its own subtree.
        return element_text(doc, target).split_whitespace().count() as u32;
    };

    // Heading — walk the whole document pre-order; start counting at the
    // target, stop at the next same-or-higher heading.
    let mut in_range = false;
    let mut words: usize = 0;
    for node in doc.tree.root().descendants() {
        if node.id() == target {
            in_range = true;
            continue;
        }
        if !in_range {
            continue;
        }
        if let Some(el) = node.value().as_element() {
            if let Some(l) = heading_level(el.name()) {
                if l <= level {
                    break;
                }
            }
        }
        if let Some(t) = node.value().as_text() {
            if !in_skipped_element(&node) {
                words += t.split_whitespace().count();
            }
        }
    }
    // The target heading's own text nodes were visited inside the range
    // (pre-order puts them right after the heading element), so they're
    // already counted.
    words as u32
}

/// [`section_passage`]'s text-collecting sibling of [`words_from_target`] —
/// SAME target-range rule (heading → forward to next same-or-higher
/// heading; container → its own subtree), collecting the joined text
/// instead of a word count. Uncapped; [`cap_passage_chars`] bounds the
/// result at the call site.
fn text_from_target(doc: &scraper::Html, target: ego_tree::NodeId) -> String {
    let target_node = doc.tree.get(target).expect("node id from this tree");
    let target_level = target_node
        .value()
        .as_element()
        .and_then(|el| heading_level(el.name()));

    let Some(level) = target_level else {
        return element_text(doc, target);
    };

    let mut in_range = false;
    let mut out = String::new();
    for node in doc.tree.root().descendants() {
        if node.id() == target {
            in_range = true;
            continue;
        }
        if !in_range {
            continue;
        }
        if let Some(el) = node.value().as_element() {
            if let Some(l) = heading_level(el.name()) {
                if l <= level {
                    break;
                }
            }
        }
        if let Some(t) = node.value().as_text() {
            if !in_skipped_element(&node) {
                out.push_str(t);
                out.push(' ');
            }
        }
    }
    out
}

// --- kb-list/1 import/export -------------------------------------------------

/// Schema discriminator for the portable list document (JSON and the
/// `kb-list` provenance comment in Markdown).
pub const EXPORT_SCHEMA: &str = "kb-list/1";

/// The portable list document — the JSON export body IS the JSON import
/// body, and the Markdown codec round-trips through the same struct.
/// Import ignores `kb`/`list_id` (targeting is the caller's job),
/// `pinned`/`archived` (header state isn't entry data) and per-entry
/// `title`s (the daemon re-resolves them).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListExport {
    pub schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kb: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_id: Option<String>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub entries: Vec<ExportEntry>,
}

/// One portable entry. On import, resolution tries `artifact_id` first,
/// then `ArtifactId::from_path(path)` — ids are path hashes, so a doc
/// exported from one kb resolves in another kb holding the same
/// source-relative path.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExportEntry {
    /// Round-trip id — preserved on replace-into-the-same-list so
    /// `created_at` survives. Absent on hand-authored docs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Source-relative path within the kb.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "source_relative"
    )]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_override: Option<ReadOverride>,
    /// Display-only (current title at export time; ignored on import).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// v0.33 X3 — remove every tombstoned entry from `list_id` in ONE
/// transaction. Tombstone rule mirrors the read path
/// (`routes/lists::enrich_artifacts`): an entry is tombstoned when
/// `get_by_ids` cannot resolve its `artifact_id`. Moved docs are rekeyed
/// by the relocate cascade, so they resolve under the new id and are
/// NOT pruned. Returns the count removed (0 when none / already clean).
/// Does not emit SSE (caller's job) and never bumps the index generation
/// (invariant #25).
///
/// Also returns the removed rows so the route can emit per-entry
/// `list.entry.removed` events mirroring the single-entry DELETE path.
pub async fn prune_list(
    storage: &StorageHandle,
    list_id: &str,
    now_unix: i64,
) -> Result<(usize, Vec<ListEntryRow>)> {
    // 404 path: list must exist.
    if storage.list_get(list_id.to_string()).await?.is_none() {
        return Err(Error::NotFound(format!("list {list_id}")));
    }
    let entries = storage.list_entries_for_list(list_id.to_string()).await?;
    if entries.is_empty() {
        return Ok((0, Vec::new()));
    }
    // Same resolution as the read path: batch get_by_ids (exact id; no
    // moves-chain lookup — relocate rekeys list_entries itself).
    let mut ids: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for e in &entries {
        if seen.insert(e.artifact_id.clone()) {
            ids.push(e.artifact_id.clone());
        }
    }
    let live: HashSet<String> = storage
        .get_by_ids(ids)
        .await?
        .into_iter()
        .map(|d| d.id)
        .collect();
    let tombstoned: Vec<String> = entries
        .into_iter()
        .filter(|e| !live.contains(&e.artifact_id))
        .map(|e| e.id)
        .collect();
    if tombstoned.is_empty() {
        return Ok((0, Vec::new()));
    }
    let removed = storage
        .list_entries_remove_many(list_id.to_string(), tombstoned, now_unix)
        .await?;
    Ok((removed.len(), removed))
}

/// Build the portable document from the wire shapes the routes already
/// assemble.
pub fn export_doc(list: &ListSummary, entries: &[ListEntry]) -> ListExport {
    ListExport {
        schema: EXPORT_SCHEMA.to_string(),
        kb: Some(list.kb.clone()),
        list_id: Some(list.id.clone()),
        title: list.title.clone(),
        description: list.description.clone(),
        pinned: list.pinned,
        archived: list.archived,
        entries: entries
            .iter()
            .map(|e| ExportEntry {
                id: Some(e.id.clone()),
                path: e.source_relative.clone(),
                artifact_id: Some(e.artifact_id.clone()),
                anchor: e.anchor.clone(),
                note: e.note.clone(),
                read_override: e.read_override,
                title: e.title.clone(),
            })
            .collect(),
    }
}

pub fn export_json(doc: &ListExport) -> String {
    let mut s = serde_json::to_string_pretty(doc).expect("ListExport serializes");
    s.push('\n');
    s
}

/// Parse the JSON document. A present-but-foreign `schema` is rejected
/// loudly; an absent one is tolerated (hand-authored minimum is
/// `{"entries":[{"path":"…"}]}` — `title` defaults empty for
/// import-into-existing-list callers).
pub fn import_from_json(s: &str) -> Result<ListExport> {
    #[derive(Deserialize)]
    struct Probe {
        #[serde(default)]
        schema: Option<String>,
    }
    let probe: Probe = serde_json::from_str(s)
        .map_err(|e| Error::BadRequest(format!("invalid list JSON: {e}")))?;
    if let Some(schema) = probe.schema.as_deref() {
        if schema != EXPORT_SCHEMA {
            return Err(Error::BadRequest(format!(
                "unsupported list schema {schema:?} (expected {EXPORT_SCHEMA:?})"
            )));
        }
    }
    // Tolerate the minimum shape: title/schema defaults.
    #[derive(Deserialize)]
    struct Loose {
        #[serde(default)]
        schema: Option<String>,
        #[serde(default)]
        kb: Option<String>,
        #[serde(default)]
        list_id: Option<String>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        pinned: bool,
        #[serde(default)]
        archived: bool,
        #[serde(default)]
        entries: Vec<ExportEntry>,
    }
    let loose: Loose = serde_json::from_str(s)
        .map_err(|e| Error::BadRequest(format!("invalid list JSON: {e}")))?;
    Ok(ListExport {
        schema: loose.schema.unwrap_or_else(|| EXPORT_SCHEMA.to_string()),
        kb: loose.kb,
        list_id: loose.list_id,
        title: loose.title.unwrap_or_default(),
        description: loose.description,
        pinned: loose.pinned,
        archived: loose.archived,
        entries: loose.entries,
    })
}

/// Markdown codec. The grammar is pinned by the golden-document test —
/// hand/Claude-writable in one shot, git-diffable, round-trippable.
pub mod md {
    use super::{Anchor, Error, ExportEntry, ListExport, ReadOverride, Result, EXPORT_SCHEMA};
    use serde::{Deserialize, Serialize};

    /// The `kb-list` provenance comment payload.
    #[derive(Debug, Serialize, Deserialize)]
    struct KbListMeta {
        schema: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kb: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        list_id: Option<String>,
    }

    /// The `kb-entry` machine-state comment payload. Fields here beat
    /// the visible markdown on conflict (checkbox / fragment).
    #[derive(Debug, Default, Serialize, Deserialize)]
    struct KbEntryMeta {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        read_override: Option<ReadOverride>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        anchor: Option<Anchor>,
    }

    /// True for a bare 12-hex artifact id used in place of a path.
    fn is_artifact_id(s: &str) -> bool {
        s.len() == 12
            && s.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    }

    /// Link text is display-only but must not break the `](` scan —
    /// square brackets are swapped for lookalikes on export.
    fn sanitize_link_text(s: &str) -> String {
        s.chars()
            .map(|c| match c {
                '[' => '⟦',
                ']' => '⟧',
                _ => c,
            })
            .collect()
    }

    /// A plain `Section{id}` rides the link's `#fragment`; anything the
    /// fragment can't carry (tag/snippet, chapter, selection) goes into
    /// the `kb-entry` comment instead.
    fn fragment_for(anchor: &Anchor) -> Option<&str> {
        match anchor {
            Anchor::Section {
                id,
                tag: None,
                snippet: None,
            } => Some(id),
            _ => None,
        }
    }

    pub fn to_markdown(doc: &ListExport) -> String {
        let mut out = String::new();
        out.push_str(&format!("# {}\n", doc.title));
        if let Some(desc) = doc.description.as_deref().filter(|d| !d.is_empty()) {
            out.push('\n');
            for line in desc.lines() {
                if line.is_empty() {
                    out.push_str(">\n");
                } else {
                    out.push_str(&format!("> {line}\n"));
                }
            }
        }
        let meta = KbListMeta {
            schema: EXPORT_SCHEMA.to_string(),
            kb: doc.kb.clone(),
            list_id: doc.list_id.clone(),
        };
        out.push('\n');
        out.push_str(&format!(
            "<!-- kb-list {} -->\n",
            serde_json::to_string(&meta).expect("meta serializes")
        ));
        out.push('\n');
        for (i, e) in doc.entries.iter().enumerate() {
            let checked = e.read_override == Some(ReadOverride::Read);
            let text = e
                .title
                .as_deref()
                .map(sanitize_link_text)
                .unwrap_or_else(|| "(removed)".to_string());
            let mut target = match (&e.path, &e.artifact_id) {
                (Some(p), _) => p.clone(),
                (None, Some(id)) => id.clone(),
                (None, None) => String::new(),
            };
            if let Some(frag) = e.anchor.as_ref().and_then(fragment_for) {
                target = format!("{target}#{frag}");
            }
            if target.contains(' ') || target.contains(')') {
                target = format!("<{target}>");
            }
            let entry_meta = KbEntryMeta {
                id: e.id.clone(),
                read_override: e.read_override,
                anchor: e
                    .anchor
                    .as_ref()
                    .filter(|a| fragment_for(a).is_none())
                    .cloned(),
            };
            out.push_str(&format!(
                "{}. [{}] [{}]({}) <!-- kb-entry {} -->\n",
                i + 1,
                if checked { 'x' } else { ' ' },
                text,
                target,
                serde_json::to_string(&entry_meta).expect("entry meta serializes"),
            ));
            if let Some(note) = e.note.as_deref().filter(|n| !n.is_empty()) {
                for line in note.lines() {
                    out.push_str(&format!("   {line}\n"));
                }
            }
        }
        out
    }

    struct ParsedItem {
        checked: bool,
        target: String,
        meta: KbEntryMeta,
    }

    /// `N. [ ] [text](target) <!-- kb-entry {…} -->` — returns `None`
    /// for lines that aren't entry items.
    fn parse_item_line(line: &str) -> Option<ParsedItem> {
        let s = line.trim_start();
        let digits = s.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let s = &s[digits..];
        let s = s.strip_prefix('.').or_else(|| s.strip_prefix(')'))?;
        let s = s.trim_start();
        let s = s.strip_prefix('[')?;
        let checked = match s.chars().next()? {
            ' ' => false,
            'x' | 'X' => true,
            _ => return None,
        };
        let s = &s[1..];
        let s = s.strip_prefix(']')?;
        let s = s.trim_start();
        let s = s.strip_prefix('[')?;
        let close = s.find("](")?;
        let after = &s[close + 2..];
        // `<…>`-wrapped targets may contain spaces / parens.
        let (target, rest) = if let Some(inner) = after.strip_prefix('<') {
            let end = inner.find('>')?;
            let rest = inner[end + 1..].strip_prefix(')')?;
            (inner[..end].to_string(), rest)
        } else {
            let end = after.find(')')?;
            (after[..end].trim().to_string(), &after[end + 1..])
        };
        let meta = rest
            .find("<!-- kb-entry ")
            .and_then(|i| {
                let json_start = i + "<!-- kb-entry ".len();
                let tail = &rest[json_start..];
                let end = tail.find(" -->")?;
                serde_json::from_str::<KbEntryMeta>(tail[..end].trim()).ok()
            })
            .unwrap_or_default();
        Some(ParsedItem {
            checked,
            target,
            meta,
        })
    }

    /// Parse a list document. Forward-compatible: anything that isn't
    /// the title, the description blockquote, a `kb-list` comment, an
    /// entry item, or an item's indented note lines is ignored.
    pub fn parse_markdown(input: &str) -> Result<ListExport> {
        let mut title: Option<String> = None;
        let mut description_lines: Vec<String> = Vec::new();
        let mut in_description = false;
        let mut meta: Option<KbListMeta> = None;
        let mut entries: Vec<ExportEntry> = Vec::new();

        for line in input.lines() {
            if title.is_none() {
                if let Some(t) = line.strip_prefix("# ") {
                    title = Some(t.trim().to_string());
                    in_description = true;
                }
                continue;
            }
            // Description: the blockquote following the title, before
            // anything else substantial.
            if in_description {
                let trimmed = line.trim_start();
                if let Some(q) = trimmed.strip_prefix('>') {
                    description_lines.push(q.strip_prefix(' ').unwrap_or(q).to_string());
                    continue;
                }
                if !trimmed.is_empty() && !description_lines.is_empty() {
                    in_description = false;
                } else if trimmed.is_empty() {
                    if !description_lines.is_empty() {
                        in_description = false;
                    }
                    continue;
                }
            }
            // kb-list provenance comment (first one wins).
            let trimmed = line.trim_start();
            if meta.is_none() && trimmed.starts_with("<!-- kb-list ") {
                if let Some(end) = trimmed.find(" -->") {
                    let json = trimmed["<!-- kb-list ".len()..end].trim();
                    if let Ok(m) = serde_json::from_str::<KbListMeta>(json) {
                        if m.schema != EXPORT_SCHEMA {
                            return Err(Error::BadRequest(format!(
                                "unsupported list schema {:?} (expected {EXPORT_SCHEMA:?})",
                                m.schema
                            )));
                        }
                        meta = Some(m);
                        continue;
                    }
                }
            }
            if let Some(item) = parse_item_line(line) {
                let (path, artifact_id, fragment) = {
                    let (base, fragment) = match item.target.split_once('#') {
                        Some((b, f)) => (b.to_string(), Some(f.to_string())),
                        None => (item.target.clone(), None),
                    };
                    if is_artifact_id(&base) {
                        (None, Some(base), fragment)
                    } else if base.is_empty() {
                        (None, None, fragment)
                    } else {
                        (Some(base), None, fragment)
                    }
                };
                // Comment fields beat the visible markdown.
                let anchor = item.meta.anchor.or_else(|| {
                    fragment.map(|id| Anchor::Section {
                        id,
                        tag: None,
                        snippet: None,
                    })
                });
                let read_override = item.meta.read_override.or(if item.checked {
                    Some(ReadOverride::Read)
                } else {
                    None
                });
                entries.push(ExportEntry {
                    id: item.meta.id,
                    path,
                    artifact_id,
                    anchor,
                    note: None,
                    read_override,
                    title: None,
                });
                continue;
            }
            // Indented continuation lines under the last item = its note.
            if !entries.is_empty() && line.starts_with("   ") && !line.trim().is_empty() {
                let last = entries.last_mut().expect("non-empty");
                let addition = line.trim_start().to_string();
                last.note = Some(match last.note.take() {
                    Some(prev) => format!("{prev}\n{addition}"),
                    None => addition,
                });
            }
        }

        let Some(title) = title else {
            return Err(Error::BadRequest(
                "list markdown must start with a `# Title` heading".into(),
            ));
        };
        let description = if description_lines.is_empty() {
            None
        } else {
            Some(description_lines.join("\n"))
        };
        let (kb, list_id) = meta.map(|m| (m.kb, m.list_id)).unwrap_or((None, None));
        Ok(ListExport {
            schema: EXPORT_SCHEMA.to_string(),
            kb,
            list_id,
            title,
            description,
            pinned: false,
            archived: false,
            entries,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reading::{SectionSummary, StopPoint};

    fn summary(
        visit_count: u32,
        completion_pct: u8,
        read_pct: u8,
        sections: Vec<SectionSummary>,
    ) -> ReadingSummary {
        ReadingSummary {
            completion_pct,
            read_pct,
            is_fully_read: completion_pct >= crate::reading::FULLY_READ_PCT,
            active_ms_total: 0,
            visit_count,
            first_read_at: None,
            last_read_at: None,
            stopped_at: None::<StopPoint>,
            sections,
            top_sections: Vec::new(),
        }
    }

    fn section(id: &str, text: &str, state: SectionState) -> SectionSummary {
        SectionSummary {
            section_id: id.to_string(),
            section_idx: 0,
            text: text.to_string(),
            level: 2,
            words: 100,
            dwell_ms: 0,
            enters: 0,
            state,
        }
    }

    #[test]
    fn est_minutes_rounds_and_floors() {
        assert_eq!(est_minutes(0), 0);
        assert_eq!(est_minutes(1), 1); // floors at 1
        assert_eq!(est_minutes(110), 1); // 0.5 → rounds to 1 (banker-free)
        assert_eq!(est_minutes(220), 1);
        assert_eq!(est_minutes(330), 2); // 1.5 → 2
        assert_eq!(est_minutes(2200), 10);
    }

    #[test]
    fn read_override_round_trips_and_rejects_typo() {
        for ov in [ReadOverride::Read, ReadOverride::Unread] {
            assert_eq!(ov.as_str().parse::<ReadOverride>().unwrap(), ov);
        }
        let err = "Read".parse::<ReadOverride>().unwrap_err();
        assert!(err.to_string().contains("invalid read override"));
    }

    #[test]
    fn id_prefixes_and_entropy() {
        let l = new_list_id();
        let e = new_entry_id();
        assert!(l.starts_with("l_") && l.len() == 14, "{l}");
        assert!(e.starts_with("le_") && e.len() == 15, "{e}");
        assert!(l[2..].chars().all(|c| c.is_ascii_hexdigit()));
        assert!(e[3..].chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(new_list_id(), new_list_id());
    }

    #[test]
    fn anchor_json_is_canonical() {
        let a = Anchor::Section {
            id: "tuning".into(),
            tag: None,
            snippet: None,
        };
        // Byte-identical across calls — the dedupe index depends on it.
        assert_eq!(anchor_to_json(&a), anchor_to_json(&a.clone()));
        assert_eq!(
            anchor_to_json(&a),
            r#"{"kind":"section","id":"tuning","tag":null,"snippet":null}"#
        );
    }

    #[test]
    fn patch_applies() {
        assert_eq!(
            Patch::<String>::Keep.apply(Some("a".into())),
            Some("a".into())
        );
        assert_eq!(Patch::<String>::Clear.apply(Some("a".into())), None);
        assert_eq!(
            Patch::Set("b".to_string()).apply(Some("a".into())),
            Some("b".into())
        );
        assert!(Patch::<String>::default().is_keep());
    }

    #[test]
    fn read_state_override_wins() {
        // A fully-read artifact still reports Unread under an unread override.
        let s = summary(3, 100, 100, vec![]);
        assert_eq!(
            derive_read_state(None, &s, Some(ReadOverride::Unread)),
            ReadState::Unread
        );
        // And a never-visited one reports Read under a read override.
        let s = summary(0, 0, 0, vec![]);
        assert_eq!(
            derive_read_state(None, &s, Some(ReadOverride::Read)),
            ReadState::Read
        );
    }

    #[test]
    fn read_state_whole_artifact_thresholds() {
        assert_eq!(
            derive_read_state(None, &summary(0, 0, 0, vec![]), None),
            ReadState::Unread
        );
        assert_eq!(
            derive_read_state(None, &summary(1, 40, 10, vec![]), None),
            ReadState::InProgress
        );
        // Scroll completion crosses the bar…
        assert_eq!(
            derive_read_state(None, &summary(1, 95, 10, vec![]), None),
            ReadState::Read
        );
        // …or the words-weighted read fraction does.
        assert_eq!(
            derive_read_state(None, &summary(1, 40, 96, vec![]), None),
            ReadState::Read
        );
        // Anchor::File and Selection use the same rule.
        assert_eq!(
            derive_read_state(Some(&Anchor::File), &summary(1, 95, 0, vec![]), None),
            ReadState::Read
        );
        let sel = Anchor::Selection {
            css_path: "p".into(),
            offset: 0,
            snippet: "quoted".into(),
        };
        assert_eq!(
            derive_read_state(Some(&sel), &summary(1, 40, 0, vec![]), None),
            ReadState::InProgress
        );
    }

    #[test]
    fn read_state_section_maps_three_states() {
        let anchor = Anchor::Section {
            id: "tuning".into(),
            tag: None,
            snippet: None,
        };
        for (st, expect) in [
            (SectionState::Read, ReadState::Read),
            (SectionState::Skim, ReadState::InProgress),
            (SectionState::Unseen, ReadState::Unread),
        ] {
            let s = summary(1, 50, 20, vec![section("tuning", "Tuning", st)]);
            assert_eq!(derive_read_state(Some(&anchor), &s, None), expect);
        }
    }

    #[test]
    fn read_state_chapter_matches_leaf_text() {
        let anchor = Anchor::Chapter {
            path: "Top > Mid > Virtual nodes".into(),
        };
        let s = summary(
            1,
            50,
            20,
            vec![
                section("a", "Intro", SectionState::Skim),
                section("b", "Virtual nodes", SectionState::Read),
            ],
        );
        assert_eq!(derive_read_state(Some(&anchor), &s, None), ReadState::Read);
        // Case-insensitive fallback.
        let anchor_ci = Anchor::Chapter {
            path: "virtual NODES".into(),
        };
        assert_eq!(
            derive_read_state(Some(&anchor_ci), &s, None),
            ReadState::Read
        );
    }

    #[test]
    fn read_state_unmatched_section_falls_back() {
        let anchor = Anchor::Section {
            id: "gone".into(),
            tag: None,
            snippet: None,
        };
        // Never visited → Unread.
        assert_eq!(
            derive_read_state(Some(&anchor), &summary(0, 0, 0, vec![]), None),
            ReadState::Unread
        );
        // Visited but the section id doesn't appear (pre-RP visits or id
        // drift) → conservative InProgress.
        let s = summary(
            2,
            80,
            30,
            vec![section("other", "Other", SectionState::Read)],
        );
        assert_eq!(
            derive_read_state(Some(&anchor), &s, None),
            ReadState::InProgress
        );
    }

    const SECTION_HTML: &str = r#"<!doctype html><html><head><title>t</title>
<style>body { color: red }</style></head><body>
<h1 id="top">Title here</h1>
<p>one two three four five</p>
<h2 id="alpha">Alpha section</h2>
<p>six seven eight</p>
<script>var nine_ten = "eleven twelve";</script>
<h3 id="alpha-sub">Alpha sub</h3>
<p>nine ten</p>
<h2 id="beta">Beta section</h2>
<p>eleven twelve thirteen</p>
<section id="boxed"><p>fourteen fifteen</p></section>
</body></html>"#;

    #[test]
    fn section_words_heading_range() {
        // h2#alpha: "Alpha section" (2) + "six seven eight" (3) + h3 "Alpha
        // sub" (2) + "nine ten" (2) = 9; stops at h2#beta; script excluded.
        let a = Anchor::Section {
            id: "alpha".into(),
            tag: None,
            snippet: None,
        };
        assert_eq!(section_words(SECTION_HTML, &a), Some(9));
        // h1#top runs to the end of the document (no other h1): everything
        // except style/script text. 2 + 5 + 9 + 2+3 + 2+2 = wait — count:
        // "Title here"(2) "one..five"(5) alpha range(9) beta(2+3) boxed(2) = 23.
        let top = Anchor::Section {
            id: "top".into(),
            tag: None,
            snippet: None,
        };
        assert_eq!(section_words(SECTION_HTML, &top), Some(23));
    }

    #[test]
    fn section_words_container_element() {
        let a = Anchor::Section {
            id: "boxed".into(),
            tag: Some("section".into()),
            snippet: None,
        };
        assert_eq!(section_words(SECTION_HTML, &a), Some(2));
    }

    #[test]
    fn section_words_chapter_and_selection() {
        let ch = Anchor::Chapter {
            path: "Title here > Beta section".into(),
        };
        // "Beta section"(2) + "eleven twelve thirteen"(3) + boxed(2) = 7
        // (section#boxed is not a heading, so the range runs to EOF).
        assert_eq!(section_words(SECTION_HTML, &ch), Some(7));
        let sel = Anchor::Selection {
            css_path: "p".into(),
            offset: 0,
            snippet: "a quoted run of six words".into(),
        };
        assert_eq!(section_words(SECTION_HTML, &sel), Some(6));
    }

    #[test]
    fn section_words_with_shared_dom_slot_matches_string_form() {
        // The list-anchor hook threads ONE parsed-DOM slot through many
        // entries; each estimate must equal the parse-per-call form, and
        // File/Selection anchors must not fill the slot.
        let anchors = [
            Anchor::File,
            Anchor::Selection {
                css_path: "p".into(),
                offset: 0,
                snippet: "a quoted run of six words".into(),
            },
            Anchor::Section {
                id: "alpha".into(),
                tag: None,
                snippet: None,
            },
            Anchor::Chapter {
                path: "Title here > Beta section".into(),
            },
        ];
        let mut dom = None;
        for (i, a) in anchors.iter().enumerate() {
            let got = section_words_with(&mut dom, SECTION_HTML, a);
            assert_eq!(got, section_words(SECTION_HTML, a), "anchor #{i}");
            if i < 2 {
                assert!(dom.is_none(), "File/Selection must not parse");
            }
        }
        assert!(dom.is_some(), "Section/Chapter fill the slot once");
    }

    #[test]
    fn section_words_missing_target_is_none() {
        let a = Anchor::Section {
            id: "nope".into(),
            tag: None,
            snippet: None,
        };
        assert_eq!(section_words(SECTION_HTML, &a), None);
        assert_eq!(section_words(SECTION_HTML, &Anchor::File), None);
        let ch = Anchor::Chapter {
            path: "No such heading".into(),
        };
        assert_eq!(section_words(SECTION_HTML, &ch), None);
    }

    // --- CT-B4 (`kb memory expand`) — section_passage ----------------------

    #[test]
    fn section_passage_heading_range_matches_the_words_from_target_range() {
        let a = Anchor::Section {
            id: "alpha".into(),
            tag: None,
            snippet: None,
        };
        let resolution = crate::review::fuzzy_resolve_anchor(SECTION_HTML, &a);
        assert_eq!(resolution, crate::review::Resolution::Exact("alpha".into()));
        let passage = section_passage(SECTION_HTML, &a, &resolution).expect("resolves");
        // Same range `section_words_heading_range` sizes at 9 words: the
        // h2 itself, its own paragraph, the h3 subsection, and ITS
        // paragraph — stopping before h2#beta.
        assert!(passage.contains("Alpha section"));
        assert!(passage.contains("six seven eight"));
        assert!(passage.contains("Alpha sub"));
        assert!(passage.contains("nine ten"));
        assert!(!passage.contains("Beta section"));
        assert!(!passage.contains("eleven twelve thirteen"));
        assert!(
            !passage.contains("Title here"),
            "starts AT the target, not before it"
        );
    }

    #[test]
    fn section_passage_container_element_is_its_own_subtree() {
        let a = Anchor::Section {
            id: "boxed".into(),
            tag: Some("section".into()),
            snippet: None,
        };
        let resolution = crate::review::fuzzy_resolve_anchor(SECTION_HTML, &a);
        let passage = section_passage(SECTION_HTML, &a, &resolution).expect("resolves");
        assert_eq!(passage.trim(), "fourteen fifteen");
    }

    #[test]
    fn section_passage_chapter_uses_the_resolution_not_the_stale_stored_path() {
        let ch = Anchor::Chapter {
            path: "Title here > Beta section".into(),
        };
        let resolution = crate::review::fuzzy_resolve_anchor(SECTION_HTML, &ch);
        assert_eq!(
            resolution,
            crate::review::Resolution::Exact("Beta section".into())
        );
        let passage = section_passage(SECTION_HTML, &ch, &resolution).expect("resolves");
        assert!(passage.contains("Beta section"));
        assert!(passage.contains("eleven twelve thirteen"));
        assert!(
            passage.contains("fourteen fifteen"),
            "runs to EOF — no next heading"
        );
    }

    #[test]
    fn section_passage_is_none_for_a_stale_resolution_never_a_guess() {
        let a = Anchor::Section {
            id: "nope".into(),
            tag: None,
            snippet: None,
        };
        let resolution = crate::review::fuzzy_resolve_anchor(SECTION_HTML, &a);
        assert_eq!(resolution, crate::review::Resolution::Stale);
        assert_eq!(section_passage(SECTION_HTML, &a, &resolution), None);

        let ch = Anchor::Chapter {
            path: "No such heading".into(),
        };
        let ch_resolution = crate::review::fuzzy_resolve_anchor(SECTION_HTML, &ch);
        assert_eq!(ch_resolution, crate::review::Resolution::Stale);
        assert_eq!(section_passage(SECTION_HTML, &ch, &ch_resolution), None);
    }

    #[test]
    fn section_passage_file_and_selection_return_none_the_caller_reads_resolution_instead() {
        let file_resolution = crate::review::fuzzy_resolve_anchor(SECTION_HTML, &Anchor::File);
        assert_eq!(
            section_passage(SECTION_HTML, &Anchor::File, &file_resolution),
            None
        );

        let sel = Anchor::Selection {
            css_path: "p".into(),
            offset: 0,
            snippet: "one two three four five".into(),
        };
        let sel_resolution = crate::review::fuzzy_resolve_anchor(SECTION_HTML, &sel);
        assert_eq!(section_passage(SECTION_HTML, &sel, &sel_resolution), None);
    }

    #[test]
    fn cap_passage_chars_collapses_whitespace_and_truncates_at_a_word_boundary() {
        assert_eq!(cap_passage_chars("  a   b\nc  "), "a b c");

        let long = "word ".repeat(2000); // well past SECTION_PASSAGE_CHAR_CAP
        let capped = cap_passage_chars(long.trim());
        assert!(capped.ends_with('…'));
        assert!(capped.chars().count() <= SECTION_PASSAGE_CHAR_CAP + 1);
        assert!(!capped.trim_end_matches('…').ends_with(' '));
    }

    // --- kb-list/1 codecs -------------------------------------------------

    /// The canonical export — `to_markdown` must reproduce this byte for
    /// byte (the format is pinned here AND in the kb-cli golden test;
    /// divergence fails tests, not users).
    const GOLDEN_MD: &str = "\
# Async Rust, properly

> From zero to executor internals — read in order.

<!-- kb-list {\"schema\":\"kb-list/1\",\"kb\":\"platform\",\"list_id\":\"l_9f3a21c4d0aa\"} -->

1. [ ] [Async from scratch](research/async/from-scratch.html) <!-- kb-entry {\"id\":\"le_a1b2c3d4e5f6\"} -->
   Why futures desugar the way they do.
2. [x] [Pinning, finally explained](research/async/pinning.html#why-pin) <!-- kb-entry {\"id\":\"le_02bd11aa34f0\",\"read_override\":\"read\"} -->
3. [ ] [Executor internals](research/async/executor.html#scheduler) <!-- kb-entry {\"id\":\"le_9c11f2e8b7d3\"} -->
   The scheduler section is the payload.
";

    fn golden_doc() -> ListExport {
        ListExport {
            schema: EXPORT_SCHEMA.to_string(),
            kb: Some("platform".into()),
            list_id: Some("l_9f3a21c4d0aa".into()),
            title: "Async Rust, properly".into(),
            description: Some("From zero to executor internals — read in order.".into()),
            pinned: false,
            archived: false,
            entries: vec![
                ExportEntry {
                    id: Some("le_a1b2c3d4e5f6".into()),
                    path: Some("research/async/from-scratch.html".into()),
                    artifact_id: None,
                    anchor: None,
                    note: Some("Why futures desugar the way they do.".into()),
                    read_override: None,
                    title: Some("Async from scratch".into()),
                },
                ExportEntry {
                    id: Some("le_02bd11aa34f0".into()),
                    path: Some("research/async/pinning.html".into()),
                    artifact_id: None,
                    anchor: Some(Anchor::Section {
                        id: "why-pin".into(),
                        tag: None,
                        snippet: None,
                    }),
                    note: None,
                    read_override: Some(ReadOverride::Read),
                    title: Some("Pinning, finally explained".into()),
                },
                ExportEntry {
                    id: Some("le_9c11f2e8b7d3".into()),
                    path: Some("research/async/executor.html".into()),
                    artifact_id: None,
                    anchor: Some(Anchor::Section {
                        id: "scheduler".into(),
                        tag: None,
                        snippet: None,
                    }),
                    note: Some("The scheduler section is the payload.".into()),
                    read_override: None,
                    title: Some("Executor internals".into()),
                },
            ],
        }
    }

    // invariant:25 kb-list-grammar
    #[test]
    fn golden_markdown_export_is_stable() {
        assert_eq!(md::to_markdown(&golden_doc()), GOLDEN_MD);
    }

    // invariant:25 kb-list-grammar
    #[test]
    fn golden_markdown_parses_back() {
        let parsed = md::parse_markdown(GOLDEN_MD).unwrap();
        // Parse drops display-only titles; everything else round-trips.
        let mut expected = golden_doc();
        for e in &mut expected.entries {
            e.title = None;
        }
        assert_eq!(parsed, expected);
    }

    #[test]
    fn markdown_round_trips_through_itself() {
        let parsed = md::parse_markdown(GOLDEN_MD).unwrap();
        let re_emitted = md::to_markdown(&parsed);
        let re_parsed = md::parse_markdown(&re_emitted).unwrap();
        assert_eq!(parsed, re_parsed);
    }

    #[test]
    fn markdown_parse_tolerates_hand_authored_docs() {
        let doc = "\
# My list

Some prose the parser ignores.

1) [X] [whatever](abcdef012345)
2. [ ] [t](research/notes.html#intro) trailing prose
   first note line
   second note line

## A heading that is ignored
";
        let parsed = md::parse_markdown(doc).unwrap();
        assert_eq!(parsed.title, "My list");
        assert_eq!(parsed.description, None);
        assert_eq!(parsed.entries.len(), 2);
        let e0 = &parsed.entries[0];
        assert_eq!(e0.artifact_id.as_deref(), Some("abcdef012345"));
        assert_eq!(e0.path, None);
        assert_eq!(e0.read_override, Some(ReadOverride::Read));
        let e1 = &parsed.entries[1];
        assert_eq!(e1.path.as_deref(), Some("research/notes.html"));
        assert_eq!(
            e1.anchor,
            Some(Anchor::Section {
                id: "intro".into(),
                tag: None,
                snippet: None
            })
        );
        assert_eq!(
            e1.note.as_deref(),
            Some("first note line\nsecond note line")
        );
    }

    #[test]
    fn markdown_rejects_missing_title_and_foreign_schema() {
        let err = md::parse_markdown("just some text\n").unwrap_err();
        assert!(err.to_string().contains("# Title"), "{err}");
        let doc = "# T\n\n<!-- kb-list {\"schema\":\"kb-list/9\"} -->\n";
        let err = md::parse_markdown(doc).unwrap_err();
        assert!(err.to_string().contains("unsupported list schema"), "{err}");
    }

    #[test]
    fn markdown_non_section_anchor_rides_the_comment_not_the_fragment() {
        let mut doc = golden_doc();
        doc.entries.truncate(1);
        doc.entries[0].anchor = Some(Anchor::Chapter {
            path: "Top > Leaf".into(),
        });
        doc.entries[0].note = None;
        let out = md::to_markdown(&doc);
        assert!(
            !out.contains(".html#"),
            "chapter anchors must not fake a fragment: {out}"
        );
        assert!(out.contains("\"anchor\":{\"kind\":\"chapter\""), "{out}");
        let parsed = md::parse_markdown(&out).unwrap();
        assert_eq!(
            parsed.entries[0].anchor,
            Some(Anchor::Chapter {
                path: "Top > Leaf".into()
            })
        );
    }

    #[test]
    fn markdown_wraps_awkward_targets_in_angle_brackets() {
        let mut doc = golden_doc();
        doc.entries.truncate(1);
        doc.entries[0].path = Some("folder with space/file (1).html".into());
        doc.entries[0].note = None;
        let out = md::to_markdown(&doc);
        assert!(out.contains("(<folder with space/file (1).html>)"), "{out}");
        let parsed = md::parse_markdown(&out).unwrap();
        assert_eq!(
            parsed.entries[0].path.as_deref(),
            Some("folder with space/file (1).html")
        );
    }

    #[test]
    fn json_round_trips_and_tolerates_minimum_shape() {
        let doc = golden_doc();
        let s = export_json(&doc);
        let parsed = import_from_json(&s).unwrap();
        assert_eq!(parsed, doc);

        let min = import_from_json(r#"{"entries":[{"path":"a/b.html"}]}"#).unwrap();
        assert_eq!(min.schema, EXPORT_SCHEMA);
        assert_eq!(min.title, "");
        assert_eq!(min.entries.len(), 1);
        assert_eq!(min.entries[0].path.as_deref(), Some("a/b.html"));

        let err = import_from_json(r#"{"schema":"other/1","entries":[]}"#).unwrap_err();
        assert!(err.to_string().contains("unsupported list schema"), "{err}");
        let err = import_from_json("not json").unwrap_err();
        assert!(err.to_string().contains("invalid list JSON"), "{err}");
    }

    // --- v0.33 X3: prune_list engine --------------------------------------

    #[tokio::test]
    async fn prune_list_removes_only_tombstones_preserves_live_order() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = crate::storage::StorageActor::spawn(
            tmp.path().join("lance"),
            tmp.path().join("idx.db"),
            None,
        )
        .await
        .unwrap();

        // Live docs in lance.
        let live_a = "aaaaaaaaaaaa";
        let live_b = "bbbbbbbbbbbb";
        let dead_a = "cccccccccccc";
        let dead_b = "dddddddddddd";
        storage
            .upsert_doc(crate::storage::schema::Doc::placeholder(
                live_a,
                "/src/a.html",
            ))
            .await
            .unwrap();
        storage
            .upsert_doc(crate::storage::schema::Doc::placeholder(
                live_b,
                "/src/b.html",
            ))
            .await
            .unwrap();

        storage
            .list_create("l_prune".into(), "Prune me".into(), None, false, 100)
            .await
            .unwrap();
        // UNIQUE (list_id, artifact_id, anchor) — each dead target must be distinct.
        for (eid, art) in [
            ("le_1", live_a),
            ("le_2", dead_a),
            ("le_3", live_b),
            ("le_4", dead_b),
        ] {
            storage
                .list_entry_add(
                    NewListEntry {
                        id: eid.into(),
                        list_id: "l_prune".into(),
                        kb: "t".into(),
                        artifact_id: art.into(),
                        anchor_json: None,
                        note: None,
                        words: None,
                        read_override: None,
                    },
                    PositionSpec::Last,
                    "operator".to_string(),
                    100,
                )
                .await
                .unwrap();
        }

        let (n, removed) = prune_list(&storage, "l_prune", 200).await.unwrap();
        assert_eq!(n, 2);
        assert_eq!(removed.len(), 2);
        let remaining = storage
            .list_entries_for_list("l_prune".into())
            .await
            .unwrap();
        assert_eq!(remaining.len(), 2);
        assert_eq!(remaining[0].id, "le_1");
        assert_eq!(remaining[0].position, 0);
        assert_eq!(remaining[1].id, "le_3");
        assert_eq!(remaining[1].position, 1);

        // Second prune is a no-op.
        let (n2, _) = prune_list(&storage, "l_prune", 201).await.unwrap();
        assert_eq!(n2, 0);

        // Unknown list → NotFound.
        let err = prune_list(&storage, "l_nope", 202).await.unwrap_err();
        assert!(err.to_string().contains("list l_nope"), "{err}");
    }
}
