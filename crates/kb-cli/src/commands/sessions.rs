//! `kb sessions` — read captured Claude Code transcripts via the
//! daemon's /api/sessions/* surface. v0.14 Track S.
//!
//! Mirrors the `kb comments` verb pattern: every call requires a
//! reachable daemon (the data lives in the per-kb V0008 sessions
//! sqlite table + the cross-kb fan-out HTTP routes; there's no
//! offline path). W0.6 adds three offline-git + daemon probes
//! (`by_commit`, `provenance_report`, `why_line`) for kb-code's
//! wave-0 sha→session wedge instrument.

use crate::http;
use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashSet;
use std::path::Path;

#[allow(clippy::too_many_arguments)]
pub async fn list(
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
    limit: usize,
    folder: Option<&str>,
    q: Option<&str>,
    project: Option<&str>,
    substance: Option<&str>,
    harness: Option<&str>,
) -> Result<()> {
    let url = require_daemon(daemon, bearer).await?;
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    // W0.6 — forward `--limit` to the server: this call previously fetched
    // the server's DEFAULT_LIMIT (50) rows unconditionally and only
    // truncated CLIENT-side, so `kb sessions list --limit 500` silently
    // capped at 50. The server clamps to its own MAX_LIMIT independently.
    let limit_s = limit.to_string();
    let mut req = client
        .get(format!("{url}/api/sessions"))
        .query(&[("limit", limit_s.as_str())]);
    if let Some(f) = folder {
        req = req.query(&[("folder", f)]);
    }
    if let Some(query) = q {
        req = req.query(&[("q", query)]);
    }
    // W4/W3.A — the P1-derived-key project axis + the substance triage
    // filter, mirroring the server params `list`/`gallery` already ship.
    if let Some(p) = project {
        req = req.query(&[("project", p)]);
    }
    if let Some(s) = substance {
        req = req.query(&[("substance", s)]);
    }
    // W5/I — the harness facet.
    if let Some(h) = harness {
        req = req.query(&[("harness", h)]);
    }
    let body: serde_json::Value = req.send().await?.error_for_status()?.json().await?;
    let empty: Vec<serde_json::Value> = Vec::new();
    let rows = body["sessions"].as_array().unwrap_or(&empty);
    if json {
        let truncated: Vec<&serde_json::Value> = rows.iter().take(limit).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "sessions": truncated,
            }))?
        );
        return Ok(());
    }
    if rows.is_empty() {
        println!("no sessions captured yet");
        return Ok(());
    }
    // A2/A3 — two-line rows: the readable display_name + a dimmed first
    // prompt, with a folder badge (A1) and the file-activity counts (A4/A6).
    for r in rows.iter().take(limit) {
        let name = r["display_name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| r["first_user_prompt"].as_str())
            .unwrap_or(r["session_id"].as_str().unwrap_or("-"));
        let started = r["started_at"].as_i64().unwrap_or(0);
        let kb = r["kb"].as_str().unwrap_or("-");
        let folder_label = r["folder"].as_str().unwrap_or("");
        let mc = r["message_count"].as_i64().unwrap_or(0);
        let mem = r["memory_count"].as_i64().unwrap_or(0);
        let fr = r["files_read_count"].as_i64().unwrap_or(0);
        let fe = r["files_edited_count"].as_i64().unwrap_or(0);
        let prompt = r["first_user_prompt"].as_str().unwrap_or("");
        let where_ = if folder_label.is_empty() {
            format!("[{kb}]")
        } else {
            format!("[{kb}/{folder_label}]")
        };
        // W5/I — harness name, dimmed by omission in the common all-claude
        // case (Proposal 5: "CLI uses the NAME, dimmed — glyph fonts are
        // unreliable in terminals"; quiet unless mixed, same rule
        // folders/rollup's per-harness suffix already uses).
        let harness = r["harness"].as_str().unwrap_or("claude");
        let harness_suffix = if harness == "claude" {
            String::new()
        } else {
            format!(" ({harness})")
        };
        println!(
            "  {ts}  {where_}  {name}{harness_suffix}",
            ts = format_started(started),
            name = truncate(name, 72),
        );
        println!(
            "      {mc:>4} msg  {mem:>2} mem  {fr:>3} read  {fe:>3} edited  {prompt}",
            prompt = truncate(prompt, 60),
        );
    }
    Ok(())
}

/// `kb sessions folders` — the A1 folder facet (working directories with
/// session counts), newest-active first.
pub async fn folders(daemon: Option<&str>, bearer: Option<&str>, json: bool) -> Result<()> {
    let url = require_daemon(daemon, bearer).await?;
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let body: serde_json::Value = client
        .get(format!("{url}/api/sessions/folders"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let empty: Vec<serde_json::Value> = Vec::new();
    let rows = body["folders"].as_array().unwrap_or(&empty);
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("no session folders yet");
        return Ok(());
    }
    for r in rows {
        let folder = r["folder"].as_str().unwrap_or("-");
        let count = r["count"].as_i64().unwrap_or(0);
        let latest = r["latest"].as_i64().unwrap_or(0);
        println!("  {count:>4}  {ts}  {folder}", ts = format_started(latest),);
    }
    Ok(())
}

/// `kb sessions rollup` (R9) — top research queries per project folder.
pub async fn rollup(
    folder: Option<&str>,
    project: Option<&str>,
    substance: Option<&str>,
    limit: u32,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let url = require_daemon(daemon, bearer).await?;
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let limit_s = limit.to_string();
    let mut params: Vec<(&str, &str)> = vec![("limit", &limit_s)];
    if let Some(f) = folder {
        params.push(("folder", f));
    }
    // W4/W3.A — the server already accepts `?project=` on this route.
    if let Some(p) = project {
        params.push(("project", p));
    }
    // L1/F1 — the server accepts `?substance=` on this route.
    if let Some(s) = substance {
        params.push(("substance", s));
    }
    let body: serde_json::Value = client
        .get(format!("{url}/api/sessions/research-rollup"))
        .query(&params)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let empty: Vec<serde_json::Value> = Vec::new();
    let folders = body["folders"].as_array().unwrap_or(&empty);
    if folders.is_empty() {
        println!("no research recorded yet");
        return Ok(());
    }
    for f in folders {
        let label = f["label"].as_str().unwrap_or("-");
        let latest = f["latest"].as_i64().unwrap_or(0);
        println!("\n▣ {label}  ({ts})", ts = format_started(latest));
        if let Some(qs) = f["queries"].as_array() {
            for q in qs {
                let count = q["count"].as_i64().unwrap_or(0);
                let kind = q["kind"].as_str().unwrap_or("?");
                let query = q["query"].as_str().unwrap_or("");
                println!("  {count:>4}×  {kind:<10}  {}", truncate(query, 56));
            }
        }
    }
    Ok(())
}

/// `kb sessions funnel` (R9) — searched → opened → edited → committed → commented.
pub async fn funnel(
    folder: Option<&str>,
    project: Option<&str>,
    substance: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let url = require_daemon(daemon, bearer).await?;
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let mut params: Vec<(&str, &str)> = Vec::new();
    if let Some(f) = folder {
        params.push(("folder", f));
    }
    // W4/W3.A — the server already accepts `?project=` on this route.
    if let Some(p) = project {
        params.push(("project", p));
    }
    // L1/F1 — the server accepts `?substance=` on this route.
    if let Some(s) = substance {
        params.push(("substance", s));
    }
    let mut req = client.get(format!("{url}/api/sessions/funnel"));
    if !params.is_empty() {
        req = req.query(&params);
    }
    let body: serde_json::Value = req.send().await?.error_for_status()?.json().await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let empty: Vec<serde_json::Value> = Vec::new();
    let stages = body["stages"].as_array().unwrap_or(&empty);
    if let Some(f) = folder {
        println!("funnel · {f}");
    } else {
        println!("funnel · all projects");
    }
    let line: String = stages
        .iter()
        .map(|s| {
            format!(
                "{} {}",
                s["stage"].as_str().unwrap_or("?"),
                s["events"].as_i64().unwrap_or(0)
            )
        })
        .collect::<Vec<_>>()
        .join("  →  ");
    println!("  {line}");
    for s in stages {
        let stage = s["stage"].as_str().unwrap_or("?");
        let events = s["events"].as_i64().unwrap_or(0);
        let sessions = s["sessions"].as_i64().unwrap_or(0);
        println!("  {stage:<10}  {events:>5} events  {sessions:>4} sessions");
    }
    Ok(())
}

/// Human-readable `Nh Nm` (or `Nm`, or `<1m`) for a seconds count — the
/// same shape `kb sessions read`'s header uses (`session_read.rs::
/// fmt_duration`), duplicated here rather than cross-module-exported since
/// it's three lines and this is the only other call site.
fn fmt_secs(secs: i64) -> String {
    let secs = secs.max(0);
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        "<1m".to_string()
    }
}

/// `kb sessions ledger` (moonshots M4) — a project's sessions/commits/
/// decisions/research grouped by UTC day over a trailing window. Thin CLI
/// presenter over `GET /api/sessions/ledger`; `--json` prints the wire
/// verbatim (the same object `/kb-weekly` reads).
pub async fn ledger(
    project: Option<&str>,
    days: Option<u32>,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let url = require_daemon(daemon, bearer).await?;
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let mut params: Vec<(&str, String)> = Vec::new();
    if let Some(p) = project {
        params.push(("project", p.to_string()));
    }
    if let Some(d) = days {
        params.push(("days", d.to_string()));
    }
    let body: serde_json::Value = client
        .get(format!("{url}/api/sessions/ledger"))
        .query(&params)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    let label = project.unwrap_or("all projects");
    let window_days = body["days"].as_u64().unwrap_or(0);
    println!("ledger · {label} · last {window_days}d");

    let empty: Vec<serde_json::Value> = Vec::new();
    let days_out = body["days_out"].as_array().unwrap_or(&empty);
    let mut any_activity = false;
    for d in days_out {
        let sessions = d["sessions"].as_array().unwrap_or(&empty);
        let commits = d["commits"].as_array().unwrap_or(&empty);
        let decisions = d["decisions_count"].as_u64().unwrap_or(0);
        if sessions.is_empty() && commits.is_empty() && decisions == 0 {
            continue; // quiet days stay out of the printed view (still in --json)
        }
        any_activity = true;
        let date = d["date"].as_str().unwrap_or("-");
        println!("\n{date}");
        for s in sessions {
            let name = s["display_name"].as_str().unwrap_or("-");
            let active = fmt_secs(s["active_secs"].as_i64().unwrap_or(0));
            println!("  · {} ({active})", truncate(name, 72));
            if let Some(outcome) = s["outcome"].as_str().filter(|s| !s.is_empty()) {
                println!("      closed  {}", truncate(outcome, 80));
            }
        }
        if !commits.is_empty() {
            println!("  commits:");
            for c in commits {
                let sha = c["sha"].as_str().unwrap_or("-------");
                let sha7: String = sha.chars().take(7).collect();
                let subject = c["subject"].as_str().unwrap_or("");
                println!("    {sha7}  {}", truncate(subject, 72));
            }
        }
        if decisions > 0 {
            println!("  decisions: {decisions}");
        }
        if let Some(topics) = d["research_topics"].as_array() {
            let names: Vec<&str> = topics.iter().filter_map(|t| t.as_str()).collect();
            if !names.is_empty() {
                println!("  researched: {}", names.join("; "));
            }
        }
    }
    if !any_activity {
        println!("\n(no captures in this window)");
    }
    let t = &body["totals"];
    println!(
        "\ntotals: {} sessions · {} commits · {} decisions · {} active",
        t["sessions"].as_u64().unwrap_or(0),
        t["commits"].as_u64().unwrap_or(0),
        t["decisions"].as_u64().unwrap_or(0),
        fmt_secs(t["active_secs"].as_i64().unwrap_or(0))
    );
    Ok(())
}

/// `kb sessions threads` — sessions clustered into continued-effort threads
/// (P7), newest thread first.
pub async fn threads(daemon: Option<&str>, bearer: Option<&str>, json: bool) -> Result<()> {
    let url = require_daemon(daemon, bearer).await?;
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let body: serde_json::Value = client
        .get(format!("{url}/api/sessions/threads"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let empty: Vec<serde_json::Value> = Vec::new();
    let rows = body["threads"].as_array().unwrap_or(&empty);
    if rows.is_empty() {
        println!("no threads yet");
        return Ok(());
    }
    for t in rows {
        let title = t["title"].as_str().unwrap_or("-");
        let label = t["label"].as_str().unwrap_or("-");
        let count = t["count"].as_i64().unwrap_or(0);
        let started = t["started_at"].as_i64().unwrap_or(0);
        let ended = t["ended_at"].as_i64().unwrap_or(0);
        println!(
            "\n▣ {title}\n  {label} · {count} session{} · {} – {}",
            if count == 1 { "" } else { "s" },
            format_started(started),
            format_started(ended),
        );
        if let Some(sessions) = t["sessions"].as_array() {
            for s in sessions {
                println!(
                    "    {}  {}",
                    format_started(s["started_at"].as_i64().unwrap_or(0)),
                    truncate(
                        s["display_name"]
                            .as_str()
                            .unwrap_or(s["session_id"].as_str().unwrap_or("-")),
                        64
                    ),
                );
            }
        }
    }
    Ok(())
}

/// `kb sessions save-thread <folder>` — materialise a folder's most-recent
/// thread into an editable kb-list/1 list (P8).
///
/// CT-E5 — `narrative` asks the daemon for the session's STORY (capture →
/// files touched → memories produced → memories recalled) instead of one
/// entry per transcript, and flips the thread's sessions to OLDEST-first: the
/// `/threads` payload is newest-first for a feed, but a story runs forwards.
/// Without the flag the daemon takes the unchanged pre-CT-E5 path (the body
/// carries `"narrative": false`, which is the route's `serde` default).
pub async fn save_thread(
    folder: &str,
    title: Option<&str>,
    narrative: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let url = require_daemon(daemon, bearer).await?;
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let body: serde_json::Value = client
        .get(format!("{url}/api/sessions/threads"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let empty: Vec<serde_json::Value> = Vec::new();
    // The most-recent thread whose folder basename or path matches `folder`.
    let thread = body["threads"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .find(|t| t["folder"].as_str() == Some(folder) || t["label"].as_str() == Some(folder));
    let Some(t) = thread else {
        return Err(anyhow!("no thread found for folder '{folder}'"));
    };
    let sessions = t["sessions"].as_array().cloned().unwrap_or_default();
    let kb = sessions
        .first()
        .and_then(|s| s["kb"].as_str())
        .ok_or_else(|| anyhow!("thread has no sessions"))?;
    let mut artifact_ids: Vec<&str> = sessions
        .iter()
        .filter_map(|s| s["artifact_id"].as_str())
        .collect();
    if narrative {
        artifact_ids.reverse(); // newest-first feed → oldest-first story
    }
    let list_title = title.unwrap_or_else(|| t["title"].as_str().unwrap_or("thread"));
    let resp: serde_json::Value = client
        .post(format!("{url}/api/sessions/threads/save"))
        .json(&serde_json::json!({
            "kb": kb,
            "title": list_title,
            "artifact_ids": artifact_ids,
            "narrative": narrative,
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    println!(
        "saved {} sessions as {} list '{}'  (kb={} id={})",
        artifact_ids.len(),
        if narrative { "narrative" } else { "flat" },
        list_title,
        resp["kb"].as_str().unwrap_or("-"),
        resp["list_id"].as_str().unwrap_or("-"),
    );
    Ok(())
}

/// The `--section` csv vocabulary (cli-grok Proposal 2 #4). `outcome` is the
/// one entry with no dedicated `/sessions/{id}/<sub>` route — it rides
/// `GET /sessions/{id}/view?fields=header` (`ViewHeader.opening`/`.outcome`).
pub const SHOW_SECTIONS: &[&str] = &[
    "files",
    "decisions",
    "commits",
    "research",
    "memories",
    "touches",
    "comments",
    "readings",
    "outcome",
];

/// `kb sessions show <sid> [--section a,b,...]` — session detail report.
/// `sections` empty = bare `show`, byte-compatible with pre-W4 behaviour
/// (every section, unconditional fetch). Non-empty = ONLY the named
/// sub-resources are fetched — kills the unconditional 8-round-trip for a
/// caller that wants one fact (cli.md §c); `--section X --json` prints just
/// that subresource's wire (an object of `{section: wire}` when >1 named).
pub async fn show(
    session_id: &str,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
    sections: &[String],
) -> Result<()> {
    for s in sections {
        if !SHOW_SECTIONS.contains(&s.as_str()) {
            return Err(anyhow!(
                "unknown --section {s:?} — expected one of {}",
                SHOW_SECTIONS.join(",")
            ));
        }
    }
    let url = require_daemon(daemon, bearer).await?;
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let all = sections.is_empty();
    let want = |s: &str| all || sections.iter().any(|x| x == s);
    let seg = http::encode_path_segment(session_id);

    let detail: serde_json::Value = client
        .get(format!("{url}/api/sessions/{seg}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let null = || serde_json::Value::Null;
    let fetch = |suffix: &'static str| {
        let client = client.clone();
        let url = format!("{url}/api/sessions/{seg}/{suffix}");
        async move {
            client
                .get(url)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await
        }
    };
    let memories: serde_json::Value = if want("memories") {
        fetch("memories").await?
    } else {
        null()
    };
    let touches: serde_json::Value = if want("touches") {
        fetch("touches").await?
    } else {
        null()
    };
    let readings: serde_json::Value = if want("readings") {
        fetch("readings").await?
    } else {
        null()
    };
    // A4/A6 — the file-activity manifest (read/edit/write, in-corpus links +
    // out-of-corpus plain paths).
    let files: serde_json::Value = if want("files") {
        fetch("files").await?
    } else {
        null()
    };
    // S9 — the decisions log.
    let decisions: serde_json::Value = if want("decisions") {
        fetch("decisions").await?
    } else {
        null()
    };
    // P5 — the commits the session produced.
    let commits: serde_json::Value = if want("commits") {
        fetch("commits").await?
    } else {
        null()
    };
    // R4 — the research / tool-usage signals the session produced.
    let research: serde_json::Value = if want("research") {
        fetch("research").await?
    } else {
        null()
    };
    // R5 — open comments on the in-corpus artifacts the session touched.
    let comments: serde_json::Value = if want("comments") {
        fetch("comments").await?
    } else {
        null()
    };
    // R3/V0029 — the closure quintuple, live via the view engine (no
    // dedicated sub-route — Outcome lives on `ViewHeader`, not a V0008 table).
    let outcome_header: serde_json::Value = if want("outcome") {
        let v: serde_json::Value = client
            .get(format!("{url}/api/sessions/{seg}/view"))
            .query(&[("fields", "header")])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        v["header"].clone()
    } else {
        null()
    };

    if json {
        if all {
            let combined = serde_json::json!({
                "session": detail,
                "memories": memories["memories"],
                "touches": touches,
                "readings": readings["readings"],
                "files": files["files"],
                "decisions": decisions["decisions"],
                "commits": commits["commits"],
                "research": research["research"],
                "comments": comments["artifacts"],
            });
            println!("{}", serde_json::to_string_pretty(&combined)?);
            return Ok(());
        }
        // `--section` scoped JSON: verbatim wire per requested subresource,
        // one entry per name (mirrors what a direct `GET .../{section}` call
        // returns — `outcome`'s IS the raw `ViewHeader`, not re-shaped).
        let mut scoped = serde_json::Map::new();
        let raw = |v: &serde_json::Value, key: &str| v.get(key).cloned().unwrap_or(v.clone());
        for s in sections {
            let v = match s.as_str() {
                "memories" => raw(&memories, "memories"),
                "touches" => touches.clone(),
                "readings" => raw(&readings, "readings"),
                "files" => raw(&files, "files"),
                "decisions" => raw(&decisions, "decisions"),
                "commits" => raw(&commits, "commits"),
                "research" => raw(&research, "research"),
                "comments" => raw(&comments, "artifacts"),
                "outcome" => outcome_header.clone(),
                other => {
                    return Err(anyhow!(
                        "unknown --section {other:?} — expected one of {}",
                        SHOW_SECTIONS.join(",")
                    ))
                }
            };
            scoped.insert(s.clone(), v);
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::Value::Object(scoped))?
        );
        return Ok(());
    }
    if let Some(name) = detail["display_name"].as_str() {
        println!("session   {name}");
        println!("id        {}", detail["session_id"].as_str().unwrap_or("-"));
    } else {
        println!("session   {}", detail["session_id"].as_str().unwrap_or("-"));
    }
    println!(
        "started   {}",
        format_started(detail["started_at"].as_i64().unwrap_or(0))
    );
    // W4/R13 — the `ended:` line: the A3 fix at the `show` grain (V0029
    // `ended_at`, always present — a session that never closed cleanly still
    // has SOME last-event timestamp).
    println!(
        "ended     {}",
        format_started(detail["ended_at"].as_i64().unwrap_or(0))
    );
    println!("kb        {}", detail["kb"].as_str().unwrap_or("-"));
    if let Some(cwd) = detail["cwd"].as_str() {
        println!("folder    {cwd}");
    }
    if let Some(branch) = detail["git_branch"].as_str() {
        println!("branch    {branch}");
    }
    // W4/R13/Proposal-5 — harness only printed when non-default (quiet in
    // the common all-claude case, per the memo's facet-quiet policy).
    match detail["harness"].as_str() {
        Some(h) if h != "claude" && !h.is_empty() => println!("harness   {h}"),
        _ => {}
    }
    if let Some(pk) = detail["project_key"].as_str().filter(|s| !s.is_empty()) {
        println!("project   {pk}");
    }
    println!(
        "messages  {}",
        detail["message_count"].as_i64().unwrap_or(0)
    );
    println!(
        "files     {} read, {} edited",
        detail["files_read_count"].as_i64().unwrap_or(0),
        detail["files_edited_count"].as_i64().unwrap_or(0),
    );
    // S9 — effort + error stats.
    let tools = detail["tool_calls"].as_i64().unwrap_or(0);
    let tokens = detail["token_total"].as_i64().unwrap_or(0);
    let errs = detail["error_count"].as_i64().unwrap_or(0);
    if tools > 0 || tokens > 0 || errs > 0 {
        let model = detail["model"].as_str().unwrap_or("-");
        println!("effort    {tools} tools · {tokens} tok · {errs} errors · {model}");
    }
    // W0.2 — subagent aggregate stats: the parent-transcript SYNCHRONOUS
    // fallback. A main-only session (no Agent/Task delegations) omits this
    // line entirely; a session that delegated prints the completed-with-
    // stats totals plus a separate launched-but-unstatted count so a zero
    // never reads as "no subagents ran" (their real numbers live in the
    // per-agent sidecar files).
    let sub_count = detail["subagent_count"].as_i64().unwrap_or(0);
    let sub_unstatted = detail["subagent_launched_unstatted"].as_i64().unwrap_or(0);
    if sub_count > 0 || sub_unstatted > 0 {
        let sub_tokens = detail["subagent_tokens"].as_i64().unwrap_or(0);
        let sub_tools = detail["subagent_tool_calls"].as_i64().unwrap_or(0);
        let sub_edited = detail["subagent_files_edited"].as_i64().unwrap_or(0);
        print!(
            "subagents {sub_count} agent{} · {sub_tokens} tok · {sub_tools} tools · {sub_edited} edited",
            if sub_count == 1 { "" } else { "s" },
        );
        if sub_unstatted > 0 {
            print!(" · {sub_unstatted} launched (no stats)");
        }
        println!();
    }
    if let Some(preview) = detail["first_user_prompt"].as_str() {
        println!("first     {}", truncate(preview, 200));
    }
    // R3/W4/R13 — the closure, immediately under the opening ask (memo:
    // "sections reordered outcome-first" — this is the highest-value line in
    // the whole redesign, the A3 fix). `detail.outcome` is the already-capped
    // 240-char wire preview (V0029 `last_assistant_text`); no extra fetch.
    if let Some(closed) = detail["outcome"].as_str().filter(|s| !s.is_empty()) {
        println!("closed    {}", truncate(closed, 200));
    }
    // S9 — the decisions log.
    let decision_rows = decisions["decisions"].as_array();
    if let Some(arr) = decision_rows {
        if !arr.is_empty() {
            println!("\ndecisions ({})", arr.len());
            for d in arr {
                let kind = d["kind"].as_str().unwrap_or("?");
                if kind == "plan" {
                    println!("  ✓ plan approved");
                } else {
                    println!(
                        "  ◆ {}\n      → {}",
                        truncate(d["prompt"].as_str().unwrap_or("-"), 72),
                        truncate(d["answer"].as_str().unwrap_or("-"), 72),
                    );
                }
            }
        }
    }
    // P5 — the commits the session produced.
    if let Some(arr) = commits["commits"].as_array() {
        if !arr.is_empty() {
            println!("\ncommits ({})", arr.len());
            for c in arr {
                let kind = c["kind"].as_str().unwrap_or("?");
                let sha = c["sha"].as_str().unwrap_or("-------");
                let subject = c["subject"].as_str().unwrap_or("");
                println!("  {sha:>8}  {kind:<6}  {}", truncate(subject, 60));
            }
        }
    }
    // W4/R13 — the structured outcome (only when explicitly fetched: bare
    // `show` already surfaced the plain 240-char `closed:` line above from
    // `detail` at zero extra cost; `--section outcome` pulls the richer
    // view-engine object — stop_reason + per-commit resolution).
    if want("outcome") && !all {
        if let Some(text) = outcome_header["outcome"]["text"]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            println!("\noutcome");
            println!("  {}", truncate(text, 200));
            if let Some(sr) = outcome_header["outcome"]["stop_reason"].as_str() {
                println!("  stop_reason: {sr}");
            }
            if let Some(commits) = outcome_header["outcome"]["commits"].as_array() {
                for c in commits {
                    let resolved = if c["resolved"].as_bool().unwrap_or(false) {
                        "resolved"
                    } else {
                        "unresolved"
                    };
                    println!(
                        "  commit: {} {} ({resolved})",
                        c["sha"].as_str().unwrap_or("-------"),
                        truncate(c["subject"].as_str().unwrap_or(""), 60),
                    );
                }
            }
        }
    }
    // R4 — what the session researched / explored (kb & web searches,
    // subagents, skills, plan presentations).
    if let Some(arr) = research["research"].as_array() {
        if !arr.is_empty() {
            println!("\nresearched ({})", arr.len());
            for r in arr {
                let kind = r["kind"].as_str().unwrap_or("?");
                let query = r["query"].as_str().unwrap_or("");
                println!("  {kind:<10}  {}", truncate(query, 60));
            }
        }
    }
    // R5 — open comments raised on the artifacts this session touched.
    if let Some(arr) = comments["artifacts"].as_array() {
        let total = comments["total"].as_i64().unwrap_or(0);
        if total > 0 {
            println!(
                "\ncomments ({total} open on {} artifact{})",
                arr.len(),
                if arr.len() == 1 { "" } else { "s" }
            );
            for a in arr {
                let title = a["title"].as_str().unwrap_or("-");
                let kb = a["kb"].as_str().unwrap_or("?");
                println!("  {} [{kb}]", truncate(title, 56));
                if let Some(cl) = a["comments"].as_array() {
                    for c in cl {
                        let who = c["author"].as_str().unwrap_or("?");
                        let body = c["body"].as_str().unwrap_or("");
                        println!("      ({who}) {}", truncate(body, 64));
                    }
                }
            }
        }
    }
    // W4/R13 — `want()`-gated: an unrequested `--section` must OMIT its
    // header entirely rather than print a misleading "(0)" (it was never
    // fetched, not genuinely empty).
    if want("memories") {
        let mem_list = memories["memories"].as_array();
        println!("\nmemories ({})", mem_list.map(|a| a.len()).unwrap_or(0));
        if let Some(arr) = mem_list {
            for m in arr {
                println!(
                    "  {}  {}",
                    m["id"].as_str().unwrap_or("-"),
                    m["title"].as_str().unwrap_or("-"),
                );
            }
        }
    }
    if want("touches") {
        let touched = touches["artifact_ids"].as_array();
        println!(
            "\ntouched artifacts ({}, {})",
            touched.map(|a| a.len()).unwrap_or(0),
            touches["confidence"].as_str().unwrap_or("?"),
        );
        if let Some(arr) = touched {
            for id in arr {
                println!("  {}", id.as_str().unwrap_or("-"));
            }
        }
    }
    // A4/A6 — files the session read/edited/wrote. In-corpus files show
    // their kb + artifact id; out-of-corpus files show the plain path.
    if want("files") {
        let file_rows = files["files"].as_array();
        println!(
            "\nfiles touched ({})",
            file_rows.map(|a| a.len()).unwrap_or(0)
        );
        if let Some(arr) = file_rows {
            for f in arr {
                let action = f["action"].as_str().unwrap_or("?");
                let path = f["path"].as_str().unwrap_or("-");
                let link = if f["in_corpus"].as_bool().unwrap_or(false) {
                    let kb = f["kb"].as_str().unwrap_or("?");
                    let id = f["target_artifact_id"].as_str().unwrap_or("?");
                    format!("  → {kb}/{id}")
                } else {
                    String::new()
                };
                println!("  {action:>5}  {}{link}", truncate(path, 60));
            }
        }
    }
    if !want("readings") {
        return Ok(());
    }
    // RP-track — what the human READ in the SPA during the session window
    // (distinct from "touched" = referenced by the agent in the transcript).
    let read = readings["readings"].as_array();
    println!(
        "\nread during session ({})",
        read.map(|a| a.len()).unwrap_or(0)
    );
    if let Some(arr) = read {
        for r in arr {
            let title = r["title"]
                .as_str()
                .or_else(|| r["source_relative"].as_str())
                .unwrap_or("-");
            let pct = r["read_pct"]
                .as_i64()
                .map(|p| format!("{p}%"))
                .unwrap_or_else(|| "—".into());
            println!("  {pct:>4}  {}", truncate(title, 64));
        }
    }
    Ok(())
}

/// `kb sessions of <artifact-id-or-path> [--kb]` (cli-grok Proposal 2 #2) —
/// the reverse link: which sessions touched this artifact, and how
/// (`GET /artifacts/{kb}/{artifact_id}/sessions`, previously unreachable from
/// the CLI at all). Resolves the target the SAME way every other
/// id-or-path-taking verb does (`http::resolve_artifact_target` — 12-hex
/// short-circuits, else the daemon's fuzzy `/lookup`).
pub async fn of(
    target: &str,
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let url = require_daemon(daemon, bearer).await?;
    let client = http::client_with_timeout_and_bearer(15, bearer)?;
    let (resolved_kb, artifact_id) =
        http::resolve_artifact_target(kb, target, daemon, bearer).await?;
    let body: serde_json::Value = client
        .get(format!(
            "{url}/api/artifacts/{}/{}/sessions",
            http::encode_path_segment(&resolved_kb),
            http::encode_path_segment(&artifact_id)
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let empty: Vec<serde_json::Value> = Vec::new();
    let rows = body["sessions"].as_array().unwrap_or(&empty);
    if rows.is_empty() {
        println!("no captured session has touched {resolved_kb}/{artifact_id}");
        return Ok(());
    }
    println!(
        "{resolved_kb}/{artifact_id} — touched by {n} session(s):",
        n = rows.len()
    );
    for s in rows {
        let name = s["display_name"].as_str().unwrap_or("-");
        let kb_ = s["kb"].as_str().unwrap_or("-");
        let started = s["started_at"].as_i64().unwrap_or(0);
        let action = s["action"].as_str().unwrap_or("read");
        let authored = s["authored"].as_bool().unwrap_or(false);
        let badge = if authored { "  (authored here)" } else { "" };
        println!();
        println!(
            "  {ts}  [{kb_}]  {action:<5}  {name}{badge}",
            ts = format_started(started),
            name = truncate(name, 64),
        );
        if let Some(p) = s["first_user_prompt"].as_str().filter(|p| !p.is_empty()) {
            println!("      asked:   {}", truncate(p, 74));
        }
        if let Some(decisions) = s["decisions"].as_array() {
            for d in decisions {
                let prompt = d["prompt"].as_str().unwrap_or("");
                match d["answer"].as_str().filter(|a| !a.is_empty()) {
                    Some(a) => println!(
                        "      decided: {} → {}",
                        truncate(prompt, 46),
                        truncate(a, 24)
                    ),
                    None => println!("      decided: {}", truncate(prompt, 74)),
                }
            }
        }
        if let Some(commits) = s["commits"].as_array() {
            for c in commits {
                let kind = c["kind"].as_str().unwrap_or("commit");
                let sha = c["sha"]
                    .as_str()
                    .map(|s| format!("{s} "))
                    .unwrap_or_default();
                let subject = c["subject"].as_str().unwrap_or("");
                println!("      {kind}:  {sha}{}", truncate(subject, 66));
            }
        }
    }
    Ok(())
}

/// `kb sessions commit-map [--since] [--limit] [--offset]` (cli-grok
/// Proposal 2 #3) — the flat bulk commit↔session feed
/// (`GET /sessions/commit-map`), previously reachable only as an internal
/// paging helper for `provenance-report`. Human output = fixed-column
/// `sha · kind · resolved · sid · subject`.
pub async fn commit_map(
    since: Option<i64>,
    limit: u32,
    offset: u32,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let url = require_daemon(daemon, bearer).await?;
    let client = http::client_with_timeout_and_bearer(15, bearer)?;
    let limit_s = limit.to_string();
    let offset_s = offset.to_string();
    let mut params: Vec<(&str, &str)> = vec![("limit", &limit_s), ("offset", &offset_s)];
    let since_s;
    if let Some(s) = since {
        since_s = s.to_string();
        params.push(("since", &since_s));
    }
    let body: serde_json::Value = client
        .get(format!("{url}/api/sessions/commit-map"))
        .query(&params)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let empty: Vec<serde_json::Value> = Vec::new();
    let rows = body["commits"].as_array().unwrap_or(&empty);
    if rows.is_empty() {
        println!("no commits recorded yet");
        return Ok(());
    }
    println!(
        "{} commit(s) · offset {offset}{}",
        rows.len(),
        body["next_offset"]
            .as_u64()
            .map(|n| format!(" · next-offset {n}"))
            .unwrap_or_default(),
    );
    for r in rows {
        let kind = r["kind"].as_str().unwrap_or("commit");
        let sha = r["sha_full"]
            .as_str()
            .or_else(|| r["sha"].as_str())
            .unwrap_or("-------");
        let resolved = if r["resolved"].as_bool().unwrap_or(false) {
            "✓"
        } else {
            " "
        };
        let sid = r["session_id"].as_str().unwrap_or("-");
        let sid8: String = sid.chars().take(8).collect();
        let subject = r["subject"].as_str().unwrap_or("");
        println!(
            "  {sha:<12}  {resolved}  {kind:<6}  {sid8}  [{kb}]  {}",
            truncate(subject, 60),
            kb = r["kb"].as_str().unwrap_or("-"),
        );
    }
    Ok(())
}

/// `kb sessions by-job <ulid>` (memo R8/ADD-2) — the grokclaude job join:
/// every session whose transcript recorded a `session_research` row
/// `kind="grok_job"` for this ulid (`GET /sessions/by-job/{ulid}`). Today
/// exclusively `driver` matches (a Claude Code session that INVOKED the
/// job); `child` matches (the grokclaude job's own capture) arrive in W5 —
/// the wire shape and this presenter already carry the distinction.
pub async fn by_job(
    ulid: &str,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let url = require_daemon(daemon, bearer).await?;
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let body: serde_json::Value = client
        .get(format!(
            "{url}/api/sessions/by-job/{}",
            http::encode_path_segment(ulid)
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let empty: Vec<serde_json::Value> = Vec::new();
    let matches = body["matches"].as_array().unwrap_or(&empty);
    if matches.is_empty() {
        println!("no session recorded invoking grokclaude job {ulid}");
        return Ok(());
    }
    println!("grokclaude job {ulid} — {} link(s):", matches.len());
    for m in matches {
        let role = m["role"].as_str().unwrap_or("driver");
        let name = m["display_name"].as_str().unwrap_or("-");
        let kb = m["kb"].as_str().unwrap_or("-");
        let started = m["started_at"].as_i64().unwrap_or(0);
        println!(
            "  {ts}  [{kb}]  {role:<6}  {}",
            truncate(name, 64),
            ts = format_started(started),
        );
    }
    Ok(())
}

/// `kb sessions resume <id>` — print a deterministic, LLM-free resume-context
/// block (goal + branch + edited files + decisions) you can paste into a fresh
/// session to pick up where this one left off. S10.
///
/// W4/Proposal-2#5 — `--json` (the one sessions verb that had none) emits the
/// SAME combined payload `show --json` shapes its `session`/`files`/
/// `decisions`/`commits` keys from, plus the computed `resume_command`.
pub async fn resume(
    session_id: &str,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let url = require_daemon(daemon, bearer).await?;
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let seg = http::encode_path_segment(session_id);
    let detail: serde_json::Value = client
        .get(format!("{url}/api/sessions/{seg}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let files: serde_json::Value = client
        .get(format!("{url}/api/sessions/{seg}/files"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let decisions: serde_json::Value = client
        .get(format!("{url}/api/sessions/{seg}/decisions"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let commits: serde_json::Value = client
        .get(format!("{url}/api/sessions/{seg}/commits"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    if json {
        let harness = detail["harness"].as_str().unwrap_or("claude");
        let resume_command =
            super::session_bundle::resume_hint(harness, session_id, detail["cwd"].as_str());
        let combined = serde_json::json!({
            "session": detail,
            "files": files["files"],
            "decisions": decisions["decisions"],
            "commits": commits["commits"],
            "resume_command": resume_command,
        });
        println!("{}", serde_json::to_string_pretty(&combined)?);
        return Ok(());
    }

    let name = detail["display_name"]
        .as_str()
        .or_else(|| detail["session_id"].as_str())
        .unwrap_or("-");
    println!("Resuming: {name}");
    if let Some(goal) = detail["first_user_prompt"].as_str() {
        println!("Goal: {goal}");
    }
    let mut where_ = Vec::new();
    if let Some(c) = detail["cwd"].as_str() {
        where_.push(c.to_string());
    }
    if let Some(b) = detail["git_branch"].as_str() {
        where_.push(format!("branch {b}"));
    }
    if !where_.is_empty() {
        println!("{}", where_.join(" · "));
    }
    if let Some(arr) = files["files"].as_array() {
        let edited: Vec<&serde_json::Value> = arr
            .iter()
            .filter(|f| f["action"].as_str() != Some("read"))
            .collect();
        if !edited.is_empty() {
            println!("\nEdited ({}):", edited.len());
            for f in edited.iter().take(30) {
                println!("  - {}", f["path"].as_str().unwrap_or("-"));
            }
        }
    }
    if let Some(arr) = decisions["decisions"].as_array() {
        if !arr.is_empty() {
            println!("\nDecisions:");
            for d in arr {
                if d["kind"].as_str() == Some("plan") {
                    println!("  - plan approved");
                } else {
                    println!(
                        "  - {} → {}",
                        d["prompt"].as_str().unwrap_or("-"),
                        d["answer"].as_str().unwrap_or("?"),
                    );
                }
            }
        }
    }
    if let Some(arr) = commits["commits"].as_array() {
        if !arr.is_empty() {
            println!("\nCommitted ({}):", arr.len());
            for c in arr {
                println!(
                    "  - {} {}",
                    c["sha"].as_str().unwrap_or("-------"),
                    c["subject"]
                        .as_str()
                        .unwrap_or(c["kind"].as_str().unwrap_or("")),
                );
            }
        }
    }
    // W4/R13/O4 — the resume hint is harness-aware: `SessionOut.harness`
    // rides the wire (V0029) for every online read, so `resume` never has to
    // guess or re-parse the transcript the way the offline export/rehydrate
    // verbs do.
    let harness = detail["harness"].as_str().unwrap_or("claude");
    let cwd = detail["cwd"].as_str();
    println!(
        "\nResume: {}",
        super::session_bundle::resume_hint(harness, session_id, cwd)
    );
    Ok(())
}

/// `kb why <file>` (R2) — the past sessions that touched a file and the
/// reasoning (prompt / decisions / commits) that produced it. Episodic-memory
/// retrieval, recall-shaped output.
pub async fn why(path: &str, daemon: Option<&str>, bearer: Option<&str>, json: bool) -> Result<()> {
    let url = require_daemon(daemon, bearer).await?;
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let body: serde_json::Value = client
        .get(format!("{url}/api/why"))
        .query(&[("path", path)])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let empty: Vec<serde_json::Value> = Vec::new();
    let sessions = body["sessions"].as_array().unwrap_or(&empty);
    let basename = body["basename"].as_str().unwrap_or(path);
    if sessions.is_empty() {
        println!("no captured session has touched {basename}");
        return Ok(());
    }
    println!(
        "why {basename} — {n} session(s), newest first",
        n = sessions.len()
    );
    for s in sessions {
        let name = s["display_name"].as_str().unwrap_or("-");
        let kb = s["kb"].as_str().unwrap_or("-");
        let started = s["started_at"].as_i64().unwrap_or(0);
        let action = s["action"].as_str().unwrap_or("");
        let fuzzy = s["confidence"].as_str() == Some("fuzzy");
        let badge = if fuzzy {
            "  [~basename match]"
        } else {
            "  [exact]"
        };
        // W4/R13/Proposal-3 — harness + project header (quiet default-claude
        // policy: only printed when non-empty/non-default).
        let harness = s["harness"].as_str().unwrap_or("claude");
        let harness_bit = if harness != "claude" {
            format!(" · {harness}")
        } else {
            String::new()
        };
        let project_bit = s["project_key"]
            .as_str()
            .filter(|p| !p.is_empty())
            .map(|p| format!(" · {p}"))
            .unwrap_or_default();
        println!();
        println!(
            "◆ {ts}  [{kb}]{project_bit}{harness_bit}  {action:<5}  {name}{badge}",
            ts = format_started(started),
            name = truncate(name, 60),
        );
        if let Some(p) = s["first_user_prompt"].as_str().filter(|p| !p.is_empty()) {
            println!("  asked   {}", truncate(p, 74));
        }
        // R3/W4 — the closure: the single highest-value line in the redesign.
        if let Some(closed) = s["outcome"].as_str().filter(|p| !p.is_empty()) {
            println!("  ended   {}", truncate(closed, 74));
        }
        if let Some(decisions) = s["decisions"].as_array() {
            let bits: Vec<String> = decisions
                .iter()
                .map(|d| {
                    let prompt = d["prompt"].as_str().unwrap_or("");
                    match d["answer"].as_str().filter(|a| !a.is_empty()) {
                        Some(a) => format!("{} → {}", truncate(prompt, 30), truncate(a, 16)),
                        None => truncate(prompt, 40),
                    }
                })
                .collect();
            if !bits.is_empty() {
                println!("  decided {}", bits.join(" · "));
            }
        }
        // W4/R13 — commit resolution status (V0025 `resolved`).
        if let Some(commits) = s["commits"].as_array() {
            if let Some(first) = commits.first() {
                let sha = first["sha"].as_str().unwrap_or("-------");
                let subject = first["subject"].as_str().unwrap_or("");
                let resolved = if first["resolved"].as_bool().unwrap_or(false) {
                    "✓resolved"
                } else {
                    "unresolved"
                };
                let more = if commits.len() > 1 {
                    format!(" (+{})", commits.len() - 1)
                } else {
                    String::new()
                };
                println!("  commits {sha} {} {resolved}{more}", truncate(subject, 46));
            }
        }
        // W4/R13 — a deterministic drill-down hint (discoverable, zero magic).
        let sid8: String = s["session_id"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(8)
            .collect();
        if !sid8.is_empty() {
            println!("  next    kb sessions read {sid8}");
        }
    }
    Ok(())
}

/// `kb recollect <q>` (R3) — semantic "has this been done?" over past
/// sessions, recall-shaped output with the recency/staleness/error/commit
/// signals surfaced so the agent can weigh each match. `--raw` prints the
/// matched digest text per hit (the true rank surface, R1) rather than the
/// transcript — fetched via the doc body (digest REPLACES body, #11).
#[allow(clippy::too_many_arguments)]
pub async fn recollect(
    query: Option<&str>,
    similar_to: Option<&str>,
    folder: Option<&str>,
    project: Option<&str>,
    since: Option<&str>,
    limit: u32,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
    raw: bool,
) -> Result<()> {
    // R7 — exactly one of query / --similar-to (clap already blocks "both").
    let query = query.map(str::trim).filter(|s| !s.is_empty());
    if query.is_none() && similar_to.is_none() {
        return Err(anyhow!("pass a query or --similar-to <session_id>"));
    }
    let url = require_daemon(daemon, bearer).await?;
    let client = http::client_with_timeout_and_bearer(15, bearer)?;
    let limit_s = limit.to_string();
    let mut params: Vec<(&str, &str)> = vec![("limit", &limit_s)];
    match (query, similar_to) {
        (Some(q), _) => params.push(("q", q)),
        (None, Some(sid)) => params.push(("similar_to", sid)),
        _ => unreachable!("validated above"),
    }
    if let Some(f) = folder {
        params.push(("folder", f));
    }
    // W4/W3.A — the P1-derived-key axis (registry id or raw project_key),
    // composes (AND) with --folder exactly as the server's `?project=` does.
    if let Some(p) = project {
        params.push(("project", p));
    }
    if let Some(s) = since {
        params.push(("since", s));
    }
    let body: serde_json::Value = client
        .get(format!("{url}/api/sessions/recollect"))
        .query(&params)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let empty: Vec<serde_json::Value> = Vec::new();
    let rows = body["sessions"].as_array().unwrap_or(&empty);
    let ms = body["ms"].as_u64().unwrap_or(0);
    if rows.is_empty() {
        let label = query
            .map(|q| format!("“{q}”"))
            .unwrap_or_else(|| format!("session {}", similar_to.unwrap_or("?")));
        println!("no past session matches {label} — try a broader query");
        return Ok(());
    }
    println!("{} session(s) in {ms} ms", rows.len());
    for r in rows {
        let sid = r["session_id"].as_str().unwrap_or("-");
        let sid8: String = sid.chars().take(8).collect();
        let name = r["display_name"].as_str().unwrap_or("-");
        let kb = r["kb"].as_str().unwrap_or("-");
        let folder_label = r["folder"].as_str().unwrap_or("");
        let score = r["score"].as_f64().unwrap_or(0.0);
        let age = r["age_days"].as_i64().unwrap_or(0);
        let stale = r["stale"].as_bool().unwrap_or(false);
        let errs = r["error_count"].as_i64().unwrap_or(0);
        let commits = r["commit_count"].as_i64().unwrap_or(0);
        let harness = r["harness"].as_str().unwrap_or("claude");
        let where_ = match (folder_label.is_empty(), harness != "claude") {
            (false, true) => format!("[{kb}/{folder_label}] · {harness}"),
            (false, false) => format!("[{kb}/{folder_label}]"),
            (true, true) => format!("[{kb}] · {harness}"),
            (true, false) => format!("[{kb}]"),
        };
        // R3 discipline: staleness/success are SURFACED signals ONLY — never
        // reorder, never drop; the server's rank order is printed verbatim.
        let stale_badge = if stale { "  ⚠ stale" } else { "" };
        println!();
        println!(
            "◆ {score:.2}  {sid8}  {where_}  {name}",
            name = truncate(name, 50),
        );
        println!("      {age}d old  {errs} err  {commits} commits{stale_badge}");
        if let Some(p) = r["first_user_prompt"].as_str().filter(|p| !p.is_empty()) {
            println!("      asked  {}", truncate(p, 70));
        }
        if let Some(closed) = r["outcome"].as_str().filter(|p| !p.is_empty()) {
            println!("      ended  {}", truncate(closed, 70));
        }
        // W4/R13/Proposal-3 `--raw` — `RecollectSessionOut.summary` IS the
        // digest excerpt (D1-B: title · first-prompt · closed) — the true
        // rank surface (R1), already on THIS response (zero extra fetch;
        // "the cheapest existing route" per the design is this one).
        if raw {
            match r["summary"].as_str().filter(|s| !s.is_empty()) {
                Some(digest) => println!("      raw    {digest}"),
                None => println!("      raw    (no digest excerpt recorded)"),
            }
        }
    }
    Ok(())
}

// ---- W0.6 — kb-code Wave 0 probes: by-commit, provenance-report, why-line ---

/// `kb sessions by-commit <sha>` — the sha→session reverse lookup
/// (`GET /api/sessions/by-commit?sha=`): every session_commits row across the
/// fleet whose sha/sha_full starts with the given full or short sha.
pub async fn by_commit(
    sha: &str,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let url = require_daemon(daemon, bearer).await?;
    let client = http::client_with_timeout_and_bearer(10, bearer)?;
    let body: serde_json::Value = client
        .get(format!("{url}/api/sessions/by-commit"))
        .query(&[("sha", sha)])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let empty: Vec<serde_json::Value> = Vec::new();
    let matches = body["matches"].as_array().unwrap_or(&empty);
    if matches.is_empty() {
        println!("no session recorded a commit matching {sha}");
        return Ok(());
    }
    println!("{} match(es) for {sha}:", matches.len());
    for m in matches {
        let kb = m["kb"].as_str().unwrap_or("-");
        let name = m["display_name"].as_str().unwrap_or("-");
        let started = m["started_at"].as_i64().unwrap_or(0);
        let kind = m["kind"].as_str().unwrap_or("commit");
        let resolved = m["resolved"].as_bool().unwrap_or(false);
        let sha_disp = m["sha_full"]
            .as_str()
            .or_else(|| m["sha"].as_str())
            .unwrap_or("-------");
        let subject = m["subject"].as_str().unwrap_or("");
        println!();
        println!(
            "  {ts}  [{kb}]  {kind:<6} {sha_disp}{res}",
            ts = format_started(started),
            res = if resolved { "" } else { "  (unresolved)" },
        );
        println!("      {}", truncate(subject, 74));
        println!("      session: {}", truncate(name, 64));
        if let Some(trailers) = m["trailers"].as_array() {
            for t in trailers.iter().filter_map(|t| t.as_str()) {
                println!("      trailer: {t}");
            }
        }
    }
    Ok(())
}

/// One repo commit as parsed from `git log`, aligned with its own
/// `Kb-Session` trailer (a second, position-aligned `git log` pass).
#[derive(Debug, Clone)]
struct RepoCommit {
    full_sha: String,
    // `short_sha`/`date`/`subject` round out the parse (and are asserted by
    // `walk_repo_commits_pairs_trailer_with_the_right_commit`) but aren't
    // consumed by today's aggregate-only report — `#[allow(dead_code)]`
    // rather than dropping them, since a future per-commit `--json` listing
    // (or `why-line`-style debugging) wants them already parsed.
    #[allow(dead_code)]
    short_sha: String,
    /// `--date=short` display date (`YYYY-MM-DD`).
    #[allow(dead_code)]
    date: String,
    /// Author unix timestamp — `%ad`'s short form isn't comparable to the
    /// capture-era boundary, so `provenance_report` also asks for `%at`.
    at_unix: i64,
    #[allow(dead_code)]
    subject: String,
    has_trailer: bool,
}

/// Walk `git -C <repo> log --format='%H %h %ad %at %s' --date=short` (newest
/// first, from HEAD) plus a second, position-aligned pass over
/// `--format='%(trailers:key=Kb-Session,valueonly,unfold)'` for the trailer
/// check. The two passes rely on `git log`'s default (reverse-chronological)
/// order being identical run-to-run against the same ref state — true unless
/// a commit lands on the repo between the two invocations (a probe-tool-scale
/// risk, not a hard guarantee).
async fn walk_repo_commits(repo: &Path) -> Result<Vec<RepoCommit>> {
    let log_out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["log", "--format=%H %h %ad %at %s", "--date=short"])
        .output()
        .await
        .context("run git log")?;
    if !log_out.status.success() {
        bail!(
            "git log failed: {}",
            String::from_utf8_lossy(&log_out.stderr).trim()
        );
    }
    let trailer_out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "log",
            "--format=%(trailers:key=Kb-Session,valueonly,unfold)",
        ])
        .output()
        .await
        .context("run git log (trailers)")?;
    if !trailer_out.status.success() {
        bail!(
            "git log (trailers) failed: {}",
            String::from_utf8_lossy(&trailer_out.stderr).trim()
        );
    }
    let log_text = String::from_utf8_lossy(&log_out.stdout);
    let trailer_text = String::from_utf8_lossy(&trailer_out.stdout);
    let trailer_lines: Vec<&str> = trailer_text.lines().collect();

    let mut commits = Vec::new();
    for (i, line) in log_text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.splitn(5, ' ');
        let (Some(full), Some(short), Some(date), Some(at)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let subject = parts.next().unwrap_or("").to_string();
        let has_trailer = trailer_lines.get(i).is_some_and(|t| !t.trim().is_empty());
        commits.push(RepoCommit {
            full_sha: full.to_ascii_lowercase(),
            short_sha: short.to_ascii_lowercase(),
            date: date.to_string(),
            at_unix: at.parse().unwrap_or(0),
            subject,
            has_trailer,
        });
    }
    Ok(commits)
}

/// One `commit-map` bulk-feed row, trimmed to what `provenance_report` needs.
#[derive(Debug, Clone)]
struct RecordedRow {
    sha: Option<String>,
    sha_full: Option<String>,
    started_at: i64,
}

/// Fetch the ENTIRE `GET /api/sessions/commit-map` feed, paging on
/// `next_offset` until a short page. Preferred over `by-commit` for
/// `provenance_report` (one bulk fetch vs. one round trip per commit).
async fn fetch_all_recorded_commits(
    url: &str,
    client: &reqwest::Client,
) -> Result<Vec<RecordedRow>> {
    let mut all = Vec::new();
    let mut offset: u64 = 0;
    const PAGE_LIMIT: u64 = 5000; // the server's MAX for /commit-map
    loop {
        let body: serde_json::Value = client
            .get(format!("{url}/api/sessions/commit-map"))
            .query(&[
                ("limit", PAGE_LIMIT.to_string()),
                ("offset", offset.to_string()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let empty: Vec<serde_json::Value> = Vec::new();
        let page = body["commits"].as_array().unwrap_or(&empty);
        let got = page.len();
        for r in page {
            all.push(RecordedRow {
                sha: r["sha"].as_str().map(|s| s.to_ascii_lowercase()),
                sha_full: r["sha_full"].as_str().map(|s| s.to_ascii_lowercase()),
                started_at: r["started_at"].as_i64().unwrap_or(0),
            });
        }
        match body["next_offset"].as_u64() {
            Some(n) if got > 0 => offset = n,
            _ => break,
        }
    }
    Ok(all)
}

/// The FIRST-matching-bucket classification for one repo commit (Wave 0
/// W0.6's wedge instrument). Pure + unit-testable: no git/HTTP inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProvenanceBucket {
    /// The commit carries its own `Kb-Session` trailer — exact join.
    Trailer,
    /// Not trailer-tagged, but the daemon's commit-map has a row whose
    /// sha/sha_full resolves to this commit.
    Recorded,
    /// Older than the earliest session capture the daemon knows about (no
    /// trailer, not recorded — kb-memory wasn't running yet).
    PreCapture,
    /// Inside the capture era, no trailer, not recorded.
    NonSession,
}

impl ProvenanceBucket {
    fn as_str(self) -> &'static str {
        match self {
            Self::Trailer => "trailer",
            Self::Recorded => "recorded",
            Self::PreCapture => "pre-capture",
            Self::NonSession => "non-session",
        }
    }
}

/// Classify one commit into the first matching bucket. `capture_era_start`
/// is `None` when the daemon holds no recorded commits at all (then
/// `pre-capture` can never fire — every non-trailer/non-recorded commit
/// falls to `non-session`).
fn classify_commit(
    has_trailer: bool,
    is_recorded: bool,
    commit_at_unix: i64,
    capture_era_start: Option<i64>,
) -> ProvenanceBucket {
    if has_trailer {
        ProvenanceBucket::Trailer
    } else if is_recorded {
        ProvenanceBucket::Recorded
    } else if capture_era_start.is_some_and(|start| commit_at_unix < start) {
        ProvenanceBucket::PreCapture
    } else {
        ProvenanceBucket::NonSession
    }
}

/// Does `full_sha` match any recorded row (`row.sha_full == full_sha`, or
/// `full_sha` starts with a recorded short `row.sha`)?
fn commit_is_recorded(
    full_sha: &str,
    recorded_full: &HashSet<String>,
    recorded_shorts: &[String],
) -> bool {
    recorded_full.contains(full_sha)
        || recorded_shorts
            .iter()
            .any(|s| full_sha.starts_with(s.as_str()))
}

/// The reverse direction — does a recorded row match any repo commit? (its
/// `sha_full` is one of `repo_full`, or its short `sha` is a prefix of one).
/// Feeds the orphan pass ("rebased-or-squashed").
fn row_matches_repo(row: &RecordedRow, repo_full: &HashSet<String>, repo_shas: &[String]) -> bool {
    if let Some(full) = &row.sha_full {
        if repo_full.contains(full) {
            return true;
        }
    }
    if let Some(short) = &row.sha {
        if repo_shas.iter().any(|c| c.starts_with(short.as_str())) {
            return true;
        }
    }
    false
}

/// `kb sessions provenance-report --repo <path>` — the WEDGE INSTRUMENT: for
/// every commit in the repo's history (from HEAD), classify it into the
/// FIRST matching bucket (trailer / recorded / pre-capture / non-session),
/// then a separate orphan pass counts recorded shas that match no commit in
/// the repo (rebased-or-squashed). Probe-grade measurement, not the final
/// wave-3 join — the classification ladder here is what wave-3 will refine.
pub async fn provenance_report(
    repo: &Path,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let url = require_daemon(daemon, bearer).await?;
    let client = http::client_with_timeout_and_bearer(30, bearer)?;

    let commits = walk_repo_commits(repo).await?;
    if commits.is_empty() {
        bail!("no commits found in {} (from HEAD)", repo.display());
    }
    let recorded = fetch_all_recorded_commits(&url, &client).await?;

    let recorded_full: HashSet<String> =
        recorded.iter().filter_map(|r| r.sha_full.clone()).collect();
    let recorded_shorts: Vec<String> = recorded.iter().filter_map(|r| r.sha.clone()).collect();
    let capture_era_start: Option<i64> = recorded.iter().map(|r| r.started_at).min();

    let mut buckets: [usize; 4] = [0; 4]; // trailer, recorded, pre-capture, non-session
    let mut era_buckets: [usize; 4] = [0; 4];
    let mut era_commit_count = 0usize;
    let mut classified: Vec<(&RepoCommit, ProvenanceBucket)> = Vec::with_capacity(commits.len());
    for c in &commits {
        let is_recorded = commit_is_recorded(&c.full_sha, &recorded_full, &recorded_shorts);
        let bucket = classify_commit(c.has_trailer, is_recorded, c.at_unix, capture_era_start);
        buckets[bucket_index(bucket)] += 1;
        if bucket != ProvenanceBucket::PreCapture {
            era_commit_count += 1;
            era_buckets[bucket_index(bucket)] += 1;
        }
        classified.push((c, bucket));
    }

    let repo_full: HashSet<String> = commits.iter().map(|c| c.full_sha.clone()).collect();
    let repo_shas: Vec<String> = commits.iter().map(|c| c.full_sha.clone()).collect();
    let orphan_count = recorded
        .iter()
        .filter(|r| !row_matches_repo(r, &repo_full, &repo_shas))
        .count();

    let total = commits.len();
    let bucket_json = |b: ProvenanceBucket| {
        let n = buckets[bucket_index(b)];
        serde_json::json!({
            "bucket": b.as_str(),
            "count": n,
            "pct": pct(n, total),
        })
    };
    let era_bucket_json = |b: ProvenanceBucket| {
        let n = era_buckets[bucket_index(b)];
        serde_json::json!({
            "bucket": b.as_str(),
            "count": n,
            "pct": pct(n, era_commit_count),
        })
    };
    let all_buckets = [
        ProvenanceBucket::Trailer,
        ProvenanceBucket::Recorded,
        ProvenanceBucket::PreCapture,
        ProvenanceBucket::NonSession,
    ];
    let non_era_buckets = [
        ProvenanceBucket::Trailer,
        ProvenanceBucket::Recorded,
        ProvenanceBucket::NonSession,
    ];

    if json {
        let body = serde_json::json!({
            "repo": repo.display().to_string(),
            "total_commits": total,
            "capture_era_start": capture_era_start,
            "capture_era_commit_count": era_commit_count,
            "buckets": all_buckets.iter().map(|b| bucket_json(*b)).collect::<Vec<_>>(),
            "capture_era_buckets": non_era_buckets.iter().map(|b| era_bucket_json(*b)).collect::<Vec<_>>(),
            "orphan_count": orphan_count,
        });
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    println!("provenance report · {}", repo.display());
    println!("  {total} commit(s) from HEAD");
    match capture_era_start {
        Some(start) => println!(
            "  capture era starts {} ({} commit(s) in-era)",
            format_started(start),
            era_commit_count
        ),
        None => println!("  no recorded session commits — capture era unknown"),
    }
    println!();
    println!("  overall:");
    for b in all_buckets {
        let n = buckets[bucket_index(b)];
        println!("    {:<12} {n:>6}  ({:>5.1}%)", b.as_str(), pct(n, total));
    }
    if era_commit_count > 0 {
        println!();
        println!("  capture-era subset ({era_commit_count} commits, pre-capture excluded):");
        for b in non_era_buckets {
            let n = era_buckets[bucket_index(b)];
            println!(
                "    {:<12} {n:>6}  ({:>5.1}%)",
                b.as_str(),
                pct(n, era_commit_count)
            );
        }
    }
    println!();
    println!("  orphans (recorded, no matching repo commit — rebased/squashed): {orphan_count}");
    Ok(())
}

fn bucket_index(b: ProvenanceBucket) -> usize {
    match b {
        ProvenanceBucket::Trailer => 0,
        ProvenanceBucket::Recorded => 1,
        ProvenanceBucket::PreCapture => 2,
        ProvenanceBucket::NonSession => 3,
    }
}

fn pct(n: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        (n as f64) / (total as f64) * 100.0
    }
}

/// `kb sessions why-line <file>:<line> --repo <path>` — a THIN probe (not the
/// final wave-3 ladder): `git blame` the line → its commit's `Kb-Session`
/// trailer if any → else the `by-commit` daemon lookup → print
/// `{sha, subject, confidence, session_id?, display_name?}`.
/// `confidence` is `"trailer"` (exact, no daemon round trip needed),
/// `"exact"` (the daemon's commit-map matched the blamed sha), or `"none"`.
pub async fn why_line(
    file_line: &str,
    repo: &Path,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let (file, line) = parse_file_line(file_line)?;
    let blame_out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "blame",
            "-L",
            &format!("{line},{line}"),
            "--porcelain",
            "--",
        ])
        .arg(&file)
        .output()
        .await
        .context("run git blame")?;
    if !blame_out.status.success() {
        bail!(
            "git blame failed: {}",
            String::from_utf8_lossy(&blame_out.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&blame_out.stdout);
    let (sha, blame_subject) = parse_blame_porcelain(&stdout)
        .ok_or_else(|| anyhow!("could not parse `git blame` output for {file_line}"))?;
    // The all-zero sha is git's "uncommitted / working-tree" sentinel.
    let is_real_commit = sha.bytes().any(|b| b != b'0');

    let mut confidence = "none";
    let mut session_id: Option<String> = None;
    let mut display_name: Option<String> = None;
    let mut subject = blame_subject;

    if is_real_commit {
        let resolved = kb_core::vcs::resolve_commit(repo, &sha).await;
        if let Some(s) = resolved.subject.clone() {
            subject = Some(s);
        }
        if let Some(kb_sid) = resolved
            .trailers
            .iter()
            .find_map(|t| t.strip_prefix("Kb-Session:").map(str::trim))
        {
            confidence = "trailer";
            session_id = Some(kb_sid.to_string());
        } else if let Ok(url) = require_daemon(daemon, bearer).await {
            let client = http::client_with_timeout_and_bearer(10, bearer)?;
            let lookup_sha = resolved.sha_full.as_deref().unwrap_or(&sha);
            if lookup_sha.len() >= 7 {
                if let Ok(resp) = client
                    .get(format!("{url}/api/sessions/by-commit"))
                    .query(&[("sha", lookup_sha)])
                    .send()
                    .await
                {
                    if let Ok(resp) = resp.error_for_status() {
                        if let Ok(body) = resp.json::<serde_json::Value>().await {
                            if let Some(m) = body["matches"].as_array().and_then(|a| a.first()) {
                                confidence = "exact";
                                session_id = m["session_id"].as_str().map(str::to_string);
                                display_name = m["display_name"].as_str().map(str::to_string);
                            }
                        }
                    }
                }
            }
        }
    }

    if json {
        let body = serde_json::json!({
            "sha": sha,
            "subject": subject,
            "confidence": confidence,
            "session_id": session_id,
            "display_name": display_name,
        });
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!("{file_line}");
    println!(
        "  sha:        {sha}{}",
        if is_real_commit {
            ""
        } else {
            "  (uncommitted)"
        }
    );
    println!("  subject:    {}", subject.as_deref().unwrap_or("-"));
    println!("  confidence: {confidence}  [probe-grade — not the final wave-3 ladder]");
    if let Some(sid) = &session_id {
        println!("  session:    {sid}");
    }
    if let Some(name) = &display_name {
        println!("  session as: {}", truncate(name, 64));
    }
    Ok(())
}

/// Parse `git blame -L N,N --porcelain` output: the first line's leading
/// token is the sha; a `summary <text>` line (when present) is the subject.
fn parse_blame_porcelain(out: &str) -> Option<(String, Option<String>)> {
    let first = out.lines().next()?;
    let sha = first.split_whitespace().next()?.to_ascii_lowercase();
    let subject = out
        .lines()
        .find_map(|l| l.strip_prefix("summary "))
        .map(str::to_string);
    Some((sha, subject))
}

/// Split `<file>:<line>` on the LAST `:` (paths don't contain `:` on the
/// platforms this probe targets — see the module doc / README's WSL2 scope).
fn parse_file_line(spec: &str) -> Result<(String, usize)> {
    let (file, line) = spec
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("expected <file>:<line>, got {spec:?}"))?;
    if file.is_empty() {
        return Err(anyhow!("expected <file>:<line>, got {spec:?}"));
    }
    let line: usize = line
        .parse()
        .map_err(|_| anyhow!("invalid line number in {spec:?}"))?;
    if line == 0 {
        bail!("line number must be >= 1");
    }
    Ok((file.to_string(), line))
}

pub(crate) async fn require_daemon(daemon: Option<&str>, bearer: Option<&str>) -> Result<String> {
    http::detect_daemon(daemon, bearer).await.ok_or_else(|| {
        anyhow!(
            "daemon not reachable{} — start it with `kb daemon`",
            daemon.map(|d| format!(" at {d}")).unwrap_or_default()
        )
    })
}

pub(crate) fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n).collect();
        out.push('…');
        out
    }
}

pub(crate) fn format_started(unix: i64) -> String {
    if unix == 0 {
        return "—".into();
    }
    // Format YYYY-MM-DD HH:MM UTC without pulling in chrono for one
    // call site — same Hinnant civil_from_days inverse the kb_core
    // sessions parser uses.
    let days = unix.div_euclid(86_400);
    let time = unix.rem_euclid(86_400);
    let hour = time / 3600;
    let min = (time % 3600) / 60;
    // civil_from_days (Hinnant). Translated from C++.
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

// --- W3.R-b — `kb sessions replay` -----------------------------------------

/// Format a beat instant as `HH:MM:SS` UTC. Same clock-free arithmetic
/// `format_started` uses (no chrono for one call site).
fn fmt_clock(unix: i64) -> String {
    if unix <= 0 {
        return "--:--:--".into();
    }
    let time = unix.rem_euclid(86_400);
    let (h, m, s) = (time / 3600, (time % 3600) / 60, time % 60);
    format!("{h:02}:{m:02}:{s:02}")
}

/// Format the gap since the previous beat. `""` for the first beat (there is
/// nothing to be relative to); `+27s` / `+2m24s` / `+1h04m` after that.
fn fmt_gap(seq: usize, delta_secs: i64) -> String {
    if seq == 0 {
        return String::new();
    }
    let d = delta_secs.max(0);
    if d < 60 {
        format!("+{d}s")
    } else if d < 3_600 {
        format!("+{}m{:02}s", d / 60, d % 60)
    } else {
        format!("+{}h{:02}m", d / 3_600, (d % 3_600) / 60)
    }
}

/// Collapse a snippet to one deterministic line (a `new_string` hunk is
/// multi-line; a timeline row is not).
fn one_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !space && !out.is_empty() {
                out.push(' ');
                space = true;
            }
        } else {
            out.push(c);
            space = false;
        }
    }
    out.trim_end().to_string()
}

/// What one beat points AT: the resolved source-relative path when the daemon
/// matched it to an artifact, else the raw transcript path verbatim (a beat
/// out of every corpus is rendered as a plain path, never dropped), else the
/// beat's own label.
fn replay_beat_target(b: &serde_json::Value) -> String {
    let beat = &b["beat"];
    let kind = beat["kind"].as_str().unwrap_or("other");
    let mut s = String::new();
    match b["source_relative"]
        .as_str()
        .or_else(|| beat["path"].as_str())
    {
        Some(p) => {
            s.push_str(p);
            if let Some(lr) = beat["line_range"].as_array() {
                if lr.len() == 2 {
                    s.push_str(&format!(
                        " (L{}-L{})",
                        lr[0].as_i64().unwrap_or(0),
                        lr[1].as_i64().unwrap_or(0)
                    ));
                }
            }
            if let Some(h) = b["heading_slug"].as_str() {
                s.push_str(&format!(" #{h}"));
            }
        }
        None => {
            let d = beat["detail"].as_str().unwrap_or("");
            if matches!(kind, "prompt" | "assistant") {
                s.push_str(&format!("\"{}\"", truncate(&one_line(d), 60)));
            } else {
                s.push_str(&one_line(d));
            }
        }
    }
    if let Some(sn) = beat["snippet"].as_str() {
        s.push_str(&format!("  \"{}\"", truncate(&one_line(sn), 48)));
    }
    let count = beat["count"].as_i64().unwrap_or(1);
    if count > 1 {
        s.push_str(&format!(" ×{count}"));
    }
    s
}

/// Render `GET /api/sessions/{sid}/replay` as a deterministic textual
/// timeline: one line per beat, fixed columns, no colour and no terminal-width
/// dependence, so the output is golden-pinnable (the kb-list/1 grammar is
/// pinned this way in two crates) and diffable between runs.
///
/// The `--json` path prints the wire bytes VERBATIM instead, which is what
/// makes "if the SPA can show it, the CLI can print it" enforceable rather
/// than aspirational — both surfaces read the same bytes.
pub fn render_replay(v: &serde_json::Value) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "session   {}\n",
        v["session_id"].as_str().unwrap_or("-")
    ));
    out.push_str(&format!("kb        {}\n", v["kb"].as_str().unwrap_or("-")));
    let empty = Vec::new();
    let beats = v["beats"].as_array().unwrap_or(&empty);
    out.push_str(&format!(
        "beats     {} shown · {} matched · {} in timeline\n",
        beats.len(),
        v["matched"].as_i64().unwrap_or(0),
        v["total_beats"].as_i64().unwrap_or(0),
    ));
    // R7/S6 — only surfaced when non-default, so every existing golden
    // fixture (which never sets `window.from_seq`) renders byte-identically.
    let from_seq = v["window"]["from_seq"].as_i64().unwrap_or(0);
    if from_seq > 0 {
        out.push_str(&format!("window    from beat {from_seq} (--from-seq)\n"));
    }
    let dur = v["duration_secs"].as_i64().unwrap_or(0);
    out.push_str(&format!(
        "span      {} → {} ({}m{:02}s)\n",
        fmt_clock(v["started_at"].as_i64().unwrap_or(0)),
        fmt_clock(v["ended_at"].as_i64().unwrap_or(0)),
        dur / 60,
        dur % 60,
    ));
    if v["scrubbed"].as_bool().unwrap_or(false) {
        out.push_str(&format!(
            "scrubbed  {} redaction(s) applied before the timeline was built\n",
            v["redactions"].as_i64().unwrap_or(0)
        ));
    }
    if v["truncated"].as_bool().unwrap_or(false) {
        out.push_str(&format!(
            "truncated {} beat(s) dropped at the replay cap\n",
            v["dropped"].as_i64().unwrap_or(0)
        ));
    }
    out.push('\n');
    if beats.is_empty() {
        out.push_str("(no beats)\n");
        return out;
    }
    for b in beats {
        let beat = &b["beat"];
        let line = format!(
            "{}  {:<7}{:<10}{}",
            fmt_clock(beat["ts_unix"].as_i64().unwrap_or(0)),
            fmt_gap(
                beat["seq"].as_u64().unwrap_or(0) as usize,
                beat["delta_secs"].as_i64().unwrap_or(0)
            ),
            beat["kind"].as_str().unwrap_or("other"),
            replay_beat_target(b),
        );
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// `kb sessions replay <session_id>` — the session-replay/1 timeline over the
/// daemon's `GET /api/sessions/{sid}/replay`.
pub async fn replay(
    session_id: &str,
    artifact: Option<&str>,
    from_seq: Option<usize>,
    limit: Option<usize>,
    daemon: Option<&str>,
    bearer: Option<&str>,
    json: bool,
) -> Result<()> {
    let url = require_daemon(daemon, bearer).await?;
    // A 33 MB capture is realistic: recover + (optional) scrub + parse is the
    // most expensive read in the sessions surface, so allow more than the
    // 10 s the metadata verbs use.
    let client = http::client_with_timeout_and_bearer(60, bearer)?;
    let mut req = client.get(format!(
        "{url}/api/sessions/{}/replay",
        http::encode_path_segment(session_id)
    ));
    if let Some(a) = artifact {
        req = req.query(&[("artifact", a)]);
    }
    // R7/S6 — the serve-window params: the daemon now computes the FULL
    // timeline and slices it here, so a long session's tail is reachable via
    // `--from-seq` rather than lost at the old 2,000-beat compute cap.
    if let Some(n) = from_seq {
        req = req.query(&[("from_seq", n.to_string())]);
    }
    if let Some(n) = limit {
        req = req.query(&[("limit", n.to_string())]);
    }
    let body = req.send().await?.error_for_status()?.text().await?;
    if json {
        // Verbatim wire bytes — same payload the SPA renders.
        println!("{body}");
        return Ok(());
    }
    let v: serde_json::Value =
        serde_json::from_str(&body).context("replay response was not JSON")?;
    print!("{}", render_replay(&v));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_caps_at_n_with_ellipsis() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hello…");
    }

    #[test]
    fn format_started_renders_known_unix() {
        // 2026-05-24 10:00:00 UTC = 1_779_616_800 (matches the
        // kb_core sessions module's own test fixture).
        assert_eq!(format_started(1_779_616_800), "2026-05-24 10:00Z");
        assert_eq!(format_started(0), "—");
    }

    // --- W0.6 probe unit tests ------------------------------------------

    #[test]
    fn parse_file_line_splits_on_last_colon() {
        assert_eq!(
            parse_file_line("src/main.rs:42").unwrap(),
            ("src/main.rs".to_string(), 42)
        );
        assert!(parse_file_line("no-colon-here").is_err());
        assert!(parse_file_line(":42").is_err(), "empty file component");
        assert!(parse_file_line("f.rs:0").is_err(), "line must be >= 1");
        assert!(parse_file_line("f.rs:nope").is_err());
    }

    #[test]
    fn parse_blame_porcelain_extracts_sha_and_summary() {
        let out = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef 12 12 1\n\
                    author kb-test\n\
                    summary feat: the real subject\n\
                    filename src/main.rs\n\
                    \tlet x = 1;\n";
        let (sha, subject) = parse_blame_porcelain(out).unwrap();
        assert_eq!(sha, "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
        assert_eq!(subject.as_deref(), Some("feat: the real subject"));
    }

    #[test]
    fn parse_blame_porcelain_none_on_empty_input() {
        assert!(parse_blame_porcelain("").is_none());
    }

    #[test]
    fn classify_commit_trailer_wins_over_everything() {
        // A trailer-tagged commit is `trailer` even if it's also recorded
        // and technically pre-capture — trailer is the FIRST-matching rule.
        assert_eq!(
            classify_commit(true, true, 100, Some(200)),
            ProvenanceBucket::Trailer
        );
    }

    #[test]
    fn classify_commit_recorded_beats_pre_capture_and_non_session() {
        assert_eq!(
            classify_commit(false, true, 100, Some(200)),
            ProvenanceBucket::Recorded
        );
    }

    #[test]
    fn classify_commit_pre_capture_only_when_era_known_and_earlier() {
        assert_eq!(
            classify_commit(false, false, 100, Some(200)),
            ProvenanceBucket::PreCapture
        );
        // On-or-after the era start is NOT pre-capture.
        assert_eq!(
            classify_commit(false, false, 200, Some(200)),
            ProvenanceBucket::NonSession
        );
    }

    #[test]
    fn classify_commit_unknown_era_never_yields_pre_capture() {
        // No recorded commits at all → era boundary unknown → every
        // non-trailer/non-recorded commit falls to non-session.
        assert_eq!(
            classify_commit(false, false, 1, None),
            ProvenanceBucket::NonSession
        );
    }

    #[test]
    fn commit_is_recorded_matches_full_and_short_prefix() {
        let full: HashSet<String> = ["aaaa000011112222333344445555666677778888".to_string()]
            .into_iter()
            .collect();
        let shorts = vec!["bbbb000".to_string()];
        assert!(commit_is_recorded(
            "aaaa000011112222333344445555666677778888",
            &full,
            &shorts
        ));
        assert!(commit_is_recorded(
            "bbbb0001111222233334444555566667777888899",
            &full,
            &shorts
        ));
        assert!(!commit_is_recorded(
            "cccc0000000000000000000000000000000000",
            &full,
            &shorts
        ));
    }

    #[test]
    fn row_matches_repo_checks_both_directions() {
        let repo_full: HashSet<String> = ["1111222233334444555566667777888899990000".to_string()]
            .into_iter()
            .collect();
        let repo_shas = vec!["1111222233334444555566667777888899990000".to_string()];
        let matching = RecordedRow {
            sha: Some("1111222".to_string()),
            sha_full: None,
            started_at: 0,
        };
        let orphan = RecordedRow {
            sha: Some("deadbee".to_string()),
            sha_full: None,
            started_at: 0,
        };
        assert!(row_matches_repo(&matching, &repo_full, &repo_shas));
        assert!(!row_matches_repo(&orphan, &repo_full, &repo_shas));
    }

    #[test]
    fn pct_handles_zero_total() {
        assert_eq!(pct(0, 0), 0.0);
        assert_eq!(pct(1, 4), 25.0);
    }

    // --- scratch-git-repo integration (real `git`, present in dev/CI) -----

    fn run_git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.email=test@kb",
                "-c",
                "user.name=kb-test",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }

    #[tokio::test]
    async fn walk_repo_commits_pairs_trailer_with_the_right_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        run_git(&root, &["init", "-q"]);
        std::fs::write(root.join("a.txt"), "one").unwrap();
        run_git(&root, &["add", "a.txt"]);
        run_git(&root, &["commit", "-q", "-m", "plain: no trailer"]);
        std::fs::write(root.join("a.txt"), "two").unwrap();
        run_git(&root, &["add", "a.txt"]);
        run_git(
            &root,
            &["commit", "-q", "-m", "feat: tagged\n\nKb-Session: sess-abc"],
        );

        let commits = walk_repo_commits(&root).await.unwrap();
        assert_eq!(commits.len(), 2);
        // Newest first (git log default order).
        assert!(
            commits[0].has_trailer,
            "the newest commit carries the trailer"
        );
        assert_eq!(commits[0].subject, "feat: tagged");
        assert!(!commits[1].has_trailer, "the older commit has none");
        assert_eq!(commits[1].subject, "plain: no trailer");
        assert!(commits[0].at_unix > 0);
        assert_eq!(commits[0].full_sha.len(), 40);
        assert!(commits[0].full_sha.starts_with(&commits[0].short_sha));
    }

    #[tokio::test]
    async fn walk_repo_commits_errors_on_non_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let err = walk_repo_commits(tmp.path()).await.unwrap_err();
        assert!(err.to_string().contains("git log"));
    }
}

/// W3.R-b — the `kb sessions replay` human renderer is GOLDEN-PINNED, exactly
/// as the kb-list/1 Markdown grammar is pinned in kb-core and kb-cli. The
/// output is a deterministic, colour-free, width-independent timeline: a
/// change to a column or a target rule has to be made deliberately here.
#[cfg(test)]
mod replay_render_tests {
    use super::*;
    use serde_json::json;

    fn wire() -> serde_json::Value {
        json!({
            "grammar": "session-replay/1",
            "session_id": "9f8b7182-d433-4a1e-9c2b-000000000001",
            "kb": "sessions",
            "artifact_id": "aaaabbbbcccc",
            "started_at": 1_753_524_251_i64,
            "ended_at": 1_753_524_715_i64,
            "duration_secs": 464,
            "records": 812,
            "metadata_skipped": 5,
            "out_of_order": 0,
            "collapsed": 3,
            "truncated": false,
            "dropped": 0,
            "total_beats": 4,
            "matched": 4,
            "scrubbed": false,
            "redactions": 0,
            "beats": [
                {
                    "beat": {
                        "seq": 0, "ts_unix": 1_753_524_251_i64, "delta_secs": 0,
                        "kind": "prompt",
                        "detail": "read the sessions recon and tell me what is missing",
                        "count": 1
                    }
                },
                {
                    "beat": {
                        "seq": 1, "ts_unix": 1_753_524_278_i64, "delta_secs": 27,
                        "kind": "read", "detail": "sessions.rs",
                        "path": "/home/u/kb/crates/kb-core/src/sessions.rs",
                        "line_range": [1, 340],
                        "count": 2
                    }
                },
                {
                    "beat": {
                        "seq": 2, "ts_unix": 1_753_524_422_i64, "delta_secs": 144,
                        "kind": "edit", "detail": "biography.ts",
                        "path": "/home/u/kb/web/src/lib/biography.ts",
                        "snippet": "export function\n  assembleBiography(rows)",
                        "count": 1
                    },
                    "kb": "docs",
                    "artifact_id": "0011aabbccdd",
                    "source_relative": "web/lib/biography.ts"
                },
                {
                    "beat": {
                        "seq": 3, "ts_unix": 1_753_524_715_i64, "delta_secs": 293,
                        "kind": "commit",
                        "detail": "873de78 docs: README API canon",
                        "count": 1
                    }
                }
            ]
        })
    }

    #[test]
    fn golden_timeline_render() {
        let got = render_replay(&wire());
        let want = "\
session   9f8b7182-d433-4a1e-9c2b-000000000001
kb        sessions
beats     4 shown · 4 matched · 4 in timeline
span      10:04:11 → 10:11:55 (7m44s)

10:04:11         prompt    \"read the sessions recon and tell me what is missing\"
10:04:38  +27s   read      /home/u/kb/crates/kb-core/src/sessions.rs (L1-L340) ×2
10:07:02  +2m24s edit      web/lib/biography.ts  \"export function assembleBiography(rows)\"
10:11:55  +4m53s commit    873de78 docs: README API canon
";
        assert_eq!(got, want, "\n--- got ---\n{got}\n--- want ---\n{want}");
    }

    /// The heading slug the daemon resolved for a ranged read rides the line —
    /// it is the id the iframe accepts as `kb:scroll-to-id`.
    #[test]
    fn heading_slug_rides_a_resolved_ranged_beat() {
        let mut v = wire();
        v["beats"][1]["source_relative"] = json!("notes/recon.html");
        v["beats"][1]["heading_slug"] = json!("kb-what-is-missing");
        let got = render_replay(&v);
        assert!(
            got.contains("notes/recon.html (L1-L340) #kb-what-is-missing ×2"),
            "{got}"
        );
    }

    /// An out-of-corpus path resolves to nothing and MUST still render, as the
    /// plain path the transcript recorded.
    #[test]
    fn unresolved_path_renders_verbatim() {
        let got = render_replay(&wire());
        assert!(
            got.contains("/home/u/kb/crates/kb-core/src/sessions.rs"),
            "{got}"
        );
    }

    /// Honest counters, honestly surfaced: a scrubbed / truncated response
    /// says so in the header rather than quietly serving a shorter timeline.
    #[test]
    fn scrub_and_truncation_are_surfaced_in_the_header() {
        let mut v = wire();
        v["scrubbed"] = json!(true);
        v["redactions"] = json!(3);
        v["truncated"] = json!(true);
        v["dropped"] = json!(112);
        let got = render_replay(&v);
        assert!(got.contains("scrubbed  3 redaction(s) applied"), "{got}");
        assert!(got.contains("truncated 112 beat(s) dropped"), "{got}");
    }

    #[test]
    fn empty_timeline_says_so() {
        let mut v = wire();
        v["beats"] = json!([]);
        v["matched"] = json!(0);
        v["total_beats"] = json!(0);
        let got = render_replay(&v);
        assert!(
            got.contains("beats     0 shown · 0 matched · 0 in timeline"),
            "{got}"
        );
        assert!(got.ends_with("(no beats)\n"), "{got}");
    }

    #[test]
    fn gap_formatting_covers_seconds_minutes_hours() {
        assert_eq!(
            fmt_gap(0, 999),
            "",
            "the first beat has nothing to be relative to"
        );
        assert_eq!(fmt_gap(1, 0), "+0s");
        assert_eq!(fmt_gap(1, 27), "+27s");
        assert_eq!(fmt_gap(1, 144), "+2m24s");
        assert_eq!(fmt_gap(1, 3_600), "+1h00m");
        assert_eq!(fmt_gap(1, 7_845), "+2h10m");
        assert_eq!(
            fmt_gap(1, -5),
            "+0s",
            "a clamped negative delta never renders as a minus"
        );
    }
}
