//! Grid-owned fallback: when take-size is refused (`grid-owned`), the
//! mirror keeps going as a watch (output-only) and the first keypress
//! takes the size back, unless the web panel owns the grid: the web
//! outranks a local attach, so a keypress then prints a short rate-limited
//! notice and takes nothing until the web releases the grid.
//!
//! There is no `exec tmux` here (an exec'd tmux would own stdin and
//! could not implement "any key takes the size back"); kb-term.conf's
//! key tables are empty, so the watch path stays inside the mirror.

/// The take-size owner string for local mirrors (`local:<pid>`). The
/// `local:` prefix is how the fallback banner distinguishes a local
/// mirror owner from a foreign device.
pub const LOCAL_OWNER_PREFIX: &str = "local:";

/// The owner (and viewer id) used by a local attach process.
#[must_use]
pub fn local_owner(pid: u32) -> String {
    format!("{LOCAL_OWNER_PREFIX}{pid}")
}

/// Whether `owner` names another local mirror (a `local:<pid>` owner).
#[must_use]
pub fn is_local_mirror_owner(owner: &str) -> bool {
    owner.starts_with(LOCAL_OWNER_PREFIX)
}

/// Whether `owner` is the web panel's relay owner.
#[must_use]
pub fn is_web_owner(owner: &str) -> bool {
    crate::runner::grid::is_web_owner(owner)
}

/// The owner named in a `grid-owned` runner message
/// (`grid is owned by <owner>[ (...)]`): the first token after the prefix.
#[must_use]
pub fn owner_from_message(message: &str) -> String {
    message
        .strip_prefix("grid is owned by ")
        .unwrap_or(message)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string()
}

/// The banner printed when take-size is refused or the grid is lost. The
/// owner decides whether the web panel, another local attach or another
/// device holds the grid.
#[must_use]
pub fn grid_banner(owner: &str) -> String {
    if is_web_owner(owner) {
        return "grid owned by the web panel; watching (release it there, or detach with Ctrl-] .) — scrollback may be incomplete: the pane size differs from the session grid".to_string();
    }
    let who = if is_local_mirror_owner(owner) {
        "another kb-term attach"
    } else {
        "another device"
    };
    format!(
        "grid owned by {who}; watching (any key takes the size back) — scrollback may be incomplete: the pane size differs from the session grid"
    )
}

/// The one-line notice a keypress prints while the web owns the grid.
pub const WEB_OWNER_NOTICE: &str =
    "the web panel owns the grid; release it there, or detach with Ctrl-] .";

/// Minimum gap between two [`WEB_OWNER_NOTICE`] lines.
pub const NOTICE_GAP: std::time::Duration = std::time::Duration::from_secs(10);

/// Rate limiter for the web-owner notice (at most one per [`NOTICE_GAP`]).
#[derive(Debug, Default)]
pub struct NoticeGate {
    last: Option<std::time::Instant>,
}

impl NoticeGate {
    /// Whether a notice may print at `now` (records it when it may).
    pub fn allow(&mut self, now: std::time::Instant) -> bool {
        match self.last {
            Some(prev) if now.saturating_duration_since(prev) < NOTICE_GAP => false,
            _ => {
                self.last = Some(now);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_banner_distinguishes_local_and_foreign() {
        let foreign = grid_banner("relay:host-7");
        assert!(
            foreign.starts_with(
                "grid owned by another device; watching (any key takes the size back)"
            ),
            "foreign banner: {foreign}"
        );
        assert!(
            foreign.contains("scrollback may be incomplete"),
            "incomplete scrollback noted: {foreign}"
        );
        let local = grid_banner("local:1234");
        assert!(
            local.starts_with(
                "grid owned by another kb-term attach; watching (any key takes the size back)"
            ),
            "local banner: {local}"
        );
        assert!(
            local.contains("scrollback may be incomplete"),
            "incomplete scrollback noted: {local}"
        );
        // A bare local owner also counts as local.
        assert!(is_local_mirror_owner("local:1"));
        assert!(!is_local_mirror_owner("relay:1"));
        assert_eq!(local_owner(42), "local:42");
    }

    #[test]
    fn web_owner_banner_notice_and_rate_limit() {
        let web = grid_banner("kb-term");
        assert!(
            web.starts_with("grid owned by the web panel; watching"),
            "{web}"
        );
        assert!(web.contains("release it there"), "{web}");
        assert!(web.contains("scrollback may be incomplete"), "{web}");
        assert!(!web.contains("any key takes the size back"), "{web}");
        // The runner's refusal message parses down to the bare owner.
        assert_eq!(
            owner_from_message("grid is owned by kb-term (the web panel; release it there)"),
            "kb-term"
        );
        assert_eq!(owner_from_message("grid is owned by local:7"), "local:7");
        // One notice per 10 s.
        let t0 = std::time::Instant::now();
        let mut gate = NoticeGate::default();
        assert!(gate.allow(t0));
        assert!(!gate.allow(t0 + std::time::Duration::from_secs(9)));
        assert!(gate.allow(t0 + std::time::Duration::from_secs(10)));
        assert!(!gate.allow(t0 + std::time::Duration::from_secs(11)));
    }
}
