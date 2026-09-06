//! Relative-asset extraction for single-artifact shares.
//!
//! `parser::extract_links` finds cross-*artifact* links (kb permalinks /
//! subdomain URLs) but NOT subresources — an artifact's `<link
//! rel="stylesheet" href="assets/style.css">`, `<script src>`, `<img
//! src>`. A folder share picks those up via the filesystem walk, but a
//! single-artifact share must collect them explicitly or the published
//! page renders unstyled. This module returns the **relative** asset refs
//! (the engine resolves them against the file's directory and copies the
//! ones that exist).
//!
//! Only document-relative refs are returned. Absolute (`/…`, `https://…`,
//! `//…`), in-page (`#…`), and non-fetch (`data:`, `mailto:`, …) refs are
//! skipped; the query string / fragment is trimmed.

use scraper::{Html, Selector};
use std::collections::HashSet;

/// Element/attribute pairs that reference a render-critical subresource.
const ASSET_REFS: &[(&str, &str)] = &[
    ("link[href]", "href"),
    ("script[src]", "src"),
    ("img[src]", "src"),
    ("source[src]", "src"),
];

/// Relative subresource refs in `html`, de-duplicated, in document order.
pub fn relative_assets(html: &str) -> Vec<String> {
    let doc = Html::parse_document(html);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (sel, attr) in ASSET_REFS {
        // The selectors are static + valid; a parse failure is a bug, not
        // input-dependent.
        let Ok(selector) = Selector::parse(sel) else {
            continue;
        };
        for el in doc.select(&selector) {
            if let Some(raw) = el.value().attr(attr) {
                if let Some(rel) = as_relative_asset(raw) {
                    if seen.insert(rel.clone()) {
                        out.push(rel);
                    }
                }
            }
        }
    }
    out
}

/// Return the path portion of `raw` if it is a document-relative asset
/// ref, else `None`. Trims `?query` and `#fragment`.
fn as_relative_asset(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    // Skip absolute / scheme / protocol-relative / in-page / non-fetch refs.
    const SKIP_PREFIXES: &[&str] = &[
        "/",
        "#",
        "http://",
        "https://",
        "//",
        "data:",
        "mailto:",
        "tel:",
        "javascript:",
    ];
    let lower = raw.to_ascii_lowercase();
    if SKIP_PREFIXES.iter().any(|p| lower.starts_with(p)) {
        return None;
    }
    let path = raw
        .split_once(['?', '#'])
        .map(|(head, _)| head)
        .unwrap_or(raw);
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_relative_link_script_img() {
        let html = r##"<!doctype html><html><head>
            <link rel="stylesheet" href="assets/style.css">
            <script src="./app.js"></script>
            </head><body>
            <img src="img/logo.png?v=2">
            <source src="../shared/clip.webm">
            </body></html>"##;
        let assets = relative_assets(html);
        assert!(assets.contains(&"assets/style.css".to_string()));
        assert!(assets.contains(&"./app.js".to_string()));
        assert!(
            assets.contains(&"img/logo.png".to_string()),
            "query trimmed"
        );
        assert!(assets.contains(&"../shared/clip.webm".to_string()));
    }

    #[test]
    fn skips_absolute_and_non_fetch_refs() {
        let html = r##"<link href="https://cdn.example/x.css">
            <link href="/root.css">
            <link href="//proto.example/y.css">
            <script src="data:text/js,1"></script>
            <a href="other.html">rel anchor not an asset</a>
            <img src="#frag">"##;
        let assets = relative_assets(html);
        assert!(assets.is_empty(), "got: {assets:?}");
    }

    #[test]
    fn dedups_repeated_refs() {
        let html = r##"<img src="a.png"><img src="a.png">"##;
        assert_eq!(relative_assets(html), vec!["a.png".to_string()]);
    }
}
