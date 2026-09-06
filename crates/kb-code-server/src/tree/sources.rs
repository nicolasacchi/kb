//! V71-F1 — the STORE half of kbc-tree/1: everything [`super::tree_v2_route`]
//! needs, read once, on the blocking pool.
//!
//! Two jobs, deliberately in one place: build the [`scope::ScopeSources`]
//! kbc-scope/1 resolves against, and build the per-path [`Facts`] the
//! decoration lanes render. Both are LANE-GATED — a request that asks for
//! no `ns:` atom and no `namespace` view never reads `entity_defs`, and a
//! request with no `decorate=` never reads annotations, findings, todos or
//! bookmarks. That gating is the whole cost model: the evidence report's
//! risk 3 ("Nine lanes × 5000 rows is a trivially catastrophic query
//! pattern") is answered by not running a lane nobody asked for, and by
//! every lane being ONE whole-repo grouped query rather than a per-row
//! lookup. There is no N+1 here and there must never be one.
//!
//! What is NOT here, and why: treemacs' third *deferred* tier (churn,
//! provenance, coverage, painted after idle). Those lanes need a second
//! round trip (`POST /api/tree/facts` keyed on the visible rows) that this
//! unit does not ship; declaring them in [`super::LANES`] with nothing
//! computing them would be the dead-surface defect, so they are absent
//! rather than empty.

use std::collections::{BTreeMap, HashMap};

use crate::routes::ApiError;
use crate::search::grammar::Diagnostic;
use crate::store::Store;

use super::scope::{self, ScopeFile, ScopeSources};
use super::{EntityPlacement, Facts, MAX_ENTITY_DEFS};

/// One request's worth of "what do I need to read".
pub struct BuildReq<'a> {
    pub store: &'a Store,
    pub repo_id: i64,
    pub repo_name: &'a str,
    pub repo_root: &'a std::path::Path,
    pub view: &'a str,
    pub worktree: Option<&'a str>,
    pub scope: Option<&'a str>,
    pub lanes: &'a [String],
    pub review_id: Option<i64>,
    /// The already-validated base ref, as a string (validation happens in
    /// the route, which owns the `Revspec` type — this module never builds
    /// a git argument from an unvalidated caller string).
    pub base: Option<&'a str>,
    pub config_scopes: BTreeMap<String, Vec<String>>,
}

/// Everything the route needs back.
pub struct Built {
    pub generation: u64,
    /// The candidate path set: every indexed file, narrowed by the scope
    /// when one applied.
    pub paths: Vec<String>,
    pub scope_applied: bool,
    pub scope_normalized: Option<String>,
    pub scope_notes: Vec<String>,
    pub scope_diagnostics: Vec<Diagnostic>,
    pub facts: HashMap<String, Facts>,
    pub entity_defs: Vec<EntityPlacement>,
    /// `(path, git name-status letter)` vs the base ref.
    pub changes: Vec<(String, String)>,
    pub build_notes: Vec<String>,
}

/// CODEOWNERS lookup order, exactly GitHub's: `.github/` first, then the
/// root, then `docs/`. The FIRST file that exists wins — a repo with two
/// of them does not merge their rules.
pub const CODEOWNERS_PATHS: [&str; 3] = [".github/CODEOWNERS", "CODEOWNERS", "docs/CODEOWNERS"];

/// GitHub's own documented cap on a CODEOWNERS file.
pub const MAX_CODEOWNERS_BYTES: u64 = 3 * 1024 * 1024;

pub fn build(req: BuildReq<'_>) -> Result<Built, ApiError> {
    let BuildReq {
        store,
        repo_id,
        repo_name,
        repo_root,
        view,
        worktree,
        scope,
        lanes,
        review_id,
        base,
        config_scopes,
    } = req;

    let generation = store.generation();
    let files = store.list_files(repo_id)?;
    let all_paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
    let mut build_notes: Vec<String> = Vec::new();

    // --- which lanes does this request actually need? --------------------
    let parsed_scope = scope.map(scope::parse);
    let scope_keys = parsed_scope
        .as_ref()
        .and_then(|p| p.expr.as_ref())
        .map(|e| e.atom_keys())
        .unwrap_or_default();
    let needs = |key: &str| scope_keys.contains(key);
    let want_entities = view == "namespace" || needs("ns");
    let want_git = lanes.iter().any(|l| l == "git") || view == "change";

    // --- the entity index (V71-G0) ---------------------------------------
    let mut entity_defs: Vec<EntityPlacement> = Vec::new();
    if want_entities {
        let rows = store.entity_defs_for_repo(repo_id, worktree, MAX_ENTITY_DEFS + 1)?;
        if rows.len() > MAX_ENTITY_DEFS {
            build_notes.push(format!(
                "entity index capped at {MAX_ENTITY_DEFS} definition rows — the namespace \
                 projection is PARTIAL for this repo"
            ));
        }
        for r in rows.into_iter().take(MAX_ENTITY_DEFS) {
            // Rows are CLAIMS; the class is minted here, per request, from
            // the same three inputs `entities::class_for` always takes
            // (crate invariant 13). Nothing about the class is stored.
            let stale = r
                .live_blob_hash
                .as_deref()
                .map(|live| live != r.blob_hash)
                .unwrap_or(true);
            let matched_via = crate::entities::MATCHED_VIA_NESTING;
            let trust =
                crate::entities::class_for(matched_via, &r.nesting, &r.zeitwerk_state, stale);
            entity_defs.push(EntityPlacement {
                fqn: r.fqn,
                path: r.path,
                kind: r.kind,
                trust,
            });
        }
    }

    // --- the scope ---------------------------------------------------------
    let mut scope_applied = false;
    let mut scope_normalized = None;
    let mut scope_notes = Vec::new();
    let mut scope_diagnostics = Vec::new();
    let mut paths = all_paths.clone();

    if let Some(raw) = scope {
        let mut src = ScopeSources {
            files: files
                .iter()
                .map(|f| ScopeFile {
                    path: f.path.clone(),
                    lang: f.lang.clone(),
                })
                .collect(),
            config_scopes,
            ..Default::default()
        };
        if needs("ns") {
            for d in &entity_defs {
                src.ns_by_path
                    .entry(d.path.clone())
                    .or_default()
                    .push(d.fqn.clone());
            }
        }
        if needs("pack") {
            src.packs = scope::packs_from_paths(all_paths.iter().map(String::as_str));
            src.sort_packs();
            if src.packs.is_empty() {
                scope_notes.push(
                    "`pack:` found no `package.yml` in the index — this repo is not a \
                     Packwerk app (or it has not been reconciled)"
                        .to_string(),
                );
            }
        }
        if needs("owner") {
            match read_codeowners(repo_root) {
                Some((path, text)) => {
                    src.owners = scope::parse_codeowners(&text);
                    scope_notes.push(format!(
                        "`owner:` read {path} ({} rules, last match wins)",
                        src.owners.len()
                    ));
                    if src
                        .owners
                        .iter()
                        .any(|(p, _)| p.contains('*') && !p.starts_with("*.") && p.trim() != "*")
                    {
                        scope_notes.push(
                            "some CODEOWNERS patterns use a `*` inside a path segment, which \
                             this reader does not match — those rules are IGNORED, not guessed"
                                .to_string(),
                        );
                    }
                }
                None => scope_notes.push(format!(
                    "`owner:` found no CODEOWNERS file (looked in {})",
                    CODEOWNERS_PATHS.join(", ")
                )),
            }
        }
        if needs("set") {
            for kind in crate::reading_sets::SET_KINDS.iter().copied() {
                for (row, _, _) in store.list_reading_sets(repo_id, Some(kind))? {
                    let members: std::collections::HashSet<String> = store
                        .reading_set_spans(&row.id)?
                        .into_iter()
                        .map(|s| s.path)
                        .collect();
                    src.sets.insert(row.id.to_lowercase(), members.clone());
                    src.sets.insert(row.name.to_lowercase(), members);
                }
            }
        }
        if needs("annot") {
            src.annot_open = store
                .open_annotation_counts_for_repo(repo_id)?
                .into_iter()
                .filter(|(_, n)| *n > 0)
                .map(|(p, _)| p)
                .collect();
        }
        if needs("todo") {
            src.todo = store
                .list_todo_items(repo_id, None, None)?
                .into_iter()
                .map(|t| t.path)
                .collect();
        }
        if needs("bookmark") {
            src.bookmark = store
                .list_bookmarks(repo_name)?
                .into_iter()
                .map(|b| b.path)
                .collect();
        }

        let resolved = scope::resolve(raw, &src);
        scope_applied = resolved.applied;
        scope_normalized = Some(resolved.normalized);
        scope_notes.extend(resolved.notes);
        scope_diagnostics = resolved.diagnostics;
        if let Some(set) = resolved.paths {
            paths.retain(|p| set.contains(p));
        }
    }

    // --- the change list ---------------------------------------------------
    let mut changes: Vec<(String, String)> = Vec::new();
    if want_git {
        match base {
            Some(base) => match crate::history::diff_files(repo_root, "diff", &["-M", base]) {
                Ok(fc) => {
                    changes = fc.into_iter().map(|c| (c.path, c.status)).collect();
                }
                Err(e) => build_notes.push(format!(
                    "the git lane is UNAVAILABLE against {base}: {e} — rows carry no git \
                     decoration rather than a confident wrong one"
                )),
            },
            None => build_notes.push("the git lane needs a base ref and none resolved".to_string()),
        }
    }

    // --- the decoration lanes ---------------------------------------------
    let mut facts: HashMap<String, Facts> = HashMap::new();
    let mut fact = |path: &str, f: &dyn Fn(&mut Facts)| {
        f(facts.entry(path.to_string()).or_default());
    };
    for lane in lanes {
        match lane.as_str() {
            "git" => {
                for (path, status) in &changes {
                    let s = status.clone();
                    fact(path, &|fx: &mut Facts| fx.git = Some(s.clone()));
                }
            }
            "review" => match review_id {
                Some(id) => {
                    let viewed: std::collections::HashSet<String> =
                        store.list_viewed(id)?.into_iter().map(|v| v.path).collect();
                    // Membership comes from the SAME change list the git
                    // lane uses when a base was given; without one the
                    // lane can only report what has been VIEWED, and says
                    // so rather than implying the file set.
                    if changes.is_empty() {
                        build_notes.push(
                            "the review lane has no base ref, so `unviewed` cannot be \
                             distinguished from `not in this review` — only viewed files \
                             are decorated"
                                .to_string(),
                        );
                        for p in &viewed {
                            fact(p, &|fx: &mut Facts| fx.review = Some("viewed".to_string()));
                        }
                    } else {
                        for (path, _) in &changes {
                            let v = viewed.contains(path);
                            fact(path, &move |fx: &mut Facts| {
                                fx.review = Some(if v { "viewed" } else { "unviewed" }.to_string())
                            });
                        }
                    }
                }
                None => build_notes.push(
                    "the `review` decoration lane needs `review=<id>` — no review was named, \
                     so nothing is decorated"
                        .to_string(),
                ),
            },
            "findings" => match review_id {
                Some(id) => {
                    for f in store.list_review_findings(id, None, false)? {
                        let sev = f.severity.clone();
                        let path = f.location_path.clone();
                        fact(&path, &|fx: &mut Facts| {
                            let better = match &fx.findings {
                                None => true,
                                Some(cur) => super::severity_rank(&sev) < super::severity_rank(cur),
                            };
                            if better {
                                fx.findings = Some(sev.clone());
                            }
                        });
                    }
                }
                None => build_notes.push(
                    "the `findings` decoration lane needs `review=<id>` — no review was \
                     named, so nothing is decorated"
                        .to_string(),
                ),
            },
            "annot" => {
                for (path, n) in store.open_annotation_counts_for_repo(repo_id)? {
                    if n > 0 {
                        let n = n as u32;
                        fact(&path, &move |fx: &mut Facts| fx.annot = Some(n));
                    }
                }
            }
            "todo" => {
                let mut counts: HashMap<String, u32> = HashMap::new();
                for t in store.list_todo_items(repo_id, None, None)? {
                    *counts.entry(t.path).or_default() += 1;
                }
                for (path, n) in counts {
                    fact(&path, &move |fx: &mut Facts| fx.todo = Some(n));
                }
            }
            "bookmark" => {
                let mut counts: HashMap<String, u32> = HashMap::new();
                for b in store.list_bookmarks(repo_name)? {
                    *counts.entry(b.path).or_default() += 1;
                }
                for (path, n) in counts {
                    fact(&path, &move |fx: &mut Facts| fx.bookmark = Some(n));
                }
            }
            // Unreachable while `resolve_lanes` and this match agree; a
            // lane that reached here decorates nothing rather than
            // silently decorating something else.
            other => build_notes.push(format!("decoration lane `{other}` has no builder")),
        }
    }

    Ok(Built {
        generation,
        paths,
        scope_applied,
        scope_normalized,
        scope_notes,
        scope_diagnostics,
        facts,
        entity_defs,
        changes,
        build_notes,
    })
}

/// Read the first CODEOWNERS file that exists, in GitHub's lookup order.
/// Text only, size-capped — the same shape `entities::zeitwerk`'s config
/// reader uses, and for the same reason (a bounded parse of a file the
/// repo, not this daemon, controls).
fn read_codeowners(repo_root: &std::path::Path) -> Option<(&'static str, String)> {
    for rel in CODEOWNERS_PATHS {
        let abs = match crate::security::paths::contained_abs_path(repo_root, rel) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let Ok(meta) = std::fs::metadata(&abs) else {
            continue;
        };
        if !meta.is_file() || meta.len() > MAX_CODEOWNERS_BYTES {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&abs) {
            return Some((rel, text));
        }
    }
    None
}
