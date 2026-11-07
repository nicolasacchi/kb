//! The closed named-key table for the `keys` RPC.
//!
//! Byte entries cover the edits an operator needs (arrows, controls,
//! digits, `y`/`n`, `/`); anything else is `bad-key`. The hidden client is
//! a plain xterm-256color client to tmux, which re-encodes per the pane's
//! requested key mode (extended-keys / kitty).

/// Bytes for a named key, or `None` (the caller answers `bad-key`).
pub fn named_key_bytes(name: &str) -> Option<&'static [u8]> {
    Some(match name {
        "Esc" => b"\x1b",
        "Tab" => b"\x09",
        "S-Tab" => b"\x1b[Z",
        "Enter" => b"\x0d",
        "Up" => b"\x1b[A",
        "Down" => b"\x1b[B",
        "Right" => b"\x1b[C",
        "Left" => b"\x1b[D",
        "C-c" => b"\x03",
        "C-d" => b"\x04",
        "C-u" => b"\x15",
        "BSpace" => b"\x7f",
        "Slash" => b"\x2f",
        "1" => b"1",
        "2" => b"2",
        "3" => b"3",
        "4" => b"4",
        "5" => b"5",
        "6" => b"6",
        "7" => b"7",
        "8" => b"8",
        "9" => b"9",
        "y" => b"y",
        "n" => b"n",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_key_table_is_pinned() {
        let cases: &[(&str, &[u8])] = &[
            ("Esc", b"\x1b"),
            ("Tab", b"\x09"),
            ("S-Tab", b"\x1b[Z"),
            ("Enter", b"\x0d"),
            ("Up", b"\x1b[A"),
            ("Down", b"\x1b[B"),
            ("Right", b"\x1b[C"),
            ("Left", b"\x1b[D"),
            ("C-c", b"\x03"),
            ("C-d", b"\x04"),
            ("C-u", b"\x15"),
            ("BSpace", b"\x7f"),
            ("Slash", b"\x2f"),
            ("1", b"1"),
            ("2", b"2"),
            ("3", b"3"),
            ("4", b"4"),
            ("5", b"5"),
            ("6", b"6"),
            ("7", b"7"),
            ("8", b"8"),
            ("9", b"9"),
            ("y", b"y"),
            ("n", b"n"),
        ];
        assert_eq!(cases.len(), 24, "the closed table must stay closed");
        for (name, want) in cases {
            assert_eq!(
                named_key_bytes(name),
                Some(*want),
                "key {name} bytes pinned"
            );
        }
        for bad in ["F1", "C-x", "esc", "enter", "", "0", "a", "Pad1", "S-Enter"] {
            assert_eq!(named_key_bytes(bad), None, "key {bad} must be bad-key");
        }
    }
}
