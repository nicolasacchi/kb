//! One module per `kb` subcommand.

pub mod add;
pub mod atlas;
pub mod atlas_field;
pub mod backup;
pub mod bench;
pub mod board;
pub mod capture;
pub mod cat;
pub mod comments;
pub mod comments_watch;
pub mod compact;
pub mod config;
pub mod context;
pub mod daemon;
pub mod daycard;
pub mod desk;
pub mod doctor;
pub mod download;
pub mod events;
pub mod exclude;
pub mod find;
pub mod fleet;
pub mod get;
pub mod graph;
pub mod history;
pub mod import;
pub mod index_page;
pub mod links;
pub mod list;
pub mod memory;
pub mod metrics;
pub mod model;
pub mod mv;
pub mod new;
pub mod notes;
pub mod prompt;
pub mod proposals;
pub mod pull;
pub mod push;
pub mod queries;
pub mod read;
pub mod reading;
pub mod refs;
pub mod reindex;
pub mod related;
pub mod reset;
pub mod restore;
pub mod resurface;
pub mod search;
pub mod session_bundle;
pub mod session_read;
pub mod sessions;
pub mod sessions_capture;
pub mod sessions_status;
pub mod share;
pub mod similar;
pub mod slate;
pub mod slo;
pub mod sources;
pub mod status;
pub mod synth;
pub mod timeline;
pub mod token;
pub mod tools;
pub mod users;
pub mod versions;
pub mod whoami;
pub mod why_memory;

use anyhow::{Context, Result};
use kb_core::config::KbConfig;
use kb_core::paths::KbPaths;
use std::path::{Path, PathBuf};

/// Resolve the kb.toml path: explicit `--config` wins; otherwise the daemon's
/// XDG config path. Falls back to `kb.toml` in the cwd if neither resolves.
pub fn resolve_config_path(explicit: Option<&PathBuf>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.clone());
    }
    let paths = KbPaths::new("default").context("XDG paths")?;
    Ok(paths.config_file())
}

/// Load `kb.toml` from `path`. If the file doesn't exist, returns
/// `KbConfig::default()` (so `kb add` and `kb status` don't blow up
/// before any kb is configured).
pub fn load_config_or_default(path: &Path) -> Result<KbConfig> {
    if path.exists() {
        Ok(KbConfig::load(path)?)
    } else {
        Ok(KbConfig::default())
    }
}
