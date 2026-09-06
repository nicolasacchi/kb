//! Position conversion between lip/1's wire shape (1-based line, 0-based
//! BYTE offset into that line's UTF-8 text) and LSP's own coordinate
//! system (0-based line, UTF-16 CODE-UNIT offset — mandated by the LSP
//! spec regardless of what encoding the server uses internally). Every
//! request converts byte->UTF-16 before calling the LSP; every response
//! converts UTF-16->byte before answering the caller ("convert both
//! directions correctly", design-lip.md Phase L1).
//!
//! Untrusted input (an HTTP caller's `col`, or a possibly-buggy LSP
//! server's `character`) is always clamped to a UTF-8/UTF-16 boundary
//! rather than trusted blindly — neither conversion function can panic.

/// Convert a lip/1 line number (1-based) to an LSP line number (0-based).
/// Saturates at 0 rather than underflowing on a caller-supplied `0`.
pub fn line_to_lsp(line_1based: u32) -> u32 {
    line_1based.saturating_sub(1)
}

/// Convert an LSP line number (0-based) back to lip/1's 1-based line.
pub fn line_from_lsp(line_0based: u32) -> u32 {
    line_0based.saturating_add(1)
}

/// Convert a lip/1 byte offset into `line` (that line's UTF-8 text, no
/// trailing newline) into an LSP UTF-16 code-unit column. A `byte_col`
/// that isn't on a char boundary (or is past the end of the line) is
/// clamped down to the nearest valid boundary — never panics, never reads
/// out of bounds.
pub fn byte_col_to_utf16(line: &str, byte_col: usize) -> u32 {
    let mut safe = byte_col.min(line.len());
    while safe > 0 && !line.is_char_boundary(safe) {
        safe -= 1;
    }
    line[..safe].encode_utf16().count() as u32
}

/// Inverse of [`byte_col_to_utf16`]: convert an LSP UTF-16 code-unit
/// column back into a byte offset into `line`. A `utf16_col` past the end
/// of the line clamps to `line.len()`.
pub fn utf16_col_to_byte(line: &str, utf16_col: u32) -> usize {
    let mut utf16_count = 0u32;
    for (byte_idx, ch) in line.char_indices() {
        if utf16_count >= utf16_col {
            return byte_idx;
        }
        utf16_count += ch.len_utf16() as u32;
    }
    line.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_conversion_round_trips() {
        assert_eq!(line_to_lsp(1), 0);
        assert_eq!(line_to_lsp(42), 41);
        assert_eq!(line_from_lsp(0), 1);
        assert_eq!(line_from_lsp(41), 42);
        // Malformed 0 (should be 1-based) saturates rather than
        // underflowing/panicking.
        assert_eq!(line_to_lsp(0), 0);
    }

    // "héllo 😀 wörld" mixes three byte/UTF-16 relationships in one line:
    // ASCII (1 byte = 1 UTF-16 unit), a 2-byte BMP char ('é'/'ö', still 1
    // UTF-16 unit each), and a 4-byte astral char ('😀', a UTF-16
    // SURROGATE PAIR = 2 units). A test that only used ASCII/BMP chars
    // could pass with an implementation that just divides byte offsets by
    // a fixed ratio; the surrogate pair is what actually exercises
    // "convert both directions correctly".
    const MULTIBYTE_LINE: &str = "héllo 😀 wörld";

    fn byte_offset_of(line: &str, needle: &str) -> usize {
        line.find(needle).expect("needle present in fixture line")
    }

    #[test]
    fn multibyte_line_byte_to_utf16_matches_hand_verified_offsets() {
        let line = MULTIBYTE_LINE;
        // 'h' — before any multibyte content: byte offset == utf16 offset.
        assert_eq!(byte_col_to_utf16(line, 0), 0);
        // Start of 'é' (byte 1) is 1 char (='h') in on both axes.
        assert_eq!(byte_col_to_utf16(line, byte_offset_of(line, "é")), 1);
        // Start of the emoji: "h"+"é"+"llo "  = 1+1+4 = 6 chars before it,
        // each of those chars is exactly 1 UTF-16 unit (all BMP).
        let emoji_byte = byte_offset_of(line, "😀");
        assert_eq!(byte_col_to_utf16(line, emoji_byte), 6);
        // Right after the emoji: the emoji itself is 1 char / 4 UTF-8
        // bytes / 2 UTF-16 units (surrogate pair) — this is the crux
        // assertion. A naive byte-length-based conversion would compute 7
        // (off by the surrogate pair's extra unit), not 8.
        let after_emoji_byte = emoji_byte + "😀".len();
        assert_eq!(byte_col_to_utf16(line, after_emoji_byte), 8);
        // End of line.
        assert_eq!(
            byte_col_to_utf16(line, line.len()),
            line.encode_utf16().count() as u32
        );
    }

    #[test]
    fn multibyte_line_round_trips_every_char_boundary() {
        let line = MULTIBYTE_LINE;
        for (byte_idx, _) in line.char_indices() {
            let utf16 = byte_col_to_utf16(line, byte_idx);
            let back = utf16_col_to_byte(line, utf16);
            assert_eq!(
                back, byte_idx,
                "round trip broke at byte {byte_idx} (utf16 {utf16})"
            );
        }
        // The end-of-line boundary too (not covered by char_indices).
        let end_utf16 = byte_col_to_utf16(line, line.len());
        assert_eq!(utf16_col_to_byte(line, end_utf16), line.len());
    }

    #[test]
    fn byte_col_to_utf16_never_panics_on_out_of_range_or_mid_char_input() {
        let line = MULTIBYTE_LINE;
        // Way past the end.
        assert_eq!(
            byte_col_to_utf16(line, line.len() + 1000),
            line.encode_utf16().count() as u32
        );
        // Mid-char (inside the 4-byte emoji): clamps down to the emoji's
        // start rather than panicking on a non-boundary slice.
        let emoji_byte = byte_offset_of(line, "😀");
        let mid_emoji = emoji_byte + 1;
        assert_eq!(byte_col_to_utf16(line, mid_emoji), 6);
    }

    #[test]
    fn utf16_col_to_byte_never_panics_on_out_of_range() {
        let line = MULTIBYTE_LINE;
        let far = line.encode_utf16().count() as u32 + 1000;
        assert_eq!(utf16_col_to_byte(line, far), line.len());
        assert_eq!(utf16_col_to_byte("", 0), 0);
        assert_eq!(utf16_col_to_byte("", 5), 0);
    }

    #[test]
    fn ascii_only_line_byte_and_utf16_columns_are_identical() {
        let line = "fn main() {}";
        for byte_col in 0..=line.len() {
            assert_eq!(byte_col_to_utf16(line, byte_col), byte_col as u32);
        }
    }
}
