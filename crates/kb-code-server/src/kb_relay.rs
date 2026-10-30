//! OL S1 — two READ-ONLY relays so the SPA can show kb's side of the fleet
//! without ever talking to kb from the browser:
//!
//! * `GET /api/kb/search?q=&limit=` — kb's `GET /api/search` (scope=all,
//!   keyword mode) reshaped to `{ hits: [{kb, id, title, url, snippet}],
//!   kb_public_url }` for the omnibox "Documents" lane.
//! * `GET /api/kb/ui` — kb's `ui.theme_family` / `ui.theme`, so a browser
//!   that has not chosen a family in kb-code can follow kb's.
//! * `GET /api/kb/blocked` (OL IDEA1) — the sessions kb reports as blocked on
//!   the operator, reduced to `{session_id, harness, project?, blocked_tool?,
//!   since_unix}` for the Home "Needs you" line. No cwd, title, last line,
//!   reason text, model or host crosses this wire.
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
use crate::join::kb_client::{
    KbBlockedSession, KbClientError, KbSearchRelay, KbUiRelay, SEARCH_RELAY_MAX_LIMIT,
};
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
/// A relayed query is cut to this many chars (on a char boundary).
pub const MAX_QUERY_CHARS: usize = 256;
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

/// Caption for a search that outlived `SEARCH_TIMEOUT` (kb reached, but slow).
pub const KB_SEARCH_TIMED_OUT: &str = "kb search timed out";

/// The closed `unavailable` vocabulary. `kb_sibling_mismatch` is the one
/// machine-readable token (the brief names it); the other two are prose.
fn unavailable_caption(err: &KbClientError) -> &'static str {
    match err {
        KbClientError::SiblingMismatch(_) => "kb_sibling_mismatch",
        KbClientError::Disabled => "kb disabled",
        KbClientError::Timeout(_) => KB_SEARCH_TIMED_OUT,
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

/// Truncate `q` to [`MAX_QUERY_CHARS`] chars, never splitting a char.
pub fn cap_query(q: &str) -> &str {
    match q.char_indices().nth(MAX_QUERY_CHARS) {
        Some((i, _)) => &q[..i],
        None => q,
    }
}

/// A permalink segment kb-code will put on the wire: not `.`/`..`. A browser
/// normalises such a segment out of `/a/{kb}/...`, escaping the artifact route.
fn segment_ok(seg: &str) -> bool {
    seg != "." && seg != ".."
}

fn hit_path_ok(kb: &str, source_relative: &str) -> bool {
    segment_ok(kb) && source_relative.split('/').all(segment_ok)
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

// One blocked-session row. `since_unix` is when the block began (kb's
// `blocked_since_unix`, else the row's last activity).
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
pub struct KbBlockedRowOut {
    pub session_id: String,
    pub harness: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub blocked_tool: Option<String>,
    #[cfg_attr(feature = "ts-export", ts(type = "number"))]
    pub since_unix: i64,
}

// `GET /api/kb/blocked`. `total` counts every blocked session kb reported;
// `sessions` is capped at BLOCKED_MAX_ROWS and `truncated` says so.
// `kb_public_url` is empty unless a browser-facing public url is configured.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
pub struct KbBlockedOut {
    pub sessions: Vec<KbBlockedRowOut>,
    pub kb_public_url: String,
    #[cfg_attr(feature = "ts-export", ts(type = "number"))]
    pub total: usize,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub unavailable: Option<String>,
}

/// Rows `/api/kb/blocked` returns at most.
pub const BLOCKED_MAX_ROWS: usize = 20;

/// A tool name kb-code will pass on: 1..=128 chars of `[A-Za-z0-9_.:-]`.
fn valid_tool_name(t: &str) -> bool {
    (1..=128).contains(&t.len())
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'))
}

/// Caption for a blocked read that failed. Timeouts are "kb unreachable"
/// here: the search-only "kb search timed out" caption would be wrong.
fn blocked_unavailable_caption(err: &KbClientError) -> &'static str {
    match err {
        KbClientError::SiblingMismatch(_) => "kb_sibling_mismatch",
        KbClientError::Disabled => "kb disabled",
        _ => "kb unreachable",
    }
}

/// Compose the `/api/kb/blocked` body. Pure. A bad `blocked_tool` is dropped
/// (the row stays); rows sort oldest-block first, then by session id.
pub fn blocked_body(
    result: Result<Vec<KbBlockedSession>, KbClientError>,
    public_url: &str,
) -> KbBlockedOut {
    match result {
        Ok(rows) => {
            let mut out: Vec<KbBlockedRowOut> = rows
                .into_iter()
                .map(|r| KbBlockedRowOut {
                    since_unix: r.blocked_since_unix.unwrap_or(r.since_unix),
                    blocked_tool: r.blocked_tool.filter(|t| valid_tool_name(t)),
                    project: r.project.filter(|p| !p.is_empty()),
                    session_id: r.session_id,
                    harness: r.harness,
                })
                .collect();
            out.sort_by(|a, b| {
                a.since_unix
                    .cmp(&b.since_unix)
                    .then_with(|| a.session_id.cmp(&b.session_id))
            });
            let total = out.len();
            out.truncate(BLOCKED_MAX_ROWS);
            KbBlockedOut {
                sessions: out,
                kb_public_url: public_url.to_string(),
                total,
                truncated: total > BLOCKED_MAX_ROWS,
                unavailable: None,
            }
        }
        Err(e) => KbBlockedOut {
            sessions: Vec::new(),
            kb_public_url: public_url.to_string(),
            total: 0,
            truncated: false,
            unavailable: Some(blocked_unavailable_caption(&e).to_string()),
        },
    }
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
                    if !hit_path_ok(kb, &h.source_relative) {
                        return None;
                    }
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

/// Caption when kb is enabled but no browser-facing `public_url` is set.
pub const PUBLIC_URL_UNSET: &str = "kb public url not configured";

/// Is `[kb_daemon] public_url` set to something non-blank?
pub fn has_public_url(cfg: &crate::config::KbDaemonSection) -> bool {
    cfg.public_url
        .as_deref()
        .is_some_and(|u| !u.trim().is_empty())
}

/// `GET /api/kb/search`.
pub async fn kb_search_route(
    State(state): State<SharedState>,
    Query(params): Query<SearchParams>,
) -> impl IntoResponse {
    let cfg = &state.kb_daemon;
    let body = if cfg.enabled && !has_public_url(cfg) {
        // `public_base()` would fall back to the internal federation url (a
        // container hostname); never put that in a browser-bound permalink.
        KbSearchOut {
            hits: Vec::new(),
            kb_public_url: String::new(),
            unavailable: Some(PUBLIC_URL_UNSET.to_string()),
        }
    } else {
        let public_base = cfg.public_base().to_string();
        let q = cap_query(params.q.trim());
        if q.chars().count() < MIN_QUERY_CHARS {
            KbSearchOut {
                hits: Vec::new(),
                kb_public_url: public_base,
                unavailable: None,
            }
        } else {
            let limit = parse_limit(params.limit.as_deref());
            search_body(state.kb_client.search(q, limit).await, &public_base)
        }
    };
    ([(header::CACHE_CONTROL, "no-store")], Json(body))
}

/// `GET /api/kb/blocked`.
pub async fn kb_blocked_route(State(state): State<SharedState>) -> impl IntoResponse {
    let cfg = &state.kb_daemon;
    // Never put the internal federation url in a browser-bound link.
    let public_url = if has_public_url(cfg) {
        cfg.public_base().to_string()
    } else {
        String::new()
    };
    let body = blocked_body(state.kb_client.blocked_sessions().await, &public_url);
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
    fn search_body_drops_dot_segment_hits() {
        let body = search_body(
            Ok(KbSearchRelay {
                hits: vec![
                    hit(Some("k"), "../../x.html"),
                    hit(Some("k"), "a/./b.html"),
                    hit(Some(".."), "x.html"),
                    hit(Some("k"), "a/..b/c.html"),
                ],
            }),
            "https://kb.example.com",
        );
        assert_eq!(body.hits.len(), 1, "only the harmless `..b` name survives");
        assert_eq!(body.hits[0].url, "https://kb.example.com/a/k/a/..b/c.html");
    }

    #[test]
    fn query_is_capped_on_a_char_boundary() {
        assert_eq!(cap_query("short"), "short");
        let long = "é".repeat(MAX_QUERY_CHARS + 50);
        let capped = cap_query(&long);
        assert_eq!(capped.chars().count(), MAX_QUERY_CHARS);
        assert!(long.starts_with(capped));
        let exact = "a".repeat(MAX_QUERY_CHARS);
        assert_eq!(cap_query(&exact), exact);
    }

    #[test]
    fn public_url_must_be_set_and_non_blank() {
        use crate::config::KbDaemonSection;
        let mut c = KbDaemonSection {
            enabled: true,
            url: Some("http://kb:4000".into()),
            token_file: None,
            public_url: None,
        };
        assert!(!has_public_url(&c));
        c.public_url = Some("  ".into());
        assert!(!has_public_url(&c));
        c.public_url = Some("https://kb.example.com".into());
        assert!(has_public_url(&c));
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
        let timed_out = search_body(Err(KbClientError::Timeout("u".into())), "");
        assert!(timed_out.hits.is_empty());
        assert_eq!(
            timed_out.unavailable.as_deref(),
            Some("kb search timed out")
        );
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

    fn blocked(
        id: &str,
        since: i64,
        blocked_since: Option<i64>,
        tool: Option<&str>,
    ) -> KbBlockedSession {
        KbBlockedSession {
            session_id: id.to_string(),
            harness: "claude".to_string(),
            project: Some("kb".to_string()),
            state: "working".to_string(),
            blocked: true,
            blocked_tool: tool.map(str::to_string),
            blocked_since_unix: blocked_since,
            since_unix: since,
        }
    }

    #[test]
    fn blocked_body_sorts_oldest_first_and_prefers_blocked_since() {
        let body = blocked_body(
            Ok(vec![
                blocked("b", 50, None, None),
                blocked("c", 500, Some(10), Some("Bash")),
                blocked("a", 50, None, None),
            ]),
            "https://kb.example.com",
        );
        let ids: Vec<&str> = body
            .sessions
            .iter()
            .map(|r| r.session_id.as_str())
            .collect();
        assert_eq!(ids, ["c", "a", "b"]);
        assert_eq!(body.sessions[0].since_unix, 10);
        // an older-kb row (no blocked_since_unix) maps with since = since_unix
        assert_eq!(body.sessions[1].since_unix, 50);
        assert_eq!(body.total, 3);
        assert!(!body.truncated);
        assert_eq!(body.kb_public_url, "https://kb.example.com");
        assert!(wire(&body).get("unavailable").is_none());
    }

    #[test]
    fn blocked_body_caps_at_twenty_and_reports_the_total() {
        let rows: Vec<_> = (0..25)
            .map(|i| blocked(&format!("s{i:02}"), i, None, None))
            .collect();
        let body = blocked_body(Ok(rows), "");
        assert_eq!(body.sessions.len(), BLOCKED_MAX_ROWS);
        assert_eq!(body.total, 25);
        assert!(body.truncated);
    }

    #[test]
    fn blocked_body_drops_a_bad_tool_but_keeps_the_row() {
        let body = blocked_body(
            Ok(vec![
                blocked("a", 1, None, Some("Bash; rm")),
                blocked("b", 2, None, Some("mcp__x__y")),
                blocked("c", 3, None, Some("")),
            ]),
            "",
        );
        assert_eq!(body.sessions.len(), 3);
        assert_eq!(body.sessions[0].blocked_tool, None);
        assert_eq!(body.sessions[1].blocked_tool.as_deref(), Some("mcp__x__y"));
        assert_eq!(body.sessions[2].blocked_tool, None);
    }

    #[test]
    fn degraded_blocked_is_an_empty_200_shape_with_a_caption() {
        let d = blocked_body(Err(KbClientError::Disabled), "");
        assert!(d.sessions.is_empty());
        assert_eq!(d.unavailable.as_deref(), Some("kb disabled"));
        assert_eq!(
            blocked_body(Err(KbClientError::SiblingMismatch("x".into())), "")
                .unavailable
                .as_deref(),
            Some("kb_sibling_mismatch")
        );
        for e in [
            KbClientError::Timeout("u".into()),
            KbClientError::Unreachable("u".into(), "boom".into()),
            KbClientError::Parse("x".into()),
            KbClientError::BadStatus(reqwest::StatusCode::NOT_FOUND),
        ] {
            let b = blocked_body(Err(e), "");
            assert_eq!(b.unavailable.as_deref(), Some("kb unreachable"));
            assert_eq!((b.total, b.truncated), (0, false));
        }
    }

    #[test]
    fn blocked_wire_carries_exactly_the_declared_keys() {
        let body = blocked_body(
            Ok(vec![blocked("a", 1, Some(2), Some("Bash"))]),
            "https://kb.example.com",
        );
        let v = wire(&body);
        let mut top: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        top.sort_unstable();
        assert_eq!(top, ["kb_public_url", "sessions", "total", "truncated"]);
        let mut row: Vec<&str> = v["sessions"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        row.sort_unstable();
        assert_eq!(
            row,
            [
                "blocked_tool",
                "harness",
                "project",
                "session_id",
                "since_unix"
            ]
        );
    }
}
