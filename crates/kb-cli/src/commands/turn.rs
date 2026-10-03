//! `kb turn "<prompt>"` — the per-prompt hook's ONE call.
//!
//! The daemon already composes recall + the turn-1 scent under a shared
//! deadline (`GET /api/turn`); this verb is the thin client. It works out the
//! repo slug locally (git main-checkout basename + the local `project_slugs`
//! aliases — no network, no `GET /api/kbs`), passes `project=` /
//! `visible_to=` so the daemon can narrow recall exactly as `kb recall`'s
//! auto scope does, and names every degraded lane in one short line so a
//! skipped lane is visible instead of silent.
//!
//! Nothing here ranks or composes memory text: `text` is printed verbatim.
//! The `degraded_note` field is the only thing this verb adds, and it is
//! derived from the response's own `degraded[]`.

use crate::commands::memory::{
    current_repo_slug, current_repo_slug_in, local_project_slug_aliases,
};
use crate::http;
use anyhow::{anyhow, Result};
use std::collections::HashMap;

/// Per-prompt budget the hook gives the daemon, ms. Sits under the 13 s
/// shared hook budget so the CLI's own HTTP margin still fits.
pub const DEFAULT_DEADLINE_MS: u64 = 9000;
/// HTTP slack on top of the server deadline (connect, serialisation).
const HTTP_MARGIN_SECS: u64 = 2;

/// `(project, visible_to)` for a repo slug, with no I/O. A local
/// `project_slugs` alias names the corpus; otherwise `memory-<slug>`. The
/// daemon checks the name against its own config and fails open on a miss.
pub(crate) fn turn_scope(
    slug: &str,
    aliases: &HashMap<String, String>,
) -> (Option<String>, Option<String>) {
    let slug = slug.trim();
    if slug.is_empty() {
        return (None, None);
    }
    let project = aliases
        .get(slug)
        .cloned()
        .unwrap_or_else(|| format!("memory-{slug}"));
    let visible = format!("{slug},{project}");
    (Some(project), Some(visible))
}

/// One short line naming every degraded lane, or `None` when nothing
/// degraded. `embed` is a keyword-only fallback (results still came back);
/// every other class means the lane's results were dropped.
pub(crate) fn degraded_note(resp: &serde_json::Value) -> Option<String> {
    let arr = resp.get("degraded")?.as_array()?;
    let mut parts: Vec<String> = Vec::new();
    for d in arr {
        let lane = d.get("lane").and_then(|v| v.as_str()).unwrap_or("?");
        let class = d
            .get("error_class")
            .and_then(|v| v.as_str())
            .unwrap_or("other");
        let part = if class == "embed" {
            format!("{lane} degraded (embed down, keyword-only)")
        } else {
            format!("{lane} skipped ({class})")
        };
        if !parts.contains(&part) {
            parts.push(part);
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!("kb: {}", parts.join("; ")))
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn turn(
    prompt: &str,
    cwd: Option<&str>,
    session: Option<&str>,
    deadline_ms: Option<u64>,
    lanes: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let base = daemon
        .unwrap_or("http://127.0.0.1:4000")
        .trim_end_matches('/')
        .to_string();
    let deadline = deadline_ms.unwrap_or(DEFAULT_DEADLINE_MS);
    let slug = match cwd {
        Some(c) => current_repo_slug_in(&std::path::PathBuf::from(c)),
        None => current_repo_slug(),
    };
    let (project, visible_to) = turn_scope(&slug, &local_project_slug_aliases());
    let secs = deadline.div_ceil(1000) + HTTP_MARGIN_SECS;
    let client = http::client_with_timeout_and_bearer(secs, bearer)?;
    let deadline_str = deadline.to_string();
    let mut req = client
        .get(format!("{base}/api/turn"))
        .query(&[("q", prompt), ("deadline_ms", deadline_str.as_str())]);
    if let Some(c) = cwd {
        req = req.query(&[("cwd", c)]);
    }
    if let Some(s) = session {
        req = req.query(&[("session", s)]);
    }
    if let Some(l) = lanes {
        req = req.query(&[("lanes", l)]);
    }
    if let Some(p) = &project {
        req = req.query(&[("project", p.as_str())]);
    }
    if let Some(v) = &visible_to {
        req = req.query(&[("visible_to", v.as_str())]);
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) if e.is_timeout() => {
            return Err(anyhow!(
                "daemon slow (GET {base}/api/turn > {secs}s) — it is up but busy, not down"
            ))
        }
        Err(e) if e.is_connect() => {
            return Err(anyhow!(
                "daemon not reachable at {base} — start it with `kb daemon`"
            ))
        }
        Err(e) => return Err(anyhow!("turn failed: {e}")),
    };
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        // 404/405 here means an older daemon without the route; the hook
        // treats ANY non-zero exit as "use the old path".
        return Err(anyhow!("turn failed: HTTP {}", status.as_u16()));
    }
    let mut value: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| anyhow!("turn: bad JSON from daemon: {e}"))?;
    let note = degraded_note(&value);
    if let (Some(n), Some(obj)) = (&note, value.as_object_mut()) {
        obj.insert("degraded_note".into(), serde_json::Value::String(n.clone()));
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        let text = value["text"].as_str().unwrap_or("");
        if !text.is_empty() {
            println!("{text}");
        }
        if let Some(n) = note {
            eprintln!("{n}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scope_derives_memory_slug_without_io() {
        let (p, v) = turn_scope("kb", &HashMap::new());
        assert_eq!(p.as_deref(), Some("memory-kb"));
        assert_eq!(v.as_deref(), Some("kb,memory-kb"));
    }

    #[test]
    fn scope_prefers_a_local_alias_and_skips_outside_a_repo() {
        let mut a = HashMap::new();
        a.insert("kb".to_string(), "memory-core".to_string());
        let (p, v) = turn_scope("kb", &a);
        assert_eq!(p.as_deref(), Some("memory-core"));
        assert_eq!(v.as_deref(), Some("kb,memory-core"));
        assert_eq!(turn_scope("", &a), (None, None));
    }

    #[test]
    fn degraded_note_names_each_lane_once() {
        let r = json!({"degraded": [
            {"kb": "turn", "lane": "recall", "error_class": "timeout"},
            {"kb": "memory-x", "lane": "recall", "error_class": "timeout"},
            {"kb": "turn", "lane": "context", "error_class": "storage"},
        ]});
        assert_eq!(
            degraded_note(&r).as_deref(),
            Some("kb: recall skipped (timeout); context skipped (storage)")
        );
    }

    #[test]
    fn degraded_note_calls_an_embed_fallback_a_fallback_not_a_skip() {
        let r = json!({"degraded": [{"kb": "m", "lane": "recall", "error_class": "embed"}]});
        assert_eq!(
            degraded_note(&r).as_deref(),
            Some("kb: recall degraded (embed down, keyword-only)")
        );
    }

    #[test]
    fn degraded_note_is_none_when_clean() {
        assert_eq!(degraded_note(&json!({"text": "x"})), None);
        assert_eq!(degraded_note(&json!({"degraded": []})), None);
    }
}
