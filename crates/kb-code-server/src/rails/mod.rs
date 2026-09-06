//! `rails/1` (V72-I1) — the Rails ENTITY INDEX: a derived view that joins
//! three things this daemon already stores into the nouns a Rails developer
//! actually names.
//!
//! # What it joins, and what it refuses to be
//!
//! Three inputs, all already indexed, none of them new:
//!
//! * `entity_defs` (V0029, `entities/1`) — every Ruby `class`/`module`
//!   definition site with the FQN the tree proves and the one the app's
//!   Zeitwerk configuration derives for the path;
//! * `rails_edges` (V0026, `rails-lens/1`) — the convention edges the lens
//!   extracts at ingest (routes, renders, associations, i18n, callbacks, …);
//! * `files`/`symbols` — the mirror index, for the templates that are not
//!   constants and the methods that are not entities.
//!
//! Eight nouns come out: `model`, `controller`, `action`, `route`, `job`,
//! `mailer`, `view`, `concern` ([`NOUNS`]).
//!
//! **Computed PER REQUEST, persisted nowhere.** There is no `rails_entity`
//! table and no migration: the join is a fold over reads this daemon
//! already serves, in the posture root invariant #2 states for the whole
//! doc↔code bridge ("kb-code mints classes, nothing is cached") and
//! `codelens/1` and `entities/1` already hold. The measured cost is in
//! `crates/kb-code-server/tests/rails_route.rs`'s own budget test; the
//! alternative — a derived table refreshed on reconcile — would buy latency
//! at the price of a fourth thing that can be stale in a system whose whole
//! honesty story is "a stale fact must never read as a fresh one".
//!
//! # The trust floor is structural
//!
//! Every row this module emits carries a [`frameworks::Trust`], the
//! Rails-lens enum with **no `Exact` variant**. That is not a policy choice
//! a future edit could relax: a directory name, a file suffix and an
//! English pluralisation are conventions, and `crates/kb-code-server/
//! CLAUDE.md`'s oracle bar says a wrong `exact` is a release blocker. One
//! function ([`noun_trust`]) mints the class, from the row's own witness
//! count plus two demotions (a drifted blob, an unreadable Zeitwerk
//! config), and its return type cannot express `exact`.
//!
//! Every row also carries its [`Witness`] list — which convention, which
//! edge, which definition produced it — so a reader can check the
//! arithmetic rather than trust the badge.
//!
//! # What it cannot say, and says so
//!
//! * **Ancestry is not indexed.** There is no `entity_edges` table yet
//!   (`entities/mod.rs` records the deferral), so "descends from
//!   `ApplicationRecord`" is not a question this daemon can answer. A model
//!   is therefore a class under an `app/models` root, corroborated by the
//!   lens's own `association`/`validation`/`scope`/`callback` edges — and
//!   the note saying exactly that rides every model response.
//! * **Method visibility is not indexed.** `extract::Symbol` has no
//!   visibility field, so the `action` noun resolves `public` from the
//!   controller's own source with a cheap line scan, under a hard read
//!   budget ([`MAX_VISIBILITY_READS`]). Past the budget an action is
//!   `visibility: "unknown"`, flagged, and the response is `partial` with
//!   the budget named — never silently "public".
//! * **A pre-V72-I1 route edge has no address.** Verb and path ride
//!   `extra_json`, written by the extractor; an edge produced by an older
//!   binary has none until its source file is re-extracted, and reads as
//!   unknown rather than `/`.

use crate::extract::Symbol;
use crate::frameworks::{EdgeKind, FrameworkEdge, Trust};
use crate::routes::ApiError;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};

pub mod filter;
pub mod orphans;
pub mod routes;

/// The wire schema every `/api/rails/*` response carries.
pub const SCHEMA: &str = "rails/1";

/// The CLOSED noun vocabulary, in reading order — the ONE home for it.
/// The `/api/rails/<plural>` route segments and `kbcq/1`'s own `rails:`
/// value vocabulary (`search::grammar::FILTER_SPECS`) both read THIS list,
/// so a ninth noun cannot exist on one surface and not the other.
pub const NOUNS: &[&str] = &[
    "model",
    "controller",
    "action",
    "route",
    "job",
    "mailer",
    "view",
    "concern",
];

/// Controller sources read for a visibility line scan in ONE request.
/// Past this the remaining actions are honestly `unknown`. Sized above the
/// controller count of the monolith this lens was written against, so the
/// budget is a backstop rather than the ordinary path.
pub const MAX_VISIBILITY_READS: usize = 600;

/// `entity_defs` rows materialised per request. `entity_defs_for_repo`
/// takes a `LIMIT`, so a cap is not optional — it is only a question of
/// whether the caller reports hitting it.
pub const MAX_ENTITY_DEFS: usize = 200_000;

/// Rows per page when the caller names no `limit`.
pub const DEFAULT_LIMIT: usize = 100;
/// Hard ceiling on `limit` — a larger one is clamped and reported.
pub const MAX_LIMIT: usize = 1000;
/// Witnesses kept per row (a partial rendered by 400 call sites would
/// otherwise carry 400 witnesses into every page).
pub const MAX_WITNESSES: usize = 8;

// --- path conventions --------------------------------------------------

const MODEL_DIR: &str = "app/models/";
const CONTROLLER_DIR: &str = "app/controllers/";
const JOB_DIR: &str = "app/jobs/";
const MAILER_DIR: &str = "app/mailers/";
const VIEW_DIR: &str = "app/views/";
const CONCERN_DIRS: &[&str] = &["app/models/concerns/", "app/controllers/concerns/"];

fn is_concern_path(path: &str) -> bool {
    CONCERN_DIRS.iter().any(|d| path.starts_with(d))
}

/// The CONSTANT-backed noun a Ruby file's path names, or `None` for a path
/// no `app/` role convention claims. The ONE home for the path→noun rule:
/// [`build_index`] and [`filter::resolve`] both read it, so the search
/// box's `model:` and `/api/rails/models` can never disagree about what a
/// model is. `view` is deliberately absent — a template is not a constant
/// and is matched on the `app/views/` prefix directly.
pub(crate) fn noun_for_path(path: &str) -> Option<&'static str> {
    if is_concern_path(path) {
        Some("concern")
    } else if path.starts_with(MODEL_DIR) {
        Some("model")
    } else if path.starts_with(CONTROLLER_DIR) && path.ends_with("_controller.rb") {
        Some("controller")
    } else if path.starts_with(JOB_DIR) {
        Some("job")
    } else if path.starts_with(MAILER_DIR) {
        Some("mailer")
    } else {
        None
    }
}

/// `app/controllers/trade/rounds_controller.rb` → `trade/rounds` — the key
/// `rails-lens/1` itself uses on the left of a `route_action` edge's
/// `dst_symbol`, which is what makes routes and actions joinable at all.
fn controller_key(path: &str) -> Option<String> {
    path.strip_prefix(CONTROLLER_DIR)?
        .strip_suffix("_controller.rb")
        .map(|s| s.to_string())
}

// --- honesty -----------------------------------------------------------

/// The four read states every response in this family reports: `ok`,
/// `empty` (with a reason), `partial` (with the budget that bit) or
/// `error`. `error` is unreachable from these handlers — a genuine failure
/// is an [`ApiError`] — and is declared so the vocabulary is complete
/// wherever it is documented.
pub const STATE_OK: &str = "ok";
pub const STATE_EMPTY: &str = "empty";
pub const STATE_PARTIAL: &str = "partial";
pub const STATE_ERROR: &str = "error";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Honesty {
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Honesty {
    pub fn ok() -> Self {
        Honesty {
            state: STATE_OK,
            reason: None,
        }
    }
    pub fn empty(reason: impl Into<String>) -> Self {
        Honesty {
            state: STATE_EMPTY,
            reason: Some(reason.into()),
        }
    }
    pub fn partial(reason: impl Into<String>) -> Self {
        Honesty {
            state: STATE_PARTIAL,
            reason: Some(reason.into()),
        }
    }
}

// --- rows --------------------------------------------------------------

/// Where a fact came from. Kinds: `convention` (a path rule), `entity` (an
/// indexed definition site), `rails-edge` (a `rails-lens/1` row, carrying
/// its own trust) and `symbol` (a mirror-index symbol).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Witness {
    pub kind: &'static str,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust: Option<Trust>,
}

impl Witness {
    fn convention(detail: impl Into<String>) -> Self {
        Witness {
            kind: "convention",
            detail: detail.into(),
            path: None,
            line: None,
            trust: None,
        }
    }
    fn entity(fqn: &str, path: &str, line: u32) -> Self {
        Witness {
            kind: "entity",
            detail: format!("entities/1 indexed a definition of {fqn} here"),
            path: Some(path.to_string()),
            line: Some(line),
            trust: None,
        }
    }
    fn symbol(name: &str, path: &str, line: u32) -> Self {
        Witness {
            kind: "symbol",
            detail: format!("method {name} in a controller class"),
            path: Some(path.to_string()),
            line: Some(line),
            trust: None,
        }
    }
    fn edge(e: &FrameworkEdge) -> Self {
        Witness {
            kind: "rails-edge",
            detail: format!("rails-lens/1 {}", e.kind.as_str()),
            path: Some(e.src_path.clone()),
            line: e.src_line,
            trust: Some(e.trust),
        }
    }
}

/// A route's address: the verb and URL the lens reconstructed plus the
/// `controller#action` it dispatches to. `verb`/`path` are `None` for an
/// edge written before the extractor recorded them, or one whose pattern
/// was not literal — unknown, never `/`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RouteTriple {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verb: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub target: String,
}

/// One Rails noun, as an ADDRESS plus its evidence.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RailsRow {
    pub noun: &'static str,
    /// The name a human types: an FQN, a `controller#action`, a route
    /// address, a template path.
    pub name: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fqn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<RouteTriple>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<&'static str>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub counts: BTreeMap<&'static str, usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<&'static str>,
    pub trust: Trust,
    pub witnesses: Vec<Witness>,
}

/// The ONE function that mints a `rails/1` row's class.
///
/// Two independent witnesses, a live blob and a Zeitwerk config that could
/// be read buys `likely`; anything less is `candidate`. Nothing buys
/// `exact` — [`Trust`] has no such variant, so this is enforced by the type
/// rather than by remembering. The witness list is on the wire beside the
/// class for exactly this reason: the arithmetic is checkable.
pub fn noun_trust(witnesses: usize, stale: bool, zeitwerk_degraded: bool) -> Trust {
    if stale || zeitwerk_degraded || witnesses < 2 {
        Trust::Candidate
    } else {
        Trust::Likely
    }
}

// --- lens freshness ----------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LensFreshness {
    /// Every `rails_edges` row for this repo.
    pub edges_total: usize,
    /// Distinct source paths that produced at least one edge.
    pub source_files: usize,
    /// Source paths whose edges were extracted from a blob that is no
    /// longer the file's live one — the lens has not caught up.
    pub stale_source_files: usize,
    /// Source paths the lens produced edges for that no longer exist in the
    /// mirror index at all.
    pub orphan_source_files: usize,
    pub grammar_version: &'static str,
    /// The store's monotonic index generation as this response was
    /// assembled — NOT a commit distance (that would be a git call per
    /// request), the same honest half `unified.rs` reports.
    pub generation: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ZeitwerkNote {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// --- the index ---------------------------------------------------------

/// The whole per-request join. Built once; `home`, every noun list and the
/// orphan report are three views of it.
pub struct RailsIndex {
    pub detected: bool,
    pub detect_reason: Option<String>,
    pub rails_version: Option<String>,
    pub version_source: Option<&'static str>,
    pub nouns: BTreeMap<&'static str, Vec<RailsRow>>,
    pub lens: LensFreshness,
    pub zeitwerk: ZeitwerkNote,
    pub notes: Vec<String>,
    /// Budget captions — non-empty means the response is `partial`.
    pub budgets: Vec<String>,
    pub(crate) edges: Vec<FrameworkEdge>,
}

impl RailsIndex {
    pub fn rows(&self, noun: &str) -> &[RailsRow] {
        self.nouns.get(noun).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn counts(&self) -> BTreeMap<&'static str, usize> {
        NOUNS
            .iter()
            .map(|noun| (*noun, self.rows(noun).len()))
            .collect()
    }

    /// `ok` / `empty` / `partial`, derived — never asserted by a caller.
    pub fn honesty(&self, rows_returned: usize) -> Honesty {
        if !self.detected {
            return Honesty::empty(
                self.detect_reason
                    .clone()
                    .unwrap_or_else(|| "not a Rails application".to_string()),
            );
        }
        if !self.budgets.is_empty() {
            return Honesty::partial(self.budgets.join("; "));
        }
        if rows_returned == 0 {
            return Honesty::empty(
                "no rows for this noun — the Rails lens produced no matching edges or \
                 definitions in this repo",
            );
        }
        Honesty::ok()
    }
}

/// Everything [`build_index`] needs from the caller, so the signature does
/// not grow one parameter per read.
pub struct IndexInputs<'a> {
    pub repo_root: &'a std::path::Path,
    pub repo_id: i64,
}

/// Build the whole index. Runs inside ONE `run_blocking` closure (the
/// 2026-08-31 starvation convention): every store read and every controller
/// source read happens here, on the blocking pool, never interleaved with
/// an await.
pub fn build_index(
    store: &crate::store::Store,
    input: IndexInputs<'_>,
) -> Result<RailsIndex, ApiError> {
    let repo_id = input.repo_id;
    let repo_root = input.repo_root;

    let detected = crate::frameworks::rails::detect_is_rails(repo_root);
    let files = store.list_files(repo_id)?;
    let edges = store.rails_edges_for_repo(repo_id)?;
    let entities = store.entity_defs_for_repo(repo_id, None, MAX_ENTITY_DEFS)?;
    let controller_symbols = store.symbols_for_repo_under_prefix(repo_id, CONTROLLER_DIR)?;
    let generation = store.generation();

    let zeitwerk = crate::entities::zeitwerk::zeitwerk_for(repo_root);
    let degraded = zeitwerk.state == crate::entities::zeitwerk::STATE_DEGRADED;

    let mut notes: Vec<String> = Vec::new();
    let mut budgets: Vec<String> = Vec::new();
    if entities.len() >= MAX_ENTITY_DEFS {
        budgets.push(format!(
            "the entity index was read up to {MAX_ENTITY_DEFS} definitions; rows beyond that \
             cap are missing from every noun on this page"
        ));
    }
    if degraded {
        notes.push(format!(
            "the app's Zeitwerk configuration could not be read ({}), so every constant name \
             here is a directory convention and caps at candidate",
            zeitwerk.reason.clone().unwrap_or_default()
        ));
    }

    let live_blob: HashMap<&str, &str> = files
        .iter()
        .map(|f| (f.path.as_str(), f.blob_hash.as_str()))
        .collect();
    let file_paths: HashSet<String> = files.iter().map(|f| f.path.clone()).collect();

    // Inbound edges, keyed by destination — the join every "who reaches
    // this?" count below is a lookup in.
    let mut by_dst: HashMap<&str, Vec<&FrameworkEdge>> = HashMap::new();
    let mut by_src: HashMap<&str, Vec<&FrameworkEdge>> = HashMap::new();
    for e in &edges {
        if let Some(d) = e.dst_path.as_deref() {
            by_dst.entry(d).or_default().push(e);
        }
        by_src.entry(e.src_path.as_str()).or_default().push(e);
    }

    let lens = lens_freshness(&edges, &by_src, &live_blob, generation);

    let mut nouns: BTreeMap<&'static str, Vec<RailsRow>> = BTreeMap::new();
    for noun in NOUNS {
        nouns.insert(*noun, Vec::new());
    }

    // --- constant-backed nouns (model / controller / job / mailer / concern)
    let own_constant = own_constant_per_path(&entities);
    for row in &entities {
        if row.kind != "class" && row.kind != "module" {
            continue;
        }
        // ONE constant per file is the noun; the `module Admin` wrapping a
        // `class Admin::ReportsController` is a NAMESPACE declaration, not a
        // second controller. Zeitwerk's own convention decides which is
        // which (see `own_constant_per_path`).
        if own_constant.get(row.path.as_str()) != Some(&row.fqn.as_str()) {
            continue;
        }
        let stale = row
            .live_blob_hash
            .as_deref()
            .map(|live| live != row.blob_hash)
            .unwrap_or(true);
        let path = row.path.as_str();
        let Some(noun) = noun_for_path(path) else {
            continue;
        };
        let inbound = by_dst.get(path).map(|v| v.as_slice()).unwrap_or(&[]);
        let outbound = by_src.get(path).map(|v| v.as_slice()).unwrap_or(&[]);

        let mut witnesses = vec![
            Witness::convention(format!(
                "path convention: {} → {noun}",
                convention_rule(noun)
            )),
            Witness::entity(&row.fqn, path, row.line_start as u32),
        ];
        let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
        let mut table = None;

        match noun {
            "model" => {
                counts.insert("associations", count_kind(outbound, EdgeKind::Association));
                counts.insert("validations", count_kind(outbound, EdgeKind::Validation));
                counts.insert("scopes", count_kind(outbound, EdgeKind::Scope));
                counts.insert("callbacks", count_kind(outbound, EdgeKind::Callback));
                counts.insert("concerns", count_kind(outbound, EdgeKind::ConcernInclude));
                table = Some(table_name_for(&row.fqn));
                push_edge_witnesses(
                    &mut witnesses,
                    outbound,
                    &[
                        EdgeKind::Association,
                        EdgeKind::Validation,
                        EdgeKind::Scope,
                        EdgeKind::Callback,
                    ],
                );
            }
            "controller" => {
                counts.insert("routes", count_kind(inbound, EdgeKind::RouteAction));
                counts.insert("renders", count_kind(outbound, EdgeKind::RenderView));
                counts.insert("concerns", count_kind(outbound, EdgeKind::ConcernInclude));
                push_edge_witnesses(&mut witnesses, inbound, &[EdgeKind::RouteAction]);
            }
            "job" => {
                counts.insert("enqueue_sites", count_kind(inbound, EdgeKind::JobEnqueue));
                push_edge_witnesses(&mut witnesses, inbound, &[EdgeKind::JobEnqueue]);
            }
            "mailer" => {
                counts.insert(
                    "deliver_sites",
                    count_kind(inbound, EdgeKind::MailerDeliver),
                );
                push_edge_witnesses(&mut witnesses, inbound, &[EdgeKind::MailerDeliver]);
            }
            "concern" => {
                counts.insert("included_by", count_kind(inbound, EdgeKind::ConcernInclude));
                push_edge_witnesses(&mut witnesses, inbound, &[EdgeKind::ConcernInclude]);
            }
            _ => {}
        }

        let mut flags = Vec::new();
        if stale {
            flags.push("blob-drifted");
        }
        let trust = noun_trust(witnesses.len(), stale, degraded);
        witnesses.truncate(MAX_WITNESSES);
        nouns.entry(noun).or_default().push(RailsRow {
            noun,
            name: row.fqn.clone(),
            path: path.to_string(),
            line: Some(row.line_start as u32),
            blob_sha: live_blob.get(path).map(|s| s.to_string()),
            fqn: Some(row.fqn.clone()),
            route: None,
            table,
            visibility: None,
            counts,
            flags,
            trust,
            witnesses,
        });
    }

    // --- views: templates are files, not constants ------------------------
    for f in &files {
        if !f.path.starts_with(VIEW_DIR) {
            continue;
        }
        let inbound = by_dst
            .get(f.path.as_str())
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let rendered_by = inbound
            .iter()
            .filter(|e| {
                matches!(
                    e.kind,
                    EdgeKind::RenderView | EdgeKind::RenderPartial | EdgeKind::ViewComponentRender
                )
            })
            .count();
        let mut witnesses = vec![Witness::convention(format!(
            "path convention: {} → view",
            convention_rule("view")
        ))];
        push_edge_witnesses(
            &mut witnesses,
            inbound,
            &[
                EdgeKind::RenderView,
                EdgeKind::RenderPartial,
                EdgeKind::ViewComponentRender,
            ],
        );
        let mut counts = BTreeMap::new();
        counts.insert("rendered_by", rendered_by);
        let name = f.path.trim_start_matches(VIEW_DIR).to_string();
        let is_partial = std::path::Path::new(&f.path)
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.starts_with('_'))
            .unwrap_or(false);
        let mut flags = Vec::new();
        if is_partial {
            flags.push("partial");
        }
        let trust = noun_trust(witnesses.len(), false, degraded);
        witnesses.truncate(MAX_WITNESSES);
        nouns.entry("view").or_default().push(RailsRow {
            noun: "view",
            name,
            path: f.path.clone(),
            line: None,
            blob_sha: Some(f.blob_hash.clone()),
            fqn: None,
            route: None,
            table: None,
            visibility: None,
            counts,
            flags,
            trust,
            witnesses,
        });
    }

    // --- actions: methods of a controller class ---------------------------
    let route_edges: Vec<&FrameworkEdge> = edges
        .iter()
        .filter(|e| e.kind == EdgeKind::RouteAction)
        .collect();
    // (controller path, action name) → the routes that reach it.
    let mut routes_for_action: HashMap<(String, String), Vec<&FrameworkEdge>> = HashMap::new();
    for e in &route_edges {
        let (Some(dst), Some(sym)) = (e.dst_path.as_deref(), e.dst_symbol.as_deref()) else {
            continue;
        };
        let Some((_, action)) = sym.split_once('#') else {
            continue;
        };
        routes_for_action
            .entry((dst.to_string(), action.to_string()))
            .or_default()
            .push(*e);
    }

    let visibility = resolve_visibility(repo_root, &controller_symbols, &mut budgets);
    for (path, sym) in &controller_symbols {
        if sym.kind != "method" || !path.ends_with("_controller.rb") || is_concern_path(path) {
            continue;
        }
        let vis = match visibility.get(path.as_str()) {
            Some(Some(cut)) if sym.line_start > *cut => "private",
            Some(_) => "public",
            // The file was past this request's read budget — say so rather
            // than assume "public" for a method nobody looked at.
            None => "unknown",
        };
        if vis == "private" {
            continue;
        }
        let routes = routes_for_action
            .get(&(path.clone(), sym.name.clone()))
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let key = controller_key(path).unwrap_or_else(|| path.clone());
        let mut witnesses = vec![Witness::symbol(&sym.name, path, sym.line_start)];
        for e in routes.iter().take(MAX_WITNESSES) {
            witnesses.push(Witness::edge(e));
        }
        let mut counts = BTreeMap::new();
        counts.insert("routes", routes.len());
        let mut flags = Vec::new();
        if vis == "unknown" {
            flags.push("visibility-unknown");
        }
        let trust = noun_trust(witnesses.len(), false, degraded);
        witnesses.truncate(MAX_WITNESSES);
        nouns.entry("action").or_default().push(RailsRow {
            noun: "action",
            name: format!("{key}#{}", sym.name),
            path: path.clone(),
            line: Some(sym.line_start),
            blob_sha: live_blob.get(path.as_str()).map(|s| s.to_string()),
            fqn: sym.container.as_ref().map(|c| format!("{c}#{}", sym.name)),
            route: None,
            table: None,
            visibility: Some(vis),
            counts,
            flags,
            trust,
            witnesses,
        });
    }

    // --- routes ------------------------------------------------------------
    let action_index: HashSet<(String, String)> = nouns
        .get("action")
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    r.name
                        .rsplit_once('#')
                        .map(|(_, a)| (r.path.clone(), a.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();

    for e in &route_edges {
        let target = e.dst_symbol.clone().unwrap_or_default();
        let addr = parse_route_addr(e.extra_json.as_deref());
        let dst = e.dst_path.clone().unwrap_or_default();
        let action = target.rsplit_once('#').map(|(_, a)| a.to_string());
        let mut flags = Vec::new();
        if !file_paths.contains(&dst) {
            flags.push("controller-missing");
        } else if action
            .as_ref()
            .map(|a| !action_index.contains(&(dst.clone(), a.clone())))
            .unwrap_or(true)
        {
            flags.push("action-missing");
        }
        if addr.0.is_none() {
            flags.push("address-unknown");
        }
        let name = match (&addr.0, &addr.1) {
            (Some(v), Some(p)) => format!("{v} {p}"),
            _ => target.clone(),
        };
        // A route's class is the EDGE's own — never re-derived upward —
        // demoted once more when its target cannot be found.
        let trust = if flags.is_empty() {
            e.trust
        } else {
            Trust::Candidate
        };
        let mut witnesses = vec![Witness::edge(e)];
        if !dst.is_empty() {
            witnesses.push(Witness::convention(format!(
                "route target resolved by convention to {dst}"
            )));
        }
        nouns.entry("route").or_default().push(RailsRow {
            noun: "route",
            name,
            path: e.src_path.clone(),
            line: e.src_line,
            blob_sha: live_blob.get(e.src_path.as_str()).map(|s| s.to_string()),
            fqn: None,
            route: Some(RouteTriple {
                verb: addr.0,
                path: addr.1,
                target,
            }),
            table: None,
            visibility: None,
            counts: BTreeMap::new(),
            flags,
            trust,
            witnesses,
        });
    }

    for rows in nouns.values_mut() {
        rows.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    }

    notes.push(
        "class ancestry is not indexed (there is no entity-edge table yet), so a model, job, \
         mailer or controller here is a class under the matching app/ root corroborated by the \
         lens's own edges — not a proven ActiveRecord/ActiveJob descendant"
            .to_string(),
    );

    let (rails_version, version_source) = read_rails_version(repo_root);
    let detect_reason = if detected {
        None
    } else {
        Some(
            "no Rails application detected: config/routes.rb and a Gemfile declaring \
             gem \"rails\" are both required"
                .to_string(),
        )
    };

    Ok(RailsIndex {
        detected,
        detect_reason,
        rails_version,
        version_source,
        nouns,
        lens,
        zeitwerk: ZeitwerkNote {
            state: zeitwerk.state.to_string(),
            reason: zeitwerk.reason.clone(),
        },
        notes,
        budgets,
        edges,
    })
}

/// For each path, THE constant that file defines — the one the app's
/// Zeitwerk configuration derives for it (`zeitwerk_fqn`), falling back to
/// the most deeply nested constant in the file when that read degraded.
///
/// A Rails file declares one constant and, on the way there, however many
/// enclosing modules its namespace needs: `app/controllers/admin/
/// reports_controller.rb` produces an `Admin` row AND an
/// `Admin::ReportsController` row. Listing both as controllers would put a
/// namespace in the census and double every namespaced count. The fallback
/// (deepest nesting) is a convention like every other fact here, and the
/// row it picks carries the `candidate` cap the degraded Zeitwerk state
/// already forces.
fn own_constant_per_path<'a>(
    entities: &'a [crate::store::EntityDefRow],
) -> HashMap<&'a str, &'a str> {
    /// The best candidate seen so far for one path. A Zeitwerk-confirmed
    /// constant always wins; otherwise the deeper nesting does; a tie keeps
    /// the first (the extractor's deterministic emission order).
    struct Best<'a> {
        fqn: &'a str,
        zeitwerk: bool,
        depth: usize,
    }
    let mut best: HashMap<&'a str, Best<'a>> = HashMap::new();
    for row in entities {
        if row.kind != "class" && row.kind != "module" {
            continue;
        }
        let cand = Best {
            fqn: row.fqn.as_str(),
            zeitwerk: row.zeitwerk_fqn.as_deref() == Some(row.fqn.as_str()),
            depth: row.fqn.matches("::").count(),
        };
        match best.get(row.path.as_str()) {
            Some(cur) if cur.zeitwerk || !(cand.zeitwerk || cand.depth > cur.depth) => {}
            _ => {
                best.insert(row.path.as_str(), cand);
            }
        }
    }
    best.into_iter().map(|(p, b)| (p, b.fqn)).collect()
}

fn convention_rule(noun: &str) -> &'static str {
    match noun {
        "model" => "app/models/**.rb",
        "controller" => "app/controllers/**_controller.rb",
        "job" => "app/jobs/**.rb",
        "mailer" => "app/mailers/**.rb",
        "concern" => "app/{models,controllers}/concerns/**.rb",
        "view" => "app/views/**",
        _ => "app/**",
    }
}

fn count_kind(edges: &[&FrameworkEdge], kind: EdgeKind) -> usize {
    edges.iter().filter(|e| e.kind == kind).count()
}

fn push_edge_witnesses(out: &mut Vec<Witness>, edges: &[&FrameworkEdge], kinds: &[EdgeKind]) {
    for e in edges.iter().filter(|e| kinds.contains(&e.kind)) {
        if out.len() >= MAX_WITNESSES {
            return;
        }
        out.push(Witness::edge(e));
    }
}

/// `{"verb":"GET","path":"/orders/:id"}` → `(Some("GET"), Some("/orders/:id"))`.
/// Absent, malformed or partial input yields `None`s — an address this
/// daemon cannot read is unknown, never invented.
fn parse_route_addr(extra_json: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(raw) = extra_json else {
        return (None, None);
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return (None, None);
    };
    (
        v.get("verb").and_then(|x| x.as_str()).map(str::to_string),
        v.get("path").and_then(|x| x.as_str()).map(str::to_string),
    )
}

fn lens_freshness(
    edges: &[FrameworkEdge],
    by_src: &HashMap<&str, Vec<&FrameworkEdge>>,
    live_blob: &HashMap<&str, &str>,
    generation: u64,
) -> LensFreshness {
    let mut orphan = 0usize;
    for path in by_src.keys() {
        if !live_blob.contains_key(*path) {
            orphan += 1;
        }
    }
    LensFreshness {
        edges_total: edges.len(),
        source_files: by_src.len(),
        // The lens replaces a path's rows wholesale on every visit and the
        // rows carry no blob, so "stale" here is the honest subset we CAN
        // see: a source path the mirror no longer knows at all.
        stale_source_files: orphan,
        orphan_source_files: orphan,
        grammar_version: crate::frameworks::RAILS_LENS_GRAMMAR_VERSION,
        generation,
    }
}

/// For each controller path, the line at or after which methods stop being
/// public — `Some(line)` for a file with a bare `private`/`protected`,
/// `None` for one without, and NO ENTRY AT ALL for a file the read budget
/// did not reach (which is what makes an action `visibility: "unknown"`
/// rather than silently public).
///
/// A LINE SCAN, deliberately not a second tree-sitter parse: this runs over
/// every controller in the repo on a request path, and the fact it needs —
/// "is there a bare `private` above this method" — is a line, not a tree.
/// It is a convention read like every other fact in this module, and it
/// misses the inline `private def foo` and `private :foo` forms, which is
/// why it can only ever DEMOTE a row and never promote one.
fn resolve_visibility(
    repo_root: &std::path::Path,
    controller_symbols: &[(String, Symbol)],
    budgets: &mut Vec<String>,
) -> HashMap<String, Option<u32>> {
    let mut paths: Vec<&str> = controller_symbols
        .iter()
        .map(|(p, _)| p.as_str())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    paths.sort_unstable();
    let total = paths.len();
    let mut out = HashMap::new();
    for path in paths.into_iter().take(MAX_VISIBILITY_READS) {
        let Ok(abs) = crate::security::paths::contained_abs_path(repo_root, path) else {
            continue;
        };
        let Ok(bytes) = std::fs::read(&abs) else {
            continue;
        };
        out.insert(path.to_string(), visibility_cut(&bytes));
    }
    if total > MAX_VISIBILITY_READS {
        budgets.push(format!(
            "method visibility was resolved for the first {MAX_VISIBILITY_READS} of {total} \
             controller files; the rest report visibility \"unknown\""
        ));
    }
    out
}

/// The 1-based line of the first bare `private`/`protected` statement.
pub(crate) fn visibility_cut(bytes: &[u8]) -> Option<u32> {
    let text = String::from_utf8_lossy(bytes);
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        let t = t.split('#').next().unwrap_or(t).trim();
        if t == "private" || t == "protected" {
            return Some(i as u32 + 1);
        }
    }
    None
}

/// `Order` → `orders`, `Billing::Invoice` → `invoices`.
///
/// ActiveRecord's default demodulizes: a namespaced model uses its LAST
/// segment unless the module declares `table_name_prefix`. Neither that nor
/// an explicit `self.table_name` is indexed, so this is a convention like
/// every other fact here and carries the row's own cap.
pub(crate) fn table_name_for(fqn: &str) -> String {
    let leaf = fqn.rsplit("::").next().unwrap_or(fqn);
    crate::frameworks::rails::routes::pluralize(&underscore(leaf))
}

/// `CamelCase` → `snake_case`, honouring an acronym run (`APIKey` →
/// `api_key`) the way ActiveSupport's own `underscore` does.
pub(crate) fn underscore(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);
    for (i, c) in chars.iter().enumerate() {
        if c.is_uppercase() && i > 0 {
            let prev_lower = chars[i - 1].is_lowercase() || chars[i - 1].is_ascii_digit();
            let next_lower = chars.get(i + 1).map(|n| n.is_lowercase()).unwrap_or(false);
            if prev_lower || next_lower {
                out.push('_');
            }
        }
        for lc in c.to_lowercase() {
            out.push(lc);
        }
    }
    out
}

/// The Rails version string, preferring `Gemfile.lock`'s resolved
/// `rails (X)` over `Gemfile`'s declared constraint. `None` = genuinely
/// unknown; the passport prints that rather than a guess.
fn read_rails_version(repo_root: &std::path::Path) -> (Option<String>, Option<&'static str>) {
    if let Ok(lock) = std::fs::read_to_string(repo_root.join("Gemfile.lock")) {
        for line in lock.lines() {
            let t = line.trim();
            if let Some(rest) = t.strip_prefix("rails (") {
                if let Some(v) = rest.strip_suffix(')') {
                    if !v.is_empty() && v.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                        return (Some(v.to_string()), Some("Gemfile.lock"));
                    }
                }
            }
        }
    }
    if let Ok(gemfile) = std::fs::read_to_string(repo_root.join("Gemfile")) {
        for line in gemfile.lines() {
            let t = line.trim();
            for prefix in [r#"gem "rails""#, "gem 'rails'"] {
                if let Some(rest) = t.strip_prefix(prefix) {
                    let rest = rest.trim_start_matches(',').trim();
                    let v: String = rest
                        .trim_matches(|c| c == '"' || c == '\'')
                        .chars()
                        .take_while(|c| *c != '"' && *c != '\'')
                        .collect();
                    let v = v.trim();
                    if !v.is_empty() {
                        return (Some(v.to_string()), Some("Gemfile"));
                    }
                    return (None, Some("Gemfile"));
                }
            }
        }
    }
    (None, None)
}

#[cfg(test)]
mod tests;
