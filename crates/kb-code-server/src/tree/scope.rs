//! V71-F1 — `kbc-scope/1`: the named path-set algebra the tree filter, the
//! tree projections and `kb-code tree --scope` all resolve through.
//!
//! ## One grammar, not a second query language
//!
//! The evidence report's risk 2 is explicit: "If the tree's filter grammar
//! diverges from the search grammar by even one atom, the operator now has
//! two DSLs and neither is memorable." So the SHARED atom keys
//! ([`KBCQ_SHARED_KEYS`] — `path:`, `ext:`, `lang:`) are not re-parsed here:
//! [`atom_from_token`] hands the token to kbcq/1's own
//! [`crate::search::grammar::parse`] and reads the answer back out of its
//! typed [`Filters`](crate::search::grammar::Filters). What this module adds
//! is (a) the BOOLEAN structure kbcq/1 has no notion of (`&&`, `||`, `!`,
//! parentheses, `$name` references) and (b) the tree-only atom keys declared
//! in [`SCOPE_ATOM_SPECS`], each of which names the resolver expression that
//! must read it.
//!
//! The tree-only keys are deliberately NOT added to kbcq/1's `FILTER_SPECS`.
//! `FILTER_SPECS` is that grammar's declaration home and every key in it
//! must have a consumer in the SEARCH lanes (`grammar.rs`'s
//! `every_declared_filter_key_has_a_consumer`); a `role:` key that parsed
//! cleanly in the search box and then did nothing there would be exactly the
//! v7.0 dead-surface defect, one layer up. Typed into the search box today,
//! `role:model` is an unknown key: kbcq/1 warns by name and searches it as a
//! word. Loud, not silent. Widening SEARCH to honour these atoms as
//! pre-filters is P3's own remaining work and is named as cut in this unit's
//! commit message.
//!
//! ## Parsing is total; RESOLUTION is what refuses
//!
//! [`parse`] never fails — every input yields a [`ParsedScope`] with
//! diagnostics. But a scope is a SET, and an atom silently dropped from a
//! set expression changes which files you are looking at without saying so.
//! So a parse that produced any diagnostic yields `expr: None`, and
//! [`resolve`] then REFUSES: the caller renders the tree UNSCOPED with the
//! refusal captioned on the wire (`applied: false` + a note naming the
//! token). An honest "I did not apply your scope, here is why" beats both a
//! silently different set and an empty tree — the same choice the projection
//! engine makes with its `Unplaced` bucket.
//!
//! ## Sources are DATA
//!
//! [`ScopeSources`] is plain owned data — no `Store`, no `AppState`, no
//! filesystem. `tree::mod` builds one from the store, reading ONLY the lanes
//! the expression actually mentions ([`Expr::atom_keys`]), and this module
//! stays a pure function of it, unit-testable without a database.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::search::grammar::{self, Diagnostic};

/// The wire/schema name, as it appears on every response that carries a
/// resolved scope.
pub const SCOPE_SCHEMA: &str = "kbc-scope/1";

/// Longest expression this parser will look at. A scope arrives on a query
/// string; bound it like every other caller-controlled parse in this crate.
pub const MAX_EXPR_BYTES: usize = 4096;

/// Most atoms one expression may contain (references expand into this
/// budget too, which is what stops a `$a` → `$b` → `$a` cycle from being
/// the only thing that bounds the walk).
pub const MAX_ATOMS: usize = 64;

/// How deep a `$name` chain may nest before the resolver refuses.
pub const MAX_REF_DEPTH: usize = 4;

/// Atom keys kbcq/1 already owns. A token whose key is in this list is
/// parsed by [`crate::search::grammar::parse`] and read back out of its
/// `Filters`, never re-parsed here — the "one grammar" half of the module
/// doc.
pub const KBCQ_SHARED_KEYS: &[&str] = &["path", "ext", "lang"];

/// One tree-only atom key's DECLARATION. `resolver_expr` is the Rust
/// expression [`tests::every_declared_scope_atom_is_resolved`] requires to
/// appear in this module's own resolver — the same source-scan shape
/// `grammar.rs` uses for `FILTER_SPECS`, with the same stated limits (it
/// proves the expression occurs, not that it is reached), backed here by a
/// second, FUNCTIONAL probe that runs every key through [`resolve`] against
/// a fixture and asserts it changes the resolved set.
#[derive(Debug, Clone, Copy)]
pub struct ScopeAtomSpec {
    pub key: &'static str,
    /// The closed value vocabulary, or `None` for a free-form value.
    pub values: Option<&'static [&'static str]>,
    /// Which [`ScopeSources`] lane the resolver must consult.
    pub source: &'static str,
    pub resolver_expr: &'static str,
    pub note: &'static str,
}

/// Presence value every "does this path have any of these" atom accepts.
pub const ANY: &str = "any";

/// The tree-only atom vocabulary. Every entry is resolved by
/// [`eval_atom`]; adding one without wiring it fails this module's own
/// dead-surface tests, by name.
pub const SCOPE_ATOM_SPECS: &[ScopeAtomSpec] = &[
    ScopeAtomSpec {
        key: "role",
        values: Some(super::roles::ROLES),
        source: "path convention",
        resolver_expr: "roles::role_for_path",
        note: "a Rails path convention — capped at `likely`, never `exact`",
    },
    ScopeAtomSpec {
        key: "ns",
        values: None,
        source: "entity_defs",
        resolver_expr: "src.ns_by_path",
        note: "a constant path and its descendants, from the V71-G0 entity index",
    },
    ScopeAtomSpec {
        key: "pack",
        values: None,
        source: "packwerk package.yml",
        resolver_expr: "src.packs",
        note: "the DEEPEST Packwerk package containing the file",
    },
    ScopeAtomSpec {
        key: "owner",
        values: None,
        source: "CODEOWNERS",
        resolver_expr: "src.owners",
        note: "GitHub's last-matching-pattern-wins semantics",
    },
    ScopeAtomSpec {
        key: "set",
        values: None,
        source: "reading_sets",
        resolver_expr: "src.sets",
        note: "a reading set's member paths, by id or title",
    },
    ScopeAtomSpec {
        key: "annot",
        // `open` ONLY: the lane this resolves against is the OPEN
        // annotation set, so accepting `annot:any` would mean one thing
        // and select another. A closed vocabulary that refuses is better
        // than one that quietly redefines a word.
        values: Some(&["open"]),
        source: "annotations",
        resolver_expr: "src.annot_open",
        note: "`annot:open` — a path carrying an unresolved annotation",
    },
    ScopeAtomSpec {
        key: "todo",
        values: Some(&[ANY]),
        source: "comments",
        resolver_expr: "src.todo",
        note: "`todo:any` — a path with an indexed TODO/FIXME",
    },
    ScopeAtomSpec {
        key: "bookmark",
        values: Some(&[ANY]),
        source: "bookmarks",
        resolver_expr: "src.bookmark",
        note: "`bookmark:any` — a path carrying a bookmark",
    },
];

/// Atoms the design's §2.2 vocabulary names that this milestone does NOT
/// resolve, each with the reason. The E1 `UNMINTED_KINDS` precedent: the
/// ledger is the anti-dead-surface device, and
/// [`tests::the_unresolved_ledger_and_the_spec_table_are_disjoint`] fails
/// BOTH ways — a key that gained a resolver but kept its ledger entry, and
/// a key declared in both places.
pub const UNRESOLVED_ATOMS: &[(&str, &str)] = &[
    (
        "review",
        "a review's file set needs a patchset range diff per evaluation; the tree's \
         `review` DECORATION lane covers the surfaced half (V71-F1), the scope atom waits",
    ),
    (
        "finding",
        "same as `review:` — the severity lane is a decoration here, not yet a set",
    ),
    (
        "session",
        "session→path attribution lives in `session_signals`; it is a RANKING signal \
         today and would need a stated freshness rule before it selects a set",
    ),
    (
        "since",
        "a time window over the mirror index is not a path set the tree can resolve \
         without a git walk per evaluation (the `change` PROJECTION does that walk once)",
    ),
    (
        "churn",
        "behavioral counters exist (`path_stats`) but the comparison grammar (`>10`) \
         is a second operator vocabulary; deferred with the ranked `--budget` map",
    ),
    (
        "diag",
        "diagnostics are computed-fresh-never-persisted (lsp-live); a set built from \
         them would be stale the moment it was stored",
    ),
    (
        "sym",
        "a single symbol is an ADDRESS, not a path set — `?sym=` already answers it",
    ),
];

// ── the expression ────────────────────────────────────────────────────────

/// A parsed scope expression. Deliberately not `Deserialize`: the only door
/// is [`parse`] (kbcq/1's `ParsedQuery` makes the same choice, for the same
/// reason — a client-supplied AST would be an unvalidated second entrance).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Atom { key: String, value: String },
    Ref(String),
    Not(Box<Expr>),
    And(Vec<Expr>),
    Or(Vec<Expr>),
}

impl Expr {
    /// Every atom key this expression mentions, `$name` references
    /// EXCLUDED (a reference's own keys are only knowable after it is
    /// looked up — [`resolve`] expands them and re-reads).
    pub fn atom_keys(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        self.walk_keys(&mut out);
        out
    }

    fn walk_keys(&self, out: &mut BTreeSet<String>) {
        match self {
            Expr::Atom { key, .. } => {
                out.insert(key.clone());
            }
            Expr::Ref(_) => {}
            Expr::Not(inner) => inner.walk_keys(out),
            Expr::And(xs) | Expr::Or(xs) => {
                for x in xs {
                    x.walk_keys(out);
                }
            }
        }
    }

    /// Every `$name` this expression references, at THIS level.
    pub fn refs(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        self.walk_refs(&mut out);
        out
    }

    fn walk_refs(&self, out: &mut BTreeSet<String>) {
        match self {
            Expr::Atom { .. } => {}
            Expr::Ref(n) => {
                out.insert(n.clone());
            }
            Expr::Not(inner) => inner.walk_refs(out),
            Expr::And(xs) | Expr::Or(xs) => {
                for x in xs {
                    x.walk_refs(out);
                }
            }
        }
    }

    fn atom_count(&self) -> usize {
        match self {
            Expr::Atom { .. } | Expr::Ref(_) => 1,
            Expr::Not(inner) => inner.atom_count(),
            Expr::And(xs) | Expr::Or(xs) => xs.iter().map(Expr::atom_count).sum(),
        }
    }

    /// The canonical re-rendering — a fixed point under re-parsing, pinned
    /// by [`tests::normalize_is_a_fixed_point`]. Parenthesised at every
    /// nesting level rather than minimally, because a scope is pasted
    /// between surfaces and an unparenthesised re-render that re-associates
    /// differently is a silently different set.
    pub fn normalize(&self) -> String {
        match self {
            Expr::Atom { key, value } => format!("{key}:{value}"),
            Expr::Ref(n) => format!("${n}"),
            Expr::Not(inner) => format!("!{}", inner.normalize()),
            Expr::And(xs) => join(xs, " && "),
            Expr::Or(xs) => join(xs, " || "),
        }
    }
}

fn join(xs: &[Expr], sep: &str) -> String {
    let parts: Vec<String> = xs
        .iter()
        .map(|x| match x {
            Expr::And(_) | Expr::Or(_) => format!("({})", x.normalize()),
            _ => x.normalize(),
        })
        .collect();
    parts.join(sep)
}

/// [`parse`]'s output. `expr` is `None` whenever ANY diagnostic fired — see
/// the module doc's "parsing is total; resolution is what refuses".
#[derive(Debug, Clone)]
pub struct ParsedScope {
    pub expr: Option<Expr>,
    pub diagnostics: Vec<Diagnostic>,
    /// The canonical rendering of `expr`, or the trimmed input when the
    /// parse refused (so an error message can echo what the author wrote).
    pub normalized: String,
}

fn warn(token: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        severity: "warning".to_string(),
        token: token.to_string(),
        message: message.into(),
        suggestion: None,
    }
}

/// Tokenise + recursive-descent parse. Total: never panics, never errors.
///
/// Grammar (juxtaposition is AND, matching kbcq/1's own space-separated
/// filters, so `role:spec ns:Reseller` means the same thing in both boxes):
///
/// ```text
/// expr  := or
/// or    := and ( '||' and )*
/// and   := unary ( '&&'? unary )*
/// unary := '!' unary | '(' or ')' | atom
/// atom  := '$' NAME | KEY ':' VALUE
/// ```
pub fn parse(raw: &str) -> ParsedScope {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return ParsedScope {
            expr: None,
            diagnostics: Vec::new(),
            normalized: String::new(),
        };
    }
    if trimmed.len() > MAX_EXPR_BYTES {
        return ParsedScope {
            expr: None,
            diagnostics: vec![warn(
                "<expression>",
                format!("scope expression exceeds {MAX_EXPR_BYTES} bytes"),
            )],
            normalized: trimmed.to_string(),
        };
    }

    let tokens = tokenize(trimmed);
    let mut p = Parser {
        tokens: &tokens,
        pos: 0,
        diagnostics: Vec::new(),
    };
    let expr = p.parse_or();
    if p.pos < p.tokens.len() {
        let tok = p.tokens[p.pos].clone();
        p.diagnostics
            .push(warn(&tok, "unexpected token — unbalanced parentheses?"));
    }
    let mut diagnostics = p.diagnostics;
    let expr = match expr {
        Some(e) if diagnostics.is_empty() => {
            if e.atom_count() > MAX_ATOMS {
                diagnostics.push(warn(
                    "<expression>",
                    format!("more than {MAX_ATOMS} atoms in one scope expression"),
                ));
                None
            } else {
                Some(e)
            }
        }
        _ => None,
    };
    let normalized = expr
        .as_ref()
        .map(Expr::normalize)
        .unwrap_or_else(|| trimmed.to_string());
    ParsedScope {
        expr,
        diagnostics,
        normalized,
    }
}

/// Split into the parser's tokens: `(`, `)`, `&&`, `||`, `!`, and words.
/// A word runs to whitespace or one of those punctuators; a double-quoted
/// run is one word verbatim (so `path:"app/my dir//*"` survives).
fn tokenize(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes: Vec<char> = raw.chars().collect();
    let mut i = 0usize;
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        if !cur.is_empty() {
            out.push(std::mem::take(cur));
        }
    };
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            c if c.is_whitespace() => {
                flush(&mut cur, &mut out);
                i += 1;
            }
            '(' | ')' | '!' => {
                flush(&mut cur, &mut out);
                out.push(c.to_string());
                i += 1;
            }
            '&' if bytes.get(i + 1) == Some(&'&') => {
                flush(&mut cur, &mut out);
                out.push("&&".to_string());
                i += 2;
            }
            '|' if bytes.get(i + 1) == Some(&'|') => {
                flush(&mut cur, &mut out);
                out.push("||".to_string());
                i += 2;
            }
            '"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != '"' {
                    cur.push(bytes[i]);
                    i += 1;
                }
                i += 1; // closing quote (or EOF — an unterminated quote just ends the word)
            }
            _ => {
                cur.push(c);
                i += 1;
            }
        }
    }
    flush(&mut cur, &mut out);
    out
}

struct Parser<'a> {
    tokens: &'a [String],
    pos: usize,
    diagnostics: Vec<Diagnostic>,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.pos).map(String::as_str)
    }

    fn parse_or(&mut self) -> Option<Expr> {
        let first = self.parse_and()?;
        let mut xs = vec![first];
        while self.peek() == Some("||") {
            self.pos += 1;
            match self.parse_and() {
                Some(e) => xs.push(e),
                None => {
                    self.diagnostics
                        .push(warn("||", "`||` with nothing on its right"));
                    return None;
                }
            }
        }
        Some(if xs.len() == 1 {
            xs.pop().expect("len 1")
        } else {
            Expr::Or(xs)
        })
    }

    fn parse_and(&mut self) -> Option<Expr> {
        let first = self.parse_unary()?;
        let mut xs = vec![first];
        loop {
            if self.peek() == Some("&&") {
                self.pos += 1;
                match self.parse_unary() {
                    Some(e) => xs.push(e),
                    None => {
                        self.diagnostics
                            .push(warn("&&", "`&&` with nothing on its right"));
                        return None;
                    }
                }
                continue;
            }
            // Juxtaposition = AND. Stop at anything that cannot START a
            // unary (a closing paren, an operator).
            match self.peek() {
                Some(")") | Some("||") | None => break,
                Some(_) => match self.parse_unary() {
                    Some(e) => xs.push(e),
                    None => return None,
                },
            }
        }
        Some(if xs.len() == 1 {
            xs.pop().expect("len 1")
        } else {
            Expr::And(xs)
        })
    }

    fn parse_unary(&mut self) -> Option<Expr> {
        match self.peek() {
            Some("!") => {
                self.pos += 1;
                let inner = self.parse_unary()?;
                Some(Expr::Not(Box::new(inner)))
            }
            Some("(") => {
                self.pos += 1;
                let inner = self.parse_or()?;
                if self.peek() != Some(")") {
                    self.diagnostics.push(warn("(", "unclosed `(`"));
                    return None;
                }
                self.pos += 1;
                Some(inner)
            }
            Some(")") | Some("&&") | Some("||") => {
                let tok = self.peek().unwrap_or("").to_string();
                self.diagnostics
                    .push(warn(&tok, "operator where an atom was expected"));
                None
            }
            None => {
                self.diagnostics
                    .push(warn("<end>", "expression ended where an atom was expected"));
                None
            }
            Some(word) => {
                let word = word.to_string();
                self.pos += 1;
                match atom_from_token(&word) {
                    Ok(e) => Some(e),
                    Err(d) => {
                        self.diagnostics.push(d);
                        None
                    }
                }
            }
        }
    }
}

/// One leaf token → an [`Expr`]. The "one grammar" seam: for a key in
/// [`KBCQ_SHARED_KEYS`] the token is handed to kbcq/1's own parser and the
/// value read back out of its typed `Filters`, so `path:`/`ext:`/`lang:`
/// cannot mean one thing in the search box and another in the tree.
pub fn atom_from_token(token: &str) -> Result<Expr, Diagnostic> {
    if let Some(name) = token.strip_prefix('$') {
        if name.is_empty() {
            return Err(warn(token, "`$` with no scope name"));
        }
        return Ok(Expr::Ref(name.to_string()));
    }
    // `-key:value` is kbcq/1's negation spelling; a scope expression spells
    // it `!key:value`, and accepting both keeps one muscle memory.
    if let Some(rest) = token.strip_prefix('-') {
        return atom_from_token(rest).map(|e| Expr::Not(Box::new(e)));
    }
    let Some((key, value)) = token.split_once(':') else {
        return Err(warn(
            token,
            "not an atom — a scope atom is `key:value`, `$name`, or a negation of one",
        ));
    };
    if value.is_empty() {
        return Err(warn(
            token,
            format!("`{key}:` with an empty value (use `{key}:{ANY}` for a presence test)"),
        ));
    }
    if KBCQ_SHARED_KEYS.contains(&key) {
        let parsed = grammar::parse(token);
        if !parsed.diagnostics.is_empty() {
            let d = parsed.diagnostics.into_iter().next().expect("non-empty");
            return Err(d);
        }
        // A shared key must have been CONSUMED by kbcq/1 (it left no query
        // text behind); if it did not, this module must not guess at it.
        if !parsed.query.is_empty() {
            return Err(warn(token, "kbcq/1 did not consume this token as a filter"));
        }
        let normalized_value = match key {
            "path" => parsed.filters.path.clone(),
            "lang" => parsed.filters.lang.clone(),
            "ext" => parsed.filters.ext.first().cloned(),
            _ => None,
        };
        let Some(v) = normalized_value else {
            return Err(warn(token, "kbcq/1 parsed this token into no value"));
        };
        return Ok(Expr::Atom {
            key: key.to_string(),
            value: v,
        });
    }
    let Some(spec) = SCOPE_ATOM_SPECS.iter().find(|s| s.key == key) else {
        if let Some((_, why)) = UNRESOLVED_ATOMS.iter().find(|(k, _)| *k == key) {
            return Err(warn(
                token,
                format!("`{key}:` is named by kbc-scope/1 but not resolved yet — {why}"),
            ));
        }
        return Err(warn(token, format!("unknown scope atom `{key}:`")));
    };
    if let Some(vocab) = spec.values {
        if !vocab.contains(&value) {
            return Err(warn(
                token,
                format!("`{key}:` accepts {vocab:?}, got {value:?}"),
            ));
        }
    }
    Ok(Expr::Atom {
        key: key.to_string(),
        value: value.to_string(),
    })
}

// ── resolution ────────────────────────────────────────────────────────────

/// One candidate file, as the resolver sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeFile {
    pub path: String,
    pub lang: String,
}

/// Everything [`resolve`] may consult — plain owned data, no `Store`, no
/// filesystem, no `AppState`. `tree::mod` fills only the lanes the
/// expression mentions (see [`Expr::atom_keys`]); an unfilled lane resolves
/// EMPTY, which is why the caller must not omit a lane the expression uses.
#[derive(Debug, Default, Clone)]
pub struct ScopeSources {
    pub files: Vec<ScopeFile>,
    /// path → every entity FQN defined in it (V71-G0's `entity_defs`).
    pub ns_by_path: HashMap<String, Vec<String>>,
    /// `(package directory, package name)`, DEEPEST-FIRST after
    /// [`ScopeSources::sort_packs`].
    pub packs: Vec<(String, String)>,
    /// CODEOWNERS rules in FILE ORDER — the last match wins (GitHub's own
    /// documented semantics), so order is load-bearing.
    pub owners: Vec<(String, Vec<String>)>,
    /// set id/title (lowercased) → member paths.
    pub sets: HashMap<String, HashSet<String>>,
    pub annot_open: HashSet<String>,
    pub todo: HashSet<String>,
    pub bookmark: HashSet<String>,
    /// `[scopes]` in `kb-code.toml` — the `$name` seeds, `source: config`.
    pub config_scopes: BTreeMap<String, Vec<String>>,
}

impl ScopeSources {
    /// Packwerk packages nest (`packs/a` inside `packs/a/b`), and a file
    /// belongs to the DEEPEST one containing it — so `pack:` matching walks
    /// this list and takes the first hit.
    pub fn sort_packs(&mut self) {
        self.packs
            .sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
    }
}

/// [`resolve`]'s answer.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// `false` = the scope was REFUSED and the caller must render unscoped,
    /// with `notes` shown. Never a silent narrowing.
    pub applied: bool,
    /// The matched path set, or `None` when the scope was refused.
    pub paths: Option<HashSet<String>>,
    pub normalized: String,
    pub notes: Vec<String>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Resolved {
    fn refused(normalized: String, diagnostics: Vec<Diagnostic>, note: String) -> Self {
        Self {
            applied: false,
            paths: None,
            normalized,
            notes: vec![note],
            diagnostics,
        }
    }
}

/// Resolve `raw` against `src`. An empty expression is `applied: false`
/// with no diagnostics — "no scope" is not a refusal.
pub fn resolve(raw: &str, src: &ScopeSources) -> Resolved {
    let parsed = parse(raw);
    let Some(expr) = parsed.expr else {
        if parsed.diagnostics.is_empty() {
            return Resolved {
                applied: false,
                paths: None,
                normalized: parsed.normalized,
                notes: Vec::new(),
                diagnostics: Vec::new(),
            };
        }
        let first = parsed
            .diagnostics
            .first()
            .map(|d| format!("{}: {}", d.token, d.message))
            .unwrap_or_else(|| "unparseable".to_string());
        return Resolved::refused(
            parsed.normalized,
            parsed.diagnostics,
            format!("scope not applied — {first}; showing the unscoped tree"),
        );
    };

    let mut notes = Vec::new();
    let expanded = match expand_refs(&expr, src, 0, &mut notes) {
        Ok(e) => e,
        Err(msg) => {
            return Resolved::refused(
                parsed.normalized,
                vec![warn("$", msg.clone())],
                format!("scope not applied — {msg}; showing the unscoped tree"),
            )
        }
    };

    let mut paths = HashSet::new();
    for f in &src.files {
        if eval(&expanded, f, src) {
            paths.insert(f.path.clone());
        }
    }
    if paths.is_empty() {
        notes.push(format!(
            "scope `{}` matched 0 of {} indexed files",
            parsed.normalized,
            src.files.len()
        ));
    }
    Resolved {
        applied: true,
        paths: Some(paths),
        normalized: parsed.normalized,
        notes,
        diagnostics: parsed.diagnostics,
    }
}

/// Replace every `$name` with the config scope it seeds, bounded by
/// [`MAX_REF_DEPTH`]. A config scope is a glob LIST, so it expands to an OR
/// of `path:` atoms — which is why a `$name` cannot smuggle in an atom key
/// the caller never asked for.
fn expand_refs(
    expr: &Expr,
    src: &ScopeSources,
    depth: usize,
    notes: &mut Vec<String>,
) -> Result<Expr, String> {
    if depth > MAX_REF_DEPTH {
        return Err(format!("`$` references nest deeper than {MAX_REF_DEPTH}"));
    }
    Ok(match expr {
        Expr::Atom { .. } => expr.clone(),
        Expr::Ref(name) => {
            let Some(globs) = src.config_scopes.get(name) else {
                let known: Vec<&str> = src.config_scopes.keys().map(String::as_str).collect();
                return Err(format!("unknown scope `${name}` (configured: {known:?})"));
            };
            if globs.is_empty() {
                return Err(format!("`${name}` is configured with no patterns"));
            }
            notes.push(format!(
                "`${name}` resolved from `[scopes]` in kb-code.toml (source: config)"
            ));
            Expr::Or(
                globs
                    .iter()
                    .map(|g| Expr::Atom {
                        key: "path".to_string(),
                        value: g.clone(),
                    })
                    .collect(),
            )
        }
        Expr::Not(inner) => Expr::Not(Box::new(expand_refs(inner, src, depth + 1, notes)?)),
        Expr::And(xs) => Expr::And(
            xs.iter()
                .map(|x| expand_refs(x, src, depth + 1, notes))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Expr::Or(xs) => Expr::Or(
            xs.iter()
                .map(|x| expand_refs(x, src, depth + 1, notes))
                .collect::<Result<Vec<_>, _>>()?,
        ),
    })
}

fn eval(expr: &Expr, f: &ScopeFile, src: &ScopeSources) -> bool {
    match expr {
        Expr::Atom { key, value } => eval_atom(key, value, f, src),
        // Every `Ref` is expanded before `eval` runs; an unexpanded one is
        // a resolver bug, and answering `false` (rather than panicking on a
        // request path) keeps it a visibly empty set rather than a 500.
        Expr::Ref(_) => false,
        Expr::Not(inner) => !eval(inner, f, src),
        Expr::And(xs) => xs.iter().all(|x| eval(x, f, src)),
        Expr::Or(xs) => xs.iter().any(|x| eval(x, f, src)),
    }
}

/// The ONE place an atom decides whether a path is in the set. Every
/// [`SCOPE_ATOM_SPECS`] entry's `resolver_expr` must appear in this
/// function's module (the dead-surface scan) AND change the resolved set in
/// the functional probe.
fn eval_atom(key: &str, value: &str, f: &ScopeFile, src: &ScopeSources) -> bool {
    match key {
        "path" => path_atom_matches(value, &f.path),
        "ext" => {
            let v = value.trim_start_matches('.').to_lowercase();
            f.path.to_lowercase().ends_with(&format!(".{v}"))
        }
        "lang" => f.lang.eq_ignore_ascii_case(value),
        "role" => super::roles::role_for_path(&f.path) == value,
        "ns" => src
            .ns_by_path
            .get(&f.path)
            .is_some_and(|fqns| fqns.iter().any(|fqn| ns_covers(value, fqn))),
        "pack" => pack_of(&f.path, &src.packs).is_some_and(|name| name == value),
        "owner" => owner_of(&f.path, &src.owners).iter().any(|o| o == value),
        "set" => src
            .sets
            .get(&value.to_lowercase())
            .is_some_and(|paths| paths.contains(&f.path)),
        "annot" => src.annot_open.contains(&f.path),
        "todo" => src.todo.contains(&f.path),
        "bookmark" => src.bookmark.contains(&f.path),
        // Unreachable while `atom_from_token` and this match agree — a key
        // that reached here without an arm selects NOTHING rather than
        // everything, so a wiring slip narrows visibly instead of widening
        // silently.
        _ => false,
    }
}

/// `ns:Reseller` covers `Reseller` and every `Reseller::…` descendant;
/// `ns:Reseller::Order` covers exactly that constant and its own
/// descendants. Never a bare-suffix match — G0's finding is that a bare
/// constant name is not an identity.
pub fn ns_covers(scope_fqn: &str, candidate: &str) -> bool {
    candidate == scope_fqn || candidate.starts_with(&format!("{scope_fqn}::"))
}

/// JetBrains' path wildcards, plus this crate's own glob shapes:
/// `dir//*` = recursive, `dir/*` = this level only, otherwise
/// [`crate::scopes::path_matches_scope_glob`] (which already covers
/// `**/x/**`, `prefix/**`, `*.ext`, …) with a plain directory PREFIX as the
/// final fallback.
pub fn path_atom_matches(pattern: &str, path: &str) -> bool {
    if let Some(dir) = pattern.strip_suffix("//*") {
        let dir = dir.trim_end_matches('/');
        return dir.is_empty() || path == dir || path.starts_with(&format!("{dir}/"));
    }
    if let Some(dir) = pattern.strip_suffix("/*") {
        let dir = dir.trim_end_matches('/');
        let Some(rest) = path.strip_prefix(&format!("{dir}/")) else {
            return dir.is_empty() && !path.contains('/');
        };
        return !rest.contains('/');
    }
    let basename = path.rsplit('/').next().unwrap_or(path);
    if crate::scopes::path_matches_scope_glob(path, basename, pattern) {
        return true;
    }
    // A bare directory or file prefix — the shape an operator types first.
    let p = pattern.trim_end_matches('/');
    !p.is_empty() && (path == p || path.starts_with(&format!("{p}/")))
}

/// The DEEPEST Packwerk package containing `path`. `packs` must be
/// deepest-first ([`ScopeSources::sort_packs`]).
pub fn pack_of<'a>(path: &str, packs: &'a [(String, String)]) -> Option<&'a str> {
    packs
        .iter()
        .find(|(dir, _)| dir.is_empty() || path.starts_with(&format!("{dir}/")))
        .map(|(_, name)| name.as_str())
}

/// CODEOWNERS: the LAST matching pattern wins (GitHub's documented rule —
/// the opposite of gitignore's first-match intuition, and the single
/// easiest thing to get backwards).
pub fn owner_of<'a>(path: &str, rules: &'a [(String, Vec<String>)]) -> &'a [String] {
    let mut hit: &[String] = &[];
    for (pattern, owners) in rules {
        if codeowners_matches(pattern, path) {
            hit = owners;
        }
    }
    hit
}

/// The CODEOWNERS pattern subset this milestone supports — `*`, a leading
/// `/` anchor, a trailing `/` directory, `*.ext`, and a literal path
/// prefix. GitHub's format has no `!` negation and no `[a-z]` ranges at
/// all, so the only thing missing here is a `*` INSIDE a path segment,
/// which [`super::sources`] reports as a note rather than silently
/// mismatching.
pub fn codeowners_matches(pattern: &str, path: &str) -> bool {
    let pat = pattern.trim();
    if pat.is_empty() {
        return false;
    }
    if pat == "*" {
        return true;
    }
    if let Some(ext) = pat.strip_prefix("*.") {
        return path.ends_with(&format!(".{ext}"));
    }
    let anchored = pat.starts_with('/');
    let body = pat.trim_start_matches('/');
    let is_dir = body.ends_with('/');
    let body = body.trim_end_matches('/');
    if body.is_empty() {
        return false;
    }
    let matches_at = |candidate: &str| -> bool {
        if is_dir {
            candidate.starts_with(&format!("{body}/"))
        } else {
            candidate == body || candidate.starts_with(&format!("{body}/"))
        }
    };
    if anchored || body.contains('/') {
        return matches_at(path);
    }
    // Unanchored, single-segment: matches at any depth.
    path.split('/').any(|seg| seg == body)
        || path.contains(&format!("/{body}/"))
        || path.starts_with(&format!("{body}/"))
}

/// Parse a CODEOWNERS file into ordered `(pattern, owners)` rules.
/// Comments and blank lines are skipped; a line with a pattern and no
/// owners is kept (GitHub treats it as "no owner", which is a real,
/// meaningful last-match).
pub fn parse_codeowners(text: &str) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(pattern) = parts.next() else {
            continue;
        };
        let owners: Vec<String> = parts.map(str::to_string).collect();
        out.push((pattern.to_string(), owners));
    }
    out
}

/// Derive Packwerk packages from the indexed path list — every
/// `package.yml` is a package rooted at its own directory. No filesystem
/// walk and no YAML parse: the package's IDENTITY is its directory, which
/// is the only thing `pack:` needs.
pub fn packs_from_paths<'a>(paths: impl Iterator<Item = &'a str>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for p in paths {
        let dir = match p {
            "package.yml" => Some(String::new()),
            _ => p.strip_suffix("/package.yml").map(str::to_string),
        };
        if let Some(dir) = dir {
            let name = if dir.is_empty() {
                ".".to_string()
            } else {
                dir.rsplit('/').next().unwrap_or(&dir).to_string()
            };
            out.push((dir, name));
        }
    }
    out
}

// ── scope FROM a selection ────────────────────────────────────────────────

/// One candidate expression for a selection, with the reason it was
/// proposed. Ordered narrowest-intent first by [`propose`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Proposal {
    pub expr: String,
    pub why: String,
    /// `true` when this expression is a literal enumeration of exactly the
    /// selection — it can never over-select, and it is always last.
    pub exact_enumeration: bool,
}

/// Propose the SMALLEST expressions covering `selection`, JetBrains'
/// scope-from-selection move (the one that removes the authoring cliff).
///
/// PURE and offline by construction: it reasons about the selected paths
/// alone, so it can never quietly widen using repo state the caller did not
/// see. The generalisation is therefore a CANDIDATE, not an answer — the
/// caller resolves each proposal against the real repo and shows the
/// "+N files you did not select" delta before anything is saved. Never
/// silently generalize: that delta is the whole point (evidence report
/// §2.2, F10).
///
/// `ns:`/`pack:` proposals are deliberately NOT here: both need repo state
/// this function does not take, and inventing one from a path prefix would
/// be exactly the silent generalisation the rule forbids.
pub fn propose(selection: &[String]) -> Vec<Proposal> {
    let mut out = Vec::new();
    if selection.is_empty() {
        return out;
    }

    // 1. one role.
    let roles: BTreeSet<&str> = selection
        .iter()
        .map(|p| super::roles::role_for_path(p))
        .collect();
    let one_role: Option<&str> = match roles.iter().next() {
        Some(r) if roles.len() == 1 && *r != super::roles::ROLE_OTHER => Some(r),
        _ => None,
    };
    if let Some(role) = one_role {
        out.push(Proposal {
            expr: format!("role:{role}"),
            why: format!("every selected file is a `{role}` by path convention"),
            exact_enumeration: false,
        });
    }

    // 2. one common directory.
    let common = common_dir(selection).filter(|d| !d.is_empty());
    if let Some(dir) = common {
        out.push(Proposal {
            expr: format!("path:{dir}//*"),
            why: format!("every selected file is under `{dir}/`"),
            exact_enumeration: false,
        });
        if let Some(role) = one_role {
            out.push(Proposal {
                expr: format!("path:{dir}//* && role:{role}"),
                why: "the directory and the role together — narrower than either".to_string(),
                exact_enumeration: false,
            });
        }
    }

    // 3. one extension.
    let exts: BTreeSet<&str> = selection
        .iter()
        .filter_map(|p| p.rsplit_once('.').map(|(_, e)| e))
        .collect();
    let one_ext = match exts.iter().next() {
        Some(e) if exts.len() == 1 && selection.iter().all(|p| p.ends_with(&format!(".{e}"))) => {
            Some(*e)
        }
        _ => None,
    };
    if let Some(ext) = one_ext {
        out.push(Proposal {
            expr: format!("ext:{ext}"),
            why: format!("every selected file ends in `.{ext}`"),
            exact_enumeration: false,
        });
    }

    // 4. the literal enumeration — always available, always last, and the
    // only proposal that cannot over-select.
    let mut literal: Vec<String> = selection.to_vec();
    literal.sort();
    literal.dedup();
    out.push(Proposal {
        expr: literal
            .iter()
            .map(|p| format!("path:{p}"))
            .collect::<Vec<_>>()
            .join(" || "),
        why: format!("exactly the {} selected file(s), enumerated", literal.len()),
        exact_enumeration: true,
    });
    out
}

/// The deepest directory every path is under, or `None` when they share
/// nothing. `""` when they share only the repo root.
pub fn common_dir(paths: &[String]) -> Option<String> {
    let first = paths.first()?;
    let mut common: Vec<&str> = first.split('/').collect();
    common.pop(); // the leaf is a file name, never a shared directory
    for p in paths.iter().skip(1) {
        let mut segs: Vec<&str> = p.split('/').collect();
        segs.pop();
        let n = common
            .iter()
            .zip(segs.iter())
            .take_while(|(a, b)| a == b)
            .count();
        common.truncate(n);
    }
    Some(common.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESOLVER_SRC: &str = include_str!("scope.rs");

    fn fixture() -> ScopeSources {
        let mut src = ScopeSources {
            files: vec![
                ScopeFile {
                    path: "app/models/reseller/order.rb".into(),
                    lang: "ruby".into(),
                },
                ScopeFile {
                    path: "app/controllers/orders_controller.rb".into(),
                    lang: "ruby".into(),
                },
                ScopeFile {
                    path: "spec/models/reseller/order_spec.rb".into(),
                    lang: "ruby".into(),
                },
                ScopeFile {
                    path: "packs/checkout/app/models/cart.rb".into(),
                    lang: "ruby".into(),
                },
                ScopeFile {
                    path: "web/src/app.ts".into(),
                    lang: "typescript".into(),
                },
                ScopeFile {
                    path: "packs/checkout/package.yml".into(),
                    lang: "yaml".into(),
                },
            ],
            ..Default::default()
        };
        src.ns_by_path.insert(
            "app/models/reseller/order.rb".into(),
            vec!["Reseller::Order".into()],
        );
        src.ns_by_path.insert(
            "app/controllers/orders_controller.rb".into(),
            vec!["OrdersController".into()],
        );
        src.packs = packs_from_paths(src.files.iter().map(|f| f.path.as_str()));
        src.sort_packs();
        src.owners = parse_codeowners("*  @global\n/app/models/ @team/models\n*.ts @web\n");
        src.sets.insert(
            "onboarding".into(),
            ["web/src/app.ts".to_string()].into_iter().collect(),
        );
        src.annot_open
            .insert("app/controllers/orders_controller.rb".into());
        src.todo.insert("web/src/app.ts".into());
        src.bookmark.insert("app/models/reseller/order.rb".into());
        src.config_scopes
            .insert("web".into(), vec!["web/**".into()]);
        src
    }

    fn paths_of(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    fn matched(expr: &str) -> Vec<String> {
        let r = resolve(expr, &fixture());
        assert!(r.applied, "{expr}: refused — {:?}", r.notes);
        let mut v: Vec<String> = r.paths.expect("applied").into_iter().collect();
        v.sort();
        v
    }

    // --- the dead-surface walks ------------------------------------------

    /// Half one: a declared atom whose resolver expression does not appear
    /// in this module at all. A source scan, with a scan's limits (it
    /// proves the expression occurs, not that it is reached) — the same
    /// trade `grammar.rs`'s own key walk and `git_argv_lint` make. Half two
    /// (below) is the functional probe that closes the gap.
    #[test]
    fn every_declared_scope_atom_is_resolved() {
        let src: String = RESOLVER_SRC
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        for spec in SCOPE_ATOM_SPECS {
            let needle: String = spec
                .resolver_expr
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            assert!(
                src.contains(&needle),
                "kbc-scope/1 declares `{}:` but nothing in scope.rs reads `{}` — either \
                 wire it or move it to UNRESOLVED_ATOMS (a scope atom that parses and \
                 selects nothing is the v7.0 dead-surface defect)",
                spec.key,
                spec.resolver_expr,
            );
            let arm = format!("\"{}\"=>", spec.key);
            assert!(
                src.contains(&arm),
                "`{}:` has no arm in eval_atom",
                spec.key
            );
        }
    }

    /// Half two: run EVERY declared atom through the real parser and the
    /// real resolver against the fixture, and require that it (a) parses
    /// clean and (b) selects a PROPER, non-empty subset — i.e. it actually
    /// discriminates. A key wired to a no-op predicate passes the source
    /// scan and fails here.
    #[test]
    fn every_declared_scope_atom_discriminates_on_a_real_fixture() {
        let src = fixture();
        let probes: &[(&str, &str)] = &[
            ("role", "model"),
            ("ns", "Reseller"),
            ("pack", "checkout"),
            ("owner", "@web"),
            ("set", "onboarding"),
            ("annot", "open"),
            ("todo", ANY),
            ("bookmark", ANY),
        ];
        for spec in SCOPE_ATOM_SPECS {
            let (_, value) = probes
                .iter()
                .find(|(k, _)| *k == spec.key)
                .unwrap_or_else(|| panic!("no probe value for declared atom `{}`", spec.key));
            let expr = format!("{}:{value}", spec.key);
            let r = resolve(&expr, &src);
            assert!(r.applied, "{expr}: refused — {:?}", r.notes);
            let hits = r.paths.expect("applied");
            assert!(
                !hits.is_empty(),
                "{expr}: selected nothing on the fixture — the atom does not discriminate"
            );
            assert!(
                hits.len() < src.files.len(),
                "{expr}: selected EVERY file — the atom does not discriminate"
            );
        }
        // The kbcq/1-shared keys go through the same bar.
        for expr in ["path:app/models//*", "ext:rb", "lang:typescript"] {
            let r = resolve(expr, &src);
            assert!(r.applied, "{expr}: refused — {:?}", r.notes);
            let hits = r.paths.expect("applied");
            assert!(!hits.is_empty() && hits.len() < src.files.len(), "{expr}");
        }
    }

    #[test]
    fn the_unresolved_ledger_and_the_spec_table_are_disjoint() {
        for (key, why) in UNRESOLVED_ATOMS {
            assert!(
                !SCOPE_ATOM_SPECS.iter().any(|s| s.key == *key),
                "`{key}:` is in BOTH the ledger and the resolved table — drop the \
                 ledger entry in the same commit that wires the resolver"
            );
            assert!(
                !KBCQ_SHARED_KEYS.contains(key),
                "`{key}:` is a kbcq/1 shared key AND on the unresolved ledger"
            );
            assert!(why.len() > 20, "`{key}:` needs a real reason, got {why:?}");
        }
    }

    // --- the "one grammar" seam ------------------------------------------

    #[test]
    fn a_shared_key_is_read_back_out_of_kbcq_not_reparsed() {
        // `ext:.RB` — kbcq/1 normalises the leading dot and the case; if
        // this module re-parsed the token itself the value would differ.
        let e = atom_from_token("ext:.RB").expect("parses");
        assert_eq!(
            e,
            Expr::Atom {
                key: "ext".into(),
                value: grammar::parse("ext:.RB").filters.ext[0].clone(),
            }
        );
    }

    #[test]
    fn an_unknown_key_refuses_the_whole_scope_rather_than_dropping_the_atom() {
        let r = resolve("role:model gorgonzola:yes", &fixture());
        assert!(!r.applied);
        assert!(r.paths.is_none());
        assert!(r.notes[0].contains("scope not applied"), "{:?}", r.notes);
    }

    #[test]
    fn an_atom_named_but_not_yet_resolved_says_so_by_name() {
        let d = atom_from_token("review:1842").expect_err("refused");
        assert!(d.message.contains("not resolved yet"), "{d:?}");
    }

    // --- the algebra ------------------------------------------------------

    #[test]
    fn juxtaposition_is_and_matching_kbcq() {
        assert_eq!(
            matched("role:model lang:ruby"),
            matched("role:model && lang:ruby")
        );
    }

    #[test]
    fn negation_and_parens_and_or_compose() {
        assert_eq!(
            matched("(role:model || role:controller) && !path:packs//*"),
            vec![
                "app/controllers/orders_controller.rb".to_string(),
                "app/models/reseller/order.rb".to_string(),
            ]
        );
        // `-key:` is kbcq/1's negation spelling and means the same thing.
        assert_eq!(matched("!role:spec"), matched("-role:spec"));
    }

    #[test]
    fn a_dollar_ref_expands_the_config_scope() {
        assert_eq!(matched("$web"), vec!["web/src/app.ts".to_string()]);
        let r = resolve("$nope", &fixture());
        assert!(!r.applied);
        assert!(r.notes[0].contains("unknown scope"), "{:?}", r.notes);
    }

    #[test]
    fn normalize_is_a_fixed_point() {
        for raw in [
            "role:model",
            "role:model lang:ruby",
            "(role:model || role:spec) && !path:vendor//*",
            "$web",
            "!ns:Reseller",
        ] {
            let once = parse(raw).normalized;
            let twice = parse(&once).normalized;
            assert_eq!(once, twice, "not a fixed point: {raw:?}");
        }
    }

    #[test]
    fn a_quoted_value_survives_tokenisation() {
        let toks = tokenize(r#"path:"app/my dir//*" && role:model"#);
        assert_eq!(toks[0], "path:app/my dir//*");
        assert_eq!(toks[1], "&&");
    }

    #[test]
    fn unbalanced_parens_refuse_rather_than_guess() {
        let r = resolve("(role:model", &fixture());
        assert!(!r.applied);
    }

    // --- the individual matchers -----------------------------------------

    #[test]
    fn jetbrains_path_wildcards() {
        assert!(path_atom_matches("app/models//*", "app/models/a/b.rb"));
        assert!(!path_atom_matches("app/models/*", "app/models/a/b.rb"));
        assert!(path_atom_matches("app/models/*", "app/models/b.rb"));
        assert!(path_atom_matches("app/models", "app/models/a/b.rb"));
    }

    #[test]
    fn codeowners_last_match_wins() {
        let rules =
            parse_codeowners("*       @global\n/apps/  @octocat\n/apps/github  @doctocat\n# c\n");
        assert_eq!(owner_of("README.md", &rules), ["@global".to_string()]);
        assert_eq!(owner_of("apps/x.rb", &rules), ["@octocat".to_string()]);
        assert_eq!(
            owner_of("apps/github/x.rb", &rules),
            ["@doctocat".to_string()]
        );
    }

    #[test]
    fn packs_come_from_the_indexed_paths_and_the_deepest_wins() {
        let mut src = ScopeSources {
            packs: packs_from_paths(
                [
                    "package.yml",
                    "packs/a/package.yml",
                    "packs/a/b/package.yml",
                ]
                .into_iter(),
            ),
            ..Default::default()
        };
        src.sort_packs();
        assert_eq!(pack_of("packs/a/b/x.rb", &src.packs), Some("b"));
        assert_eq!(pack_of("packs/a/x.rb", &src.packs), Some("a"));
        assert_eq!(pack_of("other/x.rb", &src.packs), Some("."));
    }

    #[test]
    fn propose_offers_a_generalisation_and_always_an_exact_enumeration() {
        let sel = paths_of(&["app/models/order.rb", "app/models/cart.rb"]);
        let props = propose(&sel);
        assert_eq!(props[0].expr, "role:model");
        assert!(props.iter().any(|p| p.expr == "path:app/models//*"));
        let last = props.last().expect("non-empty");
        assert!(last.exact_enumeration);
        assert_eq!(
            last.expr,
            "path:app/models/cart.rb || path:app/models/order.rb"
        );
        // Every proposal must PARSE — a proposal the operator cannot paste
        // back into the box is worse than no proposal.
        for p in &props {
            assert!(
                parse(&p.expr).expr.is_some(),
                "proposal does not parse: {:?}",
                p.expr
            );
        }
    }

    #[test]
    fn propose_never_generalises_across_two_roles() {
        let sel = paths_of(&["app/models/order.rb", "spec/models/order_spec.rb"]);
        let props = propose(&sel);
        assert!(!props.iter().any(|p| p.expr.starts_with("role:")));
        assert!(props.last().expect("non-empty").exact_enumeration);
    }

    #[test]
    fn propose_on_an_empty_selection_offers_nothing() {
        assert!(propose(&[]).is_empty());
    }

    #[test]
    fn ns_covers_descendants_never_a_bare_suffix() {
        assert!(ns_covers("Reseller", "Reseller::Order"));
        assert!(ns_covers("Reseller::Order", "Reseller::Order"));
        assert!(!ns_covers("Order", "Reseller::Order"));
        assert!(!ns_covers("Reseller", "ResellerOther::X"));
    }
}
