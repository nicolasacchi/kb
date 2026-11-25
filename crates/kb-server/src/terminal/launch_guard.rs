//! kbt2 HUBGUARD — server guard rails for `POST /api/terms`.
//!
//! Two things live here, both pure so they are testable without a runner:
//!
//! * the **cap**: [`ERR_TERMINAL_CAP`], [`cap_refusal`] and [`count_live`].
//!   The `launch` handler counts the runner's live terminals and spawns
//!   under the ONE `TerminalState::launch_lock`, so two concurrent launches
//!   cannot both pass the count check.
//! * the **audit line**: [`AuditFields`] / [`emit_audit`], one `info!` per
//!   launch attempt that passed the gate. It carries the harness, the LAST
//!   path component of the cwd, the resume sid, the resolved identity and
//!   the outcome; never prompt or handoff text, the full cwd, a header or a
//!   token.

use axum::body::Body;
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::Value;

use crate::routes::terms::{no_store, plain_token, TermItem};
use crate::terminal::gate::Problem;

/// Problem type of a launch refused because the runner already reports
/// `[terminal] max_terms` live terminals (HTTP 429, like the ticket-full
/// capacity refusal; 409 stays "state conflict on one terminal"). The SPA
/// keys on this type, never the status or the detail text.
pub(crate) const ERR_TERMINAL_CAP: &str = "urn:kb:errors:terminal-cap";

/// The tracing target of the audit line.
pub(crate) const AUDIT_TARGET: &str = "kb_server::terminal::launch_audit";

/// Longest value (in chars) any audit field carries.
const AUDIT_FIELD_CAP: usize = 64;

/// The typed 429 for a launch over the cap. `live` is what the runner
/// reported (0 when the cap itself is 0).
pub(crate) fn cap_refusal(live: u64, cap: u64) -> Response<Body> {
    let detail = if cap == 0 {
        "terminal launching is disabled (max_terms = 0)".to_string()
    } else {
        format!("terminal limit reached: {live} of {cap} live terminals; end one first")
    };
    no_store(Problem::response(
        ERR_TERMINAL_CAP,
        StatusCode::TOO_MANY_REQUESTS,
        "Too Many Requests",
        detail,
    ))
}

/// Live terminals among a parsed runner `list`: every row whose
/// `pane_dead` is not `true` (a row without the field counts as live;
/// `remain-on-exit` keeps dead panes in the list until they are killed).
pub(crate) fn count_live(items: &[TermItem]) -> u64 {
    items
        .iter()
        .filter(|i| i.extra.get("pane_dead") != Some(&Value::Bool(true)))
        .count() as u64
}

/// How a launch attempt ended, as the audit line names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Accepted,
    RefusedCap,
    BadRequest,
    RunnerError,
}

impl Outcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Outcome::Accepted => "accepted",
            Outcome::RefusedCap => "refused-cap",
            Outcome::BadRequest => "bad-request",
            Outcome::RunnerError => "runner-error",
        }
    }
}

/// The only values an audit line may carry. Every field is already
/// reduced: `-` stands for absent/unusable, and values are char-capped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuditFields {
    pub harness: String,
    pub cwd_base: String,
    pub resume_sid: String,
    pub identity: String,
    pub outcome: &'static str,
}

fn capped(s: &str) -> String {
    s.chars().take(AUDIT_FIELD_CAP).collect()
}

/// Reduce raw request facts to the audit fields. `harness` is the PARSED
/// enum string (`None` when unparseable); the cwd keeps only its final
/// component; the resume sid appears only when it is a plain token.
pub(crate) fn audit_fields(
    harness: Option<&str>,
    cwd: Option<&str>,
    resume_sid: Option<&str>,
    identity: &str,
    outcome: Outcome,
) -> AuditFields {
    let dash = || "-".to_string();
    AuditFields {
        harness: harness.map(capped).unwrap_or_else(dash),
        cwd_base: cwd
            .and_then(|c| std::path::Path::new(c).file_name())
            .map(|n| capped(&n.to_string_lossy()))
            .unwrap_or_else(dash),
        resume_sid: resume_sid
            .filter(|s| plain_token(s))
            .map(capped)
            .unwrap_or_else(dash),
        identity: if identity.is_empty() {
            dash()
        } else {
            capped(identity)
        },
        outcome: outcome.as_str(),
    }
}

/// Emit the one audit line. Free-text fields go through Debug (`?`) so a
/// control character or newline cannot forge a second line.
pub(crate) fn emit_audit(f: &AuditFields) {
    tracing::info!(
        target: "kb_server::terminal::launch_audit",
        harness = %f.harness,
        cwd_base = ?f.cwd_base,
        resume_sid = ?f.resume_sid,
        identity = ?f.identity,
        outcome = %f.outcome,
        "terminal launch attempt"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(v: serde_json::Value) -> TermItem {
        serde_json::from_value(v).unwrap()
    }

    fn base(cwd: Option<&str>) -> AuditFields {
        audit_fields(Some("omp"), cwd, None, "op", Outcome::Accepted)
    }

    #[test]
    fn cwd_base_is_the_last_component_only() {
        assert_eq!(base(Some("/secret/parent/proj")).cwd_base, "proj");
        assert_eq!(base(Some("/secret/parent/proj/")).cwd_base, "proj");
        assert_eq!(base(Some("/")).cwd_base, "-");
        assert_eq!(base(None).cwd_base, "-");
        let long = format!("/a/{}", "x".repeat(500));
        assert_eq!(base(Some(&long)).cwd_base.chars().count(), 64);
        // A newline stays inside the field and is escaped by Debug at emit.
        let nl = base(Some("/a/b\nINFO forged"));
        assert!(nl.cwd_base.contains('\n'));
        assert!(format!("{:?}", nl.cwd_base).contains("\\n"));
    }

    #[test]
    fn resume_sid_only_when_plain_and_harness_dash_when_unparsed() {
        let f = audit_fields(None, None, Some("sid-1"), "op", Outcome::BadRequest);
        assert_eq!(f.harness, "-");
        assert_eq!(f.resume_sid, "sid-1");
        let bad = audit_fields(None, None, Some("a b\n"), "op", Outcome::BadRequest);
        assert_eq!(bad.resume_sid, "-");
        assert_eq!(bad.outcome, "bad-request");
    }

    #[test]
    fn count_live_skips_only_explicitly_dead_panes() {
        let rows = vec![
            item(serde_json::json!({"id":"a1","pane_pid":1})),
            item(serde_json::json!({"id":"b1","pane_pid":2,"pane_dead":false})),
            item(serde_json::json!({"id":"c1","pane_pid":3,"pane_dead":true})),
        ];
        assert_eq!(count_live(&rows), 2);
        assert_eq!(count_live(&[]), 0);
    }

    #[test]
    fn cap_refusal_is_a_typed_429() {
        let r = cap_refusal(8, 8);
        assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(r.headers()["cache-control"], "no-store");
    }
}
