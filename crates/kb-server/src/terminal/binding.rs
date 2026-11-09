//! T13 — terminal <-> session binding rules. Beats only; nothing persisted (LF-7).
//
// Known limit: a `start` beat is the rebind signal, so a subagent beating
// `start` with the same term and pid steals the binding. A1 removes the only
// known source (omp subagents). Also: a stale non-released holder
// (waiting/cold) blocks the late-bind of a different session in the same
// terminal until it ends or presumes ended; a `start` beat is unaffected.
//
// This module is pure: no I/O, no async. The directory is in-memory only;
// a failed runner list must never replace the directory (keep the old map).

use std::collections::HashMap;
use std::sync::Mutex;

/// Maximum length of a terminal id (`^[a-z0-9]{1,32}$`). The runner mints
/// 8..=12 characters (`kb-term`'s `is_valid_term_id`); the hook, the beat
/// intake, the redaction floor, the SPA relay and this directory all accept
/// the wider 1..=32 shape so an id the runner never minted is a drop/404,
/// never a grammar rejection.
pub const TERM_ID_MAX: usize = 32;

/// `^[a-z0-9]{1,32}$` — the one grammar on every surface except the runner
/// mint (8..=12): the kb-beat hook, the beat intake, the redaction floor
/// and the SPA relay all accept 1..=32.
pub fn is_valid_term_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= TERM_ID_MAX
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// Verified iff the terminal is known to the runner AND the beat's pid is
/// exactly that pane's pid.
pub fn claim_verified(term: &str, beat_pid: Option<i64>, pane_pid: Option<i64>) -> bool {
    is_valid_term_id(term)
        && beat_pid.is_some()
        && beat_pid == pane_pid
        && pane_pid.is_some_and(|p| p > 0)
}

/// What a verified claim does to the row's `term_bound_unix` (None = the claim is dropped).
pub fn bound_at(
    event: &str,
    same_row_bound_at: Option<i64>,
    another_row_holds: bool,
    now: i64,
) -> Option<i64> {
    match event {
        "start" => Some(now),
        "end" => same_row_bound_at,
        _ => match same_row_bound_at {
            Some(t) => Some(t),
            None if !another_row_holds => Some(now),
            None => None,
        },
    }
}

/// In-memory directory of terminal id -> pane pid, refreshed from the
/// runner's `list` reply. No `.await` inside the lock (#15).
/// `refreshed_at` records the `now` of the last successful `replace` and
/// is read only through [`TermDirectory::last_refresh`]; no freshness or
/// expiry check consumes it yet (refresh pacing lives in the caller's
/// minimum-interval gate, not here).
pub struct TermDirectory {
    inner: Mutex<HashMap<String, i64>>,
    refreshed_at: Mutex<Option<i64>>,
}

impl Default for TermDirectory {
    fn default() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            refreshed_at: Mutex::new(None),
        }
    }
}

impl std::fmt::Debug for TermDirectory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TermDirectory").finish()
    }
}

impl TermDirectory {
    /// A new empty directory.
    pub fn new() -> Self {
        Self::default()
    }

    /// The pane pid known for `id`, if any.
    pub fn pane_pid(&self, id: &str) -> Option<i64> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .copied()
    }
    /// The `now` of the last successful `replace`, if any.
    pub fn last_refresh(&self) -> Option<i64> {
        *self.refreshed_at.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Replace the directory with one runner `list` result. Call only on
    /// success — a failed list must never call `replace` (keep the old map).
    pub fn replace(&self, items: impl IntoIterator<Item = (String, i64)>, now: i64) {
        let mut map = HashMap::new();
        for (id, pid) in items {
            map.insert(id, pid);
        }
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        *g = map;
        *self.refreshed_at.lock().unwrap_or_else(|e| e.into_inner()) = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn term_id_grammar_is_the_one_grammar() {
        for ok in ["a", "abc123", &"a".repeat(32)] {
            assert!(is_valid_term_id(ok), "{ok:?} must be accepted");
        }
        for bad in ["", &"a".repeat(33)] {
            assert!(!is_valid_term_id(bad), "{bad:?} must be rejected");
        }
        for bad in ["A1", "a-b", "a b", "é", "-x", "a;b", "ABC"] {
            assert!(!is_valid_term_id(bad), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn claim_verified_requires_an_exact_pid_match() {
        assert!(claim_verified("abc123", Some(4242), Some(4242)));
        assert!(!claim_verified("abc123", None, Some(4242)));
        assert!(!claim_verified("abc123", Some(0), Some(0)));
        assert!(!claim_verified("abc123", Some(9999), Some(4242)));
        assert!(!claim_verified("abc123", Some(4242), None));
        assert!(!claim_verified("Bad", Some(4242), Some(4242)));
    }
    #[test]
    fn last_refresh_tracks_the_last_successful_replace() {
        let dir = TermDirectory::new();
        assert_eq!(dir.last_refresh(), None);
        dir.replace([("abc123".to_string(), 4242)], 1_000_000);
        assert_eq!(dir.last_refresh(), Some(1_000_000));
        dir.replace([("abc123".to_string(), 4242)], 1_000_005);
        assert_eq!(dir.last_refresh(), Some(1_000_005));
    }

    #[test]
    fn bound_at_truth_table() {
        let now = 1000;
        // start: always a fresh bind regardless of prior state.
        assert_eq!(bound_at("start", None, false, now), Some(now));
        assert_eq!(bound_at("start", Some(5), true, now), Some(now));
        assert_eq!(bound_at("start", None, true, now), Some(now));
        // end: keeps an existing bind, never creates one.
        assert_eq!(bound_at("end", Some(5), false, now), Some(5));
        assert_eq!(bound_at("end", Some(5), true, now), Some(5));
        assert_eq!(bound_at("end", None, false, now), None);
        assert_eq!(bound_at("end", None, true, now), None);
        // other: keeps an existing bind; late first bind only when no one holds.
        assert_eq!(bound_at("prompt", Some(5), false, now), Some(5));
        assert_eq!(bound_at("prompt", Some(5), true, now), Some(5));
        assert_eq!(bound_at("prompt", None, false, now), Some(now));
        assert_eq!(bound_at("prompt", None, true, now), None);
        assert_eq!(bound_at("tool", None, true, now), None);
        assert_eq!(bound_at("tool", None, false, now), Some(now));
    }
}
