//! v0.14 S4 — small LRU cache for session "touches" scans.
//!
//! Each cache entry pairs a transcript-key with its
//! `extract_touched_ids` output. The key is
//! `(kb, artifact_id, mtime_unix)` so an mtime bump (re-indexed
//! transcript) naturally invalidates the entry. Cap is intentionally
//! small (256 entries); the dominant traffic pattern is the SPA atlas
//! overlay refetching the same top-10 sessions on each poll, so a tiny
//! ring is enough to absorb the hot set without dragging in a
//! dedicated `lru` crate. Mirrors `embed_cache::QueryEmbedCache`'s
//! "linear scan + push_front" deque shape.

use kb_core::sessions::{TouchedArtifact, TouchesConfidence};
use std::collections::VecDeque;
use std::sync::Mutex;

pub const DEFAULT_CAPACITY: usize = 256;

#[derive(Debug, PartialEq, Eq, Clone)]
struct CacheKey {
    kb: String,
    artifact_id: String,
    mtime_unix: i64,
}

#[derive(Debug, Clone)]
pub struct CachedTouches {
    pub artifact_ids: Vec<String>,
    pub confidence: TouchesConfidence,
    /// CT-A6 — same ids as `artifact_ids`, each tagged with its own
    /// join-tier. Additive; kept alongside `artifact_ids` rather than
    /// replacing it so every pre-existing cache consumer stays untouched.
    pub artifacts: Vec<TouchedArtifact>,
}

pub struct TouchesCache {
    inner: Mutex<VecDeque<(CacheKey, CachedTouches)>>,
    capacity: usize,
}

impl TouchesCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
        }
    }

    pub fn get(&self, kb: &str, artifact_id: &str, mtime_unix: i64) -> Option<CachedTouches> {
        let key = CacheKey {
            kb: kb.to_string(),
            artifact_id: artifact_id.to_string(),
            mtime_unix,
        };
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let pos = g.iter().position(|(k, _)| k == &key)?;
        let entry = g.remove(pos).expect("position just located");
        g.push_front(entry.clone());
        Some(entry.1)
    }

    pub fn put(&self, kb: &str, artifact_id: &str, mtime_unix: i64, value: CachedTouches) {
        let key = CacheKey {
            kb: kb.to_string(),
            artifact_id: artifact_id.to_string(),
            mtime_unix,
        };
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pos) = g.iter().position(|(k, _)| k == &key) {
            g.remove(pos);
        }
        if g.len() == self.capacity {
            g.pop_back();
        }
        g.push_front((key, value));
    }

    #[cfg(test)]
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

impl Default for TouchesCache {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cached(ids: &[&str]) -> CachedTouches {
        CachedTouches {
            artifact_ids: ids.iter().map(|s| s.to_string()).collect(),
            confidence: TouchesConfidence::Exact,
            artifacts: ids
                .iter()
                .map(|s| TouchedArtifact {
                    id: s.to_string(),
                    confidence: TouchesConfidence::Exact,
                })
                .collect(),
        }
    }

    #[test]
    fn put_then_get_round_trips() {
        let c = TouchesCache::default();
        c.put("kb1", "art-a", 100, cached(&["x"]));
        let hit = c.get("kb1", "art-a", 100).unwrap();
        assert_eq!(hit.artifact_ids, vec!["x"]);
    }

    #[test]
    fn miss_on_mtime_mismatch() {
        let c = TouchesCache::default();
        c.put("kb1", "art-a", 100, cached(&["x"]));
        assert!(c.get("kb1", "art-a", 101).is_none());
    }

    #[test]
    fn miss_on_kb_mismatch() {
        let c = TouchesCache::default();
        c.put("kb1", "art-a", 100, cached(&["x"]));
        assert!(c.get("kb2", "art-a", 100).is_none());
    }

    #[test]
    fn evicts_oldest_when_at_capacity() {
        let c = TouchesCache::new(2);
        c.put("kb1", "a", 1, cached(&["a"]));
        c.put("kb1", "b", 1, cached(&["b"]));
        c.put("kb1", "c", 1, cached(&["c"]));
        assert_eq!(c.len(), 2);
        assert!(c.get("kb1", "a", 1).is_none(), "'a' was evicted");
        assert!(c.get("kb1", "b", 1).is_some());
        assert!(c.get("kb1", "c", 1).is_some());
    }

    #[test]
    fn get_promotes_to_most_recently_used() {
        let c = TouchesCache::new(2);
        c.put("kb1", "a", 1, cached(&["a"]));
        c.put("kb1", "b", 1, cached(&["b"]));
        // Touch 'a' → it's now MRU; the next put should evict 'b'.
        let _ = c.get("kb1", "a", 1);
        c.put("kb1", "c", 1, cached(&["c"]));
        assert!(c.get("kb1", "a", 1).is_some(), "'a' survived as MRU");
        assert!(c.get("kb1", "b", 1).is_none(), "'b' was evicted");
        assert!(c.get("kb1", "c", 1).is_some());
    }

    #[test]
    fn put_replaces_existing_key() {
        let c = TouchesCache::default();
        c.put("kb1", "a", 1, cached(&["first"]));
        c.put("kb1", "a", 1, cached(&["second"]));
        assert_eq!(c.len(), 1);
        assert_eq!(c.get("kb1", "a", 1).unwrap().artifact_ids, vec!["second"]);
    }
}
