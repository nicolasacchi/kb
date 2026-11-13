//! Pane-environment allowlist, forbidden-name lists and the
//! server-env defence (unset prelude + prefix snippet).
//!
//! The pane env is built from scratch: only [`ALLOWED`] names are copied
//! out of the runner's env, plus per-harness profile env and the
//! kb-term identity pair. Outer-terminal identity variables (kitty,
//! ghostty, wezterm, ...) must never reach the harness: harnesses derive
//! terminal identity from env first. `TMUX`, `TMUX_PANE` and `TERM` are
//! tmux-owned (tmux sets them itself) and are in NEITHER forbidden list:
//! never put them in the plan env, never unset them.
//!
//! The pane shell is the non-login `/bin/sh -c`: no user profile is ever
//! sourced for a pane. The unset prelude + prefix snippet therefore
//! defend server-env inheritance — a tmux server inherits the env of
//! the first client that starts it, and `new-session -e` only adds — not
//! a login-profile reintroduction the launcher cannot have.

/// Exact env names copied from the runner env into the pane env.
pub const ALLOWED: &[&str] = &[
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "PATH",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "XDG_RUNTIME_DIR",
    "KB_DAEMON_URL",
    "KB_CACHE_DIR",
    "KB_BIN_DIR",
    "KB_SESSIONS_DIR",
    "KB_CAPTURE_SPOOL",
    "KB_CAPTURE_LOCKS",
];

/// Outer-terminal identity names that must never reach the harness and
/// must be unset by the wrapper before exec (server-env inheritance).
pub const FORBIDDEN_EXACT: &[&str] = &[
    "KITTY_WINDOW_ID",
    "KITTY_PID",
    "KITTY_INSTALLATION_DIR",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "WEZTERM_PANE",
    "WEZTERM_EXECUTABLE",
    "STY",
];

/// Forbidden name prefixes (outer-terminal identity by family).
pub const FORBIDDEN_PREFIX: &[&str] = &[
    "GHOSTTY_", "WEZTERM_", "ZELLIJ", "ITERM_", "KONSOLE_", "VTE_", "WT_",
];

/// tmux-owned names: in neither forbidden list, never in the plan env,
/// never unset (tmux sets all three in every pane itself).
pub const TMUX_OWNED: &[&str] = &["TERM", "TMUX", "TMUX_PANE"];

/// Identity/ownership names an `extra_passthrough` entry may never carry:
/// they are set by kb-term itself (or by tmux), never forwarded.
pub const NEVER_FORWARD: &[&str] = &[
    "TERM",
    "TMUX",
    "TMUX_PANE",
    "KB_SESSION_ID",
    "KB_HARNESS",
    "KB_TERM_ID",
    "KB_JOB_ULID",
];

/// Per-harness profile-env names owned by `profiles::plan` (verified
/// knobs in `profiles/table.rs`). Callers may never name them via
/// `extra_passthrough` or caller env: the profile value is authoritative.
pub const PROFILE_OWNED: &[&str] = &[
    "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN",
    "PI_IMAGE_PROTOCOL",
    "PI_FORCE_IMAGE_PROTOCOL",
    "PI_CONFIG_FILES",
];

/// Whether `name` is a profile-owned env name.
pub fn is_profile_owned(name: &str) -> bool {
    PROFILE_OWNED.contains(&name)
}

/// Whether `name` is on the copy allowlist (exact match, never prefix:
/// `LC_TERMINAL` shares the `LC_` shape but is NOT allowed).
pub fn is_allowed_name(name: &str) -> bool {
    ALLOWED.contains(&name)
}

/// Whether `name` is on a forbidden list (exact or prefix).
pub fn is_forbidden_name(name: &str) -> bool {
    FORBIDDEN_EXACT.contains(&name) || FORBIDDEN_PREFIX.iter().any(|p| name.starts_with(p))
}

/// Whether `name` is a legal POSIX env identifier.
pub fn is_valid_env_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether an `extra_passthrough` name is acceptable: `^[A-Z][A-Z0-9_]{0,63}$`,
/// on neither forbidden list, not a never-forward identity name, and not
/// a profile-owned name (the profile value is authoritative).
pub fn validate_passthrough(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => {}
        _ => return false,
    }
    if !chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') {
        return false;
    }
    if name.len() > 64 {
        return false;
    }
    !is_forbidden_name(name) && !NEVER_FORWARD.contains(&name) && !is_profile_owned(name)
}

/// The allowlist-filtered copy of `base_env`, sorted by name.
pub fn filter_env(base_env: &[(String, String)]) -> Vec<(String, String)> {
    use std::collections::BTreeMap;
    let mut map: BTreeMap<&str, &str> = BTreeMap::new();
    for (k, v) in base_env {
        if is_allowed_name(k) {
            map.insert(k.as_str(), v.as_str());
        }
    }
    map.into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// The last value for `name` in `base_env`, if any.
pub fn lookup(base_env: &[(String, String)], name: &str) -> Option<String> {
    base_env
        .iter()
        .rev()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.clone())
}

/// Server-env defence: every `FORBIDDEN_EXACT` name plus every concrete
/// forbidden-prefixed name present in `base_env`, sorted (golden-stable).
/// Never names a tmux-owned variable. The wrapper unsets these before
/// exec so a tmux server that inherited forbidden names from its first
/// client cannot pass them to the pane.
pub fn unset_after_login(base_env: &[(String, String)]) -> Vec<String> {
    let mut names: Vec<String> = FORBIDDEN_EXACT.iter().map(|s| s.to_string()).collect();
    for (k, _) in base_env {
        if FORBIDDEN_PREFIX.iter().any(|p| k.starts_with(p)) && is_valid_env_identifier(k) {
            names.push(k.clone());
        }
    }
    names.sort();
    names.dedup();
    names
}

/// `unset A B C;` for the wrapper script (empty for no names).
/// Errors on a non-identifier name rather than emitting invalid shell.
pub fn shell_unset_prelude(names: &[String]) -> Result<String, String> {
    for name in names {
        if !is_valid_env_identifier(name) {
            return Err(name.clone());
        }
    }
    if names.is_empty() {
        return Ok(String::new());
    }
    Ok(format!("unset {}; ", names.join(" ")))
}

/// POSIX loop unsetting every currently-set variable whose name starts
/// with a `FORBIDDEN_PREFIX` (covers server-inherited names that were
/// never in `base_env`).
pub fn shell_unset_prefixes_snippet() -> String {
    format!(
        "for v in $(env | sed -n 's/^\\([A-Za-z_][A-Za-z0-9_]*\\)=.*/\\1/p' | grep -E '^({})'); do unset \"$v\"; done; ",
        FORBIDDEN_PREFIX.join("|")
    )
}
