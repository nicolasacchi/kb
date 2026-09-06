//! `kb notes {list,show,new,edit,check,uncheck,append,done,archive,rm}` —
//! free-standing notes / todo-lists (N-track). Every verb talks to the
//! daemon over HTTP, mirroring `kb list` / `kb comments`. A note is an
//! ordinary Markdown artifact (`kb-category=note`), so `kb comments` already
//! works on a note's id — there's no separate notes-comment verb.
//!
//! `<target>` on the per-note verbs is a 12-hex id, a source-relative path,
//! or a unique filename/suffix — resolved via `/api/kb/{kb}/lookup`. `--kb`
//! is optional when only one kb is configured.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::io::Read;

use crate::http::{
    client_with_timeout_and_bearer, encode_path_segment, get_lookup, resolve_default_kb,
};
use crate::session_marker::{read_session_marker, stamp_kb_session};

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";

fn base_url(daemon: Option<&str>) -> String {
    daemon
        .unwrap_or(DEFAULT_DAEMON)
        .trim_end_matches('/')
        .to_string()
}

/// POST/PATCH/DELETE helper: send, map a non-2xx into a clear error carrying
/// the daemon's problem+json `detail`. Returns the parsed body (or Null).
async fn send_json(req: reqwest::RequestBuilder, what: &str) -> Result<Value> {
    let resp = req.send().await.with_context(|| what.to_string())?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("detail").and_then(|d| d.as_str()).map(str::to_string))
            .unwrap_or(text);
        anyhow::bail!("{what} failed: HTTP {status} — {detail}");
    }
    Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// Resolve `<target>` (id / path / suffix) + optional `--kb` to a concrete
/// `(kb_name, note_id)` via `/api/kb/{kb}/lookup`.
async fn resolve_note(
    kb: Option<&str>,
    target: &str,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<(String, String)> {
    let resolved_kb = resolve_default_kb(kb, daemon, bearer).await?;
    let body = get_lookup(daemon, &resolved_kb, target, bearer).await?;
    match body
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
    {
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
                "--target {target:?} matched {} notes; pick one:\n",
                candidates.len()
            );
            for c in candidates {
                let id = c.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                let rel = c
                    .get("source_relative")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                msg.push_str(&format!("  {id}  {rel}\n"));
            }
            anyhow::bail!(msg)
        }
        "not_found" => anyhow::bail!("{target:?} matched no note in {resolved_kb}"),
        other => anyhow::bail!("lookup returned unknown kind {other:?}: {body}"),
    }
}

fn read_stdin() -> Result<String> {
    let mut s = String::new();
    std::io::stdin()
        .read_to_string(&mut s)
        .context("read body from stdin")?;
    Ok(s)
}

fn scope_label(folder: &str) -> String {
    if folder.is_empty() {
        "(root)".to_string()
    } else {
        folder.to_string()
    }
}

/// `kb notes list [--kb][--folder][--status][--json]`.
pub async fn list(
    kb: Option<&str>,
    folder: Option<&str>,
    status: Option<&str>,
    json_out: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let base = base_url(daemon);
    let url = match kb {
        Some(k) => format!("{base}/api/kb/{}/notes", encode_path_segment(k)),
        None => format!("{base}/api/notes"),
    };
    let mut query: Vec<(&str, &str)> = Vec::new();
    if let Some(f) = folder {
        query.push(("folder", f));
    }
    if let Some(s) = status {
        query.push(("status", s));
    }
    let client = client_with_timeout_and_bearer(10, bearer)?;
    let body: Value = client
        .get(&url)
        .query(&query)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()?
        .json()
        .await
        .with_context(|| format!("parse JSON from {url}"))?;

    if json_out {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let notes = body
        .get("notes")
        .and_then(|n| n.as_array())
        .cloned()
        .unwrap_or_default();
    if notes.is_empty() {
        println!("no notes");
        return Ok(());
    }
    for n in &notes {
        let id = n.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let kbn = n.get("kb").and_then(|v| v.as_str()).unwrap_or("");
        let scope = scope_label(n.get("folder").and_then(|v| v.as_str()).unwrap_or(""));
        let title = n.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let done = n.get("task_done").and_then(|v| v.as_u64()).unwrap_or(0);
        let total = n.get("task_total").and_then(|v| v.as_u64()).unwrap_or(0);
        let prog = if total > 0 {
            format!("{done}/{total}")
        } else {
            "—".to_string()
        };
        let pin = if n
            .get("is_notepad")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            "*"
        } else {
            " "
        };
        let kb_col = if kbn.is_empty() {
            String::new()
        } else {
            format!("{kbn}:")
        };
        println!("{pin} {id}  {prog:>5}  {kb_col}{scope:<24}  {title}");
    }
    Ok(())
}

/// `kb notes show <target> [--kb][--json]`.
pub async fn show(
    kb: Option<&str>,
    target: &str,
    json_out: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (kb_name, id) = resolve_note(kb, target, daemon, bearer).await?;
    let base = base_url(daemon);
    let url = format!(
        "{base}/api/kb/{}/notes/{}",
        encode_path_segment(&kb_name),
        encode_path_segment(&id)
    );
    let client = client_with_timeout_and_bearer(10, bearer)?;
    let body: Value = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()?
        .json()
        .await
        .with_context(|| format!("parse JSON from {url}"))?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "# {}",
        body.get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Untitled")
    );
    println!("id:       {id}");
    println!(
        "scope:    {}",
        scope_label(body.get("folder").and_then(|v| v.as_str()).unwrap_or(""))
    );
    if let Some(s) = body.get("status").and_then(|v| v.as_str()) {
        println!("status:   {s}");
    }
    let done = body.get("task_done").and_then(|v| v.as_u64()).unwrap_or(0);
    let total = body.get("task_total").and_then(|v| v.as_u64()).unwrap_or(0);
    if total > 0 {
        println!("tasks:    {done}/{total} done");
    }
    println!(
        "comments: {}",
        body.get("comment_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    );
    let link_n = body
        .get("links")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    if link_n > 0 {
        println!("links:    {link_n} outgoing  (kb notes links {id})");
    }
    println!();
    println!(
        "{}",
        body.get("body_md").and_then(|v| v.as_str()).unwrap_or("")
    );
    Ok(())
}

/// `kb notes new [--kb][--folder][--title][--body|--stdin][--tag…][--status][--notepad][--no-session]`.
#[allow(clippy::too_many_arguments)]
pub async fn new(
    kb: Option<&str>,
    folder: Option<&str>,
    title: Option<&str>,
    body: Option<&str>,
    stdin: bool,
    tags: &[String],
    status: Option<&str>,
    notepad: bool,
    no_session: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let resolved_kb = resolve_default_kb(kb, daemon, bearer).await?;
    let mut body_md = if stdin {
        read_stdin()?
    } else {
        body.unwrap_or("").to_string()
    };
    // CT-A4 — auto-stamp `kb-session` into the note's frontmatter from the
    // current Claude Code session marker (see `session_marker` module doc),
    // the same marker `kb remember` reads. `--no-session` opts out; a
    // caller-supplied `kb-session` in `--body`/`--stdin` frontmatter always
    // wins (never overwritten).
    if !no_session {
        if let Some(sid) = read_session_marker() {
            body_md = stamp_kb_session(&body_md, &sid);
        }
    }
    let mut payload = json!({ "notepad": notepad, "body_md": body_md });
    if let Some(f) = folder {
        payload["folder"] = json!(f);
    }
    if let Some(t) = title {
        payload["title"] = json!(t);
    }
    if !tags.is_empty() {
        payload["tags"] = json!(tags);
    }
    if let Some(s) = status {
        payload["status"] = json!(s);
    }
    let base = base_url(daemon);
    let url = format!("{base}/api/kb/{}/notes", encode_path_segment(&resolved_kb));
    let client = client_with_timeout_and_bearer(10, bearer)?;
    let resp = send_json(client.post(&url).json(&payload), "create note").await?;
    println!(
        "created {}  {}",
        resp.get("id").and_then(|v| v.as_str()).unwrap_or("?"),
        resp.get("path").and_then(|v| v.as_str()).unwrap_or("?")
    );
    Ok(())
}

/// `kb notes edit <target> [--title][--body|--stdin][--status][--tag…]`.
#[allow(clippy::too_many_arguments)]
pub async fn edit(
    kb: Option<&str>,
    target: &str,
    title: Option<&str>,
    body: Option<&str>,
    stdin: bool,
    status: Option<&str>,
    tags: &[String],
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (kb_name, id) = resolve_note(kb, target, daemon, bearer).await?;
    let mut payload = json!({});
    if let Some(t) = title {
        payload["title"] = json!(t);
    }
    if stdin {
        payload["body_md"] = json!(read_stdin()?);
    } else if let Some(b) = body {
        payload["body_md"] = json!(b);
    }
    if let Some(s) = status {
        payload["status"] = json!(s);
    }
    if !tags.is_empty() {
        payload["tags"] = json!(tags);
    }
    if payload.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        anyhow::bail!(
            "nothing to edit — pass at least one of --title/--body/--stdin/--status/--tag"
        );
    }
    let base = base_url(daemon);
    let url = format!(
        "{base}/api/kb/{}/notes/{}",
        encode_path_segment(&kb_name),
        encode_path_segment(&id)
    );
    let client = client_with_timeout_and_bearer(10, bearer)?;
    send_json(client.patch(&url).json(&payload), "edit note").await?;
    println!("updated {id}");
    Ok(())
}

/// `kb notes check <target> --item N [--off]` (and `uncheck`).
pub async fn check(
    kb: Option<&str>,
    target: &str,
    item: usize,
    off: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (kb_name, id) = resolve_note(kb, target, daemon, bearer).await?;
    let base = base_url(daemon);
    let url = format!(
        "{base}/api/kb/{}/notes/{}/toggle",
        encode_path_segment(&kb_name),
        encode_path_segment(&id)
    );
    let client = client_with_timeout_and_bearer(10, bearer)?;
    let r = send_json(
        client
            .post(&url)
            .json(&json!({ "index": item, "on": !off })),
        "toggle task",
    )
    .await?;
    println!(
        "{} task {item}  ({}/{} done)",
        if off { "unchecked" } else { "checked" },
        r.get("task_done").and_then(|v| v.as_u64()).unwrap_or(0),
        r.get("task_total").and_then(|v| v.as_u64()).unwrap_or(0),
    );
    Ok(())
}

/// `kb notes append <target> --item TEXT`.
pub async fn append(
    kb: Option<&str>,
    target: &str,
    text: &str,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (kb_name, id) = resolve_note(kb, target, daemon, bearer).await?;
    let base = base_url(daemon);
    let url = format!(
        "{base}/api/kb/{}/notes/{}/tasks",
        encode_path_segment(&kb_name),
        encode_path_segment(&id)
    );
    let client = client_with_timeout_and_bearer(10, bearer)?;
    let r = send_json(
        client.post(&url).json(&json!({ "text": text })),
        "append task",
    )
    .await?;
    println!(
        "added task  ({}/{} done)",
        r.get("task_done").and_then(|v| v.as_u64()).unwrap_or(0),
        r.get("task_total").and_then(|v| v.as_u64()).unwrap_or(0),
    );
    Ok(())
}

/// `kb notes done|archive <target>` — shortcut for setting `kb-status`.
pub async fn set_status(
    kb: Option<&str>,
    target: &str,
    status_value: &str,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (kb_name, id) = resolve_note(kb, target, daemon, bearer).await?;
    let base = base_url(daemon);
    let url = format!(
        "{base}/api/kb/{}/notes/{}",
        encode_path_segment(&kb_name),
        encode_path_segment(&id)
    );
    let client = client_with_timeout_and_bearer(10, bearer)?;
    send_json(
        client.patch(&url).json(&json!({ "status": status_value })),
        "set status",
    )
    .await?;
    println!("{id} → {status_value}");
    Ok(())
}

/// `kb notes rm <target> --yes`.
pub async fn rm(
    kb: Option<&str>,
    target: &str,
    yes: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    if !yes {
        anyhow::bail!("refusing to delete without --yes");
    }
    let (kb_name, id) = resolve_note(kb, target, daemon, bearer).await?;
    let base = base_url(daemon);
    let url = format!(
        "{base}/api/kb/{}/notes/{}",
        encode_path_segment(&kb_name),
        encode_path_segment(&id)
    );
    let client = client_with_timeout_and_bearer(10, bearer)?;
    send_json(client.delete(&url), "delete note").await?;
    println!("removed {id}");
    Ok(())
}

/// Print a `kb:rel  Title  (note)` line for a resolved/backlink reference.
fn print_ref(prefix: &str, kbn: &str, rel: &str, title: &str, is_note: bool) {
    let kb_col = if kbn.is_empty() {
        String::new()
    } else {
        format!("{kbn}:")
    };
    let tag = if is_note { "  (note)" } else { "" };
    let title = if title.is_empty() {
        String::new()
    } else {
        format!("  {title}")
    };
    println!("  {prefix} {kb_col}{rel}{title}{tag}");
}

/// `kb notes links <target> [--json]` — a note's wikilinks: outgoing `[[…]]`
/// (resolved / dangling / ambiguous) + backlinks (what links here). The
/// connective-tissue view, LLM-friendly and `--json`-parseable.
pub async fn links(
    kb: Option<&str>,
    target: &str,
    json_out: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (kb_name, id) = resolve_note(kb, target, daemon, bearer).await?;
    let base = base_url(daemon);
    let url = format!(
        "{base}/api/kb/{}/notes/{}/links",
        encode_path_segment(&kb_name),
        encode_path_segment(&id)
    );
    let client = client_with_timeout_and_bearer(10, bearer)?;
    let body: Value = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()?
        .json()
        .await
        .with_context(|| format!("parse JSON from {url}"))?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let outgoing = body
        .get("outgoing")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let backlinks = body
        .get("backlinks")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    println!("outgoing ({}):", outgoing.len());
    if outgoing.is_empty() {
        println!("  (none)");
    }
    for l in &outgoing {
        let tgt = l.get("target").and_then(|v| v.as_str()).unwrap_or("?");
        match l.get("state").and_then(|v| v.as_str()).unwrap_or("?") {
            "resolved" => {
                let kbn = l.get("kb").and_then(|v| v.as_str()).unwrap_or("");
                let rel = l
                    .get("source_relative")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let is_note = l.get("is_note").and_then(|v| v.as_bool()).unwrap_or(false);
                print_ref("→", kbn, rel, tgt, is_note);
            }
            "ambiguous" => println!("  ? {tgt}  (ambiguous — matches several)"),
            _ => println!("  ✗ {tgt}  (dangling — no match yet)"),
        }
    }

    println!();
    print_backlink_list(&backlinks);
    Ok(())
}

/// `kb backlinks <target> [--json]` — what references this artifact (any
/// artifact, not just notes). The inbound side of the wikilink graph.
pub async fn backlinks(
    kb: Option<&str>,
    target: &str,
    json_out: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (kb_name, id) = resolve_note(kb, target, daemon, bearer).await?;
    let base = base_url(daemon);
    let url = format!(
        "{base}/api/kb/{}/backlinks/{}",
        encode_path_segment(&kb_name),
        encode_path_segment(&id)
    );
    let client = client_with_timeout_and_bearer(10, bearer)?;
    let body: Value = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()?
        .json()
        .await
        .with_context(|| format!("parse JSON from {url}"))?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let backlinks = body
        .get("backlinks")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    print_backlink_list(&backlinks);
    Ok(())
}

fn print_backlink_list(backlinks: &[Value]) {
    println!("backlinks ({}):", backlinks.len());
    if backlinks.is_empty() {
        println!("  (none)");
    }
    for b in backlinks {
        let title = b.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let kbn = b.get("kb").and_then(|v| v.as_str()).unwrap_or("");
        let rel = b
            .get("source_relative")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let is_note = b.get("is_note").and_then(|v| v.as_bool()).unwrap_or(false);
        print_ref("←", kbn, rel, title, is_note);
    }
}
