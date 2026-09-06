//! B4 — the `"import-heuristic"` precision tier for `GET /api/resolve`
//! (`resolve.rs`): given the identifier under the cursor, if it is imported
//! in the CURRENT file, follow the import's own module path to the file it
//! names and let `resolve.rs` search THAT file's symbols for a name match —
//! ranked above the fleet-wide `"tags-approx"` tier (same repo/other repo),
//! below `"file-local"`.
//!
//! Two independent halves, mirroring the module doc's own two-step design:
//!
//! 1. [`import_origin`] — a tree-sitter walk of the CURRENT file (on demand;
//!    no new schema, no cached table — `occurrences.rs`'s `role = "import"`
//!    rows carry only NAMES, never a source path, which is why this parses
//!    fresh each time it's needed) that answers "if `ident` is imported
//!    here, what module path did it come from, and — if renamed via an
//!    `as` clause — what name does the DEFINING file actually declare?"
//! 2. [`resolve_module_file`] — a fixed, per-language heuristic ladder that
//!    turns that module path into an actual repo-relative file, IF one
//!    exists on disk. Every candidate path is validated to stay inside the
//!    repo root (canonicalize + `starts_with`, the same gate `spa.rs`'s
//!    `read_asset` uses) before it's ever handed back.
//!
//! # Deliberately NOT done here (see the design brief)
//!
//! No type inference, no crate/package graph, no `Cargo.toml`/`package.json`
//! dependency resolution, no re-export chase beyond one hop, no scope
//! lattice. A path that doesn't exist on disk under this heuristic's fixed
//! ladder simply yields `None` — `resolve.rs` then has nothing to add at
//! this tier and falls through to `"tags-approx"` exactly as before B4.
//! Concretely out of scope (see each fn's own doc for the exact boundary):
//! cross-CRATE Rust imports (`use kb_core::Foo;` from a sibling crate — the
//! heuristic only ever looks inside the CURRENT crate's own `src/`), a
//! middle path segment click (`b` in `use a::b::C;` — only the FINAL
//! segment, i.e. the actual imported name, resolves), and any non-relative
//! TS/JS specifier (a bare package name — `import x from "lodash"` — is
//! never followed onto `node_modules`).
//!
//! # B5b — Python and Go join the ladder; Ruby and Bash deliberately don't
//!
//! Python's `import`/`from ... import` (both absolute AND relative, `from .
//! import x`) and Go's `import` specs (plain and aliased) get the SAME
//! treatment as Rust/TS — see their own `_import_origin`/`resolve_*_module_file`
//! pairs below. Python resolves against the REPO ROOT (no `sys.path`/
//! `PYTHONPATH` awareness — same "assume the obvious default" heuristic
//! Rust's bare-path-defaults-to-crate-relative choice already makes). Go
//! resolves an import path against the repo's OWN declared module path (read
//! straight from `go.mod`'s `module` line — no further `go.sum`/vendor
//! resolution), and — since a Go PACKAGE is a directory, not a file, and
//! this fn's contract returns exactly ONE file — picks the alphabetically
//! FIRST non-`_test.go` file in the resolved package directory; a symbol
//! defined in a SIBLING file of that same package is NOT found this way
//! (documented, not silently wrong: `resolve.rs`'s tags-tier still finds it,
//! just at the lower `"tags-approx"` precision). Ruby and Bash get NEITHER:
//! Ruby's `require "foo"` is an ordinary method call with no grammar-level
//! import construct to walk (see `occurrences.rs`'s own doc for the same
//! point), and Bash has no import construct whatsoever — both are honest,
//! permanent gaps, not merely unmeasured.

use crate::lang;
use std::path::{Path, PathBuf};
use tree_sitter::Node;

/// One parsed import touching an identifier. Never carries a resolved
/// filesystem path itself (see [`resolve_module_file`] for that step) — just
/// enough to attempt the resolution and to know which name to search for in
/// whatever file it resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportOrigin {
    /// The module path as written in source, in the language's own `::`/`/`
    /// notation, ending in the DEFINING name (the original name, not a
    /// rename alias — see `alias_of`) for a named/default/namespace import,
    /// or just the bare module path for a glob (`use a::b::*` has no
    /// trailing item). Never resolved against the filesystem here.
    pub raw_module: String,
    /// `Some(original)` when the clicked identifier is a RENAMED alias
    /// (`use a::b::D as E` clicking `E`, or `import { D as E }` clicking
    /// `E`, or `export { D as E }` clicking `E`) — search the defining
    /// file's symbols for `original`, not the clicked name. `None` when the
    /// clicked name already IS the defining name (including every TS/JS
    /// case this phase covers: default/named/namespace imports never
    /// rename the SOURCE side, only re-export/import aliasing does, which
    /// this covers via the same field).
    pub alias_of: Option<String>,
    /// `true` only for a Rust glob import (`use a::b::*;`) used as a
    /// FALLBACK origin when no named import in the file declares `ident`
    /// directly — see [`rust_import_origin`]'s doc. TypeScript/TSX/
    /// JavaScript never set this: every TS/JS import form binds a definite,
    /// unambiguous local name, so there is no "maybe from one of several
    /// globs" case to flag. `resolve.rs` does not need to branch on this
    /// flag to decide HOW to search (the target file's symbols are always
    /// searched by name, glob or not — see that module's doc) — it exists
    /// so a glob-derived origin can be told apart from a confident named
    /// one, and so multiple globs in one file resolve deterministically
    /// (first-declared wins, see [`rust_import_origin`]).
    pub glob: bool,
}

/// The languages this pass covers — a SUBSET of `occurrences.rs`'s own
/// `lang::TOKEN_LEVEL_LANG_IDS` (import-following only makes sense where
/// occurrences classify a `role = "import"` in the first place, but not
/// every token-level language actually HAS import syntax: Ruby's `require`
/// is a plain method call and Bash has no import construct at all — see
/// the module doc's B5b section). Rust/TypeScript/TSX/JavaScript (B4) plus
/// Python/Go (B5b).
pub fn supports(lang_id: &str) -> bool {
    matches!(
        lang_id,
        "rust" | "typescript" | "tsx" | "javascript" | "python" | "go"
    )
}

/// If `ident` is imported in `source` (parsed as `lang_id`), the import's
/// origin — `None` if `ident` isn't imported at all in this file, if the
/// language isn't one of [`supports`]'s four, or if the only governing
/// import is a non-relative TS/JS specifier (see [`ts_import_origin`]'s
/// doc — that case is explicitly out of scope, not an error).
pub fn import_origin(lang_id: &str, source: &[u8], ident: &str) -> Option<ImportOrigin> {
    match lang_id {
        "rust" => rust_import_origin(source, ident),
        "typescript" | "tsx" | "javascript" => ts_import_origin(lang_id, source, ident),
        "python" => python_import_origin(source, ident),
        "go" => go_import_origin(source, ident),
        _ => None,
    }
}

// --- Rust ------------------------------------------------------------------

/// Walk every `use_declaration` (any nesting depth — a `use` inside a
/// function body is valid Rust) and every bodiless `mod foo;` item, looking
/// for `ident` as:
/// - a bare name in a `use_list` (`use a::b::{C, D}` clicking `C`/`D`),
/// - the final `name` field of a `scoped_identifier` (`use a::b::C`
///   clicking `C` — the ONLY segment this ever matches; clicking a middle
///   segment like `b` is out of scope, see the module doc),
/// - the `alias` of a `use_as_clause` (`use a::b::D as E` clicking `E` —
///   resolves to `alias_of: Some("D".into())`),
/// - the name of a bodiless `mod foo;` declaration (`raw_module:
///   "self::foo"`) — the one case where `ident` is technically a `"def"`
///   occurrence (`occurrences.rs`'s `is_def` table), not an `"import"` one,
///   but still names a FILE worth following.
///
/// A `use a::b::*;` glob never matches `ident` by name (it has none) — every
/// glob in the file is collected separately and, ONLY if no named/aliased/
/// mod-decl match was found anywhere, the FIRST one (source order) is
/// returned as a fallback origin with `glob: true`. Multiple named imports
/// of the same local name can't legally coexist in valid Rust, so "first
/// match wins" for the named case is deterministic in practice, not just in
/// principle.
fn rust_import_origin(source: &[u8], ident: &str) -> Option<ImportOrigin> {
    let (tree, _language) = lang::parse(lang::RUST.id, source).ok()?;
    let mut named: Option<ImportOrigin> = None;
    let mut globs: Vec<ImportOrigin> = Vec::new();
    walk_rust_node(tree.root_node(), source, ident, &mut named, &mut globs);
    named.or_else(|| globs.into_iter().next())
}

fn walk_rust_node(
    node: Node<'_>,
    source: &[u8],
    ident: &str,
    named: &mut Option<ImportOrigin>,
    globs: &mut Vec<ImportOrigin>,
) {
    match node.kind() {
        "use_declaration" => {
            if let Some(argument) = node.child_by_field_name("argument") {
                walk_rust_use_clause(argument, "", source, ident, named, globs);
            }
        }
        // `mod foo;` (no body) names a CHILD FILE — `mod foo { .. }` (has a
        // body) is an inline module, nothing to follow (the code is already
        // right here, in the current file).
        "mod_item" if node.child_by_field_name("body").is_none() => {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source) {
                    if name == ident && named.is_none() {
                        *named = Some(ImportOrigin {
                            raw_module: format!("self::{name}"),
                            alias_of: None,
                            glob: false,
                        });
                    }
                }
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_rust_node(child, source, ident, named, globs);
    }
}

/// Recursively walk one `use` argument/clause, accumulating `prefix` (the
/// `::`-joined path text contributed by enclosing `scoped_use_list`s) —
/// see each node-kind arm for the exact grammar shape (verified against
/// `tree-sitter-rust 0.24.2`'s own `node-types.json`, not guessed).
fn walk_rust_use_clause(
    node: Node<'_>,
    prefix: &str,
    source: &[u8],
    ident: &str,
    named: &mut Option<ImportOrigin>,
    globs: &mut Vec<ImportOrigin>,
) {
    if named.is_some() {
        return;
    }
    match node.kind() {
        // A bare leaf name — either the whole `use foo;` argument, or one
        // entry in a `use_list` (`use a::{b, self}`).
        "identifier" | "crate" | "self" | "super" | "metavariable" => {
            if let Ok(text) = node.utf8_text(source) {
                if text == ident {
                    *named = Some(ImportOrigin {
                        raw_module: join_rust_path(prefix, text),
                        alias_of: None,
                        glob: false,
                    });
                }
            }
        }
        // A fully qualified single path (`a::b::C`) — matches ONLY on its
        // own `name` field (the final segment); a middle segment is out of
        // scope (module doc). `node`'s own text is already the correctly
        // formatted `"inner::path::C"` (tree-sitter nodes ARE their source
        // slice) so no manual path reconstruction is needed beyond joining
        // the OUTER prefix this node doesn't itself contain.
        "scoped_identifier" => {
            let Some(name_node) = node.child_by_field_name("name") else {
                return;
            };
            let Ok(name_text) = name_node.utf8_text(source) else {
                return;
            };
            if name_text == ident {
                let own_text = node.utf8_text(source).unwrap_or(name_text);
                *named = Some(ImportOrigin {
                    raw_module: join_rust_path(prefix, own_text),
                    alias_of: None,
                    glob: false,
                });
            }
        }
        // `path as alias` — clicking the ALIAS maps back to `path`'s own
        // final segment as the defining name.
        "use_as_clause" => {
            let (Some(path_node), Some(alias_node)) = (
                node.child_by_field_name("path"),
                node.child_by_field_name("alias"),
            ) else {
                return;
            };
            let Ok(alias_text) = alias_node.utf8_text(source) else {
                return;
            };
            if alias_text != ident {
                return;
            }
            let path_text = path_node.utf8_text(source).unwrap_or("");
            let original = if path_node.kind() == "scoped_identifier" {
                path_node
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or(path_text)
                    .to_string()
            } else {
                path_text.to_string()
            };
            *named = Some(ImportOrigin {
                raw_module: join_rust_path(prefix, path_text),
                alias_of: Some(original),
                glob: false,
            });
        }
        "use_list" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                walk_rust_use_clause(child, prefix, source, ident, named, globs);
                if named.is_some() {
                    return;
                }
            }
        }
        "scoped_use_list" => {
            let path_text = node
                .child_by_field_name("path")
                .and_then(|p| p.utf8_text(source).ok())
                .unwrap_or("");
            let new_prefix = join_rust_path(prefix, path_text);
            if let Some(list) = node.child_by_field_name("list") {
                walk_rust_use_clause(list, &new_prefix, source, ident, named, globs);
            }
        }
        "use_wildcard" => {
            // The wildcard's own leading path is an UNNAMED positional
            // child (no field in `node-types.json`) — `named_child(0)`,
            // absent for a bare top-level `use *;` (never valid Rust, but
            // defensive rather than panicking).
            let path_text = node
                .named_child(0)
                .and_then(|p| p.utf8_text(source).ok())
                .unwrap_or("");
            globs.push(ImportOrigin {
                raw_module: join_rust_path(prefix, path_text),
                alias_of: None,
                glob: true,
            });
        }
        _ => {}
    }
}

fn join_rust_path(prefix: &str, segment: &str) -> String {
    match (prefix.is_empty(), segment.is_empty()) {
        (true, _) => segment.to_string(),
        (false, true) => prefix.to_string(),
        (false, false) => format!("{prefix}::{segment}"),
    }
}

/// Find the crate root: the nearest ancestor directory (of `current_file`'s
/// OWN directory, walking up to and including the repo root) containing
/// `src/lib.rs` or `src/main.rs` — a monorepo/workspace layout
/// (`crates/kb-core/src/lib.rs`) resolves to `crates/kb-core`; a single-crate
/// repo (`src/lib.rs` directly under the repo root) resolves to the repo
/// root itself, which is also the unconditional fallback if no ancestor
/// matches (a file not under any `src/` at all, e.g. a build script at the
/// repo root — `crate::`-relative resolution against the repo root itself is
/// the most reasonable default, and simply won't find anything if wrong,
/// same "fails closed to nothing" honesty as every other miss in this
/// module). Returns a path RELATIVE to the repo root (`""` for the root
/// itself).
fn rust_crate_root(repo_root: &Path, current_file: &Path) -> PathBuf {
    let start_dir = current_file.parent().unwrap_or_else(|| Path::new(""));
    for ancestor in start_dir.ancestors() {
        let candidate = repo_root.join(ancestor);
        if candidate.join("src/lib.rs").is_file() || candidate.join("src/main.rs").is_file() {
            return ancestor.to_path_buf();
        }
    }
    PathBuf::new()
}

/// The directory `self::`/`super::` paths are relative to, FROM
/// `current_file`'s own module — Rust's file-to-module-tree convention:
/// `a/b.rs` (module `a::b`)'s children live under `a/b/`, UNLESS `b.rs` is
/// itself named `mod.rs`/`lib.rs`/`main.rs` (already ITS module's own
/// directory, nothing extra to descend into).
fn rust_module_dir(current_file: &Path) -> PathBuf {
    let parent = current_file.parent().unwrap_or_else(|| Path::new(""));
    let stem = current_file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if matches!(stem, "mod" | "lib" | "main") {
        parent.to_path_buf()
    } else {
        parent.join(stem)
    }
}

/// Which flavor of "base directory" a `raw_module`'s leading prefix
/// resolved to — needed because the two flavors have a DIFFERENT notion of
/// "the base's own defining file" (see [`resolve_rust_module_file`]'s final
/// fallback): a crate's `src/` directory's own file is `src/lib.rs` or
/// `src/main.rs` (a fixed pair of names, not derived from `src`'s OWN
/// name), whereas a plain module's children-directory (e.g. `src/a/b`, from
/// `self::`/`super::`) is a genuine "the directory itself IS shorthand for a
/// same-named file" case (`src/a/b.rs` or `src/a/b/mod.rs`).
enum RustBase {
    CrateSrc(PathBuf),
    ModuleDir(PathBuf),
}

impl RustBase {
    fn dir(&self) -> &Path {
        match self {
            RustBase::CrateSrc(p) | RustBase::ModuleDir(p) => p,
        }
    }
}

/// Resolve a Rust `raw_module` path (`crate::a::b::C`, `self::foo`,
/// `super::Bar`, or a bare `a::b::C` treated as crate-relative — see the
/// module doc's "Deliberately NOT done here" list for why a bare path is
/// NEVER treated as an external/sibling-crate reference) to a repo-relative
/// `.rs` file, if one exists.
///
/// Segments after the resolved base are consumed GREEDILY, longest prefix
/// first: `crate::a::b::C` tries `.../a/b/C.rs` (in case `C` is itself a
/// nested module) before `.../a/b.rs` (the common case — `C` is an item
/// defined inside it) — whichever exists first wins; `resolve.rs` always
/// searches the winning file's symbols by the CLICKED (or de-aliased) name
/// regardless of how many segments were consumed as directory vs. left over
/// as "the item", so no leftover-segment bookkeeping is needed here.
///
/// If NO prefix of the remaining segments names a file (the common
/// single-segment case: `self::c` where `c` is an item defined directly in
/// the CURRENT module's own file, or `super::sibling` likewise in the
/// PARENT module's own file), and exactly one segment remains, one more
/// candidate is tried: the base's own defining file. Deliberately gated to
/// "exactly one segment left" — a `self::bogus::Thing` with an
/// unresolvable middle segment must stay `None`, not silently fall back to
/// treating `bogus` as if it didn't exist and searching for `Thing` in the
/// wrong file.
fn resolve_rust_module_file(
    repo_root: &Path,
    current_file: &Path,
    raw_module: &str,
) -> Option<PathBuf> {
    let segments: Vec<&str> = raw_module.split("::").filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return None;
    }
    let (base, remaining): (RustBase, &[&str]) = match segments[0] {
        "crate" => (
            RustBase::CrateSrc(rust_crate_root(repo_root, current_file).join("src")),
            &segments[1..],
        ),
        "self" => (
            RustBase::ModuleDir(rust_module_dir(current_file)),
            &segments[1..],
        ),
        "super" => {
            let mut dir = rust_module_dir(current_file);
            let mut rest: &[&str] = &segments[..];
            while rest.first() == Some(&"super") {
                dir = dir.parent().map(Path::to_path_buf).unwrap_or_default();
                rest = &rest[1..];
            }
            (RustBase::ModuleDir(dir), rest)
        }
        // Bare path (no crate/self/super prefix): treated as crate-relative
        // by default (the common `use top_level_mod::Item;` shape). If
        // `segments[0]` actually names an external crate instead, every
        // candidate below simply won't exist on disk — a clean `None`, not
        // a misresolution.
        _ => (
            RustBase::CrateSrc(rust_crate_root(repo_root, current_file).join("src")),
            &segments[..],
        ),
    };
    if remaining.is_empty() {
        return None;
    }
    for take in (1..=remaining.len()).rev() {
        let rel_dir = remaining[..take].join("/");
        for candidate in [format!("{rel_dir}.rs"), format!("{rel_dir}/mod.rs")] {
            if let Some(hit) = safe_existing_file(repo_root, &base.dir().join(candidate)) {
                return Some(hit);
            }
        }
    }
    if remaining.len() == 1 {
        let own_file_candidates: Vec<PathBuf> = match &base {
            RustBase::CrateSrc(src_dir) => vec![src_dir.join("lib.rs"), src_dir.join("main.rs")],
            RustBase::ModuleDir(dir) => vec![
                PathBuf::from(format!("{}.rs", dir.display())),
                dir.join("mod.rs"),
            ],
        };
        for candidate in own_file_candidates {
            if let Some(hit) = safe_existing_file(repo_root, &candidate) {
                return Some(hit);
            }
        }
    }
    None
}

// --- TypeScript / TSX / JavaScript ------------------------------------------

/// Walk every `import_statement`/`export_statement` (any nesting — a
/// dynamic `import()` is a call expression, not one of these two
/// declaration forms, and is out of scope) looking for `ident` as:
/// - the default binding (`import Def from "./x"` clicking `Def`),
/// - the namespace binding (`import * as NS from "./x"` clicking `NS` — "the
///   module itself": `alias_of: None`, same as a default import; there is
///   no in-file name to be aliased FROM, so this is not the `use_as_clause`
///   case),
/// - a named specifier, aliased or not (`import { C, D as E } from "./x"`),
/// - a re-export specifier, aliased or not (`export { C, D as E } from
///   "./y"` — `export_specifier` has the identical `name`/`alias` field
///   shape as `import_specifier`, so [`match_specifier_like`] handles both).
///
/// Only a RELATIVE specifier (`./`, `../`) is ever followed — a bare package
/// specifier (`import x from "lodash"`) means this import is explicitly out
/// of scope for path-following (no `node_modules` resolution, no
/// `package.json` `main`/`exports` field), so it's skipped entirely: if
/// `ident`'s ONLY governing import is a bare specifier, this returns `None`
/// for it exactly as if it weren't imported at all — an honest "not
/// followable", not a wrong answer.
fn ts_import_origin(lang_id: &str, source: &[u8], ident: &str) -> Option<ImportOrigin> {
    let (tree, _language) = lang::parse(lang_id, source).ok()?;
    let mut named: Option<ImportOrigin> = None;
    walk_ts_node(tree.root_node(), source, ident, &mut named);
    named
}

fn walk_ts_node(node: Node<'_>, source: &[u8], ident: &str, named: &mut Option<ImportOrigin>) {
    if named.is_some() {
        return;
    }
    match node.kind() {
        "import_statement" => {
            if let Some(module) = node
                .child_by_field_name("source")
                .and_then(|s| relative_specifier(s, source))
            {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if child.kind() == "import_clause" {
                        match_import_clause(child, &module, source, ident, named);
                    }
                }
            }
        }
        "export_statement" => {
            if let Some(module) = node
                .child_by_field_name("source")
                .and_then(|s| relative_specifier(s, source))
            {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    if child.kind() == "export_clause" {
                        let mut c2 = child.walk();
                        for spec in child.named_children(&mut c2) {
                            if spec.kind() == "export_specifier" {
                                match_specifier_like(spec, &module, source, ident, named);
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
    if named.is_none() {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            walk_ts_node(child, source, ident, named);
        }
    }
}

/// The unquoted specifier text of an `import`/`export` `source` string node
/// — `Some` only when it's a RELATIVE specifier (see [`ts_import_origin`]'s
/// doc); `None` for a bare package specifier.
fn relative_specifier(node: Node<'_>, source: &[u8]) -> Option<String> {
    let raw = node.utf8_text(source).ok()?;
    let trimmed = raw.trim_matches(|c| c == '"' || c == '\'' || c == '`');
    if trimmed.starts_with("./") || trimmed.starts_with("../") {
        Some(trimmed.to_string())
    } else {
        None
    }
}

fn match_import_clause(
    node: Node<'_>,
    module: &str,
    source: &[u8],
    ident: &str,
    named: &mut Option<ImportOrigin>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if named.is_some() {
            return;
        }
        match child.kind() {
            // The default binding — a bare `identifier` child of
            // `import_clause` itself (`import Def, { C } from "./x"` can
            // have BOTH this and a `named_imports` sibling).
            "identifier" => {
                if let Ok(text) = child.utf8_text(source) {
                    if text == ident {
                        *named = Some(ImportOrigin {
                            raw_module: module.to_string(),
                            alias_of: None,
                            glob: false,
                        });
                    }
                }
            }
            "namespace_import" => {
                if let Some(id_node) = child.named_child(0) {
                    if let Ok(text) = id_node.utf8_text(source) {
                        if text == ident {
                            *named = Some(ImportOrigin {
                                raw_module: module.to_string(),
                                alias_of: None,
                                glob: false,
                            });
                        }
                    }
                }
            }
            "named_imports" => {
                let mut c2 = child.walk();
                for spec in child.named_children(&mut c2) {
                    if spec.kind() == "import_specifier" {
                        match_specifier_like(spec, module, source, ident, named);
                    }
                }
            }
            _ => {}
        }
    }
}

/// `import_specifier` and `export_specifier` share the identical
/// `name`(required)/`alias`(optional) field shape (verified against both
/// grammars' `node-types.json`), so one fn handles `import { D as E }` and
/// `export { D as E }` alike. The LOCAL binding — what a click in the
/// CURRENT file's own scope refers to — is `alias` when present, else
/// `name`; `alias_of` is `Some(name)` only when an alias is actually
/// present (a rename), never fabricated when there isn't one.
fn match_specifier_like(
    node: Node<'_>,
    module: &str,
    source: &[u8],
    ident: &str,
    named: &mut Option<ImportOrigin>,
) {
    if named.is_some() {
        return;
    }
    let name_node = node.child_by_field_name("name");
    let alias_node = node.child_by_field_name("alias");
    let Some(clicked_node) = alias_node.or(name_node) else {
        return;
    };
    let Ok(clicked_text) = clicked_node.utf8_text(source) else {
        return;
    };
    if clicked_text != ident {
        return;
    }
    let alias_of = if alias_node.is_some() {
        name_node
            .and_then(|n| n.utf8_text(source).ok())
            .map(str::to_string)
    } else {
        None
    };
    *named = Some(ImportOrigin {
        raw_module: module.to_string(),
        alias_of,
        glob: false,
    });
}

/// Lexically collapse `.`/`..` components WITHOUT touching the filesystem
/// (the candidate may not exist yet — we're about to try several
/// extensions against it). A leading `..` that has nothing left to pop
/// (an escape attempt past the repo root) is kept AS-IS rather than
/// dropped — [`safe_existing_file`]'s canonicalize + `starts_with` gate is
/// what actually rejects it; this fn's only job is textual normalization.
fn lexically_normalize(path: &Path) -> PathBuf {
    let mut out: Vec<std::path::Component<'_>> = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if matches!(out.last(), Some(std::path::Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push(component);
                }
            }
            other => out.push(other),
        }
    }
    out.into_iter().collect()
}

/// The standard bundler resolution ladder for a relative specifier: the
/// exact path, then `.ts`/`.tsx`/`.js`/`.jsx` appended, then `/index.ts`/
/// `/index.tsx`/`/index.js`/`/index.jsx` — in that order, first hit wins.
const TS_EXTENSION_LADDER: &[&str] = &[".ts", ".tsx", ".js", ".jsx"];
const TS_INDEX_LADDER: &[&str] = &["index.ts", "index.tsx", "index.js", "index.jsx"];

fn resolve_ts_module_file(
    repo_root: &Path,
    current_file: &Path,
    raw_module: &str,
) -> Option<PathBuf> {
    let base_dir = current_file.parent().unwrap_or_else(|| Path::new(""));
    let joined = lexically_normalize(&base_dir.join(raw_module));

    // 1. exact.
    if let Some(hit) = safe_existing_file(repo_root, &joined) {
        return Some(hit);
    }
    // 2. + extension.
    for ext in TS_EXTENSION_LADDER {
        let candidate = PathBuf::from(format!("{}{ext}", joined.display()));
        if let Some(hit) = safe_existing_file(repo_root, &candidate) {
            return Some(hit);
        }
    }
    // 3. /index.*
    for index_file in TS_INDEX_LADDER {
        if let Some(hit) = safe_existing_file(repo_root, &joined.join(index_file)) {
            return Some(hit);
        }
    }
    None
}

// --- Python (B5b) ------------------------------------------------------------

/// Walk every `import_statement`/`import_from_statement` (any nesting)
/// looking for `ident` as:
/// - the FIRST segment of a plain `import a.b.c` (Python only ever binds
///   the top-level package name locally, per the language's own import
///   semantics — clicking `b`/`c`, a MIDDLE-or-final segment, is out of
///   scope, same posture as Rust's middle-segment gap),
/// - the alias of a plain `import a.b.c as x` (`raw_module` still the FULL
///   dotted path — `alias_of: None`, same as TS's default/namespace import:
///   there's no "original name to search for," the import names the MODULE
///   itself),
/// - a `from`-imported name, plain or aliased (`from a.b import c[ as d]`),
///   ABSOLUTE or RELATIVE (`from . import c` / `from .pkg import c` / `from
///   ..pkg.sub import c`) — see [`python_module_path`] for how the leading
///   dots are encoded into `raw_module`.
///
/// A bare `from x import *` (wildcard) binds no fixed local name at all —
/// out of scope entirely (unlike Rust's glob, which at least has a
/// deterministic single fallback origin; Python's `*` could bind ANY name
/// from the target module, which this heuristic has no way to enumerate
/// without evaluating it) — `ident` simply won't match anything from such
/// an import, same as if it weren't imported.
fn python_import_origin(source: &[u8], ident: &str) -> Option<ImportOrigin> {
    let (tree, _language) = lang::parse(lang::PYTHON.id, source).ok()?;
    let mut found: Option<ImportOrigin> = None;
    walk_python_node(tree.root_node(), source, ident, &mut found);
    found
}

fn walk_python_node(node: Node<'_>, source: &[u8], ident: &str, found: &mut Option<ImportOrigin>) {
    if found.is_some() {
        return;
    }
    match node.kind() {
        "import_statement" => {
            let mut cursor = node.walk();
            for child in node.children_by_field_name("name", &mut cursor) {
                match_python_plain_import(child, source, ident, found);
                if found.is_some() {
                    return;
                }
            }
        }
        "import_from_statement" => {
            if let Some(module_node) = node.child_by_field_name("module_name") {
                if let Some(module_path) = python_module_path(module_node, source) {
                    let mut cursor = node.walk();
                    for child in node.children_by_field_name("name", &mut cursor) {
                        match_python_from_name(child, &module_path, source, ident, found);
                        if found.is_some() {
                            return;
                        }
                    }
                }
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_python_node(child, source, ident, found);
        if found.is_some() {
            return;
        }
    }
}

/// One `import_statement`'s `name` entry — either a bare `dotted_name`
/// (`import a.b.c`) or an `aliased_import` (`import a.b.c as x`).
fn match_python_plain_import(
    node: Node<'_>,
    source: &[u8],
    ident: &str,
    found: &mut Option<ImportOrigin>,
) {
    match node.kind() {
        "dotted_name" => {
            let mut cursor = node.walk();
            let segments: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
            let Some(first) = segments.first() else {
                return;
            };
            let Ok(first_text) = first.utf8_text(source) else {
                return;
            };
            if first_text == ident {
                *found = Some(ImportOrigin {
                    raw_module: dotted_name_slash_path(node, source),
                    alias_of: None,
                    glob: false,
                });
            }
        }
        "aliased_import" => {
            let (Some(name_node), Some(alias_node)) = (
                node.child_by_field_name("name"),
                node.child_by_field_name("alias"),
            ) else {
                return;
            };
            let Ok(alias_text) = alias_node.utf8_text(source) else {
                return;
            };
            if alias_text == ident {
                *found = Some(ImportOrigin {
                    raw_module: dotted_name_slash_path(name_node, source),
                    alias_of: None,
                    glob: false,
                });
            }
        }
        _ => {}
    }
}

/// One `import_from_statement`'s `name` entry — a `dotted_name` (plain
/// `from x import y`) or an `aliased_import` (`from x import y as z`).
/// `module_path` is already resolved (see [`python_module_path`]);
/// `raw_module` here APPENDS the imported name as one more trailing
/// segment, mirroring Rust's `use a::b::C` shape (`C` is just the final
/// path segment) — [`resolve_python_module_file`]'s greedy ladder then
/// tries it BOTH as "its own submodule file" and, failing that, as a
/// symbol inside `module_path`'s own file.
fn match_python_from_name(
    node: Node<'_>,
    module_path: &str,
    source: &[u8],
    ident: &str,
    found: &mut Option<ImportOrigin>,
) {
    match node.kind() {
        "dotted_name" => {
            let Ok(text) = node.utf8_text(source) else {
                return;
            };
            if text == ident {
                *found = Some(ImportOrigin {
                    raw_module: format!("{module_path}/{text}"),
                    alias_of: None,
                    glob: false,
                });
            }
        }
        "aliased_import" => {
            let (Some(name_node), Some(alias_node)) = (
                node.child_by_field_name("name"),
                node.child_by_field_name("alias"),
            ) else {
                return;
            };
            let Ok(alias_text) = alias_node.utf8_text(source) else {
                return;
            };
            if alias_text != ident {
                return;
            }
            let Ok(name_text) = name_node.utf8_text(source) else {
                return;
            };
            *found = Some(ImportOrigin {
                raw_module: format!("{module_path}/{name_text}"),
                alias_of: Some(name_text.to_string()),
                glob: false,
            });
        }
        _ => {}
    }
}

/// The `module_name` field of an `import_from_statement` — either a plain
/// `dotted_name` (absolute, `from a.b import ...`) or a `relative_import`
/// (`from . import ...` / `from .pkg import ...` / `from ..pkg.sub import
/// ...`, a dotted-leading-dots `import_prefix` child plus an OPTIONAL
/// trailing `dotted_name`). Encodes the result as N leading literal `.`
/// characters (N = the relative depth — 0 for an absolute import) followed
/// by a `/`-joined path, e.g. `"os/path"` (absolute), `"./"` (bare `from .
/// import x`), or `"../pkg/sub"` (`from ..pkg.sub import x`) —
/// [`resolve_python_module_file`]'s own convention, never a real Python
/// import spelling.
fn python_module_path(node: Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "dotted_name" => Some(dotted_name_slash_path(node, source)),
        "relative_import" => {
            let mut dots = String::new();
            let mut rest = String::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                match child.kind() {
                    "import_prefix" => dots = child.utf8_text(source).ok()?.to_string(),
                    "dotted_name" => rest = dotted_name_slash_path(child, source),
                    _ => {}
                }
            }
            if dots.is_empty() {
                return None;
            }
            Some(format!("{dots}/{rest}"))
        }
        _ => None,
    }
}

fn dotted_name_slash_path(node: Node<'_>, source: &[u8]) -> String {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter_map(|n| n.utf8_text(source).ok())
        .collect::<Vec<_>>()
        .join("/")
}

/// Resolve a `raw_module` string built by [`python_import_origin`]'s
/// helpers to a repo-relative `.py` file. Leading `.` characters (see
/// [`python_module_path`]'s doc) select the BASE directory: zero dots = the
/// repo root (absolute import, the common `import top_level_pkg.mod`
/// shape); one dot = `current_file`'s own containing directory (its
/// "package"); each EXTRA dot walks up one more parent — the identical
/// "current module dir, pop once per extra level" rule
/// [`rust_module_dir`]/[`resolve_rust_module_file`]'s `super::` chain
/// already uses, just spelled with dots instead of the `super` keyword.
///
/// The remaining (non-dot) segments are then consumed GREEDILY, longest
/// prefix first — the SAME algorithm [`resolve_rust_module_file`] uses for
/// `use a::b::C`: `<base>/pkg/Z.py` / `<base>/pkg/Z/__init__.py` (Z is
/// itself a submodule) tried BEFORE `<base>/pkg.py` / `<base>/pkg/
/// __init__.py` (Z is a symbol defined inside pkg's own file) — whichever
/// exists first wins; the caller always searches the winning file's symbols
/// by the imported (or de-aliased) name regardless of which rung matched.
fn resolve_python_module_file(
    repo_root: &Path,
    current_file: &Path,
    raw_module: &str,
) -> Option<PathBuf> {
    let dot_count = raw_module.chars().take_while(|&c| c == '.').count();
    let rest = &raw_module[dot_count..];
    let base_dir: PathBuf = if dot_count == 0 {
        PathBuf::new()
    } else {
        let mut dir = current_file
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf();
        for _ in 1..dot_count {
            dir = dir.parent().map(Path::to_path_buf).unwrap_or_default();
        }
        dir
    };
    let segments: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return None;
    }
    for take in (1..=segments.len()).rev() {
        let rel = segments[..take].join("/");
        for candidate in [format!("{rel}.py"), format!("{rel}/__init__.py")] {
            let full = if base_dir.as_os_str().is_empty() {
                PathBuf::from(&candidate)
            } else {
                base_dir.join(&candidate)
            };
            if let Some(hit) = safe_existing_file(repo_root, &full) {
                return Some(hit);
            }
        }
    }
    // `from . import c` / `from .. import c` — no package segment between
    // the dots and `c`, so the greedy loop above only ever tried `c` AS a
    // submodule of `base_dir`. The remaining possibility (mirrors Rust's
    // `self::c`/`super::c` "base's own defining file" fallback,
    // `resolve_rust_module_file`'s doc): `c` is a SYMBOL defined directly in
    // `base_dir`'s own package file. Deliberately gated to a RELATIVE import
    // (`dot_count > 0`) with exactly one segment left — an absolute
    // `from pkg import widget` already covers the analogous case via the
    // greedy loop's own `take == 1` rung (trying `pkg.py` directly against
    // the repo root), so this fallback would be redundant (and wrong-shaped
    // — there's no single "repo root's own file" pair of names) there.
    if dot_count > 0 && segments.len() == 1 {
        for candidate in [
            PathBuf::from(format!("{}.py", base_dir.display())),
            base_dir.join("__init__.py"),
        ] {
            if let Some(hit) = safe_existing_file(repo_root, &candidate) {
                return Some(hit);
            }
        }
    }
    None
}

// --- Go (B5b) ------------------------------------------------------------

/// Walk every `import_declaration` (single-spec or parenthesized
/// `import_spec_list` form — both nest `import_spec` the same way) looking
/// for `ident` as the bound local name of one spec:
/// - an EXPLICIT alias (`import f "os"` — `name` field is a
///   `package_identifier`, bound name `f`),
/// - the LAST segment of the import path, when no explicit alias is given
///   (`import "net/http"` binds `http`) — a documented heuristic, not the
///   package's own declared `package` clause name (which this pass never
///   resolves, since that would mean opening the target file first — see
///   the module doc's B5b section).
///
/// A blank import (`import _ "pkg"`, name field `blank_identifier`) and a
/// dot-import (`import . "pkg"`, name field `dot`) are both skipped
/// entirely — neither binds a clickable local name (`_` is never a real
/// identifier to search for, and `.` imports every exported name into the
/// current file's namespace directly, which this heuristic has no way to
/// enumerate without evaluating the target package — the same "can't
/// enumerate a wildcard" reasoning Python's `from x import *` gap shares).
fn go_import_origin(source: &[u8], ident: &str) -> Option<ImportOrigin> {
    let (tree, _language) = lang::parse(lang::GO.id, source).ok()?;
    let mut found: Option<ImportOrigin> = None;
    walk_go_node(tree.root_node(), source, ident, &mut found);
    found
}

fn walk_go_node(node: Node<'_>, source: &[u8], ident: &str, found: &mut Option<ImportOrigin>) {
    if found.is_some() {
        return;
    }
    if node.kind() == "import_spec" {
        match_go_import_spec(node, source, ident, found);
        return; // import_spec has no import_spec descendants to recurse into
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_go_node(child, source, ident, found);
        if found.is_some() {
            return;
        }
    }
}

fn match_go_import_spec(
    node: Node<'_>,
    source: &[u8],
    ident: &str,
    found: &mut Option<ImportOrigin>,
) {
    let Some(path_node) = node.child_by_field_name("path") else {
        return;
    };
    let Ok(path_text) = path_node.utf8_text(source) else {
        return;
    };
    let raw_module = path_text.trim_matches(|c| c == '"' || c == '`').to_string();
    if raw_module.is_empty() {
        return;
    }
    let bound_name = match node.child_by_field_name("name") {
        Some(name_node) if name_node.kind() == "package_identifier" => {
            name_node.utf8_text(source).ok().map(str::to_string)
        }
        // `_` (blank import) and `.` (dot import) bind nothing clickable —
        // see this fn's own doc.
        Some(_) => return,
        // No explicit name — the LAST path segment (a documented heuristic;
        // see the module doc's B5b section).
        None => raw_module.rsplit('/').next().map(str::to_string),
    };
    if bound_name.as_deref() == Some(ident) {
        *found = Some(ImportOrigin {
            raw_module,
            alias_of: None,
            glob: false,
        });
    }
}

/// Resolve a Go import path to a repo-relative `.go` file: read the repo's
/// own declared module path from `go.mod`'s `module` line, strip that
/// prefix from `raw_module` to get the intra-repo subdirectory, then pick
/// the alphabetically FIRST non-`_test.go` file in that directory (see the
/// module doc's B5b section for why one file, not the whole package). `None`
/// for anything this can't place inside the CURRENT repo: no `go.mod`, a
/// `raw_module` that isn't the module path itself or prefixed by it (an
/// external/stdlib import — `"fmt"`, `"github.com/other/pkg"`), or a
/// resolved directory with no `.go` files.
fn resolve_go_module_file(repo_root: &Path, raw_module: &str) -> Option<PathBuf> {
    let module_path = go_module_path(repo_root)?;
    let rel: PathBuf = if raw_module == module_path {
        PathBuf::new()
    } else if let Some(stripped) = raw_module.strip_prefix(&format!("{module_path}/")) {
        PathBuf::from(stripped)
    } else {
        return None;
    };
    let dir = if rel.as_os_str().is_empty() {
        repo_root.to_path_buf()
    } else {
        repo_root.join(&rel)
    };
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()) == Some("go")
                && !p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .ends_with("_test.go")
        })
        .collect();
    candidates.sort();
    let first = candidates.into_iter().next()?;
    let rel_file = first.strip_prefix(repo_root).ok()?.to_path_buf();
    safe_existing_file(repo_root, &rel_file)
}

/// Read the `module` directive from `<repo_root>/go.mod` — the FIRST line
/// starting with `"module "` (Go's own grammar requires it be the module
/// file's first non-comment statement in practice; a defensive `lines()`
/// scan rather than assuming line 1 costs nothing). `None` if there's no
/// `go.mod` at the repo root at all (a Go repo without modules, or simply
/// not a Go repo) or it has no `module` line.
fn go_module_path(repo_root: &Path) -> Option<String> {
    let content = std::fs::read_to_string(repo_root.join("go.mod")).ok()?;
    for line in content.lines() {
        if let Some(rest) = line.trim().strip_prefix("module ") {
            let path = rest.trim();
            if !path.is_empty() {
                return Some(path.to_string());
            }
        }
    }
    None
}

// --- shared ------------------------------------------------------------------

/// Dispatch to the per-language module-file resolver — `None` for any
/// language outside [`supports`]. `current_file` is repo-relative (the same
/// `path` query param `resolve.rs` already has in hand); the returned
/// `PathBuf` is ALSO repo-relative (suitable to feed straight into
/// `store::Store::get_file`), never absolute.
pub fn resolve_module_file(
    repo_root: &Path,
    current_file: &Path,
    lang_id: &str,
    raw_module: &str,
) -> Option<PathBuf> {
    match lang_id {
        "rust" => resolve_rust_module_file(repo_root, current_file, raw_module),
        "typescript" | "tsx" | "javascript" => {
            resolve_ts_module_file(repo_root, current_file, raw_module)
        }
        "python" => resolve_python_module_file(repo_root, current_file, raw_module),
        "go" => resolve_go_module_file(repo_root, raw_module),
        _ => None,
    }
}

/// `Some(rel_candidate)` iff `repo_root.join(rel_candidate)` names an
/// existing FILE that stays inside `repo_root` once both sides are
/// canonicalized — the same escape gate `spa.rs`'s `read_asset` uses for the
/// SPA's own static-asset serving, applied here to a heuristically
/// CONSTRUCTED (not user-request-derived, but still not to be trusted
/// blindly — a crafted `super::super::...` chain or a `../../../etc/passwd`
/// TS specifier could otherwise walk outside the repo) candidate path.
fn safe_existing_file(repo_root: &Path, rel_candidate: &Path) -> Option<PathBuf> {
    let abs = repo_root.join(rel_candidate);
    if !abs.is_file() {
        return None;
    }
    let canon_abs = abs.canonicalize().ok()?;
    let canon_root = repo_root.canonicalize().ok()?;
    if !canon_abs.starts_with(&canon_root) {
        return None;
    }
    Some(rel_candidate.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(root: &Path, rel: &str, content: &str) {
        let abs = root.join(rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(abs, content).unwrap();
    }

    // --- Rust: import_origin -------------------------------------------

    #[test]
    fn rust_plain_single_path_import() {
        let src = "use a::b::C;\nfn main() {}\n";
        let origin = rust_import_origin(src.as_bytes(), "C").unwrap();
        assert_eq!(origin.raw_module, "a::b::C");
        assert_eq!(origin.alias_of, None);
        assert!(!origin.glob);
    }

    #[test]
    fn rust_brace_list_plain_and_aliased() {
        let src = "use a::b::{C, D as E};\n";
        let c = rust_import_origin(src.as_bytes(), "C").unwrap();
        assert_eq!(c.raw_module, "a::b::C");
        assert_eq!(c.alias_of, None);

        // Clicking the ALIAS `E` maps back to the original name `D`.
        let e = rust_import_origin(src.as_bytes(), "E").unwrap();
        assert_eq!(e.raw_module, "a::b::D");
        assert_eq!(e.alias_of.as_deref(), Some("D"));

        // The original name `D` itself is never a local binding once
        // renamed — nothing in THIS file refers to plain `D`.
        assert_eq!(rust_import_origin(src.as_bytes(), "D"), None);
    }

    #[test]
    fn rust_top_level_use_as_clause_aliased() {
        let src = "use a::b::Original as Renamed;\n";
        let origin = rust_import_origin(src.as_bytes(), "Renamed").unwrap();
        assert_eq!(origin.raw_module, "a::b::Original");
        assert_eq!(origin.alias_of.as_deref(), Some("Original"));
    }

    #[test]
    fn rust_glob_is_a_fallback_only_when_nothing_named_matches() {
        let src = "use a::b::{Named};\nuse c::d::*;\n";
        // `Named` is a real named import — never falls back to the glob.
        let named = rust_import_origin(src.as_bytes(), "Named").unwrap();
        assert!(!named.glob);

        // Some OTHER identifier, not declared by any named import, falls
        // back to the glob.
        let glob = rust_import_origin(src.as_bytes(), "Mystery").unwrap();
        assert_eq!(glob.raw_module, "c::d");
        assert_eq!(glob.alias_of, None);
        assert!(glob.glob);
    }

    #[test]
    fn rust_mod_decl_without_body_is_followable() {
        let src = "mod foo;\nmod bar { fn x() {} }\n";
        let foo = rust_import_origin(src.as_bytes(), "foo").unwrap();
        assert_eq!(foo.raw_module, "self::foo");
        assert!(!foo.glob);

        // `mod bar { .. }` HAS a body — it's inline, nothing to follow.
        assert_eq!(rust_import_origin(src.as_bytes(), "bar"), None);
    }

    #[test]
    fn rust_middle_path_segment_click_is_out_of_scope() {
        // Clicking `b` (a middle segment, not the final imported name) is a
        // documented punt — only the final segment ever resolves.
        let src = "use a::b::C;\n";
        assert_eq!(rust_import_origin(src.as_bytes(), "b"), None);
    }

    #[test]
    fn rust_no_import_at_all_is_none() {
        let src = "fn main() { let widget = 1; }\n";
        assert_eq!(rust_import_origin(src.as_bytes(), "widget"), None);
    }

    // --- Rust: resolve_module_file --------------------------------------

    #[test]
    fn rust_crate_relative_path_resolves_through_the_module_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "Cargo.toml", "[package]\nname=\"fixture\"\n");
        write_file(root, "src/lib.rs", "mod a;\n");
        write_file(root, "src/a.rs", "pub mod b;\n");
        write_file(root, "src/a/b.rs", "pub fn c() {}\n");

        let hit = resolve_module_file(root, Path::new("src/lib.rs"), "rust", "crate::a::b::c")
            .expect("should resolve to src/a/b.rs");
        assert_eq!(hit, Path::new("src/a/b.rs"));
    }

    #[test]
    fn rust_bare_path_defaults_to_crate_relative() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "src/lib.rs", "mod a;\n");
        write_file(root, "src/a.rs", "pub fn widget() {}\n");

        let hit = resolve_module_file(root, Path::new("src/lib.rs"), "rust", "a::widget")
            .expect("bare path should default to crate-relative");
        assert_eq!(hit, Path::new("src/a.rs"));
    }

    #[test]
    fn rust_self_and_super_resolve_relative_to_the_current_module() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "src/lib.rs", "mod a;\n");
        write_file(root, "src/a.rs", "pub mod b; pub fn sibling() {}\n");
        write_file(root, "src/a/b.rs", "use super::sibling; use self::c::D;\n");
        write_file(root, "src/a/b/c.rs", "pub struct D;\n");

        // `super::sibling` from `src/a/b.rs` (module `a::b`) → `src/a.rs`.
        let sup = resolve_module_file(root, Path::new("src/a/b.rs"), "rust", "super::sibling")
            .expect("super:: should resolve to the parent module's file");
        assert_eq!(sup, Path::new("src/a.rs"));

        // `self::c::D` from `src/a/b.rs` → `src/a/b/c.rs`.
        let slf = resolve_module_file(root, Path::new("src/a/b.rs"), "rust", "self::c::D")
            .expect("self:: should resolve inside the current module's own directory");
        assert_eq!(slf, Path::new("src/a/b/c.rs"));
    }

    #[test]
    fn rust_mod_decl_resolves_to_the_child_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "src/lib.rs", "mod util;\n");
        write_file(root, "src/util.rs", "pub fn helper() {}\n");

        let hit = resolve_module_file(root, Path::new("src/lib.rs"), "rust", "self::util")
            .expect("mod decl's self:: path should resolve to the sibling file");
        assert_eq!(hit, Path::new("src/util.rs"));
    }

    #[test]
    fn rust_unresolvable_module_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "src/lib.rs", "\n");
        // `serde` is an external crate — there is no `src/serde.rs`.
        assert_eq!(
            resolve_module_file(root, Path::new("src/lib.rs"), "rust", "serde::Deserialize"),
            None
        );
    }

    #[test]
    fn rust_excessive_super_never_escapes_the_repo_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "src/lib.rs", "\n");
        // Far more `super::`s than there are ancestor directories — must
        // clamp harmlessly, never panic, never escape.
        let result = resolve_module_file(
            root,
            Path::new("src/lib.rs"),
            "rust",
            "super::super::super::super::etc",
        );
        assert_eq!(result, None);
    }

    // --- TS/TSX/JS: import_origin ----------------------------------------

    #[test]
    fn ts_named_plain_and_aliased() {
        let src = r#"import { C, D as E } from "./x";"#;
        let c = ts_import_origin("typescript", src.as_bytes(), "C").unwrap();
        assert_eq!(c.raw_module, "./x");
        assert_eq!(c.alias_of, None);

        let e = ts_import_origin("typescript", src.as_bytes(), "E").unwrap();
        assert_eq!(e.raw_module, "./x");
        assert_eq!(e.alias_of.as_deref(), Some("D"));
    }

    #[test]
    fn ts_default_import() {
        let src = r#"import Def from "./x";"#;
        let origin = ts_import_origin("typescript", src.as_bytes(), "Def").unwrap();
        assert_eq!(origin.raw_module, "./x");
        assert_eq!(origin.alias_of, None);
        assert!(!origin.glob);
    }

    #[test]
    fn ts_namespace_import_is_the_module_itself() {
        let src = r#"import * as NS from "./x";"#;
        let origin = ts_import_origin("typescript", src.as_bytes(), "NS").unwrap();
        assert_eq!(origin.raw_module, "./x");
        assert_eq!(origin.alias_of, None);
    }

    #[test]
    fn ts_reexport_plain_and_aliased() {
        let src = r#"export { C, D as E } from "./y";"#;
        let c = ts_import_origin("typescript", src.as_bytes(), "C").unwrap();
        assert_eq!(c.raw_module, "./y");
        assert_eq!(c.alias_of, None);

        let e = ts_import_origin("typescript", src.as_bytes(), "E").unwrap();
        assert_eq!(e.raw_module, "./y");
        assert_eq!(e.alias_of.as_deref(), Some("D"));
    }

    #[test]
    fn ts_bare_package_specifier_is_out_of_scope() {
        let src = r#"import { useState } from "react";"#;
        assert_eq!(
            ts_import_origin("typescript", src.as_bytes(), "useState"),
            None
        );
    }

    #[test]
    fn ts_no_import_at_all_is_none() {
        let src = "const widget = 1;\n";
        assert_eq!(
            ts_import_origin("typescript", src.as_bytes(), "widget"),
            None
        );
    }

    // --- TS/TSX/JS: resolve_module_file ------------------------------------

    #[test]
    fn ts_extension_ladder_resolves_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "src/app.ts", "\n");
        write_file(root, "src/widget.tsx", "export const Widget = 1;\n");

        let hit = resolve_module_file(root, Path::new("src/app.ts"), "typescript", "./widget")
            .expect("should resolve via the .tsx rung of the extension ladder");
        assert_eq!(hit, Path::new("src/widget.tsx"));
    }

    #[test]
    fn ts_index_resolution() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "src/app.ts", "\n");
        write_file(root, "src/lib/index.ts", "export const helper = 1;\n");

        let hit = resolve_module_file(root, Path::new("src/app.ts"), "typescript", "./lib")
            .expect("should resolve via the /index.ts rung");
        assert_eq!(hit, Path::new("src/lib/index.ts"));
    }

    #[test]
    fn ts_parent_relative_specifier_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "src/components/app.tsx", "\n");
        write_file(root, "src/lib/util.ts", "export const helper = 1;\n");

        let hit = resolve_module_file(
            root,
            Path::new("src/components/app.tsx"),
            "tsx",
            "../lib/util",
        )
        .expect("../ specifiers should resolve");
        assert_eq!(hit, Path::new("src/lib/util.ts"));
    }

    #[test]
    fn ts_escape_attempt_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "src/app.ts", "\n");
        // Deliberately more `../` than there are directories under `root` —
        // if this ever resolved, it would point outside the repo entirely.
        let result = resolve_module_file(
            root,
            Path::new("src/app.ts"),
            "typescript",
            "../../../../../../etc/passwd",
        );
        assert_eq!(result, None);
    }

    #[test]
    fn ts_unresolvable_module_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "src/app.ts", "\n");
        assert_eq!(
            resolve_module_file(root, Path::new("src/app.ts"), "typescript", "./nope"),
            None
        );
    }

    // --- Python (B5b): import_origin --------------------------------------

    #[test]
    fn python_plain_import_binds_the_first_segment() {
        let src = "import a.b.c\n";
        let origin = python_import_origin(src.as_bytes(), "a").unwrap();
        assert_eq!(origin.raw_module, "a/b/c");
        assert_eq!(origin.alias_of, None);

        // Middle/final segments are out of scope.
        assert_eq!(python_import_origin(src.as_bytes(), "b"), None);
        assert_eq!(python_import_origin(src.as_bytes(), "c"), None);
    }

    #[test]
    fn python_plain_import_aliased() {
        let src = "import os.path as p\n";
        let origin = python_import_origin(src.as_bytes(), "p").unwrap();
        assert_eq!(origin.raw_module, "os/path");
        assert_eq!(origin.alias_of, None);
    }

    #[test]
    fn python_from_import_plain_and_aliased() {
        let src = "from a.b import c, d as e\n";
        let c = python_import_origin(src.as_bytes(), "c").unwrap();
        assert_eq!(c.raw_module, "a/b/c");
        assert_eq!(c.alias_of, None);

        let e = python_import_origin(src.as_bytes(), "e").unwrap();
        assert_eq!(e.raw_module, "a/b/d");
        assert_eq!(e.alias_of.as_deref(), Some("d"));
    }

    #[test]
    fn python_relative_from_import() {
        let bare = "from . import c\n";
        let c = python_import_origin(bare.as_bytes(), "c").unwrap();
        assert_eq!(c.raw_module, ".//c");

        let pkg = "from .pkg import c\n";
        let c2 = python_import_origin(pkg.as_bytes(), "c").unwrap();
        assert_eq!(c2.raw_module, "./pkg/c");

        let up = "from ..pkg.sub import c\n";
        let c3 = python_import_origin(up.as_bytes(), "c").unwrap();
        assert_eq!(c3.raw_module, "../pkg/sub/c");
    }

    #[test]
    fn python_no_import_at_all_is_none() {
        let src = "def f():\n    widget = 1\n";
        assert_eq!(python_import_origin(src.as_bytes(), "widget"), None);
    }

    #[test]
    fn python_wildcard_import_binds_nothing() {
        let src = "from a.b import *\n";
        assert_eq!(python_import_origin(src.as_bytes(), "anything"), None);
    }

    // --- Python: resolve_module_file ---------------------------------------

    #[test]
    fn python_absolute_import_resolves_against_the_repo_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "app.py", "import pkg.mod\n");
        write_file(root, "pkg/mod.py", "def widget():\n    pass\n");

        let hit = resolve_module_file(root, Path::new("app.py"), "python", "pkg/mod")
            .expect("should resolve pkg/mod.py");
        assert_eq!(hit, Path::new("pkg/mod.py"));
    }

    #[test]
    fn python_from_import_prefers_the_submodule_then_falls_back_to_the_package_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "app.py", "\n");
        write_file(root, "pkg/sub.py", "def helper():\n    pass\n");

        // `from pkg import sub` — `sub` IS its own submodule file.
        let hit = resolve_module_file(root, Path::new("app.py"), "python", "pkg/sub")
            .expect("should resolve pkg/sub.py");
        assert_eq!(hit, Path::new("pkg/sub.py"));
    }

    #[test]
    fn python_from_import_falls_back_to_the_package_own_file_for_a_symbol() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "app.py", "\n");
        write_file(root, "pkg.py", "def widget():\n    pass\n");

        // `from pkg import widget` — `widget` is a SYMBOL inside pkg.py,
        // not its own submodule.
        let hit = resolve_module_file(root, Path::new("app.py"), "python", "pkg/widget")
            .expect("should fall back to pkg.py");
        assert_eq!(hit, Path::new("pkg.py"));
    }

    #[test]
    fn python_relative_import_resolves_relative_to_the_current_package() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "pkg/__init__.py", "\n");
        write_file(root, "pkg/mod.py", "\n");
        write_file(root, "pkg/sub/app.py", "from . import mod\n");
        write_file(root, "pkg/sub/__init__.py", "\n");

        // From `pkg/sub/app.py`, `from . import mod` means "look inside
        // pkg/sub/" first — no `mod.py` there, so it falls back to
        // `pkg/sub/__init__.py` (searching it for a symbol named `mod`).
        let hit = resolve_module_file(root, Path::new("pkg/sub/app.py"), "python", ".//mod")
            .expect("should resolve to pkg/sub/__init__.py");
        assert_eq!(hit, Path::new("pkg/sub/__init__.py"));
    }

    #[test]
    fn python_relative_import_up_a_level() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "pkg/mod.py", "def widget():\n    pass\n");
        write_file(root, "pkg/sub/app.py", "from .. import mod\n");

        let hit = resolve_module_file(root, Path::new("pkg/sub/app.py"), "python", "../mod")
            .expect("`..` should walk up to pkg/");
        assert_eq!(hit, Path::new("pkg/mod.py"));
    }

    #[test]
    fn python_unresolvable_import_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "app.py", "\n");
        assert_eq!(
            resolve_module_file(root, Path::new("app.py"), "python", "requests/Session"),
            None
        );
    }

    // --- Go (B5b): import_origin --------------------------------------------

    #[test]
    fn go_plain_import_binds_the_last_path_segment() {
        let src = "import (\n\t\"net/http\"\n)\n";
        let origin = go_import_origin(src.as_bytes(), "http").unwrap();
        assert_eq!(origin.raw_module, "net/http");
        assert_eq!(origin.alias_of, None);
    }

    #[test]
    fn go_aliased_import_binds_the_alias_not_the_last_segment() {
        let src = "import (\n\tf \"os\"\n)\n";
        let origin = go_import_origin(src.as_bytes(), "f").unwrap();
        assert_eq!(origin.raw_module, "os");
        // The un-aliased last segment is no longer a local binding.
        assert_eq!(go_import_origin(src.as_bytes(), "os"), None);
    }

    #[test]
    fn go_blank_and_dot_imports_bind_nothing_clickable() {
        let src = "import (\n\t_ \"os\"\n\t. \"fmt\"\n)\n";
        assert_eq!(go_import_origin(src.as_bytes(), "_"), None);
        assert_eq!(go_import_origin(src.as_bytes(), "os"), None);
        assert_eq!(go_import_origin(src.as_bytes(), "fmt"), None);
    }

    #[test]
    fn go_no_import_at_all_is_none() {
        let src = "package p\n\nfunc f() {\n\tx := 1\n\t_ = x\n}\n";
        assert_eq!(go_import_origin(src.as_bytes(), "x"), None);
    }

    // --- Go: resolve_module_file ---------------------------------------------

    #[test]
    fn go_intra_repo_import_resolves_via_go_mod() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "go.mod", "module example.com/widget\n\ngo 1.22\n");
        write_file(root, "internal/util/helper.go", "package util\n");

        let hit = resolve_module_file(
            root,
            Path::new("main.go"),
            "go",
            "example.com/widget/internal/util",
        )
        .expect("should resolve via go.mod's module path");
        assert_eq!(hit, Path::new("internal/util/helper.go"));
    }

    #[test]
    fn go_picks_the_alphabetically_first_non_test_file_in_the_package_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "go.mod", "module example.com/widget\n");
        write_file(root, "pkg/z_file.go", "package pkg\n");
        write_file(root, "pkg/a_file.go", "package pkg\n");
        write_file(root, "pkg/a_file_test.go", "package pkg\n");

        let hit = resolve_module_file(root, Path::new("main.go"), "go", "example.com/widget/pkg")
            .expect("should resolve to a file in pkg/");
        assert_eq!(hit, Path::new("pkg/a_file.go"));
    }

    #[test]
    fn go_root_package_import_resolves_too() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "go.mod", "module example.com/widget\n");
        write_file(root, "main.go", "package main\n");

        let hit = resolve_module_file(root, Path::new("other.go"), "go", "example.com/widget")
            .expect("the module's own root package should resolve");
        assert_eq!(hit, Path::new("main.go"));
    }

    #[test]
    fn go_external_import_without_a_go_mod_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        assert_eq!(
            resolve_module_file(root, Path::new("main.go"), "go", "net/http"),
            None
        );
    }

    #[test]
    fn go_external_import_with_a_go_mod_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_file(root, "go.mod", "module example.com/widget\n");
        assert_eq!(
            resolve_module_file(root, Path::new("main.go"), "go", "github.com/other/pkg"),
            None
        );
    }
}
