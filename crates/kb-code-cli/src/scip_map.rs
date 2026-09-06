//! S1 — SCIP index parsing + mapping: the CLI-only half of the SCIP
//! precision tier (`kb-code scip ingest`). Parsing/protobuf lives HERE
//! (never in `kb-code-server`) — see that crate's `src/scip.rs` module doc
//! for why the daemon stays dependency-free of the `scip` crate.
//!
//! [`map_index`] turns a parsed `scip::types::Index` into a flat list of
//! [`MappedDocument`]s, each a repo-relative `path` plus its
//! [`MappedOccurrence`]s — exactly the shape `POST /api/scip/ingest`'s wire
//! body expects per document (`kb_code_server::scip::ScipDocWire`/
//! `ScipOccWire`), MINUS the `blob_hash` (the CLI attaches that separately,
//! per-document, once it reads the file's CURRENT bytes off the working
//! tree the index was generated against — see `main.rs`'s `scip_ingest_cmd`).
//!
//! # Symbol-name extraction
//!
//! A SCIP symbol is a structured string (`scheme package_manager package
//! version descriptor...`, e.g. `rust-analyzer cargo kb_code_server 0.0.0
//! MyType#method().`). [`symbol_display_name`] parses it
//! (`scip::symbol::parse_symbol`) and takes the LAST descriptor's own
//! `name` — e.g. `method` for the example above — the same "last path
//! segment is the actual thing named" rule every language's own import-
//! following heuristic in `kb-code-server` already uses (`imports.rs`).
//! `parse_symbol` failing (a malformed symbol string) degrades to using the
//! RAW symbol text as the name — an honest fallback, not a dropped
//! occurrence.
//!
//! # Role mapping
//!
//! [`map_role`] reads ONLY the `Definition` bit (`SymbolRole::Definition as
//! i32 == 1`) of `Occurrence.symbol_roles`: set → `"def"`, unset → `"ref"`.
//! Every OTHER SCIP role bit (`Import`/`WriteAccess`/`ReadAccess`/
//! `Generated`/`Test`/`ForwardDefinition`) is deliberately ignored — the
//! phase brief scopes this to "definition vs reference," matching
//! `kb_code_server::occurrences`'s own two-role core (`"def"`/`"ref"`; that
//! module's THIRD role, `"import"`, is a tree-sitter-only concept scip rows
//! never use).
//!
//! # What's excluded, and why
//!
//! [`map_occurrence`] returns `None` (silently excluded, not an error) for:
//! - an occurrence with NO `symbol` at all (pure syntax-highlighting data —
//!   nothing to resolve),
//! - a genuinely MULTI-LINE span (`kb_code_server`'s `occurrences` table
//!   has no multi-line representation — `line`/`col_start`/`col_end` are
//!   all single-line, matching every other row that table already holds).
//!
//! # Position encoding — a known, honest limitation
//!
//! SCIP's `Document.position_encoding` field can be UTF-8, UTF-16, or
//! UTF-32 code-unit offsets (rust-analyzer's own `scip` command emits
//! UTF-8, matching `kb_code_server`'s own byte-offset column convention
//! exactly; `scip-typescript` emits UTF-16, LSP's own default). This module
//! does NOT convert between encodings — it passes `col_start`/`col_end`
//! through verbatim regardless of `position_encoding`. For a UTF-16-encoded
//! index over a file containing non-ASCII characters before the resolved
//! column on the SAME line, this can point a few bytes off; ASCII-only
//! lines (the overwhelming majority of source code) are unaffected either
//! way. Converting UTF-16 offsets to byte offsets would need the target
//! file's own text at mapping time — a scope this phase deliberately
//! doesn't take on (documented here rather than silently assumed correct).

use scip::types::{Document, Index, Occurrence as ScipOccurrence};

/// One mapped occurrence — see the module doc. `line` is 1-based (SCIP's
/// own range encoding is 0-based, matching LSP; this adds 1 to match
/// `kb_code_server`'s convention every OTHER position field in this crate
/// already uses). `col_start`/`col_end` are passed through VERBATIM from
/// SCIP's own encoding — see the module doc's "Position encoding" section.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MappedOccurrence {
    pub name: String,
    /// `"def"` | `"ref"` — see [`map_role`].
    pub role: &'static str,
    pub line: u32,
    pub col_start: u32,
    pub col_end: u32,
}

/// One document's mapped occurrences, keyed by its `relative_path` (already
/// repo-relative, per the SCIP spec: every `Document.relative_path` is
/// relative to `Metadata.project_root`, which `main.rs`'s ingest command
/// treats as the repo root).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappedDocument {
    pub path: String,
    pub occurrences: Vec<MappedOccurrence>,
}

/// Map every document in `index` — see the module doc.
pub fn map_index(index: &Index) -> Vec<MappedDocument> {
    index.documents.iter().map(map_document).collect()
}

fn map_document(doc: &Document) -> MappedDocument {
    MappedDocument {
        path: doc.relative_path.clone(),
        occurrences: doc.occurrences.iter().filter_map(map_occurrence).collect(),
    }
}

fn map_occurrence(occ: &ScipOccurrence) -> Option<MappedOccurrence> {
    if occ.symbol.is_empty() {
        return None;
    }
    let (line0, col_start, col_end) = single_line_span(occ)?;
    Some(MappedOccurrence {
        name: symbol_display_name(&occ.symbol),
        role: map_role(occ.symbol_roles),
        line: line0 + 1,
        col_start,
        col_end,
    })
}

/// `(start_line, start_char, end_char)` for `occ`, ONLY when it's a
/// single-line span — `None` for a genuine multi-line occurrence (see the
/// module doc). Prefers the TYPED `single_line_range`/`multi_line_range`
/// oneof (the current SCIP encoding) and falls back to the deprecated
/// `range: Vec<i32>` field (`[line, start, end]` or `[start_line,
/// start_char, end_line, end_char]`) for an indexer that still emits it —
/// both encodings are 0-based lines/chars per the SCIP spec (LSP
/// convention).
fn single_line_span(occ: &ScipOccurrence) -> Option<(u32, u32, u32)> {
    if occ.has_single_line_range() {
        let r = occ.single_line_range();
        return Some((
            r.line as u32,
            r.start_character as u32,
            r.end_character as u32,
        ));
    }
    if occ.has_multi_line_range() {
        let r = occ.multi_line_range();
        if r.start_line != r.end_line {
            return None;
        }
        return Some((
            r.start_line as u32,
            r.start_character as u32,
            r.end_character as u32,
        ));
    }
    match occ.range.as_slice() {
        [line, start, end] => Some((*line as u32, *start as u32, *end as u32)),
        [start_line, start, end_line, end] if start_line == end_line => {
            Some((*start_line as u32, *start as u32, *end as u32))
        }
        _ => None,
    }
}

/// The last descriptor component's own `name` — see the module doc's
/// "Symbol-name extraction" section. Falls back to the RAW symbol string
/// when `scip::symbol::parse_symbol` fails, or when the symbol has no
/// descriptors at all (shouldn't happen for a well-formed non-local
/// symbol, but `internal_local_symbol`'s own shape always has exactly one —
/// this is defensive, not expected to fire in practice).
fn symbol_display_name(symbol: &str) -> String {
    match scip::symbol::parse_symbol(symbol) {
        Ok(parsed) => parsed
            .descriptors
            .last()
            .map(|d| d.name.clone())
            .unwrap_or_else(|| symbol.to_string()),
        Err(_) => symbol.to_string(),
    }
}

/// `"def"` if the `Definition` bit (`1`) is set in `roles`, else `"ref"` —
/// see the module doc's "Role mapping" section.
fn map_role(roles: i32) -> &'static str {
    const DEFINITION_BIT: i32 = 1; // scip::types::SymbolRole::Definition
    if roles & DEFINITION_BIT != 0 {
        "def"
    } else {
        "ref"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scip::types::{Document as ScipDocument, Index as ScipIndex, SingleLineRange};

    fn occ_single_line(
        symbol: &str,
        roles: i32,
        line: i32,
        start: i32,
        end: i32,
    ) -> ScipOccurrence {
        let mut o = ScipOccurrence::new();
        o.symbol = symbol.to_string();
        o.symbol_roles = roles;
        o.set_single_line_range(SingleLineRange {
            line,
            start_character: start,
            end_character: end,
            ..Default::default()
        });
        o
    }

    fn occ_legacy_range(symbol: &str, roles: i32, range: Vec<i32>) -> ScipOccurrence {
        let mut o = ScipOccurrence::new();
        o.symbol = symbol.to_string();
        o.symbol_roles = roles;
        o.range = range;
        o
    }

    #[test]
    fn maps_a_definition_and_a_reference_with_the_last_descriptor_as_the_name() {
        let mut doc = ScipDocument::new();
        doc.relative_path = "src/lib.rs".to_string();
        doc.occurrences = vec![
            occ_single_line(
                "rust-analyzer cargo kb_code_server 0.0.0 MyType#new().",
                1, // Definition
                9,
                3,
                6,
            ),
            occ_single_line(
                "rust-analyzer cargo kb_code_server 0.0.0 MyType#new().",
                0, // no roles set -> ref
                18,
                4,
                7,
            ),
        ];
        let mut index = ScipIndex::new();
        index.documents = vec![doc];

        let mapped = map_index(&index);
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].path, "src/lib.rs");
        assert_eq!(
            mapped[0].occurrences,
            vec![
                MappedOccurrence {
                    name: "new".to_string(),
                    role: "def",
                    line: 10, // 0-based 9 + 1
                    col_start: 3,
                    col_end: 6,
                },
                MappedOccurrence {
                    name: "new".to_string(),
                    role: "ref",
                    line: 19,
                    col_start: 4,
                    col_end: 7,
                },
            ]
        );
    }

    #[test]
    fn definition_bit_is_the_only_role_bit_consulted() {
        // Definition (1) | WriteAccess (4) | Test (32) — Definition set
        // among others must still map to "def".
        assert_eq!(map_role(1 | 4 | 32), "def");
        // WriteAccess | ReadAccess without Definition -> "ref".
        assert_eq!(map_role(4 | 8), "ref");
        assert_eq!(map_role(0), "ref");
    }

    #[test]
    fn legacy_three_element_range_is_supported() {
        let mut doc = ScipDocument::new();
        doc.relative_path = "a.py".to_string();
        doc.occurrences = vec![occ_legacy_range(
            "scip-python python pkg 1.0.0 widget.",
            1,
            vec![4, 0, 6],
        )];
        let mut index = ScipIndex::new();
        index.documents = vec![doc];

        let mapped = map_index(&index);
        assert_eq!(
            mapped[0].occurrences,
            vec![MappedOccurrence {
                name: "widget".to_string(),
                role: "def",
                line: 5,
                col_start: 0,
                col_end: 6,
            }]
        );
    }

    #[test]
    fn legacy_four_element_same_line_range_is_supported() {
        let mut doc = ScipDocument::new();
        doc.relative_path = "a.py".to_string();
        doc.occurrences = vec![occ_legacy_range(
            "scip-python python pkg 1.0.0 widget.",
            0,
            vec![4, 0, 4, 6],
        )];
        let mut index = ScipIndex::new();
        index.documents = vec![doc];

        let mapped = map_index(&index);
        assert_eq!(mapped[0].occurrences[0].line, 5);
    }

    #[test]
    fn genuinely_multi_line_occurrences_are_excluded() {
        let mut doc = ScipDocument::new();
        doc.relative_path = "a.py".to_string();
        doc.occurrences = vec![occ_legacy_range(
            "scip-python python pkg 1.0.0 widget.",
            1,
            vec![4, 0, 6, 3], // start_line 4 != end_line 6
        )];
        let mut index = ScipIndex::new();
        index.documents = vec![doc];

        let mapped = map_index(&index);
        assert!(mapped[0].occurrences.is_empty(), "got: {:#?}", mapped[0]);
    }

    #[test]
    fn occurrence_with_no_symbol_is_excluded() {
        let mut doc = ScipDocument::new();
        doc.relative_path = "a.py".to_string();
        doc.occurrences = vec![occ_legacy_range("", 0, vec![1, 0, 3])];
        let mut index = ScipIndex::new();
        index.documents = vec![doc];

        let mapped = map_index(&index);
        assert!(mapped[0].occurrences.is_empty());
    }

    #[test]
    fn a_malformed_symbol_falls_back_to_the_raw_string_as_the_name() {
        let mut doc = ScipDocument::new();
        doc.relative_path = "a.py".to_string();
        // No descriptor characters at all in a way `parse_symbol` accepts
        // (missing package fields entirely) — exercised as raw fallback,
        // not asserted against `parse_symbol`'s own internal error rules.
        doc.occurrences = vec![occ_legacy_range(
            "not a valid scip symbol!!",
            1,
            vec![0, 0, 3],
        )];
        let mut index = ScipIndex::new();
        index.documents = vec![doc];

        let mapped = map_index(&index);
        assert_eq!(mapped[0].occurrences.len(), 1);
        // Either parses to SOME descriptor name, or falls back to the raw
        // string — either way, never panics and never silently drops it.
        assert!(!mapped[0].occurrences[0].name.is_empty());
    }

    #[test]
    fn map_index_visits_every_document() {
        let mut doc_a = ScipDocument::new();
        doc_a.relative_path = "a.py".to_string();
        let mut doc_b = ScipDocument::new();
        doc_b.relative_path = "b.py".to_string();
        let mut index = ScipIndex::new();
        index.documents = vec![doc_a, doc_b];

        let mapped = map_index(&index);
        assert_eq!(
            mapped.iter().map(|d| d.path.as_str()).collect::<Vec<_>>(),
            vec!["a.py", "b.py"]
        );
    }
}
