//! W3.A — the `[projects.*]` config registry (sessions-rethink synthesis memo
//! R12 / designs/projects.md P3): a READ-TIME relabel/merge layer over the
//! pure, registry-independent `derive_project` ladder (`sessions.rs`'s
//! `derive_project`). Mirrors the `CorpusMount` precedent verbatim
//! (`sessions.rs`'s `set_corpus_mounts`/`corpus_mounts`/`resolve_corpus_path`,
//! installed from config at `kb-server::lib.rs::install_corpus_mounts`):
//! **derivation never touches the registry** (a `kb reindex` reproduces
//! identical `project_key`/`repo_root` rows from capture bytes alone with
//! zero config), and **the registry never touches derivation** (a config
//! change takes effect on the very next request — no reindex, ever).
//!
//! Two lookups this module offers:
//! - [`resolve_registry_project`] — "which declared project does this
//!   session's repo_root/cwd belong to" (used by the `/api/sessions/projects`
//!   facet route to relabel/merge derived rows under a registry entry).
//! - [`resolve_project_filter`] — "the caller passed `?project=<id-or-key>`;
//!   what sqlite predicate answers it" (used by every `project=`-scoped read:
//!   list/recollect/research-rollup/funnel).

use std::sync::{Arc, RwLock};

/// One declared `[projects.<id>]` entry (designs/projects.md P3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDef {
    /// The TOML key (`[projects.kb]` → `"kb"`) — stable, operator-chosen.
    pub id: String,
    pub label: String,
    /// Absolute path prefixes. Every root matches its own subtree (a
    /// declared root ALWAYS covers everything under it — a bare prefix and a
    /// `/*`-suffixed "glob" root are handled identically here: the trailing
    /// `/*` is accepted on input and stripped at install time, since
    /// prefix-matching a directory already covers every path beneath it).
    /// Simplification vs. the design's literal glob language — documented in
    /// the W3 build report.
    pub roots: Vec<String>,
    pub kb: Option<String>,
    pub code_url: Option<String>,
    pub code_repo: Option<String>,
}

/// Process-global project registry, installed once at daemon startup (and
/// re-installed on a config-reload restart, invariant #13) from
/// `[projects.*]`. Empty by default — a fresh daemon / test resolves nothing,
/// which degrades to auto-projects everywhere (P1's "empty-registry cold
/// start" — zero config still gives a useful projects home via basenames).
static PROJECT_REGISTRY: RwLock<Option<Arc<Vec<ProjectDef>>>> = RwLock::new(None);

/// Install the daemon's project registry. Order is preserved from the input
/// (the caller sorts — kb-server installs in config-key (BTreeMap) order,
/// the same deterministic-but-not-literally-declaration-order convention
/// `[kb.*]` already uses) — first match wins on ambiguity.
pub fn set_project_registry(defs: Vec<ProjectDef>) {
    let normalized = defs
        .into_iter()
        .map(|mut d| {
            d.roots = d.roots.iter().map(|r| normalize_root(r)).collect();
            d
        })
        .collect();
    *PROJECT_REGISTRY.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(normalized));
}

/// Serializes tests that mutate the process-global registry (mirrors
/// `sessions::MOUNT_TEST_GUARD`).
#[cfg(test)]
pub(crate) static PROJECT_REGISTRY_TEST_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The current registry (cheap `Arc` clone). Empty when unset.
pub fn project_registry() -> Arc<Vec<ProjectDef>> {
    PROJECT_REGISTRY
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

/// Strip a trailing `/*` glob suffix (and any trailing slash), so every root
/// is stored as a bare absolute-path prefix.
fn normalize_root(root: &str) -> String {
    root.trim_end_matches("/*")
        .trim_end_matches('/')
        .to_string()
}

/// True when `candidate` is `root` itself or lives under it. Prefix-only
/// (see [`ProjectDef::roots`] doc — subtree coverage is inherent to a
/// directory prefix, so the "glob" distinction from the design doc collapses
/// to this one predicate).
fn root_matches(root: &str, candidate: &str) -> bool {
    !root.is_empty() && (candidate == root || candidate.starts_with(&format!("{root}/")))
}

/// The resolved identity for a session: either a declared registry entry, or
/// (caller-built) an auto-project. `source` is `"registry"` or `"derived"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProject {
    pub id: String,
    pub label: String,
    pub source: &'static str,
    pub kb: Option<String>,
    pub code_url: Option<String>,
    pub code_repo: Option<String>,
}

/// Resolve a session's registry membership from its `repo_root` (preferred —
/// a CONFIRMED git root) or `cwd` (the rung-3 fallback). First declared match
/// wins. `None` when nothing matched — the caller builds an auto-project
/// (`id = project_key`, `label = basename(repo_root_or_cwd)`).
pub fn resolve_registry_project(
    repo_root: Option<&str>,
    cwd: Option<&str>,
) -> Option<ResolvedProject> {
    let registry = project_registry();
    if registry.is_empty() {
        return None;
    }
    let candidate = repo_root
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| cwd.map(str::trim).filter(|s| !s.is_empty()))?;
    registry
        .iter()
        .find(|def| def.roots.iter().any(|r| root_matches(r, candidate)))
        .map(|def| ResolvedProject {
            id: def.id.clone(),
            label: def.label.clone(),
            source: "registry",
            kb: def.kb.clone(),
            code_url: def.code_url.clone(),
            code_repo: def.code_repo.clone(),
        })
}

/// Look up one declared registry entry by its id (`?project=<id>` resolution
/// — never matches an auto/derived project).
pub fn registry_entry(id: &str) -> Option<ProjectDef> {
    project_registry().iter().find(|d| d.id == id).cloned()
}

/// The `?project=` filter descriptor every project-scoped read resolves to
/// (designs/projects.md P4): `keys` matches `sessions.project_key` exactly
/// (`IN`); `root_prefixes` is the fallback for un-derived (`project_key IS
/// NULL`) rows, matched against `cwd` by prefix — mid-backfill correctness
/// (P2(d)): a row whose ladder hasn't run yet still surfaces under its
/// project once its cwd is under a declared root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectFilter {
    pub keys: Vec<String>,
    pub root_prefixes: Vec<String>,
}

impl ProjectFilter {
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.root_prefixes.is_empty()
    }

    /// True when `(project_key, cwd)` matches this filter — the Rust-side
    /// twin of the SQL predicate `sessions_list` builds, used by the
    /// non-cursor-paginated aggregate routes (`research_rollup`/`funnel`)
    /// that filter post-fetch rather than growing a dynamic WHERE.
    pub fn matches(&self, project_key: Option<&str>, cwd: Option<&str>) -> bool {
        if self.is_empty() {
            return true;
        }
        if let Some(k) = project_key {
            if self.keys.iter().any(|want| want == k) {
                return true;
            }
            // A NON-null project_key that didn't match `keys` still gets a
            // shot at the prefix fallback below IFF it came from cwd (rung
            // 3) rather than a confirmed repo_root — we don't have that
            // distinction here, so (matching `sessions_list`'s SQL) the
            // prefix fallback is scoped to `project_key IS NULL` only.
            return false;
        }
        cwd.map(|c| {
            self.root_prefixes
                .iter()
                .any(|p| c == p.as_str() || c.starts_with(&format!("{p}/")))
        })
        .unwrap_or(false)
    }
}

/// Resolve the raw `?project=` query value into a [`ProjectFilter`]. A value
/// matching a registered id expands to every declared root (as both an exact
/// `claude_project_slug` key — covers sessions whose ladder resolved straight
/// to that root — AND a prefix fallback for un-derived rows); anything else
/// is taken literally as a raw `project_key` (P1's derived key, e.g. what
/// `/api/sessions/projects` echoes back for an auto-project).
pub fn resolve_project_filter(param: &str) -> ProjectFilter {
    let param = param.trim();
    if param.is_empty() {
        return ProjectFilter::default();
    }
    if let Some(def) = registry_entry(param) {
        let mut keys = Vec::with_capacity(def.roots.len());
        let mut root_prefixes = Vec::with_capacity(def.roots.len());
        for root in &def.roots {
            keys.push(crate::session_bundle::claude_project_slug(root));
            root_prefixes.push(root.clone());
        }
        return ProjectFilter {
            keys,
            root_prefixes,
        };
    }
    ProjectFilter {
        keys: vec![param.to_string()],
        root_prefixes: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_registry<T>(defs: Vec<ProjectDef>, f: impl FnOnce() -> T) -> T {
        let _guard = PROJECT_REGISTRY_TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        set_project_registry(defs);
        let out = f();
        set_project_registry(Vec::new());
        out
    }

    fn kb_def() -> ProjectDef {
        ProjectDef {
            id: "kb".into(),
            label: "kb".into(),
            roots: vec![
                "/home/user/project/kb".into(),
                "/home/user/kb-worktrees/*".into(),
            ],
            kb: Some("kb".into()),
            code_url: Some("https://kbc.example.com".into()),
            code_repo: Some("kb".into()),
        }
    }

    #[test]
    fn normalize_root_strips_trailing_glob_and_slash() {
        assert_eq!(normalize_root("/a/b/*"), "/a/b");
        assert_eq!(normalize_root("/a/b/"), "/a/b");
        assert_eq!(normalize_root("/a/b"), "/a/b");
    }

    #[test]
    fn root_matches_exact_and_subtree_not_sibling() {
        assert!(root_matches("/a/b", "/a/b"));
        assert!(root_matches("/a/b", "/a/b/c"));
        assert!(!root_matches("/a/b", "/a/bc"));
        assert!(!root_matches("/a/b", "/a/c"));
    }

    #[test]
    fn resolve_registry_project_prefers_repo_root_over_cwd() {
        with_registry(vec![kb_def()], || {
            let r =
                resolve_registry_project(Some("/home/user/project/kb"), Some("/somewhere/else"))
                    .expect("matches");
            assert_eq!(r.id, "kb");
            assert_eq!(r.source, "registry");
            assert_eq!(r.kb.as_deref(), Some("kb"));
        });
    }

    #[test]
    fn resolve_registry_project_falls_back_to_cwd_and_matches_glob_root() {
        with_registry(vec![kb_def()], || {
            let r = resolve_registry_project(None, Some("/home/user/kb-worktrees/w1"))
                .expect("matches glob root");
            assert_eq!(r.id, "kb");
        });
    }

    #[test]
    fn resolve_registry_project_none_when_empty_registry() {
        with_registry(Vec::new(), || {
            assert!(resolve_registry_project(Some("/x"), Some("/x")).is_none());
        });
    }

    #[test]
    fn resolve_registry_project_none_on_no_match() {
        with_registry(vec![kb_def()], || {
            assert!(resolve_registry_project(Some("/other/repo"), Some("/other/repo")).is_none());
        });
    }

    #[test]
    fn resolve_project_filter_registry_id_expands_to_keys_and_prefixes() {
        with_registry(vec![kb_def()], || {
            let f = resolve_project_filter("kb");
            assert_eq!(f.keys.len(), 2);
            assert_eq!(f.root_prefixes.len(), 2);
            assert!(f
                .root_prefixes
                .contains(&"/home/user/project/kb".to_string()));
            assert!(f
                .root_prefixes
                .contains(&"/home/user/kb-worktrees".to_string()));
        });
    }

    #[test]
    fn resolve_project_filter_unknown_param_is_a_raw_project_key() {
        with_registry(Vec::new(), || {
            let f = resolve_project_filter("some-raw-key");
            assert_eq!(f.keys, vec!["some-raw-key".to_string()]);
            assert!(f.root_prefixes.is_empty());
        });
    }

    #[test]
    fn resolve_project_filter_empty_param_is_empty_filter() {
        let f = resolve_project_filter("   ");
        assert!(f.is_empty());
        assert!(f.matches(Some("anything"), Some("/x")));
    }

    #[test]
    fn project_filter_matches_key_or_null_key_prefix_fallback() {
        let f = ProjectFilter {
            keys: vec!["proj-a".into()],
            root_prefixes: vec!["/root/a".into()],
        };
        assert!(f.matches(Some("proj-a"), None));
        assert!(!f.matches(Some("proj-b"), Some("/root/a/sub")));
        assert!(f.matches(None, Some("/root/a/sub")));
        assert!(f.matches(None, Some("/root/a")));
        assert!(!f.matches(None, Some("/root/ax")));
        assert!(!f.matches(None, None));
    }
}
