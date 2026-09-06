//! `comments/1` (V72-J1, design D8) — the comment index.
//!
//! One scanner over every comment in a file, classified into the D8
//! taxonomy ([`classify::CommentKind`]), with a configurable annotation
//! keyword grammar ([`keywords`]) and a per-request, never-persisted drift
//! oracle ([`drift`]). It SUBSUMES the Phase-N TODO index: `todo_items`,
//! `extract::extract_todos` and `Store::replace_todo_items` are gone, and
//! `GET /api/todos` is now a filtered VIEW over this table (see
//! [`keywords::TODO_FAMILY`] and `Store::list_todo_items`). There is
//! exactly one scanner over these lines.
//!
//! ## What is stored and what is not
//!
//! STORED (per file, replaced wholesale on re-extraction): the block's
//! range, its kind, its keyword and parsed smart_todo fields, its
//! directive tool and whether that directive carried a reason, the
//! preceding symbol for a `doc` block, and the block's own text bounded at
//! [`COMMENT_TEXT_CAP_BYTES`]. These are CLAIMS about bytes, in the shape
//! `entities/1` uses (kb-code-server invariant 13).
//!
//! NEVER STORED: `doc_state`. The drift oracle is arithmetic over `git
//! blame` computed per request ([`drift`]), for the same reason the
//! doc↔code bridge computes its trust classes per request — a cached
//! freshness verdict is a lie the moment the next commit lands. It is also
//! never a VERDICT: `drifted` reports "the code under this comment changed
//! and the comment did not", with the two commits and the gap, and says
//! nothing about whether the comment is wrong. That judgement is an
//! agent-layer step (root CLAUDE.md's no-in-daemon-LLM non-goal).
//!
//! ## The row key includes this module's own version
//!
//! Rows are keyed `(repo_id, path, blob_sha, comments_version)` where
//! `comments_version` is [`COMMENTS_GRAMMAR_VERSION`] joined to the
//! FILE's language salt ([`comments_version_for`]). Two independent things
//! can invalidate a classification — this module's taxonomy rules, and the
//! tree-sitter grammar underneath them — and both are in the key, so a
//! bump to either re-extracts instead of serving a stale kind forever
//! (kb-code-server invariant 11's stale-salt lesson, applied to a
//! path-keyed table).

pub mod classify;
pub mod drift;
pub mod keywords;
pub mod routes;

pub use classify::CommentKind;
pub use keywords::{KeywordSet, SmartTodoFields};

use crate::extract::Symbol;
use crate::lang::{self, LangError};

/// This module's own taxonomy version. Bump it whenever a classification
/// RULE changes — it is half of the per-row `comments_version` key, so a
/// bump re-extracts every file instead of leaving old kinds in place.
pub const COMMENTS_GRAMMAR_VERSION: &str = "comments@1";

/// Per-block stored-text cap, in bytes. A block over the cap is truncated
/// on a char boundary and flagged `text_truncated` — never silently cut.
pub const COMMENT_TEXT_CAP_BYTES: usize = 2048;

/// Per-file ceiling on indexed blocks. A ~5k-file monolith carries tens of
/// thousands of comments; a single pathological generated file must not be
/// able to dominate the table. Hit ⇒ the tail is dropped and the file's
/// row set reports `truncated` through
/// [`CommentExtraction::truncated`].
pub const MAX_BLOCKS_PER_FILE: usize = 2_000;

/// The `comments_version` stored on every row for one file.
///
/// THREE things can change a classification and all three are in the key:
/// this module's taxonomy ([`COMMENTS_GRAMMAR_VERSION`]), the tree-sitter
/// grammar the parse ran on (`lang_salt`), and the operator's own
/// annotation vocabulary (`[comments] keywords`, folded in as a short
/// fingerprint). Leaving the keyword set out was the near-miss here: an
/// operator adding `DEBT` to the list would otherwise have seen every
/// already-indexed file keep its old, `DEBT`-blind rows forever.
pub fn comments_version_for(lang_salt: &str, keywords: &KeywordSet) -> String {
    format!(
        "{COMMENTS_GRAMMAR_VERSION}+{}+{lang_salt}",
        keywords.fingerprint()
    )
}

/// Map one extracted block onto its storage row.
pub fn to_new_comment(b: &CommentBlock) -> crate::store::NewComment {
    crate::store::NewComment {
        ordinal: i64::from(b.ordinal),
        kind: b.kind.as_str().to_string(),
        keyword: b.keyword.clone(),
        keyword_text: b.keyword_text.clone(),
        // A bag that fails to serialise is dropped, never stored half-
        // formed — the row still carries its keyword and range.
        fields_json: b
            .fields
            .as_ref()
            .and_then(|f| serde_json::to_string(f).ok()),
        line_start: i64::from(b.line_start),
        line_end: i64::from(b.line_end),
        text: b.text.clone(),
        text_truncated: b.text_truncated,
        symbol_name: b.symbol.as_ref().map(|s| s.name.clone()),
        symbol_kind: b.symbol.as_ref().map(|s| s.kind.clone()),
        symbol_line_start: b.symbol.as_ref().map(|s| i64::from(s.line_start)),
        symbol_line_end: b.symbol.as_ref().map(|s| i64::from(s.line_end)),
        directive_tool: b.directive_tool.clone(),
        directive_has_reason: b.directive_has_reason,
    }
}

/// The symbol a `doc` block sits immediately above.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocSymbol {
    pub name: String,
    pub kind: String,
    /// 1-based, inclusive — the documented body, and the range the drift
    /// oracle blames against.
    pub line_start: u32,
    pub line_end: u32,
}

/// One classified comment block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentBlock {
    /// Stable emission order (line ascending) — half of the row key, not
    /// meaningful on its own. Same contract as `extract::Symbol::ordinal`.
    pub ordinal: u32,
    pub kind: CommentKind,
    /// 1-based, inclusive.
    pub line_start: u32,
    pub line_end: u32,
    /// The block's own text, comment sigils stripped, lines joined with
    /// `\n`, bounded at [`COMMENT_TEXT_CAP_BYTES`].
    pub text: String,
    pub text_truncated: bool,
    /// `Some` only for [`CommentKind::Annotation`].
    pub keyword: Option<String>,
    /// The annotation's own trailing text — byte-identical to what the
    /// deleted `extract_todos` produced, because `GET /api/todos`'s `text`
    /// field IS this string.
    pub keyword_text: Option<String>,
    pub fields: Option<SmartTodoFields>,
    /// `Some` only for [`CommentKind::Doc`].
    pub symbol: Option<DocSymbol>,
    /// `Some` only for [`CommentKind::Directive`].
    pub directive_tool: Option<String>,
    /// `Some` only for a SUPPRESSION directive — a magic comment carries a
    /// value, not a justification, so asking whether it has a reason is a
    /// category error and the field stays `None` (never a misleading
    /// `false` that the audit lane would then report).
    pub directive_has_reason: Option<bool>,
}

/// One file's extraction result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommentExtraction {
    pub blocks: Vec<CommentBlock>,
    /// [`MAX_BLOCKS_PER_FILE`] was hit and the tail was dropped.
    pub truncated: bool,
}

/// One raw comment LINE, before classification.
struct RawLine {
    /// 1-based.
    line: u32,
    stripped: String,
    /// The line's comment sigil was `#:` (RBS inline types-as-comments).
    sigil_colon: bool,
    /// Nothing but whitespace precedes the comment on its source line. A
    /// TRAILING comment (`x = 1 # why`) is always a run of its own: it
    /// annotates the code beside it, not the comment above it.
    own_line: bool,
}

/// Extract and classify every comment block in `source`.
///
/// `symbols` is the file's already-extracted definition list — used ONLY
/// for `doc` adjacency (a run whose last line sits directly above a
/// definition). Pass an empty slice for a language with no definitions;
/// `doc` is then unreachable and every unclassified run is `prose`, which
/// is the honest answer for a file whose grammar has nothing to document.
pub fn extract_comments(
    lang_id: &str,
    source: &[u8],
    symbols: &[Symbol],
    keywords: &KeywordSet,
) -> Result<CommentExtraction, LangError> {
    let text = String::from_utf8_lossy(source);
    let src_lines: Vec<&str> = text.lines().collect();
    let (tree, _) = lang::parse(lang_id, source)?;

    let mut nodes: Vec<tree_sitter::Node<'_>> = Vec::new();
    collect_comment_nodes(tree.root_node(), &mut nodes);
    nodes.sort_by_key(|n| n.start_byte());

    let mut raws: Vec<RawLine> = Vec::new();
    for node in &nodes {
        let Ok(node_text) = node.utf8_text(source) else {
            continue;
        };
        let start_row = node.start_position().row;
        let start_col = node.start_position().column;
        let first_own_line = src_lines
            .get(start_row)
            .map(|l| {
                let b = l.as_bytes();
                let c = start_col.min(b.len());
                b[..c].iter().all(|x| x.is_ascii_whitespace())
            })
            .unwrap_or(true);
        for (i, raw) in node_text.lines().enumerate() {
            let (stripped, sigil_colon) = strip_sigil(raw);
            // An EMPTY stripped line (a bare `#`, a `/*` framing line) is
            // KEPT: it is what holds a run together. Dropping it — the
            // first shape this pass took — silently split a schema banner
            // (`# == Schema Information`, `#`, `# Table name: …`) into
            // three runs, and only the first one carried the marker that
            // makes the whole thing `generated`.
            raws.push(RawLine {
                line: (start_row + i + 1) as u32,
                stripped,
                sigil_colon,
                own_line: if i == 0 { first_own_line } else { true },
            });
        }
    }
    raws.sort_by_key(|r| r.line);
    raws.dedup_by_key(|r| r.line);

    // --- group into runs of adjacent own-line comment lines --------------
    let mut runs: Vec<Vec<RawLine>> = Vec::new();
    for raw in raws {
        let continues = !runs.is_empty()
            && raw.own_line
            && runs
                .last()
                .and_then(|r| r.last())
                .map(|prev| prev.own_line && prev.line + 1 == raw.line)
                .unwrap_or(false);
        if continues {
            runs.last_mut().expect("non-empty").push(raw);
        } else {
            runs.push(vec![raw]);
        }
    }

    // --- classify --------------------------------------------------------
    let doc_capable = lang::supports_token_level(lang_id);
    let mut blocks: Vec<CommentBlock> = Vec::new();
    let mut truncated = false;
    for run in &runs {
        if blocks.len() >= MAX_BLOCKS_PER_FILE {
            truncated = true;
            break;
        }
        let stripped: Vec<String> = run.iter().map(|r| r.stripped.clone()).collect();
        if stripped.iter().all(|s| s.is_empty()) {
            continue;
        }
        let first_line = run[0].line;
        let last_line = run[run.len() - 1].line;

        let mut kinds: Vec<Option<CommentKind>> = vec![None; run.len()];
        let mut keyword_hits: Vec<Option<keywords::KeywordHit>> =
            (0..run.len()).map(|_| None).collect();
        let mut directives: Vec<Option<(&'static classify::DirectiveSpec, bool)>> =
            vec![None; run.len()];

        // 1. Directives first — the most specific signal, and the only one
        //    that must survive a run-level verdict. A file whose header is
        //    `# frozen_string_literal: true` then an SPDX line must keep
        //    the magic comment as a DIRECTIVE; classifying the whole run
        //    `licence` would hide it from the directives audit forever.
        for i in 0..run.len() {
            if let Some(hit) = classify::directive_of(&stripped[i], run[i].sigil_colon) {
                kinds[i] = Some(CommentKind::Directive);
                directives[i] = Some(hit);
            }
        }
        // 2. Run-level verdicts, over whatever the directive pass left.
        //    The EVIDENCE is the whole run (a marker on any line speaks
        //    for the block); only the unclaimed lines are assigned.
        let claim_rest = |kinds: &mut Vec<Option<CommentKind>>, kind: CommentKind| {
            for k in kinds.iter_mut() {
                if k.is_none() {
                    *k = Some(kind);
                }
            }
        };
        if classify::is_generated(&stripped) {
            claim_rest(&mut kinds, CommentKind::Generated);
        } else if classify::is_licence(first_line, &stripped) {
            claim_rest(&mut kinds, CommentKind::Licence);
        } else {
            // 3. The remaining per-line signals.
            for i in 0..run.len() {
                if kinds[i].is_some() || stripped[i].is_empty() {
                    continue;
                }
                if let Some(hit) = classify::annotation_of(keywords, &stripped[i]) {
                    kinds[i] = Some(CommentKind::Annotation);
                    keyword_hits[i] = Some(hit);
                } else if classify::is_section(&stripped[i]) {
                    kinds[i] = Some(CommentKind::Section);
                }
            }
            // 4. A run NOTHING else claimed may be commented-out code.
            //    Gated on "nothing else claimed" so a banner rule or a
            //    stray `TODO` line inside a block can never turn the
            //    parse probe loose on text that is demonstrably prose.
            if kinds.iter().all(Option::is_none) && classify::looks_like_code(lang_id, &stripped) {
                claim_rest(&mut kinds, CommentKind::CommentedCode);
            }
        }

        // `doc` is a property of the RUN (it sits above a definition), so
        // it fills whatever the per-line pass did not claim.
        let symbol = if doc_capable {
            symbols
                .iter()
                .find(|s| s.line_start == last_line + 1)
                .map(|s| DocSymbol {
                    name: s.name.clone(),
                    kind: s.kind.clone(),
                    line_start: s.line_start,
                    line_end: s.line_end,
                })
        } else {
            None
        };
        let filler = if symbol.is_some() {
            CommentKind::Doc
        } else {
            CommentKind::Prose
        };
        for k in kinds.iter_mut() {
            if k.is_none() {
                *k = Some(filler);
            }
        }

        // --- merge adjacent same-kind lines into blocks ------------------
        //
        // `annotation` and `directive` are NEVER merged with a sibling of
        // their own kind: each carries its own keyword/tool identity, and
        // folding two `# TODO` lines into one block would destroy one of
        // them (and one row of `GET /api/todos` with it).
        let mut i = 0usize;
        while i < run.len() {
            if blocks.len() >= MAX_BLOCKS_PER_FILE {
                truncated = true;
                break;
            }
            let kind = kinds[i].expect("every line classified");
            let mergeable = !matches!(kind, CommentKind::Annotation | CommentKind::Directive);
            let mut j = i;
            if mergeable {
                while j + 1 < run.len() && kinds[j + 1] == Some(kind) {
                    j += 1;
                }
            }
            // A run's own blank framing lines (a bare `#`, a `/*` opener)
            // hold the run TOGETHER — they are what keeps a schema banner
            // one block and what keeps a doc run adjacent to the
            // definition below it — but they are not part of any block's
            // range. Shrink to the first/last line that carries text.
            let (Some(s), Some(e)) = (
                (i..=j).find(|k| !stripped[*k].is_empty()),
                (i..=j).rev().find(|k| !stripped[*k].is_empty()),
            ) else {
                i = j + 1;
                continue;
            };
            let (text, text_truncated) = cap_text(&stripped[s..=e]);
            let hit = keyword_hits[s].clone();
            let directive = directives[s];
            blocks.push(CommentBlock {
                ordinal: blocks.len() as u32,
                kind,
                line_start: run[s].line,
                line_end: run[e].line,
                text,
                text_truncated,
                keyword: hit.as_ref().map(|h| h.keyword.clone()),
                keyword_text: hit.as_ref().map(|h| h.text.clone()),
                fields: hit.and_then(|h| h.fields),
                symbol: if kind == CommentKind::Doc {
                    symbol.clone()
                } else {
                    None
                },
                directive_tool: directive.map(|(spec, _)| spec.tool.to_string()),
                directive_has_reason: directive
                    .filter(|(spec, _)| spec.suppression)
                    .map(|(_, has)| has),
            });
            i = j + 1;
        }
    }

    Ok(CommentExtraction { blocks, truncated })
}

fn collect_comment_nodes<'a>(node: tree_sitter::Node<'a>, out: &mut Vec<tree_sitter::Node<'a>>) {
    if is_comment_kind(node.kind()) {
        out.push(node);
        return; // never walk into a comment's children
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_comment_nodes(child, out);
    }
}

/// Comment-node kinds across the eleven grammars this crate links. The
/// defensive `contains("comment")` covers a grammar rename without
/// silently dropping the whole pass (inherited verbatim from the deleted
/// `extract::is_comment_kind`).
fn is_comment_kind(kind: &str) -> bool {
    matches!(kind, "comment" | "line_comment" | "block_comment") || kind.contains("comment")
}

/// Strip a line's comment sigil. Returns `(stripped, sigil_was_hash_colon)`
/// — the second flag is RBS::Inline's `#:` types-as-comments marker, which
/// is a directive whose marker is glued to the sigil rather than being a
/// word of its own.
fn strip_sigil(raw: &str) -> (String, bool) {
    let t = raw.trim();
    let mut rest = t;
    let mut sigil_colon = false;
    for prefix in ["/**", "///", "//!", "//", "/*", "<!--", "*/", "-->"] {
        if let Some(r) = rest.strip_prefix(prefix) {
            rest = r;
            break;
        }
    }
    if rest == t {
        if let Some(r) = rest.strip_prefix("#:") {
            rest = r;
            sigil_colon = true;
        } else if let Some(r) = rest.strip_prefix("#!") {
            rest = r;
        } else if let Some(r) = rest.strip_prefix('#') {
            rest = r;
        } else if let Some(r) = rest.strip_prefix("--") {
            rest = r;
        } else if let Some(r) = rest.strip_prefix('*') {
            // A block comment's continuation line (` * text`).
            rest = r;
        }
    }
    for suffix in ["*/", "-->"] {
        if let Some(r) = rest.strip_suffix(suffix) {
            rest = r;
            break;
        }
    }
    (rest.trim().to_string(), sigil_colon)
}

/// Join a block's lines and bound the result at [`COMMENT_TEXT_CAP_BYTES`]
/// on a char boundary.
fn cap_text(lines: &[String]) -> (String, bool) {
    // Blank framing lines at either end are structure, not text.
    let start = lines.iter().position(|l| !l.is_empty()).unwrap_or(0);
    let end = lines
        .iter()
        .rposition(|l| !l.is_empty())
        .map(|e| e + 1)
        .unwrap_or(start);
    let joined = lines[start..end].join("\n");
    if joined.len() <= COMMENT_TEXT_CAP_BYTES {
        return (joined, false);
    }
    let mut out = String::new();
    for ch in joined.chars() {
        if out.len() + ch.len_utf8() > COMMENT_TEXT_CAP_BYTES {
            break;
        }
        out.push(ch);
    }
    (out, true)
}

// --- the surface declaration (kb-code-server invariant 15) -----------------

/// Every route this unit ships, declared as DATA. Walked from the server
/// side by [`tests::every_declared_v72_j1_route_is_registered_and_requires_its_params`]
/// and from the CLI side by kb-code-cli's
/// `cli_requests_send_every_param_their_route_requires`.
pub const V72_J1_ROUTES: &[crate::entities::RouteContract] = &[
    routes::COMMENTS_ROUTE,
    routes::COMMENTS_FILE_ROUTE,
    routes::COMMENTS_SUMMARY_ROUTE,
    routes::COMMENTS_KEYWORDS_ROUTE,
];

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTER_SRC: &str = include_str!("../router.rs");

    fn sym(name: &str, kind: &str, start: u32, end: u32) -> Symbol {
        Symbol {
            ordinal: 0,
            name: name.to_string(),
            kind: kind.to_string(),
            line_start: start,
            line_end: end,
            col_start: 0,
            col_end: 0,
            container: None,
            signature: None,
            doc: None,
            param_min: None,
            param_max: None,
        }
    }

    fn kinds_of(blocks: &[CommentBlock]) -> Vec<(&'static str, u32, u32)> {
        blocks
            .iter()
            .map(|b| (b.kind.as_str(), b.line_start, b.line_end))
            .collect()
    }

    fn extract(lang: &str, src: &str, symbols: &[Symbol]) -> Vec<CommentBlock> {
        extract_comments(lang, src.as_bytes(), symbols, &KeywordSet::defaults())
            .unwrap()
            .blocks
    }

    #[test]
    fn sigils_are_stripped_across_the_shapes_the_grammars_produce() {
        for (raw, want) in [
            ("// a", "a"),
            ("/// a", "a"),
            ("//! a", "a"),
            ("/** a", "a"),
            (" * a", "a"),
            ("# a", "a"),
            ("-- a", "a"),
            ("<!-- a -->", "a"),
            ("/* a */", "a"),
        ] {
            assert_eq!(strip_sigil(raw).0, want, "{raw:?}");
        }
        assert!(strip_sigil("#: () -> String").1, "#: is the RBS marker");
        assert!(!strip_sigil("# ordinary").1);
    }

    #[test]
    fn a_trailing_comment_is_always_its_own_run() {
        // `x` is code, so the `# why` beside it must not merge into the
        // block above it.
        let src = "# leading prose\n# more prose\nx = 1 # why\n";
        let blocks = extract("ruby", src, &[]);
        assert_eq!(
            kinds_of(&blocks),
            vec![("prose", 1, 2), ("prose", 3, 3)],
            "{blocks:#?}"
        );
    }

    #[test]
    fn adjacent_same_kind_lines_form_one_block_with_a_range() {
        let src = "# one\n# two\n# three\n";
        let blocks = extract("ruby", src, &[]);
        assert_eq!(kinds_of(&blocks), vec![("prose", 1, 3)]);
        assert_eq!(blocks[0].text, "one\ntwo\nthree");
    }

    #[test]
    fn two_adjacent_annotations_stay_two_blocks() {
        // Merging them would destroy one keyword — and one row of
        // `GET /api/todos`.
        let src = "# TODO: first\n# FIXME: second\n";
        let blocks = extract("ruby", src, &[]);
        assert_eq!(
            kinds_of(&blocks),
            vec![("annotation", 1, 1), ("annotation", 2, 2)]
        );
        assert_eq!(blocks[0].keyword.as_deref(), Some("TODO"));
        assert_eq!(blocks[1].keyword.as_deref(), Some("FIXME"));
    }

    #[test]
    fn a_doc_run_attaches_to_the_definition_directly_below_it() {
        let src = "# Returns the total.\n# In cents.\ndef total\n  0\nend\n";
        let blocks = extract("ruby", src, &[sym("total", "method", 3, 5)]);
        assert_eq!(kinds_of(&blocks), vec![("doc", 1, 2)]);
        let s = blocks[0].symbol.as_ref().unwrap();
        assert_eq!(s.name, "total");
        assert_eq!((s.line_start, s.line_end), (3, 5));
    }

    #[test]
    fn a_blank_line_breaks_the_doc_run() {
        let src = "# Not documentation.\n\ndef total\n  0\nend\n";
        let blocks = extract("ruby", src, &[sym("total", "method", 3, 5)]);
        assert_eq!(kinds_of(&blocks), vec![("prose", 1, 1)]);
        assert!(blocks[0].symbol.is_none());
    }

    #[test]
    fn an_annotation_inside_a_doc_run_splits_it_and_keeps_both() {
        let src =
            "# Returns the total.\n# TODO: switch to cents\n# See Order#price.\ndef total\nend\n";
        let blocks = extract("ruby", src, &[sym("total", "method", 4, 5)]);
        assert_eq!(
            kinds_of(&blocks),
            vec![("doc", 1, 1), ("annotation", 2, 2), ("doc", 3, 3)]
        );
    }

    #[test]
    fn the_schema_banner_is_generated_and_never_the_classes_doc() {
        let src = "# == Schema Information\n#\n# Table name: orders\n#  id :bigint not null\n#\nclass Order\nend\n";
        let blocks = extract("ruby", src, &[sym("Order", "class", 6, 7)]);
        assert_eq!(kinds_of(&blocks), vec![("generated", 1, 4)]);
        assert!(
            blocks.iter().all(|b| b.symbol.is_none()),
            "a generated banner never documents the class below it"
        );
    }

    #[test]
    fn a_licence_header_is_licence_and_a_later_copyright_mention_is_not() {
        let head = "# SPDX-License-Identifier: MIT\n# Copyright (c) 2026 Example\nx = 1\n";
        assert_eq!(
            kinds_of(&extract("ruby", head, &[])),
            vec![("licence", 1, 2)]
        );
        let deep = format!("{}\n# Copyright 2026 Example Corp\n", "x = 1\n".repeat(20));
        let blocks = extract("ruby", &deep, &[]);
        assert_eq!(blocks[0].kind, CommentKind::Prose);
    }

    #[test]
    fn directives_carry_their_tool_and_reason_state() {
        let src = "# frozen_string_literal: true\n# rubocop:disable Metrics/AbcSize\n# rubocop:disable Metrics/AbcSize -- legacy import path\nx = 1\n";
        let blocks = extract("ruby", src, &[]);
        assert_eq!(
            kinds_of(&blocks),
            vec![
                ("directive", 1, 1),
                ("directive", 2, 2),
                ("directive", 3, 3)
            ]
        );
        // A magic comment carries a value, not a justification.
        assert_eq!(blocks[0].directive_tool.as_deref(), Some("ruby-magic"));
        assert_eq!(blocks[0].directive_has_reason, None);
        assert_eq!(blocks[1].directive_has_reason, Some(false));
        assert_eq!(blocks[2].directive_has_reason, Some(true));
    }

    #[test]
    fn commented_out_code_is_its_own_kind() {
        let src = "# order.total = 0\n# order.save!\ndef total\nend\n";
        let blocks = extract("ruby", src, &[sym("total", "method", 3, 4)]);
        assert_eq!(kinds_of(&blocks), vec![("commented_code", 1, 2)]);
        assert!(
            blocks[0].symbol.is_none(),
            "commented-out code above a def is not its doc"
        );
    }

    #[test]
    fn a_section_banner_is_section() {
        let src = "# ==== Callbacks ====\nx = 1\n";
        assert_eq!(
            kinds_of(&extract("ruby", src, &[])),
            vec![("section", 1, 1)]
        );
    }

    #[test]
    fn a_keyword_inside_a_string_literal_is_not_an_annotation() {
        // Comment NODES only — inherited from `extract_todos` and pinned
        // here so the replacement keeps the property.
        let src = "s = \"TODO inside a string must not match\"\n# TODO: real\n";
        let blocks = extract("ruby", src, &[]);
        assert_eq!(kinds_of(&blocks), vec![("annotation", 2, 2)]);
    }

    #[test]
    fn smart_todo_fields_ride_the_annotation_block() {
        let src = "# TODO(on: date('2027-09-01'), to: 'owner@example.com') drop the shim\n";
        let blocks = extract("ruby", src, &[]);
        let f = blocks[0].fields.as_ref().unwrap();
        assert_eq!(f.on_date.as_deref(), Some("2027-09-01"));
        assert_eq!(
            blocks[0].keyword_text.as_deref(),
            Some("(on: date('2027-09-01'), to: 'owner@example.com') drop the shim")
        );
    }

    #[test]
    fn every_taxonomy_kind_is_reachable_from_one_ruby_fixture() {
        let src = include_str!("../../tests/fixtures/comments/taxonomy.rb");
        let symbols = crate::extract::extract_symbols("ruby", src.as_bytes()).unwrap();
        let blocks = extract("ruby", src, &symbols);
        let seen: std::collections::BTreeSet<&str> =
            blocks.iter().map(|b| b.kind.as_str()).collect();
        for k in CommentKind::ALL {
            assert!(
                seen.contains(k.as_str()),
                "fixture never produces {}: {:#?}",
                k.as_str(),
                kinds_of(&blocks)
            );
        }
    }

    #[test]
    fn outline_tier_languages_index_comments_but_never_mint_doc() {
        let src = "# a yaml comment\nkey: 1\n";
        let blocks = extract("yaml", src, &[]);
        assert_eq!(kinds_of(&blocks), vec![("prose", 1, 1)]);
    }

    #[test]
    fn the_per_file_ceiling_truncates_loudly() {
        // 3 lines per block (comment, blank, code) so the run breaks.
        let src = "# a\n\nx = 1\n".repeat(MAX_BLOCKS_PER_FILE + 10);
        let out = extract_comments("ruby", src.as_bytes(), &[], &KeywordSet::defaults()).unwrap();
        assert!(out.truncated);
        assert_eq!(out.blocks.len(), MAX_BLOCKS_PER_FILE);
    }

    #[test]
    fn block_text_is_capped_on_a_char_boundary_and_flagged() {
        let long = format!("# {}\n", "é".repeat(COMMENT_TEXT_CAP_BYTES));
        let blocks = extract("ruby", &long, &[]);
        assert!(blocks[0].text_truncated);
        assert!(blocks[0].text.len() <= COMMENT_TEXT_CAP_BYTES);
    }

    #[test]
    fn ordinals_are_dense_and_line_ordered() {
        let src = "# a\n\n# TODO: b\n\n# c\n";
        let blocks = extract("ruby", src, &[]);
        for (i, b) in blocks.iter().enumerate() {
            assert_eq!(b.ordinal, i as u32);
        }
        assert!(blocks.windows(2).all(|w| w[0].line_start < w[1].line_start));
    }

    #[test]
    fn the_version_key_carries_the_taxonomy_the_grammar_and_the_keyword_set() {
        let defaults = KeywordSet::defaults();
        let v = comments_version_for("ruby@0.23.1+q1", &defaults);
        assert!(v.starts_with(COMMENTS_GRAMMAR_VERSION));
        assert!(v.ends_with("ruby@0.23.1+q1"));
        // A different vocabulary is a different key — otherwise an
        // operator adding a keyword would keep every already-indexed
        // file's keyword-blind rows forever.
        let custom = KeywordSet::from_config(&["DEBT".to_string()]);
        assert_ne!(v, comments_version_for("ruby@0.23.1+q1", &custom));
        // …and the same vocabulary is the same key, whatever order it
        // was typed in.
        let reordered = KeywordSet::from_config(&["TODO".into(), "DEBT".into()]);
        let same = KeywordSet::from_config(&["DEBT".into(), "TODO".into()]);
        assert_eq!(
            comments_version_for("ruby@0.23.1+q1", &reordered),
            comments_version_for("ruby@0.23.1+q1", &same)
        );
    }

    #[test]
    fn every_declared_v72_j1_route_is_registered_and_requires_its_params() {
        assert!(!V72_J1_ROUTES.is_empty());
        for c in V72_J1_ROUTES {
            let nested = c
                .path
                .strip_prefix("/api")
                .expect("every route path is /api-nested");
            assert!(
                ROUTER_SRC.contains(&format!("\"{nested}\"")),
                "{}: declared but never registered in router.rs — the v7.0 dead-surface defect",
                c.path
            );
            assert!(
                ROUTER_SRC.contains(c.handler),
                "{}: registered path but no {} handler named in router.rs",
                c.path,
                c.handler
            );
            assert!(
                (c.params_accept_without)(""),
                "{}: its own params struct rejects a COMPLETE query map",
                c.path
            );
            for p in c.required_params {
                assert!(
                    !(c.params_accept_without)(p),
                    "{}: declares {p:?} required, but its params struct accepts a request without it",
                    c.path
                );
            }
        }
    }
}
