//! SL2 (`docs/research/kb-slate-design-2026-09.html` §7 "Storage" → "Lock")
//! — the per-slate mutation lock, the cached head sequence, and the
//! per-session rate window, in ONE place.
//!
//! The design names this explicitly: "a `slate_registry::SlateRegistry` on
//! `KbHandles` (the `LiveRegistry` precedent, `state.rs:781`) that holds the
//! per-slug async lock and the cached `head_seq`, so the lock and seq
//! minting live in one place; the std mutex on the map is for get-or-insert
//! only."
//!
//! **Invariant #15 is the shape rule**: every `std::sync::Mutex` guard in
//! this file is taken and dropped inside a synchronous method — no guard
//! ever crosses an `.await`. Callers `.lock().await` the returned
//! `Arc<AsyncMutex<()>>`, exactly as `KbHandles::review_lock_for`'s callers
//! do (invariant #6).
//!
//! **Nothing here is persisted.** The ledger and `meta.json` on disk are the
//! truth; the cached head is a monotonicity SENTRY, not a source of seqs —
//! `routes::slates` re-reads `meta.json` under the lock on every append and
//! mints from THAT, then tells the registry what it wrote. If the two ever
//! disagree the disk wins and the registry logs, because a cache that can
//! hand out a seq is a cache that can duplicate one.

use kb_core::slate::MAX_POSTS_PER_SESSION_PER_MINUTE;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as AsyncMutex;

/// The rate window's width in seconds — [`MAX_POSTS_PER_SESSION_PER_MINUTE`]
/// posts per *minute* (§7 "Caps").
pub const RATE_WINDOW_SECS: i64 = 60;

/// Distinct `(slug, session)` rate keys held at once. A runaway fleet is
/// bounded to a fixed, small amount of memory the same way
/// [`crate::live_registry::REGISTRY_CAP`] bounds the beat registry; the
/// least-recently-posting key is evicted, which can only ever GRANT a post
/// that would otherwise have been refused (fail-open on eviction is right
/// here: this is a politeness cap, not a security boundary).
pub const RATE_KEY_CAP: usize = 4096;

/// One slug's cached head. `generation` rides along so a `rotate` can
/// invalidate coherently (the archive moves, the head does NOT reset —
/// see `routes::slates::rotate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachedHead {
    pub head_seq: u64,
    pub generation: u32,
}

#[derive(Default)]
pub struct SlateRegistry {
    /// Get-or-insert only; the guard never crosses an await (#15).
    locks: Mutex<BTreeMap<String, Arc<AsyncMutex<()>>>>,
    heads: Mutex<HashMap<String, CachedHead>>,
    /// `(slug, session-key)` → the instants of that session's recent posts,
    /// oldest first. Trimmed to [`RATE_WINDOW_SECS`] on every touch.
    rate: Mutex<HashMap<(String, String), VecDeque<i64>>>,
}

impl SlateRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get-or-create the per-slug mutation lock — the `review_lock_for`
    /// shape (`state.rs:969-990`), sharded per SLATE rather than per kb
    /// because the slate store is daemon-wide (§7).
    pub fn lock_for(&self, slug: &str) -> Arc<AsyncMutex<()>> {
        self.locks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(slug.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    /// The last head this process WROTE for `slug`, if any.
    pub fn cached_head(&self, slug: &str) -> Option<CachedHead> {
        self.heads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(slug)
            .copied()
    }

    /// Record what an append/rotate just committed. Called under the
    /// per-slug lock, immediately after `meta.json` lands.
    pub fn note_head(&self, slug: &str, head: CachedHead) {
        self.heads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(slug.to_string(), head);
    }

    /// Drop every cached fact about `slug` — the purge path. Rate keys go
    /// too: a purged slate's per-session budget is meaningless.
    pub fn forget(&self, slug: &str) {
        self.heads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(slug);
        self.rate
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(s, _), _| s != slug);
    }

    /// The per-session per-minute cap (§7 "Caps"; 429 `slate-rate`).
    /// Returns `true` and RECORDS the post when it fits, `false` and
    /// records nothing when it does not — a refused post never spends
    /// budget, so a client that backs off recovers on schedule.
    pub fn allow_post(&self, slug: &str, session_key: &str, now_unix: i64) -> bool {
        let mut g = self.rate.lock().unwrap_or_else(|e| e.into_inner());
        // Sweep keys whose whole window has expired — cheap at this cap and
        // only on the write path, the `LiveRegistry::record_beat` shape.
        g.retain(|_, v| {
            v.back()
                .is_some_and(|last| now_unix - last < RATE_WINDOW_SECS)
        });
        let key = (slug.to_string(), session_key.to_string());
        if !g.contains_key(&key) && g.len() >= RATE_KEY_CAP {
            if let Some(oldest) = g
                .iter()
                .min_by_key(|(_, v)| v.back().copied().unwrap_or(i64::MIN))
                .map(|(k, _)| k.clone())
            {
                g.remove(&oldest);
            }
        }
        let window = g.entry(key).or_default();
        while window
            .front()
            .is_some_and(|first| now_unix - *first >= RATE_WINDOW_SECS)
        {
            window.pop_front();
        }
        if window.len() >= MAX_POSTS_PER_SESSION_PER_MINUTE {
            return false;
        }
        window.push_back(now_unix);
        true
    }

    #[cfg(test)]
    fn rate_keys(&self) -> usize {
        self.rate.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_for_returns_the_same_handle_per_slug_and_a_distinct_one_per_slate() {
        let reg = SlateRegistry::new();
        let a1 = reg.lock_for("kb");
        let a2 = reg.lock_for("kb");
        let b = reg.lock_for("perf");
        assert!(Arc::ptr_eq(&a1, &a2), "one lock per slug");
        assert!(!Arc::ptr_eq(&a1, &b), "slates never share a lock");
    }

    #[test]
    fn the_seventh_post_in_a_minute_is_refused_and_the_next_minute_recovers() {
        let reg = SlateRegistry::new();
        let now = 1_767_225_600;
        for i in 0..MAX_POSTS_PER_SESSION_PER_MINUTE {
            assert!(reg.allow_post("kb", "sess", now), "post {i} must fit");
        }
        assert!(!reg.allow_post("kb", "sess", now), "the seventh is refused");
        // A refusal spends no budget: still refused one second later.
        assert!(!reg.allow_post("kb", "sess", now + 1));
        // The window slides off the first post's instant.
        assert!(reg.allow_post("kb", "sess", now + RATE_WINDOW_SECS));
    }

    #[test]
    fn the_rate_window_is_per_session_and_per_slate() {
        let reg = SlateRegistry::new();
        let now = 1_767_225_600;
        for _ in 0..MAX_POSTS_PER_SESSION_PER_MINUTE {
            assert!(reg.allow_post("kb", "a", now));
        }
        assert!(!reg.allow_post("kb", "a", now));
        assert!(
            reg.allow_post("kb", "b", now),
            "another session is unaffected"
        );
        assert!(
            reg.allow_post("perf", "a", now),
            "another slate is unaffected"
        );
    }

    #[test]
    fn cached_head_round_trips_and_purge_forgets_everything() {
        let reg = SlateRegistry::new();
        assert!(reg.cached_head("kb").is_none());
        reg.note_head(
            "kb",
            CachedHead {
                head_seq: 66,
                generation: 1,
            },
        );
        assert_eq!(reg.cached_head("kb").unwrap().head_seq, 66);
        assert!(reg.allow_post("kb", "sess", 1_767_225_600));
        assert_eq!(reg.rate_keys(), 1);
        reg.forget("kb");
        assert!(reg.cached_head("kb").is_none());
        assert_eq!(reg.rate_keys(), 0, "purge drops the rate window too");
    }
}
