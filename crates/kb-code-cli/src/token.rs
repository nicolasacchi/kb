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
    let default = KbPaths::new("kb-code")
        .context("resolve kb-code XDG paths")?
        .config
        .join("kb-code-token");
    Ok(resolve_token_file_path(
        std::env::var_os("KB_CODE_TOKEN_FILE"),
        default,
    ))
}

/// Pure decision half of [`token_file_path`]: an explicit, non-empty
/// override wins, else the caller's default. Split out so the ladder is
/// unit-testable without touching the process environment at all — see the
/// `tests` module doc for why that matters.
fn resolve_token_file_path(env_override: Option<std::ffi::OsString>, default: PathBuf) -> PathBuf {
    if let Some(over) = env_override {
        let over = PathBuf::from(over);
        if !over.as_os_str().is_empty() {
            return over;
        }
    }
    default
}

/// Resolve the bearer token this CLI should send, or `None` when neither
/// source yields one (the ordinary shape for a loopback daemon — no error,
/// no warning; [`crate::client_builder`] simply sends no `Authorization`
/// header, exactly like every pre-V70-A8 invocation).
///
/// Precedence: `KB_CODE_TOKEN` env (trimmed, non-empty) first, else the
/// token file (trimmed, non-empty) — see the module doc for why both exist.
pub fn resolve_bearer_token() -> Option<String> {
    resolve_bearer_token_ladder(std::env::var("KB_CODE_TOKEN").ok(), || {
        token_file_path().ok()
    })
}

/// Pure ladder driving [`resolve_bearer_token`]: `env_value` wins when
/// present and non-empty (trimmed); otherwise the (lazily resolved —
/// short-circuits the XDG-path lookup when the env wins) file path is read.
/// Split out, with the env/file halves further split below, so every branch
/// is unit-testable without any `std::env::set_var`/`remove_var`.
fn resolve_bearer_token_ladder(
    env_value: Option<String>,
    file_path: impl FnOnce() -> Option<PathBuf>,
) -> Option<String> {
    if let Some(token) = resolve_bearer_token_from_env(env_value) {
        return Some(token);
    }
    resolve_bearer_token_from_file(&file_path()?)
}

/// The `KB_CODE_TOKEN` half of the ladder: trim, degrade absent/blank to
/// `None`.
fn resolve_bearer_token_from_env(raw: Option<String>) -> Option<String> {
    let raw = raw?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// The `token_file` half of the ladder: read + trim, degrade ANY failure
/// (missing, unreadable, empty) to `None` rather than erroring — see the
/// module doc.
fn resolve_bearer_token_from_file(path: &std::path::Path) -> Option<String> {
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
    //! **No `std::env::set_var`/`remove_var` anywhere in this module.**
    //!
    //! The previous version of this suite drove the public,
    //! process-env-reading entry points (`resolve_bearer_token`,
    //! `token_file_path`) directly, so each test had to mutate
    //! `KB_CODE_TOKEN`/`KB_CODE_TOKEN_FILE` for its own duration (restored on
    //! drop via an `EnvGuard`). That guard prevented LEAKING a mutation
    //! past its own test, but did nothing to stop a *sibling* test's
    //! mutation from being observed mid-flight: `cargo test` runs `#[test]`
    //! fns on parallel threads by default, and `std::env::set_var`/`var`
    //! read/write the one process-global environment table with no
    //! synchronization between threads. That's exactly what main CI run
    //! 34048706473 (commit 7a82ca15) hit:
    //! `falls_back_to_token_file_when_env_absent` panicked with
    //! `left: None, right: Some("secret-from-file")` — this test's own
    //! `KB_CODE_TOKEN` unset raced against `env_var_wins_over_token_file_
    //! and_is_trimmed`'s concurrent `KB_CODE_TOKEN` set on another thread,
    //! so the ladder saw a non-empty env value and returned the env
    //! branch's answer instead of falling through to the file. Intermittent
    //! by nature (thread scheduling), which is why it passed on the PR runs
    //! immediately before and after.
    //!
    //! The fix here is structural, not a bigger lock: `resolve_bearer_token`
    //! and `token_file_path` are now thin wrappers over pure functions
    //! (`resolve_bearer_token_ladder` + its `_from_env`/`_from_file` halves,
    //! `resolve_token_file_path`) that take their inputs as plain
    //! parameters. Tests call the pure functions directly — there is no
    //! process env left to race on.
    use super::*;
    use std::io::Write;

    #[test]
    fn env_wins_over_a_real_token_file_and_is_trimmed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("kb-code-token");
        std::fs::File::create(&file)
            .unwrap()
            .write_all(b"secret-from-file\n")
            .unwrap();
        let result = resolve_bearer_token_ladder(Some("  secret-from-env  \n".to_string()), || {
            Some(file.clone())
        });
        assert_eq!(result.as_deref(), Some("secret-from-env"));
    }

    #[test]
    fn falls_back_to_token_file_when_env_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("kb-code-token");
        std::fs::File::create(&file)
            .unwrap()
            .write_all(b"secret-from-file\n")
            .unwrap();
        let result = resolve_bearer_token_ladder(None, || Some(file.clone()));
        assert_eq!(result.as_deref(), Some("secret-from-file"));
    }

    #[test]
    fn missing_and_empty_both_degrade_to_none_never_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("does-not-exist");
        assert_eq!(
            resolve_bearer_token_ladder(None, || Some(missing.clone())),
            None
        );

        let empty = dir.path().join("empty-token");
        std::fs::File::create(&empty).unwrap();
        assert_eq!(
            resolve_bearer_token_ladder(None, || Some(empty.clone())),
            None
        );
    }

    #[test]
    fn env_absent_or_blank_never_short_circuits_on_its_own() {
        assert_eq!(resolve_bearer_token_from_env(None), None);
        assert_eq!(
            resolve_bearer_token_from_env(Some("   \n".to_string())),
            None
        );
    }

    #[test]
    fn token_path_honours_the_env_override() {
        let want = PathBuf::from("/tmp/kb-code-token-fixture/custom-token-path");
        let default = PathBuf::from("/tmp/kb-code-token-fixture/default-path");
        assert_eq!(
            resolve_token_file_path(Some(want.clone().into_os_string()), default),
            want
        );
    }

    #[test]
    fn token_path_falls_back_to_default_when_override_absent_or_empty() {
        let default = PathBuf::from("/tmp/kb-code-token-fixture/default-path");
        assert_eq!(resolve_token_file_path(None, default.clone()), default);
        assert_eq!(
            resolve_token_file_path(Some(std::ffi::OsString::new()), default.clone()),
            default
        );
    }
}
