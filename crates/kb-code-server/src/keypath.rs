//! TOML/JSON "key-path outline" — kb-code's symbol extraction for the two
//! W2.6 data-format languages, riding the SAME model `yaml.rs` established
//! in W2.2: neither format has a `tags.scm`/function/class vocabulary to
//! tag (ADR-7's YAML reasoning extends verbatim here — see that module's
//! doc), so each walks its own tree-sitter CST directly into a hierarchical
//! dotted key-path outline, one [`Symbol`] per key, `kind = "key"`, `name` =
//! the full dotted path from the document root — same `symbols` table, same
//! `GET /api/symbols?q=` substring search, same repo-map exclusion
//! (`extract::is_repo_map_symbol` already keys off `kind == "key"`, so
//! nothing there needs to change for two more `kind = "key"` producers).
//!
//! ## Why one module for both, not a `yaml.rs`-shaped file each
//!
//! TOML and JSON share enough of the outline ALGORITHM (mapping/object keys
//! extend a dotted path; a sequence/array of mappings/objects contributes a
//! `[]` segment and recurses into each mapping/object item; a cap on depth
//! and total key count; a `capped` flag) that duplicating `yaml.rs`'s
//! ~250-line walker twice would be pure copy-paste. But their CSTs are
//! shaped too differently to share the WALK code itself: tree-sitter-yaml's
//! `block_mapping_pair` exposes `key`/`value` FIELDS and a `block_node`/
//! `flow_node` decorator-unwrapping layer neither TOML nor JSON's grammars
//! have; tree-sitter-toml-ng's `pair`/`table`/`table_array_element` expose
//! NO fields at all (position-based: first named child is the key, the rest
//! are values/pairs); tree-sitter-json's `pair` DOES have `key`/`value`
//! fields but its `key` is always a `string` node, never a bare identifier.
//! Forcing these into one shared walker would need more indirection
//! (per-grammar node-shape trait objects) than the ~120 lines of duplication
//! it would save — so this file has two independent walkers
//! ([`outline_toml`]/[`outline_json`]), each its own `impl` block, sharing
//! only the cap constants, the `emit` shape, and — rather than a new
//! near-identical struct — `yaml::YamlOutline` itself as the return type
//! (its shape, `{ symbols: Vec<Symbol>, capped: bool }`, is already generic;
//! introducing a same-shaped `KeyPathOutline` alias would just be a second
//! name for the same thing).
//!
//! ## TOML path grammar
//!
//! - A `[table]` or `[[array_of_tables]]` HEADER is one path element in its
//!   own right (its own emitted row, `kind = "key"` — mirroring YAML
//!   emitting a mapping-valued key as its own row before descending into
//!   it), and the base path for every `pair` nested inside it.
//! - A DOTTED header/key (`[a.b.c]`, or a pair key written `a.b = 1`) is
//!   taken as ONE path element whose OWN text already contains the dots
//!   (tree-sitter-toml-ng's `dotted_key` node is walked recursively —
//!   [`toml_key_text`] — and each `bare_key`/`quoted_key` leaf's text is
//!   joined with `.`), not split into synthetic per-segment intermediate
//!   rows. `[a.b.c]` therefore produces ONE row named `a.b.c`, not three
//!   (`a`, `a.b`, `a.b.c`) — TOML's implicit-intermediate-table semantics
//!   are a naming convenience, not something this outline invents rows for.
//!   Consequence: [`MAX_DEPTH`] counts TRAVERSAL steps (nested table/pair/
//!   array levels), not dot-segments in the printed path — a single
//!   `a.b.c.d.e.f.g.h.i = 1` dotted-key pair is one traversal step
//!   regardless of how many dots its name has.
//! - `[[array_of_tables]]` contributes a `[]` segment on the header path
//!   itself (`servers` → `servers[]`), exactly like YAML's block-sequence
//!   rule — repeats of the SAME header path dedupe (same `seen` set) the
//!   same way YAML's repeated sequence items do.
//! - An `array` value contributes `[]` on the preceding key ONLY when at
//!   least one item is itself an `inline_table` (`key = [{a=1}, {b=2}]`) —
//!   a plain array of scalars/arrays produces no rows beyond the key's own.
//! - An `inline_table` value (`key = {a = 1, b = 2}`) is walked exactly
//!   like a nested `[table]` would be, without a `[]` segment (it isn't a
//!   sequence).
//! - There is no TOML analogue of YAML's `---` multi-document stream: one
//!   `seen` dedup set covers the whole file.
//!
//! ## JSON path grammar
//!
//! - Every `object` key is a path element; the object's own row is emitted
//!   (like a YAML mapping-valued key) before descending.
//! - An `array` contributes `[]` on the preceding key ONLY when at least
//!   one item is itself an `object` — a plain array of scalars/arrays/
//!   nested-arrays-of-scalars produces no rows beyond the key's own
//!   (nested arrays-of-arrays are not descended into further, same
//!   simplification YAML documents for its own sequences-of-sequences).
//! - A bare top-level array (no preceding key to hang `[]` off of)
//!   contributes no rows at all — same reasoning as YAML's bare top-level
//!   sequence. A top-level OBJECT (the common case) walks normally.
//! - JSON has one implicit top-level value; one `seen` set for the file.
//!
//! ## Key text
//!
//! Neither [`toml_key_text`] nor [`json_key_text`] is a full string
//! unescaper (no `\uXXXX`/`\n`-escape decoding) — same stance `yaml.rs`'s
//! `scalar_text` documents: this is a browsing aid over the key SHAPE, not
//! a value parser. Quoted keys are stripped of exactly one layer of
//! matching quotes.
//!
//! ## Cardinality caps
//!
//! Identical numbers and semantics to `yaml.rs`: [`MAX_DEPTH`] = 8 (emit
//! the row, stop descending past it), [`MAX_KEYS`] = 500 (stop extraction
//! entirely once hit, `capped = true`).

use crate::extract::Symbol;
use crate::lang::{self, LangError};
use crate::yaml::YamlOutline;
use std::collections::BTreeSet;
use tree_sitter::Node;

pub type Result<T> = std::result::Result<T, LangError>;

const MAX_DEPTH: usize = 8;
const MAX_KEYS: usize = 500;

/// Shared walker state — see the module doc for why TOML and JSON share
/// this struct (and `emit`) but not their `walk_*` methods.
struct Walker<'s> {
    source: &'s [u8],
    symbols: Vec<Symbol>,
    capped: bool,
}

impl Walker<'_> {
    fn emit(
        &mut self,
        node: Node<'_>,
        full_path: &str,
        container: Option<String>,
        seen: &mut BTreeSet<String>,
    ) {
        if self.symbols.len() >= MAX_KEYS {
            self.capped = true;
            return;
        }
        if !seen.insert(full_path.to_string()) {
            return; // duplicate path within this file — see module doc
        }
        let start = node.start_position();
        let end = node.end_position();
        self.symbols.push(Symbol {
            ordinal: self.symbols.len() as u32,
            name: full_path.to_string(),
            kind: "key".to_string(),
            line_start: start.row as u32 + 1,
            line_end: end.row as u32 + 1,
            col_start: start.column as u32,
            col_end: end.column as u32,
            container,
            signature: None,
            doc: None,
            param_min: None,
            param_max: None,
        });
        if self.symbols.len() >= MAX_KEYS {
            self.capped = true;
        }
    }
}

// ============================================================================
// TOML
// ============================================================================

/// Parse `source` as TOML and walk its CST into a key-path outline. Never
/// errors on malformed TOML content itself — tree-sitter always returns
/// SOME tree; the only `Err` here is `lang::parse`'s own defensive
/// `ParseFailed`/`Language` variants.
pub fn outline_toml(source: &[u8]) -> Result<YamlOutline> {
    let (tree, _language) = lang::parse("toml", source)?;
    let mut w = Walker {
        source,
        symbols: Vec::new(),
        capped: false,
    };
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let doc = tree.root_node();
    let mut cursor = doc.walk();
    for child in doc.named_children(&mut cursor).collect::<Vec<_>>() {
        if w.capped {
            break;
        }
        match child.kind() {
            "pair" => w.walk_toml_pair(child, Vec::new(), None, &mut seen),
            "table" => w.walk_toml_table(child, false, &mut seen),
            "table_array_element" => w.walk_toml_table(child, true, &mut seen),
            _ => {}
        }
    }
    Ok(YamlOutline {
        symbols: w.symbols,
        capped: w.capped,
    })
}

impl Walker<'_> {
    /// A `[table]` or `[[array_of_tables]]` node: tree-sitter-toml-ng gives
    /// neither node a `key`/`value` field (see the module doc) — its FIRST
    /// named child is the header key (`bare_key`/`quoted_key`/`dotted_key`),
    /// every remaining `pair` child belongs to that section.
    fn walk_toml_table(&mut self, table: Node<'_>, is_array: bool, seen: &mut BTreeSet<String>) {
        if self.capped {
            return;
        }
        let mut cursor = table.walk();
        let mut children = table
            .named_children(&mut cursor)
            .collect::<Vec<_>>()
            .into_iter();
        let Some(header) = children.next() else {
            return;
        };
        let Some(mut header_text) = toml_key_text(header, self.source) else {
            return;
        };
        if is_array {
            header_text.push_str("[]");
        }
        self.emit(table, &header_text, None, seen);
        let path = vec![header_text.clone()];
        if self.capped || path.len() >= MAX_DEPTH {
            return;
        }
        let container = Some(header_text);
        for pair in children.filter(|n| n.kind() == "pair") {
            if self.capped {
                return;
            }
            self.walk_toml_pair(pair, path.clone(), container.clone(), seen);
        }
    }

    fn walk_toml_pair(
        &mut self,
        pair: Node<'_>,
        path: Vec<String>,
        container: Option<String>,
        seen: &mut BTreeSet<String>,
    ) {
        if self.capped {
            return;
        }
        let mut cursor = pair.walk();
        let children = pair.named_children(&mut cursor).collect::<Vec<_>>();
        let Some(key_node) = children.first().copied() else {
            return;
        };
        let Some(key_text) = toml_key_text(key_node, self.source) else {
            return;
        };
        let mut child_path = path;
        child_path.push(key_text);
        let full_path = child_path.join(".");
        self.emit(pair, &full_path, container.clone(), seen);

        if self.capped || child_path.len() >= MAX_DEPTH {
            return;
        }
        let Some(value_node) = children.get(1).copied() else {
            return;
        };
        let child_container = Some(full_path);
        self.walk_toml_value(value_node, child_path, child_container, seen);
    }

    fn walk_toml_value(
        &mut self,
        node: Node<'_>,
        path: Vec<String>,
        container: Option<String>,
        seen: &mut BTreeSet<String>,
    ) {
        if self.capped {
            return;
        }
        match node.kind() {
            "inline_table" => {
                let mut cursor = node.walk();
                for pair in node
                    .named_children(&mut cursor)
                    .filter(|n| n.kind() == "pair")
                    .collect::<Vec<_>>()
                {
                    if self.capped {
                        return;
                    }
                    self.walk_toml_pair(pair, path.clone(), container.clone(), seen);
                }
            }
            "array" => {
                if path.is_empty() {
                    return; // no preceding key to hang `[]` off of
                }
                let mut seq_path = path;
                if let Some(last) = seq_path.last_mut() {
                    last.push_str("[]");
                }
                if seq_path.len() > MAX_DEPTH {
                    return;
                }
                let item_container = Some(seq_path.join("."));
                let mut cursor = node.walk();
                for item in node.named_children(&mut cursor).collect::<Vec<_>>() {
                    if self.capped {
                        return;
                    }
                    if item.kind() != "inline_table" {
                        continue; // scalar/nested-array items: no further rows
                    }
                    let mut ic = item.walk();
                    for pair in item
                        .named_children(&mut ic)
                        .filter(|n| n.kind() == "pair")
                        .collect::<Vec<_>>()
                    {
                        if self.capped {
                            return;
                        }
                        self.walk_toml_pair(pair, seq_path.clone(), item_container.clone(), seen);
                    }
                }
            }
            _ => {}
        }
    }
}

/// A TOML key node's text: `bare_key`/`quoted_key` are leaves (quoted keys
/// stripped of one layer of matching quotes); `dotted_key` recurses over
/// its own named children and joins their texts with `.` — this correctly
/// flattens the key regardless of whether the grammar represents `a.b.c` as
/// a flat 3-child node or a nested binary tree (both shapes produce the
/// same `["a", "b", "c"]` segment order via recursion), so kb-code never
/// has to assume one shape over the other.
fn toml_key_text(node: Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "bare_key" => {
            let raw = node.utf8_text(source).ok()?.trim();
            (!raw.is_empty()).then(|| raw.to_string())
        }
        "quoted_key" => {
            let raw = node.utf8_text(source).ok()?.trim();
            let text = raw
                .strip_prefix(['\'', '"'])
                .and_then(|s| s.strip_suffix(['\'', '"']))
                .unwrap_or(raw);
            (!text.is_empty()).then(|| text.to_string())
        }
        "dotted_key" => {
            let mut cursor = node.walk();
            let segments: Vec<String> = node
                .named_children(&mut cursor)
                .filter_map(|c| toml_key_text(c, source))
                .collect();
            (!segments.is_empty()).then(|| segments.join("."))
        }
        _ => None,
    }
}

// ============================================================================
// JSON
// ============================================================================

/// Parse `source` as JSON and walk its CST into a key-path outline. Never
/// errors on malformed JSON content itself — same tree-sitter tolerance as
/// `outline_toml`/`yaml::outline`.
pub fn outline_json(source: &[u8]) -> Result<YamlOutline> {
    let (tree, _language) = lang::parse("json", source)?;
    let mut w = Walker {
        source,
        symbols: Vec::new(),
        capped: false,
    };
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let doc = tree.root_node();
    let mut cursor = doc.walk();
    for child in doc.named_children(&mut cursor).collect::<Vec<_>>() {
        if w.capped {
            break;
        }
        w.walk_json_value(child, Vec::new(), None, &mut seen);
    }
    Ok(YamlOutline {
        symbols: w.symbols,
        capped: w.capped,
    })
}

impl Walker<'_> {
    fn walk_json_value(
        &mut self,
        node: Node<'_>,
        path: Vec<String>,
        container: Option<String>,
        seen: &mut BTreeSet<String>,
    ) {
        if self.capped {
            return;
        }
        match node.kind() {
            "object" => self.walk_json_object(node, path, container, seen),
            "array" => {
                if path.is_empty() {
                    return; // bare top-level array: nothing to name it
                }
                let mut seq_path = path;
                if let Some(last) = seq_path.last_mut() {
                    last.push_str("[]");
                }
                if seq_path.len() > MAX_DEPTH {
                    return;
                }
                let item_container = Some(seq_path.join("."));
                let mut cursor = node.walk();
                for item in node.named_children(&mut cursor).collect::<Vec<_>>() {
                    if self.capped {
                        return;
                    }
                    if item.kind() == "object" {
                        self.walk_json_object(item, seq_path.clone(), item_container.clone(), seen);
                    }
                    // A non-object item (scalar, nested array) contributes
                    // no further rows — see the module doc.
                }
            }
            _ => {}
        }
    }

    fn walk_json_object(
        &mut self,
        obj: Node<'_>,
        path: Vec<String>,
        container: Option<String>,
        seen: &mut BTreeSet<String>,
    ) {
        let mut cursor = obj.walk();
        for pair in obj
            .named_children(&mut cursor)
            .filter(|n| n.kind() == "pair")
            .collect::<Vec<_>>()
        {
            if self.capped {
                return;
            }
            let Some(key_node) = pair.child_by_field_name("key") else {
                continue;
            };
            let Some(key_text) = json_key_text(key_node, self.source) else {
                continue;
            };
            let mut child_path = path.clone();
            child_path.push(key_text);
            let full_path = child_path.join(".");
            self.emit(pair, &full_path, container.clone(), seen);

            if self.capped || child_path.len() >= MAX_DEPTH {
                continue;
            }
            let Some(value_node) = pair.child_by_field_name("value") else {
                continue;
            };
            let child_container = Some(full_path.clone());
            self.walk_json_value(value_node, child_path, child_container, seen);
        }
    }
}

/// A JSON object key: always a `string` node (`{"key": ...}` — JSON has no
/// bare/single-quoted keys), stripped of its surrounding `"..."`. Not a full
/// unescaper — see the module doc.
fn json_key_text(key_node: Node<'_>, source: &[u8]) -> Option<String> {
    let raw = key_node.utf8_text(source).ok()?.trim();
    let text = raw
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(raw);
    (!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(o: &YamlOutline) -> Vec<(&str, Option<&str>)> {
        o.symbols
            .iter()
            .map(|s| (s.name.as_str(), s.container.as_deref()))
            .collect()
    }

    // --- TOML ---------------------------------------------------------

    const CARGO_TOML_FIXTURE: &str = r#"[package]
name = "kb-code-server"
version = "0.0.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }

[[bin]]
name = "kb-code-server"
path = "src/main.rs"

[[bin]]
name = "kb-code-cli"
path = "src/cli.rs"

[dev-dependencies]
tempfile = "3"
"#;

    #[test]
    fn cargo_toml_style_fixture_yields_dotted_key_paths_incl_array_of_tables() {
        let o = outline_toml(CARGO_TOML_FIXTURE.as_bytes()).unwrap();
        assert!(!o.capped);
        assert!(o.symbols.iter().all(|s| s.kind == "key"));
        assert_eq!(
            rows(&o),
            vec![
                ("package", None),
                ("package.name", Some("package")),
                ("package.version", Some("package")),
                ("package.edition", Some("package")),
                ("dependencies", None),
                ("dependencies.serde", Some("dependencies")),
                ("dependencies.serde.version", Some("dependencies.serde")),
                ("dependencies.serde.features", Some("dependencies.serde")),
                ("bin[]", None),
                ("bin[].name", Some("bin[]")),
                ("bin[].path", Some("bin[]")),
                ("dev-dependencies", None),
                ("dev-dependencies.tempfile", Some("dev-dependencies")),
            ],
            "got: {:#?}",
            o.symbols
        );
        // The second [[bin]] repeats name/path — deduped to one row each.
        assert_eq!(o.symbols.len(), 13);
    }

    #[test]
    fn dotted_table_header_is_one_row_not_synthesized_intermediates() {
        let o = outline_toml(b"[a.b.c]\nleaf = 1\n").unwrap();
        assert_eq!(
            rows(&o),
            vec![("a.b.c", None), ("a.b.c.leaf", Some("a.b.c"))]
        );
    }

    #[test]
    fn dotted_pair_key_produces_one_dotted_row() {
        let o = outline_toml(b"authors.email = \"a@b.com\"\n").unwrap();
        assert_eq!(rows(&o), vec![("authors.email", None)]);
    }

    #[test]
    fn plain_array_of_scalars_does_not_descend() {
        let o = outline_toml(b"tags = [\"a\", \"b\", \"c\"]\n").unwrap();
        assert_eq!(rows(&o), vec![("tags", None)]);
    }

    #[test]
    fn array_of_inline_tables_contributes_bracket_rows() {
        let o = outline_toml(b"points = [{ x = 1, y = 2 }, { x = 3, y = 4 }]\n").unwrap();
        assert_eq!(
            rows(&o),
            vec![
                ("points", None),
                ("points[].x", Some("points[]")),
                ("points[].y", Some("points[]")),
            ]
        );
    }

    #[test]
    fn toml_six_hundred_key_synthetic_hits_the_cap_flag() {
        let mut src = String::new();
        for i in 0..600 {
            src.push_str(&format!("k{i} = {i}\n"));
        }
        let o = outline_toml(src.as_bytes()).unwrap();
        assert_eq!(o.symbols.len(), MAX_KEYS);
        assert!(o.capped);
    }

    #[test]
    fn toml_empty_source_yields_no_symbols() {
        let o = outline_toml(b"").unwrap();
        assert_eq!(o.symbols, vec![]);
        assert!(!o.capped);
    }

    // --- JSON -----------------------------------------------------------

    const JSON_FIXTURE: &str = r#"{
  "name": "kb-code",
  "version": "0.0.0",
  "metadata": {
    "author": "kb",
    "flags": {
      "debug": true
    }
  },
  "servers": [
    { "host": "a", "port": 1 },
    { "host": "b", "port": 2 }
  ],
  "tags": ["x", "y"]
}
"#;

    #[test]
    fn json_fixture_yields_nested_dotted_paths_with_brackets_for_object_arrays() {
        let o = outline_json(JSON_FIXTURE.as_bytes()).unwrap();
        assert!(!o.capped);
        assert!(o.symbols.iter().all(|s| s.kind == "key"));
        assert_eq!(
            rows(&o),
            vec![
                ("name", None),
                ("version", None),
                ("metadata", None),
                ("metadata.author", Some("metadata")),
                ("metadata.flags", Some("metadata")),
                ("metadata.flags.debug", Some("metadata.flags")),
                ("servers", None),
                ("servers[].host", Some("servers[]")),
                ("servers[].port", Some("servers[]")),
                ("tags", None),
            ],
            "got: {:#?}",
            o.symbols
        );
    }

    #[test]
    fn json_bare_top_level_array_yields_no_rows() {
        let o = outline_json(b"[{\"a\": 1}, {\"b\": 2}]").unwrap();
        assert_eq!(o.symbols, vec![]);
    }

    #[test]
    fn json_plain_array_of_scalars_does_not_descend() {
        let o = outline_json(b"{\"tags\": [1, 2, 3]}").unwrap();
        assert_eq!(rows(&o), vec![("tags", None)]);
    }

    #[test]
    fn json_six_hundred_key_synthetic_hits_the_cap_flag() {
        let mut src = String::from("{");
        for i in 0..600 {
            if i > 0 {
                src.push(',');
            }
            src.push_str(&format!("\"k{i}\": {i}"));
        }
        src.push('}');
        let o = outline_json(src.as_bytes()).unwrap();
        assert_eq!(o.symbols.len(), MAX_KEYS);
        assert!(o.capped);
    }

    #[test]
    fn json_max_depth_stops_descent_but_still_emits_the_capped_row() {
        let depth = 12;
        let mut src = String::new();
        for _ in 0..depth {
            src.push_str("{\"a\":");
        }
        src.push('1');
        for _ in 0..depth {
            src.push('}');
        }
        let o = outline_json(src.as_bytes()).unwrap();
        assert!(!o.capped, "depth capping is not the same as the key cap");
        let max_segments = o
            .symbols
            .iter()
            .map(|s| s.name.matches('.').count() + 1)
            .max()
            .unwrap();
        assert_eq!(max_segments, MAX_DEPTH);
        assert_eq!(o.symbols.len(), MAX_DEPTH);
    }

    #[test]
    fn json_empty_object_yields_no_symbols() {
        let o = outline_json(b"{}").unwrap();
        assert_eq!(o.symbols, vec![]);
        assert!(!o.capped);
    }
}
