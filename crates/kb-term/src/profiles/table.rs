//! The per-harness launch table: binary, fresh flags, profile env,
//! experimental marker, exit steps, and resume tails.
//!
//! The resume tails mirror `kb_core::sessions::resume::resume_command`
//! exactly (as argv vectors, no quoting); a drift test in kb-server pins
//! the two together. Global flags go BEFORE the resume tokens (clap
//! accepts global-before-subcommand; the reverse is unverified).

use super::{is_plain_token, ExitStep, PlanError};

/// Argv elements kb-term must never emit in a launch (no production
/// `--kb-off`, no bare-continue resume forms).
pub const FORBIDDEN_ARGV: &[&str] = &["--kb-off", "-c", "--continue", "--resume-last"];

/// One harness row of the launch table.
pub struct Row {
    /// Bare binary name (resolved by the pane PATH).
    pub bin: &'static str,
    /// Flags appended after the binary on a fresh launch.
    pub fresh_flags: &'static [&'static str],
    pub experimental: bool,
    pub exit_steps: &'static [ExitStep],
}

/// omp (18.16.0 spiked, 18.17.0 current): kitty placeholder protocol
/// forced (auto-detection under tmux yields no protocol); private
/// overlay pins image/recapture behaviour (T7). Exit keys verified by
/// T8 (`pane_dead=1` status 0).
static OMP_EXIT: &[ExitStep] = &[
    ExitStep::Keys(&["C-c"]),
    ExitStep::WaitMs(300),
    ExitStep::Keys(&["C-c"]),
    ExitStep::WaitMs(300),
    ExitStep::Keys(&["C-d"]),
];

/// claude (2.1.300, T3 flags): no alternate screen via env.
/// UNVERIFIED: cgroup kill is the backstop.
static CLAUDE_EXIT: &[ExitStep] = &[ExitStep::Text("/exit"), ExitStep::Keys(&["Enter"])];

/// codex (0.149.0, T3 flags). UNVERIFIED: cgroup kill is the backstop.
static CODEX_EXIT: &[ExitStep] = &[
    ExitStep::Keys(&["C-c"]),
    ExitStep::WaitMs(300),
    ExitStep::Keys(&["C-c"]),
];

/// kimi (T3 flags). UNVERIFIED: cgroup kill is the backstop.
static KIMI_EXIT: &[ExitStep] = &[
    ExitStep::Keys(&["C-d"]),
    ExitStep::WaitMs(300),
    ExitStep::Keys(&["C-d"]),
];

/// grok (1.0.46/1.0.77, T3 flags; spike reached only sign-in).
/// EXPERIMENTAL. UNVERIFIED: cgroup kill is the backstop.
static GROK_EXIT: &[ExitStep] = &[
    ExitStep::Keys(&["C-c"]),
    ExitStep::WaitMs(300),
    ExitStep::Keys(&["C-c"]),
];

/// opencode (1.17.20, T3 flags). EXPERIMENTAL.
/// UNVERIFIED: cgroup kill is the backstop.
static OPENCODE_EXIT: &[ExitStep] = &[
    ExitStep::Keys(&["C-c"]),
    ExitStep::WaitMs(300),
    ExitStep::Keys(&["C-c"]),
];

pub static OMP_ROW: Row = Row {
    bin: "omp",
    fresh_flags: &[],
    experimental: false,
    exit_steps: OMP_EXIT,
};
pub static CLAUDE_ROW: Row = Row {
    bin: "claude",
    fresh_flags: &[],
    experimental: false,
    exit_steps: CLAUDE_EXIT,
};
pub static CODEX_ROW: Row = Row {
    bin: "codex",
    fresh_flags: &["--no-alt-screen"],
    experimental: false,
    exit_steps: CODEX_EXIT,
};
pub static KIMI_ROW: Row = Row {
    bin: "kimi",
    fresh_flags: &[],
    experimental: false,
    exit_steps: KIMI_EXIT,
};
pub static GROK_ROW: Row = Row {
    bin: "grok",
    fresh_flags: &["--minimal", "--no-alt-screen"],
    experimental: true,
    exit_steps: GROK_EXIT,
};
pub static OPENCODE_ROW: Row = Row {
    bin: "opencode",
    fresh_flags: &["--mini"],
    experimental: true,
    exit_steps: OPENCODE_EXIT,
};

/// The table row for `harness`, or `None` for an unknown harness.
pub fn row(harness: &str) -> Option<&'static Row> {
    match harness {
        "omp" => Some(&OMP_ROW),
        "claude" => Some(&CLAUDE_ROW),
        "codex" => Some(&CODEX_ROW),
        "kimi" => Some(&KIMI_ROW),
        "grok" => Some(&GROK_ROW),
        "opencode" => Some(&OPENCODE_ROW),
        _ => None,
    }
}

/// The exact argv kb-core's resume table prints, as a vector (no
/// quoting). grok appends `--cwd <cwd>` only when `cwd` is absolute with
/// no control characters, mirroring `grok_cwd_arg`'s gating.
pub fn resume_tail(harness: &str, sid: &str, cwd: Option<&str>) -> Result<Vec<String>, PlanError> {
    let r = row(harness).ok_or(PlanError::UnknownHarness)?;
    if !is_plain_token(sid) {
        return Err(PlanError::UnsafeToken("resume_sid"));
    }
    let bin = r.bin;
    let mut argv = vec![bin.to_string()];
    match harness {
        "claude" => {
            argv.push("-r".to_string());
        }
        "codex" => {
            argv.push("resume".to_string());
        }
        "grok" => {
            argv.push("-r".to_string());
            argv.push(sid.to_string());
            if let Some(c) = cwd.filter(|c| c.starts_with('/') && !c.chars().any(char::is_control))
            {
                argv.push("--cwd".to_string());
                argv.push(c.to_string());
            }
            return Ok(argv);
        }
        "kimi" => {
            argv.push("-S".to_string());
        }
        "opencode" => {
            argv.push("--session".to_string());
        }
        "omp" => {
            argv.push("-r".to_string());
        }
        _ => unreachable!("row() gates the closed harness set"),
    }
    argv.push(sid.to_string());
    Ok(argv)
}
