//! Notes / todo-lists (N-track). A "note" is an ordinary Markdown artifact
//! carrying `kb-category: note`; everything here is pure, side-effect-free
//! logic the server route + CLI drive:
//!
//! - **path conventions** — the canonical per-scope notepad
//!   (`_notepad.md`) vs ad-hoc named notes (`note-<slug>-<unix>.md`); the
//!   scope of a note is just the folder it lives in (`""` = kb root).
//! - **compose / split** — build a note's Markdown source (frontmatter +
//!   body) via the byte-preserving [`crate::markdown::set_frontmatter_field`]
//!   writer, and split an on-disk note back into (frontmatter, body).
//! - **checklist ops** — toggle / append / count GFM task lines
//!   (`- [ ]` / `- [x]`) in **document order**, skipping fenced code blocks.
//!   The Nth task line is the canonical toggle index — it matches the Nth
//!   rendered `<input type="checkbox">` (comrak + remark-gfm both walk the
//!   same GFM document), so the SPA's nth checkbox and the server's
//!   `toggle {index}` address the same task. Invariant pinned by
//!   `parser::tests::task_counts_match_source_scan`.
//!
//! A note is system-authored Markdown with NO `<template id="kb-prompt">`,
//! so the route can rewrite the whole file safely (the byte-preserving
//! `<meta>`-splice rule of invariant #12 is about HTML artifacts that *do*
//! carry a prompt template — not these).

use crate::docs_query::DocRow;
use crate::markdown::{self, FrontMatter};
use serde::Serialize;
use std::path::Path;

/// Deterministic filename of a scope's canonical notepad.
pub const NOTEPAD_FILE: &str = "_notepad.md";
/// The `kb-category` value that marks an artifact as a note.
pub const NOTE_CATEGORY: &str = "note";

/// Join a scope `folder` ("" = kb root) and a file name into a
/// forward-slash source-relative path.
fn join_folder(folder: &str, file: &str) -> String {
    let folder = folder.trim_matches('/');
    if folder.is_empty() {
        file.to_string()
    } else {
        format!("{folder}/{file}")
    }
}

/// Source-relative path of a scope's canonical notepad. `folder == ""`
/// yields the kb-root notepad (`_notepad.md`).
pub fn notepad_path(folder: &str) -> String {
    join_folder(folder, NOTEPAD_FILE)
}

/// Source-relative path of an ad-hoc note. `n == 0` is the base name;
/// the route increments `n` to dodge a same-second collision (mirrors the
/// memory-ingest `-n` suffix).
pub fn adhoc_path(folder: &str, slug: &str, unix: i64, n: u32) -> String {
    let slug = if slug.is_empty() { "note" } else { slug };
    let suffix = if n == 0 {
        String::new()
    } else {
        format!("-{n}")
    };
    join_folder(folder, &format!("note-{slug}-{unix}{suffix}.md"))
}

/// True when a source-relative path's file name is exactly `_notepad.md`
/// — i.e. this note is its scope's canonical notepad.
pub fn is_notepad(rel_path: &str) -> bool {
    rel_path.rsplit('/').next() == Some(NOTEPAD_FILE)
}

/// True when an artifact is an editable N-track note: a **Markdown** source
/// tagged `kb-category: note`. The markdown gate is load-bearing — `note`
/// long predates this feature as a *content* category on HTML research
/// write-ups (and the authoring template), and treating those as editable
/// todo-notes hijacks them out of the gallery and into a broken native
/// render. Every "is this a note?" site (lance `list_notes`, the route
/// `note_row` guard, the gallery exclusion, the SPA detail view) must agree
/// on this predicate. `path` is the on-disk / source path; only the
/// extension is inspected, so absolute or relative both work.
pub fn is_note(path: &str, kb_category: Option<&str>) -> bool {
    kb_category == Some(NOTE_CATEGORY) && crate::indexer::is_markdown(Path::new(path))
}

/// Filename-safe slug for an ad-hoc note title. Reuses the tag slugifier
/// (lowercase ASCII alnum, runs → `-`), falling back to `"note"` when the
/// title slugs to nothing.
pub fn slugify(title: &str) -> String {
    let s = crate::parser::slugify_tag(title);
    if s.is_empty() {
        "note".to_string()
    } else {
        s
    }
}

/// The editable fields a note carries in its frontmatter, plus the body.
#[derive(Debug, Default, Clone)]
pub struct NoteFields<'a> {
    pub title: Option<&'a str>,
    /// Rides the existing `kb-status` facet (`active`/`done`/`archived`/…).
    pub status: Option<&'a str>,
    /// Rides the existing `kb-tags` facet.
    pub tags: &'a [String],
    pub body: &'a str,
}

/// Build a note's Markdown source: a `---`-fenced frontmatter block with
/// `kb-category: note` always present (plus title/status/tags when set),
/// then the body. The frontmatter is written by
/// [`markdown::set_frontmatter_field`] — the single byte-exact writer — so
/// this never hand-rolls YAML and never emits a `<template id="kb-prompt">`.
pub fn compose_note_source(f: &NoteFields) -> String {
    let mut src = f.body.to_string();
    if let Some(t) = f.title.map(str::trim).filter(|s| !s.is_empty()) {
        src = markdown::set_frontmatter_field(&src, "title", Some(t));
    }
    src = markdown::set_frontmatter_field(&src, "kb-category", Some(NOTE_CATEGORY));
    if let Some(s) = f.status.map(str::trim).filter(|s| !s.is_empty()) {
        src = markdown::set_frontmatter_field(&src, "kb-status", Some(s));
    }
    if !f.tags.is_empty() {
        let joined = f.tags.join(", ");
        src = markdown::set_frontmatter_field(&src, "kb-tags", Some(&joined));
    }
    src
}

/// Split an on-disk note source into (frontmatter, body) — a thin wrapper
/// over [`markdown::parse_frontmatter`] so callers don't reach across
/// modules and the "category is note" understanding lives in one place.
pub fn split_note_source(src: &str) -> (FrontMatter, &str) {
    markdown::parse_frontmatter(src)
}

/// Replace a note's body, preserving the frontmatter block byte-for-byte.
/// BOM-safe (re-prepended verbatim). The inverse of editing in place: the
/// frontmatter is untouched, only the body after the closing fence changes.
pub fn replace_body(src: &str, new_body: &str) -> String {
    let (bom, rest) = match src.strip_prefix('\u{feff}') {
        Some(r) => ("\u{feff}", r),
        None => ("", src),
    };
    let (_, body) = markdown::parse_frontmatter(rest);
    let prefix = &rest[..rest.len() - body.len()];
    format!("{bom}{prefix}{new_body}")
}

// --- checklist ops -----------------------------------------------------------

/// A located GFM task list item: byte offset (within the body) of the inner
/// symbol char (the space or `x` between the brackets) and its checked state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Task {
    inner: usize,
    checked: bool,
}

/// Locate every GFM task list item in document order by parsing the body with
/// comrak's AST — the SAME parse [`crate::markdown::render_fragment`] uses, so
/// the task set + ordering is byte-identical to the rendered
/// `<input type=checkbox>` set (and to the SPA's remark-gfm render, since both
/// follow GFM). This is what makes "toggle the Nth checkbox" address the right
/// source line for *every* GFM form — bullet, ordered, blockquoted, nested —
/// and skips fenced code automatically. The earlier hand-rolled line scanner
/// only handled bullet lists and silently diverged on the rest; don't bring it
/// back. Invariant pinned by `parser::tests::task_counts_match_source_scan`.
fn scan_tasks(body: &str) -> Vec<Task> {
    use comrak::nodes::NodeValue;
    let arena = comrak::Arena::new();
    let root = comrak::parse_document(&arena, body, &crate::markdown::kb_options());
    // 0-based byte offset of each line start, to map comrak's 1-based
    // (line, byte-column) sourcepos onto a body byte offset.
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(body.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let mut out = Vec::new();
    for node in root.descendants() {
        let ast = node.data();
        let NodeValue::TaskItem(ti) = &ast.value else {
            continue;
        };
        // `symbol_sourcepos` is the single symbol char (` `/`x`/`X`); 1-based
        // line + 1-based BYTE column (parse.sourcepos_chars stays default off).
        let sp = ti.symbol_sourcepos.start;
        let Some(line0) = line_starts.get(sp.line.saturating_sub(1)) else {
            continue;
        };
        let inner = line0 + sp.column.saturating_sub(1);
        // Guard the sourcepos→byte mapping: the located byte MUST be the task
        // symbol. If a comrak bump ever changes column semantics this trips in
        // tests rather than silently flipping the wrong byte.
        debug_assert!(
            matches!(body.as_bytes().get(inner), Some(b' ' | b'x' | b'X')),
            "task symbol byte mismatch at {inner} in {body:?}"
        );
        out.push(Task {
            inner,
            checked: ti.symbol.is_some(),
        });
    }
    out
}

/// `(done, total)` GFM task counts over the body, document order, fenced
/// code skipped. The SAME scanner `toggle_task`/`append_task` use, so the
/// count and the toggle index are consistent by construction.
pub fn checklist_counts(body: &str) -> (u32, u32) {
    let tasks = scan_tasks(body);
    let done = tasks.iter().filter(|t| t.checked).count() as u32;
    (done, tasks.len() as u32)
}

/// Flip the `index`-th GFM task line (0-based, document order) to `on`.
/// Returns the rewritten body, or `None` when `index` is out of range (the
/// route maps that to 404). Toggling to the current state is a no-op write.
pub fn toggle_task(body: &str, index: usize, on: bool) -> Option<String> {
    let task = scan_tasks(body).into_iter().nth(index)?;
    let mut s = body.to_string();
    s.replace_range(task.inner..task.inner + 1, if on { "x" } else { " " });
    Some(s)
}

/// Append `- [ ] <text>` as a new unchecked task at the end of the body,
/// guaranteeing a separating newline. Returns the new body.
pub fn append_task(body: &str, text: &str) -> String {
    let text = text.trim();
    let mut s = body.to_string();
    if !s.is_empty() && !s.ends_with('\n') {
        s.push('\n');
    }
    s.push_str("- [ ] ");
    s.push_str(text);
    s.push('\n');
    s
}

// --- projection --------------------------------------------------------------

/// Gallery-free summary of a note for the list / panel views. Built from a
/// decorated [`DocRow`] (so `folder` is already derived) — no per-row file
/// reads on the list path; task counts come straight off the lance columns.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct NoteSummary {
    pub id: String,
    /// Owning kb. Empty on the per-kb route; set by the cross-kb fan-out.
    #[serde(skip_serializing_if = "String::is_empty")]
    #[cfg_attr(feature = "ts-export", ts(as = "Option<String>", optional))]
    pub kb: String,
    pub title: String,
    /// Scope folder ("" = kb root). The SPA renders "" as the kb-level scope.
    pub folder: String,
    pub source_relative: String,
    pub is_notepad: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub status: Option<String>,
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub updated_at: Option<i64>,
    pub task_done: u32,
    pub task_total: u32,
}

/// Project a decorated lance row into a [`NoteSummary`]. `kb` is left empty
/// (the per-kb route doesn't echo it; the cross-kb route fills it in).
pub fn note_summary_from_row(row: &DocRow, source_root: &Path) -> NoteSummary {
    let d = &row.doc;
    let source_relative = crate::paths::doc_rel_path(&d.path, source_root);
    let source_relative = if source_relative.is_empty() {
        d.path.clone()
    } else {
        source_relative
    };
    NoteSummary {
        id: d.id.clone(),
        kb: String::new(),
        title: d.title.clone(),
        folder: row.folder.clone(),
        is_notepad: is_notepad(&source_relative),
        source_relative,
        status: d.kb_status.clone(),
        tags: d.tags.clone(),
        updated_at: d.mtime_unix,
        task_done: d.task_done.unwrap_or(0),
        task_total: d.task_total.unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notepad_path_root_and_nested() {
        assert_eq!(notepad_path(""), "_notepad.md");
        assert_eq!(notepad_path("design"), "design/_notepad.md");
        assert_eq!(
            notepad_path("design/research"),
            "design/research/_notepad.md"
        );
        // Trailing/leading slashes are tolerated.
        assert_eq!(notepad_path("/design/"), "design/_notepad.md");
    }

    #[test]
    fn adhoc_path_shape_and_suffix() {
        assert_eq!(adhoc_path("", "deploy", 1700, 0), "note-deploy-1700.md");
        assert_eq!(
            adhoc_path("ops", "deploy", 1700, 0),
            "ops/note-deploy-1700.md"
        );
        assert_eq!(
            adhoc_path("ops", "deploy", 1700, 2),
            "ops/note-deploy-1700-2.md"
        );
        // Empty slug falls back to "note".
        assert_eq!(adhoc_path("", "", 9, 0), "note-note-9.md");
    }

    #[test]
    fn is_notepad_true_only_for_exact_name() {
        assert!(is_notepad("_notepad.md"));
        assert!(is_notepad("design/_notepad.md"));
        assert!(!is_notepad("note-x-1.md"));
        assert!(!is_notepad("design/notes/_notepad.html"));
        assert!(!is_notepad("design/my_notepad.md"));
    }

    #[test]
    fn slugify_empty_falls_back_to_note() {
        assert_eq!(slugify("Deploy Checklist"), "deploy-checklist");
        assert_eq!(slugify("   "), "note");
        assert_eq!(slugify("!!!"), "note");
    }

    #[test]
    fn compose_round_trips_through_parse_frontmatter() {
        let tags = vec!["ops".to_string(), "release".to_string()];
        let f = NoteFields {
            title: Some("Deploy checklist"),
            status: Some("active"),
            tags: &tags,
            body: "- [ ] run tests\n- [x] tag\n",
        };
        let src = compose_note_source(&f);
        let (fm, body) = split_note_source(&src);
        assert_eq!(fm.title.as_deref(), Some("Deploy checklist"));
        assert_eq!(fm.kb_category.as_deref(), Some(NOTE_CATEGORY));
        assert_eq!(fm.kb_status.as_deref(), Some("active"));
        assert_eq!(fm.kb_tags, vec!["ops", "release"]);
        assert_eq!(body, "- [ ] run tests\n- [x] tag\n");
    }

    // invariant:16 whole-file-rewrite
    #[test]
    fn compose_has_no_kb_prompt_template() {
        // Risk 1: a note's source can never carry a prompt template, so a
        // whole-file rewrite can't corrupt one (invariant #12 is about HTML
        // artifacts that DO carry it).
        let f = NoteFields {
            title: Some("t"),
            body: "body with <template id=\"x\"> not a prompt",
            ..Default::default()
        };
        let src = compose_note_source(&f);
        assert!(!src.contains(r#"<template id="kb-prompt">"#));
    }

    #[test]
    fn toggle_flips_nth_in_document_order() {
        let body = "- [ ] a\n- [ ] b\n- [x] c\n";
        let out = toggle_task(body, 1, true).unwrap();
        assert_eq!(out, "- [ ] a\n- [x] b\n- [x] c\n");
        // And back off.
        let out2 = toggle_task(&out, 2, false).unwrap();
        assert_eq!(out2, "- [ ] a\n- [x] b\n- [ ] c\n");
    }

    #[test]
    fn toggle_out_of_range_is_none() {
        assert!(toggle_task("- [ ] a\n", 5, true).is_none());
        assert!(toggle_task("no tasks here\n", 0, true).is_none());
    }

    #[test]
    fn toggle_skips_tasks_inside_code_fences() {
        let body = "- [ ] real\n```\n- [ ] fake in code\n```\n- [ ] real2\n";
        assert_eq!(
            checklist_counts(body),
            (0, 2),
            "the fenced task line is not counted"
        );
        // Index 1 is "real2", not the fenced line; the fence stays untouched.
        let out = toggle_task(body, 1, true).unwrap();
        assert_eq!(
            out,
            "- [ ] real\n```\n- [ ] fake in code\n```\n- [x] real2\n"
        );
    }

    #[test]
    fn append_adds_unchecked_line_and_newline() {
        assert_eq!(append_task("", "buy milk"), "- [ ] buy milk\n");
        assert_eq!(append_task("- [ ] a", "b"), "- [ ] a\n- [ ] b\n");
        assert_eq!(append_task("- [ ] a\n", "b"), "- [ ] a\n- [ ] b\n");
    }

    #[test]
    fn checklist_counts_done_total() {
        assert_eq!(checklist_counts("- [ ] a\n- [x] b\n- [X] c\n"), (2, 3));
        assert_eq!(checklist_counts("plain prose, no tasks"), (0, 0));
        // Nested (indented) task items still count.
        assert_eq!(checklist_counts("- [ ] a\n  - [x] nested\n"), (1, 2));
    }

    #[test]
    fn toggle_then_counts_consistent() {
        let body = "- [ ] a\n- [ ] b\n";
        let after = toggle_task(body, 0, true).unwrap();
        assert_eq!(checklist_counts(&after), (1, 2));
    }

    #[test]
    fn replace_body_preserves_frontmatter() {
        let src = "---\ntitle: T\nkb-category: note\n---\n- [ ] old\n";
        let out = replace_body(src, "- [x] new\n");
        assert_eq!(out, "---\ntitle: T\nkb-category: note\n---\n- [x] new\n");
        let (fm, body) = split_note_source(&out);
        assert_eq!(fm.title.as_deref(), Some("T"));
        assert_eq!(body, "- [x] new\n");
    }

    #[test]
    fn replace_body_is_bom_safe() {
        let src = "\u{feff}---\ntitle: T\n---\nold body\n";
        let out = replace_body(src, "new body\n");
        assert_eq!(out, "\u{feff}---\ntitle: T\n---\nnew body\n");
    }

    #[test]
    fn note_summary_from_row_sets_is_notepad_and_scope() {
        use crate::storage::lance::DocSummary;
        let root = Path::new("/srv/kb");
        let d = DocSummary {
            id: "abc".into(),
            title: "Notepad".into(),
            path: "/srv/kb/design/_notepad.md".into(),
            kb_status: Some("active".into()),
            task_done: Some(1),
            task_total: Some(3),
            ..Default::default()
        };
        let row = DocRow {
            doc: d,
            folder: "design".into(),
        };
        let s = note_summary_from_row(&row, root);
        assert_eq!(s.folder, "design");
        assert_eq!(s.source_relative, "design/_notepad.md");
        assert!(s.is_notepad);
        assert_eq!(s.task_done, 1);
        assert_eq!(s.task_total, 3);
        assert_eq!(s.status.as_deref(), Some("active"));
    }

    // invariant:16 is-note-gate
    #[test]
    fn is_note_requires_markdown_and_category() {
        // The collision fix: `note` is an editable note ONLY when Markdown.
        assert!(is_note("design/_notepad.md", Some("note")));
        assert!(is_note("/srv/kb/notes/note-x-1.md", Some("note")));
        assert!(is_note("a.markdown", Some("note")));
        // HTML artifacts merely tagged `note` (research write-ups, the
        // authoring template) are NOT editable notes — the bug we fixed.
        assert!(!is_note("research/foo.html", Some("note")));
        assert!(!is_note("a.htm", Some("note")));
        // Markdown with another category is not a note either.
        assert!(!is_note("a.md", Some("research")));
        assert!(!is_note("a.md", None));
    }

    #[test]
    fn toggle_blockquote_ordered_and_long_fences() {
        // Blockquoted tasks: comrak counts them, so index 1 is the 2nd task
        // (the old line scanner missed `>`-prefixed tasks entirely).
        let bq = "> - [ ] x\n> - [ ] y\n";
        assert_eq!(checklist_counts(bq), (0, 2));
        assert_eq!(toggle_task(bq, 1, true).unwrap(), "> - [ ] x\n> - [x] y\n");

        // Ordered-list tasks (`.` and `)`): also previously unhandled.
        let dot = "1. [ ] one\n2. [ ] two\n";
        assert_eq!(checklist_counts(dot), (0, 2));
        assert_eq!(
            toggle_task(dot, 0, true).unwrap(),
            "1. [x] one\n2. [ ] two\n"
        );
        let paren = "1) [ ] one\n2) [ ] two\n";
        assert_eq!(
            toggle_task(paren, 1, true).unwrap(),
            "1) [ ] one\n2) [x] two\n"
        );

        // A 4-backtick fence is NOT closed by a 3-backtick line; the inner
        // task stays fenced, so the only real tasks are `real` (0) + `real2` (1).
        let f = "- [ ] real\n````\n```\n- [ ] fenced\n````\n- [ ] real2\n";
        assert_eq!(checklist_counts(f), (0, 2));
        assert_eq!(
            toggle_task(f, 1, true).unwrap(),
            "- [ ] real\n````\n```\n- [ ] fenced\n````\n- [x] real2\n"
        );

        // `- [ ]x` (no whitespace after `]`) is NOT a GFM task → uncounted.
        assert_eq!(checklist_counts("- [ ]x not a task\n"), (0, 0));
        assert!(toggle_task("- [ ]x not a task\n", 0, true).is_none());
    }

    #[test]
    fn toggle_byte_offset_survives_multibyte_prior_content() {
        // comrak sourcepos columns are BYTE offsets; the line-start table maps
        // them onto the body. A multibyte heading + a multibyte first task
        // must not knock the flip off its ASCII bracket (guards the mapping;
        // the debug_assert in scan_tasks also fires on a mismatch).
        let body = "# 日本語タイトル\n\n- [ ] 最初のタスク\n- [ ] second\n";
        assert_eq!(checklist_counts(body), (0, 2));
        assert_eq!(
            toggle_task(body, 1, true).unwrap(),
            "# 日本語タイトル\n\n- [ ] 最初のタスク\n- [x] second\n"
        );
        assert_eq!(
            toggle_task(body, 0, true).unwrap(),
            "# 日本語タイトル\n\n- [x] 最初のタスク\n- [ ] second\n"
        );
    }
}
