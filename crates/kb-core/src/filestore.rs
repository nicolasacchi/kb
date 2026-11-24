//! File store writer for the terminal Files feature.
//!
//! [`store_file`] puts one operator or agent file into the files corpus as
//! two siblings under `<YYYY>/<MM>/<termid>/`:
//!
//! - `<sha12>-<slug>.<ext>.kbblob`: the VERBATIM bytes. The extension map
//!   keys on the final extension only, and `kbblob` is not in it, so the
//!   indexer never picks the original up and the artifact server never
//!   treats it as one.
//! - `<sha12>-<slug>.html`: a stub artifact carrying the metadata
//!   (`kb-category` `upload`, tags, sha256, sniffed mime, size, term id,
//!   original name). It is the ONLY file in the pair the indexer reads.
//!
//! Text-like content up to [`INLINE_MAX_BYTES`] can be inlined into the
//! stub so it is searchable, HTML-escaped inside one `<pre>` (never a
//! markdown fence, never a raw `<`), only when the caller sets
//! `index_content`. A caller may pass a scrub hook that runs on the text
//! before escaping.
//!
//! This module is a pure library: it never indexes, serves or opens a
//! `.kbblob` itself, and it has no routes or runner dependency. Callers own
//! the policy (which kb, which quota, which scrub).

use crate::capture::capture_slug_checked;
use crate::fsx::write_atomic;
use crate::ids::ArtifactId;
use crate::parser::slugify_tag;
use std::path::{Path, PathBuf};

/// Largest content (bytes) that is inlined into a stub.
pub const INLINE_MAX_BYTES: usize = 64 * 1024;
/// Default quota for the files corpus, in bytes (sum of every `.kbblob`).
pub const FILES_QUOTA_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Extension of the verbatim original.
pub const BLOB_EXT: &str = "kbblob";

const SLUG_MAX: usize = 48;
const NAME_DISPLAY_MAX: usize = 200;

#[derive(Debug, thiserror::Error)]
pub enum FileStoreError {
    #[error("bad terminal id")]
    BadTermId,
    #[error("timestamp {0} is out of range")]
    BadTime(i64),
    #[error("files quota exceeded: {used} bytes used, limit {limit}")]
    Quota { used: u64, limit: u64 },
    #[error("io: {0}")]
    Io(String),
}

impl From<crate::Error> for FileStoreError {
    fn from(e: crate::Error) -> Self {
        FileStoreError::Io(e.to_string())
    }
}

impl From<std::io::Error> for FileStoreError {
    fn from(e: std::io::Error) -> Self {
        FileStoreError::Io(e.to_string())
    }
}

/// Coarse kind of the stored bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Image,
    Pdf,
    Text,
    Binary,
}

impl FileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FileKind::Image => "image",
            FileKind::Pdf => "pdf",
            FileKind::Text => "text",
            FileKind::Binary => "binary",
        }
    }
}

pub struct StoreInput<'a> {
    /// The files corpus source root (a writable mount).
    pub source_root: &'a Path,
    /// Terminal id, `^[a-z0-9]{1,32}$`.
    pub term_id: &'a str,
    /// The name the file had on the sender's side. Untrusted.
    pub original_name: &'a str,
    pub bytes: &'a [u8],
    /// Extra caller tags (slugified; `upload` and `ext-<ext>` are always set).
    pub tags: &'a [String],
    /// Inline text-like content (up to [`INLINE_MAX_BYTES`]) into the stub.
    pub index_content: bool,
    /// Optional scrub run on the text BEFORE escaping. `None` = verbatim.
    pub scrub: Option<&'a dyn Fn(&str) -> String>,
    /// Injected clock (unix seconds, UTC) deciding the `YYYY/MM` directory.
    pub now_unix: i64,
    /// Corpus quota in bytes; see [`FILES_QUOTA_BYTES`].
    pub quota_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct StoredFile {
    pub id: ArtifactId,
    pub stub_rel: String,
    pub blob_rel: String,
    pub sha256: String,
    pub mime: String,
    pub kind: FileKind,
    pub size: u64,
    pub inlined: bool,
    /// `false` when both files already existed and nothing was rewritten.
    pub created: bool,
}

/// `^[a-z0-9]{1,32}$` (kb-core must not depend on the terminal crates).
pub fn valid_term_id(s: &str) -> bool {
    (1..=32).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// Split an untrusted name into `(display, slug, ext)`. Path separators,
/// `..`, NUL/control characters and leading dots can never survive.
fn name_parts(original: &str) -> (String, String, String) {
    let base = original.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base.chars().filter(|c| !c.is_control()).collect();
    let display: String = cleaned.chars().take(NAME_DISPLAY_MAX).collect();
    let trimmed = cleaned.trim_start_matches('.');
    let (stem, ext) = match trimmed.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, e.to_ascii_lowercase()),
        _ => (trimmed, String::new()),
    };
    let ext_ok = !ext.is_empty()
        && ext.len() <= 8
        && ext
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    let ext = if ext_ok { ext } else { "bin".to_string() };
    let slug = capture_slug_checked(stem)
        .map(|s| {
            s.chars()
                .take(SLUG_MAX)
                .collect::<String>()
                .trim_matches('-')
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "file".to_string());
    (display, slug, ext)
}

/// Is the whole buffer valid UTF-8 without NUL?
fn text_like(bytes: &[u8]) -> bool {
    !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
}

/// Sniff `(kind, mime)`. Never errors.
pub fn sniff(bytes: &[u8], ext: &str) -> (FileKind, &'static str) {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return (FileKind::Image, "image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return (FileKind::Image, "image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return (FileKind::Image, "image/gif");
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return (FileKind::Image, "image/webp");
    }
    if bytes.starts_with(b"%PDF-") {
        return (FileKind::Pdf, "application/pdf");
    }
    // Head check only: a large file's tail may be binary; inlining
    // re-checks the whole buffer.
    let head = &bytes[..bytes.len().min(8192)];
    let head_ok = !head.contains(&0)
        && match std::str::from_utf8(head) {
            Ok(_) => true,
            // A multibyte char cut by the head boundary is not invalid.
            Err(e) => e.error_len().is_none(),
        };
    if head_ok {
        let mime = match ext {
            "json" => "application/json",
            "csv" => "text/csv",
            "md" | "markdown" => "text/markdown",
            "html" | "htm" => "text/html",
            "eml" => "message/rfc822",
            _ => "text/plain",
        };
        return (FileKind::Text, mime);
    }
    (FileKind::Binary, "application/octet-stream")
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

fn keep_char(c: char) -> bool {
    c == '\n' || c == '\t' || !c.is_control()
}

/// Sum of every `*.kbblob` under `root` (no symlink follow).
fn blob_bytes_used(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for ent in rd.flatten() {
            let Ok(md) = std::fs::symlink_metadata(ent.path()) else {
                continue;
            };
            if md.is_dir() {
                stack.push(ent.path());
            } else if md.is_file()
                && ent.path().extension().and_then(|e| e.to_str()) == Some(BLOB_EXT)
            {
                total += md.len();
            }
        }
    }
    total
}

#[allow(clippy::too_many_arguments)]
fn render_stub(
    display: &str,
    ext: &str,
    tags: &[String],
    sha256: &str,
    mime: &str,
    size: u64,
    term_id: &str,
    blob_rel: &str,
    inline: Option<&str>,
) -> String {
    let shown = if display.is_empty() { "file" } else { display };
    let mut all_tags = vec!["upload".to_string(), format!("ext-{ext}")];
    for t in tags {
        let s = slugify_tag(t);
        if !s.is_empty() && !all_tags.contains(&s) {
            all_tags.push(s);
        }
    }
    let mut h = String::new();
    h.push_str("<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\n");
    h.push_str(&format!("<title>{}</title>\n", esc(shown)));
    h.push_str("<meta name=\"kb-category\" content=\"upload\">\n");
    h.push_str(&format!(
        "<meta name=\"kb-tags\" content=\"{}\">\n",
        esc(&all_tags.join(", "))
    ));
    h.push_str(&format!(
        "<meta name=\"kb-upload-sha256\" content=\"{}\">\n",
        esc(sha256)
    ));
    h.push_str(&format!(
        "<meta name=\"kb-upload-mime\" content=\"{}\">\n",
        esc(mime)
    ));
    h.push_str(&format!(
        "<meta name=\"kb-upload-size\" content=\"{size}\">\n"
    ));
    h.push_str(&format!(
        "<meta name=\"kb-upload-term\" content=\"{}\">\n",
        esc(term_id)
    ));
    h.push_str(&format!(
        "<meta name=\"kb-upload-name\" content=\"{}\">\n",
        esc(shown)
    ));
    h.push_str("</head><body>\n");
    h.push_str(&format!("<h1 id=\"upload\">{}</h1>\n", esc(shown)));
    h.push_str("<dl id=\"meta\">\n");
    for (k, v) in [
        ("name", shown.to_string()),
        ("sha256", sha256.to_string()),
        ("mime", mime.to_string()),
        ("size", size.to_string()),
        ("terminal", term_id.to_string()),
        ("blob", blob_rel.to_string()),
    ] {
        h.push_str(&format!("<dt>{k}</dt><dd>{}</dd>\n", esc(&v)));
    }
    h.push_str("</dl>\n");
    if let Some(text) = inline {
        h.push_str(&format!("<pre id=\"content\">{}</pre>\n", esc(text)));
    }
    h.push_str("</body></html>\n");
    h
}

pub fn store_file(input: StoreInput<'_>) -> Result<StoredFile, FileStoreError> {
    if !valid_term_id(input.term_id) {
        return Err(FileStoreError::BadTermId);
    }
    let when = chrono::DateTime::from_timestamp(input.now_unix, 0)
        .ok_or(FileStoreError::BadTime(input.now_unix))?;
    let (display, slug, ext) = name_parts(input.original_name);

    use sha2::{Digest, Sha256};
    let sha256 = hex::encode(Sha256::digest(input.bytes));
    let sha12 = &sha256[..12];
    let dir_rel = format!("{}/{}", when.format("%Y/%m"), input.term_id);
    let stem = format!("{sha12}-{slug}");
    let blob_rel = format!("{dir_rel}/{stem}.{ext}.{BLOB_EXT}");
    let stub_rel = format!("{dir_rel}/{stem}.html");
    let blob_abs = input.source_root.join(&blob_rel);
    let stub_abs = input.source_root.join(&stub_rel);

    let size = input.bytes.len() as u64;
    let (kind, mime) = sniff(input.bytes, &ext);

    let inline_text: Option<String> = if input.index_content
        && kind == FileKind::Text
        && input.bytes.len() <= INLINE_MAX_BYTES
        && text_like(input.bytes)
    {
        let raw = String::from_utf8_lossy(input.bytes).into_owned();
        let scrubbed = match input.scrub {
            Some(f) => f(&raw),
            None => raw,
        };
        Some(scrubbed.chars().filter(|c| keep_char(*c)).collect())
    } else {
        None
    };

    let blob_exists = blob_abs.is_file();
    let stub_exists = stub_abs.is_file();
    let created = !(blob_exists && stub_exists);

    if created {
        if !blob_exists {
            let used = blob_bytes_used(input.source_root);
            if used.saturating_add(size) > input.quota_bytes {
                return Err(FileStoreError::Quota {
                    used,
                    limit: input.quota_bytes,
                });
            }
            write_atomic(&blob_abs, input.bytes)?;
        }
        let stub = render_stub(
            &display,
            &ext,
            input.tags,
            &sha256,
            mime,
            size,
            input.term_id,
            &blob_rel,
            inline_text.as_deref(),
        );
        write_atomic(&stub_abs, stub.as_bytes())?;
    }

    let rel = crate::paths::doc_rel_path(&stub_abs.to_string_lossy(), input.source_root);
    let rel = if rel.is_empty() {
        stub_rel.clone()
    } else {
        rel
    };
    Ok(StoredFile {
        id: ArtifactId::from_path(&rel),
        stub_rel,
        blob_rel,
        sha256,
        mime: mime.to_string(),
        kind,
        size,
        inlined: inline_text.is_some(),
        created,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extmap::ExtensionMap;

    // 2026-11-24T12:00:00Z
    const NOW: i64 = 1_795_521_600;
    const BIG: u64 = 1 << 30;

    fn put(
        root: &Path,
        name: &str,
        bytes: &[u8],
        index: bool,
    ) -> Result<StoredFile, FileStoreError> {
        store_file(StoreInput {
            source_root: root,
            term_id: "ab12",
            original_name: name,
            bytes,
            tags: &[],
            index_content: index,
            scrub: None,
            now_unix: NOW,
            quota_bytes: BIG,
        })
    }

    fn read(root: &Path, rel: &str) -> String {
        std::fs::read_to_string(root.join(rel)).unwrap()
    }

    fn all_files(root: &Path) -> Vec<PathBuf> {
        let mut out = vec![];
        let mut st = vec![root.to_path_buf()];
        while let Some(d) = st.pop() {
            for e in std::fs::read_dir(d).unwrap().flatten() {
                if e.path().is_dir() {
                    st.push(e.path());
                } else {
                    out.push(e.path());
                }
            }
        }
        out
    }

    #[test]
    fn layout_uses_injected_clock_and_sha12() {
        let t = tempfile::tempdir().unwrap();
        let r = put(t.path(), "Notes.TXT", b"hello", false).unwrap();
        let sha = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(b"hello"));
        assert_eq!(r.sha256, sha);
        assert_eq!(
            r.blob_rel,
            format!("2026/11/ab12/{}-notes.txt.kbblob", &sha[..12])
        );
        assert_eq!(
            r.stub_rel,
            format!("2026/11/ab12/{}-notes.html", &sha[..12])
        );
        assert_eq!(std::fs::read(t.path().join(&r.blob_rel)).unwrap(), b"hello");
        assert!(r.created);
        assert_eq!(r.size, 5);
    }

    #[test]
    fn name_policy_table() {
        let long = format!("{}.txt", "a".repeat(300));
        let slug48 = "a".repeat(48);
        let cases: Vec<(&str, &str, &str)> = vec![
            ("../../etc/passwd", "passwd", "bin"),
            ("..\\..\\win\\x.PNG", "x", "png"),
            (".bashrc", "bashrc", "bin"),
            ("", "file", "bin"),
            ("..", "file", "bin"),
            ("a\0b.txt", "ab", "txt"),
            ("Ünï cödé.json", "n-c-d", "json"),
            ("report.toolongext1", "report", "bin"),
            ("weird.t-x", "weird", "bin"),
            (&long, &slug48, "txt"),
        ];
        for (name, slug, ext) in cases {
            let (_, s, e) = name_parts(name);
            assert_eq!((s.as_str(), e.as_str()), (slug, ext), "name {name:?}");
            assert!(!s.contains(['/', '\\', '.']) && !s.starts_with('.'));
        }
        let t = tempfile::tempdir().unwrap();
        let r = put(t.path(), "../../etc/passwd", b"x", false).unwrap();
        assert!(r.blob_rel.starts_with("2026/11/ab12/"));
        assert!(!r.blob_rel.contains(".."));
        // Nothing escaped the root.
        for f in all_files(t.path()) {
            assert!(f.starts_with(t.path().join("2026/11/ab12")));
        }
    }

    #[test]
    fn bad_term_ids_write_nothing() {
        let t = tempfile::tempdir().unwrap();
        let long33 = "a".repeat(33);
        for id in ["../x", "A", long33.as_str(), "", "a-b", "a/b"] {
            let r = store_file(StoreInput {
                source_root: t.path(),
                term_id: id,
                original_name: "f.txt",
                bytes: b"x",
                tags: &[],
                index_content: false,
                scrub: None,
                now_unix: NOW,
                quota_bytes: BIG,
            });
            assert!(matches!(r, Err(FileStoreError::BadTermId)), "{id:?}");
        }
        assert!(all_files(t.path()).is_empty());
    }

    #[test]
    fn restore_is_idempotent_and_blob_precedes_stub() {
        let t = tempfile::tempdir().unwrap();
        let a = put(t.path(), "f.txt", b"same", true).unwrap();
        assert!(t.path().join(&a.blob_rel).is_file() && t.path().join(&a.stub_rel).is_file());
        let m1 = std::fs::metadata(t.path().join(&a.stub_rel))
            .unwrap()
            .modified()
            .unwrap();
        let b1 = std::fs::metadata(t.path().join(&a.blob_rel))
            .unwrap()
            .modified()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        let b = put(t.path(), "f.txt", b"same", true).unwrap();
        assert!(!b.created);
        assert_eq!(a.id, b.id);
        assert_eq!(
            m1,
            std::fs::metadata(t.path().join(&a.stub_rel))
                .unwrap()
                .modified()
                .unwrap()
        );
        assert_eq!(
            b1,
            std::fs::metadata(t.path().join(&a.blob_rel))
                .unwrap()
                .modified()
                .unwrap()
        );
    }

    #[test]
    fn sniff_table() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n....", "png").1, "image/png");
        assert_eq!(sniff(b"\xff\xd8\xff\xe0", "jpg").1, "image/jpeg");
        assert_eq!(sniff(b"GIF89a..", "gif").1, "image/gif");
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 ", "webp").1, "image/webp");
        assert_eq!(
            sniff(b"%PDF-1.7", "pdf"),
            (FileKind::Pdf, "application/pdf")
        );
        assert_eq!(sniff(b"{}", "json").1, "application/json");
        assert_eq!(sniff(b"a,b", "csv").1, "text/csv");
        assert_eq!(sniff(b"hi", "xyz").1, "text/plain");
        assert_eq!(
            sniff(b"a\0b", "bin"),
            (FileKind::Binary, "application/octet-stream")
        );
        assert_eq!(sniff(&[0xff, 0xfe, 0xfd], "bin").0, FileKind::Binary);
        assert_eq!(sniff(b"", "txt").0, FileKind::Text);
    }

    #[test]
    fn hostile_content_cannot_break_out_of_the_stub() {
        let hostile = "```\n</pre><script>alert(1)</script>\n<template id=\"kb-prompt\">x</template>\n---\ntitle: pwned\n---\n&amp; ``` ````\nneedle-token-42\n";
        let t = tempfile::tempdir().unwrap();
        let r = put(t.path(), "evil.md", hostile.as_bytes(), true).unwrap();
        assert!(r.inlined);
        let stub = read(t.path(), &r.stub_rel);
        assert_eq!(stub.matches("<pre").count(), 1, "{stub}");
        assert!(stub.contains("&lt;/pre&gt;"));
        assert!(!stub.contains("<script"));
        assert!(!stub.contains("<template"));
        assert!(stub.contains("&amp;amp;"));
        let f = crate::parser::extract(&stub);
        assert_eq!(f.prompt, None);
        assert_eq!(f.kb_category.as_deref(), Some("upload"));
        assert!(!f.has_script);
        assert!(
            f.code.contains("needle-token-42")
                && f.code.contains("</pre><script>alert(1)</script>"),
            "hostile text must be plain text: {:?}",
            f.code
        );
        assert_eq!(
            std::fs::read(t.path().join(&r.blob_rel)).unwrap(),
            hostile.as_bytes()
        );
    }

    #[test]
    fn hostile_name_and_tags_are_escaped() {
        let t = tempfile::tempdir().unwrap();
        let tags = vec!["<b>x\"y".to_string()];
        let r = store_file(StoreInput {
            source_root: t.path(),
            term_id: "t1",
            original_name: "\"><script>x</script>.txt",
            bytes: b"hi",
            tags: &tags,
            index_content: false,
            scrub: None,
            now_unix: NOW,
            quota_bytes: BIG,
        })
        .unwrap();
        let stub = read(t.path(), &r.stub_rel);
        assert!(!stub.contains("<script"));
        let f = crate::parser::extract(&stub);
        assert_eq!(f.kb_category.as_deref(), Some("upload"));
        assert!(f.tags.contains(&"upload".to_string()));
        assert!(f.tags.contains(&"ext-txt".to_string()));
        assert!(f.tags.contains(&"b-x-y".to_string()), "{:?}", f.tags);
    }

    #[test]
    fn kbblob_is_not_indexable() {
        let m = ExtensionMap::default();
        assert!(!m.is_indexable(Path::new("a/x.json.kbblob")));
        assert!(!m.is_indexable(Path::new("a/x.html.kbblob")));
        assert!(!m.is_indexable(Path::new("a/x.md.kbblob")));
        assert!(m.is_indexable(Path::new("a/x.html")));
    }

    #[test]
    fn inlining_rules() {
        let t = tempfile::tempdir().unwrap();
        let off = put(t.path(), "a.txt", b"findme", false).unwrap();
        assert!(!off.inlined);
        assert!(!read(t.path(), &off.stub_rel).contains("<pre"));

        let exact = vec![b'a'; INLINE_MAX_BYTES];
        assert!(put(t.path(), "b.txt", &exact, true).unwrap().inlined);
        let over = vec![b'a'; INLINE_MAX_BYTES + 1];
        let o = put(t.path(), "c.txt", &over, true).unwrap();
        assert!(!o.inlined);
        assert!(!read(t.path(), &o.stub_rel).contains("<pre"));

        let nul = put(t.path(), "d.txt", b"ab\0cd", true).unwrap();
        assert!(!nul.inlined);
        let bin = put(t.path(), "e.bin", &[0xff, 0xfe, 0x00, 0x01], true).unwrap();
        assert!(!bin.inlined);
        // Binary tail past the sniffed head is still refused.
        let mut tail = vec![b'a'; 9000];
        tail.push(0xff);
        assert!(!put(t.path(), "f.txt", &tail, true).unwrap().inlined);

        let ctl = put(t.path(), "g.txt", b"a\x07b\x1b[0m\tc\nd", true).unwrap();
        let stub = read(t.path(), &ctl.stub_rel);
        assert!(
            stub.contains("<pre id=\"content\">ab[0m\tc\nd</pre>"),
            "{stub}"
        );
    }

    #[test]
    fn scrub_hook_runs_once_before_escaping() {
        let t = tempfile::tempdir().unwrap();
        let calls = std::cell::Cell::new(0);
        let scrub = |s: &str| {
            calls.set(calls.get() + 1);
            s.replace("SECRET", "<redacted>")
        };
        let r = store_file(StoreInput {
            source_root: t.path(),
            term_id: "t1",
            original_name: "k.txt",
            bytes: b"token SECRET here",
            tags: &[],
            index_content: true,
            scrub: Some(&scrub),
            now_unix: NOW,
            quota_bytes: BIG,
        })
        .unwrap();
        assert_eq!(calls.get(), 1);
        let stub = read(t.path(), &r.stub_rel);
        assert!(stub.contains("token &lt;redacted&gt; here"));
        assert!(!stub.contains("SECRET"));
        // The blob stays verbatim; scrubbing only shapes the stub.
        assert_eq!(
            std::fs::read(t.path().join(&r.blob_rel)).unwrap(),
            b"token SECRET here"
        );

        let none = put(t.path(), "k2.txt", b"token SECRET here", true).unwrap();
        assert!(read(t.path(), &none.stub_rel).contains("token SECRET here"));
    }

    #[test]
    fn quota_refuses_without_writing_and_allows_restore() {
        let t = tempfile::tempdir().unwrap();
        let mk = |root: &Path, name: &str, b: &[u8], q: u64| {
            store_file(StoreInput {
                source_root: root,
                term_id: "t1",
                original_name: name,
                bytes: b,
                tags: &[],
                index_content: false,
                scrub: None,
                now_unix: NOW,
                quota_bytes: q,
            })
        };
        let e = mk(t.path(), "a.bin", b"0123456789", 5).unwrap_err();
        assert!(matches!(e, FileStoreError::Quota { used: 0, limit: 5 }));
        assert!(all_files(t.path()).is_empty());

        mk(t.path(), "a.bin", b"0123456789", 10).unwrap();
        let e = mk(t.path(), "b.bin", b"x", 10).unwrap_err();
        assert!(matches!(
            e,
            FileStoreError::Quota {
                used: 10,
                limit: 10
            }
        ));
        let n = all_files(t.path()).len();
        assert_eq!(n, 2);
        // Re-storing the same blob does not count twice.
        let again = mk(t.path(), "a.bin", b"0123456789", 10).unwrap();
        assert!(!again.created);
    }
}
