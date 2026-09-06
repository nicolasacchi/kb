//! X2 (v0.24) — per-file exclusion + paused-source enforcement.
//!
//! Two halves:
//!
//! - [`IngestGate`] — the in-memory, runtime-mutable enforcement snapshot
//!   (excluded source-relative paths + the kb's paused flag). One gate per kb,
//!   shared by every [`crate::indexer::IngestSink`] clone and the indexer task;
//!   consulted at the three ingest seams: `indexer::walk_core`, the watcher's
//!   emit paths (`initial_walk` / `emit_for_paths`), and the top of
//!   `indexer::prepare_doc` (the per-item backstop that catches anything the
//!   producer-side seams let slip). Durable truth lives in sqlite
//!   (`excluded_files` + `sources.paused`); the daemon loads the gate at
//!   bring-up and every mutation path updates BOTH.
//!
//! - [`exclude_file`] / [`include_file`] — the two mutation ops (consumed by
//!   the X3 API/CLI and tests). Exclude = sqlite row + gate entry + an
//!   exclusion-shaped delete through the ingest sink (the
//!   [`crate::cascade::CascadeMode::KeepUserData`] cascade: index row goes,
//!   `.review` sidecar + reading history survive, `artifact.removed` fires,
//!   generation bumps). Include = row/entry removal + a forced reindex nudge
//!   (the quarantine-restore pattern), so the doc returns with its comments.
//!
//! THE TRAP (plan §x-exclusion): the reconciler never deletes a file that is
//! still present on disk, so exclusion must produce an EXPLICIT delete
//! decision — the immediate cascade here, plus the reconcile delete-pass arm
//! that routes excluded-but-still-indexed rows (excluded while the daemon was
//! down) through the same KeepUserData cascade.

use crate::indexer::{IngestSink, WatchKind};
use crate::storage::StorageHandle;
use crate::Result;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Normalise a source-relative exclusion path to the stored form: trimmed,
/// forward-slash, no leading `./` or `/`. Keeps lookups byte-exact against
/// what [`crate::paths::doc_rel_path`] derives at the ingest seams.
pub fn normalize_rel(rel: &str) -> String {
    let mut s = rel.trim().replace('\\', "/");
    loop {
        if let Some(rest) = s.strip_prefix("./") {
            s = rest.to_string();
        } else if let Some(rest) = s.strip_prefix('/') {
            s = rest.to_string();
        } else {
            break;
        }
    }
    s
}

#[derive(Default)]
struct GateState {
    /// Source-relative excluded paths, in [`normalize_rel`] form.
    excluded: HashSet<String>,
    /// D6 — the kb's source is paused (one source per kb today; the flag is
    /// per-kb-single-source like `sources.paused` itself).
    paused: bool,
}

/// The shared, runtime-mutable ingest-enforcement snapshot for one kb.
///
/// Cheap to clone (an `Arc` inside); readable from BOTH async tasks and the
/// watcher's blocking threads (std `RwLock`, never held across an await).
/// The fast path — no exclusions, not paused — takes one read lock and does
/// zero path derivation.
#[derive(Clone, Default)]
pub struct IngestGate {
    inner: Arc<RwLock<GateState>>,
}

impl IngestGate {
    /// Replace the excluded set wholesale (daemon bring-up load from sqlite).
    pub fn set_excluded(&self, paths: impl IntoIterator<Item = String>) {
        let set = paths.into_iter().map(|p| normalize_rel(&p)).collect();
        self.write().excluded = set;
    }

    /// Add one excluded rel path. Returns `false` if already present.
    pub fn add_excluded(&self, rel: &str) -> bool {
        self.write().excluded.insert(normalize_rel(rel))
    }

    /// Remove one excluded rel path. Returns `false` if it wasn't there.
    pub fn remove_excluded(&self, rel: &str) -> bool {
        self.write().excluded.remove(&normalize_rel(rel))
    }

    /// Snapshot of the excluded rel paths (reconcile's orphan-sweep exempt
    /// set — an excluded doc's KEPT history/sidecar must never be swept).
    pub fn excluded_snapshot(&self) -> Vec<String> {
        self.read().excluded.iter().cloned().collect()
    }

    /// Exact-match check against an already source-relative path.
    pub fn is_excluded_rel(&self, rel: &str) -> bool {
        let g = self.read();
        !g.excluded.is_empty() && g.excluded.contains(&normalize_rel(rel))
    }

    /// Check an absolute path against `source_root`. Fast path: an empty
    /// excluded set never derives the rel path (no canonicalize syscall).
    /// Derivation is [`crate::paths::doc_rel_path`] — THE identity derivation
    /// (canonicalises both sides) — so symlinked roots can't dodge the gate.
    pub fn is_excluded(&self, path: &Path, source_root: &Path) -> bool {
        if self.read().excluded.is_empty() {
            return false;
        }
        let rel = crate::paths::doc_rel_path(&path.to_string_lossy(), source_root);
        !rel.is_empty() && self.read().excluded.contains(&rel)
    }

    /// [`Self::is_excluded`] against ANY of the watcher's source roots (live
    /// notify events arrive absolute; the watcher holds the configured roots).
    pub fn is_excluded_under_any(&self, path: &Path, sources: &[PathBuf]) -> bool {
        if self.read().excluded.is_empty() {
            return false;
        }
        sources.iter().any(|root| self.is_excluded(path, root))
    }

    /// D6 — is the kb's source paused?
    pub fn paused(&self) -> bool {
        self.read().paused
    }

    /// Flip the paused flag (bring-up load + the pause/resume route).
    pub fn set_paused(&self, paused: bool) {
        self.write().paused = paused;
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, GateState> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, GateState> {
        self.inner.write().unwrap_or_else(|e| e.into_inner())
    }
}

/// Normalise + validate a source-relative exclusion path (X3): non-empty
/// after [`normalize_rel`], and no `..` components — an exclusion row must
/// never address anything outside the source root ([`include_file`] joins
/// it back onto the root for its reindex nudge, and the gate compares it
/// against `doc_rel_path` output, which is always traversal-free).
fn checked_rel(rel_path: &str) -> crate::Result<String> {
    let rel = normalize_rel(rel_path);
    if rel.is_empty() {
        return Err(crate::Error::BadRequest(
            "exclusion path must be a non-empty source-relative path".into(),
        ));
    }
    if Path::new(&rel)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(crate::Error::BadRequest(
            "exclusion path must not contain `..` components".into(),
        ));
    }
    Ok(rel)
}

/// Exclude one file (by source-relative path) from the index.
///
/// Order matters: the durable sqlite row FIRST (a crash after it leaves an
/// excluded-but-still-indexed row, which the reconcile delete pass self-heals
/// into the cascade on its next tick), then the live gate, then the
/// exclusion-shaped delete through the ingest sink — `send_unmapped_delete`
/// routes it to `process_delete` with `keep_user_data = true`, which runs the
/// KeepUserData cascade, invalidates the content-hash dedup cache (so a later
/// re-include genuinely reindexes), emits `artifact.removed`, and bumps the
/// index generation. The delete is pushed unconditionally: a never-indexed
/// path just cascades over zero rows (idempotent).
///
/// Returns `false` when the path was already excluded (still re-pushes the
/// delete — harmless, and it re-heals a half-applied earlier attempt).
pub async fn exclude_file(
    storage: &StorageHandle,
    sink: &IngestSink,
    source_root: &Path,
    rel_path: &str,
    note: Option<String>,
) -> Result<bool> {
    let rel = checked_rel(rel_path)?;
    let added = storage
        .add_exclusion(rel.clone(), crate::indexer::unix_now(), note)
        .await?;
    sink.gate().add_excluded(&rel);
    sink.send_unmapped_delete(source_root.join(&rel)).await;
    Ok(added)
}

/// Re-include a previously excluded file: drop the sqlite row + gate entry,
/// then nudge the indexer with a forced `Modified` (the quarantine-restore
/// pattern) so the doc reindexes immediately — comments re-anchor from the
/// preserved `.review` sidecar. The nudge is skipped when the file no longer
/// exists on disk (nothing to reindex; the row is already gone).
///
/// Returns `false` when the path wasn't excluded.
pub async fn include_file(
    storage: &StorageHandle,
    sink: &IngestSink,
    source_root: &Path,
    rel_path: &str,
) -> Result<bool> {
    let rel = checked_rel(rel_path)?;
    let removed = storage.remove_exclusion(rel.clone()).await?;
    sink.gate().remove_excluded(&rel);
    let abs = source_root.join(&rel);
    if abs.exists() {
        sink.send(WatchKind::Modified, abs, true).await;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_rel_strips_lead_dots_slashes_and_backslashes() {
        assert_eq!(normalize_rel("sub/a.html"), "sub/a.html");
        assert_eq!(normalize_rel("./sub/a.html"), "sub/a.html");
        assert_eq!(normalize_rel("/sub/a.html"), "sub/a.html");
        assert_eq!(normalize_rel(".//./a.html"), "a.html");
        assert_eq!(normalize_rel("  a.html \n"), "a.html");
        assert_eq!(normalize_rel("sub\\win.html"), "sub/win.html");
        assert_eq!(normalize_rel(""), "");
    }

    #[test]
    fn gate_add_remove_snapshot_and_rel_lookup() {
        let gate = IngestGate::default();
        assert!(
            !gate.is_excluded_rel("a.html"),
            "empty gate excludes nothing"
        );

        assert!(gate.add_excluded("./a.html"), "normalised on add");
        assert!(!gate.add_excluded("a.html"), "already present");
        assert!(gate.is_excluded_rel("a.html"));
        assert!(gate.is_excluded_rel("./a.html"), "normalised on lookup");
        assert!(!gate.is_excluded_rel("b.html"));

        gate.set_excluded(vec!["/x.md".to_string(), "y.md".to_string()]);
        let mut snap = gate.excluded_snapshot();
        snap.sort();
        assert_eq!(snap, ["x.md", "y.md"], "wholesale replace + normalise");
        assert!(!gate.is_excluded_rel("a.html"), "replaced away");

        assert!(gate.remove_excluded("x.md"));
        assert!(!gate.remove_excluded("x.md"), "already gone");
    }

    #[test]
    fn gate_is_excluded_matches_absolute_paths_under_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/drop.html"), "<html></html>").unwrap();
        std::fs::write(root.join("keep.html"), "<html></html>").unwrap();

        let gate = IngestGate::default();
        gate.add_excluded("sub/drop.html");
        assert!(gate.is_excluded(&root.join("sub/drop.html"), root));
        assert!(!gate.is_excluded(&root.join("keep.html"), root));
        // Under-any: same check across a source list.
        let sources = vec![root.to_path_buf()];
        assert!(gate.is_excluded_under_any(&root.join("sub/drop.html"), &sources));
        assert!(!gate.is_excluded_under_any(&root.join("keep.html"), &sources));
    }

    #[test]
    fn checked_rel_rejects_empty_and_traversal_accepts_normal() {
        // Empty (and normalises-to-empty) forms are refused — an exclusion
        // row must name a concrete file.
        assert!(checked_rel("").is_err());
        assert!(checked_rel("  ./ ").is_err());
        assert!(checked_rel("//").is_err());
        // `..` components are refused wherever they appear (X3 hardening:
        // the API/CLI feed operator strings straight in here, and
        // include_file joins the stored rel onto the source root).
        assert!(checked_rel("../evil.html").is_err());
        assert!(checked_rel("sub/../../evil.html").is_err());
        assert!(checked_rel("./../evil.html").is_err());
        // Normal paths pass through in normalize_rel form.
        assert_eq!(checked_rel("./sub/a.html").unwrap(), "sub/a.html");
        assert_eq!(checked_rel("/a.html").unwrap(), "a.html");
    }

    #[test]
    fn gate_paused_flag_roundtrip() {
        let gate = IngestGate::default();
        assert!(!gate.paused(), "gates start unpaused");
        gate.set_paused(true);
        assert!(gate.paused());
        gate.set_paused(false);
        assert!(!gate.paused());
    }
}
