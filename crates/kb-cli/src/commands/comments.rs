//! `kb comments {list,show,export,add,reply,resolve,unresolve,edit,delete}`
//! — every verb talks to the daemon over HTTP (R6). No verb reads or
//! writes `.review/*.json` on disk: reads go through `GET /reviews` +
//! `GET /review/{id}` + `POST .../export`, mutations through the R5
//! fine-grained endpoints. This makes the CLI a first-class client at
//! parity with the SPA and lets it work against a remote `--daemon`.
//!
//! `--path <file>` is an alternative to the 12-hex `<artifact_id>` on
//! every verb; it's resolved to an id via `/api/kb/{kb}/lookup`. `--kb`
//! is optional when only one kb is configured.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use crate::http::{client_with_timeout_and_bearer, encode_path_segment};

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";

fn base_url(daemon: Option<&str>) -> String {
    daemon
        .unwrap_or(DEFAULT_DAEMON)
        .trim_end_matches('/')
        .to_string()
}

/// Resolve a `(kb_opt, id_opt, path_opt)` trio to a concrete
/// `(kb_name, artifact_id)` via `/api/kb/{kb}/lookup`. Errors when both
/// id and path are absent, the path is ambiguous, or it doesn't resolve.
async fn resolve_target(
    kb: Option<&str>,
    artifact_id: Option<&str>,
    path: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<(String, String)> {
    match (artifact_id, path) {
        (Some(_), Some(_)) => anyhow::bail!("use either <artifact_id> or --path, not both"),
        (None, None) => anyhow::bail!("missing <artifact_id> (or pass --path <file>)"),
        (Some(id), None) => {
            let resolved_kb = crate::http::resolve_default_kb(kb, daemon, bearer).await?;
            Ok((resolved_kb, id.to_string()))
        }
        (None, Some(p)) => {
            let resolved_kb = crate::http::resolve_default_kb(kb, daemon, bearer).await?;
            let body = crate::http::get_lookup(daemon, &resolved_kb, p, bearer).await?;
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
                        "--path {p:?} matched {} artifacts in {resolved_kb}; pick one:\n",
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
                "not_found" => anyhow::bail!("--path {p:?} matched no artifact in {resolved_kb}"),
                other => anyhow::bail!("lookup returned unknown kind {other:?}: {body}"),
            }
        }
    }
}

/// Like [`resolve_target`], but when `--kb` is omitted, an artifact id is
/// given (no `--path`), and >1 kbs are configured, auto-pick the UNIQUE
/// owning kb instead of erroring. Scans **review-space** first
/// (`/reviews?status=all&artifact_id=` — the id-space `kb comments list`
/// prints, so a copied id resolves in the same space the verb operates in),
/// then falls back to **doc-space** (`/docs/{id}`) for ids with no review
/// file yet (e.g. `kb comments add`, or an id from `kb search`). 0 owners →
/// not-found error; 2+ → ambiguous error. The `--path` and explicit-`--kb`
/// forms delegate verbatim to `resolve_target`.
async fn resolve_target_auto(
    kb: Option<&str>,
    artifact_id: Option<&str>,
    path: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<(String, String)> {
    // Only the (id given, no --kb, no --path) case differs from resolve_target.
    let (Some(id), None, None) = (artifact_id, kb, path) else {
        return resolve_target(kb, artifact_id, path, daemon, bearer).await;
    };
    let names = list_kb_names(daemon, bearer).await?;
    match names.len() {
        0 => anyhow::bail!("no kbs configured"),
        1 => Ok((names.into_iter().next().unwrap(), id.to_string())),
        _ => {
            // Review-space scan (the id `kb comments list` prints). `status=all`
            // is required so a fully-resolved artifact still registers as owned.
            let mut owners = Vec::new();
            for k in &names {
                let rows =
                    fetch_review_rows(daemon, k, bearer, true, None, None, false, None, Some(id))
                        .await?;
                if !rows.is_empty() {
                    owners.push(k.clone());
                }
            }
            // Doc-space fallback when no kb has comments for this id yet.
            if owners.is_empty() {
                for k in &names {
                    if crate::http::kb_has_doc(daemon, k, id, bearer).await? {
                        owners.push(k.clone());
                    }
                }
            }
            match owners.len() {
                1 => Ok((owners.pop().unwrap(), id.to_string())),
                0 => anyhow::bail!(
                    "artifact {id} not found in any of {} configured kbs — pass --kb to pick one",
                    names.len()
                ),
                _ => anyhow::bail!(
                    "artifact {id} exists in multiple kbs ({}); pass --kb to pick one",
                    owners.join(", ")
                ),
            }
        }
    }
}

/// POST/PATCH/DELETE helper: send, then map a non-2xx into a clear error
/// carrying the daemon's problem+json `detail` when present. Returns the
/// parsed JSON body on success (or `Value::Null` for an empty body).
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

/// Multipart upload helper for attachments (Y-track). One `file` part per
/// local path (the daemon re-sniffs the type from the bytes — the filename
/// and client mime are advisory), plus the `author` so the stored Attachment
/// is attributed correctly. 60s timeout — uploads run longer than the 5s
/// JSON verbs. Returns the parsed JSON body (a staged/adopted Attachment
/// array).
async fn upload_multipart(
    url: &str,
    files: &[String],
    author: &str,
    bearer: Option<&str>,
    what: &str,
) -> Result<Value> {
    let mut form = reqwest::multipart::Form::new().text("author", author.to_string());
    for f in files {
        let bytes = std::fs::read(f).with_context(|| format!("reading {f}"))?;
        let name = std::path::Path::new(f)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        form = form.part(
            "file",
            reqwest::multipart::Part::bytes(bytes).file_name(name),
        );
    }
    let client = client_with_timeout_and_bearer(60, bearer)?;
    let resp = client
        .post(url)
        .multipart(form)
        .send()
        .await
        .with_context(|| what.to_string())?;
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

/// The inline markdown token for a staged/adopted attachment JSON object —
/// image syntax for rasters (→ `<img>`), link syntax otherwise. This is the
/// token Claude splices into a comment body to point an attachment out.
fn staged_token(a: &Value) -> String {
    let aid = a["id"].as_str().unwrap_or("?");
    let filename = a["filename"].as_str().unwrap_or("file");
    let ct = a["contentType"].as_str().unwrap_or("");
    if ct.starts_with("image/") {
        format!("![{filename}](attachment:{aid})")
    } else {
        format!("[{filename}](attachment:{aid})")
    }
}

// --- list ------------------------------------------------------------------

/// `kb comments list [--kb NAME] [--all] [--json] [--path FILE]
/// [--author you|claude] [--user NAME] [--stale] [--folder DIR]`.
///
/// Reads `GET /api/kb/{kb}/reviews`. With no `--kb`/`--path` it lists
/// every configured kb (resolved via `/api/kbs`).
#[allow(clippy::too_many_arguments)]
pub async fn list(
    kb_filter: Option<&str>,
    all: bool,
    json_out: bool,
    path: Option<&str>,
    author: Option<&str>,
    user: Option<&str>,
    stale: bool,
    folder: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    // Determine which kb(s) to query, and an optional single-artifact narrow.
    let (kbs, artifact_id): (Vec<String>, Option<String>) = match path {
        Some(p) => {
            let (kb, id) = resolve_target(kb_filter, None, Some(p), daemon, bearer).await?;
            (vec![kb], Some(id))
        }
        None => match kb_filter {
            Some(k) => (vec![k.to_string()], None),
            None => (list_kb_names(daemon, bearer).await?, None),
        },
    };

    let mut rows: Vec<Value> = Vec::new();
    for kb in &kbs {
        rows.extend(
            fetch_review_rows(
                daemon,
                kb,
                bearer,
                all,
                author,
                user,
                stale,
                folder,
                artifact_id.as_deref(),
            )
            .await?,
        );
    }

    if json_out {
        // Carry the friendly one-line label (e.g. `selection:<snippet>`)
        // alongside the structured `anchor` object so consumers needn't
        // re-derive it — parity with the human table's ANCHOR column.
        for r in rows.iter_mut() {
            if let Some(obj) = r.as_object_mut() {
                let label = anchor_label(obj.get("anchor").unwrap_or(&Value::Null));
                obj.insert("anchor_label".into(), Value::String(label));
            }
        }
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    println!(
        "{:<10} {:<14} {:<8} {:<6} {:<24} BODY",
        "KB", "ARTIFACT", "STATUS", "AUTHOR", "ANCHOR"
    );
    for r in &rows {
        let stale_mark = if r["stale"].as_bool().unwrap_or(false) {
            " ⚠"
        } else {
            ""
        };
        println!(
            "{:<10} {:<14} {:<8} {:<6} {:<24} {}{}",
            truncate(r["kb"].as_str().unwrap_or(""), 10),
            truncate(r["artifact_id"].as_str().unwrap_or(""), 14),
            r["status"].as_str().unwrap_or("?"),
            r["author"].as_str().unwrap_or("?"),
            truncate(&anchor_label(&r["anchor"]), 24),
            truncate_one_line(r["body"].as_str().unwrap_or(""), 60),
            stale_mark,
        );
    }
    if rows.is_empty() {
        println!("(no {}comments)", if all { "" } else { "open " });
    }
    Ok(())
}

// --- inbox -----------------------------------------------------------------

/// `kb comments inbox [--kb NAME] [--limit N] [--daemon URL] [--json]`.
///
/// Reads `GET /api/inbox` — the fleet-wide list of OPEN comments across every
/// configured kb, newest activity first. The human table is `kb · artifact ·
/// age · replies · excerpt`; `--json` dumps the raw `{items,total_open}`.
pub async fn inbox(
    kb_filter: Option<&str>,
    limit: Option<u32>,
    json_out: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let url = format!("{}/api/inbox", base_url(daemon));
    let client = client_with_timeout_and_bearer(5, bearer)?;
    let mut q: Vec<(&str, String)> = Vec::new();
    if let Some(k) = kb_filter {
        q.push(("kb", k.to_string()));
    }
    if let Some(n) = limit {
        q.push(("limit", n.to_string()));
    }
    let body: Value = client
        .get(&url)
        .query(&q)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .json()
        .await?;

    if json_out {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    let items = body["items"].as_array().cloned().unwrap_or_default();
    let total = body["total_open"].as_u64().unwrap_or(0);
    if items.is_empty() {
        println!("(no open comments across the fleet)");
        return Ok(());
    }
    println!(
        "{:<10} {:<24} {:<5} {:<5} EXCERPT",
        "KB", "ARTIFACT", "AGE", "REPL"
    );
    for it in &items {
        let artifact = it["title"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| it["source_relative"].as_str())
            .unwrap_or_else(|| it["artifact_id"].as_str().unwrap_or(""));
        let stale_mark = if it["stale"].as_bool().unwrap_or(false) {
            " ⚠"
        } else {
            ""
        };
        let replies = it["reply_count"].as_u64().unwrap_or(0);
        println!(
            "{:<10} {:<24} {:<5} {:<5} {}{}",
            truncate(it["kb"].as_str().unwrap_or(""), 10),
            truncate(artifact, 24),
            fmt_age(it["updated_at"].as_i64().unwrap_or(0)),
            replies,
            truncate_one_line(it["excerpt"].as_str().unwrap_or(""), 60),
            stale_mark,
        );
    }
    let shown = items.len() as u64;
    if shown < total {
        println!("\n{shown} of {total} open comment(s) — pass --limit to see more");
    } else {
        println!("\n{total} open comment(s) across the fleet");
    }
    Ok(())
}

/// Compact "how long ago" for the inbox AGE column, from a unix-secs
/// timestamp: `s`/`m`/`h`/`d`. Coarse on purpose (one column).
fn fmt_age(unix: i64) -> String {
    if unix <= 0 {
        return "-".into();
    }
    let now = chrono::Utc::now().timestamp();
    let d = (now - unix).max(0);
    if d < 60 {
        format!("{d}s")
    } else if d < 3600 {
        format!("{}m", d / 60)
    } else if d < 86_400 {
        format!("{}h", d / 3600)
    } else {
        format!("{}d", d / 86_400)
    }
}

/// GET `/api/kbs` → the configured kb names.
async fn list_kb_names(daemon: Option<&str>, bearer: Option<&str>) -> Result<Vec<String>> {
    let url = format!("{}/api/kbs", base_url(daemon));
    let client = client_with_timeout_and_bearer(5, bearer)?;
    let body: Value = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .json()
        .await?;
    Ok(body
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|k| k["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default())
}

#[allow(clippy::too_many_arguments)]
async fn fetch_review_rows(
    daemon: Option<&str>,
    kb: &str,
    bearer: Option<&str>,
    all: bool,
    author: Option<&str>,
    user: Option<&str>,
    stale: bool,
    folder: Option<&str>,
    artifact_id: Option<&str>,
) -> Result<Vec<Value>> {
    let url = format!(
        "{}/api/kb/{}/reviews",
        base_url(daemon),
        encode_path_segment(kb)
    );
    let client = client_with_timeout_and_bearer(5, bearer)?;
    let mut q: Vec<(&str, String)> = Vec::new();
    if all {
        q.push(("status", "all".to_string()));
    }
    if let Some(a) = author {
        q.push(("author", a.to_string()));
    }
    // v0.34 Z1 — attribution filter (beside the you|claude role).
    if let Some(u) = user {
        q.push(("user", u.to_string()));
    }
    if stale {
        q.push(("stale", "true".to_string()));
    }
    if let Some(f) = folder {
        q.push(("folder", f.to_string()));
    }
    if let Some(id) = artifact_id {
        q.push(("artifact_id", id.to_string()));
    }
    let body: Value = client
        .get(&url)
        .query(&q)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .json()
        .await?;
    Ok(body["comments"].as_array().cloned().unwrap_or_default())
}

// --- show ------------------------------------------------------------------

/// `kb comments show [<kb>] [<artifact_id>] [--path FILE]` — print one
/// artifact's full comment thread (bodies, replies, choices, timestamps).
pub async fn show(
    kb: Option<&str>,
    artifact_id: Option<&str>,
    path: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (kb, id) = resolve_target_auto(kb, artifact_id, path, daemon, bearer).await?;
    let url = format!(
        "{}/api/kb/{}/review/{}",
        base_url(daemon),
        encode_path_segment(&kb),
        encode_path_segment(&id),
    );
    let client = client_with_timeout_and_bearer(5, bearer)?;
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if resp.status().as_u16() == 404 {
        println!("(no comments for {kb}/{id})");
        return Ok(());
    }
    let file: Value = resp
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .json()
        .await?;

    let title = file["artifact"]["title"].as_str().unwrap_or("");
    println!(
        "# {}  ({kb}/{id})",
        if title.is_empty() { id.as_str() } else { title }
    );
    // W2.15a — the review-pass verdict, when set. Already in the fetched
    // ReviewFile (no extra round-trip), so this is a "cheap" header line —
    // unlike `list`'s flat `/reviews` rows, which don't carry it.
    if let Some(v) = file.get("verdict").filter(|v| !v.is_null()) {
        let state = v["state"].as_str().unwrap_or("?").replace('_', " ");
        let by = v["by"].as_str().unwrap_or("?");
        match v["note"].as_str().filter(|n| !n.is_empty()) {
            Some(n) => println!("Verdict: {state} (by {by}) — {n}"),
            None => println!("Verdict: {state} (by {by})"),
        }
    }
    let empty = Vec::new();
    let comments = file["comments"].as_array().unwrap_or(&empty);
    if comments.is_empty() {
        println!("(no comments)");
        return Ok(());
    }
    for c in comments {
        let edited = if c["editedAt"].is_string() {
            " (edited)"
        } else {
            ""
        };
        println!(
            "\n{} [{}] {} — {}{edited}",
            c["id"].as_str().unwrap_or("?"),
            c["status"].as_str().unwrap_or("?"),
            anchor_label(&c["anchor"]),
            c["author"].as_str().unwrap_or("?"),
        );
        println!(
            "  {}",
            c["body"].as_str().unwrap_or("").replace('\n', "\n  ")
        );
        for choice in c["choices"].as_array().unwrap_or(&empty) {
            println!("    [choice] {}", choice["label"].as_str().unwrap_or(""));
        }
        for r in c["replies"].as_array().unwrap_or(&empty) {
            let redited = if r["editedAt"].is_string() {
                " (edited)"
            } else {
                ""
            };
            println!(
                "  ↳ {} {}{redited}: {}",
                r["id"].as_str().unwrap_or("?"),
                r["author"].as_str().unwrap_or("?"),
                r["body"].as_str().unwrap_or("").replace('\n', "\n    "),
            );
        }
    }
    Ok(())
}

// --- export ----------------------------------------------------------------

/// `kb comments export [<kb>] [<artifact_id>] [--path FILE]
/// [--format claude|json|md] [--out-dir DIR]` — stream the daemon-rendered
/// export to stdout (`POST /api/kb/{kb}/review/{id}/export?format=`), OR with
/// `--out-dir` write a self-contained bundle: `review.<ext>` with inline
/// `attachment:<aid>` refs rewritten to relative `attachments/<aid>-<name>`
/// paths, plus every attachment blob downloaded into `attachments/`. The
/// bundle travels with the comments — nothing points back at the daemon.
#[allow(clippy::too_many_arguments)]
pub async fn export(
    kb: Option<&str>,
    artifact_id: Option<&str>,
    path: Option<&str>,
    format: &str,
    out_dir: Option<&str>,
    embed: bool,
    out: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (kb, id) = resolve_target_auto(kb, artifact_id, path, daemon, bearer).await?;
    if embed {
        // Portable round-trip (borrowed from redline): bake the review state
        // into a standalone copy of the artifact HTML so it travels with its
        // comments. The sidecar stays canonical; this is an export.
        return export_embed(daemon, &kb, &id, out, bearer).await;
    }
    let url = format!(
        "{}/api/kb/{}/review/{}/export",
        base_url(daemon),
        encode_path_segment(&kb),
        encode_path_segment(&id),
    );
    let client = client_with_timeout_and_bearer(5, bearer)?;
    let resp = client
        .post(&url)
        .query(&[("format", format)])
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        anyhow::bail!("export {kb}/{id} failed: HTTP {status} — {text}");
    }

    let Some(dir) = out_dir else {
        print!("{text}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        return Ok(());
    };

    // Bundle mode: enumerate attachments from the review JSON, rewrite the
    // export's inline refs to relative paths, and download each blob.
    let review = fetch_review_value(daemon, &kb, &id, bearer).await?;
    let (map, blobs) = collect_attachment_refs(&review);
    let rewritten = kb_core::attachments::rewrite_attachment_refs(&text, &map);

    let dirp = std::path::Path::new(dir);
    std::fs::create_dir_all(dirp).with_context(|| format!("creating {dir}"))?;
    let ext = if format == "json" { "json" } else { "md" };
    std::fs::write(dirp.join(format!("review.{ext}")), rewritten.as_bytes())
        .with_context(|| format!("writing review.{ext}"))?;
    if !blobs.is_empty() {
        std::fs::create_dir_all(dirp.join("attachments"))?;
        for (aid, rel) in &blobs {
            let bytes = download_attachment(daemon, &kb, &id, aid, bearer).await?;
            std::fs::write(dirp.join(rel), &bytes).with_context(|| format!("writing {rel}"))?;
        }
    }
    println!(
        "✓ wrote bundle to {dir} (review.{ext} + {} attachment(s))",
        blobs.len()
    );
    Ok(())
}

/// GET the full review JSON for an artifact (404 → an empty envelope), used
/// by `export --out-dir` to enumerate attachments.
async fn fetch_review_value(
    daemon: Option<&str>,
    kb: &str,
    id: &str,
    bearer: Option<&str>,
) -> Result<Value> {
    let url = format!(
        "{}/api/kb/{}/review/{}",
        base_url(daemon),
        encode_path_segment(kb),
        encode_path_segment(id),
    );
    let client = client_with_timeout_and_bearer(5, bearer)?;
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if resp.status().as_u16() == 404 {
        return Ok(json!({ "comments": [] }));
    }
    Ok(resp
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .json()
        .await?)
}

/// Walk a review JSON's comments + replies, building `(aid → relative path)`
/// for ref-rewriting and a `(aid, relpath)` download list. Relpath is
/// `attachments/<aid>-<sanitized-filename>` (aid-prefixed so two same-named
/// files never collide).
fn collect_attachment_refs(
    review: &Value,
) -> (
    std::collections::BTreeMap<String, String>,
    Vec<(String, String)>,
) {
    let mut map = std::collections::BTreeMap::new();
    let mut blobs = Vec::new();
    let empty = Vec::new();
    let mut gather = |node: &Value| {
        for a in node["attachments"].as_array().unwrap_or(&empty) {
            let Some(aid) = a["id"].as_str().filter(|s| !s.is_empty()) else {
                continue;
            };
            let filename =
                kb_core::attachments::sanitize_filename(a["filename"].as_str().unwrap_or("file"));
            let rel = format!("attachments/{aid}-{filename}");
            map.insert(aid.to_string(), rel.clone());
            blobs.push((aid.to_string(), rel));
        }
    };
    for c in review["comments"].as_array().unwrap_or(&empty) {
        gather(c);
        for r in c["replies"].as_array().unwrap_or(&empty) {
            gather(r);
        }
    }
    (map, blobs)
}

/// Download one attachment blob over HTTP (60s — blobs can be MBs).
async fn download_attachment(
    daemon: Option<&str>,
    kb: &str,
    id: &str,
    aid: &str,
    bearer: Option<&str>,
) -> Result<Vec<u8>> {
    let url = format!(
        "{}/api/kb/{}/review/{}/attachments/{}",
        base_url(daemon),
        encode_path_segment(kb),
        encode_path_segment(id),
        encode_path_segment(aid),
    );
    let client = client_with_timeout_and_bearer(60, bearer)?;
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        anyhow::bail!("download attachment {aid} failed: HTTP {status}");
    }
    Ok(resp.bytes().await?.to_vec())
}

/// `export --embed` mode: fetch the artifact HTML + its review JSON, splice
/// an inert `kb-review-state` block into the HTML, and write the standalone
/// copy to `--out` (or stdout). The single file then carries its comments
/// anywhere; `kb comments import` reads them back.
async fn export_embed(
    daemon: Option<&str>,
    kb: &str,
    id: &str,
    out: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let client = client_with_timeout_and_bearer(10, bearer)?;
    // Artifact HTML.
    let art_url = format!(
        "{}/api/kb/{}/artifact/{}",
        base_url(daemon),
        encode_path_segment(kb),
        encode_path_segment(id),
    );
    let resp = client
        .get(&art_url)
        .send()
        .await
        .with_context(|| format!("GET {art_url}"))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        anyhow::bail!("fetch artifact {kb}/{id} failed: HTTP {status}");
    }
    let html = resp.text().await?;
    // Review JSON → ReviewFile.
    let rev_url = format!(
        "{}/api/kb/{}/review/{}",
        base_url(daemon),
        encode_path_segment(kb),
        encode_path_segment(id),
    );
    let resp = client
        .get(&rev_url)
        .send()
        .await
        .with_context(|| format!("GET {rev_url}"))?;
    if resp.status().as_u16() == 404 {
        anyhow::bail!("{kb}/{id} has no comments to embed");
    }
    let text = resp
        .error_for_status()
        .with_context(|| format!("GET {rev_url}"))?
        .text()
        .await?;
    let review: kb_core::review::ReviewFile =
        serde_json::from_str(&text).context("parsing review JSON")?;
    let embedded =
        kb_core::review::embed_into_html(&html, &review).context("embedding review state")?;
    match out {
        Some(p) => {
            std::fs::write(p, embedded.as_bytes()).with_context(|| format!("writing {p}"))?;
            eprintln!(
                "✓ wrote {p} with {} embedded comment(s)",
                review.comments.len()
            );
        }
        None => {
            print!("{embedded}");
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
    }
    Ok(())
}

// --- apply (atomic batch) --------------------------------------------------

/// `kb comments apply [<kb>] [<artifact_id>] [--path FILE]
/// (--ops-file PATH | --ops-json STR)` — POST an ordered batch of comment
/// mutations to `/review/{id}/apply`, applied atomically server-side (one
/// lock, one save, one SSE). The payload is a JSON array of ops, or an
/// object `{"ops":[…]}`. Mirrors redline's `redline apply <file> <payload>`.
#[allow(clippy::too_many_arguments)]
pub async fn apply(
    kb: Option<&str>,
    artifact_id: Option<&str>,
    path: Option<&str>,
    ops_file: Option<&str>,
    ops_json: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let raw = match (ops_file, ops_json) {
        (Some(f), _) => std::fs::read_to_string(f).with_context(|| format!("reading {f}"))?,
        (None, Some(s)) => s.to_string(),
        (None, None) => anyhow::bail!("provide --ops-file <path> or --ops-json <json>"),
    };
    let parsed: Value = serde_json::from_str(&raw).context("parsing ops JSON")?;
    // Accept a bare array OR a {"ops":[…]} object.
    let body = if parsed.is_array() {
        json!({ "ops": parsed })
    } else {
        parsed
    };
    let (kb, id) = resolve_target_auto(kb, artifact_id, path, daemon, bearer).await?;
    let url = format!(
        "{}/api/kb/{}/review/{}/apply",
        base_url(daemon),
        encode_path_segment(&kb),
        encode_path_segment(&id),
    );
    let client = client_with_timeout_and_bearer(10, bearer)?;
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        anyhow::bail!("apply to {kb}/{id} failed: HTTP {status} — {text}");
    }
    let v: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
    println!(
        "✓ applied {} op(s) to {kb}/{id}",
        v["applied"].as_u64().unwrap_or(0)
    );
    if let Some(ids) = v["created_comment_ids"].as_array() {
        let joined: Vec<&str> = ids.iter().filter_map(|x| x.as_str()).collect();
        if !joined.is_empty() {
            println!("  created comments: {}", joined.join(", "));
        }
    }
    Ok(())
}

// --- import (portable round-trip) ------------------------------------------

/// `kb comments import <file.html> [<kb>] [<artifact_id>] [--path FILE]
/// [--force]` — read the `kb-review-state` block embedded by
/// `export --embed` and POST it to `/review/{id}/import`, restoring the
/// comments (ids/statuses/replies preserved). Refuses to overwrite existing
/// non-empty comments unless `--force`.
#[allow(clippy::too_many_arguments)]
pub async fn import(
    kb: Option<&str>,
    artifact_id: Option<&str>,
    path: Option<&str>,
    file: &str,
    force: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let html = std::fs::read_to_string(file).with_context(|| format!("reading {file}"))?;
    let review = kb_core::review::extract_from_html(&html)
        .context("parsing embedded review state")?
        .ok_or_else(|| anyhow::anyhow!("no embedded kb-review-state block found in {file}"))?;
    let (kb, id) = resolve_target_auto(kb, artifact_id, path, daemon, bearer).await?;
    let url = format!(
        "{}/api/kb/{}/review/{}/import",
        base_url(daemon),
        encode_path_segment(&kb),
        encode_path_segment(&id),
    );
    let client = client_with_timeout_and_bearer(10, bearer)?;
    let mut req = client.post(&url).json(&review);
    if force {
        req = req.query(&[("force", "true")]);
    }
    let resp = req.send().await.with_context(|| format!("POST {url}"))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        anyhow::bail!("import into {kb}/{id} failed: HTTP {status} — {text}");
    }
    let v: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
    eprintln!(
        "✓ imported {} comment(s) into {kb}/{id}",
        v["imported"]
            .as_u64()
            .unwrap_or(review.comments.len() as u64)
    );
    Ok(())
}

// --- add -------------------------------------------------------------------

/// `kb comments add [<kb>] [<artifact_id>] --body … [--anchor …]
/// [--author you|claude] [--page SRC] [--choice-json …]`.
#[allow(clippy::too_many_arguments)]
pub async fn add(
    kb: Option<&str>,
    artifact_id: Option<&str>,
    path_input: Option<&str>,
    body: &str,
    anchor_spec: &str,
    author: &str,
    choice_specs: &[String],
    page: Option<&str>,
    attach_files: &[String],
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (kb, id) = resolve_target_auto(kb, artifact_id, path_input, daemon, bearer).await?;
    let anchor = parse_anchor(anchor_spec)?;
    let author = match author {
        "you" | "claude" => author,
        other => anyhow::bail!("unknown --author {other:?}; expected 'you' or 'claude'"),
    };
    // Stage any --attach files first; append each one's inline token to the
    // body and adopt it via `attachment_ids` (so it shows in the strip AND
    // is pointed out inline) — one-shot.
    let mut body_text = body.to_string();
    let mut attachment_ids: Vec<String> = Vec::new();
    if !attach_files.is_empty() {
        let stage_url = format!(
            "{}/api/kb/{}/review/{}/attachments",
            base_url(daemon),
            encode_path_segment(&kb),
            encode_path_segment(&id),
        );
        let staged =
            upload_multipart(&stage_url, attach_files, author, bearer, "stage attachment").await?;
        for a in staged.as_array().map(|x| x.as_slice()).unwrap_or(&[]) {
            if let Some(aid) = a["id"].as_str() {
                attachment_ids.push(aid.to_string());
                body_text.push_str(&format!("\n\n{}", staged_token(a)));
            }
        }
    }
    let mut payload = json!({ "body": body_text, "anchor": anchor, "author": author });
    if let Some(choices) = parse_choices(choice_specs)? {
        payload["choices"] = choices;
    }
    if let Some(p) = page {
        payload["file"] = json!(p);
    }
    if !attachment_ids.is_empty() {
        payload["attachment_ids"] = json!(attachment_ids);
    }
    let url = format!(
        "{}/api/kb/{}/review/{}/comments",
        base_url(daemon),
        encode_path_segment(&kb),
        encode_path_segment(&id),
    );
    let client = client_with_timeout_and_bearer(5, bearer)?;
    let created = send_json(client.post(&url).json(&payload), "add comment").await?;
    let extra = if attachment_ids.is_empty() {
        String::new()
    } else {
        format!(" with {} attachment(s)", attachment_ids.len())
    };
    println!(
        "✓ added {} to {kb}/{id}{extra}",
        created["id"].as_str().unwrap_or("?")
    );
    Ok(())
}

// --- reply -----------------------------------------------------------------

/// `kb comments reply <comment_id> [--kb …] [--artifact-id …|--path …]
/// --body … [--choice-json …]` — append a Claude reply.
#[allow(clippy::too_many_arguments)]
pub async fn reply(
    kb: Option<&str>,
    artifact_id: Option<&str>,
    comment_id: &str,
    path_input: Option<&str>,
    body: &str,
    choice_specs: &[String],
    attach_files: &[String],
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (kb, id) = resolve_target_auto(kb, artifact_id, path_input, daemon, bearer).await?;
    // Stage any --attach files; append tokens + adopt via attachment_ids.
    let mut body_text = body.to_string();
    let mut attachment_ids: Vec<String> = Vec::new();
    if !attach_files.is_empty() {
        let stage_url = format!(
            "{}/api/kb/{}/review/{}/attachments",
            base_url(daemon),
            encode_path_segment(&kb),
            encode_path_segment(&id),
        );
        let staged = upload_multipart(
            &stage_url,
            attach_files,
            "claude",
            bearer,
            "stage attachment",
        )
        .await?;
        for a in staged.as_array().map(|x| x.as_slice()).unwrap_or(&[]) {
            if let Some(aid) = a["id"].as_str() {
                attachment_ids.push(aid.to_string());
                body_text.push_str(&format!("\n\n{}", staged_token(a)));
            }
        }
    }
    let mut payload = json!({ "author": "claude", "body": body_text });
    if let Some(choices) = parse_choices(choice_specs)? {
        payload["choices"] = choices;
    }
    if !attachment_ids.is_empty() {
        payload["attachment_ids"] = json!(attachment_ids);
    }
    let url = format!(
        "{}/api/kb/{}/review/{}/comments/{}/replies",
        base_url(daemon),
        encode_path_segment(&kb),
        encode_path_segment(&id),
        encode_path_segment(comment_id),
    );
    let client = client_with_timeout_and_bearer(5, bearer)?;
    let created = send_json(client.post(&url).json(&payload), "reply").await?;
    println!(
        "✓ replied {} to {comment_id} in {kb}/{id}",
        created["id"].as_str().unwrap_or("?")
    );
    Ok(())
}

// --- upload / attach (Y-track) ---------------------------------------------

/// `kb comments upload [<kb>] [<artifact_id>] <file>... [--path FILE] [--json]`
/// — stage attachment(s) WITHOUT adopting them onto a comment, and print the
/// inline `attachment:<aid>` token for each so Claude can splice it into a
/// `--body`. The purest "give me a ref" primitive; staged blobs are GC-reaped
/// after the grace window if never adopted.
#[allow(clippy::too_many_arguments)]
pub async fn upload(
    kb: Option<&str>,
    artifact_id: Option<&str>,
    path_input: Option<&str>,
    files: &[String],
    json_out: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    if files.is_empty() {
        anyhow::bail!("supply at least one <file> to upload");
    }
    let (kb, id) = resolve_target_auto(kb, artifact_id, path_input, daemon, bearer).await?;
    let url = format!(
        "{}/api/kb/{}/review/{}/attachments",
        base_url(daemon),
        encode_path_segment(&kb),
        encode_path_segment(&id),
    );
    let out = upload_multipart(&url, files, "claude", bearer, "upload").await?;
    let arr = out.as_array().cloned().unwrap_or_default();
    if json_out {
        println!("{}", serde_json::to_string_pretty(&arr)?);
        return Ok(());
    }
    for a in &arr {
        let aid = a["id"].as_str().unwrap_or("?");
        let filename = a["filename"].as_str().unwrap_or("file");
        let size = a["size"].as_u64().unwrap_or(0);
        println!("✓ staged {aid} ({filename}, {size} bytes) in {kb}/{id}");
        println!("  inline:  {}", staged_token(a));
    }
    Ok(())
}

/// `kb comments attach <comment_id> [--reply <rid>] <file>... [--kb …]
/// [--artifact-id …|--path …] [--json]` — upload + adopt attachment(s) onto an
/// existing comment (or reply). Prints the inline token for each.
#[allow(clippy::too_many_arguments)]
pub async fn attach(
    kb: Option<&str>,
    artifact_id: Option<&str>,
    comment_id: &str,
    reply_id: Option<&str>,
    path_input: Option<&str>,
    files: &[String],
    json_out: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    if files.is_empty() {
        anyhow::bail!("supply at least one <file> to attach");
    }
    let (kb, id) = resolve_target_auto(kb, artifact_id, path_input, daemon, bearer).await?;
    let base = base_url(daemon);
    let url = match reply_id {
        Some(rid) => format!(
            "{base}/api/kb/{}/review/{}/comments/{}/replies/{}/attachments",
            encode_path_segment(&kb),
            encode_path_segment(&id),
            encode_path_segment(comment_id),
            encode_path_segment(rid),
        ),
        None => format!(
            "{base}/api/kb/{}/review/{}/comments/{}/attachments",
            encode_path_segment(&kb),
            encode_path_segment(&id),
            encode_path_segment(comment_id),
        ),
    };
    let out = upload_multipart(&url, files, "claude", bearer, "attach").await?;
    let arr = out.as_array().cloned().unwrap_or_default();
    if json_out {
        println!("{}", serde_json::to_string_pretty(&arr)?);
        return Ok(());
    }
    let target = match reply_id {
        Some(rid) => format!("reply {rid}"),
        None => format!("comment {comment_id}"),
    };
    for a in &arr {
        let aid = a["id"].as_str().unwrap_or("?");
        let filename = a["filename"].as_str().unwrap_or("file");
        println!("✓ attached {aid} ({filename}) to {target} in {kb}/{id}");
        println!("  inline:  {}", staged_token(a));
    }
    Ok(())
}

// --- resolve / unresolve ---------------------------------------------------

/// `kb comments resolve [<kb>] [<artifact_id>] [<comment_id>] [--path …]
/// [--all]`.
#[allow(clippy::too_many_arguments)]
pub async fn resolve(
    kb: Option<&str>,
    artifact_id: Option<&str>,
    comment_id: Option<&str>,
    path_input: Option<&str>,
    all: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    set_status(
        kb,
        artifact_id,
        comment_id,
        path_input,
        all,
        daemon,
        bearer,
        true,
    )
    .await
}

/// `kb comments unresolve …` — reopen a resolved comment (or `--all`).
#[allow(clippy::too_many_arguments)]
pub async fn unresolve(
    kb: Option<&str>,
    artifact_id: Option<&str>,
    comment_id: Option<&str>,
    path_input: Option<&str>,
    all: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    set_status(
        kb,
        artifact_id,
        comment_id,
        path_input,
        all,
        daemon,
        bearer,
        false,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn set_status(
    kb: Option<&str>,
    artifact_id: Option<&str>,
    comment_id: Option<&str>,
    path_input: Option<&str>,
    all: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
    resolve: bool,
) -> Result<()> {
    let verb = if resolve { "resolve" } else { "unresolve" };
    if !all && comment_id.is_none() {
        anyhow::bail!("supply <comment_id> or pass --all");
    }
    let (kb, id) = resolve_target_auto(kb, artifact_id, path_input, daemon, bearer).await?;
    let client = client_with_timeout_and_bearer(5, bearer)?;
    let base = base_url(daemon);
    if all {
        let suffix = if resolve {
            "resolve-all"
        } else {
            "unresolve-all"
        };
        let url = format!(
            "{base}/api/kb/{}/review/{}/{suffix}",
            encode_path_segment(&kb),
            encode_path_segment(&id),
        );
        let out = send_json(client.post(&url), verb).await?;
        println!(
            "✓ {verb}d {} comment(s) in {kb}/{id}",
            out["flipped"].as_u64().unwrap_or(0)
        );
    } else {
        let cid = comment_id.expect("checked above");
        let url = format!(
            "{base}/api/kb/{}/review/{}/comments/{}/{verb}",
            encode_path_segment(&kb),
            encode_path_segment(&id),
            encode_path_segment(cid),
        );
        send_json(client.post(&url), verb).await?;
        println!("✓ {verb}d {cid} in {kb}/{id}");
    }
    Ok(())
}

// --- verdict (W2.15a) -------------------------------------------------------

/// `kb comments verdict <state> [--note …] [--clear] [--kb …]
/// [--artifact-id …|--path …]` — set (or, with `--clear`, remove) the
/// artifact's three-state review-pass verdict: comment | approve |
/// request-changes. Distinct from a single comment's open/resolved status
/// (`resolve`/`unresolve` above); this is the review-PASS-level signal
/// (`GET .../review/{id}`'s `verdict` field). The daemon also mirrors it
/// onto the artifact's own kb-tags as a `status-approved` /
/// `status-changes-requested` display shortcut (see `routes/comments.rs`).
#[allow(clippy::too_many_arguments)]
pub async fn verdict(
    kb: Option<&str>,
    artifact_id: Option<&str>,
    path_input: Option<&str>,
    state: Option<&str>,
    note: Option<&str>,
    clear: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (kb, id) = resolve_target_auto(kb, artifact_id, path_input, daemon, bearer).await?;
    let client = client_with_timeout_and_bearer(5, bearer)?;
    let base = base_url(daemon);
    let url = format!(
        "{base}/api/kb/{}/review/{}/verdict",
        encode_path_segment(&kb),
        encode_path_segment(&id),
    );
    if clear {
        send_json(client.delete(&url), "clear verdict").await?;
        println!("✓ cleared verdict on {kb}/{id}");
        return Ok(());
    }
    let raw_state = state.ok_or_else(|| anyhow!("supply <state> or pass --clear"))?;
    let normalized = match raw_state {
        "comment" => "comment",
        "approve" => "approve",
        "request-changes" | "request_changes" => "request_changes",
        other => anyhow::bail!(
            "unknown verdict state {other:?}; expected comment|approve|request-changes"
        ),
    };
    let mut body = json!({ "state": normalized });
    if let Some(n) = note {
        body["note"] = json!(n);
    }
    let out = send_json(client.post(&url).json(&body), "set verdict").await?;
    let echoed = out["verdict"]["state"].as_str().unwrap_or(normalized);
    println!("✓ verdict set to {echoed} on {kb}/{id}");
    Ok(())
}

// --- edit ------------------------------------------------------------------

/// `kb comments edit <comment_id> [--reply <reply_id>] --body …
/// [--kb …] [--artifact-id …|--path …]` — amend a comment or reply body.
#[allow(clippy::too_many_arguments)]
pub async fn edit(
    kb: Option<&str>,
    artifact_id: Option<&str>,
    comment_id: &str,
    reply_id: Option<&str>,
    path_input: Option<&str>,
    body: &str,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (kb, id) = resolve_target_auto(kb, artifact_id, path_input, daemon, bearer).await?;
    let base = base_url(daemon);
    let client = client_with_timeout_and_bearer(5, bearer)?;
    let url = match reply_id {
        Some(rid) => format!(
            "{base}/api/kb/{}/review/{}/comments/{}/replies/{}",
            encode_path_segment(&kb),
            encode_path_segment(&id),
            encode_path_segment(comment_id),
            encode_path_segment(rid),
        ),
        None => format!(
            "{base}/api/kb/{}/review/{}/comments/{}",
            encode_path_segment(&kb),
            encode_path_segment(&id),
            encode_path_segment(comment_id),
        ),
    };
    send_json(client.patch(&url).json(&json!({ "body": body })), "edit").await?;
    match reply_id {
        Some(rid) => println!("✓ edited reply {rid} in {kb}/{id}"),
        None => println!("✓ edited {comment_id} in {kb}/{id}"),
    }
    Ok(())
}

// --- reanchor (R9) ---------------------------------------------------------

/// `kb comments reanchor <comment_id> [--kb …] [--artifact-id …|--path …]
/// --anchor <spec>` — re-point a comment's anchor via
/// `PATCH .../comments/{cid}/anchor`. `<spec>` uses the same grammar as
/// `add --anchor` (file | chapter:PATH | section:ID |
/// selection:CSS:OFFSET:SNIPPET), parsed by the shared `parse_anchor`.
#[allow(clippy::too_many_arguments)]
pub async fn reanchor(
    kb: Option<&str>,
    artifact_id: Option<&str>,
    comment_id: &str,
    path_input: Option<&str>,
    anchor_spec: &str,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (kb, id) = resolve_target_auto(kb, artifact_id, path_input, daemon, bearer).await?;
    let anchor = parse_anchor(anchor_spec)?;
    let url = format!(
        "{}/api/kb/{}/review/{}/comments/{}/anchor",
        base_url(daemon),
        encode_path_segment(&kb),
        encode_path_segment(&id),
        encode_path_segment(comment_id),
    );
    let client = client_with_timeout_and_bearer(5, bearer)?;
    send_json(
        client.patch(&url).json(&json!({ "anchor": anchor })),
        "reanchor",
    )
    .await?;
    println!("✓ reanchored {comment_id} in {kb}/{id} → {anchor_spec}");
    Ok(())
}

// --- delete ----------------------------------------------------------------

/// `kb comments delete <comment_id> [--reply <reply_id>] --yes
/// [--kb …] [--artifact-id …|--path …]` — hard-delete a comment or reply.
/// Requires `--yes` (the deletion is not reversible).
#[allow(clippy::too_many_arguments)]
pub async fn delete(
    kb: Option<&str>,
    artifact_id: Option<&str>,
    comment_id: &str,
    reply_id: Option<&str>,
    path_input: Option<&str>,
    yes: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    if !yes {
        anyhow::bail!("refusing to delete without --yes (deletion is permanent)");
    }
    let (kb, id) = resolve_target_auto(kb, artifact_id, path_input, daemon, bearer).await?;
    let base = base_url(daemon);
    let client = client_with_timeout_and_bearer(5, bearer)?;
    let url = match reply_id {
        Some(rid) => format!(
            "{base}/api/kb/{}/review/{}/comments/{}/replies/{}",
            encode_path_segment(&kb),
            encode_path_segment(&id),
            encode_path_segment(comment_id),
            encode_path_segment(rid),
        ),
        None => format!(
            "{base}/api/kb/{}/review/{}/comments/{}",
            encode_path_segment(&kb),
            encode_path_segment(&id),
            encode_path_segment(comment_id),
        ),
    };
    send_json(client.delete(&url), "delete").await?;
    match reply_id {
        Some(rid) => println!("✓ deleted reply {rid} in {kb}/{id}"),
        None => println!("✓ deleted {comment_id} in {kb}/{id}"),
    }
    Ok(())
}

// --- anchor / choice parsing (used by `add`) -------------------------------

fn parse_anchor(spec: &str) -> Result<Value> {
    if spec == "file" {
        return Ok(json!({ "kind": "file" }));
    }
    if let Some(rest) = spec.strip_prefix("chapter:") {
        return Ok(json!({ "kind": "chapter", "path": rest }));
    }
    if let Some(rest) = spec.strip_prefix("section:") {
        return Ok(json!({ "kind": "section", "id": rest }));
    }
    if let Some(rest) = spec.strip_prefix("selection:") {
        // selection:<css_path>:<offset>:<snippet>. CSS selectors contain
        // `:` legitimately (`:nth-child(2)`), so scan colon positions from
        // the RIGHT for the last component that parses as u32 — that's the
        // offset; everything before is css_path, after is snippet.
        let (css, off, snippet) = parse_selection_payload(rest).ok_or_else(|| {
            anyhow!(
                "--anchor selection:CSS:OFFSET:SNIPPET — \
                 OFFSET must be a non-negative integer between CSS and SNIPPET (got {spec:?})"
            )
        })?;
        if css.is_empty() || snippet.is_empty() {
            anyhow::bail!(
                "--anchor selection:CSS:OFFSET:SNIPPET — needs all three fields (got {spec:?})"
            );
        }
        return Ok(json!({
            "kind": "selection",
            "css_path": css,
            "offset": off,
            "snippet": snippet,
        }));
    }
    anyhow::bail!(
        "unknown --anchor {spec:?}; expected one of: file, chapter:PATH, section:ID, selection:CSS:OFFSET:SNIPPET"
    )
}

/// Split a `selection:` payload into `(css_path, offset, snippet)` by
/// scanning colon positions from the right for the last `:N:` numeric
/// span. Returns `None` when no embedded u32 component exists.
fn parse_selection_payload(rest: &str) -> Option<(&str, u32, &str)> {
    let colons: Vec<usize> = rest.match_indices(':').map(|(i, _)| i).collect();
    if colons.len() < 2 {
        return None;
    }
    for w in colons.windows(2).rev() {
        let (l, r) = (w[0], w[1]);
        if let Ok(off) = rest[l + 1..r].parse::<u32>() {
            return Some((&rest[..l], off, &rest[r + 1..]));
        }
    }
    None
}

/// Parse repeatable `--choice-json` values into a JSON array of Choice
/// objects (`{label, reply, resolve}`). Returns `Ok(None)` when empty so
/// the caller omits the field.
fn parse_choices(specs: &[String]) -> Result<Option<Value>> {
    if specs.is_empty() {
        return Ok(None);
    }
    let mut out = Vec::with_capacity(specs.len());
    for s in specs {
        let v: Value = serde_json::from_str(s)
            .with_context(|| format!("--choice-json must be valid JSON: {s:?}"))?;
        let obj = v
            .as_object()
            .ok_or_else(|| anyhow!("--choice-json must be a JSON object: {s}"))?;
        let label = obj
            .get("label")
            .and_then(|x| x.as_str())
            .filter(|x| !x.trim().is_empty())
            .ok_or_else(|| anyhow!("--choice-json needs a non-empty string `label`: {s}"))?;
        let reply = obj
            .get("reply")
            .and_then(|x| x.as_str())
            .filter(|x| !x.trim().is_empty())
            .ok_or_else(|| anyhow!("--choice-json needs a non-empty string `reply`: {s}"))?;
        let resolve = obj
            .get("resolve")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        out.push(json!({ "label": label, "reply": reply, "resolve": resolve }));
    }
    Ok(Some(Value::Array(out)))
}

// --- formatting helpers ----------------------------------------------------

fn anchor_label(a: &Value) -> String {
    match a.get("kind").and_then(|k| k.as_str()) {
        Some("file") => "file".into(),
        Some("chapter") => format!("chapter:{}", a["path"].as_str().unwrap_or("")),
        Some("section") => format!("section:{}", a["id"].as_str().unwrap_or("")),
        Some("selection") => format!("selection:{}", a["snippet"].as_str().unwrap_or("")),
        _ => "?".into(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn truncate_one_line(s: &str, max: usize) -> String {
    truncate(s.lines().next().unwrap_or(""), max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_handles_short_strings() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 6), "hello…");
    }

    #[test]
    fn truncate_one_line_takes_first_line() {
        assert_eq!(truncate_one_line("a\nb\nc", 10), "a");
    }

    #[test]
    fn fmt_age_buckets_by_magnitude() {
        let now = chrono::Utc::now().timestamp();
        assert_eq!(fmt_age(0), "-");
        assert_eq!(fmt_age(now - 5), "5s");
        assert_eq!(fmt_age(now - 120), "2m");
        assert_eq!(fmt_age(now - 3 * 3600), "3h");
        assert_eq!(fmt_age(now - 2 * 86_400), "2d");
    }

    #[test]
    fn anchor_label_renders_each_kind() {
        assert_eq!(anchor_label(&json!({"kind":"file"})), "file");
        assert_eq!(
            anchor_label(&json!({"kind":"chapter","path":"Top > Mid"})),
            "chapter:Top > Mid"
        );
        assert_eq!(
            anchor_label(&json!({"kind":"section","id":"intro"})),
            "section:intro"
        );
        assert_eq!(
            anchor_label(&json!({"kind":"selection","snippet":"hi"})),
            "selection:hi"
        );
    }

    #[test]
    fn parse_choices_builds_array_and_defaults_resolve() {
        let specs = vec![
            r#"{"label":"Apply","reply":"yes, apply it","resolve":true}"#.to_string(),
            r#"{"label":"Skip","reply":"skip for now"}"#.to_string(),
        ];
        let out = parse_choices(&specs).unwrap().unwrap();
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["label"], "Apply");
        assert_eq!(arr[0]["resolve"], true);
        assert_eq!(arr[1]["resolve"], false);
    }

    #[test]
    fn parse_choices_empty_is_none() {
        assert!(parse_choices(&[]).unwrap().is_none());
    }

    #[test]
    fn parse_choices_rejects_bad_specs() {
        assert!(parse_choices(&["not json".to_string()]).is_err());
        assert!(parse_choices(&["[1,2]".to_string()]).is_err());
        assert!(parse_choices(&[r#"{"label":"X"}"#.to_string()]).is_err());
        assert!(parse_choices(&[r#"{"label":"","reply":"y"}"#.to_string()]).is_err());
    }

    #[test]
    fn parse_anchor_file_and_section() {
        assert_eq!(parse_anchor("file").unwrap(), json!({"kind":"file"}));
        assert_eq!(
            parse_anchor("section:intro").unwrap(),
            json!({"kind":"section","id":"intro"})
        );
    }

    #[test]
    fn parse_selection_payload_preserves_colons_in_css_pseudo() {
        let (css, off, snip) =
            parse_selection_payload("article > p:nth-child(2):42:the body text").unwrap();
        assert_eq!(css, "article > p:nth-child(2)");
        assert_eq!(off, 42);
        assert_eq!(snip, "the body text");
    }

    #[test]
    fn parse_selection_payload_no_numeric_part_returns_none() {
        assert!(parse_selection_payload("foo:bar:baz").is_none());
    }

    #[test]
    fn parse_anchor_selection_with_pseudo_round_trips() {
        let json = parse_anchor("selection:article > p:nth-child(2):42:the body").unwrap();
        assert_eq!(json["kind"], "selection");
        assert_eq!(json["css_path"], "article > p:nth-child(2)");
        assert_eq!(json["offset"], 42);
        assert_eq!(json["snippet"], "the body");
    }
}
