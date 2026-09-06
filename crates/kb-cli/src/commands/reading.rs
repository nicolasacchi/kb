//! `kb reading <target>` — RP-track reading-progress summary for an artifact:
//! how far it was read, what was read vs skimmed, where the reader stopped,
//! and which sections held their attention. Talks to the daemon's
//! `GET /api/kb/{kb}/artifacts/{id}/reading`; `<target>` is a 12-hex id, a
//! source-relative path, or a unique filename (resolved via `/lookup`, like
//! `kb list` / `kb get`). `--lite` returns the whole-page summary only;
//! `--json` emits the raw summary for Claude Code to consult before revising
//! an artifact it authored (preserve what was read, freely revise the unseen).

use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::http;

pub async fn run(
    target: &str,
    kb: Option<&str>,
    json: bool,
    lite: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let url = require_daemon(daemon, bearer).await?;
    let (kb_name, id) = resolve_target(kb, target, daemon, bearer).await?;
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let mut req_url = format!(
        "{url}/api/kb/{}/artifacts/{}/reading",
        http::encode_path_segment(&kb_name),
        http::encode_path_segment(&id),
    );
    if lite {
        req_url.push_str("?lite=true");
    }
    let summary: Value = client
        .get(&req_url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    if json {
        // Echo the daemon summary verbatim + the resolved id/kb/source,
        // mirroring `kb recall --json` so the agent gets a self-contained
        // object it can act on without a second lookup.
        let mut out = summary.clone();
        if let Some(obj) = out.as_object_mut() {
            obj.insert("kb".into(), Value::String(kb_name));
            obj.insert("id".into(), Value::String(id));
            obj.insert("source".into(), Value::String(url));
        }
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    print_human(&summary, &kb_name, &id, lite);
    Ok(())
}

fn print_human(s: &Value, kb: &str, id: &str, lite: bool) {
    let visits = s["visit_count"].as_i64().unwrap_or(0);
    println!("reading   {id}  [{kb}]");
    if visits == 0 {
        println!("not read yet (no recorded visits)");
        return;
    }
    let completion = s["completion_pct"].as_i64().unwrap_or(0);
    let read_pct = s["read_pct"].as_i64().unwrap_or(0);
    let fully = s["is_fully_read"].as_bool().unwrap_or(false);
    let active = s["active_ms_total"].as_i64().unwrap_or(0);
    let last = s["last_read_at"].as_i64().unwrap_or(0);
    println!(
        "read      {completion}% scrolled · {read_pct}% of content read{}",
        if fully { " · ✓ fully read" } else { "" }
    );
    println!(
        "active    {}  over {visits} visit{}",
        format_dur(active),
        if visits == 1 { "" } else { "s" }
    );
    if last > 0 {
        println!("last      {}", format_started(last));
    }
    if let Some(stop) = s["stopped_at"].as_object() {
        let text = stop.get("text").and_then(|v| v.as_str()).unwrap_or("?");
        let pct = stop.get("pct").and_then(|v| v.as_i64()).unwrap_or(0);
        println!("stopped   \"{}\" (~{pct}%)", truncate(text, 60));
    }
    if lite {
        return;
    }
    if let Some(arr) = s["sections"].as_array() {
        if !arr.is_empty() {
            println!("\n  {:<34}  {:>6}  read?", "section", "dwell");
            println!("  {}", "─".repeat(52));
            for sec in arr {
                let text = sec["text"].as_str().unwrap_or("?");
                let dwell = sec["dwell_ms"].as_i64().unwrap_or(0);
                let glyph = match sec["state"].as_str().unwrap_or("unseen") {
                    "read" => "✓ read",
                    "skim" => "~ skim",
                    _ => "· unseen",
                };
                println!(
                    "  {:<34}  {:>6}  {glyph}",
                    truncate(text, 34),
                    format_dur_short(dwell),
                );
            }
        }
    }
    if let Some(arr) = s["top_sections"].as_array() {
        let ids: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
        if !ids.is_empty() {
            println!("\nmost time: {}", ids.join(", "));
        }
    }
}

async fn require_daemon(daemon: Option<&str>, bearer: Option<&str>) -> Result<String> {
    http::detect_daemon(daemon, bearer).await.ok_or_else(|| {
        anyhow!(
            "daemon not reachable{} — start it with `kb daemon`",
            daemon.map(|d| format!(" at {d}")).unwrap_or_default()
        )
    })
}

/// Resolve `(kb?, target)` to `(kb_name, artifact_id)` via `/lookup`.
/// Mirrors `http::resolve_artifact_target` — a 12-hex id short-circuits;
/// a path / unique filename goes through the daemon's lookup.
async fn resolve_target(
    kb: Option<&str>,
    target: &str,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<(String, String)> {
    let resolved_kb = http::resolve_default_kb(kb, daemon, bearer).await?;
    let looks_like_id = target.len() == 12 && target.chars().all(|c| c.is_ascii_hexdigit());
    if looks_like_id {
        return Ok((resolved_kb, target.to_string()));
    }
    let body = http::get_lookup(daemon, &resolved_kb, target, bearer).await?;
    let kind = body
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    match kind {
        "exact" | "unique_suffix" => {
            let id = body
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("lookup response missing `id`"))?
                .to_string();
            Ok((resolved_kb, id))
        }
        "ambiguous" => {
            let candidates = body
                .get("candidates")
                .and_then(|v| v.as_array())
                .map(|a| a.as_slice())
                .unwrap_or(&[]);
            let mut msg = format!(
                "{target:?} matched {} artifacts in {resolved_kb}; pick one:\n",
                candidates.len()
            );
            for c in candidates {
                let cid = c.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                let rel = c
                    .get("source_relative")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                msg.push_str(&format!("  {cid}  {rel}\n"));
            }
            anyhow::bail!(msg)
        }
        "not_found" => anyhow::bail!("{target:?} matched no artifact in {resolved_kb}"),
        other => anyhow::bail!("lookup returned unknown kind {other:?}: {body}"),
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// `372000` → `6m12s`; `9500000` → `2h38m20s`.
fn format_dur(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}h{m:02}m{s:02}s")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

/// Compact dwell for the section table; `0` → `—`.
fn format_dur_short(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    if secs == 0 {
        return "—".into();
    }
    let m = secs / 60;
    let s = secs % 60;
    if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

/// `YYYY-MM-DD HH:MM UTC` without chrono (same Hinnant civil_from_days the
/// `kb sessions` verb uses).
fn format_started(unix: i64) -> String {
    if unix == 0 {
        return "—".into();
    }
    let days = unix.div_euclid(86_400);
    let time = unix.rem_euclid(86_400);
    let hour = time / 3600;
    let min = (time % 3600) / 60;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {hour:02}:{min:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_dur_buckets() {
        assert_eq!(format_dur(0), "0s");
        assert_eq!(format_dur(45_000), "45s");
        assert_eq!(format_dur(372_000), "6m12s");
        assert_eq!(format_dur(9_500_000), "2h38m20s");
    }

    #[test]
    fn format_dur_short_dashes_zero() {
        assert_eq!(format_dur_short(0), "—");
        assert_eq!(format_dur_short(18_000), "18s");
        assert_eq!(format_dur_short(125_000), "2m05s");
    }

    #[test]
    fn truncate_ellipsizes() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
    }
}
