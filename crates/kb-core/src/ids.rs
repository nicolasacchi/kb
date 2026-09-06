//! Identifier types — `ArtifactId` (sha256-prefix-12 of the
//! source-relative path), `SourceSlug`, `RunId`, `ErrorId`.
//! Topic 11 §C identifier conventions.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Path-addressed artifact id: hex-encoded SHA-256 prefix (12 chars / 6
/// bytes) of the artifact's **source-root-relative path**.
///
/// Identity follows the file, not its bytes: editing an artifact keeps
/// its id (so detail URLs stay valid and comments stay anchored);
/// renaming/moving the file changes it. The 12-hex-char shape is
/// deliberately unchanged from the old content-hash scheme so it stays
/// usable as a DNS subdomain label, a route segment, and a review-file
/// filename with zero downstream changes.
///
/// `from_html_bytes` still exists, but only as a *content hash* for
/// change/retry detection (see `indexer`) — it is no longer the id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ArtifactId(String);

impl ArtifactId {
    /// The artifact id: SHA-256 prefix of the source-relative path.
    /// `rel_path` should be forward-slash separated (see
    /// `paths::doc_rel_path`) so the id is stable across platforms.
    pub fn from_path(rel_path: &str) -> Self {
        Self::hash12(rel_path.as_bytes())
    }

    /// Content hash of the raw HTML bytes. NOT the id — used for
    /// change/retry detection only.
    pub fn from_html_bytes(bytes: &[u8]) -> Self {
        Self::hash12(bytes)
    }

    fn hash12(bytes: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let digest = hasher.finalize();
        Self(hex::encode(&digest[..6]))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ArtifactId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Source folder slug — path-derived identifier safe for URLs and SQLite keys.
/// Lowercases, replaces non-alphanumeric runs with `-`, trims leading/trailing
/// dashes. Empty input becomes `"src"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceSlug(String);

impl SourceSlug {
    pub fn from_path(path: &Path) -> Self {
        let raw = path.to_string_lossy().to_lowercase();
        let mut out = String::with_capacity(raw.len());
        let mut last_dash = false;
        for ch in raw.chars() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                out.push(ch);
                last_dash = false;
            } else if !last_dash && !out.is_empty() {
                out.push('-');
                last_dash = true;
            }
        }
        let trimmed = out.trim_matches('-').to_string();
        Self(if trimmed.is_empty() {
            "src".to_string()
        } else {
            trimmed
        })
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SourceSlug {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Run identifier — `r-<12-base32-Crockford>` (12 chars = 60 bits).
/// Mixes monotonic counter + timestamp nanos so cross-restart
/// collisions are unlikely (~10⁻¹⁵ over the lifetime of any
/// realistic deployment; was 30 bits / ~10⁻⁹ pre-LOW-fix).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(String);

impl RunId {
    pub fn new() -> Self {
        Self(format!("r-{}", encode_base32_6(fresh_seed())))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for RunId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for RunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Error identifier — `e-<12-base32-Crockford>` (60-bit namespace).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ErrorId(String);

impl ErrorId {
    pub fn new() -> Self {
        Self(format!("e-{}", encode_base32_6(fresh_seed())))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ErrorId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ErrorId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// --- helpers -----------------------------------------------------------------

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Mix monotonic counter with timestamp nanos. Avoids `rand` dep without
/// sacrificing cross-restart uniqueness.
fn fresh_seed() -> u64 {
    let counter = COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    nanos.wrapping_add(counter.wrapping_mul(0x9E3779B97F4A7C15))
}

/// Encode the low 60 bits of `seed` as 12 Crockford-base32 chars.
///
/// LOW (deep-review): pre-fix this encoded only the low 30 bits — about
/// 10⁹ codepoints. A long-running daemon with ~10⁶ runs/errors had
/// roughly 10⁻³ collision probability per pair (≈ hundreds of expected
/// collisions over months). Each collision causes `finish_run` to
/// update a stale row, breaking ok/err counts in the TUI Runs tab.
/// Using 60 bits raises the namespace to ~10¹⁸ — collision probability
/// becomes negligible over the lifetime of any realistic deployment.
fn encode_base32_6(seed: u64) -> String {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut out = String::with_capacity(12);
    let mut s = seed;
    for _ in 0..12 {
        out.push(ALPHABET[(s & 0x1f) as usize] as char);
        s >>= 5;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_id_is_deterministic() {
        let a = ArtifactId::from_html_bytes(b"<html><body>hi</body></html>");
        let b = ArtifactId::from_html_bytes(b"<html><body>hi</body></html>");
        assert_eq!(a, b);
        assert_eq!(a.as_str().len(), 12);
    }

    #[test]
    fn artifact_id_differs_per_input() {
        let a = ArtifactId::from_html_bytes(b"x");
        let b = ArtifactId::from_html_bytes(b"y");
        assert_ne!(a, b);
    }

    #[test]
    fn artifact_id_is_hex_lowercase() {
        let id = ArtifactId::from_html_bytes(b"hello");
        assert!(id
            .as_str()
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn from_path_is_deterministic_and_12_hex() {
        let a = ArtifactId::from_path("ideas/passwordless-login/portal.html");
        let b = ArtifactId::from_path("ideas/passwordless-login/portal.html");
        assert_eq!(a, b);
        assert_eq!(a.as_str().len(), 12);
        assert!(a
            .as_str()
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn from_path_differs_per_path() {
        let a = ArtifactId::from_path("a/foo.html");
        let b = ArtifactId::from_path("a/bar.html");
        assert_ne!(a, b);
    }

    #[test]
    fn source_slug_lowercases_and_dashes() {
        let s = SourceSlug::from_path(Path::new("/Home/Me/My Folder"));
        assert_eq!(s.as_str(), "home-me-my-folder");
    }

    #[test]
    fn source_slug_collapses_runs() {
        let s = SourceSlug::from_path(Path::new("/a//b///c"));
        assert_eq!(s.as_str(), "a-b-c");
    }

    #[test]
    fn source_slug_empty_becomes_src() {
        let s = SourceSlug::from_path(Path::new(""));
        assert_eq!(s.as_str(), "src");
        let s2 = SourceSlug::from_path(Path::new("///"));
        assert_eq!(s2.as_str(), "src");
    }

    #[test]
    fn source_slug_preserves_underscores_and_alphanumerics() {
        let s = SourceSlug::from_path(Path::new("/var/lib/kb_corpus_2026"));
        assert_eq!(s.as_str(), "var-lib-kb_corpus_2026");
    }

    #[test]
    fn run_id_format() {
        let r = RunId::new();
        assert!(r.as_str().starts_with("r-"));
        assert_eq!(r.as_str().len(), 14); // r- + 12 chars (60-bit namespace)
    }

    #[test]
    fn error_id_format() {
        let e = ErrorId::new();
        assert!(e.as_str().starts_with("e-"));
        assert_eq!(e.as_str().len(), 14);
    }

    #[test]
    fn run_ids_are_unique_within_session() {
        let ids: Vec<_> = (0..1000).map(|_| RunId::new()).collect();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), 1000, "1000 RunIds should all be distinct");
    }

    proptest::proptest! {
        #[test]
        fn artifact_id_determinism(bytes: Vec<u8>) {
            let a = ArtifactId::from_html_bytes(&bytes);
            let b = ArtifactId::from_html_bytes(&bytes);
            assert_eq!(a, b);
            assert_eq!(a.as_str().len(), 12);
        }
    }
}
