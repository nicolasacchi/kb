//! Server-side syntax highlighting: tree-sitter's official bundled
//! `highlights.scm` (same sourcing rule as `extract.rs`'s `tags.scm` — the
//! grammar crate's own const, never a filesystem path) → a non-overlapping,
//! in-bounds list of `Span { byte_start, byte_len, class }`. This is DATA
//! for a future SPA renderer to turn into `<span class="...">` — no HTML
//! is produced here, and kb-code does not depend on the `tree-sitter-
//! highlight` crate (this is a deliberately smaller, self-contained
//! algorithm — see "Overlap resolution" below for what it does NOT do).
//!
//! ## Capture-name → class mapping
//!
//! `highlights.scm` capture names are dotted scopes (`function.method`,
//! `punctuation.bracket`, `variable.parameter`, ...). We bucket on the
//! TOP-LEVEL scope word — everything before the first `.` — into a small
//! fixed `HighlightClass` set (`map_class`). Observed top-level scopes
//! across all eight v1 languages' bundled `highlights.scm` files:
//! `attribute`, `boolean`, `comment`, `constant`, `constructor`, `embedded`,
//! `escape`, `function`, `keyword`, `label`, `number`, `operator`,
//! `property`, `punctuation`, `string`, `type`, `variable`. `constructor`
//! maps to `Function` (a constructor call reads like a function call);
//! `boolean` (new in W2.2, from YAML's own query — `true`/`false`/`~`
//! literals) maps to `Constant` rather than growing a 16th class (kb-code's
//! W2.2 scope keeps the SAME fixed 15-class bucketing every language maps
//! into, never a new `HighlightClass` variant); anything else unmapped
//! (`embedded` — an injection-content marker, not a real highlight) falls
//! into `Other` rather than being silently dropped.
//!
//! V72-H2a adds three grammars and one scope with them: CSS's `tag`
//! (`div`, `a`, `nesting_selector`, `universal_selector`) maps to `Type`,
//! the same bucket HAML's scanner already paints a tag name into.
//! Markdown's block query brings `text` (`text.title`/`.literal`/`.uri`/
//! `.reference`) and `none`, and SCSS's brings `spell`; all three stay
//! `Other`, deliberately — folding a heading, a URI and a spell-check
//! marker into one of the fifteen code classes would be a worse lie than
//! an honest "unclassified", and the fixed 15-class set is the contract
//! `kbc-theme/1` binds to.
//!
//! **An `Other` never OVERWRITES a real class for the same exact range**
//! (V72-H2a). SCSS's query captures its `//` comments twice
//! (`(js_comment) @comment @spell`), and under a plain last-write-wins
//! dedup the unmapped `@spell` would have won and every SCSS line comment
//! would have shipped as `Other`. The rule below is therefore "later
//! patterns are more specific, EXCEPT that an unclassifiable capture never
//! displaces a classified one" — see [`map_class`]'s call site.
//!
//! ## Encoding
//!
//! `store::Store::put_highlights` serializes `Vec<Span>` as JSON
//! (`serde_json`) into the `highlights.spans` `BLOB` column — the simpler
//! of the two options the design brief allowed ("a compact binary or JSON
//! encoding"). The column is opaque, so swapping to a tighter binary
//! encoding later needs no schema migration.
//!
//! ## Overlap resolution
//!
//! Tree-sitter highlight captures are almost always leaf tokens
//! (identifiers, string literals, keyword tokens, ...) that don't nest, but
//! the SAME node can be captured by more than one pattern (a general
//! `@variable` rule and a more specific `@variable.parameter` rule
//! matching identically). We keep the LAST class seen for an EXACT
//! `(start, end)` duplicate (later patterns in `highlights.scm` are
//! conventionally more specific), then run one left-to-right greedy sweep:
//! spans are processed in `(start, end)` order, an earlier span wins
//! outright, and any later span starting before the cursor is clipped to
//! start where the previous span ended (or dropped if that empties it).
//! This guarantees a non-overlapping, in-bounds result; it is NOT full
//! nested-highlight priority stacking (`tree-sitter-highlight`'s job,
//! which this Wave doesn't need).
//!
//! ## Injections (V72-H2a)
//!
//! [`extract_highlights`] also paints every guest region
//! `crate::injection` declares for the file's language — a Markdown
//! fence's Ruby, painted with the Ruby query and shifted into Markdown
//! coordinates. [`extract_highlights_host_only`] is the same pass WITHOUT
//! that step, and is what the injection layer itself calls, which is what
//! bounds painting at exactly one level.

use crate::lang::{self, LangError};
use std::collections::BTreeMap;
use tree_sitter::StreamingIterator;

pub type Result<T> = std::result::Result<T, LangError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HighlightClass {
    Keyword,
    String,
    Comment,
    Function,
    Type,
    Number,
    Variable,
    Constant,
    Operator,
    Punctuation,
    Property,
    Attribute,
    Label,
    Escape,
    /// Any `highlights.scm` top-level scope not in the fixed set above
    /// (e.g. `embedded`) — kept rather than silently dropped.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Span {
    pub byte_start: u32,
    pub byte_len: u32,
    pub class: HighlightClass,
}

/// Highlight spans for `source`, INCLUDING every injected guest region
/// (V72-H2a): a Markdown fence's Ruby is painted with the Ruby query and
/// re-anchored into Markdown coordinates by `crate::injection`.
///
/// This is the entry point every caller wants. [`extract_highlights_host_only`]
/// is the one the injection layer itself calls, and the reason nothing
/// recurses — see `injection`'s module doc.
pub fn extract_highlights(lang_id: &str, source: &[u8]) -> Result<Vec<Span>> {
    let host = extract_highlights_host_only(lang_id, source)?;
    // HAML's own scanner already paints its Ruby fragments through the
    // SAME injection layer (it has the scanned document in hand and would
    // otherwise re-walk it), so its host-only result is already complete.
    if lang_id == "haml" || !crate::injection::is_host(lang_id) {
        return Ok(host);
    }
    Ok(crate::injection::paint(lang_id, source, host))
}

/// The HOST language's own spans, with no injected guest regions.
///
/// Structurally never reaches `crate::injection`, which is what bounds
/// injection painting at exactly one level (`injection`'s module doc). A
/// caller outside that module almost certainly wants
/// [`extract_highlights`].
pub fn extract_highlights_host_only(lang_id: &str, source: &[u8]) -> Result<Vec<Span>> {
    // PRR-N3 — ERB has no `highlights.scm` vendored in this crate (see
    // `lang::ERB`'s doc; syntax highlighting for `.erb` is out of this
    // lens's scope). Short-circuit the same way `extract::extract_symbols`
    // does, for the same reason: `ingest::index_file` calls this
    // unconditionally for every detected language, and an `Unsupported`
    // error here would abort the whole repo walk on the first `.erb` file.
    // V72-H2a note: ERB is an injection HOST, so the layer could paint its
    // Ruby fragments — but it would be painting them onto nothing, since
    // there is no `erb` highlights query for the template itself, and the
    // row's tier is `none`, so `ingest::index_file`'s plan never asks for
    // spans at all. Widening ERB's tier is D7's Herb decision, not this
    // unit's; the short-circuit stays.
    if lang_id == "erb" {
        return Ok(Vec::new());
    }
    // V72-H3 (D7) — HAML's spans come from `crate::haml`'s own scanner
    // (template tokens) PLUS this module's Ruby query run over every Ruby
    // fragment the scanner found, shifted into HAML coordinates. There is
    // no `haml` grammar and no `haml-highlights.scm`; the tier's highlight
    // promise is backed by `syntax/1`'s `Engine::Scanner` instead.
    if lang_id == "haml" {
        return Ok(crate::haml::highlights(source));
    }
    let (tree, language) = lang::parse(lang_id, source)?;
    let hl_src = lang::highlights_query(lang_id)
        .ok_or_else(|| LangError::Unsupported(lang_id.to_string()))?;
    let query = lang::compile_query(lang_id, &language, &hl_src)?;
    let capture_names = query.capture_names();

    let mut cursor = tree_sitter::QueryCursor::new();
    let mut captures = cursor.captures(&query, tree.root_node(), source);

    // Dedup exact-range duplicates: last-write-wins (BTreeMap::insert
    // overwrites), and BTreeMap's key ordering (start ASC, end ASC) is
    // exactly the order the sweep below needs — no separate sort.
    let mut by_range: BTreeMap<(usize, usize), HighlightClass> = BTreeMap::new();
    while let Some((m, capture_index)) = captures.next() {
        let cap = m.captures[*capture_index];
        let cname = capture_names[cap.index as usize];
        let scope = cname.split('.').next().unwrap_or(cname);
        let class = map_class(scope);
        // Last-write-wins, EXCEPT that an unclassifiable capture never
        // displaces a classified one — see the module doc's `@spell`
        // paragraph for the case that made this necessary.
        by_range
            .entry((cap.node.start_byte(), cap.node.end_byte()))
            .and_modify(|slot| {
                if class != HighlightClass::Other {
                    *slot = class;
                }
            })
            .or_insert(class);
    }

    let mut spans = Vec::with_capacity(by_range.len());
    let mut sweep_pos: usize = 0;
    for ((start, end), class) in by_range {
        let clipped_start = start.max(sweep_pos);
        if clipped_start >= end {
            continue; // fully swallowed by a previously emitted span
        }
        spans.push(Span {
            byte_start: clipped_start as u32,
            byte_len: (end - clipped_start) as u32,
            class,
        });
        sweep_pos = end;
    }
    Ok(spans)
}

fn map_class(scope: &str) -> HighlightClass {
    match scope {
        "keyword" => HighlightClass::Keyword,
        "string" => HighlightClass::String,
        "comment" => HighlightClass::Comment,
        "function" | "constructor" => HighlightClass::Function,
        // V72-H2a — CSS's `tag` is an element selector (`div`, `a`, `*`,
        // `&`). It reads as a type name, which is also where HAML's
        // scanner already paints a tag name.
        "type" | "tag" => HighlightClass::Type,
        "number" => HighlightClass::Number,
        "variable" => HighlightClass::Variable,
        // `boolean` (YAML's `true`/`false`/`~` literals — W2.2) folds into
        // the same bucket as other literal constants rather than growing a
        // 16th class — see the module doc.
        "constant" | "boolean" => HighlightClass::Constant,
        "operator" => HighlightClass::Operator,
        "punctuation" => HighlightClass::Punctuation,
        "property" => HighlightClass::Property,
        "attribute" => HighlightClass::Attribute,
        "label" => HighlightClass::Label,
        "escape" => HighlightClass::Escape,
        _ => HighlightClass::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUST_SNIPPET: &str = "fn add(a: i32, b: i32) -> i32 {\n    // sum\n    a + b\n}\n";
    const PYTHON_SNIPPET: &str = "def greet(name):\n    # say hi\n    return f\"hi {name}\"\n";
    const RUBY_SNIPPET: &str = "def greet(name)\n  # say hi\n  \"hi #{name}\"\nend\n";
    const TYPESCRIPT_SNIPPET: &str =
        "function add(a: number, b: number): number {\n    // sum\n    return a + b;\n}\n";
    const TSX_SNIPPET: &str =
        "function Hello(props: { name: string }) {\n    // a component\n    return <div>{props.name}</div>;\n}\n";
    const JAVASCRIPT_SNIPPET: &str = "function add(a, b) {\n    // sum\n    return a + b;\n}\n";
    const BASH_SNIPPET: &str = "greet() {\n  # say hi\n  echo \"hi $1\"\n}\n";
    const YAML_SNIPPET: &str = "# a comment\nname: web\nreplicas: 3\nenabled: true\n";
    const GO_SNIPPET: &str =
        "// add sums two ints\nfunc add(a int, b int) int {\n\treturn a + b\n}\n";
    const TOML_SNIPPET: &str = "# a comment\n[package]\nname = \"kb-code\"\nversion = 1\n";
    const JSON_SNIPPET: &str = "{\n  \"name\": \"kb-code\",\n  \"count\": 3\n}\n";
    const CSS_SNIPPET: &str = "/* tokens */\n.btn {\n  --radius: 4px;\n  color: #336699;\n}\n";
    const SCSS_SNIPPET: &str =
        "$brand: #336699;\n@mixin b($s) {\n  padding: $s;\n}\n.card { color: $brand; }\n";
    const MARKDOWN_SNIPPET: &str = "# Title\n\nSome prose.\n\n```ruby\nclass A\nend\n```\n";

    #[test]
    fn rust_small_fixture_highlight_spans_snapshot() {
        let spans = extract_highlights("rust", RUST_SNIPPET.as_bytes()).unwrap();
        insta::assert_debug_snapshot!(spans);
    }

    #[test]
    fn go_small_fixture_highlight_spans_snapshot() {
        let spans = extract_highlights("go", GO_SNIPPET.as_bytes()).unwrap();
        insta::assert_debug_snapshot!(spans);
    }

    // ── V72-H2a — the three new grammars, plus the injection lane ────────

    #[test]
    fn css_small_fixture_highlight_spans_snapshot() {
        let spans = extract_highlights("css", CSS_SNIPPET.as_bytes()).unwrap();
        insta::assert_debug_snapshot!(spans);
    }

    #[test]
    fn scss_small_fixture_highlight_spans_snapshot() {
        let spans = extract_highlights("scss", SCSS_SNIPPET.as_bytes()).unwrap();
        insta::assert_debug_snapshot!(spans);
    }

    /// The Markdown fixture's own block spans PLUS the Ruby inside its
    /// fence, re-anchored into Markdown coordinates by `crate::injection`.
    /// The golden is the proof the offsets are the fence body's, not the
    /// guest's own.
    #[test]
    fn markdown_small_fixture_with_an_injected_fence_snapshot() {
        let spans = extract_highlights("markdown", MARKDOWN_SNIPPET.as_bytes()).unwrap();
        insta::assert_debug_snapshot!(spans);
    }

    /// The host-only entry point is what the injection layer calls, and
    /// the reason nothing recurses. For a host it is strictly a SUBSET of
    /// the full result; for every other language the two are identical.
    #[test]
    fn host_only_and_full_differ_exactly_by_the_injected_regions() {
        let host_only =
            extract_highlights_host_only("markdown", MARKDOWN_SNIPPET.as_bytes()).unwrap();
        let full = extract_highlights("markdown", MARKDOWN_SNIPPET.as_bytes()).unwrap();
        assert!(
            full.len() > host_only.len(),
            "the fence must contribute spans: {} vs {}",
            full.len(),
            host_only.len()
        );
        for lang in ["rust", "ruby", "css", "scss", "yaml"] {
            let src = match lang {
                "rust" => RUST_SNIPPET,
                "ruby" => RUBY_SNIPPET,
                "css" => CSS_SNIPPET,
                "scss" => SCSS_SNIPPET,
                _ => YAML_SNIPPET,
            };
            assert_eq!(
                extract_highlights(lang, src.as_bytes()).unwrap(),
                extract_highlights_host_only(lang, src.as_bytes()).unwrap(),
                "{lang}: a non-host language must be unaffected by the injection layer"
            );
        }
    }

    /// HAML paints its Ruby through the same layer but from its own
    /// scanner entry point (it has the document in hand); the full and
    /// host-only results must therefore be IDENTICAL, or the layer would
    /// be painting it twice.
    /// The dedup rule V72-H2a added: SCSS captures its `//` comments as
    /// `@comment @spell`, and the unmapped one must not win.
    #[test]
    fn an_unclassifiable_capture_never_displaces_a_classified_one() {
        let spans = extract_highlights("scss", b"// a line comment\n$a: 1;\n").unwrap();
        let first = spans.first().expect("the comment produces a span");
        assert_eq!(
            first.class,
            HighlightClass::Comment,
            "an SCSS line comment must stay a Comment: {spans:?}"
        );
    }

    #[test]
    fn haml_is_not_painted_twice() {
        let src = b"%section#hero\n  %p= t('.title')\n";
        assert_eq!(
            extract_highlights("haml", src).unwrap(),
            extract_highlights_host_only("haml", src).unwrap()
        );
    }

    fn assert_nonoverlapping_and_inbounds(spans: &[Span], source_len: usize) {
        let mut prev_end: u32 = 0;
        for (i, s) in spans.iter().enumerate() {
            assert!(s.byte_len > 0, "span {i} has zero length: {s:?}");
            let end = s.byte_start + s.byte_len;
            assert!(
                (end as usize) <= source_len,
                "span {i} out of bounds: {s:?} vs source_len={source_len}"
            );
            assert!(
                s.byte_start >= prev_end,
                "span {i} overlaps the previous span: {s:?}, prev_end={prev_end}"
            );
            prev_end = end;
        }
    }

    #[test]
    fn highlight_spans_are_nonoverlapping_and_inbounds() {
        for (lang, src) in [
            ("rust", RUST_SNIPPET),
            ("python", PYTHON_SNIPPET),
            ("ruby", RUBY_SNIPPET),
            ("typescript", TYPESCRIPT_SNIPPET),
            ("tsx", TSX_SNIPPET),
            ("javascript", JAVASCRIPT_SNIPPET),
            ("bash", BASH_SNIPPET),
            ("yaml", YAML_SNIPPET),
            ("go", GO_SNIPPET),
            ("toml", TOML_SNIPPET),
            ("json", JSON_SNIPPET),
            ("css", CSS_SNIPPET),
            ("scss", SCSS_SNIPPET),
            ("markdown", MARKDOWN_SNIPPET),
        ] {
            let spans = extract_highlights(lang, src.as_bytes()).unwrap();
            assert!(!spans.is_empty(), "{lang} produced no spans at all");
            assert_nonoverlapping_and_inbounds(&spans, src.len());
        }
    }

    #[test]
    fn empty_source_yields_no_spans() {
        for lang in lang::ALL_LANG_IDS {
            assert_eq!(extract_highlights(lang, b"").unwrap(), vec![]);
        }
    }

    #[test]
    fn unsupported_language_errors() {
        let err = extract_highlights("cobol", b"").unwrap_err();
        assert!(matches!(err, LangError::Unsupported(_)), "got: {err:?}");
    }

    #[test]
    fn map_class_covers_every_observed_top_level_scope() {
        // Every top-level scope word actually present in the eight bundled
        // highlights.scm files (grepped at authoring time — TypeScript's is
        // concatenated with JavaScript's, see lang.rs) must map to
        // something other than the catch-all `Other`, EXCEPT `embedded`
        // (an injection marker, not a real highlight — see module doc).
        let observed = [
            "attribute",
            "boolean",
            "comment",
            "constant",
            "constructor",
            "escape",
            "function",
            "keyword",
            "label",
            "number",
            "operator",
            "property",
            "punctuation",
            "string",
            "type",
            "variable",
        ];
        for scope in observed {
            assert_ne!(
                map_class(scope),
                HighlightClass::Other,
                "scope {scope:?} unexpectedly fell through to Other"
            );
        }
        // V72-H2a — the three new grammars' own queries add two more
        // top-level scopes, both of which must land somewhere real.
        // V72-H2a — CSS's element-selector scope joins the mapped set.
        assert_eq!(map_class("tag"), HighlightClass::Type);
        // Markdown's and SCSS's remaining scopes stay `Other` on purpose:
        // a heading, a URI and a spell-check marker have no honest home in
        // the FIXED 15-class set. Pinned so the choice is a decision on
        // record rather than an oversight.
        for scope in ["text", "none", "spell"] {
            assert_eq!(
                map_class(scope),
                HighlightClass::Other,
                "scope {scope:?} is deliberately unclassified — see the module doc"
            );
        }
        assert_eq!(map_class("embedded"), HighlightClass::Other);
        assert_eq!(map_class("something-unheard-of"), HighlightClass::Other);
    }
}
