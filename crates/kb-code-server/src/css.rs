//! CSS/SCSS **stylesheet outline** — V72-H2a, design D7.
//!
//! Neither grammar ships a `tags.scm`, and neither language has a
//! function/class vocabulary to tag in the first place: a stylesheet is a
//! tree of SELECTORS, and (in SCSS) a handful of real definitions —
//! `@mixin`, `@function`, `$variable`, `%placeholder`. So CSS and SCSS
//! join `yaml`/`toml`/`json` in `extract::CST_OUTLINES`: a direct CST walk
//! that mints `extract::Symbol` rows, not a query path. `extract::
//! extract_symbols` dispatches both ids here.
//!
//! # The kind vocabulary
//!
//! Six kinds, and nothing else is a row:
//!
//! | kind | minted from | `name` |
//! |------|-------------|--------|
//! | `rule` | a `rule_set` | its selector list, whitespace-collapsed |
//! | `placeholder` | a `rule_set` whose whole selector list is one SCSS `%name` | `%name` |
//! | `at_rule` | `@media`/`@supports`/`@use`/`@import`/`@forward`/`@charset`/`@namespace`/`@at-root` and any other `at_rule` | the at-rule's HEADER text (`@media (min-width: 40rem)`) |
//! | `keyframes` | `@keyframes` | the animation name |
//! | `mixin` | SCSS `@mixin` | the mixin name |
//! | `function` | SCSS `@function` | the function name |
//! | `variable` | an SCSS `$name:` declaration, or a CSS custom property `--name:` | `$name` / `--name` |
//!
//! An ORDINARY declaration (`color: red`) is deliberately NOT a row: a
//! real stylesheet has thousands of them and an outline that lists every
//! one is a re-print of the file, not a map of it. SCSS control flow
//! (`@if`/`@each`/`@for`/`@while`/`@include` with a block) is not a row
//! either — it defines nothing — but the walk DESCENDS through it, so a
//! rule nested inside an `@each` still appears.
//!
//! `detail` (surfaced as the `outline/1` row's `detail`, and as the
//! symbol's `signature`) carries the full header for the rows whose `name`
//! is only a fragment of it: `@mixin button($size: md)` for a `mixin`
//! named `button`. A `rule` row's name IS its header, so it carries none.
//!
//! # Nesting
//!
//! `container` names the nearest enclosing row, exactly as every other
//! extractor here does. SCSS nests rule sets natively and CSS has done so
//! since nesting shipped, so this is real structure, not an invention —
//! and `outline/1` re-derives the tree from RANGE CONTAINMENT anyway (see
//! `crate::outline`), which is why the flat `container` string being the
//! nearest ancestor is enough.
//!
//! # Caps
//!
//! [`MAX_DEPTH`] (8) and [`MAX_ROWS`] (500) mirror `yaml.rs`'s, for the
//! same reason: a generated stylesheet is the pathological input, and
//! `capped` reports the cut honestly rather than silently truncating.
//!
//! # What this does NOT claim
//!
//! No occurrences, no locals, no `exact` anything. A selector is not a
//! definition anything resolves TO — `@extend %btn` and `@include button`
//! are real references and this module deliberately mints no edge for
//! them (that would be a lens, with a trust class to justify). Both ids
//! are in `extract::OUTLINE_ONLY_KINDS`' spirit via their kinds, so these
//! rows never reach a repo map.

use crate::extract::Symbol;
use crate::lang::{self, LangError};
use tree_sitter::Node;

pub type Result<T> = std::result::Result<T, LangError>;

/// Deepest nesting the walk descends to. A row at the limit is still
/// emitted; its children are not.
pub const MAX_DEPTH: usize = 8;
/// Row cap for one file. `capped` reports the cut.
pub const MAX_ROWS: usize = 500;
/// `name`/`detail` cap, in chars, post whitespace-collapse.
pub const NAME_CAP: usize = 120;

pub const KIND_RULE: &str = "rule";
pub const KIND_PLACEHOLDER: &str = "placeholder";
pub const KIND_AT_RULE: &str = "at_rule";
pub const KIND_KEYFRAMES: &str = "keyframes";
pub const KIND_MIXIN: &str = "mixin";
pub const KIND_FUNCTION: &str = "function";
pub const KIND_VARIABLE: &str = "variable";

/// Every kind this module can mint, in declaration order — the vocabulary
/// the module doc's table documents, as data, so `crate::outline`'s
/// contract test can walk it instead of re-listing the names.
pub const KINDS: &[&str] = &[
    KIND_RULE,
    KIND_PLACEHOLDER,
    KIND_AT_RULE,
    KIND_KEYFRAMES,
    KIND_MIXIN,
    KIND_FUNCTION,
    KIND_VARIABLE,
];

/// One [`outline`] call's result. `capped` is stylesheet-specific and is
/// not threaded through `extract::extract_symbols`' uniform `Vec<Symbol>`
/// return — the same shape `yaml::YamlOutline` uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyleOutline {
    pub symbols: Vec<Symbol>,
    pub capped: bool,
}

/// Walk `source` (parsed with `lang_id`'s grammar — `"css"` or `"scss"`)
/// into a stylesheet outline.
///
/// Never errors on malformed CSS: tree-sitter always returns SOME tree,
/// error nodes and all. The only `Err` is `lang::parse`'s own defensive
/// `Unsupported`/`ParseFailed`, which cannot occur for the two registered
/// ids.
pub fn outline(lang_id: &str, source: &[u8]) -> Result<StyleOutline> {
    let (tree, _language) = lang::parse(lang_id, source)?;
    let mut w = Walker {
        source,
        symbols: Vec::new(),
        capped: false,
    };
    w.walk_children(tree.root_node(), None, 0);
    Ok(StyleOutline {
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
    fn walk_children(&mut self, node: Node<'_>, container: Option<String>, depth: usize) {
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        for child in children {
            if self.capped {
                return;
            }
            self.walk(child, container.clone(), depth);
        }
    }

    fn walk(&mut self, node: Node<'_>, container: Option<String>, depth: usize) {
        let kind = node.kind();
        // A node that defines nothing but may CONTAIN definitions: SCSS
        // control flow, `@include` with a block, and the `block` wrapper
        // itself. Descend without minting and without spending depth —
        // an `@each` is not a level of structure a reader thinks in.
        if matches!(
            kind,
            "block"
                | "if_statement"
                | "else_clause"
                | "else_if_clause"
                | "each_statement"
                | "for_statement"
                | "while_statement"
                | "include_statement"
                | "keyframe_block_list"
                | "keyframe_block"
        ) {
            self.walk_children(node, container, depth);
            return;
        }
        let Some((row_kind, name, detail)) = self.classify(node) else {
            return;
        };
        self.emit(node, row_kind, &name, detail, container);
        if self.capped || depth + 1 >= MAX_DEPTH {
            return;
        }
        // Descend into whatever body this row has. A `declaration` has
        // none; every other row's children carry the nested rules.
        self.walk_children(node, Some(name), depth + 1);
    }

    /// `(kind, name, detail)` for a node that IS a row, `None` for one
    /// that is not.
    fn classify(&self, node: Node<'_>) -> Option<(&'static str, String, Option<String>)> {
        match node.kind() {
            "rule_set" => {
                let selectors = node
                    .named_children(&mut node.walk())
                    .find(|c| c.kind() == "selectors")?;
                let text = collapse(self.text(selectors)?);
                if text.is_empty() {
                    return None;
                }
                // A whole selector list that is one `%name` is an SCSS
                // placeholder — a thing `@extend` refers to by name, which
                // an ordinary selector is not.
                let is_placeholder = selectors
                    .named_children(&mut selectors.walk())
                    .filter(|c| c.kind() != "comment")
                    .map(|c| c.kind())
                    .eq(std::iter::once("placeholder"));
                Some((
                    if is_placeholder {
                        KIND_PLACEHOLDER
                    } else {
                        KIND_RULE
                    },
                    cap(&text),
                    None,
                ))
            }
            "mixin_statement" | "function_statement" => {
                let kind = if node.kind() == "mixin_statement" {
                    KIND_MIXIN
                } else {
                    KIND_FUNCTION
                };
                let name = node
                    .child_by_field_name("name")
                    .and_then(|n| self.text(n))
                    .map(collapse)?;
                Some((kind, cap(&name), Some(cap(&self.header(node)))))
            }
            "keyframes_statement" => {
                let header = self.header(node);
                let name = node
                    .named_children(&mut node.walk())
                    .find(|c| c.kind() == "keyframes_name")
                    .and_then(|n| self.text(n))
                    .map(collapse)
                    .unwrap_or_else(|| header.clone());
                Some((KIND_KEYFRAMES, cap(&name), Some(cap(&header))))
            }
            "declaration" => {
                let first = node
                    .named_children(&mut node.walk())
                    .find(|c| c.kind() != "comment")?;
                let text = collapse(self.text(first)?);
                // SCSS `$name:` and CSS `--name:` are the two declarations
                // that DEFINE something. Everything else is a property
                // setting, and listing those would re-print the file.
                //
                // `tree-sitter-scss` parses a top-level `$brand: #336699;`
                // as `(declaration (property_name) (color_value))` — the
                // `$brand` is a `property_name` whose TEXT starts with
                // `$`, not a `variable` node (that kind is used for a
                // variable in VALUE position). Both shapes are accepted
                // rather than assuming the tidier one.
                if first.kind() == "variable" || text.starts_with('$') || text.starts_with("--") {
                    Some((KIND_VARIABLE, cap(&text), Some(cap(&self.header(node)))))
                } else {
                    None
                }
            }
            "media_statement"
            | "supports_statement"
            | "at_rule"
            | "at_root_statement"
            | "import_statement"
            | "use_statement"
            | "forward_statement"
            | "charset_statement"
            | "namespace_statement"
            | "postcss_statement" => {
                let header = cap(&self.header(node));
                if header.is_empty() {
                    return None;
                }
                Some((KIND_AT_RULE, header, None))
            }
            _ => None,
        }
    }

    /// The node's text from its start up to the start of its `block`
    /// child (or its whole text when it has none), whitespace-collapsed —
    /// `extract::build_signature`'s trick, applied to a stylesheet
    /// statement so the header reads the way the source does regardless of
    /// which grammar-specific children it happens to have.
    fn header(&self, node: Node<'_>) -> String {
        let end = node
            .named_children(&mut node.walk())
            .find(|c| matches!(c.kind(), "block" | "keyframe_block_list"))
            .map(|b| b.start_byte())
            .unwrap_or_else(|| node.end_byte());
        let start = node.start_byte();
        if end <= start || end > self.source.len() {
            return collapse(
                std::str::from_utf8(&self.source[start.min(self.source.len())..node.end_byte()])
                    .unwrap_or(""),
            );
        }
        let raw = std::str::from_utf8(&self.source[start..end]).unwrap_or("");
        collapse(raw.trim_end_matches([';', '{', ' ', '\n', '\t']))
    }

    fn text(&self, node: Node<'_>) -> Option<&str> {
        node.utf8_text(self.source).ok()
    }

    fn emit(
        &mut self,
        node: Node<'_>,
        kind: &'static str,
        name: &str,
        detail: Option<String>,
        container: Option<String>,
    ) {
        if self.symbols.len() >= MAX_ROWS {
            self.capped = true;
            return;
        }
        let start = node.start_position();
        let end = node.end_position();
        self.symbols.push(Symbol {
            ordinal: self.symbols.len() as u32,
            name: name.to_string(),
            kind: kind.to_string(),
            line_start: start.row as u32 + 1,
            line_end: end.row as u32 + 1,
            col_start: start.column as u32,
            col_end: end.column as u32,
            container,
            // The header, for the rows whose `name` is a fragment of it.
            // Never a type signature — a stylesheet has none, and calling
            // this field one would be the over-claim the Parity Grid
            // exists to prevent.
            signature: detail,
            doc: None,
            param_min: None,
            param_max: None,
        });
        if self.symbols.len() >= MAX_ROWS {
            self.capped = true;
        }
    }
}

/// Collapse every whitespace run to one space and trim.
fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn cap(s: &str) -> String {
    if s.chars().count() <= NAME_CAP {
        return s.to_string();
    }
    s.chars().take(NAME_CAP).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSS_FIXTURE: &str = r#"/* tokens */
:root {
  --radius: 4px;
  --brand: #336699;
}

.btn, .btn--primary {
  color: red;
  border-radius: var(--radius);
}

@media (min-width: 40rem) {
  .btn {
    display: grid;
  }
}

@keyframes pulse {
  from { opacity: 0; }
  to { opacity: 1; }
}
"#;

    const SCSS_FIXTURE: &str = r#"@use "sass:math";

$brand: #336699;
$radius: 4px;

%card-base {
  border-radius: $radius;
}

@mixin button($size: md) {
  padding: $size;
}

@function double($n) {
  @return $n * 2;
}

.card {
  color: $brand;

  .card__title {
    font-weight: 700;
  }

  @each $name in $sizes {
    .card--#{$name} { color: red; }
  }
}
"#;

    /// The two constructs `tree-sitter-scss` 1.0.0 CANNOT parse, kept out
    /// of the fixture above and pinned here instead — see
    /// `the_scss_grammar_cannot_parse_extend_which_is_why_its_tier_is_highlight_only`.
    const SCSS_GAP_FIXTURE: &str = r#".card {
  @extend %card-base;
  color: $brand;
  .card__title { font-weight: 700; }
}
"#;

    fn rows(lang: &str, src: &str) -> Vec<(String, String, Option<String>)> {
        outline(lang, src.as_bytes())
            .unwrap()
            .symbols
            .into_iter()
            .map(|s| (s.kind, s.name, s.container))
            .collect()
    }

    #[test]
    fn css_fixture_mints_rules_custom_properties_at_rules_and_keyframes() {
        let got = rows("css", CSS_FIXTURE);
        insta::assert_debug_snapshot!(got);
        let kinds: Vec<&str> = got.iter().map(|(k, _, _)| k.as_str()).collect();
        assert!(kinds.contains(&KIND_RULE), "{got:?}");
        assert!(kinds.contains(&KIND_VARIABLE), "{got:?}");
        assert!(kinds.contains(&KIND_AT_RULE), "{got:?}");
        assert!(kinds.contains(&KIND_KEYFRAMES), "{got:?}");
        // An ordinary property setting is never a row.
        assert!(
            !got.iter().any(|(_, n, _)| n == "color"),
            "an ordinary declaration must not be an outline row: {got:?}"
        );
    }

    #[test]
    fn scss_fixture_mints_variables_placeholders_mixins_and_functions() {
        let got = rows("scss", SCSS_FIXTURE);
        insta::assert_debug_snapshot!(got);
        let by_kind = |k: &str| -> Vec<String> {
            got.iter()
                .filter(|(kind, _, _)| kind == k)
                .map(|(_, n, _)| n.clone())
                .collect()
        };
        assert_eq!(by_kind(KIND_VARIABLE), vec!["$brand", "$radius"]);
        assert_eq!(by_kind(KIND_PLACEHOLDER), vec!["%card-base"]);
        assert_eq!(by_kind(KIND_MIXIN), vec!["button"]);
        assert_eq!(by_kind(KIND_FUNCTION), vec!["double"]);
        assert!(by_kind(KIND_RULE).iter().any(|n| n == ".card__title"));
        // Control flow is descended THROUGH, never minted: the rule
        // written inside `@each` is a row, the `@each` itself is not.
        assert!(
            by_kind(KIND_RULE).iter().any(|n| n.contains("card--")),
            "a rule nested inside @each must still appear: {got:?}"
        );
    }

    #[test]
    fn a_mixin_carries_its_full_header_as_detail() {
        let symbols = outline("scss", SCSS_FIXTURE.as_bytes()).unwrap().symbols;
        let mixin = symbols
            .iter()
            .find(|s| s.kind == KIND_MIXIN)
            .expect("mixin row");
        assert_eq!(mixin.name, "button");
        assert_eq!(mixin.signature.as_deref(), Some("@mixin button($size: md)"));
    }

    /// **The finding that set SCSS's tier.** `tree-sitter-scss` 1.0.0 —
    /// the only release its upstream has ever published — cannot parse
    /// `@extend`, one of SCSS's most common directives. The failure is
    /// not a missing node: the `ERROR` swallows the REST of the enclosing
    /// block, so every rule, variable and nested selector after an
    /// `@extend` disappears from the outline with no signal at all.
    ///
    /// A silently-short outline that looks complete is worse than none,
    /// which is why `.scss` ships at tier `highlight_only` (highlighting
    /// over an error tree degrades VISIBLY — uncoloured text — where a
    /// missing outline row is invisible) while `.css`, on the official
    /// grammar, is `full`.
    ///
    /// This test is the trigger to re-evaluate: when a grammar that
    /// parses `@extend` lands, it FAILS, and flipping the tier back to
    /// `full` is a one-line change in `syntax::REGISTRY`.
    #[test]
    fn the_scss_grammar_cannot_parse_extend_which_is_why_its_tier_is_highlight_only() {
        let got = rows("scss", SCSS_GAP_FIXTURE);
        // The `.card` rule survives; everything inside it after the
        // `@extend` is gone.
        assert!(
            got.iter().any(|(k, n, _)| k == KIND_RULE && n == ".card"),
            "{got:?}"
        );
        assert!(
            !got.iter().any(|(_, n, _)| n == ".card__title"),
            "the grammar gained @extend support — re-evaluate scss's tier: {got:?}"
        );
        // And the tier says so, with a reason.
        let row = crate::syntax::row_for_lang("scss").expect("scss row");
        assert_eq!(row.tier, crate::syntax::Tier::HighlightOnly);
        assert!(row
            .note
            .expect("a non-full tier explains itself")
            .contains("@extend"));
    }

    #[test]
    fn nesting_sets_the_container_to_the_nearest_enclosing_row() {
        let symbols = outline("scss", SCSS_FIXTURE.as_bytes()).unwrap().symbols;
        let nested = symbols
            .iter()
            .find(|s| s.name == ".card__title")
            .expect(".card__title row");
        assert_eq!(nested.container.as_deref(), Some(".card"));
    }

    #[test]
    fn every_minted_kind_is_declared_in_kinds() {
        for src in [CSS_FIXTURE, SCSS_FIXTURE] {
            for lang in ["css", "scss"] {
                for s in outline(lang, src.as_bytes()).unwrap().symbols {
                    assert!(
                        KINDS.contains(&s.kind.as_str()),
                        "{lang}: minted kind {:?} is not in KINDS — the module doc's \
                         vocabulary table and the code disagree",
                        s.kind
                    );
                }
            }
        }
    }

    #[test]
    fn degenerate_input_never_panics_and_never_leaves_bounds() {
        for src in [
            "",
            "}",
            "{{{{{{",
            "@",
            "@mixin",
            ".a{",
            "$",
            "--",
            "/* unterminated",
            "@media",
            ".a { .b { .c { .d { .e { .f { .g { .h { .i { color: red } } } } } } } } }",
        ] {
            for lang in ["css", "scss"] {
                let out =
                    outline(lang, src.as_bytes()).unwrap_or_else(|e| panic!("{lang} {src:?}: {e}"));
                let lines = src.lines().count().max(1) as u32;
                for s in &out.symbols {
                    assert!(s.line_start >= 1, "{lang} {src:?}: {s:?}");
                    assert!(s.line_end >= s.line_start, "{lang} {src:?}: {s:?}");
                    assert!(s.line_start <= lines + 1, "{lang} {src:?}: {s:?}");
                }
            }
        }
    }

    #[test]
    fn a_pathological_stylesheet_reports_its_cap_rather_than_truncating_silently() {
        let src: String = (0..MAX_ROWS + 50)
            .map(|i| format!(".r{i} {{ color: red; }}\n"))
            .collect();
        let out = outline("css", src.as_bytes()).unwrap();
        assert!(out.capped, "the cap must be reported");
        assert_eq!(out.symbols.len(), MAX_ROWS);
    }

    #[test]
    fn utf8_names_are_capped_by_chars_not_bytes() {
        let long = "é".repeat(NAME_CAP + 40);
        let src = format!(".{long} {{ color: red; }}");
        let out = outline("css", src.as_bytes()).unwrap();
        for s in out.symbols {
            assert!(s.name.chars().count() <= NAME_CAP, "{:?}", s.name);
        }
    }
}
