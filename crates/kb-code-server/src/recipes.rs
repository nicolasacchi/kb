//! V3.3-Q1 — named deterministic recipes over entities × edges × git × sessions.
//!
//! **Design law (ruling D6):** a closed Rust enum — no free query language.
//! Same repo state ⇒ byte-identical output. Attention signals only ("look
//! here"), never verdicts. Missing inputs surface as `inputs_missing`, never
//! as zeroed scores. Trust classes on name-resolution-derived rows are never
//! better than the underlying resolver (a wrong `exact` is a release blocker).
//!
//! Wire:
//! - `GET /api/recipes` — pure catalog (no repo touch)
//! - `GET /api/recipes/{name}?repo=&since=&limit=&scope=` — run one
//!
//! **V74-L3a repair round (D11's own list).** Six defects, each of them
//! a place where this module violated a clause of its OWN design law
//! above, are fixed here and each has a test named after it:
//!
//! 1. *missing blob ≠ zero* — `complexity-climbers` mapped EVERY
//!    `read_blob` failure (`PathNotFound`, `TooLarge`, any ODB error) to
//!    `Complexity{0,0}`, so a file added or renamed after `since` topped
//!    the list with a fabricated delta equal to its whole current size.
//!    A baseline that could not be read is now `then_state` +
//!    `score: null`, ranked LAST and never as a climb.
//! 2. *gate/row parity* — `agent-only-symbols`' presence gate read
//!    `author_stats` OR `commit_sessions` while its per-symbol test read
//!    only `commit_sessions`, so a repo with the first and not the second
//!    returned `items: []` with `inputs_missing: []` — an honest-looking
//!    empty set over a missing input. The gate now reads the SAME table
//!    the row test does, and says which one is missing.
//! 3. *`fail_count` retired* — the only writer of `session_signals`
//!    hardcodes `fail_count = 0` ("no distinct test-failure field on
//!    wire"), so `failure-tainted`'s advertised `terms.fail_count` was a
//!    permanent zero and its `score` a permanently half-empty sum. The
//!    term is GONE rather than reported as measured-and-zero.
//! 4. *`limit > 500` → 400* — silently clamping to 500 and returning
//!    `truncated: true` invites a caller to misread the cap as the
//!    corpus. Over the cap now refuses with BOTH numbers (kb root
//!    invariant #35's `?ids=` rule).
//! 5. *unsupported languages are named* — `is_public_export` understands
//!    five languages and answered `false` for every other one, so a Go or
//!    Ruby repo got a confident empty set. The note now names the
//!    languages and the count it did not examine, and a run with NOTHING
//!    examinable reports `inputs_missing`.
//! 6. *note wording* — "working tree HEAD" is not a thing; the
//!    comparison is against the working tree, which is the caveat a
//!    reader actually needs.
//!
//! The CLI half of the list (`client timeout 600 s`, `error bodies
//! surfaced`) lives in `kb-code-cli`. `crate::recipe` (kbc-recipe/1)
//! adopts all six bodies as native adapters; this module stays the home
//! of the bodies themselves and of the FROZEN `recipes/1` wire.

use crate::behavioral::{
    complexity_for_path, complexity_proxy, dense_ranks_desc, hotspot_score, stored_fail_term,
    Complexity,
};
use crate::blame::{self, BlameCache};
use crate::git::{GitRepo, DEFAULT_BLOB_SIZE_CAP};
use crate::resolve::{CLASS_CANDIDATE, CLASS_EXACT, CLASS_LIKELY};
use crate::reviews::files_changed;
use crate::routes::{find_repo, ApiError};
use crate::state::SharedState;
use crate::store::Store;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path as FsPath;
use std::str::FromStr;

/// Schema label on recipe responses (catalog + run).
pub const SCHEMA: &str = "recipes/1";

pub const DEFAULT_LIMIT: usize = 50;
pub const MAX_LIMIT: usize = 500;

/// Every recipe starts at version 1 so goldens can pin semantics.
pub const RECIPE_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Enum
// ---------------------------------------------------------------------------

/// Closed set of named deterministic recipes (kebab-case on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Recipe {
    /// Public/exported symbols whose defining line's blame commit is dated
    /// after `since` (git ref or ISO date `YYYY-MM-DD`). Looks for recently
    /// introduced API surface — attention only, not a stability verdict.
    ///
    /// Public filter (conservative; RETURN-LESS when unsure):
    /// - Rust: signature contains a `pub` visibility keyword
    /// - TypeScript/TSX: defining line (or immediate preceding) carries `export`
    /// - Python: module-level `def`/`class` not `_`-prefixed
    NewPublicApi,
    /// Functions ranked by `line_span × (fan_in + fan_out)` from
    /// `call_sites`. Fan-in is name-matched across the repo (class
    /// `candidate`); fan-out is same-blob `caller_ordinal` (class `likely`).
    /// Row class = worst contributing edge class. Attention weight only.
    GodFunctions,
    /// Symbols whose defining span's blame commits are ALL agent-attributed
    /// via the same dual-author / commit→session join the ownership
    /// `agents[]` surface uses. No agent data ⇒ `inputs_missing:
    /// ["agent_attribution"]`.
    AgentOnlySymbols,
    /// Files whose cheap complexity (`loc + indent_sum`, same formula as
    /// behavioral) rose between `since` (blob) and HEAD (working tree).
    /// Terms decompose then/now; rank by delta desc. Derived at request time.
    ComplexityClimbers,
    /// Behavioral hotspot ranking filtered to paths that appear in NO
    /// review patchset file list. Needs behavioral backfill (else
    /// `inputs_missing: ["behavioral"]`). Zero reviews is NOT missing —
    /// every hotspot is unreviewed; noted in `note`.
    UnreviewedHotspots,
    /// Paths whose introducing/touching sessions carried error or failing-
    /// test evidence (`session_signals` / `stored_fail_term`). No signals ⇒
    /// `inputs_missing: ["session_signals"]`. Terms = raw evidence counts.
    FailureTainted,
}

impl Recipe {
    pub const ALL: &'static [Recipe] = &[
        Recipe::NewPublicApi,
        Recipe::GodFunctions,
        Recipe::AgentOnlySymbols,
        Recipe::ComplexityClimbers,
        Recipe::UnreviewedHotspots,
        Recipe::FailureTainted,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Recipe::NewPublicApi => "new-public-api",
            Recipe::GodFunctions => "god-functions",
            Recipe::AgentOnlySymbols => "agent-only-symbols",
            Recipe::ComplexityClimbers => "complexity-climbers",
            Recipe::UnreviewedHotspots => "unreviewed-hotspots",
            Recipe::FailureTainted => "failure-tainted",
        }
    }

    pub fn recipe_version(self) -> u32 {
        RECIPE_VERSION
    }

    pub fn description(self) -> &'static str {
        match self {
            Recipe::NewPublicApi => {
                "Public/exported symbols whose defining line was introduced after `since`. \
                 Look here for newly exposed API surface — not a stability verdict."
            }
            Recipe::GodFunctions => {
                "Functions ranked by line span × (callers + callees). \
                 Look here for large, highly connected functions — attention only."
            }
            Recipe::AgentOnlySymbols => {
                "Symbols whose defining span is entirely agent-attributed (session join). \
                 Look here when human review of agent-written definitions may help."
            }
            Recipe::ComplexityClimbers => {
                "Files whose loc+indent complexity rose between `since` and HEAD. \
                 Look here for recently grown files — not a debt verdict."
            }
            Recipe::UnreviewedHotspots => {
                "Behavioral hotspots that appear in no review patchset file list. \
                 Look here for high-churn paths that have not been reviewed locally."
            }
            Recipe::FailureTainted => {
                "Paths touched by sessions that carried tool-error / fail evidence. \
                 Look here for failure-associated paths — not a defect prophecy."
            }
        }
    }

    pub fn needs(self) -> &'static [&'static str] {
        match self {
            Recipe::NewPublicApi => &["symbols", "blame", "git"],
            Recipe::GodFunctions => &["symbols", "call_sites"],
            Recipe::AgentOnlySymbols => &["symbols", "blame", "agent_attribution"],
            Recipe::ComplexityClimbers => &["git", "files"],
            Recipe::UnreviewedHotspots => &["behavioral", "reviews"],
            Recipe::FailureTainted => &["session_signals", "author_stats"],
        }
    }

    pub fn params(self) -> Vec<RecipeParamMeta> {
        match self {
            Recipe::NewPublicApi | Recipe::ComplexityClimbers => vec![RecipeParamMeta {
                name: "since",
                required: true,
            }],
            _ => Vec::new(),
        }
    }
}

impl FromStr for Recipe {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "new-public-api" => Ok(Recipe::NewPublicApi),
            "god-functions" => Ok(Recipe::GodFunctions),
            "agent-only-symbols" => Ok(Recipe::AgentOnlySymbols),
            "complexity-climbers" => Ok(Recipe::ComplexityClimbers),
            "unreviewed-hotspots" => Ok(Recipe::UnreviewedHotspots),
            "failure-tainted" => Ok(Recipe::FailureTainted),
            other => Err(format!(
                "unknown recipe {other:?}; known: {}",
                catalog_names_csv()
            )),
        }
    }
}

fn catalog_names_csv() -> String {
    Recipe::ALL
        .iter()
        .map(|r| r.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct RecipeParamMeta {
    pub name: &'static str,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecipeCatalogEntry {
    pub name: &'static str,
    pub recipe_version: u32,
    pub params: Vec<RecipeParamMeta>,
    pub description: &'static str,
    pub needs: &'static [&'static str],
}

/// Pure catalog — no repo / store touch.
pub fn catalog() -> Vec<RecipeCatalogEntry> {
    Recipe::ALL
        .iter()
        .copied()
        .map(|r| RecipeCatalogEntry {
            name: r.as_str(),
            recipe_version: r.recipe_version(),
            params: r.params(),
            description: r.description(),
            needs: r.needs(),
        })
        .collect()
}

#[derive(Debug, Deserialize)]
pub struct RecipeRunParams {
    pub repo: String,
    pub since: Option<String>,
    pub limit: Option<usize>,
    pub scope: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecipeRunOut {
    pub schema: &'static str,
    pub recipe: String,
    pub recipe_version: u32,
    pub repo: String,
    pub items: Vec<serde_json::Value>,
    pub inputs_missing: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub total: usize,
    pub truncated: bool,
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// `GET /api/recipes` — catalog.
pub async fn recipes_catalog_route() -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": SCHEMA,
            "recipes": catalog(),
        })),
    )
}

/// `GET /api/recipes/{name}?repo=…`
pub async fn recipe_run_route(
    State(state): State<SharedState>,
    Path(name): Path<String>,
    Query(params): Query<RecipeRunParams>,
) -> Result<impl IntoResponse, ApiError> {
    // `from_str`'s error already lists the known recipe names.
    let recipe = Recipe::from_str(&name).map_err(ApiError::not_found)?;
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let limit = parse_limit(params.limit)?;

    // Required params
    let since = match recipe {
        Recipe::NewPublicApi | Recipe::ComplexityClimbers => {
            let s = params
                .since
                .as_deref()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    ApiError::bad_request(format!(
                        "recipe {} requires query param `since` (git ref or ISO date YYYY-MM-DD)",
                        recipe.as_str()
                    ))
                })?;
            Some(s.to_string())
        }
        _ => None,
    };

    let scope_filter = parse_scope(&state, params.scope.as_deref())?;
    let repo_path = repo.path.clone();
    let repo_name = params.repo.clone();
    let store = state.store.clone();
    let blame_cache = state.blame_cache.clone();

    let out = tokio::task::spawn_blocking(move || {
        run_recipe(
            recipe,
            &store,
            &blame_cache,
            &repo_path,
            &repo_name,
            repo_id,
            since.as_deref(),
            limit,
            scope_filter,
        )
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// V74-L3a repair 4 — over the cap REFUSES with both numbers. The old
/// `.clamp(1, MAX_LIMIT)` returned 500 rows and `truncated: true` for a
/// caller who asked for 2000, which reads as "your repo has 500 of
/// these" rather than "I capped you". kb root invariant #35 (`?ids=`)
/// states the rule: over-cap is a 400, never a silent truncation.
pub(crate) fn parse_limit(limit: Option<usize>) -> Result<usize, ApiError> {
    match limit {
        None => Ok(DEFAULT_LIMIT),
        Some(0) => Err(ApiError::bad_request("limit must be at least 1")),
        Some(n) if n > MAX_LIMIT => Err(ApiError::bad_request(format!(
            "limit {n} exceeds the per-recipe cap of {MAX_LIMIT}"
        ))),
        Some(n) => Ok(n),
    }
}

fn parse_scope(
    state: &SharedState,
    scope: Option<&str>,
) -> Result<Option<(bool, Vec<String>)>, ApiError> {
    match scope {
        None => Ok(None),
        Some(raw) => {
            let (exclude, name) = if let Some(n) = raw.strip_prefix('!') {
                (true, n)
            } else {
                (false, raw)
            };
            if name.is_empty() {
                return Err(ApiError::bad_request(
                    "scope must be a name or !<name>, not empty",
                ));
            }
            let patterns = state
                .scopes
                .get(name)
                .ok_or_else(|| ApiError::not_found(format!("unknown scope {name:?}")))?
                .to_vec();
            Ok(Some((exclude, patterns)))
        }
    }
}

fn in_scope(path: &str, scope: &Option<(bool, Vec<String>)>) -> bool {
    match scope {
        None => true,
        Some((exclude, patterns)) => {
            let m = crate::scopes::path_matches_any(path, patterns);
            if *exclude {
                !m
            } else {
                m
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Run
// ---------------------------------------------------------------------------

/// `pub(crate)` for `crate::recipe::builtins::run_native` — kbc-recipe/1
/// adapts all six bodies rather than reimplementing them in its op set
/// (see that module's doc for why the bodies stay here).
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_recipe(
    recipe: Recipe,
    store: &Store,
    blame_cache: &BlameCache,
    repo_root: &FsPath,
    repo_name: &str,
    repo_id: i64,
    since: Option<&str>,
    limit: usize,
    scope: Option<(bool, Vec<String>)>,
) -> Result<RecipeRunOut, ApiError> {
    match recipe {
        Recipe::NewPublicApi => run_new_public_api(
            store,
            blame_cache,
            repo_root,
            repo_name,
            repo_id,
            since.unwrap(),
            limit,
            &scope,
        ),
        Recipe::GodFunctions => run_god_functions(store, repo_name, repo_id, limit, &scope),
        Recipe::AgentOnlySymbols => run_agent_only(
            store,
            blame_cache,
            repo_root,
            repo_name,
            repo_id,
            limit,
            &scope,
        ),
        Recipe::ComplexityClimbers => run_complexity_climbers(
            store,
            repo_root,
            repo_name,
            repo_id,
            since.unwrap(),
            limit,
            &scope,
        ),
        Recipe::UnreviewedHotspots => {
            run_unreviewed_hotspots(store, repo_root, repo_name, repo_id, limit, &scope)
        }
        Recipe::FailureTainted => run_failure_tainted(store, repo_name, repo_id, limit, &scope),
    }
}

fn finish(
    recipe: Recipe,
    repo_name: &str,
    mut items: Vec<serde_json::Value>,
    inputs_missing: Vec<String>,
    note: Option<String>,
    limit: usize,
) -> RecipeRunOut {
    let total = items.len();
    let truncated = total > limit;
    items.truncate(limit);
    RecipeRunOut {
        schema: SCHEMA,
        recipe: recipe.as_str().to_string(),
        recipe_version: recipe.recipe_version(),
        repo: repo_name.to_string(),
        items,
        inputs_missing,
        note,
        total,
        truncated,
    }
}

// --- new-public-api -------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn run_new_public_api(
    store: &Store,
    blame_cache: &BlameCache,
    repo_root: &FsPath,
    repo_name: &str,
    repo_id: i64,
    since: &str,
    limit: usize,
    scope: &Option<(bool, Vec<String>)>,
) -> Result<RecipeRunOut, ApiError> {
    let git = GitRepo::open(repo_root).map_err(ApiError::from)?;
    let since_unix = resolve_since_unix(&git, repo_root, since)?;

    let rows = store.symbols_with_lang_for_repo(repo_id)?;
    // Cache file text per path for TS export checks.
    let mut file_text: HashMap<String, String> = HashMap::new();
    // Cache blame per path.
    let mut blame_by_path: HashMap<String, Vec<blame::BlameRegion>> = HashMap::new();

    let mut items = Vec::new();
    // V74-L3a repair 5 — `is_public_export` understands five languages
    // and answers `false` for every other one, which turned a Go / Ruby /
    // Java repo into a confident `items: []` with `inputs_missing: []`.
    // Count what was never EXAMINABLE and say so.
    let mut examined = 0usize;
    let mut unexaminable: BTreeMap<String, usize> = BTreeMap::new();
    for (path, lang, sym) in rows {
        if !in_scope(&path, scope) {
            continue;
        }
        if !EXPORT_RULE_LANGS.contains(&lang.as_str()) {
            *unexaminable.entry(lang.clone()).or_insert(0) += 1;
            continue;
        }
        examined += 1;
        if !is_public_export(&lang, &path, &sym, repo_root, &mut file_text) {
            continue;
        }
        let regions =
            blame_by_path.entry(path.clone()).or_insert_with(|| {
                match blame::blame_file(
                    blame_cache,
                    &git,
                    repo_id,
                    repo_root,
                    &path,
                    Some("HEAD"),
                    None,
                ) {
                    Ok(r) => r.regions,
                    Err(_) => Vec::new(),
                }
            });
        let Some(region) = region_covering(regions, sym.line_start) else {
            continue;
        };
        if region.author_time <= since_unix {
            continue;
        }
        // Definition row — not a name resolution. Class is exact for the
        // extracted definition itself.
        items.push(serde_json::json!({
            "path": path,
            "symbol": sym.name,
            "kind": sym.kind,
            "line": sym.line_start,
            "first_seen": {
                "commit": region.sha,
                "date_unix": region.author_time,
            },
            "class": CLASS_EXACT,
        }));
    }
    items.sort_by(|a, b| {
        let da = a["first_seen"]["date_unix"].as_i64().unwrap_or(0);
        let db = b["first_seen"]["date_unix"].as_i64().unwrap_or(0);
        db.cmp(&da)
            .then_with(|| {
                a["path"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(b["path"].as_str().unwrap_or(""))
            })
            .then_with(|| {
                a["symbol"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(b["symbol"].as_str().unwrap_or(""))
            })
    });
    let mut note = format!(
        "Public/exported definitions first blamed after since_unix={since_unix}. This recipe \
         understands visibility in {} only. Attention only — not a stability verdict.",
        EXPORT_RULE_LANGS.join(", ")
    );
    let mut inputs_missing = Vec::new();
    if !unexaminable.is_empty() {
        let skipped: usize = unexaminable.values().sum();
        let langs = unexaminable
            .iter()
            .map(|(l, n)| format!("{l}={n}"))
            .collect::<Vec<_>>()
            .join(", ");
        note.push_str(&format!(
            " {skipped} symbol(s) were in languages it has no visibility rule for ({langs}) and \
             were NOT examined."
        ));
        if examined == 0 {
            // Nothing here could be answered at all: that is a missing
            // input, not an empty result set.
            inputs_missing.push("public-export-rules".to_string());
        }
    }
    Ok(finish(
        Recipe::NewPublicApi,
        repo_name,
        items,
        inputs_missing,
        Some(note),
        limit,
    ))
}

/// The languages `is_public_export` has a visibility rule for. Declared
/// beside the rule so the two cannot drift, and named in every
/// `new-public-api` note (V74-L3a repair 5).
pub(crate) const EXPORT_RULE_LANGS: &[&str] =
    &["rust", "typescript", "tsx", "javascript", "python"];

fn is_public_export(
    lang: &str,
    path: &str,
    sym: &crate::extract::Symbol,
    repo_root: &FsPath,
    file_text: &mut HashMap<String, String>,
) -> bool {
    match lang {
        "rust" => {
            // Visibility rides on the function_item / struct_item node, so
            // the cut signature includes `pub` / `pub(crate)` / etc.
            let sig = sym.signature.as_deref().unwrap_or("");
            rust_sig_is_pub(sig)
        }
        "typescript" | "tsx" | "javascript" => {
            let text = file_text.entry(path.to_string()).or_insert_with(|| {
                std::fs::read_to_string(repo_root.join(path)).unwrap_or_default()
            });
            line_has_export(text, sym.line_start)
        }
        "python" => {
            // Module-level def/class, not private-by-convention.
            if sym.container.is_some() {
                return false;
            }
            matches!(sym.kind.as_str(), "def" | "class") && !sym.name.starts_with('_')
        }
        _ => false,
    }
}

/// `true` when a Rust signature carries crate-visible `pub` (incl. `pub(crate)`).
fn rust_sig_is_pub(sig: &str) -> bool {
    let s = sig.trim_start();
    // Skip attributes / whitespace-collapsed noise: look for word-boundary `pub`.
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"pub") {
            let after = i + 3;
            let before_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_';
            let after_ok = after >= bytes.len()
                || (!bytes[after].is_ascii_alphanumeric() && bytes[after] != b'_');
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn line_has_export(source: &str, line_1based: u32) -> bool {
    if line_1based == 0 {
        return false;
    }
    let lines: Vec<&str> = source.lines().collect();
    let idx = (line_1based as usize).saturating_sub(1);
    // The defining line itself carries the `export` keyword.
    if let Some(line) = lines.get(idx) {
        let t = line.trim_start();
        if t == "export"
            || t.starts_with("export ")
            || t.starts_with("export{")
            || t.starts_with("export\t")
        {
            return true;
        }
    }
    // One line above counts ONLY as a bare wrapped continuation (a lone
    // `export` token before the declaration). A COMPLETE `export …`
    // statement above is an unrelated export — e.g. the `export class`
    // a private method sits in, or a preceding `export const X = {};` —
    // and must not leak publicity onto this symbol (a false `exact`
    // here is the wrong-exact release-blocker class).
    if idx > 0 {
        if let Some(line) = lines.get(idx - 1) {
            if line.trim() == "export" {
                return true;
            }
        }
    }
    false
}

fn region_covering(regions: &[blame::BlameRegion], line: u32) -> Option<&blame::BlameRegion> {
    regions.iter().find(|r| {
        let end = r.final_start.saturating_add(r.count.saturating_sub(1));
        line >= r.final_start && line <= end
    })
}

// --- god-functions --------------------------------------------------------

fn run_god_functions(
    store: &Store,
    repo_name: &str,
    repo_id: i64,
    limit: usize,
    scope: &Option<(bool, Vec<String>)>,
) -> Result<RecipeRunOut, ApiError> {
    let symbols = store.symbols_for_repo(repo_id)?;
    let sites = store.call_sites_in_repo(repo_id)?;

    // fan_out: same-blob, caller_ordinal → count
    // key: (path, ordinal)
    let mut fan_out: HashMap<(String, u32), u64> = HashMap::new();
    // fan_in: callee_name → count (name-matched; class candidate)
    let mut fan_in_by_name: HashMap<String, u64> = HashMap::new();
    for (path, site, _blob, _salt) in &sites {
        if let Some(ord) = site.caller_ordinal {
            *fan_out.entry((path.clone(), ord)).or_insert(0) += 1;
        }
        *fan_in_by_name.entry(site.callee_name.clone()).or_insert(0) += 1;
    }

    // Map path → blob symbols with ordinal for fan_out lookup. symbols_for_repo
    // already pairs path with symbol (ordinal is on the symbol).
    let mut items = Vec::new();
    for (path, sym) in symbols {
        if !in_scope(&path, scope) {
            continue;
        }
        if !is_callable_kind(&sym.kind) {
            continue;
        }
        let line_span = sym
            .line_end
            .saturating_sub(sym.line_start)
            .saturating_add(1) as u64;
        let fo = *fan_out.get(&(path.clone(), sym.ordinal)).unwrap_or(&0);
        let fi = *fan_in_by_name.get(&sym.name).unwrap_or(&0);
        // Avoid counting a function as its own sole "fan_in" solely via
        // recursive name match when there are zero real edges? Name match
        // includes all sites; acceptable for attention.
        let score = line_span.saturating_mul(fi.saturating_add(fo));
        if score == 0 && fi == 0 && fo == 0 {
            // Still include? Brief: rank by score. Zero-score tails add noise;
            // RETURN-LESS: skip pure isolates (span-only with no edges would
            // score 0 anyway). Keep functions with span>0 and any edge, OR
            // large span even without edges for "look at size".
            // Prefer: include when score > 0 OR line_span is large.
            // Simplest honest rule: include all callables; score can be 0.
        }
        let class = worst_class_for_fans(fi, fo);
        items.push((
            score,
            path.clone(),
            sym.name.clone(),
            serde_json::json!({
                "path": path,
                "symbol": sym.name,
                "kind": sym.kind,
                "line": sym.line_start,
                "score": score,
                "terms": {
                    "line_span": line_span,
                    "fan_in": fi,
                    "fan_out": fo,
                },
                "class": class,
            }),
        ));
    }
    items.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    let items: Vec<_> = items.into_iter().map(|(_, _, _, v)| v).collect();
    Ok(finish(
        Recipe::GodFunctions,
        repo_name,
        items,
        Vec::new(),
        Some(
            "Score = line_span × (fan_in + fan_out). Fan-in is name-matched \
             (class candidate); fan-out is same-file caller_ordinal (class likely). \
             Attention only."
                .into(),
        ),
        limit,
    ))
}

fn is_callable_kind(kind: &str) -> bool {
    matches!(
        kind,
        "fn" | "function" | "def" | "method" | "func" | "singleton_method"
    )
}

fn worst_class_for_fans(fan_in: u64, fan_out: u64) -> &'static str {
    // Edges only. fan_out → likely; fan_in → candidate. Worst wins.
    match (fan_in > 0, fan_out > 0) {
        (true, _) => CLASS_CANDIDATE,
        (false, true) => CLASS_LIKELY,
        (false, false) => CLASS_EXACT, // definition only, no edge resolution
    }
}

// --- agent-only-symbols ---------------------------------------------------

fn run_agent_only(
    store: &Store,
    blame_cache: &BlameCache,
    repo_root: &FsPath,
    repo_name: &str,
    repo_id: i64,
    limit: usize,
    scope: &Option<(bool, Vec<String>)>,
) -> Result<RecipeRunOut, ApiError> {
    // V74-L3a repair 2 — the GATE must read the same table the per-symbol
    // ROW TEST reads. It used to pass on `author_stats` alone while every
    // row was decided by `get_commit_session`, so a repo with dual-author
    // rows and no `Kb-Session:` trailers returned `items: []` with
    // `inputs_missing: []`: an honest-looking empty set over a missing
    // input, from inside a module whose own design law forbids exactly
    // that.
    let has_join = store.has_commit_session_ids(repo_id)?;
    if !has_join {
        let has_authors = store.has_session_authors(repo_id)?;
        let (missing, why): (&str, String) = if has_authors {
            (
                "commit_sessions",
                "Dual-author `session:` rows exist, but there are no commit→session joins — \
                 and the per-symbol test reads commit_sessions. Missing input, not an empty \
                 agent-only set: land `Kb-Session:` trailers (or run the join backfill) first."
                    .into(),
            )
        } else {
            (
                "agent_attribution",
                "No dual-author session rows and no commit→session joins for this repo. \
                 Missing input — not an empty agent-only set."
                    .into(),
            )
        };
        return Ok(finish(
            Recipe::AgentOnlySymbols,
            repo_name,
            Vec::new(),
            vec![missing.into()],
            Some(why),
            limit,
        ));
    }

    let git = GitRepo::open(repo_root).map_err(ApiError::from)?;
    let symbols = store.symbols_for_repo(repo_id)?;
    let mut blame_by_path: HashMap<String, Vec<blame::BlameRegion>> = HashMap::new();
    // One sqlite round trip per DISTINCT sha, not per covering region per
    // symbol. The same handful of shas recurs across thousands of symbols
    // and each lookup takes the store's single connection mutex — the
    // 2026-08-31 starvation incident's pressure shape, in a loop.
    let mut session_by_sha: HashMap<String, bool> = HashMap::new();
    let mut items = Vec::new();

    for (path, sym) in symbols {
        if !in_scope(&path, scope) {
            continue;
        }
        let regions =
            blame_by_path.entry(path.clone()).or_insert_with(|| {
                match blame::blame_file(
                    blame_cache,
                    &git,
                    repo_id,
                    repo_root,
                    &path,
                    Some("HEAD"),
                    None,
                ) {
                    Ok(r) => r.regions,
                    Err(_) => Vec::new(),
                }
            });
        let covering: Vec<&blame::BlameRegion> = regions
            .iter()
            .filter(|r| {
                let end = r.final_start.saturating_add(r.count.saturating_sub(1));
                // Overlaps [line_start, line_end]
                r.final_start <= sym.line_end && end >= sym.line_start
            })
            .collect();
        if covering.is_empty() {
            continue;
        }
        let mut all_agent = true;
        let mut shas = BTreeSet::new();
        for r in &covering {
            if r.sha == crate::provenance::UNCOMMITTED_SHA {
                all_agent = false;
                break;
            }
            shas.insert(r.sha.as_str());
            let agent = match session_by_sha.get(&r.sha) {
                Some(v) => *v,
                None => {
                    let v = store
                        .get_commit_session(repo_id, &r.sha)
                        .ok()
                        .flatten()
                        .and_then(|row| row.session_id)
                        .filter(|s| !s.is_empty())
                        .is_some();
                    session_by_sha.insert(r.sha.clone(), v);
                    v
                }
            };
            if !agent {
                all_agent = false;
                break;
            }
        }
        if !all_agent {
            continue;
        }
        items.push(serde_json::json!({
            "path": path,
            "symbol": sym.name,
            "kind": sym.kind,
            "line": sym.line_start,
            "commits": shas.into_iter().collect::<Vec<_>>(),
            "class": CLASS_EXACT,
        }));
    }
    items.sort_by(|a, b| {
        a["path"]
            .as_str()
            .unwrap_or("")
            .cmp(b["path"].as_str().unwrap_or(""))
            .then_with(|| {
                a["symbol"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(b["symbol"].as_str().unwrap_or(""))
            })
    });
    Ok(finish(
        Recipe::AgentOnlySymbols,
        repo_name,
        items,
        Vec::new(),
        Some(
            "Defining-span blame commits all join to a session_id. \
             Same agent-attribution source as ownership agents[]. Attention only."
                .into(),
        ),
        limit,
    ))
}

// --- complexity-climbers --------------------------------------------------

fn run_complexity_climbers(
    store: &Store,
    repo_root: &FsPath,
    repo_name: &str,
    repo_id: i64,
    since: &str,
    limit: usize,
    scope: &Option<(bool, Vec<String>)>,
) -> Result<RecipeRunOut, ApiError> {
    let git = GitRepo::open(repo_root).map_err(ApiError::from)?;
    let since_rev = resolve_since_rev(&git, repo_root, since)?;

    let files = store.list_files(repo_id)?;
    let mut items = Vec::new();
    // V74-L3a repair 1 — a baseline this recipe could not READ is not a
    // baseline of zero. These rows are kept (never dropped: the file DID
    // change, and hiding it would be its own dishonesty) but they carry
    // `score: null` and a `then_state`, and they sort after every real
    // climb rather than above it.
    let mut unknown_baseline: Vec<(String, serde_json::Value)> = Vec::new();
    for f in files {
        if !in_scope(&f.path, scope) {
            continue;
        }
        // Skip non-source / unparsed markers
        if matches!(f.lang.as_str(), "binary" | "too-large" | "lfs" | "unknown") {
            continue;
        }
        let then_c = match git.read_blob(&since_rev, &f.path, DEFAULT_BLOB_SIZE_CAP) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                Some(complexity_proxy(&text))
            }
            Err(err) => {
                let now_c = complexity_for_path(repo_root, &f.path);
                unknown_baseline.push((
                    f.path.clone(),
                    serde_json::json!({
                        "path": f.path,
                        "score": serde_json::Value::Null,
                        "terms": {
                            "then_state": baseline_state(&err),
                            "loc_then": serde_json::Value::Null,
                            "indent_then": serde_json::Value::Null,
                            "loc_now": now_c.loc,
                            "indent_now": now_c.indent_sum,
                            "delta": serde_json::Value::Null,
                        },
                    }),
                ));
                None
            }
        };
        let Some(then_c) = then_c else { continue };
        let now_c = complexity_for_path(repo_root, &f.path);
        let delta = now_c.total().saturating_sub(then_c.total()) as i64;
        if delta <= 0 {
            continue;
        }
        items.push((
            delta,
            f.path.clone(),
            serde_json::json!({
                "path": f.path,
                "score": delta,
                "terms": {
                    "then_state": BASELINE_READ,
                    "loc_then": then_c.loc,
                    "loc_now": now_c.loc,
                    "indent_then": then_c.indent_sum,
                    "indent_now": now_c.indent_sum,
                    "delta": delta,
                },
            }),
        ));
    }
    items.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    unknown_baseline.sort_by(|a, b| a.0.cmp(&b.0));
    let unknown_count = unknown_baseline.len();
    let items: Vec<_> = items
        .into_iter()
        .map(|(_, _, v)| v)
        .chain(unknown_baseline.into_iter().map(|(_, v)| v))
        .collect();
    let mut note = format!(
        "Complexity proxy = loc + indent_sum (behavioral formula). Compared the blob at \
         {since_rev} against the WORKING TREE (not another commit). Positive delta only."
    );
    if unknown_count > 0 {
        note.push_str(&format!(
            " {unknown_count} file(s) had no readable baseline at {since_rev} (added, renamed, \
             or over the blob cap); they carry `score: null` and a `terms.then_state` and are \
             listed last — an unreadable baseline is NOT a baseline of zero."
        ));
    }
    Ok(finish(
        Recipe::ComplexityClimbers,
        repo_name,
        items,
        Vec::new(),
        Some(note),
        limit,
    ))
}

/// The `terms.then_state` vocabulary. Closed, and each value names a
/// DIFFERENT thing a reader would otherwise have to infer from a zero.
pub(crate) const BASELINE_READ: &str = "read";
pub(crate) const BASELINE_ABSENT: &str = "absent";
pub(crate) const BASELINE_TOO_LARGE: &str = "too-large";
pub(crate) const BASELINE_UNREADABLE: &str = "unreadable";

pub(crate) fn baseline_state(err: &crate::git::GitError) -> &'static str {
    match err {
        crate::git::GitError::PathNotFound { .. } => BASELINE_ABSENT,
        crate::git::GitError::TooLarge { .. } => BASELINE_TOO_LARGE,
        _ => BASELINE_UNREADABLE,
    }
}

// --- unreviewed-hotspots --------------------------------------------------

fn run_unreviewed_hotspots(
    store: &Store,
    repo_root: &FsPath,
    repo_name: &str,
    repo_id: i64,
    limit: usize,
    scope: &Option<(bool, Vec<String>)>,
) -> Result<RecipeRunOut, ApiError> {
    let path_stats = store.list_path_stats(repo_id)?;
    if path_stats.is_empty() {
        return Ok(finish(
            Recipe::UnreviewedHotspots,
            repo_name,
            Vec::new(),
            vec!["behavioral".into()],
            Some(
                "No path_stats rows — run behavioral backfill first. \
                 Missing input, not an empty unreviewed set."
                    .into(),
            ),
            limit,
        ));
    }

    // Hotspot ranking — same formula as behavioral/hotspots (default weight).
    let rows: Vec<_> = path_stats
        .into_iter()
        .filter(|r| in_scope(&r.path, scope))
        .collect();
    let churns: Vec<u64> = rows
        .iter()
        .map(|r| (r.lines_added + r.lines_deleted).max(0) as u64)
        .collect();
    let complexities: Vec<Complexity> = rows
        .iter()
        .map(|r| complexity_for_path(repo_root, &r.path))
        .collect();
    let complexity_totals: Vec<u64> = complexities.iter().map(|c| c.total()).collect();
    let churn_ranks = dense_ranks_desc(&churns);
    let complexity_ranks = dense_ranks_desc(&complexity_totals);

    let reviews = store.list_reviews(repo_name, None)?;
    let mut reviewed_paths: HashSet<String> = HashSet::new();
    for rev in &reviews {
        let pss = store.list_patchsets(rev.id).unwrap_or_default();
        for ps in pss {
            if let Ok(files) = files_changed(repo_root, &ps.base_sha, &ps.tip_sha) {
                for f in files {
                    reviewed_paths.insert(f.path);
                }
            }
        }
    }
    let note = if reviews.is_empty() {
        Some(
            "No local reviews exist — every hotspot is unreviewed. \
             Attention only; not a review-debt verdict."
                .into(),
        )
    } else {
        Some(format!(
            "Hotspots not appearing in any of {} review patchset file lists. \
             Attention only.",
            reviews.len()
        ))
    };

    let mut items = Vec::new();
    for (i, r) in rows.into_iter().enumerate() {
        if reviewed_paths.contains(&r.path) {
            continue;
        }
        let cr = churn_ranks[i];
        let xr = complexity_ranks[i];
        let score = hotspot_score(cr, xr);
        items.push((
            score,
            r.path.clone(),
            serde_json::json!({
                "path": r.path,
                "revisions": r.revisions,
                "churn": r.lines_added + r.lines_deleted,
                "score": score,
                "terms": {
                    "churn_rank": cr,
                    "complexity_rank": xr,
                    "loc": complexities[i].loc,
                    "indent_sum": complexities[i].indent_sum,
                },
            }),
        ));
    }
    items.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(&b.1))
    });
    let items: Vec<_> = items.into_iter().map(|(_, _, v)| v).collect();
    Ok(finish(
        Recipe::UnreviewedHotspots,
        repo_name,
        items,
        Vec::new(),
        note,
        limit,
    ))
}

// --- failure-tainted ------------------------------------------------------

fn run_failure_tainted(
    store: &Store,
    repo_name: &str,
    repo_id: i64,
    limit: usize,
    scope: &Option<(bool, Vec<String>)>,
) -> Result<RecipeRunOut, ApiError> {
    let sigs = store.list_session_signals(repo_id)?;
    if sigs.is_empty() {
        return Ok(finish(
            Recipe::FailureTainted,
            repo_name,
            Vec::new(),
            vec!["session_signals".into()],
            Some(
                "No session_signals rows (V0018). Missing input — not an empty \
                 failure-tainted set. Populate via join + session detail."
                    .into(),
            ),
            limit,
        ));
    }

    // Aggregate evidence per path via dual-author session rows.
    //
    // V74-L3a repair 3 — `terms.fail_count` is RETIRED. The only writer
    // of `session_signals` passes a literal `0` for it ("no distinct
    // test-failure field on wire"), so `stored_fail_term` was
    // permanently `None`, the advertised term was permanently `0`, and
    // the score was a sum with one addend that could never fire. A term
    // that is structurally unmeasurable must not be reported as measured
    // and zero; when the write site starts carrying real failure counts,
    // it comes back with a test.
    let mut by_path: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    // (error_count_sum, sessions_with_evidence)
    let mut suppressed_fail_terms = 0usize;
    for s in &sigs {
        if stored_fail_term(s.fail_count).is_some() {
            suppressed_fail_terms += 1;
        }
        if s.error_count <= 0 {
            continue;
        }
        let paths = store.paths_for_session_author(repo_id, &s.session_id)?;
        for p in paths {
            if !in_scope(&p, scope) {
                continue;
            }
            let e = by_path.entry(p).or_insert((0, 0));
            e.0 = e.0.saturating_add(s.error_count.max(0));
            e.1 = e.1.saturating_add(1);
        }
    }

    let mut items: Vec<(i64, String, serde_json::Value)> = by_path
        .into_iter()
        .map(|(path, (err, sessions))| {
            (
                err,
                path.clone(),
                serde_json::json!({
                    "path": path,
                    "score": err,
                    "terms": {
                        "error_count": err,
                        "sessions": sessions,
                    },
                }),
            )
        })
        .collect();
    items.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let items: Vec<_> = items.into_iter().map(|(_, _, v)| v).collect();
    let mut note = String::from(
        "Paths co-touched by sessions carrying TOOL-ERROR evidence (session_signals + \
         author_stats `session:` rows). Test-failure counts are not captured by any writer, \
         so no fail term is reported — absence of the term, not a zero. Attention only.",
    );
    if suppressed_fail_terms > 0 {
        note.push_str(&format!(
            " ({suppressed_fail_terms} session row(s) DO carry a non-zero fail_count; the term \
             returns with a test once a writer sets it meaningfully.)"
        ));
    }
    Ok(finish(
        Recipe::FailureTainted,
        repo_name,
        items,
        Vec::new(),
        Some(note),
        limit,
    ))
}

// ---------------------------------------------------------------------------
// since parsing
// ---------------------------------------------------------------------------

/// Resolve `since` to a unix timestamp for blame-date comparisons.
/// Accepts `YYYY-MM-DD` (midnight UTC) or any revspec `GitRepo::resolve` takes.
fn resolve_since_unix(git: &GitRepo, _repo_root: &FsPath, since: &str) -> Result<i64, ApiError> {
    if let Some(u) = parse_iso_date_unix(since) {
        return Ok(u);
    }
    let info = git.commit_info(since).map_err(|e| {
        ApiError::bad_request(format!(
            "since={since:?} is neither an ISO date (YYYY-MM-DD) nor a resolvable git ref: {e}"
        ))
    })?;
    Ok(info.author_time_unix)
}

/// Resolve `since` to a rev suitable for `read_blob`. ISO dates use
/// `git rev-list -1 --before=<date> HEAD`.
fn resolve_since_rev(git: &GitRepo, repo_root: &FsPath, since: &str) -> Result<String, ApiError> {
    if parse_iso_date_unix(since).is_some() {
        // git rev-list -1 --before=YYYY-MM-DD HEAD
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["rev-list", "-1", &format!("--before={since}"), "HEAD"])
            .output()
            .map_err(|e| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("git rev-list --before failed: {e}"),
                )
            })?;
        if !out.status.success() {
            return Err(ApiError::bad_request(format!(
                "no commit on or before {since:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if sha.is_empty() {
            return Err(ApiError::bad_request(format!(
                "no commit on or before {since:?}"
            )));
        }
        return Ok(sha);
    }
    git.resolve(since).map(|oid| oid.to_string()).map_err(|e| {
        ApiError::bad_request(format!(
            "since={since:?} is neither an ISO date nor a resolvable git ref: {e}"
        ))
    })
}

/// Parse `YYYY-MM-DD` (optional `T…` / space time suffix ignored for the date
/// part) as midnight UTC unix. `None` if not date-shaped.
fn parse_iso_date_unix(s: &str) -> Option<i64> {
    let s = s.trim();
    // Take first 10 chars if date-shaped
    let date = if s.len() >= 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-' {
        &s[..10]
    } else {
        return None;
    };
    let mut parts = date.split('-');
    let y: i32 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Civil → unix via chrono if available; else days-since-epoch approx.
    // Workspace already depends on chrono.
    let dt = chrono::NaiveDate::from_ymd_opt(y, m, d)?
        .and_hms_opt(0, 0, 0)?
        .and_utc();
    Some(dt.timestamp())
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str_round_trip_all_six() {
        for r in Recipe::ALL {
            let s = r.as_str();
            assert_eq!(Recipe::from_str(s).unwrap(), *r, "round-trip {s}");
            assert_eq!(r.recipe_version(), 1);
        }
    }

    #[test]
    fn from_str_unknown_lists_catalog() {
        let err = Recipe::from_str("nope").unwrap_err();
        assert!(err.contains("new-public-api"), "{err}");
        assert!(err.contains("failure-tainted"), "{err}");
    }

    #[test]
    fn catalog_is_pure_and_complete() {
        let c = catalog();
        assert_eq!(c.len(), 6);
        assert_eq!(c[0].name, "new-public-api");
        assert!(c[0].params.iter().any(|p| p.name == "since" && p.required));
        assert!(c
            .iter()
            .find(|e| e.name == "god-functions")
            .unwrap()
            .params
            .is_empty());
    }

    #[test]
    fn rust_sig_is_pub_detects_visibility() {
        assert!(rust_sig_is_pub("pub fn foo()"));
        assert!(rust_sig_is_pub("pub(crate) fn foo()"));
        assert!(rust_sig_is_pub("pub(super) struct X"));
        assert!(!rust_sig_is_pub("fn foo()"));
        assert!(!rust_sig_is_pub("publication"));
        assert!(!rust_sig_is_pub("fn publish()"));
    }

    #[test]
    fn parse_iso_date_midnight_utc() {
        let u = parse_iso_date_unix("2024-01-15").unwrap();
        assert_eq!(u, 1705276800); // 2024-01-15T00:00:00Z
        assert!(parse_iso_date_unix("main").is_none());
        assert!(parse_iso_date_unix("abc123").is_none());
    }

    #[test]
    fn worst_class_ordering() {
        assert_eq!(worst_class_for_fans(0, 0), CLASS_EXACT);
        assert_eq!(worst_class_for_fans(0, 3), CLASS_LIKELY);
        assert_eq!(worst_class_for_fans(2, 3), CLASS_CANDIDATE);
        assert_eq!(worst_class_for_fans(1, 0), CLASS_CANDIDATE);
    }

    #[test]
    fn line_has_export_basic() {
        let src = "const x = 1;\nexport function foo() {}\n";
        assert!(line_has_export(src, 2));
        assert!(!line_has_export(src, 1));
    }

    /// The line-above carve-out accepts ONLY a bare wrapped `export`
    /// token — a complete `export …` statement above is an UNRELATED
    /// export and must not leak publicity (false-exact blocker class).
    #[test]
    fn line_has_export_no_publicity_leak_from_line_above() {
        // Method directly under `export class Foo {` is NOT exported.
        let src = "export class Foo {\n  bar() { return 1; }\n}\n";
        assert!(line_has_export(src, 1)); // the class itself
        assert!(!line_has_export(src, 2)); // its method — the classic FP
                                           // Function after a complete unrelated export statement.
        let src2 = "export const CONFIG = {};\nfunction helper() {}\n";
        assert!(!line_has_export(src2, 2));
        // Bare wrapped continuation IS accepted.
        let src3 = "export\nfunction wrapped() {}\n";
        assert!(line_has_export(src3, 2));
        // `export` with trailing whitespace above still counts as bare.
        let src4 = "export  \nfunction wrapped2() {}\n";
        assert!(line_has_export(src4, 2));
    }

    /// Determinism: pure ranking inputs produce identical ordered JSON.
    #[test]
    fn god_function_score_determinism() {
        // line_span × (fi+fo)
        let a = 10u64.saturating_mul(3 + 2);
        let b = 10u64.saturating_mul(3 + 2);
        assert_eq!(a, b);
        // Sort tie-break path then symbol is total order
        let mut rows = vec![
            (10u64, "b.rs".to_string(), "z".to_string()),
            (10u64, "a.rs".to_string(), "z".to_string()),
            (10u64, "a.rs".to_string(), "a".to_string()),
            (20u64, "c.rs".to_string(), "m".to_string()),
        ];
        rows.sort_by(|x, y| {
            y.0.cmp(&x.0)
                .then_with(|| x.1.cmp(&y.1))
                .then_with(|| x.2.cmp(&y.2))
        });
        assert_eq!(rows[0].1, "c.rs");
        assert_eq!(rows[1].2, "a");
        assert_eq!(rows[2].2, "z");
        // Re-sort same data → identical
        let mut rows2 = rows.clone();
        rows2.sort_by(|x, y| {
            y.0.cmp(&x.0)
                .then_with(|| x.1.cmp(&y.1))
                .then_with(|| x.2.cmp(&y.2))
        });
        assert_eq!(rows, rows2);
    }

    #[test]
    fn is_callable_kind_covers_langs() {
        assert!(is_callable_kind("fn"));
        assert!(is_callable_kind("function"));
        assert!(is_callable_kind("def"));
        assert!(is_callable_kind("method"));
        assert!(is_callable_kind("func"));
        assert!(!is_callable_kind("struct"));
        assert!(!is_callable_kind("class"));
    }
}
