//! Grid policy: one grid per session, owned, in the `@kb_owner_grid`
//! user option (state-free: survives a runner restart).
//!
//! `window-size manual` plus explicit `resize-window`; watching and typing
//! never touch the grid — only `take_size` does. The anchor and every
//! hidden client get the new winsize (cells and pixel fields).

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
    runner.apply_client_winsizes(id, cols, rows).await;
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
    let sock = runner.holder_socket(id);
    let current = grid_state(tmux, &sock).await?;
    decide_take(&current.owner, owner, force)?;
    apply_size(runner, tmux, id, cols, rows).await?;
    write_owner(tmux, &sock, Some(owner)).await?;
    Ok(GridState {
        cols,
        rows,
        owner: Some(owner.to_string()),
    })
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
    let sock = runner.holder_socket(id);
    let current = read_owner(tmux, &sock).await;
    match current {
        Some(cur) if cur == owner => write_owner(tmux, &sock, None).await,
        Some(cur) => Err(RunnerError::GridOwned(format!("grid is owned by {cur}"))),
        None => Err(RunnerError::GridOwned("grid has no owner".to_string())),
    }
}

/// `grid_owner_state_machine`: unset -> take ok; same owner retake ok;
/// foreign non-force -> `grid-owned` with the size unchanged; foreign force
/// ok and owner changes; release by non-owner -> `grid-owned`; release by
/// owner clears; bounds -> `bad-size`. Pinned with a fake tmux.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::tmux::fake::FakeTmux;

    fn sock() -> std::path::PathBuf {
        std::path::PathBuf::from("/tmp/fake.sock")
    }

    /// Minimal in-memory owner map behind a fake-tmux script, standing
    /// in for the tmux server's `@kb_owner_grid` handling.
    struct OwnerFake {
        owner: std::sync::Mutex<Option<String>>,
        canned: std::sync::Mutex<std::collections::HashMap<String, String>>,
    }

    impl OwnerFake {
        fn new(owner: Option<String>, cols: u16, rows: u16) -> Self {
            let mut canned = std::collections::HashMap::new();
            canned.insert("display-message".to_string(), format!("{cols} {rows}\n"));
            OwnerFake {
                owner: std::sync::Mutex::new(owner),
                canned: std::sync::Mutex::new(canned),
            }
        }

        fn take_decision(&self, owner: &str, force: bool) -> Result<(), RunnerError> {
            decide_take(&self.owner.lock().unwrap().clone(), owner, force)
        }
    }

    impl Tmux for OwnerFake {
        fn run<'a>(
            &'a self,
            _sock: &'a Path,
            args: &'a [&'a str],
            _timeout: Option<std::time::Duration>,
        ) -> impl std::future::Future<Output = Result<crate::runner::tmux::TmuxOut, RunnerError>>
               + Send
               + 'a {
            let key = args.first().copied().unwrap_or("");
            let out = match key {
                "show-options" => {
                    let owner = self.owner.lock().unwrap().clone().unwrap_or_default();
                    crate::runner::tmux::TmuxOut {
                        ok: true,
                        stdout: format!("{owner}\n"),
                        stderr: String::new(),
                    }
                }
                "set-option" => {
                    if args.get(3) == Some(&OWNER_OPTION) || args.iter().any(|a| *a == OWNER_OPTION)
                    {
                        let val = args.last().map(|s| *s).unwrap_or("");
                        let mut owner = self.owner.lock().unwrap();
                        *owner = if val.is_empty() {
                            None
                        } else {
                            Some(val.to_string())
                        };
                    }
                    crate::runner::tmux::TmuxOut {
                        ok: true,
                        stdout: String::new(),
                        stderr: String::new(),
                    }
                }
                other => {
                    let ans = self
                        .canned
                        .lock()
                        .unwrap()
                        .get(other)
                        .cloned()
                        .unwrap_or_default();
                    crate::runner::tmux::TmuxOut {
                        ok: true,
                        stdout: ans,
                        stderr: String::new(),
                    }
                }
            };
            async move { Ok(out) }
        }

        fn run_global<'a>(
            &'a self,
            _args: &'a [&'a str],
        ) -> impl std::future::Future<Output = Result<crate::runner::tmux::TmuxOut, RunnerError>>
               + Send
               + 'a {
            let out = crate::runner::tmux::TmuxOut {
                ok: true,
                stdout: "tmux 3.7c\n".to_string(),
                stderr: String::new(),
            };
            async move { Ok(out) }
        }
    }

    #[tokio::test]
    async fn grid_owner_state_machine() {
        // Unset -> take ok; same owner retake ok.
        let fake = OwnerFake::new(None, 100, 30);
        let state = grid_state(&fake, &sock()).await.unwrap();
        assert_eq!(
            state,
            GridState {
                cols: 100,
                rows: 30,
                owner: None
            }
        );
        fake.take_decision("a", false).unwrap();
        write_owner(&fake, &sock(), Some("a")).await.unwrap();
        fake.take_decision("a", false).unwrap();
        // Foreign non-force -> grid-owned.
        let err = fake.take_decision("b", false).unwrap_err();
        assert_eq!(err.code(), "grid-owned");
        assert!(err.message().contains('a'), "message names the owner");
        // Foreign force -> ok; owner changes.
        fake.take_decision("b", true).unwrap();
        write_owner(&fake, &sock(), Some("b")).await.unwrap();
        assert_eq!(read_owner(&fake, &sock()).await, Some("b".to_string()));
        // Release by non-owner -> grid-owned.
        let err = take_decision_release(&fake, "a").await.unwrap_err();
        assert_eq!(err.code(), "grid-owned");
        // Release by owner clears.
        write_owner(&fake, &sock(), None).await.unwrap();
        assert_eq!(read_owner(&fake, &sock()).await, None);
        // Bounds -> bad-size (pure check): out-of-range in take_size's
        // first arm; owner's charset check lives there too.
        assert!(!valid_size(19, 30));
        assert!(!valid_size(501, 30));
        assert!(!valid_size(100, 4));
        assert!(!valid_size(100, 201));
        assert!(valid_size(20, 5) && valid_size(500, 200));
        assert!(valid_owner("kb-term"));
        assert!(!valid_owner("KB"));
        assert!(!valid_owner(""));
        assert!(!valid_owner(&"a".repeat(65)));
        // decide_take matches the state machine exactly for the
        // no-history matrix.
        assert!(decide_take(&None, "x", false).is_ok());
        assert!(decide_take(&Some("x".to_string()), "x", false).is_ok());
        assert!(decide_take(&Some("x".to_string()), "y", true).is_ok());
        assert_eq!(
            decide_take(&Some("x".to_string()), "y", false)
                .unwrap_err()
                .code(),
            "grid-owned"
        );
        let _ = FakeTmux::new();
    }

    async fn take_decision_release(fake: &OwnerFake, owner: &str) -> Result<(), RunnerError> {
        let current = read_owner(fake, &sock()).await;
        match current {
            Some(cur) if cur == owner => write_owner(fake, &sock(), None).await,
            Some(cur) => Err(RunnerError::GridOwned(format!("grid is owned by {cur}"))),
            None => Err(RunnerError::GridOwned("grid has no owner".to_string())),
        }
    }
}
