//! Grid-owned fallback: when take-size is refused (`grid-owned`), the
//! mirror keeps going as a watch (output-only) and the first keypress
//! takes the size back.
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

/// The banner printed when take-size is refused. The owner decides
/// whether another local attach holds the grid (the default policy: a
/// second non-`--watch` local attach hits this) or another device.
#[must_use]
pub fn grid_banner(owner: &str) -> String {
    let who = if is_local_mirror_owner(owner) {
        "another kb-term attach"
    } else {
        "another device"
    };
    format!(
        "grid owned by {who}; watching (any key takes the size back) — scrollback may be incomplete: the pane size differs from the session grid"
    )
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
}
