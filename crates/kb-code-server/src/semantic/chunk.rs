//! W2.3 — cAST-style code chunker: split a file's parse tree at TOP-LEVEL
//! definition granularity (reusing the EXISTING `extract::extract_symbols`
//! tree-sitter machinery — no new grammar/query plumbing), merging small
//! adjacent siblings up to an approximate token floor so a chunk is a
//! sensible embedding unit (a lone one-line `const` doesn't get its own
//! vector; a 2000-line `impl` block does).
//!
//! # Algorithm
//!
//! 1. Run `extract::extract_symbols` and keep only the TOP-LEVEL ones
//!    (`container.is_none()`) — a method inside a class/impl is part of
//!    its enclosing top-level definition's span, not a chunk boundary of
//!    its own. For YAML this is the CST-walk outline's top-level keys
//!    (`extract_symbols` dispatches there — see that fn's doc); every
//!    other language goes through `tags.scm`. KNOWN APPROXIMATION: a
//!    MULTI-document YAML file (`yaml::outline`'s doc: `---`-separated
//!    `document`s) stamps every top-level key's container as `doc[N]`, not
//!    `None` — so `container.is_none()` finds ZERO top-level symbols for
//!    such a file, and the whole file falls back to ONE undifferentiated
//!    gap chunk (step 2) rather than splitting at document/key boundaries.
//!    Single-document YAML (the common case) is unaffected. Acceptable v1
//!    scope; a future pass could special-case `doc[N]`-prefixed containers
//!    as chunk boundaries too.
//! 2. Fill the GAPS between/around those definitions (imports, top-level
//!    statements, blank runs) as their own un-named segments, so nothing
//!    in the file is silently dropped from the embeddable surface — a
//!    blank-only gap is skipped (nothing worth embedding).
//! 3. Walk the ordered segment list, absorbing consecutive segments into
//!    the CURRENT chunk while it is still under [`MERGE_FLOOR_TOKENS`]
//!    (an approximate `chars / 4` token count — no real tokenizer
//!    dependency, documented estimate only). A segment that is already at
//!    or over the floor on its own (e.g. one huge function) is never
//!    further split in this Wave — it becomes its own single, possibly
//!    large, chunk.
//! 4. Each emitted chunk is prefixed with a header line
//!    `"{path} | {names} | {first non-blank line}"` — `names` is the
//!    comma-joined top-level definition name(s) folded into the chunk, or
//!    `"module"` for a pure-gap chunk (no definition inside it).
//!
//! Chunk ids are assigned by the caller (`"{blob_hash}#{idx}"`, `idx` =
//! this module's own `Chunk::idx`, dense from 0) — this module only knows
//! about ONE file's bytes, never a blob hash.

use crate::extract::{self, Symbol};
use crate::lang;

/// Asymmetric query-time prefix for `jina-embeddings-v2-base-code`
/// (`semantic::search` prepends this to the QUERY text only — indexed
/// chunk text, above, is NEVER prefixed).
///
/// Model-card research (2026-07, HF `jinaai/jina-embeddings-v2-base-code`
/// usage examples + the Jina blog post introducing the newer
/// `jina-code-embeddings` family): this specific v2 model's own examples
/// embed queries UN-prefixed (`model.encode(["query text", "code
/// text"])`, no instruction string) — an explicit "Represent this query
/// for searching relevant code:"-style prefix is NOT part of its
/// documented convention (that pattern belongs to BGE-style instructed
/// retrieval models, and separately to Jina's NEWER `jina-code-embeddings-
/// {0.5b,1.5b}` models, which use their own different, longer instruction
/// strings). We deliberately add a light prefix anyway rather than sending
/// the bare query:
///   1. it costs nothing extra (one short prepended sentence, and jina-v2's
///      8192-token window swallows it without denting the useful budget);
///   2. asymmetric retrieval (code passages read naturally as code, while a
///      query reads naturally as a question/description) generally
///      benefits from marking which side of the pair is "the question" —
///      even models without a REQUIRED prefix often see a small
///      consistency win from one, and the downside of a no-op prefix is
///      negligible;
///   3. it keeps this call site future-proof: if the registered model is
///      ever swapped for one of Jina's newer, prefix-REQUIRING code models,
///      only this constant (and the registry entry) needs to change, not
///      the call sites that use it.
///
/// If a future model swap changes this trade-off, update this constant and
/// its doc together — don't silently drop the prefix.
pub const QUERY_PREFIX: &str = "Represent this query for searching relevant code: ";

/// Approximate token floor a chunk's BODY (excluding the header line) is
/// merged up to before starting a new chunk — see the module doc's step 3.
/// `256` per the design brief's "merging small siblings up to ~256-512
/// tokens": absorption stops as soon as the floor is reached, so an
/// individual chunk typically lands in the ~256-to-(256 + one more
/// sibling) range — NOT a hard ceiling at 512; a merge that starts just
/// under the floor can overshoot it by up to one sibling's size. That's an
/// accepted approximation (documented, not a bug) — capping it exactly
/// would require either splitting a sibling mid-node (against this Wave's
/// "definition granularity" contract) or a lookahead the merge loop
/// deliberately doesn't do.
const MERGE_FLOOR_TOKENS: usize = 256;

/// `chars / CHARS_PER_TOKEN` — the documented, dependency-free token
/// approximation (no real tokenizer; good enough to decide "keep merging
/// small siblings" vs. "flush", not used for anything embedding-correctness
/// sensitive).
const CHARS_PER_TOKEN: usize = 4;

/// Header line's `first_line` field is truncated to this many chars so one
/// extremely long single-line definition (e.g. a generated minified line)
/// doesn't blow out the header.
const HEADER_FIRST_LINE_MAX_CHARS: usize = 80;

#[derive(Debug, thiserror::Error)]
pub enum ChunkError {
    #[error(transparent)]
    Lang(#[from] lang::LangError),
    #[error("chunk source is not valid utf8")]
    NotUtf8,
}

pub type Result<T> = std::result::Result<T, ChunkError>;

/// One embeddable chunk of a single file. `line_start`/`line_end` are
/// 1-based inclusive (matches `extract::Symbol`'s convention). `text` is
/// the full embed-ready payload: the header line, a newline, then the
/// chunk's source body verbatim (NO query prefix — see `super::
/// QUERY_PREFIX`'s doc: the prefix is asymmetric, applied to the QUERY at
/// search time only, never to indexed chunk text).
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub idx: u32,
    pub line_start: u32,
    pub line_end: u32,
    pub text: String,
}

/// One ordered segment of the file: either a top-level definition
/// (`name: Some(..)`) or a gap between/around definitions (`name: None`).
struct Segment {
    line_start: u32,
    line_end: u32,
    name: Option<String>,
}

/// Chunk `source` (already known to be UTF-8 and to have a grammar for
/// `lang_id` — see the module doc). `path` is embedded verbatim into every
/// chunk's header line (display-only, not re-parsed). Returns an empty
/// `Vec` for blank/whitespace-only content.
pub fn chunk_file(lang_id: &str, path: &str, source: &[u8]) -> Result<Vec<Chunk>> {
    let text = std::str::from_utf8(source).map_err(|_| ChunkError::NotUtf8)?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let lines: Vec<&str> = text.lines().collect();
    let total_lines = lines.len() as u32;

    let symbols = extract::extract_symbols(lang_id, source)?;
    let mut top_level: Vec<&Symbol> = symbols.iter().filter(|s| s.container.is_none()).collect();
    top_level.sort_by_key(|s| s.line_start);

    let segments = build_segments(&lines, total_lines, &top_level);
    if segments.is_empty() {
        return Ok(Vec::new());
    }
    Ok(merge_segments(&lines, path, &segments))
}

/// Step 1+2 of the module doc: interleave gaps around the sorted top-level
/// definitions. A definition's own span (per `extract::Symbol`) always
/// wins a gap that would otherwise overlap it — `cursor` only ever
/// advances past an already-emitted definition's `line_end`.
fn build_segments(lines: &[&str], total_lines: u32, top_level: &[&Symbol]) -> Vec<Segment> {
    let mut segments = Vec::with_capacity(top_level.len() * 2 + 1);
    let mut cursor = 1u32;
    for sym in top_level {
        if sym.line_start > cursor {
            let gap_end = sym.line_start - 1;
            if !is_blank_range(lines, cursor, gap_end) {
                segments.push(Segment {
                    line_start: cursor,
                    line_end: gap_end,
                    name: None,
                });
            }
        }
        // A definition's span can start before `cursor` if two "top-level"
        // symbols overlap (shouldn't happen for a well-formed grammar, but
        // extract.rs's own dedup is per-node, not per-symbol-list) — clamp
        // so span_start never regresses, keeping `merge_segments`' line
        // slicing panic-free.
        let seg_start = sym.line_start.max(cursor);
        let seg_end = sym.line_end.max(seg_start);
        segments.push(Segment {
            line_start: seg_start,
            line_end: seg_end.min(total_lines),
            name: Some(sym.name.clone()),
        });
        cursor = seg_end + 1;
    }
    if cursor <= total_lines && !is_blank_range(lines, cursor, total_lines) {
        segments.push(Segment {
            line_start: cursor,
            line_end: total_lines,
            name: None,
        });
    }
    segments
}

/// Step 3+4: absorb consecutive segments into a chunk while under the
/// merge floor, then emit the header + body text.
fn merge_segments(lines: &[&str], path: &str, segments: &[Segment]) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut idx = 0u32;
    let mut i = 0usize;
    while i < segments.len() {
        let mut group_end = segments[i].line_end;
        let group_start = segments[i].line_start;
        let mut names: Vec<String> = segments[i].name.iter().cloned().collect();
        let mut body_chars = line_range_char_len(lines, group_start, group_end);
        i += 1;
        while i < segments.len() && body_chars / CHARS_PER_TOKEN < MERGE_FLOOR_TOKENS {
            let next = &segments[i];
            group_end = next.line_end;
            if let Some(n) = &next.name {
                names.push(n.clone());
            }
            body_chars += line_range_char_len(lines, next.line_start, next.line_end);
            i += 1;
        }
        let body = lines[(group_start - 1) as usize..group_end as usize].join("\n");
        let container = if names.is_empty() {
            "module".to_string()
        } else {
            names.join(", ")
        };
        let first_line = body
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim();
        let first_line = truncate_chars(first_line, HEADER_FIRST_LINE_MAX_CHARS);
        let header = format!("{path} | {container} | {first_line}");
        chunks.push(Chunk {
            idx,
            line_start: group_start,
            line_end: group_end,
            text: format!("{header}\n{body}"),
        });
        idx += 1;
    }
    chunks
}

/// Char count (not byte count — an approximation is fine here, see
/// `CHARS_PER_TOKEN`'s doc) of lines `[start, end]` (1-based, inclusive,
/// clamped to `lines`' bounds), including one newline per line boundary so
/// the estimate roughly matches the joined body's real length.
fn line_range_char_len(lines: &[&str], start: u32, end: u32) -> usize {
    let start = (start.saturating_sub(1) as usize).min(lines.len());
    let end = (end as usize).min(lines.len());
    if start >= end {
        return 0;
    }
    lines[start..end]
        .iter()
        .map(|l| l.chars().count() + 1)
        .sum()
}

fn is_blank_range(lines: &[&str], start: u32, end: u32) -> bool {
    let start = (start.saturating_sub(1) as usize).min(lines.len());
    let end = (end as usize).min(lines.len());
    if start >= end {
        return true;
    }
    lines[start..end].iter().all(|l| l.trim().is_empty())
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUST_FIXTURE: &str = r#"use std::fmt;

struct Point {
    x: i32,
    y: i32,
}

impl Point {
    fn origin() -> Point {
        Point { x: 0, y: 0 }
    }

    fn dist(&self, other: &Point) -> f64 {
        0.0
    }
}

fn top_level(a: i32) -> i32 {
    a
}
"#;

    #[test]
    fn rust_chunk_boundaries_land_on_top_level_defs() {
        let chunks = chunk_file("rust", "src/lib.rs", RUST_FIXTURE.as_bytes()).unwrap();
        // Every fixture segment here is small (well under the 256-token
        // merge floor), so the whole file merges into ONE chunk — proves
        // the merge loop absorbs siblings rather than emitting a
        // chunk-per-definition when the content is tiny.
        assert_eq!(chunks.len(), 1, "got: {chunks:#?}");
        let c = &chunks[0];
        assert_eq!(c.idx, 0);
        assert_eq!(c.line_start, 1);
        assert_eq!(c.line_end, RUST_FIXTURE.lines().count() as u32);
        let header = c.text.lines().next().unwrap();
        assert!(header.starts_with("src/lib.rs | "), "header: {header:?}");
        // The header names EVERY top-level def folded into this one chunk.
        assert!(header.contains("Point"), "header: {header:?}");
        assert!(header.contains("top_level"), "header: {header:?}");
    }

    #[test]
    fn header_line_is_first_body_line_of_the_chunk() {
        let chunks = chunk_file("rust", "src/lib.rs", RUST_FIXTURE.as_bytes()).unwrap();
        let header = chunks[0].text.lines().next().unwrap();
        // First non-blank body line is `use std::fmt;` (the file's own
        // first line) since this fixture merges into one module-spanning
        // chunk starting at line 1.
        assert!(header.ends_with("use std::fmt;"), "header: {header:?}");
    }

    #[test]
    fn a_single_definition_at_or_over_the_floor_is_not_split() {
        // One giant top-level function, well over MERGE_FLOOR_TOKENS on its
        // own — must stay a single chunk (v1 punt: no intra-definition
        // splitting), not get chopped mid-body.
        let mut src = String::from("fn big() {\n");
        for i in 0..500 {
            src.push_str(&format!("    let v{i} = {i};\n"));
        }
        src.push_str("}\n");
        let chunks = chunk_file("rust", "src/big.rs", src.as_bytes()).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].line_start, 1);
        assert_eq!(chunks[0].line_end, src.lines().count() as u32);
    }

    #[test]
    fn many_small_top_level_defs_merge_until_the_floor_then_flush() {
        // 40 tiny top-level fns (~20 chars each) — small enough that they
        // keep absorbing past a single-chunk boundary, proving multiple
        // chunks come out once the accumulated body crosses the floor
        // repeatedly, and that ordering (idx ascending, lines ascending)
        // is preserved across the flush points.
        let mut src = String::new();
        for i in 0..40 {
            src.push_str(&format!("fn f{i}() {{ let x = {i}; x + 1; }}\n"));
        }
        let chunks = chunk_file("rust", "src/many.rs", src.as_bytes()).unwrap();
        assert!(chunks.len() >= 2, "expected multiple flushes: {chunks:#?}");
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(c.idx, i as u32);
        }
        // Lines are contiguous and strictly increasing across chunks (no
        // gap, no overlap, no line dropped).
        let mut prev_end = 0u32;
        for c in &chunks {
            assert_eq!(c.line_start, prev_end + 1, "chunks: {chunks:#?}");
            assert!(c.line_end >= c.line_start);
            prev_end = c.line_end;
        }
        assert_eq!(prev_end, src.lines().count() as u32);
    }

    #[test]
    fn gap_before_first_definition_is_its_own_segment_when_non_blank() {
        let src = "// a leading module comment\nconst A: i32 = 1;\n\nfn f() {}\n";
        let chunks = chunk_file("rust", "src/g.rs", src.as_bytes()).unwrap();
        // Small file merges to one chunk; the leading comment line must
        // still be INSIDE that chunk's body (not dropped).
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].text.contains("a leading module comment"));
    }

    #[test]
    fn blank_only_content_yields_no_chunks() {
        let chunks = chunk_file("rust", "src/empty.rs", b"\n\n   \n").unwrap();
        assert!(chunks.is_empty());
    }

    #[test]
    fn empty_bytes_yield_no_chunks() {
        let chunks = chunk_file("rust", "src/empty.rs", b"").unwrap();
        assert!(chunks.is_empty());
    }

    #[test]
    fn non_utf8_bytes_error() {
        let err = chunk_file("rust", "src/bin.rs", &[0xff, 0xfe, 0x00]).unwrap_err();
        assert!(matches!(err, ChunkError::NotUtf8));
    }

    #[test]
    fn unsupported_language_errors() {
        let err = chunk_file("cobol", "x.cbl", b"IDENTIFICATION DIVISION.\n").unwrap_err();
        assert!(matches!(err, ChunkError::Lang(_)));
    }

    // --- TypeScript fixture (a second language, per the scope's "goldens
    // on a rust + ts fixture") ------------------------------------------

    const TS_FIXTURE: &str = r#"import { readFileSync } from "fs";

interface Shape {
  area(): number;
}

class Circle implements Shape {
  radius: number;
  constructor(radius: number) {
    this.radius = radius;
  }
  area(): number {
    return 3.14 * this.radius * this.radius;
  }
}

export function topLevel(a: number): number {
  return a;
}
"#;

    #[test]
    fn typescript_chunk_boundaries_land_on_top_level_defs() {
        let chunks = chunk_file("typescript", "src/shapes.ts", TS_FIXTURE.as_bytes()).unwrap();
        assert_eq!(chunks.len(), 1, "got: {chunks:#?}");
        let header = chunks[0].text.lines().next().unwrap();
        assert!(header.starts_with("src/shapes.ts | "), "header: {header:?}");
        assert!(header.contains("Shape"), "header: {header:?}");
        assert!(header.contains("Circle"), "header: {header:?}");
        assert!(header.contains("topLevel"), "header: {header:?}");
        // Methods (`area`, `constructor`) are NOT top-level — they must not
        // appear as their own segment names in a merged header (they ride
        // inside the `Circle` class's own span instead).
        assert_eq!(chunks[0].line_start, 1);
        assert_eq!(chunks[0].line_end, TS_FIXTURE.lines().count() as u32);
    }

    #[test]
    fn typescript_many_small_defs_split_into_multiple_chunks() {
        let mut src = String::new();
        for i in 0..40 {
            src.push_str(&format!(
                "export function f{i}(x: number): number {{ return x + {i}; }}\n"
            ));
        }
        let chunks = chunk_file("typescript", "src/many.ts", src.as_bytes()).unwrap();
        assert!(chunks.len() >= 2, "expected multiple flushes: {chunks:#?}");
        let mut prev_end = 0u32;
        for c in &chunks {
            assert_eq!(c.line_start, prev_end + 1);
            prev_end = c.line_end;
        }
        assert_eq!(prev_end, src.lines().count() as u32);
    }
}
