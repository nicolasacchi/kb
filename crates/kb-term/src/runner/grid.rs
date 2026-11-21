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

/// The grid owner string the web relay (`/api/terms/{id}/take-size`,
/// `release-size`) uses. Every web viewer shares it, so between web
/// viewers the latest explicit take wins.
pub const WEB_OWNER: &str = "kb-term";

/// Whether `owner` is the web relay owner.
#[must_use]
pub fn is_web_owner(owner: &str) -> bool {
    owner == WEB_OWNER
}

/// Whether `owner` is a local `kb-term attach` mirror (`local:<pid>`).
#[must_use]
pub fn is_local_owner(owner: &str) -> bool {
    owner.starts_with("local:")
}

/// The take decision (pure): the ONE precedence function.
///
/// - No current owner, or the same owner retaking: allowed.
/// - The web owner takes over a local mirror owner WITHOUT force (the
///   web outranks a local attach; the operator ruling of 2026-11-20).
///   This also frees a stale `local:<pid>` left behind by a dead attach.
/// - A local mirror can NEVER take over the web owner, force or not:
///   refused `grid-owned` with a message naming the web panel.
/// - Any other owner pair keeps the old rule: `force` overrides.
pub fn decide_take(current: &Option<String>, owner: &str, force: bool) -> Result<(), RunnerError> {
    match current {
        None => Ok(()),
        Some(cur) if cur == owner => Ok(()),
        Some(cur) if is_web_owner(owner) && is_local_owner(cur) => Ok(()),
        Some(cur) if is_local_owner(owner) && is_web_owner(cur) => Err(RunnerError::GridOwned(
            format!("grid is owned by {cur} (the web panel; release it there)"),
        )),
        Some(_) if force => Ok(()),
        Some(cur) => Err(RunnerError::GridOwned(format!("grid is owned by {cur}"))),
    }
}

/// The input gate: whether `viewer` may type into the session given the
/// grid's current `owner`. Only a viewer that is a local attach
/// (`local:<pid>`, the id the attach also uses as its grid owner) is ever
/// refused: it may type while it owns the grid or nobody does, and never
/// while another owner (the web relay, another attach) holds it. Every other
/// viewer (the web relay, the phone dock, the composer) is never refused.
pub fn decide_input(owner: &Option<String>, viewer: &str) -> Result<(), RunnerError> {
    match owner {
        Some(cur) if is_local_owner(viewer) && cur != viewer => {
            Err(RunnerError::GridOwned(format!("grid is owned by {cur}")))
        }
        _ => Ok(()),
    }
}

/// Run [`decide_input`] against the live owner. The tmux read happens only
/// for a local viewer, so web input pays nothing for the gate.
pub async fn check_input<S: crate::runner::supervisor::Supervisor>(
    runner: &Runner<S>,
    id: &str,
    viewer: &str,
) -> Result<(), RunnerError> {
    if !is_local_owner(viewer) {
        return Ok(());
    }
    let owner = read_owner(&runner.tmux, &runner.holder_socket(id)).await;
    decide_input(&owner, viewer)
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

    #[test]
    fn the_input_gate_refuses_only_a_local_viewer_under_another_owner() {
        let some = |o: &str| Some(o.to_string());
        // A local viewer: free grid and own grid pass; any other owner refuses.
        assert!(decide_input(&None, "local:7").is_ok());
        assert!(decide_input(&some("local:7"), "local:7").is_ok());
        for owner in ["kb-term", "local:8", "phone-1"] {
            let e = decide_input(&some(owner), "local:7").unwrap_err();
            assert_eq!(e.code(), "grid-owned");
            assert!(e.message().contains(owner), "{}", e.message());
        }
        // A web viewer is never refused, whoever owns the grid.
        for owner in [None, some("kb-term"), some("local:7"), some("phone-1")] {
            for viewer in ["web", "kb-term", "w-abc123", "phone-1"] {
                assert!(decide_input(&owner, viewer).is_ok(), "{owner:?} {viewer}");
            }
        }
    }
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

    #[test]
    fn precedence_table_covers_every_owner_class_and_force() {
        let web = Some(WEB_OWNER.to_string());
        let local = Some("local:41".to_string());
        let other = Some("relay-7".to_string());
        // (current, taker, force, allowed)
        let table: Vec<(Option<String>, &str, bool, bool)> = vec![
            (None, WEB_OWNER, false, true),
            (None, "local:9", false, true),
            (None, "relay-7", false, true),
            // Web over a local mirror: no force needed.
            (local.clone(), WEB_OWNER, false, true),
            (local.clone(), WEB_OWNER, true, true),
            // A local mirror never takes the web owner, even forced.
            (web.clone(), "local:9", false, false),
            (web.clone(), "local:9", true, false),
            // Web retake (latest explicit Fit wins between web viewers).
            (web.clone(), WEB_OWNER, false, true),
            (web.clone(), WEB_OWNER, true, true),
            // A local mirror retaking itself, or another local with force.
            (local.clone(), "local:41", false, true),
            (local.clone(), "local:9", false, false),
            (local.clone(), "local:9", true, true),
            // Other owner classes keep the old behaviour.
            (other.clone(), WEB_OWNER, false, false),
            (other.clone(), WEB_OWNER, true, true),
            (other.clone(), "local:9", false, false),
            (other.clone(), "local:9", true, true),
            (web.clone(), "relay-7", false, false),
            (web.clone(), "relay-7", true, true),
            (local.clone(), "relay-7", false, false),
            (local.clone(), "relay-7", true, true),
        ];
        for (cur, taker, force, allowed) in table {
            let got = decide_take(&cur, taker, force);
            assert_eq!(
                got.is_ok(),
                allowed,
                "current {cur:?} taker {taker} force {force}"
            );
            if let Err(e) = got {
                assert_eq!(e.code(), "grid-owned");
            }
        }
        // The refusal of a local mirror against the web names the web.
        let msg = decide_take(&web, "local:9", true).unwrap_err().message();
        assert!(msg.contains("web panel"), "names the web: {msg}");
        assert!(msg.starts_with("grid is owned by kb-term"), "{msg}");
    }

    #[tokio::test]
    async fn web_takes_a_local_owner_and_a_local_never_retakes_it() {
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
        // A (possibly dead) local attach owns the grid.
        take_size(&runner, &fake, id, 120, 40, "local:9", false)
            .await
            .unwrap();
        // The web takes it without force.
        let state = take_size(&runner, &fake, id, 201, 48, WEB_OWNER, false)
            .await
            .unwrap();
        assert_eq!((state.cols, state.rows), (201, 48));
        assert_eq!(read_owner(&fake, &sock).await, Some(WEB_OWNER.to_string()));
        // A local mirror, even forced, is refused and nothing resizes.
        let before = size_calls(&fake);
        for force in [false, true] {
            let err = take_size(&runner, &fake, id, 120, 40, "local:9", force)
                .await
                .unwrap_err();
            assert_eq!(err.code(), "grid-owned");
            assert!(err.message().contains("web panel"), "{}", err.message());
        }
        assert_eq!(size_calls(&fake), before, "refused takes never resize");
        assert_eq!(
            grid_state(&fake, &sock).await.unwrap(),
            GridState {
                cols: 201,
                rows: 48,
                owner: Some(WEB_OWNER.to_string())
            }
        );
        // Release only clears a matching owner: a local release is refused.
        let err = release_size(&runner, &fake, id, "local:9")
            .await
            .unwrap_err();
        assert_eq!(err.code(), "grid-owned");
        assert_eq!(read_owner(&fake, &sock).await, Some(WEB_OWNER.to_string()));
        release_size(&runner, &fake, id, WEB_OWNER).await.unwrap();
        assert_eq!(read_owner(&fake, &sock).await, None);
        // Owner none: the local mirror takes the grid again (no force).
        take_size(&runner, &fake, id, 120, 40, "local:9", false)
            .await
            .unwrap();
        assert_eq!(read_owner(&fake, &sock).await, Some("local:9".to_string()));
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
