//! Anchor corkboard — per-kb persistent set of "anchored" (bookmarked)
//! artifacts. v0.10 K1.
//!
//! External naming: the SPA + HTTP routes call this "anchors"
//! (`/api/anchors`, `anchor.added` / `anchor.removed` SSE, the Header
//! pill icon ⚓). Internally we use `corkboard` to avoid colliding with
//! `kb_core::anchors`, which tracks stale comment anchors on disk.
//!
//! Storage lives in the per-kb sqlite database (`corkboard` table,
//! migration V0005). The actual CRUD lives in
//! [`crate::storage::sqlite::Db`]; this module exposes the shared
//! types so HTTP handlers + CLI verbs depend on
//! `kb_core::corkboard::Entry` rather than the lower-level
//! `CorkboardRow`.

use serde::{Deserialize, Serialize};

/// One entry on the corkboard, projected for cross-kb HTTP responses.
///
/// The on-disk row stores only `(artifact_id, created_at)`; the HTTP
/// layer joins with the lance projection to fill `title` / `folder` /
/// `source_relative` (mirroring the cross-kb stale-anchors response).
/// `kb` is set by the route's fan-out — each kb's storage actor only
/// knows about its own corkboard.
#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "CorkboardEntry")
)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub kb: String,
    pub artifact_id: String,
    /// Unix epoch seconds when the user pinned this artifact.
    pub created_at: i64,
    /// Title from the lance projection; `None` if the artifact left
    /// lance after the pin was created (a kind of "tombstone" view —
    /// the SPA renders these greyed so the user can clean them up).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub title: Option<String>,
    /// Source-relative path for the permalink. `None` for the same
    /// "artifact gone" case.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub source_relative: Option<String>,
    /// Parent folder (relative to the kb's source root). Empty string
    /// for root-level artifacts; `None` when the artifact is gone.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub folder: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_serializes_omitting_missing_projection() {
        // The "artifact gone" case (no title/path/folder) must serialize
        // without `null` placeholders — the SPA treats absence as the
        // tombstone marker. `skip_serializing_if = "Option::is_none"`
        // delivers that contract.
        let e = Entry {
            kb: "research".into(),
            artifact_id: "abcdef012345".into(),
            created_at: 1_700_000_000,
            title: None,
            source_relative: None,
            folder: None,
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(!json.contains("title"));
        assert!(!json.contains("source_relative"));
        assert!(!json.contains("folder"));
        assert!(json.contains("\"kb\":\"research\""));
        assert!(json.contains("\"artifact_id\":\"abcdef012345\""));
    }

    #[test]
    fn entry_serializes_with_full_projection() {
        let e = Entry {
            kb: "research".into(),
            artifact_id: "abc".into(),
            created_at: 1_700_000_000,
            title: Some("kb · research index".into()),
            source_relative: Some("kb-research/index.html".into()),
            folder: Some("kb-research".into()),
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"title\":\"kb · research index\""));
        assert!(json.contains("\"source_relative\":\"kb-research/index.html\""));
        assert!(json.contains("\"folder\":\"kb-research\""));
    }
}
