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

/// One session a terminal row might be linked to by folder. Built by the
/// route from the live registry and from landed captures; the picker below
/// never sees I/O types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HintCandidate {
    pub session_id: String,
    pub harness: String,
    /// The session's working folder; `None` never matches.
    pub cwd: Option<String>,
    /// Unix seconds of the session's latest activity.
    pub last_activity_unix: i64,
    /// A subagent child (its root differs from its own id).
    pub is_subagent_child: bool,
    /// A verified binding to a DIFFERENT terminal already exists.
    pub bound_to_other_term: bool,
}

/// The derived, unverified folder-match guess for an unbound terminal row:
/// the newest candidate with the same harness and exactly the same cwd
/// (byte equality), active at or after the terminal's start (when known),
/// not a subagent child and not bound to another terminal. Two or more
/// distinct sessions tied on the newest activity are ambiguous: no hint.
/// Pure; never consulted by any gate.
pub fn pick_session_hint(
    term_harness: &str,
    term_cwd: &str,
    term_started_unix: Option<i64>,
    candidates: &[HintCandidate],
) -> Option<String> {
    let mut best: Option<(i64, &str)> = None;
    let mut tied = false;
    for c in candidates {
        if c.harness != term_harness
            || c.cwd.as_deref() != Some(term_cwd)
            || c.is_subagent_child
            || c.bound_to_other_term
            || term_started_unix.is_some_and(|t| c.last_activity_unix < t)
        {
            continue;
        }
        match best {
            None => best = Some((c.last_activity_unix, c.session_id.as_str())),
            Some((_, sid)) if sid == c.session_id => {
                if c.last_activity_unix > best.map_or(i64::MIN, |b| b.0) {
                    best = Some((c.last_activity_unix, sid));
                    tied = false;
                }
            }
            Some((at, _)) if c.last_activity_unix > at => {
                best = Some((c.last_activity_unix, c.session_id.as_str()));
                tied = false;
            }
            Some((at, _)) if c.last_activity_unix == at => tied = true,
            Some(_) => {}
        }
    }
    if tied {
        return None;
    }
    best.map(|(_, sid)| sid.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(sid: &str, at: i64) -> HintCandidate {
        HintCandidate {
            session_id: sid.into(),
            harness: "claude".into(),
            cwd: Some("/work/demo/repo".into()),
            last_activity_unix: at,
            is_subagent_child: false,
            bound_to_other_term: false,
        }
    }
    fn pick(c: &[HintCandidate]) -> Option<String> {
        pick_session_hint("claude", "/work/demo/repo", Some(100), c)
    }

    #[test]
    fn hint_matches_same_harness_and_exact_cwd() {
        assert_eq!(pick(&[cand("a", 200)]), Some("a".into()));
    }
    #[test]
    fn hint_wrong_harness_is_none() {
        let mut c = cand("a", 200);
        c.harness = "omp".into();
        assert_eq!(pick(&[c]), None);
    }
    #[test]
    fn hint_wrong_cwd_is_none() {
        for bad in [
            "/work/demo/repo/",
            "/Work/demo/repo",
            "/work/demo",
            "/work/demo/repo/sub",
            "/work/demo/rep",
        ] {
            let mut c = cand("a", 200);
            c.cwd = Some(bad.into());
            assert_eq!(pick(&[c]), None, "{bad}");
        }
    }
    #[test]
    fn hint_ambiguous_equal_recency_is_none() {
        assert_eq!(pick(&[cand("a", 200), cand("b", 200)]), None);
        // A tie on a LOWER recency does not make the newest ambiguous.
        assert_eq!(
            pick(&[cand("a", 300), cand("b", 200), cand("c", 200)]),
            Some("a".into())
        );
        // The same session listed twice is not a tie with itself.
        assert_eq!(pick(&[cand("a", 200), cand("a", 200)]), Some("a".into()));
    }
    #[test]
    fn hint_newest_wins_when_recency_differs() {
        assert_eq!(
            pick(&[cand("a", 200), cand("b", 300), cand("c", 250)]),
            Some("b".into())
        );
    }
    #[test]
    fn hint_older_than_term_start_is_none() {
        assert_eq!(pick(&[cand("a", 99)]), None);
        assert_eq!(pick(&[cand("a", 100)]), Some("a".into()));
        // Unknown start: the requirement is skipped, the hint kept.
        assert_eq!(
            pick_session_hint("claude", "/work/demo/repo", None, &[cand("a", 1)]),
            Some("a".into())
        );
    }
    #[test]
    fn hint_skips_subagent_children() {
        let mut c = cand("child", 300);
        c.is_subagent_child = true;
        assert_eq!(pick(&[c, cand("a", 200)]), Some("a".into()));
    }
    #[test]
    fn hint_skips_session_bound_to_another_term() {
        let mut c = cand("other", 300);
        c.bound_to_other_term = true;
        assert_eq!(pick(&[c]), None);
    }
    #[test]
    fn hint_candidate_without_cwd_never_matches() {
        let mut c = cand("a", 200);
        c.cwd = None;
        assert_eq!(pick(&[c]), None);
    }

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
