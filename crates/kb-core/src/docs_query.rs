//! S-milestone S1 — server-side mirror of the SPA gallery's
//! filter/sort/paginate pipeline. Pure functions over a pre-decorated
//! `Vec<DocRow>` (a [`DocSummary`] plus its derived folder string).
//! The kb-server route owns folder derivation (it has the kb's source
//! root); this module owns the predicates.
//!
//! Reference implementations the Rust must match by semantics:
//!  - `web/src/routes/gallery.tsx` filter `useMemo`
//!  - `web/src/routes/gallery.tsx::capabilityFlags`
//!  - `web/src/lib/sort.ts::sortComparator` / `groupByFolder`
//!  - `web/src/lib/derive.ts::isIndexPage` / `tagsFor` / `pathToTags`
//!
//! Aggregators (`aggregate_folders`, `aggregate_tags`) preserve the
//! response shape produced by the existing `routes/folders.rs` and
//! `routes/tags.rs` so they can be swapped in without touching the
//! SPA in S3.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

use crate::storage::lance::DocSummary;

/// One unit of work for filter/sort. The decoration is done by the
/// caller because folder derivation needs the kb's source root, which
/// lives outside `kb-core`.
#[derive(Debug, Clone)]
pub struct DocRow {
    pub doc: DocSummary,
    pub folder: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortKey {
    #[default]
    Recent,
    Indexed,
    /// v0.15 — sort by filesystem birth time (`created_unix`). Rows
    /// with no btime sort as `i64::MIN` (same convention as `Recent` /
    /// `Indexed`).
    Created,
    Title,
    Words,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortDir {
    Asc,
    #[default]
    Desc,
}

/// S7 — gallery's secondary axis for the group-by control. Selecting
/// `Folder` prepends folder ASC to the primary sort/dir, producing a
/// flat row stream the SPA can scan for folder transitions to insert
/// section headers without losing pagination.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GroupKey {
    #[default]
    None,
    Folder,
}

impl SortKey {
    pub fn default_dir(self) -> SortDir {
        match self {
            SortKey::Title => SortDir::Asc,
            _ => SortDir::Desc,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Projection {
    /// id + title + path + mtime + indexed_at. Used by the detail
    /// view's siblings popover and any "lookup" UI.
    Slim,
    /// Full [`DocSummary`] minus atlas coords. The SPA gallery's
    /// default shape.
    #[default]
    Default,
    /// `Default` plus `atlas_x/y/cluster`. The atlas view's shape.
    Atlas,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Capability {
    Svg,
    Interactive,
    Code,
    Longread,
}

impl Capability {
    pub fn matches(self, d: &DocSummary) -> bool {
        match self {
            Capability::Svg => d.svg_count.is_some_and(|n| n > 0),
            Capability::Interactive => {
                d.has_canvas.unwrap_or(false)
                    || d.has_form.unwrap_or(false)
                    || d.has_animation.unwrap_or(false)
                    || d.has_details.unwrap_or(false)
                    || d.has_drag.unwrap_or(false)
            }
            Capability::Code => d.code_block_count.is_some_and(|n| n > 0),
            Capability::Longread => d.longread.unwrap_or(false),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DocsQuery {
    pub folder: Option<String>,
    /// v0.33 X1 — when true, [`folder`](Self::folder) matches by equality
    /// only (no descendants). Default false keeps the historical
    /// descendant-inclusive prefix match. Only meaningful when `folder`
    /// is set; ignored otherwise.
    pub folder_exact: bool,
    pub tags: Vec<String>,
    pub caps: Vec<Capability>,
    pub since_unix: Option<i64>,
    /// v0.22 — positive `kb-category` include (exact match), route-injected
    /// from `?category=`. Like [`exclude_categories`](Self::exclude_categories)
    /// it gates EVERY OR branch in [`matches`] (it is not a DSL atom, so the
    /// lowering never distributes it into `alternatives`), mirroring the
    /// search route's exact-category filter. `None` = no category constraint.
    pub category: Option<String>,
    /// v0.22 — absolute `mtime_unix` window bounds (inclusive), route-injected
    /// from `?from=`/`?to=` (unix seconds). Powers the reader's "files modified
    /// around this time" pivot. Filters on `mtime_unix` SPECIFICALLY (not the
    /// `indexed_at`-or-`mtime` recency used by [`since_unix`](Self::since_unix));
    /// a row with no `mtime_unix` is dropped when either bound is set. Gated in
    /// [`matches`] across every OR branch (route-level, no DSL atom).
    pub mtime_from: Option<i64>,
    pub mtime_to: Option<i64>,
    /// W2.3a — id-set membership gate, route-injected from `?ids=` (the
    /// atlas lasso / working-set gallery pivot, invariant #35). Like
    /// `category`/the mtime window, this is a flat-param gate with no DSL
    /// atom, so [`matches`] applies it across every OR branch rather than
    /// per-conjunct. `None` or an empty set means "no id-set constraint" —
    /// every row passes this gate.
    pub ids: Option<Vec<String>>,
    pub index_only: bool,
    /// N-track — categories the row's `kb_category` must NOT be in. The
    /// gallery defaults this to `["note"]` so notes (which have their own
    /// `/notes` view + contextual panel) don't clutter the document grid or
    /// the atlas. Empty = no category exclusion. Search bypasses this filter
    /// (it doesn't route through `matches`), so notes stay findable.
    ///
    /// NB: this is a *route-injected* gate, NOT a DSL atom — [`matches`]
    /// applies it across every OR branch (it is not distributed into
    /// `alternatives`), unlike the per-conjunct `exclude_*` fields below.
    pub exclude_categories: Vec<String>,
    /// N-track — drop *editable notes* (Markdown + `kb-category: note`, per
    /// [`crate::notes::is_note`]) from the gallery/atlas/folder counts. This
    /// is markdown-aware on purpose: a plain `exclude_categories=["note"]`
    /// also hides HTML artifacts that merely use `note` as a content
    /// category, silently dropping legit content from the grid. Like
    /// `exclude_categories`, it is route-injected and applied across every OR
    /// branch (not distributed into `alternatives`).
    pub exclude_notes: bool,
    /// `NOT tag:x` — drop rows carrying ANY of these tags (the `tags`
    /// any-of test, inverted). v0.16 Q-track.
    pub exclude_tags: Vec<String>,
    /// `NOT folder:x` — drop rows under ANY of these folders
    /// (descendant-inclusive, same rule as `folder`).
    pub exclude_folders: Vec<String>,
    /// `NOT cap:x` — drop rows with ANY of these capabilities.
    pub exclude_caps: Vec<Capability>,
    /// `NOT index:true` — drop root `index.html` rows.
    pub exclude_index: bool,
    /// `OR` fan-out — additional conjuncts (the query lowered to
    /// disjunctive normal form). A row matches iff this struct's own
    /// filter fields (the base/first conjunct) OR any alternative
    /// matches. Alternatives carry ONLY filter fields — the
    /// sort/dir/group/offset/limit/projection fields are top-level-only
    /// and ignored on alternatives. Empty for any query without `OR`.
    pub alternatives: Vec<DocsQuery>,
    pub sort: SortKey,
    pub dir: SortDir,
    pub group: GroupKey,
    pub offset: u32,
    pub limit: u32,
    pub projection: Projection,
}

/// One row's verdict under the `gallery.tsx` filter pipeline. The
/// route-injected category gate (`exclude_categories`) is applied once,
/// across every OR branch; the rest of the predicate is delegated to
/// [`matches_conjunct`] over the base query and each `alternatives`
/// entry — a row matches iff the base conjunct OR any alternative
/// matches (`OR` fan-out lowered to disjunctive normal form, v0.16
/// Q-track). A query without `OR` has empty `alternatives`, so this is
/// just `matches_conjunct(r, q)`. Pulled out of [`apply_filters`] so a
/// caller can filter a *borrowed* `&[DocRow]` into `Vec<&DocRow>`
/// without cloning the whole corpus — the P1 gallery memo path.
pub fn matches(r: &DocRow, q: &DocsQuery) -> bool {
    // N-track — the `note` exclusion (and any future route-injected
    // category gate) applies to every OR branch. It is NOT a DSL atom,
    // so the lowering never distributes it into `alternatives`; gating
    // it here keeps a note hidden even when it matches an OR branch.
    if !q.exclude_categories.is_empty() {
        if let Some(cat) = r.doc.kb_category.as_deref() {
            if q.exclude_categories.iter().any(|c| c == cat) {
                return false;
            }
        }
    }
    // Editable notes (Markdown + category note) are hidden from the gallery;
    // HTML artifacts merely tagged `note` are NOT notes and stay visible.
    if q.exclude_notes && crate::notes::is_note(&r.doc.path, r.doc.kb_category.as_deref()) {
        return false;
    }
    // v0.22 — route-injected positive `kb-category` include + absolute mtime
    // window. Both are flat-param gates with no DSL atom, so (like the exclude
    // gates above) they apply ACROSS every OR branch rather than per-conjunct.
    if let Some(cat) = q.category.as_deref() {
        if r.doc.kb_category.as_deref() != Some(cat) {
            return false;
        }
    }
    if q.mtime_from.is_some() || q.mtime_to.is_some() {
        match r.doc.mtime_unix {
            Some(t) => {
                if q.mtime_from.is_some_and(|from| t < from) || q.mtime_to.is_some_and(|to| t > to)
                {
                    return false;
                }
            }
            // No mtime → can't be in any window.
            None => return false,
        }
    }
    // W2.3a — route-injected id-set membership gate (invariant #35's
    // atlas-lasso/working-set pivot). Same flat-param, every-OR-branch
    // treatment as `category`/the mtime window above; `None` or an empty
    // set means no constraint.
    if let Some(ids) = q.ids.as_ref() {
        if !ids.is_empty() && !ids.iter().any(|i| i == &r.doc.id) {
            return false;
        }
    }
    matches_conjunct(r, q) || q.alternatives.iter().any(|alt| matches_conjunct(r, alt))
}

/// One row's verdict against a SINGLE conjunct — the base [`DocsQuery`]
/// or one of its `alternatives`. The positive filters (index_only,
/// folder, tags any-of, caps all-of, since) AND the `NOT`-exclusions
/// (exclude_index / exclude_folders / exclude_tags / exclude_caps) must
/// all hold. Tag matching uses [`tags_for`] so rows with empty
/// `tags_csv` still match via their path-derived fallback.
/// `exclude_categories` is intentionally absent here — it gates every
/// branch one level up in [`matches`].
fn matches_conjunct(r: &DocRow, q: &DocsQuery) -> bool {
    if q.index_only && !is_index_page(&r.doc) {
        return false;
    }
    if q.exclude_index && is_index_page(&r.doc) {
        return false;
    }
    if let Some(f) = q.folder.as_deref() {
        let ok = if q.folder_exact {
            folder_matches_exact(&r.folder, f)
        } else {
            folder_matches(&r.folder, f)
        };
        if !ok {
            return false;
        }
    }
    if !q.exclude_folders.is_empty()
        && q.exclude_folders
            .iter()
            .any(|f| folder_matches(&r.folder, f))
    {
        return false;
    }
    // `tags_for` is the path-derived fallback; compute it once and reuse
    // it for both the any-of include test and the exclude test.
    if !q.tags.is_empty() || !q.exclude_tags.is_empty() {
        let row_tags = tags_for(&r.doc);
        if !q.tags.is_empty() && !row_tags.iter().any(|t| q.tags.iter().any(|qt| qt == t)) {
            return false;
        }
        if !q.exclude_tags.is_empty()
            && row_tags
                .iter()
                .any(|t| q.exclude_tags.iter().any(|xt| xt == t))
        {
            return false;
        }
    }
    for cap in &q.caps {
        if !cap.matches(&r.doc) {
            return false;
        }
    }
    if q.exclude_caps.iter().any(|cap| cap.matches(&r.doc)) {
        return false;
    }
    if let Some(since) = q.since_unix {
        let t = r.doc.indexed_at_unix.or(r.doc.mtime_unix);
        match t {
            Some(t) if t >= since => {}
            _ => return false,
        }
    }
    true
}

/// `gallery.tsx` filter pipeline over an OWNED `Vec<DocRow>` — a thin
/// `into_iter().filter(matches)` wrapper. NB: since P1 the docs route filters
/// borrowed `&DocRow`s via [`matches`] directly (the gallery memo holds a
/// shared `Arc<Vec<DocRow>>`), so this owned-Vec form has no production caller
/// today — it's exercised by this module's tests and kept as the documented
/// owned-pipeline convenience.
pub fn apply_filters(rows: Vec<DocRow>, q: &DocsQuery) -> Vec<DocRow> {
    rows.into_iter().filter(|r| matches(r, q)).collect()
}

/// Descendant-inclusive folder match: `"pm"` matches `"pm"`, `"pm/x"`,
/// `"pm/x/y"`. `"pm-thing"` does not.
pub fn folder_matches(row_folder: &str, filter: &str) -> bool {
    if row_folder == filter {
        return true;
    }
    if filter.is_empty() {
        return true;
    }
    if row_folder.len() <= filter.len() {
        return false;
    }
    row_folder.starts_with(filter) && row_folder.as_bytes()[filter.len()] == b'/'
}

/// Exact-folder match (v0.33 X1): `"pm"` matches only `"pm"`, never
/// `"pm/x"` or `"pm-other"`. Empty filter matches empty-folder rows only
/// (unlike the descendant form, which treats empty as "all").
pub fn folder_matches_exact(row_folder: &str, filter: &str) -> bool {
    row_folder == filter
}

/// In-place sort over an owned `&mut [DocRow]`. Like [`apply_filters`],
/// since P1 the docs route sorts borrowed refs via [`cmp_rows`] directly, so
/// this and [`sort_rows_with_group`] have no production caller today — they
/// stay as the owned-slice convenience + are exercised by this module's tests.
pub fn sort_rows(rows: &mut [DocRow], sort: SortKey, dir: SortDir) {
    sort_rows_with_group(rows, GroupKey::None, sort, dir);
}

/// S7 — like [`sort_rows`] but with an outer secondary axis. With
/// `group == Folder` the comparator becomes `(folder ASC, primary
/// sort/dir, id ASC)`, producing a flat row stream the SPA virtual
/// list can scan for folder transitions to inject section headers.
pub fn sort_rows_with_group(rows: &mut [DocRow], group: GroupKey, sort: SortKey, dir: SortDir) {
    rows.sort_by(|a, b| cmp_rows(a, b, group, sort, dir));
}

/// The `(group, sort, dir)` comparator with the `id ASC` final
/// tiebreaker, pulled out of [`sort_rows_with_group`] so a borrowed
/// `Vec<&DocRow>` can be sorted via `slice::sort_by(|a, b| cmp_rows(a, b,
/// …))` without the in-place `&mut [DocRow]` the wrapper needs — the P1
/// gallery memo path sorts references into a shared, immutable row-set.
pub fn cmp_rows(
    a: &DocRow,
    b: &DocRow,
    group: GroupKey,
    sort: SortKey,
    dir: SortDir,
) -> std::cmp::Ordering {
    if group == GroupKey::Folder {
        let folder_ord = a.folder.cmp(&b.folder);
        if folder_ord != std::cmp::Ordering::Equal {
            return folder_ord;
        }
    }
    let ord = match sort {
        SortKey::Recent => cmp_opt_time(a.doc.mtime_unix, b.doc.mtime_unix),
        SortKey::Indexed => cmp_opt_time(a.doc.indexed_at_unix, b.doc.indexed_at_unix),
        SortKey::Created => cmp_opt_time(a.doc.created_unix, b.doc.created_unix),
        SortKey::Title => a.doc.title.cmp(&b.doc.title),
        SortKey::Words => a
            .doc
            .word_count
            .unwrap_or(0)
            .cmp(&b.doc.word_count.unwrap_or(0)),
    };
    let primary = match dir {
        SortDir::Asc => ord,
        SortDir::Desc => ord.reverse(),
    };
    primary.then_with(|| a.doc.id.cmp(&b.doc.id))
}

fn cmp_opt_time(a: Option<i64>, b: Option<i64>) -> std::cmp::Ordering {
    // `None` sorts as the smallest value (matches TS `-Infinity`).
    a.unwrap_or(i64::MIN).cmp(&b.unwrap_or(i64::MIN))
}

/// `[offset .. offset + limit]`. Returns the page plus the
/// post-filter total — callers use the total for the SPA's
/// "12 of 288" badge and to decide `has_more`.
pub fn paginate<T>(rows: Vec<T>, offset: u32, limit: u32) -> (Vec<T>, u32) {
    let total = rows.len() as u32;
    let start = offset.min(total) as usize;
    let end = (offset.saturating_add(limit)).min(total) as usize;
    let page = rows.into_iter().skip(start).take(end - start).collect();
    (page, total)
}

// ---------------------------------------------------------------------
// Derivations mirroring `web/src/lib/derive.ts`. These exist server-
// side so filter/sort/aggregate produce the same answers the SPA used
// to compute client-side.
// ---------------------------------------------------------------------

const GENERIC_DIRS: &[&str] = &[
    "artifacts",
    "kb",
    "html",
    "public",
    "docs",
    "src",
    "src-tauri",
    "node_modules",
    "dist",
    "build",
];

pub fn is_index_page(d: &DocSummary) -> bool {
    if d.kb_category.as_deref() == Some("index-page") {
        return false;
    }
    let file = d.path.rsplit('/').next().unwrap_or("");
    file.eq_ignore_ascii_case("index.html")
}

/// Server tags if present; otherwise path-derived (last two non-
/// generic directory segments, slugified, dedup'd, more-specific
/// first).
pub fn tags_for(d: &DocSummary) -> Vec<String> {
    if !d.tags.is_empty() {
        return d.tags.clone();
    }
    path_to_tags(&d.path)
}

pub fn path_to_tags(path: &str) -> Vec<String> {
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() <= 1 {
        return Vec::new();
    }
    let dirs = &parts[..parts.len() - 1];
    let mut out: Vec<String> = Vec::with_capacity(2);
    for seg in dirs.iter().rev() {
        if out.len() >= 2 {
            break;
        }
        let slug: String = seg
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect();
        let slug = collapse_dashes(&slug);
        if slug.is_empty() || slug == "-" {
            continue;
        }
        if GENERIC_DIRS.iter().any(|g| *g == slug) {
            continue;
        }
        if out.iter().any(|existing| existing == &slug) {
            continue;
        }
        out.push(slug);
    }
    out
}

fn collapse_dashes(s: &str) -> String {
    // Mirrors `replace(/[^a-z0-9-]+/g, "-")` collapse behaviour: runs
    // of `-` (whether already present or produced by the char-map
    // above) flatten to a single `-`; leading/trailing dashes are
    // trimmed so a segment like `"---foo---"` becomes `"foo"`.
    let mut out = String::with_capacity(s.len());
    let mut last_dash = false;
    for c in s.chars() {
        if c == '-' {
            if !last_dash {
                out.push('-');
            }
            last_dash = true;
        } else {
            out.push(c);
            last_dash = false;
        }
    }
    out.trim_matches('-').to_string()
}

// ---------------------------------------------------------------------
// Facet aggregators. Both walk a pre-decorated row slice once. Caller
// passes the full corpus (post-S3, no cap).
// ---------------------------------------------------------------------

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FolderNode {
    pub path: String,
    pub count: u32,
    pub children: Vec<FolderNode>,
}

#[derive(Debug, Clone, Default)]
pub struct TagBucket {
    pub name: String,
    pub count: u32,
}

/// Descendant-inclusive folder counts → tree. Mirrors the existing
/// `routes::folders::list` algorithm.
pub fn aggregate_folders(rows: &[DocRow]) -> Vec<FolderNode> {
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    for r in rows {
        if r.folder.is_empty() {
            *counts.entry(String::new()).or_insert(0) += 1;
            continue;
        }
        let mut acc = String::new();
        for seg in r.folder.split('/') {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(seg);
            *counts.entry(acc.clone()).or_insert(0) += 1;
        }
    }
    let mut top: Vec<FolderNode> = counts
        .keys()
        .filter(|k| !k.is_empty() && !k.contains('/'))
        .map(|k| build_subtree(k, &counts))
        .collect();
    top.sort_by(|a, b| a.path.cmp(&b.path));
    top
}

fn build_subtree(prefix: &str, counts: &BTreeMap<String, u32>) -> FolderNode {
    let count = *counts.get(prefix).unwrap_or(&0);
    let depth = prefix.matches('/').count();
    let prefix_with_sep = format!("{prefix}/");
    let mut children: Vec<FolderNode> = counts
        .keys()
        .filter(|k| k.starts_with(&prefix_with_sep) && k.matches('/').count() == depth + 1)
        .map(|k| build_subtree(k, counts))
        .collect();
    children.sort_by(|a, b| a.path.cmp(&b.path));
    FolderNode {
        path: prefix.to_string(),
        count,
        children,
    }
}

/// Tag frequencies, sorted by `(count desc, name asc)` for stable
/// rendering. Caller truncates / colour-seeds. Mirrors the
/// `routes::tags::list` body up to the truncate step.
pub fn aggregate_tags(rows: &[DocRow]) -> Vec<TagBucket> {
    let mut counts: HashMap<String, u32> = HashMap::new();
    for r in rows {
        for t in &r.doc.tags {
            *counts.entry(t.to_string()).or_insert(0) += 1;
        }
    }
    let mut buckets: Vec<TagBucket> = counts
        .into_iter()
        .map(|(name, count)| TagBucket { name, count })
        .collect();
    buckets.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    buckets
}

/// One distinct value of a metadata field plus its document count.
/// Q-track — backs the search `/facets` endpoint (category/status/
/// severity dropdowns).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct FacetBucket {
    pub value: String,
    pub count: u32,
}

/// Aggregate distinct non-empty values of an optional `DocSummary` string
/// field, sorted by `(count desc, value asc)` — the same stable order as
/// [`aggregate_tags`].
fn aggregate_field(
    rows: &[DocRow],
    field: impl Fn(&DocSummary) -> Option<&str>,
) -> Vec<FacetBucket> {
    let mut counts: HashMap<String, u32> = HashMap::new();
    for r in rows {
        if let Some(v) = field(&r.doc).filter(|s| !s.is_empty()) {
            *counts.entry(v.to_string()).or_insert(0) += 1;
        }
    }
    let mut buckets: Vec<FacetBucket> = counts
        .into_iter()
        .map(|(value, count)| FacetBucket { value, count })
        .collect();
    buckets.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
    buckets
}

/// Q-track — distinct `kb-category` values + counts.
pub fn aggregate_categories(rows: &[DocRow]) -> Vec<FacetBucket> {
    aggregate_field(rows, |d| d.kb_category.as_deref())
}

/// Q-track — distinct `kb-status` values + counts.
pub fn aggregate_statuses(rows: &[DocRow]) -> Vec<FacetBucket> {
    aggregate_field(rows, |d| d.kb_status.as_deref())
}

/// Q-track — distinct `kb-severity` values + counts.
pub fn aggregate_severities(rows: &[DocRow]) -> Vec<FacetBucket> {
    aggregate_field(rows, |d| d.kb_severity.as_deref())
}

// ---------------------------------------------------------------------
// Tests — parity against the TS reference cases.
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(id: &str) -> DocSummary {
        DocSummary {
            id: id.into(),
            title: id.to_uppercase(),
            path: format!("/root/{id}.html"),
            ..Default::default()
        }
    }

    fn row(id: &str, folder: &str) -> DocRow {
        DocRow {
            doc: doc(id),
            folder: folder.into(),
        }
    }

    fn rows() -> Vec<DocRow> {
        vec![
            row("a", "pm"),
            row("b", "pm/initiatives"),
            row("c", "pm-other"),
            row("d", ""),
            row("e", "ideas/passwordless"),
        ]
    }

    #[test]
    fn folder_matches_is_descendant_inclusive() {
        assert!(folder_matches("pm", "pm"));
        assert!(folder_matches("pm/x", "pm"));
        assert!(folder_matches("pm/x/y", "pm"));
        assert!(!folder_matches("pm-other", "pm"));
        assert!(!folder_matches("p", "pm"));
        assert!(folder_matches("anything", "")); // empty filter is "all"
    }

    #[test]
    fn folder_matches_exact_is_equality_only() {
        assert!(folder_matches_exact("pm", "pm"));
        assert!(!folder_matches_exact("pm/x", "pm"));
        assert!(!folder_matches_exact("pm/x/y", "pm"));
        assert!(!folder_matches_exact("pm-other", "pm"));
        assert!(!folder_matches_exact("p", "pm"));
        assert!(folder_matches_exact("", ""));
        assert!(!folder_matches_exact("pm", ""));
        assert!(!folder_matches_exact("", "pm"));
    }

    #[test]
    fn filter_folder_picks_descendants_and_excludes_sibling_names() {
        let q = DocsQuery {
            folder: Some("pm".into()),
            ..Default::default()
        };
        let kept = apply_filters(rows(), &q);
        let ids: Vec<&str> = kept.iter().map(|r| r.doc.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
    }

    #[test]
    fn filter_folder_exact_excludes_descendants() {
        let q = DocsQuery {
            folder: Some("pm".into()),
            folder_exact: true,
            ..Default::default()
        };
        let kept = apply_filters(rows(), &q);
        let ids: Vec<&str> = kept.iter().map(|r| r.doc.id.as_str()).collect();
        // Only "a" lives in folder "pm"; "b" is "pm/initiatives".
        assert_eq!(ids, ["a"]);
    }

    #[test]
    fn filter_folder_exact_default_path_unchanged_when_flag_false() {
        let q = DocsQuery {
            folder: Some("pm".into()),
            folder_exact: false,
            ..Default::default()
        };
        let kept = apply_filters(rows(), &q);
        let ids: Vec<&str> = kept.iter().map(|r| r.doc.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
    }

    #[test]
    fn excludes_category_note() {
        let mut rs = rows();
        rs[0].doc.kb_category = Some("note".into());
        rs[1].doc.kb_category = Some("research".into());
        let q = DocsQuery {
            exclude_categories: vec!["note".into()],
            ..Default::default()
        };
        let kept = apply_filters(rs, &q);
        let ids: Vec<&str> = kept.iter().map(|r| r.doc.id.as_str()).collect();
        // "a" (note) dropped; everything else (incl. uncategorised) kept.
        assert!(!ids.contains(&"a"));
        assert!(ids.contains(&"b"));
        assert!(ids.contains(&"d"));
    }

    // --- v0.22: positive category include + mtime window ----------------

    // invariant:35 category-gate
    #[test]
    fn category_include_keeps_only_exact_matches() {
        let mut rs = rows();
        rs[0].doc.kb_category = Some("research".into()); // a → kept
        rs[1].doc.kb_category = Some("review".into()); // b → dropped
        rs[2].doc.kb_category = Some("research".into()); // c → kept
        rs[3].doc.kb_category = None; // d → dropped (no category)
        let q = DocsQuery {
            category: Some("research".into()),
            ..Default::default()
        };
        let kept = apply_filters(rs, &q);
        let ids: Vec<&str> = kept.iter().map(|r| r.doc.id.as_str()).collect();
        assert_eq!(ids, ["a", "c"]);
    }

    #[test]
    fn category_include_gates_across_alternatives() {
        // The positive category gate constrains every OR branch (it is not
        // distributed into `alternatives`): a row matching the alt is still
        // dropped if its category differs.
        let mut rs = rows();
        rs[0].doc.tags = vec!["atlas".into()];
        rs[0].doc.kb_category = Some("research".into()); // a: matches alt + category
        rs[1].doc.tags = vec!["atlas".into()];
        rs[1].doc.kb_category = Some("review".into()); // b: matches alt, wrong category
        let q = DocsQuery {
            tags: vec!["rust".into()],
            alternatives: vec![DocsQuery {
                tags: vec!["atlas".into()],
                ..Default::default()
            }],
            category: Some("research".into()),
            ..Default::default()
        };
        let kept = apply_filters(rs, &q);
        let ids: Vec<&str> = kept.iter().map(|r| r.doc.id.as_str()).collect();
        assert!(ids.contains(&"a"));
        assert!(!ids.contains(&"b"), "wrong-category alt match is dropped");
    }

    // invariant:35 mtime-window
    #[test]
    fn mtime_window_bounds_on_mtime_only_and_drops_undated() {
        let mut rs = rows();
        rs[0].doc.mtime_unix = Some(1_000); // a → below from
        rs[1].doc.mtime_unix = Some(1_500); // b → in window
        rs[2].doc.mtime_unix = Some(2_000); // c → in window (upper inclusive)
        rs[3].doc.mtime_unix = Some(2_500); // d → above to
        rs[4].doc.mtime_unix = None; // e → no mtime, dropped
                                     // A window must filter on mtime even when indexed_at would pass.
        rs[4].doc.indexed_at_unix = Some(1_700);
        let q = DocsQuery {
            mtime_from: Some(1_500),
            mtime_to: Some(2_000),
            ..Default::default()
        };
        let kept = apply_filters(rs, &q);
        let ids: Vec<&str> = kept.iter().map(|r| r.doc.id.as_str()).collect();
        assert_eq!(ids, ["b", "c"]);
    }

    #[test]
    fn mtime_window_open_ended_lower_bound() {
        let mut rs = rows();
        rs[0].doc.mtime_unix = Some(100);
        rs[1].doc.mtime_unix = Some(300);
        rs[2].doc.mtime_unix = None; // dropped: a bound is set
        let q = DocsQuery {
            mtime_from: Some(200),
            ..Default::default()
        };
        let kept = apply_filters(rs, &q);
        let ids: Vec<&str> = kept.iter().map(|r| r.doc.id.as_str()).collect();
        assert_eq!(ids, ["b"]);
    }

    // --- W2.3a: id-set membership gate (atlas lasso / working-set) ------

    // invariant:35 ids-gate
    #[test]
    fn ids_gate_keeps_only_the_listed_ids() {
        let rs = rows();
        let q = DocsQuery {
            ids: Some(vec!["a".into(), "c".into(), "zzz".into()]),
            ..Default::default()
        };
        let kept = apply_filters(rs, &q);
        let ids: Vec<&str> = kept.iter().map(|r| r.doc.id.as_str()).collect();
        assert_eq!(ids, ["a", "c"], "unknown id in the set matches nothing");
    }

    #[test]
    fn ids_gate_absent_or_empty_means_no_constraint() {
        let rs = rows();
        // Owned, not borrowed — `rs` is moved into `apply_filters` twice below.
        let all_ids: Vec<String> = rs.iter().map(|r| r.doc.id.clone()).collect();

        let none_kept = apply_filters(rs.clone(), &DocsQuery::default());
        assert_eq!(
            none_kept
                .iter()
                .map(|r| r.doc.id.clone())
                .collect::<Vec<_>>(),
            all_ids,
            "ids: None must not filter anything"
        );

        let empty_kept = apply_filters(
            rs,
            &DocsQuery {
                ids: Some(Vec::new()),
                ..Default::default()
            },
        );
        assert_eq!(
            empty_kept
                .iter()
                .map(|r| r.doc.id.clone())
                .collect::<Vec<_>>(),
            all_ids,
            "ids: Some(empty) must not filter anything either"
        );
    }

    #[test]
    fn ids_gate_applies_across_alternatives() {
        // The id-set gate constrains every OR branch (it is not distributed
        // into `alternatives`): a row matching the alt is still dropped if
        // its id isn't in the set.
        let mut rs = rows();
        rs[0].doc.tags = vec!["atlas".into()]; // a: matches alt, IS in the set
        rs[1].doc.tags = vec!["atlas".into()]; // b: matches alt, NOT in the set
        let q = DocsQuery {
            tags: vec!["rust".into()],
            alternatives: vec![DocsQuery {
                tags: vec!["atlas".into()],
                ..Default::default()
            }],
            ids: Some(vec!["a".into()]),
            ..Default::default()
        };
        let kept = apply_filters(rs, &q);
        let ids: Vec<&str> = kept.iter().map(|r| r.doc.id.as_str()).collect();
        assert_eq!(ids, ["a"], "wrong-id alt match is dropped");
    }

    // --- v0.16 Q-track: NOT exclude + OR alternatives -------------------

    #[test]
    fn exclude_tags_drops_rows_carrying_the_tag() {
        let mut rs = rows();
        rs[0].doc.tags = vec!["rust".into(), "draft".into()]; // a → dropped
        rs[1].doc.tags = vec!["rust".into()]; // b → kept
        rs[2].doc.tags = vec!["draft".into()]; // c → dropped
        let q = DocsQuery {
            exclude_tags: vec!["draft".into()],
            ..Default::default()
        };
        let kept = apply_filters(rs, &q);
        let ids: Vec<&str> = kept.iter().map(|r| r.doc.id.as_str()).collect();
        assert!(!ids.contains(&"a"));
        assert!(!ids.contains(&"c"));
        assert!(ids.contains(&"b"));
    }

    #[test]
    fn exclude_folders_drops_descendants() {
        // NOT folder:pm drops pm and pm/initiatives, keeps pm-other + root.
        let q = DocsQuery {
            exclude_folders: vec!["pm".into()],
            ..Default::default()
        };
        let kept = apply_filters(rows(), &q);
        let ids: Vec<&str> = kept.iter().map(|r| r.doc.id.as_str()).collect();
        assert!(!ids.contains(&"a")); // pm
        assert!(!ids.contains(&"b")); // pm/initiatives
        assert!(ids.contains(&"c")); // pm-other (sibling name, not a descendant)
        assert!(ids.contains(&"d")); // root
    }

    #[test]
    fn exclude_index_drops_root_index_html() {
        let mut rs = vec![
            DocRow {
                doc: {
                    let mut d = doc("a");
                    d.path = "/root/index.html".into();
                    d
                },
                folder: "".into(),
            },
            DocRow {
                doc: {
                    let mut d = doc("b");
                    d.path = "/root/page.html".into();
                    d
                },
                folder: "".into(),
            },
        ];
        rs.iter_mut().for_each(|r| r.doc.title = r.doc.id.clone());
        let q = DocsQuery {
            exclude_index: true,
            ..Default::default()
        };
        let kept = apply_filters(rs, &q);
        let ids: Vec<&str> = kept.iter().map(|r| r.doc.id.as_str()).collect();
        assert_eq!(ids, ["b"]);
    }

    #[test]
    fn alternatives_match_as_union() {
        // base = tag:rust, alternative = tag:atlas → a OR b kept, c dropped.
        let mut rs = rows();
        rs[0].doc.tags = vec!["rust".into()];
        rs[1].doc.tags = vec!["atlas".into()];
        rs[2].doc.tags = vec!["sql".into()];
        let q = DocsQuery {
            tags: vec!["rust".into()],
            alternatives: vec![DocsQuery {
                tags: vec!["atlas".into()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let kept = apply_filters(rs, &q);
        let ids: Vec<&str> = kept.iter().map(|r| r.doc.id.as_str()).collect();
        assert!(ids.contains(&"a"));
        assert!(ids.contains(&"b"));
        assert!(!ids.contains(&"c"));
    }

    #[test]
    fn exclude_categories_gates_across_alternatives() {
        // A row that matches an OR branch is STILL dropped if its category
        // is route-excluded — the gate is not distributed into alternatives.
        let mut rs = rows();
        rs[0].doc.tags = vec!["atlas".into()];
        rs[0].doc.kb_category = Some("note".into()); // a: matches alt, but a note
        rs[1].doc.tags = vec!["atlas".into()];
        rs[1].doc.kb_category = Some("research".into()); // b: matches alt, kept
        let q = DocsQuery {
            tags: vec!["rust".into()],
            alternatives: vec![DocsQuery {
                tags: vec!["atlas".into()],
                ..Default::default()
            }],
            exclude_categories: vec!["note".into()],
            ..Default::default()
        };
        let kept = apply_filters(rs, &q);
        let ids: Vec<&str> = kept.iter().map(|r| r.doc.id.as_str()).collect();
        assert!(
            !ids.contains(&"a"),
            "note dropped even though it matches the OR branch"
        );
        assert!(ids.contains(&"b"));
    }

    #[test]
    fn filter_tags_any_of() {
        let mut rs = rows();
        rs[0].doc.tags = vec!["rust".into(), "atlas".into()];
        rs[1].doc.tags = vec!["atlas".into()];
        rs[2].doc.tags = vec!["sql".into()];
        let q = DocsQuery {
            tags: vec!["rust".into(), "sql".into()],
            ..Default::default()
        };
        let kept = apply_filters(rs, &q);
        let ids: Vec<&str> = kept.iter().map(|r| r.doc.id.as_str()).collect();
        assert_eq!(ids, ["a", "c"]);
    }

    #[test]
    fn filter_caps_all_of() {
        let mut rs = rows();
        rs[0].doc.svg_count = Some(2);
        rs[0].doc.has_canvas = Some(true);
        rs[1].doc.has_canvas = Some(true);
        rs[2].doc.svg_count = Some(1);
        let q = DocsQuery {
            caps: vec![Capability::Svg, Capability::Interactive],
            ..Default::default()
        };
        let kept = apply_filters(rs, &q);
        let ids: Vec<&str> = kept.iter().map(|r| r.doc.id.as_str()).collect();
        assert_eq!(ids, ["a"]); // only the row with BOTH svg and interactive
    }

    // invariant:35 mtime-window
    #[test]
    fn filter_since_uses_indexed_then_mtime() {
        let mut rs = rows();
        rs[0].doc.indexed_at_unix = Some(1_000);
        rs[1].doc.indexed_at_unix = Some(2_000);
        rs[2].doc.mtime_unix = Some(1_500); // no indexed; falls back
        rs[3].doc.indexed_at_unix = None;
        rs[3].doc.mtime_unix = None; // no time at all → drops
        let q = DocsQuery {
            since_unix: Some(1_500),
            ..Default::default()
        };
        let kept = apply_filters(rs, &q);
        let ids: Vec<&str> = kept.iter().map(|r| r.doc.id.as_str()).collect();
        assert_eq!(ids, ["b", "c"]);
    }

    #[test]
    fn filter_index_only_keeps_root_index_html_and_drops_index_page_category() {
        let mut rs = vec![
            DocRow {
                doc: {
                    let mut d = doc("a");
                    d.path = "/root/index.html".into();
                    d
                },
                folder: "".into(),
            },
            DocRow {
                doc: {
                    let mut d = doc("b");
                    d.path = "/root/x/index.html".into();
                    d.kb_category = Some("index-page".into()); // generated index — skip
                    d
                },
                folder: "x".into(),
            },
            DocRow {
                doc: {
                    let mut d = doc("c");
                    d.path = "/root/x/y.html".into();
                    d
                },
                folder: "x".into(),
            },
        ];
        rs.iter_mut().for_each(|r| r.doc.title = r.doc.id.clone()); // distinct titles
        let q = DocsQuery {
            index_only: true,
            ..Default::default()
        };
        let kept = apply_filters(rs, &q);
        let ids: Vec<&str> = kept.iter().map(|r| r.doc.id.as_str()).collect();
        assert_eq!(ids, ["a"]);
    }

    #[test]
    fn sort_recent_desc_is_default_and_handles_missing_times() {
        let mut rs = rows();
        rs[0].doc.mtime_unix = Some(100);
        rs[1].doc.mtime_unix = Some(300);
        rs[2].doc.mtime_unix = Some(200);
        rs[3].doc.mtime_unix = None;
        rs[4].doc.mtime_unix = Some(50);
        sort_rows(&mut rs, SortKey::Recent, SortDir::Desc);
        let ids: Vec<&str> = rs.iter().map(|r| r.doc.id.as_str()).collect();
        // 300, 200, 100, 50, None
        assert_eq!(ids, ["b", "c", "a", "e", "d"]);
    }

    #[test]
    fn sort_title_asc_is_title_default() {
        assert_eq!(SortKey::Title.default_dir(), SortDir::Asc);
        let mut rs = vec![row("c", ""), row("a", ""), row("b", "")];
        // title is uppercase of id, so titles are "A", "B", "C"
        sort_rows(&mut rs, SortKey::Title, SortDir::Asc);
        let ids: Vec<&str> = rs.iter().map(|r| r.doc.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
    }

    #[test]
    fn sort_words_desc() {
        let mut rs = rows();
        rs[0].doc.word_count = Some(100);
        rs[1].doc.word_count = Some(500);
        rs[2].doc.word_count = None; // treated as 0
        rs[3].doc.word_count = Some(50);
        rs[4].doc.word_count = Some(1000);
        sort_rows(&mut rs, SortKey::Words, SortDir::Desc);
        let ids: Vec<&str> = rs.iter().map(|r| r.doc.id.as_str()).collect();
        assert_eq!(ids, ["e", "b", "a", "d", "c"]);
    }

    #[test]
    fn sort_rows_with_group_folder_groups_folder_then_primary() {
        // Two folders, three docs each, distinct mtimes so the
        // primary sort would interleave them in the absence of the
        // outer folder axis.
        let mut rs: Vec<DocRow> = vec![
            ("pm", "a", 100),
            ("pm", "b", 200),
            ("ideas", "c", 150),
            ("ideas", "d", 250),
            ("ideas", "e", 50),
            ("pm", "f", 75),
        ]
        .into_iter()
        .map(|(folder, id, mtime)| {
            let mut d = doc(id);
            d.mtime_unix = Some(mtime);
            DocRow {
                doc: d,
                folder: folder.into(),
            }
        })
        .collect();
        sort_rows_with_group(&mut rs, GroupKey::Folder, SortKey::Recent, SortDir::Desc);
        let pairs: Vec<(&str, &str)> = rs
            .iter()
            .map(|r| (r.folder.as_str(), r.doc.id.as_str()))
            .collect();
        // ideas (alphabetical first) by mtime DESC, then pm by mtime DESC.
        assert_eq!(
            pairs,
            vec![
                ("ideas", "d"),
                ("ideas", "c"),
                ("ideas", "e"),
                ("pm", "b"),
                ("pm", "a"),
                ("pm", "f"),
            ],
        );
    }

    #[test]
    fn paginate_returns_slice_and_total() {
        let xs = (0..10).collect::<Vec<_>>();
        let (page, total) = paginate(xs.clone(), 3, 4);
        assert_eq!(page, vec![3, 4, 5, 6]);
        assert_eq!(total, 10);
    }

    #[test]
    fn paginate_handles_offset_past_end() {
        let xs = (0..5).collect::<Vec<_>>();
        let (page, total) = paginate(xs, 10, 4);
        assert!(page.is_empty());
        assert_eq!(total, 5);
    }

    #[test]
    fn paginate_clips_to_available() {
        let xs = (0..5).collect::<Vec<_>>();
        let (page, total) = paginate(xs, 3, 10);
        assert_eq!(page, vec![3, 4]);
        assert_eq!(total, 5);
    }

    #[test]
    fn path_to_tags_drops_generic_dirs_and_dedupes() {
        assert_eq!(
            path_to_tags("/root/incidents/active/INC-42.html"),
            vec!["active".to_string(), "incidents".to_string()],
        );
        // `docs/` is generic and gets filtered, so the walk back picks
        // up `aws` then `root` (parity with TS pathToTags, which takes
        // up to 2 non-generic segments regardless of how deep).
        assert_eq!(
            path_to_tags("/root/docs/aws/setup.html"),
            vec!["aws".to_string(), "root".to_string()],
        );
        // single segment (no parent dirs): no tags
        assert!(path_to_tags("foo.html").is_empty());
        assert!(path_to_tags("/foo.html").is_empty());
        // non-alnum chars become dashes and collapse; walk picks up
        // the more-specific segment first, then root.
        assert_eq!(
            path_to_tags("/root/some folder/x.html"),
            vec!["some-folder".to_string(), "root".to_string()],
        );
    }

    #[test]
    fn tags_for_prefers_server_tags_else_path_derives() {
        let mut d = doc("a");
        d.path = "/root/incidents/active/INC.html".into();
        d.tags = vec!["explicit".into()];
        assert_eq!(tags_for(&d), vec!["explicit".to_string()]);
        d.tags.clear();
        assert_eq!(
            tags_for(&d),
            vec!["active".to_string(), "incidents".to_string()],
        );
    }

    #[test]
    fn is_index_page_true_only_for_real_index_html() {
        let mut d = doc("a");
        d.path = "/root/index.html".into();
        assert!(is_index_page(&d));
        d.path = "/root/x/index.html".into();
        assert!(is_index_page(&d));
        d.kb_category = Some("index-page".into());
        assert!(!is_index_page(&d)); // generated index — excluded
        d.kb_category = None;
        d.path = "/root/foo.html".into();
        assert!(!is_index_page(&d));
    }

    #[test]
    fn aggregate_folders_produces_tree_with_descendant_inclusive_counts() {
        let rs = vec![
            row("a", "pm"),
            row("b", "pm"),
            row("c", "pm/initiatives"),
            row("d", "ideas/x"),
            row("e", ""), // root-level doc
        ];
        let tree = aggregate_folders(&rs);
        // top-level: ideas (1) + pm (3)
        let names: Vec<&str> = tree.iter().map(|n| n.path.as_str()).collect();
        assert_eq!(names, ["ideas", "pm"]);
        let pm = tree.iter().find(|n| n.path == "pm").unwrap();
        assert_eq!(pm.count, 3); // a + b + c
        assert_eq!(pm.children.len(), 1);
        assert_eq!(pm.children[0].path, "pm/initiatives");
        assert_eq!(pm.children[0].count, 1);
    }

    #[test]
    fn aggregate_tags_orders_by_count_desc_then_name_asc() {
        let mut rs = vec![row("a", ""), row("b", ""), row("c", ""), row("d", "")];
        rs[0].doc.tags = vec!["rust".into(), "atlas".into()];
        rs[1].doc.tags = vec!["rust".into()];
        rs[2].doc.tags = vec!["atlas".into()];
        rs[3].doc.tags = vec!["sql".into()];
        let buckets = aggregate_tags(&rs);
        let summary: Vec<(String, u32)> = buckets.into_iter().map(|t| (t.name, t.count)).collect();
        assert_eq!(
            summary,
            vec![("atlas".into(), 2), ("rust".into(), 2), ("sql".into(), 1),],
        );
    }

    #[test]
    fn aggregate_categories_counts_skips_empty_and_orders() {
        let mut rs = vec![row("a", ""), row("b", ""), row("c", ""), row("d", "")];
        rs[0].doc.kb_category = Some("incident".into());
        rs[1].doc.kb_category = Some("incident".into());
        rs[2].doc.kb_category = Some("rfc".into());
        rs[3].doc.kb_category = Some(String::new()); // empty → skipped
        let got: Vec<(String, u32)> = aggregate_categories(&rs)
            .into_iter()
            .map(|b| (b.value, b.count))
            .collect();
        assert_eq!(got, vec![("incident".into(), 2), ("rfc".into(), 1)]);
        // None-valued fields produce no buckets.
        assert!(aggregate_statuses(&rs).is_empty());
    }
}
