//! V3.G2 — Sourcegraph-style filtered cross-file resolution.
//!
//! Given a querying file and a list of same-repo name-match candidates:
//! 1. FILTER: keep candidates whose defining file is import-reachable
//!    from the query file (direct `import_edges` row) OR same-directory
//!    (Sourcegraph same-package rule).
//! 2. FUZZY FALLBACK: if the filter empties a non-empty list, return the
//!    UNFILTERED list as `class=candidate` — never an empty result where
//!    names exist.
//! 3. CLASS: filter survivors → `likely`; non-survivors (when filter kept
//!    anything) → `candidate`. Exact stays reserved for scip/locals in
//!    v3.0 — even a unique filtered+qualified hit is only `likely`.
//! 4. RANK: same-file > same-dir > import-reachable > global.

use std::collections::HashSet;
use std::path::Path;

/// Provenance for a filtered cross-file hit (wire `precision`).
pub const PRECISION_IMPORT_FILTERED: &str = "import-filtered";
/// Unfiltered / fuzzy-fallback / non-survivor tags-tier hit.
pub const PRECISION_TAGS_APPROX: &str = "tags-approx";

/// How a candidate relates to the query file for ranking/class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reach {
    /// Same path as the query file (usually already handled by higher tiers).
    SameFile = 0,
    /// Same parent directory (Sourcegraph same-package).
    SameDir = 1,
    /// Direct import edge from query file → candidate file.
    ImportReachable = 2,
    /// No filter relationship.
    Global = 3,
}

impl Reach {
    pub fn class(self, filter_kept_any: bool, fuzzy_fallback: bool) -> &'static str {
        if fuzzy_fallback {
            return "candidate";
        }
        match self {
            Reach::SameFile | Reach::SameDir | Reach::ImportReachable => {
                if filter_kept_any {
                    "likely"
                } else {
                    "candidate"
                }
            }
            Reach::Global => "candidate",
        }
    }

    pub fn precision(self, fuzzy_fallback: bool) -> &'static str {
        if fuzzy_fallback {
            return PRECISION_TAGS_APPROX;
        }
        match self {
            Reach::SameFile | Reach::SameDir | Reach::ImportReachable => PRECISION_IMPORT_FILTERED,
            Reach::Global => PRECISION_TAGS_APPROX,
        }
    }
}

/// Classify reach of `cand_path` from `query_path` given the set of
/// import-reachable target paths (repo-relative).
pub fn classify_reach(
    query_path: &str,
    cand_path: &str,
    import_targets: &HashSet<String>,
) -> Reach {
    if cand_path == query_path {
        return Reach::SameFile;
    }
    if same_dir(query_path, cand_path) {
        return Reach::SameDir;
    }
    if import_targets.contains(cand_path) {
        return Reach::ImportReachable;
    }
    Reach::Global
}

fn same_dir(a: &str, b: &str) -> bool {
    let pa = Path::new(a).parent().unwrap_or_else(|| Path::new(""));
    let pb = Path::new(b).parent().unwrap_or_else(|| Path::new(""));
    pa == pb
}

/// Rank key: lower is better. `(reach, path, line)`.
pub fn rank_key(reach: Reach, path: &str, line: u32) -> (u8, String, u32) {
    (reach as u8, path.to_string(), line)
}

/// Apply the filter+fallback policy to a list of (path, …) candidates.
///
/// Returns `(tagged, fuzzy_fallback)` where each tagged item is
/// `(index_into_input, Reach, class, precision)`.
pub fn filter_and_tag(
    query_path: &str,
    cand_paths: &[String],
    import_targets: &HashSet<String>,
) -> (Vec<(usize, Reach, &'static str, &'static str)>, bool) {
    if cand_paths.is_empty() {
        return (Vec::new(), false);
    }
    let reaches: Vec<Reach> = cand_paths
        .iter()
        .map(|p| classify_reach(query_path, p, import_targets))
        .collect();
    let survivors: Vec<usize> = reaches
        .iter()
        .enumerate()
        .filter(|(_, r)| **r != Reach::Global)
        .map(|(i, _)| i)
        .collect();

    let fuzzy = survivors.is_empty();
    let filter_kept_any = !survivors.is_empty();

    let mut tagged: Vec<(usize, Reach, &'static str, &'static str)> = reaches
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            let class = r.class(filter_kept_any, fuzzy);
            let precision = r.precision(fuzzy);
            (i, r, class, precision)
        })
        .collect();

    // Rank: same-file > same-dir > import > global, then path, then we
    // leave line to the caller. Stable by original index as final tiebreak.
    tagged.sort_by(|a, b| {
        (a.1 as u8)
            .cmp(&(b.1 as u8))
            .then_with(|| cand_paths[a.0].cmp(&cand_paths[b.0]))
            .then_with(|| a.0.cmp(&b.0))
    });

    (tagged, fuzzy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_dir_passes_filter() {
        let mut targets = HashSet::new();
        targets.insert("src/other.rs".into());
        assert_eq!(
            classify_reach("src/a.rs", "src/b.rs", &targets),
            Reach::SameDir
        );
    }

    #[test]
    fn import_reachable() {
        let mut targets = HashSet::new();
        targets.insert("lib/util.ts".into());
        assert_eq!(
            classify_reach("app/main.ts", "lib/util.ts", &targets),
            Reach::ImportReachable
        );
    }

    #[test]
    fn fuzzy_fallback_when_filter_empties() {
        let paths = vec!["other/pkg.rs".into(), "far/away.rs".into()];
        let targets = HashSet::new();
        let (tagged, fuzzy) = filter_and_tag("src/main.rs", &paths, &targets);
        assert!(fuzzy);
        assert_eq!(tagged.len(), 2);
        assert!(tagged.iter().all(|t| t.2 == "candidate"));
    }

    #[test]
    fn survivors_likely_non_survivors_candidate() {
        let paths = vec![
            "src/util.rs".into(), // same dir
            "far/away.rs".into(), // global
        ];
        let targets = HashSet::new();
        let (tagged, fuzzy) = filter_and_tag("src/main.rs", &paths, &targets);
        assert!(!fuzzy);
        let by_path: std::collections::HashMap<_, _> = tagged
            .iter()
            .map(|(i, _, class, _)| (paths[*i].as_str(), *class))
            .collect();
        assert_eq!(by_path["src/util.rs"], "likely");
        assert_eq!(by_path["far/away.rs"], "candidate");
    }
}
