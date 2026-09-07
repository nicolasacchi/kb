//! The DAG runner (V74-L3a, D11).
//!
//! Four things happen here and nowhere else: params are VALIDATED
//! (`limit > 500` refuses with both numbers rather than clamping — the
//! `recipes/1` rough edge D11's repair list names), the kbc-scope/1
//! expression is RESOLVED once (a scope with a diagnostic is not applied
//! and the reason is captioned — invariant 17(b), never a silent
//! narrowing), the steps are EXECUTED in declaration order with a memo
//! and a wall-clock budget, and the views are RENDERED from the resulting
//! address sets.
//!
//! Determinism is a property of the whole path: every op sorts on the
//! address itself, the memo keys on (op, resolved args), the views read
//! rows in the order the step returned them, and nothing consults a clock
//! except the budget and `as_of`. `tests::two_runs_are_byte_identical`
//! pins it.

use super::census::{EmptyReason, StepCensus};
use super::ops::{self, Op, RunCtx};
use super::{
    Addr, ArgRef, ArgVal, ColField, Home, LoadedRecipe, ParamType, ScalarVal, TrustState,
    BLOB_UNKNOWN, DEFAULT_STEP_ROWS, MAX_STEP_ROWS, RUN_BUDGET_MS, RUN_SCHEMA,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A validation refusal: the FIELD, then why. A 400 that does not name
/// the field is a 400 the caller has to guess at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamError {
    pub field: String,
    pub message: String,
}

impl ParamError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        ParamError {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ParamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

/// The `$context` a caller supplies (`ctx.<field>=` on the wire).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

impl Context {
    pub fn field(&self, name: &str) -> Option<&str> {
        match name {
            "repo" => self.repo.as_deref(),
            "path" => self.path.as_deref(),
            "symbol" => self.symbol.as_deref(),
            "ref" => self.git_ref.as_deref(),
            "scope" => self.scope.as_deref(),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Param validation
// ---------------------------------------------------------------------------

/// Validate the caller's raw `?p.<name>=` strings against the recipe's
/// declared params, applying defaults. REFUSES with the field name on
/// the first problem; a range violation reports both bounds and the
/// value, because "out of range" without the numbers is not actionable.
pub fn validate_params(
    doc: &super::RecipeDoc,
    raw: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, ArgVal>, ParamError> {
    // An unknown `p.<name>` is a REFUSAL, not a shrug: it is nearly
    // always a typo for a real one, and silently ignoring it would run a
    // different query than the caller asked for.
    for k in raw.keys() {
        if !doc.params.iter().any(|p| &p.name == k) {
            let known: Vec<&str> = doc.params.iter().map(|p| p.name.as_str()).collect();
            return Err(ParamError::new(
                k,
                if known.is_empty() {
                    "this recipe declares no params".to_string()
                } else {
                    format!("unknown param; this recipe declares: {}", known.join(", "))
                },
            ));
        }
    }
    let mut out = BTreeMap::new();
    for spec in &doc.params {
        let given = raw.get(&spec.name).map(|s| s.as_str());
        let value = match given {
            Some(s) if !s.is_empty() => coerce(spec, s)?,
            _ => match (&spec.default, spec.required) {
                (Some(d), _) => d.clone(),
                (None, true) => {
                    return Err(ParamError::new(
                        &spec.name,
                        format!(
                            "required ({}){}",
                            spec.ty.as_str(),
                            if spec.values.is_empty() {
                                String::new()
                            } else {
                                format!("; one of: {}", spec.values.join(", "))
                            }
                        ),
                    ))
                }
                (None, false) => continue,
            },
        };
        out.insert(spec.name.clone(), value);
    }
    Ok(out)
}

fn coerce(spec: &super::ParamSpec, s: &str) -> Result<ArgVal, ParamError> {
    let v = match spec.ty {
        ParamType::Int => {
            let n: i64 = s
                .trim()
                .parse()
                .map_err(|_| ParamError::new(&spec.name, format!("{s:?} is not an integer")))?;
            range_check(spec, n as f64)?;
            ArgVal::Int(n)
        }
        ParamType::Float => {
            let n: f64 = s
                .trim()
                .parse()
                .map_err(|_| ParamError::new(&spec.name, format!("{s:?} is not a number")))?;
            range_check(spec, n)?;
            ArgVal::Float(n)
        }
        ParamType::Bool => match s.trim() {
            "1" | "true" | "yes" => ArgVal::Bool(true),
            "0" | "false" | "no" => ArgVal::Bool(false),
            other => {
                return Err(ParamError::new(
                    &spec.name,
                    format!("{other:?} is not a boolean (true|false|1|0|yes|no)"),
                ))
            }
        },
        ParamType::Enum => {
            if !spec.values.iter().any(|v| v == s) {
                return Err(ParamError::new(
                    &spec.name,
                    format!("{s:?} is not one of: {}", spec.values.join(", ")),
                ));
            }
            ArgVal::Str(s.to_string())
        }
        ParamType::Path => {
            // The SAME lexical rejection every content route applies
            // (`routes::safe_rel_path`), so a recipe param can never be
            // the thing that reaches outside the repo.
            crate::routes::safe_rel_path(s)
                .map_err(|e| ParamError::new(&spec.name, e.message().to_string()))?;
            ArgVal::Str(s.to_string())
        }
        ParamType::Ref => {
            // Invariant 3: a validated ref is a TYPE. A recipe-supplied
            // ref goes through the same constructor a hand-typed one does.
            crate::git::Revspec::parse(s)
                .map_err(|e| ParamError::new(&spec.name, e.to_string()))?;
            ArgVal::Str(s.to_string())
        }
        ParamType::Symbol | ParamType::String => ArgVal::Str(s.to_string()),
    };
    Ok(v)
}

fn range_check(spec: &super::ParamSpec, v: f64) -> Result<(), ParamError> {
    if let Some(min) = spec.min {
        if v < min {
            return Err(ParamError::new(
                &spec.name,
                format!("{v} is below the declared minimum {min}"),
            ));
        }
    }
    if let Some(max) = spec.max {
        if v > max {
            return Err(ParamError::new(
                &spec.name,
                format!("{v} is above the declared maximum {max}"),
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Wire
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeReport {
    pub expression: String,
    /// `false` = the scope was NOT applied and the run is UNSCOPED. The
    /// reason is in `notes`; a refusal never silently narrows.
    pub applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normalized: Option<String>,
    /// The number of mirror paths the expression selected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRun {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub op: Option<Op>,
    /// Set instead of `op` for a native adapter: the engine that
    /// produced these rows (`recipes/1`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub title: String,
    pub rows: Vec<Addr>,
    /// The TRUE row count before the cap.
    pub total: usize,
    pub truncated: bool,
    pub ms: u64,
    pub census: StepCensus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewRun {
    pub id: String,
    pub kind: super::ViewKind,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub title: String,
    pub step: String,
    pub columns: Vec<ViewColumn>,
    /// One row per address, cells in column order. A cell is always a
    /// string rendering of an ADDRESS field or one of that address's own
    /// scalars — never free text.
    pub rows: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewColumn {
    pub header: String,
    pub field: ColField,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Honesty {
    /// The store's monotonic mirror generation this run read. NOT a
    /// commit distance (invariant 16(d)'s rule, restated).
    pub generation: u64,
    pub as_of: String,
    pub budget_ms: u64,
    pub elapsed_ms: u64,
    pub budget_exhausted: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunOut {
    pub schema: String,
    pub recipe: String,
    pub title: String,
    pub intent: String,
    pub home: Home,
    pub source: String,
    pub trust: TrustState,
    pub repo: String,
    pub params: BTreeMap<String, ArgVal>,
    pub scope: ScopeReport,
    pub steps: Vec<StepRun>,
    pub views: Vec<ViewRun>,
    pub honesty: Honesty,
    /// Set only on a REPLAYED materialised run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay: Option<Replay>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Replay {
    pub run_id: String,
    pub created_unix: i64,
    /// The mirror generation the SNAPSHOT was computed at.
    pub generation: u64,
    /// The mirror generation NOW. Different ⇒ the snapshot is stale, and
    /// the caption says so rather than the payload pretending to be live.
    pub current_generation: u64,
    pub stale: bool,
}

// ---------------------------------------------------------------------------
// The runner
// ---------------------------------------------------------------------------

pub struct RunRequest<'a> {
    pub recipe: &'a LoadedRecipe,
    pub params: BTreeMap<String, ArgVal>,
    pub context: Context,
    /// `?scope=` overrides the recipe's own default when present.
    pub scope_override: Option<String>,
    /// `?limit=` caps EVERY step. Already range-checked by the route.
    pub limit: Option<usize>,
}

/// Execute a recipe. Sync — the caller wraps the whole thing in ONE
/// `run_blocking` hop (the 2026-08-31 starvation convention: one coarse
/// blocking hop for the whole ladder, never one per engine).
#[allow(clippy::too_many_arguments)]
pub fn run(
    req: &RunRequest<'_>,
    store: &crate::store::Store,
    repo: &crate::config::RepoEntry,
    repo_id: i64,
    scopes: &crate::config::ScopesSection,
    lanes: &crate::config::LanesSection,
    factors: crate::search::Factors,
    file_index: &crate::search::files::FileIndex,
    symbol_index: &crate::search::symbols::SymbolIndex,
    blame_cache: &crate::blame::BlameCache,
) -> Result<RunOut, String> {
    let started = std::time::Instant::now();
    let deadline = started + std::time::Duration::from_millis(RUN_BUDGET_MS as u64);
    let doc = &req.recipe.doc;
    let mut notes: Vec<String> = Vec::new();

    // --- scope, resolved ONCE ---------------------------------------------
    let expression = req
        .scope_override
        .clone()
        .unwrap_or_else(|| doc.scope.clone());
    let expression = if expression == "$context.scope" {
        req.context.scope.clone().unwrap_or_default()
    } else {
        expression
    };
    let files = store.list_files(repo_id).map_err(|e| e.to_string())?;
    let mut scope_report = ScopeReport {
        expression: expression.clone(),
        applied: false,
        normalized: None,
        matched: None,
        notes: Vec::new(),
    };
    let mut scope_paths = None;
    if !expression.trim().is_empty() {
        let src = {
            let mut s = crate::tree::scope::ScopeSources {
                config_scopes: scopes.map.clone(),
                ..Default::default()
            };
            s.files = files
                .iter()
                .map(|f| crate::tree::scope::ScopeFile {
                    path: f.path.clone(),
                    lang: f.lang.clone(),
                })
                .collect();
            s
        };
        let resolved = crate::tree::scope::resolve(&expression, &src);
        scope_report.normalized = Some(resolved.normalized.clone());
        scope_report.notes.extend(resolved.notes.clone());
        for d in &resolved.diagnostics {
            scope_report
                .notes
                .push(format!("{}: {}", d.token, d.message));
        }
        if resolved.applied {
            scope_report.applied = true;
            scope_report.matched = resolved.paths.as_ref().map(|p| p.len());
            scope_paths = resolved.paths;
        } else {
            scope_report
                .notes
                .push("scope NOT applied — this run is UNSCOPED".to_string());
            notes.push(
                "the requested scope was refused, so every step ran unscoped; see scope.notes"
                    .to_string(),
            );
        }
    }

    let ctx = RunCtx {
        store,
        repo,
        repo_id,
        repo_name: &repo.name,
        repo_root: &repo.path,
        scopes,
        lanes,
        factors,
        file_index,
        symbol_index,
        blame_cache,
        scope_paths: scope_paths.as_ref(),
        today: chrono::Utc::now().date_naive(),
        now_unix: chrono::Utc::now().timestamp(),
        deadline,
    };

    // --- the native bridge -------------------------------------------------
    let mut steps: Vec<StepRun> = Vec::new();
    let mut outputs: BTreeMap<String, Vec<Addr>> = BTreeMap::new();
    if let Some(native) = &doc.native {
        let t0 = std::time::Instant::now();
        let step = super::builtins::run_native(
            native,
            &ctx,
            &req.params,
            req.limit.unwrap_or(DEFAULT_STEP_ROWS),
        )?;
        let mut sr = step;
        sr.ms = t0.elapsed().as_millis() as u64;
        outputs.insert(sr.id.clone(), sr.rows.clone());
        steps.push(sr);
    } else {
        // --- the DAG -------------------------------------------------------
        // Memo over (op, rendered args). Two steps asking the identical
        // question of the identical engine get one answer, which is both a
        // cost saving and a determinism guarantee.
        let mut memo: BTreeMap<String, (Vec<Addr>, usize, StepCensus)> = BTreeMap::new();
        for spec in &doc.steps {
            let t0 = std::time::Instant::now();
            let mut args: ops::Args = BTreeMap::new();
            for (k, v) in &spec.args {
                let parsed = super::parse_arg(v).map_err(|e| format!("{}: {e}", spec.id))?;
                let resolved = match parsed {
                    ArgRef::Literal(l) => ops::Resolved::Val(l),
                    ArgRef::Param(name) => match req.params.get(&name) {
                        Some(v) => ops::Resolved::Val(v.clone()),
                        // An optional param the caller omitted simply
                        // removes its arg — the step runs without that
                        // narrowing rather than with an empty one.
                        None => continue,
                    },
                    ArgRef::Context(field) => match req.context.field(&field) {
                        Some(v) if !v.is_empty() => ops::Resolved::Val(ArgVal::Str(v.to_string())),
                        _ => continue,
                    },
                    ArgRef::Step(id) => match outputs.get(&id) {
                        Some(rows) => ops::Resolved::Set(rows.clone()),
                        None => return Err(format!("{}: `$steps.{id}` has not run", spec.id)),
                    },
                };
                args.insert(k.clone(), resolved);
            }
            let key = memo_key(spec.op, &args);
            let (rows, total, census) = match memo.get(&key) {
                Some(hit) => hit.clone(),
                None => {
                    let out = ops::execute(spec.op, &ctx, &args)
                        .map_err(|e| format!("{}: {e}", spec.id))?;
                    let cap = req
                        .limit
                        .map(|l| out.rows.len().min(l))
                        .unwrap_or(out.rows.len());
                    let mut rows = out.rows;
                    rows.truncate(cap);
                    let triple = (rows, out.total, out.census);
                    memo.insert(key, triple.clone());
                    triple
                }
            };
            outputs.insert(spec.id.clone(), rows.clone());
            steps.push(StepRun {
                id: spec.id.clone(),
                op: Some(spec.op),
                engine: None,
                title: spec.title.clone(),
                truncated: rows.len() < total,
                total,
                rows,
                ms: t0.elapsed().as_millis() as u64,
                census,
            });
        }
    }

    // --- views -------------------------------------------------------------
    let views = doc
        .views
        .iter()
        .map(|v| {
            let rows = outputs.get(&v.step).cloned().unwrap_or_default();
            ViewRun {
                id: v.id.clone(),
                kind: v.kind,
                title: v.title.clone(),
                step: v.step.clone(),
                columns: v
                    .columns
                    .iter()
                    .map(|c| ViewColumn {
                        header: if c.header.is_empty() {
                            c.field.as_label()
                        } else {
                            c.header.clone()
                        },
                        field: c.field.clone(),
                    })
                    .collect(),
                rows: rows
                    .iter()
                    .map(|a| v.columns.iter().map(|c| cell(a, &c.field)).collect())
                    .collect(),
            }
        })
        .collect();

    let elapsed = started.elapsed().as_millis();
    Ok(RunOut {
        schema: RUN_SCHEMA.to_string(),
        recipe: doc.slug.clone(),
        title: doc.title.clone(),
        intent: doc.intent.clone(),
        home: req.recipe.home,
        source: req.recipe.source.clone(),
        trust: req.recipe.trust,
        repo: repo.name.clone(),
        params: req.params.clone(),
        scope: scope_report,
        steps,
        views,
        honesty: Honesty {
            generation: store.generation(),
            as_of: chrono::Utc::now().to_rfc3339(),
            budget_ms: RUN_BUDGET_MS as u64,
            elapsed_ms: elapsed as u64,
            budget_exhausted: elapsed >= RUN_BUDGET_MS,
            notes,
        },
        replay: None,
    })
}

fn memo_key(op: Op, args: &ops::Args) -> String {
    let mut s = String::from(op.as_str());
    for (k, v) in args {
        s.push('|');
        s.push_str(k);
        s.push('=');
        match v {
            ops::Resolved::Val(x) => s.push_str(&x.render()),
            ops::Resolved::Set(rows) => {
                s.push_str("set:");
                for r in rows {
                    let k = r.key();
                    s.push_str(&format!("{}/{}/{}/{};", k.0, k.2, k.3, k.7));
                }
            }
        }
    }
    s
}

/// Render one cell. Every branch reads the ADDRESS — there is no path
/// through this function that produces text from anywhere else.
pub fn cell(a: &Addr, f: &ColField) -> String {
    match f {
        ColField::Path => a.path.clone().unwrap_or_default(),
        ColField::Line => a.line.map(|l| l.to_string()).unwrap_or_default(),
        ColField::Symbol => a.symbol.clone().unwrap_or_default(),
        ColField::Entity => a.entity.clone().unwrap_or_default(),
        ColField::Commit => a.commit.clone().unwrap_or_default(),
        ColField::Id => a.id.clone().unwrap_or_default(),
        ColField::Blob => a.blob.clone(),
        ColField::Trust => a.trust.clone(),
        ColField::Kind => a.kind.as_str().to_string(),
        ColField::Address => render_address(a),
        ColField::Scalar(name) => a
            .scalars
            .get(name)
            .map(ops::scalar_str)
            // An absent scalar renders as the same word an absent blob
            // does. Never a blank that reads as zero.
            .unwrap_or_else(|| BLOB_UNKNOWN.to_string()),
    }
}

/// The canonical one-line address string. Stable enough to paste into
/// the reader; never a URL (root invariant #35's one-URL-builder rule —
/// composing kb-code URLs is the SPA's `lib/codeUrl.ts`'s job).
pub fn render_address(a: &Addr) -> String {
    let mut s = String::new();
    if let Some(p) = &a.path {
        s.push_str(p);
        if let Some(l) = a.line {
            s.push(':');
            s.push_str(&l.to_string());
        }
    } else if let Some(e) = &a.entity {
        s.push_str(e);
    } else if let Some(c) = &a.commit {
        s.push_str(c);
    } else if let Some(i) = &a.id {
        s.push_str(i);
    }
    if let Some(sym) = &a.symbol {
        if !s.is_empty() {
            s.push(' ');
        }
        s.push('#');
        s.push_str(sym);
    }
    if s.is_empty() {
        a.kind.as_str().to_string()
    } else {
        s
    }
}

/// The `?limit=` refusal D11 names: over the cap is a 400 with BOTH
/// numbers, never a silent clamp (kb root invariant #35's `?ids=` rule).
pub fn parse_limit(limit: Option<usize>) -> Result<Option<usize>, ParamError> {
    match limit {
        None => Ok(None),
        Some(0) => Err(ParamError::new("limit", "limit must be at least 1")),
        Some(n) if n > MAX_STEP_ROWS => Err(ParamError::new(
            "limit",
            format!("limit {n} exceeds the per-step cap of {MAX_STEP_ROWS}"),
        )),
        Some(n) => Ok(Some(n)),
    }
}

/// Helper for `save-as set`: the reading-set spans a view's addresses
/// become. Only addresses with a path can be a span; the rest are
/// REPORTED as skipped rather than silently lost.
pub fn spans_for(rows: &[Addr]) -> (Vec<serde_json::Value>, usize) {
    let mut spans = Vec::new();
    let mut skipped = 0usize;
    for a in rows {
        let Some(path) = &a.path else {
            skipped += 1;
            continue;
        };
        let mut span = serde_json::Map::new();
        span.insert("path".into(), serde_json::Value::String(path.clone()));
        if let Some(l) = a.line {
            span.insert("line_start".into(), serde_json::Value::from(l));
            span.insert("line_end".into(), serde_json::Value::from(l));
        }
        spans.push(serde_json::Value::Object(span));
    }
    (spans, skipped)
}

/// The empty-step summary a CLI/UI prints. Public so both presenters
/// render the SAME sentence (the "two renderers of one projection will
/// diverge" risk kbc-tree/1 records).
pub fn empty_summary(step: &StepRun) -> Option<String> {
    let r = step.census.empty_reason?;
    Some(format!(
        "{} — {}{}",
        r.as_str(),
        step.census.explain(),
        if r == EmptyReason::FilteredOut {
            ""
        } else {
            " (this is NOT \"nothing to worry about\")"
        }
    ))
}

/// Convert one `recipes/1` item into an address. Used by the native
/// bridge; kept here beside `cell` so the two renderings cannot drift.
pub fn addr_from_legacy_item(repo: &str, item: &serde_json::Value) -> Addr {
    let path = item.get("path").and_then(|v| v.as_str());
    let symbol = item.get("symbol").and_then(|v| v.as_str());
    let kind = if symbol.is_some() {
        super::AddrKind::Symbol
    } else {
        super::AddrKind::File
    };
    let mut a = Addr::new(kind, repo).with_trust(item.get("class").and_then(|v| v.as_str()));
    if let Some(p) = path {
        a = a.with_path(p.to_string());
    }
    if let Some(s) = symbol {
        a = a.with_symbol(s.to_string());
    }
    if let Some(l) = item.get("line").and_then(|v| v.as_u64()) {
        a = a.with_line(l as u32);
    }
    // Every top-level scalar the legacy row carried becomes a scalar
    // here — `first_seen`, `commits`, `revisions`, `churn` were dropped
    // by BOTH presenters before (the recon's finding 5), and a recipe
    // whose whole point is `first_seen` must be able to show it.
    for (k, v) in item.as_object().into_iter().flatten() {
        match k.as_str() {
            "path" | "symbol" | "line" | "class" => {}
            "terms" => {
                for (tk, tv) in v.as_object().into_iter().flatten() {
                    if let Some(s) = json_scalar(tv) {
                        a.scalars.insert(tk.clone(), s);
                    }
                }
            }
            "first_seen" => {
                if let Some(c) = v.get("commit").and_then(|x| x.as_str()) {
                    a = a.with_commit(c.to_string());
                }
                if let Some(d) = v.get("date_unix").and_then(|x| x.as_i64()) {
                    a.scalars
                        .insert("first_seen_unix".into(), ScalarVal::Int(d));
                }
            }
            "commits" => {
                let list: Vec<String> = v
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|x| x.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                a.scalars
                    .insert("commits".into(), ScalarVal::Str(list.join(",")));
            }
            other => {
                if let Some(s) = json_scalar(v) {
                    a.scalars.insert(other.to_string(), s);
                }
            }
        }
    }
    a
}

fn json_scalar(v: &serde_json::Value) -> Option<ScalarVal> {
    match v {
        serde_json::Value::Bool(b) => Some(ScalarVal::Bool(*b)),
        serde_json::Value::String(s) => Some(ScalarVal::Str(s.clone())),
        serde_json::Value::Number(n) => n
            .as_i64()
            .map(ScalarVal::Int)
            .or_else(|| n.as_f64().map(ScalarVal::Float)),
        _ => None,
    }
}
