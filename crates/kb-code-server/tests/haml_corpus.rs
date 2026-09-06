//! V72-H3 — the `haml/1` DIVERGENCE CORPUS and its robustness sweep.
//!
//! `tests/fixtures/haml/` holds 41 synthetic templates and, beside each,
//! `<name>.expected.json`: the REAL `haml` gem's own `Haml::Parser` output,
//! projected into the shape `kb_code_server::haml::projection` produces.
//! Those expectations were generated ONCE, offline, by
//! `tests/fixtures/haml/generate_expected.rb` on a developer box — see
//! `tests/fixtures/haml/CORPUS.md` for the pinned gem version and the
//! regeneration command.
//!
//! **The gem is never invoked here, by `ci-code`, or by the daemon.** This
//! file is pure Rust reading checked-in JSON; nothing in the build depends
//! on Ruby existing anywhere, and kb-code-server's invariant 10 ("the
//! daemon never spawns a non-git process") is untouched.

use kb_code_server::frameworks::{extract_edges, FrameworkEdge};
use kb_code_server::haml::{self, extract, parser, projection};
use std::path::{Path, PathBuf};

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/haml")
}

/// Every `*.haml` in the corpus, sorted — the walk order is part of the
/// determinism, exactly as `rails_lens.rs`'s own walk is.
fn fixtures() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(corpus_dir())
        .expect("corpus dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("haml"))
        .collect();
    out.sort();
    out
}

fn expectation_path(haml: &Path) -> PathBuf {
    haml.with_extension("expected.json")
}

// ── the corpus itself ─────────────────────────────────────────────────────

/// The unit's central claim: over every construct in the corpus, this
/// scanner's tree is the tree HAML's own parser reports.
#[test]
fn the_scanner_agrees_with_the_haml_gem_over_the_whole_corpus() {
    let mut checked = 0usize;
    let mut failures: Vec<String> = Vec::new();
    for path in fixtures() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let src = std::fs::read_to_string(&path).expect("read fixture");
        let expected_raw = std::fs::read_to_string(expectation_path(&path)).unwrap_or_else(|e| {
            panic!("{name}: no checked-in expectation ({e}) — see fixtures/haml/CORPUS.md")
        });
        let expected: serde_json::Value =
            serde_json::from_str(&expected_raw).expect("expectation is JSON");
        let actual = projection::project(&parser::parse_str(&src));
        if actual != expected {
            failures.push(format!(
                "── {name} ──\nexpected (haml gem):\n{}\nactual (haml/1 scanner):\n{}",
                serde_json::to_string_pretty(&expected).unwrap(),
                serde_json::to_string_pretty(&actual).unwrap(),
            ));
        }
        checked += 1;
    }
    assert!(checked >= 40, "the corpus shrank to {checked} fixtures");
    assert!(
        failures.is_empty(),
        "{} of {checked} fixtures diverge from the haml gem:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// A fixture with no expectation would silently prove nothing, and an
/// expectation with no fixture is a rename nobody finished.
#[test]
fn every_fixture_has_an_expectation_and_every_expectation_a_fixture() {
    for path in fixtures() {
        assert!(
            expectation_path(&path).is_file(),
            "{}: missing expectation",
            path.display()
        );
    }
    for entry in std::fs::read_dir(corpus_dir())
        .expect("corpus dir")
        .flatten()
    {
        let p = entry.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".expected.json") else {
            continue;
        };
        assert!(
            corpus_dir().join(format!("{stem}.haml")).is_file(),
            "{name}: expectation with no fixture"
        );
    }
    // The corpus never learned to run the gem itself.
    let test_src = include_str!("haml_corpus.rs");
    assert!(
        !test_src.contains("Command::new"),
        "the corpus test must never spawn a process — the gem is offline-only"
    );
}

/// A well-formed fixture produces no diagnostics: the caption surface is
/// for MALFORMED input, and a corpus of legal HAML that emitted warnings
/// would make every real one invisible.
#[test]
fn no_well_formed_fixture_produces_a_diagnostic() {
    for path in fixtures() {
        let src = std::fs::read(&path).expect("read");
        let doc = haml::scan(&src);
        assert!(
            doc.diagnostics.is_empty(),
            "{}: unexpected diagnostics {:?}",
            path.display(),
            doc.diagnostics
        );
    }
}

// ── the offset map ────────────────────────────────────────────────────────

/// Every Ruby fragment in every fixture addresses real bytes, and a
/// verbatim one slices back to exactly its own text. This is the property
/// H2a's injection-aware pipeline will lift, so it is pinned corpus-wide
/// rather than on one hand-picked template.
#[test]
fn every_ruby_fragment_in_the_corpus_maps_back_to_its_source_bytes() {
    let mut total = 0usize;
    for path in fixtures() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let src = std::fs::read_to_string(&path).expect("read");
        let doc = parser::parse_str(&src);
        for f in extract::ruby_fragments(&doc, &src) {
            let slice = f
                .span
                .slice(&src)
                .unwrap_or_else(|| panic!("{name}: fragment span {:?} out of bounds", f.span));
            assert!(!slice.is_empty(), "{name}: fragment addresses no bytes");
            if f.verbatim {
                assert_eq!(slice, f.text, "{name}: {:?} is not its own source", f.kind);
            }
            assert_eq!(
                f.line,
                extract::line_at(&src, f.span.start as usize),
                "{name}: fragment line disagrees with its span"
            );
            total += 1;
        }
    }
    assert!(total > 60, "the corpus only exercised {total} fragments");
}

/// The synthesized program must be Ruby the EXISTING parser accepts —
/// that is the entire reason the `end`s are derived from the indentation
/// tree, and a fixture that produced an error tree would silently stop
/// minting edges.
#[test]
fn the_synthesized_ruby_program_parses_for_every_fixture() {
    for path in fixtures() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let src = std::fs::read_to_string(&path).expect("read");
        let doc = parser::parse_str(&src);
        let program = extract::ruby_program(&doc, &src);
        if program.source.trim().is_empty() {
            continue;
        }
        let (tree, _) =
            kb_code_server::lang::parse("ruby", program.source.as_bytes()).expect("ruby parse");
        assert!(
            !tree.root_node().has_error(),
            "{name}: the synthesized program does not parse:\n{}",
            program.source
        );
        // Every row is either a real HAML line inside the file, or an
        // honest `None` for a line this scanner invented.
        let haml_lines = src.lines().count() as u32;
        for (row, mapped) in program.lines.iter().enumerate() {
            if let Some(line) = mapped {
                assert!(
                    *line >= 1 && *line <= haml_lines,
                    "{name}: program row {row} maps to line {line}, outside 1..={haml_lines}"
                );
            }
        }
    }
}

// ── robustness ────────────────────────────────────────────────────────────

/// Byte-level mutations over the whole corpus: truncations at every
/// eighth, indentation corruption, and stray sigils. The scanner must
/// never panic, and every span it mints must still address real bytes —
/// an out-of-bounds span would panic somewhere further downstream instead,
/// which is worse than panicking here.
#[test]
fn byte_level_mutations_never_panic_and_never_mint_an_out_of_bounds_span() {
    let mut cases = 0usize;
    let mut with_diagnostics = 0usize;
    for path in fixtures() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let original = std::fs::read(&path).expect("read");
        for mutated in mutations(&original) {
            let doc = haml::scan(&mutated);
            let src = std::str::from_utf8(&mutated)
                .unwrap_or_else(|e| std::str::from_utf8(&mutated[..e.valid_up_to()]).unwrap_or(""));
            for node in &doc.nodes {
                assert!(
                    node.span.slice(src).is_some(),
                    "{name}: node span {:?} out of bounds in a mutated file",
                    node.span
                );
            }
            for f in extract::ruby_fragments(&doc, src) {
                assert!(
                    f.span.slice(src).is_some(),
                    "{name}: fragment span {:?} out of bounds in a mutated file",
                    f.span
                );
            }
            // The public entry points must survive the same input.
            let spans = haml::highlights(&mutated);
            let mut cursor = 0u32;
            for s in &spans {
                assert!(s.byte_start >= cursor, "{name}: mutated spans overlap");
                assert!(
                    (s.byte_start + s.byte_len) as usize <= src.len(),
                    "{name}: mutated span runs past the source"
                );
                cursor = s.byte_start + s.byte_len;
            }
            let _ = haml::outline(&mutated);
            if !doc.diagnostics.is_empty() {
                with_diagnostics += 1;
            }
            cases += 1;
        }
    }
    assert!(cases > 400, "the sweep only ran {cases} mutations");
    assert!(
        with_diagnostics > 0,
        "not one mutation was captioned — the diagnostic lane is dead"
    );
}

/// Indentation corruption specifically: the two shapes HAML's own parser
/// RAISES on must both produce a caption here rather than a silent
/// reinterpretation.
#[test]
fn indentation_corruption_is_always_captioned() {
    use kb_code_server::haml::DiagnosticKind;
    for path in fixtures() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let src = std::fs::read_to_string(&path).expect("read");
        if !src.lines().any(|l| l.starts_with("  ")) {
            continue; // nothing indented to corrupt
        }
        // Every two-space indent becomes a tab.
        let tabbed: String = src
            .lines()
            .map(|l| match l.strip_prefix("  ") {
                Some(rest) => format!("\t{rest}\n"),
                None => format!("{l}\n"),
            })
            .collect();
        let doc = haml::scan(tabbed.as_bytes());
        assert!(
            doc.diagnostics
                .iter()
                .any(|d| d.kind == DiagnosticKind::TabIndent),
            "{name}: tab indentation was not captioned"
        );
    }
    // A dedent that lands between two open levels.
    let doc = haml::scan(b"%a\n    %b\n  %c\n");
    assert!(doc
        .diagnostics
        .iter()
        .any(|d| d.kind == DiagnosticKind::InconsistentDedent));
}

fn mutations(original: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let n = original.len();
    if n == 0 {
        return out;
    }
    // Truncations at every eighth — the classic "half a construct".
    for k in 1..8 {
        out.push(original[..n * k / 8].to_vec());
    }
    // Indentation corruption: spaces to tabs, and one level removed.
    out.push(
        original
            .iter()
            .map(|b| if *b == b' ' { b'\t' } else { *b })
            .collect(),
    );
    out.push(
        String::from_utf8_lossy(original)
            .replace("\n  ", "\n ")
            .into_bytes(),
    );
    // Stray unbalanced delimiters, appended and prepended.
    for stray in ["%p{a:", "#{", "%a(href=\"", "  \t- if", ":filter"] {
        let mut v = original.to_vec();
        v.extend_from_slice(stray.as_bytes());
        out.push(v);
        let mut v = stray.as_bytes().to_vec();
        v.extend_from_slice(original);
        out.push(v);
    }
    out
}

// ── ERB ↔ HAML parity ─────────────────────────────────────────────────────

/// The claim that makes this unit a REUSE rather than a second minting
/// path: two equivalent templates, one ERB and one HAML, mint the same
/// Rails edges — same kinds, same lines, same targets, same trust classes.
#[test]
fn erb_and_haml_mint_the_same_edges_for_equivalent_templates() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let write = |rel: &str, body: &str| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };
    write("Gemfile", "gem \"rails\", \"8.1.3.1\"\n");
    write(
        "config/routes.rb",
        "Rails.application.routes.draw do\nend\n",
    );
    write(
        "config/locales/en.yml",
        "en:\n  orders:\n    show:\n      title: Title\n  shared:\n    footer: Footer\n",
    );
    write("app/views/orders/_row.html.erb", "<td>row</td>\n");
    write(
        "app/components/order_component.rb",
        "class OrderComponent\nend\n",
    );

    // Line-for-line equivalent: five call sites, five lines, both files.
    let erb = "\
<h1><%= t(\".title\") %></h1>
<%= render \"orders/row\" %>
<%= render OrderComponent.new %>
<%= NotifyJob.perform_later %>
<%= t(\"shared.footer\") %>
";
    let haml = "\
%h1= t(\".title\")
= render \"orders/row\"
= render OrderComponent.new
= NotifyJob.perform_later
= t(\"shared.footer\")
";
    write("app/views/orders/show.html.erb", erb);
    write("app/views/orders/show.html.haml", haml);

    /// `(kind, line, dst_kind, dst_path, dst_symbol, trust)` — everything
    /// but the source path, which is the one thing that MUST differ.
    fn key(e: &FrameworkEdge) -> String {
        format!(
            "{}@{:?} -> {:?} {:?} {:?} [{}]",
            e.kind.as_str(),
            e.src_line,
            e.dst_kind,
            e.dst_path,
            e.dst_symbol,
            e.trust.as_str()
        )
    }
    let mut from_erb: Vec<String> =
        extract_edges(root, "app/views/orders/show.html.erb", erb.as_bytes())
            .iter()
            .map(key)
            .collect();
    let mut from_haml: Vec<String> =
        extract_edges(root, "app/views/orders/show.html.haml", haml.as_bytes())
            .iter()
            .map(key)
            .collect();
    from_erb.sort();
    from_haml.sort();
    assert!(
        !from_erb.is_empty(),
        "the ERB control produced no edges — the fixture repo is wrong, not the HAML path"
    );
    assert_eq!(
        from_haml, from_erb,
        "HAML and ERB disagree about equivalent templates"
    );
    // And the edges are the ones the corpus claims: render + i18n +
    // component + job, none of them `exact` (the lens has no such tier).
    let kinds: Vec<&str> = extract_edges(root, "app/views/orders/show.html.haml", haml.as_bytes())
        .iter()
        .map(|e| e.kind.as_str())
        .collect();
    assert!(kinds.contains(&"render_partial"));
    assert!(kinds.contains(&"i18n_key"));
    assert!(kinds.contains(&"view_component_render"));
}

/// The block-spanning case ERB structurally cannot do: a `render` written
/// inside a `- if` still resolves, because the HAML walk reconstructs the
/// whole template as ONE Ruby program rather than one fragment per tag.
#[test]
fn a_render_inside_a_haml_block_still_resolves() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let write = |rel: &str, body: &str| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };
    write("Gemfile", "gem \"rails\"\n");
    write(
        "config/routes.rb",
        "Rails.application.routes.draw do\nend\n",
    );
    write("app/views/orders/_row.html.haml", "%td row\n");
    let haml = "\
- if @orders.any?
  - @orders.each do |order|
    %li
      = render \"orders/row\", order: order
";
    write("app/views/orders/index.html.haml", haml);
    let edges = extract_edges(root, "app/views/orders/index.html.haml", haml.as_bytes());
    let render: Vec<&FrameworkEdge> = edges
        .iter()
        .filter(|e| e.kind.as_str() == "render_partial")
        .collect();
    assert_eq!(render.len(), 1, "got {edges:?}");
    assert_eq!(
        render[0].dst_path.as_deref(),
        Some("app/views/orders/_row.html.haml")
    );
    assert_eq!(render[0].src_line, Some(4), "the edge names its HAML line");
    assert_eq!(render[0].trust.as_str(), "likely");
}
