//! `GET /api/sessions/{session_id}/handoff` — the cross-harness handoff BRIEF
//! (`kb-handoff/1`) for one session.
//!
//! Brief only: the daemon cannot see the host's harness files, so the route
//! renders from the session's NEWEST capture (invariant #11, the same lookup
//! `/export` does). The native-session transplant needs those host files and
//! is CLI-only (`kb sessions handoff --transplant`). The route never runs
//! anything: `command` is copy-only text for the operator's shell.
//!
//! **Fail-closed redaction (invariant #4):** a non-loopback client always
//! gets the `secrets` scrub floor, exactly the floor `/export` forces on the
//! transcript it streams. Order: (1) scrub the recovered transcript JSONL,
//! (2) parse + render the brief from the scrubbed transcript, (3) scrub the
//! rendered brief text and the `notes` again. Step 1 matters because the
//! render clips text and can cut a secret below the scrub rules' minimum
//! (or drop a PEM END marker); step 3 is a second pass. `scrubbed` is true
//! only when the floor applied.

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    extract::{ConnectInfo, Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    Json,
};
use kb_core::sessions::handoff::brief::{render_brief, BriefOptions};
use kb_core::sessions::handoff::claude::read_claude_jsonl;
use kb_core::sessions::handoff::{Harness, SourceKind, HANDOFF_SCHEMA};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;

/// Smallest/largest `?budget=` the route honours (characters). Below the
/// floor a brief cannot hold its own marker + first prompt; the ceiling keeps
/// one response well under any harness's first-prompt limit.
const BUDGET_MIN: usize = 1_000;
const BUDGET_MAX: usize = 32_000;
/// Largest `?tail=` (verbatim recent turns in the brief).
const TAIL_MAX: usize = 50;

/// Query for `GET /api/sessions/{id}/handoff`.
#[derive(Debug, Default, Deserialize)]
pub struct HandoffQuery {
    /// Target harness: `claude` | `omp`. Default: the other harness of the
    /// session's source.
    to: Option<String>,
    /// Brief size budget in characters, clamped to 1000..=32000
    /// (default [`BriefOptions::DEFAULT_BUDGET_CHARS`]).
    budget: Option<usize>,
    /// Verbatim tail turns, clamped to 0..=50 (default
    /// [`BriefOptions::DEFAULT_TAIL_TURNS`]).
    tail: Option<usize>,
}

/// Response of `GET /api/sessions/{id}/handoff`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct HandoffOut {
    /// `kb-handoff/1`.
    pub schema: String,
    pub session_id: String,
    /// The harness that produced the session (`claude` | `omp`).
    pub source_harness: String,
    /// Always `capture` — the daemon only reads kb captures.
    pub source_kind: String,
    /// The harness the brief is written for (`claude` | `omp`).
    pub target: String,
    /// The brief text: the first prompt of a NEW target-harness session.
    /// Starts with the `[kb-handoff from <harness> session <id>]` marker.
    pub brief: String,
    /// Characters in `brief` (after any scrub).
    pub chars: u32,
    /// Lanes of the brief cut or dropped to fit the budget.
    pub truncated: Vec<String>,
    /// Honesty notes from the reader (lossy source, skipped lines, …).
    pub notes: Vec<String>,
    /// Copy-only shell command that launches the handoff from the operator's
    /// machine; the server never runs it. A "not resumable" sentence when the
    /// session id is not a plain token.
    pub command: String,
    /// True when the secrets scrub floor was applied to `brief`.
    pub scrubbed: bool,
}

/// The copy-only launch command, or the shared "not resumable" sentence when
/// `session_id` is not a plain token. The plain-token predicate is kb-core's
/// (it is crate-private there); `resume_command` answers
/// [`NOT_RESUMABLE_UNSAFE`](kb_core::sessions::resume::NOT_RESUMABLE_UNSAFE)
/// for exactly the ids it rejects, so asking it keeps one definition.
fn launch_command(session_id: &str, target: Harness) -> String {
    let probe = kb_core::sessions::resume_command("omp", session_id, None);
    if probe == kb_core::sessions::resume::NOT_RESUMABLE_UNSAFE {
        return probe;
    }
    format!(
        "kb sessions handoff {session_id} --to {} --launch",
        target.as_str()
    )
}

/// `GET /api/sessions/{session_id}/handoff?to=claude|omp&budget=N&tail=N` —
/// render the handoff brief from the session's newest capture.
pub async fn handoff(
    State(state): State<Arc<KbHandles>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Query(q): Query<HandoffQuery>,
) -> Response {
    let target_arg = match q.to.as_deref() {
        None => None,
        Some(t) => match Harness::parse(t) {
            Some(h) => Some(h),
            None => {
                return error_to_problem_json(&kb_core::Error::BadRequest(
                    "`to` must be `claude` or `omp`".to_string(),
                ))
                .into_response()
            }
        },
    };

    // Locate the newest-capture row across all kbs (BTreeMap order,
    // first-wins) — the lookup `/export` does. `(abs_path, session_id,
    // source_relative)`.
    type Located = (std::path::PathBuf, String, String);
    let session_id_ref = &session_id;
    let mut futs: Vec<super::CorpusFut<'_, Option<Located>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        futs.push(Box::pin(async move {
            match ctx.storage.sessions_get(session_id_ref.clone()).await {
                Ok(Some(row)) => {
                    let abs = ctx.source_path.join(&row.source_relative);
                    Some((abs, row.session_id, row.source_relative))
                }
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!(kb = %kb_name, session_id = %session_id_ref, error = %e, "sessions_get failed in handoff");
                    None
                }
            }
        }));
    }
    let located = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .next();
    let Some((abs, row_sid, source_rel)) = located else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!("session {session_id}")))
            .into_response();
    };

    let budget_chars = q
        .budget
        .unwrap_or(BriefOptions::DEFAULT_BUDGET_CHARS)
        .clamp(BUDGET_MIN, BUDGET_MAX);
    let tail_turns = q
        .tail
        .unwrap_or(BriefOptions::DEFAULT_TAIL_TURNS)
        .clamp(0, TAIL_MAX);

    // Scrub floor (#4) decision, made before the render so the SOURCE
    // transcript is redacted while it is still whole: `render_brief` clips
    // text (head/tail around an omitted-chars line), which can cut a PEM
    // END marker or shorten a token below a scrub rule's minimum length, so
    // scrubbing only the rendered brief would let partial secrets through.
    let scrubbed = crate::scrub::looks_non_loopback(
        Some(peer.ip()),
        &headers,
        &state.origin.trusted_proxies.load(),
    );

    // Read the capture HTML (multi-MB), recover the JSONL, parse and render
    // — all off the tokio worker.
    let sid_for_err = session_id.clone();
    let rendered = tokio::task::spawn_blocking(move || {
        let html = std::fs::read_to_string(&abs).map_err(|_| {
            kb_core::Error::NotFound(format!("capture unreadable for session {sid_for_err}"))
        })?;
        let jsonl = kb_core::sessions::recover_jsonl_from_capture(&html).ok_or_else(|| {
            kb_core::Error::NotFound(format!(
                "no transcript in capture for session {sid_for_err}"
            ))
        })?;
        let jsonl = if scrubbed {
            kb_core::session_scrub::scrub_transcript(
                &jsonl,
                &kb_core::session_scrub::ScrubOptions::secrets_only(),
            )
            .0
        } else {
            jsonl
        };
        let conv = read_claude_jsonl(&jsonl, SourceKind::Capture, Some(source_rel.as_str()))
            .map_err(|e| kb_core::Error::NotFound(format!("session {sid_for_err}: {e}")))?;
        let target = target_arg.unwrap_or_else(|| conv.harness.other());
        let brief = render_brief(
            &conv,
            &BriefOptions {
                target,
                budget_chars,
                tail_turns,
            },
        );
        Ok::<_, kb_core::Error>((conv, target, brief))
    })
    .await;
    let (conv, target, brief) = match rendered {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return error_to_problem_json(&e).into_response(),
        Err(e) => {
            tracing::warn!(session_id = %session_id, error = %e, "handoff render task failed");
            return error_to_problem_json(&kb_core::Error::Internal(anyhow::anyhow!(
                "handoff render failed"
            )))
            .into_response();
        }
    };

    // Second pass over the rendered text (and the notes): defence in depth
    // for anything the render itself composed (e.g. clipped fragments).
    let mut text = brief.text;
    let mut notes = conv.notes;
    if scrubbed {
        let opts = kb_core::session_scrub::ScrubOptions::secrets_only();
        text = kb_core::session_scrub::scrub_transcript(&text, &opts).0;
        for n in &mut notes {
            *n = kb_core::session_scrub::scrub_transcript(n, &opts).0;
        }
    }

    Json(HandoffOut {
        schema: HANDOFF_SCHEMA.to_string(),
        command: launch_command(&row_sid, target),
        session_id: row_sid,
        source_harness: conv.harness.as_str().to_string(),
        source_kind: "capture".to_string(),
        target: target.as_str().to_string(),
        chars: u32::try_from(text.chars().count()).unwrap_or(u32::MAX),
        brief: text,
        truncated: brief.truncated,
        notes,
        scrubbed,
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_command_is_copy_only_text_and_refuses_unsafe_ids() {
        assert_eq!(
            launch_command("01a1e4a1-0d74", Harness::Claude),
            "kb sessions handoff 01a1e4a1-0d74 --to claude --launch"
        );
        for bad in ["a b; rm -rf /", "-rf", "", "a\nb"] {
            assert_eq!(
                launch_command(bad, Harness::Omp),
                kb_core::sessions::resume::NOT_RESUMABLE_UNSAFE,
                "{bad:?}"
            );
        }
    }
}
