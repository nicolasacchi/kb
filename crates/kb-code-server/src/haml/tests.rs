//! `haml/1` unit tests — the scanner's own contracts. The DIVERGENCE
//! CORPUS (this scanner vs. the real `haml` gem, over ~40 templates) is a
//! separate integration test, `tests/haml_corpus.rs`; these are the rules
//! that corpus cannot express because the gem has no notion of them (byte
//! ranges, the fragment stream, diagnostics, span normalisation).

use super::extract::{self, FragmentKind};
use super::parser::{self, Inline, NodeKind};
use super::projection;
use super::{scan, DiagnosticKind};

fn doc(src: &str) -> parser::Document {
    parser::parse_str(src)
}

/// `kind@line` for every node in document order, with depth as leading
/// dots — a compact shape for asserting tree SHAPE in one string.
fn shape(d: &parser::Document) -> String {
    fn walk(d: &parser::Document, id: usize, depth: usize, out: &mut Vec<String>) {
        let n = d.node(id);
        out.push(format!(
            "{}{}@{}",
            ".".repeat(depth),
            n.kind.as_str(),
            n.line
        ));
        for c in &n.children {
            walk(d, *c, depth + 1, out);
        }
    }
    let mut out = Vec::new();
    for r in &d.roots {
        walk(d, *r, 0, &mut out);
    }
    out.join(" ")
}

// ── the tree ──────────────────────────────────────────────────────────────

/// Rule 1 of the parser's module doc, and the reason the Ruby program
/// needs no second pass: `- else` is a CHILD of its `- if`, and so is
/// everything after it, exactly as HAML's own parser reports.
#[test]
fn a_mid_block_keyword_is_a_child_of_the_block_it_continues() {
    let d = doc("- if x\n  = a\n- else\n  = b\n%p after\n");
    assert_eq!(
        shape(&d),
        "silent_script@1 .script@2 .silent_script@3 .script@4 tag@5"
    );
    let d = doc("- case x\n- when 1\n  = a\n- when 2\n  = b\n- else\n  = c\n");
    assert_eq!(
        shape(&d),
        "silent_script@1 .silent_script@2 .script@3 .silent_script@4 .script@5 \
         .silent_script@6 .script@7"
    );
    // `begin`/`rescue`/`ensure` is the same shape.
    let d = doc("- begin\n  = a\n- rescue => e\n  = b\n- ensure\n  = c\n");
    assert_eq!(
        shape(&d),
        "silent_script@1 .script@2 .silent_script@3 .script@4 .silent_script@5 .script@6"
    );
}

/// HAML's own keyword table, not a superset: `while`/`for`/`def` open a
/// block by indentation but carry no keyword tag, because HAML does not
/// tag them either.
#[test]
fn the_keyword_table_is_hamls_own() {
    let cases: &[(&str, Option<&str>)] = &[
        ("- if x", Some("if")),
        ("- unless x", Some("unless")),
        ("- case x", Some("case")),
        ("- begin", Some("begin")),
        ("- else", Some("else")),
        ("- elsif y", Some("elsif")),
        ("- when 1", Some("when")),
        ("- in Foo", Some("in")),
        ("- rescue => e", Some("rescue")),
        ("- ensure", Some("ensure")),
        ("- x = if y", Some("if")),
        ("- a, b = case y", Some("case")),
        ("- while x", None),
        ("- for i in xs", None),
        ("- def foo", None),
        ("- items.each do |i|", None),
        ("- iffy", None),
        ("- content_for :head do", None),
    ];
    for (line, expected) in cases {
        let d = doc(&format!("{line}\n"));
        let NodeKind::SilentScript(s) = &d.nodes[0].kind else {
            panic!("{line}: not a silent script");
        };
        assert_eq!(s.keyword.as_deref(), *expected, "{line}");
    }
}

#[test]
fn plain_text_and_tag_shorthands_are_not_confused_for_each_other() {
    let d = doc("#{foo} bar\n. full stop\n.card\n#hero\n%p hi\n");
    assert_eq!(shape(&d), "plain@1 plain@2 tag@3 tag@4 tag@5");
    let NodeKind::Tag(t) = &d.nodes[2].kind else {
        panic!("expected a tag")
    };
    assert_eq!(t.name, "div", "a bare .class is an implicit div");
    assert!(!t.name_explicit);
    assert_eq!(t.display_name(), "div.card");
}

#[test]
fn the_outline_name_is_tag_then_id_then_classes_in_source_order() {
    let d = doc("%section#hero.big.wide\n  %ul.list\n    %li\n:javascript\n  var x = 1;\n");
    let rows = extract::outline(
        &d,
        "%section#hero.big.wide\n  %ul.list\n    %li\n:javascript\n  var x = 1;\n",
    );
    let names: Vec<(&str, &str, Option<&str>)> = rows
        .iter()
        .map(|s| (s.name.as_str(), s.kind.as_str(), s.container.as_deref()))
        .collect();
    assert_eq!(
        names,
        vec![
            ("section#hero.big.wide", "element", None),
            ("ul.list", "element", Some("section#hero.big.wide")),
            ("li", "element", Some("ul.list")),
            ("javascript", "filter", None),
        ]
    );
    // A template outline row claims nothing it did not prove.
    assert!(rows
        .iter()
        .all(|s| s.signature.is_none() && s.doc.is_none()));
    // And an element row line range covers its subtree, so the structure
    // popup can fold on it.
    assert_eq!((rows[0].line_start, rows[0].line_end), (1, 3));
}

#[test]
fn self_closing_whitespace_removal_and_escapes_are_all_read() {
    let d = doc("%br/\n%p> tight\n%q< inner\n\\= not a script\n");
    let NodeKind::Tag(br) = &d.nodes[0].kind else {
        panic!()
    };
    assert!(br.self_closing);
    let NodeKind::Tag(p) = &d.nodes[1].kind else {
        panic!()
    };
    assert!(p.nuke_outer && !p.nuke_inner);
    let NodeKind::Tag(q) = &d.nodes[2].kind else {
        panic!()
    };
    assert!(q.nuke_inner && !q.nuke_outer);
    let NodeKind::Plain(t) = &d.nodes[3].kind else {
        panic!("an escaped line is plain text")
    };
    assert_eq!(t.value, "= not a script");
}

#[test]
fn html_style_attributes_are_literal_pairs_and_ruby_hashes_are_not() {
    let src = "%a.btn#go(href=\"/x\" data-x=\"y\"){data: {c: \"row\"}} Go\n";
    let d = doc(src);
    let NodeKind::Tag(t) = &d.nodes[0].kind else {
        panic!()
    };
    assert_eq!(
        t.static_attributes(),
        vec![
            ("class".to_string(), "btn".to_string()),
            ("id".to_string(), "go".to_string()),
            ("href".to_string(), "/x".to_string()),
            ("data-x".to_string(), "y".to_string()),
        ]
    );
    // The Ruby hash contributes NO static attribute — inventing one from
    // `{data: {c: "row"}}` would be a guess.
    assert_eq!(t.attrs.len(), 2);
    let Some(Inline::Text(inline)) = &t.inline else {
        panic!("inline text")
    };
    assert_eq!(inline.value, "Go");
}

// ── continuations ─────────────────────────────────────────────────────────

#[test]
fn an_attribute_hash_may_span_lines_and_nest_braces_and_strings() {
    let src = "%a{href: url,\n   title: t(\".tip\"),\n   data: {c: \"row\"}}= x\n%p after\n";
    let d = doc(src);
    assert_eq!(shape(&d), "tag@1 tag@4", "the hash consumed lines 1-3");
    let NodeKind::Tag(t) = &d.nodes[0].kind else {
        panic!()
    };
    let inner = t.attrs[0].inner.slice(src).expect("inner slice");
    assert!(inner.contains("title: t(\".tip\")"));
    assert!(inner.ends_with("data: {c: \"row\"}"));
}

#[test]
fn a_brace_inside_a_string_inside_an_interpolation_does_not_close_the_hash() {
    let src = "%a{title: \"a#{h(\"}\")}b\", id: 1} Go\n%p after\n";
    let d = doc(src);
    assert_eq!(shape(&d), "tag@1 tag@2");
    let NodeKind::Tag(t) = &d.nodes[0].kind else {
        panic!()
    };
    let inner = t.attrs[0].inner.slice(src).expect("inner");
    assert!(inner.ends_with("id: 1"), "got {inner:?}");
}

#[test]
fn a_pipe_block_and_a_trailing_comma_both_join_into_one_logical_line() {
    let d = doc("%p= h(  |\n  \"a\" + |\n  \"b\")  |\n%q after\n");
    assert_eq!(shape(&d), "tag@1 tag@4");
    let NodeKind::Tag(t) = &d.nodes[0].kind else {
        panic!()
    };
    let Some(Inline::Script(s)) = &t.inline else {
        panic!("inline script")
    };
    assert_eq!(s.code.trim(), "h(  \"a\" + \"b\")");
    assert!(!s.verbatim, "a joined fragment is not a verbatim slice");

    let d = doc("= link_to \"a\",\n  root_path\n%p after\n");
    assert_eq!(shape(&d), "script@1 tag@3");
    let NodeKind::Script(s) = &d.nodes[0].kind else {
        panic!()
    };
    assert_eq!(s.code.trim(), "link_to \"a\", root_path");
}

#[test]
fn a_filter_swallows_its_blank_lines_and_an_unknown_filter_is_still_a_filter() {
    let src = ":javascript\n  var x = 1;\n\n  var y = 2;\n%p after\n";
    let d = doc(src);
    assert_eq!(shape(&d), "filter@1 tag@5");
    let NodeKind::Filter(f) = &d.nodes[0].kind else {
        panic!()
    };
    assert_eq!(f.name, "javascript");
    assert_eq!(f.text, "var x = 1;\n\nvar y = 2;\n");

    let d = doc(":wat\n  stuff\n%p after\n");
    assert_eq!(shape(&d), "filter@1 tag@3");
    let NodeKind::Filter(f) = &d.nodes[0].kind else {
        panic!()
    };
    assert_eq!(f.name, "wat");
    assert!(
        !parser::KNOWN_FILTERS.contains(&"wat"),
        "the point of this case is that the name is unknown"
    );
}

// ── the fragment stream and the offset map ────────────────────────────────

/// The contract H2a lifts: every fragment addresses REAL source bytes, and
/// a verbatim one slices back to exactly its own text.
#[test]
fn every_ruby_fragment_maps_back_to_the_exact_source_bytes() {
    let src = "\
%section{id: \"hero\", data: {c: t(\".c\")}}
  %h1= t(\".title\")
  %p Total: #{number_to_currency(total)}
  - items.each do |i|
    %li[i]= render \"rows/row\", item: i
  :ruby
    x = 1
";
    let d = doc(src);
    let frags = extract::ruby_fragments(&d, src);
    assert!(frags.len() >= 6, "got {} fragments", frags.len());
    for f in &frags {
        let slice = f
            .span
            .slice(src)
            .unwrap_or_else(|| panic!("fragment {:?} span {:?} is out of bounds", f.kind, f.span));
        assert!(!slice.is_empty(), "{:?} addresses no bytes", f.kind);
        if f.verbatim {
            assert_eq!(slice, f.text, "{:?} is not its own source", f.kind);
        }
        assert_eq!(
            f.line,
            extract::line_at(src, f.span.start as usize),
            "{:?} line disagrees with its span",
            f.kind
        );
    }
    let kinds: Vec<FragmentKind> = frags.iter().map(|f| f.kind).collect();
    assert!(kinds.contains(&FragmentKind::Attributes));
    assert!(kinds.contains(&FragmentKind::ObjectRef));
    assert!(kinds.contains(&FragmentKind::Interpolation));
    assert!(kinds.contains(&FragmentKind::Script));
    assert!(kinds.contains(&FragmentKind::RubyFilter));
}

#[test]
fn the_program_synthesizes_one_end_per_block_and_maps_every_row_back() {
    let src = "- if x\n  = a\n- else\n  = b\n- items.each do |i|\n  %li= i\n";
    let d = doc(src);
    let prog = extract::ruby_program(&d, src);
    let lines: Vec<&str> = prog.source.lines().collect();
    assert_eq!(
        lines,
        vec![
            "if x",
            " a",
            " else",
            " b",
            "end",
            " items.each do |i|",
            " i",
            "end",
        ]
    );
    // Every non-synthetic row re-anchors to the HAML line it came from...
    let mapped: Vec<Option<u32>> = (0..lines.len()).map(|r| prog.haml_line(r)).collect();
    assert_eq!(
        mapped,
        vec![
            Some(1),
            Some(2),
            Some(3),
            Some(4),
            None,
            Some(5),
            Some(6),
            None
        ]
    );
    // ...and a row past the end is `None`, never a panic (a Ruby parse
    // error can put a node anywhere).
    assert_eq!(prog.haml_line(999), None);
}

#[test]
fn a_multi_line_attribute_hash_maps_each_program_row_to_its_own_haml_line() {
    let src = "%a{href: url,\n   title: t(\".tip\")}\n";
    let d = doc(src);
    let prog = extract::ruby_program(&d, src);
    assert_eq!(prog.lines, vec![Some(1), Some(2)]);
    assert!(prog.source.starts_with("_haml_attributes = {href: url,"));
    assert!(prog.source.trim_end().ends_with("title: t(\".tip\")}"));
}

/// The program must be Ruby the EXISTING parser accepts — that is the
/// whole reason the `end`s are synthesized at all.
#[test]
fn the_synthesized_program_parses_as_ruby() {
    let src = "\
- if @order
  = form_for @order do |f|
    = f.text_field :name
- else
  %p= t(\".empty\")
- @items.each do |i|
  %li{class: i.css}= render \"rows/row\", item: i
";
    let d = doc(src);
    let prog = extract::ruby_program(&d, src);
    let (tree, _) = crate::lang::parse("ruby", prog.source.as_bytes()).expect("ruby parse");
    assert!(
        !tree.root_node().has_error(),
        "synthesized program did not parse:\n{}",
        prog.source
    );
}

// ── diagnostics: malformed input is captioned, never fatal ────────────────

#[test]
fn tabs_bad_dedents_and_unclosed_constructs_are_diagnostics_not_panics() {
    let kinds = |src: &str| -> Vec<DiagnosticKind> {
        scan(src.as_bytes())
            .diagnostics
            .iter()
            .map(|d| d.kind)
            .collect()
    };
    assert!(kinds("%a\n\t%b\n").contains(&DiagnosticKind::TabIndent));
    assert!(kinds("%a\n    %b\n  %c\n").contains(&DiagnosticKind::InconsistentDedent));
    assert!(kinds("%a{href: url\n").contains(&DiagnosticKind::UnclosedAttributes));
    assert!(kinds("%p a #{h(x\n").contains(&DiagnosticKind::UnclosedInterpolation));
    // A well-formed file has nothing to say.
    assert!(kinds("%a\n  %b\n").is_empty());
}

#[test]
fn invalid_utf8_is_a_diagnostic_over_the_valid_prefix() {
    let mut bytes = b"%p ok\n%q ".to_vec();
    bytes.push(0xff);
    let d = scan(&bytes);
    assert!(d
        .diagnostics
        .iter()
        .any(|x| x.kind == DiagnosticKind::InvalidUtf8));
    assert!(!d.nodes.is_empty(), "the valid prefix still parsed");
}

// ── highlight spans ───────────────────────────────────────────────────────

#[test]
fn highlight_spans_are_sorted_non_overlapping_and_in_bounds() {
    let src = "\
-# a note
%section#hero.big{data: {c: \"row\"}}
  / an html comment
  %p= t(\".title\")
  %p Total: #{total}
:javascript
  var x = 1;
";
    let spans = super::highlights(src.as_bytes());
    assert!(!spans.is_empty());
    let mut cursor = 0u32;
    for s in &spans {
        assert!(s.byte_len > 0);
        assert!(s.byte_start >= cursor, "spans overlap or are unsorted");
        assert!(
            (s.byte_start + s.byte_len) as usize <= src.len(),
            "span runs past the source"
        );
        cursor = s.byte_start + s.byte_len;
    }
    // The Ruby inside a fragment is painted by the RUBY highlighter, so a
    // string literal inside an attribute hash is a `string` span.
    assert!(spans
        .iter()
        .any(|s| s.class == crate::highlight::HighlightClass::String));
}

// ── the corpus projection's own transforms ────────────────────────────────

#[test]
fn quote_ruby_escapes_only_backslashes_and_double_quotes() {
    assert_eq!(projection::quote_ruby("Hello #{name}"), "\"Hello #{name}\"");
    assert_eq!(
        projection::quote_ruby("He said \"hi\" #{x}"),
        "\"He said \\\"hi\\\" #{x}\""
    );
    assert_eq!(projection::quote_ruby("a\\\\b"), "\"a\\\\\\\\b\"");
}

#[test]
fn split_doctype_reads_a_version_or_a_type_but_never_guesses_both() {
    assert_eq!(
        projection::split_doctype("5"),
        (Some("5".to_string()), String::new())
    );
    assert_eq!(
        projection::split_doctype("1.1"),
        (Some("1.1".to_string()), String::new())
    );
    assert_eq!(projection::split_doctype("XML"), (None, "xml".to_string()));
    assert_eq!(projection::split_doctype(""), (None, String::new()));
}

#[test]
fn interpolated_text_projects_as_hamls_own_script_node() {
    let d = doc("%p Hello #{name}\n%q Hello\n== a #{b}\nplain\n");
    let v = projection::project(&d);
    let nodes = v["nodes"].as_array().expect("nodes");
    assert_eq!(nodes[0]["inline"]["script"], "\"Hello #{name}\"");
    assert_eq!(nodes[1]["inline"]["text"], "Hello");
    assert_eq!(nodes[2]["kind"], "script");
    assert_eq!(nodes[2]["ruby"], "\"a #{b}\"");
    assert_eq!(nodes[3]["kind"], "plain");
    assert_eq!(nodes[3]["text"], "plain");
}
