//! W3.R-b — small LRU cache for `GET /api/sessions/{sid}/replay`.
//!
//! Building a replay timeline is the most expensive read in the sessions
//! surface: the capture HTML is read off disk (real captures reach ~33 MB),
//! the `<pre>` transcript is recovered, optionally scrubbed, parsed line by
//! line into beats, and every beat's raw path is resolved against the corpus.
//! The SPA scrubber re-fetches the same session while the operator drags the
//! playhead, so without a cache each drag pays the whole pipeline again.
//!
//! Shape is deliberately identical to [`crate::touches_cache`] — a
//! `Mutex<VecDeque>` LRU with a linear scan and `push_front` promotion. It is
//! a plain **process-local** cache: NOT a `StorageMsg`, not on the storage
//! actor's read lane, and it never bumps the index generation (invariant #15).
//!
//! ### Key
//!
//! `(kb, artifact_id, mtime_unix, scrub)`:
//!
//! * `artifact_id` is the NEWEST capture's artifact (invariant #11) — a fresh
//!   Stop capture mints a new artifact id, so the entry misses naturally;
//! * `mtime_unix` invalidates a re-indexed capture in place;
//! * `scrub` is the redaction posture tag. It is load-bearing for invariant
//!   #4: a loopback request may legitimately build an UNREDACTED timeline, and
//!   a later non-loopback request (whose secrets floor is forced on) must not
//!   be served that entry out of the cache. Different posture ⇒ different key
//!   ⇒ a real rebuild.
//!
//! Capacity is much smaller than `touches_cache`'s 256: one entry holds up to
//! [`REPLAY_MAX_EVENTS`](kb_core::sessions::replay::REPLAY_MAX_EVENTS) resolved
//! beats (hundreds of KB), so a big ring would be a memory hazard rather than
//! a win. 32 covers the "operator scrubs a handful of sessions" hot set.

use std::collections::VecDeque;
use std::sync::Mutex;

/// Entries retained. See the module docs for why this is 32 and not 256.
pub const DEFAULT_CAPACITY: usize = 32;

#[derive(Debug, PartialEq, Eq, Clone)]
struct CacheKey {
    kb: String,
    artifact_id: String,
    mtime_unix: i64,
    scrub: String,
}

/// A `(kb, artifact_id, mtime_unix, scrub)`-keyed LRU.
///
/// Generic over the cached value so the cache stays free of any route type
/// (the daemon hands it an `Arc<ResolvedReplay>`, cheap to clone on a hit).
pub struct ReplayCache<V: Clone> {
    inner: Mutex<VecDeque<(CacheKey, V)>>,
    capacity: usize,
}

impl<V: Clone> ReplayCache<V> {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
        }
    }

    pub fn get(&self, kb: &str, artifact_id: &str, mtime_unix: i64, scrub: &str) -> Option<V> {
        let key = CacheKey {
            kb: kb.to_string(),
            artifact_id: artifact_id.to_string(),
            mtime_unix,
            scrub: scrub.to_string(),
        };
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let pos = g.iter().position(|(k, _)| k == &key)?;
        let entry = g.remove(pos).expect("position just located");
        g.push_front(entry.clone());
        Some(entry.1)
    }

    pub fn put(&self, kb: &str, artifact_id: &str, mtime_unix: i64, scrub: &str, value: V) {
        let key = CacheKey {
            kb: kb.to_string(),
            artifact_id: artifact_id.to_string(),
            mtime_unix,
            scrub: scrub.to_string(),
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

impl<V: Clone> Default for ReplayCache<V> {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache() -> ReplayCache<String> {
        ReplayCache::default()
    }

    #[test]
    fn put_then_get_round_trips() {
        let c = cache();
        c.put("kb1", "art-a", 100, "", "beats".to_string());
        assert_eq!(c.get("kb1", "art-a", 100, "").unwrap(), "beats");
    }

    #[test]
    fn miss_on_mtime_mismatch() {
        let c = cache();
        c.put("kb1", "art-a", 100, "", "beats".to_string());
        assert!(c.get("kb1", "art-a", 101, "").is_none());
    }

    #[test]
    fn miss_on_kb_mismatch() {
        let c = cache();
        c.put("kb1", "art-a", 100, "", "beats".to_string());
        assert!(c.get("kb2", "art-a", 100, "").is_none());
    }

    /// Invariant #4: an unscrubbed (loopback) entry must NEVER be served to a
    /// request whose redaction posture differs.
    #[test]
    fn miss_on_scrub_posture_mismatch() {
        let c = cache();
        c.put("kb1", "art-a", 100, "", "raw prompt text".to_string());
        assert!(
            c.get("kb1", "art-a", 100, "s").is_none(),
            "a secrets-floor request must not read the unredacted entry"
        );
        c.put("kb1", "art-a", 100, "s", "redacted".to_string());
        assert_eq!(c.get("kb1", "art-a", 100, "s").unwrap(), "redacted");
        assert_eq!(
            c.get("kb1", "art-a", 100, "").unwrap(),
            "raw prompt text",
            "both postures coexist as distinct entries"
        );
    }

    #[test]
    fn evicts_oldest_when_at_capacity() {
        let c: ReplayCache<String> = ReplayCache::new(2);
        c.put("kb1", "a", 1, "", "a".to_string());
        c.put("kb1", "b", 1, "", "b".to_string());
        c.put("kb1", "c", 1, "", "c".to_string());
        assert_eq!(c.len(), 2);
        assert!(c.get("kb1", "a", 1, "").is_none(), "'a' was evicted");
        assert!(c.get("kb1", "b", 1, "").is_some());
        assert!(c.get("kb1", "c", 1, "").is_some());
    }

    #[test]
    fn get_promotes_to_most_recently_used() {
        let c: ReplayCache<String> = ReplayCache::new(2);
        c.put("kb1", "a", 1, "", "a".to_string());
        c.put("kb1", "b", 1, "", "b".to_string());
        let _ = c.get("kb1", "a", 1, "");
        c.put("kb1", "c", 1, "", "c".to_string());
        assert!(c.get("kb1", "a", 1, "").is_some(), "'a' survived as MRU");
        assert!(c.get("kb1", "b", 1, "").is_none(), "'b' was evicted");
        assert!(c.get("kb1", "c", 1, "").is_some());
    }

    #[test]
    fn put_replaces_existing_key() {
        let c = cache();
        c.put("kb1", "a", 1, "", "first".to_string());
        c.put("kb1", "a", 1, "", "second".to_string());
        assert_eq!(c.len(), 1);
        assert_eq!(c.get("kb1", "a", 1, "").unwrap(), "second");
    }
}
