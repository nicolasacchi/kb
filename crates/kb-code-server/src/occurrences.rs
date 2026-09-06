//! B2 — token-level identifier OCCURRENCES: a second tree-sitter extraction
//! pass, separate from `extract.rs`'s `tags.scm` definitions-only pass,
//! producing every identifier-like leaf in the CST — not just the ones a
//! `tags.scm` pattern happens to capture as a definition. Gated to the
//! languages `lang::TOKEN_LEVEL_LANG_IDS` names — B2 shipped four
//! (Rust/TypeScript/TSX/JavaScript); B5b widens this to all eight tier-1
//! languages (+ Python/Ruby/Go/Bash) — see that const's doc.
//!
//! # B5a — the single-char `ref` trim
//!
//! The B2 bench (`tests/measure/occurrences_bench.rs`, run against kb's own
//! `crates/` tree) measured this table at +142% sqlite size / +42% index
//! walk time versus symbols+highlights alone — expensive, and a large chunk
//! of it is pure noise: a one-character identifier used as a REFERENCE
//! (`i`, `x`, `_`) is essentially never something `resolve.rs`'s tags-tier
//! or B4's import-heuristic can usefully disambiguate. [`extract_occurrences`]
//! drops a `ref`-role occurrence whose name is exactly one CHARACTER (see
//! [`is_trimmed_single_char_ref`]) — `def`/`import` occurrences of any
//! length always stay, since those are exactly the rows `resolve.rs` looks
//! UP BY NAME (dropping them would break resolution, not just trim noise).
//!
//! **V3.G1 carve-out:** the four locals languages (`crate::locals::supports`
//! — rust/typescript/tsx/python) KEEP single-char refs. The scope graph
//! binds exactly those short names (`x`/`n`/…), and dropping the occurrence
//! row would leave `local_def_ordinal` with nothing to stamp — same-file
//! exact resolve would fall through. Non-locals token languages still trim.
//!
//! # Role classification
//!
//! Every identifier-like node is classified by its IMMEDIATE parent's
//! grammar node kind (plus the field name the node occupies within that
//! parent, from `TreeCursor::field_name` — NOT a query capture; this pass
//! walks the raw CST directly, not `tags.scm`):
//!
//! - **`def`** — the node is the "name" (or equivalent) child of a
//!   declaration: a function/method/struct/enum/trait/interface/class name,
//!   a `let`/`const`/parameter binding, a struct field declaration, an enum
//!   variant. See [`is_def`]'s per-language table.
//! - **`import`** — an ancestor (walking up from the node) is a Rust
//!   `use_declaration` or a TS/JS `import_statement`/`import_clause`. Checked
//!   AFTER the `def` check (an import never also matches a `def` parent
//!   shape in practice, but `def` is deliberately the first, narrower test).
//! - **`ref`** — everything else identifier-like: call-site names, type
//!   usages, field/property access, macro invocations, JSX tag names, and
//!   so on.
//!
//! This is intentionally NOT scope resolution (no binding/shadowing model,
//! no cross-file linking) — a `def` here means "sits in a declaration's name
//! position," nothing more. `resolve.rs`'s `note` field says as much to
//! anyone consuming the endpoint built on top of this table.
//!
//! # Ordinal + cap
//!
//! [`extract_occurrences`] walks the tree in preorder via a `TreeCursor`;
//! since identifier-like nodes are always LEAVES (no identifier-kind node
//! ever contains another), preorder visitation order is exactly byte-
//! position ascending — the same "ordinal = emission order" contract
//! `extract.rs`'s `Symbol::ordinal` documents, no separate sort needed.
//! [`MAX_OCCURRENCES_PER_FILE`] stops the walk early (never erroring) for a
//! pathological file — a dropped tail is an accepted, unmarked truncation
//! (see the const's own doc).
//!
//! # Cache key
//!
//! Same `(blob_hash, salt)` key as `symbols`/`highlights` (ADR-2) — but
//! `store::Store::has_occurrences` is a SEPARATE presence check from
//! `has_symbols`, so a blob that already has cached symbols/highlights (from
//! before this pass existed, or simply because occurrences derivation is
//! independently retry-able) doesn't skip occurrences derivation, and vice
//! versa. The salt string itself is unchanged/shared — this pass doesn't
//! need its own salt suffix, since `ingest::index_file` re-derives
//! occurrences under the exact same `(blob_hash, salt)` the symbols pass
//! already uses; a future grammar/query bump still invalidates both
//! passes together, which is the correct behavior (both read the same
//! parse tree).
//!
//! # `[occurrences]` config gate (B5a)
//!
//! Whether this pass runs AT ALL for a given repo is `ingest::index_file`'s
//! call — `config::OccurrencesSection::repo_enabled` (default ON, per-repo
//! denylist) — never checked in here; this module has no config knowledge,
//! it just extracts when asked.

use crate::lang::{self, LangError};
use std::collections::HashMap;
use tree_sitter::Node;

pub type Result<T> = std::result::Result<T, LangError>;

/// Stop deriving further occurrences once a file's identifier count reaches
/// this — a defensive cap against a pathological (generated/minified/data)
/// file, not a real-world ceiling for hand-written source. Dropping the tail
/// is fine to do silently (logged at `debug`, not surfaced in the row set —
/// see the module doc) rather than growing a `capped` marker column: the
/// occurrences table is a best-effort token index, not a completeness
/// guarantee the caller depends on.
pub const MAX_OCCURRENCES_PER_FILE: usize = 20_000;

/// The three occurrence roles, as stored verbatim in `occurrences.role` —
/// a plain `&'static str` (not a Rust enum), matching this crate's existing
/// convention for a small stringly-typed vocabulary living at the store
/// layer (`store::CommitSessionRow`'s `confidence`/`via` fields document the
/// same choice: "the store has no opinion on ... that conversion lives
/// elsewhere").
pub const ROLE_DEF: &str = "def";
pub const ROLE_REF: &str = "ref";
pub const ROLE_IMPORT: &str = "import";

/// `occurrences.source` — see migration `V0010__occurrences_source.sql`'s
/// doc. Every row this module (`extract_occurrences`) produces is
/// [`SOURCE_TS`]; [`SOURCE_SCIP`] is only ever written by `crate::scip`'s
/// ingest route (S1), never by this file.
pub const SOURCE_TS: &str = "ts";
/// See [`SOURCE_TS`].
pub const SOURCE_SCIP: &str = "scip";

/// One indexed identifier occurrence. `line`/`col_start`/`col_end` use the
/// exact same convention as `extract::Symbol` (1-based line, 0-based byte
/// columns — tree-sitter's own `Point`, LSP-compatible).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Occurrence {
    /// Stable emission order (byte position ascending) — see the module doc.
    pub ordinal: u32,
    pub name: String,
    /// `"def"` | `"ref"` | `"import"` — see the module doc's classification
    /// rules and the `ROLE_*` constants above.
    pub role: String,
    pub line: u32,
    pub col_start: u32,
    pub col_end: u32,
    /// `"ts"` | `"scip"` (S1) — see [`SOURCE_TS`]/[`SOURCE_SCIP`]. Always
    /// [`SOURCE_TS`] for every row this module produces.
    pub source: String,
    /// V3.G1 — ordinal of the same-file DEFINITION occurrence this
    /// reference binds to under the lexical scope graph (`crate::locals`).
    /// `None` when not locally bound (unbound identifier, a def/import row,
    /// a language without a locals.scm, or a scip-sourced row). See
    /// migration `V0013__occurrence_locals.sql`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_def_ordinal: Option<u32>,
}

/// The eight languages this pass covers (B2: Rust/TypeScript/TSX/JavaScript;
/// B5b widened to Python/Ruby/Go/Bash) — single source of truth is
/// `lang::supports_token_level`; kept as a thin re-export here so callers
/// that only import `occurrences` don't also need to know about `lang`.
pub fn supports(lang_id: &str) -> bool {
    lang::supports_token_level(lang_id)
}

/// Extract every identifier-like occurrence from `source`, parsed as
/// `lang_id`. `Err(LangError::Unsupported)` for any language outside
/// [`supports`] — `ingest::index_file` checks the same gate before ever
/// calling this, so in practice this error path is only reachable from a
/// direct/test caller.
///
/// For the four V3.G1 proof languages (`crate::locals::supports`), a second
/// locals pass fills [`Occurrence::local_def_ordinal`] on each bound
/// reference; other token-level languages leave it `None`.
pub fn extract_occurrences(lang_id: &str, source: &[u8]) -> Result<Vec<Occurrence>> {
    if !supports(lang_id) {
        return Err(LangError::Unsupported(lang_id.to_string()));
    }
    let (tree, _language) = lang::parse(lang_id, source)?;
    let mut out = Vec::new();
    let mut cursor = tree.walk();
    'walk: loop {
        let node = cursor.node();
        if is_identifier_like(lang_id, node) {
            if let Ok(name) = node.utf8_text(source) {
                if !name.is_empty() {
                    let role = classify_role(lang_id, node, cursor.field_name());
                    // B5a — the cost trim: a single-char `ref` is
                    // unresolvable noise for the tags/import tiers (see
                    // [`is_trimmed_single_char_ref`]'s doc). V3.G1: the four
                    // locals languages KEEP single-char refs — `x`/`n`/…
                    // are exactly the names the scope graph binds, and
                    // dropping them would make `local_def_ordinal` unable
                    // to attach to the occurrence row resolve looks up.
                    // `def`/`import` of any length always stay.
                    let trim =
                        is_trimmed_single_char_ref(role, name) && !crate::locals::supports(lang_id);
                    if !trim {
                        let start = node.start_position();
                        let end = node.end_position();
                        out.push(Occurrence {
                            ordinal: out.len() as u32,
                            name: name.to_string(),
                            role: role.to_string(),
                            line: start.row as u32 + 1,
                            col_start: start.column as u32,
                            col_end: end.column as u32,
                            source: SOURCE_TS.to_string(),
                            local_def_ordinal: None,
                        });
                        if out.len() >= MAX_OCCURRENCES_PER_FILE {
                            tracing::debug!(
                                lang = lang_id,
                                cap = MAX_OCCURRENCES_PER_FILE,
                                "kb-code occurrences: MAX_OCCURRENCES_PER_FILE reached — stopping early"
                            );
                            break 'walk;
                        }
                    }
                }
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                break 'walk;
            }
        }
    }
    if crate::locals::supports(lang_id) {
        apply_local_bindings(lang_id, source, &mut out)?;
    }
    Ok(out)
}

/// Stamp [`Occurrence::local_def_ordinal`] from the lexical scope graph.
/// Matches bindings to occurrence rows by `(line, col_start, name)` — the
/// occurrence pass and the locals query both land on the same identifier
/// leaf nodes, so the triple is unique within a file for our purposes.
fn apply_local_bindings(lang_id: &str, source: &[u8], out: &mut [Occurrence]) -> Result<()> {
    let bindings = crate::locals::bind_locals(lang_id, source)?;
    if bindings.is_empty() {
        return Ok(());
    }
    // def key → ordinal of the def-role occurrence at that position.
    let mut def_ordinal: HashMap<(u32, u32, String), u32> = HashMap::new();
    for occ in out.iter() {
        if occ.role == ROLE_DEF {
            def_ordinal.insert((occ.line, occ.col_start, occ.name.clone()), occ.ordinal);
        }
    }
    // ref key → mut occurrence index.
    let mut ref_index: HashMap<(u32, u32, String), usize> = HashMap::new();
    for (i, occ) in out.iter().enumerate() {
        if occ.role == ROLE_REF {
            ref_index.insert((occ.line, occ.col_start, occ.name.clone()), i);
        }
    }
    for b in &bindings {
        let Some(&def_ord) = def_ordinal.get(&(b.def_line, b.def_col, b.name.clone())) else {
            // Locals saw a def the token-level pass didn't classify as
            // ROLE_DEF (or trimmed) — leave unbound rather than invent a
            // pointer. Classify DOWN.
            continue;
        };
        if let Some(&idx) = ref_index.get(&(b.ref_line, b.ref_col, b.name.clone())) {
            out[idx].local_def_ordinal = Some(def_ord);
        }
    }
    Ok(())
}

/// B5a — the cost trim. `true` when `(role, name)` is a single-char `ref`
/// occurrence — a bare `i`/`x`/`_`-style identifier used as a REFERENCE,
/// never a definition or import. The B2 bench measured this pass at +142%
/// db size / +42% walk time on kb's own `crates/` tree for zero resolve
/// wins from these rows: a one-character ref is essentially never something
/// `resolve.rs`'s tags-tier/import-heuristic can usefully disambiguate (loop
/// counters, throwaway closures params, wildcard-ish bindings) — it's pure
/// noise inflating the table. `def`/`import` occurrences of ANY length
/// (including one character — Go's `T` generic parameter, a single-letter
/// exported const) always stay: those are exactly the rows `resolve.rs`'s
/// file-local tier and B4's import-heuristic search FOR by name, so
/// dropping them would silently break resolution rather than just trim
/// noise. Char-counted (not byte-counted) so a single non-ASCII identifier
/// character isn't miscounted as "multi-char" by its UTF-8 byte length.
fn is_trimmed_single_char_ref(role: &str, name: &str) -> bool {
    role == ROLE_REF && name.chars().count() == 1
}

/// Identifier-like leaf node kinds per language — kept small, explicit, and
/// language-scoped rather than one big cross-language set, so each list
/// reads as a complete inventory of "every leaf kind this grammar uses for a
/// name." Takes the whole `Node` (not just its `kind()`) ONLY so Bash's
/// heavily-overloaded `word` kind can be narrowed by parent shape (see that
/// arm's own doc) — every other language ignores the extra context and
/// matches on `node.kind()` alone, same as before B5b.
///
/// - **Rust**: `identifier` (values/types/most names), `type_identifier`
///   (type-position names — see `extract.rs`'s own `container_of`/`map_kind`
///   tables, which already lean on this same grammar distinction),
///   `field_identifier` (struct field declarations AND `.field` access —
///   distinguished by parent shape in [`is_def`], not by kind).
/// - **TypeScript/TSX/JavaScript**: `identifier`, `type_identifier` (TS type
///   names), `property_identifier` (object/class member names, `.prop`
///   access, JSX attribute names), `shorthand_property_identifier` (object
///   literal shorthand `{foo}`), `shorthand_property_identifier_pattern`
///   (destructuring shorthand `{foo} = x` / `({foo}) => ...`).
/// - **Python** (B5b): `identifier` ONLY — Python's grammar has no separate
///   type-position or field-access leaf kind (verified against
///   `tree-sitter-python 0.25.0`'s `node-types.json`: `attribute`'s own
///   `attribute` field is typed `identifier` too, distinguished by parent
///   shape exactly like Rust's `field_identifier` dual purpose, just without
///   a dedicated kind for it).
/// - **Ruby** (B5b): `identifier` (locals/methods/params), `constant`
///   (`Class`/`Module`/`CONST` names — Ruby's own separate leaf kind for
///   capitalized names, per `tree-sitter-ruby 0.23.1`), `instance_variable`
///   (`@foo`), `class_variable` (`@@foo`), `global_variable` (`$foo`).
/// - **Go** (B5b): `identifier`, `type_identifier`, `field_identifier`
///   (struct field declarations AND `.field`/method-call selectors — same
///   dual purpose as Rust's), `package_identifier` (the `package` clause's
///   own name, an import's bound local name, and a qualified reference's
///   package half, e.g. `pkg` in `pkg.Func`).
/// - **Bash** (B5b): `variable_name` (unambiguous — `tree-sitter-bash`'s own
///   dedicated kind for every real Bash variable, assignment/expansion/
///   for-loop-var alike). `word` is Bash's grammar kind for EVERY bareword —
///   a function name, a command name being invoked, AND a plain string-like
///   command ARGUMENT all parse as `word`; only the first two are
///   meaningful "names" (an argument like `echo hello`'s `hello` is not a
///   name to resolve). So a `word` only counts here when its immediate
///   parent is `function_definition` (the function's own name field) or
///   `command_name` (a command being invoked) — anything else (a plain
///   argument word) is silently excluded, never emitted as noise.
fn is_identifier_like(lang_id: &str, node: Node<'_>) -> bool {
    let kind = node.kind();
    match lang_id {
        "rust" => matches!(kind, "identifier" | "type_identifier" | "field_identifier"),
        "typescript" | "tsx" | "javascript" => matches!(
            kind,
            "identifier"
                | "type_identifier"
                | "property_identifier"
                | "shorthand_property_identifier"
                | "shorthand_property_identifier_pattern"
        ),
        "python" => matches!(kind, "identifier"),
        "ruby" => matches!(
            kind,
            "identifier" | "constant" | "instance_variable" | "class_variable" | "global_variable"
        ),
        "go" => matches!(
            kind,
            "identifier" | "type_identifier" | "field_identifier" | "package_identifier"
        ),
        "bash" => match kind {
            "variable_name" => true,
            "word" => matches!(
                node.parent().map(|p| p.kind()),
                Some("function_definition") | Some("command_name")
            ),
            _ => false,
        },
        _ => false,
    }
}

fn classify_role(lang_id: &str, node: Node<'_>, field: Option<&str>) -> &'static str {
    let Some(parent) = node.parent() else {
        return ROLE_REF;
    };
    if is_def(lang_id, parent, field) {
        return ROLE_DEF;
    }
    if is_within_import(lang_id, node) {
        return ROLE_IMPORT;
    }
    ROLE_REF
}

/// `true` when `(parent, field)` is a declaration's name position — see the
/// module doc's `def` bullet. Table entries are `(parent node kind, field
/// name within that parent)`; `None` for `field` matches a node that is a
/// POSITIONAL (unnamed-field) child of `parent`'s kind — Rust closure
/// parameters and plain JS function parameters are both grammar shapes with
/// no field name on the identifier itself (verified against each grammar's
/// own `node-types.json`, not guessed). Takes the whole `parent` `Node` (not
/// just its `kind()`) ONLY so Go's `:=` short-variable-declaration can look
/// one level further up (see [`is_go_short_var_left`]'s own doc) — every
/// other language ignores the extra context.
fn is_def(lang_id: &str, parent: Node<'_>, field: Option<&str>) -> bool {
    let parent_kind = parent.kind();
    match lang_id {
        "rust" => matches!(
            (parent_kind, field),
            ("function_item", Some("name"))
                | ("function_signature_item", Some("name"))
                | ("struct_item", Some("name"))
                | ("enum_item", Some("name"))
                | ("union_item", Some("name"))
                | ("trait_item", Some("name"))
                | ("mod_item", Some("name"))
                | ("type_item", Some("name"))
                | ("macro_definition", Some("name"))
                | ("const_item", Some("name"))
                | ("static_item", Some("name"))
                | ("let_declaration", Some("pattern"))
                | ("parameter", Some("pattern"))
                | ("field_declaration", Some("name"))
                | ("enum_variant", Some("name"))
                | ("closure_parameters", None)
                // V3.G1: for-loop binding (`for item in …`) so locals can
                // point `local_def_ordinal` at a real def-role row.
                | ("for_expression", Some("pattern"))
        ),
        "typescript" | "tsx" | "javascript" => matches!(
            (parent_kind, field),
            ("function_declaration", Some("name"))
                | ("generator_function_declaration", Some("name"))
                | ("function_signature", Some("name"))
                | ("method_signature", Some("name"))
                | ("abstract_method_signature", Some("name"))
                | ("class_declaration", Some("name"))
                | ("abstract_class_declaration", Some("name"))
                | ("method_definition", Some("name"))
                | ("interface_declaration", Some("name"))
                | ("type_alias_declaration", Some("name"))
                | ("enum_declaration", Some("name"))
                | ("internal_module", Some("name"))
                | ("variable_declarator", Some("name"))
                // V3.G1: TS params use the `name` field (simple) or
                // `pattern` (destructuring); both must be ROLE_DEF.
                | ("required_parameter", Some("name"))
                | ("optional_parameter", Some("name"))
                | ("required_parameter", Some("pattern"))
                | ("optional_parameter", Some("pattern"))
                | ("formal_parameters", None)
                // Single-param arrow: `x => …` (parameter field, no parens).
                | ("arrow_function", Some("parameter"))
                | ("for_in_statement", Some("left"))
                | ("catch_clause", Some("parameter"))
                | ("public_field_definition", Some("name"))
                | ("field_definition", Some("property"))
                | ("property_signature", Some("name"))
        ),
        // Python (B5b) — deliberately does NOT special-case tuple/list
        // destructuring (`a, b = 1, 2`'s `a`/`b` sit under a `pattern_list`,
        // not `assignment` directly) or attribute-assignment targets
        // (`self.x = 1`'s `x` sits under `attribute`, indistinguishable
        // from a plain `.attr` READ by parent shape alone) — both classify
        // as `ref`, an accepted gap mirroring Rust's own unhandled
        // tuple-pattern `let` and TS's unhandled object/array destructuring
        // (see those languages' own table comments/tests): this table
        // covers the SIMPLE, unambiguous binding shapes only, not a full
        // pattern-matching engine.
        "python" => matches!(
            (parent_kind, field),
            ("function_definition", Some("name"))
                | ("class_definition", Some("name"))
                | ("parameters", None) // bare `def f(x, y):`
                | ("default_parameter", Some("name")) // `def f(x=1):`
                | ("typed_parameter", None) // `def f(x: int):`
                | ("typed_default_parameter", Some("name")) // `def f(x: int = 1):`
                | ("list_splat_pattern", None) // `*args`
                | ("dictionary_splat_pattern", None) // `**kwargs`
                | ("assignment", Some("left")) // `x = 1` (bare name only)
                | ("for_statement", Some("left")) // `for x in ...:`
                // V3.G1: comprehension target (`[x for x in …]`).
                | ("for_in_clause", Some("left"))
                // `with ... as x:` / `except E as x:` BOTH bind their name
                // through an `as_pattern`'s `alias` field wrapping an
                // `as_pattern_target` node (confirmed by direct parse —
                // `tree-sitter-python 0.25.0`'s own `node-types.json` does
                // NOT list `as_pattern_target` as a top-level node type at
                // all, despite the live parser genuinely producing one;
                // `as_pattern`'s own `alias` field entry is the only trace
                // of it in the static schema — verified empirically via
                // `Tree::root_node().to_sexp()` on both fixture forms
                // rather than guessed from the incomplete JSON).
                | ("as_pattern_target", None)
        ),
        // Ruby (B5b) — same "simple bindings only" scope as Python; ALSO
        // skips `left_assignment_list`/`destructured_left_assignment`
        // (multi-assign `a, b = 1, 2`) for the identical reason.
        "ruby" => matches!(
            (parent_kind, field),
            ("method", Some("name"))
                | ("singleton_method", Some("name"))
                | ("class", Some("name"))
                | ("module", Some("name"))
                | ("method_parameters", None) // `def f(x, y)`
                | ("block_parameters", None) // `{ |x, y| ... }`
                | ("optional_parameter", Some("name")) // `def f(x = 1)`
                | ("splat_parameter", Some("name")) // `def f(*args)`
                | ("hash_splat_parameter", Some("name")) // `def f(**kwargs)`
                | ("keyword_parameter", Some("name")) // `def f(key:)`
                | ("block_parameter", Some("name")) // `def f(&blk)`
                | ("destructured_parameter", None) // nested `|(a, b), c|`
                | ("assignment", Some("left")) // `x = 1`, ALSO `@x = 1`
                | ("for", Some("pattern")) // `for x in ...`
        ),
        // Go (B5b) — the ordinary table entries below, PLUS `:=` short
        // variable declarations via [`is_go_short_var_left`] (needs a
        // grandparent check the plain `(parent_kind, field)` shape can't
        // express — see that fn's own doc for why).
        "go" => {
            matches!(
                (parent_kind, field),
                ("function_declaration", Some("name"))
                    | ("method_declaration", Some("name"))
                    | ("type_spec", Some("name"))
                    | ("type_alias", Some("name"))
                    | ("const_spec", Some("name"))
                    | ("var_spec", Some("name"))
                    | ("parameter_declaration", Some("name"))
                    | ("variadic_parameter_declaration", Some("name"))
                    | ("field_declaration", Some("name"))
            ) || is_go_short_var_left(parent)
        }
        // Bash (B5b) — `declaration_command` covers EVERY `local`/`export`/
        // `declare`/`readonly`/`typeset` form (all collapse to this one
        // grammar node; verified against `tree-sitter-bash 0.25.1`'s
        // `node-types.json` — none of those keywords gets its own node
        // kind).
        "bash" => matches!(
            (parent_kind, field),
            ("function_definition", Some("name"))
                | ("variable_assignment", Some("name")) // `VAR=value`
                | ("for_statement", Some("variable")) // `for x in ...; do`
                | ("declaration_command", None) // `local x` / `export FOO`
        ),
        _ => false,
    }
}

/// Go's `x := expr` (and `if`/`for` init forms of the same construct) binds
/// `x` — but the grammar wraps BOTH sides of `short_var_declaration` in an
/// `expression_list` (verified against `tree-sitter-go 0.25.0`'s
/// `node-types.json`: `left`/`right` are both typed `expression_list`, not
/// `identifier` directly), so a plain `(parent_kind, field)` check can't
/// tell "the identifier's `expression_list` parent is the LEFT side" from
/// "...is the RIGHT side" — `x := y` would otherwise misclassify `y` (a
/// REFERENCE to an existing binding) as a `def` too. This walks one level
/// further up and compares node IDENTITY (`Node: PartialEq`) against
/// `short_var_declaration`'s own resolved `left` field, which is exact
/// (no field-name string juggling) rather than assuming `left` is always
/// the first child.
fn is_go_short_var_left(expr_list: Node<'_>) -> bool {
    if expr_list.kind() != "expression_list" {
        return false;
    }
    let Some(grandparent) = expr_list.parent() else {
        return false;
    };
    grandparent.kind() == "short_var_declaration"
        && grandparent.child_by_field_name("left") == Some(expr_list)
}

/// `true` if any ancestor of `node` is an import construct — Rust's
/// `use_declaration` wraps every form (`use a::b;`, `use a::{b, c};`,
/// `use a::b as c;`, `use a::*;`), TS/JS's `import_statement`/
/// `import_clause` cover every import form the same way (default, named,
/// namespace, aliased), Python's `import_statement`/`import_from_statement`
/// cover `import x`/`from x import y` alike, and Go's `import_declaration`
/// covers both the single-spec and parenthesized-list forms — see the
/// module doc's `import` bullet. Ruby and Bash have NO entry here (empty
/// slice, always `false`): Ruby's `require "foo"` is an ordinary METHOD
/// CALL, not import syntax the grammar distinguishes at all (`require`
/// parses as a plain `call` node — nothing to walk up to), and Bash has no
/// import construct whatsoever (`source foo.sh` is likewise just a plain
/// `command`) — both are honest, deliberate gaps, not an oversight (see
/// this module's own doc and `imports.rs`'s doc for the same point applied
/// to the B4 import-heuristic resolve tier).
fn is_within_import(lang_id: &str, node: Node<'_>) -> bool {
    let import_kinds: &[&str] = match lang_id {
        "rust" => &["use_declaration"],
        "typescript" | "tsx" | "javascript" => &["import_statement", "import_clause"],
        "python" => &["import_statement", "import_from_statement"],
        "go" => &["import_declaration"],
        _ => &[],
    };
    let mut cur = node.parent();
    while let Some(n) = cur {
        if import_kinds.contains(&n.kind()) {
            return true;
        }
        cur = n.parent();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden row: (name, role, line, local_def_ordinal).
    fn occ(
        name: &str,
        role: &str,
        line: u32,
        local: Option<u32>,
    ) -> (String, String, u32, Option<u32>) {
        (name.to_string(), role.to_string(), line, local)
    }

    fn shorthand(occurrences: &[Occurrence]) -> Vec<(String, String, u32, Option<u32>)> {
        occurrences
            .iter()
            .map(|o| (o.name.clone(), o.role.clone(), o.line, o.local_def_ordinal))
            .collect()
    }

    // --- Rust: fn + method + use + field access + macro call ---------------

    const RUST_FIXTURE: &str = r#"use std::collections::HashMap;

struct Point {
    x: i32,
    y: i32,
}

impl Point {
    fn new(x: i32, y: i32) -> Point {
        Point { x, y }
    }

    fn dist(&self) -> i32 {
        self.x + self.y
    }
}

fn main() {
    let p = Point::new(1, 2);
    println!("{}", p.x);
}
"#;

    #[test]
    fn rust_golden_occurrence_list() {
        // V3.G1: single-char refs are KEPT for locals languages (rust is
        // one) so `local_def_ordinal` can attach — see the B5a trim carve-
        // out in `extract_occurrences`. Non-locals token languages still
        // trim (ruby/go/bash goldens + `single_char_ref_is_trimmed…`).
        //
        // `Point { x, y }` is shorthand field init: one `field_identifier`
        // per field (AST walk order: type, then fields) — not a doubled
        // field+value pair. Those field refs bind to the `new` params.
        let occurrences = extract_occurrences("rust", RUST_FIXTURE.as_bytes()).unwrap();
        assert_eq!(
            shorthand(&occurrences),
            vec![
                occ("std", "import", 1, None),
                occ("collections", "import", 1, None),
                occ("HashMap", "import", 1, None),
                occ("Point", "def", 3, None),
                occ("x", "def", 4, None), // struct field
                occ("y", "def", 5, None),
                occ("Point", "ref", 8, None), // impl Point
                occ("new", "def", 9, None),
                occ("x", "def", 9, None), // params
                occ("y", "def", 9, None),
                occ("Point", "ref", 9, None),  // return type
                occ("Point", "ref", 10, None), // constructor
                occ("x", "ref", 10, Some(8)),  // shorthand → param x (ord 8)
                occ("y", "ref", 10, Some(9)),  // shorthand → param y (ord 9)
                occ("dist", "def", 13, None),
                occ("x", "ref", 14, None), // self.x field — not a local
                occ("y", "ref", 14, None),
                occ("main", "def", 18, None),
                occ("p", "def", 19, None),
                occ("Point", "ref", 19, None),
                occ("new", "ref", 19, None),
                occ("println", "ref", 20, None),
                occ("p", "ref", 20, Some(18)), // let p
                occ("x", "ref", 20, None),     // p.x field
            ],
            "got: {occurrences:#?}"
        );
        // Ordinals are dense and byte-position ascending.
        for (i, o) in occurrences.iter().enumerate() {
            assert_eq!(o.ordinal, i as u32);
        }
        // Every row from this pass is source="ts".
        assert!(occurrences.iter().all(|o| o.source == SOURCE_TS));
    }

    // --- TSX: import + interface + arrow fn + JSX ---------------------------

    const TSX_FIXTURE: &str = r#"import { Widget } from "./widget";

interface Shape {
  area(): number;
}

const makeArea = (w: Widget): number => w.area();

export function Card(props: { title: string }) {
  return <div>{props.title}</div>;
}
"#;

    #[test]
    fn tsx_golden_occurrence_list() {
        // V3.G1: `w` ref at `w.area()` is KEPT (tsx is a locals language)
        // and binds to the param def. `props.title`'s `title` is a
        // property_identifier — not a local binding target.
        let occurrences = extract_occurrences("tsx", TSX_FIXTURE.as_bytes()).unwrap();
        assert_eq!(
            shorthand(&occurrences),
            vec![
                occ("Widget", "import", 1, None),
                occ("Shape", "def", 3, None),
                occ("area", "def", 4, None),
                occ("makeArea", "def", 7, None),
                occ("w", "def", 7, None),
                occ("Widget", "ref", 7, None),
                occ("w", "ref", 7, Some(4)), // → param w
                occ("area", "ref", 7, None),
                occ("Card", "def", 9, None),
                occ("props", "def", 9, None),
                occ("title", "def", 9, None),
                occ("div", "ref", 10, None),
                occ("props", "ref", 10, Some(9)), // → param props
                occ("title", "ref", 10, None),    // property, not local
                occ("div", "ref", 10, None),
            ],
            "got: {occurrences:#?}"
        );
    }

    // --- Python (B5b): import + from-import-as + class/method/self.attr ----

    const PYTHON_FIXTURE: &str = "import os\n\
         from collections import OrderedDict as OD\n\
         \n\
         class Greeter:\n    def __init__(self, name):\n        self.name = name\n\
         \n    def greet(self):\n        return self.name\n\
         \n\
         def main():\n    g = Greeter(\"world\")\n    print(g.greet())\n";

    #[test]
    fn python_golden_occurrence_list() {
        // V3.G1: single-char refs kept (python is a locals language). The
        // attribute field in `self.name` is NOT bound (member-field skip in
        // locals.rs); only the RHS `name` and lexical uses of `self`/`g`
        // get `local_def_ordinal`.
        let occurrences = extract_occurrences("python", PYTHON_FIXTURE.as_bytes()).unwrap();
        assert_eq!(
            shorthand(&occurrences),
            vec![
                occ("os", "import", 1, None),
                occ("collections", "import", 2, None),
                occ("OrderedDict", "import", 2, None),
                occ("OD", "import", 2, None),
                occ("Greeter", "def", 4, None),
                occ("__init__", "def", 5, None),
                occ("self", "def", 5, None),
                occ("name", "def", 5, None),
                occ("self", "ref", 6, Some(6)), // → param self
                occ("name", "ref", 6, None),    // attribute field — not lexical
                occ("name", "ref", 6, Some(7)), // RHS → param name
                occ("greet", "def", 8, None),
                occ("self", "def", 8, None),
                occ("self", "ref", 9, Some(12)), // → greet's self
                occ("name", "ref", 9, None),     // attribute field
                occ("main", "def", 11, None),
                occ("g", "def", 12, None),
                occ("Greeter", "ref", 12, Some(4)), // → class Greeter
                occ("print", "ref", 13, None),
                occ("g", "ref", 13, Some(16)), // → let g (kept: locals lang)
                occ("greet", "ref", 13, None),
            ],
            "got: {occurrences:#?}"
        );
    }

    #[test]
    fn python_with_as_and_except_as_bind_through_as_pattern_target() {
        let src = "with open(\"f\") as handle:\n    pass\ntry:\n    pass\nexcept Exception as trouble:\n    pass\n";
        let occurrences = extract_occurrences("python", src.as_bytes()).unwrap();
        assert!(
            occurrences
                .iter()
                .any(|o| o.name == "handle" && o.role == "def"),
            "with ... as should bind a def: {occurrences:#?}"
        );
        assert!(
            occurrences
                .iter()
                .any(|o| o.name == "trouble" && o.role == "def"),
            "except ... as should bind a def: {occurrences:#?}"
        );
        // `Exception` itself is a plain reference, never a def.
        assert!(occurrences
            .iter()
            .any(|o| o.name == "Exception" && o.role == "ref"));
    }

    // --- Ruby (B5b): require (a plain call, NOT import) + module/class/@ivar

    const RUBY_FIXTURE: &str = "require \"json\"\n\
         \n\
         module Shapes\n  class Circle\n    def initialize(radius)\n      @radius = radius\n    end\n\
         \n    def area\n      3.14 * @radius * @radius\n    end\n  end\nend\n\
         \n\
         def build\n  c = Shapes::Circle.new(2)\n  c.area\nend\n";

    #[test]
    fn ruby_golden_occurrence_list() {
        // Ruby is NOT a locals language — local_def_ordinal is always None;
        // single-char refs still trim under B5a.
        let occurrences = extract_occurrences("ruby", RUBY_FIXTURE.as_bytes()).unwrap();
        assert_eq!(
            shorthand(&occurrences),
            vec![
                // `require` is an ordinary METHOD CALL in Ruby's own
                // grammar (no import syntax at all) — classifies "ref",
                // never "import". See the module doc's `is_within_import`.
                occ("require", "ref", 1, None),
                occ("Shapes", "def", 3, None),
                occ("Circle", "def", 4, None),
                occ("initialize", "def", 5, None),
                occ("radius", "def", 5, None),
                // `@radius = radius`: the LEFT side (`@radius`, an
                // `instance_variable`) is a `def` of any length (7 chars —
                // never trimmed regardless); the right side is a plain
                // `ref` to the param.
                occ("@radius", "def", 6, None),
                occ("radius", "ref", 6, None),
                occ("area", "def", 9, None),
                // `3.14 * @radius * @radius` — both multi-char refs stay.
                occ("@radius", "ref", 10, None),
                occ("@radius", "ref", 10, None),
                occ("build", "def", 15, None),
                // `c = Shapes::Circle.new(2)` — `c` (1 char) is a `def`
                // (assignment left, stays regardless of length); `new` (3
                // chars) is a plain ref, stays.
                occ("c", "def", 16, None),
                occ("Shapes", "ref", 16, None),
                occ("Circle", "ref", 16, None),
                occ("new", "ref", 16, None),
                occ("area", "ref", 17, None),
                // `c.area`'s single-char RECEIVER `c` (a `ref`, not the
                // `def` above) is TRIMMED — see
                // `ruby_single_char_call_receiver_ref_is_trimmed`.
            ],
            "got: {occurrences:#?}"
        );
    }

    #[test]
    fn ruby_instance_variable_def_and_refs_survive_the_trim() {
        // `@radius` is 7 chars — nowhere NEAR the B5a single-char cutoff —
        // so both its `def` (the assignment target) and its two `ref`s
        // (the multiplication) must all survive.
        let occurrences = extract_occurrences("ruby", RUBY_FIXTURE.as_bytes()).unwrap();
        let ivar_defs = occurrences
            .iter()
            .filter(|o| o.name == "@radius" && o.role == "def")
            .count();
        let ivar_refs = occurrences
            .iter()
            .filter(|o| o.name == "@radius" && o.role == "ref")
            .count();
        assert_eq!(ivar_defs, 1, "got: {occurrences:#?}");
        assert_eq!(ivar_refs, 2, "got: {occurrences:#?}");
    }

    #[test]
    fn ruby_single_char_call_receiver_ref_is_trimmed() {
        // `c.area` on the fixture's last-but-one line — `c` is the
        // single-char receiver of a `call`, role `ref`: trimmed.
        let occurrences = extract_occurrences("ruby", RUBY_FIXTURE.as_bytes()).unwrap();
        assert!(
            !occurrences.iter().any(|o| o.name == "c" && o.role == "ref"),
            "a single-char ref must be trimmed: {occurrences:#?}"
        );
        // ...but `c`'s OWN `def` (`c = Shapes::Circle.new(2)`) must survive
        // — defs of any length always stay.
        assert!(occurrences.iter().any(|o| o.name == "c" && o.role == "def"));
    }

    // --- Go (B5b): package clause (ref) + aliased import + struct field + `:=`

    const GO_FIXTURE: &str = "package shapes\n\
         \n\
         import (\n\t\"fmt\"\n\tf \"os\"\n)\n\
         \n\
         type Point struct {\n\tX int\n}\n\
         \n\
         func main() {\n\tx, ok := f.Stat(\"a\")\n\tfmt.Println(x, ok)\n}\n";

    #[test]
    fn go_golden_occurrence_list() {
        // Go is NOT a locals language — B5a single-char ref trim still applies.
        let occurrences = extract_occurrences("go", GO_FIXTURE.as_bytes()).unwrap();
        assert_eq!(
            shorthand(&occurrences),
            vec![
                // The `package` clause's own name is a plain `ref` (not a
                // resolvable def in this table — see the module doc).
                occ("shapes", "ref", 1, None),
                // The aliased import spec's bound local name — "import"
                // role (an import binding, not a definition).
                occ("f", "import", 5, None),
                occ("Point", "def", 8, None),
                occ("X", "def", 9, None),
                occ("int", "ref", 9, None),
                occ("main", "def", 12, None),
                // `x, ok := f.Stat("a")` — BOTH sides of `:=` are `def`
                // (via `is_go_short_var_left`'s grandparent check), even
                // though `x` is a single char (defs of any length stay);
                // `f` (the selector's single-char operand) is a `ref` and
                // IS trimmed; `Stat` survives.
                occ("x", "def", 13, None),
                occ("ok", "def", 13, None),
                occ("Stat", "ref", 13, None),
                // `fmt.Println(x, ok)` — `fmt`/`Println` are multi-char
                // refs (stay); the bare-argument `x` is a single-char ref
                // (trimmed); `ok` survives.
                occ("fmt", "ref", 14, None),
                occ("Println", "ref", 14, None),
                occ("ok", "ref", 14, None),
            ],
            "got: {occurrences:#?}"
        );
    }

    // --- Bash (B5b): function name + local + expansion + invocation --------

    const BASH_OCCURRENCES_FIXTURE: &str =
        "greet() {\n  local name=\"$1\"\n  echo \"hi $name\"\n}\n\
         \n\
         for item in a b c; do\n  echo \"$item\"\ndone\n\
         export FOO\n\
         \n\
         greet world\n";

    #[test]
    fn bash_golden_occurrence_list() {
        // Bash is NOT a locals language — B5a single-char ref trim still applies.
        let occurrences = extract_occurrences("bash", BASH_OCCURRENCES_FIXTURE.as_bytes()).unwrap();
        assert_eq!(
            shorthand(&occurrences),
            vec![
                occ("greet", "def", 1, None),
                // `"$1"` (a positional parameter reference) is a
                // single-char ref — trimmed.
                occ("name", "def", 2, None),
                occ("echo", "ref", 3, None),
                occ("name", "ref", 3, None),
                occ("item", "def", 6, None),
                occ("echo", "ref", 7, None),
                occ("item", "ref", 7, None),
                occ("FOO", "def", 9, None),
                occ("greet", "ref", 11, None),
                // `greet world`'s bare argument `world` is a plain `word`
                // whose parent is neither `function_definition` nor
                // `command_name` — never even emitted as an occurrence
                // (not merely trimmed — genuinely excluded, see
                // `is_identifier_like`'s Bash arm).
            ],
            "got: {occurrences:#?}"
        );
    }

    // --- misc ----------------------------------------------------------------

    #[test]
    fn empty_source_yields_no_occurrences() {
        for lang in lang::TOKEN_LEVEL_LANG_IDS {
            assert_eq!(extract_occurrences(lang, b"").unwrap(), vec![]);
        }
    }

    #[test]
    fn unsupported_language_errors() {
        let err = extract_occurrences("yaml", b"").unwrap_err();
        assert!(matches!(err, LangError::Unsupported(_)), "got: {err:?}");
    }

    #[test]
    fn single_char_ref_is_trimmed_but_def_stays() {
        // B5a still trims single-char refs for NON-locals token languages.
        // JavaScript is token-level but not in `locals::supports`, so the
        // classic rule holds: def stays, ref of the same one-char name drops.
        let src = "let i = 1;\nconsole.log(i);\n";
        let occurrences = extract_occurrences("javascript", src.as_bytes()).unwrap();
        assert_eq!(
            shorthand(&occurrences),
            vec![
                occ("i", "def", 1, None),
                occ("console", "ref", 2, None),
                occ("log", "ref", 2, None),
                // `i` ref trimmed
            ],
            "got: {occurrences:#?}"
        );
    }

    #[test]
    fn locals_language_keeps_single_char_ref_and_binds() {
        // V3.G1 carve-out: python IS a locals language — single-char refs
        // stay so `local_def_ordinal` can attach (here: `i` → ord 0).
        let src = "i = 1\nprint(i)\n";
        let occurrences = extract_occurrences("python", src.as_bytes()).unwrap();
        assert_eq!(
            shorthand(&occurrences),
            vec![
                occ("i", "def", 1, None),
                occ("print", "ref", 2, None),
                occ("i", "ref", 2, Some(0)),
            ],
            "got: {occurrences:#?}"
        );
    }

    #[test]
    fn cap_stops_cleanly_without_erroring() {
        // A pathological file: MAX_OCCURRENCES_PER_FILE+500 top-level `const`
        // bindings, each contributing exactly one `identifier` occurrence.
        let mut src = String::new();
        for i in 0..(MAX_OCCURRENCES_PER_FILE + 500) {
            src.push_str(&format!("const v{i} = 1;\n"));
        }
        let occurrences = extract_occurrences("javascript", src.as_bytes()).unwrap();
        assert_eq!(occurrences.len(), MAX_OCCURRENCES_PER_FILE);
    }
}
