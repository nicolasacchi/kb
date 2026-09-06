//! Lazy `(repo_id, commit_sha, path)` → full blame-region-list cache — the
//! "Gitiles' shape" ADR-4 calls for: the FIRST request for a given commit's
//! blame of a file pays the real `git blame --incremental` subprocess cost;
//! every LATER request for the SAME `(repo, commit, path)` — a different
//! caller narrowing to a different line range, a second viewer of the same
//! file, a re-open of the same historical ref — is served from this cache
//! instead. `super::blame_file` always fetches/caches the WHOLE file's
//! regions and slices to a requested line range in Rust afterwards (see
//! that fn's doc); this cache never stores a range-narrowed partial result.
//!
//! Bounded LRU, the same hand-rolled linear-scan shape as
//! `kb_server::embed_cache::QueryEmbedCache` — at [`DEFAULT_CAPACITY`]
//! entries a `VecDeque` scan is cheaper than pulling in the `lru` crate for
//! this daemon's first use of an LRU (see that module's doc for the
//! identical tradeoff argument, which applies unchanged here).
//!
//! # Why this key never goes stale
//!
//! The cache key is always a RESOLVED sha, never a moving ref —
//! `super::blame_file` resolves `HEAD`/branch names/etc. to a concrete sha
//! BEFORE ever touching this cache. Commits are immutable, so a hit is
//! correct forever; there is no invalidation problem, only eviction. A
//! DIRTY working-tree blame (uncommitted edits) is never given a key at all
//! — `blame_file` never calls this cache for that case.

use super::incremental::BlameRegion;
use std::collections::VecDeque;
use std::sync::Mutex;

/// Default LRU capacity — ADR-4's own "e.g. 512 entries." A region is a
/// handful of small strings, so even a generously-sized file's full blame
/// is a few hundred KB at most; 512 entries keeps the whole cache in the
/// low tens of MB even under adversarial pressure across many large repos.
pub const DEFAULT_CAPACITY: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
struct CacheKey {
    repo_id: i64,
    sha: String,
    path: String,
}

pub struct BlameCache {
    inner: Mutex<VecDeque<(CacheKey, Vec<BlameRegion>)>>,
    capacity: usize,
}

impl BlameCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
        }
    }

    pub fn get(&self, repo_id: i64, sha: &str, path: &str) -> Option<Vec<BlameRegion>> {
        let key = CacheKey {
            repo_id,
            sha: sha.to_string(),
            path: path.to_string(),
        };
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let pos = g.iter().position(|(k, _)| *k == key)?;
        let entry = g.remove(pos).expect("position just located");
        g.push_front(entry.clone());
        Some(entry.1)
    }

    pub fn put(&self, repo_id: i64, sha: &str, path: &str, regions: Vec<BlameRegion>) {
        let key = CacheKey {
            repo_id,
            sha: sha.to_string(),
            path: path.to_string(),
        };
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pos) = g.iter().position(|(k, _)| *k == key) {
            g.remove(pos);
        }
        g.push_front((key, regions));
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
}

impl Default for BlameCache {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl std::fmt::Debug for BlameCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlameCache")
            .field("capacity", &self.capacity)
            .field("len", &self.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(sha: &str) -> BlameRegion {
        BlameRegion {
            sha: sha.to_string(),
            orig_start: 1,
            final_start: 1,
            count: 1,
            author: "Alice".to_string(),
            ..BlameRegion::default()
        }
    }

    #[test]
    fn empty_cache_misses() {
        let c = BlameCache::new(4);
        assert!(c.is_empty());
        assert!(c.get(1, "deadbeef", "f.txt").is_none());
    }

    #[test]
    fn hit_returns_the_stored_regions() {
        let c = BlameCache::new(4);
        c.put(1, "deadbeef", "f.txt", vec![region("deadbeef")]);
        let got = c.get(1, "deadbeef", "f.txt").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].sha, "deadbeef");
    }

    #[test]
    fn repo_id_sha_and_path_all_scope_the_key() {
        let c = BlameCache::new(8);
        c.put(1, "sha-a", "f.txt", vec![region("sha-a")]);

        // Different repo_id, same sha+path → miss.
        assert!(c.get(2, "sha-a", "f.txt").is_none());
        // Same repo_id, different sha, same path → miss (a different
        // immutable commit is a genuinely different cache slot).
        assert!(c.get(1, "sha-b", "f.txt").is_none());
        // Same repo_id+sha, different path → miss.
        assert!(c.get(1, "sha-a", "g.txt").is_none());
        // The exact key still hits.
        assert!(c.get(1, "sha-a", "f.txt").is_some());
    }

    #[test]
    fn lru_evicts_least_recently_used() {
        let c = BlameCache::new(2);
        c.put(1, "a", "f.txt", vec![region("a")]);
        c.put(1, "b", "f.txt", vec![region("b")]);
        // Touch "a" so it becomes MRU.
        assert!(c.get(1, "a", "f.txt").is_some());
        c.put(1, "c", "f.txt", vec![region("c")]);
        // "b" was LRU → evicted.
        assert!(c.get(1, "b", "f.txt").is_none());
        assert!(c.get(1, "a", "f.txt").is_some());
        assert!(c.get(1, "c", "f.txt").is_some());
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn put_same_key_twice_does_not_grow_and_replaces_the_value() {
        let c = BlameCache::new(4);
        c.put(1, "a", "f.txt", vec![region("a")]);
        c.put(1, "a", "f.txt", vec![region("a"), region("a")]);
        assert_eq!(c.len(), 1);
        assert_eq!(c.get(1, "a", "f.txt").unwrap().len(), 2);
    }

    #[test]
    fn default_capacity_matches_const() {
        let c = BlameCache::default();
        assert_eq!(DEFAULT_CAPACITY, 512);
        // Fill past capacity and confirm it never exceeds DEFAULT_CAPACITY.
        for i in 0..(DEFAULT_CAPACITY + 10) {
            c.put(1, &format!("sha-{i}"), "f.txt", vec![region("x")]);
        }
        assert_eq!(c.len(), DEFAULT_CAPACITY);
    }
}
