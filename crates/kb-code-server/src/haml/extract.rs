//! `haml/1` — what the rest of the daemon consumes: highlight spans, the
//! template outline, and the RUBY FRAGMENT STREAM that lets the existing,
//! unchanged Rails-lens extractors run over a `.haml` file.
//!
//! # The fragment stream is the whole point
//!
//! kb-code already knows how to find `render`/`t()`/`FooComponent.new` in
//! Ruby: `frameworks::rails::*` does it on a `tree-sitter-ruby` tree.
//! What it did not have for HAML was a way to GET that Ruby. This module
//! produces two things from one walk:
//!
//! * [`ruby_fragments`] — every Ruby span in the template (script lines,
//!   `#{…}` interpolations, `{…}` attribute hashes, `[…]` object
//!   references, a `:ruby` filter body), each with its EXACT byte range in
//!   the HAML source. That is the piece H2a's injection-aware pipeline
//!   lifts; it is deliberately independent of everything below it.
//! * [`ruby_program`] — those fragments concatenated into ONE parseable
//!   Ruby program, with synthetic `end`s derived from the indentation tree
//!   (`- if x` / body / `- else` / body → `if x` / body / `else` / body /
//!   `end`) and a LINE MAP back to HAML line numbers.
//!
//! The program is what `frameworks::rails::support::walk_haml_ruby_
//! fragments` parses, so a `.haml` view goes through the same
//! `scan_calls`/`resolve_t_call`/`walk_calls` code the `.erb` path uses,
//! byte for byte. Only the line numbers are re-anchored afterwards,
//! through [`RubyProgram::haml_line`] — nothing about the resolution,
//! the edge kinds or the trust classes is re-implemented for HAML.
//!
//! # What this module does NOT claim
//!
//! HAML mints no `exact` anything. The Rails edges its fragments feed are
//! `likely`/`candidate` like every other convention edge (the lens has no
//! `Exact` variant at all), and the outline rows carry no signature, no
//! doc and no occurrence backing — a template's Ruby fragments are not
//! scope-proven, so nothing here may pretend they are.

use super::lexer::Span;
use super::parser::{AttrForm, Document, Inline, NodeKind, Script, Tag};
use crate::extract::Symbol;
use crate::highlight::{HighlightClass, Span as HlSpan};

// ── the Ruby fragment stream ──────────────────────────────────────────────

/// Where a Ruby fragment came from. The Rails lens treats them all alike;
/// the kind exists so a reader (and H2a) can tell an attribute hash from a
/// script line without re-deriving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FragmentKind {
    /// A `=`/`~`/`&=`/`!=`/`-` line, or a tag's inline `= …`.
    Script,
    /// The inner Ruby of a `#{…}`.
    Interpolation,
    /// A `{ … }` attribute hash.
    Attributes,
    /// A `[ … ]` object reference.
    ObjectRef,
    /// A `:ruby` filter's body.
    RubyFilter,
}

/// One Ruby span in a HAML template.
#[derive(Debug, Clone)]
pub struct Fragment {
    pub kind: FragmentKind,
    /// The EXACT byte range in the HAML source.
    pub span: Span,
    /// The Ruby text. Equal to `span`'s bytes when `verbatim`; otherwise
    /// the source was reassembled (a `|` continuation, a trailing-comma
    /// continuation) and `span` still covers every byte it came from.
    pub text: String,
    pub verbatim: bool,
    /// 1-based HAML line the fragment starts on.
    pub line: u32,
}

/// Every Ruby fragment in `doc`, in document order.
pub fn ruby_fragments(doc: &Document, src: &str) -> Vec<Fragment> {
    let mut out = Vec::new();
    let mut program = ProgramBuilder::new(src, false);
    program.walk_roots(doc, &mut out);
    out
}

/// The synthesized Ruby program plus its line map.
#[derive(Debug, Clone, Default)]
pub struct RubyProgram {
    pub source: String,
    /// One entry per line of `source`: the 1-based HAML line it came from,
    /// or `None` for a line this module synthesized (an `end`, a wrapper).
    pub lines: Vec<Option<u32>>,
}

impl RubyProgram {
    /// The HAML line for a 0-based row of [`RubyProgram::source`] — the
    /// re-anchoring every Rails edge minted from this program goes
    /// through. `None` for a synthetic line, and for a row past the end
    /// (which a correct caller never asks for, and which must not panic
    /// when a Ruby parse error makes one appear).
    pub fn haml_line(&self, row: usize) -> Option<u32> {
        self.lines.get(row).copied().flatten()
    }
}

/// Build the one-parse Ruby program for `doc`.
pub fn ruby_program(doc: &Document, src: &str) -> RubyProgram {
    let mut builder = ProgramBuilder::new(src, true);
    let mut fragments = Vec::new();
    builder.walk_roots(doc, &mut fragments);
    builder.finish()
}

struct ProgramBuilder<'a> {
    src: &'a str,
    emit: bool,
    out: String,
    map: Vec<Option<u32>>,
}

impl<'a> ProgramBuilder<'a> {
    fn new(src: &'a str, emit: bool) -> Self {
        ProgramBuilder {
            src,
            emit,
            out: String::new(),
            map: Vec::new(),
        }
    }

    fn finish(self) -> RubyProgram {
        RubyProgram {
            source: self.out,
            lines: self.map,
        }
    }

    /// Push `text` (which may itself be multi-line) as program lines
    /// starting at HAML line `line`. `prefix` is applied to the FIRST line
    /// and `suffix` to the last, so a hash literal becomes a statement
    /// without moving any line.
    fn push(&mut self, text: &str, line: u32, prefix: &str, suffix: &str) {
        if !self.emit {
            return;
        }
        let pieces: Vec<&str> = text.split('\n').collect();
        for (k, piece) in pieces.iter().enumerate() {
            if k == 0 {
                self.out.push_str(prefix);
            }
            self.out.push_str(piece);
            if k + 1 == pieces.len() {
                self.out.push_str(suffix);
            }
            self.out.push('\n');
            self.map.push(Some(line + k as u32));
        }
    }

    /// A line this module invented — an `end`. Never maps to HAML source,
    /// which is exactly why the map's entry is `None` rather than a
    /// borrowed neighbouring line number.
    fn push_synthetic(&mut self, text: &str) {
        if !self.emit {
            return;
        }
        self.out.push_str(text);
        self.out.push('\n');
        self.map.push(None);
    }

    fn frag(
        &mut self,
        out: &mut Vec<Fragment>,
        kind: FragmentKind,
        span: Span,
        text: String,
        verbatim: bool,
        line: u32,
    ) {
        if text.trim().is_empty() {
            return;
        }
        let (prefix, suffix) = match kind {
            FragmentKind::Attributes => ("_haml_attributes = {", "}"),
            FragmentKind::ObjectRef => ("_haml_object_ref = [", "]"),
            _ => ("", ""),
        };
        self.push(&text, line, prefix, suffix);
        out.push(Fragment {
            kind,
            span,
            text,
            verbatim,
            line,
        });
    }

    fn interpolations(&mut self, out: &mut Vec<Fragment>, spans: &[Span]) {
        for s in spans {
            let Some(text) = s.slice(self.src) else {
                continue;
            };
            let line = line_at(self.src, s.start as usize);
            self.frag(
                out,
                FragmentKind::Interpolation,
                *s,
                text.to_string(),
                true,
                line,
            );
        }
    }

    fn walk_roots(&mut self, doc: &Document, out: &mut Vec<Fragment>) {
        let roots: Vec<usize> = doc.roots.clone();
        self.walk_list(doc, &roots, out);
    }

    fn walk_list(&mut self, doc: &Document, ids: &[usize], out: &mut Vec<Fragment>) {
        for id in ids {
            self.walk(doc, *id, out);
        }
    }

    fn walk(&mut self, doc: &Document, id: usize, out: &mut Vec<Fragment>) {
        let node = doc.node(id);
        let children = node.children.clone();
        // `opens_block` is decided by the INDENTATION TREE, not by a Ruby
        // heuristic: if a script node has children, HAML compiled them
        // inside its block, so the program needs exactly one `end`. That
        // is true for `do |x|`, for `if`, and for every construct neither
        // this scanner nor HAML itself tags.
        let mut opens_block = false;
        match &node.kind {
            NodeKind::Script(s) | NodeKind::SilentScript(s) => {
                self.script(out, s, node.line);
                opens_block = !children.is_empty();
            }
            NodeKind::Tag(tag) => {
                self.tag(out, tag);
                opens_block =
                    !children.is_empty() && matches!(&tag.inline, Some(Inline::Script(_)));
            }
            NodeKind::Plain(t) => self.interpolations(out, &t.interpolations),
            NodeKind::HtmlComment(c) => self.interpolations(out, &c.body.interpolations),
            NodeKind::Filter(f) => {
                if f.name == "ruby" {
                    if let Some(span) = f.body_span {
                        self.frag(
                            out,
                            FragmentKind::RubyFilter,
                            span,
                            f.text.clone(),
                            false,
                            node.line + 1,
                        );
                    }
                } else if let Some(span) = f.body_span {
                    let spans = super::interpolations_in(self.src, span);
                    self.interpolations(out, &spans);
                }
            }
            NodeKind::Doctype(_) | NodeKind::HamlComment(_) => {}
        }
        self.walk_list(doc, &children, out);
        if opens_block {
            self.push_synthetic("end");
        }
    }

    fn script(&mut self, out: &mut Vec<Fragment>, s: &Script, line: u32) {
        self.frag(
            out,
            FragmentKind::Script,
            s.span,
            s.code.clone(),
            s.verbatim,
            line,
        );
        // An interpolation inside a script line is already inside the Ruby
        // this fragment carries — emitting it again would double-count
        // every `t()` written as `= "#{t('.x')}"`.
    }

    fn tag(&mut self, out: &mut Vec<Fragment>, tag: &Tag) {
        for sh in &tag.shorthand {
            self.interpolations(out, &sh.interpolations);
        }
        for group in &tag.attrs {
            let Some(text) = group.inner.slice(self.src) else {
                continue;
            };
            let line = line_at(self.src, group.inner.start as usize);
            match group.form {
                AttrForm::RubyHash => self.frag(
                    out,
                    FragmentKind::Attributes,
                    group.inner,
                    text.to_string(),
                    true,
                    line,
                ),
                AttrForm::ObjectRef => self.frag(
                    out,
                    FragmentKind::ObjectRef,
                    group.inner,
                    text.to_string(),
                    true,
                    line,
                ),
                // An HTML-style group holds literal pairs; only its `#{}`
                // values are Ruby.
                AttrForm::HtmlStyle => {
                    let spans = group.interpolations.clone();
                    self.interpolations(out, &spans);
                }
            }
        }
        match &tag.inline {
            Some(Inline::Script(s)) => {
                let line = line_at(self.src, s.span.start as usize);
                self.script(out, s, line);
            }
            Some(Inline::Text(t)) => {
                let spans = t.interpolations.clone();
                self.interpolations(out, &spans);
            }
            None => {}
        }
    }
}

/// The 1-based line containing byte `offset`.
pub fn line_at(src: &str, offset: usize) -> u32 {
    let upto = offset.min(src.len());
    1 + src.as_bytes()[..upto]
        .iter()
        .filter(|b| **b == b'\n')
        .count() as u32
}

// ── the outline ───────────────────────────────────────────────────────────

/// The `symbols` kind for a HAML element row.
pub const KIND_ELEMENT: &str = "element";
/// The `symbols` kind for a HAML filter row.
pub const KIND_FILTER: &str = "filter";

/// The template outline: one row per element and per filter, in document
/// order, with `container` naming the nearest enclosing element.
///
/// Elements only. A script line is NOT an outline row — it is Ruby, and
/// this scanner mints no symbol for Ruby it did not scope-resolve (the
/// crate's oracle bar, applied to a lane that would otherwise be tempted
/// to call `- items.each do |i|` a definition).
pub fn outline(doc: &Document, src: &str) -> Vec<Symbol> {
    let mut out = Vec::new();
    for id in doc.preorder() {
        let node = doc.node(id);
        let (name, kind) = match &node.kind {
            NodeKind::Tag(tag) => (tag.display_name(), KIND_ELEMENT),
            NodeKind::Filter(f) => (f.name.clone(), KIND_FILTER),
            _ => continue,
        };
        let container = container_of(doc, id);
        let line_end = subtree_last_line(doc, id, src);
        out.push(Symbol {
            ordinal: out.len() as u32,
            name,
            kind: kind.to_string(),
            line_start: node.line,
            line_end,
            col_start: node.indent,
            col_end: node
                .span
                .end
                .saturating_sub(node.span.start)
                .saturating_add(node.indent),
            container,
            signature: None,
            doc: None,
            param_min: None,
            param_max: None,
        });
    }
    out
}

fn container_of(doc: &Document, id: usize) -> Option<String> {
    let mut cur = doc.node(id).parent;
    while let Some(p) = cur {
        match &doc.node(p).kind {
            NodeKind::Tag(tag) => return Some(tag.display_name()),
            NodeKind::Filter(f) => return Some(f.name.clone()),
            _ => cur = doc.node(p).parent,
        }
    }
    None
}

fn subtree_last_line(doc: &Document, id: usize, src: &str) -> u32 {
    let node = doc.node(id);
    let mut last = node
        .line
        .max(line_at(src, node.span.end.saturating_sub(1) as usize));
    for c in &node.children {
        last = last.max(subtree_last_line(doc, *c, src));
    }
    last
}

// ── highlight spans ───────────────────────────────────────────────────────

/// Highlight spans for a HAML file: the template's own tokens painted by
/// this scanner, plus every Ruby fragment painted by the EXISTING Ruby
/// highlighter (`highlight::extract_highlights("ruby", …)`), its spans
/// shifted into HAML coordinates.
///
/// The result is sorted and non-overlapping — the SPA's per-line integrity
/// guard requires it, and a scanner that emitted a HAML span across a Ruby
/// span would make one of the two invisible with no way to tell which.
pub fn highlight_spans(doc: &Document, src: &str) -> Vec<HlSpan> {
    let mut spans: Vec<HlSpan> = Vec::new();
    let mut push = |span: Span, class: HighlightClass| {
        if span.is_empty() {
            return;
        }
        spans.push(HlSpan {
            byte_start: span.start,
            byte_len: span.end - span.start,
            class,
        });
    };
    for id in doc.preorder() {
        match &doc.node(id).kind {
            NodeKind::Doctype(d) => {
                push(
                    Span::new(d.span.start as usize - 3, d.span.start as usize),
                    HighlightClass::Keyword,
                );
                push(d.span, HighlightClass::Constant);
            }
            NodeKind::Tag(tag) => {
                if tag.name_explicit {
                    push(
                        Span::new(tag.name_span.start as usize - 1, tag.name_span.end as usize),
                        HighlightClass::Type,
                    );
                }
                for sh in &tag.shorthand {
                    push(sh.span, HighlightClass::Attribute);
                }
                for g in &tag.attrs {
                    for a in &g.statics {
                        push(a.name_span, HighlightClass::Property);
                        push(a.value_span, HighlightClass::String);
                    }
                }
                if let Some(Inline::Script(s)) = &tag.inline {
                    push(s.sigil_span, HighlightClass::Operator);
                }
            }
            NodeKind::Script(s) | NodeKind::SilentScript(s) => {
                push(s.sigil_span, HighlightClass::Operator);
            }
            NodeKind::HamlComment(_) => {
                push(doc.node(id).span, HighlightClass::Comment);
            }
            NodeKind::HtmlComment(c) => {
                push(doc.node(id).span, HighlightClass::Comment);
                if let Some(cs) = c.conditional_span {
                    push(cs, HighlightClass::Constant);
                }
            }
            NodeKind::Filter(f) => {
                push(f.name_span, HighlightClass::Keyword);
            }
            NodeKind::Plain(_) => {}
        }
    }
    // The interpolation delimiters, and the Ruby inside every fragment.
    let fragments = ruby_fragments(doc, src);
    for f in &fragments {
        if f.kind == FragmentKind::Interpolation {
            let s = f.span.start as usize;
            push(Span::new(s.saturating_sub(2), s), HighlightClass::Escape);
            push(
                Span::new(f.span.end as usize, f.span.end as usize + 1),
                HighlightClass::Escape,
            );
        }
    }
    for f in &fragments {
        if !f.verbatim {
            // A reassembled fragment's text no longer lines up with the
            // source byte for byte, so painting it would mis-place every
            // span. Left unpainted rather than approximately painted.
            continue;
        }
        let Ok(inner) = crate::highlight::extract_highlights("ruby", f.text.as_bytes()) else {
            continue;
        };
        for s in inner {
            spans.push(HlSpan {
                byte_start: f.span.start + s.byte_start,
                byte_len: s.byte_len,
                class: s.class,
            });
        }
    }
    normalize(spans, src.len() as u32)
}

/// Sort, clamp and de-overlap. Ties prefer the LONGER span (a tag name
/// beats a one-byte sigil that starts at the same offset); overlaps after
/// that are dropped, never truncated, so no span ever claims bytes its
/// producer did not look at.
fn normalize(mut spans: Vec<HlSpan>, len: u32) -> Vec<HlSpan> {
    spans.retain(|s| s.byte_len > 0 && s.byte_start < len);
    for s in spans.iter_mut() {
        if s.byte_start + s.byte_len > len {
            s.byte_len = len - s.byte_start;
        }
    }
    spans.sort_by(|a, b| {
        a.byte_start
            .cmp(&b.byte_start)
            .then(b.byte_len.cmp(&a.byte_len))
    });
    let mut out: Vec<HlSpan> = Vec::with_capacity(spans.len());
    let mut cursor = 0u32;
    for s in spans {
        if s.byte_start < cursor {
            continue;
        }
        cursor = s.byte_start + s.byte_len;
        out.push(s);
    }
    out
}
