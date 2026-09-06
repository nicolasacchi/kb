//! `haml/1` — a first-party, indentation-aware HAML scanner (V72-H3, design
//! D7).
//!
//! # Why this exists at all
//!
//! Every other file type this daemon understands is parsed by a
//! tree-sitter grammar registered in [`crate::syntax`]. HAML has no viable
//! one: the best available grammar is a 13-star repository, nvim-treesitter
//! carries no `haml` entry, and a Rails monolith's views are roughly half
//! HAML. D7's ruling is therefore to OWN the scanner rather than depend on
//! a grammar the instrument cannot stand on — which is why `syntax/1` grew
//! a second engine ([`crate::syntax::Engine::Scanner`]) rather than a
//! fourteenth grammar row.
//!
//! # The divergence corpus, and the rule about the gem
//!
//! Correctness is pinned against the REAL `haml` gem's own
//! `Haml::Parser`, offline: `crates/kb-code-server/tests/fixtures/haml/`
//! holds ~40 synthetic templates and, beside each, the gem's parse
//! projected into [`projection`]'s shape. **The gem is never invoked by
//! CI and never by this daemon** — the expectations are generated once, by
//! hand, on a developer box (see `tests/fixtures/haml/CORPUS.md` for the
//! gem version and the regeneration command) and checked in. `ci-code`
//! runs a pure-Rust diff against those files; there is no Ruby anywhere in
//! the build, and invariant 10 (the daemon never spawns a non-git process)
//! is untouched.
//!
//! # The three surfaces
//!
//! * [`parser::parse_str`] → a [`parser::Document`]: the indentation tree,
//!   every node carrying exact byte ranges. Its own module doc holds the
//!   three rules that are not obvious (mid-block keywords, byte-compared
//!   indentation, source-resolved continuations).
//! * [`extract`] → highlight spans, the template outline, and the Ruby
//!   FRAGMENT STREAM plus a synthesized one-parse Ruby program with a line
//!   map. The fragment stream is what makes the EXISTING Rails-lens
//!   extractors work on `.haml` unchanged.
//! * [`projection`] → the corpus's comparison shape.
//!
//! # Honesty posture
//!
//! Nothing here mints `exact`. The Rails edges the fragments feed are
//! `likely`/`candidate` exactly as the `.erb` path's are (the lens has no
//! `Exact` variant), the outline rows carry no signature/doc/occurrence
//! backing, and a malformed file produces DIAGNOSTICS plus whatever
//! structure was recoverable — never a panic, and never a silently
//! reinterpreted tree. Diagnostics live on the returned value and are
//! persisted nowhere: this crate has no diagnostics table and V72-H3 does
//! not add one.

pub mod extract;
pub mod lexer;
pub mod parser;
pub mod projection;

pub use lexer::Span;
pub use parser::{Document, Node, NodeKind};

/// The scanner's own version string — the `syntax/1` registry's
/// `Engine::Scanner` payload and the cache-salt component in
/// [`crate::lang::HAML`]. Bump BOTH together when a change to this module
/// would produce different derived rows for unchanged bytes, exactly as a
/// grammar version bump does for a tree-sitter row.
pub const SCANNER_VERSION: &str = "haml/1";

/// What a scan could not do, and where. A HAML file that HAML itself would
/// reject still produces a tree here — the diagnostic is the caption on
/// that tree, not a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticKind {
    /// A tab in the indentation. HAML requires spaces; the byte count is
    /// still used, so the tree is the one the author probably meant.
    TabIndent,
    /// A dedent landed between two open levels, closing nothing.
    InconsistentDedent,
    /// An attribute group's bracket never balanced.
    UnclosedAttributes,
    /// A `#{` with no matching `}`.
    UnclosedInterpolation,
    /// A `|` continuation ran past [`parser::MAX_CONTINUATION_LINES`].
    UnterminatedMultiline,
    /// The source was not valid UTF-8; everything from the first bad byte
    /// on was not scanned.
    InvalidUtf8,
    /// Nesting hit [`parser::MAX_DEPTH`].
    DepthLimit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Diagnostic {
    pub kind: DiagnosticKind,
    /// 1-based.
    pub line: u32,
    pub span: Span,
}

/// Scan HAML bytes. Total: every input produces a [`Document`], and
/// invalid UTF-8 is a diagnostic over the valid prefix rather than an
/// error the caller has to handle (an ingest walk that aborted on one
/// malformed template would take the whole repo's derivation with it).
pub fn scan(source: &[u8]) -> Document {
    match std::str::from_utf8(source) {
        Ok(src) => parser::parse_str(src),
        Err(e) => {
            let upto = e.valid_up_to();
            // `valid_up_to` is by definition a valid boundary — no unsafe
            // needed, and the fallback keeps this total.
            let src = std::str::from_utf8(&source[..upto]).unwrap_or("");
            let mut doc = parser::parse_str(src);
            doc.diagnostics.push(Diagnostic {
                kind: DiagnosticKind::InvalidUtf8,
                line: extract::line_at(src, upto),
                span: Span::new(upto, source.len()),
            });
            doc
        }
    }
}

/// The valid-UTF-8 prefix of `source` — the one place the bytes→str step
/// happens, so every entry point below agrees about what was scanned.
fn valid_prefix(source: &[u8]) -> &str {
    match std::str::from_utf8(source) {
        Ok(s) => s,
        Err(e) => std::str::from_utf8(&source[..e.valid_up_to()]).unwrap_or(""),
    }
}

/// The template outline — the ONE call `extract::extract_symbols` makes
/// for a `.haml` file. Total: a malformed template yields whatever
/// structure was recoverable, never an error the ingest walk would
/// propagate out of the whole repo pass.
pub fn outline(source: &[u8]) -> Vec<crate::extract::Symbol> {
    let src = valid_prefix(source);
    extract::outline(&parser::parse_str(src), src)
}

/// Highlight spans — the ONE call `highlight::extract_highlights` makes
/// for a `.haml` file. Includes the Ruby fragments, painted by the
/// EXISTING Ruby highlighter (see [`extract::highlight_spans`]).
pub fn highlights(source: &[u8]) -> Vec<crate::highlight::Span> {
    let src = valid_prefix(source);
    extract::highlight_spans(&parser::parse_str(src), src)
}

/// Every `#{…}`'s inner-Ruby span within `span`, for a caller that has a
/// source range but no parser context (a filter body, which the parser
/// deliberately leaves opaque until someone asks).
pub fn interpolations_in(src: &str, span: Span) -> Vec<Span> {
    let bytes = src.as_bytes();
    let start = (span.start as usize).min(bytes.len());
    let end = (span.end as usize).min(bytes.len());
    let mut out = Vec::new();
    let mut i = start;
    while i + 1 < end {
        if bytes[i] == b'#' && bytes[i + 1] == b'{' && !(i > start && bytes[i - 1] == b'\\') {
            match parser::scan_balanced(src, i + 1, b'{', b'}', end) {
                Some(close) => {
                    out.push(Span::new(i + 2, close - 1));
                    i = close;
                    continue;
                }
                None => break,
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests;
