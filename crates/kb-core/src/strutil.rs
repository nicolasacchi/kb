//! Shared UTF-8 string helpers.
//!
//! Byte-index truncation must snap to a char boundary — slicing mid-codepoint
//! panics. The stdlib's `str::floor_char_boundary` is still nightly-only, so
//! the floor walk lives here once and is reused by share/CLI error snippets
//! and short-hash display (and anywhere else that would otherwise hand-roll
//! `&s[..max]`).

/// Largest byte index `<= idx` that lands on a UTF-8 char boundary of `s`
/// (safe to slice `&s[..that]`).
pub fn floor_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut i = idx;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Truncate `s` to at most `max` **bytes**, snapping down to a char boundary,
/// and append `…` when anything was cut. Under-budget inputs are returned
/// unchanged (no ellipsis).
pub fn truncate_bytes_ellipsis(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let end = floor_char_boundary(s, max);
    format!("{}…", &s[..end])
}

/// Minimal percent-decoder for SPA-permalink path segments (the SPA emits them
/// via `encodeURIComponent`). Decodes `%XX` byte escapes; a malformed escape is
/// kept verbatim. Lossy UTF-8 — corpus paths are valid UTF-8 in practice. Used
/// by both `share::classify_cross_link` (export link rewriting) and
/// `parser::parse_artifact_link` (indexed cross-artifact edges), which must
/// decode path-form permalinks with identical semantics before hashing them
/// to artifact ids.
pub(crate) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (
                (b[i + 1] as char).to_digit(16),
                (b[i + 2] as char).to_digit(16),
            ) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_char_boundary_snaps_mid_multibyte() {
        // "à" is two bytes (C3 A0). Index 1 is mid-char → floors to 0.
        let s = "àb";
        assert_eq!(floor_char_boundary(s, 0), 0);
        assert_eq!(floor_char_boundary(s, 1), 0);
        assert_eq!(floor_char_boundary(s, 2), 2);
        assert_eq!(floor_char_boundary(s, 3), 3);
        assert_eq!(floor_char_boundary(s, 99), s.len());
    }

    #[test]
    fn truncate_bytes_ellipsis_at_multibyte_boundary() {
        // "日本語" is 9 bytes (3×3). max=4 lands mid second char → keep first.
        let s = "日本語";
        let out = truncate_bytes_ellipsis(s, 4);
        assert_eq!(out, "日…");
        assert!(out.is_char_boundary(out.len()));
        // max inside first char → empty head + ellipsis.
        let out = truncate_bytes_ellipsis(s, 1);
        assert_eq!(out, "…");
        // Under budget: unchanged, no ellipsis.
        assert_eq!(truncate_bytes_ellipsis(s, 9), s);
        assert_eq!(truncate_bytes_ellipsis(s, 100), s);
    }

    #[test]
    fn truncate_bytes_ellipsis_ascii_short_and_long() {
        assert_eq!(truncate_bytes_ellipsis("hello", 12), "hello");
        assert_eq!(
            truncate_bytes_ellipsis("abcdefghijklm", 12),
            "abcdefghijkl…"
        );
    }
}
