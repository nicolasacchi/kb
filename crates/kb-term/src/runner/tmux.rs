//! The tmux driver: argv-only invocations of the pinned tmux binary.
//!
//! The environment is cleared (only `LANG` and `HOME` are set) so an id,
//! a path or user text can never be reinterpreted by a shell. Set-option
//! values go after `--`.

use std::path::Path;
use std::time::Duration;

use tokio::process::Command;

use super::RunnerError;

/// The result of one tmux invocation.
#[derive(Debug, Clone)]
pub struct TmuxOut {
    /// Whether tmux exited 0.
    pub ok: bool,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
}

/// tmux operations the runner needs. Async in the trait (Rust 1.91): the
/// production impl and the test fake both satisfy it.
pub trait Tmux: Send + Sync {
    /// Run the pinned tmux against `sock` with `args` (5 s default timeout
    /// in the real impl; `timeout` overrides for stale detection/slow polls).
    fn run<'a>(
        &'a self,
        sock: &'a Path,
        args: &'a [&'a str],
        timeout: Option<Duration>,
    ) -> impl std::future::Future<Output = Result<TmuxOut, RunnerError>> + Send + 'a;

    /// Run the pinned tmux with NO socket (used for `-V`).
    fn run_global<'a>(
        &'a self,
        args: &'a [&'a str],
    ) -> impl std::future::Future<Output = Result<TmuxOut, RunnerError>> + Send + 'a;

    /// The pinned tmux's `-V` output.
    async fn version(&self) -> Result<String, String> {
        let out = self.run_global(&["-V"]).await.map_err(|e| e.message())?;
        if out.ok {
            Ok(out.stdout)
        } else {
            Err(out.stderr)
        }
    }
}

/// The production tmux driver: `<tmux-bin> -S <sock> <args>`, env cleared.
#[derive(Debug, Clone)]
pub struct RealTmux {
    bin: String,
}

impl RealTmux {
    /// The pinned tmux binary path.
    pub fn new(bin: String) -> Self {
        RealTmux { bin }
    }

    async fn exec(&self, argv: &[String], timeout: Duration) -> Result<TmuxOut, RunnerError> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(argv);
        cmd.env_clear();
        cmd.env("LANG", "C.UTF-8");
        cmd.env("HOME", std::env::var("HOME").unwrap_or_default());
        cmd.kill_on_drop(true);
        let out = tokio::time::timeout(timeout, cmd.output())
            .await
            .map_err(|_| RunnerError::TmuxFailed("tmux command timed out".to_string()))?
            .map_err(|e| RunnerError::TmuxFailed(format!("spawn tmux: {e}")))?;
        Ok(TmuxOut {
            ok: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

impl Tmux for RealTmux {
    fn run<'a>(
        &'a self,
        sock: &'a Path,
        args: &'a [&'a str],
        timeout: Option<Duration>,
    ) -> impl std::future::Future<Output = Result<TmuxOut, RunnerError>> + Send + 'a {
        let sock = sock.to_path_buf();
        let args: Vec<String> = args.iter().map(ToString::to_string).collect();
        let this = self.clone();
        async move { self::run_real(this, sock, args, timeout).await }
    }

    fn run_global<'a>(
        &'a self,
        args: &'a [&'a str],
    ) -> impl std::future::Future<Output = Result<TmuxOut, RunnerError>> + Send + 'a {
        let args: Vec<String> = args.iter().map(ToString::to_string).collect();
        let this = self.clone();
        async move { self::run_global_real(this, args).await }
    }
}

async fn run_real(
    this: RealTmux,
    sock: std::path::PathBuf,
    args: Vec<String>,
    timeout: Option<Duration>,
) -> Result<TmuxOut, RunnerError> {
    let mut argv = vec!["-S".to_string(), sock.to_string_lossy().into_owned()];
    argv.extend(args.into_iter());
    this.exec(&argv, timeout.unwrap_or(Duration::from_secs(5)))
        .await
}

async fn run_global_real(this: RealTmux, args: Vec<String>) -> Result<TmuxOut, RunnerError> {
    this.exec(&args, Duration::from_secs(5)).await
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::sync::Mutex;

    /// A fake [`Tmux`] that records every call and answers from a script.
    #[derive(Default)]
    pub struct FakeTmux {
        /// `(socket, args)` per call, in order.
        pub calls: Mutex<Vec<(String, Vec<String>)>>,
        /// Canned stdout per first-arg key (e.g. `"display-message"`).
        pub canned: Mutex<std::collections::HashMap<String, String>>,
        /// Canned `-V` output.
        pub version_out: Mutex<String>,
        /// When true, every call "times out".
        pub timeout: bool,
    }

    impl FakeTmux {
        /// A fake that answers `show-options` etc. with empty success.
        pub fn new() -> Self {
            FakeTmux {
                version_out: Mutex::new("tmux 3.7c\n".to_string()),
                ..FakeTmux::default()
            }
        }

        /// Record one call and compute its canned answer.
        fn answer(
            &self,
            sock: &Path,
            args: &[&str],
            _timeout: Option<Duration>,
        ) -> Result<TmuxOut, RunnerError> {
            self.calls.lock().unwrap().push((
                sock.to_string_lossy().into_owned(),
                args.iter().map(ToString::to_string).collect(),
            ));
            if self.timeout {
                return Err(RunnerError::TmuxFailed(
                    "tmux command timed out".to_string(),
                ));
            }
            let key = args.first().copied().unwrap_or("");
            let out = self
                .canned
                .lock()
                .unwrap()
                .get(key)
                .cloned()
                .unwrap_or_default();
            Ok(TmuxOut {
                ok: true,
                stdout: out,
                stderr: String::new(),
            })
        }
    }

    impl Tmux for FakeTmux {
        fn run<'a>(
            &'a self,
            sock: &'a Path,
            args: &'a [&'a str],
            timeout: Option<Duration>,
        ) -> impl std::future::Future<Output = Result<TmuxOut, RunnerError>> + Send + 'a {
            // The answer is computed synchronously; the future is only
            // scheduled, so the !Send Mutex guards never cross an await.
            let out = self.answer(sock, args, timeout);
            async move { out }
        }

        fn run_global<'a>(
            &'a self,
            _args: &'a [&'a str],
        ) -> impl std::future::Future<Output = Result<TmuxOut, RunnerError>> + Send + 'a {
            let out = TmuxOut {
                ok: true,
                stdout: self.version_out.lock().unwrap().clone(),
                stderr: String::new(),
            };
            async move { Ok(out) }
        }
    }
}
