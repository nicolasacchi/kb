//! `kbc-recipe/1`'s HTTP surface (V74-L3a, D11 + D21).
//!
//! **Why a `/api/recipe/*` prefix beside the plural `/api/recipes*`.**
//! `recipes/1` already owns `/api/recipes/{name}` — the depth-2 PARAM
//! slot — so a new depth-2 literal (`/api/recipes/catalog`) would shadow
//! any recipe that happened to be called `catalog`, and a depth-3 family
//! under the same param would read as a sub-resource of one recipe. The
//! `canvas` → `boards` split (invariant 22, `router.rs`' own comment)
//! took the identical decision for the identical reason: a frozen family
//! keeps its paths, and the new one gets its own prefix.
//!
//! **What is a read and what is a mutation.** A run MUTATES NOTHING and
//! is a GET, which is also what makes a run URL shareable and replayable
//! (D11's "typed params → `?p.<name>=`"). The four writers —
//! `materialise`, `trust`, `new`, `delete` — are POSTs on the
//! loopback-only sub-router, because each of them records a decision this
//! daemon will honour later, and D22's local-canonical ruling is
//! unchanged.
//!
//! **`?cmd=`/`?p.` never execute a mutation** (D21): every value a
//! caller supplies here lands in a declared, typed param that a
//! CLOSED op set consumes. There is no arg that names a route, a
//! command, or a lane's tool.

use super::run::{Context, ParamError, RunRequest};
use super::{loader, run, Home, LoadedRecipe, TrustState};
use crate::routes::{find_repo, ApiError};
use crate::state::SharedState;
use crate::store::StoreBlocking;
use axum::extract::{Path, Query, RawQuery, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Slugs the router's own literal segments already occupy. A recipe by
/// one of these names is refused at LOAD rather than silently shadowed
/// (`boards::routes::RESERVED_SLUGS`' precedent).
pub const RESERVED_SLUGS: &[&str] = &["runs", "new"];

/// `urn:` problem types this family mints.
pub const ERR_UNTRUSTED: &str = "urn:kb:errors:recipe-untrusted";
pub const ERR_PARAM: &str = "urn:kb:errors:recipe-param";

// ---------------------------------------------------------------------------
// Params
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RepoParams {
    pub repo: String,
}

#[derive(Debug, Deserialize)]
pub struct RunParams {
    pub repo: String,
    /// Overrides the recipe's own `scope` (a kbc-scope/1 expression).
    #[serde(default)]
    pub scope: Option<String>,
    /// Caps EVERY step. Over [`super::MAX_STEP_ROWS`] is a 400 naming
    /// both numbers — never a silent clamp.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Return only this view (the whole set is returned when absent).
    #[serde(default)]
    pub view: Option<String>,
}

/// `?p.<name>=` and `?ctx.<field>=`, pulled out of the raw query because
/// `serde_urlencoded` cannot flatten a map beside typed fields.
fn dotted(raw: Option<&str>) -> (BTreeMap<String, String>, Context) {
    let mut params = BTreeMap::new();
    let mut ctx = Context::default();
    for (k, v) in parse_query(raw.unwrap_or_default()) {
        if let Some(name) = k.strip_prefix("p.") {
            params.insert(name.to_string(), v);
        } else if let Some(field) = k.strip_prefix("ctx.") {
            match field {
                "repo" => ctx.repo = Some(v),
                "path" => ctx.path = Some(v),
                "symbol" => ctx.symbol = Some(v),
                "ref" => ctx.git_ref = Some(v),
                "scope" => ctx.scope = Some(v),
                // An unknown `ctx.` key is ignored rather than refused:
                // `$context` is the reader's position, and a SPA sending
                // a field a newer/older daemon does not know must not
                // fail the whole run.
                _ => {}
            }
        }
    }
    (params, ctx)
}

// ---------------------------------------------------------------------------
// Wire
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct CatalogEntry {
    #[serde(flatten)]
    pub recipe: LoadedRecipe,
    /// The `kb-code` line that runs this recipe with its declared
    /// params — D11's "print the CLI line". Composed here, once, so the
    /// SPA never has to build it (root invariant #35's discipline).
    pub cli: String,
}

#[derive(Debug, Serialize)]
pub struct CatalogOut {
    pub schema: &'static str,
    pub repo: String,
    pub intents: &'static [&'static str],
    pub recipes: Vec<CatalogEntry>,
    /// Recipes that could NOT be loaded, with the reason. Reported so a
    /// broken authored recipe is visible instead of absent.
    pub problems: Vec<loader::LoadProblem>,
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// `GET /api/recipe?repo=` — the catalog, with each row's home and trust.
pub async fn catalog_route(
    State(state): State<SharedState>,
    Query(params): Query<RepoParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo_name = repo.name.clone();
    let repo_root = repo.path.clone();
    let name_bg = repo_name.clone();
    let (recipes, problems) = state
        .store
        .run_blocking(move |store| loader::catalog(store, repo_id, &name_bg, &repo_root))
        .await;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(CatalogOut {
            schema: super::SCHEMA,
            intents: super::INTENT_GROUPS,
            recipes: recipes
                .into_iter()
                .map(|r| CatalogEntry {
                    cli: cli_line(&r, &repo_name),
                    recipe: r,
                })
                .collect(),
            repo: repo_name,
            problems,
        }),
    ))
}

/// `GET /api/recipe/{slug}?repo=` — one recipe, with its params, views
/// and (for a repo file) the trust diff.
pub async fn show_route(
    State(state): State<SharedState>,
    Path(slug): Path<String>,
    Query(params): Query<RepoParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo_name = repo.name.clone();
    let repo_root = repo.path.clone();
    let recipe = resolve_one(&state, repo_id, &repo_name, &repo_root, &slug).await?;
    let cli = cli_line(&recipe, &repo_name);
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": super::SCHEMA,
            "repo": repo_name,
            "recipe": recipe,
            "cli": cli,
            "ops": super::ops::Op::ALL.iter().map(|o| o.as_str()).collect::<Vec<_>>(),
        })),
    ))
}

/// `GET /api/recipe/{slug}/lint?repo=` — would this load, and would it
/// run? A LIST of problems, never a first error (the `canvas lint`
/// precedent: an agent retries the whole document).
pub async fn lint_route(
    State(state): State<SharedState>,
    Path(slug): Path<String>,
    Query(params): Query<RepoParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo_name = repo.name.clone();
    let repo_root = repo.path.clone();
    let (recipes, problems) = state
        .store
        .run_blocking(move |store| loader::catalog(store, repo_id, &repo_name, &repo_root))
        .await;
    let mine: Vec<_> = problems
        .into_iter()
        .filter(|p| p.path.contains(&slug))
        .collect();
    let found = recipes.iter().find(|r| r.doc.slug == slug);
    let mut report: Vec<serde_json::Value> = mine
        .iter()
        .map(|p| serde_json::json!({"severity": "refuse", "at": p.path, "message": p.message}))
        .collect();
    if let Some(r) = found {
        if r.trust != TrustState::Trusted {
            report.push(serde_json::json!({
                "severity": "warn",
                "at": "trust",
                "message": format!(
                    "this recipe is `{}` and will not run until it is trusted \
                     (`kb-code recipe trust {slug} --repo <r>`)",
                    r.trust.as_str()
                ),
            }));
        }
        if let Some(home) = r.shadowed_by {
            report.push(serde_json::json!({
                "severity": "warn",
                "at": "slug",
                "message": format!(
                    "this repo file shadows a {} recipe of the same slug",
                    home.as_str()
                ),
            }));
        }
    } else if report.is_empty() {
        return Err(ApiError::not_found(format!("no recipe {slug:?}")));
    }
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": super::SCHEMA,
            "recipe": slug,
            "ok": report.iter().all(|r| r["severity"] != "refuse"),
            "report": report,
        })),
    ))
}

/// `GET /api/recipe/{slug}/run?repo=&p.<n>=&ctx.<n>=&scope=&limit=&view=`
/// — a bearer READ. Mutates nothing.
pub async fn run_route(
    State(state): State<SharedState>,
    Path(slug): Path<String>,
    Query(params): Query<RunParams>,
    RawQuery(raw): RawQuery,
) -> Result<impl IntoResponse, ApiError> {
    let out = run_inner(&state, &slug, &params, raw.as_deref()).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// `POST /api/recipe/{slug}/materialise?…` — loopback. Runs the recipe
/// and STORES the result under a `run_` id with the mirror generation it
/// was computed at, so a URL can replay exactly this answer later and
/// say plainly that it is a snapshot.
pub async fn materialise_route(
    State(state): State<SharedState>,
    Path(slug): Path<String>,
    Query(params): Query<RunParams>,
    RawQuery(raw): RawQuery,
) -> Result<impl IntoResponse, ApiError> {
    let (_repo, repo_id) = find_repo(&state, &params.repo)?;
    let out = run_inner(&state, &slug, &params, raw.as_deref()).await?;
    let id = format!("run_{}", crate::annotations::new_annotation_id());
    let body = serde_json::to_string(&out).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("serialize run: {e}"),
        )
    })?;
    let params_json = serde_json::to_string(&out.params).unwrap_or_else(|_| "{}".into());
    let scope = out.scope.expression.clone();
    let generation = out.honesty.generation;
    let now = chrono::Utc::now().timestamp();
    let slug2 = slug.clone();
    let id2 = id.clone();
    state
        .store
        .run_blocking(move |store| {
            store.insert_recipe_run(
                &id2,
                repo_id,
                &slug2,
                &params_json,
                if scope.is_empty() { None } else { Some(&scope) },
                generation,
                &body,
                now,
            )
        })
        .await?;
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": super::RUN_SCHEMA,
            "run_id": id,
            "generation": generation,
            "created_unix": now,
        })),
    ))
}

/// `GET /api/recipe/runs/{id}` — replay a materialised run. The stored
/// body comes back verbatim with a `replay` block naming both
/// generations; a caller can see it is a snapshot without diffing.
pub async fn replay_route(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let id2 = id.clone();
    let row = state
        .store
        .run_blocking(move |store| store.get_recipe_run(&id2))
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no materialised run {id:?}")))?;
    let current = state.store.generation();
    let mut body: run::RunOut = serde_json::from_str(&row.result_json).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("stored run is unreadable: {e}"),
        )
    })?;
    body.replay = Some(run::Replay {
        run_id: row.id,
        created_unix: row.created_unix,
        generation: row.generation,
        current_generation: current,
        stale: row.generation != current,
    });
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(body)))
}

#[derive(Debug, Deserialize)]
pub struct TrustBody {
    pub repo: String,
}

/// `POST /api/recipe/{slug}/trust` — loopback. Accept the bytes CURRENTLY
/// at the repo's default ref. Deliberately not "trust this slug forever":
/// the row records the content hash, so the next change re-arms the
/// prompt with a diff.
pub async fn trust_route(
    State(state): State<SharedState>,
    Path(slug): Path<String>,
    Json(body): Json<TrustBody>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &body.repo)?;
    let repo_root = repo.path.clone();
    let slug2 = slug.clone();
    let now = chrono::Utc::now().timestamp();
    let out = state
        .store
        .run_blocking(move |store| {
            let (files, _) = loader::read_repo_files(&repo_root);
            let Some(f) = files.into_iter().find(|f| f.slug == slug2) else {
                return Ok::<_, ApiError>(None);
            };
            // Refuse to trust something that will not load — accepting an
            // unparseable file would record a decision about bytes nobody
            // can run.
            super::load_toml(&f.text)
                .map_err(|e| ApiError::bad_request(format!("{slug2} does not load: {e}")))?;
            store.put_recipe_trust(repo_id, &slug2, &f.path, &f.blob, &f.text, now)?;
            Ok(Some((f.path, f.blob)))
        })
        .await?;
    let Some((path, blob)) = out else {
        return Err(ApiError::not_found(format!(
            "no {}/{slug}.toml at this repo's default ref",
            loader::RECIPE_DIR
        )));
    };
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": super::SCHEMA,
            "recipe": slug,
            "trusted": true,
            "source": format!("repo:{path}@{blob}"),
            "content_hash": blob,
        })),
    ))
}

#[derive(Debug, Deserialize)]
pub struct NewBody {
    /// `None` = available on every repo.
    #[serde(default)]
    pub repo: Option<String>,
    /// The recipe document. Type-checked through the SAME door a repo
    /// file goes through, so `recipe new` cannot store something the
    /// runner would refuse.
    pub recipe: serde_json::Value,
}

/// `POST /api/recipe/new` — loopback. Writes to the DAEMON, never into
/// the tree (D11).
pub async fn new_route(
    State(state): State<SharedState>,
    Json(body): Json<NewBody>,
) -> Result<impl IntoResponse, ApiError> {
    let doc = super::load_json(body.recipe)
        .map_err(|e| ApiError::bad_request(format!("recipe does not load: {e}")))?;
    if RESERVED_SLUGS.contains(&doc.slug.as_str()) {
        return Err(ApiError::bad_request(format!(
            "slug {:?} is reserved by a route segment; known reserved: {}",
            doc.slug,
            RESERVED_SLUGS.join(", ")
        )));
    }
    if doc.native.is_some() {
        return Err(ApiError::bad_request(
            "a stored recipe may not claim a `native` body — that body is compiled in",
        ));
    }
    if let Some(r) = &body.repo {
        find_repo(&state, r)?;
    }
    let slug = doc.slug.clone();
    let title = doc.title.clone();
    let repo = body.repo.clone();
    let json = serde_json::to_string(&doc)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let now = chrono::Utc::now().timestamp();
    let slug2 = slug.clone();
    state
        .store
        .run_blocking(move |store| {
            store.put_recipe_server(&slug2, repo.as_deref(), &title, &json, now)
        })
        .await?;
    Ok((
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": super::SCHEMA,
            "recipe": slug,
            "home": Home::Server.as_str(),
        })),
    ))
}

/// `DELETE /api/recipe/{slug}` — loopback. Removes a SERVER row only; a
/// repo file and a builtin are not this daemon's to delete.
pub async fn delete_route(
    State(state): State<SharedState>,
    Path(slug): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let slug2 = slug.clone();
    let gone = state
        .store
        .run_blocking(move |store| store.delete_recipe_server(&slug2))
        .await?;
    if !gone {
        return Err(ApiError::not_found(format!(
            "no server-stored recipe {slug:?} (a builtin or a repo file is not deletable here)"
        )));
    }
    Ok((
        StatusCode::NO_CONTENT,
        [(header::CACHE_CONTROL, "no-store")],
    ))
}

// ---------------------------------------------------------------------------
// Shared
// ---------------------------------------------------------------------------

async fn resolve_one(
    state: &SharedState,
    repo_id: i64,
    repo_name: &str,
    repo_root: &std::path::Path,
    slug: &str,
) -> Result<LoadedRecipe, ApiError> {
    let name = repo_name.to_string();
    let root = repo_root.to_path_buf();
    let (recipes, problems) = state
        .store
        .run_blocking(move |store| loader::catalog(store, repo_id, &name, &root))
        .await;
    recipes
        .into_iter()
        .find(|r| r.doc.slug == slug)
        .ok_or_else(|| {
            let hint = problems
                .iter()
                .find(|p| p.path.contains(slug))
                .map(|p| format!(" — a file for it exists but does not load: {}", p.message))
                .unwrap_or_default();
            ApiError::not_found(format!("no recipe {slug:?}{hint}"))
        })
}

async fn run_inner(
    state: &SharedState,
    slug: &str,
    params: &RunParams,
    raw: Option<&str>,
) -> Result<run::RunOut, ApiError> {
    let (repo, repo_id) = find_repo(state, &params.repo)?;
    let repo = repo.clone();
    let recipe = resolve_one(state, repo_id, &repo.name, &repo.path, slug).await?;
    if !recipe.trust.runnable() {
        // A refusal that TELLS the caller what to do. `changed` also
        // ships the diff, because "accept again?" without it is a prompt
        // nobody can answer honestly.
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            format!(
                "recipe {slug:?} is `{}` — accept it with \
                 `kb-code recipe trust {slug} --repo {}` (loopback only)",
                recipe.trust.as_str(),
                repo.name
            ),
        )
        .with_problem_type(ERR_UNTRUSTED));
    }
    let limit = run::parse_limit(params.limit).map_err(param_error)?;
    let (raw_params, context) = dotted(raw);
    let resolved = run::validate_params(&recipe.doc, &raw_params).map_err(param_error)?;

    let req_scope = params.scope.clone();
    let store = state.store.clone();
    let factors = state.search_factors;
    let scopes = state.scopes.clone();
    let lanes = state.lanes.clone();
    let file_index = state.file_index.clone();
    let symbol_index = state.symbol_index.clone();
    let blame_cache = state.blame_cache.clone();
    let out = store
        .run_blocking(move |s| {
            let req = RunRequest {
                recipe: &recipe,
                params: resolved,
                context,
                scope_override: req_scope,
                limit,
            };
            run::run(
                &req,
                s,
                &repo,
                repo_id,
                &scopes,
                &lanes,
                factors,
                &file_index,
                &symbol_index,
                &blame_cache,
            )
        })
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let mut out = out;
    if let Some(want) = &params.view {
        if !out.views.iter().any(|v| &v.id == want) {
            return Err(ApiError::bad_request(format!(
                "no view {want:?}; this recipe declares: {}",
                out.views
                    .iter()
                    .map(|v| v.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        out.views.retain(|v| &v.id == want);
    }
    Ok(out)
}

fn param_error(e: ParamError) -> ApiError {
    ApiError::bad_request(format!("p.{}: {}", e.field, e.message)).with_problem_type(ERR_PARAM)
}

/// `application/x-www-form-urlencoded` pairs. Hand-rolled rather than
/// pulling `form_urlencoded` into this crate's dependency graph for
/// twenty lines: `Query<T>` already handles the TYPED half, and this
/// only has to recover the `p.`/`ctx.` keys serde cannot flatten.
pub fn parse_query(raw: &str) -> Vec<(String, String)> {
    raw.split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(b) => {
                    out.push(b);
                    i += 3;
                }
                Err(_) => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// D11's "print the CLI line" — composed server-side, once.
pub fn cli_line(r: &LoadedRecipe, repo: &str) -> String {
    let mut s = format!("kb-code recipe run {} --repo {repo}", r.doc.slug);
    for p in &r.doc.params {
        if p.required {
            s.push_str(&format!(" --p {}=<{}>", p.name, p.ty.as_str()));
        }
    }
    s
}

// ---------------------------------------------------------------------------
// Route contracts (invariant 15)
// ---------------------------------------------------------------------------

use crate::entities::RouteContract;

fn accepts_without<T: serde::de::DeserializeOwned>(pairs: &[(&str, &str)], omit: &str) -> bool {
    let map: serde_json::Map<String, serde_json::Value> = pairs
        .iter()
        .filter(|(k, _)| *k != omit)
        .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
        .collect();
    serde_json::from_value::<T>(serde_json::Value::Object(map)).is_ok()
}

pub fn repo_params_accept_without(omit: &str) -> bool {
    accepts_without::<RepoParams>(&[("repo", "r")], omit)
}

pub fn run_params_accept_without(omit: &str) -> bool {
    accepts_without::<RunParams>(
        &[("repo", "r"), ("scope", "path:app//*"), ("view", "rows")],
        omit,
    )
}

pub const RECIPE_CATALOG_ROUTE: RouteContract = RouteContract {
    path: "/api/recipe",
    handler: "recipe::routes::catalog_route",
    required_params: &["repo"],
    params_accept_without: repo_params_accept_without,
};

pub const RECIPE_SHOW_ROUTE: RouteContract = RouteContract {
    path: "/api/recipe/{slug}",
    handler: "recipe::routes::show_route",
    required_params: &["repo"],
    params_accept_without: repo_params_accept_without,
};

pub const RECIPE_LINT_ROUTE: RouteContract = RouteContract {
    path: "/api/recipe/{slug}/lint",
    handler: "recipe::routes::lint_route",
    required_params: &["repo"],
    params_accept_without: repo_params_accept_without,
};

pub const RECIPE_RUN_ROUTE: RouteContract = RouteContract {
    path: "/api/recipe/{slug}/run",
    handler: "recipe::routes::run_route",
    required_params: &["repo"],
    params_accept_without: run_params_accept_without,
};

/// The GET surface. The four mutations are absent for the reason
/// `boards::V74_L1_ROUTES` records: a `RouteContract` describes a
/// query-param surface, and a POST whose payload IS the contract has
/// nothing for `params_accept_without` to say. `replay_route` is absent
/// too — its only input is a path segment.
pub const V74_L3A_ROUTES: &[RouteContract] = &[
    RECIPE_CATALOG_ROUTE,
    RECIPE_SHOW_ROUTE,
    RECIPE_LINT_ROUTE,
    RECIPE_RUN_ROUTE,
];
