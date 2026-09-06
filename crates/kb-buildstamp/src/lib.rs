//! Compile-time git stamp consts — see `build.rs` for the probe and the
//! PF-B1 rationale (a leaf crate is the only place a `.git/HEAD` watcher
//! can live without recompiling the kb-server lib on every commit).
//!
//! ONLY bin targets may depend on this crate: the stamped value changes on
//! every commit, so any LIB that links it drags its whole dependent tree
//! into the per-commit rebuild — exactly the cascade this crate exists to
//! break. `kb` (kb-cli) uses both consts (clap `version` +
//! `kb_server::set_build_stamp` before `serve_loop`); the standalone
//! `kb-server` bin deliberately does NOT link this crate and stamps from
//! plain `option_env!` instead (the kb-code-server precedent), degrading
//! to `unknown`/`0.0.0-dev` in dev — which the SPA drift guard treats as
//! "no stamp, no-op" by contract.

/// Git commit this binary was built from (`--short=12`, `-dirty` suffix
/// when the tree is dirty; `unknown` absent git / override). Surfaced on
/// `GET /api/identity` as `build_sha` for the SPA drift guard.
pub const BUILD_SHA: &str = env!("KB_GIT_SHA");

/// Tag-derived `git describe` stamp with the `v` prefix stripped
/// (e.g. `0.13-42-g9fd65fd`), NOT the Cargo version — the workspace pins
/// that at 0.0.0 forever and versions via tags. Backs `kb --version` and
/// `/api/identity.version`; `0.0.0-dev` absent git / override.
pub const VERSION: &str = env!("KB_GIT_DESCRIBE");
