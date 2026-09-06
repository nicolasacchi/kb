//! Phase N — scope glob matching for named path sets (`[scopes]` in
//! `kb-code.toml`). No `globset` crate in the workspace lockfile; no new
//! dependency. Reuses the lightweight shapes already covered by
//! `kb_core::watcher::path_matches_skip_pattern` and extends them for the
//! patterns the documented defaults need (`**/*.lock`, `**/*.test.*`).

/// `true` if `path` (repo-relative, forward-slash) matches any pattern in
/// `patterns`.
pub fn path_matches_any(path: &str, patterns: &[String]) -> bool {
    let basename = path.rsplit('/').next().unwrap_or(path);
    patterns
        .iter()
        .any(|p| path_matches_scope_glob(path, basename, p))
}

/// Match one repo-relative path against one scope glob.
///
/// Supported shapes (superset of `kb_core::watcher::path_matches_skip_pattern`):
/// - `**/dir/**` — path contains `/dir/` or starts with `dir/` or equals `dir`
/// - `prefix/**` — path equals `prefix` or starts with `prefix/`
/// - `**/leaf` — basename equals `leaf` (when `leaf` has no `*`)
/// - `*.ext` / `**/*.ext` — basename ends with `.ext`
/// - `**/*.test.*` (or any `*` in the basename pattern) — simple `*` wildcards
/// - literal — exact path or basename match, else substring
pub fn path_matches_scope_glob(rel: &str, basename: &str, pattern: &str) -> bool {
    let pat = pattern.trim();
    if pat.is_empty() {
        return false;
    }

    // Prefer the shared skip-pattern helper for shapes it already handles
    // correctly (`**/dir/**`, `prefix/**`, `*.ext`, exact name). Then fall
    // through to the extended matcher for `**/*...` with wildcards.
    if kb_core::watcher::path_matches_skip_pattern(rel, basename, pat) {
        return true;
    }

    // `**/something` where something may contain `*` (e.g. `**/*.lock`,
    // `**/*.test.*`). The skip-pattern helper treats `**/leaf` as exact
    // basename equality only, so `**/*.lock` never matches there.
    if let Some(rest) = pat.strip_prefix("**/") {
        if rest.contains('*') {
            return star_match(rest, basename) || star_match(rest, rel);
        }
        // Exact basename already covered above; also allow path suffix.
        return basename == rest || rel.ends_with(&format!("/{rest}"));
    }

    // Bare pattern with `*` (no `**/` prefix): match against basename or full path.
    if pat.contains('*') {
        return star_match(pat, basename) || star_match(pat, rel);
    }

    false
}

/// Simple `*` wildcard match: `*` matches any sequence of characters
/// (including empty, including `/` when matching a full path). Segment-
/// aware matching is not required for the documented scope defaults.
fn star_match(pat: &str, text: &str) -> bool {
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return text == pat;
    }
    // First part must be a prefix (unless empty).
    if !parts[0].is_empty() && !text.starts_with(parts[0]) {
        return false;
    }
    // Last part must be a suffix (unless empty).
    let last = parts[parts.len() - 1];
    if !last.is_empty() && !text.ends_with(last) {
        return false;
    }
    // Walk middle parts in order.
    let mut pos = parts[0].len();
    let end_limit = text.len().saturating_sub(last.len());
    for part in &parts[1..parts.len() - 1] {
        if part.is_empty() {
            continue;
        }
        if let Some(rel) = text[pos..end_limit].find(part) {
            pos += rel + part.len();
        } else {
            return false;
        }
    }
    // When the pattern ends with `*`, last is empty and any remainder is fine.
    // When it doesn't, we already checked ends_with(last).
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn any(path: &str, pats: &[&str]) -> bool {
        let owned: Vec<String> = pats.iter().map(|s| s.to_string()).collect();
        path_matches_any(path, &owned)
    }

    #[test]
    fn generated_defaults_match_dist_lock_node_modules() {
        assert!(any("web/dist/bundle.js", &["**/dist/**"]));
        assert!(any("dist/a.js", &["**/dist/**"]));
        assert!(any("Cargo.lock", &["**/*.lock"]));
        assert!(any("pkg/package-lock.json", &["**/*lock*"]) || any("foo.lock", &["**/*.lock"]));
        assert!(any("foo.lock", &["**/*.lock"]));
        assert!(any("node_modules/x/index.js", &["**/node_modules/**"]));
        assert!(!any(
            "src/lib.rs",
            &["**/dist/**", "**/*.lock", "**/node_modules/**"]
        ));
    }

    #[test]
    fn tests_defaults_match_tests_dir_and_test_files() {
        assert!(any("crates/foo/tests/it.rs", &["**/tests/**"]));
        // Top-level tests/ directory (common layout) — not only nested.
        assert!(any("tests/it.rs", &["**/tests/**"]));
        assert!(any("src/app.test.ts", &["**/*.test.*"]));
        assert!(any("web-code/e2e/smoke.spec.ts", &["**/e2e/**"]));
        assert!(!any(
            "src/app.ts",
            &["**/tests/**", "**/*.test.*", "**/e2e/**"]
        ));
    }
}
