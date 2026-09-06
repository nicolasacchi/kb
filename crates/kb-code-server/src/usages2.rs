//! V71-E1 — `GET /api/usages/2`: the `usages/2` wire (D4).
//!
//! `usages/1` (`crate::usages`) answers "where else does this name appear,
//! and how much do we trust each answer". It is FROZEN and still served,
//! byte-identical. This module answers the four further questions D4 says a
//! reader actually has, over the SAME ladder (`usages::usages_core` — there
//! is exactly one classifier here, deliberately: the counts path already
//! drifted from the rows path once, and one of those numbers is always
//! wrong):
//!
//! - **what is this row DOING** — [`UsageKind`], a CLOSED vocabulary
//!   (D4 lists all 34 names; nothing else may appear on the wire) with
//!   `unclassified` as a first-class, honest outcome;
//! - **which orthogonal facts hold at once** — [`Roles`], a SCIP-style
//!   bitset, because "a write, in test code" is two facts and not one
//!   category;
//! - **why should I believe it** — `precision` per ROW (the existing
//!   `resolve::PRECISION_*` vocabulary), so an `exact` from scip, from the
//!   locals graph, from the Ruby STRICT lane and from a live LSP are
//!   distinguishable instead of collapsing into one undifferentiated
//!   block; plus `enclosing` (Kythe's `childof`: blame the caller, not the
//!   file) and `blob_sha` (the row is pinned to bytes, as review comments
//!   are);
//! - **how much am I NOT seeing** — [`Capped`], in-band, with the TRUE
//!   total. A silent cap is a correctness bug (research §7.4); this wire
//!   has no way to truncate quietly.
//!
//! # Ruby
//!
//! The static ladder has no `exact` tier for Ruby at all (`locals::supports`
//! is Rust/TS/TSX/Python), so for the v7.1 target corpus every row landed
//! in `likely`/`candidate` regardless of how provable it was. D4 opens one
//! door: `intel::ruby_strict`'s STRICT rule, evaluated per request against
//! the file's own bytes. It mints `exact` only for a same-file local with
//! an unambiguous binding, a name that is not a method in the enclosing
//! hierarchy, and an enclosing method free of `eval`/`instance_eval`/
//! `binding`/`send`/`define_method`/`method_missing`. Everything else
//! stays `likely`, and the refused clause is NAMED on the wire
//! ([`RubyStrictNote`]) rather than left as an unexplained demotion.
//!
//! The oracle set (`oracle_ruby_locals`, built from real client files and
//! kept outside this repository) is the release gate for that lane: a wrong `exact`
//! fails the build.
//!
//! # Not here
//!
//! Filter/group/cursor params (`kind=`, `trust=`, `exclude=`, `group=`)
//! belong to E2's dock and are deliberately absent — a param the server
//! accepts and no caller sends is the same dead surface as a key with no
//! handler. Nothing in this module is persisted or cached (root CLAUDE.md
//! invariant #2).

use crate::routes::{find_repo, read_repo_file, ApiError};
use crate::state::SharedState;
use crate::store::{Store, StoreBlocking};
use crate::usages::{CoreOut, CoreRow, UsageSymbol};
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

pub const USAGES2_SCHEMA: &str = "usages/2";

/// Per-class cap. Same numbers as `usages/1` — but where v1 reports one
/// bare `truncated` bool, v2 reports [`Capped`] per class with the true
/// total and the reason.
pub const DEFAULT_LIMIT: usize = 500;
pub const MAX_LIMIT: usize = 500;

/// V71-E1 — the Ruby STRICT lane's own `precision`, distinct from
/// `PRECISION_LOCALS` (the four ingest-stamped languages) so a reader can
/// tell which rule minted an `exact`. Maps to `exact` in
/// `resolve::class_for_precision`.
pub const PRECISION_RUBY_STRICT: &str = "ruby-locals-strict";

/// Path globs backing the `vendor` role bit when the operator has not named
/// a `[scopes] vendor` set. A PATH heuristic, never a claim about content.
const DEFAULT_VENDOR_GLOBS: &[&str] = &["vendor/**", "**/node_modules/**", "**/.bundle/**"];
/// Path globs backing the `generated` role bit, likewise overridable via
/// `[scopes] generated`.
const DEFAULT_GENERATED_GLOBS: &[&str] = &[
    "**/generated/**",
    "**/*.gen.*",
    "**/*_pb.rb",
    "db/schema.rb",
];

// --- the closed kind vocabulary -------------------------------------------

/// What a usage row is doing — D4's CLOSED vocabulary, in D4's own order
/// and with D4's own wire names.
///
/// Closed means closed: a consumer may switch exhaustively on these names,
/// and a new one is a wire change, not an implementation detail. Several
/// variants have no mint site in this milestone (Ruby is the language with
/// full coverage, and some kinds need extraction passes that do not exist
/// yet) — that is recorded, per variant with a reason, by
/// [`UNMINTED_KINDS`] and enforced by the `usages2_every_declared_kind_is_
/// minted_or_listed` test rather than being left to be discovered as a
/// silently dead surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageKind {
    // Structural.
    Def,
    Decl,
    Call,
    Read,
    Write,
    Mutate,
    Import,
    Include,
    Extend,
    Prepend,
    Inherit,
    Override,
    Alias,
    Instantiate,
    Typed,
    Rescue,
    YieldTo,
    // Dynamic / textual.
    SymbolMention,
    StringMention,
    SendDynamic,
    CommentMention,
    // Rails lens.
    Route,
    ViewRender,
    Layout,
    Helper,
    I18nKey,
    Association,
    Callback,
    JobEnqueue,
    Migration,
    Fixture,
    Factory,
    ConfigKey,
    // Fallback.
    Unclassified,
}

impl UsageKind {
    /// Every kind, in D4's declaration order. The wire test walks this.
    pub const ALL: &'static [UsageKind] = &[
        UsageKind::Def,
        UsageKind::Decl,
        UsageKind::Call,
        UsageKind::Read,
        UsageKind::Write,
        UsageKind::Mutate,
        UsageKind::Import,
        UsageKind::Include,
        UsageKind::Extend,
        UsageKind::Prepend,
        UsageKind::Inherit,
        UsageKind::Override,
        UsageKind::Alias,
        UsageKind::Instantiate,
        UsageKind::Typed,
        UsageKind::Rescue,
        UsageKind::YieldTo,
        UsageKind::SymbolMention,
        UsageKind::StringMention,
        UsageKind::SendDynamic,
        UsageKind::CommentMention,
        UsageKind::Route,
        UsageKind::ViewRender,
        UsageKind::Layout,
        UsageKind::Helper,
        UsageKind::I18nKey,
        UsageKind::Association,
        UsageKind::Callback,
        UsageKind::JobEnqueue,
        UsageKind::Migration,
        UsageKind::Fixture,
        UsageKind::Factory,
        UsageKind::ConfigKey,
        UsageKind::Unclassified,
    ];

    /// The wire name — the single source of truth for serialisation is the
    /// `serde(rename_all)` derive; this mirror exists so the vocabulary can
    /// be walked without a serializer, and the wire test asserts the two
    /// agree for every variant.
    pub fn as_str(self) -> &'static str {
        match self {
            UsageKind::Def => "def",
            UsageKind::Decl => "decl",
            UsageKind::Call => "call",
            UsageKind::Read => "read",
            UsageKind::Write => "write",
            UsageKind::Mutate => "mutate",
            UsageKind::Import => "import",
            UsageKind::Include => "include",
            UsageKind::Extend => "extend",
            UsageKind::Prepend => "prepend",
            UsageKind::Inherit => "inherit",
            UsageKind::Override => "override",
            UsageKind::Alias => "alias",
            UsageKind::Instantiate => "instantiate",
            UsageKind::Typed => "typed",
            UsageKind::Rescue => "rescue",
            UsageKind::YieldTo => "yield_to",
            UsageKind::SymbolMention => "symbol_mention",
            UsageKind::StringMention => "string_mention",
            UsageKind::SendDynamic => "send_dynamic",
            UsageKind::CommentMention => "comment_mention",
            UsageKind::Route => "route",
            UsageKind::ViewRender => "view_render",
            UsageKind::Layout => "layout",
            UsageKind::Helper => "helper",
            UsageKind::I18nKey => "i18n_key",
            UsageKind::Association => "association",
            UsageKind::Callback => "callback",
            UsageKind::JobEnqueue => "job_enqueue",
            UsageKind::Migration => "migration",
            UsageKind::Fixture => "fixture",
            UsageKind::Factory => "factory",
            UsageKind::ConfigKey => "config_key",
            UsageKind::Unclassified => "unclassified",
        }
    }
}

/// The kinds this milestone declares but cannot mint, each with the reason.
///
/// This is the anti-dead-surface ledger. D4 fixes the vocabulary, so every
/// name ships on the wire from day one and a consumer can switch on it
/// exhaustively — but a name no code path can ever produce is exactly the
/// v7.0 defect class (a registry row with no handler), so it is written
/// down here and pinned by a test. Deleting a row from this list without
/// adding a mint site fails the build; adding a NEW unmintable kind fails
/// the build unless it is listed here with a reason.
pub const UNMINTED_KINDS: &[(UsageKind, &str)] = &[
    (
        UsageKind::Decl,
        "no extraction pass separates a declaration from a definition \
         (Ruby's attr_* macros are symbol literals, which the occurrence \
         pass does not index)",
    ),
    (
        UsageKind::Override,
        "needs the supertype's own method table proven, not just its NAME; \
         the hierarchy read exists (intel::ruby_strict) but the claim does not",
    ),
    (
        UsageKind::Typed,
        "Ruby has no type positions outside RBS/sorbet sig blocks, and the \
         typed languages' annotation positions are not extracted",
    ),
    (
        UsageKind::YieldTo,
        "`yield` is a keyword, not an identifier occurrence",
    ),
    (
        UsageKind::SymbolMention,
        "`:foo` is a simple_symbol leaf; the occurrences pass indexes \
         identifier-like nodes only",
    ),
    (
        UsageKind::StringMention,
        "the occurrences pass deliberately excludes string tokens; the \
         mentions lane is E2's explicit grep chip",
    ),
    (
        UsageKind::SendDynamic,
        "the send/public_send/constantize argument is a symbol or string \
         literal — same absence as symbol_mention",
    ),
    (
        UsageKind::CommentMention,
        "the occurrences pass deliberately excludes comment tokens",
    ),
    (
        UsageKind::Layout,
        "the Rails lens has no layout edge kind (frameworks::EdgeKind)",
    ),
    (
        UsageKind::Migration,
        "the Rails lens has no migration edge kind",
    ),
    (
        UsageKind::Fixture,
        "the Rails lens has no fixture edge kind",
    ),
    (
        UsageKind::Factory,
        "the Rails lens has no factory edge kind",
    ),
    (
        UsageKind::ConfigKey,
        "the Rails lens has no config-key edge kind",
    ),
];

/// Rails-lens edge kind → usage kind. Every arm is a name D4's vocabulary
/// actually has; an edge whose meaning has no honest name here maps to
/// `unclassified` rather than being forced into a neighbouring one (a
/// wrong `kind` is as dangerous as a wrong `exact` — research §7.1).
pub fn kind_from_rails_edge(edge: crate::frameworks::EdgeKind, line_text: &str) -> UsageKind {
    use crate::frameworks::EdgeKind as E;
    match edge {
        E::RouteAction | E::RouteFile | E::DeviseOverride => UsageKind::Route,
        E::RenderPartial | E::RenderView | E::ViewComponentRender | E::TurboStreamTarget => {
            UsageKind::ViewRender
        }
        E::Association => UsageKind::Association,
        E::Callback => UsageKind::Callback,
        E::JobEnqueue => UsageKind::JobEnqueue,
        E::I18nKey => UsageKind::I18nKey,
        E::HelperFor => UsageKind::Helper,
        // The lens folds include/extend/prepend into ONE edge kind, so the
        // distinction is read back off the source line — the only thing
        // here that proves which keyword was written.
        E::ConcernInclude => {
            let first = line_text.trim_start();
            if first.starts_with("include ") || first.starts_with("include(") {
                UsageKind::Include
            } else if first.starts_with("extend ") || first.starts_with("extend(") {
                UsageKind::Extend
            } else if first.starts_with("prepend ") || first.starts_with("prepend(") {
                UsageKind::Prepend
            } else {
                UsageKind::Unclassified
            }
        }
        // Scope/validation/delegate/mailer-deliver/Stimulus/spec-subject
        // have no honest name in D4's closed vocabulary.
        E::Scope
        | E::Validation
        | E::Delegate
        | E::MailerDeliver
        | E::StimulusBinding
        | E::SpecSubject => UsageKind::Unclassified,
    }
}

// --- the role bitset ------------------------------------------------------

/// SCIP's `SymbolRole` bit values, verbatim, plus one kb-code extension.
///
/// Roles are ORTHOGONAL flags, not a category: an occurrence can be a
/// write, in test code, and generated, all at once — flattening that into
/// one `kind` is what forces every consumer to regex the path. SCIP's own
/// bits are used unchanged so a future scip-sourced role set can be OR'd
/// in without a translation table.
///
/// `test`/`generated`/`vendor` are derived from the PATH here (via
/// `[scopes]`, defaulting to the same globs `/api/impact/analysis` uses),
/// not from an indexer's own knowledge as SCIP intends — the honest
/// consequence is that they are heuristics about location, never claims
/// about content, and they never affect a trust class.
pub mod roles {
    pub const DEFINITION: u32 = 0x1;
    pub const IMPORT: u32 = 0x2;
    pub const WRITE_ACCESS: u32 = 0x4;
    pub const READ_ACCESS: u32 = 0x8;
    pub const GENERATED: u32 = 0x10;
    pub const TEST: u32 = 0x20;
    /// SCIP's bit; never minted here (kb-code has no forward-declaration
    /// concept) — declared so the numbering matches SCIP's.
    pub const FORWARD_DEFINITION: u32 = 0x40;
    /// kb-code's own bit, above SCIP's range.
    pub const VENDOR: u32 = 0x80;

    /// `(bit, wire name)` in ascending bit order — the decode table.
    pub const ALL: &[(u32, &str)] = &[
        (DEFINITION, "definition"),
        (IMPORT, "import"),
        (WRITE_ACCESS, "write_access"),
        (READ_ACCESS, "read_access"),
        (GENERATED, "generated"),
        (TEST, "test"),
        (FORWARD_DEFINITION, "forward_definition"),
        (VENDOR, "vendor"),
    ];

    /// Decode a bitset into its wire names, ascending bit order.
    pub fn names(bits: u32) -> Vec<&'static str> {
        ALL.iter()
            .filter(|(b, _)| bits & b != 0)
            .map(|(_, n)| *n)
            .collect()
    }
}

// --- wire types -----------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct Usages2Params {
    pub repo: String,
    pub path: String,
    pub line: u32,
    pub col: u32,
    #[serde(rename = "ref")]
    pub rev: Option<String>,
    /// Per-class cap (default 500, max 500). Whatever it hides is reported
    /// by [`Usages2Out::capped`] with the true total.
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EnclosingSymbol {
    pub name: String,
    pub kind: String,
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UsageRow2 {
    pub path: String,
    pub line: u32,
    pub col: u32,
    /// End column (0-based, exclusive) when the row came from an indexed
    /// occurrence; absent for a convention edge that names a LINE only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub col_end: Option<u32>,
    /// The blob this row was read from — a row is pinned to bytes, so a
    /// consumer can tell a stale result from a fresh one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob_sha: Option<String>,
    pub kind: UsageKind,
    pub roles: u32,
    pub role_names: Vec<&'static str>,
    /// The row's own trust class — the group it is in, carried per row so a
    /// flattened list stays honest.
    pub trust: &'static str,
    /// Which tier produced it (`resolve::PRECISION_*` + [`PRECISION_RUBY_STRICT`]).
    pub precision: &'static str,
    /// `usages/1`'s read/write tag, kept for continuity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<&'static str>,
    /// Kythe's `childof`: the innermost symbol containing this line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enclosing: Option<EnclosingSymbol>,
    pub context: String,
}

/// One class's cap report — always present in [`Usages2Out::capped`] when
/// something was hidden, never a bare boolean.
#[derive(Debug, Clone, Serialize)]
pub struct Capped {
    /// `"exact"` | `"likely"` | `"candidate"`.
    pub group: &'static str,
    pub returned: usize,
    /// The TRUE total before the cap.
    pub total: usize,
    /// Why (`"page"` today — the per-class `limit`).
    pub reason: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct Totals {
    pub exact: usize,
    pub likely: usize,
    pub candidate: usize,
    pub all: usize,
}

/// The Ruby STRICT lane's verdict, present only when the lane ran.
#[derive(Debug, Clone, Serialize)]
pub struct RubyStrictNote {
    /// `true` when the lane minted `exact`.
    pub exact: bool,
    /// `"strict"` | `"ambiguous-binding"` | `"hierarchy-method"` |
    /// `"dynamic-enclosing"` — the clause that decided it.
    pub verdict: &'static str,
    /// The enclosing constant hierarchy clause (b) was evaluated against,
    /// outermost first (`include`/`prepend`/`extend` included).
    pub hierarchy: Vec<String>,
    /// How many same-file sites the binding produced.
    pub sites: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Usages2Out {
    pub schema: &'static str,
    pub symbol: UsageSymbol,
    pub class_of_definition: &'static str,
    pub exact: Vec<UsageRow2>,
    pub likely: Vec<UsageRow2>,
    pub candidate: Vec<UsageRow2>,
    pub totals: Totals,
    /// Empty when nothing was hidden. NEVER a silent cap.
    pub capped: Vec<Capped>,
    /// Count per kind over the TRUE totals (not the returned page), so a
    /// capped answer still says what it is made of.
    pub kind_totals: BTreeMap<&'static str, usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ruby_strict: Option<RubyStrictNote>,
}

/// `GET /api/usages/2?repo=&path=&line=&col=[&ref=][&limit=]`.
///
/// Additive: `usages/1` keeps serving unchanged at `/api/usages`. Rides the
/// ordinary `auth_bearer` read path — no new admission surface (it reads
/// strictly less than `GET /api/file` already does).
pub async fn usages2_route(
    State(state): State<SharedState>,
    Query(params): Query<Usages2Params>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo = repo.clone();
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let path = params.path.clone();
    let line = params.line;
    let col = params.col;
    let rev = params.rev.clone();
    let scopes = state.scopes.clone();
    // 2026-08-31 incident (store.rs module doc): one coarse blocking hop
    // for the whole synchronous ladder + enrichment; the lip overlay below
    // is the async leg.
    let repo_bg = repo.clone();
    let mut out = state
        .store
        .run_blocking(move |store| {
            usages2_at(
                store,
                &repo_bg,
                repo_id,
                &path,
                line,
                col,
                rev.as_deref(),
                &scopes,
                limit,
            )
        })
        .await?;
    overlay_lsp_live(&state, &repo, &params, limit, &mut out).await;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// The synchronous half of [`usages2_route`] — the classified ladder plus
/// v2's enrichment, as ONE call. Factored out (V72-G1.1) so the entity
/// dossier can delegate to this ENGINE rather than growing a second
/// scanner: `usages/1` and `usages/2` already share `usages::usages_core`
/// for exactly that reason (the counts path drifted from the rows path
/// once, and one of those numbers is always wrong). The lsp-live overlay
/// is deliberately NOT part of it — that leg is async, and this entry
/// point exists to be called from inside a blocking hop.
#[allow(clippy::too_many_arguments)]
pub(crate) fn usages2_at(
    store: &Store,
    repo: &crate::config::RepoEntry,
    repo_id: i64,
    path: &str,
    line: u32,
    col: u32,
    rev: Option<&str>,
    scopes: &crate::config::ScopesSection,
    limit: usize,
) -> Result<Usages2Out, ApiError> {
    let core = crate::usages::usages_core(store, repo, repo_id, path, line, col, rev)?;
    enrich(
        store, repo, repo_id, path, line, col, rev, scopes, core, limit,
    )
}

/// The lsp-live leg: the SAME `crate::lip` call `usages/1`'s overlay makes
/// (one implementation, `lip::lsp_live_reference_locations`), projected
/// into v2 rows with `precision: "lsp-live"` so a live-LSP reference is
/// distinguishable from a scip one inside `exact`. A no-op in every
/// degrade case, exactly as v1's is.
async fn overlay_lsp_live(
    state: &SharedState,
    repo: &crate::config::RepoEntry,
    params: &Usages2Params,
    limit: usize,
    out: &mut Usages2Out,
) {
    let locs = crate::lip::lsp_live_reference_locations(
        state,
        repo,
        &params.path,
        params.rev.as_deref(),
        params.line,
        params.col,
    )
    .await;
    if locs.is_empty() {
        return;
    }
    let mut seen: std::collections::HashSet<(String, u32, u32)> = out
        .exact
        .iter()
        .map(|r| (r.path.clone(), r.line, r.col))
        .collect();
    let mut added = 0usize;
    for (path, line, col) in locs {
        if !seen.insert((path.clone(), line, col)) {
            continue;
        }
        added += 1;
        if out.exact.len() < limit {
            let context = crate::lip::line_context(repo, &path, line);
            out.exact.push(UsageRow2 {
                path,
                line,
                col,
                col_end: None,
                blob_sha: None,
                kind: UsageKind::Unclassified,
                roles: 0,
                role_names: Vec::new(),
                trust: crate::resolve::CLASS_EXACT,
                precision: crate::resolve::PRECISION_LSP_LIVE,
                access: None,
                enclosing: None,
                context,
            });
        }
    }
    if added == 0 {
        return;
    }
    out.totals.exact += added;
    out.totals.all += added;
    *out.kind_totals
        .entry(UsageKind::Unclassified.as_str())
        .or_insert(0) += added;
    recompute_capped(out, limit);
}

/// Rebuild the `capped` list from the current returned/total counts.
fn recompute_capped(out: &mut Usages2Out, limit: usize) {
    out.capped.clear();
    for (group, returned, total) in [
        ("exact", out.exact.len(), out.totals.exact),
        ("likely", out.likely.len(), out.totals.likely),
        ("candidate", out.candidate.len(), out.totals.candidate),
    ] {
        if total > returned {
            out.capped.push(Capped {
                group,
                returned,
                total,
                reason: "page",
            });
        }
    }
    let _ = limit;
}

#[allow(clippy::too_many_arguments)]
fn enrich(
    store: &Store,
    repo: &crate::config::RepoEntry,
    repo_id: i64,
    path: &str,
    line: u32,
    col: u32,
    rev: Option<&str>,
    scopes: &crate::config::ScopesSection,
    core: CoreOut,
    limit: usize,
) -> Result<Usages2Out, ApiError> {
    let read = read_repo_file(repo, path, rev)?;
    let query_lang = crate::lang::detect(path, Some(&read.bytes)).map(|l| l.id);

    let mut groups: [(&'static str, Vec<CoreRow>); 3] = [
        (crate::resolve::CLASS_EXACT, core.exact),
        (crate::resolve::CLASS_LIKELY, core.likely),
        (crate::resolve::CLASS_CANDIDATE, core.candidate),
    ];

    // --- the Ruby STRICT lane -------------------------------------------
    let mut ruby_note: Option<RubyStrictNote> = None;
    if query_lang == Some("ruby") {
        let lane = crate::intel::ruby_strict::locals_lane(&read.bytes, line, col, |name, hier| {
            hierarchy_has_method(store, repo_id, name, hier)
        });
        if let Some(lane) = lane {
            let (trust, precision) = if lane.verdict.is_exact() {
                (crate::resolve::CLASS_EXACT, PRECISION_RUBY_STRICT)
            } else {
                // A refused clause is still a same-file binding — better
                // than a bare name match, and honestly named.
                (
                    crate::resolve::CLASS_LIKELY,
                    crate::resolve::PRECISION_FILE_LOCAL,
                )
            };
            apply_ruby_lane(&mut groups, path, &lane, trust, precision, &read.bytes);
            ruby_note = Some(RubyStrictNote {
                exact: lane.verdict.is_exact(),
                verdict: lane.verdict.as_str(),
                hierarchy: lane.hierarchy,
                sites: lane.sites.len(),
            });
        }
    }

    // --- per-file enrichment (one parse / one symbol load per PATH) ------
    let mut files: HashMap<String, FileFacts> = HashMap::new();
    let mut positions: BTreeMap<String, Vec<(u32, u32)>> = BTreeMap::new();
    for (_, rows) in groups.iter() {
        for r in rows {
            positions
                .entry(r.row.path.clone())
                .or_default()
                .push((r.row.line, r.row.col));
        }
    }
    let test_globs = crate::impact_analysis::test_scope_patterns(scopes);
    let vendor_globs = scope_globs(scopes, "vendor", DEFAULT_VENDOR_GLOBS);
    let generated_globs = scope_globs(scopes, "generated", DEFAULT_GENERATED_GLOBS);
    for (p, pos) in &positions {
        let facts = file_facts(
            store,
            repo,
            repo_id,
            p,
            pos,
            if p == path {
                Some((read.bytes.clone(), read.blob_hash.clone()))
            } else {
                None
            },
        );
        files.insert(p.clone(), facts);
    }

    let mut out_groups: Vec<Vec<UsageRow2>> = Vec::new();
    let mut totals = Totals {
        exact: 0,
        likely: 0,
        candidate: 0,
        all: 0,
    };
    let mut kind_totals: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut capped: Vec<Capped> = Vec::new();
    for (group, rows) in groups {
        let total = rows.len();
        let mut built: Vec<UsageRow2> = Vec::with_capacity(rows.len().min(limit));
        for (i, r) in rows.into_iter().enumerate() {
            let facts = files.get(&r.row.path);
            let row = build_row(
                group,
                r,
                facts,
                &test_globs,
                &vendor_globs,
                &generated_globs,
            );
            *kind_totals.entry(row.kind.as_str()).or_insert(0) += 1;
            if i < limit {
                built.push(row);
            }
        }
        match group {
            "exact" => totals.exact = total,
            "likely" => totals.likely = total,
            _ => totals.candidate = total,
        }
        totals.all += total;
        if total > built.len() {
            capped.push(Capped {
                group,
                returned: built.len(),
                total,
                reason: "page",
            });
        }
        out_groups.push(built);
    }
    let mut it = out_groups.into_iter();
    Ok(Usages2Out {
        schema: USAGES2_SCHEMA,
        symbol: core.symbol,
        class_of_definition: core.class_of_definition,
        exact: it.next().unwrap_or_default(),
        likely: it.next().unwrap_or_default(),
        candidate: it.next().unwrap_or_default(),
        totals,
        capped,
        kind_totals,
        ruby_strict: ruby_note,
    })
}

/// `[scopes] <name>` when the operator named one, else `defaults`.
fn scope_globs(
    scopes: &crate::config::ScopesSection,
    name: &str,
    defaults: &[&str],
) -> Vec<String> {
    if let Some(pats) = scopes.map.get(name) {
        if !pats.is_empty() {
            return pats.clone();
        }
    }
    defaults.iter().map(|s| (*s).to_string()).collect()
}

/// Per-file derivations shared by every row in that file.
struct FileFacts {
    blob_sha: Option<String>,
    symbols: Vec<crate::extract::Symbol>,
    /// Parallel to the positions passed in — the CST kind at each.
    kinds: HashMap<(u32, u32), UsageKind>,
    /// Line text, for the Rails include/extend/prepend read-back.
    lines: Vec<String>,
}

fn file_facts(
    store: &Store,
    repo: &crate::config::RepoEntry,
    repo_id: i64,
    path: &str,
    positions: &[(u32, u32)],
    preread: Option<(Vec<u8>, String)>,
) -> FileFacts {
    let (bytes, blob_sha) = match preread {
        Some((b, h)) => (Some(b), Some(h)),
        None => match read_repo_file(repo, path, None) {
            Ok(r) => (Some(r.bytes), Some(r.blob_hash)),
            // A row can legitimately name a file this daemon may not read
            // (the secret denylist) or that vanished between the index and
            // now: the row survives without excerpt-derived facts.
            Err(_) => (
                None,
                store
                    .get_file(repo_id, path)
                    .ok()
                    .flatten()
                    .map(|f| f.blob_hash),
            ),
        },
    };
    let lang = crate::lang::detect(path, bytes.as_deref());
    let mut kinds = HashMap::new();
    let mut lines = Vec::new();
    if let (Some(b), Some(l)) = (bytes.as_deref(), lang) {
        let classified = crate::intel::usekind::classify_file(l.id, b, positions);
        for (pos, k) in positions.iter().zip(classified) {
            if let Some(k) = k {
                kinds.insert(*pos, k);
            }
        }
        if let Ok(text) = std::str::from_utf8(b) {
            lines = text.lines().map(|s| s.to_string()).collect();
        }
    }
    let symbols = match (&blob_sha, lang) {
        (Some(h), Some(l)) => store.symbols_for_blob(h, l.symbol_salt).unwrap_or_default(),
        _ => Vec::new(),
    };
    FileFacts {
        blob_sha,
        symbols,
        kinds,
        lines,
    }
}

/// Does any symbol named `name` in this repo sit on one of `hierarchy`'s
/// constants? — D4's clause (b).
///
/// A NAME join, deliberately: resolving a constant to a file would be a
/// `likely`-grade claim of its own, and this predicate only ever DEMOTES,
/// so a false positive costs an `exact` and a false negative is caught by
/// the other two clauses. Kind-gated to callable symbols so a nested class
/// of the same name does not count.
fn hierarchy_has_method(store: &Store, repo_id: i64, name: &str, hierarchy: &[String]) -> bool {
    if hierarchy.is_empty() {
        return false;
    }
    let Ok(hits) = store.symbols_named_in_repo(repo_id, name) else {
        // Cannot prove absence → refuse (demote).
        return true;
    };
    symbols_name_a_hierarchy_method(&hits, hierarchy)
}

/// The pure half of [`hierarchy_has_method`] — split out so the Ruby oracle
/// (`tests/oracle/oracle_ruby_locals.rs`) evaluates clause (b) with the
/// SHIPPED predicate over symbols it extracts itself, rather than a
/// look-alike written in the test.
pub fn symbols_name_a_hierarchy_method(
    hits: &[(String, crate::extract::Symbol)],
    hierarchy: &[String],
) -> bool {
    hits.iter().any(|(_, sym)| {
        let callable = matches!(
            sym.kind.as_str(),
            "method" | "function" | "singleton_method" | "def"
        );
        callable
            && sym
                .container
                .as_deref()
                .is_some_and(|c| hierarchy.iter().any(|h| same_constant(h, c)))
    })
}

/// Do two constant paths name the same class/module for clause (b)'s
/// purposes? Full equality, or the same LAST segment (`Feeds::ReviewsJob`
/// vs `ReviewsJob`) — `extract::container_of` and
/// `ruby_strict::enclosing_hierarchy` read the name from different nodes
/// and one may be qualified where the other is not. Matching loosely can
/// only ever DEMOTE, which is the safe direction.
fn same_constant(a: &str, b: &str) -> bool {
    a == b || last_segment(a) == last_segment(b)
}

fn last_segment(s: &str) -> &str {
    s.rsplit("::").next().unwrap_or(s)
}

/// Move every site of the Ruby binding into `trust`'s group, adding the
/// ones the occurrence table never had (Ruby trims single-character `ref`
/// occurrences at ingest, so `rescue … => e`'s `e` has no row at all).
fn apply_ruby_lane(
    groups: &mut [(&'static str, Vec<CoreRow>); 3],
    path: &str,
    lane: &crate::intel::ruby_strict::LocalsLane,
    trust: &'static str,
    precision: &'static str,
    bytes: &[u8],
) {
    let mut moved: Vec<CoreRow> = Vec::new();
    let wanted: Vec<(u32, u32)> = lane.sites.iter().map(|s| (s.line, s.col)).collect();
    for (_, rows) in groups.iter_mut() {
        let mut keep = Vec::with_capacity(rows.len());
        for mut r in rows.drain(..) {
            if r.row.path == path && wanted.contains(&(r.row.line, r.row.col)) {
                r.detail.precision = precision;
                moved.push(r);
            } else {
                keep.push(r);
            }
        }
        *rows = keep;
    }
    let text = std::str::from_utf8(bytes).unwrap_or("");
    for site in &lane.sites {
        if moved
            .iter()
            .any(|r| r.row.line == site.line && r.row.col == site.col)
        {
            continue;
        }
        let context: String = text
            .lines()
            .nth(site.line.saturating_sub(1) as usize)
            .unwrap_or("")
            .trim()
            .chars()
            .take(200)
            .collect();
        moved.push(CoreRow {
            row: crate::usages::UsageRow {
                path: path.to_string(),
                line: site.line,
                col: site.col,
                kind: if site.is_def { "def" } else { "ref" }.to_string(),
                access: None,
                context,
            },
            detail: crate::usages::RowDetail {
                precision,
                occ_role: Some(if site.is_def { "def" } else { "ref" }.to_string()),
                rails_kind: None,
                col_end: Some(site.col + lane.name.chars().count() as u32),
            },
        });
    }
    moved.sort_by(|a, b| a.row.line.cmp(&b.row.line).then(a.row.col.cmp(&b.row.col)));
    for (group, rows) in groups.iter_mut() {
        if *group == trust {
            rows.extend(moved);
            rows.sort_by(|a, b| {
                a.row
                    .path
                    .cmp(&b.row.path)
                    .then_with(|| a.row.line.cmp(&b.row.line))
                    .then_with(|| a.row.col.cmp(&b.row.col))
            });
            break;
        }
    }
}

fn build_row(
    trust: &'static str,
    r: CoreRow,
    facts: Option<&FileFacts>,
    test_globs: &[String],
    vendor_globs: &[String],
    generated_globs: &[String],
) -> UsageRow2 {
    let line_text = facts
        .and_then(|f| f.lines.get(r.row.line.saturating_sub(1) as usize))
        .map(|s| s.as_str())
        .unwrap_or(r.row.context.as_str());
    let kind = resolve_kind(&r, facts, line_text);
    let path = r.row.path.clone();
    let mut bits = 0u32;
    match r.detail.occ_role.as_deref() {
        Some("def") => bits |= roles::DEFINITION,
        Some("import") => bits |= roles::IMPORT,
        _ => {}
    }
    if r.row.access == Some("write") || matches!(kind, UsageKind::Write | UsageKind::Mutate) {
        bits |= roles::WRITE_ACCESS;
    }
    if r.row.access == Some("read") || kind == UsageKind::Read {
        bits |= roles::READ_ACCESS;
    }
    if crate::scopes::path_matches_any(&path, test_globs) {
        bits |= roles::TEST;
    }
    if crate::scopes::path_matches_any(&path, vendor_globs) {
        bits |= roles::VENDOR;
    }
    if crate::scopes::path_matches_any(&path, generated_globs) {
        bits |= roles::GENERATED;
    }
    let enclosing = facts.and_then(|f| {
        crate::annotations::enclosing_symbol(&f.symbols, r.row.line).map(|s| EnclosingSymbol {
            name: s.name.clone(),
            kind: s.kind.clone(),
            line: s.line_start,
            container: s.container.clone(),
        })
    });
    UsageRow2 {
        path,
        line: r.row.line,
        col: r.row.col,
        col_end: r.detail.col_end,
        blob_sha: facts.and_then(|f| f.blob_sha.clone()),
        kind,
        roles: bits,
        role_names: roles::names(bits),
        trust,
        precision: r.detail.precision,
        access: r.row.access,
        enclosing,
        context: r.row.context,
    }
}

/// The kind ladder, most-provable first. Nothing here guesses: the last
/// arm is `unclassified`, which D4 requires to stay a first-class outcome.
fn resolve_kind(r: &CoreRow, facts: Option<&FileFacts>, line_text: &str) -> UsageKind {
    if let Some(edge) = r.detail.rails_kind {
        return kind_from_rails_edge(edge, line_text);
    }
    match r.detail.occ_role.as_deref() {
        Some("def") => return UsageKind::Def,
        Some("import") => return UsageKind::Import,
        _ => {}
    }
    if let Some(k) = facts.and_then(|f| f.kinds.get(&(r.row.line, r.row.col)).copied()) {
        return k;
    }
    // The caller's own fallbacks, from data the ladder already had: a
    // read/write tag from `intel::access`, and a Ruby row the STRICT lane
    // BOUND (a bound local in a value position is a read — the one thing
    // `usekind` refuses to assume for Ruby without binding information).
    match r.row.access {
        Some("write") => return UsageKind::Write,
        Some("read") => return UsageKind::Read,
        _ => {}
    }
    if r.detail.precision == PRECISION_RUBY_STRICT
        || r.detail.precision == crate::resolve::PRECISION_FILE_LOCAL
    {
        return UsageKind::Read;
    }
    UsageKind::Unclassified
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_serialises_to_its_documented_wire_name() {
        for k in UsageKind::ALL {
            let json = serde_json::to_string(k).unwrap();
            assert_eq!(
                json,
                format!("\"{}\"", k.as_str()),
                "serde and as_str disagree for {k:?}"
            );
        }
    }

    #[test]
    fn role_bits_and_names_round_trip() {
        assert_eq!(roles::names(0), Vec::<&str>::new());
        assert_eq!(
            roles::names(roles::DEFINITION | roles::TEST),
            vec!["definition", "test"]
        );
        // Every declared bit decodes to exactly one name.
        for (bit, name) in roles::ALL {
            assert_eq!(roles::names(*bit), vec![*name]);
        }
    }

    #[test]
    fn rails_concern_include_reads_the_keyword_off_the_line() {
        use crate::frameworks::EdgeKind as E;
        assert_eq!(
            kind_from_rails_edge(E::ConcernInclude, "  include Payable"),
            UsageKind::Include
        );
        assert_eq!(
            kind_from_rails_edge(E::ConcernInclude, "  extend Findable"),
            UsageKind::Extend
        );
        assert_eq!(
            kind_from_rails_edge(E::ConcernInclude, "  prepend Auditable"),
            UsageKind::Prepend
        );
        // Unreadable line → unclassified, never a guessed `include`.
        assert_eq!(
            kind_from_rails_edge(E::ConcernInclude, "MIXINS.each { |m| send(:include, m) }"),
            UsageKind::Unclassified
        );
    }
}
