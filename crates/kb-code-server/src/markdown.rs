//! Markdown **document outline** and **fenced-code injection regions** —
//! V72-H2a, design D7.
//!
//! `tree-sitter-md` ships two grammars; kb-code registers the BLOCK one
//! (`lang::MARKDOWN`), which is the one that has the two things a code
//! reader wants from prose: the heading tree, and the fences that hold
//! actual code.
//!
//! # The outline
//!
//! One row per HEADING SECTION, `kind = "heading"`. The block grammar
//! nests `section` nodes under their heading, so an `##` under an `#` is
//! literally a child node — the row's `line_end` is the SECTION's end, not
//! the heading line's, which is what makes `crate::outline`'s
//! range-containment nesting reproduce the document's own structure
//! instead of a flat list. `detail` (and the symbol's `signature`) is the
//! level, `h1`..`h6`, because two sibling `###`s under different `##`s are
//! only distinguishable by their tree position and a reader scanning a
//! flat `GET /api/symbols` list deserves to be told which is which.
//!
//! A setext heading (`Title` over `=====`) is a row too, at the level its
//! underline declares. One quirk of the block grammar is worked around
//! rather than inherited: it opens a `section` for a setext heading only
//! when the heading LEADS one, so a setext heading following an ATX
//! heading in the same section is a plain sibling node with no section of
//! its own. Such a row is still emitted, with the ENCLOSING section's end
//! as its extent — the best reading available, and named here rather than
//! silently dropping a heading a reader can see.
//!
//! What is NOT a row: fenced code blocks (they are injection HOSTS, see
//! below — a fence is not a definition), lists, tables, and links.
//! **Links are deliberately not minted as candidate refs.** A `[text]
//! (path/to.rb)` in prose is a real reference and kb ALREADY extracts
//! exactly that class of hint, corpus-side, in `kb_core::coderefs` (root
//! CLAUDE.md invariant #2: kb extracts HINTS, kb-code mints CLASSES,
//! nothing is cached). Minting a second, kb-code-local doc→code reference
//! lane here would be a competing extractor over the same text with its
//! own drift — the opposite of that invariant's one-direction rule.
//!
//! # The injections
//!
//! [`fenced_regions`] returns one region per fenced code block whose info
//! string names a language the `syntax/1` registry can actually parse.
//! `crate::injection` is what consumes it; this module only knows where
//! the fences are.
//!
//! Two honest refusals, both because a WRONG offset is worse than none:
//!
//! * a fence whose `code_fence_content` carries a NON-EMPTY
//!   `block_continuation` child yields NO region. Every fence has
//!   continuation children — at top level they are zero-width markers the
//!   grammar emits per line — but inside a block quote or a list item they
//!   consume the real `> ` / indent prefix bytes, so the content is not a
//!   contiguous slice of the file and painting it would shift every span
//!   past the first one. The WIDTH is the test, not the presence.
//! * a fence whose info string names nothing the registry parses yields no
//!   region, and the fence body is left unpainted rather than guessed at.
//!
//! An info string is resolved against the registry itself, never a
//! hand-kept alias table: ```` ```ruby ```` matches a row's `lang`,
//! ```` ```rb ```` matches a row's EXTENSION. That is why `sh`, `yml`,
//! `py`, `ts` and `rs` all work without a line of new data.

use crate::extract::Symbol;
use crate::lang::{self, LangError};
use tree_sitter::Node;

pub type Result<T> = std::result::Result<T, LangError>;

/// Row cap for one file. `capped` reports the cut.
pub const MAX_ROWS: usize = 500;
/// Heading-name cap, in chars, post whitespace-collapse.
pub const NAME_CAP: usize = 200;

/// The only kind this module mints.
pub const KIND_HEADING: &str = "heading";

/// One [`outline`] call's result — the `yaml::YamlOutline` shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocOutline {
    pub symbols: Vec<Symbol>,
    pub capped: bool,
}

/// One fenced code block that a guest grammar can be run over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fence {
    /// The registry `lang` the info string resolved to.
    pub lang: &'static str,
    /// The fence body's byte range in the Markdown source — a CONTIGUOUS
    /// slice by construction (see the module doc's first refusal).
    pub byte_start: u32,
    pub byte_end: u32,
    /// 0-based row of `byte_start` in the Markdown source.
    pub row_start: u32,
}

/// Walk `source` into a heading outline. Never errors on malformed
/// Markdown (there is barely such a thing); the only `Err` is
/// `lang::parse`'s own defensive variants.
pub fn outline(source: &[u8]) -> Result<DocOutline> {
    let (tree, _language) = lang::parse("markdown", source)?;
    let mut w = Walker {
        source,
        symbols: Vec::new(),
        capped: false,
    };
    w.walk(tree.root_node(), None);
    Ok(DocOutline {
        symbols: w.symbols,
        capped: w.capped,
    })
}

struct Walker<'s> {
    source: &'s [u8],
    symbols: Vec<Symbol>,
    capped: bool,
}

impl Walker<'_> {
    /// Walk `node`'s `section` children. Only sections are descended
    /// into: a heading the block grammar did NOT wrap in a section — the
    /// shape one takes directly inside a block quote or a list item — is
    /// not a row, because the outline is the DOCUMENT's structure and a
    /// heading inside a quotation is quoted text.
    fn walk(&mut self, node: Node<'_>, container: Option<String>) {
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node
            .named_children(&mut cursor)
            .filter(|c| c.kind() == "section")
            .collect();
        for child in children {
            if self.capped {
                return;
            }
            self.section(child, container.clone());
        }
    }

    /// A `section`: its LEADING heading is the row, the SECTION's end is
    /// the row's `line_end`, and its nested sections hang off it. A later
    /// `setext_heading` sibling — the grammar quirk the module doc names —
    /// is emitted too, with this section's end as its extent.
    fn section(&mut self, node: Node<'_>, container: Option<String>) {
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        let mut inner = container;
        for child in children {
            if self.capped {
                return;
            }
            match child.kind() {
                // `inner` is already whichever container applies: for the
                // section's LEADING heading it is still what was passed
                // in, and for a later setext sibling it is the heading
                // that one follows.
                "atx_heading" | "setext_heading" => {
                    if let Some(name) = self.emit_heading(child, node, inner.clone()) {
                        inner = Some(name);
                    }
                }
                "section" => self.section(child, inner.clone()),
                _ => {}
            }
        }
    }

    /// Emit one heading row. `extent` is the node whose end the row covers
    /// (the enclosing `section`, so the row spans everything under the
    /// heading). Returns the row's name, for use as its children's
    /// container.
    fn emit_heading(
        &mut self,
        heading: Node<'_>,
        extent: Node<'_>,
        container: Option<String>,
    ) -> Option<String> {
        if self.symbols.len() >= MAX_ROWS {
            self.capped = true;
            return None;
        }
        let level = heading_level(heading);
        let name = self.heading_text(heading).unwrap_or_default();
        let name = if name.is_empty() {
            // An empty `##` still has structure; naming it by its level is
            // better than dropping a level of the tree.
            format!("(untitled h{level})")
        } else {
            name
        };
        let start = heading.start_position();
        let end = extent.end_position();
        self.symbols.push(Symbol {
            ordinal: self.symbols.len() as u32,
            name: name.clone(),
            kind: KIND_HEADING.to_string(),
            line_start: start.row as u32 + 1,
            line_end: (end.row as u32 + 1).max(start.row as u32 + 1),
            col_start: start.column as u32,
            col_end: heading.end_position().column as u32,
            container,
            // The level. Not a type signature — see `css.rs`'s same note.
            signature: Some(format!("h{level}")),
            doc: None,
            param_min: None,
            param_max: None,
        });
        if self.symbols.len() >= MAX_ROWS {
            self.capped = true;
        }
        Some(name)
    }

    /// The heading's text: its `heading_content` field (an `inline` node
    /// for ATX, a `paragraph` for setext), whitespace-collapsed and
    /// capped. The inline grammar is NOT run — the raw text is what an
    /// outline row wants, `**bold**` markers and all, and running a second
    /// grammar to strip them would be a rendering decision this lane does
    /// not make.
    fn heading_text(&self, heading: Node<'_>) -> Option<String> {
        let content = heading.child_by_field_name("heading_content")?;
        let raw = content.utf8_text(self.source).ok()?;
        // A trailing closing run (`## Title ##`) is ATX syntax, not text.
        let text = raw.trim().trim_end_matches('#').trim();
        Some(cap(&text.split_whitespace().collect::<Vec<_>>().join(" ")))
    }
}

/// 1..=6 from the marker child, defaulting to 1 for a shape with none.
fn heading_level(heading: Node<'_>) -> u32 {
    let mut cursor = heading.walk();
    for child in heading.children(&mut cursor) {
        let k = child.kind();
        if let Some(rest) = k.strip_prefix("atx_h") {
            if let Some(n) = rest.strip_suffix("_marker").and_then(|d| d.parse().ok()) {
                return n;
            }
        }
        if let Some(rest) = k.strip_prefix("setext_h") {
            if let Some(n) = rest.strip_suffix("_underline").and_then(|d| d.parse().ok()) {
                return n;
            }
        }
    }
    1
}

fn cap(s: &str) -> String {
    if s.chars().count() <= NAME_CAP {
        return s.to_string();
    }
    s.chars().take(NAME_CAP).collect()
}

/// Every fenced code block whose info string resolves to a registry
/// language kb-code can parse, in document order. See the module doc for
/// the two shapes that are deliberately skipped.
pub fn fenced_regions(source: &[u8]) -> Result<Vec<Fence>> {
    let (tree, _language) = lang::parse("markdown", source)?;
    let mut out = Vec::new();
    collect_fences(tree.root_node(), source, &mut out);
    Ok(out)
}

fn collect_fences(node: Node<'_>, source: &[u8], out: &mut Vec<Fence>) {
    if node.kind() == "fenced_code_block" {
        if let Some(f) = fence_of(node, source) {
            out.push(f);
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor).collect::<Vec<_>>() {
        collect_fences(child, source, out);
    }
}

fn fence_of(node: Node<'_>, source: &[u8]) -> Option<Fence> {
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
    let info = children.iter().find(|c| c.kind() == "info_string")?;
    let content = children.iter().find(|c| c.kind() == "code_fence_content")?;
    // Not contiguous — see the module doc's first refusal. A ZERO-WIDTH
    // continuation is the per-line marker every fence carries; a non-empty
    // one has eaten real prefix bytes.
    let mut content_cursor = content.walk();
    if content
        .children(&mut content_cursor)
        .any(|c| c.kind() == "block_continuation" && c.end_byte() > c.start_byte())
    {
        return None;
    }
    let tag = info.utf8_text(source).ok()?;
    let lang = resolve_info_string(tag)?;
    let start = content.start_byte() as u32;
    let end = content.end_byte() as u32;
    if end <= start {
        return None;
    }
    Some(Fence {
        lang,
        byte_start: start,
        byte_end: end,
        row_start: content.start_position().row as u32,
    })
}

/// Resolve a fence's info string to a registry `lang`, or `None`.
///
/// Derived from the `syntax/1` registry, never a hand-kept alias table:
/// the first whitespace-delimited word, lowercased, is matched against
/// every row's `lang` first and then against every row's EXTENSIONS. A row
/// nothing parses (`sql`, `dockerfile`) resolves to `None`, and so does a
/// row with a grammar but no highlights query (`erb`) — there would be
/// nothing to run, so declaring an injection there would be a claim with
/// no derivation behind it.
pub fn resolve_info_string(info: &str) -> Option<&'static str> {
    let word = info
        .split_whitespace()
        .next()?
        .trim_matches(['{', '}', ',']);
    if word.is_empty() {
        return None;
    }
    let lower = word.to_ascii_lowercase();
    // The SAME predicate the wire's `injections` list is derived from —
    // see `injection::is_paintable_guest`.
    let usable =
        |row: &&'static crate::syntax::SyntaxRow| crate::injection::is_paintable_guest(row.lang);
    if let Some(row) = crate::syntax::REGISTRY
        .iter()
        .find(|r| r.lang == lower && usable(r))
    {
        return Some(row.lang);
    }
    crate::syntax::REGISTRY
        .iter()
        .find(|r| r.extensions.contains(&lower.as_str()) && usable(r))
        .map(|r| r.lang)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"# Title

Intro prose.

## Setup

Run this:

```bash
echo hi
```

### Details

Some `inline code` and a [link](app/models/order.rb).

## Usage

```ruby
class Order
end
```

```
no info string
```

```cobol
IDENTIFICATION DIVISION.
```

Setext Heading
==============

Trailing prose.
"#;

    fn rows(src: &str) -> Vec<(String, Option<String>, u32, u32)> {
        outline(src.as_bytes())
            .unwrap()
            .symbols
            .into_iter()
            .map(|s| (s.name, s.signature, s.line_start, s.line_end))
            .collect()
    }

    #[test]
    fn heading_outline_snapshot() {
        insta::assert_debug_snapshot!(rows(FIXTURE));
    }

    #[test]
    fn a_section_row_spans_its_whole_section_not_just_the_heading_line() {
        let symbols = outline(FIXTURE.as_bytes()).unwrap().symbols;
        let setup = symbols.iter().find(|s| s.name == "Setup").expect("Setup");
        assert!(
            setup.line_end > setup.line_start + 1,
            "a heading row must cover its section so containment nesting works: {setup:?}"
        );
        let details = symbols
            .iter()
            .find(|s| s.name == "Details")
            .expect("Details");
        assert!(
            details.line_start > setup.line_start && details.line_end <= setup.line_end,
            "### must be contained by its ##: {setup:?} / {details:?}"
        );
        assert_eq!(details.container.as_deref(), Some("Setup"));
        assert_eq!(details.signature.as_deref(), Some("h3"));
    }

    #[test]
    fn every_minted_row_is_a_heading() {
        for s in outline(FIXTURE.as_bytes()).unwrap().symbols {
            assert_eq!(s.kind, KIND_HEADING, "{s:?}");
        }
    }

    #[test]
    fn fences_resolve_through_the_registry_and_skip_what_nothing_parses() {
        let fences = fenced_regions(FIXTURE.as_bytes()).unwrap();
        let langs: Vec<&str> = fences.iter().map(|f| f.lang).collect();
        // ```bash and ```ruby resolve; the bare fence and ```cobol do not.
        assert_eq!(langs, vec!["bash", "ruby"], "{fences:?}");
        for f in &fences {
            let body = &FIXTURE.as_bytes()[f.byte_start as usize..f.byte_end as usize];
            let text = std::str::from_utf8(body).unwrap();
            assert!(
                !text.contains("```"),
                "a fence body must exclude its delimiters: {text:?}"
            );
        }
    }

    #[test]
    fn info_string_aliases_come_from_the_registrys_own_extensions() {
        assert_eq!(resolve_info_string("ruby"), Some("ruby"));
        assert_eq!(resolve_info_string("rb"), Some("ruby"));
        assert_eq!(resolve_info_string("RB"), Some("ruby"));
        assert_eq!(resolve_info_string("py"), Some("python"));
        assert_eq!(resolve_info_string("ts"), Some("typescript"));
        assert_eq!(resolve_info_string("yml"), Some("yaml"));
        assert_eq!(resolve_info_string("sh"), Some("bash"));
        assert_eq!(resolve_info_string("scss"), Some("scss"));
        assert_eq!(resolve_info_string("rust ignore"), Some("rust"));
        // A NAMED row nothing parses has no grammar to run.
        assert_eq!(resolve_info_string("sql"), None);
        assert_eq!(resolve_info_string("dockerfile"), None);
        // A grammar with no highlights query paints nothing, so it is not
        // a usable guest either — see `injection::is_paintable_guest`.
        assert_eq!(resolve_info_string("erb"), None);
        // A HIGHLIGHT_ONLY row still paints, so it stays a usable guest.
        assert_eq!(resolve_info_string("scss"), Some("scss"));
        assert_eq!(resolve_info_string("cobol"), None);
        assert_eq!(resolve_info_string(""), None);
        assert_eq!(resolve_info_string("   "), None);
    }

    /// The grammar quirk the module doc names: a setext heading after an
    /// ATX heading gets no section of its own, and must still be a row.
    #[test]
    fn a_setext_heading_that_the_grammar_did_not_wrap_in_a_section_is_still_a_row() {
        let got = rows("# A\n\ntext\n\nSetext\n======\n\ntail\n");
        let names: Vec<&str> = got.iter().map(|(n, _, _, _)| n.as_str()).collect();
        assert_eq!(names, vec!["A", "Setext"], "{got:?}");
        let setext = got.iter().find(|(n, _, _, _)| n == "Setext").unwrap();
        assert_eq!(setext.1.as_deref(), Some("h1"));
    }

    #[test]
    fn a_fence_inside_a_block_quote_is_skipped_rather_than_mis_offset() {
        let src = "> ```ruby\n> class A\n> end\n> ```\n";
        let fences = fenced_regions(src.as_bytes()).unwrap();
        assert!(
            fences.is_empty(),
            "a non-contiguous fence body must yield no region: {fences:?}"
        );
    }

    #[test]
    fn degenerate_input_never_panics() {
        for src in [
            "",
            "#",
            "######",
            "#######",
            "```",
            "```ruby",
            "```ruby\n",
            "````\n```\n````\n",
            "# a\n## b\n### c\n#### d\n##### e\n###### f\n",
            "```markdown\n# nested\n```\n",
            "=\n",
        ] {
            let out = outline(src.as_bytes()).unwrap_or_else(|e| panic!("{src:?}: {e}"));
            for s in &out.symbols {
                assert!(s.line_end >= s.line_start, "{src:?}: {s:?}");
            }
            let fences = fenced_regions(src.as_bytes()).unwrap();
            for f in fences {
                assert!(f.byte_end as usize <= src.len(), "{src:?}: {f:?}");
                assert!(f.byte_start < f.byte_end, "{src:?}: {f:?}");
            }
        }
    }

    #[test]
    fn a_pathological_document_reports_its_cap() {
        let src: String = (0..MAX_ROWS + 50).map(|i| format!("# h{i}\n\n")).collect();
        let out = outline(src.as_bytes()).unwrap();
        assert!(out.capped);
        assert_eq!(out.symbols.len(), MAX_ROWS);
    }
}
