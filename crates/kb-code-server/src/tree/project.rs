//! V71-F1 — kbc-tree/1's PURE engine: the wire types, the four projections
//! and the one flattener. Split out of [`super`] (which keeps the route,
//! its params and the `RouteContract`) for one reason: everything here is a
//! total function of its arguments — no axum, no `Store`, no `AppState`, no
//! filesystem — so it can be exercised without the 8 GB test-binary link
//! this crate's lib suite needs. The route is a thin caller; the shapes and
//! the arithmetic live here.
//!
//! Read [`super`]'s module doc first: the projection model, the honesty
//! rule (`unplaced` is not optional) and the cost model are stated there.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::Serialize;

use crate::search::grammar::Diagnostic;
use crate::search::matcher::{HaystackKind, NameMatcher};

use super::roles;

pub const TREE_SCHEMA: &str = "kbc-tree/1";

/// The four projections this milestone ships, in menu order (`t`/`T` cycle
/// this list). CLOSED — an unknown `view=` is a 400 naming the vocabulary,
/// never a silent fallback to `physical` (a silent fallback is how a typo
/// becomes "the tree is wrong and nobody knows why").
pub const VIEWS: [&str; 4] = ["physical", "role", "namespace", "change"];

/// The decoration lanes a caller may ask for.
pub const LANES: [&str; 6] = ["git", "review", "findings", "annot", "todo", "bookmark"];

/// Three slots, a hard budget — the evidence report's §2.3 ("a hard budget
/// — the VS Code decorations API caps at one badge for the same reason").
/// A fourth lane is DROPPED and named in `notes`.
pub const MAX_DECORATION_LANES: usize = 3;

/// Rows one response may carry, before `truncated` fires.
pub const MAX_TREE_ROWS: usize = 10_000;
pub const DEFAULT_LIMIT: usize = 2_000;

/// Unplaced files listed in full before the bucket itself is capped (the
/// COUNT is always exact — see [`TreeOut::unplaced_total`]).
pub const MAX_UNPLACED_ROWS: usize = 200;

/// The two filter modes, VS Code's exact duality (§2.4 of the evidence
/// report): `filter` prunes non-matches but keeps a match's ancestry;
/// `highlight` keeps every row and badges the ancestors with a match count.
pub const MODE_FILTER: &str = "filter";
pub const MODE_HIGHLIGHT: &str = "highlight";

// ── the wire ──────────────────────────────────────────────────────────────

/// What a row IS. `dir` and `file` are physical; `group` is an inferred
/// bucket (a role, a namespace segment, a change status); `entity` is a
/// constant from the V71-G0 index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RowKind {
    Dir,
    File,
    Group,
    Entity,
}

/// The decoration payload. Uniform across files and groups: a file's
/// `annot` is its own open-annotation count, a group's is the subtree sum.
/// Every field is `Option` and OMITTED when its lane was not requested —
/// absence means "not asked for", and a lane that WAS asked for reports 0
/// explicitly, so a client can tell the two apart.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Facts {
    /// A file's git status vs the base — the porcelain letter(s)
    /// (`A`/`M`/`D`/`R`/`?`). Files only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<String>,
    /// A group's changed-descendant count. Groups only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_changed: Option<u32>,
    /// `viewed` / `unviewed` for a file that is part of the named review;
    /// omitted for a file that is not in it at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_viewed: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_total: Option<u32>,
    /// Worst finding severity present (`blocker` > `concern` > `ok`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub findings: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annot: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub todo: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bookmark: Option<u32>,
}

impl Facts {
    fn fold(&mut self, child: &Facts) {
        if let Some(n) = child.git_changed {
            *self.git_changed.get_or_insert(0) += n;
        }
        if child.git.is_some() {
            *self.git_changed.get_or_insert(0) += 1;
        }
        if let Some(v) = child.review_viewed {
            *self.review_viewed.get_or_insert(0) += v;
        }
        if let Some(t) = child.review_total {
            *self.review_total.get_or_insert(0) += t;
        }
        if let Some(s) = &child.review {
            *self.review_total.get_or_insert(0) += 1;
            if s == "viewed" {
                *self.review_viewed.get_or_insert(0) += 1;
            }
        }
        if let Some(sev) = &child.findings {
            let better = match &self.findings {
                None => true,
                Some(cur) => severity_rank(sev) < severity_rank(cur),
            };
            if better {
                self.findings = Some(sev.clone());
            }
        }
        for (dst, src) in [
            (&mut self.annot, child.annot),
            (&mut self.todo, child.todo),
            (&mut self.bookmark, child.bookmark),
        ] {
            if let Some(n) = src {
                *dst.get_or_insert(0) += n;
            }
        }
    }
}

pub fn severity_rank(s: &str) -> u8 {
    match s {
        "blocker" => 0,
        "concern" => 1,
        "ok" => 2,
        _ => 3,
    }
}

/// One flattened row. `id` is stable WITHIN one response (it is the row's
/// projection-relative key) and is what `expand=` names — deliberately not
/// a cross-request identity, which the field's doc says rather than
/// implying the stronger promise (V71-D1's `hit_id` made the same call).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TreeRow {
    pub id: String,
    pub kind: RowKind,
    pub label: String,
    pub depth: u32,
    /// The repo-relative path, for a row that HAS one (a file always; a
    /// physical directory too; a role/namespace group never).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The constant path, for an `entity` row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ent: Option<String>,
    /// `exact` | `likely` | `candidate` — present ONLY on an INFERRED row.
    /// A physical directory has no trust class because it is not a claim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust: Option<String>,
    /// Direct children this row has in the projection (0 for a file).
    pub children: u32,
    /// Files in this row's whole subtree (1 for a file).
    pub files: u32,
    /// `true` when this row has children the response did NOT include
    /// (unexpanded, or past the depth budget) — the per-row half of
    /// `truncated`.
    #[serde(skip_serializing_if = "is_false")]
    pub has_more: bool,
    /// UTF-16 `[start, end)` ranges into `label`, from `search::matcher`
    /// (crate invariant 16b: ONE matcher, and its indices are converted to
    /// UTF-16 once, server-side, because the only consumer is JavaScript).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub match_ranges: Vec<[u32; 2]>,
    /// Matches in this row's subtree — the ancestor badge VS Code's
    /// highlight mode shows. 0 on a non-matching leaf.
    #[serde(skip_serializing_if = "is_zero")]
    pub match_count: u32,
    #[serde(skip_serializing_if = "Facts::is_empty")]
    pub facts: Facts,
}

impl Facts {
    fn is_empty(&self) -> bool {
        *self == Facts::default()
    }
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// An in-band cap report. There is no way for this wire to truncate
/// silently: every field a cap touched names the cap that touched it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Truncation {
    /// `"limit"` | `"max_rows"` | `"unplaced"` | `"entity_defs"`
    pub by: String,
    pub returned: u32,
    /// The true total, when it is known — `null` when computing it would
    /// have cost the very walk the cap avoided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u32>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TreeCounts {
    /// Files the projection considered (post-scope, pre-filter).
    pub files: u32,
    /// Rows the projection produced before the row cap.
    pub rows: u32,
    /// Files the filter matched — `null` when there was no filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TreeOut {
    pub schema: &'static str,
    pub repo: String,
    pub view: String,
    pub views_available: Vec<&'static str>,
    pub generation: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    pub depth: u32,
    /// The scope as it actually RAN — kbc-scope/1's canonical rendering,
    /// or `null` when none was applied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// `false` when a scope was SUPPLIED and refused; the tree below is
    /// then the UNSCOPED one and `notes` says why (never a silently
    /// different set, never an empty tree).
    pub scope_applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    pub mode: String,
    pub decorate: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    pub counts: TreeCounts,
    pub rows: Vec<TreeRow>,
    /// Files this projection could not place — the non-negotiable bucket.
    pub unplaced: Vec<TreeRow>,
    /// The EXACT number of unplaced files, even when `unplaced` itself was
    /// capped at [`MAX_UNPLACED_ROWS`].
    pub unplaced_total: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<Truncation>,
    /// Every degrade, cap and refusal, in prose, for a surface to render
    /// verbatim. Never empty when something was dropped.
    pub notes: Vec<String>,
    pub diagnostics: Vec<Diagnostic>,
}

// ── the intermediate tree ────────────────────────────────────────────────

/// What a projection produces. Flattening (depth budget, `expand=`,
/// filtering, decoration folding) is shared by all four, so a projection
/// only has to answer "what is the shape".
#[derive(Debug, Clone)]
pub struct Node {
    pub key: String,
    pub kind: RowKind,
    pub label: String,
    pub path: Option<String>,
    pub ent: Option<String>,
    pub trust: Option<String>,
    pub children: Vec<Node>,
}

impl Node {
    fn group(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            kind: RowKind::Group,
            label: label.into(),
            path: None,
            ent: None,
            trust: None,
            children: Vec::new(),
        }
    }

    fn file(path: &str, label: &str) -> Self {
        Self {
            key: format!("f:{path}"),
            kind: RowKind::File,
            label: label.to_string(),
            path: Some(path.to_string()),
            ent: None,
            trust: None,
            children: Vec::new(),
        }
    }
}

/// The `physical` projection — directories → files, the shape every editor
/// ships and the one this tree keeps as its escape hatch (the report's W5
/// argues for a namespace-first DEFAULT; that is a per-repo preference, not
/// a wire decision, so it is not made here).
pub fn project_physical(paths: &[String]) -> Vec<Node> {
    // Two passes over the path list, no per-insert rebuild: collect every
    // directory's DIRECT children first, then build the node tree once.
    // (The obvious recursive-insert version is quadratic in a directory's
    // fan-out, which on the 6,553-file target repo is a measurable cost for
    // no benefit.)
    let mut children: BTreeMap<String, BTreeSet<(bool, String)>> = BTreeMap::new();
    for p in paths {
        let mut parent = String::new();
        let segs: Vec<&str> = p.split('/').collect();
        for (i, seg) in segs.iter().enumerate() {
            let is_file = i + 1 == segs.len();
            let full = if parent.is_empty() {
                (*seg).to_string()
            } else {
                format!("{parent}/{seg}")
            };
            children
                .entry(parent.clone())
                .or_default()
                .insert((is_file, full.clone()));
            if is_file {
                break;
            }
            parent = full;
        }
    }

    fn build(dir: &str, children: &BTreeMap<String, BTreeSet<(bool, String)>>) -> Vec<Node> {
        let Some(kids) = children.get(dir) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(kids.len());
        for (is_file, full) in kids {
            let label = full.rsplit('/').next().unwrap_or(full).to_string();
            if *is_file {
                out.push(Node::file(full, &label));
            } else {
                out.push(Node {
                    key: format!("d:{full}"),
                    kind: RowKind::Dir,
                    label,
                    path: Some(full.clone()),
                    ent: None,
                    trust: None,
                    children: build(full, children),
                });
            }
        }
        out
    }
    let mut out = build("", &children);
    sort_nodes(&mut out);
    out
}

/// Directories first, then files, each case-insensitively alphabetical —
/// the same order `web-code/src/lib/tree.ts`'s `sortTreeEntries` used
/// before this wire existed, so the repoint does not reshuffle the tree
/// under the operator.
fn sort_nodes(nodes: &mut Vec<Node>) {
    nodes.sort_by(|a, b| {
        let rank = |k: RowKind| match k {
            RowKind::Dir | RowKind::Group => 0,
            RowKind::Entity => 1,
            RowKind::File => 2,
        };
        rank(a.kind)
            .cmp(&rank(b.kind))
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
            .then_with(|| a.key.cmp(&b.key))
    });
    for n in nodes.iter_mut() {
        sort_nodes(&mut n.children);
    }
}

/// The `role` projection — a Rails role bucket per file, from a path
/// CONVENTION. Every bucket carries [`roles::ROLE_TRUST`]; `other` is the
/// unplaced bucket and is returned SEPARATELY (never as a bucket that
/// looks like the rest).
pub fn project_role(paths: &[String]) -> (Vec<Node>, Vec<String>) {
    let mut buckets: BTreeMap<&'static str, Vec<Node>> = BTreeMap::new();
    let mut unplaced = Vec::new();
    for p in paths {
        let role = roles::role_for_path(p);
        if role == roles::ROLE_OTHER {
            unplaced.push(p.clone());
            continue;
        }
        let label = p.rsplit('/').next().unwrap_or(p);
        buckets.entry(role).or_default().push(Node::file(p, label));
    }
    let mut out = Vec::new();
    for role in roles::ROLES {
        if *role == roles::ROLE_OTHER {
            continue;
        }
        let Some(files) = buckets.remove(role) else {
            continue;
        };
        let mut n = Node::group(format!("r:{role}"), *role);
        n.trust = Some(roles::ROLE_TRUST.to_string());
        n.children = files;
        out.push(n);
    }
    for n in out.iter_mut() {
        sort_nodes(&mut n.children);
    }
    (out, unplaced)
}

/// One entity CLAIM, as the `namespace` projection needs it.
#[derive(Debug, Clone)]
pub struct EntityPlacement {
    pub fqn: String,
    pub path: String,
    pub kind: String,
    /// The class [`crate::entities::class_for`] minted for this claim, per
    /// request — never read from the row (crate invariant 13).
    pub trust: &'static str,
}

/// The `namespace` projection — the constant tree from the V71-G0 entity
/// index, each constant expanding into the FILES that define it. A file
/// with no entity claim is unplaced; so is every file in a repo the entity
/// indexer does not cover, which is why the caller captions an empty
/// namespace tree rather than showing an empty one.
pub fn project_namespace(paths: &[String], defs: &[EntityPlacement]) -> (Vec<Node>, Vec<String>) {
    let in_scope: HashSet<&str> = paths.iter().map(String::as_str).collect();
    let mut placed: HashSet<&str> = HashSet::new();
    // fqn → (kind, worst trust, files)
    let mut by_fqn: BTreeMap<&str, (String, &'static str, BTreeSet<&str>)> = BTreeMap::new();
    for d in defs {
        if !in_scope.contains(d.path.as_str()) {
            continue;
        }
        placed.insert(d.path.as_str());
        let e = by_fqn
            .entry(d.fqn.as_str())
            .or_insert_with(|| (d.kind.clone(), d.trust, BTreeSet::new()));
        // A constant's own class is the WEAKEST of its definition sites'
        // — one candidate claim among three exact ones does not make the
        // group exact. (`class_for`'s vocabulary, ordered here only.)
        if trust_rank(d.trust) > trust_rank(e.1) {
            e.1 = d.trust;
        }
        e.2.insert(d.path.as_str());
    }

    // Build the constant hierarchy from the FQN segments. An intermediate
    // segment nobody defines (`Reseller` when only `Reseller::Order` has a
    // row) becomes a group with NO trust class — it is a naming container,
    // not a claim about a definition.
    let mut roots: Vec<Node> = Vec::new();
    let mut nodes: Vec<Node> = Vec::new();
    let mut key_to_idx: HashMap<String, usize> = HashMap::new();
    let mut children_of: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut root_idx: Vec<usize> = Vec::new();

    for (fqn, (_kind, trust, files)) in &by_fqn {
        let segs: Vec<&str> = fqn.split("::").collect();
        let mut parent: Option<usize> = None;
        let mut acc = String::new();
        for (i, seg) in segs.iter().enumerate() {
            if i > 0 {
                acc.push_str("::");
            }
            acc.push_str(seg);
            let key = format!("e:{acc}");
            let idx = match key_to_idx.get(&key) {
                Some(i) => *i,
                None => {
                    let mut n = Node::group(key.clone(), *seg);
                    n.kind = RowKind::Entity;
                    n.ent = Some(acc.clone());
                    nodes.push(n);
                    let idx = nodes.len() - 1;
                    key_to_idx.insert(key.clone(), idx);
                    match parent {
                        Some(p) => children_of.entry(p).or_default().push(idx),
                        None => root_idx.push(idx),
                    }
                    idx
                }
            };
            if i + 1 == segs.len() {
                nodes[idx].trust = Some((*trust).to_string());
                for f in files {
                    let label = f.rsplit('/').next().unwrap_or(f);
                    let file_idx = {
                        nodes.push(Node::file(f, label));
                        nodes.len() - 1
                    };
                    children_of.entry(idx).or_default().push(file_idx);
                }
            }
            parent = Some(idx);
        }
    }

    fn build(idx: usize, nodes: &[Node], children_of: &HashMap<usize, Vec<usize>>) -> Node {
        let mut n = nodes[idx].clone();
        n.children = children_of
            .get(&idx)
            .map(|kids| kids.iter().map(|k| build(*k, nodes, children_of)).collect())
            .unwrap_or_default();
        n
    }
    for r in &root_idx {
        roots.push(build(*r, &nodes, &children_of));
    }
    sort_nodes(&mut roots);

    let unplaced: Vec<String> = paths
        .iter()
        .filter(|p| !placed.contains(p.as_str()))
        .cloned()
        .collect();
    (roots, unplaced)
}

fn trust_rank(t: &str) -> u8 {
    match t {
        "exact" => 0,
        "likely" => 1,
        "candidate" => 2,
        _ => 3,
    }
}

/// The `change` projection — the diff tree against a base ref, grouped by
/// git status so the reading order is "what happened", not the alphabet.
/// An indexed file that did NOT change is unplaced BY CONSTRUCTION and the
/// caller says so in `notes` rather than listing thousands of rows in the
/// bucket (the one place the unplaced bucket is a COUNT and a caption, not
/// a list — stated on the wire, never implied).
pub fn project_change(changes: &[(String, String)]) -> Vec<Node> {
    let mut by_status: BTreeMap<&'static str, Vec<Node>> = BTreeMap::new();
    for (path, status) in changes {
        let bucket = change_bucket(status);
        let label = path.rsplit('/').next().unwrap_or(path);
        let mut f = Node::file(path, label);
        f.trust = None;
        by_status.entry(bucket).or_default().push(f);
    }
    let mut out = Vec::new();
    for bucket in CHANGE_BUCKETS {
        let Some(files) = by_status.remove(bucket) else {
            continue;
        };
        let mut n = Node::group(format!("c:{bucket}"), *bucket);
        n.children = files;
        out.push(n);
    }
    for n in out.iter_mut() {
        sort_nodes(&mut n.children);
    }
    out
}

pub const CHANGE_BUCKETS: &[&str] = &["added", "modified", "renamed", "deleted", "other"];

fn change_bucket(status: &str) -> &'static str {
    match status.chars().next() {
        Some('A') => "added",
        Some('M') => "modified",
        Some('R') | Some('C') => "renamed",
        Some('D') => "deleted",
        _ => "other",
    }
}

// ── flattening ────────────────────────────────────────────────────────────

/// Everything the flattener needs that is not the shape itself.
pub struct FlattenOpts<'a> {
    pub expand: &'a HashSet<String>,
    /// Groups at a depth below this render their children without being
    /// named in `expand`. `0` = unlimited.
    pub depth: u32,
    pub limit: usize,
    pub mode: &'a str,
    pub matcher: Option<&'a mut NameMatcher>,
    pub facts: &'a HashMap<String, Facts>,
    pub want_facts: bool,
}

struct FlatState {
    rows: Vec<TreeRow>,
    matched: u32,
    truncated: bool,
    total_rows: u32,
}

/// Flatten a projection into wire rows. ONE function for all four
/// projections and both filter modes — a second flattener is how the two
/// modes would drift apart.
pub fn flatten(nodes: &[Node], opts: &mut FlattenOpts<'_>) -> (Vec<TreeRow>, u32, u32, bool) {
    let mut st = FlatState {
        rows: Vec::new(),
        matched: 0,
        truncated: false,
        total_rows: 0,
    };
    for n in nodes {
        walk(n, 0, &mut st, opts);
    }
    (st.rows, st.matched, st.total_rows, st.truncated)
}

/// Returns `(files_in_subtree, matches_in_subtree, facts)` for `node`,
/// pushing rows as it goes. The post-order fold is what lets an ancestor
/// carry both its subtree's decoration aggregate and its match count in one
/// pass — the rows are emitted pre-order and PATCHED, so the response is
/// still in reading order.
fn walk(
    node: &Node,
    depth: u32,
    st: &mut FlatState,
    opts: &mut FlattenOpts<'_>,
) -> (u32, u32, Facts) {
    let row_index = st.rows.len();
    let over_limit = st.rows.len() >= opts.limit || st.rows.len() >= MAX_TREE_ROWS;
    if over_limit {
        st.truncated = true;
    }
    st.total_rows += 1;

    let (ranges, self_match) = match opts.matcher.as_deref_mut() {
        Some(m) => match m.score(&node.label, HaystackKind::Path) {
            Some(nm) => (nm.ranges, 1u32),
            None => (Vec::new(), 0u32),
        },
        None => (Vec::new(), 0u32),
    };

    let mut own_facts = if opts.want_facts {
        node.path
            .as_ref()
            .and_then(|p| opts.facts.get(p))
            .cloned()
            .unwrap_or_default()
    } else {
        Facts::default()
    };

    if !over_limit {
        st.rows.push(TreeRow {
            id: node.key.clone(),
            kind: node.kind,
            label: node.label.clone(),
            depth,
            path: node.path.clone(),
            ent: node.ent.clone(),
            trust: node.trust.clone(),
            children: node.children.len() as u32,
            files: 0,
            has_more: false,
            match_ranges: ranges,
            match_count: 0,
            facts: Facts::default(),
        });
    }

    // `depth` is a LEVEL COUNT: `1` renders the roots alone, `2` their
    // children too, `0` the whole projection. A group past the budget is
    // still COUNTED (`count_only`) so its collapsed row reports the same
    // numbers it would show open.
    let expanded = opts.depth == 0 || depth + 1 < opts.depth || opts.expand.contains(&node.key);
    let mut files = if node.kind == RowKind::File { 1 } else { 0 };
    let mut matches = self_match;
    if !node.children.is_empty() {
        if expanded {
            for c in &node.children {
                let (f, m, cf) = walk(c, depth + 1, st, opts);
                files += f;
                matches += m;
                if opts.want_facts {
                    own_facts.fold(&cf);
                }
            }
        } else {
            // Not expanded: still COUNT the subtree so the collapsed row
            // tells the truth about what it hides.
            let (f, m, cf) = count_only(node, opts);
            files += f;
            matches += m;
            if opts.want_facts {
                own_facts.fold(&cf);
            }
        }
    }

    if self_match > 0 {
        st.matched += 1;
    }
    if !over_limit {
        if let Some(row) = st.rows.get_mut(row_index) {
            row.files = files;
            row.match_count = matches;
            row.has_more = !node.children.is_empty() && !expanded;
            row.facts = own_facts.clone();
        }
    }
    // `filter` mode prunes: a subtree with no match anywhere in it (and no
    // match on the row itself) is dropped, ancestry and all — but a row
    // that DOES have a matching descendant survives, which is the
    // "matched rows keep their ancestry for orientation" half.
    if opts.matcher.is_some() && opts.mode == MODE_FILTER && matches == 0 && !over_limit {
        st.rows.truncate(row_index);
    }
    (files, matches, own_facts)
}

/// The collapsed-subtree fold — same arithmetic as [`walk`] with no row
/// emission, so a closed folder's aggregate is never a different number
/// from the one it would show open.
fn count_only(node: &Node, opts: &mut FlattenOpts<'_>) -> (u32, u32, Facts) {
    let mut files = 0;
    let mut matches = 0;
    let mut facts = Facts::default();
    for c in &node.children {
        let self_match = match opts.matcher.as_deref_mut() {
            Some(m) => u32::from(m.score(&c.label, HaystackKind::Path).is_some()),
            None => 0,
        };
        let mut child_facts = if opts.want_facts {
            c.path
                .as_ref()
                .and_then(|p| opts.facts.get(p))
                .cloned()
                .unwrap_or_default()
        } else {
            Facts::default()
        };
        let (f, m, cf) = count_only(c, opts);
        if opts.want_facts {
            child_facts.fold(&cf);
        }
        files += f + u32::from(c.kind == RowKind::File);
        matches += m + self_match;
        if opts.want_facts {
            facts.fold(&child_facts);
        }
    }
    (files, matches, facts)
}

/// Split a `decorate=` CSV into the lanes that will run and the ones the
/// budget dropped. An unknown lane name is dropped too, and named.
pub fn resolve_lanes(raw: Option<&str>) -> (Vec<String>, Vec<String>) {
    let Some(raw) = raw else {
        return (Vec::new(), Vec::new());
    };
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for name in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if !LANES.contains(&name) {
            dropped.push(name.to_string());
            continue;
        }
        if kept.iter().any(|k: &String| k == name) {
            continue;
        }
        if kept.len() >= MAX_DECORATION_LANES {
            dropped.push(name.to_string());
            continue;
        }
        kept.push(name.to_string());
    }
    (kept, dropped)
}

/// Re-base match ranges computed over `haystack` onto its `label` SUFFIX,
/// in UTF-16 units. Used by the SPA-facing rows when the matcher scored the
/// full label path but the row renders only its leaf.
pub fn rebase_ranges(haystack: &str, label: &str, ranges: &[[u32; 2]]) -> Vec<[u32; 2]> {
    let Some(prefix) = haystack.strip_suffix(label) else {
        return Vec::new();
    };
    let off = prefix.encode_utf16().count() as u32;
    let len = label.encode_utf16().count() as u32;
    ranges
        .iter()
        .filter_map(|[s, e]| {
            let s2 = s.saturating_sub(off);
            let e2 = e.saturating_sub(off);
            (*e > off && s2 < len).then_some([s2.min(len), e2.min(len)])
        })
        .filter(|[s, e]| e > s)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    fn flat(nodes: &[Node], depth: u32) -> Vec<TreeRow> {
        let expand = HashSet::new();
        let facts = HashMap::new();
        let mut opts = FlattenOpts {
            expand: &expand,
            depth,
            limit: DEFAULT_LIMIT,
            mode: MODE_FILTER,
            matcher: None,
            facts: &facts,
            want_facts: false,
        };
        flatten(nodes, &mut opts).0
    }

    // --- projections ------------------------------------------------------

    #[test]
    fn physical_nests_directories_and_sorts_dirs_first() {
        let nodes = project_physical(&paths(&[
            "README.md",
            "app/models/order.rb",
            "app/models/cart.rb",
            "app/controllers/x.rb",
        ]));
        let rows = flat(&nodes, 0);
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "app",
                "controllers",
                "x.rb",
                "models",
                "cart.rb",
                "order.rb",
                "README.md"
            ]
        );
        let app = &rows[0];
        assert_eq!(app.kind, RowKind::Dir);
        assert_eq!(app.files, 3, "a folder counts its whole subtree");
        assert!(app.trust.is_none(), "a directory is not a claim");
    }

    #[test]
    fn a_collapsed_folder_reports_the_same_counts_it_would_open_with() {
        let all = paths(&["app/models/a.rb", "app/models/b.rb", "app/x.rb"]);
        let nodes = project_physical(&all);
        let open = flat(&nodes, 0);
        let shut = flat(&nodes, 1);
        let open_app = open.iter().find(|r| r.label == "app").expect("app");
        let shut_app = shut.iter().find(|r| r.label == "app").expect("app");
        assert_eq!(open_app.files, shut_app.files);
        assert!(!open_app.has_more);
        assert!(shut_app.has_more, "a closed folder says it hides rows");
        assert_eq!(shut.len(), 1, "depth 1 renders the roots alone");
    }

    #[test]
    fn depth_is_a_level_count() {
        let nodes = project_physical(&paths(&["a/b/c.rb", "a/d.rb"]));
        assert_eq!(flat(&nodes, 1).len(), 1);
        assert_eq!(flat(&nodes, 2).len(), 3);
        assert_eq!(flat(&nodes, 0).len(), 4);
    }

    #[test]
    fn an_expand_key_opens_one_group_past_the_depth_budget() {
        let nodes = project_physical(&paths(&["a/b/c.rb", "a/d.rb"]));
        let expand: HashSet<String> = ["d:a".to_string()].into_iter().collect();
        let facts = HashMap::new();
        let mut opts = FlattenOpts {
            expand: &expand,
            depth: 1,
            limit: DEFAULT_LIMIT,
            mode: MODE_FILTER,
            matcher: None,
            facts: &facts,
            want_facts: false,
        };
        let rows = flatten(&nodes, &mut opts).0;
        assert_eq!(
            rows.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "d.rb"]
        );
    }

    #[test]
    fn role_buckets_carry_likely_and_never_swallow_an_unplaceable_file() {
        let (nodes, unplaced) = project_role(&paths(&[
            "app/models/order.rb",
            "spec/models/order_spec.rb",
            "vendor/thing.rb",
        ]));
        let rows = flat(&nodes, 0);
        let model = rows.iter().find(|r| r.label == "model").expect("model");
        assert_eq!(model.trust.as_deref(), Some(roles::ROLE_TRUST));
        assert_ne!(model.trust.as_deref(), Some("exact"));
        assert_eq!(unplaced, vec!["vendor/thing.rb".to_string()]);
    }

    #[test]
    fn namespace_groups_by_constant_and_a_group_takes_its_weakest_claim() {
        let defs = vec![
            EntityPlacement {
                fqn: "Reseller::Order".into(),
                path: "app/models/reseller/order.rb".into(),
                kind: "class".into(),
                trust: "exact",
            },
            EntityPlacement {
                fqn: "Reseller::Order".into(),
                path: "app/services/reseller/order_service.rb".into(),
                kind: "class".into(),
                trust: "candidate",
            },
        ];
        let (nodes, unplaced) = project_namespace(
            &paths(&[
                "app/models/reseller/order.rb",
                "app/services/reseller/order_service.rb",
                "config/routes.rb",
            ]),
            &defs,
        );
        let rows = flat(&nodes, 0);
        let order = rows.iter().find(|r| r.label == "Order").expect("Order");
        assert_eq!(order.ent.as_deref(), Some("Reseller::Order"));
        assert_eq!(
            order.trust.as_deref(),
            Some("candidate"),
            "one candidate claim does not become an exact group"
        );
        let reseller = rows
            .iter()
            .find(|r| r.label == "Reseller")
            .expect("Reseller");
        assert!(
            reseller.trust.is_none(),
            "a naming container nobody defines carries no class"
        );
        assert_eq!(unplaced, vec!["config/routes.rb".to_string()]);
    }

    #[test]
    fn namespace_ignores_a_claim_whose_path_the_scope_removed() {
        let defs = vec![EntityPlacement {
            fqn: "Reseller::Order".into(),
            path: "app/models/reseller/order.rb".into(),
            kind: "class".into(),
            trust: "exact",
        }];
        // The scope kept only `config/routes.rb`, so the claim is out of
        // scope and `config/routes.rb` is unplaced — never a row for a file
        // the caller filtered away.
        let (nodes, unplaced) = project_namespace(&paths(&["config/routes.rb"]), &defs);
        assert!(nodes.is_empty());
        assert_eq!(unplaced, vec!["config/routes.rb".to_string()]);
    }

    #[test]
    fn change_groups_by_status_in_reading_order() {
        let rows = flat(
            &project_change(&[
                ("b.rb".into(), "M".into()),
                ("a.rb".into(), "A".into()),
                ("c.rb".into(), "D".into()),
            ]),
            0,
        );
        let groups: Vec<&str> = rows
            .iter()
            .filter(|r| r.kind == RowKind::Group)
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(groups, vec!["added", "modified", "deleted"]);
    }

    // --- filtering --------------------------------------------------------

    #[test]
    fn filter_mode_prunes_but_keeps_a_matches_ancestry() {
        let nodes = project_physical(&paths(&["app/models/order.rb", "app/models/cart.rb"]));
        let expand = HashSet::new();
        let facts = HashMap::new();
        let mut m = NameMatcher::new("order", HaystackKind::Path);
        let mut opts = FlattenOpts {
            expand: &expand,
            depth: 0,
            limit: DEFAULT_LIMIT,
            mode: MODE_FILTER,
            matcher: Some(&mut m),
            facts: &facts,
            want_facts: false,
        };
        let (rows, matched, _, _) = flatten(&nodes, &mut opts);
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, vec!["app", "models", "order.rb"]);
        assert_eq!(matched, 1);
        assert_eq!(rows[0].match_count, 1, "the ancestor carries the badge");
    }

    #[test]
    fn highlight_mode_keeps_every_row_and_badges_the_ancestors() {
        let nodes = project_physical(&paths(&["app/models/order.rb", "app/models/cart.rb"]));
        let expand = HashSet::new();
        let facts = HashMap::new();
        let mut m = NameMatcher::new("order", HaystackKind::Path);
        let mut opts = FlattenOpts {
            expand: &expand,
            depth: 0,
            limit: DEFAULT_LIMIT,
            mode: MODE_HIGHLIGHT,
            matcher: Some(&mut m),
            facts: &facts,
            want_facts: false,
        };
        let (rows, _, _, _) = flatten(&nodes, &mut opts);
        assert_eq!(rows.len(), 4, "nothing is pruned in highlight mode");
        assert_eq!(rows[0].match_count, 1);
        let order = rows.iter().find(|r| r.label == "order.rb").expect("order");
        assert!(!order.match_ranges.is_empty(), "the leaf carries ranges");
    }

    // --- decorations ------------------------------------------------------

    #[test]
    fn the_lane_budget_is_three_and_it_names_what_it_dropped() {
        let (kept, dropped) = resolve_lanes(Some("git,annot,findings,todo,nonsense"));
        assert_eq!(kept, vec!["git", "annot", "findings"]);
        assert_eq!(dropped, vec!["todo", "nonsense"]);
        assert_eq!(resolve_lanes(None), (Vec::new(), Vec::new()));
        // A repeat is not a second slot.
        assert_eq!(resolve_lanes(Some("git,git,annot")).0, vec!["git", "annot"]);
    }

    #[test]
    fn a_folder_aggregate_is_the_fold_of_its_subtree() {
        let nodes = project_physical(&paths(&["app/a.rb", "app/b.rb"]));
        let expand = HashSet::new();
        let mut facts = HashMap::new();
        facts.insert(
            "app/a.rb".to_string(),
            Facts {
                annot: Some(2),
                findings: Some("concern".into()),
                git: Some("M".into()),
                ..Default::default()
            },
        );
        facts.insert(
            "app/b.rb".to_string(),
            Facts {
                annot: Some(1),
                findings: Some("blocker".into()),
                ..Default::default()
            },
        );
        let mut opts = FlattenOpts {
            expand: &expand,
            depth: 0,
            limit: DEFAULT_LIMIT,
            mode: MODE_FILTER,
            matcher: None,
            facts: &facts,
            want_facts: true,
        };
        let (rows, _, _, _) = flatten(&nodes, &mut opts);
        let app = &rows[0];
        assert_eq!(app.facts.annot, Some(3));
        assert_eq!(
            app.facts.findings.as_deref(),
            Some("blocker"),
            "a folder shows the WORST severity in its subtree"
        );
        assert_eq!(app.facts.git_changed, Some(1));
    }

    #[test]
    fn a_closed_folders_aggregate_equals_its_open_one() {
        let nodes = project_physical(&paths(&["app/a.rb", "app/b.rb"]));
        let expand = HashSet::new();
        let mut facts = HashMap::new();
        facts.insert(
            "app/a.rb".to_string(),
            Facts {
                annot: Some(2),
                git: Some("M".into()),
                ..Default::default()
            },
        );
        facts.insert(
            "app/b.rb".to_string(),
            Facts {
                annot: Some(5),
                ..Default::default()
            },
        );
        let fold = |depth: u32| {
            let mut opts = FlattenOpts {
                expand: &expand,
                depth,
                limit: DEFAULT_LIMIT,
                mode: MODE_FILTER,
                matcher: None,
                facts: &facts,
                want_facts: true,
            };
            flatten(&nodes, &mut opts).0[0].facts.clone()
        };
        assert_eq!(fold(0), fold(1));
    }

    // --- honesty ----------------------------------------------------------

    #[test]
    fn the_row_cap_reports_itself_in_band() {
        let many: Vec<String> = (0..50).map(|i| format!("d/f{i:03}.rb")).collect();
        let nodes = project_physical(&many);
        let expand = HashSet::new();
        let facts = HashMap::new();
        let mut opts = FlattenOpts {
            expand: &expand,
            depth: 0,
            limit: 10,
            mode: MODE_FILTER,
            matcher: None,
            facts: &facts,
            want_facts: false,
        };
        let (rows, _, total, truncated) = flatten(&nodes, &mut opts);
        assert!(truncated);
        assert_eq!(rows.len(), 10);
        assert_eq!(total, 51, "the TRUE total is still counted");
    }

    #[test]
    fn rebase_ranges_maps_a_path_match_onto_the_leaf_label() {
        let r = rebase_ranges("app/models/order.rb", "order.rb", &[[11, 16]]);
        assert_eq!(r, vec![[0, 5]]);
        assert!(rebase_ranges("app/models/order.rb", "order.rb", &[[0, 3]]).is_empty());
        assert!(rebase_ranges("nope", "order.rb", &[[0, 3]]).is_empty());
    }

    #[test]
    fn severity_rank_orders_blocker_first_and_an_unknown_last() {
        assert!(severity_rank("blocker") < severity_rank("concern"));
        assert!(severity_rank("concern") < severity_rank("ok"));
        assert!(severity_rank("ok") < severity_rank("nonsense"));
    }
}
