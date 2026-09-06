//! YAML "key-path outline" — kb-code's YAML symbol extraction
//! (kb-code-server W2.2). YAML has no `tags.scm` concept upstream
//! (tree-sitter-yaml bundles only a `HIGHLIGHTS_QUERY` — see `lang.rs`'s
//! `tags_query` doc) and there's no function/class/interface vocabulary to
//! tag in the first place — ADR-7's decision is that YAML isn't a "tags"
//! language at all. Instead this module walks the tree-sitter-yaml CST
//! directly and emits one `extract::Symbol` per BLOCK-MAPPING KEY,
//! `kind = "key"`, `name` = the full DOTTED PATH from the document root
//! (e.g. `spec.template.spec.containers[].image`) — a hierarchical outline
//! a browsing UI can render as a tree, and `GET /api/symbols?q=` can
//! substring-search over for free (same `symbols` table, same route).
//!
//! ## Path grammar
//!
//! - Each nested `block_mapping_pair` extends the path with `.{key}`.
//! - A sequence value contributes a literal `[]` segment — appended to the
//!   PRECEDING key, not a new dotted segment — ONLY when at least one of
//!   its items is itself a mapping: `containers:` followed by a list of
//!   scalars produces no rows beyond the `containers` row itself; a list of
//!   mappings produces `containers[].<child-key>` rows. Items are NOT
//!   indexed by position (`[0]`/`[1]`): every item shares the same `[]`
//!   segment, so two sibling mapping-items with the same key produce the
//!   SAME path (deduped — see below). Nested sequences-of-sequences are
//!   not descended into further (uncommon in practice, out of scope here).
//! - A bare top-level sequence (a document that's just a list, no
//!   preceding key to hang `[]` off of) contributes no rows at all.
//!
//! ## Multi-document files
//!
//! One `---`-separated YAML `stream` can hold several `document`s. Each
//! document gets its own outline: the DEDUP set (below) is scoped PER
//! DOCUMENT, so two documents with identical shapes (e.g. two Kubernetes
//! `Deployment` manifests concatenated in one file, a very common pattern)
//! each produce their own full row set rather than the second being
//! silently swallowed by the first's dedup. For a file with MORE THAN ONE
//! document, every TOP-LEVEL key's `container` is stamped `doc[N]`
//! (0-based) instead of `None`, so a flat listing still shows which
//! document a row belongs to; the `name` (the dotted path itself) stays a
//! clean, doc-index-free string, and nested keys keep their normal
//! parent-path container unaffected by the doc marker.
//!
//! ## Cardinality caps
//!
//! - `MAX_DEPTH` (8): a path that has already reached 8 dot/bracket
//!   segments is still emitted, but its children are not — descent stops
//!   there. Never trips on realistic hand-written YAML (a k8s Deployment
//!   manifest tops out around 6); exists for pathological/generated input.
//! - `MAX_KEYS` (500): once 500 rows have been emitted (across the whole
//!   file, all documents), extraction stops early. `YamlOutline::capped`
//!   reports whether the cap was hit; this is YAML-specific (not threaded
//!   into the shared `Vec<Symbol>` return type `extract::extract_symbols`
//!   exposes, since that interface is uniform across all eight languages) —
//!   a future caller that wants to warn on a pathologically large YAML file
//!   can call `outline` directly instead of going through
//!   `extract::extract_symbols`.
//! - Repeated paths (within one document's dedup scope) collapse to one
//!   row — see "Path grammar" above. Recursion still happens into every
//!   sequence item's mapping regardless of whether ITS OWN row was a
//!   dedup-skip: a later item can have keys an earlier item didn't, and
//!   those child paths are still new.
//!
//! ## `kind = "key"` and the repo-map exclusion
//!
//! YAML key-path rows share the `symbols` table with real code symbols
//! (`fn`/`class`/...) so `/api/symbols` and its substring search work on
//! them for free — but they are NOT code symbols, and a future "repo map"
//! feature (a compact per-file symbol skeleton for agent context) should
//! exclude them. Rather than a new schema column, the exclusion rule is
//! KIND-based: `extract::is_repo_map_symbol` returns `false` for
//! `kind == "key"` (currently the only excluded kind) — see that function's
//! doc for why a kind-based filter is the "simplest correct thing" here (no
//! migration, no new nullable column, no risk of a hand-constructed row
//! drifting out of sync with a separate boolean flag).

use crate::extract::Symbol;
use crate::lang::{self, LangError};
use std::collections::BTreeSet;
use tree_sitter::Node;

pub type Result<T> = std::result::Result<T, LangError>;

const MAX_DEPTH: usize = 8;
const MAX_KEYS: usize = 500;

/// The result of one [`outline`] call — `symbols` is what
/// `extract::extract_symbols` exposes to the rest of the pipeline;
/// `capped` is YAML-specific (see the module doc's "Cardinality caps").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YamlOutline {
    pub symbols: Vec<Symbol>,
    pub capped: bool,
}

/// Parse `source` as YAML and walk its CST into a key-path outline. Never
/// errors on malformed YAML content itself — tree-sitter always returns
/// SOME tree, error nodes and all (the same tolerance `extract_symbols`
/// relies on for a broken Bash fixture); the only `Err` here is
/// `lang::parse`'s own defensive `ParseFailed`/`Language` variants, which
/// don't occur in practice for a registered grammar.
pub fn outline(source: &[u8]) -> Result<YamlOutline> {
    let (tree, _language) = lang::parse("yaml", source)?;
    let mut w = Walker {
        source,
        symbols: Vec::new(),
        capped: false,
    };
    let stream = tree.root_node();
    let mut stream_cursor = stream.walk();
    let documents: Vec<Node<'_>> = stream
        .named_children(&mut stream_cursor)
        .filter(|n| n.kind() == "document")
        .collect();
    let multi_doc = documents.len() > 1;
    for (doc_index, doc) in documents.into_iter().enumerate() {
        if w.capped {
            break;
        }
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let root_container = multi_doc.then(|| format!("doc[{doc_index}]"));
        let mut doc_cursor = doc.walk();
        for child in doc.named_children(&mut doc_cursor).collect::<Vec<_>>() {
            w.walk_value(child, Vec::new(), root_container.clone(), &mut seen);
        }
    }
    Ok(YamlOutline {
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
    /// Unwrap `node` (a `document` child, a mapping-pair value, or a
    /// sequence item) down to its real shape and dispatch: a mapping
    /// extends `path` directly (`walk_mapping`); a sequence extends `path`
    /// with a `[]` segment on its LAST element and descends into any item
    /// that is itself a mapping (see the module doc's "Path grammar");
    /// anything else (scalar, alias, ...) is a leaf — nothing to walk.
    fn walk_value(
        &mut self,
        node: Node<'_>,
        path: Vec<String>,
        container: Option<String>,
        seen: &mut BTreeSet<String>,
    ) {
        if self.capped {
            return;
        }
        let Some(inner) = unwrap_value(node) else {
            return;
        };
        match inner.kind() {
            "block_mapping" | "flow_mapping" => self.walk_mapping(inner, path, container, seen),
            "block_sequence" | "flow_sequence" => {
                // No preceding key to hang `[]` off of (a bare top-level
                // list) — nothing to name, nothing to walk.
                if path.is_empty() {
                    return;
                }
                let mut seq_path = path;
                if let Some(last) = seq_path.last_mut() {
                    last.push_str("[]");
                }
                if seq_path.len() > MAX_DEPTH {
                    return;
                }
                let mut cursor = inner.walk();
                for item in inner.named_children(&mut cursor).collect::<Vec<_>>() {
                    if self.capped {
                        return;
                    }
                    let item_value = match item.kind() {
                        "block_sequence_item" => item.named_child(0),
                        // `flow_sequence`'s own children are `flow_node`/
                        // `flow_pair` directly, no wrapping "item" node.
                        _ => Some(item),
                    };
                    let Some(item_value) = item_value else {
                        continue;
                    };
                    let Some(item_inner) = unwrap_value(item_value) else {
                        continue;
                    };
                    if matches!(item_inner.kind(), "block_mapping" | "flow_mapping") {
                        // The container for THIS item's own pairs is the
                        // bracketed sequence path itself (`seq_path`), NOT
                        // the stale `container` this fn was called with —
                        // that was the container of the SEQUENCE KEY
                        // (`containers`'s own parent), one level too high
                        // now that we've descended past the `[]` segment.
                        let item_container = Some(seq_path.join("."));
                        self.walk_mapping(item_inner, seq_path.clone(), item_container, seen);
                    }
                    // A non-mapping item (scalar, nested sequence, ...)
                    // contributes no further path segments — see the
                    // module doc.
                }
            }
            _ => {}
        }
    }

    fn walk_mapping(
        &mut self,
        mapping: Node<'_>,
        path: Vec<String>,
        container: Option<String>,
        seen: &mut BTreeSet<String>,
    ) {
        let mut cursor = mapping.walk();
        for pair in mapping
            .named_children(&mut cursor)
            .filter(|n| matches!(n.kind(), "block_mapping_pair" | "flow_pair"))
            .collect::<Vec<_>>()
        {
            if self.capped {
                return;
            }
            let Some(key_node) = pair.child_by_field_name("key") else {
                continue;
            };
            let Some(key_text) = scalar_text(key_node, self.source) else {
                continue;
            };
            let mut child_path = path.clone();
            child_path.push(key_text);
            let full_path = child_path.join(".");
            self.emit(&pair, &full_path, container.clone(), seen);

            // Depth cap: emit this row, but don't descend past MAX_DEPTH
            // segments — see the module doc's "Cardinality caps".
            if self.capped || child_path.len() >= MAX_DEPTH {
                continue;
            }
            let Some(value_node) = pair.child_by_field_name("value") else {
                continue;
            };
            let child_container = Some(full_path.clone());
            self.walk_value(value_node, child_path, child_container, seen);
        }
    }

    fn emit(
        &mut self,
        pair: &Node<'_>,
        full_path: &str,
        container: Option<String>,
        seen: &mut BTreeSet<String>,
    ) {
        if self.symbols.len() >= MAX_KEYS {
            self.capped = true;
            return;
        }
        if !seen.insert(full_path.to_string()) {
            return; // duplicate path within this document — see module doc
        }
        let start = pair.start_position();
        let end = pair.end_position();
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

/// A `block_node`/`flow_node` wrapper carries decorator children (`anchor`/
/// `tag`) alongside the real value — return the first child that IS the
/// real value (mapping/sequence/scalar/alias), skipping decorators. `node`
/// is returned unchanged if it's already an unwrapped shape (every call
/// site above only ever passes a `block_node`/`flow_node` in practice,
/// since that's the only shape a `key`/`value` field or sequence-item child
/// holds, but this stays defensive rather than assuming it).
fn unwrap_value(node: Node<'_>) -> Option<Node<'_>> {
    match node.kind() {
        "block_node" | "flow_node" => {
            let mut cursor = node.walk();
            let found = node
                .named_children(&mut cursor)
                .find(|c| !matches!(c.kind(), "anchor" | "tag" | "comment"));
            found
        }
        _ => Some(node),
    }
}

/// Extract a mapping key's TEXT: unwrap down to the innermost scalar and
/// return its raw source text, trimmed and (for single/double-quoted keys)
/// stripped of exactly one layer of matching quotes. This is NOT a full
/// YAML scalar unescaper (no `\n`-escape decoding, no flow-mapping-key
/// object support) — kb-code's outline is a browsing aid over the key
/// SHAPE, not a YAML value parser; see the module doc.
fn scalar_text(key_field: Node<'_>, source: &[u8]) -> Option<String> {
    let inner = unwrap_value(key_field)?;
    let raw = inner.utf8_text(source).ok()?.trim();
    let text = match inner.kind() {
        "single_quote_scalar" | "double_quote_scalar" | "string_scalar" => raw
            .strip_prefix(['\'', '"'])
            .and_then(|s| s.strip_suffix(['\'', '"']))
            .unwrap_or(raw)
            .to_string(),
        _ => raw.to_string(),
    };
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
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

    const K8S_FIXTURE: &str = r#"apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
  labels:
    app: web
spec:
  replicas: 3
  selector:
    matchLabels:
      app: web
  template:
    metadata:
      labels:
        app: web
    spec:
      containers:
        - name: app
          image: nginx:1.25
          ports:
            - containerPort: 80
        - name: sidecar
          image: envoy:1.28
"#;

    #[test]
    fn k8s_manifest_fixture_yields_nested_dotted_key_paths_with_caps_respected() {
        let o = outline(K8S_FIXTURE.as_bytes()).unwrap();
        assert!(!o.capped, "a realistic manifest must never trip the caps");
        assert!(o.symbols.iter().all(|s| s.kind == "key"));
        assert_eq!(
            rows(&o),
            vec![
                ("apiVersion", None),
                ("kind", None),
                ("metadata", None),
                ("metadata.name", Some("metadata")),
                ("metadata.labels", Some("metadata")),
                ("metadata.labels.app", Some("metadata.labels")),
                ("spec", None),
                ("spec.replicas", Some("spec")),
                ("spec.selector", Some("spec")),
                ("spec.selector.matchLabels", Some("spec.selector")),
                (
                    "spec.selector.matchLabels.app",
                    Some("spec.selector.matchLabels")
                ),
                ("spec.template", Some("spec")),
                ("spec.template.metadata", Some("spec.template")),
                (
                    "spec.template.metadata.labels",
                    Some("spec.template.metadata")
                ),
                (
                    "spec.template.metadata.labels.app",
                    Some("spec.template.metadata.labels")
                ),
                ("spec.template.spec", Some("spec.template")),
                ("spec.template.spec.containers", Some("spec.template.spec")),
                (
                    "spec.template.spec.containers[].name",
                    Some("spec.template.spec.containers[]")
                ),
                (
                    "spec.template.spec.containers[].image",
                    Some("spec.template.spec.containers[]")
                ),
                (
                    "spec.template.spec.containers[].ports",
                    Some("spec.template.spec.containers[]")
                ),
                (
                    "spec.template.spec.containers[].ports[].containerPort",
                    Some("spec.template.spec.containers[].ports[]")
                ),
            ],
            "got: {:#?}",
            o.symbols
        );
        // The second `containers` item repeats `name`/`image` — deduped to
        // one row each (21 total, not 23).
        assert_eq!(o.symbols.len(), 21);
    }

    #[test]
    fn sequence_of_scalars_does_not_descend() {
        let o = outline(b"tags:\n  - a\n  - b\n  - c\n").unwrap();
        assert_eq!(rows(&o), vec![("tags", None)]);
    }

    #[test]
    fn three_document_file_yields_per_doc_outlines() {
        let src = "---\nname: first\nvalue: 1\n---\nname: second\nvalue: 2\n---\nname: third\nnested:\n  x: 1\n";
        let o = outline(src.as_bytes()).unwrap();
        assert!(!o.capped);
        assert_eq!(
            rows(&o),
            vec![
                ("name", Some("doc[0]")),
                ("value", Some("doc[0]")),
                ("name", Some("doc[1]")),
                ("value", Some("doc[1]")),
                ("name", Some("doc[2]")),
                ("nested", Some("doc[2]")),
                ("nested.x", Some("nested")),
            ]
        );
    }

    #[test]
    fn six_hundred_key_synthetic_hits_the_cap_flag() {
        let mut src = String::new();
        for i in 0..600 {
            src.push_str(&format!("k{i}: {i}\n"));
        }
        let o = outline(src.as_bytes()).unwrap();
        assert_eq!(o.symbols.len(), MAX_KEYS);
        assert!(o.capped, "600 unique top-level keys must trip the cap");
    }

    #[test]
    fn max_depth_stops_descent_but_still_emits_the_capped_row() {
        let depth = 12;
        let mut lines = Vec::new();
        for i in 0..depth {
            lines.push(format!("{}a{i}:", "  ".repeat(i)));
        }
        lines.push(format!("{}leaf: 1", "  ".repeat(depth)));
        let src = lines.join("\n") + "\n";
        let o = outline(src.as_bytes()).unwrap();
        assert!(!o.capped, "depth capping is not the same as the key cap");
        let max_segments = o
            .symbols
            .iter()
            .map(|s| s.name.matches('.').count() + 1)
            .max()
            .unwrap();
        assert_eq!(max_segments, MAX_DEPTH);
        assert_eq!(o.symbols.len(), MAX_DEPTH);
        assert!(!o.symbols.iter().any(|s| s.name.contains("leaf")));
    }

    #[test]
    fn empty_source_yields_no_symbols() {
        let o = outline(b"").unwrap();
        assert_eq!(o.symbols, vec![]);
        assert!(!o.capped);
    }
}
