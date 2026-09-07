//! V71-G0 — `entities/1`: the entity index and its `?ent=` address.
//!
//! An "entity" here is a Ruby constant that names a class or a module. The
//! evidence report's thesis (`/tmp/kbc7/research/module-class-explorer.md`)
//! is that in a Rails monolith an entity is *definitionally scattered* —
//! `module Reseller` lives in `app/models/reseller/`,
//! `app/controllers/reseller/`, `app/services/reseller/` — so the file is
//! the wrong unit of comprehension. This module is the INDEX that a later
//! unit's entity page (D6) reads; it is not the page.
//!
//! ## What is stored, and what is computed
//!
//! Rows in `entity_defs` (migration V0029) are CLAIMS, never classes: each
//! records one definition site, the FQN the TREE proves for it (the
//! literal `class`/`module` nesting in that file), the FQN the app's
//! Zeitwerk configuration would derive for the PATH, what that
//! configuration read was worth, and the blob the claim came from. The
//! trust class is computed per request by [`class_for`] from four inputs —
//! which arm of the query matched, how completely the tree determines the
//! nesting, the config state, and whether the blob is still the live one —
//! and is NEVER persisted. That is root invariant
//! #2's posture (kb extracts hints, kb-code mints classes, nothing is
//! cached) and design §P8's ("facts persist as claims ... with no class
//! column; class = min(lane ceiling, anchor state, freshness) computed per
//! request in one function").
//!
//! The ladder is deliberately narrow: the ONLY way to reach `exact` is a
//! definition addressed by the name the tree literally nests, whose
//! nesting the tree fully determines, whose blob is still current. A
//! convention-derived name — however plausible — is capped at `likely`,
//! and at `candidate` when the Zeitwerk read itself degraded. "A wrong
//! exact is a release blocker", and no path→constant convention can prove
//! anything about a tree.
//!
//! The narrowest case is worth naming because it nearly shipped as a wrong
//! `exact`: tree-sitter-ruby's `tags.scm` captures only the LAST constant
//! of a compact definition, so `class Reseller::Order` reaches the
//! `symbols` table as the bare name `Order`. [`defs_for_file`] recovers the
//! dropped scope from the definition's own source line, and marks the one
//! shape that recovery cannot settle — a compact path INSIDE an enclosing
//! module, which Ruby resolves at runtime — as [`NESTING_AMBIGUOUS`], which
//! [`class_for`] structurally cannot turn into `exact`.
//!
//! ## Why the FQN comes out of the `symbols` table
//!
//! [`defs_for_file`] reconstructs each definition's nesting chain from
//! RANGE CONTAINMENT over the rows the symbols pass already produced (an
//! enclosing `class`/`module` both starts before and ends after everything
//! nested in it, and — since `extract.rs` emits in byte order — always
//! carries a smaller `ordinal`). That is a second read of the same tree's
//! output, not a second parse of the tree: the ingest hook adds one
//! `symbols_for_blob` lookup per Ruby file and no new tree-sitter work.
//! `extract.rs`'s `container` field cannot do this job — it names only the
//! NEAREST enclosing construct, so `Reseller::Billing::Order` would come
//! back as `Billing`.
//!
//! ## Deliberately not here (cut, with reasons)
//!
//! - `entity_edges` (superclass / include / prepend / extend). Ruby is not
//!   one of `hierarchy::supports_hierarchy`'s proof languages, so these
//!   need their own CST walk — which the plan already assigns to unit E1
//!   ("hierarchy incl. include/prepend/extend"). Creating the table here
//!   with no writer is the v7.0 dead-surface defect, so it is not created.
//! - Members (`Foo#bar`, `Foo.bar`). The member table is D6/G1's; the
//!   address parser ([`validate_ent`]) recognises the two member spellings
//!   only so it can REFUSE them by name instead of 404ing as if the
//!   constant did not exist.
//! - `opener_form` (`class_eval` / `concern`). Not derivable from the
//!   symbols table; it needs the same CST walk the edges do.

/// V72-G1.1 — `entity/1`: the entity DOSSIER (`GET /api/entity/dossier`).
/// A SIBLING of `entities/1` above, not a widening of it: the index
/// answers an ADDRESSING question and legitimately returns many entities,
/// while a dossier is everything about exactly one. See that module's own
/// doc for the six rules it is built around.
pub mod dossier;
/// V72-G1.1 — the per-request Ruby BODY scanners the dossier reads
/// (visibility, `attr_*`, constants, mixins, metaprogramming holes). Pure,
/// and capped at `likely` by construction: a line scan proves less than a
/// tree does.
pub mod ruby_body;
pub mod zeitwerk;

use crate::extract::Symbol;
use crate::routes::{find_repo, ApiError};
use crate::state::SharedState;
use crate::store::StoreBlocking;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub const ENTITY_SCHEMA: &str = "entities/1";

/// The two symbol kinds that name a constant. `singleton_class` (`class <<
/// self`) is deliberately absent: its captured name is the RECEIVER
/// (`self`), not a constant, so it names no entity.
pub const ENTITY_KINDS: [&str; 2] = ["class", "module"];

/// Per-file cap on indexed definition sites — a bound on one pathological
/// generated file, in the same spirit as `lenses::DECLARATION_CAP`.
pub const MAX_DEFS_PER_FILE: usize = 500;

/// Per-query cap on returned definition sites.
pub const MAX_DEFS_PER_QUERY: usize = 500;

/// Which arm of the `?ent=` lookup matched a row — the first input to
/// [`class_for`].
pub const MATCHED_VIA_NESTING: &str = "nesting";
pub const MATCHED_VIA_ZEITWERK: &str = "zeitwerk";

/// How completely the TREE determines a definition's FQN — the second
/// input to [`class_for`], and the reason this unit does not mint a wrong
/// `exact`. tree-sitter-ruby's tags query captures only the LAST constant
/// of a compact path (`class Reseller::Order` is captured as the name
/// `Order`), so [`defs_for_file`] recovers the dropped scope from the
/// definition's own source line. That recovery is exact at the top level
/// — but a compact path nested INSIDE a module is a constant lookup Ruby
/// performs at RUNTIME (`module M; class A::B` is `M::A::B` when `M::A`
/// exists and `::A::B` otherwise), which no tree can settle.
pub const NESTING_LEXICAL: &str = "lexical";
pub const NESTING_AMBIGUOUS: &str = "ambiguous";

/// How the QUERY reached the entity: its own fully-qualified name, or the
/// last segment of one (`Order` → `Reseller::Order`). Orthogonal to trust:
/// addressing an entity loosely does not weaken what its definition sites
/// prove, it only makes the ANSWER possibly ambiguous — which is reported
/// as `ambiguous`, never resolved first-wins (the evidence report's risk 3,
/// and kb's own `(kb, id)` keying discipline).
pub const MATCHED_BY_FQN: &str = "fqn";
pub const MATCHED_BY_LAST_SEGMENT: &str = "last-segment";

/// `true` for a language this unit indexes entities for. Ruby only, and
/// deliberately so: the Zeitwerk path→constant convention is Ruby's, and a
/// language whose FQN rules this module has not implemented must produce no
/// rows rather than plausible-looking wrong ones.
pub fn indexes_lang(lang_id: &str) -> bool {
    lang_id == "ruby"
}

pub fn is_entity_kind(kind: &str) -> bool {
    ENTITY_KINDS.contains(&kind)
}

/// The trust ladder, computed per request and never persisted.
///
/// | matched via | nesting | zeitwerk state | blob | class |
/// |---|---|---|---|---|
/// | nesting  | lexical   | any      | live  | `exact` |
/// | nesting  | ambiguous | any      | live  | `likely` |
/// | nesting  | any       | any      | stale | `likely` |
/// | zeitwerk | any       | read     | live  | `likely` |
/// | zeitwerk | any       | read     | stale | `candidate` |
/// | zeitwerk | any       | degraded | any   | `candidate` |
///
/// `exact` is reachable from exactly ONE row of that table: a definition
/// addressed by the name the tree literally nests, whose nesting the tree
/// fully determines, whose indexed blob is still the live one. A
/// convention never mints one; neither does a runtime constant lookup this
/// index cannot perform; neither does a claim about bytes that have since
/// changed.
pub fn class_for(
    matched_via: &str,
    nesting: &str,
    zeitwerk_state: &str,
    stale: bool,
) -> &'static str {
    if stale {
        // A claim about bytes that are no longer there can never be
        // better than its own evidence kind, one rung down.
        return if matched_via == MATCHED_VIA_NESTING {
            crate::resolve::CLASS_LIKELY
        } else {
            crate::resolve::CLASS_CANDIDATE
        };
    }
    match (matched_via, nesting, zeitwerk_state) {
        (MATCHED_VIA_NESTING, NESTING_LEXICAL, _) => crate::resolve::CLASS_EXACT,
        (MATCHED_VIA_NESTING, _, _) => crate::resolve::CLASS_LIKELY,
        (MATCHED_VIA_ZEITWERK, _, zeitwerk::STATE_READ) => crate::resolve::CLASS_LIKELY,
        _ => crate::resolve::CLASS_CANDIDATE,
    }
}

// --- extraction (pure) ------------------------------------------------------

/// One definition site, as extracted — the shape `Store::
/// replace_entity_defs` writes. Carries no trust class by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityDefClaim {
    /// The FQN the tree proves: literal `class`/`module` nesting,
    /// including any compact scope recovered from the source line.
    pub fqn: String,
    /// `"class"` | `"module"`.
    pub kind: String,
    /// [`NESTING_LEXICAL`] | [`NESTING_AMBIGUOUS`] — how completely the
    /// tree determines [`Self::fqn`].
    pub nesting: &'static str,
    pub line_start: u32,
    pub line_end: u32,
    /// The constant this PATH is expected to define under the app's
    /// Zeitwerk configuration, attached to the one definition that answers
    /// to it. `None` on every other definition in the file, and on every
    /// definition in a file under no autoload root.
    pub zeitwerk_fqn: Option<String>,
}

/// Every class/module definition site in one file, with its nesting-proved
/// FQN. Pure: `source` is the file's own bytes, `symbols` its symbols-pass
/// output and `zeitwerk` the app's already-read configuration.
///
/// `source` is needed for ONE thing — [`compact_scope`] — and the reason is
/// worth stating: tree-sitter-ruby's `tags.scm` captures a compact
/// definition's LAST constant only (`(scope_resolution name: (_) @name)`),
/// so `class Reseller::Order` reaches the `symbols` table as the bare name
/// `Order`. Trusting that name would have this index claim, at `exact`,
/// that a top-level `Order` is defined in a file that defines no such
/// thing — a wrong `exact`, which is a release blocker. The scope is
/// recovered from the definition's own source line instead.
pub fn defs_for_file(
    rel_path: &str,
    source: &[u8],
    symbols: &[Symbol],
    zeitwerk: &zeitwerk::Zeitwerk,
) -> Vec<EntityDefClaim> {
    let mut entities: Vec<&Symbol> = symbols
        .iter()
        .filter(|s| is_entity_kind(&s.kind) && is_constant_path(&s.name))
        .collect();
    entities.sort_by_key(|s| s.ordinal);

    let mut out: Vec<EntityDefClaim> = Vec::new();
    for (i, sym) in entities.iter().enumerate() {
        // The NEAREST enclosing definition: it spans this one's whole
        // range and — since `extract.rs` emits in byte order — was
        // emitted before it, so the last such entry is the innermost.
        // Its already-computed FQN is used, not its raw captured NAME:
        // an enclosing `class Reseller::Order` is captured as `Order`,
        // and a chain built from names would place its nested `Line` at
        // `Order::Line`.
        let enclosing: Option<&str> = (0..i)
            .rev()
            .find(|j| {
                entities[*j].line_start <= sym.line_start && entities[*j].line_end >= sym.line_end
            })
            .map(|j| out[j].fqn.as_str());
        let compact = nth_line(source, sym.line_start).and_then(|line| {
            // V72-G1.1 — the name's OWN column, recovered from the line.
            // See [`name_col_on_line`]: `Symbol::col_start` is the
            // DEFINITION node's column, so passing it here made this
            // whole recovery inert.
            let col = name_col_on_line(line, &sym.name).unwrap_or(sym.col_start as usize);
            compact_scope(line, col)
        });
        let (chain, nesting): (Vec<&str>, &'static str) = match &compact {
            // `class ::Order` — root-anchored: the enclosing modules are
            // explicitly NOT part of the name, and the tree says so.
            Some(prefix) if prefix.is_empty() => (vec![sym.name.as_str()], NESTING_LEXICAL),
            // `class A::B` at the top level: unambiguous. Nested inside a
            // module, Ruby resolves `A` at runtime — the lexically-nearest
            // reading is recorded, and marked as the guess it is.
            Some(prefix) => {
                let mut chain: Vec<&str> = enclosing.into_iter().collect();
                chain.extend(prefix.iter().map(|s| s.as_str()));
                chain.push(sym.name.as_str());
                let nesting = if enclosing.is_none() {
                    NESTING_LEXICAL
                } else {
                    NESTING_AMBIGUOUS
                };
                (chain, nesting)
            }
            None => {
                let mut chain: Vec<&str> = enclosing.into_iter().collect();
                chain.push(sym.name.as_str());
                (chain, NESTING_LEXICAL)
            }
        };
        out.push(EntityDefClaim {
            fqn: join_constant_path(&chain),
            kind: sym.kind.clone(),
            nesting,
            line_start: sym.line_start,
            line_end: sym.line_end,
            zeitwerk_fqn: None,
        });
        if out.len() >= MAX_DEFS_PER_FILE {
            break;
        }
    }

    // The Zeitwerk expectation belongs to the FILE, so it is attached to
    // the ONE definition that answers to it — preferring a whole-FQN
    // agreement, falling back to a last-segment match (the shape a
    // collapsed directory or a mis-namespaced file produces). No match at
    // all leaves every row's `zeitwerk_fqn` `None`: the honest "the
    // convention expected something this file does not contain" state,
    // never a row invented to satisfy the convention.
    if let Some(expected) = zeitwerk.constant_for_path(rel_path) {
        let idx = out.iter().position(|d| d.fqn == expected).or_else(|| {
            let want = last_segment(&expected);
            out.iter().position(|d| last_segment(&d.fqn) == want)
        });
        if let Some(idx) = idx {
            out[idx].zeitwerk_fqn = Some(expected);
        }
    }
    out
}

/// Join a nesting chain into one constant path, flattening the compact
/// form: `class Reseller::Order` inside `module Api` is captured as the
/// single name `Reseller::Order`, so the chain is joined and then
/// re-split rather than blindly concatenated.
fn join_constant_path(chain: &[&str]) -> String {
    chain
        .iter()
        .flat_map(|c| c.split("::"))
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("::")
}

pub(crate) fn last_segment(fqn: &str) -> &str {
    fqn.rsplit("::").next().unwrap_or(fqn)
}

/// The 1-based `n`th line of `source` as UTF-8, or `None` when the line is
/// past the end or is not valid UTF-8 (`ingest::index_file` has already
/// rejected non-UTF-8 files, so the second case is unreachable in
/// practice — it is still handled rather than unwrapped).
fn nth_line(source: &[u8], n: u32) -> Option<&str> {
    let idx = (n as usize).checked_sub(1)?;
    let line = source.split(|b| *b == b'\n').nth(idx)?;
    std::str::from_utf8(line).ok()
}

/// The 0-based BYTE column of a definition's own captured NAME on its
/// opener line.
///
/// **V72-G1.1 defect fix.** [`crate::extract::Symbol::col_start`] is the
/// DEFINITION node's column (the `class`/`module` keyword), never the
/// captured name's — `extract_symbols` reads both columns off
/// `c.node`, the definition node. Passing it to [`compact_scope`] made
/// that function look at the line's leading INDENTATION, which can never
/// end in `::`, so the compact-scope recovery this module's own doc
/// describes returned `None` for every real file and was inert from the
/// day it shipped: `class Reseller::Order` was indexed as a top-level
/// `Order`, at `exact` — the wrong-`exact` the recovery exists to
/// prevent. The V71-G0 unit tests missed it because they hand-set
/// `col_start` to the name column, a value the real extractor never
/// produces.
///
/// Whole-word so `class Order < OrderBase` finds `Order` at the name, not
/// inside the superclass; `None` (falling back to `col_start`, i.e. the
/// pre-fix behaviour) when the name is not spelled on that line at all.
fn name_col_on_line(line: &str, name: &str) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    let hay = line.as_bytes();
    let needle = name.as_bytes();
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut i = 0usize;
    while i + needle.len() <= hay.len() {
        if &hay[i..i + needle.len()] == needle {
            let before_ok = i == 0 || !ident(hay[i - 1]);
            let after = i + needle.len();
            let after_ok = after >= hay.len() || !ident(hay[after]);
            if before_ok && after_ok {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// The compact scope a definition's own source line spells before its
/// captured name — `["Reseller"]` for `class Reseller::Order`, an EMPTY
/// vec for the root-anchored `class ::Order`, and `None` when there is no
/// `::` before the name at all (the ordinary `class Order`).
///
/// `name_col` is the captured name node's 0-based BYTE offset within the
/// line (`Symbol::col_start`'s own convention), so the text before it is
/// exactly what the parser skipped. Pure text: a header this function
/// cannot read yields `None`, which degrades to the plain nesting chain
/// rather than to a guess.
fn compact_scope(line: &str, name_col: usize) -> Option<Vec<String>> {
    let head = line.get(..name_col)?.trim_end();
    let base = head.strip_suffix("::")?;
    // Walk back over the constant path immediately preceding the `::`.
    let start = base
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_alphanumeric() || *c == '_' || *c == ':')
        .map(|(i, _)| i)
        .last()
        .unwrap_or(base.len());
    Some(
        base[start..]
            .split("::")
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect(),
    )
}

/// `true` for a Ruby constant path: one or more `::`-separated segments,
/// each starting with an ASCII uppercase letter and continuing in
/// `[A-Za-z0-9_]`. Also the `?ent=` address grammar (see [`validate_ent`])
/// — one rule, used by both the extractor and the address parser, so a
/// name the index can hold is exactly a name a caller can ask for.
pub fn is_constant_path(s: &str) -> bool {
    !s.is_empty()
        && s.split("::").all(|seg| {
            let mut chars = seg.chars();
            matches!(chars.next(), Some(c) if c.is_ascii_uppercase())
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

// --- the worktree key -------------------------------------------------------

fn worktree_cache() -> &'static Mutex<HashMap<PathBuf, String>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The `entity_defs.worktree` key for a checkout: `""` for a main worktree
/// (and for anything git cannot open), the LINKED WORKTREE NAME otherwise —
/// the last component of gix's private `git_dir`
/// (`<main>/.git/worktrees/<name>`), which is exactly what `git worktree
/// add` names it.
///
/// Cached for the process lifetime keyed on the root: a checkout does not
/// become a different worktree while the daemon runs, and the ingest path
/// asks this once per Ruby file.
pub fn worktree_key_for(repo_root: &Path) -> String {
    // Two separate lock acquisitions, deliberately: `GitRepo::open` does
    // filesystem work, and holding this mutex across it would serialise
    // every Ruby file in a reconcile behind one `gix::discover`. Two
    // threads racing to compute the same key is harmless — the derivation
    // is pure and idempotent, so the loser simply overwrites an identical
    // value.
    if let Some(hit) = worktree_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(repo_root)
    {
        return hit.clone();
    }
    let key = crate::git::GitRepo::open(repo_root)
        .ok()
        .filter(|g| g.is_worktree())
        .and_then(|g| {
            g.git_dir()
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default();
    worktree_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(repo_root.to_path_buf(), key.clone());
    key
}

// --- the `?ent=` address ----------------------------------------------------

/// Validate a `?ent=` address. Accepts a constant path only. The two
/// MEMBER spellings the design's address grammar reserves (`Foo#bar`,
/// `Foo.bar`) are recognised solely to refuse them by name — a member is
/// the member table's business (D6/G1), and a bare 404 would read as "no
/// such constant", which is a different and wrong answer.
pub fn validate_ent(raw: &str) -> Result<&str, ApiError> {
    let ent = raw.trim();
    if ent.is_empty() {
        return Err(ApiError::bad_request("ent must not be empty"));
    }
    if ent.contains('#') || (ent.contains('.') && !ent.contains("..")) {
        return Err(ApiError::bad_request(format!(
            "member addressing is not resolvable yet: {ent:?} names a member, and the entity index carries class/module definition sites only"
        )));
    }
    if !is_constant_path(ent) {
        return Err(ApiError::bad_request(format!(
            "ent must be a Ruby constant path (Foo, Foo::Bar), got {ent:?}"
        )));
    }
    Ok(ent)
}

// --- the route --------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct EntityParams {
    pub repo: String,
    pub ent: String,
    /// Optional checkout filter — the `entity_defs.worktree` key. ABSENT
    /// means every checkout this repo id covers (today always exactly one
    /// — see the migration's doc on why the column exists before it can
    /// vary); an explicit empty string is a real filter value, naming the
    /// MAIN worktree.
    #[serde(default)]
    pub worktree: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EntityDefOut {
    pub path: String,
    pub line_start: i64,
    pub line_end: i64,
    pub kind: String,
    /// `exact` | `likely` | `candidate` — computed for THIS request by
    /// [`class_for`], never read from storage.
    pub trust: &'static str,
    /// `nesting` | `zeitwerk` — which name the query matched.
    pub matched_via: &'static str,
    /// `lexical` | `ambiguous` — how completely the tree determines this
    /// definition's FQN (see [`NESTING_AMBIGUOUS`]). Surfaced rather than
    /// folded silently into `trust`: a reader deserves to know WHY a
    /// definition that looks tree-proved is only `likely`.
    pub nesting: String,
    /// The constant the path convention derives, when this row carries
    /// one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zeitwerk_fqn: Option<String>,
    /// `true` when the indexed blob is no longer the one at this path —
    /// the claim is about bytes that have changed since, which is why it
    /// can no longer be `exact`.
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct EntityOut {
    pub fqn: String,
    pub kind: String,
    /// A CENSUS, never an aggregate verdict: an entity with one `exact`
    /// and one `candidate` definition site has both, and saying so is more
    /// useful (and more honest) than collapsing it to either.
    pub trust_counts: TrustCounts,
    pub definitions: Vec<EntityDefOut>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TrustCounts {
    pub exact: usize,
    pub likely: usize,
    pub candidate: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ZeitwerkOut {
    /// `read` | `degraded`.
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub roots: Vec<String>,
    pub acronyms: Vec<String>,
    pub collapse: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EntityQueryOut {
    pub schema: &'static str,
    pub repo: String,
    pub ent: String,
    /// `fqn` | `last-segment` — how the query reached these entities.
    pub matched_by: &'static str,
    /// `true` when more than one distinct FQN answers to this address. The
    /// caller is given every one of them; nothing is merged on a bare name
    /// (evidence report risk 3).
    pub ambiguous: bool,
    pub entities: Vec<EntityOut>,
    pub zeitwerk: ZeitwerkOut,
    pub truncated: bool,
    /// Honest empty/degraded captions — always present (possibly empty),
    /// never a silent absence.
    pub notes: Vec<String>,
}

/// `GET /api/entity?repo=&ent=[&worktree=]` — every definition site of one
/// entity, with a per-site trust class.
pub async fn entity_route(
    State(state): State<SharedState>,
    Query(params): Query<EntityParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let ent = validate_ent(&params.ent)?.to_string();
    let repo_root = repo.path.clone();
    let repo_name = repo.name.clone();
    let worktree = params.worktree.clone();

    let ent_q = ent.clone();
    // 2026-08-31 incident (store.rs module doc): ONE hop to the blocking
    // pool. The Zeitwerk read belongs inside it too — it is cached, but a
    // cold call `stat`s four paths and reads up to three files, which is
    // filesystem work that must never happen on an async worker.
    let (rows, zw) = state
        .store
        .run_blocking(move |store| {
            let rows = store.entity_defs_for_name(
                repo_id,
                worktree.as_deref(),
                &ent_q,
                MAX_DEFS_PER_QUERY + 1,
            )?;
            Ok::<_, ApiError>((rows, zeitwerk::zeitwerk_for(&repo_root)))
        })
        .await?;

    let mut notes: Vec<String> = Vec::new();
    if zw.state == zeitwerk::STATE_DEGRADED {
        notes.push(match &zw.reason {
            Some(r) => format!("zeitwerk read degraded: {r} — every convention-derived name is capped at candidate"),
            None => "zeitwerk read degraded — every convention-derived name is capped at candidate".to_string(),
        });
    }
    let out = build_entity_query(&repo_name, &ent, rows, &zw, notes);
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// The pure half of [`entity_route`]: fold store rows into the wire shape,
/// classing each row as it goes. Split out so the whole response — trust
/// ladder, census, ambiguity, truncation, captions — is unit-testable
/// without a daemon.
pub fn build_entity_query(
    repo: &str,
    ent: &str,
    mut rows: Vec<crate::store::EntityDefRow>,
    zw: &zeitwerk::Zeitwerk,
    mut notes: Vec<String>,
) -> EntityQueryOut {
    let truncated = rows.len() > MAX_DEFS_PER_QUERY;
    rows.truncate(MAX_DEFS_PER_QUERY);
    if truncated {
        notes.push(format!(
            "more than {MAX_DEFS_PER_QUERY} definition sites — the list is truncated"
        ));
    }
    let matched_by = if rows
        .iter()
        .any(|r| r.fqn == ent || r.zeitwerk_fqn.as_deref() == Some(ent))
    {
        MATCHED_BY_FQN
    } else {
        MATCHED_BY_LAST_SEGMENT
    };

    let mut order: Vec<String> = Vec::new();
    let mut grouped: HashMap<String, EntityOut> = HashMap::new();
    for r in rows {
        let matched_via = if r.fqn == ent
            || (matched_by == MATCHED_BY_LAST_SEGMENT && last_segment(&r.fqn) == last_segment(ent))
        {
            MATCHED_VIA_NESTING
        } else {
            MATCHED_VIA_ZEITWERK
        };
        let stale = r.live_blob_hash.as_deref() != Some(r.blob_hash.as_str());
        let trust = class_for(matched_via, &r.nesting, &r.zeitwerk_state, stale);
        let entry = grouped.entry(r.fqn.clone()).or_insert_with(|| {
            order.push(r.fqn.clone());
            EntityOut {
                fqn: r.fqn.clone(),
                kind: r.kind.clone(),
                trust_counts: TrustCounts::default(),
                definitions: Vec::new(),
            }
        });
        if trust == crate::resolve::CLASS_EXACT {
            entry.trust_counts.exact += 1;
        } else if trust == crate::resolve::CLASS_LIKELY {
            entry.trust_counts.likely += 1;
        } else {
            entry.trust_counts.candidate += 1;
        }
        entry.definitions.push(EntityDefOut {
            path: r.path,
            line_start: r.line_start,
            line_end: r.line_end,
            kind: r.kind,
            trust,
            matched_via,
            nesting: r.nesting,
            zeitwerk_fqn: r.zeitwerk_fqn,
            stale,
        });
    }
    let entities: Vec<EntityOut> = order
        .into_iter()
        .filter_map(|k| grouped.remove(&k))
        .collect();
    if entities.is_empty() {
        notes.push(format!(
            "no indexed class or module answers to {ent:?} — the entity index covers Ruby class/module definition sites only"
        ));
    }
    if entities.len() > 1 {
        notes.push(format!(
            "{} distinct constants answer to {ent:?} — nothing is merged on a bare name",
            entities.len()
        ));
    }
    EntityQueryOut {
        schema: ENTITY_SCHEMA,
        repo: repo.to_string(),
        ent: ent.to_string(),
        matched_by,
        ambiguous: entities.len() > 1,
        entities,
        zeitwerk: ZeitwerkOut {
            state: zw.state,
            reason: zw.reason.clone(),
            roots: zw.roots.clone(),
            acronyms: zw.acronyms.clone(),
            collapse: zw.collapse.clone(),
        },
        truncated,
        notes,
    }
}

// --- the surface declaration (and what makes it un-dead) --------------------

/// One route this unit ships, declared as DATA so a test can walk the
/// declaration against the thing that must implement it.
///
/// The v7.0 defect class this exists to stop: five `pane.*` registry rows
/// with no handler, and a `review impact` CLI verb that never sent the
/// param its route required. A `RouteContract` is therefore checked from
/// BOTH sides — `entities::tests::
/// every_declared_v71_g0_route_is_registered_and_requires_its_params` (this
/// crate) proves the path is registered in `router.rs` with the named
/// handler and that the handler's own params struct genuinely refuses each
/// required param, and `kb-code-cli`'s
/// `cli_requests_send_every_param_their_route_requires` proves the CLI
/// verb sends them.
#[derive(Debug, Clone, Copy)]
pub struct RouteContract {
    /// Full path as a caller addresses it, `/api`-prefixed.
    pub path: &'static str,
    /// `module::function` of the axum handler, as it appears in
    /// `router.rs`.
    pub handler: &'static str,
    /// Params without which this route cannot answer.
    pub required_params: &'static [&'static str],
    /// Deserialize this route's OWN params struct from a complete query
    /// map with `omit` removed (`""` removes nothing); `true` when it
    /// still succeeds. Supplied per route so the check runs against the
    /// REAL struct, not a restatement of it.
    pub params_accept_without: fn(omit: &str) -> bool,
}

pub const ENTITY_ROUTE: RouteContract = RouteContract {
    path: "/api/entity",
    handler: "entities::entity_route",
    required_params: &["repo", "ent"],
    params_accept_without: entity_params_accept_without,
};

fn entity_params_accept_without(omit: &str) -> bool {
    let mut map = serde_json::Map::new();
    for (k, v) in [("repo", "r"), ("ent", "Foo")] {
        if k != omit {
            map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
        }
    }
    serde_json::from_value::<EntityParams>(serde_json::Value::Object(map)).is_ok()
}

/// Every route unit V71-G0 adds. Both the server-side and the CLI-side
/// dead-surface tests walk THIS list — adding a route without adding it
/// here is the one gap neither test can see, which is why the list lives
/// next to the routes rather than in a test file.
pub const V71_G0_ROUTES: &[RouteContract] = &[ENTITY_ROUTE, crate::seq::SEQ_ROUTE];

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTER_SRC: &str = include_str!("../router.rs");

    fn sym(ordinal: u32, name: &str, kind: &str, start: u32, end: u32) -> Symbol {
        Symbol {
            ordinal,
            name: name.to_string(),
            kind: kind.to_string(),
            line_start: start,
            line_end: end,
            col_start: 0,
            col_end: 0,
            container: None,
            signature: None,
            doc: None,
            param_min: None,
            param_max: None,
        }
    }

    /// A one-line Ruby source whose `class`/`module` header matches the
    /// symbol fixture's own `col_start`, so [`compact_scope`] reads what a
    /// real file would give it.
    fn ruby_source(lines: &[&str]) -> Vec<u8> {
        lines.join("\n").into_bytes()
    }

    fn zw_read(roots: &[&str]) -> zeitwerk::Zeitwerk {
        zeitwerk::Zeitwerk {
            state: zeitwerk::STATE_READ,
            reason: None,
            roots: roots.iter().map(|r| r.to_string()).collect(),
            collapse: Vec::new(),
            acronyms: Vec::new(),
            ignore: Vec::new(),
        }
    }

    // --- the dead-surface walk ------------------------------------------

    #[test]
    fn every_declared_v71_g0_route_is_registered_and_requires_its_params() {
        assert!(
            !V71_G0_ROUTES.is_empty(),
            "the unit declares at least one route"
        );
        // V72-I1 joins the SAME walk rather than starting a second one:
        // one loop over every declared contract, so a route added to any
        // unit's list is checked here by construction.
        for c in V71_G0_ROUTES
            .iter()
            .chain(crate::rails::routes::V72_I1_ROUTES.iter())
            // V73-K1 joins the SAME walk rather than starting a third one.
            .chain(crate::review_doc::routes::V73_K1_ROUTES.iter())
            // V74-L1 joins the SAME walk, one milestone later — a
            // `kbc-canvas/1` read declared in `boards::V74_L1_ROUTES` with
            // no registration in router.rs fails HERE, by path.
            .chain(crate::boards::V74_L1_ROUTES.iter())
            // V73-K3 — the timeline, the claim register, the two
            // pseudo-file reads and the hunk↔turn join, the same way.
            .chain(crate::review_timeline::V73_K3_ROUTES.iter())
            // V74-L3a joins the SAME walk: `kbc-recipe/1`'s four reads.
            .chain(crate::recipe::routes::V74_L3A_ROUTES.iter())
            // V74-L3b — kbc-tour/1's four reads and kbc-trail/1's four,
            // the same way. A tour read declared with no registration in
            // router.rs fails HERE, by path.
            .chain(crate::tours::V74_L3B_TOUR_ROUTES.iter())
            .chain(crate::trails::V74_L3B_TRAIL_ROUTES.iter())
        {
            let nested = c
                .path
                .strip_prefix("/api")
                .expect("every route path is /api-nested");
            assert!(
                ROUTER_SRC.contains(&format!("\"{nested}\"")),
                "{}: declared but never registered in router.rs — the v7.0 dead-surface defect",
                c.path
            );
            assert!(
                ROUTER_SRC.contains(c.handler),
                "{}: registered path but no {} handler named in router.rs",
                c.path,
                c.handler
            );
            assert!(
                (c.params_accept_without)(""),
                "{}: its own params struct rejects a COMPLETE query map",
                c.path
            );
            for p in c.required_params {
                assert!(
                    !(c.params_accept_without)(p),
                    "{}: declares {p:?} required, but its params struct accepts a request without it",
                    c.path
                );
            }
        }
    }

    // --- the trust ladder ------------------------------------------------

    #[test]
    fn only_a_live_fully_determined_nesting_match_can_ever_be_exact() {
        let read = zeitwerk::STATE_READ;
        let degraded = zeitwerk::STATE_DEGRADED;
        assert_eq!(
            class_for(MATCHED_VIA_NESTING, NESTING_LEXICAL, read, false),
            "exact"
        );
        assert_eq!(
            class_for(MATCHED_VIA_NESTING, NESTING_LEXICAL, degraded, false),
            "exact",
            "a tree-proved nesting does not depend on the config read at all"
        );
        assert_eq!(
            class_for(MATCHED_VIA_NESTING, NESTING_AMBIGUOUS, read, false),
            "likely",
            "a compact path Ruby resolves at runtime is never exact"
        );
        assert_eq!(
            class_for(MATCHED_VIA_NESTING, NESTING_LEXICAL, read, true),
            "likely"
        );
        assert_eq!(
            class_for(MATCHED_VIA_ZEITWERK, NESTING_LEXICAL, read, false),
            "likely"
        );
        assert_eq!(
            class_for(MATCHED_VIA_ZEITWERK, NESTING_LEXICAL, read, true),
            "candidate"
        );
        assert_eq!(
            class_for(MATCHED_VIA_ZEITWERK, NESTING_LEXICAL, degraded, false),
            "candidate"
        );
        // Exhaustive: nothing else in the whole input space reaches exact.
        for via in [MATCHED_VIA_NESTING, MATCHED_VIA_ZEITWERK, "something-else"] {
            for nesting in [NESTING_LEXICAL, NESTING_AMBIGUOUS, "unknown"] {
                for state in [read, degraded, "unknown"] {
                    for stale in [true, false] {
                        if class_for(via, nesting, state, stale) == "exact" {
                            assert_eq!(
                                (via, nesting, stale),
                                (MATCHED_VIA_NESTING, NESTING_LEXICAL, false),
                                "a wrong exact is a release blocker:                                  {via}/{nesting}/{state}/{stale}"
                            );
                        }
                    }
                }
            }
        }
    }

    // --- extraction -------------------------------------------------------

    #[test]
    fn nesting_is_reconstructed_from_range_containment_not_the_container_field() {
        // module Reseller / module Billing / class Order — `container`
        // would only ever name `Billing`.
        let src = ruby_source(&[
            "module Reseller",
            "  module Billing",
            "    class Order",
            "      def total",
            "      end",
            "    end",
            "  end",
            "end",
        ]);
        let mut symbols = vec![
            sym(0, "Reseller", "module", 1, 8),
            sym(1, "Billing", "module", 2, 7),
            sym(2, "Order", "class", 3, 6),
            sym(3, "total", "method", 4, 5),
        ];
        symbols[0].col_start = 7;
        symbols[1].col_start = 9;
        symbols[2].col_start = 10;
        let defs = defs_for_file(
            "app/models/x.rb",
            &src,
            &symbols,
            &zeitwerk::Zeitwerk::default(),
        );
        let fqns: Vec<&str> = defs.iter().map(|d| d.fqn.as_str()).collect();
        assert_eq!(
            fqns,
            vec!["Reseller", "Reseller::Billing", "Reseller::Billing::Order"]
        );
        assert!(
            defs.iter().all(|d| d.nesting == NESTING_LEXICAL),
            "plain nesting is fully determined by the tree"
        );
        assert!(
            defs.iter().all(|d| d.kind != "method"),
            "members are not entities in G0"
        );
    }

    #[test]
    fn a_compact_top_level_definition_recovers_the_scope_the_tags_query_drops() {
        // tree-sitter-ruby captures `class Reseller::Order` as the bare
        // name `Order`. Trusting that would claim, at `exact`, that this
        // file defines a top-level `Order`.
        let src = ruby_source(&["class Reseller::Order", "end"]);
        let mut symbols = vec![sym(0, "Order", "class", 1, 2)];
        symbols[0].col_start = 16;
        let defs = defs_for_file(
            "app/models/reseller/order.rb",
            &src,
            &symbols,
            &zeitwerk::Zeitwerk::default(),
        );
        assert_eq!(defs[0].fqn, "Reseller::Order");
        assert_eq!(defs[0].nesting, NESTING_LEXICAL);
    }

    #[test]
    fn a_compact_path_inside_a_module_is_recorded_but_marked_ambiguous() {
        // `module M; class A::B` is `M::A::B` when `M::A` exists and
        // `::A::B` otherwise — a RUNTIME constant lookup. The lexically
        // nearest reading is recorded and can never be exact.
        let src = ruby_source(&["module M", "  class A::B", "  end", "end"]);
        let mut symbols = vec![sym(0, "M", "module", 1, 4), sym(1, "B", "class", 2, 3)];
        symbols[0].col_start = 7;
        symbols[1].col_start = 11;
        let defs = defs_for_file(
            "app/models/x.rb",
            &src,
            &symbols,
            &zeitwerk::Zeitwerk::default(),
        );
        assert_eq!(defs[1].fqn, "M::A::B");
        assert_eq!(defs[1].nesting, NESTING_AMBIGUOUS);
        assert_eq!(
            class_for(
                MATCHED_VIA_NESTING,
                defs[1].nesting,
                zeitwerk::STATE_READ,
                false
            ),
            "likely"
        );
    }

    #[test]
    fn a_root_anchored_definition_ignores_its_enclosing_modules() {
        let src = ruby_source(&["module M", "  class ::Order", "  end", "end"]);
        let mut symbols = vec![sym(0, "M", "module", 1, 4), sym(1, "Order", "class", 2, 3)];
        symbols[0].col_start = 7;
        symbols[1].col_start = 10;
        let defs = defs_for_file(
            "app/models/x.rb",
            &src,
            &symbols,
            &zeitwerk::Zeitwerk::default(),
        );
        assert_eq!(defs[1].fqn, "Order");
        assert_eq!(defs[1].nesting, NESTING_LEXICAL);
    }

    #[test]
    fn two_sibling_definitions_do_not_nest_inside_each_other() {
        let src = ruby_source(&["class First", "end", "", "class Second", "end"]);
        let mut symbols = vec![
            sym(0, "First", "class", 1, 2),
            sym(1, "Second", "class", 4, 5),
        ];
        symbols[0].col_start = 6;
        symbols[1].col_start = 6;
        let defs = defs_for_file(
            "app/models/x.rb",
            &src,
            &symbols,
            &zeitwerk::Zeitwerk::default(),
        );
        assert_eq!(defs[0].fqn, "First");
        assert_eq!(defs[1].fqn, "Second");
    }

    #[test]
    fn a_singleton_class_is_not_an_entity() {
        let src = ruby_source(&["class Order", "  class << self", "  end", "end"]);
        let mut symbols = vec![
            sym(0, "Order", "class", 1, 4),
            sym(1, "self", "singleton_class", 2, 3),
        ];
        symbols[0].col_start = 6;
        let defs = defs_for_file(
            "app/models/order.rb",
            &src,
            &symbols,
            &zeitwerk::Zeitwerk::default(),
        );
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].fqn, "Order");
    }

    #[test]
    fn the_zeitwerk_expectation_attaches_to_the_one_definition_that_answers_to_it() {
        let zw = zw_read(&["app/models"]);
        let src = ruby_source(&["module Reseller", "  class Order", "  end", "end"]);
        let mut symbols = vec![
            sym(0, "Reseller", "module", 1, 4),
            sym(1, "Order", "class", 2, 3),
        ];
        symbols[0].col_start = 7;
        symbols[1].col_start = 8;
        let defs = defs_for_file("app/models/reseller/order.rb", &src, &symbols, &zw);
        assert_eq!(
            defs[0].zeitwerk_fqn, None,
            "the namespace module is not the file's constant"
        );
        assert_eq!(defs[1].zeitwerk_fqn.as_deref(), Some("Reseller::Order"));
    }

    #[test]
    fn a_file_whose_definitions_do_not_answer_to_the_convention_gets_no_zeitwerk_claim() {
        let zw = zw_read(&["app/models"]);
        let src = ruby_source(&["class SomethingElse", "end"]);
        let mut symbols = vec![sym(0, "SomethingElse", "class", 1, 2)];
        symbols[0].col_start = 6;
        let defs = defs_for_file("app/models/reseller/order.rb", &src, &symbols, &zw);
        assert_eq!(defs[0].zeitwerk_fqn, None);
    }

    #[test]
    fn a_mis_namespaced_definition_keeps_its_tree_fqn_and_gains_the_convention_name() {
        // `class Order` at top level in `app/models/reseller/order.rb`: the
        // tree proves `Order`; the convention says `Reseller::Order`. Both
        // are addressable, at different trust.
        let zw = zw_read(&["app/models"]);
        let src = ruby_source(&["class Order", "end"]);
        let mut symbols = vec![sym(0, "Order", "class", 1, 2)];
        symbols[0].col_start = 6;
        let defs = defs_for_file("app/models/reseller/order.rb", &src, &symbols, &zw);
        assert_eq!(defs[0].fqn, "Order");
        assert_eq!(defs[0].zeitwerk_fqn.as_deref(), Some("Reseller::Order"));
    }

    #[test]
    fn compact_scope_reads_only_what_the_line_actually_spells() {
        assert_eq!(compact_scope("class Order", 6), None);
        assert_eq!(
            compact_scope("class Reseller::Order", 16),
            Some(vec!["Reseller".to_string()])
        );
        assert_eq!(
            compact_scope("  class A::B::C", 14),
            Some(vec!["A".to_string(), "B".to_string()])
        );
        assert_eq!(compact_scope("  class ::Order", 10), Some(Vec::new()));
        // A column past the end of the line yields nothing, never a panic.
        assert_eq!(compact_scope("class Order", 99), None);
    }

    #[test]
    fn nth_line_is_one_based_and_total() {
        let src = ruby_source(&["a", "b", "c"]);
        assert_eq!(nth_line(&src, 1), Some("a"));
        assert_eq!(nth_line(&src, 3), Some("c"));
        assert_eq!(nth_line(&src, 4), None);
        assert_eq!(nth_line(&src, 0), None);
    }

    #[test]
    fn extraction_is_capped_per_file() {
        let symbols: Vec<Symbol> = (0..(MAX_DEFS_PER_FILE as u32 + 50))
            .map(|i| sym(i, &format!("K{i}"), "class", i * 2 + 1, i * 2 + 2))
            .collect();
        let defs = defs_for_file(
            "app/models/x.rb",
            b"",
            &symbols,
            &zeitwerk::Zeitwerk::default(),
        );
        assert_eq!(defs.len(), MAX_DEFS_PER_FILE);
    }

    // --- the address ------------------------------------------------------

    #[test]
    fn the_ent_address_accepts_constant_paths_and_refuses_members_by_name() {
        assert_eq!(validate_ent("Order").unwrap(), "Order");
        assert_eq!(
            validate_ent(" Reseller::Order ").unwrap(),
            "Reseller::Order"
        );
        let member = validate_ent("Reseller::Order#total").unwrap_err();
        assert!(format!("{member:?}").contains("member"), "{member:?}");
        let singleton = validate_ent("Reseller::Order.build").unwrap_err();
        assert!(format!("{singleton:?}").contains("member"), "{singleton:?}");
        assert!(validate_ent("").is_err());
        assert!(validate_ent("lower_case").is_err());
        assert!(validate_ent("Foo::").is_err());
        assert!(validate_ent("Foo%Bar").is_err());
    }

    // --- the response fold -------------------------------------------------

    fn row(fqn: &str, path: &str, zfqn: Option<&str>, live: bool) -> crate::store::EntityDefRow {
        crate::store::EntityDefRow {
            worktree: String::new(),
            path: path.to_string(),
            fqn: fqn.to_string(),
            kind: "class".to_string(),
            nesting: NESTING_LEXICAL.to_string(),
            line_start: 1,
            line_end: 9,
            zeitwerk_fqn: zfqn.map(|s| s.to_string()),
            zeitwerk_state: zeitwerk::STATE_READ.to_string(),
            blob_hash: "blob1".to_string(),
            live_blob_hash: live.then(|| "blob1".to_string()),
        }
    }

    #[test]
    fn every_reopening_of_one_entity_is_grouped_with_a_trust_census() {
        let rows = vec![
            row(
                "Reseller::Order",
                "app/models/reseller/order.rb",
                Some("Reseller::Order"),
                true,
            ),
            row(
                "Reseller::Order",
                "app/decorators/reseller/order.rb",
                None,
                true,
            ),
        ];
        let out = build_entity_query(
            "repo",
            "Reseller::Order",
            rows,
            &zw_read(&["app/models"]),
            Vec::new(),
        );
        assert_eq!(out.entities.len(), 1);
        assert!(!out.ambiguous);
        assert_eq!(out.matched_by, MATCHED_BY_FQN);
        assert_eq!(out.entities[0].definitions.len(), 2);
        assert_eq!(out.entities[0].trust_counts.exact, 2);
    }

    #[test]
    fn a_convention_only_match_is_likely_and_never_exact() {
        // The tree proves `Order`; the caller asked for `Reseller::Order`,
        // which only the convention supplies.
        let rows = vec![row(
            "Order",
            "app/models/reseller/order.rb",
            Some("Reseller::Order"),
            true,
        )];
        let out = build_entity_query(
            "repo",
            "Reseller::Order",
            rows,
            &zw_read(&["app/models"]),
            Vec::new(),
        );
        let def = &out.entities[0].definitions[0];
        assert_eq!(def.matched_via, MATCHED_VIA_ZEITWERK);
        assert_eq!(def.trust, "likely");
        assert_eq!(out.entities[0].trust_counts.exact, 0);
    }

    #[test]
    fn a_drifted_blob_demotes_the_claim_and_says_so() {
        let rows = vec![row("Order", "app/models/order.rb", None, false)];
        let out = build_entity_query("repo", "Order", rows, &zw_read(&["app/models"]), Vec::new());
        let def = &out.entities[0].definitions[0];
        assert!(def.stale);
        assert_eq!(def.trust, "likely", "a stale nesting claim is never exact");
    }

    #[test]
    fn two_constants_answering_to_one_bare_name_are_reported_as_ambiguous_never_merged() {
        let rows = vec![
            row(
                "Reseller::Order",
                "app/models/reseller/order.rb",
                None,
                true,
            ),
            row("Billing::Order", "app/models/billing/order.rb", None, true),
        ];
        let out = build_entity_query("repo", "Order", rows, &zw_read(&["app/models"]), Vec::new());
        assert!(out.ambiguous);
        assert_eq!(out.matched_by, MATCHED_BY_LAST_SEGMENT);
        assert_eq!(out.entities.len(), 2);
        assert!(out.notes.iter().any(|n| n.contains("distinct constants")));
    }

    #[test]
    fn an_empty_answer_carries_a_reason_never_a_silent_empty_list() {
        let out = build_entity_query("repo", "Nope", Vec::new(), &zw_read(&[]), Vec::new());
        assert!(out.entities.is_empty());
        assert!(out.notes.iter().any(|n| n.contains("no indexed class")));
    }

    #[test]
    fn a_degraded_zeitwerk_read_is_captioned_on_the_response() {
        let zw = zeitwerk::Zeitwerk::none("no config/application.rb");
        let mut notes = Vec::new();
        notes.push(format!(
            "zeitwerk read degraded: {} — every convention-derived name is capped at candidate",
            zw.reason.clone().unwrap_or_default()
        ));
        let out = build_entity_query("repo", "Order", Vec::new(), &zw, notes);
        assert_eq!(out.zeitwerk.state, zeitwerk::STATE_DEGRADED);
        assert!(out.notes.iter().any(|n| n.contains("degraded")));
    }

    #[test]
    fn the_query_is_capped_and_says_when_it_truncated() {
        let rows: Vec<crate::store::EntityDefRow> = (0..(MAX_DEFS_PER_QUERY + 1))
            .map(|i| row("Order", &format!("app/models/o{i}.rb"), None, true))
            .collect();
        let out = build_entity_query("repo", "Order", rows, &zw_read(&[]), Vec::new());
        assert!(out.truncated);
        assert_eq!(out.entities[0].definitions.len(), MAX_DEFS_PER_QUERY);
        assert!(out.notes.iter().any(|n| n.contains("truncated")));
    }

    #[test]
    fn the_worktree_key_of_a_plain_directory_is_the_main_worktree_empty_string() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(worktree_key_for(tmp.path()), "");
    }

    #[test]
    fn is_constant_path_matches_the_ent_grammar() {
        assert!(is_constant_path("Foo"));
        assert!(is_constant_path("Foo::Bar2_x"));
        assert!(!is_constant_path("foo"));
        assert!(!is_constant_path("Foo::"));
        assert!(!is_constant_path(""));
        assert!(!is_constant_path("Foo Bar"));
    }
}
