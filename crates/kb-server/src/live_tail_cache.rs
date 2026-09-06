//! W7 (sessions-rethink R15/LF-4) — a tiny LRU over live-tail incremental
//! interpretation state, so a client polling `GET /api/sessions/{sid}/live`
//! every few seconds doesn't pay a bootstrap-window reparse on every hit.
//!
//! **Never load-bearing** (the design's kill-criterion 3): a miss — cold
//! start, a `from` that doesn't match any cached offset, server restart, or
//! simple eviction — always falls back to the STATELESS bootstrap path
//! (`kb_core::sessions::tail::read_bootstrap_window` +
//! `kb_core::sessions::view::view_bootstrap`), which is correct on its own.
//! This cache only saves the reparse cost; deleting it would only be a perf
//! regression, never a correctness one.
//!
//! Keyed on `(sid, inode)` rather than `(sid, mtime)` — the inode changes on
//! truncation/rotation/a compacting rewrite (a fresh inode after
//! `open(O_TRUNC)` or an editor's atomic rename-over), so a stale carry is
//! never reused across a rewrite even if the new file happens to share an
//! mtime with the old one at second resolution; `TailReader::read_delta`'s
//! own `truncated_restart` detection is the belt-and-suspenders check at the
//! call site regardless.
//!
//! 5-minute idle TTL (design archaeology, the retired cockpit prototype's lesson #4: "lazy
//! open + idle reap") — checked on `get` (no background sweep thread; a
//! cache this small doesn't need one), so an abandoned follow session's
//! carry doesn't sit forever. Capacity 8 (LF-4) — the "operator has a
//! handful of sessions open across a couple of terminals/tabs" hot set;
//! much smaller than `replay_cache`'s 32 because a `ViewCarry` holds live
//! join state (pending tool calls, task registry), not just a rendered IR.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use kb_core::sessions::view::ViewCarry;

pub const DEFAULT_CAPACITY: usize = 8;
pub const IDLE_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
struct CacheKey {
    sid: String,
    inode: u64,
}

struct Entry {
    offset: u64,
    carry: ViewCarry,
    last_hit: Instant,
}

/// A `(sid, inode)`-keyed LRU. See the module docs for why the key
/// includes the inode and why a miss is always safe.
pub struct LiveTailCache {
    inner: Mutex<VecDeque<(CacheKey, Entry)>>,
    capacity: usize,
}

impl LiveTailCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
        }
    }

    /// Returns the cached carry for `(sid, inode)` IFF present, not idled
    /// out, AND its recorded offset equals `from` exactly — a mismatch
    /// means the client is asking for a different window than what this
    /// carry represents (a lost response, a second tab, a server restart
    /// that raced a client's still-in-flight poll), and the caller must
    /// fall back to a stateless bootstrap rather than feed lines into a
    /// carry that doesn't start where the client thinks it does.
    pub fn get(&self, sid: &str, inode: u64, from: u64) -> Option<ViewCarry> {
        let key = CacheKey {
            sid: sid.to_string(),
            inode,
        };
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let pos = g.iter().position(|(k, _)| k == &key)?;
        if g[pos].1.last_hit.elapsed() > IDLE_TTL {
            // Genuinely stale — evict.
            g.remove(pos);
            return None;
        }
        if g[pos].1.offset != from {
            // A `from` that doesn't match this entry's offset is a miss for
            // THIS query, but the entry itself is still fresh — leave it in
            // place (a later query at the RIGHT offset must still hit).
            // Only `put` (a successful read_delta) replaces it.
            return None;
        }
        let mut entry = g.remove(pos).expect("position just located");
        entry.1.last_hit = Instant::now();
        let carry = entry.1.carry.clone();
        g.push_front(entry);
        Some(carry)
    }

    /// Record `carry`'s state as of `offset` (the position a SUBSEQUENT
    /// request must present as its own `from` to hit this entry).
    pub fn put(&self, sid: &str, inode: u64, offset: u64, carry: ViewCarry) {
        let key = CacheKey {
            sid: sid.to_string(),
            inode,
        };
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pos) = g.iter().position(|(k, _)| k == &key) {
            g.remove(pos);
        }
        if g.len() == self.capacity {
            g.pop_back();
        }
        g.push_front((
            key,
            Entry {
                offset,
                carry,
                last_hit: Instant::now(),
            },
        ));
    }

    #[cfg(test)]
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

impl Default for LiveTailCache {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

/// The file's inode, used as half of the cache key (see module docs). `0`
/// on a non-unix target (out of scope for kb per CLAUDE.md, but this keeps
/// the crate portable rather than `#[cfg]`-excluding the whole cache) — a
/// constant inode there just means the cache degrades to keying on `sid`
/// alone, which is safe (a same-sid rewrite would still be caught by
/// `TailReader`'s own `truncated_restart` detection at the call site).
#[cfg(unix)]
pub fn file_inode(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.ino()
}

#[cfg(not(unix))]
pub fn file_inode(_meta: &std::fs::Metadata) -> u64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn carry() -> ViewCarry {
        ViewCarry::default()
    }

    #[test]
    fn put_then_get_round_trips_on_exact_offset_match() {
        let c = LiveTailCache::default();
        c.put("sid-1", 42, 100, carry());
        assert!(c.get("sid-1", 42, 100).is_some());
    }

    #[test]
    fn get_misses_on_offset_mismatch() {
        let c = LiveTailCache::default();
        c.put("sid-1", 42, 100, carry());
        assert!(
            c.get("sid-1", 42, 99).is_none(),
            "a from that doesn't match the cached offset must miss"
        );
    }

    #[test]
    fn get_misses_on_inode_mismatch() {
        let c = LiveTailCache::default();
        c.put("sid-1", 42, 100, carry());
        assert!(
            c.get("sid-1", 43, 100).is_none(),
            "a different inode (rewrite) must miss even at the same offset"
        );
    }

    #[test]
    fn get_misses_on_sid_mismatch() {
        let c = LiveTailCache::default();
        c.put("sid-1", 42, 100, carry());
        assert!(c.get("sid-2", 42, 100).is_none());
    }

    #[test]
    fn evicts_oldest_when_at_capacity() {
        let c = LiveTailCache::new(2);
        c.put("a", 1, 10, carry());
        c.put("b", 1, 10, carry());
        c.put("c", 1, 10, carry());
        assert_eq!(c.len(), 2);
        assert!(c.get("a", 1, 10).is_none(), "'a' was evicted");
        assert!(c.get("b", 1, 10).is_some());
        assert!(c.get("c", 1, 10).is_some());
    }

    #[test]
    fn get_promotes_to_most_recently_used() {
        let c = LiveTailCache::new(2);
        c.put("a", 1, 10, carry());
        c.put("b", 1, 10, carry());
        let _ = c.get("a", 1, 10);
        c.put("c", 1, 10, carry());
        assert!(c.get("a", 1, 10).is_some(), "'a' survived as MRU");
        assert!(c.get("b", 1, 10).is_none(), "'b' was evicted");
    }

    #[test]
    fn a_hit_removes_the_stale_entry_after_a_put_updates_the_offset() {
        let c = LiveTailCache::default();
        c.put("sid-1", 42, 100, carry());
        c.put("sid-1", 42, 200, carry()); // e.g. after a successful read_delta
        assert!(
            c.get("sid-1", 42, 100).is_none(),
            "old offset no longer valid"
        );
        assert!(c.get("sid-1", 42, 200).is_some());
        assert_eq!(c.len(), 1, "put must replace, not accumulate, the same key");
    }

    #[test]
    fn idle_entry_past_the_ttl_misses_and_is_evicted() {
        let c = LiveTailCache::default();
        c.put("sid-1", 42, 100, carry());
        // Directly age the entry past the TTL rather than sleeping in a
        // unit test.
        {
            let mut g = c.inner.lock().unwrap();
            g[0].1.last_hit = Instant::now() - IDLE_TTL - Duration::from_secs(1);
        }
        assert!(
            c.get("sid-1", 42, 100).is_none(),
            "idled-out entry must miss"
        );
        assert_eq!(
            c.len(),
            0,
            "the idled-out entry must be evicted on the missed get"
        );
    }
}
