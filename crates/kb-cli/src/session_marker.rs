//! CT-A4 — shared session-marker helpers used by every CLI verb that
//! auto-stamps `kb-session` on artifact creation. Extracted out of
//! `commands::memory` (the original v0.14 S1 home) so `kb remember` and
//! `kb notes new` read the SAME marker the SAME way instead of growing two
//! copies of the same best-effort file read.

use std::path::Path;

/// v0.14 S1 — read the current Claude Code session id from the marker file
/// the SessionStart / UserPromptSubmit hooks maintain at
/// `${XDG_CACHE_HOME:-$HOME/.cache}/kb/current-session`. Any failure (file
/// missing, unreadable, empty) returns `None` — session stamping is
/// best-effort, never blocks the caller.
pub fn read_session_marker() -> Option<String> {
    let cache_dir = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))?;
    read_session_marker_at(&cache_dir)
}

/// Pure helper for `read_session_marker`: takes the resolved cache
/// directory so tests can run in parallel without racing on
/// `XDG_CACHE_HOME`. Trims whitespace (the marker is a single-line
/// write so a trailing newline always exists).
pub fn read_session_marker_at(cache_dir: &Path) -> Option<String> {
    let path = cache_dir.join("kb").join("current-session");
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// CT-A4 — splice `kb-session: <session_id>` into a Markdown source's
/// frontmatter, going through the SAME `kb_core::markdown::set_frontmatter_field`
/// every other frontmatter facet (title/kb-status/kb-tags) is written with —
/// no bespoke YAML writer here. That function already creates a fresh
/// `---`-fenced block when the source has none, so this always ends up with
/// a `kb-session` line either way; it never has to special-case "no
/// frontmatter yet" (see `crates/kb-core/src/markdown.rs::set_frontmatter_field`
/// doc comment). **Never overwrites an existing `kb-session` value** — a
/// caller that already named a session (e.g. hand-authored frontmatter, or a
/// second stamp attempt) wins over the marker.
pub fn stamp_kb_session(src: &str, session_id: &str) -> String {
    let (fm, _) = kb_core::markdown::parse_frontmatter(src);
    if fm.kb_session.is_some() {
        return src.to_string();
    }
    kb_core::markdown::set_frontmatter_field(src, "kb-session", Some(session_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_session_marker_at_returns_trimmed_id() {
        let tmp = tempfile::tempdir().unwrap();
        let marker_dir = tmp.path().join("kb");
        std::fs::create_dir_all(&marker_dir).unwrap();
        std::fs::write(marker_dir.join("current-session"), "  sess-xyz-123  \n").unwrap();
        assert_eq!(
            read_session_marker_at(tmp.path()).as_deref(),
            Some("sess-xyz-123")
        );
    }

    #[test]
    fn read_session_marker_at_returns_none_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        // No marker file written under tmp/kb/current-session.
        assert!(read_session_marker_at(tmp.path()).is_none());
    }

    #[test]
    fn read_session_marker_at_returns_none_when_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let marker_dir = tmp.path().join("kb");
        std::fs::create_dir_all(&marker_dir).unwrap();
        std::fs::write(marker_dir.join("current-session"), "\n   \n").unwrap();
        assert!(
            read_session_marker_at(tmp.path()).is_none(),
            "whitespace-only marker → None"
        );
    }

    // ---- stamp_kb_session ------------------------------------------------

    #[test]
    fn stamp_kb_session_creates_frontmatter_when_none_exists() {
        let src = "# Hello\n\nbody text\n";
        let out = stamp_kb_session(src, "sess-1");
        let (fm, body) = kb_core::markdown::parse_frontmatter(&out);
        assert_eq!(fm.kb_session.as_deref(), Some("sess-1"));
        assert_eq!(body, src, "body must be untouched");
    }

    #[test]
    fn stamp_kb_session_adds_the_key_into_an_existing_frontmatter_block() {
        let src = "---\ntitle: Existing\nkb-tags: a, b\n---\nbody\n";
        let out = stamp_kb_session(src, "sess-2");
        let (fm, body) = kb_core::markdown::parse_frontmatter(&out);
        assert_eq!(fm.kb_session.as_deref(), Some("sess-2"));
        assert_eq!(fm.title.as_deref(), Some("Existing"), "other keys survive");
        assert_eq!(fm.kb_tags, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(body, "body\n");
    }

    #[test]
    fn stamp_kb_session_never_overwrites_an_existing_value() {
        let src = "---\nkb-session: already-set\n---\nbody\n";
        let out = stamp_kb_session(src, "sess-new");
        assert_eq!(out, src, "an existing kb-session value must win untouched");
    }

    #[test]
    fn stamp_kb_session_on_empty_body_yields_a_clean_frontmatter_only_doc() {
        // The notepad-creation path starts from an empty body.
        let out = stamp_kb_session("", "sess-3");
        let (fm, body) = kb_core::markdown::parse_frontmatter(&out);
        assert_eq!(fm.kb_session.as_deref(), Some("sess-3"));
        assert_eq!(body, "");
    }

    /// End-to-end regression: `kb notes new` stamps the CLIENT-composed
    /// `body_md` payload before it ever reaches the daemon, and the
    /// daemon-side `compose_note_source` (kb-core, unmodified by CT-A4)
    /// layers title/kb-category/kb-status/kb-tags into the SAME frontmatter
    /// block afterwards via repeated `set_frontmatter_field` calls. Proves
    /// the two compose cleanly into one block rather than fighting or
    /// double-fencing.
    #[test]
    fn stamp_kb_session_composes_with_compose_note_source() {
        let stamped = stamp_kb_session("my note body\n", "sess-compose");
        let tags = vec!["x".to_string()];
        let fields = kb_core::notes::NoteFields {
            title: Some("A Title"),
            status: Some("active"),
            tags: &tags,
            body: &stamped,
        };
        let composed = kb_core::notes::compose_note_source(&fields);
        let (fm, body) = kb_core::markdown::parse_frontmatter(&composed);
        assert_eq!(fm.kb_session.as_deref(), Some("sess-compose"));
        assert_eq!(fm.title.as_deref(), Some("A Title"));
        assert_eq!(fm.kb_category.as_deref(), Some("note"));
        assert_eq!(fm.kb_status.as_deref(), Some("active"));
        assert_eq!(fm.kb_tags, vec!["x".to_string()]);
        assert_eq!(body, "my note body\n");
    }
}
