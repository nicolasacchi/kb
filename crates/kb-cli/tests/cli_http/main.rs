//! HTTP-backed kb-cli integration tests (in-process daemon).
//!
//! Filter example: `cargo test -p kb-cli --test cli_http comments::`

#[allow(unused)]
#[path = "../common/mod.rs"]
mod common;

mod capture;
mod cat_read;
mod comments;
mod desk;
mod diff_between_epoch;
mod download;
mod events;
mod exclude;
mod memory_cli;
mod notes;
mod queries;
mod search_json;
mod share;
mod slate;
