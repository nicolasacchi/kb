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
//! `punctuation.bracket`, `variable.parameter`, ...). `map_class` buckets
//! them into the fixed [`HighlightClass`] set, and the lookup is TWO
//! levels deep (V72-H2b): the first two scope words joined by a `.` are
//! tried first, then the top-level word alone. That is the whole widening
//! mechanism — a sub-scope earns a class only by being listed, and
//! everything else keeps falling back to exactly the bucket it had before.
//! `comment.documentation` still reads as `Comment`, `string.escape` still
//! as `String`.
//!
//! Observed top-level scopes across the bundled `highlights.scm` files:
//! `attribute`, `boolean`, `comment`, `constant`, `constructor`,
//! `embedded`, `escape`, `function`, `keyword`, `label`, `number`,
//! `operator`, `property`, `punctuation`, `string`, `tag`, `text`, `type`,
//! `variable`, plus SCSS's `spell` and Markdown's `none`. `constructor`
//! maps to `Function` (a constructor call reads like a function call);
//! CSS's `tag` (`div`, `a`, `nesting_selector`, `universal_selector`) maps
//! to `Type`, the same bucket HAML's scanner already paints a tag name
//! into; Markdown's `text`/`none` and SCSS's `spell` stay `Other`
//! deliberately — folding a heading, a URI and a spell-check marker into a
//! CODE class would be a worse lie than an honest "unclassified".
//! `embedded` (an injection-content marker, not a real highlight) is
//! `Other` for the same reason. Anything unmapped falls into `Other`
//! rather than being silently dropped.
//!
//! ## The role table (V72-H2b, D16)
//!
//! The class set IS the `kbc-theme/1` contract — every member is bound to
//! a `--syn-*` CSS variable in `web-code/src/styles/tokens.css`, derived
//! per theme by `web-code/src/themes/derive.ts`, and rendered as
//! `.kbc-hl-<role>` (`web-code/src/lib/decorations.ts`). D16 widens it
//! from fifteen to EIGHTEEN, and the three new members were picked by
//! counting the capture names the queries in this build actually emit —
//! not by wish. Coverage, over the fourteen `highlights.scm` sources
//! `lang::highlights_query` returns (TypeScript and TSX both concatenate
//! JavaScript's):
//!
//! | new class            | capture names                                                              | grammars |
//! |----------------------|----------------------------------------------------------------------------|----------|
//! | `ConstantBuiltin`    | `constant.builtin`, plus YAML/TOML's top-level `boolean`                    | 9        |
//! | `PunctuationSpecial` | `punctuation.special`                                                       | 7        |
//! | `StringSpecial`      | `string.special`, `string.special.regex`, `.symbol`, `.key`                 | 7        |
//!
//! `boolean` moves with them: `true`/`false`/`~`/`null` ARE builtin
//! constants, and painting them as user constants was the closest honest
//! bucket only while `ConstantBuiltin` did not exist.
//!
//! MEASURED AND NOT ADOPTED, so the choice is on record rather than an
//! oversight (the `usages2::UNMINTED_KINDS` precedent, pinned by
//! `deferred_role_candidates_are_recorded_with_their_evidence`):
//! `constructor` (6 grammars), `variable.parameter` (5 — JavaScript emits
//! it only from `highlights-params.scm`, which `HIGHLIGHT_QUERY` does not
//! include), `type.builtin` (3), `namespace` (**0** — no grammar in this
//! build emits it AT ALL; CSS has an `@namespace` AT-RULE, which is a
//! keyword in the query's pattern text and not a capture name, and reading
//! one for the other is exactly the mistake a COUNTED table exists to
//! catch), and Markdown's `text.*` family
//! (1 grammar, 4 captures — one bucket for headings, links, literals and
//! references would be a fold, and four would spend every remaining slot
//! on one file type). D16's budget is "~18"; these are what 21 would have
//! been.
//!
//! Widening this set bumps [`ROLE_TABLE_VERSION`], which every language's
//! `lang::LangInfo::highlight_salt` embeds — so the whole corpus re-paints
//! exactly once and no symbol row is touched. Price it first:
//! `kb-code reextract --bill`.
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

/// The ROLE TABLE version, embedded in every
/// `lang::LangInfo::highlight_salt` (pinned by
/// `lang::tests::the_two_salt_families_are_disjoint_and_role_versioned`).
///
/// `1` = the fifteen classes shipped from W1 through v7.1. `2` = V72-H2b's
/// eighteen. Bump it in the SAME edit that adds, removes or re-buckets a
/// [`HighlightClass`], and bump every `highlight_salt` with it: cached
/// spans carry class values, so a re-bucketing that leaves the salts alone
/// serves rows painted under the old vocabulary forever.
pub const ROLE_TABLE_VERSION: u32 = 2;

/// Every role, in wire order — the vocabulary `kbc-theme/1` binds to.
/// Declared as strings beside the enum (rather than derived from it) for
/// exactly one reason: it is the list the SPA mirrors
/// (`web-code/src/api/types.ts`, `themes/derive.ts`'s `SYNTAX_ROLES`,
/// `styles/tokens.css`'s `--syn-*`), and a lock-step contract needs a
/// literal on each side. `roles_match_the_serialized_class_names` pins it
/// to the enum's own serde output, so the two can never drift.
pub const ROLES: &[&str] = &[
    "keyword",
    "string",
    "string-special",
    "comment",
    "function",
    "type",
    "number",
    "variable",
    "constant",
    "constant-builtin",
    "operator",
    "punctuation",
    "punctuation-special",
    "property",
    "attribute",
    "label",
    "escape",
    "other",
];

/// The eighteen highlight roles. `kebab-case` on the wire: every one of
/// the fifteen pre-V72-H2b members is a single word and serializes
/// byte-identically under `kebab-case` and `snake_case`
/// (`the_fifteen_legacy_roles_serialize_byte_identically` pins that), and
/// the three new members read better as `constant-builtin` than
/// `constant_builtin` in a CSS class and a custom property.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HighlightClass {
    Keyword,
    String,
    /// V72-H2b — `string.special` and its sub-scopes: a Ruby symbol or
    /// regex literal, a JS template/regex, a JSON object KEY, a CSS/TOML
    /// url. Not a plain string, and reading a JSON file where the keys and
    /// the values are one colour is the case that made this a role.
    StringSpecial,
    Comment,
    Function,
    Type,
    Number,
    Variable,
    Constant,
    /// V72-H2b — `constant.builtin` (`nil`, `None`, `true`, `self`,
    /// `null`) plus YAML/TOML's top-level `boolean`. The language owns
    /// these; a user constant is a different thing.
    ConstantBuiltin,
    Operator,
    Punctuation,
    /// V72-H2b — `punctuation.special`: interpolation delimiters (`#{`,
    /// `${`), YAML's directive/document markers, Markdown's block markers.
    PunctuationSpecial,
    Property,
    Attribute,
    Label,
    Escape,
    /// Any `highlights.scm` scope not in the fixed set above (e.g.
    /// `embedded`, Markdown's `text.*`) — kept rather than silently
    /// dropped.
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
        // V72-H2b: the FULL capture name, not its top-level word —
        // `map_class` owns the two-level lookup so there is exactly one
        // place that decides how specific a scope is allowed to be.
        let class = map_class(cname);
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

/// The first TWO dotted scope words of `cname`, and the first alone —
/// `map_class`'s two lookup keys, in priority order. Pure and total: a
/// capture with no dot yields the same string twice, and the specific
/// lookup simply misses.
fn scope_keys(cname: &str) -> (&str, &str) {
    let top = cname.split('.').next().unwrap_or(cname);
    let two = match cname[top.len()..].strip_prefix('.') {
        Some(rest) => {
            let second = rest.split('.').next().unwrap_or(rest);
            &cname[..top.len() + 1 + second.len()]
        }
        None => top,
    };
    (two, top)
}

/// Bucket a capture name into a role. V72-H2b: the SPECIFIC (two-word)
/// scope wins when it is listed, else the top-level word decides exactly as
/// it did before — so every unlisted sub-scope keeps its historical class.
fn map_class(cname: &str) -> HighlightClass {
    let (specific, top) = scope_keys(cname);
    match specific {
        // V72-H2b (D16) — the three widened roles. Every other dotted
        // scope falls through to the top-level match below.
        "constant.builtin" => return HighlightClass::ConstantBuiltin,
        "punctuation.special" => return HighlightClass::PunctuationSpecial,
        "string.special" => return HighlightClass::StringSpecial,
        _ => {}
    }
    match top {
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
        "constant" => HighlightClass::Constant,
        // V72-H2b — YAML/TOML's `boolean` (`true`/`false`/`~`/`null`) is a
        // BUILTIN constant; it rode `Constant` only for as long as there
        // was no honest bucket for it. See the module doc.
        "boolean" => HighlightClass::ConstantBuiltin,
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

    // ── V72-H2b (D16) — the eighteen-role table ──────────────────────────

    /// Every capture name the queries in THIS BUILD emit, per language id.
    /// The evidence the widening was picked from, read from the grammars
    /// rather than restated from a note.
    fn capture_names_for(lang_id: &str) -> Vec<String> {
        let src = lang::highlights_query(lang_id).expect("a highlights query");
        // `lang::parse` is the public door to the compiled grammar (it
        // hands back the `Language` beside the tree); parsing empty bytes
        // is the cheapest way through it.
        let (_tree, language) = lang::parse(lang_id, b"").expect("a grammar");
        let query = lang::compile_query(lang_id, &language, &src).expect("compiles");
        query
            .capture_names()
            .iter()
            .map(|n| (*n).to_string())
            .collect()
    }

    /// How many of this build's grammars emit a capture whose two-level
    /// scope key is `key`.
    fn grammars_emitting(key: &str) -> usize {
        lang::ALL_LANG_IDS
            .iter()
            .filter(|id| capture_names_for(id).iter().any(|n| scope_keys(n).0 == key))
            .count()
    }

    /// The three roles D16 adopted, and the exact coverage that picked
    /// them. A grammar bump that moves one of these numbers is precisely
    /// when the choice deserves re-reading — so the numbers are asserted,
    /// not narrated.
    #[test]
    fn every_widened_role_is_reachable_from_the_queries_in_this_build() {
        let adopted = [
            ("constant.builtin", 9usize, HighlightClass::ConstantBuiltin),
            ("punctuation.special", 7, HighlightClass::PunctuationSpecial),
            ("string.special", 7, HighlightClass::StringSpecial),
        ];
        for (key, expected, class) in adopted {
            let n = grammars_emitting(key);
            assert_eq!(
                n, expected,
                "{key}: {n} grammar(s) emit it, the module doc's table says {expected} —                  update the table (and re-read the choice) in the same edit"
            );
            assert_eq!(map_class(key), class);
        }
    }

    /// MEASURED AND NOT ADOPTED — the `usages2::UNMINTED_KINDS` precedent.
    /// Each candidate keeps its historical class, and its coverage is
    /// pinned so "we looked" stays true rather than becoming folklore.
    #[test]
    fn deferred_role_candidates_are_recorded_with_their_evidence() {
        let deferred = [
            ("constructor", 6usize, HighlightClass::Function),
            ("variable.parameter", 5, HighlightClass::Variable),
            ("type.builtin", 3, HighlightClass::Type),
            ("namespace", 0, HighlightClass::Other),
        ];
        for (key, expected, folds_into) in deferred {
            let n = grammars_emitting(key);
            assert_eq!(
                n, expected,
                "{key}: {n} grammar(s) emit it, the module doc's table says {expected}"
            );
            assert_eq!(
                map_class(key),
                folds_into,
                "{key} is deferred — it must keep folding into its historical class"
            );
        }
        // Markdown's `text.*` family: one grammar, four captures.
        let md = capture_names_for("markdown");
        assert_eq!(
            md.iter().filter(|n| scope_keys(n).1 == "text").count(),
            4,
            "markdown's text.* family: {md:?}"
        );
        for n in &md {
            if scope_keys(n).1 == "text" {
                assert_eq!(map_class(n), HighlightClass::Other);
            }
        }
    }

    /// [`ROLES`] is what the SPA mirrors; the enum is what the wire
    /// carries. They are two literals, so they get one test.
    #[test]
    fn roles_match_the_serialized_class_names() {
        let all = [
            HighlightClass::Keyword,
            HighlightClass::String,
            HighlightClass::StringSpecial,
            HighlightClass::Comment,
            HighlightClass::Function,
            HighlightClass::Type,
            HighlightClass::Number,
            HighlightClass::Variable,
            HighlightClass::Constant,
            HighlightClass::ConstantBuiltin,
            HighlightClass::Operator,
            HighlightClass::Punctuation,
            HighlightClass::PunctuationSpecial,
            HighlightClass::Property,
            HighlightClass::Attribute,
            HighlightClass::Label,
            HighlightClass::Escape,
            HighlightClass::Other,
        ];
        let wire: Vec<String> = all
            .iter()
            .map(|c| {
                serde_json::to_value(c)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(wire, ROLES, "ROLES must be the serde names, in wire order");
        assert_eq!(ROLES.len(), 18, "D16's budget is eighteen roles");
        assert_eq!(ROLE_TABLE_VERSION, 2);
    }

    /// The `snake_case` → `kebab-case` switch is byte-identical for every
    /// role that existed before V72-H2b — the reason it was safe to make.
    #[test]
    fn the_fifteen_legacy_roles_serialize_byte_identically() {
        for (class, legacy) in [
            (HighlightClass::Keyword, "keyword"),
            (HighlightClass::String, "string"),
            (HighlightClass::Comment, "comment"),
            (HighlightClass::Function, "function"),
            (HighlightClass::Type, "type"),
            (HighlightClass::Number, "number"),
            (HighlightClass::Variable, "variable"),
            (HighlightClass::Constant, "constant"),
            (HighlightClass::Operator, "operator"),
            (HighlightClass::Punctuation, "punctuation"),
            (HighlightClass::Property, "property"),
            (HighlightClass::Attribute, "attribute"),
            (HighlightClass::Label, "label"),
            (HighlightClass::Escape, "escape"),
            (HighlightClass::Other, "other"),
        ] {
            assert_eq!(
                serde_json::to_value(class).unwrap(),
                serde_json::json!(legacy)
            );
        }
    }

    /// The two-level lookup promotes ONLY what is listed; every other
    /// dotted scope keeps the class its top-level word always gave it.
    #[test]
    fn the_two_level_lookup_only_promotes_listed_sub_scopes() {
        assert_eq!(
            scope_keys("string.special.regex"),
            ("string.special", "string")
        );
        assert_eq!(scope_keys("comment"), ("comment", "comment"));
        assert_eq!(
            scope_keys("function.method.builtin"),
            ("function.method", "function")
        );
        for (cname, class) in [
            ("comment.documentation", HighlightClass::Comment),
            ("string.escape", HighlightClass::String),
            ("function.macro", HighlightClass::Function),
            ("function.method.builtin", HighlightClass::Function),
            ("punctuation.bracket", HighlightClass::Punctuation),
            ("punctuation.delimiter", HighlightClass::Punctuation),
            ("variable.builtin", HighlightClass::Variable),
            ("keyword.return", HighlightClass::Keyword),
            ("text.title", HighlightClass::Other),
        ] {
            assert_eq!(map_class(cname), class, "{cname}");
        }
        // The promoted three, at every depth they actually occur.
        for cname in [
            "string.special",
            "string.special.key",
            "string.special.symbol",
        ] {
            assert_eq!(map_class(cname), HighlightClass::StringSpecial, "{cname}");
        }
        assert_eq!(
            map_class("constant.builtin"),
            HighlightClass::ConstantBuiltin
        );
        assert_eq!(map_class("boolean"), HighlightClass::ConstantBuiltin);
        assert_eq!(map_class("constant"), HighlightClass::Constant);
        assert_eq!(
            map_class("punctuation.special"),
            HighlightClass::PunctuationSpecial
        );
    }
}
