//! Grid-owned fallback: when take-size is refused (`grid-owned`), the
//! mirror keeps going as a watch (output-only) and the first keypress
//! takes the size back, unless the web panel owns the grid: the web
//! outranks a local attach, so a keypress then takes nothing until the web
//! releases the grid. The attach says so on its status line (see
//! [`super::status`]), never inside the mirrored rows.
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

/// The status-line text while the attach watches because it does not own
/// the grid. `owner` is the current grid owner (`None` = nobody: the web
/// released it). The text says exactly what the next key does: under the
/// web it does nothing, otherwise it takes the grid back and is NOT sent to
/// the session.
#[must_use]
pub fn grid_status(owner: Option<&str>) -> String {
    match owner {
        Some(o) if is_web_owner(o) => {
            "web panel owns the grid \u{2014} watching; detach: Ctrl-] .".to_string()
        }
        Some(o) if is_local_mirror_owner(o) => {
            "another kb-term attach owns the grid \u{2014} watching; any key takes it back (that key is not sent); detach: Ctrl-] ."
                .to_string()
        }
        // An owner released between the refusal and the banner (or a refusal
        // message that named nobody) arrives as an empty string: a fixed
        // neutral label, never a blank name.
        Some(o) if o.trim().is_empty() => {
            "another client owns the grid \u{2014} watching; any key takes it back (that key is not sent); detach: Ctrl-] ."
                .to_string()
        }
        Some(_) => {
            "another device owns the grid \u{2014} watching; any key takes it back (that key is not sent); detach: Ctrl-] ."
                .to_string()
        }
        None => "press any key to take the grid back (that key is not sent); detach: Ctrl-] ."
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_status_names_the_owner_class_and_what_the_next_key_does() {
        let web = grid_status(Some("kb-term"));
        assert!(web.starts_with("web panel owns the grid"), "{web}");
        assert!(web.contains("watching"), "{web}");
        assert!(web.contains("Ctrl-] ."), "{web}");
        assert!(
            !web.contains("any key"),
            "a key under the web does nothing: {web}"
        );
        let free = grid_status(None);
        assert!(
            free.starts_with("press any key to take the grid back (that key is not sent)"),
            "{free}"
        );
        let local = grid_status(Some("local:1234"));
        assert!(
            local.starts_with("another kb-term attach owns the grid"),
            "{local}"
        );
        assert!(local.contains("that key is not sent"), "{local}");
        let foreign = grid_status(Some("relay:host-7"));
        assert!(
            foreign.starts_with("another device owns the grid"),
            "{foreign}"
        );
        assert!(foreign.contains("that key is not sent"), "{foreign}");
        for t in [&web, &free, &local, &foreign] {
            assert!(!t.chars().any(char::is_control), "one printable row: {t:?}");
        }
        // An empty / blank owner never renders a blank name: the neutral label.
        for blank in ["", "   "] {
            let t = grid_status(Some(blank));
            assert!(t.starts_with("another client owns the grid"), "{t:?}");
            assert!(t.contains("that key is not sent"), "{t:?}");
        }
        assert!(is_local_mirror_owner("local:1"));
        assert!(!is_local_mirror_owner("relay:1"));
        assert_eq!(local_owner(42), "local:42");
    }

    #[test]
    fn the_runner_refusal_message_parses_down_to_the_bare_owner() {
        assert_eq!(
            owner_from_message("grid is owned by kb-term (the web panel; release it there)"),
            "kb-term"
        );
        assert_eq!(owner_from_message("grid is owned by local:7"), "local:7");
    }
}
