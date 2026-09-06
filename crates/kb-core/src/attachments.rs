//! Y-track — comment/reply file & image attachments.
//!
//! The blob bytes live on disk under
//! `<state>/<kb>/.attachments/<artifact_id>/<aid>` (keyed by a random
//! `a_<hex>` id); a per-artifact `_manifest.json` holds the metadata
//! (filename / content-type / size / created_at / author / adopted). The
//! review JSON carries a denormalized [`crate::review::Attachment`] copy
//! inline (the read path); this module owns the storage-side primitives:
//!
//!   - [`sniff_allowed`] — the magic-byte content sniff that IS the upload
//!     gate. NEVER trusts a client-supplied type. SVG/HTML/JS are not
//!     recognised as inline-image types — at most they sniff as
//!     `text/plain` and the serve route force-downloads them, so user
//!     content can't execute from the daemon origin.
//!   - [`sanitize_filename`] — a display-only basename (never a path).
//!   - [`Manifest`] + [`load_manifest`]/[`save_manifest`] — the staging /
//!     GC record, guarded by the same per-kb `review_lock` as the review
//!     file.
//!   - [`gc_plan`] — the pure reference-counted GC decision (adopted +
//!     unreferenced → reap now; staged + past grace → reap; staged + fresh
//!     → keep, so an in-flight compose never loses its file).
//!   - [`rewrite_attachment_refs`] — `attachment:<aid>` → a relative bundle
//!     path, for the export/share portability paths.
//!
//! Everything here is pure / filesystem-only and LLM-free; the HTTP wiring
//! (multipart, body limits, serve headers, the review_lock critical
//! section) lives in `kb-server`.

use crate::review::{Attachment, Author};
use crate::{Error, Result};
use chrono::{DateTime, Duration, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;

/// Default per-file upload cap (10 MiB). Override via
/// `[server.attachments] max_file_bytes`.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Default cap on attachments adopted onto one comment/reply.
pub const DEFAULT_MAX_PER_COMMENT: usize = 20;

/// Default grace window (hours) before a staged-but-never-adopted blob is
/// GC'd. Generous so a slow or restarted compose doesn't lose its upload.
pub const DEFAULT_GC_GRACE_HOURS: i64 = 24;

// --- content sniff + filename hygiene --------------------------------------

/// Magic-byte content sniff against the v1 allowlist. Returns the canonical
/// `Content-Type` (a `&'static str` the serve route emits verbatim), or
/// `None` if the bytes aren't an allowed type.
///
/// This sniff IS the upload gate — the client's declared type is never
/// trusted. The allowlist is raster images (png/jpeg/gif/webp), PDF, and
/// UTF-8 text. SVG/HTML/JS are deliberately NOT recognised as image types;
/// valid-UTF-8 markup sniffs as `text/plain` and the serve route forces a
/// download (`Content-Disposition: attachment` + `nosniff`), so it can
/// never execute inline from the daemon origin. Truly binary unknown types
/// are rejected.
pub fn sniff_allowed(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 8 && bytes[..8] == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A] {
        return Some("image/png");
    }
    if bytes.len() >= 3 && bytes[..3] == [0xFF, 0xD8, 0xFF] {
        return Some("image/jpeg");
    }
    if bytes.len() >= 6 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.len() >= 5 && &bytes[..5] == b"%PDF-" {
        return Some("application/pdf");
    }
    if is_plain_text(bytes) {
        return Some("text/plain; charset=utf-8");
    }
    None
}

/// Valid, non-empty UTF-8 with no control chars other than tab / newline /
/// carriage-return — i.e. human-readable text (logs, code, diffs, csv, md).
fn is_plain_text(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => !s
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r')),
        Err(_) => false,
    }
}

/// Whether a (sniffed) content-type is a raster image the serve route may
/// send inline. Everything else is force-downloaded — this is the keystone
/// of the attachment XSS guard (root invariant #18).
pub fn is_inline_image(content_type: &str) -> bool {
    matches!(
        content_type,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}

/// Reduce an uploaded filename to a safe display basename. Strips directory
/// components (`/` and `\`), control chars, and a `"` (which would break a
/// quoted `Content-Disposition`); caps the length; maps empty / `.` / `..`
/// to `"file"`. NEVER used to build a filesystem path — the blob is stored
/// under the random `aid` — only for display + `Content-Disposition`.
pub fn sanitize_filename(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if c == '"' { '_' } else { c })
        .collect();
    let capped: String = cleaned.trim().chars().take(120).collect();
    let capped = capped.trim().to_string();
    if capped.is_empty() || capped == "." || capped == ".." {
        "file".to_string()
    } else {
        capped
    }
}

// --- manifest (staging / GC record) ----------------------------------------

/// Per-artifact attachment metadata map. Lives at
/// `<state>/<kb>/.attachments/<artifact_id>/_manifest.json`. Tolerant load
/// (missing/corrupt → empty) mirrors the anchor-state sidecar.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub items: BTreeMap<String, ManifestEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestEntry {
    pub filename: String,
    #[serde(rename = "contentType")]
    pub content_type: String,
    pub size: u64,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
    pub author: Author,
    /// `true` once a comment/reply has adopted this blob. A staged
    /// (`false`) entry is GC-eligible only after the grace window; an
    /// adopted entry is GC'd the instant it's referenced by no live
    /// comment/reply (see [`gc_plan`]).
    #[serde(default)]
    pub adopted: bool,
}

impl ManifestEntry {
    /// Build the denormalized [`Attachment`] the review JSON carries, given
    /// the blob's `aid` (the manifest key).
    pub fn to_attachment(&self, aid: &str) -> Attachment {
        Attachment {
            id: aid.to_string(),
            filename: self.filename.clone(),
            content_type: self.content_type.clone(),
            size: self.size,
            created_at: self.created_at,
            author: self.author,
            user: None,
        }
    }
}

/// Load the manifest, tolerating a missing or corrupt file (→ empty). The
/// manifest is non-authoritative cache-like state (the review JSON owns the
/// adopted set), so a parse error self-heals rather than failing a request.
pub fn load_manifest(path: &Path) -> Manifest {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => Manifest::default(),
    }
}

/// Atomically persist the manifest (tmp + rename + parent fsync), reusing
/// the shared `fsx::write_atomic` primitive.
pub fn save_manifest(path: &Path, manifest: &Manifest) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(manifest).map_err(Error::from)?;
    crate::fsx::write_atomic(path, &bytes)
}

/// Pure GC decision over a manifest. Given the set of attachment ids still
/// referenced by some live comment/reply (`referenced`), the current time,
/// and the staged-grace window, return `(aids_to_delete, pruned_manifest)`:
///
///   - adopted + referenced  → keep
///   - adopted + unreferenced → delete now (its owning comment/reply is gone)
///   - staged  + within grace → keep (an in-flight / restarted compose)
///   - staged  + past grace   → delete (abandoned upload)
///
/// The caller deletes the returned blob files, then saves `pruned_manifest`.
pub fn gc_plan(
    manifest: &Manifest,
    referenced: &HashSet<String>,
    now: DateTime<Utc>,
    grace: Duration,
) -> (Vec<String>, Manifest) {
    let mut to_delete = Vec::new();
    let mut kept = Manifest {
        version: manifest.version,
        items: BTreeMap::new(),
    };
    for (aid, entry) in &manifest.items {
        // A referenced blob is ALWAYS kept, regardless of its `adopted`
        // flag — this is robust to the tiny crash window between a route
        // saving the review file (which references the blob) and saving the
        // manifest (which flips `adopted`). Otherwise: an unreferenced
        // adopted blob is an orphan (its comment/reply was deleted) and is
        // reaped now; an unreferenced staged blob is reaped after the grace
        // window (an abandoned compose).
        let keep = referenced.contains(aid)
            || (!entry.adopted && now.signed_duration_since(entry.created_at) < grace);
        if keep {
            kept.items.insert(aid.clone(), entry.clone());
        } else {
            to_delete.push(aid.clone());
        }
    }
    (to_delete, kept)
}

// --- inline-ref rewriting (export / share portability) ---------------------

/// Rewrite `attachment:<aid>` inline references in a comment body to the
/// path each `aid` maps to (e.g. `attachments/a_xxxx-chart.png`), for the
/// self-contained export bundle + static-share render. Ids absent from
/// `map` are left verbatim (they render as a placeholder downstream rather
/// than a broken relative link). Operates on the markdown source, so the
/// surrounding `![alt](…)` / `[label](…)` syntax is preserved.
pub fn rewrite_attachment_refs(body: &str, map: &BTreeMap<String, String>) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"attachment:(a_[0-9a-fA-F]+)").expect("valid regex"));
    re.replace_all(body, |caps: &regex::Captures| {
        let whole = caps.get(0).map(|m| m.as_str()).unwrap_or_default();
        let aid = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
        match map.get(aid) {
            Some(path) => path.clone(),
            None => whole.to_string(),
        }
    })
    .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes() -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        v.extend_from_slice(&[0, 0, 0, 0]); // a little body
        v
    }

    // invariant:18 sniff-gate
    #[test]
    fn sniff_recognises_each_allowed_type() {
        assert_eq!(sniff_allowed(&png_bytes()), Some("image/png"));
        assert_eq!(
            sniff_allowed(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0]),
            Some("image/jpeg")
        );
        assert_eq!(sniff_allowed(b"GIF89a\x00\x00"), Some("image/gif"));
        assert_eq!(sniff_allowed(b"GIF87a\x00\x00"), Some("image/gif"));
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBP....");
        assert_eq!(sniff_allowed(&webp), Some("image/webp"));
        assert_eq!(sniff_allowed(b"%PDF-1.7\n..."), Some("application/pdf"));
        assert_eq!(
            sniff_allowed(b"a plain text log line\n"),
            Some("text/plain; charset=utf-8")
        );
    }

    #[test]
    fn sniff_rejects_binary_and_empty() {
        // Invalid UTF-8 (0xff) and a NUL-bearing control payload → rejected.
        assert_eq!(sniff_allowed(&[0x00, 0x01, 0x02, 0xFF, 0xFE]), None);
        assert_eq!(sniff_allowed(&[]), None);
    }

    #[test]
    fn html_svg_js_never_sniff_as_inline_image() {
        // The XSS guard at the sniff layer: markup payloads must NEVER be
        // classified as a raster image (the only inline-served type). Valid
        // UTF-8 markup is accepted only as text/plain, which the serve route
        // force-downloads — so it can't execute from the daemon origin.
        for payload in [
            &b"<script>alert(1)</script>"[..],
            b"<svg onload=alert(1)></svg>",
            b"<!doctype html><html><body>x</body></html>",
        ] {
            let ct = sniff_allowed(payload).expect("utf-8 markup accepted as text");
            assert_eq!(ct, "text/plain; charset=utf-8");
            assert!(!is_inline_image(ct), "markup must never be inline-served");
        }
        // A file *named* `.png` whose BYTES are HTML sniffs as text, not png
        // — the sniff ignores the (untrusted) filename entirely.
        assert_eq!(
            sniff_allowed(b"<html>not really a png</html>"),
            Some("text/plain; charset=utf-8")
        );
    }

    #[test]
    fn is_inline_image_is_raster_only() {
        for ct in ["image/png", "image/jpeg", "image/gif", "image/webp"] {
            assert!(is_inline_image(ct));
        }
        for ct in [
            "application/pdf",
            "text/plain; charset=utf-8",
            "image/svg+xml",
            "text/html",
            "application/octet-stream",
        ] {
            assert!(!is_inline_image(ct), "{ct} must not be inline");
        }
    }

    #[test]
    fn sanitize_filename_strips_paths_controls_and_quotes() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("a/b/c.png"), "c.png");
        assert_eq!(sanitize_filename(r"C:\Users\x\shot.png"), "shot.png");
        assert_eq!(sanitize_filename("a\x00b\nc.png"), "abc.png");
        assert_eq!(sanitize_filename("evil\"name.png"), "evil_name.png");
        assert_eq!(sanitize_filename(".."), "file");
        assert_eq!(sanitize_filename("."), "file");
        assert_eq!(sanitize_filename(""), "file");
        assert_eq!(sanitize_filename("   "), "file");
        // Length cap.
        let long = "x".repeat(300);
        assert_eq!(sanitize_filename(&long).len(), 120);
    }

    fn entry(adopted: bool, created_at: DateTime<Utc>) -> ManifestEntry {
        ManifestEntry {
            filename: "f.png".into(),
            content_type: "image/png".into(),
            size: 10,
            created_at,
            author: Author::You,
            adopted,
        }
    }

    // invariant:18 ref-counted-gc
    #[test]
    fn gc_plan_reaps_orphans_and_expired_staged_but_keeps_fresh_and_referenced() {
        let now = Utc::now();
        let grace = Duration::hours(24);
        let fresh = now - Duration::hours(1);
        let old = now - Duration::hours(48);

        let mut m = Manifest::default();
        m.items.insert("a_referenced".into(), entry(true, old)); // adopted + referenced → keep
        m.items.insert("a_orphan".into(), entry(true, fresh)); // adopted + unreferenced → delete
        m.items.insert("a_staged_fresh".into(), entry(false, fresh)); // staged < grace → keep
        m.items.insert("a_staged_old".into(), entry(false, old)); // staged > grace → delete

        let referenced: HashSet<String> = ["a_referenced".to_string()].into_iter().collect();
        let (to_delete, kept) = gc_plan(&m, &referenced, now, grace);

        let mut del = to_delete.clone();
        del.sort();
        assert_eq!(
            del,
            vec!["a_orphan".to_string(), "a_staged_old".to_string()]
        );
        assert!(kept.items.contains_key("a_referenced"));
        assert!(kept.items.contains_key("a_staged_fresh"));
        assert_eq!(kept.items.len(), 2);
    }

    #[test]
    fn gc_plan_keeps_referenced_even_if_marked_staged() {
        // Crash-window robustness: a blob the review file references must be
        // kept even if the manifest still marks it `staged` and it's well
        // past the grace window (review saved, manifest flip not yet
        // persisted before a crash). `referenced` wins over `adopted`.
        let now = Utc::now();
        let mut m = Manifest::default();
        m.items
            .insert("a_ref".into(), entry(false, now - Duration::hours(72)));
        let referenced: HashSet<String> = ["a_ref".to_string()].into_iter().collect();
        let (to_delete, kept) = gc_plan(&m, &referenced, now, Duration::hours(24));
        assert!(to_delete.is_empty(), "a referenced blob is never reaped");
        assert!(kept.items.contains_key("a_ref"));
    }

    #[test]
    fn manifest_save_load_round_trip_and_tolerant() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sub/_manifest.json");
        // Missing file → empty.
        assert!(load_manifest(&path).items.is_empty());

        let mut m = Manifest::default();
        m.items.insert("a_one".into(), entry(true, Utc::now()));
        save_manifest(&path, &m).unwrap();

        let loaded = load_manifest(&path);
        assert_eq!(loaded.items.len(), 1);
        let e = loaded.items.get("a_one").unwrap();
        assert_eq!(e.content_type, "image/png");
        assert!(e.adopted);
        // camelCase on the wire.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"contentType\"") && raw.contains("\"createdAt\""));

        // Corrupt file → tolerant empty (never errors a request).
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(load_manifest(&path).items.is_empty());
    }

    #[test]
    fn manifest_entry_to_attachment_copies_fields() {
        let e = entry(true, Utc::now());
        let att = e.to_attachment("a_xyz");
        assert_eq!(att.id, "a_xyz");
        assert_eq!(att.filename, "f.png");
        assert_eq!(att.content_type, "image/png");
        assert_eq!(att.size, 10);
    }

    #[test]
    fn rewrite_attachment_refs_maps_known_and_leaves_unknown() {
        let mut map = BTreeMap::new();
        map.insert(
            "a_aaaaaaaaaaaa".to_string(),
            "attachments/a_aaaaaaaaaaaa-chart.png".to_string(),
        );
        let body = "See ![chart](attachment:a_aaaaaaaaaaaa) and \
                    [missing](attachment:a_bbbbbbbbbbbb).";
        let out = rewrite_attachment_refs(body, &map);
        assert!(out.contains("![chart](attachments/a_aaaaaaaaaaaa-chart.png)"));
        // Unknown id left verbatim (renders as a placeholder downstream).
        assert!(out.contains("[missing](attachment:a_bbbbbbbbbbbb)"));
    }
}
