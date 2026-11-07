//! Offline kb-cli integration tests (no daemon boot).
//!
//! Filter example: `cargo test -p kb-cli --test cli_offline lookup::`

#[allow(unused)]
#[path = "../common/mod.rs"]
mod common;

mod adapter_codex;
mod adapter_grok;
mod adapter_opencode;
mod atlas;
mod chores;
mod doctor;
mod doctor_hooks;
mod fleet;
mod hook_shell;
mod import_memory;
mod json_output;
mod lookup;
mod public_mirror;
mod pull;
mod push;
mod reindex;
mod sessions_scrub;
mod status;
mod status_watch;
mod test_sandbox_lint;
mod token;
mod version;
