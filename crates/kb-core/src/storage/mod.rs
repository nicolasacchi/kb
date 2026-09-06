//! Storage glue — sqlite (sources, errors, runs, edges) + lance (FTS-only
//! in v0.0.1; embedding column reserved nullable). Single-writer per kb is
//! enforced by an mpsc-driven actor (added in phase 8).

pub mod actor;
pub mod backup;
pub mod lance;
pub mod schema;
pub mod sqlite;

pub use actor::{StorageActor, StorageHandle};
