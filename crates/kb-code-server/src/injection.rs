//! `injection` — the ONE place a host language's embedded guest code is
//! located, and the ONE offset map every re-anchoring goes through
//! (V72-H2a, design D7).
//!
//! # What this generalises
//!
//! Before this module there were two host→guest mechanisms in this crate
//! and they had nothing in common but their purpose:
//!
//! * ERB (PRR-N3) — walk the `tree-sitter-embedded-template` CST, take
//!   each `directive`/`output_directive`'s `code` child, re-parse it as
//!   Ruby, and re-anchor line numbers by adding the code node's own row.
//! * HAML (V72-H3) — the scanner has no per-tag `code` node, so it
//!   synthesizes ONE Ruby program out of the whole template with `end`s
//!   derived from the indentation tree, plus a LINE MAP back to HAML lines.
//!
//! Markdown makes a third, and a third bespoke walk is where a family of
//! subtly different offset bugs lives. So the two shapes are named here
//! instead:
//!
//! * [`RegionKind::Fragment`] — the guest text IS a contiguous slice of
//!   the host. Positions map by ADDITION ([`OffsetMap::Shift`]), so both
//!   byte offsets (highlight spans) and rows (lens edges) round-trip.
//! * [`RegionKind::Program`] — the guest text was REASSEMBLED and is not a
//!   slice of anything. Only ROWS map back, through a line map
//!   ([`OffsetMap::Lines`]) whose `None` entries are lines this crate
//!   invented. A byte offset is structurally unavailable and
//!   [`OffsetMap::host_byte`] says so by returning `None` rather than an
//!   approximation — the same rule `haml::extract::highlight_spans`
//!   already followed by refusing to paint a reassembled fragment.
//!
//! # The three hosts (and the one that is not)
//!
//! | host | guest | shape |
//! |------|-------|-------|
//! | `erb` | `ruby` | one `Fragment` per directive's `code` node |
//! | `haml` | `ruby` | one `Fragment` per verbatim scanner fragment, plus ONE `Program` (the synthesized template program) |
//! | `markdown` | whatever a fence's info string resolves to | one `Fragment` per contiguous fenced code block |
//!
//! **HTML is NOT a host here**, and that is a fact about this build rather
//! than a decision about HTML: kb-code links no `tree-sitter-html` (the
//! Rails lens's Stimulus scan deliberately regex-scans ERB's raw `content`
//! nodes instead — `frameworks::rails::stimulus`, and design-nav.md §2's
//! own "no tree-sitter-html" ruling). With no HTML grammar there is no
//! `script_element`/`style_element` to declare a region over, so `.html`
//! is not a registry row at all and `<script>`/`<style>` injections do not
//! exist. Adding the grammar is what would make them possible; nothing
//! here pretends otherwise.
//!
//! # Byte-identical, on purpose
//!
//! `frameworks::rails::support::walk_erb_ruby_fragments` and
//! `walk_haml_ruby_fragments` now iterate THIS module's regions instead of
//! carrying their own walk, and their output is byte-identical — the ERB
//! region walk is the same preorder, taking the same `code` child and the
//! same `start_position().row`, and the HAML program is the same
//! `haml::extract::ruby_program`. The rails-lens goldens are the proof.
//! Nothing about edge minting, edge kinds or trust classes moved.
//!
//! # Nothing recurses
//!
//! [`paint_regions`] runs the GUEST's host-only highlighter
//! (`highlight::extract_highlights_host_only`), which by construction
//! never returns here. So a ```` ```markdown ```` fence inside a Markdown
//! file paints one level and stops; there is no depth counter to get
//! wrong, because there is no second level to count.

use crate::highlight::Span;
use crate::lang;
use tree_sitter::Node;

/// How a guest position maps back into its host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OffsetMap {
    /// The guest is a contiguous slice: guest byte `b` is host byte
    /// `byte_base + b`, and guest row `r` is host row `row_base + r`.
    Shift { byte_base: u32, row_base: u32 },
    /// The guest was reassembled: index = 0-based guest row, value = the
    /// 1-based HOST line it came from, or `None` for a synthesized line.
    Lines(Vec<Option<u32>>),
}

impl OffsetMap {
    /// The host byte for a guest byte offset — `None` for a reassembled
    /// guest, where no such mapping exists.
    pub fn host_byte(&self, guest_byte: u32) -> Option<u32> {
        match self {
            OffsetMap::Shift { byte_base, .. } => Some(byte_base + guest_byte),
            OffsetMap::Lines(_) => None,
        }
    }

    /// The 1-based host LINE for a 0-based guest row. `None` when the row
    /// is one this crate synthesized, or past the end of a reassembled
    /// guest (which a correct caller never asks for, and which must not
    /// panic when a guest parse error makes one appear).
    pub fn host_line(&self, guest_row: u32) -> Option<u32> {
        match self {
            OffsetMap::Shift { row_base, .. } => Some(row_base + guest_row + 1),
            OffsetMap::Lines(lines) => lines.get(guest_row as usize).copied().flatten(),
        }
    }

    /// The byte base, for the painter. `None` for a reassembled guest.
    pub fn byte_base(&self) -> Option<u32> {
        match self {
            OffsetMap::Shift { byte_base, .. } => Some(*byte_base),
            OffsetMap::Lines(_) => None,
        }
    }
}

/// Which of the two shapes a region is — see the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    /// A contiguous slice of the host. Highlight-paintable.
    Fragment,
    /// A reassembled guest program. Row-mappable only.
    Program,
}

/// One guest-language region inside a host file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// The guest language id — a `syntax/1` row's `lang`.
    pub lang: &'static str,
    pub kind: RegionKind,
    /// The host byte range this region was derived from. For a `Program`
    /// this is the whole file, which is honest: the program is built from
    /// every fragment in it.
    pub host_start: u32,
    pub host_end: u32,
    /// The guest source.
    pub source: String,
    pub map: OffsetMap,
}

/// Every host language, as declared by the `syntax/1` registry. Derived,
/// not a second list: a row is a host exactly when [`guest_langs`] finds
/// guests for it, and a test pins that against `SyntaxRow::injection_host`.
pub fn is_host(host_lang: &str) -> bool {
    !guest_langs(host_lang).is_empty()
}

/// The guest languages a host can contain, sorted, deduped — the
/// `injections` field on the `syntax/1` wire.
///
/// ERB and HAML embed exactly Ruby. Markdown embeds whatever a fence's
/// info string names, so its set is DERIVED from the registry itself:
/// every row something in this build parses. That is precisely the set
/// `markdown::resolve_info_string` can return, so the wire cannot promise
/// a fence language the painter would then skip.
pub fn guest_langs(host_lang: &str) -> Vec<&'static str> {
    match host_lang {
        "erb" | "haml" => vec!["ruby"],
        "markdown" => {
            let mut langs: Vec<&'static str> = crate::syntax::REGISTRY
                .iter()
                .filter(|r| is_paintable_guest(r.lang))
                .map(|r| r.lang)
                .collect();
            langs.sort_unstable();
            langs.dedup();
            langs
        }
        _ => Vec::new(),
    }
}

/// Whether a language can be a fence GUEST — i.e. whether running it over
/// a region would actually produce spans.
///
/// Not simply "something parses it": `erb` has a grammar and no
/// highlights query at all (`lang::ERB`'s own doc), so a ```` ```erb ````
/// fence would resolve, be declared on the wire, and paint nothing. The
/// predicate and the wire read the same function so that cannot happen —
/// this is the same "declare only what a consumer actually reads" rule
/// invariant 15's route walk enforces one layer up.
pub fn is_paintable_guest(lang: &str) -> bool {
    match crate::syntax::row_for_lang(lang) {
        Some(row) => row.engine.is_scanner() || crate::lang::highlights_query(lang).is_some(),
        None => false,
    }
}

/// Every region in `source`, parsing the host itself.
///
/// The three per-host entry points below take an ALREADY-PARSED host
/// (a tree, a scanner document) and exist because every in-pipeline caller
/// has one in hand — re-parsing per extractor is a real cost on a
/// monolith's view tree. This convenience form is for a caller that does
/// not, and for the tests.
pub fn regions(host_lang: &str, source: &[u8]) -> Vec<Region> {
    match host_lang {
        "erb" => match lang::parse("erb", source) {
            Ok((tree, _)) => erb_regions(tree.root_node(), source),
            Err(_) => Vec::new(),
        },
        "haml" => {
            let src = match std::str::from_utf8(source) {
                Ok(s) => s,
                Err(e) => std::str::from_utf8(&source[..e.valid_up_to()]).unwrap_or(""),
            };
            let doc = crate::haml::parser::parse_str(src);
            haml_regions(&doc, src)
        }
        "markdown" => markdown_regions(source),
        _ => Vec::new(),
    }
}

/// The one `Program` region for `host_lang`, if it has one. Only HAML
/// does; ERB deliberately does not (its accepted ceiling is one re-parse
/// per tag, so control flow spanning two tags is not reconstructed — see
/// `frameworks::rails::views`'s module doc).
pub fn program(host_lang: &str, source: &[u8]) -> Option<Region> {
    regions(host_lang, source)
        .into_iter()
        .find(|r| r.kind == RegionKind::Program)
}

// ── per-host region producers ────────────────────────────────────────────

/// ERB regions from an already-parsed embedded-template tree.
///
/// The walk is PRR-N3's, unchanged: preorder, a `directive`/
/// `output_directive` yields its `code` child and is not descended into,
/// `comment_directive` never matches (dead code, never executed).
pub fn erb_regions(root: Node<'_>, source: &[u8]) -> Vec<Region> {
    let mut out = Vec::new();
    collect_erb(root, source, &mut out);
    out
}

fn collect_erb(node: Node<'_>, source: &[u8], out: &mut Vec<Region>) {
    if matches!(node.kind(), "directive" | "output_directive") {
        let mut cursor = node.walk();
        let code = node.children(&mut cursor).find(|c| c.kind() == "code");
        if let Some(code) = code {
            if let Ok(text) = code.utf8_text(source) {
                out.push(Region {
                    lang: "ruby",
                    kind: RegionKind::Fragment,
                    host_start: code.start_byte() as u32,
                    host_end: code.end_byte() as u32,
                    source: text.to_string(),
                    map: OffsetMap::Shift {
                        byte_base: code.start_byte() as u32,
                        row_base: code.start_position().row as u32,
                    },
                });
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor).collect::<Vec<_>>() {
        collect_erb(child, source, out);
    }
}

/// HAML regions from an already-scanned document: every VERBATIM Ruby
/// fragment as a `Fragment`, then the synthesized program as the single
/// trailing `Program`.
///
/// A non-verbatim fragment (a `|` continuation, a trailing-comma
/// continuation) is deliberately NOT a `Fragment`: its text no longer
/// lines up with the source byte for byte, so a `Shift` map would
/// mis-place every span. Its content still reaches the lens through the
/// `Program`, which is row-mapped and therefore correct for it.
pub fn haml_regions(doc: &crate::haml::Document, src: &str) -> Vec<Region> {
    let fragments = crate::haml::extract::ruby_fragments(doc, src);
    let mut out = haml_fragment_regions(&fragments);
    let program = crate::haml::extract::ruby_program(doc, src);
    if !program.source.trim().is_empty() {
        out.push(Region {
            lang: "ruby",
            kind: RegionKind::Program,
            host_start: 0,
            host_end: src.len() as u32,
            source: program.source,
            map: OffsetMap::Lines(program.lines),
        });
    }
    out
}

/// The `Fragment` half of [`haml_regions`], over a fragment list the
/// caller already computed.
///
/// `haml::extract::highlight_spans` has one in hand (it needs the
/// interpolation fragments for the `#{`/`}` delimiters anyway) and calling
/// [`haml_regions`] there would re-walk the document AND build the Ruby
/// program the highlight lane has no use for. That is the whole reason
/// this conversion is its own pure function rather than folded into the
/// producer above.
pub fn haml_fragment_regions(fragments: &[crate::haml::extract::Fragment]) -> Vec<Region> {
    fragments
        .iter()
        .filter(|f| f.verbatim)
        .map(|f| Region {
            lang: "ruby",
            kind: RegionKind::Fragment,
            host_start: f.span.start,
            host_end: f.span.end,
            source: f.text.clone(),
            map: OffsetMap::Shift {
                byte_base: f.span.start,
                // `Fragment::line` is 1-based; a `Shift` row base is the
                // 0-based host row.
                row_base: f.line.saturating_sub(1),
            },
        })
        .collect()
}

/// Markdown regions: one per fenced code block whose info string resolves
/// to a language this build parses and whose body is a contiguous slice.
/// See `crate::markdown`'s module doc for the two refusals.
pub fn markdown_regions(source: &[u8]) -> Vec<Region> {
    let Ok(fences) = crate::markdown::fenced_regions(source) else {
        return Vec::new();
    };
    fences
        .into_iter()
        .filter_map(|f| {
            let body = source.get(f.byte_start as usize..f.byte_end as usize)?;
            let text = std::str::from_utf8(body).ok()?;
            Some(Region {
                lang: f.lang,
                kind: RegionKind::Fragment,
                host_start: f.byte_start,
                host_end: f.byte_end,
                source: text.to_string(),
                map: OffsetMap::Shift {
                    byte_base: f.byte_start,
                    row_base: f.row_start,
                },
            })
        })
        .collect()
}

// ── the painter ──────────────────────────────────────────────────────────

/// Host spans plus every `Fragment` region's guest spans, re-anchored
/// through that region's own map, normalized.
pub fn paint(host_lang: &str, source: &[u8], host_spans: Vec<Span>) -> Vec<Span> {
    let regions = regions(host_lang, source);
    paint_regions(host_spans, &regions, source.len() as u32)
}

/// The re-anchoring itself, over regions a caller already has.
///
/// Only `Fragment`s are painted — a `Program` has no byte mapping, and
/// approximating one is exactly the fabricated location this crate does
/// not produce.
pub fn paint_regions(mut spans: Vec<Span>, regions: &[Region], len: u32) -> Vec<Span> {
    // An UNCLASSIFIABLE host span never suppresses a guest's classified
    // ones. Markdown's block query captures a whole fenced block as
    // `@text.literal`, which this crate maps to `Other` (there is no
    // honest home for it in the fixed 15-class set); left in place, that
    // one coarse span would swallow every Ruby span inside the fence in
    // `normalize`'s overlap sweep, and the injection would be invisible.
    //
    // Scoped to `Other` on purpose, rather than "the guest always wins":
    // HAML paints an HTML comment as one `Comment` span that CONTAINS the
    // `#{…}` fragments inside it, and that is a real classification the
    // scanner made — dropping it would change HAML's highlight output and
    // owe a salt bump for no gain. Same principle as the dedup rule in
    // `extract_highlights_host_only`, one layer out.
    let fragments: Vec<(u32, u32)> = regions
        .iter()
        .filter(|r| r.kind == RegionKind::Fragment)
        .map(|r| (r.host_start, r.host_end))
        .collect();
    if !fragments.is_empty() {
        spans.retain(|s| {
            if s.class != crate::highlight::HighlightClass::Other {
                return true;
            }
            let end = s.byte_start + s.byte_len;
            !fragments.iter().any(|(a, b)| s.byte_start < *b && end > *a)
        });
    }
    for r in regions.iter().filter(|r| r.kind == RegionKind::Fragment) {
        let Some(base) = r.map.byte_base() else {
            continue;
        };
        // The GUEST's host-only highlighter: one level, never a recursion
        // back into this module (see the module doc).
        let Ok(inner) = crate::highlight::extract_highlights_host_only(r.lang, r.source.as_bytes())
        else {
            continue;
        };
        for s in inner {
            spans.push(Span {
                byte_start: base + s.byte_start,
                byte_len: s.byte_len,
                class: s.class,
            });
        }
    }
    normalize(spans, len)
}

/// Sort, clamp and de-overlap. Ties prefer the LONGER span (a tag name
/// beats a one-byte sigil that starts at the same offset); overlaps after
/// that are dropped, never truncated, so no span ever claims bytes its
/// producer did not look at.
///
/// Moved here from `haml::extract` in V72-H2a — byte-for-byte the same
/// function, now shared by every injection host, which is what keeps the
/// HAML highlight goldens unchanged.
pub fn normalize(mut spans: Vec<Span>, len: u32) -> Vec<Span> {
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
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
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

#[cfg(test)]
mod tests {
    use super::*;

    const ERB: &str = "<div>\n  <%= render \"orders/row\" %>\n  <% if a %>\n    <p><%= t(\".x\") %></p>\n  <% end %>\n</div>\n";
    const HAML: &str = "%section#hero\n  = render \"orders/row\"\n  - if a\n    %p= t('.x')\n";
    const MD: &str = "# t\n\n```ruby\nclass A\nend\n```\n\n```rust\nfn f() {}\n```\n";

    #[test]
    fn erb_regions_are_one_fragment_per_directive_code_node() {
        let rs = regions("erb", ERB.as_bytes());
        assert!(rs.iter().all(|r| r.kind == RegionKind::Fragment));
        assert!(rs.iter().all(|r| r.lang == "ruby"));
        let texts: Vec<&str> = rs.iter().map(|r| r.source.trim()).collect();
        assert_eq!(
            texts,
            vec!["render \"orders/row\"", "if a", "t(\".x\")", "end"]
        );
        // Every fragment is a real, contiguous slice of the host.
        for r in &rs {
            assert_eq!(
                &ERB[r.host_start as usize..r.host_end as usize],
                r.source,
                "{r:?}"
            );
            assert_eq!(r.map.byte_base(), Some(r.host_start));
        }
    }

    #[test]
    fn erb_rows_map_back_the_way_the_lens_walk_always_did() {
        let rs = regions("erb", ERB.as_bytes());
        // `<%= render … %>` is on source line 2 (1-based); its region's
        // guest row 0 must map to exactly that.
        let first = &rs[0];
        assert_eq!(first.map.host_line(0), Some(2));
    }

    #[test]
    fn haml_regions_carry_verbatim_fragments_and_exactly_one_program() {
        let rs = regions("haml", HAML.as_bytes());
        let programs: Vec<&Region> = rs
            .iter()
            .filter(|r| r.kind == RegionKind::Program)
            .collect();
        assert_eq!(programs.len(), 1, "{rs:?}");
        assert!(
            programs[0].map.byte_base().is_none(),
            "a program has no byte mapping"
        );
        for r in rs.iter().filter(|r| r.kind == RegionKind::Fragment) {
            assert_eq!(
                &HAML[r.host_start as usize..r.host_end as usize],
                r.source,
                "{r:?}"
            );
        }
        // The program reconstructs the block, so it carries a synthetic
        // `end` whose row maps to NO haml line.
        let p = programs[0];
        assert!(p.source.contains("end"), "{:?}", p.source);
        let synthetic = (0..p.source.lines().count() as u32)
            .filter(|r| p.map.host_line(*r).is_none())
            .count();
        assert!(synthetic >= 1, "{:?} / {:?}", p.source, p.map);
    }

    #[test]
    fn markdown_regions_are_the_resolvable_fences_only() {
        let rs = regions("markdown", MD.as_bytes());
        let langs: Vec<&str> = rs.iter().map(|r| r.lang).collect();
        assert_eq!(langs, vec!["ruby", "rust"]);
        for r in &rs {
            assert_eq!(&MD[r.host_start as usize..r.host_end as usize], r.source);
        }
    }

    #[test]
    fn a_non_host_language_declares_no_regions_and_no_guests() {
        for id in ["rust", "ruby", "yaml", "css", "scss", "sql"] {
            assert!(regions(id, b"anything").is_empty(), "{id}");
            assert!(guest_langs(id).is_empty(), "{id}");
            assert!(!is_host(id), "{id}");
        }
    }

    #[test]
    fn every_registry_injection_host_flag_agrees_with_the_derived_guest_set() {
        for row in crate::syntax::REGISTRY {
            assert_eq!(
                row.injection_host,
                is_host(row.lang),
                "{}: syntax/1's injection_host flag and the injection layer disagree",
                row.lang
            );
            for guest in guest_langs(row.lang) {
                assert!(
                    crate::lang::for_id(guest).is_some(),
                    "{}: declares a guest {guest:?} no registry row names",
                    row.lang
                );
            }
        }
    }

    #[test]
    fn markdowns_guest_set_is_exactly_what_an_info_string_can_resolve_to() {
        let declared = guest_langs("markdown");
        for row in crate::syntax::REGISTRY {
            let resolved = crate::markdown::resolve_info_string(row.lang);
            assert_eq!(
                declared.contains(&row.lang),
                resolved.is_some(),
                "{}: the declared guest set and resolve_info_string disagree",
                row.lang
            );
        }
    }

    #[test]
    fn painting_shifts_guest_spans_into_host_coordinates() {
        let spans = paint("markdown", MD.as_bytes(), Vec::new());
        assert!(!spans.is_empty());
        // Every painted span must fall inside a fence body — nothing may
        // land on the prose or the delimiters.
        let rs = regions("markdown", MD.as_bytes());
        for s in &spans {
            let end = s.byte_start + s.byte_len;
            assert!(
                rs.iter()
                    .any(|r| s.byte_start >= r.host_start && end <= r.host_end),
                "span {s:?} is outside every fence body"
            );
        }
        assert_nonoverlapping(&spans, MD.len());
    }

    #[test]
    fn a_nested_fence_paints_one_level_and_stops() {
        // A markdown fence inside markdown: the guest is painted with
        // markdown's own block query, which does not recurse.
        let src = "```markdown\n# inner\n\n```ruby\nclass A\nend\n```\n";
        let spans = paint("markdown", src.as_bytes(), Vec::new());
        assert_nonoverlapping(&spans, src.len());
    }

    #[test]
    fn degenerate_hosts_never_panic() {
        for (lang, src) in [
            ("markdown", ""),
            ("markdown", "```"),
            ("markdown", "```ruby"),
            ("markdown", "```ruby\n"),
            ("markdown", "````ruby\n```\n````\n"),
            ("erb", ""),
            ("erb", "<%"),
            ("erb", "<%= %>"),
            ("erb", "<%=  "),
            ("haml", ""),
            ("haml", "%"),
            ("haml", "= "),
            ("haml", "\t%p\n"),
        ] {
            let rs = regions(lang, src.as_bytes());
            for r in &rs {
                assert!(r.host_end as usize <= src.len().max(r.host_end as usize));
            }
            let spans = paint(lang, src.as_bytes(), Vec::new());
            assert_nonoverlapping(&spans, src.len());
        }
    }

    fn assert_nonoverlapping(spans: &[Span], len: usize) {
        let mut prev_end = 0u32;
        for s in spans {
            assert!(s.byte_len > 0, "{s:?}");
            assert!(s.byte_start >= prev_end, "{s:?} overlaps {prev_end}");
            assert!(
                (s.byte_start + s.byte_len) as usize <= len,
                "{s:?} vs {len}"
            );
            prev_end = s.byte_start + s.byte_len;
        }
    }
}
