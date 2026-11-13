//! Grid policy: one grid per session, owned, in the `@kb_owner_grid`
//! user option (state-free: survives a runner restart).
//!
//! `window-size manual` plus explicit `resize-window`; watching and typing
//! never touch the grid — only `take_size` does. The anchor and every
//! hidden client get the new winsize (cells and pixel fields).
//!
//! Crash window (accepted): `take_size` resizes the window before
//! recording `@kb_owner_grid`, so a crash between the two steps leaves
//! the new size with a stale or missing owner. This is deliberate: the
//! window size is truth (reconciled by discovery), and the owner is
//! advisory — recording the owner first would claim a size that was
//! never applied and refuse every other owner against a phantom. A
//! replay (same owner retake, or a forced take) repairs the pair, so
//! `take_size` is idempotent-safe under retry.

use std::path::Path;

use crate::runner::tmux::Tmux;
use crate::runner::{Runner, RunnerError};

/// The grid owner option key.
pub const OWNER_OPTION: &str = "@kb_owner_grid";
/// Grid bounds: 20..=500 cols, 5..=200 rows.
pub const MIN_COLS: u16 = 20;
/// Grid bounds.
pub const MAX_COLS: u16 = 500;
/// Grid bounds.
pub const MIN_ROWS: u16 = 5;
/// Grid bounds.
pub const MAX_ROWS: u16 = 200;

/// Whether `owner` is a valid grid owner: `[a-z0-9:_-]{1,64}`.
pub fn valid_owner(owner: &str) -> bool {
    (1..=64).contains(&owner.len())
        && owner.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == ':' || c == '_' || c == '-'
        })
}

/// Whether `(cols, rows)` is in bounds.
pub fn valid_size(cols: u16, rows: u16) -> bool {
    (MIN_COLS..=MAX_COLS).contains(&cols) && (MIN_ROWS..=MAX_ROWS).contains(&rows)
}

/// Current grid: (cols, rows, owner).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridState {
    /// Grid columns.
    pub cols: u16,
    /// Grid rows.
    pub rows: u16,
    /// Current owner, if any.
    pub owner: Option<String>,
}

/// The take decision (pure): `force` overrides a foreign owner.
pub fn decide_take(current: &Option<String>, owner: &str, force: bool) -> Result<(), RunnerError> {
    match current {
        None => Ok(()),
        Some(cur) if cur == owner || force => Ok(()),
        Some(cur) => Err(RunnerError::GridOwned(format!("grid is owned by {cur}"))),
    }
}

/// Read the current grid + owner from the holder.
pub async fn grid_state<T: Tmux>(tmux: &T, sock: &Path) -> Result<GridState, RunnerError> {
    let out = tmux
        .run(
            sock,
            &[
                "display-message",
                "-p",
                "-t",
                "kb",
                "#{window_width} #{window_height}",
            ],
            None,
        )
        .await?;
    if !out.ok {
        return Err(RunnerError::NotFound(format!(
            "no holder at {}",
            sock.display()
        )));
    }
    let mut parts = out.stdout.split_whitespace();
    let cols: u16 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let rows: u16 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    Ok(GridState {
        cols,
        rows,
        owner: read_owner(tmux, sock).await,
    })
}

/// Read `@kb_owner_grid` (empty/unset reads as `None`).
pub async fn read_owner<T: Tmux>(tmux: &T, sock: &Path) -> Option<String> {
    let out = tmux
        .run(sock, &["show-options", "-gqv", OWNER_OPTION], None)
        .await
        .ok()?;
    let val = out.stdout.trim().to_string();
    if val.is_empty() {
        None
    } else {
        Some(val)
    }
}

/// Set `@kb_owner_grid` to `owner` (`None` clears it).
pub async fn write_owner<T: Tmux>(
    tmux: &T,
    sock: &Path,
    owner: Option<&str>,
) -> Result<(), RunnerError> {
    let out = tmux
        .run(
            sock,
            &["set-option", "-g", OWNER_OPTION, owner.unwrap_or("")],
            None,
        )
        .await?;
    if !out.ok {
        return Err(RunnerError::TmuxFailed(format!(
            "set-option {OWNER_OPTION} failed: {}",
            out.stderr.trim()
        )));
    }
    Ok(())
}

/// `tune`/resize the window and apply the winsize to every client pty
/// (anchor + hidden viewers). Watching/typing never call this.
pub async fn apply_size<S, T>(
    runner: &Runner<S>,
    tmux: &T,
    id: &str,
    cols: u16,
    rows: u16,
) -> Result<(), RunnerError>
where
    S: crate::runner::supervisor::Supervisor,
    T: Tmux,
{
    let sock = runner.holder_socket(id);
    let out = tmux
        .run(
            &sock,
            &[
                "resize-window",
                "-t",
                "kb",
                "-x",
                &cols.to_string(),
                "-y",
                &rows.to_string(),
            ],
            None,
        )
        .await?;
    if !out.ok {
        return Err(RunnerError::NotFound(format!("no holder for {id}")));
    }
    runner.apply_client_winsizes(id, cols, rows).await?;
    Ok(())
}

/// Take the grid: check bounds + owner, resize, record the owner.
pub async fn take_size<S, T>(
    runner: &Runner<S>,
    tmux: &T,
    id: &str,
    cols: u16,
    rows: u16,
    owner: &str,
    force: bool,
) -> Result<GridState, RunnerError>
where
    S: crate::runner::supervisor::Supervisor,
    T: Tmux,
{
    if !valid_size(cols, rows) {
        return Err(RunnerError::BadSize(format!(
            "cols {cols} rows {rows} out of 20..=500 x 5..=200"
        )));
    }
    if !valid_owner(owner) {
        return Err(RunnerError::BadSize(format!("bad grid owner `{owner}`")));
    }
    // Per-session serialization (like launch/kill): the
    // read-decide-resize-write sequence is atomic per id, so two
    // concurrent foreign takes cannot both read an unset owner.
    let _lock = runner.id_lock(id).await;
    let lock = _lock.lock().await;
    let sock = runner.holder_socket(id);
    let current = grid_state(tmux, &sock).await?;
    decide_take(&current.owner, owner, force)?;
    apply_size(runner, tmux, id, cols, rows).await?;
    write_owner(tmux, &sock, Some(owner)).await?;
    drop(lock);
    Ok(GridState {
        cols,
        rows,
        owner: Some(owner.to_string()),
    })
}

/// Legacy `resize{id, cols, rows}` RPC (no owner claim on the wire):
/// obeys the grid-ownership policy — bounds via `valid_size`
/// (`bad-size`), and any existing `@kb_owner_grid` owner refuses the
/// resize (`grid-owned`), because the legacy op cannot prove it is the
/// owner. With no owner recorded it takes the grid through
/// `apply_size`, so the anchor AND every viewer pty get the new
/// winsize (cells + pixel fields), exactly like `take_size`.
pub async fn resize_legacy<S, T>(
    runner: &Runner<S>,
    tmux: &T,
    id: &str,
    cols: u16,
    rows: u16,
) -> Result<(), RunnerError>
where
    S: crate::runner::supervisor::Supervisor,
    T: Tmux,
{
    if !valid_size(cols, rows) {
        return Err(RunnerError::BadSize(format!(
            "cols {cols} rows {rows} out of 20..=500 x 5..=200"
        )));
    }
    let _lock = runner.id_lock(id).await;
    let lock = _lock.lock().await;
    let sock = runner.holder_socket(id);
    let current = grid_state(tmux, &sock).await?;
    if let Some(cur) = current.owner {
        return Err(RunnerError::GridOwned(format!("grid is owned by {cur}")));
    }
    apply_size(runner, tmux, id, cols, rows).await?;
    drop(lock);
    Ok(())
}

/// Release the grid: clears the option only if the owner matches.
pub async fn release_size<S, T>(
    runner: &Runner<S>,
    tmux: &T,
    id: &str,
    owner: &str,
) -> Result<(), RunnerError>
where
    S: crate::runner::supervisor::Supervisor,
    T: Tmux,
{
    if !valid_owner(owner) {
        return Err(RunnerError::BadSize(format!("bad grid owner `{owner}`")));
    }
    // Same per-session serialization as take_size (release races a take).
    let _lock = runner.id_lock(id).await;
    let lock = _lock.lock().await;
    let sock = runner.holder_socket(id);
    let current = read_owner(tmux, &sock).await;
    let res = match current {
        Some(cur) if cur == owner => write_owner(tmux, &sock, None).await,
        Some(cur) => Err(RunnerError::GridOwned(format!("grid is owned by {cur}"))),
        None => Err(RunnerError::GridOwned("grid has no owner".to_string())),
    };
    drop(lock);
    res
}

/// `grid_owner_state_machine`: unset -> take ok; same owner retake ok;
/// foreign non-force -> `grid-owned` with the size unchanged; foreign force
/// ok and owner changes; release by non-owner -> `grid-owned`; release by
/// owner clears; bounds -> `bad-size`. Runs the REAL `take_size` /
/// `release_size` / `grid_state` / `apply_size` against a stateful fake
/// tmux, so a dropped `resize-window`, a skipped
/// `set-option @kb_owner_grid`, or a broken release cannot pass.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::config::RunnerConfig;
    use crate::runner::supervisor::DirectSupervisor;
    use crate::runner::tmux::fake::FakeTmux;
    use crate::runner::Runner;

    fn size_calls(fake: &FakeTmux) -> usize {
        fake.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, a)| a.first().map(String::as_str) == Some("resize-window"))
            .count()
    }

    #[tokio::test]
    async fn grid_owner_state_machine() {
        // A runner whose scripted tmux tracks owner + size (no anchors,
        // so apply_size's client-winsize step is a no-op here).
        let dir = tempfile::tempdir().unwrap();
        let cfg = RunnerConfig::test_config(
            dir.path().join("run"),
            "/bin/true".to_string(),
            dir.path().join("conf"),
        );
        let runner: Runner<DirectSupervisor> = Runner::direct(cfg);
        let fake = FakeTmux::new();
        let id = "abcd1234";
        let sock = runner.holder_socket(id);

        // grid_state reads the tracked size.
        let state = grid_state(&fake, &sock).await.unwrap();
        assert_eq!(
            state,
            GridState {
                cols: 100,
                rows: 30,
                owner: None
            }
        );
        // Unset -> take ok (resize-window + set-option both run).
        let before = size_calls(&fake);
        let state = take_size(&runner, &fake, id, 100, 30, "a", false)
            .await
            .unwrap();
        assert_eq!(
            state,
            GridState {
                cols: 100,
                rows: 30,
                owner: Some("a".to_string())
            }
        );
        assert_eq!(size_calls(&fake), before + 1, "allowed take resizes");
        assert_eq!(read_owner(&fake, &sock).await, Some("a".to_string()));
        // Same owner retake ok.
        take_size(&runner, &fake, id, 100, 30, "a", false)
            .await
            .unwrap();
        // Foreign non-force -> grid-owned, no resize, size/owner unchanged.
        let before_refused = size_calls(&fake);
        let err = take_size(&runner, &fake, id, 80, 24, "b", false)
            .await
            .unwrap_err();
        assert_eq!(err.code(), "grid-owned");
        assert!(err.message().contains('a'), "message names the owner");
        assert_eq!(
            size_calls(&fake),
            before_refused,
            "refused take must not resize"
        );
        assert_eq!(
            grid_state(&fake, &sock).await.unwrap(),
            GridState {
                cols: 100,
                rows: 30,
                owner: Some("a".to_string())
            }
        );
        // Foreign force -> ok; owner changes; size moves.
        take_size(&runner, &fake, id, 80, 24, "b", true)
            .await
            .unwrap();
        assert_eq!(
            grid_state(&fake, &sock).await.unwrap(),
            GridState {
                cols: 80,
                rows: 24,
                owner: Some("b".to_string())
            }
        );
        // Release by non-owner -> grid-owned, owner unchanged.
        let err = release_size(&runner, &fake, id, "a").await.unwrap_err();
        assert_eq!(err.code(), "grid-owned");
        assert_eq!(read_owner(&fake, &sock).await, Some("b".to_string()));
        // Release by owner clears (stateful set-option "").
        release_size(&runner, &fake, id, "b").await.unwrap();
        assert_eq!(read_owner(&fake, &sock).await, None);
        // Bounds -> bad-size through the real take_size (no tmux writes).
        let before_bounds = size_calls(&fake);
        for (cols, rows) in [(19u16, 30u16), (501, 30), (100, 4), (100, 201)] {
            let err = take_size(&runner, &fake, id, cols, rows, "x", false)
                .await
                .unwrap_err();
            assert_eq!(
                err.code(),
                "bad-size",
                "take {cols}x{rows} must be bad-size"
            );
        }
        assert_eq!(size_calls(&fake), before_bounds);
        // Bad owner charset -> bad-size through the real take_size and
        // release_size (the first arm of each, no tmux writes).
        for bad in ["KB", "has space", "", "x/y"] {
            let err = take_size(&runner, &fake, id, 100, 30, bad, false)
                .await
                .unwrap_err();
            assert_eq!(
                err.code(),
                "bad-size",
                "take owner {bad:?} must be bad-size"
            );
            let err = release_size(&runner, &fake, id, bad).await.unwrap_err();
            assert_eq!(
                err.code(),
                "bad-size",
                "release owner {bad:?} must be bad-size"
            );
        }
        assert_eq!(size_calls(&fake), before_bounds);
        // Pure helpers still agree with the machine.
        assert!(valid_size(20, 5) && valid_size(500, 200));
        assert!(valid_owner("kb-term"));
        assert!(!valid_owner("KB"));
        assert!(!valid_owner(""));
        assert!(!valid_owner(&"a".repeat(65)));
        assert!(decide_take(&None, "x", false).is_ok());
        assert!(decide_take(&Some("x".to_string()), "x", false).is_ok());
        assert!(decide_take(&Some("x".to_string()), "y", true).is_ok());
        assert_eq!(
            decide_take(&Some("x".to_string()), "y", false)
                .unwrap_err()
                .code(),
            "grid-owned"
        );
    }

    #[tokio::test]
    async fn legacy_resize_obeys_ownership_and_bounds() {
        // The legacy `resize{id,cols,rows}` RPC carries no owner claim,
        // so it must refuse whenever an owner is recorded (`grid-owned`,
        // no resize), and apply through `apply_size` (resize-window +
        // client winsizes) when no owner is set. Bounds are `bad-size`.
        let dir = tempfile::tempdir().unwrap();
        let cfg = RunnerConfig::test_config(
            dir.path().join("run"),
            "/bin/true".to_string(),
            dir.path().join("conf"),
        );
        let runner: Runner<DirectSupervisor> = Runner::direct(cfg);
        let fake = FakeTmux::new();
        let id = "abcd1234";
        let sock = runner.holder_socket(id);
        // No owner: legacy resize applies (window moves, fake tracks it).
        resize_legacy(&runner, &fake, id, 80, 24).await.unwrap();
        assert_eq!(
            grid_state(&fake, &sock).await.unwrap(),
            GridState {
                cols: 80,
                rows: 24,
                owner: None
            }
        );
        // Take as `a`: legacy resize now refuses, size unchanged.
        take_size(&runner, &fake, id, 100, 30, "a", false)
            .await
            .unwrap();
        let before = size_calls(&fake);
        let err = resize_legacy(&runner, &fake, id, 80, 24).await.unwrap_err();
        assert_eq!(err.code(), "grid-owned");
        assert!(err.message().contains('a'), "message names the owner");
        assert_eq!(size_calls(&fake), before, "refused resize must not resize");
        assert_eq!(
            grid_state(&fake, &sock).await.unwrap(),
            GridState {
                cols: 100,
                rows: 30,
                owner: Some("a".to_string())
            }
        );
        // Release by the owner: legacy resize applies again.
        release_size(&runner, &fake, id, "a").await.unwrap();
        resize_legacy(&runner, &fake, id, 120, 40).await.unwrap();
        assert_eq!(
            grid_state(&fake, &sock).await.unwrap(),
            GridState {
                cols: 120,
                rows: 40,
                owner: None
            }
        );
        // Bounds -> bad-size through the real resize_legacy (no writes).
        let before_bounds = size_calls(&fake);
        for (cols, rows) in [(19u16, 30u16), (501, 30), (100, 4), (100, 201)] {
            let err = resize_legacy(&runner, &fake, id, cols, rows)
                .await
                .unwrap_err();
            assert_eq!(
                err.code(),
                "bad-size",
                "legacy resize {cols}x{rows} must be bad-size"
            );
        }
        assert_eq!(size_calls(&fake), before_bounds);
    }
}
