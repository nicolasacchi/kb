//! The attach's one-row status line.
//!
//! The mirror paints the session into the operator's terminal, so anything
//! else the attach says (the grid is owned by the web, press a key to take
//! it back) must never be written at the mirror's cursor: it would overwrite
//! mirrored rows and nothing would ever repaint them. Instead the notice
//! lives on the outer terminal's LAST row, painted as one self-contained
//! sequence that saves and restores the cursor, and is repainted after every
//! stream write that ends at a ground state, so a stream that scrolls or
//! overwrites that row never leaves the notice half-erased.
//!
//! [`GroundTracker`] is the "may I inject bytes now?" check: a paint that
//! landed inside a half-written escape sequence or UTF-8 character of the
//! stream would corrupt both.

use super::width::fit;

/// Where the byte stream the terminal has been given currently is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum G {
    Ground,
    /// After a lone ESC.
    Esc,
    /// ESC plus intermediate bytes (0x20-0x2F), waiting for the final.
    EscInter,
    Csi,
    /// Inside an OSC/DCS/SOS/PM/APC string.
    Str,
    /// Inside a string, just after an ESC (possible ST).
    StrEsc,
}

/// A small VT parser that only answers "does the stream written so far end
/// between sequences, on a whole UTF-8 character?".
#[derive(Debug, Clone)]
pub struct GroundTracker {
    g: G,
    /// UTF-8 continuation bytes still owed.
    cont: u8,
}

impl Default for GroundTracker {
    fn default() -> Self {
        Self {
            g: G::Ground,
            cont: 0,
        }
    }
}

impl GroundTracker {
    /// Feed bytes the terminal was just given.
    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.step(b);
        }
    }

    /// Whether the stream is between sequences and between characters.
    #[must_use]
    pub fn at_ground(&self) -> bool {
        self.g == G::Ground && self.cont == 0
    }

    fn step(&mut self, b: u8) {
        if self.cont > 0 && (0x80..0xC0).contains(&b) {
            self.cont -= 1;
            return;
        }
        // A non-continuation byte aborts a pending multi-byte character.
        self.cont = 0;
        match self.g {
            G::Ground => match b {
                0x1B => self.g = G::Esc,
                0xC0..=0xDF => self.cont = 1,
                0xE0..=0xEF => self.cont = 2,
                0xF0..=0xF7 => self.cont = 3,
                _ => {}
            },
            G::Esc => match b {
                b'[' => self.g = G::Csi,
                b']' | b'P' | b'X' | b'^' | b'_' => self.g = G::Str,
                0x1B => {}
                0x20..=0x2F => self.g = G::EscInter,
                _ => self.g = G::Ground,
            },
            G::EscInter => match b {
                0x1B => self.g = G::Esc,
                0x20..=0x2F => {}
                _ => self.g = G::Ground,
            },
            G::Csi => match b {
                0x1B => self.g = G::Esc,
                0x18 | 0x1A => self.g = G::Ground,
                0x40..=0x7E => self.g = G::Ground,
                _ => {}
            },
            G::Str => match b {
                0x07 | 0x18 | 0x1A => self.g = G::Ground,
                0x1B => self.g = G::StrEsc,
                _ => {}
            },
            G::StrEsc => match b {
                b'\\' => self.g = G::Ground,
                0x1B => {}
                // Any other ESC-introduced byte ends the string and is the
                // next sequence's first byte.
                b'[' => self.g = G::Csi,
                b']' | b'P' | b'X' | b'^' | b'_' => self.g = G::Str,
                _ => self.g = G::Ground,
            },
        }
    }
}

/// The bytes that paint `text` on the last row of a `cols` x `rows`
/// terminal without disturbing the mirror: save cursor (DECSC), origin mode
/// off, CUP to the last row, reverse video, the text fitted to the width,
/// erase to the end of the row, reset attributes, restore cursor (DECRC).
/// DECSC/DECRC also bracket the cursor's SGR and origin state, so the
/// stream's own state survives. No line feed is ever emitted.
#[must_use]
pub fn paint_bytes(text: &str, cols: u16, rows: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 40);
    out.extend_from_slice(b"\x1b7\x1b[?6l");
    out.extend_from_slice(format!("\x1b[{};1H", rows.max(1)).as_bytes());
    out.extend_from_slice(b"\x1b[0;7m");
    // Control bytes in a notice would be interpreted by the terminal.
    let clean: String = text.chars().filter(|c| !c.is_control()).collect();
    out.extend_from_slice(fit(&clean, cols).as_bytes());
    out.extend_from_slice(b"\x1b[K\x1b[0m\x1b8");
    out
}

/// The bytes that erase the notice row again (same save/restore bracket,
/// default attributes, no reverse video).
#[must_use]
pub fn clear_bytes(size: (u16, u16)) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b7\x1b[?6l");
    out.extend_from_slice(format!("\x1b[{};1H", size.1.max(1)).as_bytes());
    out.extend_from_slice(b"\x1b[0m\x1b[K\x1b8");
    out
}

/// The status line's state: the current text (if any) and the ground
/// tracker over everything written to the terminal.
#[derive(Debug, Default)]
pub struct StatusLine {
    text: Option<String>,
    ground: GroundTracker,
}

impl StatusLine {
    /// Replace the notice (`None` clears the state only; the join clear
    /// erases the row on screen).
    pub fn set(&mut self, text: Option<String>) {
        self.text = text;
    }

    /// The notice currently shown, if any.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// Account for stream bytes that were just written and return the
    /// repaint to append (empty when no notice is set or the stream stopped
    /// inside a sequence; the next write that ends at ground repaints).
    pub fn after_write(&mut self, written: &[u8], size: (u16, u16)) -> Vec<u8> {
        self.ground.feed(written);
        self.paint_now(size)
    }

    /// The paint for the current notice at `size`, when it is safe.
    #[must_use]
    pub fn paint_now(&self, size: (u16, u16)) -> Vec<u8> {
        match &self.text {
            Some(t) if self.ground.at_ground() => paint_bytes(t, size.0, size.1),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_notice_is_bracketed_by_decsc_decrc_and_never_emits_a_line_feed() {
        let b = paint_bytes("web panel owns the grid - watching", 80, 24);
        assert!(b.starts_with(b"\x1b7"), "DECSC first: {b:?}");
        assert!(b.ends_with(b"\x1b8"), "DECRC last: {b:?}");
        assert!(!b.contains(&b'\n'), "no LF of any kind: {b:?}");
        assert!(!b.contains(&b'\r'), "no CR either: {b:?}");
        let s = String::from_utf8(b).unwrap();
        assert!(s.contains("\x1b[24;1H"), "CUP to the last row: {s:?}");
        assert!(s.contains("\x1b[0;7m"), "reverse video: {s:?}");
        assert!(s.contains("\x1b[K"), "erase to the end of the row: {s:?}");
        // The ESC 7 / ESC 8 pair appears exactly once: a nested pair would
        // overwrite the single save slot.
        assert_eq!(s.matches("\x1b7").count(), 1);
        assert_eq!(s.matches("\x1b8").count(), 1);
    }

    #[test]
    fn a_paint_changes_only_the_last_row_and_leaves_the_cursor_where_it_was() {
        use super::super::testscreen::Screen;
        let mut scr = Screen::new(40, 6);
        scr.feed(b"line one\r\nline two\r\n$ typed");
        let before: Vec<String> = (0..5).map(|i| scr.row(i)).collect();
        let cursor = scr.cursor();
        scr.feed(&paint_bytes("watching: press a key", 40, 6));
        let after: Vec<String> = (0..5).map(|i| scr.row(i)).collect();
        assert_eq!(before, after, "the mirrored rows are untouched");
        assert_eq!(scr.row(5), "watching: press a key");
        assert_eq!(scr.cursor(), cursor, "the mirror's cursor is restored");
        scr.feed(&clear_bytes((40, 6)));
        assert_eq!(scr.row(5), "", "the notice row is erased again");
        assert_eq!(scr.cursor(), cursor);
    }

    #[test]
    fn the_notice_row_is_erased_inside_the_same_bracket() {
        let b = clear_bytes((80, 24));
        assert_eq!(b, b"\x1b7\x1b[?6l\x1b[24;1H\x1b[0m\x1b[K\x1b8".to_vec());
    }

    #[test]
    fn a_notice_is_fitted_to_the_outer_width_and_control_bytes_are_dropped() {
        let long = "x".repeat(200);
        let b = paint_bytes(&long, 40, 10);
        let s = String::from_utf8(b).unwrap();
        assert_eq!(
            s.matches('x').count(),
            39,
            "cols - 1 cells, never the last column"
        );
        let s = String::from_utf8(paint_bytes("a\x1b[2Jb\nc", 80, 10)).unwrap();
        assert!(
            !s.contains("\x1b[2J"),
            "an ESC inside a notice never reaches the tty: {s:?}"
        );
        assert!(s.contains("a[2Jbc"), "text kept, controls dropped: {s:?}");
    }

    #[test]
    fn the_ground_tracker_knows_when_a_write_stopped_inside_a_sequence() {
        let e = "\u{e9}".as_bytes();
        let euro = "\u{20ac}".as_bytes();
        let smile = "\u{1F600}".as_bytes();
        let cases: Vec<(Vec<u8>, bool)> = vec![
            (b"plain text\r\n".to_vec(), true),
            (b"\x1b[38;2;1;2;3m".to_vec(), true),
            (b"\x1b[38;2;1".to_vec(), false),
            (b"\x1b".to_vec(), false),
            (b"\x1b]0;title".to_vec(), false),
            (b"\x1b]0;title\x07".to_vec(), true),
            (b"\x1b]0;title\x1b\\".to_vec(), true),
            (b"\x1b]0;title\x1b".to_vec(), false),
            (b"\x1b_Gq=2;AAAA".to_vec(), false),
            (b"\x1b_Gq=2;AAAA\x1b\\".to_vec(), true),
            (b"\x1b(B".to_vec(), true),
            (b"\x1b(".to_vec(), false),
            (e.to_vec(), true),
            (e[..1].to_vec(), false),
            (euro[..2].to_vec(), false),
            (smile.to_vec(), true),
            (smile[..3].to_vec(), false),
        ];
        for (bytes, want) in &cases {
            let mut t = GroundTracker::default();
            t.feed(bytes);
            assert_eq!(t.at_ground(), *want, "{bytes:?}");
        }
        // A sequence split across two writes completes on the second.
        let mut t = GroundTracker::default();
        t.feed(b"abc\x1b[3");
        assert!(!t.at_ground());
        t.feed(b"8;5;9m");
        assert!(t.at_ground());
    }

    #[test]
    fn the_status_repaints_after_a_write_only_when_the_stream_is_at_ground() {
        let mut s = StatusLine::default();
        assert!(
            s.after_write(b"row\r\n", (80, 24)).is_empty(),
            "no notice, no paint"
        );
        s.set(Some("watching".to_string()));
        let p = s.after_write(b"more\r\n", (80, 24));
        assert!(p.starts_with(b"\x1b7") && p.ends_with(b"\x1b8"));
        // A write that stops mid-CSI must not be followed by a paint.
        assert!(s.after_write(b"x\x1b[38;2;1", (80, 24)).is_empty());
        let p = s.after_write(b"0;2m", (80, 24));
        assert!(!p.is_empty(), "the next write that ends at ground repaints");
        s.set(None);
        assert!(s.after_write(b"y", (80, 24)).is_empty());
    }
}
