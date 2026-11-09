//! Enter policy: only after output has been quiet, only when the caller
//! says the session is not blocked.
//!
//! The caller supplies the beat-derived `blocked` flag (the runner has
//! no beat state). `from_composer` after a `paste_only` paste (`/` or
//! `@` — the composer pastes; a bare Enter must not submit) answers
//! `paste-only`. Otherwise the runner waits until the session has been
//! quiet for `enter_quiet_ms` (default 400 ms), giving up after
//! `enter_max_wait_ms` with `busy`, then writes `0d` through the named
//! viewer's hidden client. A `keys`/`enter` clears the `paste_only`
//! flag.
//!
//! The quiet clock is a stand-in: each session's anchor pump records
//! `last_output`, and tmux redraws the attached anchor whenever pane
//! content changes (`status off`, so only pane output), making it a
//! usable activity signal. Error sources: tmux redraw batching (a few
//! ms); it can only lag the pane, so it errs toward waiting longer. The
//! tap (a later unit) supplies a better quiet source; the gate here is
//! unchanged.

use std::future::Future;
use std::time::Duration;

use crate::runner::{Runner, RunnerError};

/// A source of "how long has session `id` been quiet".
///
/// Anchor-backed here; the tap replaces the anchor-backed clock later.
///
/// Contract note: the T9 brief pinned a sync `quiet_for(&self, id) ->
/// Option<Duration>`; the merged reality is this async trait (the
/// anchor-backed impl must lock an async map), and later units code
/// against the async form.
pub trait QuietSource: Send + Sync {
    /// Time since the session's last output (`None` if never active).
    fn quiet_for(&self, id: &str) -> impl Future<Output = Option<Duration>> + Send;
}

/// The anchor-backed quiet clock (over the runner's anchor pumps).
pub struct AnchorQuiet<'a, S: crate::runner::supervisor::Supervisor> {
    /// The runner whose anchor pumps supply `last_output`.
    pub runner: &'a Runner<S>,
}

impl<S: crate::runner::supervisor::Supervisor> QuietSource for AnchorQuiet<'_, S> {
    fn quiet_for(&self, id: &str) -> impl Future<Output = Option<Duration>> + Send {
        let id = id.to_string();
        let runner = self.runner;
        async move { runner.anchor_quiet_for(&id).await }
    }
}

/// The pure gate: blocked and paste-only refusal (nothing written).
pub fn enter_gate(
    blocked: bool,
    from_composer: bool,
    last_paste_only: bool,
) -> Result<(), RunnerError> {
    if blocked {
        return Err(RunnerError::Blocked(
            "session is blocked; Enter not sent".to_string(),
        ));
    }
    if from_composer && last_paste_only {
        return Err(RunnerError::PasteOnly(
            "last paste was paste_only; send the key explicitly".to_string(),
        ));
    }
    Ok(())
}

/// Wait until `quiet_for >= quiet_ms`, up to `max_wait_ms` (busy). Polls
/// every 20 ms; `poll` is a testable closure (fake clocks become quiet at
/// scripted poll counts without sleeping past their own tiny bounds).
pub async fn wait_quiet<F, Fut>(
    mut poll: F,
    quiet_ms: u64,
    max_wait_ms: u64,
) -> Result<(), RunnerError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<Duration>>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_millis(max_wait_ms);
    loop {
        let quiet = poll().await;
        if quiet.is_some_and(|d| d >= Duration::from_millis(quiet_ms)) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(RunnerError::Busy(
                "output did not go quiet within the wait".to_string(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A fake quiet source for tests: quiet after `quiet_after_polls` polls
/// (`None` = never).
#[derive(Debug)]
pub struct FakeQuiet {
    /// Polls until ready (`None` = never).
    pub quiet_after_polls: Option<u64>,
    /// Poll counter (shared with the test).
    pub polls: std::sync::Arc<std::sync::Mutex<u64>>,
}

impl FakeQuiet {
    /// A clock that is quiet from the start.
    pub fn quiet() -> Self {
        FakeQuiet {
            quiet_after_polls: Some(0),
            polls: std::sync::Arc::new(std::sync::Mutex::new(0)),
        }
    }

    /// A clock that never goes quiet.
    pub fn never() -> Self {
        FakeQuiet {
            quiet_after_polls: None,
            polls: std::sync::Arc::new(std::sync::Mutex::new(0)),
        }
    }

    /// A clock that goes quiet on poll `n` (1-based).
    pub fn after(n: u64) -> Self {
        FakeQuiet {
            quiet_after_polls: Some(n),
            polls: std::sync::Arc::new(std::sync::Mutex::new(0)),
        }
    }

    /// One poll (test only).
    pub fn poll(&self) -> Option<Duration> {
        let mut n = self.polls.lock().unwrap();
        *n += 1;
        match self.quiet_after_polls {
            Some(after) if *n >= after.max(1) => Some(Duration::from_secs(1)),
            _ => Some(Duration::from_millis(0)),
        }
    }
}

impl QuietSource for FakeQuiet {
    fn quiet_for(&self, _id: &str) -> impl Future<Output = Option<Duration>> + Send {
        let out = self.poll();
        async move { out }
    }
}

/// `enter_gate_blocked_busy_and_paste_only` (pure halves: gate, busy,
/// paste-only, success-after-keys). The full async path (blocked ->
/// nothing written; quiet wait -> written once; never quiet -> busy) is
/// pinned over the fake wait here and end-to-end by the term-int `enter`
/// test.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_gate_blocked_busy_and_paste_only() {
        // Blocked: refused before anything else.
        assert_eq!(
            enter_gate(true, false, false).unwrap_err().code(),
            "blocked"
        );
        assert_eq!(enter_gate(true, true, true).unwrap_err().code(), "blocked");
        // Composer after a paste-only paste: refused.
        assert_eq!(
            enter_gate(false, true, true).unwrap_err().code(),
            "paste-only"
        );
        // Composer after a plain paste, or a plain enter after keys: ok
        // (keys clear the flag, so a later enter is no longer gated).
        assert!(enter_gate(false, true, false).is_ok());
        assert!(enter_gate(false, false, true).is_ok());
        assert!(enter_gate(false, false, false).is_ok());
    }

    #[tokio::test]
    async fn wait_quiet_busy_and_success() {
        // Already quiet: immediate.
        wait_quiet(
            || std::future::ready(Some(Duration::from_secs(1))),
            400,
            100,
        )
        .await
        .unwrap();
        // Never quiet: busy after the max wait.
        let err = wait_quiet(
            || std::future::ready(Some(Duration::from_millis(0))),
            400,
            60,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code(), "busy");
        // No output recorded yet (None reads as "not quiet"): busy too.
        let err = wait_quiet(|| std::future::ready(None), 400, 60)
            .await
            .unwrap_err();
        assert_eq!(err.code(), "busy");
        // Fake clock that goes quiet on poll 3: written (success).
        let fake = FakeQuiet::after(3);
        let polls = fake.polls.clone();
        wait_quiet(|| std::future::ready(fake.poll()), 400, 5000)
            .await
            .unwrap();
        assert!(*polls.lock().unwrap() >= 3);
        // Fake clock never quiet: busy.
        let fake = FakeQuiet::never();
        let err = wait_quiet(|| std::future::ready(fake.poll()), 400, 60)
            .await
            .unwrap_err();
        assert_eq!(err.code(), "busy");
    }
}
