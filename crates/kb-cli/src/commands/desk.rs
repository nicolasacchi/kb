//! `kb desk` — ephemeral LLM↔human handoff.
//!
//! Modeled mechanically on [`super::capture`]: multipart POST to the
//! daemon, `client_with_timeout_and_bearer`, `--json` purity, no local
//! write. The daemon owns `handoff/<slug>.<ext>` via the capture engine's
//! stable-name upsert. `ls` rides `GET /api/desk`; `promote` composes
//! relocate + `PATCH …/meta` (no new server write surface).

use anyhow::{bail, Context, Result};
use reqwest::multipart::{Form, Part};
use serde_json::Value;
use std::io::Read;
use std::io::{IsTerminal, Write};

use crate::http::{
    client_with_timeout_and_bearer, encode_path_segment, resolve_artifact_target,
    resolve_default_kb, send_json,
};
use crate::session_marker;

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";

fn base_url(daemon: Option<&str>) -> String {
    daemon
        .unwrap_or(DEFAULT_DAEMON)
        .trim_end_matches('/')
        .to_string()
}

/// Parse a humane duration (`45m` / `24h` / `7d`) into seconds.
pub(crate) fn parse_ttl(raw: &str) -> Result<u64> {
    let s = raw.trim();
    let split = s
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| anyhow::anyhow!("ttl must look like 45m, 24h, or 7d (got {raw:?})"))?;
    if split == 0 {
        bail!("ttl must look like 45m, 24h, or 7d (got {raw:?})");
    }
    let (num, unit) = s.split_at(split);
    let n: u64 = num
        .parse()
        .with_context(|| format!("ttl number in {raw:?}"))?;
    let secs = match unit {
        "m" => n.saturating_mul(60),
        "h" => n.saturating_mul(3600),
        "d" => n.saturating_mul(86400),
        _ => bail!("ttl unit must be m, h, or d (got {raw:?})"),
    };
    if secs == 0 {
        bail!("ttl must be greater than zero");
    }
    Ok(secs)
}

fn build_offer_fields(
    name: &str,
    title: Option<&str>,
    tags: Option<&str>,
    sanitize: bool,
    session: Option<&str>,
    ttl_secs: Option<u64>,
) -> Vec<(&'static str, String)> {
    let mut fields = vec![("name", name.to_string())];
    if let Some(t) = title {
        fields.push(("title", t.to_string()));
    }
    if let Some(t) = tags {
        fields.push(("tags", t.to_string()));
    }
    if sanitize {
        fields.push(("sanitize", "true".to_string()));
    }
    if let Some(s) = session {
        fields.push(("session", s.to_string()));
    }
    if let Some(n) = ttl_secs {
        fields.push(("ttl_secs", n.to_string()));
    }
    fields
}

/// `kb desk offer <FILE|-> --as <slug> …`
#[allow(clippy::too_many_arguments)]
pub async fn offer(
    file: &str,
    slug: &str,
    kb: Option<&str>,
    title: Option<&str>,
    tags: Option<&str>,
    ttl: Option<&str>,
    sanitize: bool,
    open: bool,
    json: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let ttl_secs = match ttl {
        Some(raw) => Some(parse_ttl(raw)?),
        None => None,
    };
    let session = session_marker::read_session_marker();
    let resolved_kb = resolve_default_kb(kb, daemon, bearer).await?;
    let base = base_url(daemon);
    let post_url = format!("{base}/api/kb/{}/desk", encode_path_segment(&resolved_kb));

    let mut form = Form::new();
    for (field, value) in
        build_offer_fields(slug, title, tags, sanitize, session.as_deref(), ttl_secs)
    {
        form = form.text(field, value);
    }

    if file == "-" {
        let mut buf = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buf)
            .context("reading stdin")?;
        form = form.part("files", Part::bytes(buf).file_name(format!("{slug}.md")));
    } else {
        let bytes = std::fs::read(file).with_context(|| format!("reading {file}"))?;
        let fname = std::path::Path::new(file)
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| file.to_string());
        form = form.part("files", Part::bytes(bytes).file_name(fname));
    }

    let client = client_with_timeout_and_bearer(60, bearer)?;
    let body = send_json(client.post(&post_url).multipart(form), "desk offer").await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("?");
    let rel = body
        .get("source_relative")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let kbn = body
        .get("kb")
        .and_then(|v| v.as_str())
        .unwrap_or(&resolved_kb);
    let created = body
        .get("created")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let encoded_rel = rel
        .split('/')
        .map(encode_path_segment)
        .collect::<Vec<_>>()
        .join("/");
    let permalink = format!("{base}/a/{}/{encoded_rel}", encode_path_segment(kbn));
    let verb = if created { "offered" } else { "updated" };
    println!("✓ {verb} {id}  {rel}");
    println!("  {permalink}");
    println!("  kb desk wait --path {rel} --once --timeout 900");

    if open {
        let _ = std::process::Command::new("xdg-open")
            .arg(&permalink)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    Ok(())
}

/// `kb desk update <id|path> <FILE>`
pub async fn update(
    target: &str,
    file: &str,
    kb: Option<&str>,
    json: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (resolved_kb, id) = resolve_artifact_target(kb, target, daemon, bearer).await?;
    let base = base_url(daemon);
    let put_url = format!(
        "{base}/api/kb/{}/artifacts/{}/content",
        encode_path_segment(&resolved_kb),
        encode_path_segment(&id)
    );
    let bytes = std::fs::read(file).with_context(|| format!("reading {file}"))?;
    let fname = std::path::Path::new(file)
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| file.to_string());
    let form = Form::new().part("files", Part::bytes(bytes).file_name(fname));
    let client = client_with_timeout_and_bearer(60, bearer)?;
    let body = send_json(client.put(&put_url).multipart(form), "desk update").await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let rel = body
        .get("source_relative")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let nbytes = body.get("bytes").and_then(|v| v.as_u64()).unwrap_or(0);
    println!("✓ updated {id}  {rel}  ({nbytes} bytes)");
    Ok(())
}

/// `kb desk ls` — `GET /api/desk` (optionally `?kb=`). `--all` drops the kb filter.
pub async fn ls(
    kb: Option<&str>,
    all: bool,
    json: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let base = base_url(daemon);
    let url = if all {
        format!("{base}/api/desk")
    } else {
        let resolved_kb = resolve_default_kb(kb, daemon, bearer).await?;
        format!("{base}/api/desk?kb={}", encode_path_segment(&resolved_kb))
    };
    let client = client_with_timeout_and_bearer(15, bearer)?;
    let body = send_json(client.get(&url), "desk ls").await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    let items = body
        .get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let attention = body.get("attention").and_then(|v| v.as_u64()).unwrap_or(0);

    if all {
        println!(
            "  {:<12} {:<28} {:<14} {:>9} {:<12} READ",
            "KB", "PATH", "ID", "COMMENTS", "UPDATED"
        );
    } else {
        println!(
            "  {:<28} {:<14} {:>9} {:<12} READ",
            "PATH", "ID", "COMMENTS", "UPDATED"
        );
    }
    for r in &items {
        let rel = r
            .get("source_relative")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let id = r.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let open = r.get("comments_open").and_then(|v| v.as_u64()).unwrap_or(0);
        let total = r
            .get("comments_total")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let mtime = r
            .get("updated_unix")
            .and_then(|v| v.as_i64())
            .map(|t| t.to_string())
            .unwrap_or_else(|| "-".into());
        let read = r.get("read_state").and_then(|v| v.as_str()).unwrap_or("-");
        let changed = r
            .get("changed_since_read")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let attn = if read == "never-opened" || changed {
            "*"
        } else {
            " "
        };
        if all {
            let kbn = r.get("kb").and_then(|v| v.as_str()).unwrap_or("?");
            println!(
                "{attn} {:<12} {:<28} {:<14} {:>3}/{:<5} {:<12} {read}",
                truncate(kbn, 12),
                truncate(rel, 28),
                truncate(id, 14),
                open,
                total,
                truncate(&mtime, 12),
            );
        } else {
            println!(
                "{attn} {:<28} {:<14} {:>3}/{:<5} {:<12} {read}",
                truncate(rel, 28),
                truncate(id, 14),
                open,
                total,
                truncate(&mtime, 12),
            );
        }
    }
    if items.is_empty() {
        println!("(no handoff drafts)");
    }
    println!("attention: {attention}");
    Ok(())
}

/// `kb desk promote <path-or-id> --to <dest>` — relocate + drop `draft`.
#[allow(clippy::too_many_arguments)]
pub async fn promote(
    target: &str,
    to: &str,
    category: Option<&str>,
    keep_draft_tag: bool,
    kb: Option<&str>,
    json: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    refuse_promote_dest(to)?;
    let (resolved_kb, id) = resolve_artifact_target(kb, target, daemon, bearer).await?;
    let base = base_url(daemon);
    let client = client_with_timeout_and_bearer(30, bearer)?;

    let doc_url = format!(
        "{base}/api/kb/{}/docs/{}",
        encode_path_segment(&resolved_kb),
        encode_path_segment(&id)
    );
    let doc = send_json(client.get(&doc_url), "desk promote lookup").await?;
    let old_rel = doc
        .get("source_relative")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if !old_rel.starts_with("handoff/") {
        bail!("not a desk artifact");
    }

    let move_url = format!(
        "{base}/api/kb/{}/docs/{}/move",
        encode_path_segment(&resolved_kb),
        encode_path_segment(&id)
    );
    let moved = send_json(
        client
            .post(&move_url)
            .json(&serde_json::json!({ "to": to.trim() })),
        "desk promote move",
    )
    .await?;
    let old_id = moved
        .get("old_id")
        .and_then(|v| v.as_str())
        .unwrap_or(&id)
        .to_string();
    let new_id = moved
        .get("new_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("move response missing new_id"))?
        .to_string();
    let new_rel = moved
        .get("new_source_rel")
        .and_then(|v| v.as_str())
        .unwrap_or(to)
        .to_string();
    let old_rel = moved
        .get("old_source_rel")
        .and_then(|v| v.as_str())
        .unwrap_or(&old_rel)
        .to_string();

    if !keep_draft_tag || category.is_some() {
        let mut patch = serde_json::Map::new();
        if !keep_draft_tag {
            let new_doc_url = format!(
                "{base}/api/kb/{}/docs/{}",
                encode_path_segment(&resolved_kb),
                encode_path_segment(&new_id)
            );
            let new_doc = send_json(client.get(&new_doc_url), "desk promote tags").await?;
            let tags: Vec<String> = new_doc
                .get("tags")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|t| t.as_str())
                        .filter(|t| *t != "draft")
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            patch.insert("tags".into(), serde_json::json!(tags));
        }
        if let Some(cat) = category {
            patch.insert("category".into(), serde_json::json!(cat));
        }
        let meta_url = format!(
            "{base}/api/kb/{}/artifacts/{}/meta",
            encode_path_segment(&resolved_kb),
            encode_path_segment(&new_id)
        );
        let _ = send_json(
            client.patch(&meta_url).json(&Value::Object(patch)),
            "desk promote meta",
        )
        .await?;
    }

    let encoded_rel = new_rel
        .split('/')
        .map(encode_path_segment)
        .collect::<Vec<_>>()
        .join("/");
    let permalink = format!(
        "{base}/a/{}/{encoded_rel}",
        encode_path_segment(&resolved_kb)
    );

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "kb": resolved_kb,
                "old_id": old_id,
                "new_id": new_id,
                "old_source_relative": old_rel,
                "source_relative": new_rel,
                "url": permalink,
            }))?
        );
        return Ok(());
    }
    println!("promoted {old_rel} → {new_rel} (id {old_id} → {new_id})");
    Ok(())
}

fn refuse_promote_dest(to: &str) -> Result<()> {
    let to = to.trim();
    if to.is_empty() {
        bail!("--to must not be empty");
    }
    if to == "handoff" || to.starts_with("handoff/") {
        bail!("--to must not start with handoff/ (promote means leaving the desk)");
    }
    Ok(())
}

async fn comment_counts(
    base: &str,
    kb: &str,
    id: &str,
    bearer: Option<&str>,
) -> Result<(u64, u64)> {
    if id.is_empty() {
        return Ok((0, 0));
    }
    let url = format!(
        "{base}/api/kb/{}/review/{}",
        encode_path_segment(kb),
        encode_path_segment(id)
    );
    let client = client_with_timeout_and_bearer(5, bearer)?;
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if resp.status().as_u16() == 404 {
        return Ok((0, 0));
    }
    let body: Value = resp
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .json()
        .await?;
    Ok(count_comments(&body))
}

pub(crate) fn count_comments(review: &Value) -> (u64, u64) {
    let comments = match review.get("comments").and_then(|v| v.as_array()) {
        Some(c) => c,
        None => return (0, 0),
    };
    let total = comments.len() as u64;
    let open = comments
        .iter()
        .filter(|c| c.get("status").and_then(|s| s.as_str()) == Some("open"))
        .count() as u64;
    (open, total)
}

/// `kb desk wait` — thin delegation to comments-watch.
#[allow(clippy::too_many_arguments)]
pub async fn wait(
    path: Option<&str>,
    once: bool,
    timeout: Option<u64>,
    json: bool,
    kb: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let path = path.unwrap_or("handoff");
    crate::commands::comments_watch::run(kb, path, json, once, timeout, false, daemon, bearer).await
}

/// `kb desk expire <id|path>`
#[allow(clippy::too_many_arguments)]
pub async fn expire(
    target: &str,
    kb: Option<&str>,
    force: bool,
    yes: bool,
    json: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (resolved_kb, id) = resolve_artifact_target(kb, target, daemon, bearer).await?;
    let base = base_url(daemon);
    let (open, _total) = comment_counts(&base, &resolved_kb, &id, bearer).await?;
    if open > 0 && !force {
        bail!("refusing to expire {id}: {open} open comment(s) — pass --force to override");
    }
    if !confirm_expire(yes, json)? {
        bail!("aborted");
    }
    let url = format!(
        "{base}/api/kb/{}/artifacts/{}?purge=true",
        encode_path_segment(&resolved_kb),
        encode_path_segment(&id)
    );
    let client = client_with_timeout_and_bearer(15, bearer)?;
    let body = send_json(client.delete(&url), "desk expire").await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!("✓ expired {id}");
    Ok(())
}

fn confirm_expire(yes: bool, json: bool) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    if json || !std::io::stdin().is_terminal() {
        bail!("refusing to expire without --yes (non-interactive / --json)");
    }
    eprint!("Expire this desk artifact (hard delete source + index)? [y/N] ");
    let _ = std::io::stderr().flush();
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf)?;
    Ok(matches!(buf.trim(), "y" | "Y" | "yes" | "YES"))
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ttl_humane_durations() {
        assert_eq!(parse_ttl("45m").unwrap(), 45 * 60);
        assert_eq!(parse_ttl("24h").unwrap(), 24 * 3600);
        assert_eq!(parse_ttl("7d").unwrap(), 7 * 86400);
        assert!(parse_ttl("0m").is_err());
        assert!(parse_ttl("12").is_err());
        assert!(parse_ttl("5w").is_err());
        assert!(parse_ttl("").is_err());
        assert!(parse_ttl("m45").is_err());
    }

    #[test]
    fn offer_fields_include_session_and_ttl() {
        let f = build_offer_fields(
            "brief",
            Some("T"),
            Some("a,b"),
            true,
            Some("sess-1"),
            Some(3600),
        );
        assert!(f.contains(&("name", "brief".into())));
        assert!(f.contains(&("session", "sess-1".into())));
        assert!(f.contains(&("ttl_secs", "3600".into())));
        assert!(f.contains(&("sanitize", "true".into())));
        assert!(f.contains(&("title", "T".into())));
        assert!(f.contains(&("tags", "a,b".into())));
    }

    #[test]
    fn offer_fields_omit_absent_session() {
        let f = build_offer_fields("brief", None, None, false, None, None);
        assert_eq!(f, vec![("name", "brief".into())]);
    }

    #[test]
    fn count_comments_open_vs_total() {
        let review = serde_json::json!({
            "comments": [
                {"id": "c_1", "status": "open"},
                {"id": "c_2", "status": "resolved"},
                {"id": "c_3", "status": "open"}
            ]
        });
        assert_eq!(count_comments(&review), (2, 3));
        assert_eq!(count_comments(&serde_json::json!({})), (0, 0));
    }

    #[test]
    fn refuse_promote_dest_rejects_handoff_and_empty() {
        assert!(refuse_promote_dest("notes/x.md").is_ok());
        assert!(refuse_promote_dest("handoff/x.md").is_err());
        assert!(refuse_promote_dest("handoff").is_err());
        assert!(refuse_promote_dest("  ").is_err());
        assert!(refuse_promote_dest("").is_err());
        let err = refuse_promote_dest("handoff/x.md").unwrap_err().to_string();
        assert!(err.contains("handoff/"), "{err}");
    }
}
