//! W2.4 — the Search-Everywhere box's SESSIONS lane (`~query` in
//! [`super::grammar`]): federates over HTTP to the OPERATOR'S OWN `kb`
//! daemon's `GET /api/sessions/recollect` (`crates/kb-server/src/routes/
//! sessions.rs::recollect`, R3 — "has something like this been done?" over
//! kb's session digests). `kb-code-server` and `kb` are separate processes
//! (this workspace's two sibling daemons — see `lib.rs`'s module doc); this
//! is the ONLY lane in the box that leaves the process.
//!
//! Config: [`crate::config::KbDaemonSection`] (`[kb_daemon]` in
//! `kb-code.toml`) — V76-R4f: DISABLED unless the operator configures
//! `url` (see that struct's doc for the resolution table); `search`
//! returns [`SessionsSearchError::Disabled`] without ever touching the
//! network when it is off, same shape as an unreachable daemon but its own
//! distinct reason. A SHORT, fixed timeout ([`TIMEOUT`], 1.5s) — this lane
//! must never be the reason
//! the whole box misses its ~2s soft budget; an unreachable/slow kb daemon
//! degrades to [`SessionsSearchError::Unreachable`] (→ that section's
//! `unavailable_reason`), never a hung or failed box.
//!
//! Every hit is labelled `source: "kb digests"` — R1's own framing: what
//! comes back is a DETERMINISTIC digest excerpt kb already indexed, not a
//! live re-read of the transcript (that's the LOOPBACK-only transcripts
//! lane's job, a different corpus entirely — see `crate::transcripts`).

use crate::config::KbDaemonSection;
use serde::Serialize;
use std::time::Duration;

/// Fixed per the design brief — not configurable (the box's overall ~2s
/// soft budget is achieved BY CONSTRUCTION from each lane's own bound; see
/// `search::unified`'s module doc).
pub const TIMEOUT: Duration = Duration::from_millis(1_500);

/// One session hit, mapped from kb's `RecollectSessionOut`
/// (`crates/kb-server/src/routes/sessions.rs`) into the box's lane-shaped
/// output. `files_changed` is carried through ONLY if kb's response happens
/// to include it (it doesn't, as of R3/R9 — `RecollectSessionOut` has no
/// such field) — honest best-effort per the design brief ("if present"),
/// not a fabricated guarantee.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionHit {
    pub session_id: String,
    pub title: String,
    pub score: f32,
    pub started_at: i64,
    /// Always `"kb digests"` — see the module doc.
    pub source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_changed: Option<serde_json::Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionsSearchError {
    #[error("sessions lane is disabled ([kb_daemon] enabled = false)")]
    Disabled,
    #[error("q must not be empty")]
    EmptyQuery,
    #[error("build http client: {0}")]
    ClientBuild(String),
    #[error("kb daemon unreachable at {0}: {1}")]
    Unreachable(String, String),
    #[error("kb daemon returned {0}")]
    BadStatus(reqwest::StatusCode),
    #[error("parse kb daemon response: {0}")]
    Parse(String),
}

pub type Result<T> = std::result::Result<T, SessionsSearchError>;

/// `GET {cfg.url}/api/sessions/recollect?q=&limit=` — see the module doc.
/// `q` must already be non-empty (the caller, `unified::run_sessions`,
/// checks this before ever building a client — matches kb's own recollect
/// route, which 400s an empty `q`).
pub async fn search(cfg: &KbDaemonSection, q: &str, limit: usize) -> Result<Vec<SessionHit>> {
    if !cfg.enabled {
        return Err(SessionsSearchError::Disabled);
    }
    if q.trim().is_empty() {
        return Err(SessionsSearchError::EmptyQuery);
    }
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| SessionsSearchError::ClientBuild(e.to_string()))?;
    let url = format!(
        "{}/api/sessions/recollect",
        cfg.url_str().trim_end_matches('/')
    );
    // `[kb_daemon] token_file` — needed when kb's `auth_bearer` doesn't see
    // this daemon as loopback (docker-published prod); see
    // `KbDaemonSection::bearer_token`'s doc.
    let mut rb = client
        .get(&url)
        .query(&[("q", q), ("limit", &limit.to_string())]);
    if let Some(token) = cfg.bearer_token() {
        rb = rb.bearer_auth(token);
    }
    let resp = rb
        .send()
        .await
        .map_err(|e| SessionsSearchError::Unreachable(url.clone(), e.to_string()))?;
    if !resp.status().is_success() {
        return Err(SessionsSearchError::BadStatus(resp.status()));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| SessionsSearchError::Parse(e.to_string()))?;
    Ok(parse_response(&body))
}

/// Map kb's `{sessions: [RecollectSessionOut, ...], ms}` body into
/// [`SessionHit`]s — split out from [`search`] so a fixture JSON body can be
/// pinned in a unit test without a real HTTP round-trip.
fn parse_response(body: &serde_json::Value) -> Vec<SessionHit> {
    body.get("sessions")
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .map(|s| SessionHit {
                    session_id: s
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    title: s
                        .get("display_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    score: s.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                    started_at: s.get("started_at").and_then(|v| v.as_i64()).unwrap_or(0),
                    source: "kb digests",
                    files_changed: s.get("files_changed").cloned(),
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_response_maps_recollect_shape_and_labels_the_source() {
        let body = serde_json::json!({
            "sessions": [
                {
                    "session_id": "s1",
                    "kb": "kb",
                    "display_name": "fixed the gizmo race",
                    "started_at": 1_700_000_000,
                    "score": 0.87,
                    "age_days": 2,
                    "stale": false,
                    "error_count": 0,
                    "commit_count": 1,
                    "committed": true
                }
            ],
            "ms": 12
        });
        let hits = parse_response(&body);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "s1");
        assert_eq!(hits[0].title, "fixed the gizmo race");
        assert_eq!(hits[0].started_at, 1_700_000_000);
        assert!((hits[0].score - 0.87).abs() < 1e-6);
        assert_eq!(hits[0].source, "kb digests");
        assert_eq!(hits[0].files_changed, None);
    }

    #[test]
    fn parse_response_carries_files_changed_through_when_present() {
        let body = serde_json::json!({
            "sessions": [
                {
                    "session_id": "s1",
                    "display_name": "t",
                    "started_at": 1,
                    "score": 0.5,
                    "files_changed": ["a.rs", "b.rs"]
                }
            ]
        });
        let hits = parse_response(&body);
        assert_eq!(
            hits[0].files_changed,
            Some(serde_json::json!(["a.rs", "b.rs"]))
        );
    }

    #[test]
    fn parse_response_missing_sessions_array_is_empty_not_a_panic() {
        assert!(parse_response(&serde_json::json!({})).is_empty());
        assert!(parse_response(&serde_json::json!({"sessions": []})).is_empty());
    }

    #[tokio::test]
    async fn disabled_config_short_circuits_without_a_network_call() {
        let cfg = KbDaemonSection {
            enabled: false,
            url: Some("http://127.0.0.1:0".to_string()),
            token_file: None,
            public_url: None,
        };
        let err = search(&cfg, "widget", 8).await.unwrap_err();
        assert!(matches!(err, SessionsSearchError::Disabled));
    }

    #[tokio::test]
    async fn empty_query_is_rejected_without_a_network_call() {
        let cfg = KbDaemonSection {
            enabled: true,
            url: Some("http://127.0.0.1:0".to_string()),
            token_file: None,
            public_url: None,
        };
        let err = search(&cfg, "   ", 8).await.unwrap_err();
        assert!(matches!(err, SessionsSearchError::EmptyQuery));
    }

    #[tokio::test]
    async fn unreachable_daemon_surfaces_as_unreachable_not_a_panic() {
        // Port 0 never accepts a real connection — a fast, deterministic
        // "unreachable" without depending on any live daemon.
        let cfg = KbDaemonSection {
            enabled: true,
            url: Some("http://127.0.0.1:0".to_string()),
            token_file: None,
            public_url: None,
        };
        let err = search(&cfg, "widget", 8).await.unwrap_err();
        assert!(matches!(err, SessionsSearchError::Unreachable(_, _)));
    }
}
