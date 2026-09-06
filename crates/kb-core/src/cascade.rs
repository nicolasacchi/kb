//! R2 — the systematic artifact delete-cascade engine.
//!
//! One engine, two modes. When an artifact leaves the corpus (a file unlink,
//! reconcile's synthetic delete, or — later — an X2 exclusion), EVERYTHING
//! tied to its path-stable id must go, atomically where it can and
//! best-effort where cross-store atomicity is impossible:
//!
//!   - **lance** doc row + its chunk rows (dropped FIRST by the storage actor,
//!     mirroring the `DropKbData` ordering: the search-visible state goes
//!     before the sqlite metadata, so a mid-cascade failure never leaves the
//!     index disagreeing with the sidecar tables);
//!   - **sqlite** dependents, all in ONE transaction (see
//!     [`crate::storage::sqlite::Db::cascade_delete_doc`] + its `CASCADE_STEPS`
//!     registry);
//!   - **filesystem** state — the `.review/<id>.json` sidecar and the
//!     `.attachments/<id>/` blob dir — under the per-kb `review_lock`
//!     (invariants #6 / #18). This can't go through the storage actor (it owns
//!     only lance + sqlite), so it lives HERE in the engine wrapper.
//!
//! ## Modes
//!
//! - [`CascadeMode::Full`] — a real delete. Removes every store above.
//! - [`CascadeMode::KeepUserData`] — the exclusion shape (consumed by X2):
//!   removes the same EXCEPT it keeps the `.review` sidecar + attachments and
//!   the reading `history` + `reading_sections` rows, so an excluded-then-
//!   re-included artifact keeps its comments and reading progress.
//!
//! ## Events
//!
//! The storage actor has no `EventBus` (and we don't add one — invariant #2),
//! so the engine RETURNS what it removed in a [`CascadeReport`] and the caller
//! (the indexer's `process_delete`, the reconcile sweep) emits the SSE: one
//! `list.updated` per affected reading list (invariant #25) and, on a Full
//! delete that dropped a `sessions` row, `session.deleted`.

use crate::ids::ArtifactId;
use crate::storage::StorageHandle;
use crate::Result;
use std::collections::HashSet;
use std::path::Path;

/// Which stores a cascade touches. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CascadeMode {
    /// A real delete — remove every store tied to the artifact.
    Full,
    /// Exclusion shape (X2): remove everything EXCEPT the `.review` sidecar +
    /// attachments and the reading `history` + `reading_sections` rows.
    KeepUserData,
}

/// What a single-artifact cascade removed, for the caller to turn into SSE.
#[derive(Debug, Clone, Default)]
pub struct CascadeReport {
    /// `sessions` rows removed (>0 ⇒ caller emits `session.deleted`).
    pub sessions_removed: usize,
}

/// Cascade one artifact out of a kb.
///
/// Order: storage actor (lance doc+chunks, then the sqlite tx, then one
/// `bump_generation`) FIRST; then, for [`CascadeMode::Full`] only, the
/// filesystem sidecar + attachment removal under `review_lock`. A storage
/// failure propagates (the caller records it); a filesystem failure is
/// best-effort (logged — the important state already committed, and a leftover
/// sidecar is reclaimed by nothing today but is harmless, so we never fail the
/// delete on it).
///
/// `review_dir` is the kb's `.review` directory (`None` disables all fs
/// cleanup — the shape tests use). `review_lock` is the kb's shared per-kb
/// comment lock (`None` when unavailable, e.g. the broadcast-bridge test
/// shim — the fs mutation then runs unlocked, tolerable only because those
/// paths never race a live comment route).
pub async fn delete_artifact(
    storage: &StorageHandle,
    artifact_id: &ArtifactId,
    del_path: &Path,
    mode: CascadeMode,
    review_dir: Option<&Path>,
    review_lock: Option<&tokio::sync::Mutex<()>>,
) -> Result<CascadeReport> {
    let outcome = storage
        .cascade_delete_doc(artifact_id.clone(), del_path.to_path_buf(), mode)
        .await?;

    if matches!(mode, CascadeMode::Full) {
        if let Some(dir) = review_dir {
            remove_review_sidecar_and_attachments(dir, artifact_id.as_str(), review_lock).await;
        }
    }

    Ok(CascadeReport {
        sessions_removed: outcome.sessions_removed,
    })
}

/// Delete the `.review/<id>.json` sidecar and the whole `.attachments/<id>/`
/// blob dir for a fully-removed artifact, holding `review_lock` across both so
/// a concurrent comment-route mutation on the (now-vanished) artifact can't
/// race the removal (invariants #6 / #18). Best-effort: a missing file/dir is
/// success; any other error is logged, never propagated.
async fn remove_review_sidecar_and_attachments(
    review_dir: &Path,
    artifact_id: &str,
    review_lock: Option<&tokio::sync::Mutex<()>>,
) {
    // The `.attachments/<id>/` dir is a sibling of `.review/` under the kb
    // state root (`paths::kb_attachment_dir`); derive it from `review_dir` so
    // the engine needs no extra threading.
    let review_file = review_dir.join(format!("{artifact_id}.json"));
    let attachments_dir = review_dir
        .parent()
        .map(|state| state.join(".attachments").join(artifact_id));

    let _guard = match review_lock {
        Some(lock) => Some(lock.lock().await),
        None => None,
    };

    // W0.8 — the 2026-07-15 incident was invisible: a sidecar vanished with
    // NO log line anywhere (reconcile's own log showed deletes=0, because the
    // drop came from this event-driven path). `remove_file_if_present` now
    // reports whether it actually removed something (vs. the file already
    // being absent — an idempotent no-op, not worth a line), so every REAL
    // sidecar deletion is logged at info with the artifact id + path.
    match remove_file_if_present(&review_file) {
        Ok(true) => {
            tracing::info!(
                artifact_id = %artifact_id,
                path = %review_file.display(),
                "cascade: removed review sidecar for deleted artifact",
            );
        }
        Ok(false) => {}
        Err(e) => {
            tracing::warn!(
                path = %review_file.display(),
                error = %e,
                "cascade: failed to remove review sidecar",
            );
        }
    }
    if let Some(dir) = attachments_dir {
        match remove_dir_all_if_present(&dir) {
            Ok(true) => {
                tracing::info!(
                    artifact_id = %artifact_id,
                    path = %dir.display(),
                    "cascade: removed attachments dir for deleted artifact",
                );
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(
                    path = %dir.display(),
                    error = %e,
                    "cascade: failed to remove attachments dir",
                );
            }
        }
    }
}

/// Returns `Ok(true)` when a file was actually removed, `Ok(false)` when it
/// was already absent (still success — deletes are idempotent; the caller
/// only logs the `true` case).
fn remove_file_if_present(path: &Path) -> std::io::Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Returns `Ok(true)` when a directory was actually removed, `Ok(false)`
/// when it was already absent.
fn remove_dir_all_if_present(path: &Path) -> std::io::Result<bool> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// What the reconcile orphan sweep removed, for the caller to turn into SSE.
#[derive(Debug, Clone, Default)]
pub struct SweepReport {
    /// Total sqlite rows reclaimed across every swept table.
    pub total_rows: usize,
}

/// R2 — the reconcile orphan backstop. Prunes sqlite dependents that reference
/// artifact ids no longer present in lance — catching leaks from pre-v0.24
/// deletes that never cascaded (`edges`/`corkboard`/`pinned_memories`/
/// `history`+`reading_sections`). `list_entries` is excluded — an entry
/// pointing at a deleted artifact is an intentional read-time tombstone.
///
/// `live_ids` is the live lance doc-id set (reconcile already holds it).
/// `exempt_paths` is the forward-compat hook for X2: source-relative paths
/// whose (kept) sqlite state must be preserved even though they have no lance
/// row — each is hashed to its [`ArtifactId`] and unioned into the keep set, so
/// an excluded doc's kept history/sidecar is never swept as an orphan. Empty
/// today.
pub async fn sweep_orphans(
    storage: &StorageHandle,
    live_ids: HashSet<String>,
    exempt_paths: &[String],
) -> Result<SweepReport> {
    let mut keep = live_ids;
    for rel in exempt_paths {
        keep.insert(ArtifactId::from_path(rel).as_str().to_string());
    }
    let out = storage.sweep_orphans(keep).await?;
    Ok(SweepReport {
        total_rows: out.total_rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::StorageActor;
    use std::sync::Arc;

    async fn handle() -> StorageHandle {
        let tmp = tempfile::tempdir().unwrap();
        let lance = tmp.path().join("lance");
        let sqlite = tmp.path().join("index.db");
        std::mem::forget(tmp); // keep the dir alive for the test's lifetime
        StorageActor::spawn(lance, sqlite, None).await.unwrap()
    }

    #[tokio::test]
    async fn full_delete_removes_lance_row_and_sidecar() {
        let storage = handle().await;
        let tmp = tempfile::tempdir().unwrap();
        let review_dir = tmp.path().join(".review");
        std::fs::create_dir_all(&review_dir).unwrap();

        let id = ArtifactId::from_path("doomed.html");
        let sidecar = review_dir.join(format!("{}.json", id.as_str()));
        std::fs::write(&sidecar, b"{}").unwrap();
        // `.attachments/<id>/` sits beside `.review/` under the state root.
        let att_dir = tmp.path().join(".attachments").join(id.as_str());
        std::fs::create_dir_all(&att_dir).unwrap();
        std::fs::write(att_dir.join("a_blob"), b"bytes").unwrap();

        // Seed a lance row at a known path.
        let doc = crate::storage::schema::Doc::placeholder(id.as_str(), "/corpus/doomed.html");
        storage.upsert_doc(doc).await.unwrap();
        assert_eq!(storage.count_rows().await.unwrap(), 1);

        let lock = Arc::new(tokio::sync::Mutex::new(()));
        delete_artifact(
            &storage,
            &id,
            Path::new("/corpus/doomed.html"),
            CascadeMode::Full,
            Some(&review_dir),
            Some(&lock),
        )
        .await
        .unwrap();

        assert_eq!(storage.count_rows().await.unwrap(), 0, "lance row gone");
        assert!(!sidecar.exists(), "review sidecar removed");
        assert!(!att_dir.exists(), "attachments dir removed");
    }

    #[tokio::test]
    async fn keep_user_data_preserves_the_sidecar() {
        let storage = handle().await;
        let tmp = tempfile::tempdir().unwrap();
        let review_dir = tmp.path().join(".review");
        std::fs::create_dir_all(&review_dir).unwrap();

        let id = ArtifactId::from_path("kept.html");
        let sidecar = review_dir.join(format!("{}.json", id.as_str()));
        std::fs::write(&sidecar, b"{}").unwrap();

        let doc = crate::storage::schema::Doc::placeholder(id.as_str(), "/corpus/kept.html");
        storage.upsert_doc(doc).await.unwrap();

        let lock = Arc::new(tokio::sync::Mutex::new(()));
        delete_artifact(
            &storage,
            &id,
            Path::new("/corpus/kept.html"),
            CascadeMode::KeepUserData,
            Some(&review_dir),
            Some(&lock),
        )
        .await
        .unwrap();

        assert_eq!(
            storage.count_rows().await.unwrap(),
            0,
            "lance row still goes"
        );
        assert!(sidecar.exists(), "KeepUserData keeps the review sidecar");
    }

    #[tokio::test]
    async fn sweep_respects_the_exempt_set() {
        let storage = handle().await;
        // Two dangling corkboard rows: one whose source-rel path is exempt,
        // one that is a true orphan. No lance rows at all → both are absent
        // from the live set.
        let exempt_id = ArtifactId::from_path("excluded/keep.html");
        let orphan_id = ArtifactId::from_path("gone/orphan.html");
        storage
            .corkboard_add(exempt_id.as_str().to_string(), 1)
            .await
            .unwrap();
        storage
            .corkboard_add(orphan_id.as_str().to_string(), 1)
            .await
            .unwrap();

        let report = sweep_orphans(
            &storage,
            HashSet::new(),
            &["excluded/keep.html".to_string()],
        )
        .await
        .unwrap();
        assert_eq!(report.total_rows, 1, "only the true orphan is swept");

        let ids = storage.corkboard_list().await.unwrap();
        let present: HashSet<String> = ids.into_iter().map(|r| r.artifact_id).collect();
        assert!(present.contains(exempt_id.as_str()), "exempt id preserved");
        assert!(!present.contains(orphan_id.as_str()), "orphan swept");
    }
}
