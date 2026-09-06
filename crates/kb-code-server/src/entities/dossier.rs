//! V72-G1.1 — `entity/1`: the entity DOSSIER wire (`GET /api/entity/dossier`).
//!
//! `entities/1` (`super::entity_route`, V71-G0) answers "which constants
//! answer to this address, and where is each one defined". It is FROZEN
//! and still served byte-identically at `GET /api/entity` — the same
//! one-ladder-two-wires treatment `/usages/2` and `/tree/2` got, and the
//! reason a dossier is a SIBLING path rather than a widened `entities/1`:
//! the index answers an ADDRESSING question and legitimately returns many
//! entities, while a dossier is everything about exactly ONE.
//!
//! This module is D6's page, minus the page: definitions, a merged member
//! table, hierarchy, grouped usages, the metaprogramming holes, and the
//! namespace tree — the wire the reader shell (G1.2) renders and
//! `kb-code entity` prints.
//!
//! # Six rules this module is built around
//!
//! 1. **Nothing is persisted.** Every claim here is computed from the
//!    entity index's stored CLAIMS plus the live bytes, per request
//!    (crate invariant 13, root CLAUDE.md #2). There is no migration in
//!    this unit and no new table.
//! 2. **The usages lane is the usages/2 ENGINE, never a second scanner.**
//!    [`UsagesInput`] carries a real [`Usages2Out`]; this module regroups
//!    its rows by kind and copies each row's own `trust` verbatim. It has
//!    no way to mint a class, which is what makes "a wrong `exact` is a
//!    release blocker" structural here rather than a discipline.
//! 3. **A line scan is capped at `likely`.** `attr_accessor`, constants,
//!    `alias_method`, `include`/`prepend`/`extend` and the superclass are
//!    all invisible to tree-sitter-ruby's `tags.scm` (they are ordinary
//!    method calls), so `super::ruby_body` reads them off the source. A
//!    member the TREE proves inherits its block's own class; a member a
//!    LINE proves never exceeds `likely`.
//! 4. **A constant reference is a RUNTIME lookup.** [`resolve_reference`]
//!    is the one function that turns a written superclass/mixin name into
//!    a class, and it reaches `exact` from exactly one shape — a
//!    root-anchored or top-level-lexical reference whose every definition
//!    site is itself `exact`. A reference written inside a module is
//!    nesting-relative and caps at `likely`, because `module M; class A <
//!    B` is `M::B` when `M::B` exists and `::B` otherwise, and no tree
//!    settles that.
//! 5. **Every list carries its TRUE total and an explicit `truncated`.**
//!    The budget is a ROW budget spent in a fixed, documented lane order
//!    ([`LANE_PRIORITY`]), and every row it drops is counted by lane in
//!    [`BudgetReport::dropped`]. A budget that hid work silently would be
//!    worse than no budget.
//! 6. **Four states, one field.** [`Honesty::state`] is `ok`, `partial`
//!    or `empty`, always with a `reason` on the latter two; a genuine
//!    error is the route's own `ApiError` (the fourth state), never a
//!    200 with a plausible-looking empty body.

use super::ruby_body as rb;
use super::zeitwerk;
use super::{class_for, last_segment, TrustCounts, ZeitwerkOut};
use crate::extract::Symbol;
use crate::resolve::{CLASS_CANDIDATE, CLASS_EXACT, CLASS_LIKELY};
use crate::store::{EntityDefRow, StoreBlocking};
use crate::usages2::{UsageKind, UsageRow2, Usages2Out};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub const DOSSIER_SCHEMA: &str = "entity/1";

/// Default/max rows per usage KIND group. The default is small on purpose:
/// a dossier is read to orient, and the true total is always on the group.
pub const DEFAULT_USAGES_PER_KIND: usize = 20;
pub const MAX_USAGES_PER_KIND: usize = 200;

/// The response budget, counted in ROWS (definitions + members + hierarchy
/// edges + holes + namespace children + usage rows). Rows rather than
/// bytes because a row count is deterministic, explainable in the response
/// itself, and does not need a serialization pass to enforce.
pub const DEFAULT_BUDGET: usize = 600;
pub const MAX_BUDGET: usize = 10_000;

/// Bound on how many distinct definition FILES this route opens. An entity
/// reopened in 400 files is real in a monolith; reading all of them is a
/// per-request IO fan-out on an IO-bound host.
pub const MAX_DEFINITION_FILES: usize = 64;

/// Bound on the superclass chain walk. Each hop is one more file read.
pub const MAX_ANCESTOR_DEPTH: usize = 8;

/// Bound on the whole-repo `entity_defs` read behind `namespace_tree` and
/// the descendant/implementor enclosing-entity lookup.
pub const MAX_REPO_DEFS: usize = 20_000;

/// The entity KIND vocabulary. `constant` is reachable: an address that
/// names no class/module but resolves to a constant assignment inside its
/// parent's body is answered as a constant rather than 404'd. `unknown` is
/// reachable too — an address answered by rows that disagree about their
/// own kind (a real Ruby `superclass mismatch` bug) is reported as
/// `unknown`, not silently resolved to whichever row sorted first.
pub const ENTITY_KIND_CLASS: &str = "class";
pub const ENTITY_KIND_MODULE: &str = "module";
pub const ENTITY_KIND_CONSTANT: &str = "constant";
pub const ENTITY_KIND_UNKNOWN: &str = "unknown";
pub const ENTITY_KINDS: [&str; 4] = [
    ENTITY_KIND_CLASS,
    ENTITY_KIND_MODULE,
    ENTITY_KIND_CONSTANT,
    ENTITY_KIND_UNKNOWN,
];

/// The definition-BLOCK kind vocabulary. `reopen` names a block that is
/// not the one the app's Zeitwerk configuration expects to define the
/// constant — evidence, not a guess about load order (see
/// [`DefinitionBlock::kind`]).
pub const BLOCK_REOPEN: &str = "reopen";

/// A reference this index cannot resolve to any definition it holds.
/// Beside `exact`/`likely`/`candidate` on [`Ancestor::resolved`] and
/// [`Mixin::resolved`] — an honest fourth outcome, never a silent drop.
pub const RESOLVED_UNRESOLVED: &str = "unresolved";

pub const STATE_OK: &str = "ok";
pub const STATE_PARTIAL: &str = "partial";
pub const STATE_EMPTY: &str = "empty";

/// The order the row budget is spent in. Declared as data so the response
/// can name it and a test can pin it: a reader who asks for 50 rows gets
/// the entity's identity first and its long tail of usages last, always.
pub const LANE_PRIORITY: [&str; 8] = [
    "definitions",
    "ancestors",
    "mixins",
    "members",
    "unknown_members",
    "namespace_tree",
    "descendants",
    "implementors",
];

// ---------------------------------------------------------------------------
// wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct EntitySummary {
    pub fqn: String,
    /// One of [`ENTITY_KINDS`].
    pub kind: &'static str,
    /// The enclosing constant path, or `null` at the top level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// EVERY file that reopens this entity, in response order.
    pub files: Vec<String>,
    /// A census of the definition sites' trust classes, never an aggregate
    /// verdict — `entities/1`'s own posture, kept.
    pub trust_counts: TrustCounts,
}

#[derive(Debug, Clone, Serialize)]
pub struct DefinitionBlock {
    pub path: String,
    pub line_start: u32,
    pub line_end: u32,
    /// The blob these lines were read from — a block is pinned to bytes,
    /// as review comments are. `null` when the file is no longer in the
    /// mirror.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob_sha: Option<String>,
    /// `class` | `module` | [`BLOCK_REOPEN`] | `constant`. The autoload
    /// canonical block (the one whose PATH the app's Zeitwerk
    /// configuration expects to define this constant) keeps its literal
    /// keyword; every other block is a `reopen`. When no block is
    /// canonical — the config read degraded, or the paths are under no
    /// autoload root — every block keeps its keyword and `notes` says the
    /// primary could not be determined. Nothing here claims a LOAD order,
    /// which Ruby decides at runtime.
    pub kind: String,
    /// Position in this response's definition order (0-based). The
    /// canonical block, when known, is index 0.
    pub reopening_index: usize,
    /// The literal opener chain — `module Reseller; class Order` — read
    /// off the source, never reconstructed from the FQN.
    pub opener: String,
    /// `top-level` | `nested` | `compact` | `mixed` | `unknown`.
    pub opener_form: &'static str,
    pub trust: &'static str,
    pub matched_via: &'static str,
    pub nesting: String,
    pub stale: bool,
    /// `true` when the path carries no live bytes any more (deleted, or
    /// never mirrored) — the block's lines could not be read.
    pub missing: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemberRow {
    pub name: String,
    /// One of [`rb::MEMBER_KINDS`].
    pub kind: &'static str,
    /// One of [`rb::VISIBILITIES`] — `unknown` when a `private` keyword's
    /// scope could not be resolved.
    pub visibility: &'static str,
    /// The FQN whose body defines this member.
    pub defining_type: String,
    /// `true` only for rows that came from an ancestor or a mixin, and
    /// only when the request asked for them (`?inherited=1`).
    pub inherited: bool,
    pub path: String,
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob_sha: Option<String>,
    /// `tree` | `macro` | `assignment` — how the row was found. Surfaced
    /// rather than folded into `trust`: a reader deserves to know why an
    /// `attr_accessor` is only `likely`.
    pub via: &'static str,
    pub trust: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct Ancestor {
    /// The constant AS WRITTEN at the reference site.
    pub written: String,
    /// The FQN it resolved to, or `null` when it resolved to nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fqn: Option<String>,
    /// `exact` | `likely` | `candidate` | [`RESOLVED_UNRESOLVED`].
    pub resolved: &'static str,
    /// 1 = the direct superclass.
    pub depth: usize,
    /// Where the reference is written.
    pub from_path: String,
    pub from_line: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Mixin {
    /// `include` | `prepend` | `extend`.
    pub kind: &'static str,
    pub written: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fqn: Option<String>,
    pub resolved: &'static str,
    pub from_path: String,
    pub from_line: u32,
}

/// A subclass or an implementor — derived from the usages/2 lane's own
/// `inherit`/`include`/`prepend`/`extend` rows, never from a second scan.
#[derive(Debug, Clone, Serialize)]
pub struct Related {
    /// The entity whose definition block encloses the reference, when the
    /// entity index names one. `null` — with `path`/`line` still given —
    /// when it does not; an unnamed inheriting site is reported, never
    /// dropped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fqn: Option<String>,
    /// `include` | `prepend` | `extend` for an implementor; `inherit` for
    /// a descendant.
    pub via: &'static str,
    pub path: String,
    pub line: u32,
    /// The usages row's OWN trust class, carried verbatim.
    pub trust: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct Hierarchy {
    /// The superclass chain, nearest first. Empty for a module.
    pub ancestors: Vec<Ancestor>,
    pub mixins: Vec<Mixin>,
    pub descendants: Vec<Related>,
    pub implementors: Vec<Related>,
    /// Honest captions — a chain stopped by the depth bound, a superclass
    /// two reopenings disagree about, a lane the usages engine could not
    /// feed.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageGroup {
    /// A [`UsageKind`] wire name.
    pub kind: &'static str,
    /// The TRUE total for this kind, before `usages_per_kind` and before
    /// the budget — read off the usages/2 engine's own `kind_totals`,
    /// which counts pre-cap.
    pub total: usize,
    pub truncated: bool,
    /// A census over the RETURNED rows — `census_basis` says so. The
    /// engine reports no per-kind trust totals, and inventing one from a
    /// page would be exactly the corpus-estimate dishonesty the facets
    /// lane refuses.
    pub trust_census: TrustCounts,
    pub census_basis: &'static str,
    pub rows: Vec<UsageRow2>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageAnchor {
    pub path: String,
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsagesSection {
    /// `ok` | `empty` | `error`.
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The position the usages/2 engine was run at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<UsageAnchor>,
    pub groups: Vec<UsageGroup>,
    pub total: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnknownMember {
    /// One of [`rb::UNKNOWN_MECHANISMS`].
    pub mechanism: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name_hint: Option<String>,
    pub path: String,
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob_sha: Option<String>,
    pub context: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct NamespaceChild {
    pub fqn: String,
    /// The direct child SEGMENT under the addressed entity.
    pub segment: String,
    /// `class` | `module` | `namespace` (a name that only appears as a
    /// prefix of deeper entities — its own definition is not indexed).
    pub kind: String,
    /// Definition sites of the child itself.
    pub definitions: usize,
    /// Distinct entities strictly below the child.
    pub descendants: usize,
    pub trust_counts: TrustCounts,
}

#[derive(Debug, Clone, Serialize)]
pub struct Dropped {
    pub definitions: usize,
    pub members: usize,
    pub ancestors: usize,
    pub mixins: usize,
    pub descendants: usize,
    pub implementors: usize,
    pub unknown_members: usize,
    pub namespace_tree: usize,
    pub usages: usize,
}

impl Dropped {
    fn total(&self) -> usize {
        self.definitions
            + self.members
            + self.ancestors
            + self.mixins
            + self.descendants
            + self.implementors
            + self.unknown_members
            + self.namespace_tree
            + self.usages
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BudgetReport {
    /// The effective budget, in ROWS.
    pub requested: usize,
    /// Rows actually emitted.
    pub spent: usize,
    /// Rows removed, by lane. Never silent.
    pub dropped: Dropped,
    /// The order the budget was spent in — [`LANE_PRIORITY`], with
    /// `usages` always last.
    pub order: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Honesty {
    /// [`STATE_OK`] | [`STATE_PARTIAL`] | [`STATE_EMPTY`]. (An `error` is
    /// the route's own `ApiError`, never a 200 with a plausible body.)
    ///
    /// The line between `ok` and `partial` is a RULING, not an accident: a
    /// lane the CALLER capped — `usages_per_kind` — stays `ok`, because
    /// the caller chose that cut and each group reports its true `total`
    /// and its own `truncated` in band; a lane the BUDGET cut is
    /// `partial`, because the caller asked for a size and did not choose
    /// WHERE it landed. Everything else that makes an answer less than it
    /// claims — a missing or stale definition file, a visibility this
    /// scanner refused to resolve, a degraded Zeitwerk read, a usages lane
    /// that could not run — is `partial` too, and named in
    /// [`Self::notes`].
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub budget: BudgetReport,
    /// Every caption this response owes its reader, in the order they were
    /// raised. Always present, possibly empty — never a silent absence.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DossierOut {
    pub schema: &'static str,
    pub repo: String,
    /// The address as it was asked.
    pub ent: String,
    pub entity: EntitySummary,
    /// Non-empty ONLY when the address is ambiguous: every FQN that
    /// answers to it. Nothing is merged on a bare name.
    pub candidates: Vec<String>,
    pub definitions: Vec<DefinitionBlock>,
    pub members: Vec<MemberRow>,
    pub hierarchy: Hierarchy,
    pub usages: UsagesSection,
    pub unknown_members: Vec<UnknownMember>,
    pub namespace_tree: Vec<NamespaceChild>,
    pub zeitwerk: ZeitwerkOut,
    pub honesty: Honesty,
}

// ---------------------------------------------------------------------------
// inputs
// ---------------------------------------------------------------------------

/// What [`build_dossier`] is allowed to ask its caller for. The route
/// implements it over the store and the working tree inside ONE blocking
/// hop; a test implements it over in-memory maps, which is what makes the
/// whole envelope — ladder, budget, captions — assertable without a
/// daemon.
pub trait DossierSource {
    /// Live bytes + blob sha for a repo-relative path, or `None` when the
    /// path carries no live bytes any more.
    fn read(&self, path: &str) -> Option<(Vec<u8>, String)>;
    /// Every indexed definition site answering to `name`.
    fn defs_for_name(&self, name: &str) -> Vec<EntityDefRow>;
    /// Every indexed definition site in the repo, capped by the
    /// implementation at [`MAX_REPO_DEFS`].
    fn all_defs(&self) -> &[EntityDefRow];
}

/// The usages/2 engine's answer, or the honest reason there isn't one.
#[derive(Debug, Default)]
pub struct UsagesInput {
    pub out: Option<Usages2Out>,
    pub anchor: Option<UsageAnchor>,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DossierOptions {
    pub inherited: bool,
    pub budget: usize,
    pub usages_per_kind: usize,
}

impl Default for DossierOptions {
    fn default() -> Self {
        Self {
            inherited: false,
            budget: DEFAULT_BUDGET,
            usages_per_kind: DEFAULT_USAGES_PER_KIND,
        }
    }
}

/// A refusal the ROUTE turns into an `ApiError`. A plain struct rather
/// than an `ApiError` so the pure builder stays free of axum and a test
/// can read the status and the machine `reason` directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DossierRefusal {
    pub status: u16,
    /// Machine-readable degrade code (`entity-unknown`).
    pub reason: &'static str,
    pub message: String,
}

pub const REASON_ENTITY_UNKNOWN: &str = "entity-unknown";

// ---------------------------------------------------------------------------
// the reference ladder
// ---------------------------------------------------------------------------

/// Turn a constant reference WRITTEN at a site into a class.
///
/// `cref_is_object` is `true` only when Ruby would evaluate this reference
/// with an EMPTY lexical nesting — a superclass on a top-level or compact
/// `class Foo < Bar` opener, which is evaluated in the enclosing scope
/// before the body is entered. A mixin inside a body is never that.
///
/// | reference | index answer | class |
/// |---|---|---|
/// | root-anchored (`::A::B`) or `cref_is_object` | one FQN, every site `exact` | `exact` |
/// | root-anchored or `cref_is_object` | one FQN, some site weaker | that FQN's best site class, capped at `likely` |
/// | nesting-relative | one FQN | `likely` |
/// | any | more than one FQN answers | `candidate` |
/// | any | nothing answers | [`RESOLVED_UNRESOLVED`] |
///
/// The narrow `exact` mint is the point. `class Order < Base` inside
/// `module Reseller` is `Reseller::Base` when that constant exists and
/// `::Base` otherwise — a lookup Ruby performs at RUNTIME against the
/// loaded object space, which this index cannot perform and must not
/// pretend to. A root-anchored reference has no such freedom, and neither
/// does one written with an empty lexical nesting: Ruby resolves both from
/// `Object`.
pub fn resolve_reference(
    written: &str,
    cref_is_object: bool,
    matches: &[EntityDefRow],
) -> (Option<String>, &'static str) {
    if matches.is_empty() {
        return (None, RESOLVED_UNRESOLVED);
    }
    let distinct: BTreeSet<&str> = matches.iter().map(|r| r.fqn.as_str()).collect();
    if distinct.len() > 1 {
        // More than one constant answers — nothing is merged on a bare
        // name (the `entities/1` risk-3 posture), and an ambiguous
        // reference is a candidate, never a pick.
        return (None, CLASS_CANDIDATE);
    }
    let fqn = matches[0].fqn.clone();
    let root_anchored = written.starts_with("::");
    let unambiguous_lexically = root_anchored || cref_is_object;
    let mut best = CLASS_CANDIDATE;
    for r in matches {
        let stale = r.live_blob_hash.as_deref() != Some(r.blob_hash.as_str());
        let c = class_for(
            super::MATCHED_VIA_NESTING,
            &r.nesting,
            &r.zeitwerk_state,
            stale,
        );
        best = better(best, c);
    }
    let class = if unambiguous_lexically {
        best
    } else {
        cap(best, CLASS_LIKELY)
    };
    (Some(fqn), class)
}

fn rank(c: &str) -> u8 {
    match c {
        CLASS_EXACT => 3,
        CLASS_LIKELY => 2,
        CLASS_CANDIDATE => 1,
        _ => 0,
    }
}

fn better(a: &'static str, b: &'static str) -> &'static str {
    if rank(b) > rank(a) {
        b
    } else {
        a
    }
}

/// `min(class, ceiling)` over the ladder — the ONE way this module lowers
/// a class, so a cap can never be spelled as an accidental raise.
fn cap(class: &'static str, ceiling: &'static str) -> &'static str {
    if rank(class) > rank(ceiling) {
        ceiling
    } else {
        class
    }
}

// ---------------------------------------------------------------------------
// the builder
// ---------------------------------------------------------------------------

struct FileCtx {
    lines: Vec<String>,
    symbols: Vec<Symbol>,
    blob_sha: String,
}

struct Files<'a> {
    src: &'a dyn DossierSource,
    cache: HashMap<String, Option<FileCtx>>,
}

impl<'a> Files<'a> {
    fn new(src: &'a dyn DossierSource) -> Self {
        Self {
            src,
            cache: HashMap::new(),
        }
    }

    fn get(&mut self, path: &str) -> Option<&FileCtx> {
        if !self.cache.contains_key(path) {
            let built = self.src.read(path).map(|(bytes, blob_sha)| {
                let symbols = crate::extract::extract_symbols("ruby", &bytes).unwrap_or_default();
                FileCtx {
                    lines: rb::lines_of(&bytes),
                    symbols,
                    blob_sha,
                }
            });
            self.cache.insert(path.to_string(), built);
        }
        self.cache.get(path).and_then(|v| v.as_ref())
    }
}

fn namespace_of(fqn: &str) -> Option<String> {
    fqn.rfind("::").map(|i| fqn[..i].to_string())
}

/// `build_dossier`'s whole job, minus IO: fold the index's claims plus the
/// live bytes into `entity/1`.
pub fn build_dossier(
    repo: &str,
    ent: &str,
    opts: &DossierOptions,
    src: &dyn DossierSource,
    zw: &zeitwerk::Zeitwerk,
    usages: UsagesInput,
) -> Result<DossierOut, DossierRefusal> {
    let mut notes: Vec<String> = Vec::new();
    if zw.state == zeitwerk::STATE_DEGRADED {
        notes.push(match &zw.reason {
            Some(r) => format!(
                "zeitwerk read degraded: {r} — no definition block can be named the autoload \
                 canonical one, and every convention-derived name is capped at candidate"
            ),
            None => "zeitwerk read degraded — no definition block can be named the autoload \
                     canonical one, and every convention-derived name is capped at candidate"
                .to_string(),
        });
    }

    let rows = src.defs_for_name(ent);
    if rows.is_empty() {
        // The constant fallback: an address that names no class or module
        // may still name a CONSTANT assigned inside its parent's body.
        if let Some(out) = constant_dossier(repo, ent, opts, src, zw, notes.clone()) {
            return Ok(out);
        }
        return Err(DossierRefusal {
            status: 404,
            reason: REASON_ENTITY_UNKNOWN,
            message: format!(
                "no indexed class, module or constant answers to {ent:?} — the entity index \
                 covers Ruby definition sites only"
            ),
        });
    }

    let distinct: Vec<String> = {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        rows.iter().map(|r| r.fqn.clone()).for_each(|f| {
            seen.insert(f);
        });
        seen.into_iter().collect()
    };
    if distinct.len() > 1 {
        return Ok(ambiguous_dossier(repo, ent, opts, zw, distinct, notes));
    }

    let fqn = rows[0].fqn.clone();
    let mut files = Files::new(src);
    let mut budget = BudgetSpend::new(opts.budget);

    // --- definitions -------------------------------------------------------
    let mut ordered = rows.clone();
    ordered.sort_by(|a, b| {
        let ca = a.zeitwerk_fqn.as_deref() == Some(fqn.as_str());
        let cb = b.zeitwerk_fqn.as_deref() == Some(fqn.as_str());
        cb.cmp(&ca)
            .then(a.path.cmp(&b.path))
            .then(a.line_start.cmp(&b.line_start))
    });
    let distinct_paths: BTreeSet<&str> = ordered.iter().map(|r| r.path.as_str()).collect();
    if distinct_paths.len() > MAX_DEFINITION_FILES {
        notes.push(format!(
            "{} distinct files reopen {fqn} — only the first {MAX_DEFINITION_FILES} are read; \
             the rest are listed as definition sites without members",
            distinct_paths.len()
        ));
    }
    let readable: BTreeSet<String> = distinct_paths
        .iter()
        .take(MAX_DEFINITION_FILES)
        .map(|p| p.to_string())
        .collect();

    let has_canonical = ordered
        .iter()
        .any(|r| r.zeitwerk_fqn.as_deref() == Some(fqn.as_str()));
    if !has_canonical && zw.state != zeitwerk::STATE_DEGRADED {
        notes.push(format!(
            "no definition site of {fqn} sits at the path the app's Zeitwerk configuration \
             expects — the primary definition could not be determined, so every block keeps its \
             own keyword and none is called a reopening"
        ));
    }

    let mut kinds: BTreeSet<String> = BTreeSet::new();
    let mut definitions: Vec<DefinitionBlock> = Vec::new();
    let mut trust_counts = TrustCounts::default();
    let mut files_seen: Vec<String> = Vec::new();
    let mut missing_files = 0usize;
    for (i, r) in ordered.iter().enumerate() {
        kinds.insert(r.kind.clone());
        let stale = r.live_blob_hash.as_deref() != Some(r.blob_hash.as_str());
        let matched_via = if r.fqn == ent || last_segment(&r.fqn) == last_segment(ent) {
            super::MATCHED_VIA_NESTING
        } else {
            super::MATCHED_VIA_ZEITWERK
        };
        let trust = class_for(matched_via, &r.nesting, &r.zeitwerk_state, stale);
        match trust {
            CLASS_EXACT => trust_counts.exact += 1,
            CLASS_LIKELY => trust_counts.likely += 1,
            _ => trust_counts.candidate += 1,
        }
        let line_start = r.line_start.max(0) as u32;
        let line_end = r.line_end.max(0) as u32;
        let readable_here = readable.contains(&r.path);
        let ctx = if readable_here {
            files.get(&r.path)
        } else {
            None
        };
        let (opener, opener_form) = match ctx {
            Some(c) => rb::opener_form(&c.lines, &c.symbols, line_start),
            None => (String::new(), rb::OPENER_UNKNOWN),
        };
        let blob_sha = ctx.map(|c| c.blob_sha.clone());
        let missing = readable_here && ctx.is_none();
        if missing {
            missing_files += 1;
        }
        if !files_seen.iter().any(|p| p == &r.path) {
            files_seen.push(r.path.clone());
        }
        let canonical = has_canonical && r.zeitwerk_fqn.as_deref() == Some(fqn.as_str());
        let kind = if has_canonical && !canonical {
            BLOCK_REOPEN.to_string()
        } else {
            r.kind.clone()
        };
        definitions.push(DefinitionBlock {
            path: r.path.clone(),
            line_start,
            line_end,
            blob_sha,
            kind,
            reopening_index: i,
            opener,
            opener_form,
            trust,
            matched_via,
            nesting: r.nesting.clone(),
            stale,
            missing,
        });
    }
    if missing_files > 0 {
        notes.push(format!(
            "{missing_files} definition site(s) name a path with no live bytes — those blocks \
             contribute no members, hierarchy or holes"
        ));
    }

    let entity_kind: &'static str = if kinds.len() > 1 {
        notes.push(format!(
            "definition sites of {fqn} disagree about its kind ({}) — reported as unknown rather \
             than resolved to whichever row sorted first",
            kinds.iter().cloned().collect::<Vec<_>>().join(", ")
        ));
        ENTITY_KIND_UNKNOWN
    } else {
        match kinds.iter().next().map(|s| s.as_str()) {
            Some("class") => ENTITY_KIND_CLASS,
            Some("module") => ENTITY_KIND_MODULE,
            _ => ENTITY_KIND_UNKNOWN,
        }
    };

    if definitions.iter().all(|d| d.missing) && !definitions.is_empty() {
        let out = empty_dossier(
            repo,
            ent,
            &fqn,
            entity_kind,
            opts,
            zw,
            definitions,
            trust_counts,
            files_seen,
            notes,
            format!(
                "{fqn} is in the entity index, but none of its definition sites carries live \
                 bytes any more — the files were deleted or are not mirrored"
            ),
        );
        return Ok(out);
    }

    // --- members, holes, mixins, superclass -------------------------------
    let mut members: Vec<MemberRow> = Vec::new();
    let mut unknown_members: Vec<UnknownMember> = Vec::new();
    let mut mixin_refs: Vec<(rb::MixinRef, String)> = Vec::new();
    let mut superclasses: Vec<(String, String, u32, bool)> = Vec::new();
    let mut vis_refusals: Vec<String> = Vec::new();
    for d in &definitions {
        if d.missing || !readable.contains(&d.path) {
            continue;
        }
        let Some(ctx) = files.get(&d.path) else {
            continue;
        };
        let body = rb::direct_body_lines(&ctx.lines, &ctx.symbols, d.line_start, d.line_end);
        let vis = rb::scan_visibility(&ctx.lines, &body);
        if let Some(r) = &vis.refused {
            vis_refusals.push(format!("{}:{} — {r}", d.path, d.line_start));
        }
        let block_trust = d.trust;
        collect_members(&fqn, d, ctx, &body, &vis, block_trust, false, &mut members);
        for h in rb::scan_unknown_members(&ctx.lines, d.line_start, d.line_end) {
            unknown_members.push(UnknownMember {
                mechanism: h.mechanism,
                name_hint: h.name_hint,
                path: d.path.clone(),
                line: h.line,
                blob_sha: d.blob_sha.clone(),
                context: h.context,
            });
        }
        for m in rb::mixins_of(&ctx.lines, &body) {
            mixin_refs.push((m, d.path.clone()));
        }
        if let Some(sc) = rb::superclass_of(&ctx.lines, d.line_start) {
            superclasses.push((
                sc,
                d.path.clone(),
                d.line_start,
                d.opener_form == rb::OPENER_TOP_LEVEL || d.opener_form == rb::OPENER_COMPACT,
            ));
        }
    }
    for r in vis_refusals {
        notes.push(format!("visibility could not be resolved at {r}"));
    }

    // --- hierarchy ---------------------------------------------------------
    let mut hnotes: Vec<String> = Vec::new();
    let mut mixins: Vec<Mixin> = Vec::new();
    for (m, path) in &mixin_refs {
        // A mixin is evaluated INSIDE the block's own body, so its cref is
        // never `[Object]` — only a root-anchored `::M` is lexically
        // unambiguous here. (A superclass is different: `class Foo < Bar`
        // evaluates `Bar` in the ENCLOSING scope, which is why
        // `walk_ancestors` may pass `true`.)
        let matches = src.defs_for_name(m.written.trim_start_matches("::"));
        let (target, resolved) = resolve_reference(&m.written, false, &matches);
        mixins.push(Mixin {
            kind: m.kind,
            written: m.written.clone(),
            fqn: target,
            resolved,
            from_path: path.clone(),
            from_line: m.line,
        });
    }
    mixins.sort_by(|a, b| {
        a.from_path
            .cmp(&b.from_path)
            .then(a.from_line.cmp(&b.from_line))
    });

    let distinct_super: BTreeSet<&str> =
        superclasses.iter().map(|(s, _, _, _)| s.as_str()).collect();
    if distinct_super.len() > 1 {
        hnotes.push(format!(
            "definition sites of {fqn} name {} different superclasses ({}) — the chain is walked \
             from the first only",
            distinct_super.len(),
            distinct_super
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let mut ancestors: Vec<Ancestor> = Vec::new();
    if let Some((written, path, line, top_level)) = superclasses.first().cloned() {
        walk_ancestors(
            src,
            &mut files,
            &written,
            &path,
            line,
            top_level,
            &mut ancestors,
            &mut hnotes,
        );
    }

    let (descendants, implementors, usages_section) =
        project_usages(&usages, src, opts.usages_per_kind, &mut hnotes);

    // --- inherited members -------------------------------------------------
    if opts.inherited {
        let mut sources: Vec<(String, &'static str)> = Vec::new();
        for a in &ancestors {
            if let Some(f) = &a.fqn {
                sources.push((f.clone(), a.resolved));
            }
        }
        for m in &mixins {
            if let Some(f) = &m.fqn {
                sources.push((f.clone(), m.resolved));
            }
        }
        for (afqn, resolved) in sources {
            if resolved == RESOLVED_UNRESOLVED {
                continue;
            }
            let arows = src.defs_for_name(&afqn);
            for r in arows
                .iter()
                .filter(|r| r.fqn == afqn)
                .take(MAX_DEFINITION_FILES)
            {
                let line_start = r.line_start.max(0) as u32;
                let line_end = r.line_end.max(0) as u32;
                let stale = r.live_blob_hash.as_deref() != Some(r.blob_hash.as_str());
                // An inherited member is never `exact`: Ruby's method
                // lookup can be shadowed by a mixin this index cannot
                // order, so the row is capped one rung below whatever its
                // own block would otherwise prove.
                let block_trust = cap(
                    cap(
                        class_for(
                            super::MATCHED_VIA_NESTING,
                            &r.nesting,
                            &r.zeitwerk_state,
                            stale,
                        ),
                        resolved,
                    ),
                    CLASS_LIKELY,
                );
                let Some(ctx) = files.get(&r.path) else {
                    continue;
                };
                let body = rb::direct_body_lines(&ctx.lines, &ctx.symbols, line_start, line_end);
                let vis = rb::scan_visibility(&ctx.lines, &body);
                let block = DefinitionBlock {
                    path: r.path.clone(),
                    line_start,
                    line_end,
                    blob_sha: Some(ctx.blob_sha.clone()),
                    kind: r.kind.clone(),
                    reopening_index: 0,
                    opener: String::new(),
                    opener_form: rb::OPENER_UNKNOWN,
                    trust: block_trust,
                    matched_via: super::MATCHED_VIA_NESTING,
                    nesting: r.nesting.clone(),
                    stale,
                    missing: false,
                };
                collect_members(
                    &afqn,
                    &block,
                    ctx,
                    &body,
                    &vis,
                    block_trust,
                    true,
                    &mut members,
                );
            }
        }
    } else if !ancestors.is_empty() || !mixins.is_empty() {
        notes.push(
            "inherited members are not included — pass inherited=1 (`--inherited`) to merge the \
             ancestors' and mixins' own member tables into this one"
                .to_string(),
        );
    }

    members.sort_by(|a, b| {
        rb::visibility_rank(a.visibility)
            .cmp(&rb::visibility_rank(b.visibility))
            .then(a.name.cmp(&b.name))
            .then(a.defining_type.cmp(&b.defining_type))
            .then(a.path.cmp(&b.path))
            .then(a.line.cmp(&b.line))
    });
    unknown_members.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then(a.line.cmp(&b.line))
            .then(a.mechanism.cmp(b.mechanism))
    });

    // --- namespace tree ----------------------------------------------------
    let namespace_tree = build_namespace_tree(&fqn, src.all_defs());

    // --- budget ------------------------------------------------------------
    let mut out = DossierOut {
        schema: DOSSIER_SCHEMA,
        repo: repo.to_string(),
        ent: ent.to_string(),
        entity: EntitySummary {
            fqn: fqn.clone(),
            kind: entity_kind,
            namespace: namespace_of(&fqn),
            files: files_seen,
            trust_counts,
        },
        candidates: Vec::new(),
        definitions,
        members,
        hierarchy: Hierarchy {
            ancestors,
            mixins,
            descendants,
            implementors,
            notes: hnotes,
        },
        usages: usages_section,
        unknown_members,
        namespace_tree,
        zeitwerk: zeitwerk_out(zw),
        honesty: Honesty {
            state: STATE_OK,
            reason: None,
            budget: BudgetReport {
                requested: opts.budget,
                spent: 0,
                dropped: Dropped {
                    definitions: 0,
                    members: 0,
                    ancestors: 0,
                    mixins: 0,
                    descendants: 0,
                    implementors: 0,
                    unknown_members: 0,
                    namespace_tree: 0,
                    usages: 0,
                },
                order: LANE_PRIORITY.iter().copied().chain(["usages"]).collect(),
            },
            notes: Vec::new(),
        },
    };
    budget.apply(&mut out);
    finish_honesty(&mut out, notes);
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn collect_members(
    owner_fqn: &str,
    block: &DefinitionBlock,
    ctx: &FileCtx,
    body: &[u32],
    vis: &rb::VisibilityScan,
    block_trust: &'static str,
    inherited: bool,
    out: &mut Vec<MemberRow>,
) {
    let singletons =
        rb::singleton_blocks(&ctx.lines, &ctx.symbols, block.line_start, block.line_end);
    let in_singleton = |line: u32| singletons.iter().any(|(a, b)| line > *a && line <= *b);
    let children = rb::direct_children(&ctx.symbols, block.line_start, block.line_end);
    let mut push = |name: &str,
                    kind: &'static str,
                    visibility: &'static str,
                    line: u32,
                    via: &'static str,
                    trust: &'static str| {
        out.push(MemberRow {
            name: name.to_string(),
            kind,
            visibility,
            defining_type: owner_fqn.to_string(),
            inherited,
            path: block.path.clone(),
            line,
            blob_sha: block.blob_sha.clone(),
            via,
            trust,
        })
    };
    for s in &children {
        // A `class << self` block is invisible to range containment (see
        // `rb::singleton_blocks`), so its own `def`s reach this loop —
        // they are CLASS methods and are handled with the block below.
        if in_singleton(s.line_start) {
            continue;
        }
        let kind: &'static str = match s.kind.as_str() {
            "method" => "instance_method",
            "singleton_method" => "singleton_method",
            "alias" => "alias",
            _ => continue,
        };
        push(
            &s.name,
            kind,
            vis.for_member(&s.name, s.line_start),
            s.line_start,
            rb::VIA_TREE,
            block_trust,
        );
    }
    // The two singleton shapes, each with its OWN visibility scope: the
    // `class << self` line ranges this scanner recovered, and the
    // `class << Foo` blocks the symbols table does capture.
    let mut scopes: Vec<(u32, u32)> = singletons.clone();
    for sc in children.iter().filter(|s| s.kind == "singleton_class") {
        scopes.push((sc.line_start, sc.line_end));
    }
    for (a, b) in scopes {
        let sbody = rb::direct_body_lines(&ctx.lines, &ctx.symbols, a, b);
        let svis = rb::scan_visibility(&ctx.lines, &sbody);
        for m in rb::direct_children(&ctx.symbols, a, b) {
            if !matches!(m.kind.as_str(), "method" | "singleton_method" | "alias") {
                continue;
            }
            let kind = if m.kind == "alias" {
                "alias"
            } else {
                "singleton_method"
            };
            push(
                &m.name,
                kind,
                svis.for_member(&m.name, m.line_start),
                m.line_start,
                rb::VIA_TREE,
                block_trust,
            );
        }
        for sm in rb::scan_scanned_members(&ctx.lines, &sbody) {
            push(
                &sm.name,
                sm.kind,
                svis.for_member(&sm.name, sm.line),
                sm.line,
                sm.via,
                // A line scan proves less than a tree: never `exact`.
                cap(block_trust, CLASS_LIKELY),
            );
        }
    }
    for sm in rb::scan_scanned_members(&ctx.lines, body) {
        push(
            &sm.name,
            sm.kind,
            vis.for_member(&sm.name, sm.line),
            sm.line,
            sm.via,
            cap(block_trust, CLASS_LIKELY),
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_ancestors(
    src: &dyn DossierSource,
    files: &mut Files<'_>,
    first_written: &str,
    from_path: &str,
    from_line: u32,
    top_level: bool,
    out: &mut Vec<Ancestor>,
    notes: &mut Vec<String>,
) {
    let mut written = first_written.to_string();
    let mut path = from_path.to_string();
    let mut line = from_line;
    let mut lexically_unambiguous = top_level;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for depth in 1..=MAX_ANCESTOR_DEPTH {
        let matches = src.defs_for_name(written.trim_start_matches("::"));
        let (fqn, resolved) = resolve_reference(&written, lexically_unambiguous, &matches);
        out.push(Ancestor {
            written: written.clone(),
            fqn: fqn.clone(),
            resolved,
            depth,
            from_path: path.clone(),
            from_line: line,
        });
        let Some(fqn) = fqn else { return };
        if !seen.insert(fqn.clone()) {
            notes.push(format!(
                "the superclass chain revisits {fqn} — walked no further (a cycle the index \
                 cannot have, so the reference resolution is the thing to distrust)"
            ));
            return;
        }
        if depth == MAX_ANCESTOR_DEPTH {
            notes.push(format!(
                "the superclass chain is walked at most {MAX_ANCESTOR_DEPTH} deep — {fqn} may \
                 itself have a superclass this response does not name"
            ));
            return;
        }
        // Find the next link from the resolved ancestor's own blocks.
        let mut next: Option<(String, String, u32, bool)> = None;
        for r in matches.iter().filter(|r| r.fqn == fqn) {
            let ls = r.line_start.max(0) as u32;
            let Some(ctx) = files.get(&r.path) else {
                continue;
            };
            if let Some(sc) = rb::superclass_of(&ctx.lines, ls) {
                let (_, form) = rb::opener_form(&ctx.lines, &ctx.symbols, ls);
                next = Some((
                    sc,
                    r.path.clone(),
                    ls,
                    form == rb::OPENER_TOP_LEVEL || form == rb::OPENER_COMPACT,
                ));
                break;
            }
        }
        match next {
            Some((w, p, l, t)) => {
                written = w;
                path = p;
                line = l;
                lexically_unambiguous = t;
            }
            None => return,
        }
    }
}

/// Regroup the usages/2 answer by kind and split off the structural rows
/// the hierarchy lane owns. Copies each row VERBATIM — this function has
/// no way to change a `trust`, which is what makes the oracle bar
/// structural rather than a discipline.
fn project_usages(
    input: &UsagesInput,
    src: &dyn DossierSource,
    per_kind: usize,
    hnotes: &mut Vec<String>,
) -> (Vec<Related>, Vec<Related>, UsagesSection) {
    let Some(out) = &input.out else {
        let reason = input
            .unavailable_reason
            .clone()
            .unwrap_or_else(|| "the usages lane did not run".to_string());
        hnotes.push(format!(
            "descendants and implementors are derived from the usages lane, which did not run: \
             {reason}"
        ));
        return (
            Vec::new(),
            Vec::new(),
            UsagesSection {
                state: "error",
                reason: Some(reason),
                anchor: input.anchor.clone(),
                groups: Vec::new(),
                total: 0,
                truncated: false,
            },
        );
    };
    let mut by_kind: BTreeMap<&'static str, Vec<UsageRow2>> = BTreeMap::new();
    for r in out.exact.iter().chain(&out.likely).chain(&out.candidate) {
        by_kind.entry(r.kind.as_str()).or_default().push(r.clone());
    }
    let mut descendants: Vec<Related> = Vec::new();
    let mut implementors: Vec<Related> = Vec::new();
    let index = EnclosingIndex::new(src.all_defs());
    for (kind, via) in [
        (UsageKind::Inherit, "inherit"),
        (UsageKind::Include, "include"),
        (UsageKind::Prepend, "prepend"),
        (UsageKind::Extend, "extend"),
    ] {
        for r in by_kind.get(kind.as_str()).into_iter().flatten() {
            let related = Related {
                fqn: index.enclosing(&r.path, r.line),
                via,
                path: r.path.clone(),
                line: r.line,
                trust: r.trust,
            };
            if kind == UsageKind::Inherit {
                descendants.push(related);
            } else {
                implementors.push(related);
            }
        }
    }
    descendants.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
    implementors.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));

    let mut groups: Vec<UsageGroup> = Vec::new();
    let mut total = 0usize;
    let mut truncated = false;
    for k in UsageKind::ALL {
        let name = k.as_str();
        let Some(rows) = by_kind.get(name) else {
            continue;
        };
        // The engine's `kind_totals` counts BEFORE its own per-class cap;
        // fall back to what we hold when a kind is somehow absent from it.
        let true_total = out.kind_totals.get(name).copied().unwrap_or(rows.len());
        let kept: Vec<UsageRow2> = rows.iter().take(per_kind).cloned().collect();
        let mut census = TrustCounts::default();
        for r in &kept {
            match r.trust {
                CLASS_EXACT => census.exact += 1,
                CLASS_LIKELY => census.likely += 1,
                _ => census.candidate += 1,
            }
        }
        total += true_total;
        let group_truncated = true_total > kept.len();
        truncated |= group_truncated;
        groups.push(UsageGroup {
            kind: name,
            total: true_total,
            truncated: group_truncated,
            trust_census: census,
            census_basis: "returned",
            rows: kept,
        });
    }
    let state = if groups.is_empty() {
        STATE_EMPTY
    } else {
        STATE_OK
    };
    let reason = if groups.is_empty() {
        Some("the usages lane found no occurrence of this entity's name".to_string())
    } else {
        None
    };
    (
        descendants,
        implementors,
        UsagesSection {
            state,
            reason,
            anchor: input.anchor.clone(),
            groups,
            total,
            truncated,
        },
    )
}

/// path → the entity blocks in it, for "which entity encloses this line".
struct EnclosingIndex<'a> {
    by_path: HashMap<&'a str, Vec<&'a EntityDefRow>>,
}

impl<'a> EnclosingIndex<'a> {
    fn new(rows: &'a [EntityDefRow]) -> Self {
        let mut by_path: HashMap<&'a str, Vec<&'a EntityDefRow>> = HashMap::new();
        for r in rows {
            by_path.entry(r.path.as_str()).or_default().push(r);
        }
        Self { by_path }
    }

    /// The INNERMOST indexed entity whose block contains `line`.
    fn enclosing(&self, path: &str, line: u32) -> Option<String> {
        self.by_path
            .get(path)?
            .iter()
            .filter(|r| r.line_start <= i64::from(line) && r.line_end >= i64::from(line))
            .min_by_key(|r| r.line_end - r.line_start)
            .map(|r| r.fqn.clone())
    }
}

fn build_namespace_tree(fqn: &str, all: &[EntityDefRow]) -> Vec<NamespaceChild> {
    let prefix = format!("{fqn}::");
    let mut children: BTreeMap<String, (BTreeSet<String>, Vec<&EntityDefRow>)> = BTreeMap::new();
    for r in all {
        let Some(rest) = r.fqn.strip_prefix(&prefix) else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        let segment = rest.split("::").next().unwrap_or(rest).to_string();
        let child_fqn = format!("{prefix}{segment}");
        let entry = children.entry(segment).or_default();
        if r.fqn != child_fqn {
            entry.0.insert(r.fqn.clone());
        } else {
            entry.1.push(r);
        }
    }
    children
        .into_iter()
        .map(|(segment, (deeper, own))| {
            let mut counts = TrustCounts::default();
            for r in &own {
                let stale = r.live_blob_hash.as_deref() != Some(r.blob_hash.as_str());
                match class_for(
                    super::MATCHED_VIA_NESTING,
                    &r.nesting,
                    &r.zeitwerk_state,
                    stale,
                ) {
                    CLASS_EXACT => counts.exact += 1,
                    CLASS_LIKELY => counts.likely += 1,
                    _ => counts.candidate += 1,
                }
            }
            let kind = own
                .first()
                .map(|r| r.kind.clone())
                .unwrap_or_else(|| "namespace".to_string());
            NamespaceChild {
                fqn: format!("{prefix}{segment}"),
                segment,
                kind,
                definitions: own.len(),
                descendants: deeper.len(),
                trust_counts: counts,
            }
        })
        .collect()
}

fn zeitwerk_out(zw: &zeitwerk::Zeitwerk) -> ZeitwerkOut {
    ZeitwerkOut {
        state: zw.state,
        reason: zw.reason.clone(),
        roots: zw.roots.clone(),
        acronyms: zw.acronyms.clone(),
        collapse: zw.collapse.clone(),
    }
}

// --- the budget -------------------------------------------------------------

struct BudgetSpend {
    remaining: usize,
    requested: usize,
}

impl BudgetSpend {
    fn new(requested: usize) -> Self {
        Self {
            remaining: requested,
            requested,
        }
    }

    fn take<T>(&mut self, rows: &mut Vec<T>) -> usize {
        if rows.len() <= self.remaining {
            self.remaining -= rows.len();
            return 0;
        }
        let dropped = rows.len() - self.remaining;
        rows.truncate(self.remaining);
        self.remaining = 0;
        dropped
    }

    fn apply(&mut self, out: &mut DossierOut) {
        let mut dropped = Dropped {
            definitions: 0,
            members: 0,
            ancestors: 0,
            mixins: 0,
            descendants: 0,
            implementors: 0,
            unknown_members: 0,
            namespace_tree: 0,
            usages: 0,
        };
        dropped.definitions = self.take(&mut out.definitions);
        dropped.ancestors = self.take(&mut out.hierarchy.ancestors);
        dropped.mixins = self.take(&mut out.hierarchy.mixins);
        dropped.members = self.take(&mut out.members);
        dropped.unknown_members = self.take(&mut out.unknown_members);
        dropped.namespace_tree = self.take(&mut out.namespace_tree);
        dropped.descendants = self.take(&mut out.hierarchy.descendants);
        dropped.implementors = self.take(&mut out.hierarchy.implementors);
        for g in out.usages.groups.iter_mut() {
            dropped.usages += self.take(&mut g.rows);
            g.truncated = g.total > g.rows.len();
        }
        out.usages.truncated = out.usages.groups.iter().any(|g| g.truncated);
        out.honesty.budget.requested = self.requested;
        out.honesty.budget.spent = self.requested - self.remaining;
        out.honesty.budget.dropped = dropped;
    }
}

fn finish_honesty(out: &mut DossierOut, mut notes: Vec<String>) {
    let dropped = out.honesty.budget.dropped.total();
    if dropped > 0 {
        notes.push(format!(
            "the {}-row budget dropped {dropped} row(s) — every one is counted by lane in \
             honesty.budget.dropped, spent in the order honesty.budget.order names",
            out.honesty.budget.requested
        ));
    }
    let degraded = dropped > 0
        || out.usages.state != STATE_OK
        || out.definitions.iter().any(|d| d.missing || d.stale)
        || out.members.iter().any(|m| m.visibility == rb::VIS_UNKNOWN)
        || out.zeitwerk.state == zeitwerk::STATE_DEGRADED;
    if degraded {
        out.honesty.state = STATE_PARTIAL;
        out.honesty.reason = Some(
            "some lane of this dossier is incomplete or degraded — honesty.notes names each one"
                .to_string(),
        );
    }
    out.honesty.notes = notes;
}

// --- the three early-return shapes -----------------------------------------

fn empty_budget(requested: usize) -> BudgetReport {
    BudgetReport {
        requested,
        spent: 0,
        dropped: Dropped {
            definitions: 0,
            members: 0,
            ancestors: 0,
            mixins: 0,
            descendants: 0,
            implementors: 0,
            unknown_members: 0,
            namespace_tree: 0,
            usages: 0,
        },
        order: LANE_PRIORITY.iter().copied().chain(["usages"]).collect(),
    }
}

fn empty_usages(reason: &str) -> UsagesSection {
    UsagesSection {
        state: STATE_EMPTY,
        reason: Some(reason.to_string()),
        anchor: None,
        groups: Vec::new(),
        total: 0,
        truncated: false,
    }
}

fn ambiguous_dossier(
    repo: &str,
    ent: &str,
    opts: &DossierOptions,
    zw: &zeitwerk::Zeitwerk,
    candidates: Vec<String>,
    mut notes: Vec<String>,
) -> DossierOut {
    notes.push(format!(
        "{} distinct constants answer to {ent:?} — a dossier is about ONE entity, so nothing is \
         merged and nothing is picked; re-address with a full constant path from `candidates`",
        candidates.len()
    ));
    DossierOut {
        schema: DOSSIER_SCHEMA,
        repo: repo.to_string(),
        ent: ent.to_string(),
        entity: EntitySummary {
            fqn: ent.to_string(),
            kind: ENTITY_KIND_UNKNOWN,
            namespace: namespace_of(ent),
            files: Vec::new(),
            trust_counts: TrustCounts::default(),
        },
        candidates,
        definitions: Vec::new(),
        members: Vec::new(),
        hierarchy: Hierarchy {
            ancestors: Vec::new(),
            mixins: Vec::new(),
            descendants: Vec::new(),
            implementors: Vec::new(),
            notes: Vec::new(),
        },
        usages: empty_usages("the address is ambiguous — no single entity to report usages for"),
        unknown_members: Vec::new(),
        namespace_tree: Vec::new(),
        zeitwerk: zeitwerk_out(zw),
        honesty: Honesty {
            state: STATE_EMPTY,
            reason: Some(format!("{ent:?} is ambiguous")),
            budget: empty_budget(opts.budget),
            notes,
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn empty_dossier(
    repo: &str,
    ent: &str,
    fqn: &str,
    kind: &'static str,
    opts: &DossierOptions,
    zw: &zeitwerk::Zeitwerk,
    definitions: Vec<DefinitionBlock>,
    trust_counts: TrustCounts,
    files: Vec<String>,
    mut notes: Vec<String>,
    reason: String,
) -> DossierOut {
    notes.push(reason.clone());
    DossierOut {
        schema: DOSSIER_SCHEMA,
        repo: repo.to_string(),
        ent: ent.to_string(),
        entity: EntitySummary {
            fqn: fqn.to_string(),
            kind,
            namespace: namespace_of(fqn),
            files,
            trust_counts,
        },
        candidates: Vec::new(),
        definitions,
        members: Vec::new(),
        hierarchy: Hierarchy {
            ancestors: Vec::new(),
            mixins: Vec::new(),
            descendants: Vec::new(),
            implementors: Vec::new(),
            notes: Vec::new(),
        },
        usages: empty_usages("no live definition block to anchor the usages lane on"),
        unknown_members: Vec::new(),
        namespace_tree: Vec::new(),
        zeitwerk: zeitwerk_out(zw),
        honesty: Honesty {
            state: STATE_EMPTY,
            reason: Some(reason),
            budget: empty_budget(opts.budget),
            notes,
        },
    }
}

/// The constant fallback: `Reseller::Order::TAX` names no class or module,
/// but its parent's body may assign it. Answering that as a `constant`
/// entity is better than a 404 that reads as "no such thing".
fn constant_dossier(
    repo: &str,
    ent: &str,
    opts: &DossierOptions,
    src: &dyn DossierSource,
    zw: &zeitwerk::Zeitwerk,
    mut notes: Vec<String>,
) -> Option<DossierOut> {
    let parent = namespace_of(ent)?;
    let name = last_segment(ent).to_string();
    let rows = src.defs_for_name(&parent);
    if rows.is_empty() {
        return None;
    }
    let mut files = Files::new(src);
    let mut definitions: Vec<DefinitionBlock> = Vec::new();
    let mut seen_files: Vec<String> = Vec::new();
    for r in rows.iter().filter(|r| r.fqn == parent) {
        let ls = r.line_start.max(0) as u32;
        let le = r.line_end.max(0) as u32;
        let stale = r.live_blob_hash.as_deref() != Some(r.blob_hash.as_str());
        let Some(ctx) = files.get(&r.path) else {
            continue;
        };
        let body = rb::direct_body_lines(&ctx.lines, &ctx.symbols, ls, le);
        for sm in rb::scan_scanned_members(&ctx.lines, &body) {
            if sm.kind != "constant" || sm.name != name {
                continue;
            }
            if !seen_files.iter().any(|p| p == &r.path) {
                seen_files.push(r.path.clone());
            }
            definitions.push(DefinitionBlock {
                path: r.path.clone(),
                line_start: sm.line,
                line_end: sm.line,
                blob_sha: Some(ctx.blob_sha.clone()),
                kind: ENTITY_KIND_CONSTANT.to_string(),
                reopening_index: definitions.len(),
                opener: String::new(),
                opener_form: rb::OPENER_UNKNOWN,
                // An assignment found by a LINE scan inside a block: never
                // better than `likely`, whatever the block proves.
                trust: cap(
                    class_for(
                        super::MATCHED_VIA_NESTING,
                        &r.nesting,
                        &r.zeitwerk_state,
                        stale,
                    ),
                    CLASS_LIKELY,
                ),
                matched_via: super::MATCHED_VIA_NESTING,
                nesting: r.nesting.clone(),
                stale,
                missing: false,
            });
        }
    }
    if definitions.is_empty() {
        return None;
    }
    let mut counts = TrustCounts::default();
    for d in &definitions {
        match d.trust {
            CLASS_EXACT => counts.exact += 1,
            CLASS_LIKELY => counts.likely += 1,
            _ => counts.candidate += 1,
        }
    }
    notes.push(format!(
        "{ent:?} names no class or module — it is a CONSTANT assigned in {parent}'s body, found \
         by a line scan and therefore never better than likely"
    ));
    Some(DossierOut {
        schema: DOSSIER_SCHEMA,
        repo: repo.to_string(),
        ent: ent.to_string(),
        entity: EntitySummary {
            fqn: ent.to_string(),
            kind: ENTITY_KIND_CONSTANT,
            namespace: Some(parent),
            files: seen_files,
            trust_counts: counts,
        },
        candidates: Vec::new(),
        definitions,
        members: Vec::new(),
        hierarchy: Hierarchy {
            ancestors: Vec::new(),
            mixins: Vec::new(),
            descendants: Vec::new(),
            implementors: Vec::new(),
            notes: Vec::new(),
        },
        usages: empty_usages("a constant carries no member table and no usages anchor of its own"),
        unknown_members: Vec::new(),
        namespace_tree: Vec::new(),
        zeitwerk: zeitwerk_out(zw),
        honesty: Honesty {
            state: STATE_PARTIAL,
            reason: Some("the address resolved to a constant, not to an entity".to_string()),
            budget: empty_budget(opts.budget),
            notes,
        },
    })
}

// ---------------------------------------------------------------------------
// the route
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct DossierParams {
    pub repo: String,
    pub ent: String,
    #[serde(default)]
    pub worktree: Option<String>,
    /// `1`/`true`/`yes`/`on` merges the ancestors' and mixins' member
    /// tables into this one. A string rather than a `bool` because the
    /// documented address is `?inherited=1`, which `serde_urlencoded`
    /// will not read as a bool.
    #[serde(default)]
    pub inherited: Option<String>,
    /// Response ROW budget (default [`DEFAULT_BUDGET`], max
    /// [`MAX_BUDGET`]).
    #[serde(default)]
    pub budget: Option<usize>,
    /// Rows per usage KIND group (default [`DEFAULT_USAGES_PER_KIND`],
    /// max [`MAX_USAGES_PER_KIND`]).
    #[serde(default)]
    pub usages_per_kind: Option<usize>,
}

pub fn truthy(v: Option<&str>) -> bool {
    matches!(
        v.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

impl DossierParams {
    pub fn options(&self) -> DossierOptions {
        DossierOptions {
            inherited: truthy(self.inherited.as_deref()),
            budget: self.budget.unwrap_or(DEFAULT_BUDGET).clamp(1, MAX_BUDGET),
            usages_per_kind: self
                .usages_per_kind
                .unwrap_or(DEFAULT_USAGES_PER_KIND)
                .clamp(0, MAX_USAGES_PER_KIND),
        }
    }
}

/// The route's own [`DossierSource`]: the store's entity rows plus the
/// working tree, both read inside ONE blocking hop (the 2026-08-31
/// starvation incident's rule — a filesystem read must never happen on an
/// async worker).
struct StoreSource<'a> {
    store: &'a crate::store::Store,
    repo: &'a crate::config::RepoEntry,
    repo_id: i64,
    worktree: Option<String>,
    all: Vec<EntityDefRow>,
}

impl DossierSource for StoreSource<'_> {
    fn read(&self, path: &str) -> Option<(Vec<u8>, String)> {
        crate::routes::read_repo_file(self.repo, path, None)
            .ok()
            .map(|r| (r.bytes, r.blob_hash))
    }

    fn defs_for_name(&self, name: &str) -> Vec<EntityDefRow> {
        self.store
            .entity_defs_for_name(
                self.repo_id,
                self.worktree.as_deref(),
                name,
                super::MAX_DEFS_PER_QUERY,
            )
            .unwrap_or_default()
    }

    fn all_defs(&self) -> &[EntityDefRow] {
        &self.all
    }
}

/// `GET /api/entity/dossier?repo=&ent=[&worktree=][&inherited=1][&budget=]
/// [&usages_per_kind=]`.
///
/// An ordinary `auth_bearer` READ: it reads strictly less than `GET
/// /api/file` already does, mutates nothing and serves no transcript text,
/// so it sits on the same sub-router `/entity` does.
pub async fn dossier_route(
    axum::extract::State(state): axum::extract::State<crate::state::SharedState>,
    axum::extract::Query(params): axum::extract::Query<DossierParams>,
) -> Result<impl axum::response::IntoResponse, crate::routes::ApiError> {
    let (repo, repo_id) = crate::routes::find_repo(&state, &params.repo)?;
    let ent = super::validate_ent(&params.ent)?.to_string();
    let repo = repo.clone();
    let opts = params.options();
    let worktree = params.worktree.clone();
    let scopes = state.scopes.clone();
    let repo_bg = repo.clone();
    let repo_name = repo.name.clone();
    let ent_bg = ent.clone();

    let built = state
        .store
        .run_blocking(move |store| {
            let all = store
                .entity_defs_for_repo(repo_id, worktree.as_deref(), MAX_REPO_DEFS)
                .unwrap_or_default();
            let src = StoreSource {
                store,
                repo: &repo_bg,
                repo_id,
                worktree,
                all,
            };
            let zw = zeitwerk::zeitwerk_for(&repo_bg.path);
            let usages = usages_for_entity(store, &repo_bg, repo_id, &ent_bg, &src, &scopes);
            build_dossier(&repo_name, &ent_bg, &opts, &src, &zw, usages)
        })
        .await;

    let out = built.map_err(|r| {
        let e = if r.status == 404 {
            crate::routes::ApiError::not_found(r.message)
        } else {
            crate::routes::ApiError::bad_request(r.message)
        };
        e.with_reason(r.reason)
    })?;
    Ok((
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        axum::Json(out),
    ))
}

/// Anchor the usages/2 ENGINE on the entity's own name in its first
/// readable definition block. Returns the honest reason instead when there
/// is no such anchor — never a fabricated empty list.
fn usages_for_entity(
    store: &crate::store::Store,
    repo: &crate::config::RepoEntry,
    repo_id: i64,
    ent: &str,
    src: &dyn DossierSource,
    scopes: &crate::config::ScopesSection,
) -> UsagesInput {
    let rows = src.defs_for_name(ent);
    let Some(first) = rows.first() else {
        return UsagesInput {
            unavailable_reason: Some("no definition site to anchor on".to_string()),
            ..Default::default()
        };
    };
    let name = last_segment(&first.fqn).to_string();
    let line = first.line_start.max(1) as u32;
    let Some((bytes, _)) = src.read(&first.path) else {
        return UsagesInput {
            unavailable_reason: Some(format!("{} carries no live bytes", first.path)),
            ..Default::default()
        };
    };
    let lines = rb::lines_of(&bytes);
    let Some(col) = lines
        .get((line - 1) as usize)
        .and_then(|l| rb::word_col(l, &name))
    else {
        return UsagesInput {
            unavailable_reason: Some(format!(
                "{}:{line} does not spell {name} — the usages lane needs a name position",
                first.path
            )),
            ..Default::default()
        };
    };
    let anchor = UsageAnchor {
        path: first.path.clone(),
        line,
        col,
    };
    match crate::usages2::usages2_at(
        store,
        repo,
        repo_id,
        &first.path,
        line,
        col,
        None,
        scopes,
        crate::usages2::MAX_LIMIT,
    ) {
        Ok(out) => UsagesInput {
            out: Some(out),
            anchor: Some(anchor),
            unavailable_reason: None,
        },
        Err(e) => UsagesInput {
            out: None,
            anchor: Some(anchor),
            unavailable_reason: Some(e.message().to_string()),
        },
    }
}

// --- the surface declaration ------------------------------------------------

pub const DOSSIER_ROUTE: super::RouteContract = super::RouteContract {
    path: "/api/entity/dossier",
    handler: "entities::dossier::dossier_route",
    required_params: &["repo", "ent"],
    params_accept_without: dossier_params_accept_without,
};

fn dossier_params_accept_without(omit: &str) -> bool {
    let mut map = serde_json::Map::new();
    for (k, v) in [
        ("repo", serde_json::Value::String("r".into())),
        ("ent", serde_json::Value::String("Foo".into())),
        ("worktree", serde_json::Value::String(String::new())),
        ("inherited", serde_json::Value::String("1".into())),
        ("budget", serde_json::Value::from(50u64)),
        ("usages_per_kind", serde_json::Value::from(5u64)),
    ] {
        if k != omit {
            map.insert(k.to_string(), v);
        }
    }
    serde_json::from_value::<DossierParams>(serde_json::Value::Object(map)).is_ok()
}

/// Every route unit V72-G1.1 adds. Walked from BOTH sides, exactly as
/// `entities::V71_G0_ROUTES` is.
pub const V72_G1_ROUTES: &[super::RouteContract] = &[DOSSIER_ROUTE];
