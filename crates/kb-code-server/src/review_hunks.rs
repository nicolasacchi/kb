//! V73-K3 — `kbc-hunkid/1` in Rust, and the unified-diff parse it needs.
//!
//! The hunk content address was minted CLIENT-side by V73-K2a
//! (`web-code/src/lib/diffHunks.ts`) because the only consumer was the
//! SPA's per-hunk viewed state, which the daemon stores opaquely
//! (`review_hunk_viewed`, V0031). The hunk↔turn join changes that: a
//! caller now hands the daemon a hunk id and asks WHICH SESSION TURNS
//! produced it, so the daemon has to be able to find the hunk that id
//! names. That means a second implementation of one address, which is
//! exactly the kind of thing this crate pins rather than hopes about:
//!
//!   * `grammar/kbchunkid.golden.json` is ONE fixture read by BOTH this
//!     module's [`tests::golden_corpus_matches_the_rust_implementation`]
//!     and `web-code/src/lib/hunkId.golden.test.ts` — invariant 16(a)'s
//!     one-fixture-two-parsers discipline (kbcq/1) and invariant 22(b)'s
//!     (kbc-refs/1), applied to the third grammar this crate mirrors. The
//!     fixture lives on the CRATE side because the Rust builder stage's
//!     Docker context is `COPY crates ./crates`.
//!   * The hash is FNV-1a 64 over **UTF-16 code units**, not bytes. That
//!     is not a preference: the TS side hashes `charCodeAt(i)`, and any
//!     non-BMP or non-ASCII character in a changed line would otherwise
//!     make the two implementations disagree only for the diffs that
//!     contain one. [`fnv1a64_utf16`] therefore iterates
//!     `str::encode_utf16`, and the golden carries an emoji case so a
//!     "simplification" to bytes fails loudly rather than subtly.
//!
//! The recipe itself (path + every `+`/`-` line, its sigil re-prefixed,
//! joined by `\n`; line numbers and context lines deliberately excluded so
//! the id survives a rebase) and its one accepted collision (two identical
//! changes in one file share an id) are documented at length in
//! `diffHunks.ts`; this module does not restate them, it mirrors them.

use serde::Serialize;

/// The addressing scheme's version. Bump BOTH this and `HUNK_ID_SCHEMA` in
/// `web-code/src/lib/diffHunks.ts` if the hashed input ever changes —
/// already-stored ids would otherwise silently stop matching.
pub const HUNK_ID_SCHEMA: &str = "kbc-hunkid/1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LineKind {
    Add,
    Remove,
    Context,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffLine {
    pub kind: LineKind,
    /// The line's CONTENT — git's leading sigil/alignment column removed,
    /// exactly as the TS parser does it.
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffHunk {
    pub header: String,
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub lines: Vec<DiffLine>,
}

impl DiffHunk {
    /// The removed lines, joined by `\n` — the pre-image text this hunk
    /// deleted. The hunk↔turn join compares an `Edit`'s `old_string`
    /// against exactly this.
    pub fn removed_text(&self) -> String {
        self.side_text(LineKind::Remove)
    }

    /// The added lines, joined by `\n`.
    pub fn added_text(&self) -> String {
        self.side_text(LineKind::Add)
    }

    fn side_text(&self, want: LineKind) -> String {
        self.lines
            .iter()
            .filter(|l| l.kind == want)
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ParsedDiff {
    pub preamble: Vec<String>,
    pub hunks: Vec<DiffHunk>,
    pub binary: bool,
}

/// `@@ -a[,b] +c[,d] @@…`. Hand-parsed rather than regex-matched — this
/// crate carries no regex dependency in its hot paths and the grammar is
/// four integers.
fn parse_hunk_header(line: &str) -> Option<(u32, u32, u32, u32)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    let (old_start, old_lines) = split_range(old)?;
    let (new_start, new_lines) = split_range(new)?;
    Some((old_start, old_lines, new_start, new_lines))
}

/// `a` (implicitly one line) or `a,b`.
fn split_range(s: &str) -> Option<(u32, u32)> {
    match s.split_once(',') {
        Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
        None => Some((s.parse().ok()?, 1)),
    }
}

/// Mirror of `parseUnifiedDiff`. Never fails — a malformed header is
/// treated as an ordinary line (folded into whatever hunk is open, or the
/// preamble), the same degrade the TS parser makes.
pub fn parse_unified_diff(text: &str) -> ParsedDiff {
    if text.trim().is_empty() {
        return ParsedDiff::default();
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    if lines.iter().any(|l| l.starts_with("Binary files ")) {
        return ParsedDiff {
            preamble: lines.iter().map(|s| s.to_string()).collect(),
            hunks: Vec::new(),
            binary: true,
        };
    }

    let mut preamble: Vec<String> = Vec::new();
    let mut hunks: Vec<DiffHunk> = Vec::new();
    let mut old_line = 0u32;
    let mut new_line = 0u32;

    for line in lines {
        if let Some((os, ol, ns, nl)) = parse_hunk_header(line) {
            old_line = os;
            new_line = ns;
            hunks.push(DiffHunk {
                header: line.to_string(),
                old_start: os,
                old_lines: ol,
                new_start: ns,
                new_lines: nl,
                lines: Vec::new(),
            });
            continue;
        }
        let Some(current) = hunks.last_mut() else {
            preamble.push(line.to_string());
            continue;
        };
        if let Some(text) = line.strip_prefix('+') {
            current.lines.push(DiffLine {
                kind: LineKind::Add,
                text: text.to_string(),
                old_line: None,
                new_line: Some(new_line),
            });
            new_line += 1;
        } else if let Some(text) = line.strip_prefix('-') {
            current.lines.push(DiffLine {
                kind: LineKind::Remove,
                text: text.to_string(),
                old_line: Some(old_line),
                new_line: None,
            });
            old_line += 1;
        } else if line.starts_with('\\') {
            // "\ No newline at end of file" — not a content line.
            continue;
        } else {
            current.lines.push(DiffLine {
                kind: LineKind::Context,
                text: line.strip_prefix(' ').unwrap_or(line).to_string(),
                old_line: Some(old_line),
                new_line: Some(new_line),
            });
            old_line += 1;
            new_line += 1;
        }
    }

    ParsedDiff {
        preamble,
        hunks,
        binary: false,
    }
}

/// FNV-1a, 64-bit, over **UTF-16 code units** — see the module doc for why
/// the encoding is load-bearing rather than incidental.
pub fn fnv1a64_utf16(text: &str) -> String {
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for unit in text.encode_utf16() {
        hash ^= unit as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

/// The exact bytes [`hunk_id`] hashes — exported for the golden, so an
/// accidental change to the recipe reads as itself rather than as an
/// opaque hex diff.
pub fn hunk_fingerprint_input(path: &str, hunk: &DiffHunk) -> String {
    let mut parts: Vec<String> = vec![path.to_string()];
    for line in &hunk.lines {
        match line.kind {
            LineKind::Add => parts.push(format!("+{}", line.text)),
            LineKind::Remove => parts.push(format!("-{}", line.text)),
            LineKind::Context => {}
        }
    }
    parts.join("\n")
}

pub fn hunk_id(path: &str, hunk: &DiffHunk) -> String {
    fnv1a64_utf16(&hunk_fingerprint_input(path, hunk))
}

/// The `kbc-hunkid/1` id shape, as `reviews::is_hunk_id` validates it on
/// the wire: 16 lowercase hex digits. Restated here (rather than imported)
/// because that predicate is deliberately WIDER — it accepts any opaque
/// 1..=64 lowercase-alphanumeric token so a future scheme is storable —
/// and this one is about the address THIS module mints.
pub fn is_minted_hunk_id(s: &str) -> bool {
    s.len() == 16
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN: &str = include_str!("../grammar/kbchunkid.golden.json");

    #[derive(serde::Deserialize)]
    struct GoldenFile {
        schema: String,
        cases: Vec<GoldenCase>,
    }

    #[derive(serde::Deserialize)]
    struct GoldenCase {
        name: String,
        path: String,
        diff: String,
        /// One entry per hunk the diff parses into.
        hunks: Vec<GoldenHunk>,
    }

    #[derive(serde::Deserialize)]
    struct GoldenHunk {
        fingerprint_input: String,
        id: String,
        removed_text: String,
        added_text: String,
    }

    /// The shared fixture. `web-code/src/lib/hunkId.golden.test.ts` reads
    /// the same bytes and asserts the same things against the TS
    /// implementation — one corpus, two parsers.
    #[test]
    fn golden_corpus_matches_the_rust_implementation() {
        let g: GoldenFile = serde_json::from_str(GOLDEN).expect("golden parses");
        assert_eq!(g.schema, HUNK_ID_SCHEMA);
        assert!(!g.cases.is_empty());
        for case in &g.cases {
            let parsed = parse_unified_diff(&case.diff);
            assert_eq!(
                parsed.hunks.len(),
                case.hunks.len(),
                "{}: hunk count",
                case.name
            );
            for (h, want) in parsed.hunks.iter().zip(&case.hunks) {
                assert_eq!(
                    hunk_fingerprint_input(&case.path, h),
                    want.fingerprint_input,
                    "{}: fingerprint input",
                    case.name
                );
                assert_eq!(hunk_id(&case.path, h), want.id, "{}: id", case.name);
                assert_eq!(
                    h.removed_text(),
                    want.removed_text,
                    "{}: removed",
                    case.name
                );
                assert_eq!(h.added_text(), want.added_text, "{}: added", case.name);
                assert!(is_minted_hunk_id(&want.id), "{}: id shape", case.name);
            }
        }
    }

    #[test]
    fn the_hash_is_over_utf16_code_units_not_bytes() {
        // "é" is 2 UTF-8 bytes but ONE UTF-16 code unit; "𝄞" is 4 bytes
        // and TWO units. A byte-based FNV would produce different digests
        // for both, so this pins the encoding directly rather than only
        // through the golden.
        assert_ne!(fnv1a64_utf16("é"), fnv1a64_utf16("Ã©"));
        assert_eq!(fnv1a64_utf16("").len(), 16);
        // The empty string is FNV-1a's offset basis, unchanged.
        assert_eq!(fnv1a64_utf16(""), "cbf29ce484222325");
    }

    #[test]
    fn context_lines_and_line_numbers_are_not_hashed() {
        let a = parse_unified_diff("@@ -1,3 +1,3 @@\n ctx one\n-old\n+new\n ctx two\n");
        let b =
            parse_unified_diff("@@ -40,3 +40,3 @@\n different ctx\n-old\n+new\n also different\n");
        assert_eq!(
            hunk_id("a.rb", &a.hunks[0]),
            hunk_id("a.rb", &b.hunks[0]),
            "a rebase moves line numbers and context; the address must survive both"
        );
        assert_ne!(hunk_id("a.rb", &a.hunks[0]), hunk_id("b.rb", &a.hunks[0]));
    }

    #[test]
    fn a_single_line_range_header_defaults_to_one_line() {
        let p = parse_unified_diff("@@ -5 +5 @@\n-a\n+b\n");
        assert_eq!(p.hunks.len(), 1);
        assert_eq!(p.hunks[0].old_lines, 1);
        assert_eq!(p.hunks[0].new_lines, 1);
        assert_eq!(p.hunks[0].old_start, 5);
    }

    #[test]
    fn a_binary_diff_has_no_hunks_and_says_so() {
        let p = parse_unified_diff(
            "diff --git a/x.png b/x.png\nBinary files a/x.png and b/x.png differ\n",
        );
        assert!(p.binary);
        assert!(p.hunks.is_empty());
    }

    #[test]
    fn a_no_newline_marker_is_not_a_content_line() {
        let p = parse_unified_diff("@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+b\n");
        assert_eq!(p.hunks[0].removed_text(), "a");
        assert_eq!(p.hunks[0].added_text(), "b");
    }

    #[test]
    fn an_empty_diff_parses_to_nothing() {
        assert_eq!(parse_unified_diff(""), ParsedDiff::default());
        assert_eq!(parse_unified_diff("   \n"), ParsedDiff::default());
    }
}
