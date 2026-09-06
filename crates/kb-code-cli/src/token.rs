//! V70-A8 (D20 CLI hygiene, secret-hygiene item) — where this CLI's own
//! bearer token comes from when talking to a non-loopback `kb-code-server`.
//!
//! # Why this exists
//!
//! `lib.rs::bind_and_spawn` already reads `KB_CODE_TOKEN` server-side (the
//! VALUE the daemon expects on `Authorization: Bearer <token>`, invariant
//! #4's fail-closed-on-a-token-less-public-bind rule). Before this unit,
//! this CLI had NO way at all to SEND that header — every request built by
//! [`crate::client_builder`] went out unauthenticated, so a `kb-code`
//! invocation against any daemon reachable only over a token-gated bind
//! (recon `cli-agent-surface.md` open question 2: "does the CLI need
//! bearer-token support … or is 'loopback-only client' the deliberate
//! posture? kbc.example.com is token-gated today") simply 401'd on every call.
//!
//! # The two sources, and why no `--token` flag
//!
//! A token in `argv` leaks to `ps`, to shell history, and to any transcript
//! that logs the invoked command line — D20 rules this out explicitly
//! ("token_file … with KB_CODE_TOKEN for containers … no --token flag").
//! So exactly two sources, checked in this order:
//!
//! 1. **`KB_CODE_TOKEN`** (env) — for containers/CI, where an env var is
//!    the natural secret-injection surface and there is often no
//!    persistent, permission-controllable filesystem to hold a file.
//! 2. **`token_file`** (default `<config>/kb-code-token`, i.e. the SAME
//!    shared `KbPaths::new("kb-code").config` directory `kb-code.toml`
//!    lives in, per that file's own doc: `KbPaths`' config root is the
//!    fixed `kb` directory regardless of the daemon-name argument, so
//!    collision-avoidance comes from the FILENAME — `kb-code-token`,
//!    distinct from kb's own `<config>/token`, never the same file) —
//!    override the path with `KB_CODE_TOKEN_FILE`. Mirrors kb's own
//!    `KbDaemonSection::bearer_token`/`GithubSection::bearer_token`
//!    convention exactly: read + trim, degrade to `None` on ANY failure
//!    (missing file, unreadable, empty) rather than erroring — a
//!    token-less request is a normal, expected shape for the common
//!    loopback-daemon case, not a misconfiguration to fail loudly over.
//!
//! Neither source is EVER echoed back by this module — [`token_path`]
//! prints the FILE PATH only (`kb-code token path`, mirroring kb's own
//! `kb token path`); there is no `kb-code token show`.

use anyhow::{Context, Result};
use kb_core::paths::KbPaths;
use std::path::PathBuf;

/// Resolve the token FILE path: `KB_CODE_TOKEN_FILE` if set (non-empty,
/// trimmed), else `<config>/kb-code-token`. This is a PATH, never a
/// secret, so it is safe to build even when the file doesn't exist and
/// safe to print (`kb-code token path`'s whole job).
pub fn token_file_path() -> Result<PathBuf> {
    if let Some(over) = std::env::var_os("KB_CODE_TOKEN_FILE") {
        let over = PathBuf::from(over);
        if !over.as_os_str().is_empty() {
            return Ok(over);
        }
    }
    let paths = KbPaths::new("kb-code").context("resolve kb-code XDG paths")?;
    Ok(paths.config.join("kb-code-token"))
}

/// Resolve the bearer token this CLI should send, or `None` when neither
/// source yields one (the ordinary shape for a loopback daemon — no error,
/// no warning; [`crate::client_builder`] simply sends no `Authorization`
/// header, exactly like every pre-V70-A8 invocation).
///
/// Precedence: `KB_CODE_TOKEN` env (trimmed, non-empty) first, else the
/// token file (trimmed, non-empty) — see the module doc for why both exist.
pub fn resolve_bearer_token() -> Option<String> {
    if let Ok(raw) = std::env::var("KB_CODE_TOKEN") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    let path = token_file_path().ok()?;
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// `kb-code token path` — print the resolved FILE PATH (never the token
/// itself; there is no `kb-code token show`, deliberately — see module
/// doc). Prints the path whether or not the file currently exists (a
/// caller running this before ever writing the file still learns WHERE to
/// write it, `chmod 600`'d, themselves).
pub fn cmd_token_path() -> Result<()> {
    let path = token_file_path()?;
    println!("{}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// RAII guard: sets an env var for the duration of one test, restores
    /// the prior value (or removes it) on drop — even on panic — so a
    /// failing assertion can never leak `KB_CODE_TOKEN`/`KB_CODE_TOKEN_FILE`
    /// into a sibling test in the same process. `std::env::set_var` is
    /// process-global, same caveat every other env-based test in this
    /// workspace already carries (e.g. `KB_HOME` in kb-core's own suite).
    struct EnvGuard {
        key: &'static str,
        prior: Option<std::ffi::OsString>,
    }
    impl EnvGuard {
        fn set(key: &'static str, value: &std::ffi::OsStr) -> Self {
            let prior = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, prior }
        }
        fn unset(key: &'static str) -> Self {
            let prior = std::env::var_os(key);
            std::env::remove_var(key);
            Self { key, prior }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.prior.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn env_var_wins_over_token_file_and_is_trimmed() {
        let _clear_file_override = EnvGuard::unset("KB_CODE_TOKEN_FILE");
        let _env = EnvGuard::set(
            "KB_CODE_TOKEN",
            std::ffi::OsStr::new("  secret-from-env  \n"),
        );
        assert_eq!(resolve_bearer_token().as_deref(), Some("secret-from-env"));
    }

    #[test]
    fn falls_back_to_token_file_when_env_absent() {
        let _clear_env = EnvGuard::unset("KB_CODE_TOKEN");
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("kb-code-token");
        std::fs::File::create(&file)
            .unwrap()
            .write_all(b"secret-from-file\n")
            .unwrap();
        let _file_override = EnvGuard::set("KB_CODE_TOKEN_FILE", file.as_os_str());
        assert_eq!(resolve_bearer_token().as_deref(), Some("secret-from-file"));
    }

    #[test]
    fn missing_and_empty_both_degrade_to_none_never_an_error() {
        let _clear_env = EnvGuard::unset("KB_CODE_TOKEN");
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("does-not-exist");
        {
            let _file_override = EnvGuard::set("KB_CODE_TOKEN_FILE", missing.as_os_str());
            assert_eq!(resolve_bearer_token(), None);
        }
        let empty = dir.path().join("empty-token");
        std::fs::File::create(&empty).unwrap();
        let _file_override = EnvGuard::set("KB_CODE_TOKEN_FILE", empty.as_os_str());
        assert_eq!(resolve_bearer_token(), None);
    }

    #[test]
    fn token_path_honours_the_env_override() {
        let dir = tempfile::tempdir().expect("tempdir");
        let want = dir.path().join("custom-token-path");
        let _file_override = EnvGuard::set("KB_CODE_TOKEN_FILE", want.as_os_str());
        assert_eq!(token_file_path().unwrap(), want);
    }
}
