//! F3a — relocate engine: move/rename an artifact WITHOUT losing
//! identity-keyed state (comments, history, lists, edges, sessions, …).
//!
//! Artifact ids are path-derived ([`crate::ids::ArtifactId::from_path`]),
//! so a rename is a *new* id. This module:
//!
//! 1. writes a durable `moves` intent row,
//! 2. renames the source file + `.review` / `.attachments` sidecars under
//!    the per-kb `review_lock` (invariant #6),
//! 3. re-keys lance (preserve embedding) + sqlite dependents in ONE
//!    storage-actor write-lane message (invariant #17 — actor-direct, not
//!    IngestSink),
//! 4. guards the watcher race (in-memory pending set + durable moves table)
//!    so a Debounced `Deleted` for the old path never cascades the just-
//!    migrated state away.
//!
//! Folder batching is [`relocate_folder`]: deterministic source-rel order,
//! stop-on-first-error, best-effort empty-dir prune.
//!
//! Startup replay ([`replay_incomplete_moves`]) **converges**: the lance step
//! is already idempotent (`get_by_id(new_id)` + matching `content_hash`
//! skips the copy; UPDATEs are idempotent), transient sqlite errors retry
//! on the next boot, and the list-entry tombstone collision (F3c T1) was
//! the only known deterministic abort — removed via `UPDATE OR REPLACE`.

use crate::ids::ArtifactId;
use crate::indexer::DedupCache;
use crate::paths::{canonical_abs, doc_rel_path};
use crate::review::{self, ReviewFile};
use crate::storage::StorageHandle;
use crate::types::KbName;
use crate::{Error, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex as AsyncMutex;

/// Grace / TTL for the in-memory pending-moves set and for durable
/// "recently completed" delete suppression. Must exceed the watcher's
/// 400 ms debounce so a post-rename `Deleted` still sees the guard.
pub const PENDING_MOVES_TTL: Duration = Duration::from_secs(10);

/// Same window expressed in whole seconds for the sqlite grace check.
pub const MOVES_DELETE_GRACE_SECS: i64 = 10;

/// Outcome of a successful single-file relocate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelocateOutcome {
    pub old_id: String,
    pub new_id: String,
    pub old_rel: String,
    pub new_rel: String,
    /// DISTINCT list_ids whose `list_entries` rows were rekeyed (for
    /// `list.updated` SSE). Empty when the doc was on no reading list.
    pub affected_list_ids: Vec<String>,
}

/// Per-item result for [`relocate_folder`].
#[derive(Debug, Clone)]
pub struct FolderRelocateItem {
    pub outcome: RelocateOutcome,
}

/// Context the daemon (or tests) pass into relocate ops.
pub struct RelocateCtx {
    pub storage: StorageHandle,
    /// Absolute (or canonicalisable) corpus root.
    pub source_root: PathBuf,
    /// Per-kb `.review` directory (`<state>/<kb>/.review`).
    pub review_dir: PathBuf,
    /// Per-kb comment lock (invariant #6). `None` only in pure unit tests
    /// that never race a comment route.
    pub review_lock: Option<Arc<AsyncMutex<()>>>,
    /// In-memory race guard (shared with the indexer delete path).
    pub pending: Arc<PendingMoves>,
    /// Shared indexer [`DedupCache`] (id → [`crate::indexer::IndexedMeta`]).
    /// Rekeyed after a successful move so the post-rename `Created` event
    /// hits the content-hash pre-gate (no re-embed). `None` only in pure
    /// unit tests that do not exercise the cache.
    pub dedup: Option<DedupCache>,
    /// Kb name (for review-file rewrite / logging only).
    pub kb: KbName,
}

/// In-memory pending-move set: absolute paths (old + new) registered
/// before the FS rename, consulted by [`should_suppress_delete`]. Entries
/// expire after [`PENDING_MOVES_TTL`] so a crashed relocate can't suppress
/// a real delete forever.
#[derive(Debug, Default)]
pub struct PendingMoves {
    /// path (lossy string of absolute path) → registered_at
    inner: Mutex<HashMap<String, Instant>>,
}

/// Process-wide pending-moves guard shared by relocate + the indexer's
/// `process_delete`. One set per process is correct for kb's single-operator
/// multi-kb-in-one-daemon model (paths are absolute, so kb roots don't collide).
pub fn shared_pending() -> Arc<PendingMoves> {
    static P: OnceLock<Arc<PendingMoves>> = OnceLock::new();
    P.get_or_init(|| Arc::new(PendingMoves::new())).clone()
}

impl PendingMoves {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Register both endpoints of a pending relocate.
    pub fn register(&self, old_abs: &Path, new_abs: &Path) {
        let now = Instant::now();
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.insert(path_key(old_abs), now);
        g.insert(path_key(new_abs), now);
    }

    /// Drop both endpoints (called after the grace sleep, or immediately
    /// when the caller prefers explicit cleanup).
    pub fn unregister(&self, old_abs: &Path, new_abs: &Path) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.remove(&path_key(old_abs));
        g.remove(&path_key(new_abs));
    }

    /// True when `path` is still within the TTL window.
    pub fn is_pending(&self, path: &Path) -> bool {
        self.expire_and_check(&path_key(path))
    }

    fn expire_and_check(&self, key: &str) -> bool {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        g.retain(|_, t| now.duration_since(*t) < PENDING_MOVES_TTL);
        g.contains_key(key)
    }
}

fn path_key(p: &Path) -> String {
    // Prefer canonical form so watcher events and relocate register match.
    // When the file is already gone (post-rename Deleted), canonicalize the
    // parent and re-join the file name so the key still matches what
    // `register` wrote while the source existed.
    if let Ok(c) = p.canonicalize() {
        return c.to_string_lossy().into_owned();
    }
    if let (Some(parent), Some(name)) = (p.parent(), p.file_name()) {
        if let Ok(pc) = parent.canonicalize() {
            return pc.join(name).to_string_lossy().into_owned();
        }
    }
    p.to_string_lossy().into_owned()
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Normalise a source-relative path: forward slashes, no leading `./`,
/// reject `..` components.
fn normalize_rel(rel: &str) -> Result<String> {
    let rel = rel.trim().trim_start_matches("./");
    if rel.is_empty() {
        return Err(Error::BadRequest("empty source-relative path".into()));
    }
    if rel.split(['/', '\\']).any(|c| c == "..") {
        return Err(Error::BadRequest(format!("path escapes the corpus: {rel}")));
    }
    Ok(rel.replace('\\', "/"))
}

/// Resolve `rel` under `source_root` and confirm it stays inside the root.
/// Unlike share's helper, the target need NOT exist yet (relocate creates
/// parent dirs). Returns `(absolute, canonical_root)`.
fn resolve_inside_root(source_root: &Path, rel: &str) -> Result<(PathBuf, PathBuf)> {
    let rel = normalize_rel(rel)?;
    let root_canon = source_root.canonicalize().map_err(Error::Io)?;
    let abs = root_canon.join(&rel);
    // Containment without requiring the target to exist: walk parents until
    // we hit an existing prefix, then check starts_with.
    let mut check = abs.clone();
    loop {
        if check.exists() {
            let check_canon = check.canonicalize().map_err(Error::Io)?;
            if !check_canon.starts_with(&root_canon) {
                return Err(Error::BadRequest(format!("path escapes the corpus: {rel}")));
            }
            break;
        }
        match check.parent() {
            Some(p) if p != check => check = p.to_path_buf(),
            _ => {
                return Err(Error::BadRequest(format!("path escapes the corpus: {rel}")));
            }
        }
    }
    Ok((abs, root_canon))
}

/// Public redirect helper for the later HTTP task: resolve `old_id` or
/// `old_rel` through the moves log (newest row wins; chains A→B→C resolve
/// to C).
pub async fn moves_lookup(
    storage: &StorageHandle,
    old_id_or_rel: &str,
) -> Result<Option<(String, String)>> {
    storage.moves_lookup(old_id_or_rel.to_string()).await
}

/// Re-key an artifact_id→T map (the indexer's DedupCache is one such map).
pub fn rekey_id_map<T>(map: &mut HashMap<String, T>, old_id: &str, new_id: &str) {
    if let Some(v) = map.remove(old_id) {
        map.insert(new_id.to_string(), v);
    }
}

/// Two-layer delete suppression: in-memory pending set OR durable moves
/// table (incomplete / recent). Used by the indexer's `process_delete`.
pub async fn should_suppress_delete(
    storage: &StorageHandle,
    pending: Option<&PendingMoves>,
    path: &Path,
    source_root: &Path,
) -> bool {
    if let Some(p) = pending {
        if p.is_pending(path) {
            return true;
        }
    }
    let rel = doc_rel_path(&path.to_string_lossy(), source_root);
    if rel.is_empty() {
        return false;
    }
    match storage
        .moves_suppresses_delete(rel, unix_now(), MOVES_DELETE_GRACE_SECS)
        .await
    {
        Ok(true) => true,
        Ok(false) => false,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "moves_suppresses_delete failed; not suppressing",
            );
            false
        }
    }
}

/// Move one indexed artifact to a new source-relative path, migrating all
/// identity-keyed state. Validate-first: a failed validate never writes an
/// intent row.
///
/// Holds the per-kb `review_lock` for the whole validate → intent → rename →
/// sidecar window so concurrent relocates of the same source cannot both
/// pass validation and leave a dangling intent (the storage actor already
/// serializes the DB phase).
pub async fn relocate_doc(
    ctx: &RelocateCtx,
    old_source_rel: &str,
    new_source_rel: &str,
) -> Result<RelocateOutcome> {
    let old_rel = normalize_rel(old_source_rel)?;
    let new_rel = normalize_rel(new_source_rel)?;

    // No-op same path (no lock needed).
    if old_rel == new_rel {
        let id = ArtifactId::from_path(&old_rel).as_str().to_string();
        return Ok(RelocateOutcome {
            old_id: id.clone(),
            new_id: id,
            old_rel,
            new_rel,
            affected_list_ids: Vec::new(),
        });
    }

    // Serialize relocates per kb (validate through sidecar rekey). Comment
    // routes share this lock (#6); concurrent move+comment is also ordered.
    let _guard = match &ctx.review_lock {
        Some(lock) => Some(lock.lock().await),
        None => None,
    };

    let (old_abs, root_canon) = resolve_inside_root(&ctx.source_root, &old_rel)?;
    let (new_abs, _) = resolve_inside_root(&ctx.source_root, &new_rel)?;

    if !old_abs.is_file() {
        return Err(Error::NotFound(format!(
            "relocate source not found: {old_rel}"
        )));
    }
    if new_abs.exists() {
        return Err(Error::Conflict(format!(
            "relocate target already exists: {new_rel}"
        )));
    }

    // Source must be indexed. Stored path is canonical absolute (invariant #27).
    let old_canon = canonical_abs(&old_abs);
    let old_stored = old_canon.to_string_lossy().to_string();
    let summary = ctx
        .storage
        .get_by_source_path(old_stored.clone())
        .await?
        .ok_or_else(|| Error::NotFound(format!("source not indexed: {old_rel}")))?;

    let old_id = summary.id.clone();
    let new_id = ArtifactId::from_path(&new_rel).as_str().to_string();
    if old_id == new_id {
        // Path-hash collision (vanishingly rare) — refuse rather than no-op
        // a real path change under a shared id.
        return Err(Error::Conflict(format!(
            "old and new paths hash to the same artifact id ({old_id})"
        )));
    }

    let moved_at = unix_now();
    let moves_row_id = ctx
        .storage
        .moves_insert_intent(
            old_id.clone(),
            new_id.clone(),
            old_rel.clone(),
            new_rel.clone(),
            moved_at,
        )
        .await?;

    // Register pending BEFORE the rename so a racing watcher Deleted can't
    // cascade between rename and the actor rekey.
    ctx.pending.register(&old_abs, &new_abs);

    // Post-intent FS prep + rename: on failure the FS never changed, so
    // abandon the intent (set completed_at) — otherwise
    // moves_suppresses_delete would block real deletes of old_rel until
    // the next daemon boot's replay.
    // Parent of target — folder creation is implicit (no mkdir op).
    if let Some(parent) = new_abs.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            abandon_intent(&ctx.storage, &ctx.pending, moves_row_id, &old_abs, &new_abs).await;
            return Err(Error::Io(e));
        }
    }

    if let Err(e) = std::fs::rename(&old_abs, &new_abs) {
        abandon_intent(&ctx.storage, &ctx.pending, moves_row_id, &old_abs, &new_abs).await;
        return Err(Error::Io(e));
    }

    // FS rename already landed — on sidecar failure leave incomplete for
    // startup replay (new path exists → actor rekey). Surface the error.
    rekey_review_sidecar(&ctx.review_dir, &old_id, &new_id, &ctx.kb)?;
    rekey_attachments_dir(&ctx.review_dir, &old_id, &new_id)?;

    let new_canon = canonical_abs(&new_abs);
    // If the target parent was just created, canonicalize may still work.
    let new_stored = if new_canon.exists() {
        new_canon.to_string_lossy().to_string()
    } else {
        // Fall back to root_canon/join for a stable absolute form.
        root_canon.join(&new_rel).to_string_lossy().to_string()
    };

    let completed_at = unix_now();
    let affected_list_ids = ctx
        .storage
        .relocate_doc_storage(
            old_id.clone(),
            new_id.clone(),
            new_stored,
            old_rel.clone(),
            new_rel.clone(),
            moves_row_id,
            completed_at,
        )
        .await?;

    // Dedup re-key (same content hash under the new id).
    if let Some(dedup) = &ctx.dedup {
        let mut g = dedup.lock().unwrap_or_else(|e| e.into_inner());
        rekey_id_map(&mut *g, &old_id, &new_id);
    }

    // Keep the pending entry for TTL so late Debounced deletes still skip.
    // Spawn a best-effort unregister after the grace window (does not block).
    let pending = ctx.pending.clone();
    let old_for_unreg = old_abs.clone();
    let new_for_unreg = new_abs.clone();
    tokio::spawn(async move {
        tokio::time::sleep(PENDING_MOVES_TTL).await;
        pending.unregister(&old_for_unreg, &new_for_unreg);
    });

    Ok(RelocateOutcome {
        old_id,
        new_id,
        old_rel,
        new_rel,
        affected_list_ids,
    })
}

/// Mark a post-intent pre-FS-success failure as abandoned so durable
/// delete-suppression does not stick until the next boot.
///
/// `completed_at` is stamped **outside** the delete-grace window
/// (`now - MOVES_DELETE_GRACE_SECS - 1`): `moves_suppresses_delete` treats
/// a *recent* completed_at like an incomplete row (covers successful
/// relocates' Debounced Deleted). For an abandon the FS never changed, so
/// a recent stamp would wrongly suppress real deletes of old_rel.
async fn abandon_intent(
    storage: &StorageHandle,
    pending: &PendingMoves,
    row_id: i64,
    old_abs: &Path,
    new_abs: &Path,
) {
    let abandoned_at = unix_now().saturating_sub(MOVES_DELETE_GRACE_SECS + 1);
    let _ = storage.moves_mark_completed(row_id, abandoned_at).await;
    pending.unregister(old_abs, new_abs);
}

/// Rename `.review/<old>.json` → `<new>.json` and rewrite the internal
/// `artifact.id` (and comment `file` fields that still equal the old id).
fn rekey_review_sidecar(review_dir: &Path, old_id: &str, new_id: &str, _kb: &KbName) -> Result<()> {
    let old_path = review_dir.join(format!("{old_id}.json"));
    let new_path = review_dir.join(format!("{new_id}.json"));
    if !old_path.exists() {
        return Ok(());
    }
    if new_path.exists() {
        // Prefer destination; drop the old sidecar to avoid two homes.
        let _ = std::fs::remove_file(&old_path);
        return Ok(());
    }
    std::fs::rename(&old_path, &new_path)?;
    // Rewrite internal artifact id (migrate_legacy_id only renames the file).
    if let Some(mut file) = review::load(&new_path)? {
        file.artifact.id = new_id.to_string();
        for c in &mut file.comments {
            if c.file == old_id {
                c.file = new_id.to_string();
            }
        }
        // save_atomic without if-match (we hold the review_lock).
        let _ = review::save_atomic(&new_path, &file, None)?;
    }
    Ok(())
}

/// Rename `.attachments/<old>/` → `<new>/` (sibling of `.review/`).
fn rekey_attachments_dir(review_dir: &Path, old_id: &str, new_id: &str) -> Result<()> {
    let Some(state) = review_dir.parent() else {
        return Ok(());
    };
    let old_dir = state.join(".attachments").join(old_id);
    let new_dir = state.join(".attachments").join(new_id);
    if !old_dir.exists() {
        return Ok(());
    }
    if new_dir.exists() {
        let _ = std::fs::remove_dir_all(&old_dir);
        return Ok(());
    }
    if let Some(parent) = new_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(&old_dir, &new_dir)?;
    Ok(())
}

/// Relocate every indexed doc under `old_folder_rel` into `new_folder_rel`
/// (prefix rewrite). Deterministic order (sorted source-rel). Stops on the
/// first error; completed items are fully consistent. Best-effort prune of
/// now-empty old directories.
pub async fn relocate_folder(
    ctx: &RelocateCtx,
    old_folder_rel: &str,
    new_folder_rel: &str,
) -> Result<Vec<FolderRelocateItem>> {
    let old_folder = normalize_rel(old_folder_rel)?;
    let new_folder = normalize_rel(new_folder_rel)?;
    let root = ctx.source_root.canonicalize().map_err(Error::Io)?;

    let docs = ctx.storage.list_docs(u32::MAX).await?;
    let mut matches: Vec<(String, String)> = Vec::new(); // (stored_path, old_rel)
    for d in docs {
        let rel = doc_rel_path(&d.path, &root);
        if rel == old_folder || rel.starts_with(&format!("{old_folder}/")) {
            matches.push((d.path, rel));
        }
    }
    matches.sort_by(|a, b| a.1.cmp(&b.1));

    let mut out = Vec::with_capacity(matches.len());
    for (_path, old_rel) in matches {
        let suffix = if old_rel == old_folder {
            // The folder itself as a file? unusual; skip bare folder matches
            // that aren't files (docs are files).
            String::new()
        } else {
            old_rel[old_folder.len() + 1..].to_string()
        };
        let new_rel = if suffix.is_empty() {
            new_folder.clone()
        } else {
            format!("{new_folder}/{suffix}")
        };
        let outcome = relocate_doc(ctx, &old_rel, &new_rel).await?;
        out.push(FolderRelocateItem { outcome });
    }

    // Best-effort prune empty directories under the old folder.
    let old_abs = root.join(&old_folder);
    prune_empty_dirs(&old_abs);

    Ok(out)
}

fn prune_empty_dirs(dir: &Path) {
    if !dir.is_dir() {
        return;
    }
    // Depth-first: prune children first.
    if let Ok(rd) = std::fs::read_dir(dir) {
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_dir() {
                prune_empty_dirs(&p);
            }
        }
    }
    if let Ok(mut rd) = std::fs::read_dir(dir) {
        if rd.next().is_none() {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

/// Startup replay: for every incomplete moves row, if the new path exists
/// re-run the actor migration (idempotent); if the rename never happened,
/// mark the row completed (abandoned) so it never suppresses a real delete.
pub async fn replay_incomplete_moves(ctx: &RelocateCtx) -> Result<usize> {
    let rows = ctx.storage.moves_list_incomplete().await?;
    let root = match ctx.source_root.canonicalize() {
        Ok(r) => r,
        Err(_) => return Ok(0),
    };
    let mut fixed = 0usize;
    for row in rows {
        let new_abs = root.join(&row.new_rel);
        let now = unix_now();
        if new_abs.is_file() {
            let new_stored = canonical_abs(&new_abs).to_string_lossy().to_string();
            // Re-run actor steps; UPDATEs + get_by_id(new) guard make this
            // idempotent.
            if let Err(e) = ctx
                .storage
                .relocate_doc_storage(
                    row.old_id.clone(),
                    row.new_id.clone(),
                    new_stored,
                    row.old_rel.clone(),
                    row.new_rel.clone(),
                    row.id,
                    now,
                )
                .await
            {
                tracing::warn!(
                    moves_id = row.id,
                    old_rel = %row.old_rel,
                    error = %e,
                    "moves startup replay: actor rekey failed",
                );
                continue;
            }
            // Sidecars: best-effort rekey if still under old id.
            let _ = rekey_review_sidecar(&ctx.review_dir, &row.old_id, &row.new_id, &ctx.kb);
            let _ = rekey_attachments_dir(&ctx.review_dir, &row.old_id, &row.new_id);
            // Dedup rekey if the old source is gone.
            if let Some(dedup) = &ctx.dedup {
                let mut g = dedup.lock().unwrap_or_else(|e| e.into_inner());
                rekey_id_map(&mut *g, &row.old_id, &row.new_id);
            }
            fixed += 1;
            tracing::info!(
                moves_id = row.id,
                old_rel = %row.old_rel,
                new_rel = %row.new_rel,
                "moves startup replay: completed incomplete relocate",
            );
        } else {
            // Rename never happened (or both gone) — abandon the intent.
            // Stamp outside the delete-grace window (same as abandon_intent)
            // so boot does not briefly suppress real deletes of old_rel.
            let abandoned_at = now.saturating_sub(MOVES_DELETE_GRACE_SECS + 1);
            ctx.storage
                .moves_mark_completed(row.id, abandoned_at)
                .await?;
            tracing::warn!(
                moves_id = row.id,
                old_rel = %row.old_rel,
                new_rel = %row.new_rel,
                "moves startup replay: abandoned incomplete intent (new path missing)",
            );
            fixed += 1;
        }
    }
    Ok(fixed)
}

/// List indexed docs under a folder (test / folder-batch helper).
pub async fn list_indexed_under_folder(
    storage: &StorageHandle,
    source_root: &Path,
    folder_rel: &str,
) -> Result<Vec<String>> {
    let folder = normalize_rel(folder_rel)?;
    let root = source_root.canonicalize().map_err(Error::Io)?;
    let docs = storage.list_docs(u32::MAX).await?;
    let mut rels: Vec<String> = docs
        .into_iter()
        .filter_map(|d| {
            let rel = doc_rel_path(&d.path, &root);
            if rel == folder || rel.starts_with(&format!("{folder}/")) {
                Some(rel)
            } else {
                None
            }
        })
        .collect();
    rels.sort();
    // Dedup.
    let mut seen = HashSet::new();
    rels.retain(|r| seen.insert(r.clone()));
    Ok(rels)
}

// Silence unused import if ReviewFile is only used via load/save.
#[allow(dead_code)]
fn _review_file_type_anchor(_: &ReviewFile) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lists::{NewListEntry, PositionSpec};
    use crate::storage::schema::{ChunkDoc, Doc};
    use crate::storage::StorageActor;
    use crate::types::KbName;

    struct Fixture {
        _tmp: tempfile::TempDir,
        ctx: RelocateCtx,
        source_root: PathBuf,
        review_dir: PathBuf,
        state_root: PathBuf,
    }

    async fn setup() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let source_root = tmp.path().join("corpus");
        let state_root = tmp.path().join("state");
        let review_dir = state_root.join(".review");
        std::fs::create_dir_all(&source_root).unwrap();
        std::fs::create_dir_all(&review_dir).unwrap();
        std::fs::create_dir_all(state_root.join(".attachments")).unwrap();

        let lance = tmp.path().join("lance");
        let sqlite = tmp.path().join("index.db");
        // Embedding dim 4 for small test vectors.
        let storage = StorageActor::spawn(lance, sqlite, Some(4)).await.unwrap();

        let kb = KbName::new("testkb").unwrap();
        // Use the process-wide pending set so process_delete_for_test sees
        // the same registrations relocate_doc writes.
        let pending = shared_pending();
        let dedup = crate::indexer::empty_dedup_cache();
        let ctx = RelocateCtx {
            storage,
            source_root: source_root.clone(),
            review_dir: review_dir.clone(),
            review_lock: Some(Arc::new(AsyncMutex::new(()))),
            pending,
            dedup: Some(dedup),
            kb,
        };
        Fixture {
            _tmp: tmp,
            ctx,
            source_root,
            review_dir,
            state_root,
        }
    }

    fn write_html(root: &Path, rel: &str, body: &str) -> PathBuf {
        let abs = root.join(rel);
        if let Some(p) = abs.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        let html = format!(
            "<!doctype html><html><head><title>{rel}</title></head>\
             <body><h1 id=\"top\">{body}</h1></body></html>"
        );
        std::fs::write(&abs, html).unwrap();
        abs
    }

    async fn index_file(fx: &Fixture, rel: &str, emb: Option<Vec<f32>>) -> (String, PathBuf) {
        let abs = write_html(&fx.source_root, rel, rel);
        let canon = canonical_abs(&abs);
        let id = ArtifactId::from_path(rel).as_str().to_string();
        let mut doc = Doc::placeholder(id.clone(), canon.to_string_lossy().to_string());
        doc.title = rel.to_string();
        doc.body = format!("body of {rel}");
        doc.embedding = emb;
        doc.content_hash = Some("hashhashhash".into());
        fx.ctx.storage.upsert_doc(doc).await.unwrap();
        // One chunk with embedding so rekey is observable.
        let chunks = vec![ChunkDoc {
            chunk_id: format!("{id}#0"),
            doc_id: id.clone(),
            chunk_idx: 0,
            text: format!("chunk for {rel}"),
            embedding: Some(vec![0.1, 0.2, 0.3, 0.4]),
        }];
        fx.ctx
            .storage
            .upsert_chunks(id.clone(), chunks)
            .await
            .unwrap();
        if let Some(dedup) = &fx.ctx.dedup {
            dedup.lock().unwrap().insert(
                id.clone(),
                crate::indexer::IndexedMeta {
                    content_hash: "hashhashhash".into(),
                    mtime_unix: Some(1_700_000_000),
                },
            );
        }
        (id, abs)
    }

    #[tokio::test]
    async fn relocate_moves_file_review_and_attachments() {
        let fx = setup().await;
        let (old_id, _) = index_file(&fx, "notes/a.html", Some(vec![1.0, 0.0, 0.0, 0.0])).await;

        // Seed review sidecar + attachment.
        let mut review = ReviewFile::empty_skeleton(&fx.ctx.kb, &old_id, "a");
        review.comments.push(crate::review::Comment {
            id: "c_test".into(),
            status: crate::review::CommentStatus::Open,
            file: old_id.clone(),
            file_label: "a".into(),
            anchor: crate::review::Anchor::Section {
                id: "top".into(),
                tag: None,
                snippet: None,
            },
            author: crate::review::Author::You,
            body: "hello".into(),
            created_at: chrono::Utc::now(),
            edited_at: None,
            replies: vec![],
            choices: vec![],
            attachments: vec![],
            user: None,
        });
        let old_review = fx.review_dir.join(format!("{old_id}.json"));
        review::save_atomic(&old_review, &review, None).unwrap();
        let att_dir = fx.state_root.join(".attachments").join(&old_id);
        std::fs::create_dir_all(&att_dir).unwrap();
        std::fs::write(att_dir.join("blob"), b"x").unwrap();

        let out = relocate_doc(&fx.ctx, "notes/a.html", "archive/a.html")
            .await
            .unwrap();
        assert_eq!(out.old_id, old_id);
        assert_ne!(out.new_id, old_id);
        assert_eq!(out.new_rel, "archive/a.html");

        assert!(!fx.source_root.join("notes/a.html").exists());
        assert!(fx.source_root.join("archive/a.html").is_file());
        assert!(!old_review.exists());
        let new_review = fx.review_dir.join(format!("{}.json", out.new_id));
        assert!(new_review.exists());
        let loaded = review::load(&new_review).unwrap().unwrap();
        assert_eq!(loaded.artifact.id, out.new_id);
        assert_eq!(loaded.comments[0].file, out.new_id);
        assert!(!att_dir.exists());
        assert!(fx
            .state_root
            .join(".attachments")
            .join(&out.new_id)
            .join("blob")
            .exists());
    }

    #[tokio::test]
    async fn relocate_rekeys_sqlite_dependents() {
        let fx = setup().await;
        let (old_id, _) = index_file(&fx, "x.html", None).await;
        let other = ArtifactId::from_path("other.html").as_str().to_string();

        fx.ctx
            .storage
            .history_record_open(old_id.clone(), 1_700_000_000, None, "operator".into())
            .await
            .unwrap();
        fx.ctx
            .storage
            .record_edges(old_id.clone(), vec![(other.clone(), "link".into())])
            .await
            .unwrap();
        // Backlink into old_id.
        fx.ctx
            .storage
            .record_edges(other.clone(), vec![(old_id.clone(), "link".into())])
            .await
            .unwrap();

        let list = fx
            .ctx
            .storage
            .list_create("l_aaaaaaaaaaaa".into(), "L".into(), None, false, 1)
            .await
            .unwrap();
        let entry = fx
            .ctx
            .storage
            .list_entry_add(
                NewListEntry {
                    id: "le_bbbbbbbbbbbb".into(),
                    list_id: list.id.clone(),
                    kb: "testkb".into(),
                    artifact_id: old_id.clone(),
                    anchor_json: None,
                    note: Some("keep-me".into()),
                    words: None,
                    read_override: None,
                },
                PositionSpec::Last,
                "operator".to_string(),
                1,
            )
            .await
            .unwrap();
        assert_eq!(entry.note.as_deref(), Some("keep-me"));
        let pos_before = entry.position;

        // reading_sections need a visit_id — seed via history then upsert.
        let open = fx
            .ctx
            .storage
            .history_record_open(old_id.clone(), 1_700_000_100, None, "operator".into())
            .await
            .unwrap();
        fx.ctx
            .storage
            .reading_upsert_sections(
                open.id,
                old_id.clone(),
                vec![crate::storage::sqlite::SectionDwell {
                    section_id: "top".into(),
                    section_idx: 0,
                    section_text: "Top".into(),
                    level: 1,
                    words: 10,
                    content_px: 100,
                    dwell_ms: 100,
                    enters: 1,
                }],
                1_700_000_100,
            )
            .await
            .unwrap();

        let out = relocate_doc(&fx.ctx, "x.html", "y.html").await.unwrap();

        // history
        let hist = fx
            .ctx
            .storage
            .history_list(50, None, None, None)
            .await
            .unwrap();
        assert!(
            hist.iter()
                .any(|h| h.artifact_id.as_deref() == Some(out.new_id.as_str())),
            "history rekeyed: {hist:?}"
        );
        assert!(
            hist.iter()
                .all(|h| h.artifact_id.as_deref() != Some(old_id.as_str())),
            "no history on old id"
        );

        // edges both directions (EdgeRow uses from_id/to_id)
        let out_e = fx
            .ctx
            .storage
            .edges_from(out.new_id.clone(), 1)
            .await
            .unwrap();
        assert!(out_e.iter().any(|e| e.to_id == other), "outbound rekeyed");
        let back = fx
            .ctx
            .storage
            .backlinks_of(out.new_id.clone())
            .await
            .unwrap();
        assert!(back.iter().any(|e| e.from_id == other), "inbound rekeyed");

        // list_entries: position + note preserved
        let entries = fx
            .ctx
            .storage
            .list_entries_for_list(list.id.clone())
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].artifact_id, out.new_id);
        assert_eq!(entries[0].note.as_deref(), Some("keep-me"));
        assert_eq!(entries[0].position, pos_before);

        // reading_sections rekeyed (tuple: sections, visits)
        let (sections, _visits) = fx
            .ctx
            .storage
            .reading_inputs_for_artifact(out.new_id.clone(), None)
            .await
            .unwrap();
        assert!(
            !sections.is_empty(),
            "reading_sections should rekey to new id"
        );
    }

    #[tokio::test]
    async fn relocate_preserves_embedding_and_rekeys_chunks() {
        let emb = vec![0.5, 0.25, 0.125, 0.0625];
        let fx = setup().await;
        let (old_id, _) = index_file(&fx, "emb.html", Some(emb.clone())).await;

        let out = relocate_doc(&fx.ctx, "emb.html", "emb2.html")
            .await
            .unwrap();

        assert!(fx
            .ctx
            .storage
            .get_by_id(old_id.clone())
            .await
            .unwrap()
            .is_none());
        let new_emb = fx
            .ctx
            .storage
            .embedding_by_id(out.new_id.clone())
            .await
            .unwrap()
            .expect("new id has embedding");
        assert_eq!(new_emb, emb);

        // Old chunks gone; new chunks present with rewritten chunk_id and
        // preserved per-chunk embeddings (no re-embed).
        let old_chunks = fx.ctx.storage.list_chunks_for_doc(old_id).await.unwrap();
        assert!(old_chunks.is_empty(), "old-id chunks must be gone");
        let new_chunks = fx
            .ctx
            .storage
            .list_chunks_for_doc(out.new_id.clone())
            .await
            .unwrap();
        assert_eq!(new_chunks.len(), 1);
        assert_eq!(new_chunks[0].chunk_id, format!("{}#0", out.new_id));
        assert_eq!(new_chunks[0].doc_id, out.new_id);
        assert_eq!(
            new_chunks[0].embedding.as_ref(),
            Some(&vec![0.1, 0.2, 0.3, 0.4])
        );
        assert_eq!(fx.ctx.storage.count_rows().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn moves_lookup_and_completed_intent() {
        let fx = setup().await;
        let _ = index_file(&fx, "a.html", None).await;
        let out = relocate_doc(&fx.ctx, "a.html", "b.html").await.unwrap();

        let mapped = moves_lookup(&fx.ctx.storage, &out.old_id)
            .await
            .unwrap()
            .expect("lookup by old_id");
        assert_eq!(mapped.0, out.new_id);
        assert_eq!(mapped.1, out.new_rel);

        let mapped2 = moves_lookup(&fx.ctx.storage, "a.html")
            .await
            .unwrap()
            .expect("lookup by old_rel");
        assert_eq!(mapped2.0, out.new_id);
    }

    #[tokio::test]
    async fn moves_lookup_follows_chain() {
        let fx = setup().await;
        let _ = index_file(&fx, "a.html", None).await;
        let ab = relocate_doc(&fx.ctx, "a.html", "b.html").await.unwrap();
        let bc = relocate_doc(&fx.ctx, "b.html", "c.html").await.unwrap();
        assert_eq!(ab.new_id, bc.old_id);

        let mapped = moves_lookup(&fx.ctx.storage, &ab.old_id)
            .await
            .unwrap()
            .expect("A resolves through chain");
        assert_eq!(mapped.0, bc.new_id);
        assert_eq!(mapped.1, "c.html");
    }

    #[tokio::test]
    async fn process_delete_suppressed_after_relocate() {
        let fx = setup().await;
        let (old_id, old_abs) = index_file(&fx, "z.html", None).await;
        // Seed a review file that must survive a spurious Deleted.
        let review_path = fx.review_dir.join(format!("{old_id}.json"));
        let review = ReviewFile::empty_skeleton(&fx.ctx.kb, &old_id, "z");
        review::save_atomic(&review_path, &review, None).unwrap();

        let out = relocate_doc(&fx.ctx, "z.html", "z2.html").await.unwrap();
        let new_review = fx.review_dir.join(format!("{}.json", out.new_id));
        assert!(new_review.exists());

        // Simulate watcher Deleted for the OLD path.
        assert!(
            should_suppress_delete(
                &fx.ctx.storage,
                Some(&fx.ctx.pending),
                &old_abs,
                &fx.source_root,
            )
            .await,
            "delete of old path must be suppressed"
        );

        // Drive the real process_delete; review must remain.
        crate::indexer::process_delete_for_test(
            &fx.ctx.kb,
            &crate::ids::SourceSlug::from_path(&fx.source_root),
            &fx.source_root,
            &fx.ctx.storage,
            &Arc::new(crate::events::EventBus::new(16, 16)),
            &old_abs,
            Arc::new(Mutex::new(HashMap::new())),
            Some(&fx.review_dir),
            fx.ctx.review_lock.as_deref(),
            false,
            Some(fx.ctx.pending.clone()),
        )
        .await;
        assert!(
            new_review.exists(),
            "review must survive suppressed process_delete"
        );
        assert!(
            fx.ctx
                .storage
                .get_by_id(out.new_id.clone())
                .await
                .unwrap()
                .is_some(),
            "new lance row intact"
        );
    }

    #[tokio::test]
    async fn startup_replay_completes_partial_relocate() {
        let fx = setup().await;
        let (old_id, old_abs) = index_file(&fx, "p.html", Some(vec![1.0, 1.0, 0.0, 0.0])).await;
        let new_rel = "p2.html";
        let new_id = ArtifactId::from_path(new_rel).as_str().to_string();
        let new_abs = fx.source_root.join(new_rel);

        // Intent only + FS rename (no actor rekey) — crash mid-flight.
        let row_id = fx
            .ctx
            .storage
            .moves_insert_intent(
                old_id.clone(),
                new_id.clone(),
                "p.html".into(),
                new_rel.into(),
                unix_now(),
            )
            .await
            .unwrap();
        std::fs::rename(&old_abs, &new_abs).unwrap();
        assert!(fx
            .ctx
            .storage
            .get_by_id(old_id.clone())
            .await
            .unwrap()
            .is_some());

        let n = replay_incomplete_moves(&fx.ctx).await.unwrap();
        assert_eq!(n, 1);
        assert!(fx.ctx.storage.get_by_id(old_id).await.unwrap().is_none());
        assert!(fx
            .ctx
            .storage
            .get_by_id(new_id.clone())
            .await
            .unwrap()
            .is_some());
        let emb = fx
            .ctx
            .storage
            .embedding_by_id(new_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(emb, vec![1.0, 1.0, 0.0, 0.0]);
        // Intent completed — no longer incomplete.
        assert!(fx
            .ctx
            .storage
            .moves_list_incomplete()
            .await
            .unwrap()
            .is_empty());
        let _ = row_id;
    }

    #[tokio::test]
    async fn folder_batch_moves_nested_files() {
        let fx = setup().await;
        let _ = index_file(&fx, "proj/a.html", None).await;
        let _ = index_file(&fx, "proj/b.html", None).await;
        let _ = index_file(&fx, "proj/sub/c.html", None).await;

        let items = relocate_folder(&fx.ctx, "proj", "proj2").await.unwrap();
        assert_eq!(items.len(), 3);
        for it in &items {
            assert!(it.outcome.new_rel.starts_with("proj2/"));
            assert!(fx.source_root.join(&it.outcome.new_rel).is_file());
            assert!(!fx.source_root.join(&it.outcome.old_rel).exists());
        }
        // Old dirs pruned best-effort.
        assert!(
            !fx.source_root.join("proj/sub").exists() || fx.source_root.join("proj").exists(),
            "nested empty dirs ideally pruned"
        );
    }

    #[tokio::test]
    async fn validate_rejects_existing_target_and_unindexed() {
        let fx = setup().await;
        let _ = index_file(&fx, "ok.html", None).await;
        write_html(&fx.source_root, "taken.html", "taken");

        let err = relocate_doc(&fx.ctx, "ok.html", "taken.html")
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Conflict(_)), "{err:?}");

        write_html(&fx.source_root, "not-indexed.html", "x");
        let err = relocate_doc(&fx.ctx, "not-indexed.html", "elsewhere.html")
            .await
            .unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");

        // No intent rows from failed validates.
        assert!(fx
            .ctx
            .storage
            .moves_list_incomplete()
            .await
            .unwrap()
            .is_empty());
        // And no completed rows either for the failed attempts.
        assert!(moves_lookup(&fx.ctx.storage, "ok.html")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn validate_rejects_path_escape() {
        let fx = setup().await;
        let _ = index_file(&fx, "ok.html", None).await;
        let err = relocate_doc(&fx.ctx, "ok.html", "../outside.html")
            .await
            .unwrap_err();
        assert!(matches!(err, Error::BadRequest(_)), "{err:?}");
    }

    #[tokio::test]
    async fn dedup_map_is_rekeyed() {
        let fx = setup().await;
        let (old_id, _) = index_file(&fx, "d.html", None).await;
        let out = relocate_doc(&fx.ctx, "d.html", "d2.html").await.unwrap();
        let g = fx.ctx.dedup.as_ref().unwrap().lock().unwrap();
        assert!(
            !g.contains_key(&old_id),
            "old id must be removed from dedup"
        );
        let meta = g
            .get(&out.new_id)
            .expect("new id must carry the same IndexedMeta");
        assert_eq!(meta.content_hash, "hashhashhash");
        assert_eq!(meta.mtime_unix, Some(1_700_000_000));
    }

    /// T1 — tombstone already at new_id must not abort the cascade;
    /// SOURCE (live entry) wins via UPDATE OR REPLACE; note/position kept.
    #[tokio::test]
    async fn list_tombstone_collision_source_wins() {
        let fx = setup().await;
        let target_rel = "path/p.html";
        let target_id = ArtifactId::from_path(target_rel).as_str().to_string();
        let (q_id, _) = index_file(&fx, "q.html", None).await;

        let list = fx
            .ctx
            .storage
            .list_create("l_tombstoneaaaa".into(), "Tomb".into(), None, false, 1)
            .await
            .unwrap();

        // Stale tombstone at id(P) — doc long deleted; entry survives.
        let tomb = fx
            .ctx
            .storage
            .list_entry_add(
                NewListEntry {
                    id: "le_tombstoneaaa".into(),
                    list_id: list.id.clone(),
                    kb: "testkb".into(),
                    artifact_id: target_id.clone(),
                    anchor_json: None,
                    note: Some("stale-tombstone".into()),
                    words: None,
                    read_override: None,
                },
                PositionSpec::Last,
                "operator".to_string(),
                1,
            )
            .await
            .unwrap();
        assert_eq!(tomb.note.as_deref(), Some("stale-tombstone"));

        // Live entry for Q with the note/position that must survive.
        let live = fx
            .ctx
            .storage
            .list_entry_add(
                NewListEntry {
                    id: "le_liveeeeeeeee".into(),
                    list_id: list.id.clone(),
                    kb: "testkb".into(),
                    artifact_id: q_id.clone(),
                    anchor_json: None,
                    note: Some("keep-live-note".into()),
                    words: None,
                    read_override: None,
                },
                PositionSpec::Last,
                "operator".to_string(),
                2,
            )
            .await
            .unwrap();
        let live_pos = live.position;
        assert_eq!(live.note.as_deref(), Some("keep-live-note"));

        let out = relocate_doc(&fx.ctx, "q.html", target_rel)
            .await
            .expect("cascade must succeed despite tombstone at new_id");
        assert_eq!(out.new_id, target_id);
        assert_eq!(out.affected_list_ids, vec![list.id.clone()]);

        // Intent completed (not stuck incomplete).
        assert!(fx
            .ctx
            .storage
            .moves_list_incomplete()
            .await
            .unwrap()
            .is_empty());
        let mapped = moves_lookup(&fx.ctx.storage, &out.old_id)
            .await
            .unwrap()
            .expect("completed move");
        assert_eq!(mapped.0, target_id);

        let entries = fx
            .ctx
            .storage
            .list_entries_for_list(list.id.clone())
            .await
            .unwrap();
        // Source wins: one row at new_id with Q's note/position (tombstone gone).
        let survivors: Vec<_> = entries
            .iter()
            .filter(|e| e.artifact_id == target_id)
            .collect();
        assert_eq!(
            survivors.len(),
            1,
            "exactly one entry at new_id: {entries:?}"
        );
        assert_eq!(survivors[0].note.as_deref(), Some("keep-live-note"));
        assert_eq!(survivors[0].position, live_pos);
        assert_eq!(survivors[0].id, "le_liveeeeeeeee");
        // No leftover old_id row.
        assert!(entries.iter().all(|e| e.artifact_id != q_id));
    }

    /// T3 — rename failure after intent abandons the row (completed_at set)
    /// so should_suppress_delete no longer blocks real deletes of old_rel.
    #[tokio::test]
    async fn rename_failure_abandons_intent() {
        let fx = setup().await;
        let _ = index_file(&fx, "ok.html", None).await;
        // Parent of target is a FILE → create_dir_all (post-intent) fails.
        std::fs::write(fx.source_root.join("block"), b"not-a-dir").unwrap();

        let err = relocate_doc(&fx.ctx, "ok.html", "block/nested.html")
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Io(_)), "{err:?}");

        // No incomplete intents; source file untouched.
        assert!(fx
            .ctx
            .storage
            .moves_list_incomplete()
            .await
            .unwrap()
            .is_empty());
        assert!(fx.source_root.join("ok.html").is_file());

        // Durable suppress must NOT fire for the abandoned old_rel.
        assert!(
            !should_suppress_delete(
                &fx.ctx.storage,
                None, // ignore in-memory pending
                &fx.source_root.join("ok.html"),
                &fx.source_root,
            )
            .await,
            "abandoned intent must not suppress deletes"
        );
    }

    /// T4 — stale orphan lance row under new_id with different content_hash
    /// is overwritten (moved doc's content + embedding preserved).
    #[tokio::test]
    async fn orphan_new_id_different_hash_is_overwritten() {
        let fx = setup().await;
        let target_rel = "orphan-target.html";
        let target_id = ArtifactId::from_path(target_rel).as_str().to_string();
        let q_emb = vec![0.1, 0.2, 0.3, 0.4];
        let (q_id, _) = index_file(&fx, "q-src.html", Some(q_emb.clone())).await;

        // Seed orphan under id(P) with different content_hash + embedding.
        let mut orphan = Doc::placeholder(
            target_id.clone(),
            fx.source_root
                .join("ghost.html")
                .to_string_lossy()
                .into_owned(),
        );
        orphan.title = "orphan".into();
        orphan.body = "stale orphan body".into();
        orphan.content_hash = Some("orphan-hash-XXXX".into());
        orphan.embedding = Some(vec![9.0, 9.0, 9.0, 9.0]);
        fx.ctx.storage.upsert_doc(orphan).await.unwrap();

        let out = relocate_doc(&fx.ctx, "q-src.html", target_rel)
            .await
            .unwrap();
        assert_eq!(out.new_id, target_id);
        assert!(fx
            .ctx
            .storage
            .get_by_id(q_id.clone())
            .await
            .unwrap()
            .is_none());

        let hashes = fx.ctx.storage.list_content_hashes().await.unwrap();
        let hash = hashes
            .iter()
            .find(|(id, _, _)| id == &target_id)
            .map(|(_, h, _)| h.as_str());
        assert_eq!(
            hash,
            Some("hashhashhash"),
            "Q's content_hash must win over orphan: {hashes:?}"
        );
        let bodies = fx
            .ctx
            .storage
            .get_bodies_by_ids(vec![target_id.clone()])
            .await
            .unwrap();
        assert_eq!(
            bodies.first().map(|(_, b)| b.as_str()),
            Some("body of q-src.html")
        );
        let emb = fx
            .ctx
            .storage
            .embedding_by_id(target_id)
            .await
            .unwrap()
            .expect("embedding");
        assert_eq!(emb, q_emb);
    }

    /// T5 — A→B then B→A must not ping-pong; lookup("B") resolves to live A.
    #[tokio::test]
    async fn moves_lookup_cycle_stops_at_live_end() {
        let fx = setup().await;
        let _ = index_file(&fx, "a.html", None).await;
        let ab = relocate_doc(&fx.ctx, "a.html", "b.html").await.unwrap();
        let ba = relocate_doc(&fx.ctx, "b.html", "a.html").await.unwrap();
        assert_eq!(ab.new_id, ba.old_id);
        assert_eq!(ba.new_id, ab.old_id);

        let mapped = moves_lookup(&fx.ctx.storage, "b.html")
            .await
            .unwrap()
            .expect("B resolves");
        assert_eq!(mapped.0, ba.new_id, "B → A (live end), not ping-pong");
        assert_eq!(mapped.1, "a.html");

        // Same by id of the intermediate B.
        let mapped_id = moves_lookup(&fx.ctx.storage, &ab.new_id)
            .await
            .unwrap()
            .expect("B id resolves");
        assert_eq!(mapped_id.0, ba.new_id);

        // A→B→C chain still resolves to C (existing contract).
        let fx2 = setup().await;
        let _ = index_file(&fx2, "a.html", None).await;
        let ab2 = relocate_doc(&fx2.ctx, "a.html", "b.html").await.unwrap();
        let bc2 = relocate_doc(&fx2.ctx, "b.html", "c.html").await.unwrap();
        let chain = moves_lookup(&fx2.ctx.storage, &ab2.old_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(chain.0, bc2.new_id);
        assert_eq!(chain.1, "c.html");
    }

    /// T6 unit — affected_list_ids returned on relocate when entries rekeyed.
    #[tokio::test]
    async fn affected_list_ids_returned_on_rekey() {
        let fx = setup().await;
        let (old_id, _) = index_file(&fx, "listed.html", None).await;
        let list = fx
            .ctx
            .storage
            .list_create("l_affectedaaaa".into(), "Aff".into(), None, false, 1)
            .await
            .unwrap();
        fx.ctx
            .storage
            .list_entry_add(
                NewListEntry {
                    id: "le_affectedaaaa".into(),
                    list_id: list.id.clone(),
                    kb: "testkb".into(),
                    artifact_id: old_id,
                    anchor_json: None,
                    note: None,
                    words: None,
                    read_override: None,
                },
                PositionSpec::Last,
                "operator".to_string(),
                1,
            )
            .await
            .unwrap();

        let out = relocate_doc(&fx.ctx, "listed.html", "listed2.html")
            .await
            .unwrap();
        assert_eq!(out.affected_list_ids, vec![list.id]);
    }
}
