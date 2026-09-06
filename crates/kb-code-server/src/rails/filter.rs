//! Resolving `kbcq/1`'s Rails facet atoms (`model:`, `controller:`,
//! `action:`, `route:`, `job:`, `rails:<noun>`) to a FILE SET.
//!
//! Deliberately NOT [`super::build_index`]. The search box asks this
//! question on a keystroke, so a filter resolves only what its own noun
//! needs — at most two store reads and NO file reads — where the full join
//! reads four tables and every controller source. The two agree on what a
//! noun is because both read the same conventions from
//! [`super`](super#path-conventions); they differ only in how much
//! evidence they carry back, and this one carries none: a filter answers
//! "which files", not "with what confidence". Confidence lives on
//! `/api/rails/*`, where there is a row to hang it on.
//!
//! Refuse-don't-guess, twice over. A repo that is not a Rails application,
//! and a request with more than one repo in scope (where two repos'
//! same-named paths would alias), both leave the atom UNAPPLIED with a
//! stated reason rather than silently matching the wrong thing — the same
//! posture `kbc-scope/1` takes when a scope will not resolve.

use crate::frameworks::EdgeKind;
use crate::store::Store;
use std::collections::HashSet;
use std::path::Path;

/// What a request's Rails atoms resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// The nouns named, in the grammar's declaration order.
    pub nouns: Vec<&'static str>,
    /// `true` when the file set below was actually applied to the lanes.
    pub applied: bool,
    /// Why it was not applied, or a note about what it matched.
    pub reason: Option<String>,
    pub paths: HashSet<String>,
}

impl Selection {
    /// The lane caption: what narrowed this section, in the caller's own
    /// words plus the honest count.
    pub fn caption(&self) -> String {
        let nouns = self.nouns.join(", ");
        match (self.applied, &self.reason) {
            (true, _) => format!(
                "narrowed by rails/1 {nouns} — {} file{} matched",
                self.paths.len(),
                if self.paths.len() == 1 { "" } else { "s" }
            ),
            (false, Some(r)) => format!("rails/1 {nouns} not applied: {r}"),
            (false, None) => format!("rails/1 {nouns} not applied"),
        }
    }
}

fn matches(needle: &Option<String>, hay: &str) -> bool {
    match needle {
        None => true,
        Some(n) => hay.to_lowercase().contains(&n.to_lowercase()),
    }
}

/// Resolve every atom against ONE repo, UNIONing the per-noun file sets.
///
/// `atoms` comes from `grammar::Filters::rails_atoms` — `(noun, value)`,
/// where `None` is the generic `rails:<noun>` form (the whole noun).
pub fn resolve(
    store: &Store,
    repo_root: &Path,
    repo_id: i64,
    atoms: &[(&'static str, Option<String>)],
) -> Selection {
    let nouns: Vec<&'static str> = atoms.iter().map(|(n, _)| *n).collect();
    if !crate::frameworks::rails::detect_is_rails(repo_root) {
        return Selection {
            nouns,
            applied: false,
            reason: Some(
                "this repo is not a Rails application (config/routes.rb and a Gemfile \
                 declaring gem \"rails\" are both required), so the atom was ignored rather \
                 than matched against a tree it does not describe"
                    .to_string(),
            ),
            paths: HashSet::new(),
        };
    }

    let mut paths: HashSet<String> = HashSet::new();
    let mut failed: Option<String> = None;
    for (noun, value) in atoms {
        match resolve_one(store, repo_id, noun, value) {
            Ok(set) => paths.extend(set),
            Err(e) => failed = Some(e),
        }
    }
    if let Some(reason) = failed {
        return Selection {
            nouns,
            applied: false,
            reason: Some(reason),
            paths: HashSet::new(),
        };
    }
    Selection {
        nouns,
        applied: true,
        reason: None,
        paths,
    }
}

fn resolve_one(
    store: &Store,
    repo_id: i64,
    noun: &str,
    value: &Option<String>,
) -> Result<HashSet<String>, String> {
    let mut out = HashSet::new();
    match noun {
        "model" | "controller" | "job" | "mailer" | "concern" => {
            let rows = store
                .entity_defs_for_repo(repo_id, None, super::MAX_ENTITY_DEFS)
                .map_err(|e| e.to_string())?;
            for r in rows {
                if super::noun_for_path(&r.path) != Some(noun) {
                    continue;
                }
                if matches(value, &r.fqn) {
                    out.insert(r.path);
                }
            }
        }
        "view" => {
            let files = store.list_files(repo_id).map_err(|e| e.to_string())?;
            for f in files {
                let Some(rel) = f.path.strip_prefix(super::VIEW_DIR) else {
                    continue;
                };
                if matches(value, rel) {
                    out.insert(f.path);
                }
            }
        }
        "action" | "route" => {
            let edges = store
                .rails_edges_by_kind(repo_id, EdgeKind::RouteAction.as_str())
                .map_err(|e| e.to_string())?;
            for e in edges {
                let target = e.dst_symbol.clone().unwrap_or_default();
                let hit = if noun == "action" {
                    matches(value, &target)
                } else {
                    // A route is addressable BOTH ways: by the verb+path
                    // the extractor reconstructed and by the
                    // `controller#action` it dispatches to — a reader who
                    // knows either one can find it.
                    let (verb, path) = super::parse_route_addr(e.extra_json.as_deref());
                    let address = match (verb, path) {
                        (Some(v), Some(p)) => format!("{v} {p}"),
                        _ => String::new(),
                    };
                    matches(value, &target) || (!address.is_empty() && matches(value, &address))
                };
                if !hit {
                    continue;
                }
                // Both ends of the edge are where this route "lives": the
                // routes file that declares it and the controller that
                // answers it.
                if let Some(dst) = e.dst_path.clone() {
                    out.insert(dst);
                }
                if noun == "route" {
                    out.insert(e.src_path.clone());
                }
            }
        }
        other => return Err(format!("unknown Rails noun {other:?}")),
    }
    Ok(out)
}
