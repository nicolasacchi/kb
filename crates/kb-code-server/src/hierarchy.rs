//! V3.1-H1 — call/type hierarchy: content-addressed extraction + bearer
//! HTTP APIs (`GET /api/hierarchy/{callees,callers,types}`).
//!
//! # Extraction
//!
//! Call sites and type relations are content-addressed like `import_specs`
//! (keyed `(blob_hash, salt, ordinal)`), replaced wholesale on re-extract.
//! Four proof languages only: rust / typescript / tsx / python.
//!
//! # Trust class
//!
//! Every hierarchy edge carries the class of the **name resolution** that
//! produced its endpoint — never better. Dynamic dispatch / trait-object /
//! duck-typed calls are forced to `candidate` even when a same-name method
//! would otherwise rank `likely`. A wrong `exact` is a release blocker.

use crate::extract::Symbol;
use crate::intel::{arity, cross_file};
use crate::lang;
use crate::resolve::{
    resolve_position, word_at, CLASS_CANDIDATE, CLASS_EXACT, CLASS_LIKELY, MAX_CANDIDATES,
};
use crate::routes::{find_repo, read_repo_file, ApiError};
use crate::state::SharedState;
use crate::store::{Store, StoreBlocking};
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use tree_sitter::Node;

pub const HIERARCHY_SCHEMA: &str = "hierarchy/1";
/// Design perf budget — loud truncate past this many call_sites per file.
pub const CALL_SITES_CAP: usize = 20_000;
/// Cap on caller groups returned by `/hierarchy/callers`.
pub const CALLERS_GROUP_CAP: usize = 200;
const QUALIFIER_CAP: usize = 120;

/// PF-K1 — a request-scoped memo over [`crate::routes::read_repo_file`]'s
/// git-blob reads, keyed by the EXACT `(path, rev)` pair passed to it
/// (`rev=""` stands in for the working-tree `None` case — never a real git
/// rev, so it can't collide). `is_dynamic_call` re-reads the same caller
/// file's bytes once per call site inside `callers_at`'s per-caller-group
/// loop (a fresh `GitRepo::open` + full tree-walk blob read every time,
/// uncached), and `review_impact::compute_impact`'s changed-symbol loop
/// calls `callers_at` up to `review_impact::MAX_CHANGED_SYMBOLS` (20) times
/// per request — [`callers_at_with_cache`] lets every one of those calls
/// share ONE cache. Caches a failed/missing read (`None`) too, so a
/// repeatedly-missing `(path, rev)` pair never re-opens the repo. Pure
/// memoization: the cached value at a given key is byte-identical to what
/// an uncached `read_repo_file(...).ok().map(|r| r.bytes)` would have
/// returned for that same `(path, rev)` — no semantic change.
pub type BlobReadCache = HashMap<(String, String), Option<Vec<u8>>>;

/// Cached wrapper around [`crate::routes::read_repo_file`] — see
/// [`BlobReadCache`]'s own doc.
fn read_repo_file_cached(
    repo: &crate::config::RepoEntry,
    path: &str,
    rev: Option<&str>,
    cache: &mut BlobReadCache,
) -> Option<Vec<u8>> {
    let key = (path.to_string(), rev.unwrap_or("").to_string());
    if let Some(hit) = cache.get(&key) {
        return hit.clone();
    }
    let result = read_repo_file(repo, path, rev).ok().map(|r| r.bytes);
    cache.insert(key, result.clone());
    result
}

/// Languages that extract call/type edges (salt-bumped with this feature).
pub fn supports_hierarchy(lang_id: &str) -> bool {
    matches!(lang_id, "rust" | "typescript" | "tsx" | "python")
}

// ---------------------------------------------------------------------------
// Extracted rows
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSite {
    pub ordinal: u32,
    pub callee_name: String,
    pub callee_qualifier: Option<String>,
    pub line: u32,
    pub col: u32,
    pub arg_count: Option<u32>,
    pub caller_ordinal: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeRelation {
    pub ordinal: u32,
    /// `impl` | `supertrait` | `extends` | `implements` | `bases`
    pub kind: String,
    pub subject: String,
    pub object: String,
    pub line: u32,
}

/// Extract call sites for a proof language. `symbols` is the symbols-pass
/// list for this blob (used for `caller_ordinal`). Empty for unsupported
/// languages.
pub fn extract_call_sites(lang_id: &str, source: &[u8], symbols: &[Symbol]) -> Vec<CallSite> {
    if !supports_hierarchy(lang_id) {
        return Vec::new();
    }
    let Ok((tree, _)) = lang::parse(lang_id, source) else {
        return Vec::new();
    };
    // (name, qualifier, line, col, arg_count)
    type RawCall = (String, Option<String>, u32, u32, Option<u32>);
    let mut raw: Vec<RawCall> = Vec::new();
    walk_calls(lang_id, tree.root_node(), source, &mut raw);
    let mut out = Vec::with_capacity(raw.len().min(CALL_SITES_CAP));
    let mut truncated = false;
    for (i, (name, qual, line, col, argc)) in raw.into_iter().enumerate() {
        if i >= CALL_SITES_CAP {
            truncated = true;
            break;
        }
        let caller_ordinal = innermost_caller(symbols, line, col);
        out.push(CallSite {
            ordinal: i as u32,
            callee_name: name,
            callee_qualifier: qual,
            line,
            col,
            arg_count: argc,
            caller_ordinal,
        });
    }
    if truncated {
        tracing::warn!(
            lang = lang_id,
            cap = CALL_SITES_CAP,
            "call_sites truncated at per-file cap (design perf budget)"
        );
    }
    out
}

/// Extract type relations for a proof language.
pub fn extract_type_relations(lang_id: &str, source: &[u8]) -> Vec<TypeRelation> {
    if !supports_hierarchy(lang_id) {
        return Vec::new();
    }
    let Ok((tree, _)) = lang::parse(lang_id, source) else {
        return Vec::new();
    };
    let mut raw: Vec<(String, String, String, u32)> = Vec::new();
    walk_type_relations(lang_id, tree.root_node(), source, &mut raw);
    raw.into_iter()
        .enumerate()
        .map(|(i, (kind, subject, object, line))| TypeRelation {
            ordinal: i as u32,
            kind,
            subject,
            object,
            line,
        })
        .collect()
}

fn innermost_caller(symbols: &[Symbol], line: u32, col: u32) -> Option<u32> {
    let mut best: Option<(u32, u32)> = None; // (span_len, ordinal)
    for s in symbols {
        if !is_callable_kind(&s.kind) {
            continue;
        }
        if line < s.line_start || line > s.line_end {
            continue;
        }
        // Name line: require col inside the name for the start line only
        // when we're exactly on the name line of a nested symbol — for
        // body containment, any col on an interior line is fine.
        if line == s.line_start && col < s.col_start {
            continue;
        }
        if line == s.line_end && s.line_start != s.line_end && col > s.col_end {
            // col_end is name end, not body end — don't use it for body.
        }
        let span = s.line_end.saturating_sub(s.line_start);
        match best {
            None => best = Some((span, s.ordinal)),
            Some((best_span, _)) if span < best_span => best = Some((span, s.ordinal)),
            Some((best_span, best_ord)) if span == best_span && s.ordinal > best_ord => {
                // Prefer later (inner) ordinals on equal span.
                best = Some((span, s.ordinal));
            }
            _ => {}
        }
    }
    best.map(|(_, o)| o)
}

pub fn is_callable_kind_pub(kind: &str) -> bool {
    is_callable_kind(kind)
}

fn is_callable_kind(kind: &str) -> bool {
    matches!(
        kind,
        "fn" | "method" | "function" | "def" | "func" | "constructor"
    )
}

// --- call walk ---------------------------------------------------------------

/// (callee_name, qualifier, line, col, arg_count)
type RawCallSite = (String, Option<String>, u32, u32, Option<u32>);

fn walk_calls(lang_id: &str, node: Node<'_>, source: &[u8], out: &mut Vec<RawCallSite>) {
    if arity::is_call_kind(lang_id, node.kind()) {
        if let Some(site) = call_site_from_node(lang_id, node, source) {
            out.push(site);
        }
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        walk_calls(lang_id, child, source, out);
    }
}

fn call_site_from_node(lang_id: &str, call: Node<'_>, source: &[u8]) -> Option<RawCallSite> {
    // Skip macro_invocation for hierarchy edges (name is a macro, not a fn).
    if call.kind() == "macro_invocation" {
        return None;
    }
    let (name, qual, name_node) = callee_parts(lang_id, call, source)?;
    if name.is_empty() || !is_ident_like(&name) {
        return None;
    }
    let line = (name_node.start_position().row as u32) + 1;
    let col = name_node.start_position().column as u32;
    let argc = Some(arity::count_args(lang_id, call, source));
    let qual = qual.and_then(|q| {
        let t = q.trim();
        if t.is_empty() {
            None
        } else {
            Some(cap_str(t, QUALIFIER_CAP))
        }
    });
    Some((name, qual, line, col, argc))
}

fn callee_parts<'a>(
    lang_id: &str,
    call: Node<'a>,
    source: &[u8],
) -> Option<(String, Option<String>, Node<'a>)> {
    if call.kind() == "new_expression" {
        // `new Foo(...)` / `new foo.Bar(...)` — type identifier is the callee.
        let mut c = call.walk();
        for child in call.named_children(&mut c) {
            if matches!(
                child.kind(),
                "identifier" | "type_identifier" | "member_expression"
            ) {
                return leaf_name_and_qual(child, source);
            }
        }
        return None;
    }
    let func = call.child_by_field_name("function").or_else(|| {
        // python `call` has function field; fallback first non-args child
        let mut c = call.walk();
        for child in call.named_children(&mut c) {
            if !matches!(child.kind(), "arguments" | "argument_list") {
                return Some(child);
            }
        }
        None
    })?;
    let _ = lang_id;
    leaf_name_and_qual(func, source)
}

fn leaf_name_and_qual<'a>(
    func: Node<'a>,
    source: &[u8],
) -> Option<(String, Option<String>, Node<'a>)> {
    match func.kind() {
        "identifier" | "type_identifier" | "property_identifier" | "field_identifier" => {
            let name = func.utf8_text(source).ok()?.to_string();
            Some((name, None, func))
        }
        "field_expression" | "member_expression" | "attribute" => {
            // receiver.field / obj.method
            let field = func
                .child_by_field_name("field")
                .or_else(|| func.child_by_field_name("property"))
                .or_else(|| func.child_by_field_name("attribute"))
                .or_else(|| {
                    let mut c = func.walk();
                    func.named_children(&mut c).last()
                })?;
            let name = field.utf8_text(source).ok()?.to_string();
            let object = func
                .child_by_field_name("value")
                .or_else(|| func.child_by_field_name("object"))
                .or_else(|| func.named_child(0));
            let qual = object
                .and_then(|o| o.utf8_text(source).ok())
                .map(|s| s.to_string());
            Some((name, qual, field))
        }
        "scoped_identifier" | "scoped_type_identifier" => {
            // foo::bar — name is the rightmost identifier
            let name_node = func.child_by_field_name("name").or_else(|| {
                let mut c = func.walk();
                func.named_children(&mut c).last()
            })?;
            let name = name_node.utf8_text(source).ok()?.to_string();
            let path = func.child_by_field_name("path");
            let qual = path
                .and_then(|p| p.utf8_text(source).ok())
                .map(|s| s.to_string())
                .or_else(|| {
                    // whole text without trailing ::name
                    let full = func.utf8_text(source).ok()?;
                    full.rsplit_once("::").map(|(p, _)| p.to_string())
                });
            Some((name, qual, name_node))
        }
        _ => {
            // Fallback: rightmost identifier leaf.
            if let Some(id) = rightmost_ident(func) {
                let name = id.utf8_text(source).ok()?.to_string();
                let full = func.utf8_text(source).ok().unwrap_or("");
                let qual = full
                    .strip_suffix(&name)
                    .map(|p| p.trim_end_matches(['.', ':']).trim().to_string())
                    .filter(|s| !s.is_empty());
                Some((name, qual, id))
            } else {
                None
            }
        }
    }
}

fn rightmost_ident<'a>(node: Node<'a>) -> Option<Node<'a>> {
    if matches!(
        node.kind(),
        "identifier" | "type_identifier" | "property_identifier" | "field_identifier"
    ) {
        return Some(node);
    }
    let mut c = node.walk();
    let children: Vec<_> = node.named_children(&mut c).collect();
    for child in children.into_iter().rev() {
        if let Some(id) = rightmost_ident(child) {
            return Some(id);
        }
    }
    None
}

fn is_ident_like(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

fn cap_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

// --- type relations walk -----------------------------------------------------

fn walk_type_relations(
    lang_id: &str,
    node: Node<'_>,
    source: &[u8],
    out: &mut Vec<(String, String, String, u32)>,
) {
    match lang_id {
        "rust" => rust_type_rel(node, source, out),
        "typescript" | "tsx" => ts_type_rel(node, source, out),
        "python" => py_type_rel(node, source, out),
        _ => {}
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        walk_type_relations(lang_id, child, source, out);
    }
}

fn rust_type_rel(node: Node<'_>, source: &[u8], out: &mut Vec<(String, String, String, u32)>) {
    match node.kind() {
        "impl_item" => {
            // `impl Trait for Type` — trait field present; bare `impl Type` skipped.
            let Some(trait_node) = node.child_by_field_name("trait") else {
                return;
            };
            let Some(type_node) = node.child_by_field_name("type") else {
                return;
            };
            // Skip cfg-gated / macro-ish: if type text has `!` or `<` nested macros.
            let Ok(tr) = trait_node.utf8_text(source) else {
                return;
            };
            let Ok(ty) = type_node.utf8_text(source) else {
                return;
            };
            let object = simple_type_name(tr);
            let subject = simple_type_name(ty);
            if object.is_empty() || subject.is_empty() {
                return;
            }
            let line = (node.start_position().row as u32) + 1;
            out.push(("impl".into(), subject, object, line));
        }
        "trait_item" => {
            // `trait Sub: Super + Other`
            let Some(name_node) = node.child_by_field_name("name") else {
                return;
            };
            let Ok(subject) = name_node.utf8_text(source) else {
                return;
            };
            let line = (node.start_position().row as u32) + 1;
            // Bounds live in a `trait_bounds` / `where_clause` child, or
            // directly after `:` as type identifiers.
            collect_rust_supertraits(node, source, subject, line, out);
        }
        _ => {}
    }
}

fn collect_rust_supertraits(
    trait_item: Node<'_>,
    source: &[u8],
    subject: &str,
    line: u32,
    out: &mut Vec<(String, String, String, u32)>,
) {
    let mut c = trait_item.walk();
    for child in trait_item.children(&mut c) {
        match child.kind() {
            "trait_bounds" | "where_clause" => {
                for name in collect_type_idents(child, source) {
                    if name != subject && is_ident_like(&name) {
                        out.push(("supertrait".into(), subject.to_string(), name, line));
                    }
                }
            }
            "type_identifier" | "identifier" => {
                // Direct bound after `:` without a trait_bounds wrapper
                // (some grammar versions).
                if let Ok(name) = child.utf8_text(source) {
                    if name != subject && is_ident_like(name) {
                        // Only if it appears after the name field (bound position).
                        if child.start_byte()
                            > trait_item
                                .child_by_field_name("name")
                                .map(|n| n.end_byte())
                                .unwrap_or(0)
                            && trait_item
                                .child_by_field_name("body")
                                .map(|b| child.start_byte() < b.start_byte())
                                .unwrap_or(true)
                        {
                            out.push((
                                "supertrait".into(),
                                subject.to_string(),
                                name.to_string(),
                                line,
                            ));
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn collect_type_idents(node: Node<'_>, source: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    collect_type_idents_into(node, source, &mut out);
    out
}

fn collect_type_idents_into(node: Node<'_>, source: &[u8], out: &mut Vec<String>) {
    if matches!(node.kind(), "type_identifier" | "identifier") {
        if let Ok(t) = node.utf8_text(source) {
            out.push(t.to_string());
        }
        return;
    }
    // Don't descend into generic args deeply for supertrait names — take
    // the head of constrained types only.
    if node.kind() == "generic_type" {
        if let Some(name) = node.child_by_field_name("type") {
            collect_type_idents_into(name, source, out);
        }
        return;
    }
    let mut c = node.walk();
    for child in node.named_children(&mut c) {
        collect_type_idents_into(child, source, out);
    }
}

fn simple_type_name(text: &str) -> String {
    // `crate::mod::Foo<T>` → `Foo`; skip dyn/impl prefixes for subject of
    // impl Trait for Type (type side is usually concrete).
    let t = text.trim();
    let t = t.strip_prefix("dyn ").unwrap_or(t).trim();
    let head = t.split(['<', ' ', '(']).next().unwrap_or(t);
    let name = head.rsplit("::").next().unwrap_or(head).trim();
    name.to_string()
}

fn ts_type_rel(node: Node<'_>, source: &[u8], out: &mut Vec<(String, String, String, u32)>) {
    if !matches!(
        node.kind(),
        "class_declaration" | "abstract_class_declaration" | "class"
    ) {
        // interfaces can extend too
        if node.kind() == "interface_declaration" {
            let Some(name_node) = node.child_by_field_name("name") else {
                return;
            };
            let Ok(subject) = name_node.utf8_text(source) else {
                return;
            };
            let line = (node.start_position().row as u32) + 1;
            if let Some(heritage) = node.child_by_field_name("extends") {
                collect_ts_type_list(heritage, source, &mut |obj| {
                    out.push(("extends".into(), subject.to_string(), obj, line));
                });
            }
            // Some grammars put heritage under a child.
            let mut c = node.walk();
            for child in node.named_children(&mut c) {
                if child.kind() == "extends_type_clause" || child.kind() == "extends_clause" {
                    collect_ts_type_list(child, source, &mut |obj| {
                        out.push(("extends".into(), subject.to_string(), obj, line));
                    });
                }
            }
        }
        return;
    }
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Ok(subject) = name_node.utf8_text(source) else {
        return;
    };
    let line = (node.start_position().row as u32) + 1;
    let mut c = node.walk();
    for child in node.named_children(&mut c) {
        match child.kind() {
            "class_heritage" => {
                let mut hc = child.walk();
                for h in child.named_children(&mut hc) {
                    match h.kind() {
                        "extends_clause" => {
                            collect_ts_type_list(h, source, &mut |obj| {
                                out.push(("extends".into(), subject.to_string(), obj, line));
                            });
                        }
                        "implements_clause" => {
                            collect_ts_type_list(h, source, &mut |obj| {
                                out.push(("implements".into(), subject.to_string(), obj, line));
                            });
                        }
                        _ => {}
                    }
                }
            }
            "extends_clause" => {
                collect_ts_type_list(child, source, &mut |obj| {
                    out.push(("extends".into(), subject.to_string(), obj, line));
                });
            }
            "implements_clause" => {
                collect_ts_type_list(child, source, &mut |obj| {
                    out.push(("implements".into(), subject.to_string(), obj, line));
                });
            }
            _ => {}
        }
    }
}

fn collect_ts_type_list(node: Node<'_>, source: &[u8], f: &mut dyn FnMut(String)) {
    let mut c = node.walk();
    for child in node.named_children(&mut c) {
        match child.kind() {
            "type_identifier" | "identifier" => {
                if let Ok(t) = child.utf8_text(source) {
                    if is_ident_like(t) {
                        f(t.to_string());
                    }
                }
            }
            "generic_type" | "nested_type_identifier" | "member_expression" => {
                let name = simple_type_name(child.utf8_text(source).unwrap_or(""));
                if is_ident_like(&name) {
                    f(name);
                }
            }
            "type_arguments" | "comment" => {}
            _ => {
                // expression types etc. — try simple name
                if let Ok(t) = child.utf8_text(source) {
                    let name = simple_type_name(t);
                    if is_ident_like(&name) {
                        f(name);
                    }
                }
            }
        }
    }
}

fn py_type_rel(node: Node<'_>, source: &[u8], out: &mut Vec<(String, String, String, u32)>) {
    if node.kind() != "class_definition" {
        return;
    }
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Ok(subject) = name_node.utf8_text(source) else {
        return;
    };
    let line = (node.start_position().row as u32) + 1;
    let Some(superclasses) = node.child_by_field_name("superclasses") else {
        return;
    };
    let mut c = superclasses.walk();
    for child in superclasses.named_children(&mut c) {
        // Skip keyword arguments (metaclass=...).
        if child.kind() == "keyword_argument" {
            continue;
        }
        // Computed bases (subscript, call) — skip (conservative).
        if matches!(child.kind(), "call" | "subscript" | "attribute") {
            // Attribute like package.Base is ok if simple.
            if child.kind() == "attribute" {
                if let Ok(t) = child.utf8_text(source) {
                    let name = simple_type_name(t);
                    if name != "object" && is_ident_like(&name) {
                        out.push(("bases".into(), subject.to_string(), name, line));
                    }
                }
            }
            continue;
        }
        if matches!(child.kind(), "identifier" | "type") {
            if let Ok(t) = child.utf8_text(source) {
                let name = simple_type_name(t);
                if name != "object" && is_ident_like(&name) {
                    out.push(("bases".into(), subject.to_string(), name, line));
                }
            }
            continue;
        }
        // argument_list wraps bases in some versions — already walking named.
        if let Ok(t) = child.utf8_text(source) {
            let name = simple_type_name(t);
            if name != "object" && is_ident_like(&name) && !t.contains('(') && !t.contains('[') {
                out.push(("bases".into(), subject.to_string(), name, line));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP API
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct HierarchyPosParams {
    pub repo: String,
    pub path: String,
    pub line: u32,
    pub col: u32,
    #[serde(rename = "ref")]
    pub rev: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct HierarchyTypesParams {
    pub repo: String,
    pub name: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolveTarget {
    pub path: String,
    pub line: u32,
    pub class: &'static str,
    pub precision: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct CalleeSite {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qualifier: Option<String>,
    pub line: u32,
    pub col: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arg_count: Option<u32>,
    pub class: &'static str,
    pub target: Option<ResolveTarget>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CalleesOut {
    pub schema: &'static str,
    pub function: FunctionRef,
    pub callees: Vec<CalleeSite>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionRef {
    pub name: String,
    pub kind: String,
    pub path: String,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct EnclosingRef {
    pub name: String,
    pub kind: String,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallerSite {
    pub line: u32,
    pub col: u32,
    pub class: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallerGroup {
    pub path: String,
    pub enclosing: Option<EnclosingRef>,
    pub sites: Vec<CallerSite>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallersOut {
    pub schema: &'static str,
    pub function: FunctionRef,
    pub callers: Vec<CallerGroup>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TypeEdge {
    pub name: String,
    pub kind: String,
    pub via: TypeVia,
    pub class: &'static str,
    pub target: Option<TypeTarget>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TypeVia {
    pub path: String,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct TypeTarget {
    pub path: String,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct TypesOut {
    pub schema: &'static str,
    pub name: String,
    pub supertypes: Vec<TypeEdge>,
    pub subtypes: Vec<TypeEdge>,
}

/// `GET /api/hierarchy/callees`
pub async fn callees_route(
    State(state): State<SharedState>,
    Query(params): Query<HierarchyPosParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo = repo.clone();
    let path = params.path.clone();
    let line = params.line;
    let col = params.col;
    let rev = params.rev.clone();
    // 2026-08-31 incident (store.rs module doc): coarse-wrap the whole
    // sync callees composition on the blocking pool.
    let state_bg = state.clone();
    let out = state
        .store
        .run_blocking(move |store| {
            callees_at(
                store,
                &state_bg.repos,
                &state_bg.repo_ids,
                &repo,
                repo_id,
                &path,
                line,
                col,
                rev.as_deref(),
            )
        })
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// `GET /api/hierarchy/callers`
pub async fn callers_route(
    State(state): State<SharedState>,
    Query(params): Query<HierarchyPosParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo = repo.clone();
    let path = params.path.clone();
    let line = params.line;
    let col = params.col;
    let rev = params.rev.clone();
    // 2026-08-31 incident (store.rs module doc): coarse-wrap the whole
    // sync callers composition on the blocking pool.
    let out = state
        .store
        .run_blocking(move |store| {
            callers_at(store, &repo, repo_id, &path, line, col, rev.as_deref())
        })
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// `GET /api/hierarchy/types`
pub async fn types_route(
    State(state): State<SharedState>,
    Query(params): Query<HierarchyTypesParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo = repo.clone();
    let name = params.name.clone();
    let path = params.path.clone();
    // 2026-08-31 incident (store.rs module doc): coarse-wrap the whole
    // sync type-hierarchy composition on the blocking pool.
    let out = state
        .store
        .run_blocking(move |store| types_at(store, &repo, repo_id, &name, path.as_deref()))
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

#[allow(clippy::too_many_arguments)]
pub fn callees_at(
    store: &Store,
    repos: &[crate::config::RepoEntry],
    repo_ids: &HashMap<String, i64>,
    repo: &crate::config::RepoEntry,
    repo_id: i64,
    path: &str,
    line: u32,
    col: u32,
    rev: Option<&str>,
) -> Result<CalleesOut, ApiError> {
    let (sym, salt, blob_hash) = require_callable_def(store, repo, repo_id, path, line, col, rev)?;
    let sites = store.call_sites_for_blob(&blob_hash, salt)?;
    let mut callees: Vec<CalleeSite> = Vec::new();
    // PF-K1 — every site in this loop reads the SAME (path, rev) blob for
    // its dynamic-call check; one function-scoped cache turns N tree-walk
    // reads into one.
    let mut blob_cache = BlobReadCache::new();
    for site in sites
        .into_iter()
        .filter(|s| s.caller_ordinal == Some(sym.ordinal))
    {
        let resolved = resolve_position(
            store, repos, repo_ids, repo, repo_id, path, site.line, site.col, rev,
        )
        .ok();
        let dynamic = is_dynamic_call(
            store,
            repo,
            path,
            &site,
            rev,
            salt,
            &blob_hash,
            &mut blob_cache,
        )?;
        let (class, target) = match resolved.and_then(|r| r.candidates.into_iter().next()) {
            Some(c) => {
                let class = if dynamic { CLASS_CANDIDATE } else { c.class };
                (
                    class,
                    Some(ResolveTarget {
                        path: c.path,
                        line: c.line,
                        class,
                        precision: c.precision,
                    }),
                )
            }
            None => (CLASS_CANDIDATE, None),
        };
        callees.push(CalleeSite {
            name: site.callee_name,
            qualifier: site.callee_qualifier,
            line: site.line,
            col: site.col,
            arg_count: site.arg_count,
            class,
            target,
        });
    }
    callees.sort_by(|a, b| {
        class_rank(a.class)
            .cmp(&class_rank(b.class))
            .then_with(|| a.line.cmp(&b.line))
            .then_with(|| a.col.cmp(&b.col))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(CalleesOut {
        schema: HIERARCHY_SCHEMA,
        function: FunctionRef {
            name: sym.name,
            kind: sym.kind,
            path: path.to_string(),
            line: sym.line_start,
        },
        callees,
    })
}

/// [`callers_at_with_cache`] with a fresh, call-scoped [`BlobReadCache`] —
/// every EXISTING caller (the `/hierarchy/callers` route,
/// `impact_analysis`'s BFS) keeps this exact signature and gets the same
/// intra-call caching it always implicitly had (one cache per invocation),
/// just now explicit. `review_impact::compute_impact` is the one caller
/// that shares a cache ACROSS multiple `callers_at` calls in one request —
/// it calls [`callers_at_with_cache`] directly instead (PF-K1).
#[allow(clippy::too_many_arguments)]
pub fn callers_at(
    store: &Store,
    repo: &crate::config::RepoEntry,
    repo_id: i64,
    path: &str,
    line: u32,
    col: u32,
    rev: Option<&str>,
) -> Result<CallersOut, ApiError> {
    let mut cache = BlobReadCache::new();
    callers_at_with_cache(store, repo, repo_id, path, line, col, rev, &mut cache)
}

#[allow(clippy::too_many_arguments)]
pub fn callers_at_with_cache(
    store: &Store,
    repo: &crate::config::RepoEntry,
    repo_id: i64,
    path: &str,
    line: u32,
    col: u32,
    rev: Option<&str>,
    cache: &mut BlobReadCache,
) -> Result<CallersOut, ApiError> {
    let (sym, _salt, _blob) = require_callable_def(store, repo, repo_id, path, line, col, rev)?;
    let def_name = sym.name.clone();
    let def_path = path.to_string();
    let param_min = sym.param_min;
    let param_max = sym.param_max;

    // Repo-wide call sites named like the def.
    let all_sites = store.call_sites_by_callee_in_repo(repo_id, &def_name)?;

    // Group by (path, enclosing ordinal)
    #[derive(Default)]
    struct Acc {
        sites: Vec<(u32, u32, Option<u32>, Option<String>)>, // line,col,argc,blob_salt path already outer
        blob_hash: String,
        salt: String,
    }
    // key: (path, caller_ordinal)
    let mut groups: BTreeMap<(String, Option<u32>), Acc> = BTreeMap::new();
    for (cpath, site, blob_hash, salt) in all_sites {
        let key = (cpath.clone(), site.caller_ordinal);
        let e = groups.entry(key).or_default();
        e.blob_hash = blob_hash;
        e.salt = salt;
        e.sites
            .push((site.line, site.col, site.arg_count, site.callee_qualifier));
    }

    // Import targets from def file (for reverse reach) + per-caller file.
    let def_fid = store.file_id(repo_id, &def_path)?;
    let def_import_targets: HashSet<String> = match def_fid {
        Some(fid) => store.import_target_paths(fid)?.into_iter().collect(),
        None => HashSet::new(),
    };

    let mut callers: Vec<CallerGroup> = Vec::new();
    for ((cpath, caller_ord), acc) in groups {
        // Classify reach of calling file ↔ def file.
        let caller_fid = store.file_id(repo_id, &cpath)?;
        let mut caller_targets = HashSet::new();
        if let Some(fid) = caller_fid {
            if let Ok(ps) = store.import_target_paths(fid) {
                caller_targets.extend(ps);
            }
        }
        let reach = cross_file::classify_reach(&cpath, &def_path, &caller_targets);
        let reverse = cross_file::classify_reach(&def_path, &cpath, &def_import_targets);
        let base_likely = matches!(
            reach,
            cross_file::Reach::SameFile
                | cross_file::Reach::SameDir
                | cross_file::Reach::ImportReachable
        ) || matches!(
            reverse,
            cross_file::Reach::SameDir | cross_file::Reach::ImportReachable
        );

        let mut sites_out: Vec<CallerSite> = Vec::new();
        for (line, col, argc, qual) in acc.sites {
            let mut class = if base_likely {
                CLASS_LIKELY
            } else {
                CLASS_CANDIDATE
            };
            // Arity demotion.
            if class == CLASS_LIKELY {
                if let Some(n) = argc {
                    if arity::arity_rejects(n, param_min, param_max) {
                        class = CLASS_CANDIDATE;
                    }
                }
            }
            // Dynamic / duck-typed demotion.
            let fake_site = CallSite {
                ordinal: 0,
                callee_name: def_name.clone(),
                callee_qualifier: qual,
                line,
                col,
                arg_count: argc,
                caller_ordinal: caller_ord,
            };
            if is_dynamic_call(
                store,
                repo,
                &cpath,
                &fake_site,
                None,
                &acc.salt,
                &acc.blob_hash,
                cache,
            )? {
                class = CLASS_CANDIDATE;
            }
            sites_out.push(CallerSite { line, col, class });
        }
        sites_out.sort_by(|a, b| {
            class_rank(a.class)
                .cmp(&class_rank(b.class))
                .then_with(|| a.line.cmp(&b.line))
                .then_with(|| a.col.cmp(&b.col))
        });

        let enclosing = match caller_ord {
            Some(ord) => {
                let syms = store.symbols_for_blob(&acc.blob_hash, &acc.salt)?;
                syms.into_iter()
                    .find(|s| s.ordinal == ord)
                    .map(|s| EnclosingRef {
                        name: s.name,
                        kind: s.kind,
                        line: s.line_start,
                    })
            }
            None => None,
        };
        callers.push(CallerGroup {
            path: cpath,
            enclosing,
            sites: sites_out,
        });
    }

    // Deterministic order: best class in group, then path.
    callers.sort_by(|a, b| {
        let ac = a
            .sites
            .iter()
            .map(|s| class_rank(s.class))
            .min()
            .unwrap_or(99);
        let bc = b
            .sites
            .iter()
            .map(|s| class_rank(s.class))
            .min()
            .unwrap_or(99);
        ac.cmp(&bc).then_with(|| a.path.cmp(&b.path)).then_with(|| {
            a.enclosing
                .as_ref()
                .map(|e| e.line)
                .cmp(&b.enclosing.as_ref().map(|e| e.line))
        })
    });

    let truncated = callers.len() > CALLERS_GROUP_CAP;
    callers.truncate(CALLERS_GROUP_CAP);

    Ok(CallersOut {
        schema: HIERARCHY_SCHEMA,
        function: FunctionRef {
            name: def_name,
            kind: sym.kind,
            path: def_path,
            line: sym.line_start,
        },
        callers,
        truncated,
    })
}

pub fn types_at(
    store: &Store,
    _repo: &crate::config::RepoEntry,
    repo_id: i64,
    name: &str,
    disambiguate_path: Option<&str>,
) -> Result<TypesOut, ApiError> {
    if name.is_empty() {
        return Err(ApiError::bad_request("name must be non-empty"));
    }
    let rels = store.type_relations_for_name_in_repo(repo_id, name)?;
    // Optional path disambiguation: keep rows whose via path matches.
    let rels: Vec<_> = if let Some(p) = disambiguate_path {
        rels.into_iter().filter(|(path, _)| path == p).collect()
    } else {
        rels
    };

    // Import targets for class of endpoints — use first subject-defining
    // path if available, else empty.
    let anchor_path = disambiguate_path
        .map(|s| s.to_string())
        .or_else(|| {
            // Prefer a path where `name` is a symbol definition.
            store
                .symbols_named_in_repo(repo_id, name)
                .ok()
                .and_then(|v| v.into_iter().next().map(|(p, _)| p))
        })
        .unwrap_or_default();

    let mut supertypes = Vec::new();
    let mut subtypes = Vec::new();

    for (via_path, rel) in rels {
        // subject == name → edge goes to object (supertype / implemented)
        // object == name → edge comes from subject (subtype / implementor)
        if rel.subject == name {
            let (class, target) =
                resolve_type_endpoint(store, repo_id, &via_path, &rel.object, &anchor_path)?;
            supertypes.push(TypeEdge {
                name: rel.object,
                kind: rel.kind,
                via: TypeVia {
                    path: via_path,
                    line: rel.line,
                },
                class,
                target,
            });
        } else if rel.object == name {
            let (class, target) =
                resolve_type_endpoint(store, repo_id, &via_path, &rel.subject, &anchor_path)?;
            subtypes.push(TypeEdge {
                name: rel.subject,
                kind: rel.kind,
                via: TypeVia {
                    path: via_path,
                    line: rel.line,
                },
                class,
                target,
            });
        }
    }

    let sort_edges = |v: &mut Vec<TypeEdge>| {
        v.sort_by(|a, b| {
            class_rank(a.class)
                .cmp(&class_rank(b.class))
                .then_with(|| a.via.path.cmp(&b.via.path))
                .then_with(|| a.via.line.cmp(&b.via.line))
                .then_with(|| a.name.cmp(&b.name))
        });
    };
    sort_edges(&mut supertypes);
    sort_edges(&mut subtypes);

    Ok(TypesOut {
        schema: HIERARCHY_SCHEMA,
        name: name.to_string(),
        supertypes,
        subtypes,
    })
}

fn resolve_type_endpoint(
    store: &Store,
    repo_id: i64,
    via_path: &str,
    type_name: &str,
    _anchor_path: &str,
) -> Result<(&'static str, Option<TypeTarget>), ApiError> {
    let defs = store.symbols_named_in_repo(repo_id, type_name)?;
    if defs.is_empty() {
        return Ok((CLASS_CANDIDATE, None));
    }
    let paths: Vec<String> = defs.iter().map(|(p, _)| p.clone()).collect();
    let via_fid = store.file_id(repo_id, via_path)?;
    let import_targets: HashSet<String> = match via_fid {
        Some(fid) => store.import_target_paths(fid)?.into_iter().collect(),
        None => HashSet::new(),
    };
    let (tagged, _fuzzy) = cross_file::filter_and_tag(via_path, &paths, &import_targets);
    // Pick best tagged.
    if let Some((idx, _reach, class, _prec)) = tagged.first() {
        let (p, s) = &defs[*idx];
        Ok((
            *class,
            Some(TypeTarget {
                path: p.clone(),
                line: s.line_start,
            }),
        ))
    } else {
        let (p, s) = &defs[0];
        Ok((
            CLASS_CANDIDATE,
            Some(TypeTarget {
                path: p.clone(),
                line: s.line_start,
            }),
        ))
    }
}

fn require_callable_def(
    store: &Store,
    repo: &crate::config::RepoEntry,
    repo_id: i64,
    path: &str,
    line: u32,
    col: u32,
    rev: Option<&str>,
) -> Result<(Symbol, &'static str, String), ApiError> {
    if line < 1 {
        return Err(ApiError::bad_request("line must be >= 1 (1-based)"));
    }
    let read = read_repo_file(repo, path, rev)?;
    let lang_info = lang::detect(path, Some(&read.bytes)).ok_or_else(|| {
        ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("{path}: unsupported language for hierarchy"),
        )
    })?;
    if !supports_hierarchy(lang_info.id) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "{path}: hierarchy edges only for rust/typescript/tsx/python (got {})",
                lang_info.id
            ),
        ));
    }
    let symbols = store.symbols_for_blob(&read.blob_hash, lang_info.salt)?;
    // Prefer a callable whose name covers (line, col).
    let on_name = symbols.iter().find(|s| {
        is_callable_kind(&s.kind)
            && s.line_start == line
            && col >= s.col_start
            && col < s.col_end.max(s.col_start + 1)
    });
    let sym = if let Some(s) = on_name {
        s.clone()
    } else {
        // Fallback: any callable whose body range contains the position
        // and whose name line is this line (cursor on `fn` keyword etc.).
        let content = std::str::from_utf8(&read.bytes).unwrap_or("");
        let lines: Vec<&str> = content.split('\n').collect();
        let line_text = lines
            .get((line - 1) as usize)
            .map(|l| l.strip_suffix('\r').unwrap_or(l))
            .unwrap_or("");
        let word = word_at(line_text.as_bytes(), col as usize);
        symbols
            .iter()
            .find(|s| {
                is_callable_kind(&s.kind)
                    && s.line_start == line
                    && word.as_ref().map(|w| w == &s.name).unwrap_or(false)
            })
            .cloned()
            .or_else(|| {
                // Last resort: symbol name equals word anywhere with matching line_start.
                word.and_then(|w| {
                    symbols
                        .iter()
                        .find(|s| is_callable_kind(&s.kind) && s.name == w && s.line_start == line)
                        .cloned()
                })
            })
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!(
                        "position {path}:{line}:{col} is not on a function/method definition — \
                         place the cursor on a fn/method/def name"
                    ),
                )
            })?
    };
    let _ = repo_id;
    Ok((sym, lang_info.salt, read.blob_hash))
}

/// Force candidate for dynamic dispatch / trait-object / duck-typed calls.
#[allow(clippy::too_many_arguments)]
fn is_dynamic_call(
    store: &Store,
    repo: &crate::config::RepoEntry,
    path: &str,
    site: &CallSite,
    rev: Option<&str>,
    salt: &str,
    blob_hash: &str,
    cache: &mut BlobReadCache,
) -> Result<bool, ApiError> {
    let Some(qual) = site.callee_qualifier.as_deref() else {
        // Bare function call — not dynamic method dispatch.
        return Ok(false);
    };
    let lang_id = lang::detect(path, None).map(|l| l.id).unwrap_or("");
    // Trait object: qualifier type or text contains `dyn `.
    if qual.contains("dyn ") || qual.contains("dyn\n") {
        return Ok(true);
    }
    // Read source for local type hints on the receiver identifier — PF-K1:
    // memoized by (path, rev) in `cache`, shared across every call site in
    // `callers_at`'s caller-group loop (see `BlobReadCache`'s own doc).
    let bytes = read_repo_file_cached(repo, path, rev, cache).unwrap_or_default();
    let receiver = qual
        .split(['.', ':'])
        .next()
        .unwrap_or(qual)
        .trim()
        .trim_start_matches('&')
        .trim()
        .to_string();
    if receiver == "self" || receiver == "Self" || receiver == "super" || receiver == "crate" {
        // self.method on a concrete impl is fine (likely). Trait-object self
        // is rare in fixtures; leave as non-dynamic unless type says dyn.
        return Ok(false);
    }
    // Path-style qualifiers (module::fn) are not method dispatch.
    if qual.contains("::") && !qual.contains('.') {
        return Ok(false);
    }
    // Look at locals / line text for type of receiver.
    if let Ok(occs) = store.occurrences_for_blob(blob_hash, salt) {
        // Find a def for receiver in this blob and inspect its line for dyn/interface.
        if let Some(def) = occs
            .iter()
            .find(|o| o.role == "def" && o.name == receiver && o.line <= site.line)
        {
            if let Ok(content) = std::str::from_utf8(&bytes) {
                if let Some(line_text) = content.lines().nth((def.line.saturating_sub(1)) as usize)
                {
                    if line_text.contains("dyn ") {
                        return Ok(true);
                    }
                    // TS interface-typed: `const x: Drawable = ...` where Drawable
                    // is an interface — we don't have kind here cheaply; treat
                    // `as Drawable` trait-ish uppercase after colon as candidate
                    // only when the call is method-style AND no class constructor
                    // assign is present. Conservative: if annotation present and
                    // no `new ` on the line, demote for ts.
                    if matches!(lang_id, "typescript" | "tsx")
                        && line_text.contains(':')
                        && !line_text.contains("new ")
                    {
                        // Could be interface-typed — demote method calls.
                        return Ok(true);
                    }
                }
            }
        }
    }
    // Python: method call on a plain identifier without proven class type → duck-typed.
    if lang_id == "python" && is_ident_like(receiver.trim()) {
        // If assignment line shows ClassName( → concrete, else duck.
        if let Ok(content) = std::str::from_utf8(&bytes) {
            let mut proven = false;
            for (i, line_text) in content.lines().enumerate() {
                let ln = (i + 1) as u32;
                if ln > site.line {
                    break;
                }
                // `x = Foo(` or `x: Foo =`
                if line_text.contains(&receiver)
                    && (line_text.contains(" = ") || line_text.contains(":"))
                {
                    if let Some(hint) = arity::type_name_from_def_line("python", line_text) {
                        if hint
                            .chars()
                            .next()
                            .map(|c| c.is_uppercase())
                            .unwrap_or(false)
                        {
                            proven = true;
                            break;
                        }
                    }
                }
            }
            if !proven {
                return Ok(true);
            }
        } else {
            return Ok(true);
        }
    }
    let _ = MAX_CANDIDATES;
    let _ = CLASS_EXACT;
    Ok(false)
}

fn class_rank(class: &str) -> u8 {
    match class {
        CLASS_EXACT => 0,
        CLASS_LIKELY => 1,
        CLASS_CANDIDATE => 2,
        _ => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_extracts_call_and_impl() {
        let src = br#"
trait Draw { fn draw(&self); }
struct Circle;
impl Draw for Circle {
    fn draw(&self) { helper(); }
}
fn helper() {}
fn run(c: &Circle) {
    c.draw();
    helper();
}
"#;
        let symbols = crate::extract::extract_symbols("rust", src).unwrap();
        let sites = extract_call_sites("rust", src, &symbols);
        assert!(
            sites.iter().any(|s| s.callee_name == "draw"),
            "sites={sites:?}"
        );
        assert!(sites.iter().any(|s| s.callee_name == "helper"));
        let rels = extract_type_relations("rust", src);
        assert!(
            rels.iter()
                .any(|r| r.kind == "impl" && r.subject == "Circle" && r.object == "Draw"),
            "rels={rels:?}"
        );
    }

    #[test]
    fn ts_extracts_extends_implements() {
        let src = br#"
interface Drawable { draw(): void; }
class Shape { area(): number { return 0; } }
class Circle extends Shape implements Drawable {
  draw(): void { this.area(); }
}
"#;
        let symbols = crate::extract::extract_symbols("typescript", src).unwrap();
        let sites = extract_call_sites("typescript", src, &symbols);
        assert!(sites.iter().any(|s| s.callee_name == "area"));
        let rels = extract_type_relations("typescript", src);
        assert!(
            rels.iter()
                .any(|r| r.kind == "extends" && r.subject == "Circle" && r.object == "Shape"),
            "rels={rels:?}"
        );
        assert!(
            rels.iter()
                .any(|r| r.kind == "implements" && r.subject == "Circle" && r.object == "Drawable"),
            "rels={rels:?}"
        );
    }

    #[test]
    fn python_extracts_bases_and_calls() {
        let src = b"
class Base: pass
class Mid(Base): pass
class Leaf(Mid):
    def run(self):
        helper()
        self.duck()
def helper(): pass
";
        let symbols = crate::extract::extract_symbols("python", src).unwrap();
        let sites = extract_call_sites("python", src, &symbols);
        assert!(
            sites.iter().any(|s| s.callee_name == "helper"),
            "sites={sites:?}"
        );
        let rels = extract_type_relations("python", src);
        assert!(
            rels.iter()
                .any(|r| r.kind == "bases" && r.subject == "Mid" && r.object == "Base"),
            "rels={rels:?}"
        );
        assert!(
            rels.iter()
                .any(|r| r.kind == "bases" && r.subject == "Leaf" && r.object == "Mid"),
            "rels={rels:?}"
        );
    }
}
