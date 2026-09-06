//! Persisted anchor-stale tracker — the sidecar that lets the indexer
//! still emit `comment.anchor_resolved` for a comment that went stale
//! in a *previous* daemon process.
//!
//! v0.5 P4 introduced an in-process `HashSet` of stale comment ids; it
//! died on restart, so the first reindex after a restart could never
//! fire `comment.anchor_resolved` (nothing was "previously stale").
//! This sidecar closes that gap.
//!
//! One file per kb at `<state>/<kb>/.anchors-stale.json`, beside the
//! `.review/` directory. Holds the `(artifact_id, comment_id)` pairs
//! whose last anchor re-resolve returned `Stale` — the kb is implicit
//! in the path. Written atomically (tmpfile + rename + parent fsync)
//! via the same primitive as kb-comments review files (see invariant
//! #9 + `fsx::write_atomic`).
//!
//! v0.7.1 P2 — keyed on `(artifact_id, comment_id)`, not the bare
//! `comment_id` of v0.7. The SPA's comment ids aren't guaranteed unique
//! across artifacts within a kb, so the bare-id key let one artifact's
//! resolve clear a *different* artifact's stale flag. Schema bumped to
//! 2; a v1 sidecar loads as empty — the set rebuilds on the next
//! reindex (the sidecar is safe to lose).
//!
//! v0.8 #4 — schema v3 adds `anchor_kind` + `fuzzy_score` per entry so
//! the SPA's cold-load endpoint can surface fuzzy-match strength after
//! a page reload (pre-v3 the cold load fell back to "stale" + 0,
//! losing information the live SSE event carried). v2 sidecars load
//! cleanly into v3 entries with default metadata; older v1 still
//! loads as empty (forces rebuild on next reindex).

use crate::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const FILE_NAME: &str = ".anchors-stale.json";
const SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Serialize, Deserialize)]
struct StaleAnchorsFile {
    version: u32,
    stale: Vec<StaleAnchor>,
}

/// One stale-anchor record: which `comment` on which `artifact`, plus
/// the v3 metadata (`anchor_kind`, `fuzzy_score`) captured from the
/// indexer's `fuzzy_resolve_anchor` pass.
#[derive(Debug, Serialize, Deserialize)]
struct StaleAnchor {
    artifact: String,
    comment: String,
    #[serde(default = "default_anchor_kind")]
    anchor_kind: String,
    #[serde(default)]
    fuzzy_score: f32,
}

fn default_anchor_kind() -> String {
    "stale".to_string()
}

/// In-memory shape of one persisted stale anchor. Mirror of the
/// disk struct; the public `load` API hands these out so callers
/// can read the metadata directly (the v2 era handed back bare
/// tuples).
#[derive(Debug, Clone, PartialEq)]
pub struct StaleAnchorEntry {
    pub anchor_kind: String,
    pub fuzzy_score: f32,
}

/// Sidecar path for a kb whose review directory is `review_dir`
/// (`<state>/<kb>/.review`). The sidecar sits beside it at
/// `<state>/<kb>/.anchors-stale.json`.
pub fn sidecar_path(review_dir: &Path) -> PathBuf {
    // `review_dir` is always `<state>/<kb>/.review` — a parent is
    // guaranteed. The `unwrap_or` keeps a root-path misconfig from
    // panicking in release; the assert surfaces it loudly in dev/tests
    // rather than silently parking the sidecar inside `.review/`.
    debug_assert!(
        review_dir.parent().is_some(),
        "review_dir must have a parent: {review_dir:?}"
    );
    review_dir.parent().unwrap_or(review_dir).join(FILE_NAME)
}

/// Load the persisted stale anchor map. Keyed on
/// `(artifact_id, comment_id)`; value carries the anchor_kind +
/// fuzzy_score captured at the most recent stale transition.
///
/// A missing file is the steady state (no comment has gone stale yet)
/// → empty map. A v2 sidecar loads cleanly (anchor_kind defaults to
/// "stale", fuzzy_score to 0) so a daemon upgrading from v0.7.1 sees
/// every persisted entry — only the metadata is fresh from this point
/// on. A v1 sidecar (bare comment-id strings, not objects) or a
/// corrupt file loads as empty with a `warn` log; a bad sidecar must
/// never wedge the indexer.
pub fn load(path: &Path) -> HashMap<(String, String), StaleAnchorEntry> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return HashMap::new(),
    };
    match serde_json::from_slice::<StaleAnchorsFile>(&bytes) {
        // v2 + v3 share a forwards-compatible object shape — v2
        // entries lack anchor_kind/fuzzy_score, which
        // `#[serde(default)]` fills in. v1 stored bare strings and
        // doesn't match StaleAnchor's struct shape; serde rejects it
        // at parse time and we land in the `Err` arm.
        Ok(f) if f.version == SCHEMA_VERSION || f.version == 2 => f
            .stale
            .into_iter()
            .map(|s| {
                (
                    (s.artifact, s.comment),
                    StaleAnchorEntry {
                        anchor_kind: s.anchor_kind,
                        fuzzy_score: s.fuzzy_score,
                    },
                )
            })
            .collect(),
        Ok(f) => {
            tracing::warn!(
                path = %path.display(),
                version = f.version,
                "unknown .anchors-stale.json schema version; starting empty"
            );
            HashMap::new()
        }
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "corrupt .anchors-stale.json; starting empty"
            );
            HashMap::new()
        }
    }
}

/// Persist the stale anchor map atomically. Entries are sorted by
/// `(artifact, comment)` so the on-disk form is stable (no spurious
/// diffs / etag churn).
pub fn save(path: &Path, stale: &HashMap<(String, String), StaleAnchorEntry>) -> Result<()> {
    let mut entries: Vec<((String, String), StaleAnchorEntry)> =
        stale.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let file = StaleAnchorsFile {
        version: SCHEMA_VERSION,
        stale: entries
            .into_iter()
            .map(|((artifact, comment), meta)| StaleAnchor {
                artifact,
                comment,
                anchor_kind: meta.anchor_kind,
                fuzzy_score: meta.fuzzy_score,
            })
            .collect(),
    };
    let json = serde_json::to_vec_pretty(&file)?;
    crate::fsx::write_atomic(path, &json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_sits_beside_the_review_dir() {
        let p = sidecar_path(Path::new("/state/mykb/.review"));
        assert_eq!(p, Path::new("/state/mykb/.anchors-stale.json"));
    }

    #[test]
    fn missing_file_loads_as_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".anchors-stale.json");
        assert!(load(&path).is_empty());
    }

    fn entry(kind: &str, score: f32) -> StaleAnchorEntry {
        StaleAnchorEntry {
            anchor_kind: kind.to_string(),
            fuzzy_score: score,
        }
    }

    #[test]
    fn save_then_load_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".anchors-stale.json");
        let mut set: HashMap<(String, String), StaleAnchorEntry> = HashMap::new();
        set.insert(
            ("artifact-a".to_string(), "c_one".to_string()),
            entry("stale", 0.0),
        );
        set.insert(
            ("artifact-b".to_string(), "c_two".to_string()),
            entry("fuzzy", 0.62),
        );
        save(&path, &set).unwrap();
        let loaded = load(&path);
        assert_eq!(loaded, set);
        // Metadata survives the round-trip — `anchor_kind` and the
        // exact f32 bits of `fuzzy_score` (no rounding through serde
        // since the value is finite + within representable range).
        assert_eq!(
            loaded
                .get(&("artifact-b".to_string(), "c_two".to_string()))
                .unwrap()
                .fuzzy_score,
            0.62
        );
    }

    #[test]
    fn distinct_artifacts_keep_a_colliding_comment_id_separate() {
        // v0.7.1 P2 — the same comment id on two artifacts must be two
        // independent entries (the bare-comment-id key conflated them,
        // so resolving one cleared the other's stale flag).
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".anchors-stale.json");
        let mut set: HashMap<(String, String), StaleAnchorEntry> = HashMap::new();
        set.insert(
            ("artifact-a".to_string(), "c_1".to_string()),
            entry("stale", 0.0),
        );
        set.insert(
            ("artifact-b".to_string(), "c_1".to_string()),
            entry("stale", 0.0),
        );
        save(&path, &set).unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.len(), 2);
        assert!(loaded.contains_key(&("artifact-a".to_string(), "c_1".to_string())));
        assert!(loaded.contains_key(&("artifact-b".to_string(), "c_1".to_string())));
    }

    #[test]
    fn corrupt_file_loads_as_empty_without_panicking() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".anchors-stale.json");
        std::fs::write(&path, b"{not valid json").unwrap();
        assert!(load(&path).is_empty());
        // The next save must still succeed, overwriting the garbage.
        let mut set: HashMap<(String, String), StaleAnchorEntry> = HashMap::new();
        set.insert(
            ("artifact-a".to_string(), "c_recovered".to_string()),
            entry("stale", 0.0),
        );
        save(&path, &set).unwrap();
        assert_eq!(load(&path), set);
    }

    #[test]
    fn v2_sidecar_loads_with_default_metadata() {
        // #4 — schema v2 written by a previous daemon process must
        // load cleanly under v3 with anchor_kind defaulting to "stale"
        // and fuzzy_score to 0. Pre-fix the version-equality check
        // would have rejected v2 outright, wiping the persisted set.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".anchors-stale.json");
        std::fs::write(
            &path,
            br#"{"version": 2, "stale": [{"artifact": "art-a", "comment": "c_1"}]}"#,
        )
        .unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.len(), 1);
        let got = loaded
            .get(&("art-a".to_string(), "c_1".to_string()))
            .expect("entry present");
        assert_eq!(got.anchor_kind, "stale");
        assert_eq!(got.fuzzy_score, 0.0);
    }

    #[test]
    fn unknown_schema_version_loads_as_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".anchors-stale.json");
        std::fs::write(
            &path,
            br#"{"version": 999, "stale": [{"artifact": "a", "comment": "c_x"}]}"#,
        )
        .unwrap();
        assert!(load(&path).is_empty());
    }

    #[test]
    fn v3_metadata_persists_anchor_kind_and_fuzzy_score() {
        // #4 — direct schema v3 round-trip. fuzzy_score values close
        // to the daemon's accept threshold are the operationally
        // useful range; pin them through serde to catch any future
        // precision-loss regression.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".anchors-stale.json");
        let mut set: HashMap<(String, String), StaleAnchorEntry> = HashMap::new();
        set.insert(
            ("art-a".to_string(), "c_fuzzy".to_string()),
            entry("fuzzy", 0.42),
        );
        save(&path, &set).unwrap();
        // Inspect raw bytes to verify the v3 fields are present on
        // disk — guards against accidental serde rename / skip.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains("\"anchor_kind\"") && raw.contains("\"fuzzy_score\""),
            "v3 metadata not present in serialised form: {raw}"
        );
        assert!(raw.contains("\"version\": 3"));
        let loaded = load(&path);
        let got = loaded
            .get(&("art-a".to_string(), "c_fuzzy".to_string()))
            .expect("entry present");
        assert_eq!(got.anchor_kind, "fuzzy");
        assert!((got.fuzzy_score - 0.42).abs() < 1e-6);
    }

    #[test]
    fn v1_sidecar_loads_as_empty() {
        // The pre-v0.7.1 v1 format stored bare comment-id strings; it
        // must load as empty (not panic) so the next reindex rebuilds.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".anchors-stale.json");
        std::fs::write(&path, br#"{"version": 1, "stale": ["c_x", "c_y"]}"#).unwrap();
        assert!(load(&path).is_empty());
    }
}
