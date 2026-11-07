//! kb-termd's own configuration (clap flags with env fallbacks).
//!
//! This is not `kb.toml`: the runner is operator-local, takes no kb
//! config, and logs only to stderr (filtered by `RUST_LOG`).

use std::path::PathBuf;

use clap::{Parser, ValueEnum};

/// How holders are started and stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SupervisorKind {
    /// `systemctl --user start kb-term@<id>.service` per session.
    Systemd,
    /// Spawn (and reap) the holder processes directly. CI has no systemd.
    Direct,
}

impl Default for SupervisorKind {
    fn default() -> Self {
        SupervisorKind::Systemd
    }
}

/// kb-termd configuration. `pane_shell` exists for the launch profiles
/// (T10); there is no flag for it.
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// Base dir: `<run-dir>/s/*.sock` holders, `<run-dir>/runner.sock`.
    pub run_dir: PathBuf,
    /// The PINNED tmux binary; never resolved via `PATH`.
    pub tmux_bin: String,
    /// The tmux config loaded by every holder.
    pub conf: PathBuf,
    /// `systemd` or `direct` holder supervision.
    pub supervisor: SupervisorKind,
    /// The tap binary (`direct` mode only).
    pub tap_bin: Option<PathBuf>,
    /// Wait for a control-mode client before releasing the harness.
    pub require_tap: bool,
    /// How long to wait for the tap (seconds).
    pub tap_ready_secs: u64,
    /// Default kill grace (seconds), clamped to 0–60 by the kill RPC.
    pub kill_grace_secs: u64,
    /// The tmux version the pinned binary must report.
    pub expected_tmux: String,
    /// Default grid columns (the spike values; an open operator decision).
    pub default_cols: u16,
    /// Default grid rows.
    pub default_rows: u16,
    /// Pixel size per cell, as `WxH` (for winsize pixel fields).
    pub cell_px: String,
    /// Shell the harness runs under (launch profiles land in T10).
    pub pane_shell: PathBuf,
    /// Args for `pane_shell` (the T10 launch profiles consume both).
    pub pane_shell_args: Vec<String>,
}

#[derive(Debug, Parser)]
#[command(name = "kb-termd", version, about = "kb-term runner")]
struct Cli {
    /// Base dir for session sockets and the runner socket.
    #[arg(long, env = "KB_TERM_RUN_DIR", default_value = "/run/kb-term")]
    run_dir: PathBuf,
    /// The pinned tmux binary (never resolved via PATH).
    #[arg(long, env = "KB_TMUX_BIN", default_value_t = default_tmux_bin())]
    tmux_bin: String,
    /// The tmux config loaded by every holder.
    #[arg(long, default_value_t = default_conf())]
    conf: String,
    /// `systemd` or `direct` holder supervision.
    #[arg(long, value_enum, default_value_t = SupervisorKind::Systemd)]
    supervisor: SupervisorKind,
    /// The tap binary (direct mode only).
    #[arg(long)]
    tap_bin: Option<PathBuf>,
    /// Wait for a control-mode client before releasing the harness.
    #[arg(long, value_parser = clap::value_parser!(bool), default_value = "true")]
    require_tap: bool,
    /// How long to wait for the tap (seconds).
    #[arg(long, default_value_t = 10)]
    tap_ready_secs: u64,
    /// Default kill grace (seconds).
    #[arg(long, default_value_t = 5)]
    kill_grace_secs: u64,
    /// The tmux version the pinned binary must report.
    #[arg(long, default_value = "3.7c")]
    expected_tmux: String,
    /// Default grid columns.
    #[arg(long, default_value_t = 120)]
    default_cols: u16,
    /// Default grid rows.
    #[arg(long, default_value_t = 40)]
    default_rows: u16,
    /// Pixel size per cell, as `WxH`.
    #[arg(long, default_value = "9x18")]
    cell_px: String,
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

fn default_tmux_bin() -> String {
    format!("{}/.local/libexec/kb-term/tmux", home())
}

fn default_conf() -> String {
    format!("{}/.local/share/kb-term/kb-term.conf", home())
}

impl RunnerConfig {
    /// Parse the command line (called by the kb-termd binary).
    pub fn from_cli() -> Self {
        let cli = Cli::parse();
        Self::from_cli_parts(cli)
    }

    fn from_cli_parts(cli: Cli) -> Self {
        RunnerConfig {
            run_dir: cli.run_dir,
            tmux_bin: cli.tmux_bin,
            conf: PathBuf::from(cli.conf),
            supervisor: cli.supervisor,
            tap_bin: cli.tap_bin,
            require_tap: cli.require_tap,
            tap_ready_secs: cli.tap_ready_secs,
            kill_grace_secs: cli.kill_grace_secs,
            expected_tmux: cli.expected_tmux,
            default_cols: cli.default_cols,
            default_rows: cli.default_rows,
            cell_px: cli.cell_px,
            pane_shell: PathBuf::from("/bin/sh"),
            pane_shell_args: vec!["-c".to_string()],
        }
    }

    /// A config for tests / in-process use.
    pub fn test_config(run_dir: PathBuf, tmux_bin: String, conf: PathBuf) -> Self {
        RunnerConfig {
            run_dir,
            tmux_bin,
            conf,
            supervisor: SupervisorKind::Direct,
            tap_bin: None,
            require_tap: false,
            tap_ready_secs: 10,
            kill_grace_secs: 5,
            expected_tmux: "3.7c".to_string(),
            default_cols: 120,
            default_rows: 40,
            cell_px: "9x18".to_string(),
            pane_shell: PathBuf::from("/bin/sh"),
            pane_shell_args: vec!["-c".to_string()],
        }
    }

    /// Pixel size per cell as `(width, height)`.
    pub fn cell_px_dims(&self) -> (u16, u16) {
        parse_cell_px(&self.cell_px).unwrap_or((9, 18))
    }
}

/// Parse a `WxH` cell size.
pub fn parse_cell_px(s: &str) -> Option<(u16, u16)> {
    let (w, h) = s.split_once('x')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conf_is_versioned_and_has_every_documented_option() {
        let conf = include_str!("../../share/kb-term.conf");
        assert!(
            conf.contains("@kb_conf_version 2"),
            "conf must carry the version marker"
        );
        for line in [
            "set -g default-terminal \"tmux-256color\"",
            "set -g prefix None",
            "set -g prefix2 None",
            "unbind-key -a -T root",
            "unbind-key -a -T prefix",
            "unbind-key -a -T copy-mode",
            "unbind-key -a -T copy-mode-vi",
            "set -g mouse off",
            "set -g window-size manual",
            "set -g default-size 120x40",
            "extended-keys always",
            "extended-keys-format csi-u",
            "history-limit 50000",
            "status off",
            "remain-on-exit on",
            "remain-on-exit-format \"\"",
            "exit-empty off",
            "allow-passthrough off",
        ] {
            assert!(conf.contains(line), "conf must contain `{line}`");
        }
        for removed in [
            "window-size latest",
            "focus-events on",
            "set-clipboard on",
            "client-active",
        ] {
            assert!(
                !conf.contains(removed),
                "conf must not contain the removed option `{removed}`"
            );
        }
        assert!(!conf.contains("mouse on"), "conf must not enable the mouse");
    }

    #[test]
    fn cell_px_parses_and_falls_back() {
        assert_eq!(parse_cell_px("9x18"), Some((9, 18)));
        assert_eq!(parse_cell_px("junk"), None);
        let cfg = RunnerConfig::test_config(
            PathBuf::from("/tmp/kb-term-test"),
            "/bin/true".to_string(),
            PathBuf::from("/dev/null"),
        );
        assert_eq!(cfg.cell_px_dims(), (9, 18));
    }
}
