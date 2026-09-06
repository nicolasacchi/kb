//! kb-core — types, parsing, storage, indexer, events.
//!
//! No HTTP, no terminal, no CLI. Pure library consumed by `kb-server`
//! and `kb-cli`.

pub mod anchors;
pub mod atlas;
pub mod atlas_field;
pub mod atlas_labels;
pub mod attachments;
pub mod capture;
pub mod cascade;
pub mod chunk;
pub mod coderefs;
pub mod config;
pub mod corkboard;
pub mod docs_query;
pub mod embed;
pub mod embed_ipc;
pub mod enrich;
pub mod error;
pub mod events;
pub mod exclusions;
pub mod extmap;
pub mod fsx;
pub mod fusion;
pub mod graph_report;
pub mod headings;
pub mod history;
pub mod identity;
pub mod ids;
pub mod iframe;
pub mod indexer;
pub mod links;
pub mod lists;
pub mod markdown;
pub mod memory;
pub mod mentions;
pub mod meta_edit;
pub mod metrics;
pub mod notes;
pub mod parser;
pub mod paths;
pub mod procrustes;
pub mod query;
pub mod reading;
pub mod relocate;
pub mod resurface;
pub mod review;
pub mod scrub;
pub mod session_bundle;
pub mod session_render;
pub mod session_scrub;
pub mod sessions;
pub mod share;
pub mod sibling;
pub mod slate;
pub mod slo;
pub mod storage;
pub mod strutil;
#[cfg(test)]
pub(crate) mod test_support;
pub mod timeparse;
pub mod tracing_init;
pub mod triage;
pub mod types;
pub mod vcs;
pub mod versions;
pub mod watcher;
pub mod webhook_url;

pub use error::{Error, Result};
