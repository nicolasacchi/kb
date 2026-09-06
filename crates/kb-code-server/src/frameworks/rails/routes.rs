//! PRR-N3 — `config/routes.rb` / `config/routes/*.rb` DSL → `controller#action`
//! edges (`EdgeKind::RouteAction`), plus the real `draw(:name)` file-split
//! convention (`EdgeKind::RouteFile`).
//!
//! # The `draw()` convention (verified against the real acme-shop repo)
//!
//! `config/routes.rb` in the real target repo is ONLY:
//! ```ruby
//! AcmeShop::Application.routes.draw do
//!   draw(:admin)
//!   draw(:api)
//!   # … 10 more
//! end
//! ```
//! Each `draw(:name)` — bare, no receiver, no block — pulls in
//! `config/routes/<name>.rb`, whose CONTENTS are the actual route DSL at
//! the top level (no `X.routes.draw do` wrapper of their own). [`extract`]
//! handles both shapes: it looks for the `X.routes.draw do … end` wrapper
//! call (a `call` node named `draw`, WITH a receiver and a block) and, if
//! found, walks that block's body; otherwise it walks the whole program's
//! top-level statements directly (the `config/routes/*.rb` shape). A bare
//! `draw(:name)` (no receiver, no block) found while walking either shape
//! emits one `route_file` edge — routes.rb legitimately contains these at
//! its top level; a split file re-`draw`-ing yet another split file would
//! also be picked up the same way, though the real repo is only ever one
//! level deep.
//!
//! # DSL coverage (honest ~80%, per design-nav.md §8 open question 4)
//!
//! Handles: `resources`/`resource` (incl. `only:`/`except:` as a literal
//! `[...]`/`%i[...]` array or a single symbol, `controller:`), `member`/
//! `collection` blocks (bare `get :action` / `on: :member` inline form),
//! nested resources, `namespace` (incl. `module:` override), `scope`
//! (`module:`/`controller:`), a standalone `controller :x do … end` block,
//! `get`/`post`/`patch`/`put`/`delete` in both `to: 'ctrl#action'` string
//! form (incl. the leading-`/`-escapes-the-current-namespace convention —
//! seen in the real repo's `root to: '/backoffice/home#index'`) and bare
//! symbol/string form (action name inferred from the enclosing
//! `member`/`collection`/`controller`-block controller, or — when NONE of
//! those set a default controller — the current `namespace`/`scope module:`
//! chain itself, matching real Rails' own convention: `namespace :ops do
//! get :test end` resolves to `Ops#test`; verified this is real and used in
//! the target repo — `app/controllers/api/internal/ops_controller.rb`
//! exists for exactly this shape), `root` (both `to:` and bare-string
//! forms), and `constraints`/`authenticate`/`shallow` (transparent descend
//! — no edge of their own, block processed with the SAME context; the real
//! repo's one `authenticate` block only wraps `mount` calls, which are
//! dropped anyway, so nothing is lost by not treating it as a hard drop).
//!
//! Deliberately DROPPED (no edge, and — for `direct`/`mount`/`devise_*` —
//! no descent into the block either, since Devise's own route helpers
//! generate routes this DSL-level walk can't safely reconstruct): `direct`,
//! `mount`, `devise_for`, `devise_scope`, `devise_group`, `use_doorkeeper`,
//! `resolve`. A verb call whose action/controller can't be resolved from a
//! LITERAL (e.g. a bare `get 'some/:param/path'` with no `to:`/`action:` at
//! all, or a top-level bare verb with no enclosing namespace/resource/
//! controller-block context) is silently dropped — never fabricated.
//!
//! # The route ADDRESS (`extra_json`, V72-I1)
//!
//! Every `route_action` edge carries `{"verb": "GET", "path":
//! "/trade/rounds/:id"}` in `extra_json` — the HTTP method and the URL
//! pattern, reconstructed from the same DSL walk that resolves the
//! controller. Two independent axes are tracked through the walk:
//! [`RouteCtx::module_prefix`] (where the controller lives on disk) and
//! [`RouteCtx::path_prefix`] (what URL answers), because `namespace` moves
//! both, `scope module:` moves only the first and `scope path:`/a bare
//! positional only the second. `resources` contributes its own name (or an
//! explicit `path:`), a `member` block contributes `:id`, a nested resource
//! contributes the parent's `:<singular>_id`, and a leading `/` on a verb
//! call's pattern escapes the enclosing scope exactly as it already does
//! for a `to:` controller.
//!
//! This is ADDITIVE CONTENT, not a grammar bump: no `kind` is added and no
//! `kind`'s meaning changes, so `RAILS_LENS_GRAMMAR_VERSION` stays
//! `rails-lens/1` (see `frameworks/mod.rs`'s grammar section). A route
//! whose URL this walk cannot reconstruct — a non-literal pattern — carries
//! NO `extra_json` at all rather than a guessed one, and an edge written by
//! a pre-V72-I1 binary likewise has none until its source file is
//! re-extracted; both read as "unknown", never as "/".
//!
//! `update` answers PATCH *and* PUT in real Rails; the lens records PATCH
//! (Rails' own primary form since 4.0) rather than doubling every edge.
//!
//! # Trust
//!
//! Every emitted `route_action` edge is `Trust::Likely` except when an
//! `only:`/`except:` filter's argument isn't a literal array/symbol (a
//! local variable, a method call) — the DSL then falls back to the full
//! default CRUD action set at `Trust::Candidate`, since the true filtered
//! set is genuinely unknown. `route_file` edges are always `Likely` (a
//! `draw(:name)` argument is always a literal symbol in practice; a
//! non-literal one is dropped like any other unresolvable case).

use crate::frameworks::{EdgeKind, FrameworkEdge, Trust};
use std::path::Path;
use tree_sitter::Node;

/// Default CRUD action set for `resources` (plural — includes `index`).
const RESOURCES_ACTIONS: &[&str] = &[
    "index", "show", "new", "create", "edit", "update", "destroy",
];
/// Default action set for `resource` (singular — no `index`, there's only
/// ever one).
const RESOURCE_ACTIONS: &[&str] = &["show", "new", "create", "edit", "update", "destroy"];

#[derive(Debug, Clone, Default)]
struct RouteCtx {
    /// Controller MODULE path segments, built from `namespace`/
    /// `scope module:` — joined with `/` to form the controller's on-disk
    /// directory prefix (e.g. `["trade"]` → `trade/rounds_controller.rb`).
    module_prefix: Vec<String>,
    /// The controller a bare verb-call symbol/string resolves against —
    /// set by `resources`/`resource` (the resource's own controller),
    /// `controller :x do … end`, or `scope controller:`. `None` outside any
    /// of those (a bare verb call then falls back to `module_prefix`
    /// itself — the `namespace :ops do get :test end` convention).
    default_controller: Option<String>,
    /// URL path segments committed so far (no leading slash). A SEPARATE
    /// axis from `module_prefix`: `namespace` moves both, `scope module:`
    /// moves only the module, `scope path:` only the URL.
    path_prefix: Vec<String>,
    /// The dynamic segment Rails inserts for a route declared directly
    /// inside a `resources` block with no `on:` — `:order_id` for
    /// `resources :orders`. `member` replaces it with `member_param`,
    /// `collection` clears it. `None` outside any resource block.
    child_param: Option<String>,
    /// The member segment of the enclosing resource — `:id` for a plural
    /// `resources`, `None` for a singular `resource` (which has no member
    /// id). What a `member do … end` block uses as its `child_param`.
    member_param: Option<String>,
}

/// The URL a `route_action` edge answers, reconstructed from the DSL walk:
/// the HTTP method plus the path pattern with Rails' own dynamic segments
/// (`:id`, `:order_id`). CONVENTION-derived like every other fact this lens
/// produces — it rides `extra_json` and inherits the edge's own
/// likely/candidate cap unchanged (D7's "routes gain verb + path as
/// additive content", explicitly NOT a `rails-lens/2` bump: no `kind` is
/// added and no `kind`'s meaning changes).
#[derive(Debug, Clone)]
struct RouteAddr {
    verb: &'static str,
    url: String,
}

impl RouteAddr {
    fn new(verb: &'static str, segments: &[String]) -> Self {
        RouteAddr {
            verb,
            url: url_from(segments),
        }
    }
}

impl RouteCtx {
    /// The URL base a child route inherits: the committed prefix plus the
    /// enclosing resource's dynamic segment, if there is one.
    fn child_base(&self) -> Vec<String> {
        let mut segs = self.path_prefix.clone();
        if let Some(p) = &self.child_param {
            segs.push(p.clone());
        }
        segs
    }

    /// `member do … end` — every route inside answers under the member
    /// segment (`/orders/:id/publish`).
    fn member_scope(&self) -> RouteCtx {
        RouteCtx {
            child_param: self.member_param.clone(),
            ..self.clone()
        }
    }

    /// `collection do … end` — every route inside answers on the
    /// collection itself (`/orders/search`), with no member segment.
    fn collection_scope(&self) -> RouteCtx {
        RouteCtx {
            child_param: None,
            ..self.clone()
        }
    }
}

/// `["trade", "rounds", ":id"]` → `/trade/rounds/:id`; the empty prefix is
/// the application root, `/`.
fn url_from(segments: &[String]) -> String {
    let joined = segments
        .iter()
        .map(|s| s.trim_matches('/'))
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    if joined.is_empty() {
        "/".to_string()
    } else {
        format!("/{joined}")
    }
}

/// `{"verb":"GET","path":"/trade/rounds/:id"}` — the value is JSON-escaped
/// (a route path is authored text and may legitimately contain a quote).
fn route_extra_json(addr: &RouteAddr) -> String {
    format!(
        r#"{{"verb":"{}","path":{}}}"#,
        addr.verb,
        serde_json::Value::String(addr.url.clone())
    )
}

/// The HTTP verb a `resources`/`resource` action answers, and the segments
/// appended to the resource's own base path. `update` answers PATCH *and*
/// PUT in real Rails; the lens records PATCH (Rails' own primary since 4.0)
/// rather than doubling every edge.
fn resource_action_addr(action: &str, base: &[String], plural: bool) -> Option<RouteAddr> {
    let member: &[&str] = if plural { &[":id"] } else { &[] };
    let (verb, tail): (&'static str, Vec<&str>) = match action {
        "index" => ("GET", vec![]),
        "create" => ("POST", vec![]),
        "new" => ("GET", vec!["new"]),
        "edit" => ("GET", [member, &["edit"][..]].concat()),
        "show" => ("GET", member.to_vec()),
        "update" => ("PATCH", member.to_vec()),
        "destroy" => ("DELETE", member.to_vec()),
        // A `member`/`collection` action reaches this table only through
        // `handle_verb`, which builds its own address; anything else is a
        // set this function does not model — say so by returning None
        // rather than inventing a verb.
        _ => return None,
    };
    let mut segs = base.to_vec();
    segs.extend(tail.into_iter().map(|s| s.to_string()));
    Some(RouteAddr::new(verb, &segs))
}

/// Entry point — see the module doc. `path` is the source-relative path
/// (used verbatim as `FrameworkEdge::src_path`).
pub fn extract(path: &str, bytes: &[u8]) -> Vec<FrameworkEdge> {
    let Ok((tree, _language)) = crate::lang::parse("ruby", bytes) else {
        return Vec::new();
    };
    let root = tree.root_node();
    let mut out = Vec::new();
    let ctx = RouteCtx::default();
    match find_draw_wrapper_body(root, bytes) {
        Some(body) => walk_body(body, bytes, &ctx, path, &mut out),
        None => walk_body(root, bytes, &ctx, path, &mut out),
    }
    out
}

/// Find the `SomeApp.routes.draw do … end` wrapper at the top level and
/// return its block body, if present (the root `config/routes.rb` shape).
fn find_draw_wrapper_body<'a>(root: Node<'a>, source: &[u8]) -> Option<Node<'a>> {
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        if child.kind() != "call" {
            continue;
        }
        if call_method_name(child, source).as_deref() != Some("draw") {
            continue;
        }
        // The wrapper has a receiver (`X.routes.draw`); a bare `draw(:name)`
        // file-include call does not.
        if child.child_by_field_name("receiver").is_none() {
            continue;
        }
        if let Some(body) = call_block_body(child) {
            return Some(body);
        }
    }
    None
}

/// Walk every statement in a `program`/`body_statement`/`block_body` node.
fn walk_body(node: Node, source: &[u8], ctx: &RouteCtx, path: &str, out: &mut Vec<FrameworkEdge>) {
    let mut cursor = node.walk();
    for stmt in node.named_children(&mut cursor) {
        walk_stmt(stmt, source, ctx, path, out);
    }
}

fn walk_stmt(node: Node, source: &[u8], ctx: &RouteCtx, path: &str, out: &mut Vec<FrameworkEdge>) {
    if node.kind() != "call" {
        // Comments, conditionals, plain expressions — not part of the
        // honest ~80% coverage (see module doc). Never recursed into: we
        // don't know what context they might change.
        return;
    }
    let Some(method) = call_method_name(node, source) else {
        return;
    };
    match method.as_str() {
        "resources" => handle_resources(node, source, ctx, path, out, true),
        "resource" => handle_resources(node, source, ctx, path, out, false),
        "member" => descend_with(node, source, ctx.member_scope(), path, out),
        "collection" => descend_with(node, source, ctx.collection_scope(), path, out),
        "namespace" => handle_namespace(node, source, ctx, path, out),
        "scope" => handle_scope(node, source, ctx, path, out),
        "controller" => handle_controller_block(node, source, ctx, path, out),
        // Transparent descend: no edge of their own, same context for the
        // block — see the module doc's DSL-coverage section.
        "constraints" | "authenticate" | "shallow" => {
            descend_with(node, source, ctx.clone(), path, out)
        }
        "root" => handle_root(node, source, ctx, path, out),
        "get" => handle_verb(node, source, ctx, path, out, "GET"),
        "post" => handle_verb(node, source, ctx, path, out, "POST"),
        "put" => handle_verb(node, source, ctx, path, out, "PUT"),
        "patch" => handle_verb(node, source, ctx, path, out, "PATCH"),
        "delete" => handle_verb(node, source, ctx, path, out, "DELETE"),
        "draw" => handle_draw(node, source, path, out),
        // Honest drop list — see the module doc. Deliberately do NOT
        // descend into their blocks (Devise/mount-style route generation
        // isn't reconstructable from this DSL walk).
        "direct" | "mount" | "devise_for" | "devise_scope" | "devise_group" | "resolve"
        | "use_doorkeeper" => {}
        _ => {
            // Unrecognized call: never descend (see module doc — avoid
            // producing edges under a scope whose semantics aren't
            // understood).
        }
    }
}

fn descend_with(
    node: Node,
    source: &[u8],
    ctx: RouteCtx,
    path: &str,
    out: &mut Vec<FrameworkEdge>,
) {
    if let Some(body) = call_block_body(node) {
        walk_body(body, source, &ctx, path, out);
    }
}

// --- resources / resource ---------------------------------------------------

fn handle_resources(
    node: Node,
    source: &[u8],
    ctx: &RouteCtx,
    path: &str,
    out: &mut Vec<FrameworkEdge>,
    plural: bool,
) {
    let args = call_args(node);
    let mut names = Vec::new();
    let mut positional_done = false;
    let mut opts: Vec<Node> = Vec::new();
    for a in &args {
        if !positional_done {
            if let Some(name) = literal_string_or_symbol(*a, source) {
                names.push(name);
                continue;
            }
            positional_done = true;
        }
        collect_pairs_into(*a, &mut opts);
    }
    if names.is_empty() {
        return;
    }

    let controller_opt = find_pair_literal(&opts, "controller", source);
    let path_opt = find_pair_literal(&opts, "path", source);
    let only = find_pair_symbol_list(&opts, "only", source);
    let except = find_pair_symbol_list(&opts, "except", source);
    let base_actions: &[&str] = if plural {
        RESOURCES_ACTIONS
    } else {
        RESOURCE_ACTIONS
    };
    let (actions, trust) = resolve_action_set(base_actions, only.as_ref(), except.as_ref());
    let line = src_line(node);
    let outer_base = ctx.child_base();

    for name in &names {
        let controller_leaf = controller_opt.clone().unwrap_or_else(|| {
            if plural {
                name.clone()
            } else {
                pluralize(name)
            }
        });
        let full_controller = join_path(&ctx.module_prefix, &controller_leaf);
        // The URL segment is the resource's OWN name (or an explicit
        // `path:`), never the controller override — Rails routes on the
        // resource name and dispatches to the controller.
        let segment = path_opt.clone().unwrap_or_else(|| name.clone());
        let mut resource_base = outer_base.clone();
        resource_base.push(segment);
        for action in &actions {
            let addr = resource_action_addr(action, &resource_base, plural);
            out.push(make_route_edge(
                path,
                line,
                &full_controller,
                action,
                trust,
                addr.as_ref(),
            ));
        }
        if let Some(body) = call_block_body(node) {
            let new_ctx = RouteCtx {
                module_prefix: ctx.module_prefix.clone(),
                default_controller: Some(full_controller),
                path_prefix: resource_base,
                // A route declared directly inside a `resources` block with
                // no `on:` nests under the parent's own id (Rails: `/photos/
                // :photo_id/preview`); a singular `resource` has no id.
                child_param: if plural {
                    Some(format!(":{}_id", singularize(name)))
                } else {
                    None
                },
                member_param: if plural {
                    Some(":id".to_string())
                } else {
                    None
                },
            };
            walk_body(body, source, &new_ctx, path, out);
        }
    }
}

/// Inverse of [`pluralize`] for the nested-resource param convention
/// (`resources :rounds do resources :catalogs end` → `/rounds/:round_id/
/// catalogs`). The same honest approximation, in the same three cases; a
/// name it cannot singularise is returned unchanged rather than mangled.
fn singularize(name: &str) -> String {
    if let Some(stem) = name.strip_suffix("ies") {
        return format!("{stem}y");
    }
    for suffix in ["sses", "xes", "ches", "shes"] {
        if let Some(stem) = name.strip_suffix("es") {
            if name.ends_with(suffix) {
                return stem.to_string();
            }
        }
    }
    match name.strip_suffix('s') {
        Some(stem) if !stem.is_empty() => stem.to_string(),
        _ => name.to_string(),
    }
}

enum OptList {
    Literal(Vec<String>),
    NonLiteral,
}

fn resolve_action_set(
    base: &[&'static str],
    only: Option<&OptList>,
    except: Option<&OptList>,
) -> (Vec<&'static str>, Trust) {
    match only {
        Some(OptList::Literal(list)) => {
            let set = base
                .iter()
                .filter(|a| list.iter().any(|l| l == *a))
                .copied()
                .collect();
            return (set, Trust::Likely);
        }
        Some(OptList::NonLiteral) => return (base.to_vec(), Trust::Candidate),
        None => {}
    }
    match except {
        Some(OptList::Literal(list)) => {
            let set = base
                .iter()
                .filter(|a| !list.iter().any(|l| l == *a))
                .copied()
                .collect();
            (set, Trust::Likely)
        }
        Some(OptList::NonLiteral) => (base.to_vec(), Trust::Candidate),
        None => (base.to_vec(), Trust::Likely),
    }
}

// --- namespace / scope / controller block -----------------------------------

fn handle_namespace(
    node: Node,
    source: &[u8],
    ctx: &RouteCtx,
    path: &str,
    out: &mut Vec<FrameworkEdge>,
) {
    let args = call_args(node);
    let Some(first) = args.first() else {
        return;
    };
    let Some(name) = literal_string_or_symbol(*first, source) else {
        return;
    };
    let mut opts = Vec::new();
    for a in &args[1..] {
        collect_pairs_into(*a, &mut opts);
    }
    let segment = find_pair_literal(&opts, "module", source).unwrap_or_else(|| name.clone());
    // `namespace :admin, module: "backoffice"` moves the MODULE only;
    // `path:` moves the URL only. Two independent axes.
    let url_segment = find_pair_literal(&opts, "path", source).unwrap_or(name);
    let controller_opt = find_pair_literal(&opts, "controller", source);

    if let Some(body) = call_block_body(node) {
        let mut module_prefix = ctx.module_prefix.clone();
        module_prefix.push(segment);
        let default_controller = controller_opt.map(|c| join_path(&module_prefix, &c));
        let mut path_prefix = ctx.child_base();
        path_prefix.push(url_segment);
        let new_ctx = RouteCtx {
            module_prefix,
            default_controller,
            path_prefix,
            child_param: None,
            member_param: None,
        };
        walk_body(body, source, &new_ctx, path, out);
    }
}

fn handle_scope(
    node: Node,
    source: &[u8],
    ctx: &RouteCtx,
    path: &str,
    out: &mut Vec<FrameworkEdge>,
) {
    let args = call_args(node);
    let mut opts = Vec::new();
    // A leading positional literal is a URL scope (`scope "admin" do`), not
    // an option pair — Rails' `scope path:` written the short way.
    let positional_path = args
        .first()
        .and_then(|a| literal_string_or_symbol(*a, source));
    for a in &args {
        collect_pairs_into(*a, &mut opts);
    }
    let mut module_prefix = ctx.module_prefix.clone();
    let mut default_controller = ctx.default_controller.clone();
    if let Some(module_val) = find_pair_literal(&opts, "module", source) {
        module_prefix.push(module_val);
        default_controller = None;
    }
    if let Some(ctrl) = find_pair_literal(&opts, "controller", source) {
        default_controller = Some(join_path(&module_prefix, &ctrl));
    }
    let mut path_prefix = ctx.child_base();
    if let Some(seg) = find_pair_literal(&opts, "path", source).or(positional_path) {
        path_prefix.push(seg);
    }
    if let Some(body) = call_block_body(node) {
        let new_ctx = RouteCtx {
            module_prefix,
            default_controller,
            path_prefix,
            child_param: None,
            member_param: None,
        };
        walk_body(body, source, &new_ctx, path, out);
    }
}

fn handle_controller_block(
    node: Node,
    source: &[u8],
    ctx: &RouteCtx,
    path: &str,
    out: &mut Vec<FrameworkEdge>,
) {
    let args = call_args(node);
    let Some(first) = args.first() else {
        return;
    };
    let Some(name) = literal_string_or_symbol(*first, source) else {
        return;
    };
    if let Some(body) = call_block_body(node) {
        // A `controller :x do … end` block renames the DISPATCH target,
        // never the URL — the enclosing URL scope (including any resource
        // param) is carried through untouched.
        let new_ctx = RouteCtx {
            default_controller: Some(join_path(&ctx.module_prefix, &name)),
            ..ctx.clone()
        };
        walk_body(body, source, &new_ctx, path, out);
    }
}

// --- verbs / root / draw -----------------------------------------------------

fn handle_verb(
    node: Node,
    source: &[u8],
    ctx: &RouteCtx,
    path: &str,
    out: &mut Vec<FrameworkEdge>,
    verb: &'static str,
) {
    let args = call_args(node);
    let Some(first) = args.first().copied() else {
        return;
    };
    let mut opts = Vec::new();
    for a in &args[1..] {
        collect_pairs_into(*a, &mut opts);
    }
    let line = src_line(node);
    // `on: :member` / `on: :collection` is the inline form of the block
    // scopes — same URL rule, written on one line.
    let base = match find_pair_literal(&opts, "on", source).as_deref() {
        Some("member") => ctx.member_scope().child_base(),
        Some("collection") => ctx.collection_scope().child_base(),
        _ => ctx.child_base(),
    };
    let explicit_path = find_pair_literal(&opts, "path", source);

    if let Some(to) = find_pair_literal(&opts, "to", source) {
        if let Some((controller, action)) = resolve_to_string(&to, &ctx.module_prefix) {
            // With `to:`, the FIRST positional is the URL pattern itself
            // (`get "stores", to: "store#stores"`). A leading `/` escapes
            // the enclosing scope, exactly as it does for the controller.
            let addr = explicit_path
                .clone()
                .or_else(|| literal_string_or_symbol(first, source))
                .map(|pattern| verb_addr(verb, &base, &pattern));
            out.push(make_route_edge(
                path,
                line,
                &controller,
                &action,
                Trust::Likely,
                addr.as_ref(),
            ));
        }
        return;
    }

    let controller = find_pair_literal(&opts, "controller", source)
        .map(|c| join_path(&ctx.module_prefix, &c))
        .or_else(|| ctx.default_controller.clone())
        .or_else(|| {
            if ctx.module_prefix.is_empty() {
                None
            } else {
                Some(ctx.module_prefix.join("/"))
            }
        });
    let Some(controller) = controller else {
        return; // unresolvable controller — never fabricate.
    };

    let action = if let Some(a) = find_pair_literal(&opts, "action", source) {
        a
    } else if let Some(a) = literal_string_or_symbol(first, source) {
        // A path-pattern string (contains a `/` or a `:param`) with no
        // explicit `action:`/`to:` doesn't name an action — drop.
        if a.contains('/') || a.contains(':') {
            return;
        }
        a
    } else {
        return; // non-literal first arg, no action: — drop.
    };

    // The URL pattern is `path:` if given, else the FIRST positional
    // (which is the pattern whenever an explicit `action:` supplied the
    // action name), else the action name itself — `get :test` → `/test`.
    let pattern = explicit_path
        .or_else(|| literal_string_or_symbol(first, source))
        .unwrap_or_else(|| action.clone());
    let addr = verb_addr(verb, &base, &pattern);
    out.push(make_route_edge(
        path,
        line,
        &controller,
        &action,
        Trust::Likely,
        Some(&addr),
    ));
}

/// A verb call's address: the enclosing URL scope plus the route's own
/// pattern, with Rails' leading-`/` escape (an absolute pattern ignores the
/// scope, the same convention `resolve_to_string` honours for controllers).
fn verb_addr(verb: &'static str, base: &[String], pattern: &str) -> RouteAddr {
    if pattern.starts_with('/') {
        return RouteAddr {
            verb,
            url: url_from(&[pattern.to_string()]),
        };
    }
    let mut segs = base.to_vec();
    segs.push(pattern.to_string());
    RouteAddr::new(verb, &segs)
}

fn handle_root(
    node: Node,
    source: &[u8],
    ctx: &RouteCtx,
    path: &str,
    out: &mut Vec<FrameworkEdge>,
) {
    let args = call_args(node);
    let mut opts = Vec::new();
    for a in &args {
        collect_pairs_into(*a, &mut opts);
    }
    let to = find_pair_literal(&opts, "to", source).or_else(|| {
        args.first()
            .and_then(|a| literal_string_or_symbol(*a, source))
    });
    let Some(to) = to else {
        return;
    };
    let Some((controller, action)) = resolve_to_string(&to, &ctx.module_prefix) else {
        return;
    };
    let addr = RouteAddr::new("GET", &ctx.path_prefix);
    out.push(make_route_edge(
        path,
        src_line(node),
        &controller,
        &action,
        Trust::Likely,
        Some(&addr),
    ));
}

/// A bare `draw(:name)` file-include call (no receiver, no block) — see the
/// module doc. Called on every `call` node named `draw`; the wrapper shape
/// (`X.routes.draw do … end`) is filtered out by the receiver/block checks
/// so it never double-emits here (it's consumed by
/// `find_draw_wrapper_body` before `walk_body` ever sees it as a
/// statement).
fn handle_draw(node: Node, source: &[u8], path: &str, out: &mut Vec<FrameworkEdge>) {
    if node.child_by_field_name("receiver").is_some() || node.child_by_field_name("block").is_some()
    {
        return;
    }
    let args = call_args(node);
    let Some(name) = args
        .first()
        .and_then(|a| literal_string_or_symbol(*a, source))
    else {
        return;
    };
    out.push(FrameworkEdge {
        kind: EdgeKind::RouteFile,
        src_path: path.to_string(),
        src_line: Some(src_line(node)),
        src_symbol: None,
        dst_kind: Some("routes_file".to_string()),
        dst_path: Some(format!("config/routes/{name}.rb")),
        dst_symbol: None,
        trust: Trust::Likely,
        extra_json: None,
    });
}

fn make_route_edge(
    path: &str,
    line: u32,
    controller: &str,
    action: &str,
    trust: Trust,
    addr: Option<&RouteAddr>,
) -> FrameworkEdge {
    FrameworkEdge {
        kind: EdgeKind::RouteAction,
        src_path: path.to_string(),
        src_line: Some(line),
        src_symbol: None,
        dst_kind: Some("controller_action".to_string()),
        dst_path: Some(format!("app/controllers/{controller}_controller.rb")),
        dst_symbol: Some(format!("{controller}#{action}")),
        trust,
        // ABSENT, never guessed: a route whose URL this walk could not
        // reconstruct (a non-literal pattern) carries no `path` at all.
        extra_json: addr.map(route_extra_json),
    }
}

// --- shared helpers ----------------------------------------------------------

fn src_line(node: Node) -> u32 {
    node.start_position().row as u32 + 1
}

fn join_path(prefix: &[String], leaf: &str) -> String {
    if prefix.is_empty() {
        leaf.to_string()
    } else {
        format!("{}/{}", prefix.join("/"), leaf)
    }
}

/// `"ctrl#action"` (or `"/ctrl#action"` — a leading `/` escapes the current
/// namespace, e.g. the real repo's `root to: '/backoffice/home#index'`
/// inside `scope module: 'admin'`) → `(resolved_controller, action)`.
/// `None` if `to` isn't `#`-shaped at all (e.g. a redirect lambda/proc, or
/// a bare route name with no controller — genuinely not this grammar's
/// concern, drop).
fn resolve_to_string(to: &str, module_prefix: &[String]) -> Option<(String, String)> {
    let (ctrl, action) = to.split_once('#')?;
    if action.is_empty() || ctrl.is_empty() {
        return None;
    }
    if let Some(stripped) = ctrl.strip_prefix('/') {
        Some((stripped.to_string(), action.to_string()))
    } else {
        Some((join_path(module_prefix, ctrl), action.to_string()))
    }
}

/// Simple English pluralizer for `resource`'s (singular) controller-name
/// convention (`resource :session` → `SessionsController`). Covers the
/// common cases (`s`/`x`/`ch`/`sh` → `+es`, consonant+`y` → `ies`, else
/// `+s`) — an honest, documented approximation, not a full inflector.
pub(crate) fn pluralize(name: &str) -> String {
    if name.ends_with('s') || name.ends_with('x') || name.ends_with("ch") || name.ends_with("sh") {
        format!("{name}es")
    } else if name.ends_with('y') && !ends_with_vowel_then_y(name) {
        format!("{}ies", &name[..name.len() - 1])
    } else {
        format!("{name}s")
    }
}

fn ends_with_vowel_then_y(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.len() < 2 {
        return false;
    }
    matches!(bytes[bytes.len() - 2], b'a' | b'e' | b'i' | b'o' | b'u')
}

fn call_method_name(node: Node, source: &[u8]) -> Option<String> {
    let m = node.child_by_field_name("method")?;
    m.utf8_text(source).ok().map(|s| s.to_string())
}

fn call_args(node: Node<'_>) -> Vec<Node<'_>> {
    let Some(list) = node.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut cursor = list.walk();
    list.named_children(&mut cursor).collect()
}

fn call_block_body(node: Node<'_>) -> Option<Node<'_>> {
    let block = node.child_by_field_name("block")?;
    block_body_node(block)
}

fn block_body_node(block: Node<'_>) -> Option<Node<'_>> {
    match block.kind() {
        "do_block" | "block" => block.child_by_field_name("body"),
        _ => None,
    }
}

/// Push `node` onto `opts` if it's a bare `pair` (the common trailing-hash-
/// without-braces shape, e.g. `only: [:show]`); if it's an explicit `hash`
/// literal (`{only: [:show]}`), flatten its `pair` children in instead.
/// Anything else (a positional literal already consumed elsewhere, a
/// non-literal expression) is ignored.
fn collect_pairs_into<'a>(node: Node<'a>, opts: &mut Vec<Node<'a>>) {
    match node.kind() {
        "pair" => opts.push(node),
        "hash" => {
            let mut cursor = node.walk();
            for c in node.named_children(&mut cursor) {
                if c.kind() == "pair" {
                    opts.push(c);
                }
            }
        }
        _ => {}
    }
}

fn pair_key_name(pair: Node, source: &[u8]) -> Option<String> {
    let key = pair.child_by_field_name("key")?;
    match key.kind() {
        "hash_key_symbol" => key
            .utf8_text(source)
            .ok()
            .map(|s| s.trim_end_matches(':').to_string()),
        "simple_symbol" => literal_symbol(key, source),
        "string" => literal_string(key, source),
        "identifier" => key.utf8_text(source).ok().map(|s| s.to_string()),
        _ => None,
    }
}

fn find_pair_literal(opts: &[Node], key: &str, source: &[u8]) -> Option<String> {
    let pair = opts
        .iter()
        .find(|p| pair_key_name(**p, source).as_deref() == Some(key))?;
    let value = pair.child_by_field_name("value")?;
    literal_string_or_symbol(value, source)
}

fn find_pair_symbol_list(opts: &[Node], key: &str, source: &[u8]) -> Option<OptList> {
    let pair = opts
        .iter()
        .find(|p| pair_key_name(**p, source).as_deref() == Some(key))?;
    let value = pair.child_by_field_name("value")?;
    match value.kind() {
        "array" => {
            let mut cursor = value.walk();
            let mut names = Vec::new();
            for c in value.named_children(&mut cursor) {
                match literal_string_or_symbol(c, source) {
                    Some(n) => names.push(n),
                    None => return Some(OptList::NonLiteral),
                }
            }
            Some(OptList::Literal(names))
        }
        "symbol_array" | "string_array" => {
            let mut cursor = value.walk();
            let names: Vec<String> = value
                .named_children(&mut cursor)
                .filter_map(|c| c.utf8_text(source).ok())
                .map(|s| {
                    s.trim_start_matches(':')
                        .trim_matches(['"', '\''])
                        .to_string()
                })
                .collect();
            Some(OptList::Literal(names))
        }
        _ => match literal_string_or_symbol(value, source) {
            Some(n) => Some(OptList::Literal(vec![n])),
            None => Some(OptList::NonLiteral),
        },
    }
}

/// A plain (non-interpolated) string or symbol literal's text value —
/// `None` for anything else, INCLUDING an interpolated string (never treat
/// `"#{x}"` as if it were a literal). Shared by every "is this argument a
/// literal I can resolve" check in this module.
fn literal_string_or_symbol(node: Node, source: &[u8]) -> Option<String> {
    literal_string(node, source).or_else(|| literal_symbol(node, source))
}

fn literal_string(node: Node, source: &[u8]) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }
    let mut cursor = node.walk();
    if node
        .children(&mut cursor)
        .any(|c| c.kind() == "interpolation")
    {
        return None;
    }
    let text = node.utf8_text(source).ok()?;
    if text.len() < 2 {
        return None;
    }
    Some(text[1..text.len() - 1].to_string())
}

fn literal_symbol(node: Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "simple_symbol" => {
            let text = node.utf8_text(source).ok()?;
            Some(text.trim_start_matches(':').to_string())
        }
        "delimited_symbol" => {
            let mut cursor = node.walk();
            if node
                .children(&mut cursor)
                .any(|c| c.kind() == "interpolation")
            {
                return None;
            }
            let text = node.utf8_text(source).ok()?;
            let after_colon = text.strip_prefix(':')?;
            if after_colon.len() < 2 {
                return None;
            }
            Some(after_colon[1..after_colon.len() - 1].to_string())
        }
        _ => None,
    }
}

// --- PRR-N4: devise_for → override controller edges -------------------------

/// The closed set of controller basenames Devise's own routing macro can
/// generate overrides for (`devise_for :users, controllers: { sessions:
/// "users/sessions" }` is the OFFICIAL override mechanism, but in practice
/// apps just drop a same-named file under the conventional path and Devise
/// finds it automatically — this extractor doesn't parse the `controllers:`
/// option at all, it existence-checks every possible override basename
/// instead, see the module doc below).
const DEVISE_OVERRIDE_CONTROLLERS: &[&str] = &[
    "sessions",
    "registrations",
    "passwords",
    "confirmations",
    "unlocks",
    "omniauth_callbacks",
];

/// PRR-N4: `devise_for :users` (Devise's own route-generating macro) →
/// `app/controllers/<plural>/<override>_controller.rb`, but ONLY for
/// override files that ACTUALLY EXIST on disk — Devise generates a dozen
/// implicit routes whose controllers are usually Devise's own gem-internal
/// ones; an app only sometimes overrides a subset. Emitting an edge for
/// every possible override regardless of existence would violate the
/// "never fabricate a target" trust law, so existence is the gate, not the
/// DSL call itself. ALWAYS `Trust::Candidate` (see `frameworks` module
/// doc's grammar table) — a same-named controller file existing is
/// suggestive, not proof it's actually wired as THIS `devise_for`'s
/// override (Devise's `controllers:` option can point anywhere; this
/// extractor never parses it).
///
/// A SEPARATE, targeted whole-tree scan — not threaded through
/// [`extract`]'s `RouteCtx`/`walk_stmt` machinery, whose main walk still
/// treats `devise_for` as a hard drop (see
/// `direct_mount_devise_authenticate_are_honestly_dropped`). Consequence:
/// a `devise_for` nested inside a `namespace`/`scope` block is a
/// documented gap (the real acme-shop repo's own `devise_for :user` is
/// top-level and unnamespaced, so this covers the actual target repo
/// exactly).
pub fn extract_devise_overrides(repo_root: &Path, path: &str, bytes: &[u8]) -> Vec<FrameworkEdge> {
    let Ok((tree, _language)) = crate::lang::parse("ruby", bytes) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    find_devise_for_calls(tree.root_node(), bytes, path, repo_root, &mut out);
    out
}

fn find_devise_for_calls(
    node: Node,
    source: &[u8],
    path: &str,
    repo_root: &Path,
    out: &mut Vec<FrameworkEdge>,
) {
    if node.kind() == "call" && call_method_name(node, source).as_deref() == Some("devise_for") {
        out.extend(resolve_devise_for(node, source, path, repo_root));
        return; // never descend into its own args/block further.
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        find_devise_for_calls(child, source, path, repo_root, out);
    }
}

fn resolve_devise_for(
    node: Node,
    source: &[u8],
    path: &str,
    repo_root: &Path,
) -> Vec<FrameworkEdge> {
    let args = call_args(node);
    let Some(first) = args.first() else {
        return Vec::new();
    };
    let Some(name) = literal_string_or_symbol(*first, source) else {
        return Vec::new();
    };
    // Devise pluralizes the given mapping name for its routing scope; a
    // name that already LOOKS plural (ends in `s`) is left as-is rather
    // than run through `pluralize` again (which is not idempotent for an
    // already-plural word, e.g. would turn "users" into "userses").
    let plural = if name.ends_with('s') {
        name.clone()
    } else {
        pluralize(&name)
    };
    let line = src_line(node);
    let mut out = Vec::new();
    for ctrl in DEVISE_OVERRIDE_CONTROLLERS {
        let dst_path = format!("app/controllers/{plural}/{ctrl}_controller.rb");
        if repo_root.join(&dst_path).is_file() {
            out.push(FrameworkEdge {
                kind: EdgeKind::DeviseOverride,
                src_path: path.to_string(),
                src_line: Some(line),
                src_symbol: None,
                dst_kind: Some("controller_override".to_string()),
                dst_path: Some(dst_path),
                dst_symbol: None,
                trust: Trust::Candidate,
                extra_json: Some(format!(r#"{{"resource":"{name}"}}"#)),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(edges: &[FrameworkEdge]) -> Vec<&'static str> {
        edges.iter().map(|e| e.kind.as_str()).collect()
    }

    fn dst_symbols(edges: &[FrameworkEdge]) -> Vec<String> {
        edges.iter().filter_map(|e| e.dst_symbol.clone()).collect()
    }

    /// `controller#action` → the `extra_json` address the walk recorded, as
    /// `"VERB path"`, or `"?"` when the walk recorded none.
    fn addresses(edges: &[FrameworkEdge]) -> Vec<(String, String)> {
        edges
            .iter()
            .filter(|e| e.kind == EdgeKind::RouteAction)
            .map(|e| {
                let sym = e.dst_symbol.clone().unwrap_or_default();
                let addr = match &e.extra_json {
                    None => "?".to_string(),
                    Some(raw) => {
                        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
                        format!(
                            "{} {}",
                            v["verb"].as_str().unwrap(),
                            v["path"].as_str().unwrap()
                        )
                    }
                };
                (sym, addr)
            })
            .collect()
    }

    fn address_of(edges: &[FrameworkEdge], sym: &str) -> String {
        addresses(edges)
            .into_iter()
            .find(|(s, _)| s == sym)
            .unwrap_or_else(|| panic!("no edge for {sym}: {:?}", addresses(edges)))
            .1
    }

    #[test]
    fn simple_resources_produces_full_crud_at_likely() {
        let src = b"Rails.application.routes.draw do\n  resources :users\nend\n";
        let edges = extract("config/routes.rb", src);
        assert_eq!(edges.len(), 7);
        assert!(edges.iter().all(|e| matches!(e.trust, Trust::Likely)));
        assert!(dst_symbols(&edges).contains(&"users#index".to_string()));
        assert!(dst_symbols(&edges).contains(&"users#destroy".to_string()));
    }

    #[test]
    fn resources_only_filters_to_literal_set() {
        let src =
            b"Rails.application.routes.draw do\n  resources :rounds, only: %i[index show]\nend\n";
        let edges = extract("config/routes.rb", src);
        let got = dst_symbols(&edges);
        assert_eq!(got.len(), 2);
        assert!(got.contains(&"rounds#index".to_string()));
        assert!(got.contains(&"rounds#show".to_string()));
    }

    #[test]
    fn resources_except_filters_out_literal_set() {
        let src =
            b"Rails.application.routes.draw do\n  resources :billing, except: [:destroy]\nend\n";
        let edges = extract("config/routes.rb", src);
        assert!(!dst_symbols(&edges).contains(&"billing#destroy".to_string()));
        assert_eq!(edges.len(), 6);
    }

    #[test]
    fn resources_only_non_literal_falls_back_to_full_set_at_candidate() {
        let src = b"Rails.application.routes.draw do\n  resources :users, only: SOME_CONST\nend\n";
        let edges = extract("config/routes.rb", src);
        assert_eq!(edges.len(), 7);
        assert!(edges.iter().all(|e| matches!(e.trust, Trust::Candidate)));
    }

    #[test]
    fn namespace_prefixes_controller_module() {
        let src = b"Rails.application.routes.draw do\n  namespace :trade do\n    resources :rounds, only: [:index]\n  end\nend\n";
        let edges = extract("config/routes.rb", src);
        assert_eq!(dst_symbols(&edges), vec!["trade/rounds#index".to_string()]);
        assert_eq!(
            edges[0].dst_path,
            Some("app/controllers/trade/rounds_controller.rb".to_string())
        );
    }

    #[test]
    fn member_and_collection_bare_symbols_resolve_against_the_resource_controller() {
        let src = br#"
Rails.application.routes.draw do
  namespace :trade do
    resources :rounds, only: [] do
      collection do
        get :search_pharmacies
      end
      member do
        post :merge_catalogs
      end
    end
  end
end
"#;
        let edges = extract("config/routes.rb", src);
        let got = dst_symbols(&edges);
        assert!(got.contains(&"trade/rounds#search_pharmacies".to_string()));
        assert!(got.contains(&"trade/rounds#merge_catalogs".to_string()));
    }

    #[test]
    fn explicit_to_string_form_with_leading_slash_escapes_the_namespace() {
        let src = br#"
Rails.application.routes.draw do
  scope module: 'admin', path: 'admin', as: :admin do
    root to: '/backoffice/home#index', as: :root
    get 'stores', to: 'store#stores'
  end
end
"#;
        let edges = extract("config/routes.rb", src);
        let got = dst_symbols(&edges);
        assert!(got.contains(&"backoffice/home#index".to_string()));
        assert!(got.contains(&"admin/store#stores".to_string()));
    }

    #[test]
    fn namespace_default_controller_matches_real_rails_convention() {
        // Verified against the real repo: `app/controllers/api/internal/
        // ops_controller.rb` exists for exactly this shape (no `resources`/
        // `controller` block, just a bare `get` inside nested namespaces).
        let src = br#"
Rails.application.routes.draw do
  namespace :api do
    namespace :internal do
      namespace :ops do
        get :test
      end
    end
  end
end
"#;
        let edges = extract("config/routes.rb", src);
        assert_eq!(
            dst_symbols(&edges),
            vec!["api/internal/ops#test".to_string()]
        );
    }

    #[test]
    fn controller_block_sets_default_controller_for_bare_verbs() {
        let src = br#"
Rails.application.routes.draw do
  namespace :internal do
    controller :shutdown do
      get :shutdown
      put :toggle_alb
    end
  end
end
"#;
        let edges = extract("config/routes.rb", src);
        let got = dst_symbols(&edges);
        assert!(got.contains(&"internal/shutdown#shutdown".to_string()));
        assert!(got.contains(&"internal/shutdown#toggle_alb".to_string()));
    }

    #[test]
    fn direct_mount_devise_authenticate_are_honestly_dropped() {
        let src = br#"
Rails.application.routes.draw do
  mount HealthMonitor::Engine, at: '/'
  authenticate :user, ->(u) { u.admin? } do
    mount Sidekiq::Web => '/sidekiq'
  end
  direct :terms_document do
    "https://example.com/terms"
  end
  devise_for :user, path: 'admin'
  devise_scope :user do
    get :login, controller: :sessions, action: :new
  end
  resources :users, only: [:index]
end
"#;
        let edges = extract("config/routes.rb", src);
        // Only the plain `resources :users` survives — devise_scope's
        // otherwise-resolvable get is dropped along with its construct
        // (see the module doc's honest-drop-list rationale).
        assert_eq!(dst_symbols(&edges), vec!["users#index".to_string()]);
    }

    #[test]
    fn non_literal_render_style_argument_is_dropped_not_fabricated() {
        let src = b"Rails.application.routes.draw do\n  get some_dynamic_path, to: 'x#y'\nend\n";
        // `some_dynamic_path` (a bare identifier) is still a fine first
        // positional arg here since `to:` is explicit and literal — this
        // exercises that a non-literal FIRST arg doesn't block a literal
        // `to:` from resolving.
        let edges = extract("config/routes.rb", src);
        assert_eq!(dst_symbols(&edges), vec!["x#y".to_string()]);
    }

    #[test]
    fn bare_verb_with_no_resolvable_controller_is_dropped() {
        let src = b"Rails.application.routes.draw do\n  get :orphan\nend\n";
        let edges = extract("config/routes.rb", src);
        assert!(edges.is_empty());
    }

    #[test]
    fn draw_file_include_emits_route_file_edges() {
        let src = b"AcmeShop::Application.routes.draw do\n  draw(:admin)\n  draw(:trade)\nend\n";
        let edges = extract("config/routes.rb", src);
        assert_eq!(kinds(&edges), vec!["route_file", "route_file"]);
        assert_eq!(
            edges[0].dst_path,
            Some("config/routes/admin.rb".to_string())
        );
        assert_eq!(
            edges[1].dst_path,
            Some("config/routes/trade.rb".to_string())
        );
        assert!(edges.iter().all(|e| matches!(e.trust, Trust::Likely)));
    }

    #[test]
    fn split_routes_file_has_no_draw_wrapper_and_is_walked_directly() {
        // config/routes/trade.rb's real shape: top-level `constraints do …
        // end`, no `X.routes.draw do` wrapper at all.
        let src = br#"
constraints RoutesConstraint::Admin do
  namespace :trade do
    resources :customers
  end
end
"#;
        let edges = extract("config/routes/trade.rb", src);
        assert!(dst_symbols(&edges).contains(&"trade/customers#index".to_string()));
    }

    #[test]
    fn nested_resources_get_their_own_controller_not_the_parent() {
        let src = br#"
Rails.application.routes.draw do
  namespace :trade do
    resources :offers, only: [] do
      resources :consolidated_offer_items, only: [:destroy]
    end
  end
end
"#;
        let edges = extract("config/routes.rb", src);
        assert_eq!(
            dst_symbols(&edges),
            vec!["trade/consolidated_offer_items#destroy".to_string()]
        );
    }

    #[test]
    fn singular_resource_pluralizes_the_default_controller() {
        let src = b"Rails.application.routes.draw do\n  resource :session, only: [:create]\nend\n";
        let edges = extract("config/routes.rb", src);
        assert_eq!(dst_symbols(&edges), vec!["sessions#create".to_string()]);
    }

    // --- PRR-N4: devise_for → override edges --------------------------------

    fn write(dir: &std::path::Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    #[test]
    fn devise_for_emits_candidate_edges_only_for_existing_override_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "app/controllers/users/omniauth_callbacks_controller.rb",
            "class Users::OmniauthCallbacksController; end\n",
        );
        let src = b"Rails.application.routes.draw do\n  devise_for :user, controllers: { omniauth_callbacks: 'users/omniauth_callbacks' }\nend\n";
        let edges = extract_devise_overrides(root, "config/routes.rb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, EdgeKind::DeviseOverride);
        assert_eq!(edges[0].trust, Trust::Candidate);
        assert_eq!(
            edges[0].dst_path,
            Some("app/controllers/users/omniauth_callbacks_controller.rb".to_string())
        );
    }

    #[test]
    fn devise_for_with_no_override_files_emits_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let src = b"Rails.application.routes.draw do\n  devise_for :user\nend\n";
        let edges = extract_devise_overrides(root, "config/routes.rb", src);
        assert!(edges.is_empty());
    }

    #[test]
    fn devise_for_already_plural_resource_name_is_not_double_pluralized() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "app/controllers/admins/sessions_controller.rb",
            "class Admins::SessionsController; end\n",
        );
        let src = b"Rails.application.routes.draw do\n  devise_for :admins\nend\n";
        let edges = extract_devise_overrides(root, "config/routes.rb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(
            edges[0].dst_path,
            Some("app/controllers/admins/sessions_controller.rb".to_string())
        );
    }

    #[test]
    fn main_route_walk_still_drops_devise_for_entirely() {
        // The N3 `extract()` DSL walk (routes/views resolution) must remain
        // untouched by the N4 devise_for extension — see
        // `direct_mount_devise_authenticate_are_honestly_dropped` above.
        let src = b"Rails.application.routes.draw do\n  devise_for :user\nend\n";
        let edges = extract("config/routes.rb", src);
        assert!(edges.is_empty());
    }
    // --- V72-I1: the route ADDRESS ------------------------------------

    #[test]
    fn resources_actions_get_their_restful_verb_and_path() {
        let src = b"Rails.application.routes.draw do\n  resources :orders\nend\n";
        let edges = extract("config/routes.rb", src);
        assert_eq!(address_of(&edges, "orders#index"), "GET /orders");
        assert_eq!(address_of(&edges, "orders#create"), "POST /orders");
        assert_eq!(address_of(&edges, "orders#new"), "GET /orders/new");
        assert_eq!(address_of(&edges, "orders#show"), "GET /orders/:id");
        assert_eq!(address_of(&edges, "orders#edit"), "GET /orders/:id/edit");
        // Rails answers `update` on PATCH *and* PUT; the lens records PATCH
        // rather than doubling every edge (see the module doc).
        assert_eq!(address_of(&edges, "orders#update"), "PATCH /orders/:id");
        assert_eq!(address_of(&edges, "orders#destroy"), "DELETE /orders/:id");
    }

    #[test]
    fn a_singular_resource_has_no_member_segment() {
        let src = b"Rails.application.routes.draw do\n  resource :profile\nend\n";
        let edges = extract("config/routes.rb", src);
        assert_eq!(address_of(&edges, "profiles#show"), "GET /profile");
        assert_eq!(address_of(&edges, "profiles#edit"), "GET /profile/edit");
        assert_eq!(address_of(&edges, "profiles#update"), "PATCH /profile");
    }

    #[test]
    fn namespace_moves_both_axes_and_scope_module_moves_only_the_module() {
        let src = b"Rails.application.routes.draw do\n  namespace :admin do\n    resources :reports, only: [:index]\n  end\n  scope module: :internal do\n    resources :flags, only: [:index]\n  end\nend\n";
        let edges = extract("config/routes.rb", src);
        assert_eq!(
            address_of(&edges, "admin/reports#index"),
            "GET /admin/reports"
        );
        // `scope module:` renames the controller, never the URL.
        assert_eq!(address_of(&edges, "internal/flags#index"), "GET /flags");
    }

    #[test]
    fn member_collection_and_a_nested_resource_each_take_their_own_segment() {
        let src = b"Rails.application.routes.draw do\n  resources :rounds, only: [] do\n    collection do\n      get :search\n    end\n    member do\n      post :merge\n    end\n    resources :catalogs, only: [:create]\n  end\nend\n";
        let edges = extract("config/routes.rb", src);
        assert_eq!(address_of(&edges, "rounds#search"), "GET /rounds/search");
        assert_eq!(address_of(&edges, "rounds#merge"), "POST /rounds/:id/merge");
        assert_eq!(
            address_of(&edges, "catalogs#create"),
            "POST /rounds/:round_id/catalogs"
        );
    }

    #[test]
    fn the_inline_on_member_form_matches_the_block_form() {
        let src = b"Rails.application.routes.draw do\n  resources :rounds, only: [] do\n    post :merge, on: :member\n    get :search, on: :collection\n  end\nend\n";
        let edges = extract("config/routes.rb", src);
        assert_eq!(address_of(&edges, "rounds#merge"), "POST /rounds/:id/merge");
        assert_eq!(address_of(&edges, "rounds#search"), "GET /rounds/search");
    }

    #[test]
    fn a_verb_call_takes_its_pattern_and_a_leading_slash_escapes_the_scope() {
        let src = b"Rails.application.routes.draw do\n  namespace :admin do\n    get 'stores', to: 'store#stores'\n    get 'ping', to: '/health#ping'\n  end\n  root to: 'home#index'\nend\n";
        let edges = extract("config/routes.rb", src);
        assert_eq!(
            address_of(&edges, "admin/store#stores"),
            "GET /admin/stores"
        );
        // A leading `/` on the CONTROLLER escapes the namespace; the URL
        // pattern itself is still scope-relative here.
        assert_eq!(address_of(&edges, "health#ping"), "GET /admin/ping");
        assert_eq!(address_of(&edges, "home#index"), "GET /");
    }

    #[test]
    fn scope_path_and_a_bare_positional_scope_move_only_the_url() {
        let src = b"Rails.application.routes.draw do\n  scope 'v1' do\n    resources :orders, only: [:index]\n  end\n  scope path: 'v2' do\n    resources :carts, only: [:index]\n  end\nend\n";
        let edges = extract("config/routes.rb", src);
        assert_eq!(address_of(&edges, "orders#index"), "GET /v1/orders");
        assert_eq!(address_of(&edges, "carts#index"), "GET /v2/carts");
    }

    #[test]
    fn an_explicit_path_option_renames_the_url_but_not_the_controller() {
        let src = b"Rails.application.routes.draw do\n  resources :orders, path: 'ordini', only: [:index, :show]\nend\n";
        let edges = extract("config/routes.rb", src);
        assert_eq!(address_of(&edges, "orders#index"), "GET /ordini");
        assert_eq!(address_of(&edges, "orders#show"), "GET /ordini/:id");
    }

    #[test]
    fn the_address_is_json_escaped_and_the_grammar_version_never_moved() {
        // The URL is authored text; the value must survive a quote.
        let addr = RouteAddr {
            verb: "GET",
            url: "/a\"b".to_string(),
        };
        let raw = route_extra_json(&addr);
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["path"].as_str().unwrap(), "/a\"b");
        // Additive content is NOT a grammar bump (module doc, D7).
        assert_eq!(
            crate::frameworks::RAILS_LENS_GRAMMAR_VERSION,
            "rails-lens/1"
        );
    }
    /// The `tests/fixtures/rails-lens/config/routes/trade.rb` shape,
    /// verbatim and inline: a split routes file with NO
    /// `X.routes.draw do` wrapper (the `draw(:name)` convention's other
    /// half), a namespace, `only: %i[…]`, a collection block, a member
    /// block and a nested resource — the combination the golden walks, in
    /// a form that fails HERE, without a fixture read, when it drifts.
    #[test]
    fn the_split_routes_file_shape_gets_every_address_too() {
        let src = b"# frozen_string_literal: true\n\nnamespace :trade do\n  resources :rounds, only: %i[index show] do\n    collection do\n      get :search_pharmacies\n    end\n    member do\n      post :merge_catalogs\n    end\n    resources :catalogs, only: [:create]\n  end\nend\n";
        let edges = extract("config/routes/trade.rb", src);
        assert_eq!(
            address_of(&edges, "trade/rounds#index"),
            "GET /trade/rounds"
        );
        assert_eq!(
            address_of(&edges, "trade/rounds#show"),
            "GET /trade/rounds/:id"
        );
        assert_eq!(
            address_of(&edges, "trade/rounds#search_pharmacies"),
            "GET /trade/rounds/search_pharmacies"
        );
        assert_eq!(
            address_of(&edges, "trade/rounds#merge_catalogs"),
            "POST /trade/rounds/:id/merge_catalogs"
        );
        assert_eq!(
            address_of(&edges, "trade/catalogs#create"),
            "POST /trade/rounds/:round_id/catalogs"
        );
    }
}
