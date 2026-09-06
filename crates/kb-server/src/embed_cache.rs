//! Daemon-wide query-embedding LRU cache + an async wrapper around the
//! per-kb embedder. The synchronous `embed_one()` round-trip through the
//! `kb-embedder` IPC subprocess takes ~50–100 ms per call; repeating it
//! on every keystroke (and across kbs that share the same model) was
//! the dominant cost in `/api/search` and `/api/memory/recall`.
//!
//! Three layered wins:
//!
//! 1. **LRU memoisation.** Keyed on (model_name, query). Same query
//!    → return the cached vector immediately. Capacity is modest
//!    (`DEFAULT_CAPACITY` = 1024) so a linear-scan deque is cheaper than
//!    dragging in the `lru` crate (a 1024-scan is noise next to a ~100 ms
//!    embed).
//!
//! 2. **`spawn_blocking` around the embed call.** The std `Mutex` on
//!    the embedder is held across a blocking IPC round-trip; doing that
//!    on a tokio worker thread starves the runtime. The helper wraps
//!    the call so the worker is freed while we wait.
//!
//! 3. **Disk persistence** (`QueryEmbedCache::load_or_new` / `save`).
//!    The in-memory cache is wiped on every (re)start — including the
//!    CE-track in-process config restart — so the top `PERSIST_CAP` entries
//!    are saved on shutdown and reloaded on boot, keeping hot queries warm
//!    across restarts. A genuinely-novel query's first embed is still
//!    irreducible; persistence only spares repeats.
//!
//! The model name is part of the cache key because two kbs with the
//! same `embedding_model` produce bit-identical vectors for the same
//! input (`embed_one` is deterministic — see
//! `kb_core::embed::tests::embed_one_is_deterministic`). It is ALSO why the
//! persisted file stores + re-interns the model name: two distinct models can
//! share a dim, so a vector served under the wrong model key is silent garbage.

use kb_core::embed::{model_info, Embedder};
use kb_core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// In-memory LRU capacity. 1024 (was 256) — a 1024-dim bge-large vector is
/// ~4 KB, so 1024 entries ≈ 4 MB resident, negligible — to cut eviction
/// during high-cardinality agent sessions.
pub const DEFAULT_CAPACITY: usize = 1024;

/// How many of the most-recently-used entries persist to disk on shutdown
/// (newest-first). Capped below the in-memory capacity to bound the on-disk
/// file (~2.5 MB of JSON f32 at bge-large width) and keep the shutdown rewrite
/// fast. The LRU front is the most likely to be re-issued, so the top
/// `PERSIST_CAP` are the entries worth keeping warm across a restart — note
/// the high end of a full 1024-entry cache is intentionally NOT persisted.
const PERSIST_CAP: usize = 256;

/// On-disk schema version for the persisted cache file. Bump on a
/// format-incompatible change so `load_or_new` can reject old files.
const PERSIST_VERSION: u32 = 1;

#[derive(Debug, PartialEq, Eq, Clone)]
struct CacheKey {
    model: &'static str,
    query: String,
}

pub struct QueryEmbedCache {
    inner: Mutex<VecDeque<(CacheKey, Vec<f32>)>>,
    capacity: usize,
}

/// On-disk form. `model` is stored as an owned String and re-interned to the
/// `&'static` registry name on load (entries for a model no longer in
/// `SUPPORTED_MODELS` are dropped). `serde_json` round-trips `f32` exactly
/// (f32 → f64 → f32 is lossless), so no binary/base64 dep is needed.
#[derive(Serialize, Deserialize)]
struct PersistedCache {
    version: u32,
    /// Newest-first (LRU front first), so a load restores recency order.
    entries: Vec<PersistedEntry>,
}

#[derive(Serialize, Deserialize)]
struct PersistedEntry {
    model: String,
    query: String,
    vec: Vec<f32>,
}

impl QueryEmbedCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
        }
    }

    fn get(&self, key: &CacheKey) -> Option<Vec<f32>> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let pos = g.iter().position(|(k, _)| k == key)?;
        let entry = g.remove(pos).expect("position just located");
        g.push_front(entry.clone());
        Some(entry.1)
    }

    fn put(&self, key: CacheKey, vec: Vec<f32>) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pos) = g.iter().position(|(k, _)| *k == key) {
            g.remove(pos);
        }
        g.push_front((key, vec));
        while g.len() > self.capacity {
            g.pop_back();
        }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Build a cache of `capacity`, pre-warmed from a previously-saved file at
    /// `path`. The in-memory cache is wiped on every daemon (re)start —
    /// including the CE-track in-process config restart (invariant #13) — so
    /// without this, previously-hot queries re-pay the full embed after any
    /// restart. Best-effort: a missing file (first boot) or a corrupt/old-
    /// version file → an empty cache + a `warn!`, never fatal (mirrors
    /// `load_memory_policy`). Called from `KbHandles::new`, before any kb /
    /// embedder exists — it needs neither.
    pub fn load_or_new(capacity: usize, path: &Path) -> Self {
        let cache = Self::new(capacity);
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => return cache, // absent on first boot / after a wipe
        };
        let parsed: PersistedCache = match serde_json::from_slice::<PersistedCache>(&bytes) {
            Ok(p) if p.version == PERSIST_VERSION => p,
            Ok(_) => return cache, // older/newer schema — start clean
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e,
                    "query-embed cache: unreadable file, starting empty");
                return cache;
            }
        };
        let mut g = cache.inner.lock().unwrap_or_else(|e| e.into_inner());
        for entry in parsed.entries {
            if g.len() >= capacity {
                break;
            }
            // Re-intern the model to its &'static registry name. Dropping
            // unknown-model entries is SAFETY-CRITICAL: two distinct models can
            // share a dim (bge-base-en-v1.5 and jina-embeddings-v2-base-code
            // are both 768), and serving one's vector under the other's key
            // would return silently-wrong results with no dim error.
            let Some(model) = model_info(&entry.model).map(|m| m.name) else {
                continue;
            };
            g.push_back((
                CacheKey {
                    model,
                    query: entry.query,
                },
                entry.vec,
            ));
        }
        drop(g);
        cache
    }

    /// Atomically persist the most-recently-used `PERSIST_CAP` entries to
    /// `path` so they survive a restart. Snapshots under the lock then writes
    /// OUTSIDE it (a multi-MB file write must not stall in-flight searches).
    /// An empty cache writes nothing (leaves any prior file untouched). Uses
    /// `fsx::write_atomic` (tmp + rename + parent fsync), so a crash
    /// mid-write can't leave a torn file the next boot would choke on.
    pub fn save(&self, path: &Path) -> Result<()> {
        let entries: Vec<PersistedEntry> = {
            let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            g.iter()
                .take(PERSIST_CAP)
                .map(|(k, v)| PersistedEntry {
                    model: k.model.to_string(),
                    query: k.query.clone(),
                    vec: v.clone(),
                })
                .collect()
        };
        if entries.is_empty() {
            return Ok(());
        }
        let doc = PersistedCache {
            version: PERSIST_VERSION,
            entries,
        };
        let bytes = serde_json::to_vec(&doc)
            .map_err(|e| Error::Storage(format!("serialize embed cache: {e}")))?;
        kb_core::fsx::write_atomic(path, &bytes)
    }
}

impl Default for QueryEmbedCache {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl std::fmt::Debug for QueryEmbedCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryEmbedCache")
            .field("capacity", &self.capacity)
            .field("len", &self.len())
            .finish()
    }
}

pub struct EmbedOutcome {
    pub vec: Vec<f32>,
    /// Wall-time of the embedding step in ms. `0` on cache hit.
    pub embed_ms: u64,
    pub cache_hit: bool,
}

/// Embed `query` for `embedder`, consulting `cache` first. On miss runs
/// `embed_one` inside `spawn_blocking` so the IPC round-trip doesn't
/// pin a tokio worker thread, then writes the result to the cache.
pub async fn embed_query(
    cache: &Arc<QueryEmbedCache>,
    embedder: &Arc<Mutex<Embedder>>,
    query: &str,
) -> Result<EmbedOutcome> {
    let model = {
        let guard = embedder.lock().unwrap_or_else(|e| e.into_inner());
        guard.model_name()
    };
    let key = CacheKey {
        model,
        query: query.to_string(),
    };

    if let Some(vec) = cache.get(&key) {
        return Ok(EmbedOutcome {
            vec,
            embed_ms: 0,
            cache_hit: true,
        });
    }

    // Query-priority lane: mark this model as having a query contending for the
    // shared embedder, so a concurrent reindex yields the mutex between its
    // chunk mini-batches instead of making this latency-sensitive embed wait a
    // whole document (2026-07-03 recall-stall fix). Held across the embed;
    // dropped when the function returns.
    let _lane = kb_core::embed::QueryLaneGuard::enter(model);

    let embedder = embedder.clone();
    let query_owned = query.to_string();
    let started = Instant::now();
    let result = tokio::task::spawn_blocking(move || {
        let mut g = embedder.lock().unwrap_or_else(|e| e.into_inner());
        g.embed_one(&query_owned)
    })
    .await
    .map_err(|e| Error::Storage(format!("embed task join: {e}")))??;
    let embed_ms = started.elapsed().as_millis() as u64;

    cache.put(key, result.clone());
    Ok(EmbedOutcome {
        vec: result,
        embed_ms,
        cache_hit: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(model: &'static str, q: &str) -> CacheKey {
        CacheKey {
            model,
            query: q.to_string(),
        }
    }

    #[test]
    fn empty_cache_misses() {
        let c = QueryEmbedCache::new(4);
        assert!(c.is_empty());
        assert!(c.get(&key("bge-small-en-v1.5", "rust")).is_none());
    }

    #[test]
    fn hit_returns_stored_vector() {
        let c = QueryEmbedCache::new(4);
        c.put(key("bge-small-en-v1.5", "rust"), vec![1.0, 2.0]);
        assert_eq!(
            c.get(&key("bge-small-en-v1.5", "rust")),
            Some(vec![1.0, 2.0])
        );
    }

    #[test]
    fn model_scopes_cache_keys() {
        let c = QueryEmbedCache::new(4);
        c.put(key("bge-small-en-v1.5", "rust"), vec![1.0]);
        // Different model name → cache miss even though the query matches.
        assert!(c.get(&key("bge-base-en-v1.5", "rust")).is_none());
    }

    #[test]
    fn lru_evicts_least_recently_used() {
        let c = QueryEmbedCache::new(2);
        c.put(key("m", "a"), vec![1.0]);
        c.put(key("m", "b"), vec![2.0]);
        // Touch "a" so it's MRU.
        assert_eq!(c.get(&key("m", "a")), Some(vec![1.0]));
        c.put(key("m", "c"), vec![3.0]);
        // "b" was LRU → evicted.
        assert!(c.get(&key("m", "b")).is_none());
        assert_eq!(c.get(&key("m", "a")), Some(vec![1.0]));
        assert_eq!(c.get(&key("m", "c")), Some(vec![3.0]));
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn put_same_key_twice_does_not_grow() {
        let c = QueryEmbedCache::new(4);
        c.put(key("m", "a"), vec![1.0]);
        c.put(key("m", "a"), vec![9.0]);
        assert_eq!(c.len(), 1);
        assert_eq!(c.get(&key("m", "a")), Some(vec![9.0]));
    }

    #[test]
    fn default_capacity_matches_const() {
        let c = QueryEmbedCache::default();
        assert_eq!(c.capacity, DEFAULT_CAPACITY);
        assert_eq!(DEFAULT_CAPACITY, 1024);
    }

    #[test]
    fn persist_round_trip_preserves_entries_and_vectors() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ec.json");
        let c = QueryEmbedCache::new(8);
        // Real model names so the load-time model_info intern keeps them.
        c.put(key("bge-small-en-v1.5", "alpha"), vec![0.1, 0.2, 0.3]);
        c.put(key("bge-small-en-v1.5", "beta"), vec![0.4, 0.5, 0.6]);
        c.save(&path).unwrap();

        let loaded = QueryEmbedCache::load_or_new(8, &path);
        assert_eq!(loaded.len(), 2);
        assert_eq!(
            loaded.get(&key("bge-small-en-v1.5", "alpha")),
            Some(vec![0.1, 0.2, 0.3]),
            "f32 vectors round-trip bit-exactly through JSON"
        );
        assert_eq!(
            loaded.get(&key("bge-small-en-v1.5", "beta")),
            Some(vec![0.4, 0.5, 0.6])
        );
    }

    #[test]
    fn load_tolerates_missing_and_corrupt_file() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(QueryEmbedCache::load_or_new(4, &tmp.path().join("nope.json")).is_empty());
        let corrupt = tmp.path().join("bad.json");
        std::fs::write(&corrupt, b"{ not valid json ]").unwrap();
        assert!(QueryEmbedCache::load_or_new(4, &corrupt).is_empty());
        // Wrong schema version → start clean.
        let oldver = tmp.path().join("old.json");
        std::fs::write(&oldver, br#"{"version":999,"entries":[]}"#).unwrap();
        assert!(QueryEmbedCache::load_or_new(4, &oldver).is_empty());
    }

    #[test]
    fn load_skips_entries_for_unknown_models() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ec.json");
        // A made-up model must be dropped; the valid one kept.
        let json = r#"{"version":1,"entries":[
            {"model":"made-up-model-xyz","query":"q","vec":[1.0]},
            {"model":"bge-small-en-v1.5","query":"q","vec":[2.0]}
        ]}"#;
        std::fs::write(&path, json).unwrap();
        let c = QueryEmbedCache::load_or_new(8, &path);
        assert_eq!(c.len(), 1, "unknown-model entry is dropped on load");
        assert_eq!(c.get(&key("bge-small-en-v1.5", "q")), Some(vec![2.0]));
    }

    #[test]
    fn same_dim_different_model_entries_stay_isolated_across_persist() {
        // bge-base-en-v1.5 and jina-embeddings-v2-base-code are BOTH 768-dim.
        // Feeding one model's vector into the other's index returns
        // silently-wrong results (passes dim checks, no error). The model-name
        // key MUST keep them distinct through a save/load round-trip.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ec.json");
        let c = QueryEmbedCache::new(8);
        c.put(key("bge-base-en-v1.5", "same query"), vec![1.0; 4]);
        c.put(
            key("jina-embeddings-v2-base-code", "same query"),
            vec![2.0; 4],
        );
        c.save(&path).unwrap();

        let loaded = QueryEmbedCache::load_or_new(8, &path);
        assert_eq!(
            loaded.get(&key("bge-base-en-v1.5", "same query")),
            Some(vec![1.0; 4])
        );
        assert_eq!(
            loaded.get(&key("jina-embeddings-v2-base-code", "same query")),
            Some(vec![2.0; 4]),
            "same-dim different-model vectors must not collide after reload"
        );
    }

    #[test]
    fn save_writes_nothing_for_empty_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ec.json");
        QueryEmbedCache::new(4).save(&path).unwrap();
        assert!(!path.exists(), "an empty cache leaves no file behind");
    }
}
