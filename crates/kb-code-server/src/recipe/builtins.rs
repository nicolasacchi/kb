//! The shipped catalog (V74-L3a, D11: "eight built-ins ship with the
//! runner … the six existing recipes are repaired").
//!
//! Two families live here.
//!
//! **The eight DAG built-ins** are written as TOML — the same bytes a
//! repo file would carry — and parsed through the SAME
//! [`super::load_toml`] door. That is deliberate: if the shipped catalog
//! went in as hand-built Rust structs, the loader and the type-checker
//! would have no user until somebody wrote a repo file, and the first
//! person to write one would be the first person to find the bugs.
//! `tests::every_builtin_loads` is therefore a real test of the loader.
//!
//! **The six natives** are the `recipes/1` recipes (V3.3-Q1), adopted
//! onto this runner as adapters. Their BODIES stay in `crate::recipes` —
//! `new-public-api`'s language-specific export rules and
//! `god-functions`' fan-in/fan-out fold are not expressible in the op set
//! and pretending otherwise would mean growing the op set one
//! recipe-shaped variant at a time. What they gain by being here is
//! everything AROUND the body: typed params validated once, the
//! `limit > 500` refusal, kbc-scope/1 scoping, a census, views that
//! render the columns both old presenters dropped, and one `run`/`show`/
//! `lint` surface for all fourteen. `GET /api/recipes` and `GET
//! /api/recipes/{name}` are unchanged for `Recipes.tsx` (see
//! `docs/kb-code.md` for the two behaviours D11's repair list changes on
//! purpose).

use super::census::StepCensus;
use super::ops::RunCtx;
use super::run::{addr_from_legacy_item, StepRun};
use super::{ArgVal, Home, LoadedRecipe, RecipeDoc, TrustState};
use std::collections::BTreeMap;

/// The `recipes/1` slugs this runner adapts. Order is `recipes.rs`'s own.
pub const NATIVE_SLUGS: &[&str] = &[
    "new-public-api",
    "god-functions",
    "agent-only-symbols",
    "complexity-climbers",
    "unreviewed-hotspots",
    "failure-tainted",
];

// ---------------------------------------------------------------------------
// The eight
// ---------------------------------------------------------------------------

pub const ENTRY_POINTS: &str = r#"
slug = "orient:entry-points"
title = "Entry points"
intent = "orienting"
description_md = """
Every way the outside world gets in: HTTP routes, background jobs and
mailers, plus the classes the entity index knows about. Attention only —
a list of doors, not a verdict about any of them.
"""

params = [
  { name = "top", type = "int", default = 100, min = 1, max = 500, description = "rows per lane" },
]

steps = [
  { id = "routes",   op = "rails",   title = "HTTP routes",   args = { noun = "route", limit = "$p.top" } },
  { id = "jobs",     op = "rails",   title = "Background jobs", args = { noun = "job", limit = "$p.top" } },
  { id = "mailers",  op = "rails",   title = "Mailers",       args = { noun = "mailer", limit = "$p.top" } },
  { id = "inbound",  op = "set_ops", title = "Every door",    args = { mode = "union", from = "$steps.routes", with = "$steps.jobs" } },
  { id = "doors",    op = "set_ops", title = "…plus mailers", args = { mode = "union", from = "$steps.inbound", with = "$steps.mailers" } },
  { id = "classes",  op = "entity",  title = "Indexed classes", args = { kind = "class", limit = "$p.top" } },
]

views = [
  { id = "doors", kind = "table", title = "Entry points", step = "doors", columns = [
    { header = "Entry", field = "entity" },
    { header = "Path", field = "path" },
    { header = "Line", field = "line" },
    { header = "Kind", field = { scalar = "noun" } },
    { header = "Trust", field = "trust" },
  ]},
  { id = "routes", kind = "list", title = "Routes", step = "routes", columns = [
    { header = "Route", field = "address" },
    { header = "Trust", field = "trust" },
  ]},
  { id = "classes", kind = "tree", title = "Classes", step = "classes", columns = [
    { header = "Entity", field = "entity" },
    { header = "Path", field = "path" },
    { header = "Trust", field = "trust" },
  ]},
]
"#;

pub const HOT_AND_COLD: &str = r#"
slug = "orient:hot-and-cold"
title = "Hot and cold"
intent = "orienting"
description_md = """
The files this repo actually keeps changing, and what is defined inside
the hottest of them. Churn is a behavioral counter — it says where the
work has been, never whether that work was good.
"""

params = [
  { name = "min_revisions", type = "int", default = 3, min = 1, max = 10000, description = "ignore files touched fewer times" },
  { name = "top", type = "int", default = 25, min = 1, max = 500 },
]

steps = [
  { id = "churn",  op = "churn",   title = "Churn counters", args = { min_revisions = "$p.min_revisions" } },
  { id = "ranked", op = "set_ops", title = "Hottest first",  args = { mode = "sort", from = "$steps.churn", by = "churn", desc = true } },
  { id = "top",    op = "set_ops", title = "The top slice",  args = { mode = "limit", from = "$steps.ranked", n = "$p.top" } },
  { id = "inside", op = "outline", title = "What lives there", args = { from = "$steps.top", limit = 500 } },
]

views = [
  { id = "hot", kind = "table", title = "Hot files", step = "top", columns = [
    { header = "Path", field = "path" },
    { header = "Revisions", field = { scalar = "revisions" } },
    { header = "Churn", field = { scalar = "churn" } },
  ]},
  { id = "inside", kind = "table", title = "Symbols inside them", step = "inside", columns = [
    { header = "Symbol", field = "symbol" },
    { header = "Kind", field = { scalar = "kind" } },
    { header = "Path", field = "path" },
    { header = "Line", field = "line" },
    { header = "Span", field = { scalar = "line_span" } },
  ]},
]
"#;

pub const BLAST_RADIUS: &str = r#"
slug = "review:blast-radius"
title = "Blast radius"
intent = "reviewing"
description_md = """
What a window of change touched, what defines those files, who calls
those definitions, and which of those callers are tests. Every usage row
carries usages/2's OWN trust class — this recipe never raises one.
"""

params = [
  { name = "since", type = "string", required = true, description = "YYYY-MM-DD or a duration (30d, 12h)" },
  { name = "test_prefix", type = "string", default = "spec/", description = "path prefix that means \"a test\"" },
  { name = "top", type = "int", default = 40, min = 1, max = 500 },
]

steps = [
  { id = "changed", op = "git_log", title = "Files in the window", args = { since = "$p.since", emit = "files", limit = 500 } },
  { id = "defs",    op = "outline", title = "What they define",    args = { from = "$steps.changed", limit = 500 } },
  { id = "callers", op = "usages",  title = "Who calls them",      args = { from = "$steps.defs", limit = 500 } },
  { id = "tests",   op = "set_ops", title = "…that are tests",     args = { mode = "filter", from = "$steps.callers", path_prefix = "$p.test_prefix" } },
  { id = "authors", op = "blame",   title = "Who to ask",          args = { from = "$steps.changed", limit = 200 } },
  { id = "prior",   op = "review",  title = "Findings already filed", args = { kind = "findings", limit = "$p.top" } },
]

views = [
  { id = "callers", kind = "table", title = "Callers", step = "callers", columns = [
    { header = "Path", field = "path" },
    { header = "Line", field = "line" },
    { header = "Of", field = "symbol" },
    { header = "Trust", field = "trust" },
    { header = "Kind", field = { scalar = "usage_kind" } },
  ]},
  { id = "tests", kind = "list", title = "Tests that touch them", step = "tests", columns = [
    { header = "Where", field = "address" },
    { header = "Trust", field = "trust" },
  ]},
  { id = "changed", kind = "table", title = "Changed files", step = "changed", columns = [
    { header = "Path", field = "path" },
    { header = "Churn", field = { scalar = "churn" } },
  ]},
  { id = "prior", kind = "table", title = "Prior findings", step = "prior", columns = [
    { header = "Slug", field = "id" },
    { header = "Severity", field = { scalar = "severity" } },
    { header = "Title", field = { scalar = "title" } },
    { header = "Path", field = "path" },
  ]},
]
"#;

pub const UNTESTED_CHANGES: &str = r#"
slug = "review:untested-changes"
title = "Untested changes"
intent = "reviewing"
description_md = """
Files a window of change touched that are neither tests themselves nor
covered by a coverage lane. **Read the census**: with no coverage lane
enabled this is "no coverage evidence exists", which is a very different
claim from "these files are untested".
"""

params = [
  { name = "since", type = "string", required = true, description = "YYYY-MM-DD or a duration (30d, 12h)" },
  { name = "test_scope", type = "string", default = "path:spec//*||path:test//*", description = "kbc-scope/1 expression selecting the tests" },
  { name = "lane", type = "string", default = "coverage.simplecov", description = "the aug-lane/1 coverage lane to consult" },
]

steps = [
  { id = "changed",  op = "git_log", title = "Files in the window", args = { since = "$p.since", emit = "files", limit = 500 } },
  { id = "tests",    op = "tree",    title = "The test tree",       args = { scope = "$p.test_scope", limit = 500 } },
  { id = "code",     op = "set_ops", title = "…that are not tests", args = { mode = "diff", from = "$steps.changed", with = "$steps.tests" } },
  { id = "covered",  op = "facts",   title = "Coverage evidence",   args = { lane = "$p.lane", from = "$steps.code", limit = 500 } },
  { id = "covered_files", op = "map", title = "…as files",          args = { from = "$steps.covered", to = "file-of" } },
  { id = "untested", op = "set_ops", title = "No evidence either way", args = { mode = "diff", from = "$steps.code", with = "$steps.covered_files" } },
]

views = [
  { id = "untested", kind = "table", title = "Changed, no test or coverage evidence", step = "untested", columns = [
    { header = "Path", field = "path" },
    { header = "Churn", field = { scalar = "churn" } },
    { header = "Blob", field = "blob" },
  ]},
  { id = "covered", kind = "table", title = "Coverage facts found", step = "covered", columns = [
    { header = "Path", field = "path" },
    { header = "Lane", field = { scalar = "lane" } },
    { header = "Trust", field = "trust" },
  ]},
  { id = "tests", kind = "list", title = "Test files in scope", step = "tests", columns = [
    { header = "Path", field = "path" },
  ]},
]
"#;

pub const FLAKY_CANDIDATES: &str = r#"
slug = "tests:flaky-candidates"
title = "Flaky candidates"
intent = "checking-tests"
description_md = """
Test files ranked by how often they get edited. A test nobody can leave
alone is worth a look — this is an attention signal about the FILE, and
it is not evidence that any test is flaky.
"""

params = [
  { name = "test_scope", type = "string", default = "path:spec//*||path:test//*", description = "kbc-scope/1 expression selecting the tests" },
  { name = "min_revisions", type = "int", default = 4, min = 1, max = 10000 },
  { name = "top", type = "int", default = 30, min = 1, max = 500 },
]

steps = [
  { id = "tests",   op = "tree",    title = "The test tree", args = { scope = "$p.test_scope", limit = 500 } },
  { id = "churn",   op = "churn",   title = "Churn counters", args = { min_revisions = "$p.min_revisions" } },
  { id = "churned", op = "set_ops", title = "Tests, with churn", args = { mode = "intersect", from = "$steps.tests", with = "$steps.churn" } },
  { id = "ranked",  op = "set_ops", title = "Most edited first", args = { mode = "sort", from = "$steps.churned", by = "revisions", desc = true } },
  { id = "top",     op = "set_ops", title = "The top slice", args = { mode = "limit", from = "$steps.ranked", n = "$p.top" } },
]

views = [
  { id = "top", kind = "table", title = "Most-edited tests", step = "top", columns = [
    { header = "Path", field = "path" },
    { header = "Revisions", field = { scalar = "revisions" } },
    { header = "Churn", field = { scalar = "churn" } },
  ]},
]
"#;

pub const RAILS_ORPHANS: &str = r#"
slug = "rails:orphans"
title = "Rails orphans"
intent = "rails"
description_md = """
One rails/1 orphan lane. Every row is `likely` or `candidate` — the
Rails lens has no `exact`, structurally — and the lane's own standing
caption applies: an empty lane is not a clean bill of health.
"""

params = [
  { name = "lane", type = "enum", default = "action_without_route", values = [
    "route_without_action",
    "action_without_route",
    "view_never_rendered",
    "model_referenced_only_from_its_own_file",
    "job_never_enqueued",
    "locale_key_never_referenced",
  ]},
  { name = "top", type = "int", default = 100, min = 1, max = 500 },
]

steps = [
  { id = "orphans", op = "rails", title = "The lane", args = { orphans = "$p.lane", limit = "$p.top" } },
]

views = [
  { id = "orphans", kind = "table", title = "Orphans", step = "orphans", columns = [
    { header = "Name", field = { scalar = "name" } },
    { header = "Path", field = "path" },
    { header = "Line", field = "line" },
    { header = "Trust", field = "trust" },
    { header = "Lane", field = { scalar = "orphan_lane" } },
  ]},
]
"#;

pub const AGED_TODOS: &str = r#"
slug = "hygiene:aged-todos"
title = "Aged and unreasoned annotations"
intent = "hygiene"
description_md = """
`TODO(on: date('…'))` annotations whose date has passed, plus suppression
directives that carry no reason. Both states are computed on this read
and stored nowhere — comments/1 keeps no `state` column on purpose.
"""

params = [
  { name = "top", type = "int", default = 100, min = 1, max = 500 },
]

steps = [
  { id = "aged",       op = "comments", title = "Past their date", args = { state = "aged", limit = "$p.top" } },
  { id = "unreasoned", op = "comments", title = "No reason given", args = { state = "unreasoned", limit = "$p.top" } },
  { id = "both",       op = "set_ops",  title = "Everything", args = { mode = "union", from = "$steps.aged", with = "$steps.unreasoned" } },
]

views = [
  { id = "both", kind = "table", title = "Annotations to revisit", step = "both", columns = [
    { header = "State", field = { scalar = "state" } },
    { header = "Path", field = "path" },
    { header = "Line", field = "line" },
    { header = "Age (days)", field = { scalar = "age_days" } },
    { header = "Text", field = { scalar = "text" } },
  ]},
  { id = "aged", kind = "list", title = "Aged", step = "aged", columns = [
    { header = "Where", field = "address" },
    { header = "Age", field = { scalar = "age_days" } },
  ]},
]
"#;

pub const DRIFTED_DOCS: &str = r#"
slug = "hygiene:drifted-docs"
title = "Drifted doc comments"
intent = "hygiene"
description_md = """
Doc comments whose body was last touched before the code they document.
Drift needs a blame per file, so this recipe runs under a hard file
budget and the census says how many files it reached — the rest report
`unknown`, never `fresh`.
"""

params = [
  { name = "top", type = "int", default = 50, min = 1, max = 500 },
]

steps = [
  { id = "drifted", op = "comments", title = "Doc comments behind their code", args = { state = "drifted", kind = "doc", limit = "$p.top" } },
  { id = "files",   op = "map",      title = "…the files they live in", args = { from = "$steps.drifted", to = "file-of" } },
]

views = [
  { id = "drifted", kind = "table", title = "Drifted docs", step = "drifted", columns = [
    { header = "Path", field = "path" },
    { header = "Line", field = "line" },
    { header = "Symbol", field = "symbol" },
    { header = "State", field = { scalar = "state" } },
    { header = "Text", field = { scalar = "text" } },
  ]},
  { id = "files", kind = "list", title = "Files to revisit", step = "files", columns = [
    { header = "Path", field = "path" },
  ]},
]
"#;

pub const DAG_BUILTINS: &[&str] = &[
    ENTRY_POINTS,
    HOT_AND_COLD,
    BLAST_RADIUS,
    UNTESTED_CHANGES,
    FLAKY_CANDIDATES,
    RAILS_ORPHANS,
    AGED_TODOS,
    DRIFTED_DOCS,
];

// ---------------------------------------------------------------------------
// Native adapters
// ---------------------------------------------------------------------------

/// The adapter document for one `recipes/1` native. Params mirror what
/// `crate::recipes::Recipe::params` declares, typed for the first time.
fn native_doc(slug: &str) -> RecipeDoc {
    let (title, intent, description, params) = match slug {
        "new-public-api" => (
            "New public API",
            "reviewing",
            "Public/exported definitions first blamed after `since`. Look here for newly \
             exposed API surface — not a stability verdict.",
            vec![since_param()],
        ),
        "god-functions" => (
            "God functions",
            "orienting",
            "Functions ranked by line span × (callers + callees). Attention only.",
            vec![],
        ),
        "agent-only-symbols" => (
            "Agent-only symbols",
            "reviewing",
            "Symbols whose defining span is entirely agent-attributed via the commit→session \
             join.",
            vec![],
        ),
        "complexity-climbers" => (
            "Complexity climbers",
            "reviewing",
            "Files whose loc+indent complexity rose between `since` and the working tree.",
            vec![since_param()],
        ),
        "unreviewed-hotspots" => (
            "Unreviewed hotspots",
            "reviewing",
            "Behavioral hotspots appearing in no local review's patchset file list.",
            vec![],
        ),
        "failure-tainted" => (
            "Failure-tainted paths",
            "hygiene",
            "Paths co-touched by sessions that carried tool-error evidence. Attention only — \
             not a defect prophecy.",
            vec![],
        ),
        other => unreachable!("unknown native {other}"),
    };
    RecipeDoc {
        slug: slug.to_string(),
        title: title.to_string(),
        intent: intent.to_string(),
        description_md: description.to_string(),
        params,
        scope: "$context.scope".to_string(),
        steps: Vec::new(),
        views: native_views(slug),
        native: Some(slug.to_string()),
    }
}

fn since_param() -> super::ParamSpec {
    super::ParamSpec {
        name: "since".into(),
        ty: super::ParamType::String,
        required: true,
        default: None,
        min: None,
        max: None,
        values: Vec::new(),
        description: "a git ref or an ISO date (YYYY-MM-DD)".into(),
    }
}

/// The columns each native's rows actually carry. This is the repair the
/// recon's finding 5 names: `first_seen`, `commits`, `revisions` and
/// `churn` were dropped by BOTH the SPA table and the CLI table, so the
/// one thing `new-public-api` exists to tell you was visible only in
/// `--json`.
fn native_views(slug: &str) -> Vec<super::ViewSpec> {
    use super::{ColField, ColumnSpec, ViewKind, ViewSpec};
    let col = |header: &str, field: ColField| ColumnSpec {
        header: header.to_string(),
        field,
    };
    let scalar = |n: &str| ColField::Scalar(n.to_string());
    let columns = match slug {
        "new-public-api" => vec![
            col("Symbol", ColField::Symbol),
            col("Kind", scalar("kind")),
            col("Path", ColField::Path),
            col("Line", ColField::Line),
            col("First seen", ColField::Commit),
            col("At (unix)", scalar("first_seen_unix")),
            col("Trust", ColField::Trust),
        ],
        "god-functions" => vec![
            col("Symbol", ColField::Symbol),
            col("Path", ColField::Path),
            col("Score", scalar("score")),
            col("Span", scalar("line_span")),
            col("Fan-in", scalar("fan_in")),
            col("Fan-out", scalar("fan_out")),
            col("Trust", ColField::Trust),
        ],
        "agent-only-symbols" => vec![
            col("Symbol", ColField::Symbol),
            col("Path", ColField::Path),
            col("Line", ColField::Line),
            col("Commits", scalar("commits")),
            col("Trust", ColField::Trust),
        ],
        "complexity-climbers" => vec![
            col("Path", ColField::Path),
            col("Delta", scalar("delta")),
            col("Baseline", scalar("then_state")),
            col("loc then", scalar("loc_then")),
            col("loc now", scalar("loc_now")),
        ],
        "unreviewed-hotspots" => vec![
            col("Path", ColField::Path),
            col("Score", scalar("score")),
            col("Revisions", scalar("revisions")),
            col("Churn", scalar("churn")),
        ],
        "failure-tainted" => vec![
            col("Path", ColField::Path),
            col("Score", scalar("score")),
            col("Errors", scalar("error_count")),
            col("Sessions", scalar("sessions")),
        ],
        _ => vec![col("Address", ColField::Address)],
    };
    vec![ViewSpec {
        id: "rows".into(),
        kind: ViewKind::Table,
        title: String::new(),
        step: "native".into(),
        columns,
    }]
}

/// Run one native body through `recipes/1` and adapt its rows.
pub fn run_native(
    slug: &str,
    ctx: &RunCtx<'_>,
    params: &BTreeMap<String, ArgVal>,
    limit: usize,
) -> Result<StepRun, String> {
    use std::str::FromStr;
    let recipe = crate::recipes::Recipe::from_str(slug)?;
    let since = params
        .get("since")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let out = crate::recipes::run_recipe(
        recipe,
        ctx.store,
        ctx.blame_cache,
        ctx.repo_root,
        ctx.repo_name,
        ctx.repo_id,
        since.as_deref(),
        // The native body caps itself; the RUNNER's own cap is applied
        // below, after scoping, so a scoped run is not silently a
        // top-N-then-filter.
        crate::recipes::MAX_LIMIT,
        None,
    )
    .map_err(|e| e.message().to_string())?;

    let mut census = StepCensus::new();
    census.input("native_rows", out.total as i64);
    for m in &out.inputs_missing {
        census.filter(format!("MISSING INPUT: {m}"));
    }
    if let Some(note) = &out.note {
        census.note(note.clone());
    }
    let mut rows: Vec<super::Addr> = out
        .items
        .iter()
        .map(|it| addr_from_legacy_item(ctx.repo_name, it))
        .collect();
    let before = rows.len();
    rows.retain(|a| a.path.as_deref().is_none_or(|p| ctx.in_scope(p)));
    if rows.len() < before {
        census.input("scope_excluded", (before - rows.len()) as i64);
        census.filter("kbc-scope/1 path set");
    }
    let total = rows.len();
    rows.truncate(limit);
    if total == 0 {
        census.empty_reason = Some(if out.inputs_missing.is_empty() {
            super::census::EmptyReason::FilteredOut
        } else {
            super::census::EmptyReason::NoIndex
        });
    }
    Ok(StepRun {
        id: "native".into(),
        op: None,
        engine: Some(crate::recipes::SCHEMA.to_string()),
        title: String::new(),
        truncated: rows.len() < total,
        total,
        rows,
        ms: 0,
        census,
    })
}

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

/// Every compiled-in recipe, loaded and type-checked. Panics on a broken
/// builtin — that is a build-time bug in THIS file, not a runtime input,
/// and `tests::every_builtin_loads` catches it in CI first.
pub fn all() -> Vec<LoadedRecipe> {
    let mut out: Vec<LoadedRecipe> = DAG_BUILTINS
        .iter()
        .map(|t| {
            let doc = super::load_toml(t)
                .unwrap_or_else(|e| panic!("built-in recipe failed to load: {e}"));
            loaded(doc)
        })
        .collect();
    for slug in NATIVE_SLUGS {
        let doc =
            super::load(native_doc(slug)).unwrap_or_else(|e| panic!("native adapter {slug}: {e}"));
        out.push(loaded(doc));
    }
    out.sort_by(|a, b| a.doc.slug.cmp(&b.doc.slug));
    out
}

fn loaded(doc: RecipeDoc) -> LoadedRecipe {
    LoadedRecipe {
        doc,
        home: Home::Builtin,
        source: "builtin".into(),
        trust: TrustState::Trusted,
        trust_diff: None,
        shadowed_by: None,
    }
}
