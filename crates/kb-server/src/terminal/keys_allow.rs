//! The phone drive vocabulary the relay accepts (T18): the closed set of
//! key names for `POST /api/terms/{id}/keys`, and the viewer-id charset
//! the drive routes validate before they reach the runner.
//!
//! The runner owns the bytes: `kb-term`'s closed key table
//! (`runner/keys.rs`, `named_key_bytes`) maps each name below to fixed
//! bytes and answers `bad-key` for anything else. This list is the
//! relay's own copy of the NAMES so an unknown key is refused here, with
//! its own error type (`urn:kb:errors:bad-key`), before a runner round
//! trip. kb-server has no `kb-term` edge, so the two lists are kept in
//! lock-step by a test that reads the runner's source (see the tests
//! below) and by the SPA's `phone/keys.ts` test that reads this file.

/// `urn:kb:errors:bad-key` — a key name outside [`NAMED_KEYS`].
pub(crate) const ERR_TERMINAL_BAD_KEY: &str = "urn:kb:errors:bad-key";

/// The most key names one `keys` request may carry.
pub(crate) const MAX_KEYS_PER_REQUEST: usize = 32;

/// Every key name the relay forwards, in the runner table's order.
pub(crate) const NAMED_KEYS: [&str; 24] = [
    "Esc", "Tab", "S-Tab", "Enter", "Up", "Down", "Right", "Left", "C-c", "C-d", "C-u", "BSpace",
    "Slash", "1", "2", "3", "4", "5", "6", "7", "8", "9", "y", "n",
];

/// Whether `name` is a key the relay forwards.
pub(crate) fn is_named_key(name: &str) -> bool {
    NAMED_KEYS.contains(&name)
}

/// The tap's viewer-id grammar, `[a-z0-9:_-]{1,64}`: the id the WebSocket
/// `hello` frame hands a panel and the drive routes pass on so the tap can
/// tell which device typed. Validated here so a client string never
/// reaches the runner unchecked; never persisted.
pub(crate) fn is_valid_viewer_id(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b':' | b'_' | b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewer_id_grammar_matches_the_tap() {
        for ok in ["ws-abcd123456", "a", "phone:1", "a_b-c:1", &"a".repeat(64)] {
            assert!(is_valid_viewer_id(ok), "{ok:?} must pass");
        }
        for bad in [
            "",
            "Web",
            "a b",
            "a/b",
            "../x",
            "wörk",
            &"a".repeat(65),
            "a\0b",
        ] {
            assert!(!is_valid_viewer_id(bad), "{bad:?} must be refused");
        }
    }

    /// The names the runner's table maps, read from its source: every
    /// `"Name" => b"..."` arm of `named_key_bytes`.
    fn runner_table_names() -> Vec<String> {
        let src = include_str!("../../../kb-term/src/runner/keys.rs");
        let body = src
            .split("pub fn named_key_bytes")
            .nth(1)
            .expect("named_key_bytes");
        let body = body.split("#[cfg(test)]").next().unwrap();
        body.lines()
            .filter_map(|l| {
                let l = l.trim();
                let rest = l.strip_prefix('"')?;
                let (name, tail) = rest.split_once('"')?;
                tail.trim_start()
                    .starts_with("=> b\"")
                    .then(|| name.to_string())
            })
            .collect()
    }

    /// Twin of `named_key_table_is_pinned` in kb-term's `runner/keys.rs`
    /// and of `keys.test.ts` on the SPA side: the relay accepts exactly
    /// the names the runner maps, no more and no fewer.
    #[test]
    fn relay_key_names_equal_the_runner_table() {
        let table = runner_table_names();
        assert_eq!(table.len(), NAMED_KEYS.len(), "runner table: {table:?}");
        for (i, name) in table.iter().enumerate() {
            assert_eq!(NAMED_KEYS[i], name, "position {i}");
        }
    }

    #[test]
    fn unknown_and_lookalike_keys_are_refused() {
        for bad in [
            "Escape", "BTab", "/", "F1", "C-x", "esc", "enter", "", "0", "a",
        ] {
            assert!(!is_named_key(bad), "{bad:?} must be refused");
        }
        for ok in NAMED_KEYS {
            assert!(is_named_key(ok));
        }
    }
}
