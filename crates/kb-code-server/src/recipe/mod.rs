//! `kbc-recipe/1` — the typed recipe runner (V74-L3a, design of record
//! `docs/research/kb-code-v7-continuum-2026-09.html` §Decisions **D11** +
//! **D21**, Track L).
//!
//! A recipe is a NAMED, PARAMETERISED, DETERMINISTIC question asked of
//! kb-code's own indexes. It is written down — in the repo, in server
//! storage, or compiled in — rather than retyped, and it is composed out
//! of a CLOSED set of this daemon's own primitives rather than out of a
//! query language.
//!
//! ## The five rules
//!
//! 1. **The op set is closed, and the type system is the guard.**
//!    [`ops::Op`] is a Rust enum; there is no user function, no shell, no
//!    `eval`. Crucially, **a recipe can never reference an exec lane** —
//!    not because a check rejects it but because no variant of [`ops::Op`]
//!    names one (D21; crate invariant 10, "the daemon never spawns a
//!    non-git process", and invariant 21(b)'s two-lane-kind ruling). The
//!    `facts` op reads `lane_facts` rows an operator's own CLI already
//!    ingested; it cannot cause a tool to run.
//! 2. **Every cell of a result is an ADDRESS.** [`Addr`] is the ONE row
//!    type every op emits and every view renders. A column is either one
//!    of its address fields or a scalar DERIVED from one ([`ColField`]) —
//!    there is no free-text cell, so a result is always something a reader
//!    can navigate to.
//! 3. **A trust class is BORROWED, never minted here.** An op copies the
//!    class the underlying engine already computed (`usages2`'s own
//!    `trust`, `entities::class_for`, `rails::noun_trust`) and this module
//!    has no code that raises one. A row whose engine reports nothing
//!    carries [`TRUST_UNKNOWN`], which is also what a missing blob reads
//!    as — never a zero, never an absent field (invariants 13/20/21/22,
//!    root invariant #2).
//! 4. **An empty step says WHY.** Every step ships a
//!    [`census::StepCensus`] naming its inputs, the filters it applied and
//!    — when it returned nothing — a reason from a CLOSED vocabulary
//!    ([`census::EmptyReason`]). "0 rows" and "this lane is switched off"
//!    are different facts and must never render the same.
//! 5. **A run is deterministic for a given mirror state.** Every op sorts
//!    totally on the address itself, and the runner memoises identical
//!    step calls, so two runs against one mirror generation are
//!    byte-identical (pinned by `run::tests::two_runs_are_byte_identical`).
//!    Nothing computed per request is persisted unless the operator asks
//!    for a materialised run over loopback.
//!
//! ## Where a recipe lives
//!
//! Three homes, and every catalog row says which one it came from
//! ([`Home`]):
//!
//! - `builtin` — the eight shipped in [`builtins`], plus the six
//!   `recipes/1` natives (see [`builtins::NATIVE_SLUGS`]).
//! - `repo` — `.kbc/recipes/*.toml`, read ONLY from the repo's DEFAULT ref
//!   through the ODB and never from the working tree, under
//!   trust-on-first-use ([`loader`]).
//! - `server` — agent-authored and UI-saved (`recipe new --from-json -`),
//!   which writes to the daemon and never into the tree.
//!
//! **A repo file WINS on a slug collision** and the shadowed server row is
//! REPORTED (`shadowed_by`), not silently dropped — the dead-surface
//! defect class this crate keeps designing out.

pub mod builtins;
pub mod census;
pub mod loader;
pub mod ops;
pub mod routes;
pub mod run;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Schema label on every catalog/show response.
pub const SCHEMA: &str = "kbc-recipe/1";

/// Schema label on a run (and on a materialised run's stored body).
pub const RUN_SCHEMA: &str = "kbc-recipe-run/1";

/// Hard per-step row cap. A step that would return more REFUSES with the
/// count (`limit > MAX_STEP_ROWS` is a 400 naming both numbers) rather
/// than silently clamping — kb root invariant #35's `?ids=` rule, which
/// `recipes/1` violated until D11's repair list named it.
pub const MAX_STEP_ROWS: usize = 500;

/// Default rows per step when a recipe names no `limit`.
pub const DEFAULT_STEP_ROWS: usize = 50;

/// Views per recipe (D11: "four result views").
pub const MAX_VIEWS: usize = 4;

/// Steps per recipe. A DAG this size is already at the edge of what a
/// human can hold; past it the recipe wants to be two recipes.
pub const MAX_STEPS: usize = 24;

/// Declared params per recipe.
pub const MAX_PARAMS: usize = 12;

/// Whole-run wall-clock budget. A step that has not STARTED when this is
/// spent is reported `budget-exhausted` with the elapsed time, so a slow
/// run degrades into an honest partial rather than an opaque hang (the
/// recipes/1 rough edge D11's "client timeout 600 s" repair addresses
/// from the other end).
pub const RUN_BUDGET_MS: u128 = 20_000;

/// The class an address carries when the engine behind it reported none.
/// NOT a synonym for "no data": a row is here, and what is unknown is how
/// far to trust its position.
pub const TRUST_UNKNOWN: &str = "unknown";

/// What a missing blob reads as. Never `""`, never absent, never `0`.
pub const BLOB_UNKNOWN: &str = "unknown";

// ---------------------------------------------------------------------------
// Addresses
// ---------------------------------------------------------------------------

/// The kinds of thing a step can produce. Closed: an op declares which
/// kind it consumes and which it produces, so the DAG is type-checked at
/// LOAD time and a mis-wired step fails by name instead of at run time
/// with an empty table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AddrKind {
    /// A file in the mirror. `path` is always set.
    File,
    /// A line inside a file. `path` + `line` are always set.
    Line,
    /// An extracted symbol. `path` + `symbol` are always set.
    Symbol,
    /// An `entity/1` FQN. `entity` is always set.
    Entity,
    /// A git commit. `commit` is always set.
    Commit,
    /// A `comments/1` annotation-in-source row. `path` + `line` are set.
    Comment,
    /// An `aug-lane/1` fact. `id` names the lane fact.
    Fact,
    /// A `kbc-review/1` finding. `id` is the finding slug.
    Finding,
    /// A `kbc-canvas/1` board node. `id` is the node id.
    Node,
}

impl AddrKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AddrKind::File => "file",
            AddrKind::Line => "line",
            AddrKind::Symbol => "symbol",
            AddrKind::Entity => "entity",
            AddrKind::Commit => "commit",
            AddrKind::Comment => "comment",
            AddrKind::Fact => "fact",
            AddrKind::Finding => "finding",
            AddrKind::Node => "node",
        }
    }

    pub const ALL: &'static [AddrKind] = &[
        AddrKind::File,
        AddrKind::Line,
        AddrKind::Symbol,
        AddrKind::Entity,
        AddrKind::Commit,
        AddrKind::Comment,
        AddrKind::Fact,
        AddrKind::Finding,
        AddrKind::Node,
    ];
}

/// ONE row type for every op and every view.
///
/// `trust` and `blob` are **not** `Option`: an address whose engine
/// reported neither says [`TRUST_UNKNOWN`]/[`BLOB_UNKNOWN`] in the
/// payload, because a reader who cannot see the difference between "the
/// blob is gone" and "nobody looked" will read the second as the first
/// (D11's own "missing blob ≠ zero" repair, applied to the new runner
/// from the start).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Addr {
    pub kind: AddrKind,
    pub repo: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// The id of a comment / fact / finding / board node.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Always present. `"unknown"` when the engine reported no blob.
    pub blob: String,
    /// Always present. `"unknown"` when the engine minted no class.
    pub trust: String,
    /// Derived numbers and short labels, each computed FROM this address
    /// by the op that emitted it (a churn count, a line span, a severity).
    /// Never free prose about something else.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scalars: BTreeMap<String, ScalarVal>,
}

impl Addr {
    pub fn new(kind: AddrKind, repo: &str) -> Self {
        Addr {
            kind,
            repo: repo.to_string(),
            path: None,
            line: None,
            symbol: None,
            entity: None,
            commit: None,
            id: None,
            blob: BLOB_UNKNOWN.to_string(),
            trust: TRUST_UNKNOWN.to_string(),
            scalars: BTreeMap::new(),
        }
    }

    pub fn with_path(mut self, p: impl Into<String>) -> Self {
        self.path = Some(p.into());
        self
    }
    pub fn with_line(mut self, l: u32) -> Self {
        self.line = Some(l);
        self
    }
    pub fn with_symbol(mut self, s: impl Into<String>) -> Self {
        self.symbol = Some(s.into());
        self
    }
    pub fn with_entity(mut self, e: impl Into<String>) -> Self {
        self.entity = Some(e.into());
        self
    }
    pub fn with_commit(mut self, c: impl Into<String>) -> Self {
        self.commit = Some(c.into());
        self
    }
    pub fn with_id(mut self, i: impl Into<String>) -> Self {
        self.id = Some(i.into());
        self
    }
    /// `None` (or an empty string) stays [`BLOB_UNKNOWN`] — the whole point.
    pub fn with_blob(mut self, b: Option<&str>) -> Self {
        if let Some(b) = b.filter(|s| !s.is_empty()) {
            self.blob = b.to_string();
        }
        self
    }
    /// `None` (or an empty string) stays [`TRUST_UNKNOWN`].
    pub fn with_trust(mut self, t: Option<&str>) -> Self {
        if let Some(t) = t.filter(|s| !s.is_empty()) {
            self.trust = t.to_string();
        }
        self
    }
    pub fn scalar(mut self, k: &str, v: impl Into<ScalarVal>) -> Self {
        self.scalars.insert(k.to_string(), v.into());
        self
    }

    /// The identity a `set_ops` union/intersect/diff keys on, and the
    /// TOTAL order every op sorts by. Deliberately excludes `scalars`:
    /// two rows naming the same place from two lanes are ONE place.
    pub fn key(&self) -> (u8, &str, &str, u32, &str, &str, &str, &str) {
        (
            AddrKind::ALL
                .iter()
                .position(|k| *k == self.kind)
                .unwrap_or(0) as u8,
            self.repo.as_str(),
            self.path.as_deref().unwrap_or(""),
            self.line.unwrap_or(0),
            self.symbol.as_deref().unwrap_or(""),
            self.entity.as_deref().unwrap_or(""),
            self.commit.as_deref().unwrap_or(""),
            self.id.as_deref().unwrap_or(""),
        )
    }
}

/// A scalar a view may render. Closed so a "cell" can never become free
/// prose smuggled through a recipe file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ScalarVal {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
}

impl Eq for ScalarVal {}

impl From<i64> for ScalarVal {
    fn from(v: i64) -> Self {
        ScalarVal::Int(v)
    }
}
impl From<u32> for ScalarVal {
    fn from(v: u32) -> Self {
        ScalarVal::Int(v as i64)
    }
}
impl From<u64> for ScalarVal {
    fn from(v: u64) -> Self {
        ScalarVal::Int(v as i64)
    }
}
impl From<usize> for ScalarVal {
    fn from(v: usize) -> Self {
        ScalarVal::Int(v as i64)
    }
}
impl From<f64> for ScalarVal {
    fn from(v: f64) -> Self {
        ScalarVal::Float(v)
    }
}
impl From<String> for ScalarVal {
    fn from(v: String) -> Self {
        ScalarVal::Str(v)
    }
}
impl From<&str> for ScalarVal {
    fn from(v: &str) -> Self {
        ScalarVal::Str(v.to_string())
    }
}
impl From<bool> for ScalarVal {
    fn from(v: bool) -> Self {
        ScalarVal::Bool(v)
    }
}

impl ScalarVal {
    /// The sort key for `set_ops sort by=<scalar>`. A missing/non-numeric
    /// scalar sorts LAST rather than as zero — same law as `blob`.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            ScalarVal::Int(i) => Some(*i as f64),
            ScalarVal::Float(f) => Some(*f),
            ScalarVal::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            ScalarVal::Str(_) => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Params
// ---------------------------------------------------------------------------

/// Typed parameter kinds. `path`/`symbol`/`ref` are STRING shapes with
/// their own validation (a `ref` goes through `git::revspec::Revspec`, so
/// invariant 3's "a validated ref is a TYPE" holds for a recipe-supplied
/// ref exactly as it does for a hand-typed one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamType {
    String,
    Int,
    Float,
    Bool,
    Enum,
    Path,
    Symbol,
    Ref,
}

impl ParamType {
    pub fn as_str(self) -> &'static str {
        match self {
            ParamType::String => "string",
            ParamType::Int => "int",
            ParamType::Float => "float",
            ParamType::Bool => "bool",
            ParamType::Enum => "enum",
            ParamType::Path => "path",
            ParamType::Symbol => "symbol",
            ParamType::Ref => "ref",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParamSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: ParamType,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<ArgVal>,
    /// Inclusive lower bound for `int`/`float`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// Inclusive upper bound for `int`/`float`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Closed value set for `enum`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// A literal argument value, deserialized identically from TOML and JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ArgVal {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<String>),
}

impl ArgVal {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            ArgVal::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            ArgVal::Int(i) => Some(*i),
            ArgVal::Float(f) => Some(*f as i64),
            ArgVal::Bool(b) => Some(*b as i64),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            ArgVal::Int(i) => Some(*i as f64),
            ArgVal::Float(f) => Some(*f),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            ArgVal::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn render(&self) -> String {
        match self {
            ArgVal::Bool(b) => b.to_string(),
            ArgVal::Int(i) => i.to_string(),
            ArgVal::Float(f) => f.to_string(),
            ArgVal::Str(s) => s.clone(),
            ArgVal::List(v) => v.join(","),
        }
    }
}

// ---------------------------------------------------------------------------
// Steps + views
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepSpec {
    pub id: String,
    pub op: ops::Op,
    #[serde(default)]
    pub args: BTreeMap<String, ArgVal>,
    /// Short prose shown beside the step's census. Optional.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewKind {
    List,
    Table,
    Tree,
    Graph,
}

impl ViewKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ViewKind::List => "list",
            ViewKind::Table => "table",
            ViewKind::Tree => "tree",
            ViewKind::Graph => "graph",
        }
    }
}

/// What one column renders. Closed by construction: an address field, or
/// a scalar the emitting op derived from that same address. There is no
/// variant that could render something else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColField {
    Path,
    Line,
    Symbol,
    Entity,
    Commit,
    Id,
    Blob,
    Trust,
    Kind,
    /// The whole address, rendered as its canonical string.
    Address,
    /// `scalar = "<name>"` — one of the emitting op's own derived numbers.
    Scalar(String),
}

impl ColField {
    pub fn as_label(&self) -> String {
        match self {
            ColField::Path => "path".into(),
            ColField::Line => "line".into(),
            ColField::Symbol => "symbol".into(),
            ColField::Entity => "entity".into(),
            ColField::Commit => "commit".into(),
            ColField::Id => "id".into(),
            ColField::Blob => "blob".into(),
            ColField::Trust => "trust".into(),
            ColField::Kind => "kind".into(),
            ColField::Address => "address".into(),
            ColField::Scalar(s) => s.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColumnSpec {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub header: String,
    pub field: ColField,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewSpec {
    pub id: String,
    pub kind: ViewKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// The step whose addresses this view renders.
    pub step: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<ColumnSpec>,
}

// ---------------------------------------------------------------------------
// The document
// ---------------------------------------------------------------------------

/// The closed set of intent groups a recipe home renders as sections
/// (D11: "a recipe home with intent groups that pre-fill the scope chip
/// and print the CLI line"). Closed so the home cannot grow a section per
/// author's mood; a recipe naming an unknown group is a LOAD error.
pub const INTENT_GROUPS: &[&str] = &[
    "orienting",
    "reviewing",
    "checking-tests",
    "rails",
    "hygiene",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeDoc {
    pub slug: String,
    pub title: String,
    /// One of [`INTENT_GROUPS`].
    pub intent: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description_md: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<ParamSpec>,
    /// A `kbc-scope/1` expression. Defaults to `$context.scope` — the
    /// scope chip the caller already has open — so a recipe inherits the
    /// reader's own narrowing rather than silently widening it.
    #[serde(default = "default_scope")]
    pub scope: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<StepSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub views: Vec<ViewSpec>,
    /// Marks a document as an adapter over one of the six `recipes/1`
    /// natives (see [`builtins::NATIVE_SLUGS`]). Only a BUILTIN may set
    /// it: a repo or server document naming a native is a load error,
    /// because a compiled-in body is not something a file can claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<String>,
}

fn default_scope() -> String {
    "$context.scope".to_string()
}

/// Which home a catalog row came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Home {
    Builtin,
    Repo,
    Server,
}

impl Home {
    pub fn as_str(self) -> &'static str {
        match self {
            Home::Builtin => "builtin",
            Home::Repo => "repo",
            Home::Server => "server",
        }
    }
}

/// Trust-on-first-use state for a repo-versioned recipe. `builtin` and
/// `server` documents are always [`TrustState::Trusted`] — the operator
/// installed the binary and the operator (or their agent, over loopback)
/// wrote the server row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustState {
    Trusted,
    /// Never seen before. Runnable only with an explicit accept.
    Untrusted,
    /// Trusted once, and the bytes have moved since. Runnable only after
    /// a re-accept; the DIFF is surfaced so the operator sees what moved.
    Changed,
}

impl TrustState {
    pub fn as_str(self) -> &'static str {
        match self {
            TrustState::Trusted => "trusted",
            TrustState::Untrusted => "untrusted",
            TrustState::Changed => "changed",
        }
    }
    pub fn runnable(self) -> bool {
        matches!(self, TrustState::Trusted)
    }
}

/// A loaded recipe: the document plus where it came from and what it is
/// worth. Constructed only by [`load`] / [`loader`] / [`builtins`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoadedRecipe {
    #[serde(flatten)]
    pub doc: RecipeDoc,
    pub home: Home,
    /// `repo:<path>@<blob>` for a repo file, `server` / `builtin`
    /// otherwise — D11's own `source` vocabulary.
    pub source: String,
    pub trust: TrustState,
    /// The unified diff between the trusted bytes and the current ones.
    /// Present only when `trust == changed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust_diff: Option<String>,
    /// A server row this repo file shadows, reported rather than dropped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadowed_by: Option<Home>,
}

// ---------------------------------------------------------------------------
// Load + type-check
// ---------------------------------------------------------------------------

/// A load failure names the STEP (or param, or view) it failed on. A
/// recipe that will not load must say which line to fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError {
    pub at: String,
    pub message: String,
}

impl LoadError {
    pub fn new(at: impl Into<String>, message: impl Into<String>) -> Self {
        LoadError {
            at: at.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.at, self.message)
    }
}

/// A parsed step argument: a literal, or a reference into the params, the
/// request context, or an earlier step.
#[derive(Debug, Clone, PartialEq)]
pub enum ArgRef {
    Literal(ArgVal),
    Param(String),
    Context(String),
    /// `$steps.<id>` — the whole address set of an earlier step.
    Step(String),
}

/// Parse one raw argument. A `$`-prefixed STRING is a reference; anything
/// else is a literal. There is no escape hatch and no interpolation —
/// `"$p.x and $p.y"` is a literal string, not a template, because a
/// template would be the beginning of a query language (D21).
pub fn parse_arg(raw: &ArgVal) -> Result<ArgRef, String> {
    let Some(s) = raw.as_str() else {
        return Ok(ArgRef::Literal(raw.clone()));
    };
    let Some(rest) = s.strip_prefix('$') else {
        return Ok(ArgRef::Literal(raw.clone()));
    };
    if let Some(name) = rest.strip_prefix("p.") {
        // A param NAME, not a template: `"$p.a and $p.b"` is a malformed
        // reference, never a param literally called `a and $p.b`. There is
        // no interpolation here, because a template would be the beginning
        // of a query language (D21).
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!(
                "`$p.{name}` is not a param reference — param names are [A-Za-z0-9_] and there \
                 is no interpolation"
            ));
        }
        return Ok(ArgRef::Param(name.to_string()));
    }
    if let Some(field) = rest.strip_prefix("context.") {
        if !CONTEXT_FIELDS.contains(&field) {
            return Err(format!(
                "unknown `$context.{field}`; known: {}",
                CONTEXT_FIELDS.join(", ")
            ));
        }
        return Ok(ArgRef::Context(field.to_string()));
    }
    if let Some(id) = rest.strip_prefix("steps.") {
        // `$steps.<id>` only. A `$steps.<id>.<field>` projection would be
        // a second, weaker `map` op with no type-check; use `map` (whose
        // transforms ARE type-checked) instead.
        if id.is_empty() || id.contains('.') {
            return Err(
                "`$steps.<id>` names a whole step; use the `map` op to project a field".into(),
            );
        }
        return Ok(ArgRef::Step(id.to_string()));
    }
    Err(format!(
        "unknown reference `${rest}`; expected `$p.<name>`, `$context.<field>` or `$steps.<id>`"
    ))
}

/// The `$context` magic params a caller may supply (`ctx.<name>=` on the
/// wire). Closed: `$context` is the READER's current position, not an
/// arbitrary bag.
pub const CONTEXT_FIELDS: &[&str] = &["repo", "path", "symbol", "ref", "scope"];

/// Parse + type-check a document. This is the ONE door: `builtins`, the
/// repo loader and `recipe new` all come through here, so a recipe that
/// can be listed is a recipe that can be run.
pub fn load(doc: RecipeDoc) -> Result<RecipeDoc, LoadError> {
    if doc.slug.is_empty() || !doc.slug.chars().all(is_slug_char) {
        return Err(LoadError::new(
            "slug",
            format!(
                "slug {:?} must be non-empty and made of [a-z0-9:_-] (an intent prefix like \
                 `orient:` is allowed)",
                doc.slug
            ),
        ));
    }
    if doc.title.trim().is_empty() {
        return Err(LoadError::new("title", "title must not be empty"));
    }
    if !INTENT_GROUPS.contains(&doc.intent.as_str()) {
        return Err(LoadError::new(
            "intent",
            format!(
                "unknown intent group {:?}; known: {}",
                doc.intent,
                INTENT_GROUPS.join(", ")
            ),
        ));
    }
    if doc.params.len() > MAX_PARAMS {
        return Err(LoadError::new(
            "params",
            format!(
                "{} params exceeds the cap of {MAX_PARAMS}",
                doc.params.len()
            ),
        ));
    }
    let mut seen_params: Vec<&str> = Vec::new();
    for p in &doc.params {
        if p.name.is_empty()
            || !p
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(LoadError::new(
                format!("params.{}", p.name),
                "param names are [A-Za-z0-9_]",
            ));
        }
        if seen_params.contains(&p.name.as_str()) {
            return Err(LoadError::new(
                format!("params.{}", p.name),
                "duplicate param name",
            ));
        }
        seen_params.push(&p.name);
        if p.ty == ParamType::Enum && p.values.is_empty() {
            return Err(LoadError::new(
                format!("params.{}", p.name),
                "an `enum` param must declare `values`",
            ));
        }
        if p.ty != ParamType::Enum && !p.values.is_empty() {
            return Err(LoadError::new(
                format!("params.{}", p.name),
                "`values` is only meaningful on an `enum` param",
            ));
        }
        if let (Some(min), Some(max)) = (p.min, p.max) {
            if min > max {
                return Err(LoadError::new(
                    format!("params.{}", p.name),
                    format!("min {min} is above max {max}"),
                ));
            }
        }
        if p.required && p.default.is_some() {
            return Err(LoadError::new(
                format!("params.{}", p.name),
                "a required param may not also carry a default",
            ));
        }
    }

    // A native adapter carries no steps of its own — its body is compiled
    // in. Everything else must have at least one step, or it is a catalog
    // row that answers nothing (the dead-surface class).
    if let Some(native) = &doc.native {
        if !builtins::NATIVE_SLUGS.contains(&native.as_str()) {
            return Err(LoadError::new(
                "native",
                format!(
                    "unknown native recipe {native:?}; known: {}",
                    builtins::NATIVE_SLUGS.join(", ")
                ),
            ));
        }
        if !doc.steps.is_empty() {
            return Err(LoadError::new(
                "steps",
                "a `native` recipe's body is compiled in and may declare no steps",
            ));
        }
        return Ok(doc);
    }

    if doc.steps.is_empty() {
        return Err(LoadError::new("steps", "a recipe must declare a step"));
    }
    if doc.steps.len() > MAX_STEPS {
        return Err(LoadError::new(
            "steps",
            format!("{} steps exceeds the cap of {MAX_STEPS}", doc.steps.len()),
        ));
    }
    if doc.views.len() > MAX_VIEWS {
        return Err(LoadError::new(
            "views",
            format!("{} views exceeds the cap of {MAX_VIEWS}", doc.views.len()),
        ));
    }

    // --- the DAG type-check ------------------------------------------------
    let mut kinds: BTreeMap<String, AddrKind> = BTreeMap::new();
    for step in &doc.steps {
        let at = format!("steps.{}", step.id);
        if step.id.is_empty() || !step.id.chars().all(is_step_id_char) {
            return Err(LoadError::new(&at, "step ids are [a-z0-9_-]"));
        }
        if kinds.contains_key(&step.id) {
            return Err(LoadError::new(&at, "duplicate step id"));
        }
        let spec = step.op.spec();

        // Args: known keys only, references resolvable, and every
        // step-reference's OUTPUT kind must be one the op accepts.
        let mut input_kinds: Vec<(String, AddrKind)> = Vec::new();
        for (k, v) in &step.args {
            if !spec.args.contains(&k.as_str()) {
                return Err(LoadError::new(
                    &at,
                    format!(
                        "unknown arg {k:?} for op `{}`; known: {}",
                        step.op.as_str(),
                        spec.args.join(", ")
                    ),
                ));
            }
            let arg = parse_arg(v).map_err(|e| LoadError::new(&at, format!("arg {k:?}: {e}")))?;
            match &arg {
                ArgRef::Param(name) => {
                    if !doc.params.iter().any(|p| &p.name == name) {
                        return Err(LoadError::new(
                            &at,
                            format!("arg {k:?} references undeclared param `$p.{name}`"),
                        ));
                    }
                }
                ArgRef::Step(id) => {
                    let Some(kind) = kinds.get(id) else {
                        return Err(LoadError::new(
                            &at,
                            format!(
                                "arg {k:?} references `$steps.{id}`, which is not a step \
                                 declared BEFORE this one (the DAG is the declaration order)"
                            ),
                        ));
                    };
                    input_kinds.push((k.clone(), *kind));
                }
                ArgRef::Context(_) | ArgRef::Literal(_) => {}
            }
        }
        for required in spec.required_args {
            if !step.args.contains_key(*required) {
                return Err(LoadError::new(
                    &at,
                    format!("op `{}` requires arg {required:?}", step.op.as_str()),
                ));
            }
        }
        let out = step
            .op
            .output_kind(&step.args, &input_kinds)
            .map_err(|e| LoadError::new(&at, e))?;
        kinds.insert(step.id.clone(), out);
    }

    let mut seen_views: Vec<&str> = Vec::new();
    for view in &doc.views {
        let at = format!("views.{}", view.id);
        if view.id.is_empty() || !view.id.chars().all(is_step_id_char) {
            return Err(LoadError::new(&at, "view ids are [a-z0-9_-]"));
        }
        if seen_views.contains(&view.id.as_str()) {
            return Err(LoadError::new(&at, "duplicate view id"));
        }
        seen_views.push(&view.id);
        if !kinds.contains_key(&view.step) {
            return Err(LoadError::new(
                &at,
                format!("view renders `{}`, which is not a declared step", view.step),
            ));
        }
        if view.columns.is_empty() {
            return Err(LoadError::new(
                &at,
                "a view must declare at least one column (every cell is an address)",
            ));
        }
    }

    Ok(doc)
}

fn is_slug_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' || c == ':'
}

fn is_step_id_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'
}

/// Parse a recipe document out of TOML (the `.kbc/recipes/*.toml` shape)
/// and type-check it in one step.
pub fn load_toml(text: &str) -> Result<RecipeDoc, LoadError> {
    let doc: RecipeDoc =
        toml::from_str(text).map_err(|e| LoadError::new("toml", e.message().to_string()))?;
    load(doc)
}

/// Parse a recipe document out of JSON (the `recipe new --from-json -`
/// shape) and type-check it in one step.
pub fn load_json(value: serde_json::Value) -> Result<RecipeDoc, LoadError> {
    let doc: RecipeDoc =
        serde_json::from_value(value).map_err(|e| LoadError::new("json", e.to_string()))?;
    load(doc)
}

#[cfg(test)]
mod tests;
