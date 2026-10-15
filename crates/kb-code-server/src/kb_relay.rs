//! OL S1 — two READ-ONLY relays so the SPA can show kb's side of the fleet
//! without ever talking to kb from the browser:
//!
//! * `GET /api/kb/search?q=&limit=` — kb's `GET /api/search` (scope=all,
//!   keyword mode) reshaped to `{ hits: [{kb, id, title, url, snippet}],
//!   kb_public_url }` for the omnibox "Documents" lane.
//! * `GET /api/kb/ui` — kb's `ui.theme_family` / `ui.theme`, so a browser
//!   that has not chosen a family in kb-code can follow kb's.
//!
//! Invariant #2: both go through [`crate::join::kb_client::KbClient`] — the
//! one live call direction is kb-code -> kb, kb-sibling/1 handshake included,
//! so a mismatched peer fails CLOSED. Both are ordinary `auth_bearer` reads;
//! neither touches the doc-lens CORS route set (the browser reaches them
//! same-origin only). Nothing is cached or persisted here.
//!
//! Degradation is part of the contract: an unreachable, disabled or
//! mismatched kb answers **200** with an empty payload and an `unavailable`
//! caption, never a 5xx, so the omnibox renders a muted line instead of an
//! error.

use crate::doclens::doc_href;
use crate::join::kb_client::{KbClientError, KbSearchRelay, KbUiRelay, SEARCH_RELAY_MAX_LIMIT};
use crate::state::SharedState;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

/// Queries shorter than this (in chars, after trimming) never reach kb.
pub const MIN_QUERY_CHARS: usize = 2;
/// Rows returned when the caller names no `limit`.
pub const DEFAULT_LIMIT: u32 = 8;
/// A relayed snippet is cut to this many chars.
const SNIPPET_CAP: usize = 300;

#[derive(Debug, Deserialize)]
pub struct SearchParams {
    #[serde(default)]
    pub q: String,
    /// Kept as text so a junk value degrades to the default instead of a 400.
    #[serde(default)]
    pub limit: Option<String>,
}

/// The closed `unavailable` vocabulary. `kb_sibling_mismatch` is the one
/// machine-readable token (the brief names it); the other two are prose.
fn unavailable_caption(err: &KbClientError) -> &'static str {
    match err {
        KbClientError::SiblingMismatch(_) => "kb_sibling_mismatch",
        KbClientError::Disabled => "kb disabled",
        KbClientError::Unreachable(_, _)
        | KbClientError::BadStatus(_)
        | KbClientError::Parse(_)
        | KbClientError::ClientBuild(_)
        | KbClientError::InvalidPath(_) => "kb unreachable",
    }
}

fn parse_limit(raw: Option<&str>) -> u32 {
    raw.and_then(|s| s.trim().parse::<u32>().ok())
        .map(|n| n.clamp(1, SEARCH_RELAY_MAX_LIMIT))
        .unwrap_or(DEFAULT_LIMIT)
}

fn cap_snippet(s: &str) -> String {
    if s.chars().count() <= SNIPPET_CAP {
        return s.to_string();
    }
    let mut out: String = s.chars().take(SNIPPET_CAP).collect();
    out.push('…');
    out
}

// Wire shapes (ts-export: `web-code/src/api/generated/`). Plain `//` comments
// on purpose - a `///` here would be copied into the generated TypeScript.

// One Documents-lane row. `url` is the artifact permalink on kb's PUBLIC url.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
pub struct KbDocHitOut {
    pub kb: String,
    pub id: String,
    pub title: String,
    pub url: String,
    pub snippet: Option<String>,
}

// `GET /api/kb/search`. `unavailable` is absent on a healthy answer.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
pub struct KbSearchOut {
    pub hits: Vec<KbDocHitOut>,
    pub kb_public_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub unavailable: Option<String>,
}

// `GET /api/kb/ui`. Both values are null when kb has none or is unavailable.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
pub struct KbUiOut {
    pub theme: Option<String>,
    pub theme_family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub unavailable: Option<String>,
}

/// Compose the `/api/kb/search` body from kb's answer. Pure. A hit with no
/// `kb` (single-kb shape) or no resolvable permalink is dropped rather than
/// rendered as a dead row.
pub fn search_body(result: Result<KbSearchRelay, KbClientError>, public_base: &str) -> KbSearchOut {
    match result {
        Ok(relay) => KbSearchOut {
            hits: relay
                .hits
                .iter()
                .filter_map(|h| {
                    let kb = h.kb.as_deref().filter(|k| !k.is_empty())?;
                    let url = doc_href(public_base, kb, &h.source_relative)?;
                    Some(KbDocHitOut {
                        kb: kb.to_string(),
                        id: h.id.clone(),
                        title: h.title.clone(),
                        url,
                        snippet: h.snippet.as_deref().map(cap_snippet),
                    })
                })
                .collect(),
            kb_public_url: public_base.to_string(),
            unavailable: None,
        },
        Err(e) => KbSearchOut {
            hits: Vec::new(),
            kb_public_url: public_base.to_string(),
            unavailable: Some(unavailable_caption(&e).to_string()),
        },
    }
}

/// A family id kb-code will pass on: `[a-z0-9-]{1,40}`. The SPA still
/// coerces it against its own registry; this keeps arbitrary kb-supplied
/// text off the wire.
pub fn valid_family_slug(s: &str) -> bool {
    (1..=40).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Compose the `/api/kb/ui` body. Pure. Unknown / malformed values become
/// `null` (ignored by the SPA), never an error.
pub fn ui_body(result: Result<KbUiRelay, KbClientError>) -> KbUiOut {
    match result {
        Ok(ui) => KbUiOut {
            theme: ui
                .theme
                .filter(|t| matches!(t.as_str(), "light" | "dark" | "system")),
            theme_family: ui.theme_family.filter(|f| valid_family_slug(f)),
            unavailable: None,
        },
        Err(e) => KbUiOut {
            theme: None,
            theme_family: None,
            unavailable: Some(unavailable_caption(&e).to_string()),
        },
    }
}

/// `GET /api/kb/search`.
pub async fn kb_search_route(
    State(state): State<SharedState>,
    Query(params): Query<SearchParams>,
) -> impl IntoResponse {
    let public_base = state.kb_daemon.public_base().to_string();
    let q = params.q.trim();
    let body = if q.chars().count() < MIN_QUERY_CHARS {
        KbSearchOut {
            hits: Vec::new(),
            kb_public_url: public_base,
            unavailable: None,
        }
    } else {
        let limit = parse_limit(params.limit.as_deref());
        search_body(state.kb_client.search(q, limit).await, &public_base)
    };
    ([(header::CACHE_CONTROL, "no-store")], Json(body))
}

/// `GET /api/kb/ui`.
pub async fn kb_ui_route(State(state): State<SharedState>) -> impl IntoResponse {
    let body = ui_body(state.kb_client.ui_settings().await);
    ([(header::CACHE_CONTROL, "no-store")], Json(body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::join::kb_client::KbSearchHit;

    fn hit(kb: Option<&str>, rel: &str) -> KbSearchHit {
        KbSearchHit {
            kb: kb.map(str::to_string),
            id: "id1".to_string(),
            title: "T".to_string(),
            source_relative: rel.to_string(),
            snippet: Some("s".to_string()),
        }
    }

    fn wire<T: Serialize>(v: &T) -> serde_json::Value {
        serde_json::to_value(v).unwrap()
    }

    #[test]
    fn search_body_builds_permalinks_on_the_public_base() {
        let body = search_body(
            Ok(KbSearchRelay {
                hits: vec![hit(Some("platform"), "docs/a b.html")],
            }),
            "https://kb.example.com/",
        );
        assert_eq!(body.kb_public_url, "https://kb.example.com/");
        assert_eq!(
            body.hits[0].url,
            "https://kb.example.com/a/platform/docs/a%20b.html"
        );
        assert_eq!(body.hits[0].kb, "platform");
        // Healthy answers carry no `unavailable` key at all on the wire.
        assert!(wire(&body).get("unavailable").is_none());
    }

    #[test]
    fn search_body_drops_hits_it_cannot_link() {
        let body = search_body(
            Ok(KbSearchRelay {
                hits: vec![
                    hit(None, "a.html"),
                    hit(Some("k"), ""),
                    hit(Some("k"), "ok.html"),
                ],
            }),
            "https://kb.example.com",
        );
        assert_eq!(body.hits.len(), 1);
        assert_eq!(body.hits[0].url, "https://kb.example.com/a/k/ok.html");
    }

    #[test]
    fn search_body_caps_a_long_snippet() {
        let mut h = hit(Some("k"), "a.html");
        h.snippet = Some("x".repeat(1000));
        let body = search_body(
            Ok(KbSearchRelay { hits: vec![h] }),
            "https://kb.example.com",
        );
        let snip = body.hits[0].snippet.as_deref().unwrap();
        assert_eq!(snip.chars().count(), SNIPPET_CAP + 1);
        assert!(snip.ends_with('…'));
    }

    #[test]
    fn degraded_search_is_an_empty_200_shape_with_a_caption() {
        let unreachable = search_body(
            Err(KbClientError::Unreachable("u".into(), "boom".into())),
            "https://kb.example.com",
        );
        assert!(unreachable.hits.is_empty());
        assert_eq!(unreachable.unavailable.as_deref(), Some("kb unreachable"));
        assert_eq!(wire(&unreachable)["hits"], serde_json::json!([]));

        let mismatch = search_body(
            Err(KbClientError::SiblingMismatch("x".into())),
            "https://kb.example.com",
        );
        assert!(mismatch.hits.is_empty());
        assert_eq!(mismatch.unavailable.as_deref(), Some("kb_sibling_mismatch"));

        // A non-2xx or garbage body from kb is "unreachable" to the caller,
        // never a 5xx.
        let bad = search_body(
            Err(KbClientError::BadStatus(reqwest::StatusCode::NOT_FOUND)),
            "",
        );
        assert_eq!(bad.unavailable.as_deref(), Some("kb unreachable"));
        let parse = search_body(Err(KbClientError::Parse("x".into())), "");
        assert_eq!(parse.unavailable.as_deref(), Some("kb unreachable"));
        assert_eq!(
            search_body(Err(KbClientError::Disabled), "")
                .unavailable
                .as_deref(),
            Some("kb disabled")
        );
    }

    #[test]
    fn limit_parses_clamps_and_degrades() {
        assert_eq!(parse_limit(None), DEFAULT_LIMIT);
        assert_eq!(parse_limit(Some("junk")), DEFAULT_LIMIT);
        assert_eq!(parse_limit(Some("-3")), DEFAULT_LIMIT);
        assert_eq!(parse_limit(Some("0")), 1);
        assert_eq!(parse_limit(Some("5")), 5);
        assert_eq!(parse_limit(Some("500")), SEARCH_RELAY_MAX_LIMIT);
    }

    #[test]
    fn family_slug_is_the_closed_charset() {
        for ok in ["kb-classic", "catppuccin", "a", "x1-2"] {
            assert!(valid_family_slug(ok), "{ok}");
        }
        let too_long = "a".repeat(41);
        for bad in [
            "",
            "Kb",
            "a b",
            "a_b",
            "../x",
            "<script>",
            too_long.as_str(),
        ] {
            assert!(!valid_family_slug(bad), "{bad}");
        }
    }

    #[test]
    fn ui_body_passes_valid_values_and_nulls_the_rest() {
        let ok = ui_body(Ok(KbUiRelay {
            theme: Some("dark".into()),
            theme_family: Some("catppuccin".into()),
        }));
        assert_eq!(ok.theme.as_deref(), Some("dark"));
        assert_eq!(ok.theme_family.as_deref(), Some("catppuccin"));
        assert!(wire(&ok).get("unavailable").is_none());

        let junk = ui_body(Ok(KbUiRelay {
            theme: Some("neon".into()),
            theme_family: Some("<b>x</b>".into()),
        }));
        assert!(junk.theme.is_none());
        assert!(junk.theme_family.is_none());
        // null on the wire, not an absent key.
        assert!(wire(&junk)["theme_family"].is_null());

        assert!(ui_body(Ok(KbUiRelay::default())).theme_family.is_none());
    }

    #[test]
    fn degraded_ui_carries_the_same_captions() {
        let d = ui_body(Err(KbClientError::Disabled));
        assert!(d.theme_family.is_none());
        assert_eq!(d.unavailable.as_deref(), Some("kb disabled"));
        assert_eq!(
            ui_body(Err(KbClientError::SiblingMismatch("x".into())))
                .unavailable
                .as_deref(),
            Some("kb_sibling_mismatch")
        );
    }
}
