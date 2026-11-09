//! Display-cell width helpers for label truncation: `cell_width` and
//! `fit` are ports of the spike `mirror_standin.py` (the width rules and
//! the cols-1 truncation), with the range tables in [`super::tables`].

use super::tables::{WIDE, ZERO};

fn in_ranges(cp: u32, table: &[(u32, u32)]) -> bool {
    // Binary search: both tables are sorted and disjoint.
    let mut lo = 0usize;
    let mut hi = table.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        let (a, b) = table[mid];
        if cp < a {
            hi = mid;
        } else if cp > b {
            lo = mid + 1;
        } else {
            return true;
        }
    }
    false
}

/// Display-cell width of one codepoint, following the spike `cell_width`:
/// combining marks are 0, East Asian W/F are 2, everything else
/// (including the kitty placeholder `U+10EEEE`) is 1. NUL is 0 (it never
/// appears in label text; this only keeps the arithmetic total).
/// Regional indicators (flags) are 2: tmux 3.7c renders them wide
/// though unicodedata marks them EA-neutral (pinned by the
/// table-driven test).
#[must_use]
pub fn cell_width(cp: u32) -> u32 {
    if cp == 0 {
        return 0;
    }
    // Regional indicators (flags) render 2 cells in tmux 3.7c even
    // though unicodedata marks them EA-neutral; the table-driven
    // tests pin this (they mirror the tmux 3.7c widths table).
    if (0x1F1E6..=0x1F1FF).contains(&cp) {
        return 2;
    }
    if in_ranges(cp, ZERO) {
        0
    } else if in_ranges(cp, WIDE) {
        2
    } else {
        1
    }
}

/// Display-cell width of a string (sum over its chars).
#[must_use]
pub fn str_width(s: &str) -> usize {
    s.chars().map(|c| cell_width(c as u32) as usize).sum()
}

/// Truncate `s` to at most `cols - 1` display cells (never touches the
/// last column, so no pending-wrap ambiguity), never splitting a
/// multi-byte char. A char wider than the remaining budget ends the
/// result (spike `fit` semantics: break, not pad).
#[must_use]
pub fn fit(s: &str, cols: u16) -> String {
    let limit = (cols.max(1) - 1) as usize;
    let mut out = String::new();
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = cell_width(ch as u32) as usize;
        if w + cw > limit {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_width_table() {
        // (code point, cells). Case-file style: tmux 3.7c width tables
        // (spike `widths-tmux-3.7c.json` shapes), inlined as literals.
        let cases: &[(u32, u32)] = &[
            (0x41, 1),     // 'A' ASCII narrow
            (0x20, 1),     // space
            (0x39, 1),     // '9'
            (0x301, 0),    // COMBINING ACUTE ACCENT
            (0x300, 0),    // COMBINING GRAVE ACCENT
            (0x34E, 0),    // combining grapheme joiner
            (0x1, 1),      // control (narrow per the port)
            (0x6F22, 2),   // CJK ideograph
            (0x3042, 2),   // hiragana
            (0x30A2, 2),   // katakana
            (0xD55C, 2),   // hangul syllable
            (0xFF61, 1),   // HALFWIDTH IDEOGRAPHIC FULL STOP (narrow)
            (0xFF21, 2),   // fullwidth A
            (0x1F600, 2),  // emoji (W)
            (0x10EEEE, 1), // the kitty placeholder (1 cell per xterm/tmux)
            (0x1F1FA, 2),  // regional indicator (W)
            (0x0, 0),      // NUL
            (0x930, 1),    // Devanagari letter (not a combining mark)
            (0x93C, 0),    // Devanagari sign nukta (combining)
        ];
        for (cp, want) in cases {
            assert_eq!(
                cell_width(*cp),
                *want,
                "cell_width(U+{cp:04X}) = {} want {want}",
                cell_width(*cp)
            );
        }
        // str_width sums codepoints.
        assert_eq!(str_width("hello"), 5);
        assert_eq!(str_width("あい"), 4);
        assert_eq!(str_width("á"), 1); // 'a' + combining acute
        assert_eq!(str_width(""), 0);
    }

    #[test]
    fn fit_truncates_to_cols_minus_one() {
        // cols-1 is the budget.
        assert_eq!(fit("hello world", 12), "hello world"); // 11 <= 11
        assert_eq!(fit("hello world", 5), "hell"); // budget 4
        assert_eq!(fit("日本語abc", 6), "日本"); // 2+2=4, next wide would exceed 5
        assert_eq!(fit("abc日本", 5), "abc"); // wide at the edge is dropped whole
        assert_eq!(fit("", 20), "");
        assert_eq!(fit("x", 1), ""); // cols=1 -> budget 0
                                     // Combining marks ride along with their base char (width 0).
        assert_eq!(fit("áb", 2), "á"); // budget 1: 'a' + combining
        assert_eq!(str_width(&fit("日本語abc", 6)), 4);
    }
}
