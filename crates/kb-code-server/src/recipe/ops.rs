//! The CLOSED op set (V74-L3a, D11 + D21).
//!
//! Every step of a recipe names one [`Op`]. There is no variant that runs
//! a process, no variant that takes a shell string, and — the rule D21
//! states outright — **no variant that names an exec lane**. That is not
//! a check somebody remembered to write: `Op` is a Rust enum, so an
//! author cannot spell one, a repo file cannot deserialize into one, and
//! `ops::tests::the_op_set_cannot_reach_an_exec_lane` walks the whole
//! variant list to keep it that way as the set grows.
//!
//! Each variant declares the arg names it accepts, which of them are
//! required, and the address kind it produces ([`Op::output_kind`]) — so
//! a DAG is type-checked at LOAD time and a mis-wired step fails by NAME
//! rather than at run time with an empty table nobody can explain.
//!
//! ## What an op may and may not do
//!
//! An op is a THIN adapter over an engine this crate already ships. It
//! copies that engine's own trust class verbatim (`usages/2`'s `trust`,
//! `entities::class_for`, `rails::noun_trust`, `lanes::classing`) and has
//! no code path that raises one — invariants 13/20/21/22's posture,
//! applied to a layer whose whole job is pointing at other layers. Where
//! an engine reports no class the address says
//! [`super::TRUST_UNKNOWN`], and where it reports no blob the address
//! says [`super::BLOB_UNKNOWN`]; neither ever reads as a zero.

use super::census::{EmptyReason, StepCensus};
use super::{Addr, AddrKind, ArgVal, ScalarVal, BLOB_UNKNOWN, MAX_STEP_ROWS};
use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

/// The closed op set. Kebab-case on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// kbcq/1 (`search::grammar` + the files/symbols/text lanes).
    Search,
    /// usages/2 (`usages2::usages2_at`) from a symbol address.
    Usages,
    /// entity/1 (`entity_defs` + `entities::class_for`).
    Entity,
    /// The per-file symbol list the outline is a view of.
    Outline,
    /// comments/1 (`comments::drift`'s states, computed here as there).
    Comments,
    /// aug-lane/1 facts (`lane_facts`, gated by `[lanes]`).
    Facts,
    /// rails/1 noun lists and orphan lanes.
    Rails,
    /// kbc-tree/1's path set under a kbc-scope/1 expression.
    Tree,
    /// The repo-wide commit walk (`behavioral::walk_commits_capped`).
    GitLog,
    /// `git blame` regions for the input addresses' files.
    Blame,
    /// Behavioral `path_stats` churn counters.
    Churn,
    /// kbc-canvas/1 board nodes (read).
    Boards,
    /// kbc-review/1 findings (and the finding-shaped timeline events).
    Review,
    /// Pure algebra over earlier steps' address sets.
    SetOps,
    /// Address → address, over a fixed set of transforms.
    Map,
}

/// What an op accepts and produces. `output` is `None` for the two ops
/// whose kind depends on their args or inputs (`search`, `set_ops`,
/// `map`, `rails`) — [`Op::output_kind`] decides those, and the decision
/// runs at load.
#[derive(Debug, Clone, Copy)]
pub struct OpSpec {
    pub args: &'static [&'static str],
    pub required_args: &'static [&'static str],
    /// The kinds a `$steps.<id>` arg may carry into this op.
    pub accepts: &'static [AddrKind],
    pub output: Option<AddrKind>,
    pub summary: &'static str,
}

impl Op {
    pub const ALL: &'static [Op] = &[
        Op::Search,
        Op::Usages,
        Op::Entity,
        Op::Outline,
        Op::Comments,
        Op::Facts,
        Op::Rails,
        Op::Tree,
        Op::GitLog,
        Op::Blame,
        Op::Churn,
        Op::Boards,
        Op::Review,
        Op::SetOps,
        Op::Map,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Op::Search => "search",
            Op::Usages => "usages",
            Op::Entity => "entity",
            Op::Outline => "outline",
            Op::Comments => "comments",
            Op::Facts => "facts",
            Op::Rails => "rails",
            Op::Tree => "tree",
            Op::GitLog => "git_log",
            Op::Blame => "blame",
            Op::Churn => "churn",
            Op::Boards => "boards",
            Op::Review => "review",
            Op::SetOps => "set_ops",
            Op::Map => "map",
        }
    }

    pub fn spec(self) -> OpSpec {
        match self {
            Op::Search => OpSpec {
                args: &["q", "lane", "limit"],
                required_args: &["q"],
                accepts: &[],
                output: None,
                summary: "kbcq/1 search; `lane` picks files (default), symbols or text",
            },
            Op::Usages => OpSpec {
                args: &["from", "limit", "trust"],
                required_args: &["from"],
                accepts: &[AddrKind::Symbol],
                output: Some(AddrKind::Line),
                summary: "usages/2 for each input symbol; each row keeps the engine's own trust",
            },
            Op::Entity => OpSpec {
                args: &["from", "name", "kind", "limit"],
                required_args: &[],
                accepts: &[AddrKind::File],
                output: Some(AddrKind::Entity),
                summary: "entity/1 definitions, optionally narrowed to the input files",
            },
            Op::Outline => OpSpec {
                args: &["from", "kinds", "limit"],
                required_args: &["from"],
                accepts: &[AddrKind::File],
                output: Some(AddrKind::Symbol),
                summary: "the symbols of each input file (the rows /api/outline is a view of)",
            },
            Op::Comments => OpSpec {
                args: &["from", "kind", "keyword", "state", "limit"],
                required_args: &[],
                accepts: &[AddrKind::File],
                output: Some(AddrKind::Comment),
                summary: "comments/1 rows; `state` is computed per request, never stored",
            },
            Op::Facts => OpSpec {
                args: &["lane", "from", "kind", "min_severity", "limit"],
                required_args: &["lane"],
                accepts: &[AddrKind::File],
                output: Some(AddrKind::Fact),
                summary: "aug-lane/1 facts for the input files; a disabled lane says so",
            },
            Op::Rails => OpSpec {
                args: &["noun", "orphans", "limit"],
                required_args: &[],
                accepts: &[],
                output: None,
                summary: "rails/1 — one noun list, or one orphan lane",
            },
            Op::Tree => OpSpec {
                args: &["scope", "ext", "limit"],
                required_args: &[],
                accepts: &[],
                output: Some(AddrKind::File),
                summary: "the mirror's file set under a kbc-scope/1 expression",
            },
            Op::GitLog => OpSpec {
                args: &["since", "range", "emit", "limit"],
                required_args: &[],
                accepts: &[],
                output: None,
                summary: "the repo's commit walk; `emit` picks commits (default) or the \
                          files they touched",
            },
            Op::Blame => OpSpec {
                args: &["from", "limit"],
                required_args: &["from"],
                accepts: &[AddrKind::File, AddrKind::Symbol, AddrKind::Line],
                output: Some(AddrKind::Line),
                summary: "blame regions covering the input addresses",
            },
            Op::Churn => OpSpec {
                args: &["min_revisions", "limit"],
                required_args: &[],
                accepts: &[],
                output: Some(AddrKind::File),
                summary: "behavioral path_stats churn counters",
            },
            Op::Boards => OpSpec {
                args: &["status", "limit"],
                required_args: &[],
                accepts: &[],
                output: Some(AddrKind::Node),
                summary: "kbc-canvas/1 board nodes, re-resolved on this read",
            },
            Op::Review => OpSpec {
                args: &["kind", "state", "severity", "disposition", "limit"],
                required_args: &["kind"],
                accepts: &[],
                output: Some(AddrKind::Finding),
                summary: "kbc-review/1 findings (`kind=findings`) or their timeline events",
            },
            Op::SetOps => OpSpec {
                args: &[
                    "mode",
                    "from",
                    "with",
                    "by",
                    "desc",
                    "n",
                    "trust",
                    "path_prefix",
                    "min",
                    "max",
                ],
                required_args: &["mode", "from"],
                accepts: AddrKind::ALL,
                output: None,
                summary: "union | intersect | diff | filter | sort | limit over earlier steps",
            },
            Op::Map => OpSpec {
                args: &["from", "to", "limit"],
                required_args: &["from", "to"],
                accepts: AddrKind::ALL,
                output: None,
                summary: "file-of | symbol-of | entity-of | definitions-of",
            },
        }
    }

    /// The load-time type-check. `input_kinds` pairs an arg name with the
    /// output kind of the step it references.
    pub fn output_kind(
        self,
        args: &BTreeMap<String, ArgVal>,
        input_kinds: &[(String, AddrKind)],
    ) -> Result<AddrKind, String> {
        let spec = self.spec();
        // Every step input must be a kind this op accepts. This is the
        // whole point of the exercise: a `usages` fed a commit fails HERE,
        // with the step named, instead of returning nothing at run time.
        for (arg, kind) in input_kinds {
            if !spec.accepts.contains(kind) {
                return Err(format!(
                    "arg {arg:?} carries {} addresses, but op `{}` accepts {}",
                    kind.as_str(),
                    self.as_str(),
                    spec.accepts
                        .iter()
                        .map(|k| k.as_str())
                        .collect::<Vec<_>>()
                        .join(" | ")
                ));
            }
        }
        if let Some(out) = spec.output {
            return Ok(out);
        }
        match self {
            Op::Search => match kind_deciding_literal(args, "lane", self)?.unwrap_or("files") {
                "files" => Ok(AddrKind::File),
                "symbols" => Ok(AddrKind::Symbol),
                "text" => Ok(AddrKind::Line),
                other => Err(format!(
                    "unknown `lane` {other:?}; known: files | symbols | text"
                )),
            },
            Op::GitLog => match kind_deciding_literal(args, "emit", self)?.unwrap_or("commits") {
                "commits" => Ok(AddrKind::Commit),
                "files" => Ok(AddrKind::File),
                other => Err(format!("unknown `emit` {other:?}; known: commits | files")),
            },
            Op::Rails => {
                // Which KEY is present decides the kind; the VALUE does
                // not, so it may be a `$p.<name>` and its vocabulary is
                // checked at RUN time (`exec_rails`, with its own census
                // reason). `rails:orphans`' whole point is an enum param.
                let has_noun = args.contains_key("noun");
                let has_orphans = args.contains_key("orphans");
                match (has_noun, has_orphans) {
                    (true, false) => {
                        if let Some(n) = deferred_literal(args, "noun") {
                            if !crate::rails::NOUNS.contains(&n) {
                                return Err(format!(
                                    "unknown rails noun {n:?}; known: {}",
                                    crate::rails::NOUNS.join(", ")
                                ));
                            }
                        }
                        Ok(AddrKind::Entity)
                    }
                    (false, true) => {
                        if let Some(lane) = deferred_literal(args, "orphans") {
                            if !ORPHAN_LANES.contains(&lane) {
                                return Err(format!(
                                    "unknown orphan lane {lane:?}; known: {}",
                                    ORPHAN_LANES.join(", ")
                                ));
                            }
                        }
                        Ok(AddrKind::File)
                    }
                    _ => Err("op `rails` needs exactly one of `noun` or `orphans`".into()),
                }
            }
            Op::SetOps => {
                let mode =
                    kind_deciding_literal(args, "mode", self)?.ok_or("`mode` must be a string")?;
                if !SET_MODES.contains(&mode) {
                    return Err(format!(
                        "unknown set_ops mode {mode:?}; known: {}",
                        SET_MODES.join(" | ")
                    ));
                }
                let binary = matches!(mode, "union" | "intersect" | "diff");
                if binary && !args.contains_key("with") {
                    return Err(format!("set_ops mode {mode:?} needs a second set (`with`)"));
                }
                if !binary && args.contains_key("with") {
                    return Err(format!(
                        "set_ops mode {mode:?} takes one set; `with` is only for union/intersect/diff"
                    ));
                }
                if mode == "sort" && !args.contains_key("by") {
                    return Err("set_ops mode \"sort\" needs `by` (a scalar name)".into());
                }
                if mode == "limit" && !args.contains_key("n") {
                    return Err("set_ops mode \"limit\" needs `n`".into());
                }
                // Every input must agree on one kind — a union of files
                // and commits is not a set, it is a category error.
                let mut kinds: Vec<AddrKind> = input_kinds.iter().map(|(_, k)| *k).collect();
                kinds.dedup();
                match kinds.len() {
                    0 => Err("set_ops needs at least one `$steps.<id>` input".into()),
                    1 => Ok(kinds[0]),
                    _ => Err(format!(
                        "set_ops inputs disagree on kind ({}); one set has one kind",
                        input_kinds
                            .iter()
                            .map(|(a, k)| format!("{a}:{}", k.as_str()))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )),
                }
            }
            Op::Map => {
                let to = kind_deciding_literal(args, "to", self)?.ok_or("`to` must be a string")?;
                let from_kind = input_kinds
                    .iter()
                    .find(|(a, _)| a == "from")
                    .map(|(_, k)| *k)
                    .ok_or("`from` must reference a step")?;
                map_output_kind(to, from_kind)
            }
            _ => unreachable!("every fixed-output op returned above"),
        }
    }
}

/// An arg whose VALUE decides a step's output kind must be a LITERAL: the
/// DAG is type-checked at LOAD, and a `$p.<name>` there would make the
/// step's own kind depend on a value nobody has supplied yet. The refusal
/// says so rather than leaving an author to discover it as a mysterious
/// "unknown lane `$p.x`".
fn kind_deciding_literal<'a>(
    args: &'a BTreeMap<String, ArgVal>,
    key: &str,
    op: Op,
) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None => Ok(None),
        Some(v) => match v.as_str() {
            Some(s) if s.starts_with('$') => Err(format!(
                "arg {key:?} decides what `{}` PRODUCES, so it must be a literal — a \
                 reference there could not be type-checked at load. Split the recipe into \
                 one step per {key}.",
                op.as_str()
            )),
            Some(s) => Ok(Some(s)),
            None => Err(format!("arg {key:?} must be a string")),
        },
    }
}

/// A closed-vocabulary arg whose value does NOT decide the output kind:
/// a literal is checked here, a reference is checked at RUN time by the
/// op itself (which refuses with its own census reason).
fn deferred_literal<'a>(args: &'a BTreeMap<String, ArgVal>, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.starts_with('$'))
}

/// The `rails/1` orphan lane ids a recipe may name. Mirrors
/// `rails::orphans`' own lane list; a name outside it is a LOAD error.
pub const ORPHAN_LANES: &[&str] = &[
    "route_without_action",
    "action_without_route",
    "view_never_rendered",
    "model_referenced_only_from_its_own_file",
    "job_never_enqueued",
    "locale_key_never_referenced",
];

pub const SET_MODES: &[&str] = &["union", "intersect", "diff", "filter", "sort", "limit"];

/// The closed transform set. Deliberately small: a transform that cannot
/// be stated as "this address, seen as that kind of address" belongs in
/// an op with an engine behind it, not here.
pub const MAP_TRANSFORMS: &[&str] = &["file-of", "symbol-of", "entity-of", "definitions-of"];

fn map_output_kind(to: &str, from: AddrKind) -> Result<AddrKind, String> {
    match to {
        "file-of" => Ok(AddrKind::File),
        "symbol-of" => match from {
            AddrKind::Line | AddrKind::Comment | AddrKind::Fact => Ok(AddrKind::Symbol),
            other => Err(format!(
                "`symbol-of` needs a line-shaped address (line | comment | fact), not {}",
                other.as_str()
            )),
        },
        "entity-of" => match from {
            AddrKind::File | AddrKind::Symbol => Ok(AddrKind::Entity),
            other => Err(format!(
                "`entity-of` needs a file or symbol address, not {}",
                other.as_str()
            )),
        },
        "definitions-of" => match from {
            AddrKind::Entity | AddrKind::Symbol => Ok(AddrKind::Symbol),
            other => Err(format!(
                "`definitions-of` needs an entity or symbol address, not {}",
                other.as_str()
            )),
        },
        other => Err(format!(
            "unknown map transform {other:?}; known: {}",
            MAP_TRANSFORMS.join(" | ")
        )),
    }
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// Everything an executor may touch. Assembled ONCE per run inside a
/// single `run_blocking` hop (the 2026-08-31 starvation lesson: one
/// coarse blocking hop for the whole ladder, never one per engine).
pub struct RunCtx<'a> {
    pub store: &'a Store,
    pub repo: &'a crate::config::RepoEntry,
    pub repo_id: i64,
    pub repo_name: &'a str,
    pub repo_root: &'a Path,
    pub scopes: &'a crate::config::ScopesSection,
    pub lanes: &'a crate::config::LanesSection,
    pub factors: crate::search::Factors,
    pub file_index: &'a crate::search::files::FileIndex,
    pub symbol_index: &'a crate::search::symbols::SymbolIndex,
    pub blame_cache: &'a crate::blame::BlameCache,
    /// The resolved kbc-scope/1 path set. `None` = the run is unscoped
    /// (either nothing was asked, or the scope REFUSED and the reason is
    /// captioned on the response — never a silent narrowing).
    pub scope_paths: Option<&'a HashSet<String>>,
    pub today: chrono::NaiveDate,
    pub now_unix: i64,
    /// Wall-clock deadline for the whole run.
    pub deadline: std::time::Instant,
}

impl RunCtx<'_> {
    pub fn in_scope(&self, path: &str) -> bool {
        match self.scope_paths {
            None => true,
            Some(set) => set.contains(path),
        }
    }
    pub fn out_of_budget(&self) -> bool {
        std::time::Instant::now() >= self.deadline
    }
}

/// One resolved argument: a literal the author wrote, or the address set
/// an earlier step produced.
#[derive(Debug, Clone)]
pub enum Resolved {
    Val(ArgVal),
    Set(Vec<Addr>),
}

impl Resolved {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Resolved::Val(v) => v.as_str(),
            Resolved::Set(_) => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Resolved::Val(v) => v.as_i64(),
            Resolved::Set(_) => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Resolved::Val(v) => v.as_bool(),
            Resolved::Set(_) => None,
        }
    }
    pub fn as_set(&self) -> Option<&[Addr]> {
        match self {
            Resolved::Set(s) => Some(s),
            Resolved::Val(_) => None,
        }
    }
}

pub type Args = BTreeMap<String, Resolved>;

/// What one step produced, plus the honest account of how.
pub struct StepOutput {
    pub rows: Vec<Addr>,
    /// The TRUE size before the row cap. `rows.len() < total` ⇒ truncated.
    pub total: usize,
    pub census: StepCensus,
}

impl StepOutput {
    fn finish(mut rows: Vec<Addr>, limit: usize, mut census: StepCensus) -> Self {
        // ONE total order for every op: score desc (when the op derived
        // one), then the address itself. Without the address tie-break a
        // run is only as deterministic as sqlite's row order, which is
        // not a promise.
        rows.sort_by(|a, b| {
            let sa = a.scalars.get("score").and_then(|s| s.as_f64());
            let sb = b.scalars.get("score").and_then(|s| s.as_f64());
            match (sa, sb) {
                (Some(x), Some(y)) => y
                    .partial_cmp(&x)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.key().cmp(&b.key())),
                _ => a.key().cmp(&b.key()),
            }
        });
        let total = rows.len();
        rows.truncate(limit);
        if total == 0 && census.empty_reason.is_none() {
            census.empty_reason = Some(EmptyReason::FilteredOut);
        }
        StepOutput {
            rows,
            total,
            census,
        }
    }
}

fn limit_of(args: &Args, default: usize) -> usize {
    args.get("limit")
        .and_then(|v| v.as_i64())
        .filter(|n| *n > 0)
        .map(|n| (n as usize).min(MAX_STEP_ROWS))
        .unwrap_or(default)
}

fn csv(args: &Args, key: &str) -> Vec<String> {
    match args.get(key) {
        Some(Resolved::Val(ArgVal::List(v))) => v.clone(),
        Some(Resolved::Val(ArgVal::Str(s))) => s
            .split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// Run one step. Errors are reserved for a REFUSAL the caller must
/// surface (a bad param that slipped past validation, a git failure); an
/// EMPTY result is never an error — it is a census.
pub fn execute(op: Op, ctx: &RunCtx<'_>, args: &Args) -> Result<StepOutput, String> {
    if ctx.out_of_budget() {
        return Ok(StepOutput {
            rows: Vec::new(),
            total: 0,
            census: StepCensus::empty(EmptyReason::BudgetExhausted),
        });
    }
    match op {
        Op::Search => exec_search(ctx, args),
        Op::Usages => exec_usages(ctx, args),
        Op::Entity => exec_entity(ctx, args),
        Op::Outline => exec_outline(ctx, args),
        Op::Comments => exec_comments(ctx, args),
        Op::Facts => exec_facts(ctx, args),
        Op::Rails => exec_rails(ctx, args),
        Op::Tree => exec_tree(ctx, args),
        Op::GitLog => exec_git_log(ctx, args),
        Op::Blame => exec_blame(ctx, args),
        Op::Churn => exec_churn(ctx, args),
        Op::Boards => exec_boards(ctx, args),
        Op::Review => exec_review(ctx, args),
        Op::SetOps => exec_set_ops(ctx, args),
        Op::Map => exec_map(ctx, args),
    }
}

// --- search ---------------------------------------------------------------

fn exec_search(ctx: &RunCtx<'_>, args: &Args) -> Result<StepOutput, String> {
    let q = args.get("q").and_then(|v| v.as_str()).unwrap_or("");
    let lane = args.get("lane").and_then(|v| v.as_str()).unwrap_or("files");
    let limit = limit_of(args, super::DEFAULT_STEP_ROWS);
    let mut census = StepCensus::new();
    census.input("query_len", q.len() as i64);
    if q.trim().is_empty() {
        return Ok(StepOutput {
            rows: Vec::new(),
            total: 0,
            census: StepCensus::empty(EmptyReason::ParamEmpty).with_filter("q is empty"),
        });
    }
    let parsed = crate::search::grammar::parse(q);
    for d in &parsed.diagnostics {
        census.note(format!("kbcq/1: {} ({})", d.message, d.token));
    }
    let repos = vec![(ctx.repo_name.to_string(), ctx.repo_id)];
    let opts = crate::search::LaneOpts {
        path_filter: parsed.filters.path.as_deref(),
        factors: ctx.factors,
        explain: false,
        candidate_paths: None,
    };
    let mut rows = Vec::new();
    match lane {
        "files" => {
            let hits = ctx
                .file_index
                .search(
                    ctx.store,
                    &repos,
                    &parsed.query,
                    MAX_STEP_ROWS,
                    ctx.now_unix * 1000,
                    &opts,
                )
                .map_err(|e| e.to_string())?;
            census.input("hits", hits.len() as i64);
            for h in hits {
                if !ctx.in_scope(&h.path) {
                    continue;
                }
                rows.push(
                    Addr::new(AddrKind::File, &h.repo)
                        .with_path(h.path)
                        .with_blob(h.blob_sha.as_deref())
                        .scalar("score", h.score),
                );
            }
        }
        "symbols" => {
            let hits = ctx
                .symbol_index
                .search(ctx.store, &repos, &parsed.query, MAX_STEP_ROWS, &opts)
                .map_err(|e| e.to_string())?;
            census.input("hits", hits.len() as i64);
            for h in hits {
                if !ctx.in_scope(&h.path) {
                    continue;
                }
                rows.push(
                    Addr::new(AddrKind::Symbol, &h.repo)
                        .with_path(h.path)
                        .with_line(h.symbol.line_start)
                        .with_symbol(h.symbol.name.clone())
                        .scalar("kind", h.symbol.kind.clone())
                        .scalar("score", h.score as i64),
                );
            }
        }
        "text" => {
            let res = crate::search::text::search_text(
                ctx.store,
                ctx.repo_root,
                ctx.repo_id,
                &parsed.query,
                false,
                parsed.filters.case.unwrap_or(false),
                std::time::Duration::from_secs(5),
                &opts,
            )
            .map_err(|e| e.to_string())?;
            let mut n = 0i64;
            for f in res.results {
                if !ctx.in_scope(&f.path) {
                    continue;
                }
                for m in f.matches {
                    n += 1;
                    rows.push(
                        Addr::new(AddrKind::Line, ctx.repo_name)
                            .with_path(f.path.clone())
                            .with_line(m.line_no as u32)
                            .scalar("text", m.line.trim().to_string()),
                    );
                }
            }
            census.input("matches", n);
        }
        other => return Err(format!("unknown search lane {other:?}")),
    }
    if ctx.scope_paths.is_some() {
        census.filter("kbc-scope/1 path set");
    }
    Ok(StepOutput::finish(rows, limit, census))
}

// --- usages ---------------------------------------------------------------

fn exec_usages(ctx: &RunCtx<'_>, args: &Args) -> Result<StepOutput, String> {
    let limit = limit_of(args, super::DEFAULT_STEP_ROWS);
    let want_trust = csv(args, "trust");
    let Some(input) = args.get("from").and_then(|v| v.as_set()) else {
        return Err("`from` must reference a step".into());
    };
    let mut census = StepCensus::new();
    census.input("symbols", input.len() as i64);
    if input.is_empty() {
        return Ok(StepOutput {
            rows: Vec::new(),
            total: 0,
            census: StepCensus::empty(EmptyReason::UpstreamEmpty),
        });
    }
    let mut rows = Vec::new();
    let mut unanchored = 0i64;
    for a in input {
        if ctx.out_of_budget() {
            census.note("stopped at the run budget; the input set was not exhausted");
            break;
        }
        let (Some(path), Some(line), Some(name)) = (a.path.as_deref(), a.line, a.symbol.as_deref())
        else {
            unanchored += 1;
            continue;
        };
        // usages/2 addresses a CURSOR, so a symbol row needs a column.
        // A line that does not spell the name is an honest skip, never a
        // fabricated column (`dossier::usages_for_entity`'s own rule).
        let Some(col) = column_of(ctx.repo_root, path, line, name) else {
            unanchored += 1;
            continue;
        };
        let out = match crate::usages2::usages2_at(
            ctx.store,
            ctx.repo,
            ctx.repo_id,
            path,
            line,
            col,
            None,
            ctx.scopes,
            MAX_STEP_ROWS,
        ) {
            Ok(o) => o,
            Err(_) => {
                unanchored += 1;
                continue;
            }
        };
        for group in [&out.exact, &out.likely, &out.candidate] {
            for u in group {
                if !want_trust.is_empty() && !want_trust.iter().any(|t| t == u.trust) {
                    continue;
                }
                if !ctx.in_scope(&u.path) {
                    continue;
                }
                rows.push(
                    Addr::new(AddrKind::Line, ctx.repo_name)
                        .with_path(u.path.clone())
                        .with_line(u.line)
                        .with_symbol(name.to_string())
                        .with_blob(u.blob_sha.as_deref())
                        .with_trust(Some(u.trust))
                        .scalar("usage_kind", u.kind.as_str())
                        .scalar("text", u.context.clone()),
                );
            }
        }
    }
    if unanchored > 0 {
        census.input("unanchored_symbols", unanchored);
        census.note(format!(
            "{unanchored} input symbol(s) could not be anchored to a column on their own line \
             and were skipped rather than guessed"
        ));
    }
    if !want_trust.is_empty() {
        census.filter(format!("trust in [{}]", want_trust.join(", ")));
    }
    Ok(StepOutput::finish(rows, limit, census))
}

/// The byte column of `name` on 1-based `line` of `path`, or `None`.
/// Reads the WORKING TREE, which is what `usages/2` itself resolves
/// against.
fn column_of(repo_root: &Path, path: &str, line: u32, name: &str) -> Option<u32> {
    let text = std::fs::read_to_string(repo_root.join(path)).ok()?;
    let l = text.lines().nth(line.checked_sub(1)? as usize)?;
    let idx = l.find(name)?;
    Some(idx as u32)
}

// --- entity ---------------------------------------------------------------

fn exec_entity(ctx: &RunCtx<'_>, args: &Args) -> Result<StepOutput, String> {
    let limit = limit_of(args, super::DEFAULT_STEP_ROWS);
    let want_kind = args.get("kind").and_then(|v| v.as_str());
    let want_name = args.get("name").and_then(|v| v.as_str());
    let paths: Option<HashSet<String>> = args
        .get("from")
        .and_then(|v| v.as_set())
        .map(|s| s.iter().filter_map(|a| a.path.clone()).collect());
    let mut census = StepCensus::new();
    if let Some(p) = &paths {
        census.input("input_files", p.len() as i64);
        if p.is_empty() {
            return Ok(StepOutput {
                rows: Vec::new(),
                total: 0,
                census: StepCensus::empty(EmptyReason::UpstreamEmpty),
            });
        }
    }
    let defs = ctx
        .store
        .entity_defs_for_repo(ctx.repo_id, None, 200_000)
        .map_err(|e| e.to_string())?;
    census.input("entity_defs", defs.len() as i64);
    if defs.is_empty() {
        return Ok(StepOutput {
            rows: Vec::new(),
            total: 0,
            census: StepCensus::empty(EmptyReason::NoIndex)
                .with_filter("entity_defs is empty — this repo has no indexed entities"),
        });
    }
    let mut rows = Vec::new();
    for d in defs {
        if let Some(p) = &paths {
            if !p.contains(&d.path) {
                continue;
            }
        }
        if !ctx.in_scope(&d.path) {
            continue;
        }
        if let Some(k) = want_kind {
            if d.kind != k {
                continue;
            }
        }
        if let Some(n) = want_name {
            if !d.fqn.contains(n) {
                continue;
            }
        }
        let stale = d.live_blob_hash.as_deref() != Some(d.blob_hash.as_str());
        // BORROWED, never minted here — invariant 13's one classing fn.
        let class = crate::entities::class_for(
            crate::entities::MATCHED_VIA_NESTING,
            &d.nesting,
            &d.zeitwerk_state,
            stale,
        );
        rows.push(
            Addr::new(AddrKind::Entity, ctx.repo_name)
                .with_entity(d.fqn.clone())
                .with_path(d.path.clone())
                .with_line(d.line_start.max(1) as u32)
                .with_blob(Some(d.blob_hash.as_str()))
                .with_trust(Some(class))
                .scalar("kind", d.kind.clone())
                .scalar("stale", stale)
                .scalar("line_span", (d.line_end - d.line_start + 1).max(0)),
        );
    }
    if want_kind.is_some() {
        census.filter("kind");
    }
    if want_name.is_some() {
        census.filter("name substring");
    }
    Ok(StepOutput::finish(rows, limit, census))
}

// --- outline --------------------------------------------------------------

fn exec_outline(ctx: &RunCtx<'_>, args: &Args) -> Result<StepOutput, String> {
    let limit = limit_of(args, super::DEFAULT_STEP_ROWS);
    let kinds = csv(args, "kinds");
    let Some(input) = args.get("from").and_then(|v| v.as_set()) else {
        return Err("`from` must reference a step".into());
    };
    let want: HashSet<&str> = input.iter().filter_map(|a| a.path.as_deref()).collect();
    let mut census = StepCensus::new();
    census.input("input_files", want.len() as i64);
    if want.is_empty() {
        return Ok(StepOutput {
            rows: Vec::new(),
            total: 0,
            census: StepCensus::empty(EmptyReason::UpstreamEmpty),
        });
    }
    let symbols = ctx
        .store
        .symbols_for_repo(ctx.repo_id)
        .map_err(|e| e.to_string())?;
    census.input("repo_symbols", symbols.len() as i64);
    let mut rows = Vec::new();
    for (path, sym) in symbols {
        if !want.contains(path.as_str()) {
            continue;
        }
        if !kinds.is_empty() && !kinds.iter().any(|k| k == &sym.kind) {
            continue;
        }
        rows.push(
            Addr::new(AddrKind::Symbol, ctx.repo_name)
                .with_path(path)
                .with_line(sym.line_start)
                .with_symbol(sym.name.clone())
                .scalar("kind", sym.kind.clone())
                .scalar(
                    "line_span",
                    sym.line_end
                        .saturating_sub(sym.line_start)
                        .saturating_add(1),
                ),
        );
    }
    if !kinds.is_empty() {
        census.filter(format!("kind in [{}]", kinds.join(", ")));
    }
    Ok(StepOutput::finish(rows, limit, census))
}

// --- comments -------------------------------------------------------------

/// Files blamed for one `comments` step. The route's own budget
/// (`comments::routes::MAX_BLAMED_FILES`) restated here for the same
/// reason it exists there: `drifted` costs a blame per file, and a recipe
/// step must degrade into a captioned partial rather than a stall.
pub const MAX_BLAMED_FILES: usize = 20;

fn exec_comments(ctx: &RunCtx<'_>, args: &Args) -> Result<StepOutput, String> {
    let limit = limit_of(args, super::DEFAULT_STEP_ROWS);
    let kind = args.get("kind").and_then(|v| v.as_str());
    let keyword = args.get("keyword").and_then(|v| v.as_str());
    let want_state = args.get("state").and_then(|v| v.as_str());
    if let Some(s) = want_state {
        if !crate::comments::drift::STATE_NAMES.contains(&s) {
            return Err(format!(
                "unknown comment state {s:?}; known: {}",
                crate::comments::drift::STATE_NAMES.join(", ")
            ));
        }
    }
    let paths: Option<HashSet<String>> = args
        .get("from")
        .and_then(|v| v.as_set())
        .map(|s| s.iter().filter_map(|a| a.path.clone()).collect());
    let mut census = StepCensus::new();
    if let Some(p) = &paths {
        census.input("input_files", p.len() as i64);
        if p.is_empty() {
            return Ok(StepOutput {
                rows: Vec::new(),
                total: 0,
                census: StepCensus::empty(EmptyReason::UpstreamEmpty),
            });
        }
    }
    let all = ctx
        .store
        .list_comments(ctx.repo_id, kind, keyword, None, 5_000)
        .map_err(|e| e.to_string())?;
    census.input("comment_rows", all.len() as i64);
    if all.is_empty() {
        return Ok(StepOutput {
            rows: Vec::new(),
            total: 0,
            census: StepCensus::empty(EmptyReason::NoIndex)
                .with_filter("no comments/1 rows for this repo"),
        });
    }
    let scoped: Vec<_> = all
        .into_iter()
        .filter(|r| paths.as_ref().is_none_or(|p| p.contains(&r.path)))
        .filter(|r| ctx.in_scope(&r.path))
        .collect();

    // `drifted` is the only state that needs blame. Compute it under a
    // hard file budget and SAY when the budget bit.
    let needs_blame = want_state == Some("drifted");
    let mut blames: BTreeMap<String, Option<crate::comments::drift::FileBlame>> = BTreeMap::new();
    if needs_blame {
        let mut files: Vec<&str> = scoped.iter().map(|r| r.path.as_str()).collect();
        files.sort_unstable();
        files.dedup();
        let budgeted = files.len().min(MAX_BLAMED_FILES);
        if files.len() > budgeted {
            census.note(format!(
                "blame budget: {budgeted} of {} files blamed; the rest report state \
                 \"unknown\" rather than \"fresh\"",
                files.len()
            ));
        }
        if let Ok(git) = crate::git::GitRepo::open(ctx.repo_root) {
            for f in files.into_iter().take(budgeted) {
                if ctx.out_of_budget() {
                    break;
                }
                let fb = crate::blame::blame_file(
                    ctx.blame_cache,
                    &git,
                    ctx.repo_id,
                    ctx.repo_root,
                    f,
                    Some("HEAD"),
                    None,
                )
                .ok()
                .map(|r| crate::comments::drift::FileBlame::new(r.regions));
                blames.insert(f.to_string(), fb);
            }
        }
    }

    let mut rows = Vec::new();
    for r in scoped {
        let state = comment_state(&r, blames.get(&r.path).and_then(|b| b.as_ref()), ctx.today);
        if let Some(want) = want_state {
            if state.state != want {
                continue;
            }
        }
        let mut addr = Addr::new(AddrKind::Comment, ctx.repo_name)
            .with_path(r.path.clone())
            .with_line(r.line_start.max(1) as u32)
            .with_id(format!("{}#{}", r.path, r.ordinal))
            .with_blob(Some(r.blob_sha.as_str()))
            .scalar("kind", r.kind.clone())
            .scalar("state", state.state)
            .scalar("text", r.text.clone());
        if let Some(k) = &r.keyword {
            addr = addr.scalar("keyword", k.clone());
        }
        if let Some(age) = state.age_days {
            addr = addr.scalar("age_days", age).scalar("score", age as f64);
        }
        if let Some(sym) = &r.symbol_name {
            addr = addr.with_symbol(sym.clone());
        }
        rows.push(addr);
    }
    if let Some(s) = want_state {
        census.filter(format!("state == {s}"));
    }
    if kind.is_some() {
        census.filter("kind");
    }
    if keyword.is_some() {
        census.filter("keyword");
    }
    Ok(StepOutput::finish(rows, limit, census))
}

/// Row → state, over the SAME `comments::drift` primitives the route
/// uses. Not a second oracle: every mint below is one of drift's own
/// three functions, and this fn only picks which one a row is subject to.
fn comment_state(
    r: &crate::store::CommentRow,
    blame: Option<&crate::comments::drift::FileBlame>,
    today: chrono::NaiveDate,
) -> crate::comments::drift::CommentState {
    use crate::comments::classify::CommentKind;
    match CommentKind::parse(&r.kind) {
        Some(CommentKind::Annotation) => {
            let fields = r.fields_json.as_deref().and_then(|j| {
                serde_json::from_str::<crate::comments::keywords::SmartTodoFields>(j).ok()
            });
            crate::comments::drift::annotation_state(fields.as_ref(), today)
        }
        Some(CommentKind::Directive) => crate::comments::drift::directive_state(
            r.directive_tool.as_deref(),
            r.directive_has_reason,
        ),
        Some(CommentKind::Doc) => match blame {
            Some(fb) => crate::comments::drift::doc_state(
                fb,
                r.line_start.max(1) as u32,
                r.line_end.max(1) as u32,
                r.symbol_line_start.unwrap_or(r.line_end).max(1) as u32,
                r.symbol_line_end.unwrap_or(r.line_end).max(1) as u32,
            ),
            // No blame was taken for this file (budget, or the state the
            // caller asked for never needed one). "unknown" — never
            // "fresh", which would be a claim nobody checked.
            None => crate::comments::drift::CommentState {
                state: "unknown",
                reason: Some("no blame taken for this file"),
                age_days: None,
                code_commit: None,
                doc_commit: None,
                on_date: None,
                tool: None,
            },
        },
        _ => crate::comments::drift::CommentState {
            state: "none",
            reason: None,
            age_days: None,
            code_commit: None,
            doc_commit: None,
            on_date: None,
            tool: None,
        },
    }
}

// --- facts ----------------------------------------------------------------

fn exec_facts(ctx: &RunCtx<'_>, args: &Args) -> Result<StepOutput, String> {
    let limit = limit_of(args, super::DEFAULT_STEP_ROWS);
    let lane = args
        .get("lane")
        .and_then(|v| v.as_str())
        .ok_or("`lane` must be a string")?;
    let want_kind = args.get("kind").and_then(|v| v.as_str());
    let mut census = StepCensus::new();
    let Some(resolved) = crate::lanes::resolve(lane) else {
        return Ok(StepOutput {
            rows: Vec::new(),
            total: 0,
            census: StepCensus::empty(EmptyReason::LaneUnknown)
                .with_filter(format!("no lane named {lane:?}")),
        });
    };
    // A lane is enabled ONLY by `[lanes]` in kb-code.toml (invariant
    // 21(a)) — never by a recipe, and never by a file inside a repo. A
    // recipe that names a disabled lane says so; it does not turn it on.
    if !ctx.lanes.is_enabled(&resolved.id) {
        return Ok(StepOutput {
            rows: Vec::new(),
            total: 0,
            census: StepCensus::empty(EmptyReason::LaneDisabled).with_filter(format!(
                "lane {:?} is not enabled in [lanes] (a recipe can never enable one)",
                resolved.id
            )),
        });
    }
    let Some(input) = args.get("from").and_then(|v| v.as_set()) else {
        return Ok(StepOutput {
            rows: Vec::new(),
            total: 0,
            census: StepCensus::empty(EmptyReason::NoInputs)
                .with_filter("`facts` reads per path; give it a `from` step"),
        });
    };
    let mut paths: Vec<&str> = input.iter().filter_map(|a| a.path.as_deref()).collect();
    paths.sort_unstable();
    paths.dedup();
    census.input("input_files", paths.len() as i64);
    if paths.is_empty() {
        return Ok(StepOutput {
            rows: Vec::new(),
            total: 0,
            census: StepCensus::empty(EmptyReason::UpstreamEmpty),
        });
    }
    // The live blob per path, read ONCE — `classing::class_for` needs it
    // to decide whether a fact is still about the bytes on disk, and a
    // per-fact lookup would be an N+1 against the one store connection.
    let live_blobs: BTreeMap<String, String> = ctx
        .store
        .list_files(ctx.repo_id)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|f| (f.path, f.blob_hash))
        .collect();
    let mut rows = Vec::new();
    let mut seen = 0i64;
    for p in paths {
        if ctx.out_of_budget() {
            census.note("stopped at the run budget; the input set was not exhausted");
            break;
        }
        let facts = ctx
            .store
            .lane_facts_for_path(ctx.repo_id, p, Some(&resolved.id), MAX_STEP_ROWS)
            .map_err(|e| e.to_string())?;
        for f in facts {
            seen += 1;
            if let Some(k) = want_kind {
                if f.kind != k {
                    continue;
                }
            }
            let live = live_blobs.get(p).cloned();
            let anchor = crate::lanes::classing::FactAnchor {
                blob_sha: &f.blob_sha,
                sha_source: &f.sha_source,
                range_start: f.range_start,
                range_end: f.range_end,
                snippet: f.snippet.as_deref(),
                cap: None,
            };
            let classed = crate::lanes::classing::class_for(
                resolved.ceiling(),
                &anchor,
                live.as_deref(),
                None,
            );
            let mut addr = Addr::new(AddrKind::Fact, ctx.repo_name)
                .with_path(f.path.clone())
                .with_id(format!("{}:{}", resolved.id, f.id))
                .with_blob(Some(f.blob_sha.as_str()))
                .with_trust(Some(classed.class.as_str()))
                .scalar("lane", resolved.id.clone())
                .scalar("fact_kind", f.kind.clone())
                .scalar("tool", f.tool.clone());
            if let Some(l) = classed.line.or(f.range_start) {
                addr = addr.with_line(l);
            }
            if let Some(sev) = &f.severity {
                addr = addr.scalar("severity", sev.clone());
            }
            rows.push(addr);
        }
    }
    census.input("facts", seen);
    if want_kind.is_some() {
        census.filter("fact kind");
    }
    Ok(StepOutput::finish(rows, limit, census))
}

// --- rails ----------------------------------------------------------------

fn exec_rails(ctx: &RunCtx<'_>, args: &Args) -> Result<StepOutput, String> {
    let limit = limit_of(args, super::DEFAULT_STEP_ROWS);
    let mut census = StepCensus::new();
    let idx = crate::rails::build_index(
        ctx.store,
        crate::rails::IndexInputs {
            repo_root: ctx.repo_root,
            repo_id: ctx.repo_id,
        },
    )
    .map_err(|e| e.message().to_string())?;
    let counts = idx.counts();
    let total_nouns: usize = counts.values().sum();
    census.input("rails_rows", total_nouns as i64);
    if total_nouns == 0 {
        return Ok(StepOutput {
            rows: Vec::new(),
            total: 0,
            census: StepCensus::empty(EmptyReason::NotARailsApp)
                .with_filter("rails/1 detected no Rails structure in this repo"),
        });
    }
    let mut rows = Vec::new();
    if let Some(noun) = args.get("noun").and_then(|v| v.as_str()) {
        // A noun that came from a param is checked HERE, since the load
        // could only see `$p.<name>`. An unknown one is a census, not a
        // silent empty list.
        if !crate::rails::NOUNS.contains(&noun) {
            return Ok(StepOutput {
                rows: Vec::new(),
                total: 0,
                census: StepCensus::empty(EmptyReason::LaneUnknown).with_filter(format!(
                    "unknown rails noun {noun:?}; known: {}",
                    crate::rails::NOUNS.join(", ")
                )),
            });
        }
        for r in idx.rows(noun) {
            if !ctx.in_scope(&r.path) {
                continue;
            }
            let mut addr = Addr::new(AddrKind::Entity, ctx.repo_name)
                .with_entity(r.fqn.clone().unwrap_or_else(|| r.name.clone()))
                .with_path(r.path.clone())
                .with_blob(r.blob_sha.as_deref())
                // rails/1's Trust has no Exact variant — the ceiling is a
                // TYPE there and is copied verbatim here (invariant 20(b)).
                .with_trust(Some(r.trust.as_str()))
                .scalar("noun", r.noun)
                .scalar("name", r.name.clone());
            if let Some(l) = r.line {
                addr = addr.with_line(l);
            }
            for (k, v) in &r.counts {
                addr = addr.scalar(k, *v as i64);
            }
            if !r.flags.is_empty() {
                addr = addr.scalar("flags", r.flags.join(","));
            }
            rows.push(addr);
        }
        census.filter(format!("noun == {noun}"));
    } else if let Some(lane_id) = args.get("orphans").and_then(|v| v.as_str()) {
        let report = crate::rails::orphans::build_report(ctx.repo_name, &idx, ctx.repo_root);
        let Some(lane) = report.lanes.iter().find(|l| l.id == lane_id) else {
            return Ok(StepOutput {
                rows: Vec::new(),
                total: 0,
                census: StepCensus::empty(EmptyReason::LaneUnknown)
                    .with_filter(format!("rails/1 reported no orphan lane {lane_id:?}")),
            });
        };
        census.input("lane_total", lane.total as i64);
        if lane.state != "ok" {
            return Ok(StepOutput {
                rows: Vec::new(),
                total: 0,
                census: StepCensus::empty(EmptyReason::LaneUnavailable).with_filter(
                    lane.reason
                        .clone()
                        .unwrap_or_else(|| format!("lane state {}", lane.state)),
                ),
            });
        }
        for r in &lane.rows {
            if !ctx.in_scope(&r.path) {
                continue;
            }
            let mut addr = Addr::new(AddrKind::File, ctx.repo_name)
                .with_path(r.path.clone())
                .with_trust(Some(r.trust.as_str()))
                .scalar("orphan_lane", lane.id)
                .scalar("name", r.name.clone());
            if let Some(l) = r.line {
                addr = addr.with_line(l);
            }
            rows.push(addr);
        }
        census.filter(format!("orphan lane == {lane_id}"));
        census.note(report.caption);
    } else {
        return Err("op `rails` needs exactly one of `noun` or `orphans`".into());
    }
    Ok(StepOutput::finish(rows, limit, census))
}

// --- tree -----------------------------------------------------------------

fn exec_tree(ctx: &RunCtx<'_>, args: &Args) -> Result<StepOutput, String> {
    let limit = limit_of(args, MAX_STEP_ROWS);
    let exts = csv(args, "ext");
    let mut census = StepCensus::new();
    let files = ctx
        .store
        .list_files(ctx.repo_id)
        .map_err(|e| e.to_string())?;
    census.input("mirror_files", files.len() as i64);
    if files.is_empty() {
        return Ok(StepOutput {
            rows: Vec::new(),
            total: 0,
            census: StepCensus::empty(EmptyReason::NoIndex)
                .with_filter("the mirror has no files for this repo yet"),
        });
    }
    // A step-local `scope` narrows FURTHER inside the run's own scope; it
    // never widens it. Resolution refuses rather than guessing, exactly
    // as kbc-tree/1 does (invariant 17(b)).
    let step_scope = args.get("scope").and_then(|v| v.as_str()).unwrap_or("");
    let step_paths = if step_scope.trim().is_empty() {
        None
    } else {
        let src = scope_sources_for(ctx, &files);
        let r = crate::tree::scope::resolve(step_scope, &src);
        if !r.applied {
            census.note(format!(
                "step scope {step_scope:?} was NOT applied ({}); this step is unscoped",
                r.diagnostics
                    .iter()
                    .map(|d| d.message.clone())
                    .chain(r.notes.iter().cloned())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
            None
        } else {
            census.filter(format!("scope {}", r.normalized));
            r.paths
        }
    };
    let mut rows = Vec::new();
    let mut scoped_out = 0i64;
    for f in files {
        if !ctx.in_scope(&f.path) {
            scoped_out += 1;
            continue;
        }
        if let Some(sp) = &step_paths {
            if !sp.contains(&f.path) {
                scoped_out += 1;
                continue;
            }
        }
        if !exts.is_empty() {
            let ext = f.path.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
            if !exts.iter().any(|e| e.trim_start_matches('.') == ext) {
                continue;
            }
        }
        rows.push(
            Addr::new(AddrKind::File, ctx.repo_name)
                .with_path(f.path.clone())
                .with_blob(Some(f.blob_hash.as_str()))
                .scalar("lang", f.lang.clone()),
        );
    }
    if scoped_out > 0 {
        census.input("scope_excluded", scoped_out);
    }
    if !exts.is_empty() {
        census.filter(format!("ext in [{}]", exts.join(", ")));
    }
    if rows.is_empty() && scoped_out > 0 {
        // The repo HAS files; the caller's own narrowing removed them.
        // That is a fact about the question, not about the repo, and it
        // must not read as `filtered-out` (the one clean reason).
        census.empty_reason = Some(EmptyReason::ScopeExcluded);
    }
    Ok(StepOutput::finish(rows, limit, census))
}

/// The scope sources a recipe can fill without paying for the whole
/// kbc-tree/1 decoration build. Only the `path`/`ext`/`lang` lanes and
/// the config scopes — every other atom resolves EMPTY, which
/// `tree::scope` documents, so a recipe naming one gets an honest refusal
/// rather than a silent everything.
pub fn scope_sources_for(
    ctx: &RunCtx<'_>,
    files: &[crate::store::FileRow],
) -> crate::tree::scope::ScopeSources {
    let mut src = crate::tree::scope::ScopeSources {
        config_scopes: ctx.scopes.map.clone(),
        ..Default::default()
    };
    src.files = files
        .iter()
        .map(|f| crate::tree::scope::ScopeFile {
            path: f.path.clone(),
            lang: f.lang.clone(),
        })
        .collect();
    src
}

// --- git_log --------------------------------------------------------------

fn exec_git_log(ctx: &RunCtx<'_>, args: &Args) -> Result<StepOutput, String> {
    let limit = limit_of(args, super::DEFAULT_STEP_ROWS);
    let since = args.get("since").and_then(|v| v.as_str());
    let range = args.get("range").and_then(|v| v.as_str());
    let mut census = StepCensus::new();
    // A caller-supplied range reaches git's option parser, so it goes
    // through the validated TYPE (invariant 3) before anything else.
    if let Some(r) = range {
        crate::git::RefRange::parse(r).map_err(|e| format!("range {r:?}: {e}"))?;
    }
    let since_unix = match since {
        Some(s) => Some(parse_since_unix(s)?),
        None => None,
    };
    let (commits, truncated) = crate::behavioral::walk_commits_capped(
        ctx.repo_root,
        range,
        since_unix,
        Some(MAX_STEP_ROWS),
    )
    .map_err(|e| e.to_string())?;
    census.input("commits", commits.len() as i64);
    if truncated {
        census.note(format!(
            "the commit walk was capped at {MAX_STEP_ROWS}; these are the newest"
        ));
    }
    if commits.is_empty() {
        return Ok(StepOutput {
            rows: Vec::new(),
            total: 0,
            census: StepCensus::empty(EmptyReason::NoInputs)
                .with_filter("no commits in the requested window"),
        });
    }
    let emit = args
        .get("emit")
        .and_then(|v| v.as_str())
        .unwrap_or("commits");
    let mut rows = Vec::new();
    if emit == "files" {
        // One row per PATH the window touched, with the window's churn
        // folded onto it. A rename shows as two paths, because git's own
        // `-M` output names both and collapsing them would be a claim
        // about identity this op cannot make.
        let mut by_path: BTreeMap<String, (i64, i64)> = BTreeMap::new();
        for c in &commits {
            for (p, a, d) in &c.files {
                if !ctx.in_scope(p) {
                    continue;
                }
                let e = by_path.entry(p.clone()).or_insert((0, 0));
                e.0 = e.0.saturating_add(a + d);
                e.1 += 1;
            }
        }
        census.input("touched_paths", by_path.len() as i64);
        for (path, (churn, touches)) in by_path {
            rows.push(
                Addr::new(AddrKind::File, ctx.repo_name)
                    .with_path(path)
                    .scalar("churn", churn)
                    .scalar("commits", touches)
                    .scalar("score", churn as f64),
            );
        }
        census.filter("emit = files");
    } else {
        for c in commits {
            let touched = c.files.iter().filter(|(p, _, _)| ctx.in_scope(p)).count();
            if ctx.scope_paths.is_some() && touched == 0 {
                continue;
            }
            let churn: i64 = c
                .files
                .iter()
                .filter(|(p, _, _)| ctx.in_scope(p))
                .map(|(_, a, d)| a + d)
                .sum();
            rows.push(
                Addr::new(AddrKind::Commit, ctx.repo_name)
                    .with_commit(c.sha.clone())
                    .scalar("author", c.author.clone())
                    .scalar("commit_unix", c.commit_unix)
                    .scalar("files", touched as i64)
                    .scalar("churn", churn)
                    .scalar("score", c.commit_unix as f64),
            );
        }
    }
    if ctx.scope_paths.is_some() {
        census.filter("kbc-scope/1 path set");
    }
    Ok(StepOutput::finish(rows, limit, census))
}

/// `YYYY-MM-DD` or a duration suffix (`30d`, `12h`). A git ref is NOT
/// accepted here: `walk_commits_capped` takes a unix instant, and
/// resolving a ref to one would be a second `since` grammar beside
/// `recipes/1`'s own.
fn parse_since_unix(s: &str) -> Result<i64, String> {
    let s = s.trim();
    if let Some(d) = s.strip_suffix('d').and_then(|n| n.parse::<i64>().ok()) {
        return Ok(chrono::Utc::now().timestamp() - d * 86_400);
    }
    if let Some(h) = s.strip_suffix('h').and_then(|n| n.parse::<i64>().ok()) {
        return Ok(chrono::Utc::now().timestamp() - h * 3_600);
    }
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() == 3 {
        if let (Ok(y), Ok(m), Ok(d)) = (
            parts[0].parse::<i32>(),
            parts[1].parse::<u32>(),
            parts[2].parse::<u32>(),
        ) {
            if let Some(dt) = chrono::NaiveDate::from_ymd_opt(y, m, d) {
                return Ok(dt.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp());
            }
        }
    }
    Err(format!(
        "since {s:?} must be `YYYY-MM-DD` or a duration (`30d`, `12h`)"
    ))
}

// --- blame ----------------------------------------------------------------

fn exec_blame(ctx: &RunCtx<'_>, args: &Args) -> Result<StepOutput, String> {
    let limit = limit_of(args, super::DEFAULT_STEP_ROWS);
    let Some(input) = args.get("from").and_then(|v| v.as_set()) else {
        return Err("`from` must reference a step".into());
    };
    let mut census = StepCensus::new();
    let mut paths: Vec<&str> = input.iter().filter_map(|a| a.path.as_deref()).collect();
    paths.sort_unstable();
    paths.dedup();
    census.input("input_files", paths.len() as i64);
    if paths.is_empty() {
        return Ok(StepOutput {
            rows: Vec::new(),
            total: 0,
            census: StepCensus::empty(EmptyReason::UpstreamEmpty),
        });
    }
    let budgeted = paths.len().min(MAX_BLAMED_FILES);
    if paths.len() > budgeted {
        census.note(format!(
            "blame budget: {budgeted} of {} files blamed",
            paths.len()
        ));
    }
    let git = crate::git::GitRepo::open(ctx.repo_root).map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    for p in paths.into_iter().take(budgeted) {
        if ctx.out_of_budget() {
            census.note("stopped at the run budget");
            break;
        }
        let Ok(res) = crate::blame::blame_file(
            ctx.blame_cache,
            &git,
            ctx.repo_id,
            ctx.repo_root,
            p,
            Some("HEAD"),
            None,
        ) else {
            continue;
        };
        for r in res.regions {
            rows.push(
                Addr::new(AddrKind::Line, ctx.repo_name)
                    .with_path(p.to_string())
                    .with_line(r.final_start)
                    .with_commit(r.sha.clone())
                    .scalar("author", r.author.clone())
                    .scalar("author_time", r.author_time)
                    .scalar("lines", r.count as i64)
                    .scalar("subject", r.subject.clone()),
            );
        }
    }
    Ok(StepOutput::finish(rows, limit, census))
}

// --- churn ----------------------------------------------------------------

fn exec_churn(ctx: &RunCtx<'_>, args: &Args) -> Result<StepOutput, String> {
    let limit = limit_of(args, super::DEFAULT_STEP_ROWS);
    let min_rev = args
        .get("min_revisions")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let mut census = StepCensus::new();
    let stats = ctx
        .store
        .list_path_stats(ctx.repo_id)
        .map_err(|e| e.to_string())?;
    census.input("path_stats", stats.len() as i64);
    if stats.is_empty() {
        // A missing behavioral backfill is a MISSING INPUT, never an
        // empty answer — the design law recipes/1 states in its own header
        // and violated in three places (D11's repair list).
        return Ok(StepOutput {
            rows: Vec::new(),
            total: 0,
            census: StepCensus::empty(EmptyReason::NoIndex).with_filter(
                "no behavioral path_stats rows — run `kb-code backfill --repo <r>` first",
            ),
        });
    }
    let mut rows = Vec::new();
    for s in stats {
        if !ctx.in_scope(&s.path) || s.revisions < min_rev {
            continue;
        }
        let churn = s.lines_added.saturating_add(s.lines_deleted);
        let mut addr = Addr::new(AddrKind::File, ctx.repo_name)
            .with_path(s.path.clone())
            .scalar("revisions", s.revisions)
            .scalar("churn", churn)
            .scalar("score", churn as f64);
        if let Some(t) = s.last_touch_unix {
            addr = addr.scalar("last_touch_unix", t);
        }
        rows.push(addr);
    }
    if min_rev > 0 {
        census.filter(format!("revisions >= {min_rev}"));
    }
    Ok(StepOutput::finish(rows, limit, census))
}

// --- boards ---------------------------------------------------------------

fn exec_boards(ctx: &RunCtx<'_>, args: &Args) -> Result<StepOutput, String> {
    let limit = limit_of(args, super::DEFAULT_STEP_ROWS);
    let status = args.get("status").and_then(|v| v.as_str());
    if let Some(s) = status {
        if !crate::boards::is_valid_status(s) {
            return Err(format!(
                "unknown board status {s:?}; known: {}",
                crate::boards::STATUSES.join(", ")
            ));
        }
    }
    let mut census = StepCensus::new();
    let boards = ctx
        .store
        // V74-L3b — the kind is REQUIRED now (a tour is a `canvas_boards`
        // row too, V0039); the `boards` op means BOARDS.
        .list_canvas_boards(ctx.repo_id, crate::tours::BOARD_KIND_BOARD, status)
        .map_err(|e| e.to_string())?;
    census.input("boards", boards.len() as i64);
    if boards.is_empty() {
        return Ok(StepOutput {
            rows: Vec::new(),
            total: 0,
            census: StepCensus::empty(EmptyReason::NoInputs)
                .with_filter("no kbc-canvas/1 boards for this repo"),
        });
    }
    let mut rows = Vec::new();
    for b in boards {
        if ctx.out_of_budget() {
            census.note("stopped at the run budget");
            break;
        }
        let Ok(board) =
            crate::boards::routes::resolve_board(ctx.store, ctx.repo, ctx.repo_id, &b.slug, false)
        else {
            continue;
        };
        for n in board.nodes {
            let mut addr = Addr::new(AddrKind::Node, ctx.repo_name)
                .with_id(format!("{}/{}", b.slug, n.id))
                .scalar("board", b.slug.clone())
                .scalar("node_kind", n.kind.clone())
                .scalar("state", n.state)
                .scalar("reason", n.reason)
                .scalar("address", n.address.clone());
            if let Some(code) = &n.code {
                addr = addr
                    .with_path(code.path.clone())
                    .with_line(code.range[0])
                    .with_blob(code.current_blob_sha.as_deref());
            }
            if let Some(t) = &n.title {
                addr = addr.scalar("title", t.clone());
            }
            rows.push(addr);
        }
    }
    if status.is_some() {
        census.filter("board status");
    }
    Ok(StepOutput::finish(rows, limit, census))
}

// --- review ---------------------------------------------------------------

fn exec_review(ctx: &RunCtx<'_>, args: &Args) -> Result<StepOutput, String> {
    let limit = limit_of(args, super::DEFAULT_STEP_ROWS);
    let kind = args
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or("`kind` must be a string")?;
    if !matches!(kind, "findings" | "timeline") {
        return Err(format!(
            "unknown review read {kind:?}; known: findings | timeline"
        ));
    }
    let state = args.get("state").and_then(|v| v.as_str());
    let severity = args.get("severity").and_then(|v| v.as_str());
    let disposition = args.get("disposition").and_then(|v| v.as_str());
    let mut census = StepCensus::new();
    let reviews = ctx
        .store
        .list_reviews(ctx.repo_name, state)
        .map_err(|e| e.to_string())?;
    census.input("reviews", reviews.len() as i64);
    if reviews.is_empty() {
        return Ok(StepOutput {
            rows: Vec::new(),
            total: 0,
            census: StepCensus::empty(EmptyReason::NoInputs)
                .with_filter("no local reviews for this repo"),
        });
    }
    let mut rows = Vec::new();
    let mut seen = 0i64;
    for r in &reviews {
        let findings = ctx
            .store
            .list_review_findings(r.id, disposition, false)
            .map_err(|e| e.to_string())?;
        for f in findings {
            seen += 1;
            if let Some(s) = severity {
                if f.severity != s {
                    continue;
                }
            }
            let mut addr = Addr::new(AddrKind::Finding, ctx.repo_name)
                .with_id(f.slug.clone())
                .scalar("review_id", r.id)
                .scalar("severity", f.severity.clone())
                .scalar("title", f.title.clone())
                .scalar("origin", f.origin.clone())
                .scalar("blocking", f.blocking);
            if !f.location_path.is_empty() {
                addr = addr.with_path(f.location_path.clone());
                if let Some(lines) = &f.location_lines {
                    if let Some(first) = lines
                        .split([',', '-'])
                        .next()
                        .and_then(|n| n.trim().parse::<u32>().ok())
                    {
                        addr = addr.with_line(first);
                    }
                }
            }
            if let Some(d) = &f.disposition {
                addr = addr.scalar("disposition", d.clone());
            }
            if kind == "timeline" {
                addr = addr
                    .scalar("at", f.created_at)
                    .scalar("score", f.created_at as f64);
            }
            rows.push(addr);
        }
    }
    census.input("findings", seen);
    if kind == "timeline" {
        // Deliberate and stated: a timeline carries events (a verdict, a
        // patchset, a comment) that are not addresses, and this op emits
        // ADDRESSES. Rather than invent one, it reports the finding-shaped
        // events in timeline order and says what it left out.
        census.note(
            "timeline mode reports the FINDING-shaped events only; verdict / patchset / \
             comment events are not addresses and are not invented as ones",
        );
    }
    if severity.is_some() {
        census.filter("severity");
    }
    if disposition.is_some() {
        census.filter("disposition");
    }
    if state.is_some() {
        census.filter("review state");
    }
    Ok(StepOutput::finish(rows, limit, census))
}

// --- set_ops --------------------------------------------------------------

fn exec_set_ops(ctx: &RunCtx<'_>, args: &Args) -> Result<StepOutput, String> {
    let mode = args
        .get("mode")
        .and_then(|v| v.as_str())
        .ok_or("`mode` must be a string")?;
    let a = args
        .get("from")
        .and_then(|v| v.as_set())
        .ok_or("`from` must reference a step")?
        .to_vec();
    let mut census = StepCensus::new();
    census.input("left", a.len() as i64);
    let b = args
        .get("with")
        .and_then(|v| v.as_set())
        .map(|s| s.to_vec());
    if let Some(b) = &b {
        census.input("right", b.len() as i64);
    }
    let limit = limit_of(args, MAX_STEP_ROWS);
    let out: Vec<Addr> = match mode {
        "union" => {
            let b = b.unwrap_or_default();
            let mut merged: BTreeMap<_, Addr> = BTreeMap::new();
            for x in a.into_iter().chain(b) {
                merged
                    .entry(owned_key(&x))
                    .and_modify(|e| {
                        for (k, v) in &x.scalars {
                            e.scalars.entry(k.clone()).or_insert_with(|| v.clone());
                        }
                    })
                    .or_insert(x);
            }
            merged.into_values().collect()
        }
        "intersect" => {
            let b = b.unwrap_or_default();
            let right: HashSet<_> = b.iter().map(owned_key).collect();
            // The RIGHT side's scalars ride along, so an intersect can
            // carry the second lane's numbers into the view.
            let by_key: BTreeMap<_, &Addr> = b.iter().map(|x| (owned_key(x), x)).collect();
            a.into_iter()
                .filter(|x| right.contains(&owned_key(x)))
                .map(|mut x| {
                    if let Some(other) = by_key.get(&owned_key(&x)) {
                        for (k, v) in &other.scalars {
                            x.scalars.entry(k.clone()).or_insert_with(|| v.clone());
                        }
                    }
                    x
                })
                .collect()
        }
        "diff" => {
            let b = b.unwrap_or_default();
            let right: HashSet<_> = b.iter().map(owned_key).collect();
            a.into_iter()
                .filter(|x| !right.contains(&owned_key(x)))
                .collect()
        }
        "filter" => {
            let trust = csv(args, "trust");
            let prefix = args.get("path_prefix").and_then(|v| v.as_str());
            let by = args.get("by").and_then(|v| v.as_str());
            let min = args.get("min").and_then(|v| match v {
                Resolved::Val(x) => x.as_f64(),
                _ => None,
            });
            let max = args.get("max").and_then(|v| match v {
                Resolved::Val(x) => x.as_f64(),
                _ => None,
            });
            if (min.is_some() || max.is_some()) && by.is_none() {
                return Err("`min`/`max` need `by` (the scalar to compare)".into());
            }
            if !trust.is_empty() {
                census.filter(format!("trust in [{}]", trust.join(", ")));
            }
            if let Some(p) = prefix {
                census.filter(format!("path starts with {p}"));
            }
            if let Some(k) = by {
                census.filter(format!(
                    "{k} in [{}, {}]",
                    min.map(|v| v.to_string()).unwrap_or_else(|| "-inf".into()),
                    max.map(|v| v.to_string()).unwrap_or_else(|| "+inf".into())
                ));
            }
            a.into_iter()
                .filter(|x| trust.is_empty() || trust.iter().any(|t| t == &x.trust))
                .filter(|x| {
                    prefix.is_none_or(|p| x.path.as_deref().is_some_and(|q| q.starts_with(p)))
                })
                .filter(|x| {
                    let Some(k) = by else { return true };
                    // A row whose scalar is ABSENT is dropped, never
                    // treated as zero — the same law as `blob`.
                    let Some(v) = x.scalars.get(k).and_then(|s| s.as_f64()) else {
                        return false;
                    };
                    min.is_none_or(|m| v >= m) && max.is_none_or(|m| v <= m)
                })
                .collect()
        }
        "sort" => {
            let by = args
                .get("by")
                .and_then(|v| v.as_str())
                .ok_or("`sort` needs `by`")?
                .to_string();
            let desc = args.get("desc").and_then(|v| v.as_bool()).unwrap_or(true);
            let mut v = a;
            v.sort_by(|x, y| {
                let sx = x.scalars.get(&by).and_then(|s| s.as_f64());
                let sy = y.scalars.get(&by).and_then(|s| s.as_f64());
                let ord = match (sx, sy) {
                    (Some(p), Some(q)) => p.partial_cmp(&q).unwrap_or(std::cmp::Ordering::Equal),
                    // Missing sorts LAST in either direction: it is not a
                    // small number, it is an absent one.
                    (Some(_), None) => std::cmp::Ordering::Greater,
                    (None, Some(_)) => std::cmp::Ordering::Less,
                    (None, None) => std::cmp::Ordering::Equal,
                };
                let ord = if desc { ord.reverse() } else { ord };
                ord.then_with(|| x.key().cmp(&y.key()))
            });
            census.filter(format!(
                "sorted by {by} {}",
                if desc { "desc" } else { "asc" }
            ));
            // `sort` is the one mode whose ORDER is the answer, so it
            // returns early rather than being re-sorted by `finish`.
            let total = v.len();
            v.truncate(limit);
            return Ok(StepOutput {
                rows: v,
                total,
                census,
            });
        }
        "limit" => {
            let n = args
                .get("n")
                .and_then(|v| v.as_i64())
                .filter(|n| *n > 0)
                .ok_or("`limit` needs a positive `n`")? as usize;
            census.filter(format!("first {n}"));
            let total = a.len();
            let mut v = a;
            v.truncate(n.min(limit));
            return Ok(StepOutput {
                rows: v,
                total,
                census,
            });
        }
        other => return Err(format!("unknown set_ops mode {other:?}")),
    };
    let _ = ctx;
    Ok(StepOutput::finish(out, limit, census))
}

type OwnedKey = (u8, String, String, u32, String, String, String, String);

fn owned_key(a: &Addr) -> OwnedKey {
    let k = a.key();
    (
        k.0,
        k.1.to_string(),
        k.2.to_string(),
        k.3,
        k.4.to_string(),
        k.5.to_string(),
        k.6.to_string(),
        k.7.to_string(),
    )
}

// --- map ------------------------------------------------------------------

fn exec_map(ctx: &RunCtx<'_>, args: &Args) -> Result<StepOutput, String> {
    let limit = limit_of(args, MAX_STEP_ROWS);
    let to = args
        .get("to")
        .and_then(|v| v.as_str())
        .ok_or("`to` must be a string")?;
    let input = args
        .get("from")
        .and_then(|v| v.as_set())
        .ok_or("`from` must reference a step")?;
    let mut census = StepCensus::new();
    census.input("input", input.len() as i64);
    census.filter(format!("map {to}"));
    if input.is_empty() {
        return Ok(StepOutput {
            rows: Vec::new(),
            total: 0,
            census: StepCensus::empty(EmptyReason::UpstreamEmpty),
        });
    }
    let mut rows: Vec<Addr> = Vec::new();
    let mut dropped = 0i64;
    match to {
        "file-of" => {
            let mut seen: HashSet<String> = HashSet::new();
            for a in input {
                let Some(p) = a.path.clone() else {
                    dropped += 1;
                    continue;
                };
                if !seen.insert(p.clone()) {
                    continue;
                }
                rows.push(Addr::new(AddrKind::File, &a.repo).with_path(p).with_blob(
                    if a.blob == BLOB_UNKNOWN {
                        None
                    } else {
                        Some(a.blob.as_str())
                    },
                ));
            }
        }
        "symbol-of" => {
            let symbols = ctx
                .store
                .symbols_for_repo(ctx.repo_id)
                .map_err(|e| e.to_string())?;
            let mut by_path: BTreeMap<&str, Vec<&crate::extract::Symbol>> = BTreeMap::new();
            for (p, s) in &symbols {
                by_path.entry(p.as_str()).or_default().push(s);
            }
            let mut seen: HashSet<(String, String)> = HashSet::new();
            for a in input {
                let (Some(p), Some(l)) = (a.path.as_deref(), a.line) else {
                    dropped += 1;
                    continue;
                };
                // The INNERMOST symbol whose span covers the line. A line
                // outside every span is dropped, never attached to the
                // nearest one.
                let best = by_path.get(p).and_then(|v| {
                    v.iter()
                        .filter(|s| s.line_start <= l && l <= s.line_end)
                        .min_by_key(|s| s.line_end.saturating_sub(s.line_start))
                });
                let Some(s) = best else {
                    dropped += 1;
                    continue;
                };
                if !seen.insert((p.to_string(), s.name.clone())) {
                    continue;
                }
                rows.push(
                    Addr::new(AddrKind::Symbol, &a.repo)
                        .with_path(p.to_string())
                        .with_line(s.line_start)
                        .with_symbol(s.name.clone())
                        .scalar("kind", s.kind.clone()),
                );
            }
        }
        "entity-of" => {
            let defs = ctx
                .store
                .entity_defs_for_repo(ctx.repo_id, None, 200_000)
                .map_err(|e| e.to_string())?;
            let mut seen: HashSet<String> = HashSet::new();
            for a in input {
                let Some(p) = a.path.as_deref() else {
                    dropped += 1;
                    continue;
                };
                let mut any = false;
                for d in defs.iter().filter(|d| d.path == p) {
                    if let Some(sym) = a.symbol.as_deref() {
                        if !d.fqn.ends_with(sym) {
                            continue;
                        }
                    }
                    any = true;
                    if !seen.insert(d.fqn.clone()) {
                        continue;
                    }
                    let stale = d.live_blob_hash.as_deref() != Some(d.blob_hash.as_str());
                    rows.push(
                        Addr::new(AddrKind::Entity, &a.repo)
                            .with_entity(d.fqn.clone())
                            .with_path(d.path.clone())
                            .with_line(d.line_start.max(1) as u32)
                            .with_blob(Some(d.blob_hash.as_str()))
                            .with_trust(Some(crate::entities::class_for(
                                crate::entities::MATCHED_VIA_NESTING,
                                &d.nesting,
                                &d.zeitwerk_state,
                                stale,
                            )))
                            .scalar("kind", d.kind.clone()),
                    );
                }
                if !any {
                    dropped += 1;
                }
            }
        }
        "definitions-of" => {
            let symbols = ctx
                .store
                .symbols_for_repo(ctx.repo_id)
                .map_err(|e| e.to_string())?;
            let mut seen: HashSet<(String, String)> = HashSet::new();
            for a in input {
                let name = a
                    .symbol
                    .clone()
                    .or_else(|| a.entity.as_ref().map(|e| last_segment(e).to_string()));
                let Some(name) = name else {
                    dropped += 1;
                    continue;
                };
                let mut any = false;
                for (p, s) in symbols.iter().filter(|(_, s)| s.name == name) {
                    any = true;
                    if !seen.insert((p.clone(), s.name.clone())) {
                        continue;
                    }
                    rows.push(
                        Addr::new(AddrKind::Symbol, &a.repo)
                            .with_path(p.clone())
                            .with_line(s.line_start)
                            .with_symbol(s.name.clone())
                            .scalar("kind", s.kind.clone()),
                    );
                }
                if !any {
                    dropped += 1;
                }
            }
            census.note(
                "`definitions-of` matches on the last name segment; a name the index cannot \
                 disambiguate returns EVERY definition site rather than picking one",
            );
        }
        other => return Err(format!("unknown map transform {other:?}")),
    }
    if dropped > 0 {
        census.input("unmapped", dropped);
        census.note(format!(
            "{dropped} input address(es) had nothing to map to and were dropped rather than \
             mapped to a guess"
        ));
    }
    Ok(StepOutput::finish(rows, limit, census))
}

fn last_segment(fqn: &str) -> &str {
    fqn.rsplit("::").next().unwrap_or(fqn)
}

/// Scalar helper used by views + the CLI's table renderer.
pub fn scalar_str(v: &ScalarVal) -> String {
    match v {
        ScalarVal::Int(i) => i.to_string(),
        ScalarVal::Float(f) => format!("{f:.4}"),
        ScalarVal::Str(s) => s.clone(),
        ScalarVal::Bool(b) => b.to_string(),
    }
}
