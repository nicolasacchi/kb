//! Repo-versioned recipes and trust-on-first-use (V74-L3a, D11).
//!
//! `.kbc/recipes/*.toml` is a **read-only human-authored input**. Three
//! rules follow, and each of them is the reason for one function here.
//!
//! 1. **Only from the repo's DEFAULT ref, only through the ODB.** Never
//!    the working tree, and never `HEAD` on whatever branch happens to be
//!    checked out. A recipe is something the team agreed to; a file on a
//!    feature branch (or an uncommitted edit) has agreed to nothing, and
//!    a reader who trusted one on branch A would silently be running a
//!    different question on branch B.
//! 2. **Trust on first use, keyed on the CONTENT.** The op set is closed
//!    and structurally cannot reach an exec lane (D21, invariant 10), so
//!    a hostile recipe cannot run a process — but it can cost a great
//!    deal of IO and it can point a reader at the wrong evidence. First
//!    sight is [`TrustState::Untrusted`]; a changed hash is
//!    [`TrustState::Changed`] **with a diff**, because "trust this
//!    again?" without showing what moved is a prompt nobody can answer.
//!    This is `[lanes]`' own `.vscode/tasks.json` posture (invariant
//!    21(a)) one layer up.
//! 3. **The daemon never writes into the tree.** `recipe new --from-json
//!    -` writes a `recipes_server` row; a repo file WINS on a slug
//!    collision, and the shadowed server row is REPORTED rather than
//!    dropped.

use super::{Home, LoadedRecipe, TrustState};
use crate::git::GitRepo;
use crate::store::Store;

/// Where a repo keeps its recipes. A fixed path, not configurable: a
/// config key naming a different directory would be one more thing a
/// repo could influence about how this daemon reads it.
pub const RECIPE_DIR: &str = ".kbc/recipes";

/// Per-file cap. A recipe is a page of TOML; anything larger is a
/// mistake, and reading it would be paying ODB cost for a file this
/// loader is going to refuse anyway.
pub const MAX_RECIPE_BYTES: u64 = 64 * 1024;

/// Files read per repo per catalog build.
pub const MAX_RECIPE_FILES: usize = 64;

/// One `.kbc/recipes/*.toml` as it was found, before trust is applied.
#[derive(Debug, Clone)]
pub struct RepoFile {
    pub slug: String,
    pub path: String,
    /// The git blob oid — a content address, which is exactly what TOFU
    /// wants to key on.
    pub blob: String,
    pub text: String,
}

/// A file that could not be loaded. REPORTED, never silently skipped:
/// an author who wrote a broken recipe must see why, and a catalog that
/// quietly omits it is the dead-surface defect.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LoadProblem {
    pub path: String,
    pub message: String,
}

/// Read every recipe file at the repo's default ref. Sync — runs inside
/// the caller's one blocking hop.
pub fn read_repo_files(repo_root: &std::path::Path) -> (Vec<RepoFile>, Vec<LoadProblem>) {
    let mut problems = Vec::new();
    let Ok(git) = GitRepo::open(repo_root) else {
        return (Vec::new(), problems);
    };
    let Some(rev) = git.default_branch() else {
        // No default branch (a bare init, a detached HEAD with no
        // origin/HEAD): there is no ref this loader is willing to read
        // from, and it says so rather than falling back to HEAD.
        problems.push(LoadProblem {
            path: RECIPE_DIR.to_string(),
            message: "this repo has no resolvable default branch; repo-versioned recipes are \
                      read ONLY from the default ref"
                .into(),
        });
        return (Vec::new(), problems);
    };
    let entries = match git.list_tree(&rev, RECIPE_DIR) {
        Ok(e) => e,
        // A repo with no `.kbc/recipes` is the ordinary case, not a
        // problem worth reporting.
        Err(_) => return (Vec::new(), problems),
    };
    let mut out = Vec::new();
    for e in entries.into_iter().take(MAX_RECIPE_FILES) {
        if !matches!(e.kind, crate::git::EntryKind::File) {
            continue;
        }
        let Some(slug) = e.name.strip_suffix(".toml") else {
            continue;
        };
        let path = format!("{RECIPE_DIR}/{}", e.name);
        if e.size.unwrap_or(0) > MAX_RECIPE_BYTES {
            problems.push(LoadProblem {
                path: path.clone(),
                message: format!(
                    "{} bytes exceeds the {MAX_RECIPE_BYTES}-byte per-recipe cap",
                    e.size.unwrap_or(0)
                ),
            });
            continue;
        }
        let bytes = match git.read_blob(&rev, &path, MAX_RECIPE_BYTES) {
            Ok(b) => b,
            Err(err) => {
                problems.push(LoadProblem {
                    path: path.clone(),
                    message: err.to_string(),
                });
                continue;
            }
        };
        let Ok(text) = String::from_utf8(bytes) else {
            problems.push(LoadProblem {
                path: path.clone(),
                message: "not valid UTF-8".into(),
            });
            continue;
        };
        out.push(RepoFile {
            slug: slug.to_string(),
            path,
            blob: e.oid,
            text,
        });
    }
    (out, problems)
}

/// Apply trust-on-first-use to one file. `stored` is the
/// `(content_hash, trusted_body)` this daemon recorded, if any.
pub fn trust_for(file: &RepoFile, stored: Option<(&str, &str)>) -> (TrustState, Option<String>) {
    match stored {
        None => (TrustState::Untrusted, None),
        Some((hash, _)) if hash == file.blob => (TrustState::Trusted, None),
        Some((_, body)) => (
            TrustState::Changed,
            Some(unified_diff(body, &file.text, &file.path)),
        ),
    }
}

/// Build the whole catalog for one repo: the builtins, the server rows,
/// and the repo files — with the collision rule applied and REPORTED.
pub fn catalog(
    store: &Store,
    repo_id: i64,
    repo_name: &str,
    repo_root: &std::path::Path,
) -> (Vec<LoadedRecipe>, Vec<LoadProblem>) {
    let mut by_slug: std::collections::BTreeMap<String, LoadedRecipe> =
        std::collections::BTreeMap::new();
    for b in super::builtins::all() {
        by_slug.insert(b.doc.slug.clone(), b);
    }

    let mut problems = Vec::new();

    // Server rows shadow a builtin of the same slug — an operator's own
    // saved recipe is more specific than a shipped one.
    match store.list_recipes_server(Some(repo_name)) {
        Ok(rows) => {
            for row in rows {
                match serde_json::from_str::<super::RecipeDoc>(&row.body_json)
                    .map_err(|e| e.to_string())
                    .and_then(|d| super::load(d).map_err(|e| e.to_string()))
                {
                    Ok(doc) => {
                        by_slug.insert(
                            doc.slug.clone(),
                            LoadedRecipe {
                                doc,
                                home: Home::Server,
                                source: "server".into(),
                                trust: TrustState::Trusted,
                                trust_diff: None,
                                shadowed_by: None,
                            },
                        );
                    }
                    Err(e) => problems.push(LoadProblem {
                        path: format!("server:{}", row.slug),
                        message: e,
                    }),
                }
            }
        }
        Err(e) => problems.push(LoadProblem {
            path: "server".into(),
            message: e.to_string(),
        }),
    }

    let (files, mut file_problems) = read_repo_files(repo_root);
    problems.append(&mut file_problems);
    for f in files {
        let stored = store.get_recipe_trust(repo_id, &f.slug).ok().flatten();
        let (trust, diff) = trust_for(
            &f,
            stored
                .as_ref()
                .map(|r| (r.content_hash.as_str(), r.trusted_body.as_str())),
        );
        match super::load_toml(&f.text) {
            Ok(doc) => {
                if doc.slug != f.slug {
                    problems.push(LoadProblem {
                        path: f.path.clone(),
                        message: format!(
                            "declares slug {:?} but the file is named {:?}.toml; the FILE NAME \
                             is the slug",
                            doc.slug, f.slug
                        ),
                    });
                    continue;
                }
                // A repo file WINS on a slug collision, and what it
                // shadowed is reported rather than dropped (D11).
                let shadowed = by_slug.get(&doc.slug).map(|r| r.home);
                by_slug.insert(
                    doc.slug.clone(),
                    LoadedRecipe {
                        doc,
                        home: Home::Repo,
                        source: format!("repo:{}@{}", f.path, f.blob),
                        trust,
                        trust_diff: diff,
                        shadowed_by: shadowed,
                    },
                );
            }
            Err(e) => problems.push(LoadProblem {
                path: f.path.clone(),
                message: e.to_string(),
            }),
        }
    }
    (by_slug.into_values().collect(), problems)
}

/// A minimal unified diff, enough to answer "what moved?" on a page of
/// TOML. Deliberately not a general diff engine: this crate already has
/// one for patchsets (`history`/`interdiff`) that operates on git
/// ranges, and neither belongs in the other's job.
pub fn unified_diff(old: &str, new: &str, path: &str) -> String {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    // Longest common subsequence over lines — a page of TOML is small
    // enough that the O(n·m) table is free, and a heuristic diff would
    // make "what changed?" a guess.
    let n = a.len();
    let m = b.len();
    let mut table = vec![0usize; (n + 1) * (m + 1)];
    let idx = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[idx(i, j)] = if a[i] == b[j] {
                table[idx(i + 1, j + 1)] + 1
            } else {
                table[idx(i + 1, j)].max(table[idx(i, j + 1)])
            };
        }
    }
    let mut out = format!("--- trusted:{path}\n+++ current:{path}\n");
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if a[i] == b[j] {
            out.push_str(&format!(" {}\n", a[i]));
            i += 1;
            j += 1;
        } else if table[idx(i + 1, j)] >= table[idx(i, j + 1)] {
            out.push_str(&format!("-{}\n", a[i]));
            i += 1;
        } else {
            out.push_str(&format!("+{}\n", b[j]));
            j += 1;
        }
    }
    for line in a.iter().skip(i) {
        out.push_str(&format!("-{line}\n"));
    }
    for line in b.iter().skip(j) {
        out.push_str(&format!("+{line}\n"));
    }
    out
}
