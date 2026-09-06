//! X1 (v0.24) — the configurable indexable-extension map.
//!
//! Which file extensions a kb indexes, and which of the two existing parse
//! pipelines each maps onto, used to be hardcoded in
//! [`crate::indexer::is_indexable`] / [`crate::indexer::is_markdown`]
//! (`html`/`htm` → HTML, `md`/`markdown` → Markdown). This module lifts that
//! into a per-kb-resolvable [`ExtensionMap`] so an operator can, e.g., index
//! `.txt` through the Markdown pipeline.
//!
//! **D1 (operator-locked): there are exactly two parse pipelines and no more.**
//! An extension maps ONLY onto [`Pipeline::Html`] or [`Pipeline::Markdown`] —
//! the two pipelines `index_file` already drives. An unknown pipeline name is a
//! hard config error (see [`ExtensionMap::from_config`] / `KbConfig::validate`),
//! never a new parser. This module adds configurability, NOT extensibility of
//! the parse surface.
//!
//! Matching is on a path's FINAL extension only (via `Path::extension`), so a
//! `capture.html.tmp` temp file has extension `tmp` and never matches `html` —
//! the same collision-avoidance the pre-X1 `is_indexable` had.

use std::collections::BTreeMap;
use std::path::Path;

/// The parse pipeline an extension maps onto. D1: these two are the ENTIRE
/// set — `index_file` dispatches [`crate::parser::extract`] for `Html` and
/// [`crate::parser::extract_markdown`] for `Markdown`. Do NOT add a third
/// variant (that would mean a new parser, which the milestone forbids).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pipeline {
    Html,
    Markdown,
}

impl Pipeline {
    /// Parse a config pipeline name (case-insensitive, trimmed). Returns
    /// `None` for anything other than `html` / `markdown` — the caller turns
    /// that into a hard config error.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "html" => Some(Pipeline::Html),
            "markdown" => Some(Pipeline::Markdown),
            _ => None,
        }
    }

    /// The canonical config name (round-trips through [`Pipeline::parse`]).
    pub fn as_str(self) -> &'static str {
        match self {
            Pipeline::Html => "html",
            Pipeline::Markdown => "markdown",
        }
    }
}

/// Maps a lowercased, dot-less extension (`"html"`, `"txt"`) to the parse
/// [`Pipeline`] the indexer uses for it. Also THE ingest gate: an extension
/// absent from the map is not indexable.
///
/// Resolved per-kb by `KbConfig::resolved_extension_map` (per-kb section →
/// daemon `[indexer]` default → the built-in [`ExtensionMap::default`]).
/// Threaded to the ingest seams via the [`crate::indexer::IngestSink`] (walk /
/// watcher / reconcile) and as an explicit arg to `index_file` (parse
/// dispatch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionMap {
    map: BTreeMap<String, Pipeline>,
}

impl Default for ExtensionMap {
    /// The built-in default = exactly the pre-X1 hardcoded set: `html`/`htm`
    /// → HTML, `md`/`markdown` → Markdown. A daemon with no
    /// `indexable_extensions` configured anywhere behaves identically to
    /// before X1.
    fn default() -> Self {
        let mut map = BTreeMap::new();
        map.insert("html".to_string(), Pipeline::Html);
        map.insert("htm".to_string(), Pipeline::Html);
        map.insert("md".to_string(), Pipeline::Markdown);
        map.insert("markdown".to_string(), Pipeline::Markdown);
        Self { map }
    }
}

/// Validate a single `extension = "pipeline"` config entry. Returns `Err`
/// with a human-readable message when the extension key is empty or contains
/// a dot, or the pipeline name is neither `html` nor `markdown`. Shared by
/// [`ExtensionMap::from_config`] (boot-time build) and `KbConfig::validate`
/// (write-time hard check) so the two can't diverge.
pub fn validate_entry(ext: &str, pipeline: &str) -> Result<(), String> {
    let key = ext.trim();
    if key.is_empty() {
        return Err("extension key must not be empty".to_string());
    }
    // `Path::extension` never yields a dot, so a dotted key (`.txt`,
    // `tar.gz`) could never match a real file — reject it as a config typo
    // rather than silently indexing nothing.
    if key.contains('.') {
        return Err(format!(
            "extension key `{key}` must be a bare extension with no dot \
             (write `txt`, not `.txt` or `tar.gz`)"
        ));
    }
    if Pipeline::parse(pipeline).is_none() {
        return Err(format!(
            "`{pipeline}` is not a known parse pipeline (expected `html` or `markdown`)"
        ));
    }
    Ok(())
}

impl ExtensionMap {
    /// Build a map from a raw `extension → pipeline-name` config table
    /// (`Option<BTreeMap<String,String>>` on `IndexerSection` / `KbSection`).
    /// Extension keys are trimmed + lowercased; pipeline names are parsed to
    /// [`Pipeline`]. Returns `Err` on the first invalid entry (see
    /// [`validate_entry`]) — the boot resolver logs the error and falls back
    /// to the next precedence layer, and `KbConfig::validate` rejects the
    /// write outright.
    pub fn from_config(raw: &BTreeMap<String, String>) -> Result<Self, String> {
        let mut map = BTreeMap::new();
        for (ext, pipeline_name) in raw {
            validate_entry(ext, pipeline_name)?;
            let key = ext.trim().to_ascii_lowercase();
            // `validate_entry` above already guaranteed a known pipeline.
            let pipeline = Pipeline::parse(pipeline_name)
                .expect("validate_entry guarantees a known pipeline name");
            map.insert(key, pipeline);
        }
        Ok(Self { map })
    }

    /// The parse pipeline for `path`, keyed on its FINAL extension only
    /// (lowercased). `None` when the extension is unmapped — which the ingest
    /// gate reads as "not indexable".
    pub fn pipeline(&self, path: &Path) -> Option<Pipeline> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        self.map.get(&ext).copied()
    }

    /// THE ingest gate: is `path`'s extension present in this map? Replaces the
    /// pre-X1 free `indexer::is_indexable`.
    pub fn is_indexable(&self, path: &Path) -> bool {
        self.pipeline(path).is_some()
    }

    /// True when `path`'s mapped pipeline is [`Pipeline::Markdown`] — the
    /// per-kb-resolved replacement for the hardcoded `crate::indexer::is_markdown`
    /// extension check at every render/parse-dispatch call site (artifact serve,
    /// download-bytes, meta-tag edit, graph report, …). An unmapped extension
    /// (not indexable at all) is never Markdown here.
    pub fn is_markdown(&self, path: &Path) -> bool {
        matches!(self.pipeline(path), Some(Pipeline::Markdown))
    }

    /// Number of mapped extensions (used only by tests / diagnostics).
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True when no extension is mapped (the map indexes nothing).
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn default_map_matches_pre_x1_behaviour() {
        let m = ExtensionMap::default();
        // Indexable set: html/htm/md/markdown (case-insensitive), nothing else.
        assert!(m.is_indexable(Path::new("/x/y.html")));
        assert!(m.is_indexable(Path::new("/x/y.HTML")));
        assert!(m.is_indexable(Path::new("/x/y.htm")));
        assert!(m.is_indexable(Path::new("/x/y.md")));
        assert!(m.is_indexable(Path::new("/x/y.markdown")));
        assert!(!m.is_indexable(Path::new("/x/y.txt")));
        assert!(!m.is_indexable(Path::new("/x/y")));
        // Pipeline dispatch matches the old is_markdown gate.
        assert_eq!(m.pipeline(Path::new("a.html")), Some(Pipeline::Html));
        assert_eq!(m.pipeline(Path::new("a.htm")), Some(Pipeline::Html));
        assert_eq!(m.pipeline(Path::new("a.MD")), Some(Pipeline::Markdown));
        assert_eq!(
            m.pipeline(Path::new("a.markdown")),
            Some(Pipeline::Markdown)
        );
        assert_eq!(m.pipeline(Path::new("a.txt")), None);
    }

    #[test]
    fn matches_final_extension_only() {
        // A `.html.tmp` capture temp file must NOT match `html` — its final
        // extension is `tmp` (the pre-X1 collision guard).
        let m = ExtensionMap::default();
        assert!(!m.is_indexable(Path::new("capture.html.tmp")));
        assert_eq!(m.pipeline(Path::new("capture.html.tmp")), None);
    }

    #[test]
    fn from_config_maps_txt_to_markdown() {
        let m = ExtensionMap::from_config(&cfg(&[("txt", "markdown"), ("html", "html")])).unwrap();
        assert_eq!(m.pipeline(Path::new("notes.txt")), Some(Pipeline::Markdown));
        assert!(m.is_indexable(Path::new("notes.txt")));
        assert_eq!(m.pipeline(Path::new("page.html")), Some(Pipeline::Html));
        // Only what the config named is indexable — md is NOT in this map.
        assert!(!m.is_indexable(Path::new("readme.md")));
    }

    #[test]
    fn is_markdown_reflects_the_resolved_pipeline_not_the_raw_extension() {
        // SC5 — the whole point of this helper: a config-mapped `.txt` reads as
        // Markdown, while the built-in default still gates on `.md`/`.markdown`.
        let mapped = ExtensionMap::from_config(&cfg(&[("txt", "markdown")])).unwrap();
        assert!(mapped.is_markdown(Path::new("notes.txt")));
        assert!(
            !mapped.is_markdown(Path::new("notes.md")),
            "not in THIS map"
        );

        let default = ExtensionMap::default();
        assert!(default.is_markdown(Path::new("a.md")));
        assert!(default.is_markdown(Path::new("a.markdown")));
        assert!(!default.is_markdown(Path::new("a.html")));
        assert!(!default.is_markdown(Path::new("a.txt")));
        assert!(!default.is_markdown(Path::new("a")));
    }

    #[test]
    fn from_config_normalises_key_case_and_whitespace() {
        let m = ExtensionMap::from_config(&cfg(&[(" TXT ", "Markdown")])).unwrap();
        assert_eq!(m.pipeline(Path::new("a.txt")), Some(Pipeline::Markdown));
        assert_eq!(m.pipeline(Path::new("a.TXT")), Some(Pipeline::Markdown));
    }

    #[test]
    fn from_config_rejects_unknown_pipeline() {
        let err = ExtensionMap::from_config(&cfg(&[("pdf", "pdf")])).unwrap_err();
        assert!(err.contains("not a known parse pipeline"), "{err}");
    }

    #[test]
    fn from_config_rejects_empty_and_dotted_keys() {
        let empty = ExtensionMap::from_config(&cfg(&[("", "html")])).unwrap_err();
        assert!(empty.contains("must not be empty"), "{empty}");
        let dotted = ExtensionMap::from_config(&cfg(&[(".txt", "markdown")])).unwrap_err();
        assert!(dotted.contains("no dot"), "{dotted}");
        let compound = ExtensionMap::from_config(&cfg(&[("tar.gz", "html")])).unwrap_err();
        assert!(compound.contains("no dot"), "{compound}");
    }

    #[test]
    fn pipeline_parse_is_case_insensitive_and_rejects_unknown() {
        assert_eq!(Pipeline::parse("HTML"), Some(Pipeline::Html));
        assert_eq!(Pipeline::parse("  markdown "), Some(Pipeline::Markdown));
        assert_eq!(Pipeline::parse("pdf"), None);
        assert_eq!(Pipeline::Html.as_str(), "html");
        assert_eq!(Pipeline::Markdown.as_str(), "markdown");
    }
}
