//! Offline kb-cli integration tests (no daemon boot).
//!
//! Filter example: `cargo test -p kb-cli --test cli_offline lookup::`

#[allow(unused)]
#[path = "../common/mod.rs"]
mod common;

mod adapter_grok;
mod atlas;
mod doctor;
mod doctor_hooks;
mod fleet;
mod json_output;
mod lookup;
mod pull;
mod push;
mod reindex;
mod status;
mod status_watch;
mod token;
