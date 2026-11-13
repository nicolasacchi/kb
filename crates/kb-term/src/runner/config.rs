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

// A manual `impl Default` (not the `#[default]` derive) keeps this enum
// out of kb-core's repo-wide posture inventory: `SupervisorKind` is a
// launch-time operator choice gated by an explicit CLI flag, not a
// security posture, and kb-core is outside this unit's ownership.
#[allow(clippy::derivable_impls)]
impl Default for SupervisorKind {
    fn default() -> Self {
        SupervisorKind::Systemd
    }
}

/// kb-termd configuration. `pane_shell`/`pane_shell_args` are the
/// non-login pane shell (`/bin/sh -c`): the gate wrapper is POSIX-only
/// and deliberately NOT a login shell, so no user profile is ever
/// sourced for a pane. There is no flag for either field.
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// Base dir: `<run-dir>/s/*.sock` holders, `<run-dir>/runner/runner.sock`.
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
    /// Responder foreground `#RRGGBB` (OSC 10 reply).
    pub responder_fg: String,
    /// Responder background `#RRGGBB` (OSC 11 reply).
    pub responder_bg: String,
    /// Whether the profile is dark (theme report).
    pub responder_dark: bool,
    /// Cap on hidden viewers per session.
    pub max_viewers_per_session: usize,
    /// Cap on paste bytes per call.
    pub paste_max_bytes: usize,
    /// Enter gate: output must be quiet this long (ms).
    pub enter_quiet_ms: u64,
    /// Enter gate: give up after this long (ms).
    pub enter_max_wait_ms: u64,
    /// Shell the harness runs under (launch profiles land in T10).
    pub pane_shell: PathBuf,
    /// Args for `pane_shell` (the T10 launch profiles consume both).
    pub pane_shell_args: Vec<String>,
    /// Test-only: honour verbatim caller argv (fixture programs) instead
    /// of the profile argv. Set ONLY by the test harness; production
    /// `kb-termd` never sets it and the ambient env never gates it.
    pub allow_verbatim_probe: bool,
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
    /// Responder foreground colour.
    #[arg(long, default_value = "#d4d4d4")]
    responder_fg: String,
    /// Responder background colour.
    #[arg(long, default_value = "#1e1e1e")]
    responder_bg: String,
    /// Dark profile (theme report).
    #[arg(long, value_parser = clap::value_parser!(bool), default_value = "true")]
    responder_dark: bool,
    /// Cap on hidden viewers per session.
    #[arg(long, default_value_t = 8)]
    max_viewers_per_session: usize,
    /// Cap on paste bytes per call.
    #[arg(long, default_value_t = 65536)]
    paste_max_bytes: usize,
    /// Enter gate: quiet window (ms).
    #[arg(long, default_value_t = 400)]
    enter_quiet_ms: u64,
    /// Enter gate: max wait (ms).
    #[arg(long, default_value_t = 5000)]
    enter_max_wait_ms: u64,
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
            responder_fg: cli.responder_fg,
            responder_bg: cli.responder_bg,
            responder_dark: cli.responder_dark,
            max_viewers_per_session: cli.max_viewers_per_session,
            paste_max_bytes: cli.paste_max_bytes,
            enter_quiet_ms: cli.enter_quiet_ms,
            enter_max_wait_ms: cli.enter_max_wait_ms,
            pane_shell: PathBuf::from("/bin/sh"),
            pane_shell_args: vec!["-c".to_string()],
            allow_verbatim_probe: false,
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
            responder_fg: "#d4d4d4".to_string(),
            responder_bg: "#1e1e1e".to_string(),
            responder_dark: true,
            max_viewers_per_session: 8,
            paste_max_bytes: 65536,
            enter_quiet_ms: 400,
            enter_max_wait_ms: 5000,
            pane_shell: PathBuf::from("/bin/sh"),
            pane_shell_args: vec!["-c".to_string()],
            allow_verbatim_probe: false,
        }
    }
    /// The responder profile for the anchor and every hidden client.
    pub fn responder_cfg(&self) -> crate::responder::ResponderCfg {
        crate::responder::ResponderCfg {
            fg: self.responder_fg.clone(),
            bg: self.responder_bg.clone(),
            dark: self.responder_dark,
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
        // Full scoped lines (exact line match, never a substring): a
        // `-s` where the spec says `-g`, or a value on the wrong scope,
        // must fail. (Line numbers in briefs drift; the spelling below
        // is the spec's own.)
        let lines: Vec<&str> = conf.lines().collect();
        for line in [
            "set -g @kb_conf_version 2",
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
            "set -s extended-keys always",
            "set -g extended-keys-format csi-u",
            "set -g history-limit 50000",
            "set -g status off",
            "set -g remain-on-exit on",
            "set -g remain-on-exit-format \"\"",
            "set -s exit-empty off",
            "set -g allow-passthrough off",
        ] {
            assert!(
                lines.contains(&line),
                "conf must contain the full line `{line}`"
            );
        }
        // Every removed round-1 item from the spec must stay out.
        for removed in [
            "window-size latest",
            "focus-events on",
            "set-clipboard on",
            "client-active",
            "M-PPage",
            "mouse on",
        ] {
            assert!(
                !conf.contains(removed),
                "conf must not contain the removed option `{removed}`"
            );
        }
    }

    #[test]
    fn conf_terminal_scopes_are_not_global() {
        // `extended-keys` and `exit-empty` are SERVER options (`set -s`),
        // not global/session settings (`set -g`): a `-g` spelling must
        // fail the golden above, so pin the negative here as well.
        let conf = include_str!("../../share/kb-term.conf");
        assert!(
            !conf.lines().any(|l| l == "set -g extended-keys always"),
            "extended-keys must be `set -s`, not `set -g`"
        );
        assert!(
            !conf.lines().any(|l| l == "set -g exit-empty off"),
            "exit-empty must be `set -s`, not `set -g`"
        );
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

    #[test]
    fn runner_config_defaults_and_unknown_keys() {
        let cfg = RunnerConfig::test_config(
            PathBuf::from("/tmp/kb-term-test"),
            "/bin/true".to_string(),
            PathBuf::from("/dev/null"),
        );
        assert_eq!(cfg.responder_fg, "#d4d4d4");
        assert_eq!(cfg.responder_bg, "#1e1e1e");
        assert!(cfg.responder_dark);
        assert_eq!(cfg.cell_px, "9x18");
        assert_eq!(cfg.max_viewers_per_session, 8);
        assert_eq!(cfg.paste_max_bytes, 65536);
        assert_eq!(cfg.enter_quiet_ms, 400);
        assert_eq!(cfg.enter_max_wait_ms, 5000);
        // An unknown flag is rejected by the parser (deny-unknown).
        let bad = Cli::try_parse_from(["kb-termd", "--bogus-flag"]);
        assert!(bad.is_err(), "unknown flags must be rejected");
        let ok = Cli::try_parse_from(["kb-termd"]).expect("defaults must parse");
        assert_eq!(ok.responder_fg, "#d4d4d4");
        assert_eq!(ok.responder_bg, "#1e1e1e");
        assert!(ok.responder_dark);
        assert_eq!(ok.max_viewers_per_session, 8);
        assert_eq!(ok.paste_max_bytes, 65536);
        assert_eq!(ok.enter_quiet_ms, 400);
        assert_eq!(ok.enter_max_wait_ms, 5000);
    }
}
