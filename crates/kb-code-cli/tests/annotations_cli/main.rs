//! Annotations + hook CLI harness.
//!
//! Filter example: `cargo test -p kb-code-cli --test annotations_cli`

#[allow(unused)]
#[path = "../common/mod.rs"]
mod common;

mod annotations;
mod annotations_codex_hook;
mod annotations_hook;
mod v4;
mod watch_unified;
