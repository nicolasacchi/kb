//! kb-lip's TOML config: one file, one language server, `--config <path>`
//! on the CLI (design-lip.md §"Components" 1: "config gives argv,
//! workspace root, initializationOptions").

use serde::Deserialize;
use std::path::{Path, PathBuf};

fn default_port() -> u16 {
    4841
}

/// Capped exponential backoff for LSP-child restart attempts after a
/// crash. `next(attempt)` (attempt is 0-based, i.e. the Nth restart try)
/// returns the delay before that attempt, capped at `max_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct RestartBackoff {
    #[serde(default = "RestartBackoff::default_initial_ms")]
    pub initial_ms: u64,
    #[serde(default = "RestartBackoff::default_max_ms")]
    pub max_ms: u64,
    #[serde(default = "RestartBackoff::default_multiplier")]
    pub multiplier: f64,
}

impl RestartBackoff {
    fn default_initial_ms() -> u64 {
        200
    }
    fn default_max_ms() -> u64 {
        30_000
    }
    fn default_multiplier() -> f64 {
        2.0
    }

    /// Delay before restart attempt `attempt` (0-based: the first retry
    /// after a crash is `attempt == 0`), capped at `max_ms`.
    pub fn delay_ms(&self, attempt: u32) -> u64 {
        let scaled = (self.initial_ms as f64) * self.multiplier.powi(attempt as i32);
        if !scaled.is_finite() || scaled > self.max_ms as f64 {
            self.max_ms
        } else {
            scaled as u64
        }
    }
}

impl Default for RestartBackoff {
    fn default() -> Self {
        Self {
            initial_ms: Self::default_initial_ms(),
            max_ms: Self::default_max_ms(),
            multiplier: Self::default_multiplier(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Language ids this provider covers (e.g. `["ruby"]`). The first
    /// entry is used as the LSP `languageId` on `textDocument/didOpen`
    /// (a single-adapter-per-language-server design — see README/design
    /// doc; multi-language servers are a documented v1 limitation).
    pub lang_ids: Vec<String>,
    /// argv for the child language server, spawned directly (never
    /// through a shell) — `command[0]` is the binary, the rest are args.
    pub command: Vec<String>,
    pub workspace_root: PathBuf,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Raw `initializationOptions` passed verbatim in the LSP `initialize`
    /// request params.
    #[serde(default)]
    pub initialization_options: Option<serde_json::Value>,
    #[serde(default)]
    pub restart_backoff: RestartBackoff,
    /// `POST /lip/diagnostics`'s bounded wait for the first
    /// `publishDiagnostics` on a fresh open (design-addendum-2.md §D:
    /// "waits ≤2s configurable for the first publish"). Configurable
    /// DOWNWARD only — clamped to [`Config::MAX_DIAGNOSTICS_WAIT_MS`] in
    /// [`Config::parse`], never upward, so a misconfigured value can't
    /// turn a read endpoint into a multi-second hang.
    #[serde(default = "Config::default_diagnostics_wait_ms")]
    pub diagnostics_wait_ms: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("read config {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parse config {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("config `command` must not be empty")]
    EmptyCommand,
    #[error("config `lang_ids` must not be empty")]
    EmptyLangIds,
}

impl Config {
    /// Hard ceiling for `diagnostics_wait_ms` (design-addendum-2.md §D:
    /// "≤2s configurable").
    pub const MAX_DIAGNOSTICS_WAIT_MS: u64 = 2000;

    fn default_diagnostics_wait_ms() -> u64 {
        Self::MAX_DIAGNOSTICS_WAIT_MS
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text, path)
    }

    fn parse(text: &str, path: &Path) -> Result<Self, ConfigError> {
        let mut cfg: Config = toml::from_str(text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        cfg.validate()?;
        cfg.diagnostics_wait_ms = cfg.diagnostics_wait_ms.min(Self::MAX_DIAGNOSTICS_WAIT_MS);
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.command.is_empty() {
            return Err(ConfigError::EmptyCommand);
        }
        if self.lang_ids.is_empty() {
            return Err(ConfigError::EmptyLangIds);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_minimal_config() {
        let toml = r#"
            lang_ids = ["ruby"]
            command = ["ruby-lsp"]
            workspace_root = "/repo"
        "#;
        let cfg = Config::parse(toml, Path::new("test.toml")).unwrap();
        assert_eq!(cfg.lang_ids, vec!["ruby".to_string()]);
        assert_eq!(cfg.command, vec!["ruby-lsp".to_string()]);
        assert_eq!(cfg.workspace_root, PathBuf::from("/repo"));
        assert_eq!(cfg.port, 4841);
        assert!(cfg.initialization_options.is_none());
        assert_eq!(cfg.restart_backoff, RestartBackoff::default());
        assert_eq!(cfg.diagnostics_wait_ms, Config::MAX_DIAGNOSTICS_WAIT_MS);
    }

    #[test]
    fn diagnostics_wait_ms_accepts_a_value_under_the_cap() {
        let toml = r#"
            lang_ids = ["ruby"]
            command = ["ruby-lsp"]
            workspace_root = "/repo"
            diagnostics_wait_ms = 500
        "#;
        let cfg = Config::parse(toml, Path::new("test.toml")).unwrap();
        assert_eq!(cfg.diagnostics_wait_ms, 500);
    }

    #[test]
    fn diagnostics_wait_ms_clamps_down_to_the_cap_never_up() {
        let toml = r#"
            lang_ids = ["ruby"]
            command = ["ruby-lsp"]
            workspace_root = "/repo"
            diagnostics_wait_ms = 999999
        "#;
        let cfg = Config::parse(toml, Path::new("test.toml")).unwrap();
        assert_eq!(cfg.diagnostics_wait_ms, Config::MAX_DIAGNOSTICS_WAIT_MS);
    }

    #[test]
    fn parses_a_full_config() {
        let toml = r#"
            lang_ids = ["ruby", "erb"]
            command = ["ruby-lsp", "--stdio"]
            workspace_root = "/repo"
            port = 5000
            initialization_options = { foo = "bar", n = 3 }

            [restart_backoff]
            initial_ms = 100
            max_ms = 5000
            multiplier = 1.5
        "#;
        let cfg = Config::parse(toml, Path::new("test.toml")).unwrap();
        assert_eq!(cfg.lang_ids, vec!["ruby".to_string(), "erb".to_string()]);
        assert_eq!(cfg.port, 5000);
        assert_eq!(
            cfg.initialization_options,
            Some(serde_json::json!({"foo": "bar", "n": 3}))
        );
        assert_eq!(
            cfg.restart_backoff,
            RestartBackoff {
                initial_ms: 100,
                max_ms: 5000,
                multiplier: 1.5
            }
        );
    }

    #[test]
    fn partial_restart_backoff_table_fills_in_defaults() {
        let toml = r#"
            lang_ids = ["ruby"]
            command = ["ruby-lsp"]
            workspace_root = "/repo"

            [restart_backoff]
            initial_ms = 50
        "#;
        let cfg = Config::parse(toml, Path::new("test.toml")).unwrap();
        assert_eq!(cfg.restart_backoff.initial_ms, 50);
        assert_eq!(cfg.restart_backoff.max_ms, RestartBackoff::default_max_ms());
    }

    #[test]
    fn rejects_empty_command() {
        let toml = r#"
            lang_ids = ["ruby"]
            command = []
            workspace_root = "/repo"
        "#;
        let err = Config::parse(toml, Path::new("test.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::EmptyCommand));
    }

    #[test]
    fn rejects_empty_lang_ids() {
        let toml = r#"
            lang_ids = []
            command = ["ruby-lsp"]
            workspace_root = "/repo"
        "#;
        let err = Config::parse(toml, Path::new("test.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::EmptyLangIds));
    }

    #[test]
    fn rejects_malformed_toml() {
        let err = Config::parse("not valid toml [[[", Path::new("test.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn load_missing_file_errors() {
        let err = Config::load(Path::new("/definitely/does/not/exist.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::Read { .. }));
    }

    #[test]
    fn backoff_delay_is_capped_exponential() {
        let b = RestartBackoff {
            initial_ms: 200,
            max_ms: 3000,
            multiplier: 2.0,
        };
        assert_eq!(b.delay_ms(0), 200);
        assert_eq!(b.delay_ms(1), 400);
        assert_eq!(b.delay_ms(2), 800);
        assert_eq!(b.delay_ms(3), 1600);
        // Would be 3200 uncapped — must clamp at max_ms.
        assert_eq!(b.delay_ms(4), 3000);
        assert_eq!(b.delay_ms(20), 3000);
    }
}
